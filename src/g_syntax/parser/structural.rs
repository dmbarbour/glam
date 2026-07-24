//! Structural expression parsing for `let`, `where`, objects, and `with`.
//!
//! `let`, `where`, object, and `with` expressions produce complete syntax
//! trees. Object bodies share the recursive declaration parser used by
//! top-level object declarations.

use super::super::keywords::{canonical_keyword, reserved_keyword_message};
use super::super::{Diagnostic, ObjectExpr, Severity, SyntaxExpr};
use super::declaration::{parse_nonempty_object_body, parse_object_body};
use super::expression::{parse_expression_chain_view, syntax_operator};
use super::expression_context::{ExpressionContext, ParsedExpression, validate_expression_floor};
use super::input::{TokenRange, TokenView};
use super::layout::{LayoutBase, LayoutView};
use super::lexical::{Delimiter, LeadingTrivia, SpannedToken, TokenKind};

type ParseResult<T> = Result<T, Vec<Diagnostic>>;

#[derive(Debug, PartialEq, Eq)]
struct ObjectHeader {
    name: Option<Box<SyntaxExpr>>,
    alias: Option<String>,
    deps: Vec<SyntaxExpr>,
    has_with: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct WithHeader {
    base: Box<SyntaxExpr>,
    alias: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StructuralBody {
    Braced { end: usize },
    Layout { start: usize },
}

#[cfg(test)]
pub(in crate::g_syntax::parser) fn parse_compound_expression_fragment(
    source: &[u8],
) -> ParseResult<SyntaxExpr> {
    super::input::parse_expression_fragment(source, |view| {
        parse_expression_in_context(view, ExpressionContext::for_fragment(view))
    })
}

pub(in crate::g_syntax::parser) fn parse_expression_in_context(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> ParseResult<SyntaxExpr> {
    let view = trim_layout(view);
    let parsed = parse_expression_extent(view, context.complete())?;
    debug_assert_eq!(parsed.end(), view.range().end());
    parsed
        .into_expression()
        .map_err(|message| error_at_view(view, message))
}

pub(in crate::g_syntax::parser) fn parse_expression_extent(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> ParseResult<ParsedExpression> {
    let view = trim_layout(view);
    let (context, floor_diagnostics) = validate_expression_floor(view, context);
    if !floor_diagnostics.is_empty() {
        return Err(floor_diagnostics);
    }
    let parsed = if let Some(result) = parse_parenthesized_structural(view, context) {
        ParsedExpression::new(result?, view.range().end())
    } else if let Some(result) = parse_let(view, context) {
        ParsedExpression::new(result?, view.range().end())
    } else if let Some(result) = parse_object(view, context) {
        result?
    } else if let Some(result) = parse_with(view, context) {
        result?
    } else if let Some(result) = parse_where(view, context) {
        result?
    } else {
        ParsedExpression::from_chain(
            parse_expression_chain_view(view, context)?,
            view.range().end(),
        )
    };
    let parsed = resume_expression_suffixes(view, context, parsed)?;
    if parsed.end() < view.range().end() && !context.permits_yield() {
        let tail = trim_layout(view_between(view, parsed.end(), view.range().end()));
        let found = tail
            .source_text()
            .and_then(|source| source.split_whitespace().next())
            .unwrap_or("unrecognized input");
        return Err(error_at_view(
            tail,
            format!(
                "expression cannot resume with `{found}` after the preceding layout body ended"
            ),
        ));
    }
    Ok(parsed)
}

fn parse_parenthesized_structural(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Option<ParseResult<SyntaxExpr>> {
    let (open_index, open_token) = view.first_significant()?;
    let (close_index, _) = view.last_significant()?;
    let TokenKind::Open {
        group,
        delimiter: super::lexical::Delimiter::Parenthesis,
    } = open_token.kind()
    else {
        return None;
    };
    let delimiter_group = view.group(*group)?;
    if open_index != delimiter_group.open_token()
        || delimiter_group.close_token() != Some(close_index)
    {
        return None;
    }

    let contents = trim_layout(view.group_contents(*group)?);
    let context = context.complete();
    let starts_structural = contents
        .first_significant()
        .is_some_and(|(_, token)| token_is_name(token, "let") || token_is_name(token, "object"))
        || !contextual_keywords(contents, "where").is_empty()
        || has_compound_with_body(contents);
    starts_structural.then(|| {
        parse_expression_extent(contents, context).and_then(|parsed| {
            parsed
                .into_expression()
                .map_err(|message| error_at_view(contents, message))
        })
    })
}

fn parse_let(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Option<ParseResult<SyntaxExpr>> {
    let (let_index, let_token) = view.first_significant()?;
    if !token_is_name(let_token, "let") {
        return None;
    }
    let next = next_significant_after(view, let_index)?;
    if next.token().leading() == LeadingTrivia::Joint {
        return None;
    }

    let rest = trim_layout(view_between(view, let_index + 1, view.range().end()));
    if is_layout_empty(rest) {
        return Some(Err(error_at_token(
            view,
            let_token,
            "let expression requires bindings and a body",
        )));
    }

    let in_index = contextual_keywords(rest, "in").into_iter().next();
    let (bindings, braced_bindings, body) = if let Some(in_index) = in_index {
        let bindings_view = trim_layout(view_between(rest, rest.range().start(), in_index));
        if bindings_view
            .top_level()
            .any(|indexed| matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
        {
            return Some(Err(error_at_token(
                view,
                let_token,
                "multi-line let expression must not use `in`",
            )));
        }
        let body = trim_layout(view_between(rest, in_index + 1, rest.range().end()));
        if let Some(bindings) = parse_braced_bindings(bindings_view, "let", context) {
            (bindings, true, body)
        } else if !top_level_symbols(bindings_view, ";").is_empty() {
            (
                Err(error_at_view(
                    bindings_view,
                    "naked semicolon-separated `let` bindings are not supported; use `let { ... } in ...`",
                )),
                false,
                body,
            )
        } else {
            (
                parse_binding_views(vec![bindings_view], context),
                false,
                body,
            )
        }
    } else {
        match split_multiline_let(view, let_index, rest) {
            Ok((bindings, body)) => (parse_binding_views(bindings, context), false, body),
            Err(diagnostics) => return Some(Err(diagnostics)),
        }
    };

    let bindings = match bindings {
        Ok(bindings) => bindings,
        Err(diagnostics) => return Some(Err(diagnostics)),
    };
    if bindings.is_empty() && !braced_bindings {
        return Some(Err(error_at_token(
            view,
            let_token,
            "let expression requires at least one binding",
        )));
    }
    if is_layout_empty(body) {
        return Some(Err(error_at_token(
            view,
            let_token,
            "let expression requires a body",
        )));
    }
    let body = match parse_expression_in_context(body, context) {
        Ok(body) => body,
        Err(diagnostics) => return Some(Err(diagnostics)),
    };

    Some(Ok(SyntaxExpr::Let {
        bindings,
        body: Box::new(body),
    }))
}

fn parse_where(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Option<ParseResult<ParsedExpression>> {
    let where_index = contextual_keywords(view, "where").into_iter().last()?;
    let where_token = view.token_at(where_index)?;
    let body = trim_layout(view_between(view, view.range().start(), where_index));
    if is_layout_empty(body) {
        return Some(Err(error_at_token(
            view,
            where_token,
            "where expression requires a body",
        )));
    }
    let body = match parse_expression_in_context(body, context) {
        Ok(body) => body,
        Err(diagnostics) => return Some(Err(diagnostics)),
    };
    Some(parse_where_suffix(view, context, where_index, body))
}

fn parse_where_suffix(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
    where_index: usize,
    body: SyntaxExpr,
) -> ParseResult<ParsedExpression> {
    let where_token = view
        .token_at(where_index)
        .expect("a selected where suffix remains within its expression view");
    let suffix_end = contextual_keywords(view, "where")
        .into_iter()
        .find(|candidate| *candidate > where_index)
        .unwrap_or(view.range().end());
    let all_bindings = trim_layout(view_between(view, where_index + 1, suffix_end));
    if is_layout_empty(all_bindings) {
        return Err(error_at_token(
            view,
            where_token,
            "where expression requires at least one binding or an explicit `{}` group",
        ));
    }

    let (bindings_view, end) = if braced_contents(all_bindings).is_some() {
        (all_bindings, all_bindings.range().end())
    } else if all_bindings
        .top_level()
        .any(|indexed| matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
    {
        let layout = LayoutView::new(all_bindings);
        let block = layout
            .block(layout.inferred_base())
            .map_err(|error| vec![Diagnostic::error(error.line(), error.message())])?;
        if !block.statements().is_empty() && !context.accepts_layout_anchor(block.anchor()) {
            return Err(vec![Diagnostic::error(
                block.statements()[0].line(),
                format!(
                    "where binding layout begins at indentation {}; expected more than continuation floor {}",
                    block.anchor(),
                    context.continuation_floor()
                ),
            )]);
        }
        if let Some(boundary) = block.boundary()
            && all_bindings
                .subview(boundary.tokens())
                .is_some_and(|line| !top_level_symbols(line, "=").is_empty())
        {
            return Err(vec![Diagnostic::error(
                boundary.line(),
                format!(
                    "multi-line where binding is indented {} spaces; expected sibling indentation {}",
                    boundary.indentation(),
                    block.anchor()
                ),
            )]);
        }
        (
            trim_layout(view_between(
                all_bindings,
                all_bindings.range().start(),
                block.end(),
            )),
            block.end(),
        )
    } else {
        (all_bindings, all_bindings.range().end())
    };

    let (bindings, braced_bindings) = if let Some(bindings) =
        parse_braced_bindings(bindings_view, "where", context)
    {
        (bindings, true)
    } else if bindings_view
        .top_level()
        .any(|indexed| matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
    {
        let layout = LayoutView::new(bindings_view);
        let statements = match layout.statements(layout.inferred_base()) {
            Ok(statements) => statements,
            Err(error) => {
                return Err(vec![Diagnostic::error(error.line(), error.message())]);
            }
        };
        let binding_views = statements
            .into_iter()
            .filter_map(|statement| bindings_view.subview(statement.tokens()))
            .collect();
        (parse_binding_views(binding_views, context), false)
    } else if !top_level_symbols(bindings_view, ";").is_empty() {
        (
            Err(error_at_view(
                bindings_view,
                "naked semicolon-separated `where` bindings are not supported; use `where { ... }`",
            )),
            false,
        )
    } else {
        (parse_binding_views(vec![bindings_view], context), false)
    };
    let bindings = bindings?;
    if bindings.is_empty() && !braced_bindings {
        return Err(error_at_token(
            view,
            where_token,
            "where expression requires at least one binding",
        ));
    }

    Ok(ParsedExpression::new(
        SyntaxExpr::Let {
            bindings,
            body: Box::new(body),
        },
        end,
    ))
}

fn resume_expression_suffixes(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
    mut parsed: ParsedExpression,
) -> ParseResult<ParsedExpression> {
    while parsed.end() < view.range().end() {
        let tail = trim_layout(view_between(view, parsed.end(), view.range().end()));
        let Some((boundary_index, token)) = tail.first_significant() else {
            break;
        };
        if token_is_name(token, "where")
            && (token.leading() != LeadingTrivia::Joint
                || begins_layout_line(view, boundary_index, token))
        {
            let body = parsed
                .into_expression()
                .map_err(|message| error_at_view(tail, message))?;
            parsed = parse_where_suffix(view, context, boundary_index, body)?;
            continue;
        }

        let Some(operator) = syntax_operator(token) else {
            break;
        };
        if token.leading() == LeadingTrivia::Joint
            && !begins_layout_line(view, boundary_index, token)
        {
            break;
        }
        let indentation = begins_layout_line(view, boundary_index, token)
            .then(|| view.line_indentation_at(boundary_index).unwrap_or(0));
        let operand_start = boundary_index + 1;
        let operand_tail = view_between(view, operand_start, view.range().end());
        let operand_end = next_resumption_boundary(operand_tail, indentation)
            .map_or(view.range().end(), |line| line.start());
        let operand_view = trim_layout(view_between(view, operand_start, operand_end));
        if is_layout_empty(operand_view) {
            return Err(error_at_token(
                view,
                token,
                "infix operator requires a right operand",
            ));
        }
        let operand_context = indentation
            .map_or(context, |indentation| {
                context.with_continuation_floor(indentation)
            })
            .may_yield();
        let right = parse_expression_extent(operand_view, operand_context)?;
        if right.end() != operand_view.range().end() {
            let unconsumed =
                trim_layout(view_between(view, right.end(), operand_view.range().end()));
            return Err(error_at_view(
                unconsumed,
                "right operand ended before an unrecognized layout boundary",
            ));
        }
        let mut chain = parsed.into_chain();
        chain
            .append(operator, right.into_chain(), indentation, context)
            .map_err(|message| error_at_token(view, token, message))?;
        parsed = ParsedExpression::from_chain(chain, operand_end);
    }
    Ok(parsed)
}

fn begins_layout_line(
    view: TokenView<'_, '_>,
    token_index: usize,
    token: &SpannedToken<'_>,
) -> bool {
    token.leading() == LeadingTrivia::LineBreak
        || token_index.checked_sub(1).is_some_and(|previous| {
            matches!(
                view.token_at(previous).map(SpannedToken::kind),
                Some(TokenKind::LineStart { .. })
            )
        })
}

fn next_resumption_boundary(
    view: TokenView<'_, '_>,
    current_anchor: Option<usize>,
) -> Option<super::layout::LayoutLine> {
    LayoutView::new(view).lines().into_iter().find(|line| {
        if current_anchor.is_some_and(|anchor| line.indentation() > anchor) {
            return false;
        }
        view.subview(line.tokens())
            .and_then(TokenView::first_significant)
            .is_some_and(|(_, token)| {
                syntax_operator(token).is_some() || token_is_name(token, "where")
            })
    })
}

fn parse_braced_bindings(
    view: TokenView<'_, '_>,
    construct: &str,
    context: ExpressionContext,
) -> Option<ParseResult<Vec<(String, SyntaxExpr)>>> {
    split_braced_members(view, &format!("`{construct}` binding group"))
        .map(|members| members.and_then(|members| parse_binding_views(members, context)))
}

fn parse_binding_views(
    views: Vec<TokenView<'_, '_>>,
    context: ExpressionContext,
) -> ParseResult<Vec<(String, SyntaxExpr)>> {
    views
        .into_iter()
        .map(trim_layout)
        .filter(|view| !is_layout_empty(*view))
        .map(|view| parse_binding(view, context))
        .collect()
}

fn parse_binding(
    view: TokenView<'_, '_>,
    parent_context: ExpressionContext,
) -> ParseResult<(String, SyntaxExpr)> {
    let context = parent_context.child_owner(view);
    let Some(equal_index) = top_level_symbols(view, "=").into_iter().next() else {
        return Err(error_at_view(
            view,
            format!(
                "local binding `{}` must use `=`",
                view.source_text().unwrap_or("").trim()
            ),
        ));
    };
    let name_view = trim_layout(view_between(view, view.range().start(), equal_index));
    let value_view = trim_layout(view_between(view, equal_index + 1, view.range().end()));
    let Some(name) = local_name(name_view) else {
        if let Some(keyword) = single_reserved_keyword(name_view) {
            return Err(error_at_view(name_view, reserved_keyword_message(keyword)));
        }
        return Err(error_at_view(
            name_view,
            format!(
                "invalid local binding name `{}`",
                name_view.source_text().unwrap_or("").trim()
            ),
        ));
    };
    if is_layout_empty(value_view) {
        return Err(error_at_view(
            view,
            format!("local binding `{name}` requires a value"),
        ));
    }
    parse_expression_in_context(value_view, context).map(|value| (name.to_owned(), value))
}

fn parse_object(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Option<ParseResult<ParsedExpression>> {
    let (_, head) = view.first_significant()?;
    if !token_is_name(head, "object") {
        return None;
    }
    if !contextual_keywords(view, "where").is_empty() && !has_compound_with_body(view) {
        return None;
    }
    let (owned_view, end) = match structural_body_extent(view, context) {
        Ok(owned) => owned,
        Err(diagnostics) => return Some(Err(diagnostics)),
    };
    let object_line = view
        .first_significant()
        .and_then(|(_, token)| view.line_at_span(token.span()))
        .unwrap_or(1);
    let (_, body_view) = split_compound_header_body(owned_view);
    let header = match parse_object_header(owned_view, context)? {
        Ok(header) => header,
        Err(diagnostics) => return Some(Err(diagnostics)),
    };
    let mut diagnostics = Vec::new();
    let body = if header.has_with {
        let Some(body_view) = body_view else {
            return Some(Err(vec![Diagnostic::error(
                object_line,
                "object `with` requires a body; use `with {}` for an explicit empty body",
            )]));
        };
        if braced_contents(body_view).is_some() {
            parse_object_body(body_view, context, &mut diagnostics)
        } else {
            parse_nonempty_object_body(
                body_view,
                object_line,
                "object `with` body cannot be empty; use `with {}` for an explicit empty body",
                context,
                &mut diagnostics,
            )
            .unwrap_or_default()
        }
    } else {
        Vec::new()
    };
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return Some(Err(diagnostics));
    }
    Some(Ok(ParsedExpression::new(
        SyntaxExpr::Object(ObjectExpr {
            name: header.name,
            alias: header.alias,
            deps: header.deps,
            body,
        }),
        end,
    )))
}

fn parse_with(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Option<ParseResult<ParsedExpression>> {
    if !has_compound_with_body(view) {
        return None;
    }
    let (owned_view, end) = match structural_body_extent(view, context) {
        Ok(owned) => owned,
        Err(diagnostics) => return Some(Err(diagnostics)),
    };
    let (_, body_view) = split_compound_header_body(owned_view);
    let body_view = body_view?;
    let header = match parse_with_header(owned_view, context)? {
        Ok(header) => header,
        Err(diagnostics) => return Some(Err(diagnostics)),
    };
    let mut diagnostics = Vec::new();
    let body = if braced_contents(body_view).is_some() {
        parse_object_body(body_view, context, &mut diagnostics)
    } else {
        let line = owned_view
            .first_significant()
            .and_then(|(_, token)| owned_view.line_at_span(token.span()))
            .unwrap_or(1);
        parse_nonempty_object_body(
            body_view,
            line,
            "`with` body cannot be empty; use `with {}` for an explicit empty body",
            context,
            &mut diagnostics,
        )
        .unwrap_or_default()
    };
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return Some(Err(diagnostics));
    }
    let expression = SyntaxExpr::With {
        base: header.base,
        alias: header.alias,
        body,
    };
    let (header_view, _) = split_compound_header_body(owned_view);
    let chain = match leading_operator_anchor(header_view, context) {
        Ok(Some(anchor)) => match super::expression::InfixChain::single(expression)
            .with_resumption_anchor(anchor, context)
        {
            Ok(chain) => chain,
            Err(message) => return Some(Err(error_at_view(header_view, message))),
        },
        Ok(None) => super::expression::InfixChain::single(expression),
        Err(diagnostics) => return Some(Err(diagnostics)),
    };
    Some(Ok(ParsedExpression::from_chain(chain, end)))
}

/// Selects the exact structural prefix owned by an object or `with`
/// expression. Layout bodies yield their first dedented line; braced bodies
/// end at their matching delimiter.
fn structural_body_extent<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    context: ExpressionContext,
) -> ParseResult<(TokenView<'lex, 'source>, usize)> {
    match find_structural_body(view) {
        Some(StructuralBody::Braced { end }) => {
            Ok((view_between(view, view.range().start(), end), end))
        }
        Some(StructuralBody::Layout { start }) => {
            let header = trim_layout(view_between(view, view.range().start(), start));
            let body_context = match leading_operator_anchor(header, context)? {
                Some(anchor) => context.with_continuation_floor(anchor),
                None => context,
            };
            let body = view_between(view, start, view.range().end());
            let layout = LayoutView::new(body);
            let base = layout.inferred_base();
            let block = layout
                .block(base)
                .map_err(|error| vec![Diagnostic::error(error.line(), error.message())])?;
            if !block.statements().is_empty() && !body_context.accepts_layout_anchor(block.anchor())
            {
                let line = block.statements()[0].line();
                return Err(vec![Diagnostic::error(
                    line,
                    format!(
                        "nested layout begins at indentation {}; expected more than continuation floor {}",
                        block.anchor(),
                        body_context.continuation_floor()
                    ),
                )]);
            }
            if matches!(base, LayoutBase::Hanging(_))
                && let Some(boundary) = block.boundary()
                && body
                    .subview(boundary.tokens())
                    .is_some_and(line_begins_object_member)
            {
                return Err(vec![Diagnostic::error(
                    boundary.line(),
                    format!(
                        "hanging `with` member is indented {} spaces; expected sibling indentation {}",
                        boundary.indentation(),
                        block.anchor()
                    ),
                )]);
            }
            let end = block.end();
            Ok((view_between(view, view.range().start(), end), end))
        }
        None => Ok((view, view.range().end())),
    }
}

fn leading_operator_anchor(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> ParseResult<Option<usize>> {
    let mut anchor = None;
    for line in LayoutView::new(view).lines() {
        let Some((_, token)) = view
            .subview(line.tokens())
            .and_then(TokenView::first_significant)
        else {
            continue;
        };
        if syntax_operator(token).is_none() {
            continue;
        }
        if !context.accepts_layout_anchor(line.indentation()) {
            return Err(vec![Diagnostic::error(
                line.line(),
                format!(
                    "leading infix operator is indented {} spaces; expected more than continuation floor {}",
                    line.indentation(),
                    context.continuation_floor()
                ),
            )]);
        }
        if let Some(expected) = anchor
            && line.indentation() != expected
        {
            return Err(vec![Diagnostic::error(
                line.line(),
                format!(
                    "leading infix operators must align at indentation {expected}; found indentation {}",
                    line.indentation()
                ),
            )]);
        }
        anchor = Some(line.indentation());
    }
    Ok(anchor)
}

fn has_compound_with_body(view: TokenView<'_, '_>) -> bool {
    find_structural_body(view).is_some()
}

fn line_begins_object_member(view: TokenView<'_, '_>) -> bool {
    view.first_significant()
        .is_some_and(|(_, token)| token_is_name(token, "object") || token_is_name(token, "extend"))
        || ["=", ":=", "::="]
            .into_iter()
            .any(|operator| !top_level_symbols(view, operator).is_empty())
}

fn find_structural_body(view: TokenView<'_, '_>) -> Option<StructuralBody> {
    let where_boundary = contextual_keywords(view, "where")
        .into_iter()
        .next()
        .unwrap_or(view.range().end());
    for indexed in view.top_level() {
        if indexed.index() >= where_boundary {
            break;
        }
        let TokenKind::Open {
            group,
            delimiter: Delimiter::Brace,
        } = indexed.token().kind()
        else {
            continue;
        };
        let header = trim_layout(view_between(view, view.range().start(), indexed.index()));
        if !last_significant_is_contextual_name(header, "with") {
            continue;
        }
        let close = view.group(*group).and_then(|group| group.close_token())?;
        return Some(StructuralBody::Braced { end: close + 1 });
    }

    for with_index in contextual_keywords(view, "with") {
        if with_index >= where_boundary {
            break;
        }
        let with_token = view.token_at(with_index)?;
        let Some(next) = next_significant_after(view, with_index) else {
            continue;
        };
        if next.index() >= where_boundary {
            continue;
        }
        if view.line_at_span(with_token.span()) == view.line_at_span(next.token().span()) {
            return Some(StructuralBody::Layout {
                start: next.index(),
            });
        }
    }

    let lines = LayoutView::new(view).lines();
    for pair in lines.windows(2) {
        if pair[0].tokens().start() >= where_boundary {
            break;
        }
        let header_line = view.subview(pair[0].tokens())?;
        if last_significant_is_contextual_name(header_line, "with") {
            return Some(StructuralBody::Layout {
                start: pair[1].start(),
            });
        }
    }

    None
}

fn split_multiline_let<'lex, 'source>(
    full: TokenView<'lex, 'source>,
    let_index: usize,
    rest: TokenView<'lex, 'source>,
) -> ParseResult<(Vec<TokenView<'lex, 'source>>, TokenView<'lex, 'source>)> {
    let lines = LayoutView::new(rest).lines();
    let Some(first) = lines.first().copied() else {
        return Err(error_at_view(
            rest,
            "multi-line let expression requires a body or `in`",
        ));
    };
    let Some((_, first_binding_token)) = rest
        .subview(first.tokens())
        .and_then(TokenView::first_significant)
    else {
        return Err(error_at_view(
            rest,
            "let expression requires at least one binding",
        ));
    };
    let let_token = full
        .token_at(let_index)
        .expect("the let token remains within the full expression view");
    let let_column = full
        .column_at_span(let_token.span())
        .expect("the let token has a source column");
    let binding_column = full
        .column_at_span(first_binding_token.span())
        .expect("the first let binding has a source column");
    let first_is_inline =
        full.line_at_span(let_token.span()) == full.line_at_span(first_binding_token.span());
    let base = if first_is_inline {
        LayoutBase::Hanging(binding_column)
    } else {
        LayoutBase::FirstLine
    };
    let block = LayoutView::new(rest)
        .block(base)
        .map_err(|error| vec![Diagnostic::error(error.line(), error.message())])?;

    for line in lines.into_iter().skip(1) {
        if line.start() >= block.end() {
            break;
        }
        let line_view = rest
            .subview(line.tokens())
            .expect("layout lines remain within their source view");
        let begins_binding = !top_level_symbols(line_view, "=").is_empty();
        if begins_binding && line.indentation() != block.anchor() {
            return Err(vec![Diagnostic::error(
                line.line(),
                format!(
                    "multi-line let binding is indented {} spaces; expected sibling indentation {}",
                    line.indentation(),
                    block.anchor()
                ),
            )]);
        }
    }

    let Some(boundary) = block.boundary() else {
        return Err(error_at_view(
            rest,
            "multi-line let expression requires a body",
        ));
    };
    let boundary_view = rest
        .subview(boundary.tokens())
        .expect("a let layout boundary remains within the source view");
    if !top_level_symbols(boundary_view, "=").is_empty() {
        return Err(vec![Diagnostic::error(
            boundary.line(),
            format!(
                "multi-line let binding is indented {} spaces; expected sibling indentation {}",
                boundary.indentation(),
                block.anchor()
            ),
        )]);
    }
    if boundary.indentation() != let_column {
        return Err(vec![Diagnostic::error(
            boundary.line(),
            format!(
                "multi-line let body is indented {} spaces; expected alignment with `let` at indentation {}",
                boundary.indentation(),
                let_column
            ),
        )]);
    }
    let bindings = block
        .into_statements()
        .into_iter()
        .filter_map(|statement| rest.subview(statement.tokens()))
        .map(trim_layout)
        .collect();
    let body = trim_layout(view_between(
        rest,
        boundary.tokens().start(),
        rest.range().end(),
    ));
    Ok((bindings, body))
}

fn parse_object_header(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Option<ParseResult<ObjectHeader>> {
    let (header_view, body) = split_compound_header_body(view);
    let (object_index, object_token) = header_view.first_significant()?;
    if !token_is_name(object_token, "object") {
        return None;
    }
    let body_present = body.is_some();
    let mut header = trim_layout(view_between(
        header_view,
        object_index + 1,
        header_view.range().end(),
    ));
    if is_layout_empty(header) {
        return Some(Err(error_at_token(
            view,
            object_token,
            "object expression requires a name expression or `_`",
        )));
    }

    let has_with = last_significant_is_contextual_name(header, "with");
    if has_with {
        let with_index = header
            .last_significant()
            .map(|(index, _)| index)
            .expect("nonempty header has a last token");
        header = trim_layout(view_between(header, header.range().start(), with_index));
    } else if body_present {
        return Some(Err(error_at_token(
            view,
            object_token,
            "object expression body requires `with` in the expression header",
        )));
    }

    let as_index = contextual_keywords(header, "as").into_iter().next();
    let extends_index = contextual_keywords(header, "extends").into_iter().next();
    let name_end = [as_index, extends_index]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(header.range().end());
    let name_view = trim_layout(view_between(header, header.range().start(), name_end));
    let name = if view_is_single_name(name_view, "_") {
        None
    } else {
        match parse_expression_in_context(name_view, context.complete()) {
            Ok(name) => Some(Box::new(name)),
            Err(diagnostics) => return Some(Err(diagnostics)),
        }
    };

    let alias = if let Some(as_index) = as_index {
        let alias_end = extends_index.unwrap_or(header.range().end());
        let alias_view = trim_layout(view_between(header, as_index + 1, alias_end));
        match object_alias_name(alias_view) {
            Some(alias) => Some(alias.to_owned()),
            None => {
                if let Some(keyword) = single_reserved_keyword(alias_view) {
                    return Some(Err(error_at_view(
                        alias_view,
                        reserved_keyword_message(keyword),
                    )));
                }
                return Some(Err(error_at_view(
                    alias_view,
                    "`as` requires a valid object alias name",
                )));
            }
        }
    } else {
        None
    };
    if alias.is_some() && !has_with {
        return Some(Err(error_at_view(
            header,
            "object expression `as` requires a `with` body",
        )));
    }

    let deps = if let Some(extends_index) = extends_index {
        if as_index.is_some_and(|as_index| as_index > extends_index) {
            return Some(Err(error_at_view(
                header,
                "object expression `as` must precede `extends`",
            )));
        }
        let deps_view = trim_layout(view_between(
            header,
            extends_index + 1,
            header.range().end(),
        ));
        match parse_object_parents(
            deps_view,
            view.line_at_span(object_token.span()).unwrap_or(1),
            context.complete(),
        ) {
            Ok(deps) => deps,
            Err(diagnostics) => return Some(Err(diagnostics)),
        }
    } else {
        Vec::new()
    };

    Some(Ok(ObjectHeader {
        name,
        alias,
        deps,
        has_with,
    }))
}

fn parse_with_header(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Option<ParseResult<WithHeader>> {
    let (header, body) = split_compound_header_body(view);
    body?;
    let header = trim_layout(header);
    if !last_significant_is_contextual_name(header, "with") {
        return None;
    }
    let with_index = header
        .last_significant()
        .map(|(index, _)| index)
        .expect("with header has a final token");
    let base_and_alias = trim_layout(view_between(header, header.range().start(), with_index));
    let as_index = contextual_keywords(base_and_alias, "as").into_iter().last();
    let (base_view, alias) = if let Some(as_index) = as_index {
        let alias_view = trim_layout(view_between(
            base_and_alias,
            as_index + 1,
            base_and_alias.range().end(),
        ));
        if view_is_single_name(alias_view, "_") {
            (
                trim_layout(view_between(
                    base_and_alias,
                    base_and_alias.range().start(),
                    as_index,
                )),
                None,
            )
        } else if let Some(alias) = object_alias_name(alias_view) {
            (
                trim_layout(view_between(
                    base_and_alias,
                    base_and_alias.range().start(),
                    as_index,
                )),
                Some(alias.to_owned()),
            )
        } else if let Some(keyword) = single_reserved_keyword(alias_view) {
            return Some(Err(error_at_view(
                alias_view,
                reserved_keyword_message(keyword),
            )));
        } else {
            (base_and_alias, None)
        }
    } else {
        (base_and_alias, None)
    };
    if is_layout_empty(base_view) {
        return Some(Err(error_at_view(
            header,
            "with expression requires a base expression",
        )));
    }
    let base = match parse_expression_in_context(base_view, context.complete()) {
        Ok(base) => Box::new(base),
        Err(diagnostics) => return Some(Err(diagnostics)),
    };
    Some(Ok(WithHeader { base, alias }))
}

pub(in crate::g_syntax::parser) fn contextual_keywords(
    view: TokenView<'_, '_>,
    expected: &str,
) -> Vec<usize> {
    view.top_level()
        .filter(|indexed| {
            (indexed.index() == view.range().start()
                || indexed.token().leading() != LeadingTrivia::Joint
                || indexed.index().checked_sub(1).is_some_and(|previous| {
                    view.token_at(previous)
                        .is_some_and(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
                }))
                && token_is_name(indexed.token(), expected)
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

pub(in crate::g_syntax::parser) fn split_top_level<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    separator: &str,
) -> Vec<TokenView<'lex, 'source>> {
    let mut start = view.range().start();
    let mut parts = Vec::new();
    for separator in top_level_symbols(view, separator) {
        parts.push(view_between(view, start, separator));
        start = separator + 1;
    }
    parts.push(view_between(view, start, view.range().end()));
    parts
}

pub(in crate::g_syntax::parser) fn braced_contents<'lex, 'source>(
    view: TokenView<'lex, 'source>,
) -> Option<TokenView<'lex, 'source>> {
    let view = trim_layout(view);
    let (open_index, open_token) = view.first_significant()?;
    let (close_index, close_token) = view.last_significant()?;
    let TokenKind::Open {
        group,
        delimiter: Delimiter::Brace,
    } = open_token.kind()
    else {
        return None;
    };
    if !matches!(
        close_token.kind(),
        TokenKind::Close {
            group: close_group,
            delimiter: Delimiter::Brace,
        } if close_group == group
    ) {
        return None;
    }
    let delimiter_group = view.group(*group)?;
    (delimiter_group.open_token() == open_index
        && delimiter_group.close_token() == Some(close_index))
    .then(|| view.group_contents(*group))
    .flatten()
}

pub(in crate::g_syntax::parser) fn split_braced_members<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    context: &str,
) -> Option<ParseResult<Vec<TokenView<'lex, 'source>>>> {
    let contents = braced_contents(view)?;
    if is_layout_empty(contents) {
        return Some(Ok(Vec::new()));
    }

    let members = split_top_level(contents, ";")
        .into_iter()
        .map(trim_layout)
        .collect::<Vec<_>>();
    if !members.iter().any(|member| !is_layout_empty(*member)) {
        return Some(Err(error_at_view(
            view,
            format!("{context} cannot contain only semicolons; use `{{}}` for an empty body"),
        )));
    }
    if members
        .iter()
        .enumerate()
        .any(|(index, member)| is_layout_empty(*member) && index != 0 && index != members.len() - 1)
    {
        return Some(Err(error_at_view(
            view,
            format!("{context} contains an empty member between semicolons"),
        )));
    }
    Some(Ok(members
        .into_iter()
        .filter(|member| !is_layout_empty(*member))
        .collect()))
}

pub(in crate::g_syntax::parser) fn split_compound_header_body<'lex, 'source>(
    view: TokenView<'lex, 'source>,
) -> (TokenView<'lex, 'source>, Option<TokenView<'lex, 'source>>) {
    if let Some(StructuralBody::Layout { start: header_end }) = find_structural_body(view) {
        return (
            view_between(view, view.range().start(), header_end),
            Some(view_between(view, header_end, view.range().end())),
        );
    }

    let last_index = view.last_significant().map(|(index, _)| index);
    let braced_open = view.top_level().find_map(|indexed| {
        let TokenKind::Open {
            group,
            delimiter: Delimiter::Brace,
        } = indexed.token().kind()
        else {
            return None;
        };
        (view.group(*group).and_then(|group| group.close_token()) == last_index)
            .then_some(indexed.index())
    });
    if let Some(body_start) = braced_open {
        let header = trim_layout(view_between(view, view.range().start(), body_start));
        if last_significant_is_contextual_name(header, "with") {
            return (
                header,
                Some(view_between(view, body_start, view.range().end())),
            );
        }
    }

    (view, None)
}

pub(in crate::g_syntax::parser) fn parse_object_parents(
    view: TokenView<'_, '_>,
    line: usize,
    context: ExpressionContext,
) -> ParseResult<Vec<SyntaxExpr>> {
    if is_layout_empty(view) {
        return Err(vec![Diagnostic::error(
            line,
            "object `extends` requires at least one parent expression",
        )]);
    }

    let parent_views = split_top_level(view, ",");
    let mut parents = Vec::with_capacity(parent_views.len());
    for parent in parent_views {
        let parent = trim_layout(parent);
        if is_layout_empty(parent) {
            return Err(vec![Diagnostic::error(
                line,
                "object `extends` contains an empty parent expression",
            )]);
        }
        parents.push(parse_expression_in_context(parent, context.complete())?);
    }
    Ok(parents)
}

pub(in crate::g_syntax::parser) fn local_name<'source>(
    view: TokenView<'_, 'source>,
) -> Option<&'source str> {
    local_name_allowing(view, false)
}

pub(in crate::g_syntax::parser) fn object_alias_name<'source>(
    view: TokenView<'_, 'source>,
) -> Option<&'source str> {
    local_name_allowing(view, true)
}

fn local_name_allowing<'source>(
    view: TokenView<'_, 'source>,
    special_self: bool,
) -> Option<&'source str> {
    let mut significant = view
        .tokens()
        .iter()
        .filter(|token| !matches!(token.kind(), TokenKind::LineStart { .. }));
    let TokenKind::Name(name) = significant.next()?.kind() else {
        return None;
    };
    let valid = *name == "_"
        || name.starts_with(|character: char| character.is_ascii_alphabetic())
        || name.strip_prefix('_').is_some_and(|rest| {
            rest.starts_with(|character: char| character.is_ascii_alphabetic())
        });
    if !valid || significant.next().is_some() {
        return None;
    }
    match canonical_keyword(name) {
        Some(keyword) if special_self && keyword.spelling() == "self" => Some(*name),
        Some(_) => None,
        None => Some(*name),
    }
}

pub(in crate::g_syntax::parser) fn single_reserved_keyword(
    view: TokenView<'_, '_>,
) -> Option<super::super::keywords::Keyword> {
    let mut significant = view
        .tokens()
        .iter()
        .filter(|token| !matches!(token.kind(), TokenKind::LineStart { .. }));
    let TokenKind::Name(name) = significant.next()?.kind() else {
        return None;
    };
    (significant.next().is_none())
        .then(|| canonical_keyword(name))
        .flatten()
}

fn next_significant_after<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    absolute_index: usize,
) -> Option<super::input::IndexedToken<'lex, 'source>> {
    view.top_level().find(|indexed| {
        indexed.index() > absolute_index
            && !matches!(indexed.token().kind(), TokenKind::LineStart { .. })
    })
}

pub(in crate::g_syntax::parser) fn token_is_name(token: &SpannedToken<'_>, expected: &str) -> bool {
    matches!(token.kind(), TokenKind::Name(name) if *name == expected)
}

fn last_significant_is_contextual_name(view: TokenView<'_, '_>, expected: &str) -> bool {
    view.last_significant().is_some_and(|(index, token)| {
        token_is_name(token, expected)
            && (token.leading() != LeadingTrivia::Joint
                || index.checked_sub(1).is_some_and(|previous| {
                    view.token_at(previous)
                        .is_some_and(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
                }))
    })
}

fn view_is_single_name(view: TokenView<'_, '_>, expected: &str) -> bool {
    let mut significant = view
        .tokens()
        .iter()
        .filter(|token| !matches!(token.kind(), TokenKind::LineStart { .. }));
    significant
        .next()
        .is_some_and(|token| token_is_name(token, expected))
        && significant.next().is_none()
}

pub(in crate::g_syntax::parser) fn trim_layout<'lex, 'source>(
    mut view: TokenView<'lex, 'source>,
) -> TokenView<'lex, 'source> {
    let tokens = view.tokens();
    let leading = tokens
        .iter()
        .take_while(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
        .count();
    let trailing = tokens
        .iter()
        .rev()
        .take_while(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
        .count();
    let end = tokens.len().saturating_sub(trailing).max(leading);
    view = view
        .slice(leading..end)
        .expect("trimming layout tokens preserves an ordered range");
    view
}

pub(in crate::g_syntax::parser) fn is_layout_empty(view: TokenView<'_, '_>) -> bool {
    view.tokens()
        .iter()
        .all(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
}

pub(in crate::g_syntax::parser) fn view_between<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    start: usize,
    end: usize,
) -> TokenView<'lex, 'source> {
    view.subview(TokenRange::new(start, end).expect("ordered token indices form a range"))
        .expect("subexpression range remains within its source view")
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
mod tests;
