use super::super::{Declaration, Diagnostic, ParsedSource};
use super::declaration::{
    SimpleDeclaration, is_abstract_object_declaration, parse_declaration, parse_simple_declaration,
    validate_language_position,
};
use super::expression_context::{ExpressionContext, validate_expression_floor};
use super::input::{ParseSession, TokenView};
use super::layout::validate_delimited_layouts;
use super::lexical::{DeclarationSection, LexedSource, TokenKind, lex_source};
use super::logical::LogicalSource;

pub fn parse_source(source: &[u8]) -> ParsedSource {
    let mut parser = StagedSourceParser::new(source);
    let mut declarations = Vec::new();
    while let Some(declaration) = parser.next_declaration() {
        declarations.push(declaration);
    }
    let diagnostics = parser.finish(&declarations);
    ParsedSource {
        declarations,
        diagnostics,
    }
}

#[cfg(test)]
pub(super) fn parse_lexed(lexical: &super::lexical::LexedSource<'_>) -> ParsedSource {
    let mut diagnostics = lexical.diagnostics().to_vec();
    if lexical.has_errors() {
        return ParsedSource {
            declarations: Vec::new(),
            diagnostics,
        };
    }
    report_orphan_continuations(lexical, &mut diagnostics);
    diagnostics.extend(validate_delimited_layouts(lexical));

    let mut declarations = Vec::with_capacity(lexical.declarations().len());
    for declaration in lexical.declarations() {
        declarations.push(parse_lexical_declaration(
            lexical,
            declaration,
            &mut diagnostics,
        ));
    }

    validate_language_position(&declarations, &mut diagnostics);

    ParsedSource {
        declarations,
        diagnostics,
    }
}

/// Declaration-at-a-time parser over one immutable lexical result.
///
/// Macro expansion will eventually replace selected logical declaration items
/// before this stage. Keeping lexical ownership here lets ordinary compilation
/// lower each declaration before parsing the next without rescanning source.
pub(in crate::g_syntax) struct StagedSourceParser<'source> {
    lexical: Option<LexedSource<'source>>,
    diagnostics: Vec<Diagnostic>,
    next_declaration: usize,
    validate_language: bool,
}

impl<'source> StagedSourceParser<'source> {
    pub(in crate::g_syntax) fn new(source: &'source [u8]) -> Self {
        let text = match std::str::from_utf8(source) {
            Ok(text) => text,
            Err(err) => {
                return Self {
                    lexical: None,
                    diagnostics: vec![Diagnostic::error(
                        1,
                        format!("source is not valid UTF-8: {err}"),
                    )],
                    next_declaration: 0,
                    validate_language: false,
                };
            }
        };

        let lexical = lex_source(text);
        debug_assert!(lexical.invariants_hold());
        let validate_language = !lexical.has_errors();
        if validate_language {
            debug_assert!(LogicalSource::from_original(&lexical).round_trips(&lexical));
        }
        let mut diagnostics = lexical.diagnostics().to_vec();
        if validate_language {
            report_orphan_continuations(&lexical, &mut diagnostics);
            diagnostics.extend(validate_delimited_layouts(&lexical));
        }
        Self {
            lexical: Some(lexical),
            diagnostics,
            next_declaration: 0,
            validate_language,
        }
    }

    pub(in crate::g_syntax) fn next_declaration(&mut self) -> Option<Declaration> {
        let lexical = self.lexical.as_ref()?;
        if lexical.has_errors() {
            return None;
        }
        let declaration = lexical.declarations().get(self.next_declaration)?;
        self.next_declaration += 1;
        Some(parse_lexical_declaration(
            lexical,
            declaration,
            &mut self.diagnostics,
        ))
    }

    pub(in crate::g_syntax) fn finish(mut self, declarations: &[Declaration]) -> Vec<Diagnostic> {
        if self.validate_language {
            validate_language_position(declarations, &mut self.diagnostics);
        }
        self.diagnostics
    }
}

fn parse_lexical_declaration(
    lexical: &LexedSource<'_>,
    declaration: &DeclarationSection,
    diagnostics: &mut Vec<Diagnostic>,
) -> Declaration {
    let line = declaration.line();
    let view = TokenView::declaration(lexical, declaration);
    let mut token_session = ParseSession::new(lexical);
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
        validate_simple_continuation_indentation(view, diagnostics);
        parse_simple_declaration(view, line, simple, &mut token_session)
    } else {
        parse_declaration(view, line, diagnostics)
    };
    diagnostics.extend(token_session.into_diagnostics());
    Declaration {
        line,
        kind,
        preview: declaration_preview(view),
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
