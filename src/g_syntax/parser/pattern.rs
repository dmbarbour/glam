//! Pattern grammar shared by pattern-bearing `.g` syntax.
//!
//! The initial slice supports captures, the inaccessible wildcard, and
//! redundant parenthesized grouping. Structural patterns are added here
//! without teaching the ordinary expression parser about pattern semantics.

use crate::g_syntax::keywords::{canonical_keyword, reserved_keyword_message};
use crate::g_syntax::{Diagnostic, SyntaxPattern, SyntaxPatternKind};

use super::input::{TokenRange, TokenView};
use super::lexical::{Delimiter, TokenKind};
use super::structural::trim_layout;

type ParseResult<T> = Result<T, Vec<Diagnostic>>;

pub(in crate::g_syntax::parser) fn parse_pattern(
    view: TokenView<'_, '_>,
) -> ParseResult<SyntaxPattern> {
    let view = trim_layout(view);
    let Some((first_index, first)) = view.first_significant() else {
        return Err(error_at_view(view, "expected a pattern"));
    };
    let (last_index, _) = view
        .last_significant()
        .expect("a pattern with a first token has a last token");

    let as_indices = view
        .top_level()
        .filter(|indexed| matches!(indexed.token().kind(), TokenKind::Name(name) if *name == "as"))
        .map(|indexed| indexed.index())
        .collect::<Vec<_>>();
    if !as_indices.is_empty() {
        let mut start = view.range().start();
        let mut parts = Vec::with_capacity(as_indices.len() + 1);
        for as_index in as_indices {
            let part = trim_layout(view_between(view, start, as_index));
            if part.first_significant().is_none() {
                return Err(error_at_view(
                    view,
                    "pattern `as` requires a pattern on both sides",
                ));
            }
            parts.push(part);
            start = as_index + 1;
        }
        let final_part = trim_layout(view_between(view, start, view.range().end()));
        if final_part.first_significant().is_none() {
            return Err(error_at_view(
                view,
                "pattern `as` requires a pattern on both sides",
            ));
        }
        parts.push(final_part);

        let mut parts = parts.into_iter();
        let mut pattern = parse_pattern(parts.next().expect("as pattern has a left side"))?;
        for right in parts {
            pattern = SyntaxPattern {
                kind: SyntaxPatternKind::As(Box::new(pattern), Box::new(parse_pattern(right)?)),
            };
        }
        return Ok(pattern);
    }

    match first.kind() {
        TokenKind::Name(name) if first_index == last_index => {
            if let Some(keyword) = canonical_keyword(name) {
                return Err(error_at_view(view, reserved_keyword_message(keyword)));
            }
            if *name == "_" {
                return Ok(SyntaxPattern::wildcard());
            }
            if is_local_name(name) {
                return Ok(SyntaxPattern::capture(*name));
            }
        }
        TokenKind::Open {
            group,
            delimiter: Delimiter::Parenthesis,
        } => {
            let Some(delimiter_group) = view.group(*group) else {
                return Err(error_at_view(
                    view,
                    "parenthesized pattern refers to an unknown delimiter group",
                ));
            };
            if first_index == delimiter_group.open_token()
                && delimiter_group.close_token() == Some(last_index)
            {
                let contents = trim_layout(
                    view.group_contents(*group)
                        .expect("a pattern delimiter group retains its contents"),
                );
                if contents.first_significant().is_none() {
                    return Err(error_at_view(view, "unit patterns are not supported yet"));
                }
                return parse_pattern(contents).map(|pattern| SyntaxPattern {
                    kind: SyntaxPatternKind::Group(Box::new(pattern)),
                });
            }
        }
        _ => {}
    }

    Err(error_at_view(
        view,
        "pattern currently supports only local names, `_`, `P as Q`, or parenthesized patterns",
    ))
}

fn is_local_name(name: &str) -> bool {
    name.starts_with(|character: char| character.is_ascii_alphabetic())
        || name
            .strip_prefix('_')
            .is_some_and(|rest| rest.starts_with(|character: char| character.is_ascii_alphabetic()))
}

fn error_at_view(view: TokenView<'_, '_>, message: impl Into<String>) -> Vec<Diagnostic> {
    let line = view
        .first_significant()
        .and_then(|(_, token)| view.line_at_span(token.span()))
        .unwrap_or(1);
    vec![Diagnostic::error(line, message)]
}

fn view_between<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    start: usize,
    end: usize,
) -> TokenView<'lex, 'source> {
    view.subview(TokenRange::new(start, end).expect("ordered token indices form a range"))
        .expect("pattern range remains within its source view")
}
