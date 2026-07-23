//! Structural expression parsing for `let`, `where`, objects, and `with`.
//!
//! `let`, `where`, object, and `with` expressions produce complete syntax
//! trees. Object bodies share the recursive declaration parser used by
//! top-level object declarations.

use super::super::keywords::{canonical_keyword, reserved_keyword_message};
use super::super::{Diagnostic, ObjectExpr, Severity, SyntaxExpr};
use super::declaration::{parse_nonempty_object_body, parse_object_body};
use super::expression::parse_expression_view;
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
    Ok(parsed.into_expression())
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
    let expression = if let Some(result) = parse_parenthesized_structural(view, context) {
        result?
    } else if let Some(result) = parse_let(view, context) {
        result?
    } else if let Some(result) = parse_where(view, context) {
        result?
    } else if let Some(result) = parse_object(view, context) {
        result?
    } else if let Some(result) = parse_with(view, context) {
        result?
    } else {
        parse_expression_view(view, context)?
    };
    let parsed = ParsedExpression::new(expression, view.range().end());
    if parsed.end() < view.range().end() && !context.permits_yield() {
        return Err(error_at_view(
            view,
            "expression ended before its complete token range",
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
    parse_let(contents, context)
        .or_else(|| parse_where(contents, context))
        .or_else(|| parse_object(contents, context))
        .or_else(|| parse_with(contents, context))
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
) -> Option<ParseResult<SyntaxExpr>> {
    let where_index = contextual_keywords(view, "where").into_iter().last()?;
    let where_token = view.token_at(where_index)?;
    let body = trim_layout(view_between(view, view.range().start(), where_index));
    let bindings = trim_layout(view_between(view, where_index + 1, view.range().end()));
    if is_layout_empty(body) {
        return Some(Err(error_at_token(
            view,
            where_token,
            "where expression requires a body",
        )));
    }
    if is_layout_empty(bindings) {
        return Some(Err(error_at_token(
            view,
            where_token,
            "where expression requires at least one binding or an explicit `{}` group",
        )));
    }

    let (bindings, braced_bindings) = if let Some(bindings) =
        parse_braced_bindings(bindings, "where", context)
    {
        (bindings, true)
    } else if bindings
        .top_level()
        .any(|indexed| matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
    {
        let layout = LayoutView::new(bindings);
        let lines = layout.lines();
        if let Some(first) = lines.first()
            && let Some(misaligned) = lines.iter().skip(1).find(|line| {
                line.indentation() != first.indentation()
                    && bindings
                        .subview(line.tokens())
                        .is_some_and(|line| !top_level_symbols(line, "=").is_empty())
            })
        {
            return Some(Err(vec![Diagnostic::error(
                misaligned.line(),
                "multi-line where binding names must align under the first binding",
            )]));
        }
        let statements = match layout.statements(LayoutBase::FirstLine) {
            Ok(statements) => statements,
            Err(error) => {
                return Some(Err(vec![Diagnostic::error(error.line(), error.message())]));
            }
        };
        let binding_views = statements
            .into_iter()
            .filter_map(|statement| bindings.subview(statement.tokens()))
            .collect();
        (parse_binding_views(binding_views, context), false)
    } else if !top_level_symbols(bindings, ";").is_empty() {
        (
            Err(error_at_view(
                bindings,
                "naked semicolon-separated `where` bindings are not supported; use `where { ... }`",
            )),
            false,
        )
    } else {
        (parse_binding_views(vec![bindings], context), false)
    };
    let bindings = match bindings {
        Ok(bindings) => bindings,
        Err(diagnostics) => return Some(Err(diagnostics)),
    };
    if bindings.is_empty() && !braced_bindings {
        return Some(Err(error_at_token(
            view,
            where_token,
            "where expression requires at least one binding",
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
) -> Option<ParseResult<SyntaxExpr>> {
    let object_line = view
        .first_significant()
        .and_then(|(_, token)| view.line_at_span(token.span()))
        .unwrap_or(1);
    let (_, body_view) = split_compound_header_body(view);
    let header = match parse_object_header(view, context)? {
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
    Some(Ok(SyntaxExpr::Object(ObjectExpr {
        name: header.name,
        alias: header.alias,
        deps: header.deps,
        body,
    })))
}

fn parse_with(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Option<ParseResult<SyntaxExpr>> {
    let (_, body_view) = split_compound_header_body(view);
    let body_view = body_view?;
    let header = match parse_with_header(view, context)? {
        Ok(header) => header,
        Err(diagnostics) => return Some(Err(diagnostics)),
    };
    let mut diagnostics = Vec::new();
    let body = if braced_contents(body_view).is_some() {
        parse_object_body(body_view, context, &mut diagnostics)
    } else {
        let line = view
            .first_significant()
            .and_then(|(_, token)| view.line_at_span(token.span()))
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
    Some(Ok(SyntaxExpr::With {
        base: header.base,
        alias: header.alias,
        body,
    }))
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
    let let_column = token_column(
        full,
        full.token_at(let_index)
            .expect("the let token remains within the full expression view"),
    );
    let binding_column = token_column(full, first_binding_token);
    let mut binding_starts = vec![first.tokens().start()];
    let mut body_start = None;

    for line in lines.into_iter().skip(1) {
        let line_view = rest
            .subview(line.tokens())
            .expect("layout lines remain within their source view");
        let begins_binding = !top_level_symbols(line_view, "=").is_empty();

        if line.indentation() <= let_column {
            if line.indentation() != let_column {
                return Err(vec![Diagnostic::error(
                    line.line(),
                    "multi-line let body must align with `let`",
                )]);
            }
            if begins_binding {
                return Err(vec![Diagnostic::error(
                    line.line(),
                    "multi-line let binding names must align under the first binding",
                )]);
            }
            body_start = Some(line.tokens().start());
            break;
        }

        if begins_binding {
            if line.indentation() != binding_column {
                return Err(vec![Diagnostic::error(
                    line.line(),
                    "multi-line let binding names must align under the first binding",
                )]);
            }
            binding_starts.push(line.tokens().start());
        }
    }

    let Some(body_start) = body_start else {
        return Err(error_at_view(
            rest,
            "multi-line let expression requires a body",
        ));
    };
    binding_starts.push(body_start);
    let bindings = binding_starts
        .windows(2)
        .map(|bounds| trim_layout(view_between(rest, bounds[0], bounds[1])))
        .collect();
    let body = trim_layout(view_between(rest, body_start, rest.range().end()));
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
    if let Some(header_end) = first_top_level_line_start(view) {
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

pub(in crate::g_syntax::parser) fn first_top_level_line_start(
    view: TokenView<'_, '_>,
) -> Option<usize> {
    view.top_level().find_map(|indexed| {
        matches!(indexed.token().kind(), TokenKind::LineStart { .. }).then(|| indexed.index())
    })
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

fn token_column(view: TokenView<'_, '_>, token: &SpannedToken<'_>) -> usize {
    view.line_at_span(token.span())
        .and_then(|line| view.line_span(line))
        .map_or(0, |line| token.span().start() - line.start())
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
