//! The structural layer's entry points to the term parser (`parse_body`,
//! `parse_expression_in_context`, `parse_expression_extent`), a body's
//! `where`, and the keyword, view and object-header helpers that the
//! declaration parser and the term parser share.

use super::super::keywords::{canonical_keyword, reserved_keyword_message};
use super::super::{Diagnostic, ObjectExpr, ObjectRealization, SyntaxExpr};
use super::expression::parse_expression_chain_view;
use super::expression_context::{ExpressionContext, ParsedExpression, validate_expression_floor};
use super::input::{TokenRange, TokenView};
use super::lexical::{Delimiter, LeadingTrivia, SpannedToken, TokenKind};
use super::term::may_head_keyword_form;

type ParseResult<T> = Result<T, Vec<Diagnostic>>;

#[derive(Debug, PartialEq, Eq)]
struct ObjectHeader {
    realization: ObjectRealization,
    name: Option<Box<SyntaxExpr>>,
    alias: Option<String>,
    deps: Vec<SyntaxExpr>,
    has_with: bool,
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
    // The term parser reads the bindings as a frame does.
    let (bindings, end) = super::term::parse_where_group(view, context, where_index)?;
    let parsed = ParsedExpression::new(
        SyntaxExpr::Let {
            bindings,
            body: Box::new(body),
        },
        end,
    );
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

/// Parses `view` as one expression, which may end before the view's end
/// at a `where` that the body around it takes, or at a `->` where
/// `context` ends tails there.
pub(in crate::g_syntax::parser) fn parse_expression_extent(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> ParseResult<ParsedExpression> {
    let view = trim_layout(view);
    let (context, floor_diagnostics) = validate_expression_floor(view, context);
    if !floor_diagnostics.is_empty() {
        return Err(floor_diagnostics);
    }
    parse_expression_chain_view(view, context)
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
    let view = trim_layout(view);
    let parsed = parse_expression_extent(view, context)?;
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

fn object_expression_head(view: TokenView<'_, '_>) -> Option<(ObjectRealization, usize)> {
    if let Some(object_index) = keyword_head(view, "object") {
        return Some((ObjectRealization::Instance, object_index));
    }
    let abstract_index = keyword_head(view, "abstract")?;
    let object = next_significant_after(view, abstract_index)?;
    is_contextual_keyword_at(view, object.index(), "object")
        .then_some((ObjectRealization::Abstract, object.index()))
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
