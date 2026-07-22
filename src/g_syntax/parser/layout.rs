use chumsky::prelude::*;

use super::super::Diagnostic;

pub(super) fn glam_name<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>>
{
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

pub(super) fn local_name<'src>()
-> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    choice((
        just('_')
            .ignore_then(glam_name())
            .map(|name| format!("_{name}")),
        just('_').to("_".to_owned()),
        glam_name(),
    ))
}

pub(super) fn whitespace0<'src>() -> impl Parser<'src, &'src str, (), extra::Err<Rich<'src, char>>>
{
    one_of(" \r\n").repeated().ignored()
}

pub(super) fn whitespace1<'src>() -> impl Parser<'src, &'src str, (), extra::Err<Rich<'src, char>>>
{
    one_of(" \r\n").repeated().at_least(1).ignored()
}

pub(super) fn is_glam_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\r' | '\n')
}

pub(super) fn first_word(text: &str) -> Option<&str> {
    text.split(is_glam_whitespace).next()
}

pub(super) fn strip_comment(line: &str) -> &str {
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

pub(super) fn opens_multiline_text(line: &str) -> bool {
    line.trim_end().ends_with("\"\"\"")
}

pub(super) fn closes_multiline_text(line: &str) -> bool {
    line.trim_start().starts_with("\"\"\"")
}

pub(super) fn is_indented(line: &str) -> bool {
    line.starts_with(' ')
}

pub(super) fn indentation_width(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

pub(super) fn strip_indent_width(line: &str, width: usize) -> &str {
    let mut remaining = width;
    for (index, ch) in line.char_indices() {
        if remaining == 0 || ch != ' ' {
            return &line[index..];
        }
        remaining = remaining.saturating_sub(ch.len_utf8());
    }
    ""
}

pub(super) fn unsupported_whitespace_diagnostics(text: &str) -> Vec<Diagnostic> {
    split_lines(text)
        .into_iter()
        .filter_map(|line| {
            line.text
                .chars()
                .find(|ch| ch.is_whitespace() && *ch != ' ')
                .map(|ch| {
                    Diagnostic::error(
                        line.number,
                        format!(
                            "unsupported whitespace U+{:04X}; .g source permits only SP, CR, and LF",
                            ch as u32
                        ),
                    )
                })
        })
        .collect()
}

pub(super) fn is_dedent_closer(trimmed: &str) -> bool {
    !trimmed.is_empty() && trimmed.chars().all(|ch| matches!(ch, '}' | ']' | ')'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LayoutStatement<'a> {
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
pub(super) fn split_layout_statements(text: &str) -> Result<Vec<LayoutStatement<'_>>, String> {
    let lines = split_lines(text);
    let mut starts = Vec::new();
    let mut in_multiline_text = false;

    for line in lines {
        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if in_multiline_text {
            in_multiline_text = !closes_multiline_text(line.text);
            continue;
        }
        if opens_multiline_text(line.text) {
            in_multiline_text = true;
        }
        if is_indented(line.text) || is_dedent_closer(trimmed) {
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
        statements.push(LayoutStatement {
            text: text[start..end].trim_end(),
            line_offset,
        });
    }
    Ok(statements)
}

pub(super) fn line_ending_diagnostics(text: &str) -> Vec<Diagnostic> {
    let mut has_lf = false;
    let mut has_crlf = false;
    let mut has_cr = false;
    let bytes = text.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                has_crlf = true;
                index += 2;
            }
            b'\r' => {
                has_cr = true;
                index += 1;
            }
            b'\n' => {
                has_lf = true;
                index += 1;
            }
            _ => index += 1,
        }
    }

    let kinds = [has_lf, has_crlf, has_cr]
        .into_iter()
        .filter(|present| *present)
        .count();

    if kinds > 1 {
        vec![Diagnostic::warn(1, "source uses inconsistent line endings")]
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PhysicalLine<'a> {
    pub(super) number: usize,
    pub(super) start: usize,
    pub(super) text: &'a str,
}

pub(super) fn split_lines(text: &str) -> Vec<PhysicalLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut number = 1;
    let bytes = text.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                lines.push(PhysicalLine {
                    number,
                    start,
                    text: &text[start..index],
                });
                index += 2;
                start = index;
                number += 1;
            }
            b'\r' | b'\n' => {
                lines.push(PhysicalLine {
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
        lines.push(PhysicalLine {
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
        let statements = split_layout_statements(text).unwrap();

        assert_eq!(
            statements,
            vec![
                LayoutStatement {
                    text: ".first {\r\n  value\r\n}",
                    line_offset: 0,
                },
                LayoutStatement {
                    text: ".second\r\n  continuation",
                    line_offset: 3,
                },
            ]
        );
    }

    #[test]
    fn layout_statements_do_not_split_multiline_text() {
        let text = ".write \"\"\"\ntext at column zero\n\"\"\"\n.next";
        let statements = split_layout_statements(text).unwrap();

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
            split_layout_statements("  continuation").unwrap_err(),
            "layout block begins with a continuation line"
        );
    }
}
