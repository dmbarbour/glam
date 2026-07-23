use super::super::{Declaration, Diagnostic, ParsedSource};
use super::declaration::{
    SimpleDeclaration, classify_legacy_declaration, parse_simple_declaration,
    validate_language_position,
};
use super::input::{ParseSession, TokenView};
use super::layout::{
    legacy_closes_multiline_text, legacy_indentation_width, legacy_is_dedent_closer,
    legacy_is_indented, legacy_opens_multiline_text, legacy_split_lines, legacy_strip_comment,
    legacy_strip_indent_width,
};
use super::lexical::{TokenKind, lex_source};

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
    report_orphan_continuations(&lexical, &mut diagnostics);

    let mut declarations = Vec::with_capacity(lexical.declarations().len());
    let mut token_session = ParseSession::new(&lexical);
    for (line, view) in TokenView::declarations(&lexical) {
        let head = view
            .first_significant()
            .and_then(|(_, token)| match token.kind() {
                TokenKind::Name(name) => Some(*name),
                _ => None,
            });
        let simple = head.and_then(SimpleDeclaration::from_head);
        let (kind, text) = if let Some(simple) = simple {
            validate_simple_continuation_indentation(view, &mut diagnostics);
            let kind = parse_simple_declaration(view, line, simple, &mut token_session);
            let text = declaration_preview(view);
            (kind, text)
        } else {
            let text = legacy_normalize_declaration(
                view.source_text().unwrap_or(""),
                line,
                &mut diagnostics,
            );
            let kind = classify_legacy_declaration(&text, line, &mut diagnostics);
            (kind, text)
        };
        diagnostics.extend(token_session.take_diagnostics());
        declarations.push(Declaration { line, kind, text });
    }

    validate_language_position(&declarations, &mut diagnostics);

    ParsedSource {
        declarations,
        diagnostics,
    }
}

fn report_orphan_continuations(
    lexical: &super::lexical::LexedSource<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let first_declaration = lexical
        .declarations()
        .first()
        .map_or(lexical.tokens().len(), |declaration| {
            declaration.tokens().start
        });
    for token in &lexical.tokens()[..first_declaration] {
        if matches!(
            token.kind(),
            TokenKind::LineStart { indentation } if *indentation > 0
        ) {
            diagnostics.push(Diagnostic::error(
                lexical.line_at_byte(token.span().start()).unwrap_or(1),
                "continuation line without a preceding declaration",
            ));
        }
    }
}

fn declaration_preview(view: TokenView<'_, '_>) -> String {
    view.source_text()
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned()
}

fn validate_simple_continuation_indentation(
    view: TokenView<'_, '_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut continuation_indent = None;
    for token in view.tokens().iter().skip(1) {
        let TokenKind::LineStart { indentation } = token.kind() else {
            continue;
        };
        match continuation_indent {
            Some(base) if *indentation < base => {
                diagnostics.push(Diagnostic::error(
                    view.line_at_span(token.span()).unwrap_or(1),
                    "continuation indentation must align with or exceed the first continuation line",
                ));
            }
            None => continuation_indent = Some(*indentation),
            _ => {}
        }
    }
}

/// Compatibility bridge for declarations whose grammars still consume text.
///
/// The lexer has already selected exactly one declaration section. This
/// adapter only recreates the indentation-normalized text expected by the
/// object, extend, and definition parsers; it must not rediscover declaration
/// boundaries. Delete it after those parsers migrate in Phase 8.
fn legacy_normalize_declaration(
    source: &str,
    first_line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> String {
    let physical_lines = legacy_split_lines(source);
    let Some(first) = physical_lines.first() else {
        return String::new();
    };
    let mut text = legacy_strip_comment(first.text).trim().to_owned();
    let mut continuation_indent = None;
    let mut in_multiline_text = legacy_opens_multiline_text(&text);

    for next in physical_lines.iter().skip(1) {
        let line = first_line + next.number - 1;
        let raw_trimmed = next.text.trim();

        if in_multiline_text {
            if raw_trimmed.is_empty() || raw_trimmed.starts_with('#') {
                continue;
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
            continue;
        }

        let next_trimmed = legacy_strip_comment(next.text).trim();
        if next_trimmed.is_empty() {
            continue;
        }

        if !legacy_is_indented(next.text) && !legacy_is_dedent_closer(next_trimmed) {
            diagnostics.push(Diagnostic::error(
                line,
                "continuation line must be indented within its declaration",
            ));
        }

        if legacy_is_indented(next.text) {
            let next_indent = legacy_indentation_width(next.text);
            match continuation_indent {
                Some(indent) if next_indent < indent => {
                    diagnostics.push(Diagnostic::error(
                        line,
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
    }

    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_adapter_normalizes_one_selected_declaration_without_grouping() {
        let mut diagnostics = Vec::new();
        let text = legacy_normalize_declaration(
            "value = do\n    .first # comment\n    .second",
            7,
            &mut diagnostics,
        );

        assert_eq!(text, "value = do\n.first\n.second");
        assert_eq!(diagnostics, []);
    }

    #[test]
    fn legacy_adapter_keeps_dedent_closers_in_the_selected_declaration() {
        let mut diagnostics = Vec::new();
        let text = legacy_normalize_declaration("value = (\n  42\n)", 1, &mut diagnostics);

        assert_eq!(text, "value = (\n42\n)");
        assert_eq!(diagnostics, []);
    }
}
