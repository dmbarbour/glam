use super::*;
use crate::runtime::RuntimeValueRoot;

mod definitions;
mod imports;
mod objects;

pub(in crate::g_syntax) use definitions::*;
pub(in crate::g_syntax) use imports::*;
pub(in crate::g_syntax) use objects::*;

pub(in crate::g_syntax) fn lower_source(source: &[u8], context: &CompileContext) -> LoweredSource {
    let mut parser = parser::StagedSourceParser::new(source);
    let mut lowerer = ModuleLowerer::new(context);
    let mut language = None;
    while let Some(declarations) =
        parser.next_expanded_declarations(context, &lowerer.definitions(), language.as_ref())
    {
        for declaration in declarations {
            if let DeclarationKind::Language(declared) = &declaration.kind {
                language = Some(declared.clone());
            }
            lowerer.lower_declaration(declaration);
        }
    }
    let diagnostics = parser.finish(lowerer.parsed_declarations());
    lowerer.finish(diagnostics)
}

pub(in crate::g_syntax) struct ModuleLowerer<'context> {
    context: &'context CompileContext,
    definitions: RuntimeValueRoot,
    module_reflection: RuntimeValueRoot,
    diagnostics: Vec<Diagnostic>,
    parsed_declarations: Vec<Declaration>,
}

impl<'context> ModuleLowerer<'context> {
    pub(in crate::g_syntax) fn new(context: &'context CompileContext) -> Self {
        let module_reflection = compiler_values::reflection_annotator_root(
            context.values(),
            context.abstract_global_path("refl"),
            context.final_defs(),
        );
        Self {
            context,
            definitions: context.prior_defs_root().clone(),
            module_reflection,
            diagnostics: Vec::new(),
            parsed_declarations: Vec::new(),
        }
    }

    pub(in crate::g_syntax) fn lower_declaration(&mut self, declaration: Declaration) {
        let line = declaration.line;
        let result = match &declaration.kind {
            DeclarationKind::Import(_) | DeclarationKind::Unique(_) => {
                let (result, definitions) =
                    self.context.values().with_runtime_value_access(|access| {
                        let mut definitions = self.definitions.clone_core_with(&access);
                        let result = match &declaration.kind {
                            DeclarationKind::Import(import) => {
                                lower_import(import, line, self.context, &access, &mut definitions)
                            }
                            DeclarationKind::Unique(names) => {
                                lower_unique(names, line, self.context, &access, &mut definitions)
                            }
                            _ => unreachable!("access-bound declarations were matched above"),
                        };
                        (result, access.root_runtime_value(definitions))
                    });
                self.definitions = definitions;
                result
            }
            kind @ (DeclarationKind::Definition(_)
            | DeclarationKind::Object(_)
            | DeclarationKind::Extend(_)) => {
                let (definitions, module_reflection) =
                    self.context.values().with_runtime_value_access(|access| {
                        (
                            self.definitions.clone_core_with(&access),
                            ReflectionBoundary {
                                annotator: self.module_reflection.clone_core_with(&access),
                            },
                        )
                    });
                let scope = NameScope::module_with_reflection(
                    self.context,
                    definitions.clone(),
                    module_reflection,
                );
                let resolved = match kind {
                    DeclarationKind::Definition(definition) => resolve_module_definition(
                        definition,
                        line,
                        self.context,
                        definitions,
                        &scope,
                    ),
                    DeclarationKind::Object(object) => {
                        lower_object(object, line, self.context, definitions, &scope)
                    }
                    DeclarationKind::Extend(extend) => {
                        lower_extend(extend, line, self.context, definitions, &scope)
                    }
                    _ => unreachable!("resolved declarations were matched above"),
                };
                match resolved {
                    Ok(resolved) => {
                        self.definitions =
                            self.context
                                .values()
                                .construct_runtime_value_root(|access| {
                                    lower_resolved_expr_in(access, resolved)
                                });
                        Ok(())
                    }
                    Err(diagnostic) => Err(diagnostic),
                }
            }
            DeclarationKind::Language(_)
            | DeclarationKind::Abstract(_)
            | DeclarationKind::Unknown => Ok(()),
        };
        if let Err(diagnostic) = result {
            self.diagnostics.push(diagnostic);
        }
        self.parsed_declarations.push(declaration);
    }

    pub(in crate::g_syntax) fn parsed_declarations(&self) -> &[Declaration] {
        &self.parsed_declarations
    }

    pub(in crate::g_syntax) fn definitions(&self) -> Value {
        self.context
            .values()
            .with_runtime_value_access(|access| self.definitions.clone_core_with(&access))
    }

    pub(in crate::g_syntax) fn finish(
        self,
        mut source_diagnostics: Vec<Diagnostic>,
    ) -> LoweredSource {
        source_diagnostics.extend(check_file_global_local_shadowing(&self.parsed_declarations));
        source_diagnostics.extend(self.diagnostics);
        let definitions_root = self.definitions;
        let definitions = self
            .context
            .values()
            .with_runtime_value_access(|access| definitions_root.clone_core_with(&access));
        LoweredSource {
            definitions,
            diagnostics: source_diagnostics,
            definitions_root,
        }
    }
}

#[cfg(test)]
pub(in crate::g_syntax) fn lower_parsed_source(
    parsed: ParsedSource,
    context: &CompileContext,
) -> LoweredSource {
    let ParsedSource {
        declarations,
        diagnostics,
    } = parsed;
    let mut lowerer = ModuleLowerer::new(context);
    for declaration in declarations {
        lowerer.lower_declaration(declaration);
    }
    lowerer.finish(diagnostics)
}

fn resolve_module_definition(
    definition: &DefinitionDecl,
    line: usize,
    context: &CompileContext,
    definitions: Value,
    scope: &NameScope,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let mut locals = ResolverContext::default();
    lower_definition_resolved(
        definition,
        line,
        context,
        &ResolvedRoot::Provided(definitions),
        &scope.resolved(),
        &mut locals,
    )
}
