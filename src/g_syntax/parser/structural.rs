//! Structural expression parsing for `let`, `where`, `using`, objects, and
//! `with`.
//!
//! Each parser consumes a lexer-owned hard range or returns an exact yielded
//! boundary. Postfix `where` and leading infix operators resume from those
//! boundaries. Object bodies share the recursive declaration parser used by
//! top-level object declarations.

use super::super::keywords::{canonical_keyword, reserved_keyword_message};
use super::super::{Diagnostic, ObjectExpr, ObjectRealization, Severity, SyntaxExpr};
use super::conditional::{
    is_postfix_if_candidate, parse_match_expression, parse_postfix_if_suffix,
    parse_try_match_expression,
};
use super::declaration::{parse_nonempty_object_body, parse_object_body};
use super::expression::{parse_expression_chain_view, syntax_operator};
use super::expression_context::{ExpressionContext, ParsedExpression, validate_expression_floor};
use super::input::{TokenRange, TokenView};
use super::layout::LayoutView;
use super::lexical::{Delimiter, LeadingTrivia, SpannedToken, TokenKind};
use super::term::{legacy_grammar, may_head_keyword_form};

type ParseResult<T> = Result<T, Vec<Diagnostic>>;

#[derive(Debug, PartialEq, Eq)]
struct ObjectHeader {
    realization: ObjectRealization,
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
        parse_body(view, ExpressionContext::for_fragment(view))
    })
}

pub(in crate::g_syntax::parser) fn parse_expression_in_context(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> ParseResult<SyntaxExpr> {
    let view = trim_layout(view);
    let parsed = parse_expression_extent(view, context.complete())?;
    if parsed.end() < view.range().end() {
        return Err(misplaced_where(view, parsed.end()));
    }
    parsed
        .into_expression()
        .map_err(|message| error_at_view(view, message))
}

/// Parses a body that a `where` may end: a definition's or binding's
/// right-hand side, a statement's expression, an arm's result, or a
/// parenthesized expression's contents. Each `where` scopes everything
/// before it in the body, and no expression inside it ends at one.
pub(in crate::g_syntax::parser) fn parse_body(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> ParseResult<SyntaxExpr> {
    if legacy_grammar() {
        return parse_expression_in_context(view, context);
    }
    let view = trim_layout(view);
    // A lone `where` reads as a name, which the expression parser reports
    // as a reserved keyword.
    if let Some(where_index) = keyword_head(view, "where")
        && view
            .last_significant()
            .is_some_and(|(last, _)| last > where_index)
    {
        let where_token = view
            .token_at(where_index)
            .expect("a body's `where` lies in its view");
        return Err(error_at_token(
            view,
            where_token,
            "where expression requires a body",
        ));
    }
    let parsed = parse_expression_extent(view, context.complete())?;
    where_suffix(view, context, parsed)?
        .into_expression()
        .map_err(|message| error_at_view(view, message))
}

/// Applies the `where` at the end of `parsed`, an expression that begins a
/// body in `view`, if one is there: it scopes everything before it, and its
/// bindings end the body.
pub(in crate::g_syntax::parser) fn where_suffix(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
    parsed: ParsedExpression,
) -> ParseResult<ParsedExpression> {
    let Some(where_index) = where_at(view, parsed.end()) else {
        return Ok(parsed);
    };
    let body = parsed
        .into_expression()
        .map_err(|message| error_at_view(view, message))?;
    // The term parser reads the bindings as a frame does; the legacy
    // grammar's structural parser read them from views.
    let parsed = if legacy_grammar() {
        parse_where_suffix(view, context, where_index, body)?
    } else {
        let (bindings, end) = super::term::parse_where_group(view, context, where_index)?;
        ParsedExpression::new(
            SyntaxExpr::Let {
                bindings,
                body: Box::new(body),
            },
            end,
        )
    };
    if parsed.end() < view.range().end() {
        if let Some(another) = where_at(view, parsed.end()).and_then(|index| view.token_at(index)) {
            return Err(error_at_token(
                view,
                another,
                "a definition, statement or arm takes one `where`: put these bindings in its group",
            ));
        }
        let tail = trim_layout(view_between(view, parsed.end(), view.range().end()));
        return Err(error_at_view(
            tail,
            "nothing may follow a `where`'s bindings, which end its definition, statement or arm",
        ));
    }
    Ok(parsed)
}

/// The `where` an expression that ended at `end` stopped at, if any.
pub(in crate::g_syntax::parser) fn where_at(view: TokenView<'_, '_>, end: usize) -> Option<usize> {
    trim_layout(view_between(view, end, view.range().end()))
        .first_significant()
        .map(|(index, _)| index)
        .filter(|index| is_contextual_keyword_at(view, *index, "where"))
}

/// The error for a `where` at `end` that ends an expression which is no
/// body.
pub(in crate::g_syntax::parser) fn misplaced_where(
    view: TokenView<'_, '_>,
    end: usize,
) -> Vec<Diagnostic> {
    let tail = trim_layout(view_between(view, end, view.range().end()));
    match where_at(view, end).and_then(|index| view.token_at(index)) {
        Some(where_token) => error_at_token(
            view,
            where_token,
            "`where` scopes a whole definition, statement or arm; parenthesize `(… where …)` or use `let … in` for part of an expression",
        ),
        None => error_at_view(tail, "expression ended before its view"),
    }
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
    let mut leading_match = false;
    let parsed = if let Some(result) = parse_parenthesized_structural(view, context) {
        ParsedExpression::new(result?, view.range().end())
    } else if legacy_grammar()
        && let Some(result) = parse_let(view, context)
    {
        result?
    } else if keyword_head(view, "let").is_some() {
        // A `let` is a frame of the term parser, which the legacy grammar's
        // structural parser read.
        parse_expression_chain_view(view, context)?
    } else if legacy_grammar()
        && let Some(result) = parse_object(view, context)
    {
        result?
    } else if keyword_head(view, "if").is_some() || keyword_head(view, "try").is_some() {
        // A conditional is a frame of the term parser, which takes the
        // whole view: its `else` branch owns any `where` or `with` in it.
        parse_expression_chain_view(view, context)?
    } else if (keyword_head(view, "match").is_some() || keyword_head(view, "try_match").is_some())
        && (!legacy_grammar() || contextual_keywords(view, "where").next().is_none())
    {
        // A match is a frame of the term parser, and an atom like any form:
        // what follows its arms reads as the expression grammar reads it.
        // In the legacy grammar, a `where` after them suffixed the whole
        // view here.
        parse_expression_chain_view(view, context)?
    } else if let Some(match_index) = keyword_head(view, "match") {
        leading_match = true;
        parse_match_expression(view, match_index, context.may_yield())?
    } else if let Some(try_match_index) = keyword_head(view, "try_match") {
        leading_match = true;
        parse_try_match_expression(view, try_match_index, context.may_yield())?
    } else if legacy_grammar()
        && let Some(result) = parse_with(view, context)
    {
        result?
    } else if postfix_if_suffix(view).is_some() {
        // So does a postfix conditional, whose fallback owns any `where`.
        parse_expression_chain_view(view, context)?
    } else if let Some(where_index) = contextual_keywords(view, "where")
        .next()
        .filter(|_| legacy_grammar())
    {
        let where_token = view
            .token_at(where_index)
            .expect("a selected where suffix remains within its expression view");
        let body = trim_layout(view_between(view, view.range().start(), where_index));
        if is_layout_empty(body) {
            return Err(error_at_token(
                view,
                where_token,
                "where expression requires a body",
            ));
        }
        // A body that ends at a `->` ends this expression there, and the
        // `where` belongs to what follows the arrow.
        parse_expression_extent(body, context.complete_keeping_arrow())?
    } else {
        parse_expression_chain_view(view, context)?
    };
    let parsed = resume_expression_suffixes(view, context, parsed)?;
    // An expression may always end at a `where`, which the body around it
    // takes.
    if parsed.end() < view.range().end()
        && !context.permits_yield()
        && !stops_at_arrow(view, context, parsed.end())
        && (legacy_grammar() || where_at(view, parsed.end()).is_none())
    {
        // What follows a match's arms before its `where` reads as the
        // expression grammar reads it.
        if leading_match {
            return parse_expression_chain_view(view, context);
        }
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

/// Whether an expression ending at `end` stops at a `->` that ends its open
/// tails.
fn stops_at_arrow(view: TokenView<'_, '_>, context: ExpressionContext, end: usize) -> bool {
    context.arrow_ends_tail()
        && trim_layout(view_between(view, end, view.range().end()))
            .first_significant()
            .is_some_and(|(_, token)| matches!(token.kind(), TokenKind::Symbol("->")))
}

/// Parses an open tail: an `else` branch, a postfix `if`'s fallback, or a
/// `let` body. It runs to `form_end`, its form's end, unless `context` ends
/// tails at a `->` or a `where` ends it. Returns the tail and where its form
/// ends.
pub(in crate::g_syntax::parser) fn parse_open_tail(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
    form_end: usize,
) -> ParseResult<(SyntaxExpr, usize)> {
    if !context.arrow_ends_tail() && legacy_grammar() {
        return parse_expression_in_context(view, context).map(|tail| (tail, form_end));
    }
    let view = trim_layout(view);
    let parsed = parse_expression_extent(view, context.complete_keeping_arrow())?;
    let end = if parsed.end() < view.range().end() {
        parsed.end()
    } else {
        form_end
    };
    let tail = parsed
        .into_expression()
        .map_err(|message| error_at_view(view, message))?;
    Ok((tail, end))
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
    // The expression grammar parses a tuple and delimits its members: a
    // tuple's items are no bodies, and a match leading one, as in
    // `(match y as {…}, 1)`, ends at its comma, since its arms cannot hold
    // an unparenthesized one. The legacy grammar read other tuples here.
    let leads_with_match = ["match", "try_match"]
        .into_iter()
        .any(|head| keyword_head(contents, head).is_some());
    if (leads_with_match || !legacy_grammar()) && !top_level_symbols(contents, ",").is_empty() {
        return None;
    }
    let context = context.complete();
    let starts_structural = ["let", "using", "try", "object"]
        .into_iter()
        .any(|head| keyword_head(contents, head).is_some())
        || contextual_keywords(contents, "where").next().is_some()
        || postfix_if_suffix(contents).is_some()
        || has_compound_with_body(contents);
    starts_structural.then(|| {
        if legacy_grammar() {
            parse_expression_extent(contents, context).and_then(|parsed| {
                parsed
                    .into_expression()
                    .map_err(|message| error_at_view(contents, message))
            })
        } else {
            parse_body(contents, context)
        }
    })
}

pub(in crate::g_syntax::parser) fn parse_using_expression(
    view: TokenView<'_, '_>,
    using_index: usize,
    context: ExpressionContext,
) -> ParseResult<ParsedExpression> {
    let using_token = view
        .token_at(using_index)
        .expect("a selected `using` head remains inside its expression view");
    let hard_end = structural_expression_hard_end(view, using_index);
    let rest = trim_layout(view_between(view, using_index + 1, hard_end));
    if is_layout_empty(rest) {
        return Err(error_at_token(
            view,
            using_token,
            "using expression requires a namespace and `in` or `do` body",
        ));
    }

    let in_boundary = contextual_keywords(rest, "in").next();
    let (boundary, body_start) = if let Some(boundary) = in_boundary {
        (boundary, boundary + 1)
    } else if let Some(boundary) = contextual_keywords(rest, "do").next() {
        (boundary, boundary)
    } else {
        return Err(error_at_token(
            view,
            using_token,
            "using expression requires `in` or a trailing `do` body",
        ));
    };

    let namespace_view = trim_layout(view_between(rest, rest.range().start(), boundary));
    if is_layout_empty(namespace_view) {
        return Err(error_at_token(
            view,
            using_token,
            "using expression requires a namespace before its body",
        ));
    }
    let body_view = trim_layout(view_between(rest, body_start, rest.range().end()));
    if is_layout_empty(body_view) {
        return Err(error_at_token(
            view,
            using_token,
            "using expression requires a body",
        ));
    }

    let namespace = parse_expression_in_context(namespace_view, context.complete())?;
    let body = parse_expression_extent(body_view, context.may_yield())?;
    let end = body.end();
    let body = body
        .into_expression()
        .map_err(|message| error_at_view(body_view, message))?;
    Ok(ParsedExpression::new(
        SyntaxExpr::Using {
            namespace: Box::new(namespace),
            body: Box::new(body),
        },
        end,
    ))
}

fn structural_expression_hard_end(view: TokenView<'_, '_>, head_index: usize) -> usize {
    let containing_close = view
        .source()
        .enclosing_group(head_index)
        .and_then(|group| view.source().groups()[group].close_token())
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

fn parse_let(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Option<ParseResult<ParsedExpression>> {
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

    let in_index = contextual_keywords(rest, "in").next();
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
    let (body, end) = match parse_open_tail(body, context, view.range().end()) {
        Ok(body) => body,
        Err(diagnostics) => return Some(Err(diagnostics)),
    };

    Some(Ok(ParsedExpression::new(
        SyntaxExpr::Let {
            bindings,
            body: Box::new(body),
        },
        end,
    )))
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
    // The legacy grammar ended a group at the next `where`, wherever it
    // stood. A `where` in a binding now scopes that binding, and a group
    // ends at its `}` or its layout block's end, which ends the body.
    let suffix_end = if legacy_grammar() {
        contextual_keyword_after(view, where_index, "where").unwrap_or(view.range().end())
    } else {
        braced_group_end(view, where_index + 1).unwrap_or(view.range().end())
    };
    let all_bindings = trim_layout(view_between(view, where_index + 1, suffix_end));
    if is_layout_empty(all_bindings) {
        return Err(error_at_token(
            view,
            where_token,
            "where expression requires at least one binding or an explicit `{}` group",
        ));
    }

    let (bindings, end, braced_bindings) = if let Some(bindings) =
        parse_braced_bindings(all_bindings, "where", context)
    {
        (bindings?, all_bindings.range().end(), true)
    } else if all_bindings
        .top_level()
        .any(|indexed| matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
    {
        let block = LayoutView::new(all_bindings).block();
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
        // A line-led `where` after the block is a second `where`, which the
        // body reports.
        if let Some(boundary) = block.boundary()
            && all_bindings.subview(boundary.tokens()).is_some_and(|line| {
                !top_level_symbols(line, "=").is_empty()
                    && (legacy_grammar() || keyword_head(line, "where").is_none())
            })
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
        let end = block.end();
        let binding_views = block
            .into_statements()
            .into_iter()
            .filter_map(|statement| all_bindings.subview(statement.tokens()))
            .collect();
        (parse_binding_views(binding_views, context)?, end, false)
    } else if !top_level_symbols(all_bindings, ";").is_empty() {
        return Err(error_at_view(
            all_bindings,
            "naked semicolon-separated `where` bindings are not supported; use `where { ... }`",
        ));
    } else {
        (
            parse_binding_views(vec![all_bindings], context)?,
            all_bindings.range().end(),
            false,
        )
    };
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

/// The end of a brace group that begins at `start`'s first significant
/// token, past its `}`.
fn braced_group_end(view: TokenView<'_, '_>, start: usize) -> Option<usize> {
    let rest = view_between(view, start, view.range().end());
    let (_, token) = rest.first_significant()?;
    let TokenKind::Open {
        group,
        delimiter: Delimiter::Brace,
    } = token.kind()
    else {
        return None;
    };
    view.group(*group)?
        .close_token()
        .map(|close| close + 1)
        .filter(|end| *end <= view.range().end())
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
        // A `where` ends the expression for the body around it, which the
        // legacy grammar's view-level dispatch took to be this one.
        if token_is_name(token, "where")
            && (token.leading() != LeadingTrivia::Joint
                || begins_layout_line(view, boundary_index, token))
        {
            if !legacy_grammar() {
                break;
            }
            let body = parsed
                .into_expression()
                .map_err(|message| error_at_view(tail, message))?;
            parsed = parse_where_suffix(view, context, boundary_index, body)?;
            continue;
        }
        if token_is_name(token, "if")
            && is_postfix_if_candidate(view, boundary_index)
            && (token.leading() != LeadingTrivia::Joint
                || begins_layout_line(view, boundary_index, token))
        {
            let then_result = parsed
                .into_expression()
                .map_err(|message| error_at_view(tail, message))?;
            parsed = parse_postfix_if_suffix(view, boundary_index, context, then_result)?;
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
        let operand_end = match where_at(view, right.end()) {
            Some(_) if !legacy_grammar() && right.end() < operand_view.range().end() => right.end(),
            _ => operand_end,
        };
        if right.end() != operand_view.range().end() && right.end() != operand_end {
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
                syntax_operator(token).is_some()
                    || token_is_name(token, "where")
                    || token_is_name(token, "if")
            })
    })
}

fn postfix_if_suffix(view: TokenView<'_, '_>) -> Option<usize> {
    contextual_keywords(view, "if")
        .next()
        .filter(|index| is_postfix_if_candidate(view, *index))
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
    let head_view = trim_layout(view_between(view, view.range().start(), equal_index));
    let value_view = trim_layout(view_between(view, equal_index + 1, view.range().end()));
    let (name, parameters) = parse_local_binding_head(head_view)?;
    if is_layout_empty(value_view) {
        return Err(error_at_view(
            view,
            format!("local binding `{name}` requires a value"),
        ));
    }
    let value = parse_body(value_view, context)?;
    let value = if parameters.is_empty() {
        value
    } else {
        SyntaxExpr::Lambda(parameters, Box::new(value))
    };
    Ok((name.to_owned(), value))
}

/// A local binding's head: its name, then any parameters, as a module
/// definition takes them. `go k = Body` is sugar for `go = \k -> Body`.
fn parse_local_binding_head<'source>(
    view: TokenView<'_, 'source>,
) -> ParseResult<(&'source str, Vec<String>)> {
    // Each significant top-level token with its own one-token view.
    let tokens = view
        .top_level()
        .filter(|indexed| !matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
        .map(|indexed| {
            let index = indexed.index();
            (
                indexed.token().leading(),
                view_between(view, index, index + 1),
            )
        })
        .collect::<Vec<_>>();
    let Some(((_, name_view), parameter_tokens)) = tokens.split_first() else {
        return Err(error_at_view(view, "invalid local binding name ``"));
    };
    let Some(name) = local_name(*name_view) else {
        if let Some(keyword) = single_reserved_keyword(*name_view) {
            return Err(error_at_view(*name_view, reserved_keyword_message(keyword)));
        }
        return Err(error_at_view(
            view,
            format!(
                "invalid local binding name `{}`",
                view.source_text().unwrap_or("").trim()
            ),
        ));
    };
    let mut parameters = Vec::new();
    for (leading, parameter_view) in parameter_tokens {
        let Some(parameter) = local_name(*parameter_view) else {
            if let Some(keyword) = single_reserved_keyword(*parameter_view) {
                return Err(error_at_view(
                    *parameter_view,
                    reserved_keyword_message(keyword),
                ));
            }
            return Err(error_at_view(
                *parameter_view,
                format!("local binding `{name}` parameters must be local names"),
            ));
        };
        if *leading == LeadingTrivia::Joint {
            return Err(error_at_view(
                *parameter_view,
                format!("local binding `{name}` parameters must be separated from its name"),
            ));
        }
        parameters.push(parameter.to_owned());
    }
    Ok((name, parameters))
}

fn parse_object(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Option<ParseResult<ParsedExpression>> {
    object_expression_head(view)?;
    if let Some(where_index) = contextual_keywords(view, "where").next()
        && !has_compound_with_body(view)
    {
        // A bodyless object ends at its `where`, which the legacy grammar's
        // view-level dispatch took first.
        if legacy_grammar() {
            return None;
        }
        return parse_object(
            trim_layout(view_between(view, view.range().start(), where_index)),
            context,
        );
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
            realization: header.realization,
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
            let block = LayoutView::new(body).block();
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
            if block.hanging()
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

pub(in crate::g_syntax::parser) fn line_begins_object_member(view: TokenView<'_, '_>) -> bool {
    ["object", "extend"]
        .into_iter()
        .any(|head| keyword_head(view, head).is_some())
        || object_expression_head(view)
            .is_some_and(|(realization, _)| realization == ObjectRealization::Abstract)
        || ["=", ":=", "::="]
            .into_iter()
            .any(|operator| !top_level_symbols(view, operator).is_empty())
}

fn find_structural_body(view: TokenView<'_, '_>) -> Option<StructuralBody> {
    let where_boundary = contextual_keywords(view, "where")
        .next()
        .unwrap_or(view.range().end());
    // A `match` or `try_match` head owns what follows it, so no later `with`
    // opens an update or object body here: an update in its subject or arms
    // is its own. A head is judged as the term parser judges it, so a match
    // that is a tag's payload, as in `(k):match x as {…}`, owns what follows
    // it too.
    let match_head = view
        .top_level()
        .find(|indexed| {
            matches!(
                indexed.token().kind(),
                TokenKind::Name("match" | "try_match")
            ) && may_head_keyword_form(view, indexed.index())
        })
        .map_or(view.range().end(), |indexed| indexed.index());
    for with_index in contextual_keywords(view, "with") {
        if with_index >= where_boundary || with_index > match_head {
            break;
        }
        let Some(next) = next_significant_after(view, with_index) else {
            continue;
        };
        if next.index() >= where_boundary {
            continue;
        }
        if let TokenKind::Open {
            group,
            delimiter: Delimiter::Brace,
        } = next.token().kind()
        {
            let close = view.group(*group).and_then(|group| group.close_token())?;
            return Some(StructuralBody::Braced { end: close + 1 });
        }
        return Some(StructuralBody::Layout {
            start: next.index(),
        });
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
    if rest
        .subview(first.tokens())
        .and_then(TokenView::first_significant)
        .is_none()
    {
        return Err(error_at_view(
            rest,
            "let expression requires at least one binding",
        ));
    }
    let let_token = full
        .token_at(let_index)
        .expect("the let token remains within the full expression view");
    let let_column = full
        .column_at_span(let_token.span())
        .expect("the let token has a source column");
    let block = LayoutView::new(rest).block();

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
    let (realization, object_index) = object_expression_head(header_view)?;
    let object_token = header_view
        .token_at(object_index)
        .expect("object expression head remains within its header");
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

    let as_index = contextual_keywords(header, "as").next();
    let extends_index = contextual_keywords(header, "extends").next();
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
        realization,
        name,
        alias,
        deps,
        has_with,
    }))
}

/// An object expression's header, `(abstract)? object (NameExpr|_) (as
/// Name)? (extends ExpressionList)? with?`, read from its view: the object
/// without its body.
pub(in crate::g_syntax::parser) fn parse_object_expression_header(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> ParseResult<ObjectExpr> {
    let header = parse_object_header(view, context)
        .expect("an object expression's header begins with its keyword")?;
    Ok(ObjectExpr {
        realization: header.realization,
        name: header.name,
        alias: header.alias,
        deps: header.deps,
        body: Vec::new(),
    })
}

#[cfg(test)]
/// Whether the view's dispatch reads a form the term parser reads as a
/// frame but the legacy grammar's view-level dispatch read structurally: a
/// `let` or an object leading it, or a `with` body at its level.
pub(in crate::g_syntax::parser) fn dispatched_structurally(view: TokenView<'_, '_>) -> bool {
    keyword_head(view, "let").is_some()
        || object_expression_head(view).is_some()
        || has_compound_with_body(view)
}

fn object_expression_head(view: TokenView<'_, '_>) -> Option<(ObjectRealization, usize)> {
    if let Some(object_index) = keyword_head(view, "object") {
        return Some((ObjectRealization::Instance, object_index));
    }
    let abstract_index = keyword_head(view, "abstract")?;
    let object = next_significant_after(view, abstract_index)?;
    is_contextual_keyword_at(view, object.index(), "object")
        .then_some((ObjectRealization::Abstract, object.index()))
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
    let as_index = contextual_keywords(base_and_alias, "as").last();
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

/// The top-level tokens of `view` that are the keyword `expected` in a
/// keyword position, lazily, so a caller that wants the first stops there.
pub(in crate::g_syntax::parser) fn contextual_keywords<'lex, 'source, 'e>(
    view: TokenView<'lex, 'source>,
    expected: &'e str,
) -> impl Iterator<Item = usize> + use<'lex, 'source, 'e> {
    view.top_level()
        .map(|indexed| indexed.index())
        .filter(move |index| is_contextual_keyword_at(view, *index, expected))
}

/// The first contextual `expected` after token `after` in `view`, judged as
/// in `view`, scanning only what follows `after`.
fn contextual_keyword_after(
    view: TokenView<'_, '_>,
    after: usize,
    expected: &str,
) -> Option<usize> {
    view_between(view, after + 1, view.range().end())
        .top_level()
        .map(|indexed| indexed.index())
        .find(|index| is_contextual_keyword_at(view, *index, expected))
}

/// The first significant token of `view`, when it is the keyword `expected`
/// in a keyword position.
pub(in crate::g_syntax::parser) fn keyword_head(
    view: TokenView<'_, '_>,
    expected: &str,
) -> Option<usize> {
    view.first_significant()
        .map(|(index, _)| index)
        .filter(|index| is_contextual_keyword_at(view, *index, expected))
}

fn is_contextual_keyword_at(view: TokenView<'_, '_>, index: usize, expected: &str) -> bool {
    view.token_at(index)
        .is_some_and(|token| token_is_name(token, expected))
        && in_keyword_position(view, index)
}

/// Whether the token at `index` stands where a keyword has meaning: first in
/// `view` or in a group's contents, after a space, or first on its line, and
/// not before a key mark.
pub(in crate::g_syntax::parser) fn in_keyword_position(
    view: TokenView<'_, '_>,
    index: usize,
) -> bool {
    view.token_at(index).is_some_and(|token| {
        (index == view.range().start()
            || token.leading() != LeadingTrivia::Joint
            || index.checked_sub(1).is_some_and(|previous| {
                view.token_at(previous).is_some_and(|token| {
                    matches!(
                        token.kind(),
                        TokenKind::LineStart { .. } | TokenKind::Open { .. }
                    )
                })
            }))
            && !before_key_mark(view, index)
    })
}

/// Whether a joint `:` or `.` follows the token at `index`. A name there is
/// a tag's key (`where:Value`) or a path's root (`if.x`), so a keyword
/// spelled there is never a keyword: the tag is data, and the path an error,
/// since a keyword may not root a path.
pub(in crate::g_syntax::parser) fn before_key_mark(view: TokenView<'_, '_>, index: usize) -> bool {
    view.token_at(index + 1).is_some_and(|mark| {
        mark.leading() == LeadingTrivia::Joint
            && matches!(mark.kind(), TokenKind::Symbol(":" | "."))
    })
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
