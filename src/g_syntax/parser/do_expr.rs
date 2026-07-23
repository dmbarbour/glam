//! `do` statement and block grammar.
//!
//! Both layout and braced forms consume borrowed ranges from the source-wide
//! lexical pass. A statement expression is handed back to the complete token
//! expression parser; no source substring is normalized or re-lexed.

use crate::g_syntax::keywords::{canonical_keyword, reserved_keyword_message};
use crate::g_syntax::{Diagnostic, DoExpr, DoStep, DoStepKind, SyntaxExpr};

use super::input::{TokenRange, TokenView};
use super::layout::{LayoutBase, LayoutView};
use super::lexical::{Delimiter, LeadingTrivia, SpannedToken, TokenKind};
use super::structural::{parse_expression, single_reserved_keyword};

type ParseResult<T> = Result<T, Vec<Diagnostic>>;

enum ParsedDoStatement {
    Abstract(Vec<String>),
    Bind { name: String, operation: SyntaxExpr },
    ValueBind { name: String, value: SyntaxExpr },
    Expr(SyntaxExpr),
}

/// Parses the `do` atom beginning at `do_index` and returns the first token
/// after it. The caller can therefore consume the atom as one ordinary
/// expression node without rewriting its source.
pub(in crate::g_syntax::parser) fn parse_do_atom(
    view: TokenView<'_, '_>,
    do_index: usize,
) -> ParseResult<(SyntaxExpr, usize)> {
    let Some(do_token) = view.token_at(do_index) else {
        return Err(error_at_view(
            view,
            "do expression starts outside its token view",
        ));
    };
    if !token_is_name(do_token, "do") {
        return Err(error_at_token(view, do_token, "expected `do`"));
    }

    let after_do = view_between(view, do_index + 1, view.range().end());
    let Some((next_index, next)) = after_do.first_significant() else {
        return Err(error_at_token(
            view,
            do_token,
            "layout do expression requires a newline-delimited block",
        ));
    };
    let separated = next.leading() != LeadingTrivia::Joint
        || after_do
            .tokens()
            .iter()
            .take(next_index - after_do.range().start())
            .any(|token| matches!(token.kind(), TokenKind::LineStart { .. }));
    if !separated {
        return Err(error_at_token(
            view,
            do_token,
            "`do` must be separated from its body",
        ));
    }

    if let TokenKind::Open {
        group: group_id,
        delimiter: Delimiter::Brace,
    } = next.kind()
    {
        let Some(group) = view.group(*group_id) else {
            return Err(error_at_token(
                view,
                next,
                "braced do expression refers to an unknown delimiter group",
            ));
        };
        let Some(close) = group.close_token() else {
            return Err(error_at_token(
                view,
                next,
                "braced do expression has an unmatched or mismatched `}`",
            ));
        };
        let Some(body) = view.group_contents(*group_id) else {
            return Err(error_at_token(
                view,
                next,
                "braced do expression body falls outside its token view",
            ));
        };
        return parse_braced_body(body, do_token).map(|expr| (expr, close + 1));
    }

    parse_layout_body(after_do, do_token).map(|expr| (expr, view.range().end()))
}

fn parse_layout_body(
    body: TokenView<'_, '_>,
    do_token: &SpannedToken<'_>,
) -> ParseResult<SyntaxExpr> {
    let statements = LayoutView::new(body)
        .statements(LayoutBase::FirstLine)
        .map_err(|error| {
            let message = if error.message().contains("expected at least") {
                format!(
                    "layout line {} is indented less than the first statement",
                    error.line()
                )
            } else {
                error.message().to_owned()
            };
            vec![Diagnostic::error(error.line(), message)]
        })?;
    if statements.is_empty() {
        return Err(error_at_token(
            body,
            do_token,
            "layout do expression requires at least one statement",
        ));
    }
    let statements = statements
        .into_iter()
        .map(|statement| {
            let view = body
                .subview(statement.tokens())
                .expect("layout statements remain within the do body");
            parse_statement(view).map(|statement_kind| (statement.line(), statement_kind))
        })
        .collect::<ParseResult<Vec<_>>>()?;
    finish_do_statements(statements).map_err(|message| error_at_token(body, do_token, message))
}

fn parse_braced_body(
    body: TokenView<'_, '_>,
    do_token: &SpannedToken<'_>,
) -> ParseResult<SyntaxExpr> {
    if is_layout_empty(body) {
        let line = body.line_at_span(do_token.span()).unwrap_or(1);
        return Ok(empty_do_expr(line));
    }

    let mut statements = split_top_level(body, ";");
    if statements
        .first()
        .is_some_and(|view| is_layout_empty(*view))
    {
        statements.remove(0);
    }
    if statements.last().is_some_and(|view| is_layout_empty(*view)) {
        statements.pop();
    }
    if statements.is_empty() {
        return Err(error_at_token(
            body,
            do_token,
            "`do {;}` is invalid; a semicolon is not an empty computation",
        ));
    }

    let statements = statements
        .into_iter()
        .map(trim_layout)
        .map(|statement| {
            if is_layout_empty(statement) {
                return Err(error_at_token(
                    body,
                    do_token,
                    "braced do block contains an empty statement between semicolons",
                ));
            }
            let line = statement
                .first_significant()
                .and_then(|(_, token)| statement.line_at_span(token.span()))
                .unwrap_or(1);
            parse_statement(statement).map(|statement_kind| (line, statement_kind))
        })
        .collect::<ParseResult<Vec<_>>>()?;
    finish_do_statements(statements).map_err(|message| error_at_token(body, do_token, message))
}

fn finish_do_statements(
    mut statements: Vec<(usize, ParsedDoStatement)>,
) -> Result<SyntaxExpr, String> {
    let (result_line, result_statement) = statements
        .pop()
        .expect("do statement sequence should be checked as non-empty");

    let mut steps = Vec::with_capacity(statements.len());
    for (line, statement) in statements {
        let kind = match statement {
            ParsedDoStatement::Abstract(names) => DoStepKind::Abstract(names),
            ParsedDoStatement::Bind { name, operation } => DoStepKind::Bind { name, operation },
            ParsedDoStatement::ValueBind { name, value } => DoStepKind::ValueBind { name, value },
            ParsedDoStatement::Expr(expr) => DoStepKind::Then(expr),
        };
        steps.push(DoStep { line, kind });
    }

    let result = match result_statement {
        ParsedDoStatement::Expr(expr) => expr,
        ParsedDoStatement::Abstract(_) => {
            return Err(
                "do block cannot end with an abstract declaration; add its fulfillment and a final expression"
                    .to_owned(),
            );
        }
        ParsedDoStatement::Bind { .. } | ParsedDoStatement::ValueBind { .. } => {
            return Err("do block cannot end with a binding; add a final expression".to_owned());
        }
    };
    Ok(SyntaxExpr::Do(DoExpr {
        steps,
        result_line,
        result: Box::new(result),
    }))
}

fn empty_do_expr(line: usize) -> SyntaxExpr {
    SyntaxExpr::Do(DoExpr {
        steps: Vec::new(),
        result_line: line,
        result: Box::new(SyntaxExpr::Apply(
            Box::new(SyntaxExpr::Effect(vec!["r".to_owned()])),
            Box::new(SyntaxExpr::Unit),
        )),
    })
}

fn parse_statement(view: TokenView<'_, '_>) -> ParseResult<ParsedDoStatement> {
    let view = trim_layout(view);
    if starts_with_contextual_name(view, "abstract") {
        return parse_abstract(view);
    }

    if let Some(arrow) = top_level_symbols(view, "<-").into_iter().next() {
        let pattern = trim_layout(view_between(view, view.range().start(), arrow));
        let operation = trim_layout(view_between(view, arrow + 1, view.range().end()));
        let Some(name) = local_name(pattern) else {
            if let Some(keyword) = single_reserved_keyword(pattern) {
                return Err(error_at_view(pattern, reserved_keyword_message(keyword)));
            }
            return Err(error_at_view(
                pattern,
                "patterns are not yet supported in do bindings; expected a local name before `<-`",
            ));
        };
        if is_layout_empty(operation) {
            return Err(error_at_view(
                view,
                format!("do binding `{name}` requires an operation after `<-`"),
            ));
        }
        return parse_expression(operation).map(|operation| ParsedDoStatement::Bind {
            name: name.to_owned(),
            operation,
        });
    }

    if let Some(equal) = top_level_symbols(view, "=").into_iter().next() {
        let pattern = trim_layout(view_between(view, view.range().start(), equal));
        let value = trim_layout(view_between(view, equal + 1, view.range().end()));
        let Some(name) = local_name(pattern) else {
            if let Some(keyword) = single_reserved_keyword(pattern) {
                return Err(error_at_view(pattern, reserved_keyword_message(keyword)));
            }
            return Err(error_at_view(
                pattern,
                "patterns are not yet supported in do value bindings; expected a local name before `=`",
            ));
        };
        if is_layout_empty(value) {
            return Err(error_at_view(
                view,
                format!("do value binding `{name}` requires a value after `=`"),
            ));
        }
        return parse_expression(value).map(|value| ParsedDoStatement::ValueBind {
            name: name.to_owned(),
            value,
        });
    }

    if let Ok(expr) = parse_expression(view) {
        return Ok(ParsedDoStatement::Expr(expr));
    }

    let arrows = top_level_symbols(view, "->");
    if let Some(first) = arrows.first().copied()
        && is_layout_empty(trim_layout(view_between(view, view.range().start(), first)))
    {
        return Err(error_at_view(
            view,
            "a do forward binding requires an operation before `->`",
        ));
    }
    for arrow in arrows.into_iter().rev() {
        let operation = trim_layout(view_between(view, view.range().start(), arrow));
        let Ok(operation) = parse_expression(operation) else {
            continue;
        };
        let name = trim_layout(view_between(view, arrow + 1, view.range().end()));
        let Some(name) = local_name(name) else {
            if let Some(keyword) = single_reserved_keyword(name) {
                return Err(error_at_view(name, reserved_keyword_message(keyword)));
            }
            return Err(error_at_view(
                view,
                "a do forward binding requires exactly one local name after `->`",
            ));
        };
        return Ok(ParsedDoStatement::Bind {
            name: name.to_owned(),
            operation,
        });
    }

    parse_expression(view).map(ParsedDoStatement::Expr)
}

fn parse_abstract(view: TokenView<'_, '_>) -> ParseResult<ParsedDoStatement> {
    let (abstract_index, abstract_token) = view
        .first_significant()
        .expect("abstract statement has a leading token");
    let names = trim_layout(view_between(view, abstract_index + 1, view.range().end()));
    let names = split_top_level(names, ",")
        .into_iter()
        .map(trim_layout)
        .map(|name| {
            local_name(name)
                .filter(|name| *name != "_")
                .map(str::to_owned)
                .ok_or_else(|| {
                    if local_name(name) == Some("_") {
                        error_at_view(
                            name,
                            "do abstract declaration cannot use the inaccessible `_` name",
                        )
                    } else if let Some(keyword) = single_reserved_keyword(name) {
                        error_at_view(name, reserved_keyword_message(keyword))
                    } else {
                        error_at_token(
                            view,
                            abstract_token,
                            "do abstract declaration requires one or more comma-separated local names",
                        )
                    }
                })
        })
        .collect::<ParseResult<Vec<_>>>()?;
    if names.is_empty() {
        return Err(error_at_token(
            view,
            abstract_token,
            "do abstract declaration requires one or more comma-separated local names",
        ));
    }
    Ok(ParsedDoStatement::Abstract(names))
}

fn starts_with_contextual_name(view: TokenView<'_, '_>, expected: &str) -> bool {
    view.first_significant().is_some_and(|(index, token)| {
        token_is_name(token, expected)
            && (index == view.range().start() || token.leading() != LeadingTrivia::Joint)
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

fn split_top_level<'lex, 'source>(
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

fn local_name<'source>(view: TokenView<'_, 'source>) -> Option<&'source str> {
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
    (valid && significant.next().is_none() && canonical_keyword(name).is_none()).then_some(*name)
}

fn token_is_name(token: &SpannedToken<'_>, expected: &str) -> bool {
    matches!(token.kind(), TokenKind::Name(name) if *name == expected)
}

fn trim_layout<'lex, 'source>(view: TokenView<'lex, 'source>) -> TokenView<'lex, 'source> {
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
    view.slice(leading..tokens.len().saturating_sub(trailing))
        .expect("trimming layout tokens preserves an ordered range")
}

fn is_layout_empty(view: TokenView<'_, '_>) -> bool {
    view.tokens()
        .iter()
        .all(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
}

fn view_between<'lex, 'source>(
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
