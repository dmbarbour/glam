use chumsky::prelude::*;

use super::input::{TokenRange, TokenView};
use super::lexical::TokenKind;

#[allow(
    dead_code,
    reason = "token-native layout is consumed as production grammars migrate after phase 2"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutBase {
    FirstLine,
    Indentation(usize),
}

#[allow(
    dead_code,
    reason = "token-native layout is consumed as production grammars migrate after phase 2"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LayoutLine {
    line: usize,
    indentation: usize,
    tokens: TokenRange,
}

#[allow(
    dead_code,
    reason = "token-native layout is consumed as production grammars migrate after phase 2"
)]
impl LayoutLine {
    pub(super) fn line(self) -> usize {
        self.line
    }

    pub(super) fn indentation(self) -> usize {
        self.indentation
    }

    pub(super) fn tokens(self) -> TokenRange {
        self.tokens
    }
}

#[allow(
    dead_code,
    reason = "token-native layout is consumed as production grammars migrate after phase 2"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LayoutStatement {
    line: usize,
    tokens: TokenRange,
}

#[allow(
    dead_code,
    reason = "token-native layout is consumed as production grammars migrate after phase 2"
)]
impl LayoutStatement {
    pub(super) fn line(self) -> usize {
        self.line
    }

    pub(super) fn tokens(self) -> TokenRange {
        self.tokens
    }
}

#[allow(
    dead_code,
    reason = "token-native layout is consumed as production grammars migrate after phase 2"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LayoutError {
    line: usize,
    message: String,
}

#[allow(
    dead_code,
    reason = "token-native layout is consumed as production grammars migrate after phase 2"
)]
impl LayoutError {
    pub(super) fn line(&self) -> usize {
        self.line
    }

    pub(super) fn message(&self) -> &str {
        &self.message
    }
}

#[allow(
    dead_code,
    reason = "token-native layout is consumed as production grammars migrate after phase 2"
)]
#[derive(Debug, Clone, Copy)]
pub(super) struct LayoutView<'lex, 'source> {
    tokens: TokenView<'lex, 'source>,
}

#[allow(
    dead_code,
    reason = "token-native layout is consumed as production grammars migrate after phase 2"
)]
impl<'lex, 'source> LayoutView<'lex, 'source> {
    pub(super) fn new(tokens: TokenView<'lex, 'source>) -> Self {
        Self { tokens }
    }

    pub(super) fn tokens(self) -> TokenView<'lex, 'source> {
        self.tokens
    }

    /// Returns nonempty source lines at this view's delimiter depth.
    ///
    /// Line starts nested in a balanced group are skipped with that group, and
    /// multiline text is already one lexical token. Neither can accidentally
    /// establish layout for the surrounding construct.
    pub(super) fn lines(self) -> Vec<LayoutLine> {
        if self.tokens.is_empty() {
            return Vec::new();
        }

        let line_starts = self
            .tokens
            .top_level()
            .filter_map(|indexed| match indexed.token().kind() {
                TokenKind::LineStart { indentation } => {
                    Some((indexed.index(), *indentation, indexed.token().span()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut lines = Vec::with_capacity(line_starts.len().saturating_add(1));

        if line_starts
            .first()
            .is_none_or(|(index, _, _)| *index > self.tokens.range().start())
        {
            let start = self.tokens.range().start();
            let end = line_starts
                .first()
                .map_or(self.tokens.range().end(), |(index, _, _)| *index);
            if start < end
                && let Some(token) = self.tokens.token_at(start)
            {
                let line = self.tokens.line_at_span(token.span()).unwrap_or(1);
                let indentation = self.tokens.line_indentation_at(start).unwrap_or(0);
                lines.push(LayoutLine {
                    line,
                    indentation,
                    tokens: TokenRange::new(start, end)
                        .expect("ordered token boundaries form a range"),
                });
            }
        }

        for (position, (line_start, indentation, span)) in line_starts.iter().enumerate() {
            let start = line_start + 1;
            let end = line_starts
                .get(position + 1)
                .map_or(self.tokens.range().end(), |(index, _, _)| *index);
            if start < end {
                lines.push(LayoutLine {
                    line: self.tokens.line_at_span(*span).unwrap_or(1),
                    indentation: *indentation,
                    tokens: TokenRange::new(start, end)
                        .expect("ordered token boundaries form a range"),
                });
            }
        }

        lines
    }

    /// Groups lines into statements according to one construct-selected base.
    ///
    /// A line at the base begins a statement and a deeper line continues it.
    /// A line below the base is rejected, except that a closer-only line stays
    /// with the preceding statement because delimiter ownership is lexical.
    pub(super) fn statements(self, base: LayoutBase) -> Result<Vec<LayoutStatement>, LayoutError> {
        let lines = self.lines();
        let Some(first) = lines.first().copied() else {
            return Ok(Vec::new());
        };
        let base = match base {
            LayoutBase::FirstLine => first.indentation,
            LayoutBase::Indentation(indentation) => indentation,
        };
        let mut statements: Vec<LayoutStatement> = Vec::new();

        for line in lines {
            let closer_only = self.line_is_closer_only(line);
            if line.indentation < base && !closer_only {
                return Err(LayoutError {
                    line: line.line,
                    message: format!(
                        "layout line is indented {} spaces; expected at least {base}",
                        line.indentation
                    ),
                });
            }

            if line.indentation == base && !closer_only {
                statements.push(LayoutStatement {
                    line: line.line,
                    tokens: line.tokens,
                });
            } else if let Some(statement) = statements.last_mut() {
                statement.tokens = TokenRange::new(statement.tokens.start(), line.tokens.end())
                    .expect("continuation extends an ordered statement range");
            } else {
                return Err(LayoutError {
                    line: line.line,
                    message: "layout block begins with a continuation line".to_owned(),
                });
            }
        }

        Ok(statements)
    }

    fn line_is_closer_only(self, line: LayoutLine) -> bool {
        let Some(view) = self.tokens.subview(line.tokens) else {
            return false;
        };
        let mut tokens = view
            .top_level()
            .filter(|token| !matches!(token.token().kind(), TokenKind::LineStart { .. }));
        let Some(first) = tokens.next() else {
            return false;
        };
        matches!(first.token().kind(), TokenKind::Close { .. })
            && tokens.all(|token| matches!(token.token().kind(), TokenKind::Close { .. }))
    }
}

pub(super) fn legacy_glam_name<'src>()
-> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    text::ascii::ident().try_map(|name: &str, span| {
        if name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
        {
            Ok(name.to_owned())
        } else {
            Err(Rich::custom(span, "expected name"))
        }
    })
}

pub(super) fn legacy_local_name<'src>()
-> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    choice((
        just('_')
            .ignore_then(legacy_glam_name())
            .map(|name| format!("_{name}")),
        just('_').to("_".to_owned()),
        legacy_glam_name(),
    ))
}

pub(super) fn legacy_whitespace0<'src>()
-> impl Parser<'src, &'src str, (), extra::Err<Rich<'src, char>>> {
    one_of(" \r\n").repeated().ignored()
}

pub(super) fn legacy_whitespace1<'src>()
-> impl Parser<'src, &'src str, (), extra::Err<Rich<'src, char>>> {
    one_of(" \r\n").repeated().at_least(1).ignored()
}

pub(super) fn legacy_is_glam_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\r' | '\n')
}

pub(super) fn legacy_first_word(text: &str) -> Option<&str> {
    text.split(legacy_is_glam_whitespace).next()
}

pub(super) fn legacy_strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quoted = false;
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index..].starts_with(b"\"\"\"") {
            // A multiline delimiter does not turn the remainder of its own
            // source line into text.
            index += 3;
            continue;
        }
        match bytes[index] {
            b'"' => quoted = !quoted,
            b'#' if !quoted => return &line[..index],
            _ => {}
        }
        index += 1;
    }

    line
}

pub(super) fn legacy_opens_multiline_text(line: &str) -> bool {
    line.trim_end().ends_with("\"\"\"")
}

pub(super) fn legacy_closes_multiline_text(line: &str) -> bool {
    line.trim_start().starts_with("\"\"\"")
}

pub(super) fn legacy_is_indented(line: &str) -> bool {
    line.starts_with(' ')
}

pub(super) fn legacy_indentation_width(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

pub(super) fn legacy_strip_indent_width(line: &str, width: usize) -> &str {
    let mut remaining = width;
    for (index, ch) in line.char_indices() {
        if remaining == 0 || ch != ' ' {
            return &line[index..];
        }
        remaining = remaining.saturating_sub(ch.len_utf8());
    }
    ""
}

pub(super) fn legacy_is_dedent_closer(trimmed: &str) -> bool {
    !trimmed.is_empty() && trimmed.chars().all(|ch| matches!(ch, '}' | ']' | ')'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LegacyLayoutStatement<'a> {
    pub(super) text: &'a str,
    /// Zero-based line offset within the supplied block text.
    pub(super) line_offset: usize,
}

/// Splits a normalized layout block without interpreting its statements.
///
/// Each non-empty, unindented line begins a statement. Indented lines,
/// dedented delimiter-only closers, and multiline text remain attached to the
/// preceding statement. Source parsing removes comment-only lines before this
/// helper receives a declaration body.
pub(super) fn legacy_split_layout_statements(
    text: &str,
) -> Result<Vec<LegacyLayoutStatement<'_>>, String> {
    let lines = legacy_split_lines(text);
    let mut starts = Vec::new();
    let mut in_multiline_text = false;

    for line in lines {
        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if in_multiline_text {
            in_multiline_text = !legacy_closes_multiline_text(line.text);
            continue;
        }
        if legacy_opens_multiline_text(line.text) {
            in_multiline_text = true;
        }
        if legacy_is_indented(line.text) || legacy_is_dedent_closer(trimmed) {
            if starts.is_empty() {
                return Err("layout block begins with a continuation line".to_owned());
            }
            continue;
        }
        starts.push((line.start, line.number - 1));
    }

    let mut statements = Vec::with_capacity(starts.len());
    for (index, &(start, line_offset)) in starts.iter().enumerate() {
        let end = starts
            .get(index + 1)
            .map_or(text.len(), |&(next_start, _)| next_start);
        statements.push(LegacyLayoutStatement {
            text: text[start..end].trim_end(),
            line_offset,
        });
    }
    Ok(statements)
}

/// Removes the indentation established by the first non-empty block line.
/// This is needed when one layout expression is nested inside another
/// statement; top-level declaration collection has already performed the same
/// normalization for the outermost expression.
pub(super) fn legacy_dedent_layout_block(text: &str) -> Result<String, String> {
    let lines = legacy_split_lines(text);
    let base_indent = lines
        .iter()
        .find(|line| !line.text.trim().is_empty())
        .map_or(0, |line| legacy_indentation_width(line.text));
    let mut normalized = String::with_capacity(text.len());
    let mut in_multiline_text = false;

    for (index, line) in lines.into_iter().enumerate() {
        if index > 0 {
            normalized.push('\n');
        }
        let trimmed = line.text.trim();
        if !trimmed.is_empty()
            && !in_multiline_text
            && legacy_indentation_width(line.text) < base_indent
        {
            return Err(format!(
                "layout line {} is indented less than the first statement",
                line.number
            ));
        }
        normalized.push_str(legacy_strip_indent_width(line.text, base_indent));

        if in_multiline_text {
            in_multiline_text = !legacy_closes_multiline_text(line.text);
        } else if legacy_opens_multiline_text(line.text) {
            in_multiline_text = true;
        }
    }

    Ok(normalized)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LegacyPhysicalLine<'a> {
    pub(super) number: usize,
    pub(super) start: usize,
    pub(super) text: &'a str,
}

pub(super) fn legacy_split_lines(text: &str) -> Vec<LegacyPhysicalLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut number = 1;
    let bytes = text.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                lines.push(LegacyPhysicalLine {
                    number,
                    start,
                    text: &text[start..index],
                });
                index += 2;
                start = index;
                number += 1;
            }
            b'\r' | b'\n' => {
                lines.push(LegacyPhysicalLine {
                    number,
                    start,
                    text: &text[start..index],
                });
                index += 1;
                start = index;
                number += 1;
            }
            _ => index += 1,
        }
    }

    if start < text.len() || text.is_empty() {
        lines.push(LegacyPhysicalLine {
            number,
            start,
            text: &text[start..],
        });
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_statements_keep_continuations_and_line_offsets() {
        let text = ".first {\r\n  value\r\n}\r\n.second\r\n  continuation";
        let statements = legacy_split_layout_statements(text).unwrap();

        assert_eq!(
            statements,
            vec![
                LegacyLayoutStatement {
                    text: ".first {\r\n  value\r\n}",
                    line_offset: 0,
                },
                LegacyLayoutStatement {
                    text: ".second\r\n  continuation",
                    line_offset: 3,
                },
            ]
        );
    }

    #[test]
    fn layout_statements_do_not_split_multiline_text() {
        let text = ".write \"\"\"\ntext at column zero\n\"\"\"\n.next";
        let statements = legacy_split_layout_statements(text).unwrap();

        assert_eq!(statements.len(), 2);
        assert_eq!(
            statements[0].text,
            ".write \"\"\"\ntext at column zero\n\"\"\""
        );
        assert_eq!(statements[0].line_offset, 0);
        assert_eq!(statements[1].text, ".next");
        assert_eq!(statements[1].line_offset, 3);
    }

    #[test]
    fn layout_statements_reject_an_initial_continuation() {
        assert_eq!(
            legacy_split_layout_statements("  continuation").unwrap_err(),
            "layout block begins with a continuation line"
        );
    }
}
