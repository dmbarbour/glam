use super::*;

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
        parser.next_expanded_declarations(context, lowerer.definitions(), language.as_ref())
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
    definitions: Value,
    module_reflection: ReflectionBoundary<Value>,
    diagnostics: Vec<Diagnostic>,
    parsed_declarations: Vec<Declaration>,
}

impl<'context> ModuleLowerer<'context> {
    pub(in crate::g_syntax) fn new(context: &'context CompileContext) -> Self {
        Self {
            context,
            definitions: context.prior_defs().clone(),
            module_reflection: ReflectionBoundary {
                annotator: compiler_values::reflection_annotator_value(
                    context.abstract_global_path("refl"),
                    context.final_defs().clone(),
                ),
            },
            diagnostics: Vec::new(),
            parsed_declarations: Vec::new(),
        }
    }

    pub(in crate::g_syntax) fn lower_declaration(&mut self, declaration: Declaration) {
        let line = declaration.line;
        let result = match &declaration.kind {
            DeclarationKind::Import(import) => {
                lower_import(import, line, self.context, &mut self.definitions)
            }
            DeclarationKind::Unique(names) => {
                lower_unique(names, line, self.context, &mut self.definitions)
            }
            DeclarationKind::Definition(definition) => {
                let scope = NameScope::module_with_reflection(
                    self.context,
                    self.definitions.clone(),
                    self.module_reflection.clone(),
                );
                lower_definition(
                    definition,
                    line,
                    self.context,
                    &mut self.definitions,
                    &scope,
                )
            }
            DeclarationKind::Object(object) => {
                let scope = NameScope::module_with_reflection(
                    self.context,
                    self.definitions.clone(),
                    self.module_reflection.clone(),
                );
                lower_object(object, line, self.context, &mut self.definitions, &scope)
            }
            DeclarationKind::Extend(extend) => {
                let scope = NameScope::module_with_reflection(
                    self.context,
                    self.definitions.clone(),
                    self.module_reflection.clone(),
                );
                lower_extend(extend, line, self.context, &mut self.definitions, &scope)
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

    pub(in crate::g_syntax) fn definitions(&self) -> &Value {
        &self.definitions
    }

    pub(in crate::g_syntax) fn finish(
        self,
        mut source_diagnostics: Vec<Diagnostic>,
    ) -> LoweredSource {
        source_diagnostics.extend(check_file_global_local_shadowing(&self.parsed_declarations));
        source_diagnostics.extend(self.diagnostics);
        LoweredSource {
            definitions: self.definitions,
            diagnostics: source_diagnostics,
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

pub(super) fn lower_definition(
    definition: &DefinitionDecl,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
    scope: &NameScope,
) -> Result<(), Diagnostic> {
    let mut locals = ResolverContext::default();
    let definitions_root = ResolvedRoot::Provided(definitions.clone());
    let resolved = lower_definition_resolved(
        definition,
        line,
        context,
        &definitions_root,
        &scope.resolved(),
        &mut locals,
    )?;
    *definitions = lower_resolved_expr(resolved);
    Ok(())
}
