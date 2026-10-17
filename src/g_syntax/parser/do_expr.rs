//! `do` statement and block grammar.
//!
//! Both layout and braced forms consume borrowed ranges from the source-wide
//! lexical pass. A statement expression is handed back to the complete token
//! expression parser; no source substring is normalized or re-lexed.

use crate::g_syntax::{DoExpr, DoStep, DoStepKind, SyntaxExpr, SyntaxPattern};

use super::input::{TokenRange, TokenView};
use super::lexical::TokenKind;
use super::pattern::{parse_pattern, pattern_binder};
use super::structural::{keyword_head, trim_layout};

pub(in crate::g_syntax::parser) enum ParsedDoStatement {
    Declare(Vec<String>),
    Bind {
        pattern: SyntaxPattern,
        operation: SyntaxExpr,
    },
    ValueBind {
        pattern: SyntaxPattern,
        value: SyntaxExpr,
    },
    Expr(SyntaxExpr),
}

/// Whether a continuation line reads as a statement of its own: a
/// `declare`, a binding by `<-` or `=`, or an operation bound by `->` to a
/// pattern. The last is judged by whether a pattern follows the line's
/// first top-level `->`, which only diagnoses misaligned layout.
pub(in crate::g_syntax::parser) fn line_begins_do_statement(view: TokenView<'_, '_>) -> bool {
    keyword_head(view, "declare").is_some()
        || pattern_binder(view).is_some()
        || top_level_symbols(view, "->")
            .into_iter()
            .next()
            .is_some_and(|arrow| {
                parse_pattern(trim_layout(view_between(
                    view,
                    arrow + 1,
                    view.range().end(),
                )))
                .is_ok()
            })
}

pub(in crate::g_syntax::parser) fn finish_do_statements(
    mut statements: Vec<(usize, ParsedDoStatement)>,
) -> Result<SyntaxExpr, String> {
    let (result_line, result_statement) = statements
        .pop()
        .expect("do statement sequence should be checked as non-empty");

    let mut steps = Vec::with_capacity(statements.len());
    for (line, statement) in statements {
        let kind = match statement {
            ParsedDoStatement::Declare(names) => DoStepKind::Declare(names),
            ParsedDoStatement::Bind { pattern, operation } => {
                DoStepKind::Bind { pattern, operation }
            }
            ParsedDoStatement::ValueBind { pattern, value } => {
                DoStepKind::ValueBind { pattern, value }
            }
            ParsedDoStatement::Expr(expr) => DoStepKind::Then(expr),
        };
        steps.push(DoStep { line, kind });
    }

    let result = match result_statement {
        ParsedDoStatement::Expr(expr) => expr,
        ParsedDoStatement::Declare(_) => {
            return Err(
                "do block cannot end with a `declare` statement; add its fulfillment and a final expression"
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

pub(in crate::g_syntax::parser) fn empty_do_expr(line: usize) -> SyntaxExpr {
    SyntaxExpr::Do(DoExpr {
        steps: Vec::new(),
        result_line: line,
        result: Box::new(SyntaxExpr::Apply(
            Box::new(SyntaxExpr::Effect(vec!["r".to_owned()])),
            Box::new(SyntaxExpr::Unit),
        )),
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

fn view_between<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    start: usize,
    end: usize,
) -> TokenView<'lex, 'source> {
    view.subview(TokenRange::new(start, end).expect("ordered token indices form a range"))
        .expect("subexpression range remains within its source view")
}

#[cfg(test)]
mod tests;
