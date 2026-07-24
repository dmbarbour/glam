use super::super::{Declaration, Diagnostic, ParsedSource};
use super::declaration::{
    SimpleDeclaration, is_abstract_object_declaration, parse_declaration, parse_simple_declaration,
    validate_language_position,
};
use super::expression_context::{ExpressionContext, validate_expression_floor};
use super::input::{ParseSession, TokenView};
use super::layout::validate_delimited_layouts;
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
    diagnostics.extend(validate_delimited_layouts(&lexical));

    let mut declarations = Vec::with_capacity(lexical.declarations().len());
    let mut token_session = ParseSession::new(&lexical);
    for (line, view) in TokenView::declarations(&lexical) {
        let head = view
            .first_significant()
            .and_then(|(_, token)| match token.kind() {
                TokenKind::Name(name) => Some(*name),
                _ => None,
            });
        let simple = head
            .and_then(SimpleDeclaration::from_head)
            .filter(|_| !is_abstract_object_declaration(view));
        let kind = if let Some(simple) = simple {
            let (_, mut floor_diagnostics) =
                validate_expression_floor(view, ExpressionContext::for_owner(view));
            diagnostics.append(&mut floor_diagnostics);
            validate_simple_continuation_indentation(view, &mut diagnostics);
            parse_simple_declaration(view, line, simple, &mut token_session)
        } else {
            parse_declaration(view, line, &mut diagnostics)
        };
        diagnostics.extend(token_session.take_diagnostics());
        let preview = declaration_preview(view);
        declarations.push(Declaration {
            line,
            kind,
            preview,
        });
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
