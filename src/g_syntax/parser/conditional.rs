//! Structural parsing for conditional expressions.
//!
//! Conditional keywords own borrowed token ranges. Delimiter groups and
//! nested `if` expressions therefore cannot donate `then` or `else` to an
//! enclosing conditional.

use crate::g_syntax::{
    Diagnostic, IfExpr, MatchArm, MatchExpr, MatchOutcome, MatchWhenExpr, SyntaxExpr, WhenArm,
};

use super::expression_context::{ExpressionContext, ParsedExpression};
use super::input::{TokenRange, TokenView};
use super::layout::LayoutView;
use super::lexical::{Delimiter, LeadingTrivia, SpannedToken, TokenKind};
use super::pattern::{parse_complete_pattern, parse_guard_clauses};
use super::structural::{
    is_layout_empty, parse_expression_in_context, split_braced_members, trim_layout,
};

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

pub(in crate::g_syntax::parser) fn parse_match_expression(
    view: TokenView<'_, '_>,
    match_index: usize,
    context: ExpressionContext,
) -> ParseResult<ParsedExpression> {
    let Some(match_token) = view.token_at(match_index) else {
        return Err(error_at_view(
            view,
            "match expression starts outside its token view",
        ));
    };
    if !token_is_name(match_token, "match") {
        return Err(error_at_token(view, match_token, "expected `match`"));
    }

    let after_match = trim_layout(view_between(view, match_index + 1, view.range().end()));
    if let Some((when_index, when_token)) = after_match.first_significant()
        && token_is_name(when_token, "when")
        && is_contextual_keyword(after_match, when_index)
    {
        let body = view_between(after_match, when_index + 1, after_match.range().end());
        let (members, end) = choice_member_views(
            body,
            context,
            "match-when body",
            "layout `match when` requires at least one arm",
            true,
        )?;
        let arms = members
            .into_iter()
            .map(|arm| parse_when_arm(arm, context))
            .collect::<ParseResult<Vec<_>>>()?;
        return Ok(ParsedExpression::new(
            SyntaxExpr::MatchWhen(MatchWhenExpr { arms }),
            end,
        ));
    }

    let with_index = view
        .top_level()
        .find(|indexed| {
            indexed.index() > match_index
                && is_contextual_keyword(view, indexed.index())
                && token_is_name(indexed.token(), "with")
        })
        .map(|indexed| indexed.index())
        .ok_or_else(|| {
            error_at_token(
                view,
                match_token,
                "match expression requires `with` and at least one layout arm or an explicit `{}` body",
            )
        })?;
    let subject = trim_layout(view_between(view, match_index + 1, with_index));
    if is_layout_empty(subject) {
        return Err(error_at_token(
            view,
            match_token,
            "match expression requires a subject before `with`",
        ));
    }
    let subject = parse_expression_in_context(subject, context.child_owner(subject))?;

    let after_with = view_between(view, with_index + 1, view.range().end());
    let (members, end) = choice_member_views(
        after_with,
        context,
        "match body",
        "layout match expression requires at least one arm",
        true,
    )?;
    let arms = members
        .into_iter()
        .map(|arm| parse_match_arm(arm, context))
        .collect::<ParseResult<Vec<_>>>()?;
    Ok(ParsedExpression::new(
        SyntaxExpr::Match(MatchExpr {
            subject: Box::new(subject),
            arms,
        }),
        end,
    ))
}

fn parse_match_arm(view: TokenView<'_, '_>, context: ExpressionContext) -> ParseResult<MatchArm> {
    let view = trim_layout(view);
    let (head, outcome) = split_choice_arm(view, context, "match arm")?;
    let when = top_level_contextual_names(head, "when");
    if when.len() > 1 {
        return Err(error_at_view(
            head,
            "match arm permits one top-level `when` guard boundary",
        ));
    }
    let (pattern, guards) = if let Some(when) = when.first().copied() {
        let pattern = trim_layout(view_between(head, head.range().start(), when));
        let guards = trim_layout(view_between(head, when + 1, head.range().end()));
        if is_layout_empty(pattern) {
            return Err(error_at_view(
                head,
                "match arm requires a pattern before `when`",
            ));
        }
        if is_layout_empty(guards) {
            return Err(error_at_view(
                head,
                "match arm requires guards after `when`",
            ));
        }
        (
            parse_complete_pattern(pattern)?,
            parse_guard_clauses(guards, "match arm guard")?,
        )
    } else {
        (parse_complete_pattern(head)?, Vec::new())
    };

    Ok(MatchArm {
        line: line_of_view(view),
        pattern,
        guards,
        outcome,
    })
}

fn parse_when_arm(view: TokenView<'_, '_>, context: ExpressionContext) -> ParseResult<WhenArm> {
    let view = trim_layout(view);
    let (guards, outcome) = split_choice_arm(view, context, "match-when arm")?;
    if is_layout_empty(guards) {
        return Err(error_at_view(
            view,
            "match-when arm requires guards before its outcome",
        ));
    }
    Ok(WhenArm {
        line: line_of_view(view),
        guards: parse_guard_clauses(guards, "match-when arm guard")?,
        outcome,
    })
}

fn split_choice_arm<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    context: ExpressionContext,
    label: &str,
) -> ParseResult<(TokenView<'lex, 'source>, MatchOutcome)> {
    let required_head = if label == "match arm" {
        "a pattern"
    } else {
        "guards"
    };
    let lines = LayoutView::new(view).lines();
    let Some(first_line) = lines.first().copied() else {
        return Err(error_at_view(view, format!("{label} cannot be empty")));
    };
    let first = trim_layout(
        view.subview(first_line.tokens())
            .expect("choice arm first line remains inside its view"),
    );
    let arrows = top_level_symbols(first, "=>");
    if arrows.len() > 1 {
        return Err(error_at_view(
            first,
            format!("{label} permits exactly one top-level `=>`"),
        ));
    }
    if let Some(arrow) = arrows.first().copied() {
        let head = trim_layout(view_between(first, first.range().start(), arrow));
        let result = trim_layout(view_between(view, arrow + 1, view.range().end()));
        if is_layout_empty(head) {
            return Err(error_at_view(
                view,
                format!("{label} requires {required_head} before `=>`"),
            ));
        }
        if is_layout_empty(result) {
            return Err(error_at_view(
                view,
                format!("{label} requires a result after `=>`"),
            ));
        }
        let line = line_of_view(result);
        let expression = parse_expression_in_context(result, context.child_owner(result))?;
        return Ok((head, MatchOutcome::Value { line, expression }));
    }

    let nested_when = top_level_contextual_names(first, "when")
        .into_iter()
        .next_back()
        .ok_or_else(|| {
            error_at_view(
                first,
                format!("{label} requires `=>` or a trailing `when` child block"),
            )
        })?;
    let head = trim_layout(view_between(first, first.range().start(), nested_when));
    if is_layout_empty(head) {
        return Err(error_at_view(
            first,
            format!("{label} requires {required_head} before nested `when`"),
        ));
    }
    let nested_body = view_between(view, nested_when + 1, view.range().end());
    let nested_context = context.with_continuation_floor(first_line.indentation());
    let (members, _) = choice_member_views(
        nested_body,
        nested_context,
        "nested match-when body",
        "nested `when` requires at least one child arm",
        false,
    )?;
    let arms = members
        .into_iter()
        .map(|arm| parse_when_arm(arm, nested_context))
        .collect::<ParseResult<Vec<_>>>()?;
    Ok((head, MatchOutcome::Nested(arms)))
}

fn choice_member_views<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    context: ExpressionContext,
    body_label: &str,
    missing_layout_message: &str,
    brace_can_start_member: bool,
) -> ParseResult<(Vec<TokenView<'lex, 'source>>, usize)> {
    let Some((body_start, body_token)) = view.first_significant() else {
        return Err(error_at_view(view, missing_layout_message));
    };
    if let TokenKind::Open {
        group,
        delimiter: Delimiter::Brace,
    } = body_token.kind()
    {
        let delimiter = view
            .group(*group)
            .expect("lexed choice brace refers to its delimiter group");
        let Some(close) = delimiter.close_token() else {
            return Err(error_at_token(
                view,
                body_token,
                format!("{body_label} has an unmatched or mismatched `}}`"),
            ));
        };
        let after_group = trim_layout(view_between(view, close + 1, view.range().end()));
        let group_is_member_head = brace_can_start_member
            && after_group.first_significant().is_some_and(|(_, token)| {
                matches!(token.kind(), TokenKind::Symbol("=>")) || token_is_name(token, "when")
            });
        if !group_is_member_head {
            let body = view_between(view, body_start, close + 1);
            let members = split_braced_members(body, body_label)
                .expect("choice body begins with a complete brace group")?;
            return Ok((members, close + 1));
        }
    }

    let block = LayoutView::new(view).block();
    if block.statements().is_empty() {
        return Err(error_at_view(view, missing_layout_message));
    }
    if !context.accepts_layout_anchor(block.anchor()) {
        return Err(vec![Diagnostic::error(
            block.statements()[0].line(),
            format!(
                "{body_label} begins at indentation {}; expected more than continuation floor {}",
                block.anchor(),
                context.continuation_floor()
            ),
        )]);
    }
    let end = block.end();
    let members = block
        .into_statements()
        .into_iter()
        .map(|statement| {
            view.subview(statement.tokens())
                .expect("layout choice arm remains within its body")
        })
        .collect();
    Ok((members, end))
}

fn top_level_contextual_names(view: TokenView<'_, '_>, expected: &str) -> Vec<usize> {
    view.top_level()
        .filter(|indexed| {
            is_contextual_keyword(view, indexed.index()) && token_is_name(indexed.token(), expected)
        })
        .map(|indexed| indexed.index())
        .collect()
}

fn top_level_symbols(view: TokenView<'_, '_>, expected: &str) -> Vec<usize> {
    view.top_level()
        .filter(|indexed| {
            matches!(indexed.token().kind(), TokenKind::Symbol(symbol) if *symbol == expected)
        })
        .map(|indexed| indexed.index())
        .collect()
}

fn line_of_view(view: TokenView<'_, '_>) -> usize {
    view.first_significant()
        .and_then(|(_, token)| view.line_at_span(token.span()))
        .unwrap_or(1)
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

    #[test]
    fn flat_match_parses_layout_and_braced_arms() {
        let SyntaxExpr::Match(layout) = parse(
            "match subject with\n  1 => \"one\"\n  value when value == 2 => \"two\"\n  _ => \"other\"",
        ) else {
            panic!("layout syntax should produce a match expression");
        };
        assert!(matches!(
            layout.subject.as_ref(),
            SyntaxExpr::Name(name) if name == "subject"
        ));
        assert_eq!(layout.arms.len(), 3);
        assert!(layout.arms[0].guards.is_empty());
        assert_eq!(layout.arms[1].guards.len(), 1);
        assert!(matches!(
            &layout.arms[1].pattern.kind,
            SyntaxPatternKind::Capture(name) if name == "value"
        ));

        let SyntaxExpr::Match(dict_pattern) =
            parse("match subject with\n  {} => \"empty\"\n  _ => \"other\"")
        else {
            panic!("a dictionary pattern must not become the braced match body");
        };
        assert_eq!(dict_pattern.arms.len(), 2);
        assert!(matches!(
            dict_pattern.arms[0].pattern.kind,
            SyntaxPatternKind::Dict { .. }
        ));

        let SyntaxExpr::Match(braced) =
            parse("match subject with {; [head] ++ tail => head; _ => 0;}")
        else {
            panic!("braced syntax should produce a match expression");
        };
        assert_eq!(braced.arms.len(), 2);

        let SyntaxExpr::Match(empty) = parse("match subject with {}") else {
            panic!("an explicit empty match should remain a match expression");
        };
        assert!(empty.arms.is_empty());
    }

    #[test]
    fn match_when_and_hierarchical_outcomes_parse_as_choice_trees() {
        let SyntaxExpr::MatchWhen(guard_only) = parse(
            "match when\n  value = 1 when\n    value == 2 => \"bad\"\n    value == 1 => value\n  _ => 0",
        ) else {
            panic!("guard-only syntax should produce a match-when expression");
        };
        assert_eq!(guard_only.arms.len(), 2);
        let MatchOutcome::Nested(children) = &guard_only.arms[0].outcome else {
            panic!("the first guard-only arm should own nested choices");
        };
        assert_eq!(children.len(), 2);

        let SyntaxExpr::Match(subject) = parse(
            "match subject with\n  value when value == 1 when\n    next = value + 1 => next\n    _ => value\n  _ => 0",
        ) else {
            panic!("source should produce a subject match");
        };
        let MatchOutcome::Nested(children) = &subject.arms[0].outcome else {
            panic!("the first subject arm should own nested choices");
        };
        assert_eq!(subject.arms[0].guards.len(), 1);
        assert_eq!(children.len(), 2);

        let SyntaxExpr::MatchWhen(braced) =
            parse("match when { _ when { 1 == 2 => \"bad\"; _ => \"ok\"; }; }")
        else {
            panic!("braced nested choices should produce a match-when expression");
        };
        assert!(matches!(braced.arms[0].outcome, MatchOutcome::Nested(_)));

        let SyntaxExpr::MatchWhen(empty) = parse("match when {}") else {
            panic!("an explicit empty guard-only search should remain a match-when expression");
        };
        assert!(empty.arms.is_empty());
    }

    #[test]
    fn complete_match_patterns_accept_unparenthesized_views() {
        let SyntaxExpr::Match(forward) = parse("match subject with { inspect -> value => value; }")
        else {
            panic!("source should produce a match expression");
        };
        assert!(matches!(
            forward.arms[0].pattern.kind,
            SyntaxPatternKind::View { .. }
        ));

        let SyntaxExpr::Match(backward) =
            parse("match subject with { value <- inspect => value; }")
        else {
            panic!("source should produce a match expression");
        };
        assert!(matches!(
            backward.arms[0].pattern.kind,
            SyntaxPatternKind::View { .. }
        ));
    }

    #[test]
    fn malformed_match_reports_its_missing_boundary_or_arm_part() {
        for (source, expected) in [
            ("match value", "requires `with`"),
            (
                "match value with",
                "layout match expression requires at least one arm",
            ),
            ("match value with { 1 }", "requires `=>`"),
            (
                "match value with { => 1 }",
                "requires a pattern before `=>`",
            ),
            ("match value with { 1 => }", "requires a result after `=>`"),
            (
                "match value with { value when => value }",
                "requires guards after `when`",
            ),
            (
                "match value with { _ => 1 => 2 }",
                "exactly one top-level `=>`",
            ),
        ] {
            let diagnostics =
                super::super::input::parse_expression_fragment(source.as_bytes(), |view| {
                    super::super::structural::parse_expression_in_context(
                        view,
                        ExpressionContext::for_fragment(view),
                    )
                })
                .expect_err("malformed match should fail");
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(expected)),
                "`{source}` reported {diagnostics:#?} instead of `{expected}`"
            );
        }
    }

    #[test]
    fn malformed_hierarchical_match_reports_missing_prefixes_and_children() {
        for (source, expected) in [
            (
                "match when",
                "layout `match when` requires at least one arm",
            ),
            (
                "match when\n  _ when",
                "nested `when` requires at least one child arm",
            ),
            (
                "match value with\n  when\n    _ => 1",
                "requires a pattern before nested `when`",
            ),
            ("match when\n  => 1", "requires guards before `=>`"),
        ] {
            let diagnostics =
                super::super::input::parse_expression_fragment(source.as_bytes(), |view| {
                    super::super::structural::parse_expression_in_context(
                        view,
                        ExpressionContext::for_fragment(view),
                    )
                })
                .expect_err("malformed hierarchical match should fail");
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(expected)),
                "`{source}` reported {diagnostics:#?} instead of `{expected}`"
            );
        }
    }
}
