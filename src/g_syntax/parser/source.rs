use super::super::{Declaration, Diagnostic, ParsedSource};
use super::declaration::{classify_declaration, validate_language_position};
use super::layout::{
    legacy_closes_multiline_text, legacy_indentation_width, legacy_is_dedent_closer,
    legacy_is_indented, legacy_opens_multiline_text, legacy_split_lines, legacy_strip_comment,
    legacy_strip_indent_width,
};
use super::lexical::lex_source;

pub fn parse_source(source: &[u8]) -> ParsedSource {
    let text = match std::str::from_utf8(source) {
        Ok(text) => text,
        Err(err) => {
            return ParsedSource {
                declarations: Vec::new(),
                diagnostics: vec![Diagnostic::error(
                    1,
                    format!("source is not valid UTF-8: {err}"),
                )],
            };
        }
    };

    let lexical = lex_source(text);
    debug_assert!(lexical.invariants_hold());
    let has_lexical_errors = lexical.has_errors();
    let mut diagnostics = lexical.diagnostics().to_vec();
    if has_lexical_errors {
        return ParsedSource {
            declarations: Vec::new(),
            diagnostics,
        };
    }
    let physical_lines = legacy_split_lines(text);
    let mut declarations = Vec::new();
    let mut index = 0;

    while index < physical_lines.len() {
        let line = physical_lines[index];
        let trimmed = legacy_strip_comment(line.text).trim();

        if trimmed.is_empty() {
            index += 1;
            continue;
        }

        if legacy_is_indented(line.text) {
            diagnostics.push(Diagnostic::error(
                line.number,
                "continuation line without a preceding declaration",
            ));
            index += 1;
            continue;
        }

        let start_line = line.number;
        let mut text = String::from(trimmed);
        index += 1;
        let mut continuation_indent = None;
        let mut in_multiline_text = legacy_opens_multiline_text(&text);

        while index < physical_lines.len() {
            let next = physical_lines[index];
            let raw_trimmed = next.text.trim();

            if in_multiline_text {
                if raw_trimmed.is_empty() || raw_trimmed.starts_with('#') {
                    index += 1;
                    continue;
                }
                if !legacy_is_indented(next.text) {
                    break;
                }

                let closes_text = legacy_closes_multiline_text(next.text);
                let source_line = if closes_text {
                    legacy_strip_comment(next.text).trim_end()
                } else {
                    next.text
                };
                let next_text = continuation_indent
                    .map(|indent| legacy_strip_indent_width(source_line, indent))
                    .unwrap_or_else(|| source_line.trim_start());
                text.push('\n');
                text.push_str(next_text);
                in_multiline_text = !closes_text;
                index += 1;
                continue;
            }

            let next_trimmed = legacy_strip_comment(next.text).trim();

            if next_trimmed.is_empty() {
                index += 1;
                continue;
            }

            if !legacy_is_indented(next.text) && !legacy_is_dedent_closer(next_trimmed) {
                break;
            }

            if legacy_is_indented(next.text) {
                let next_indent = legacy_indentation_width(next.text);
                match continuation_indent {
                    Some(indent) if next_indent < indent => {
                        diagnostics.push(Diagnostic::error(
                            next.number,
                            "continuation indentation must align with or exceed the first continuation line",
                        ));
                    }
                    None => continuation_indent = Some(next_indent),
                    _ => {}
                }
            }

            let next_text = legacy_strip_comment(next.text).trim_end();
            let next_text = continuation_indent
                .map(|indent| legacy_strip_indent_width(next_text, indent))
                .unwrap_or(next_trimmed);
            text.push('\n');
            text.push_str(next_text.trim_end());
            in_multiline_text = legacy_opens_multiline_text(next_text);
            index += 1;
        }

        declarations.push(Declaration {
            line: start_line,
            kind: classify_declaration(&text, start_line, &mut diagnostics),
            text,
        });
    }

    validate_language_position(&declarations, &mut diagnostics);

    ParsedSource {
        declarations,
        diagnostics,
    }
}
