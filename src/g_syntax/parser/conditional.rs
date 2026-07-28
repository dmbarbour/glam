//! Structural parsing for conditional expressions.
//!
//! Conditional keywords own borrowed token ranges. Delimiter groups and
//! nested `if` expressions therefore cannot donate `then` or `else` to an
//! enclosing conditional.

use crate::g_syntax::{
    ConditionalMode, ConditionalResultMode, Diagnostic, IfExpr, MatchArm, MatchCommitment,
    MatchExpr, MatchOutcome, MatchWhenExpr, SyntaxExpr, WhenArm,
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

#[derive(Clone, Copy)]
struct ThenBoundary {
    keyword: usize,
    result_start: usize,
    mode: ConditionalResultMode,
}

pub(in crate::g_syntax::parser) fn parse_if_expression(
    view: TokenView<'_, '_>,
    if_index: usize,
    context: ExpressionContext,
) -> ParseResult<ParsedExpression> {
    parse_if_like_expression(view, if_index, context, "if", ConditionalMode::Pure)
}

pub(in crate::g_syntax::parser) fn parse_try_expression(
    view: TokenView<'_, '_>,
    try_index: usize,
    context: ExpressionContext,
) -> ParseResult<ParsedExpression> {
    parse_if_like_expression(view, try_index, context, "try", ConditionalMode::Host)
}

pub(in crate::g_syntax::parser) fn is_postfix_if_candidate(
    view: TokenView<'_, '_>,
    if_index: usize,
) -> bool {
    matches!(
        postfix_if_boundary(view, if_index),
        PostfixIfBoundary::Postfix(_)
    )
}

pub(in crate::g_syntax::parser) fn parse_postfix_if_suffix(
    view: TokenView<'_, '_>,
    if_index: usize,
    context: ExpressionContext,
    then_result: SyntaxExpr,
) -> ParseResult<ParsedExpression> {
    let if_token = view
        .token_at(if_index)
        .expect("a selected postfix `if` remains inside its expression view");
    let end = conditional_hard_end(view, if_index);
    let owned = view_between(view, if_index, end);
    let else_index = match postfix_if_boundary(owned, if_index) {
        PostfixIfBoundary::Prefix => {
            return Err(error_at_token(
                owned,
                if_token,
                "postfix `if` does not use `then`; parenthesize a prefix `if` used inside its guard",
            ));
        }
        PostfixIfBoundary::Postfix(Some(index)) => index,
        PostfixIfBoundary::Postfix(None) => {
            return Err(error_at_token(
                owned,
                if_token,
                "postfix `if` requires `else` and a fallback result",
            ));
        }
    };
    let guards = trim_layout(view_between(owned, if_index + 1, else_index));
    let else_result = trim_layout(view_between(owned, else_index + 1, owned.range().end()));
    if is_layout_empty(guards) {
        return Err(error_at_token(
            owned,
            if_token,
            "postfix `if` requires guards before `else`",
        ));
    }
    if is_layout_empty(else_result) {
        return Err(error_at_token(
            owned,
            owned
                .token_at(else_index)
                .expect("selected `else` belongs to the postfix conditional"),
            "postfix `if` requires a fallback after `else`",
        ));
    }

    let guards = parse_guard_clauses(guards, "if guard")?;
    let else_result = parse_expression_in_context(else_result, context.child_owner(else_result))?;
    Ok(ParsedExpression::new(
        SyntaxExpr::If(IfExpr {
            mode: ConditionalMode::Pure,
            guards,
            then_mode: ConditionalResultMode::Ordinary,
            then_result: Box::new(then_result),
            else_result: Box::new(else_result),
        }),
        end,
    ))
}

fn parse_if_like_expression(
    view: TokenView<'_, '_>,
    head_index: usize,
    context: ExpressionContext,
    head: &str,
    mode: ConditionalMode,
) -> ParseResult<ParsedExpression> {
    let Some(head_token) = view.token_at(head_index) else {
        return Err(error_at_view(
            view,
            format!("{head} expression starts outside its token view"),
        ));
    };
    if !token_is_name(head_token, head) {
        return Err(error_at_token(
            view,
            head_token,
            format!("expected `{head}`"),
        ));
    }

    let end = conditional_hard_end(view, head_index);
    let owned = view_between(view, head_index, end);
    let (then_boundary, else_index) = conditional_boundaries(owned, head_index, head)?;
    let guards = trim_layout(view_between(owned, head_index + 1, then_boundary.keyword));
    let then_result = trim_layout(view_between(owned, then_boundary.result_start, else_index));
    let else_result = trim_layout(view_between(owned, else_index + 1, owned.range().end()));

    if is_layout_empty(guards) {
        return Err(error_at_token(
            owned,
            head_token,
            format!("{head} expression requires a guard before `then`"),
        ));
    }
    if is_layout_empty(then_result) {
        return Err(error_at_token(
            owned,
            owned
                .token_at(then_boundary.keyword)
                .expect("selected `then` belongs to the conditional"),
            format!(
                "{head} expression requires a result after `{}`",
                match then_boundary.mode {
                    ConditionalResultMode::Ordinary => "then",
                    ConditionalResultMode::Tentative => "then?",
                }
            ),
        ));
    }
    if is_layout_empty(else_result) {
        return Err(error_at_token(
            owned,
            owned
                .token_at(else_index)
                .expect("selected `else` belongs to the conditional"),
            format!("{head} expression requires a fallback after `else`"),
        ));
    }

    let guards = parse_guard_clauses(guards, &format!("{head} guard"))?;
    let then_result = parse_expression_in_context(then_result, context.child_owner(then_result))?;
    let else_result = parse_expression_in_context(else_result, context.child_owner(else_result))?;

    Ok(ParsedExpression::new(
        SyntaxExpr::If(IfExpr {
            mode,
            guards,
            then_mode: then_boundary.mode,
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
    parse_match_like_expression(view, match_index, context, "match", ConditionalMode::Pure)
}

pub(in crate::g_syntax::parser) fn parse_try_match_expression(
    view: TokenView<'_, '_>,
    try_match_index: usize,
    context: ExpressionContext,
) -> ParseResult<ParsedExpression> {
    parse_match_like_expression(
        view,
        try_match_index,
        context,
        "try_match",
        ConditionalMode::Host,
    )
}

#[derive(Clone, Copy)]
enum PostfixIfBoundary {
    Prefix,
    Postfix(Option<usize>),
}

fn postfix_if_boundary(view: TokenView<'_, '_>, if_index: usize) -> PostfixIfBoundary {
    let mut nested = Vec::new();
    for indexed in view.top_level() {
        if indexed.index() <= if_index || !is_contextual_keyword(view, indexed.index()) {
            continue;
        }
        match indexed.token().kind() {
            TokenKind::Name("if" | "try") => nested.push(IfPhase::Guards),
            TokenKind::Name("then") => {
                if let Some(phase) = nested.last_mut() {
                    *phase = IfPhase::Then;
                } else {
                    return PostfixIfBoundary::Prefix;
                }
            }
            TokenKind::Name("else") if nested.pop().is_none() => {
                return PostfixIfBoundary::Postfix(Some(indexed.index()));
            }
            TokenKind::Name("else") => {}
            _ => {}
        }
    }
    PostfixIfBoundary::Postfix(None)
}

fn parse_match_like_expression(
    view: TokenView<'_, '_>,
    head_index: usize,
    context: ExpressionContext,
    head: &str,
    mode: ConditionalMode,
) -> ParseResult<ParsedExpression> {
    let Some(head_token) = view.token_at(head_index) else {
        return Err(error_at_view(
            view,
            format!("{head} expression starts outside its token view"),
        ));
    };
    if !token_is_name(head_token, head) {
        return Err(error_at_token(
            view,
            head_token,
            format!("expected `{head}`"),
        ));
    }

    let star = view.token_at(head_index + 1).filter(|token| {
        matches!(token.kind(), TokenKind::Symbol("*")) && token.leading() == LeadingTrivia::Joint
    });
    if let Some(token) = view.token_at(head_index + 1)
        && matches!(token.kind(), TokenKind::Symbol("*"))
        && token.leading() != LeadingTrivia::Joint
    {
        return Err(error_at_token(
            view,
            token,
            format!("the open form is spelled `{head}*` without intervening whitespace"),
        ));
    }
    let commitment = if star.is_some() {
        MatchCommitment::Open
    } else {
        MatchCommitment::Cut
    };
    let body_start = head_index + 1 + usize::from(star.is_some());
    let display_head = if star.is_some() {
        format!("{head}*")
    } else {
        head.to_owned()
    };
    let line = view.line_at_span(head_token.span()).unwrap_or(1);

    let after_head = trim_layout(view_between(view, body_start, view.range().end()));
    if let Some((when_index, when_token)) = after_head.first_significant()
        && token_is_name(when_token, "when")
        && is_contextual_keyword(after_head, when_index)
    {
        let body = view_between(after_head, when_index + 1, after_head.range().end());
        let (arms, end) = choice_members(
            body,
            context,
            &format!("{display_head}-when body"),
            &format!("layout `{display_head} when` requires at least one arm"),
            true,
            parse_when_arm,
        )?;
        return Ok(ParsedExpression::new(
            SyntaxExpr::MatchWhen(MatchWhenExpr {
                line,
                mode,
                commitment,
                arms,
            }),
            end,
        ));
    }

    let with_index = view
        .top_level()
        .find(|indexed| {
            indexed.index() > head_index
                && is_contextual_keyword(view, indexed.index())
                && token_is_name(indexed.token(), "with")
        })
        .map(|indexed| indexed.index())
        .ok_or_else(|| {
            error_at_token(
                view,
                head_token,
                format!(
                    "{display_head} expression requires `with` and at least one layout arm or an explicit `{{}}` body"
                ),
            )
        })?;
    let subject = trim_layout(view_between(view, body_start, with_index));
    if is_layout_empty(subject) {
        return Err(error_at_token(
            view,
            head_token,
            format!("{display_head} expression requires a subject before `with`"),
        ));
    }
    let subject = parse_expression_in_context(subject, context.child_owner(subject))?;

    let after_with = view_between(view, with_index + 1, view.range().end());
    let (arms, end) = choice_members(
        after_with,
        context,
        &format!("{display_head} body"),
        &format!("layout {display_head} expression requires at least one arm"),
        true,
        parse_match_arm,
    )?;
    Ok(ParsedExpression::new(
        SyntaxExpr::Match(MatchExpr {
            line,
            mode,
            commitment,
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
    let arrows = first
        .top_level()
        .filter_map(|indexed| match indexed.token().kind() {
            TokenKind::Symbol("=>") => Some((indexed.index(), ConditionalResultMode::Ordinary)),
            TokenKind::Symbol("=>?") => Some((indexed.index(), ConditionalResultMode::Tentative)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if arrows.len() > 1 {
        return Err(error_at_view(
            first,
            format!("{label} permits exactly one top-level `=>` or `=>?`"),
        ));
    }
    if let Some((arrow, mode)) = arrows.first().copied() {
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
        return Ok((
            head,
            MatchOutcome::Result {
                line,
                mode,
                expression,
            },
        ));
    }

    let nested_when = top_level_contextual_names(first, "when")
        .into_iter()
        .next_back()
        .ok_or_else(|| {
            error_at_view(
                first,
                format!("{label} requires `=>`, `=>?`, or a trailing `when` child block"),
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
    let (arms, _) = choice_members(
        nested_body,
        nested_context,
        "nested match-when body",
        "nested `when` requires at least one child arm",
        false,
        parse_when_arm,
    )?;
    Ok((head, MatchOutcome::Nested(arms)))
}

fn choice_members<'lex, 'source, T, F>(
    view: TokenView<'lex, 'source>,
    context: ExpressionContext,
    body_label: &str,
    missing_layout_message: &str,
    brace_can_start_member: bool,
    parse_member: F,
) -> ParseResult<(Vec<T>, usize)>
where
    F: Fn(TokenView<'lex, 'source>, ExpressionContext) -> ParseResult<T>,
{
    let Some((body_start, body_token)) = view.first_significant() else {
        return Err(error_at_view(view, missing_layout_message));
    };
    let braced = if let TokenKind::Open {
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
        Some((close, view_between(view, body_start, close + 1)))
    } else {
        None
    };

    let block = LayoutView::new(view).block();
    if block.statements().is_empty() {
        return Err(error_at_view(view, missing_layout_message));
    }
    if !context.accepts_layout_anchor(block.anchor()) {
        let diagnostics = vec![Diagnostic::error(
            block.statements()[0].line(),
            format!(
                "{body_label} begins at indentation {}; expected more than continuation floor {}",
                block.anchor(),
                context.continuation_floor()
            ),
        )];
        return match braced {
            Some((close, body)) => {
                parse_braced_choice_members(body, close, context, body_label, &parse_member)
            }
            None => Err(diagnostics),
        };
    }
    let end = block.end();
    let members = block
        .into_statements()
        .into_iter()
        .map(|statement| {
            view.subview(statement.tokens())
                .expect("layout choice arm remains within its body")
        })
        .collect::<Vec<_>>();

    if !brace_can_start_member && let Some((close, body)) = braced {
        return parse_braced_choice_members(body, close, context, body_label, &parse_member);
    }

    let mut members = members.into_iter();
    let first = members
        .next()
        .expect("a nonempty layout block contains its first member");
    let first = match parse_member(first, context) {
        Ok(first) => first,
        Err(diagnostics) => {
            return match braced {
                Some((close, body)) => {
                    parse_braced_choice_members(body, close, context, body_label, &parse_member)
                }
                None => Err(diagnostics),
            };
        }
    };

    let mut parsed = Vec::with_capacity(members.len() + 1);
    parsed.push(first);
    for member in members {
        parsed.push(parse_member(member, context)?);
    }
    Ok((parsed, end))
}

fn parse_braced_choice_members<'lex, 'source, T, F>(
    body: TokenView<'lex, 'source>,
    close: usize,
    context: ExpressionContext,
    body_label: &str,
    parse_member: &F,
) -> ParseResult<(Vec<T>, usize)>
where
    F: Fn(TokenView<'lex, 'source>, ExpressionContext) -> ParseResult<T>,
{
    let members = split_braced_members(body, body_label)
        .expect("choice body begins with a complete brace group")?;
    let parsed = members
        .into_iter()
        .map(|member| parse_member(member, context))
        .collect::<ParseResult<Vec<_>>>()?;
    Ok((parsed, close + 1))
}

fn top_level_contextual_names(view: TokenView<'_, '_>, expected: &str) -> Vec<usize> {
    view.top_level()
        .filter(|indexed| {
            is_contextual_keyword(view, indexed.index()) && token_is_name(indexed.token(), expected)
        })
        .map(|indexed| indexed.index())
        .collect()
}

fn line_of_view(view: TokenView<'_, '_>) -> usize {
    view.first_significant()
        .and_then(|(_, token)| view.line_at_span(token.span()))
        .unwrap_or(1)
}

fn conditional_boundaries(
    view: TokenView<'_, '_>,
    head_index: usize,
    head: &str,
) -> ParseResult<(ThenBoundary, usize)> {
    let mut phases = vec![IfPhase::Guards];
    let mut outer_then = None;

    for indexed in view.top_level() {
        if indexed.index() <= head_index || !is_contextual_keyword(view, indexed.index()) {
            continue;
        }
        match indexed.token().kind() {
            TokenKind::Name("if" | "try") => phases.push(IfPhase::Guards),
            TokenKind::Name("then") => {
                let Some(phase) = phases.last_mut() else {
                    continue;
                };
                if matches!(phase, IfPhase::Then) {
                    return Err(error_at_token(
                        view,
                        indexed.token(),
                        format!("unexpected `then`; a {head} branch is already active"),
                    ));
                }
                *phase = IfPhase::Then;
                if phases.len() == 1 {
                    let tentative = view.token_at(indexed.index() + 1).is_some_and(|token| {
                        token.leading() == LeadingTrivia::Joint
                            && matches!(token.kind(), TokenKind::Symbol("?"))
                    });
                    outer_then = Some(ThenBoundary {
                        keyword: indexed.index(),
                        result_start: indexed.index() + usize::from(tentative) + 1,
                        mode: if tentative {
                            ConditionalResultMode::Tentative
                        } else {
                            ConditionalResultMode::Ordinary
                        },
                    });
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
                        format!("{head} expression requires `then` before `else`"),
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
        format!("{head} expression requires `else` and a fallback result")
    } else {
        format!("{head} expression requires `then` and `else`")
    };
    Err(error_at_view(view, message))
}

fn conditional_hard_end(view: TokenView<'_, '_>, head_index: usize) -> usize {
    let containing_close = view
        .source()
        .groups()
        .iter()
        .filter_map(|group| {
            let close = group.close_token()?;
            (group.open_token() < head_index && head_index < close)
                .then_some((group.open_token(), close))
        })
        .max_by_key(|(open, _)| *open)
        .map(|(_, close)| close)
        .unwrap_or(view.range().end())
        .min(view.range().end());
    let candidate = view_between(view, head_index, containing_close);
    candidate
        .top_level()
        .find_map(|indexed| {
            (indexed.index() > head_index
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
        assert_eq!(if_expr.mode, ConditionalMode::Pure);
        assert_eq!(if_expr.then_mode, ConditionalResultMode::Ordinary);
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
    fn tentative_result_markers_are_owned_by_their_conditional_branches() {
        let SyntaxExpr::If(if_expr) = parse("if _ then? .r 1 else 2") else {
            panic!("tentative prefix syntax should produce an if expression");
        };
        assert_eq!(if_expr.then_mode, ConditionalResultMode::Tentative);
        assert!(matches!(
            if_expr.then_result.as_ref(),
            SyntaxExpr::Apply(_, _)
        ));

        let SyntaxExpr::Match(subject) = parse("match value with { 1 =>? .fail; _ => 2; }") else {
            panic!("tentative arrow syntax should produce a match expression");
        };
        assert!(matches!(
            subject.arms[0].outcome,
            MatchOutcome::Result {
                mode: ConditionalResultMode::Tentative,
                ..
            }
        ));
        assert!(matches!(
            subject.arms[1].outcome,
            MatchOutcome::Result {
                mode: ConditionalResultMode::Ordinary,
                ..
            }
        ));

        let SyntaxExpr::MatchWhen(nested) =
            parse("match when { _ when { _ =>? .fail; _ => 3; }; }")
        else {
            panic!("nested tentative syntax should preserve its choice tree");
        };
        let MatchOutcome::Nested(children) = &nested.arms[0].outcome else {
            panic!("outer arm should own child alternatives");
        };
        assert!(matches!(
            children[0].outcome,
            MatchOutcome::Result {
                mode: ConditionalResultMode::Tentative,
                ..
            }
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
    fn postfix_if_reorders_its_source_result_behind_the_guards() {
        let SyntaxExpr::If(if_expr) = parse("value if value = 42 else 0") else {
            panic!("postfix syntax should produce the shared if expression");
        };
        assert_eq!(if_expr.mode, ConditionalMode::Pure);
        assert_eq!(if_expr.then_mode, ConditionalResultMode::Ordinary);
        assert!(matches!(
            if_expr.guards.as_slice(),
            [SyntaxGuardClause::ValueBind { pattern, .. }]
                if matches!(&pattern.kind, SyntaxPatternKind::Capture(name) if name == "value")
        ));
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
    fn postfix_if_supports_layout_results_and_structural_success_values() {
        for source in [
            "value if value = 42\n      else 0",
            "value if\n  value = 42\nelse\n  0",
            "(value if value = 42 else 0)",
        ] {
            assert!(
                matches!(parse(source), SyntaxExpr::If(_)),
                "`{source}` should produce a postfix conditional"
            );
        }

        let SyntaxExpr::If(structural) = parse("do\n  .r 1\nif _ else .r 0") else {
            panic!("a completed structural result should resume into postfix `if`");
        };
        assert!(matches!(structural.then_result.as_ref(), SyntaxExpr::Do(_)));
    }

    #[test]
    fn postfix_fallback_may_be_a_right_nested_prefix_if() {
        let SyntaxExpr::If(outer) = parse("1 if _ else if _ then 2 else 3") else {
            panic!("source should produce an outer postfix conditional");
        };
        assert!(matches!(outer.then_result.as_ref(), SyntaxExpr::Number(_)));
        assert!(matches!(outer.else_result.as_ref(), SyntaxExpr::If(_)));
    }

    #[test]
    fn prefix_if_remains_a_structural_application_argument() {
        let SyntaxExpr::Apply(_, argument) = parse("f if _ then 1 else 2") else {
            panic!("a prefix conditional after a function should remain its argument");
        };
        assert!(matches!(argument.as_ref(), SyntaxExpr::If(_)));
    }

    #[test]
    fn malformed_postfix_if_reports_its_missing_guard_or_fallback() {
        for (source, expected) in [
            (
                "value if guard",
                "postfix `if` requires `else` and a fallback result",
            ),
            (
                "value if else fallback",
                "postfix `if` requires guards before `else`",
            ),
            (
                "value if guard else",
                "postfix `if` requires a fallback after `else`",
            ),
        ] {
            let diagnostics =
                super::super::input::parse_expression_fragment(source.as_bytes(), |view| {
                    super::super::structural::parse_expression_in_context(
                        view,
                        ExpressionContext::for_fragment(view),
                    )
                })
                .expect_err("malformed postfix conditional should fail");
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(expected)),
                "`{source}` reported {diagnostics:#?} instead of `{expected}`"
            );
        }
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
    fn malformed_try_variants_report_their_host_construct() {
        for (source, expected) in [
            (
                "try _ else 0",
                "try expression requires `then` before `else`",
            ),
            (
                "try _ then 1",
                "try expression requires `else` and a fallback result",
            ),
            ("try_match value", "try_match expression requires `with`"),
            (
                "try_match value with",
                "layout try_match expression requires at least one arm",
            ),
        ] {
            let diagnostics =
                super::super::input::parse_expression_fragment(source.as_bytes(), |view| {
                    super::super::structural::parse_expression_in_context(
                        view,
                        ExpressionContext::for_fragment(view),
                    )
                })
                .expect_err("malformed host conditional should fail");
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

        let SyntaxExpr::Match(dict_as_pattern) =
            parse("match subject with\n  {x:item} as whole => whole\n  _ => {}")
        else {
            panic!("a complete dictionary `as` pattern should establish layout");
        };
        assert_eq!(dict_as_pattern.arms.len(), 2);
        assert!(matches!(
            dict_as_pattern.arms[0].pattern.kind,
            SyntaxPatternKind::As(_, _)
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
    fn match_subject_with_expression_requires_explicit_grouping() {
        let SyntaxExpr::Match(grouped) =
            parse("match (base with { x := 1 }) with { {x:item} => item; _ => 0; }")
        else {
            panic!("a grouped `with` expression should remain the match subject");
        };
        assert!(matches!(grouped.subject.as_ref(), SyntaxExpr::With { .. }));

        let diagnostics = super::super::input::parse_expression_fragment(
            b"match base with { x := 1 } with { {x:item} => item; _ => 0; }",
            |view| {
                super::super::structural::parse_expression_in_context(
                    view,
                    ExpressionContext::for_fragment(view),
                )
            },
        )
        .expect_err("an ungrouped first `with` must belong to the match");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("match arm")),
            "ungrouped subject update reported {diagnostics:#?}"
        );
    }

    #[test]
    fn try_variants_reuse_conditional_shapes_in_host_mode() {
        let SyntaxExpr::If(try_expr) = parse("try value = 42 and _ then value else 0") else {
            panic!("try syntax should produce the shared if expression");
        };
        assert_eq!(try_expr.mode, ConditionalMode::Host);
        assert_eq!(try_expr.guards.len(), 2);

        let SyntaxExpr::Match(subject) =
            parse("try_match value with { 42 => \"yes\"; _ => \"no\"; }")
        else {
            panic!("try_match subject syntax should produce the shared match expression");
        };
        assert_eq!(subject.mode, ConditionalMode::Host);
        assert_eq!(subject.commitment, MatchCommitment::Cut);
        assert_eq!(subject.arms.len(), 2);

        let SyntaxExpr::MatchWhen(guard_only) =
            parse("try_match when { _ when { _ => \"nested\"; }; }")
        else {
            panic!("try_match when should produce the shared guard-only match expression");
        };
        assert_eq!(guard_only.mode, ConditionalMode::Host);
        assert_eq!(guard_only.commitment, MatchCommitment::Cut);
        assert!(matches!(
            guard_only.arms[0].outcome,
            MatchOutcome::Nested(_)
        ));

        let SyntaxExpr::MatchWhen(empty) = parse("try_match when {}") else {
            panic!("explicit empty try_match when should remain a host search");
        };
        assert_eq!(empty.mode, ConditionalMode::Host);
        assert_eq!(empty.commitment, MatchCommitment::Cut);
        assert!(empty.arms.is_empty());
    }

    #[test]
    fn starred_match_forms_are_joint_open_choices() {
        let SyntaxExpr::Match(subject) = parse("match* value with { 42 => \"yes\"; _ => \"no\"; }")
        else {
            panic!("match* subject syntax should produce a match expression");
        };
        assert_eq!(subject.mode, ConditionalMode::Pure);
        assert_eq!(subject.commitment, MatchCommitment::Open);
        assert_eq!(subject.arms.len(), 2);

        let SyntaxExpr::MatchWhen(guard_only) =
            parse("try_match* when { _ when { _ => \"nested\"; }; }")
        else {
            panic!("try_match* when should produce a match-when expression");
        };
        assert_eq!(guard_only.mode, ConditionalMode::Host);
        assert_eq!(guard_only.commitment, MatchCommitment::Open);
        assert!(matches!(
            guard_only.arms[0].outcome,
            MatchOutcome::Nested(_)
        ));

        assert!(matches!(
            parse("(try_match* value with { _ => value; })"),
            SyntaxExpr::Match(MatchExpr {
                mode: ConditionalMode::Host,
                commitment: MatchCommitment::Open,
                ..
            })
        ));

        let diagnostics = super::super::input::parse_expression_fragment(
            b"match * value with { _ => 1; }",
            |view| {
                super::super::structural::parse_expression_in_context(
                    view,
                    ExpressionContext::for_fragment(view),
                )
            },
        )
        .expect_err("whitespace must not separate a starred match head");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("open form is spelled `match*` without intervening whitespace")
        }));
    }

    #[test]
    fn nested_if_and_try_boundaries_remain_owned_by_their_heads() {
        let SyntaxExpr::If(outer_try) = parse("try _ then if _ then 1 else 2 else 3") else {
            panic!("source should produce an outer try expression");
        };
        assert_eq!(outer_try.mode, ConditionalMode::Host);
        assert!(matches!(
            outer_try.then_result.as_ref(),
            SyntaxExpr::If(IfExpr {
                mode: ConditionalMode::Pure,
                ..
            })
        ));

        let SyntaxExpr::If(outer_if) = parse("if _ then try _ then 1 else 2 else 3") else {
            panic!("source should produce an outer if expression");
        };
        assert_eq!(outer_if.mode, ConditionalMode::Pure);
        assert!(matches!(
            outer_if.then_result.as_ref(),
            SyntaxExpr::If(IfExpr {
                mode: ConditionalMode::Host,
                ..
            })
        ));
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
    fn complete_match_patterns_accept_unparenthesized_views_and_predicates() {
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

        let SyntaxExpr::Match(predicate) =
            parse("match subject with { equals 42 value => value; }")
        else {
            panic!("source should produce a match expression");
        };
        assert!(matches!(
            predicate.arms[0].pattern.kind,
            SyntaxPatternKind::Predicate { .. }
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
