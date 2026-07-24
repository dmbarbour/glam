//! Pattern grammar shared by pattern-bearing `.g` syntax.
//!
//! Patterns are parsed independently from expressions. Quoted paths become
//! exact list patterns here rather than retaining expression-level path
//! construction.

use crate::g_syntax::keywords::{canonical_keyword, reserved_keyword_message};
use crate::g_syntax::{Diagnostic, SyntaxPattern, SyntaxPatternKind, SyntaxPatternLiteral};
use crate::number::Number;

use super::input::{TokenRange, TokenView};
use super::lexical::{Delimiter, LeadingTrivia, TokenKind};
use super::structural::{is_layout_empty, split_top_level, trim_layout};

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
            let right = parse_pattern(right)?;
            if !pattern.is_irrefutable() || !right.is_irrefutable() {
                return Err(error_at_view(
                    view,
                    "`P as Q` currently requires irrefutable patterns on both sides",
                ));
            }
            pattern = SyntaxPattern {
                kind: SyntaxPatternKind::As(Box::new(pattern), Box::new(right)),
            };
        }
        return Ok(pattern);
    }

    let append_parts = split_top_level(view, "++");
    if append_parts.len() > 1 {
        return parse_list_append_pattern(view, append_parts);
    }

    if matches!(first.kind(), TokenKind::Symbol("'")) {
        return parse_quoted_pattern(view);
    }

    match first.kind() {
        TokenKind::Number(number) if first_index == last_index => {
            return Number::parse(number)
                .map(|number| SyntaxPattern {
                    kind: SyntaxPatternKind::Literal(SyntaxPatternLiteral::Number(number)),
                })
                .map_err(|error| {
                    error_at_view(view, format!("invalid number literal `{number}`: {error}"))
                });
        }
        TokenKind::Text(id) if first_index == last_index => {
            let Some(text) = view.text(*id) else {
                return Err(error_at_view(view, "pattern refers to unknown text data"));
            };
            return Ok(SyntaxPattern {
                kind: SyntaxPatternKind::Literal(SyntaxPatternLiteral::Text(
                    text.value().to_owned(),
                )),
            });
        }
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
                    return Ok(SyntaxPattern {
                        kind: SyntaxPatternKind::Literal(SyntaxPatternLiteral::Unit),
                    });
                }
                return parse_pattern(contents).map(|pattern| SyntaxPattern {
                    kind: SyntaxPatternKind::Group(Box::new(pattern)),
                });
            }
        }
        TokenKind::Open {
            group,
            delimiter: Delimiter::Bracket,
        } => {
            let Some(delimiter_group) = view.group(*group) else {
                return Err(error_at_view(
                    view,
                    "list pattern refers to an unknown delimiter group",
                ));
            };
            if first_index == delimiter_group.open_token()
                && delimiter_group.close_token() == Some(last_index)
            {
                let contents = view
                    .group_contents(*group)
                    .expect("a pattern delimiter group retains its contents");
                let items = parse_pattern_list_items(contents, "list pattern")?;
                return Ok(fixed_list_pattern(items));
            }
        }
        _ => {}
    }

    Err(error_at_view(
        view,
        "expected a capture, literal, list pattern, quoted-path pattern, or parenthesized pattern",
    ))
}

fn parse_list_append_pattern(
    view: TokenView<'_, '_>,
    parts: Vec<TokenView<'_, '_>>,
) -> ParseResult<SyntaxPattern> {
    let mut prefix = Vec::new();
    let mut middle = None;
    let mut suffix = Vec::new();

    for part in parts.into_iter().map(trim_layout) {
        if is_layout_empty(part) {
            return Err(error_at_view(
                view,
                "list pattern `++` requires a pattern on both sides",
            ));
        }
        let pattern = parse_pattern(part)?;
        match pattern {
            SyntaxPattern {
                kind:
                    SyntaxPatternKind::List {
                        prefix: fixed,
                        middle: None,
                        suffix: fixed_suffix,
                    },
            } => {
                let target = if middle.is_some() {
                    &mut suffix
                } else {
                    &mut prefix
                };
                target.extend(fixed);
                target.extend(fixed_suffix);
            }
            SyntaxPattern {
                kind: SyntaxPatternKind::Literal(SyntaxPatternLiteral::Text(text)),
            } => {
                let target = if middle.is_some() {
                    &mut suffix
                } else {
                    &mut prefix
                };
                target.extend(text.bytes().map(number_pattern_from_byte));
            }
            pattern if pattern.is_irrefutable() && middle.is_none() => {
                middle = Some(Box::new(pattern));
            }
            _ if middle.is_some() => {
                return Err(error_at_view(
                    part,
                    "list patterns permit only one variable-length segment",
                ));
            }
            _ => {
                return Err(error_at_view(
                    part,
                    "a variable-length list segment must be an irrefutable pattern",
                ));
            }
        }
    }

    Ok(SyntaxPattern {
        kind: SyntaxPatternKind::List {
            prefix,
            middle,
            suffix,
        },
    })
}

fn parse_quoted_pattern(view: TokenView<'_, '_>) -> ParseResult<SyntaxPattern> {
    let tokens = view
        .top_level()
        .filter(|indexed| !matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
        .collect::<Vec<_>>();

    if tokens.len() == 2
        && matches!(tokens[1].token().kind(), TokenKind::Name(_))
        && tokens[1].token().leading() == LeadingTrivia::Joint
    {
        let TokenKind::Name(name) = tokens[1].token().kind() else {
            unreachable!("atom pattern was checked as a name");
        };
        return Ok(SyntaxPattern {
            kind: SyntaxPatternKind::Literal(SyntaxPatternLiteral::Atom((*name).to_owned())),
        });
    }

    let mut items = Vec::new();
    let mut index = 1;
    while index < tokens.len() {
        if !matches!(tokens[index].token().kind(), TokenKind::Symbol("."))
            || tokens[index].token().leading() != LeadingTrivia::Joint
        {
            return Err(error_at_view(
                view,
                "quoted-path patterns require joint `.component` suffixes",
            ));
        }
        index += 1;
        let Some(component) = tokens.get(index) else {
            return Err(error_at_view(
                view,
                "quoted-path pattern requires a component after `.`",
            ));
        };
        if component.token().leading() != LeadingTrivia::Joint {
            return Err(error_at_view(
                view,
                "quoted-path patterns require joint `.component` suffixes",
            ));
        }
        match component.token().kind() {
            TokenKind::Name(name) => items.push(SyntaxPattern {
                kind: SyntaxPatternKind::Literal(SyntaxPatternLiteral::Atom((*name).to_owned())),
            }),
            TokenKind::Open {
                group,
                delimiter: Delimiter::Bracket,
            } => {
                let contents = view
                    .group_contents(*group)
                    .ok_or_else(|| error_at_view(view, "invalid quoted-path pattern list"))?;
                let path_items = parse_pattern_list_items(contents, "quoted-path pattern")?;
                if path_items
                    .iter()
                    .any(|pattern| !matches!(pattern.kind, SyntaxPatternKind::Literal(_)))
                {
                    return Err(error_at_view(
                        contents,
                        "quoted-path pattern components must be literals",
                    ));
                }
                items.extend(path_items);
            }
            TokenKind::Open {
                delimiter: Delimiter::Parenthesis,
                ..
            } => {
                return Err(error_at_view(
                    view,
                    "quoted-path patterns cannot contain computed path splices",
                ));
            }
            _ => {
                return Err(error_at_view(
                    view,
                    "quoted-path pattern components must be names or literal lists",
                ));
            }
        }
        index += 1;
    }

    if items.is_empty() {
        return Err(error_at_view(
            view,
            "quoted-path pattern requires at least one path suffix",
        ));
    }
    Ok(fixed_list_pattern(items))
}

fn parse_pattern_list_items(
    contents: TokenView<'_, '_>,
    context: &str,
) -> ParseResult<Vec<SyntaxPattern>> {
    if is_layout_empty(contents) {
        return Ok(Vec::new());
    }
    let parts = split_top_level(contents, ",")
        .into_iter()
        .map(trim_layout)
        .collect::<Vec<_>>();
    let mut items = Vec::new();
    for (index, part) in parts.iter().copied().enumerate() {
        if is_layout_empty(part) {
            if index == 0 || index + 1 == parts.len() {
                continue;
            }
            return Err(error_at_view(
                contents,
                format!("{context} contains an empty item between commas"),
            ));
        }
        items.push(parse_pattern(part)?);
    }
    Ok(items)
}

fn fixed_list_pattern(items: Vec<SyntaxPattern>) -> SyntaxPattern {
    SyntaxPattern {
        kind: SyntaxPatternKind::List {
            prefix: items,
            middle: None,
            suffix: Vec::new(),
        },
    }
}

fn number_pattern_from_byte(byte: u8) -> SyntaxPattern {
    SyntaxPattern {
        kind: SyntaxPatternKind::Literal(SyntaxPatternLiteral::Number(Number::from_u8(byte))),
    }
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
