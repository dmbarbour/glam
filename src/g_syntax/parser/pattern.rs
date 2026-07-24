//! Pattern grammar shared by pattern-bearing `.g` syntax.
//!
//! Patterns are parsed independently from expressions. Fixed quoted paths
//! become exact list patterns; computed components remain affine syntax-owned
//! expressions until their matching step.

use crate::g_syntax::keywords::{canonical_keyword, reserved_keyword_message};
use crate::g_syntax::{
    Diagnostic, SyntaxDictPatternEntry, SyntaxExpr, SyntaxKeyExpr, SyntaxPattern,
    SyntaxPatternKind, SyntaxPatternLiteral,
};
use crate::number::Number;

use super::expression_context::ExpressionContext;
use super::input::{TokenRange, TokenView};
use super::lexical::{Delimiter, LeadingTrivia, TokenKind};
use super::structural::{
    is_layout_empty, parse_expression_in_context, split_top_level, trim_layout,
};

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

    let colons = top_level_symbols(view, ":");
    if let Some(colon) = colons.first().copied() {
        return parse_tag_pattern(view, colon);
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
                if !top_level_symbols(contents, ",").is_empty() {
                    let items = parse_pattern_list_items(contents, "tuple pattern")?;
                    return Ok(tuple_pattern(items));
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
        TokenKind::Open {
            group,
            delimiter: Delimiter::Brace,
        } => {
            let Some(delimiter_group) = view.group(*group) else {
                return Err(error_at_view(
                    view,
                    "dictionary pattern refers to an unknown delimiter group",
                ));
            };
            if first_index == delimiter_group.open_token()
                && delimiter_group.close_token() == Some(last_index)
            {
                let contents = view
                    .group_contents(*group)
                    .expect("a pattern delimiter group retains its contents");
                return parse_dict_pattern(contents);
            }
        }
        _ => {}
    }

    Err(error_at_view(
        view,
        "expected a capture, literal, list pattern, quoted-path pattern, or parenthesized pattern",
    ))
}

fn parse_tag_pattern(view: TokenView<'_, '_>, colon: usize) -> ParseResult<SyntaxPattern> {
    let left = trim_layout(view_between(view, view.range().start(), colon));
    let right = trim_layout(view_between(view, colon + 1, view.range().end()));
    if is_layout_empty(right) {
        return Err(error_at_view(
            view,
            "tag pattern requires a payload pattern after `:`",
        ));
    }
    let Some((_, right_first)) = right.first_significant() else {
        unreachable!("nonempty tag payload has a first token");
    };
    if right_first.leading() != LeadingTrivia::Joint {
        return Err(error_at_view(
            view,
            "tag pattern payload must be joint with `:`",
        ));
    }

    if is_layout_empty(left) {
        let payload = parse_pattern(right)?;
        let SyntaxPatternKind::Capture(name) = &payload.kind else {
            return Err(error_at_view(
                right,
                "`:name` pattern shorthand requires one capture name",
            ));
        };
        return Ok(dict_pattern(
            vec![dict_entry(
                static_pattern_path([canonical_capture_name(name)]),
                false,
                payload,
            )],
            None,
        ));
    }

    let Some((_, colon_token)) = view
        .top_level()
        .find(|indexed| indexed.index() == colon)
        .map(|indexed| (indexed.index(), indexed.token()))
    else {
        unreachable!("selected tag colon belongs to its view");
    };
    if colon_token.leading() != LeadingTrivia::Joint {
        return Err(error_at_view(
            view,
            "tag pattern `:` must be joint with its static path",
        ));
    }
    let path = parse_static_path(left, "tag pattern")?;
    Ok(dict_pattern(
        vec![dict_entry(
            static_pattern_path(path.iter().map(String::as_str)),
            false,
            parse_pattern(right)?,
        )],
        None,
    ))
}

fn parse_dict_pattern(contents: TokenView<'_, '_>) -> ParseResult<SyntaxPattern> {
    if is_layout_empty(contents) {
        return Ok(dict_pattern(Vec::new(), None));
    }
    let members = split_top_level(contents, ",")
        .into_iter()
        .map(trim_layout)
        .collect::<Vec<_>>();
    let mut entries = Vec::new();
    let mut remainder = None;

    for (index, member) in members.iter().copied().enumerate() {
        if is_layout_empty(member) {
            if index == 0 || index + 1 == members.len() {
                continue;
            }
            return Err(error_at_view(
                contents,
                "dictionary pattern contains an empty item between commas",
            ));
        }
        let colons = top_level_symbols(member, ":");
        let Some(colon) = colons.first().copied() else {
            if remainder.is_some() {
                return Err(error_at_view(
                    member,
                    "dictionary pattern permits only one remainder",
                ));
            }
            if members[index + 1..]
                .iter()
                .copied()
                .any(|later| !is_layout_empty(later))
            {
                return Err(error_at_view(
                    member,
                    "dictionary remainder must be the final pattern item",
                ));
            }
            let pattern = parse_pattern(member)?;
            if !pattern.is_irrefutable() {
                return Err(error_at_view(
                    member,
                    "dictionary remainder must be an irrefutable pattern",
                ));
            }
            remainder = Some(Box::new(pattern));
            continue;
        };

        let path = trim_layout(view_between(member, member.range().start(), colon));
        let (path, optional) = strip_optional_dict_marker(member, path, colon)?;
        let payload = trim_layout(view_between(member, colon + 1, member.range().end()));
        if is_layout_empty(payload) {
            return Err(error_at_view(
                member,
                if optional {
                    "optional dictionary entry pattern requires a payload after `?:`"
                } else {
                    "dictionary entry pattern requires a payload after `:`"
                },
            ));
        }
        let (path, payload) = if is_layout_empty(path) {
            if optional {
                return Err(error_at_view(
                    member,
                    "optional dictionary entry pattern requires a path before `?:`",
                ));
            }
            let payload = parse_pattern(payload)?;
            let SyntaxPatternKind::Capture(name) = &payload.kind else {
                return Err(error_at_view(
                    member,
                    "`:name` dictionary shorthand requires one capture name",
                ));
            };
            (static_pattern_path([canonical_capture_name(name)]), payload)
        } else {
            (
                parse_pattern_path(path, "dictionary pattern", false)?,
                parse_pattern(payload)?,
            )
        };
        if entries
            .iter()
            .any(|entry: &SyntaxDictPatternEntry| entry.path == path)
        {
            return Err(error_at_view(
                member,
                "dictionary pattern repeats the same path expression",
            ));
        }
        entries.push(dict_entry(path, optional, payload));
    }

    Ok(dict_pattern(entries, remainder))
}

fn parse_static_path(view: TokenView<'_, '_>, context: &str) -> ParseResult<Vec<String>> {
    let tokens = view
        .top_level()
        .filter(|indexed| !matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
        .collect::<Vec<_>>();
    let mut path = Vec::new();
    let mut expect_name = true;
    for token in tokens {
        if expect_name {
            let TokenKind::Name(name) = token.token().kind() else {
                return Err(error_at_view(
                    view,
                    format!("{context} requires a static name path"),
                ));
            };
            path.push((*name).to_owned());
        } else if !matches!(token.token().kind(), TokenKind::Symbol("."))
            || token.token().leading() != LeadingTrivia::Joint
        {
            return Err(error_at_view(
                view,
                format!("{context} requires a joint static name path"),
            ));
        }
        expect_name = !expect_name;
    }
    if path.is_empty() || expect_name {
        return Err(error_at_view(
            view,
            format!("{context} requires a complete static name path"),
        ));
    }
    Ok(path)
}

fn parse_pattern_path(
    view: TokenView<'_, '_>,
    context: &str,
    leading_dot: bool,
) -> ParseResult<Vec<SyntaxKeyExpr>> {
    let tokens = view
        .top_level()
        .filter(|indexed| !matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
        .collect::<Vec<_>>();
    let mut path = Vec::new();
    let mut index = 0;
    let mut require_dot = leading_dot;

    while index < tokens.len() {
        if require_dot {
            let dot = &tokens[index];
            if !matches!(dot.token().kind(), TokenKind::Symbol("."))
                || dot.token().leading() != LeadingTrivia::Joint
            {
                return Err(error_at_view(
                    view,
                    format!("{context} requires joint `.component` path suffixes"),
                ));
            }
            index += 1;
            if index == tokens.len() {
                return Err(error_at_view(
                    view,
                    format!("{context} requires a component after `.`"),
                ));
            }
            if tokens[index].token().leading() != LeadingTrivia::Joint {
                return Err(error_at_view(
                    view,
                    format!("{context} requires joint `.component` path suffixes"),
                ));
            }
        }

        match tokens[index].token().kind() {
            TokenKind::Name(name) => path.push(SyntaxKeyExpr::Atom((*name).to_owned())),
            TokenKind::Open {
                group,
                delimiter: Delimiter::Bracket,
            } => {
                let contents = view.group_contents(*group).ok_or_else(|| {
                    error_at_view(view, format!("{context} contains an invalid key list"))
                })?;
                path.extend(parse_pattern_path_list(contents, context)?);
            }
            TokenKind::Open {
                group,
                delimiter: Delimiter::Parenthesis,
            } => {
                let contents = view.group_contents(*group).ok_or_else(|| {
                    error_at_view(view, format!("{context} contains an invalid path splice"))
                })?;
                if is_layout_empty(contents) {
                    return Err(error_at_view(
                        contents,
                        format!("{context} path splice requires an expression"),
                    ));
                }
                let expr =
                    parse_expression_in_context(contents, ExpressionContext::for_owner(contents))?;
                path.push(SyntaxKeyExpr::PathIndex(Box::new(expr)));
            }
            _ => {
                return Err(error_at_view(
                    view,
                    format!("{context} requires a name, key list, or path splice"),
                ));
            }
        }
        index += 1;
        require_dot = true;
    }

    if path.is_empty() {
        return Err(error_at_view(
            view,
            format!("{context} requires at least one path component"),
        ));
    }
    Ok(path)
}

fn parse_pattern_path_list(
    contents: TokenView<'_, '_>,
    context: &str,
) -> ParseResult<Vec<SyntaxKeyExpr>> {
    if is_layout_empty(contents) {
        return Ok(Vec::new());
    }
    let parts = split_top_level(contents, ",")
        .into_iter()
        .map(trim_layout)
        .collect::<Vec<_>>();
    let mut keys = Vec::new();
    for (index, part) in parts.iter().copied().enumerate() {
        if is_layout_empty(part) {
            if index == 0 || index + 1 == parts.len() {
                continue;
            }
            return Err(error_at_view(
                contents,
                format!("{context} contains an empty key between commas"),
            ));
        }
        let expr = parse_expression_in_context(part, ExpressionContext::for_owner(part))?;
        keys.push(match expr {
            SyntaxExpr::Atom(name) => SyntaxKeyExpr::Atom(name),
            expr => SyntaxKeyExpr::Index(Box::new(expr)),
        });
    }
    Ok(keys)
}

fn strip_optional_dict_marker<'lex, 'source>(
    member: TokenView<'lex, 'source>,
    path: TokenView<'lex, 'source>,
    colon: usize,
) -> ParseResult<(TokenView<'lex, 'source>, bool)> {
    let Some((question, token)) = path.last_significant() else {
        return Ok((path, false));
    };
    if !matches!(token.kind(), TokenKind::Symbol("?")) {
        return Ok((path, false));
    }
    if token.leading() != LeadingTrivia::Joint {
        return Err(error_at_view(
            member,
            "optional dictionary entry marker `?` must be joint with its path",
        ));
    }
    let colon_token = member
        .top_level()
        .find(|indexed| indexed.index() == colon)
        .map(|indexed| indexed.token())
        .expect("selected dictionary colon belongs to its member");
    if colon_token.leading() != LeadingTrivia::Joint {
        return Err(error_at_view(
            member,
            "optional dictionary entry marker must be written as joint `?:`",
        ));
    }
    Ok((
        trim_layout(view_between(path, path.range().start(), question)),
        true,
    ))
}

fn tuple_pattern(items: Vec<SyntaxPattern>) -> SyntaxPattern {
    dict_pattern(
        vec![dict_entry(
            static_pattern_path(["tuple"]),
            false,
            fixed_list_pattern(items),
        )],
        None,
    )
}

fn quoted_path_pattern(path: Vec<SyntaxKeyExpr>) -> SyntaxPattern {
    if path.iter().all(fixed_quoted_path_component) {
        return fixed_list_pattern(
            path.into_iter()
                .map(fixed_quoted_path_component_pattern)
                .collect(),
        );
    }
    SyntaxPattern {
        kind: SyntaxPatternKind::QuotedPath(path),
    }
}

fn fixed_quoted_path_component(component: &SyntaxKeyExpr) -> bool {
    match component {
        SyntaxKeyExpr::Atom(_) => true,
        SyntaxKeyExpr::Index(expr) => matches!(
            expr.as_ref(),
            SyntaxExpr::Unit | SyntaxExpr::Number(_) | SyntaxExpr::Text(_) | SyntaxExpr::Atom(_)
        ),
        SyntaxKeyExpr::PathIndex(_) => false,
    }
}

fn fixed_quoted_path_component_pattern(component: SyntaxKeyExpr) -> SyntaxPattern {
    let literal = match component {
        SyntaxKeyExpr::Atom(name) => SyntaxPatternLiteral::Atom(name),
        SyntaxKeyExpr::Index(expr) => match *expr {
            SyntaxExpr::Unit => SyntaxPatternLiteral::Unit,
            SyntaxExpr::Number(number) => SyntaxPatternLiteral::Number(number),
            SyntaxExpr::Text(text) => SyntaxPatternLiteral::Text(text),
            SyntaxExpr::Atom(name) => SyntaxPatternLiteral::Atom(name),
            _ => unreachable!("fixed quoted-path component was classified as a literal"),
        },
        SyntaxKeyExpr::PathIndex(_) => {
            unreachable!("fixed quoted-path component cannot be a path splice")
        }
    };
    SyntaxPattern {
        kind: SyntaxPatternKind::Literal(literal),
    }
}

fn dict_pattern(
    entries: Vec<SyntaxDictPatternEntry>,
    remainder: Option<Box<SyntaxPattern>>,
) -> SyntaxPattern {
    SyntaxPattern {
        kind: SyntaxPatternKind::Dict { entries, remainder },
    }
}

fn dict_entry(
    path: Vec<SyntaxKeyExpr>,
    optional: bool,
    pattern: SyntaxPattern,
) -> SyntaxDictPatternEntry {
    SyntaxDictPatternEntry {
        path,
        optional,
        pattern,
    }
}

fn static_pattern_path<'a>(parts: impl IntoIterator<Item = &'a str>) -> Vec<SyntaxKeyExpr> {
    parts
        .into_iter()
        .map(|name| SyntaxKeyExpr::Atom(name.to_owned()))
        .collect()
}

fn canonical_capture_name(name: &str) -> &str {
    name.strip_prefix('_').unwrap_or(name)
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

    let quote = tokens
        .first()
        .expect("a quoted pattern retains its quote token")
        .index();
    let suffix = trim_layout(view_between(view, quote + 1, view.range().end()));
    let path = parse_pattern_path(suffix, "quoted-path pattern", true)?;
    Ok(quoted_path_pattern(path))
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

fn top_level_symbols(view: TokenView<'_, '_>, expected: &str) -> Vec<usize> {
    view.top_level()
        .filter(|indexed| {
            matches!(indexed.token().kind(), TokenKind::Symbol(symbol) if *symbol == expected)
        })
        .map(|indexed| indexed.index())
        .collect()
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
