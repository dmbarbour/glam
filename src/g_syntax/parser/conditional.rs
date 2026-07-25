//! Structural parsing for conditional expressions.
//!
//! Conditional keywords own borrowed token ranges. Delimiter groups and
//! nested `if` expressions therefore cannot donate `then` or `else` to an
//! enclosing conditional.

use crate::g_syntax::{Diagnostic, IfExpr, SyntaxExpr};

use super::expression_context::{ExpressionContext, ParsedExpression};
use super::input::{TokenRange, TokenView};
use super::lexical::{LeadingTrivia, SpannedToken, TokenKind};
use super::pattern::parse_guard_clauses;
use super::structural::{is_layout_empty, parse_expression_in_context, trim_layout};

type ParseResult<T> = Result<T, Vec<Diagnostic>>;

#[derive(Clone, Copy)]
enum IfPhase {
    Guards,
    Then,
}

pub(in crate::g_syntax::parser) fn parse_if_expression(
    view: TokenView<'_, '_>,
    if_index: usize,
    context: ExpressionContext,
) -> ParseResult<ParsedExpression> {
    let Some(if_token) = view.token_at(if_index) else {
        return Err(error_at_view(
            view,
            "if expression starts outside its token view",
        ));
    };
    if !token_is_name(if_token, "if") {
        return Err(error_at_token(view, if_token, "expected `if`"));
    }

    let end = conditional_hard_end(view, if_index);
    let owned = view_between(view, if_index, end);
    let (then_index, else_index) = conditional_boundaries(owned, if_index)?;
    let guards = trim_layout(view_between(owned, if_index + 1, then_index));
    let then_result = trim_layout(view_between(owned, then_index + 1, else_index));
    let else_result = trim_layout(view_between(owned, else_index + 1, owned.range().end()));

    if is_layout_empty(guards) {
        return Err(error_at_token(
            owned,
            if_token,
            "if expression requires a guard before `then`",
        ));
    }
    if is_layout_empty(then_result) {
        return Err(error_at_token(
            owned,
            owned
                .token_at(then_index)
                .expect("selected `then` belongs to the conditional"),
            "if expression requires a result after `then`",
        ));
    }
    if is_layout_empty(else_result) {
        return Err(error_at_token(
            owned,
            owned
                .token_at(else_index)
                .expect("selected `else` belongs to the conditional"),
            "if expression requires a fallback after `else`",
        ));
    }

    let guards = parse_guard_clauses(guards, "if guard")?;
    let then_result = parse_expression_in_context(then_result, context.child_owner(then_result))?;
    let else_result = parse_expression_in_context(else_result, context.child_owner(else_result))?;

    Ok(ParsedExpression::new(
        SyntaxExpr::If(IfExpr {
            guards,
            then_result: Box::new(then_result),
            else_result: Box::new(else_result),
        }),
        end,
    ))
}

fn conditional_boundaries(view: TokenView<'_, '_>, if_index: usize) -> ParseResult<(usize, usize)> {
    let mut phases = vec![IfPhase::Guards];
    let mut outer_then = None;

    for indexed in view.top_level() {
        if indexed.index() <= if_index || !is_contextual_keyword(view, indexed.index()) {
            continue;
        }
        match indexed.token().kind() {
            TokenKind::Name("if") => phases.push(IfPhase::Guards),
            TokenKind::Name("then") => {
                let Some(phase) = phases.last_mut() else {
                    continue;
                };
                if matches!(phase, IfPhase::Then) {
                    return Err(error_at_token(
                        view,
                        indexed.token(),
                        "unexpected `then`; an if branch is already active",
                    ));
                }
                *phase = IfPhase::Then;
                if phases.len() == 1 {
                    outer_then = Some(indexed.index());
                }
            }
            TokenKind::Name("else") => {
                let Some(phase) = phases.last() else {
                    continue;
                };
                if matches!(phase, IfPhase::Guards) {
                    return Err(error_at_token(
                        view,
                        indexed.token(),
                        "if expression requires `then` before `else`",
                    ));
                }
                if phases.len() == 1 {
                    return Ok((
                        outer_then.expect("outer if phase changed only at its `then`"),
                        indexed.index(),
                    ));
                }
                phases.pop();
            }
            _ => {}
        }
    }

    let message = if outer_then.is_some() {
        "if expression requires `else` and a fallback result"
    } else {
        "if expression requires `then` and `else`"
    };
    Err(error_at_view(view, message))
}

fn conditional_hard_end(view: TokenView<'_, '_>, if_index: usize) -> usize {
    let containing_close = view
        .source()
        .groups()
        .iter()
        .filter_map(|group| {
            let close = group.close_token()?;
            (group.open_token() < if_index && if_index < close)
                .then_some((group.open_token(), close))
        })
        .max_by_key(|(open, _)| *open)
        .map(|(_, close)| close)
        .unwrap_or(view.range().end())
        .min(view.range().end());
    let candidate = view_between(view, if_index, containing_close);
    candidate
        .top_level()
        .find_map(|indexed| {
            (indexed.index() > if_index
                && matches!(indexed.token().kind(), TokenKind::Symbol("," | ";")))
            .then_some(indexed.index())
        })
        .unwrap_or(containing_close)
}

fn is_contextual_keyword(view: TokenView<'_, '_>, index: usize) -> bool {
    let Some(token) = view.token_at(index) else {
        return false;
    };
    token.leading() != LeadingTrivia::Joint
        || index == view.range().start()
        || index.checked_sub(1).is_some_and(|previous| {
            view.token_at(previous)
                .is_some_and(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
        })
}

fn token_is_name(token: &SpannedToken<'_>, expected: &str) -> bool {
    matches!(token.kind(), TokenKind::Name(name) if *name == expected)
}

fn view_between<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    start: usize,
    end: usize,
) -> TokenView<'lex, 'source> {
    view.subview(TokenRange::new(start, end).expect("ordered token indices form a range"))
        .expect("conditional range remains within its source view")
}

fn error_at_view(view: TokenView<'_, '_>, message: impl Into<String>) -> Vec<Diagnostic> {
    let line = view
        .first_significant()
        .and_then(|(_, token)| view.line_at_span(token.span()))
        .unwrap_or(1);
    vec![Diagnostic::error(line, message)]
}

fn error_at_token(
    view: TokenView<'_, '_>,
    token: &SpannedToken<'_>,
    message: impl Into<String>,
) -> Vec<Diagnostic> {
    vec![Diagnostic::error(
        view.line_at_span(token.span()).unwrap_or(1),
        message,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::g_syntax::{SyntaxGuardClause, SyntaxPatternKind};

    fn parse(source: &str) -> SyntaxExpr {
        super::super::input::parse_expression_fragment(source.as_bytes(), |view| {
            super::super::structural::parse_expression_in_context(
                view,
                ExpressionContext::for_fragment(view),
            )
        })
        .unwrap_or_else(|diagnostics| panic!("`{source}` reported {diagnostics:#?}"))
    }

    #[test]
    fn prefix_if_owns_guards_and_both_results() {
        let SyntaxExpr::If(if_expr) = parse("if value = 42 and _ then value else 0") else {
            panic!("prefix syntax should produce an if expression");
        };
        assert_eq!(if_expr.guards.len(), 2);
        assert!(matches!(
            &if_expr.guards[0],
            SyntaxGuardClause::ValueBind { pattern, .. }
                if matches!(&pattern.kind, SyntaxPatternKind::Capture(name) if name == "value")
        ));
        assert!(matches!(if_expr.guards[1], SyntaxGuardClause::Pass));
        assert!(matches!(
            if_expr.then_result.as_ref(),
            SyntaxExpr::Name(name) if name == "value"
        ));
        assert!(matches!(
            if_expr.else_result.as_ref(),
            SyntaxExpr::Number(_)
        ));
    }

    #[test]
    fn nested_if_keywords_do_not_escape_to_the_outer_conditional() {
        let SyntaxExpr::If(outer) = parse("if _ then if _ then 1 else 2 else 3") else {
            panic!("source should produce an outer if expression");
        };
        assert!(matches!(outer.then_result.as_ref(), SyntaxExpr::If(_)));
        assert!(matches!(outer.else_result.as_ref(), SyntaxExpr::Number(_)));
    }

    #[test]
    fn prefix_if_supports_hanging_and_next_line_results() {
        let hanging = parse("if _ then first\n          else second");
        assert!(matches!(hanging, SyntaxExpr::If(_)));

        let next_line = parse("if _ then\n  first\n  else\n    second");
        assert!(matches!(next_line, SyntaxExpr::If(_)));
    }

    #[test]
    fn malformed_prefix_if_reports_its_missing_boundary_or_result() {
        for (source, expected) in [
            ("if _ else 0", "requires `then` before `else`"),
            ("if _ then 1", "requires `else`"),
            ("if _ then else 0", "requires a result after `then`"),
            ("if _ then 1 else", "requires a fallback after `else`"),
        ] {
            let diagnostics =
                super::super::input::parse_expression_fragment(source.as_bytes(), |view| {
                    super::super::structural::parse_expression_in_context(
                        view,
                        ExpressionContext::for_fragment(view),
                    )
                })
                .expect_err("malformed conditional should fail");
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(expected)),
                "`{source}` reported {diagnostics:#?} instead of `{expected}`"
            );
        }
    }
}
