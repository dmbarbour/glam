#[cfg(test)]
use super::super::ParsedSource;
use super::super::{
    Declaration, Diagnostic, InspectedDeclaration, InspectedDeclarationKind, InspectedSource,
};
use super::declaration::{
    SimpleDeclaration, is_abstract_object_declaration, parse_declaration, parse_simple_declaration,
    validate_language_position,
};
use super::expression_context::{ExpressionContext, validate_expression_floor};
use super::input::{ParseSession, TokenView};
use super::layout::validate_delimited_layouts;
use super::lexical::{DeclarationSection, LexedSource, TokenKind, lex_source};
use super::logical::LogicalSource;
use super::logical::{DeclarationMacroWork, EMBEDDED_MARKER, OriginalMacroInvocation};
use crate::api::Value as PublicValue;
use crate::compiler::CompileContext;
use crate::core::{Atom, Dict, Key, List, Value};
use crate::evaluation::EvaluationPumpOutcome;
use crate::number::Number;
use crate::{api::CompilationExecution, eval};

const MACRO_LOOKUP_STEP_BUDGET: usize = 256;

pub(crate) fn inspect_source(source: &[u8]) -> InspectedSource {
    let mut parser = StagedSourceParser::new(source);
    let mut declarations = Vec::new();
    while let Some(declaration) = parser.next_inspected_declaration() {
        declarations.push(declaration);
    }
    let diagnostics = parser.finish_inspection(&declarations);
    InspectedSource {
        declarations,
        diagnostics,
    }
}

#[cfg(test)]
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
/// Macro expansion replaces selected logical declaration items before this
/// owner materializes ordinary parser tokens. Keeping lexical ownership here
/// lets compilation lower each declaration before parsing the next without
/// rescanning source.
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

    #[cfg(test)]
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

    fn next_inspected_declaration(&mut self) -> Option<InspectedDeclaration> {
        let lexical = self.lexical.as_ref()?;
        if lexical.has_errors() {
            return None;
        }
        let declaration = lexical.declarations().get(self.next_declaration)?.clone();
        self.next_declaration += 1;
        match DeclarationMacroWork::from_original(lexical, &declaration) {
            Ok(Some(_)) => {
                let view = TokenView::declaration(lexical, &declaration);
                Some(InspectedDeclaration {
                    line: declaration.line(),
                    kind: InspectedDeclarationKind::MacroDeferred,
                    preview: declaration_preview(view),
                })
            }
            Err(diagnostic) => {
                let view = TokenView::declaration(lexical, &declaration);
                self.diagnostics.push(diagnostic);
                Some(InspectedDeclaration {
                    line: declaration.line(),
                    kind: InspectedDeclarationKind::MacroDeferred,
                    preview: declaration_preview(view),
                })
            }
            Ok(None) => {
                let declaration =
                    parse_lexical_declaration(lexical, &declaration, &mut self.diagnostics);
                Some(InspectedDeclaration {
                    line: declaration.line,
                    kind: InspectedDeclarationKind::Parsed(declaration.kind),
                    preview: declaration.preview,
                })
            }
        }
    }

    pub(in crate::g_syntax) fn next_expanded_declarations(
        &mut self,
        context: &CompileContext,
        prior_definitions: &Value,
        language: Option<&super::super::LanguageDecl>,
    ) -> Option<Vec<Declaration>> {
        let lexical = self.lexical.as_ref()?;
        if lexical.has_errors() {
            return None;
        }
        let declaration = lexical.declarations().get(self.next_declaration)?.clone();
        self.next_declaration += 1;
        let mut work = match DeclarationMacroWork::from_original(lexical, &declaration) {
            Ok(Some(work)) => work,
            Ok(None) => {
                return Some(vec![parse_lexical_declaration(
                    lexical,
                    &declaration,
                    &mut self.diagnostics,
                )]);
            }
            Err(diagnostic) => {
                self.diagnostics.push(diagnostic);
                return Some(Vec::new());
            }
        };
        let Some(language) = language else {
            self.diagnostics.push(Diagnostic::error(
                declaration.line(),
                "source macros require a preceding `language` declaration",
            ));
            return Some(Vec::new());
        };
        let Some(execution) = context.compilation_execution() else {
            self.diagnostics.push(Diagnostic::error(
                declaration.line(),
                "source macro expansion requires a compilation execution context",
            ));
            return Some(Vec::new());
        };
        let base_environment = match macro_lookup(
            execution,
            prior_definitions,
            &[
                Key::atom_from_text("meta"),
                Key::atom_from_text("macro"),
                Key::atom_from_text("env"),
            ],
            false,
        ) {
            Ok(environment) => environment,
            Err(error) => {
                self.diagnostics.push(Diagnostic::error(
                    declaration.line(),
                    format!("macro environment could not be selected: {error}"),
                ));
                return Some(Vec::new());
            }
        };
        let environment = super::super::compiler_values::macro_environment(
            context.values(),
            base_environment,
            declared_language_value(language),
        );
        let invocations = work.invocations().to_vec();
        let mut macro_diagnostics = Vec::new();
        for original in invocations {
            let invocation = match work.current_invocation(context.values(), &original) {
                Ok(invocation) => invocation,
                Err(mut diagnostics) => {
                    self.diagnostics.append(&mut diagnostics);
                    return Some(Vec::new());
                }
            };
            let keys = original
                .path
                .iter()
                .map(Key::atom_from_text)
                .collect::<Vec<_>>();
            let effect = match macro_lookup(execution, prior_definitions, &keys, true) {
                Ok(effect) if effect != Value::Dict(Dict::new_sync()) => effect,
                Ok(_) => {
                    self.diagnostics.push(macro_compiler_diagnostic(
                        context.values(),
                        &original,
                        format!("macro `{}` is not defined", original.path.join(".")),
                        None,
                        &[],
                        std::slice::from_ref(&original),
                    ));
                    return Some(Vec::new());
                }
                Err(error) => {
                    self.diagnostics.push(macro_compiler_diagnostic(
                        context.values(),
                        &original,
                        format!(
                            "macro `{}` could not be selected: {error}",
                            original.path.join(".")
                        ),
                        None,
                        &[],
                        std::slice::from_ref(&original),
                    ));
                    return Some(Vec::new());
                }
            };
            let run = match super::super::macro_expansion::run_macro_effect(
                execution,
                effect,
                environment.clone(),
                invocation.input.clone(),
            ) {
                Ok(run) => run,
                Err(failure) => {
                    let position = failure.frontier().map(|frontier| {
                        let (line, column) = work.position_at(frontier);
                        (frontier, line, column)
                    });
                    let case_detail = failure
                        .cases()
                        .iter()
                        .map(|case| {
                            format!(
                                "\n  while parsing: {}",
                                super::super::macro_expansion::render_macro_case(execution, case)
                            )
                        })
                        .collect::<String>();
                    let position_detail = position.map_or_else(String::new, |(_, line, column)| {
                        format!(" at input line {line}, column {column}")
                    });
                    self.diagnostics.push(macro_compiler_diagnostic(
                        context.values(),
                        &original,
                        format!(
                            "macro `{}` failed{position_detail}: {}{case_detail}",
                            original.path.join("."),
                            failure.message()
                        ),
                        position,
                        failure.cases(),
                        std::slice::from_ref(&original),
                    ));
                    return Some(Vec::new());
                }
            };
            if let Err(diagnostics) =
                work.splice(&invocation, run.consumed_end(), run.output(), original.line)
            {
                self.diagnostics
                    .extend(diagnostics.into_iter().map(|diagnostic| {
                        macro_compiler_diagnostic(
                            context.values(),
                            &original,
                            format!(
                                "macro `{}` generated invalid source structure: {}",
                                original.path.join("."),
                                diagnostic.message
                            ),
                            None,
                            &[],
                            std::slice::from_ref(&original),
                        )
                    }));
                return Some(Vec::new());
            }
            macro_diagnostics.push((original.start, original.clone(), run.diagnostics().to_vec()));
        }
        let (rewritten, embedded) = work.materialize();
        let diagnostics_before = self.diagnostics.len();
        let declarations = parse_expanded_declaration(&rewritten, embedded, &mut self.diagnostics);
        let accepted = !self.diagnostics[diagnostics_before..]
            .iter()
            .any(|diagnostic| diagnostic.severity == crate::diagnostic::Severity::Error);
        if accepted {
            macro_diagnostics.sort_by_key(|(start, _, _)| *start);
            for (_, original, diagnostics) in macro_diagnostics {
                for diagnostic in diagnostics {
                    let emission = apply_macro_context(
                        context.values(),
                        diagnostic.emission().as_core().clone(),
                        None,
                        &[],
                        std::slice::from_ref(&original),
                    );
                    context.emit_diagnostic(diagnostic.severity(), emission);
                }
            }
        } else {
            let excerpt = work.normalized_excerpt();
            let mut frames = work.invocations().to_vec();
            frames.sort_by_key(|frame| frame.start);
            let primary = frames
                .first()
                .expect("macro work must retain at least one invocation");
            for diagnostic in &mut self.diagnostics[diagnostics_before..] {
                if diagnostic.severity != crate::diagnostic::Severity::Error {
                    continue;
                }
                let parser_line = diagnostic.line;
                let parser_message = std::mem::take(&mut diagnostic.message);
                let excerpt = if excerpt.is_empty() {
                    "<empty>".to_owned()
                } else {
                    excerpt.clone()
                };
                *diagnostic = macro_compiler_diagnostic(
                    context.values(),
                    primary,
                    format!(
                        "expanded declaration is invalid `.g` syntax (generated line {parser_line}): {parser_message}; expansion: `{excerpt}`"
                    ),
                    None,
                    &[],
                    &frames,
                );
            }
        }
        Some(declarations)
    }

    pub(in crate::g_syntax) fn finish(mut self, declarations: &[Declaration]) -> Vec<Diagnostic> {
        if self.validate_language {
            validate_language_position(declarations, &mut self.diagnostics);
        }
        self.diagnostics
    }

    fn finish_inspection(mut self, declarations: &[InspectedDeclaration]) -> Vec<Diagnostic> {
        if self.validate_language {
            validate_inspected_language_position(declarations, &mut self.diagnostics);
        }
        self.diagnostics
    }
}

fn validate_inspected_language_position(
    declarations: &[InspectedDeclaration],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(first) = declarations.first() else {
        diagnostics.push(Diagnostic::error(
            1,
            "empty source has no language declaration",
        ));
        return;
    };

    if !matches!(
        &first.kind,
        InspectedDeclarationKind::Parsed(super::super::DeclarationKind::Language(_))
    ) {
        diagnostics.push(Diagnostic::error(
            first.line,
            "first declaration should be a language version declaration",
        ));
    }

    for declaration in declarations.iter().skip(1) {
        if matches!(
            &declaration.kind,
            InspectedDeclarationKind::Parsed(super::super::DeclarationKind::Language(_))
        ) {
            diagnostics.push(Diagnostic::error(
                declaration.line,
                "language declaration must appear before all other declarations",
            ));
        }
    }
}

fn macro_compiler_diagnostic(
    values: &crate::core::CoreValueFactory,
    invocation: &OriginalMacroInvocation,
    message: String,
    frontier: Option<(usize, usize, usize)>,
    cases: &[PublicValue],
    frames: &[OriginalMacroInvocation],
) -> Diagnostic {
    let emission = crate::diagnostic::text_message(Some(invocation.line), &message);
    let emission = apply_macro_context(values, emission, frontier, cases, frames);
    Diagnostic::error(invocation.line, message).with_emission(emission)
}

fn apply_macro_context(
    values: &crate::core::CoreValueFactory,
    message: Value,
    frontier: Option<(usize, usize, usize)>,
    cases: &[PublicValue],
    frames: &[OriginalMacroInvocation],
) -> Value {
    let mut context = Dict::new_sync().insert(
        Key::atom_from_text("frames"),
        Value::List(List::from_values(
            frames.iter().map(macro_frame_value).collect(),
        )),
    );
    if let Some((byte, line, column)) = frontier {
        let position = Dict::new_sync()
            .insert(
                Key::atom_from_text("byte"),
                Value::Number(Number::from_usize(byte)),
            )
            .insert(
                Key::atom_from_text("line"),
                Value::Number(Number::from_usize(line)),
            )
            .insert(
                Key::atom_from_text("column"),
                Value::Number(Number::from_usize(column)),
            );
        context = context.insert(Key::atom_from_text("input_position"), Value::Dict(position));
    }
    if !cases.is_empty() {
        context = context.insert(
            Key::atom_from_text("cases"),
            Value::List(List::from_values(
                cases.iter().map(|case| case.as_core().clone()).collect(),
            )),
        );
    }
    let updates =
        Value::Dict(Dict::new_sync().insert(Key::atom_from_text("macro"), Value::Dict(context)));
    crate::diagnostic::apply_emission_updates(values, message.clone(), updates).unwrap_or(message)
}

fn macro_frame_value(frame: &OriginalMacroInvocation) -> Value {
    let path = Value::List(List::from_values(
        frame
            .path
            .iter()
            .map(|part| Value::binary_from_text(part))
            .collect(),
    ));
    let invocation = Dict::new_sync()
        .insert(
            Key::atom_from_text("id"),
            Value::Number(Number::from_usize(frame.id)),
        )
        .insert(
            Key::atom_from_text("line"),
            Value::Number(Number::from_usize(frame.line)),
        )
        .insert(
            Key::atom_from_text("declaration_byte"),
            Value::Number(Number::from_usize(frame.start)),
        );
    Value::Dict(
        Dict::new_sync()
            .insert(Key::atom_from_text("path"), path)
            .insert(Key::atom_from_text("invocation"), Value::Dict(invocation)),
    )
}

fn macro_lookup(
    execution: &CompilationExecution,
    root: &Value,
    path: &[Key],
    force_result: bool,
) -> Result<Value, String> {
    let mut current = root.clone();
    for key in path {
        let evaluated = force_macro_lookup_value(execution, current)?;
        let Value::Dict(dict) = evaluated else {
            return Err("macro path traverses a non-dictionary value".to_owned());
        };
        current = dict
            .get(key)
            .cloned()
            .unwrap_or_else(|| Value::Dict(Dict::new_sync()));
    }
    if force_result {
        force_macro_lookup_value(execution, current)
    } else {
        Ok(current)
    }
}

fn force_macro_lookup_value(
    execution: &CompilationExecution,
    mut value: Value,
) -> Result<Value, String> {
    loop {
        match eval::eval_value(execution.lookup_context(), &value) {
            Ok(next @ (Value::Lazy(_) | Value::Promised(_))) => value = next,
            Ok(value) => return Ok(value),
            Err(error) => {
                let Some(wait) = error.blocked_on() else {
                    return Err(error.to_string());
                };
                match execution
                    .lookup_context()
                    .pump_wait(&wait.0, MACRO_LOOKUP_STEP_BUDGET)
                {
                    EvaluationPumpOutcome::TargetReady
                    | EvaluationPumpOutcome::Busy
                    | EvaluationPumpOutcome::BudgetExhausted => {}
                    EvaluationPumpOutcome::NoProgress => {
                        return Err(
                            "macro lookup is waiting on a lazy producer unavailable to the macro demand session"
                                .to_owned(),
                        );
                    }
                }
            }
        }
    }
}

fn parse_expanded_declaration(
    rewritten: &str,
    embedded: Vec<Value>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Declaration> {
    let lexical =
        match lex_source(rewritten).replace_unknowns_with_embedded(EMBEDDED_MARKER, embedded) {
            Ok(lexical) => lexical,
            Err(error) => {
                diagnostics.push(Diagnostic::error(1, error));
                return Vec::new();
            }
        };
    diagnostics.extend(lexical.diagnostics().iter().cloned());
    if lexical.has_errors() {
        return Vec::new();
    }
    report_orphan_continuations(&lexical, diagnostics);
    diagnostics.extend(validate_delimited_layouts(&lexical));
    lexical
        .declarations()
        .iter()
        .map(|declaration| parse_lexical_declaration(&lexical, declaration, diagnostics))
        .collect()
}

fn declared_language_value(language: &super::super::LanguageDecl) -> Value {
    Value::Dict(
        Dict::new_sync()
            .insert(
                Key::atom_from_text("base"),
                Value::Atom(Atom::from_key(&Key::binary_from_text(&language.base))),
            )
            .insert(
                Key::atom_from_text("extensions"),
                Value::List(List::from_values(
                    language
                        .extensions
                        .iter()
                        .map(|extension| {
                            Value::Atom(Atom::from_key(&Key::binary_from_text(extension)))
                        })
                        .collect(),
                )),
            ),
    )
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
