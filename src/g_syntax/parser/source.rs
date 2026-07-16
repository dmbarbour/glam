use super::super::{Declaration, Diagnostic, ParsedSource};
use super::declaration::{classify_declaration, validate_language_position};
use super::layout::{
    indentation_width, is_dedent_closer, is_indented, line_ending_diagnostics, split_lines,
    strip_comment, strip_indent_width,
};

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

    let mut diagnostics = line_ending_diagnostics(text);
    let physical_lines = split_lines(text);
    let mut declarations = Vec::new();
    let mut index = 0;

    while index < physical_lines.len() {
        let line = physical_lines[index];
        let trimmed = strip_comment(line.text).trim();

        if trimmed.is_empty() {
            index += 1;
            continue;
        }

        if is_indented(line.text) {
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

        while index < physical_lines.len() {
            let next = physical_lines[index];
            let next_trimmed = strip_comment(next.text).trim();

            if next_trimmed.is_empty() {
                index += 1;
                continue;
            }

            if !is_indented(next.text) && !is_dedent_closer(next_trimmed) {
                break;
            }

            if is_indented(next.text) {
                let next_indent = indentation_width(next.text);
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

            let next_text = strip_comment(next.text).trim_end();
            let next_text = continuation_indent
                .map(|indent| strip_indent_width(next_text, indent))
                .unwrap_or(next_trimmed);
            text.push('\n');
            text.push_str(next_text.trim_end());
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
