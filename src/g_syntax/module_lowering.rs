use std::collections::{BTreeSet, HashMap};

use super::*;
use crate::runtime::RuntimeValueRoot;

mod definitions;
mod imports;
mod journal;
mod objects;

pub(in crate::g_syntax) use definitions::*;
pub(in crate::g_syntax) use imports::*;
use journal::{JournalOp, PendingJournal};
pub(in crate::g_syntax) use objects::*;

pub(in crate::g_syntax) fn lower_source(source: &[u8], context: &CompileContext) -> LoweredSource {
    let mut parser = parser::StagedSourceParser::new(source);
    let mut lowerer = ModuleLowerer::new(context);
    let mut language = None;
    loop {
        #[cfg(feature = "glam-prof")]
        let next =
            context
                .values()
                .evaluation_profile()
                .time(crate::profiling::Phase::Parse, || {
                    parser.next_expanded_declarations(
                        context,
                        lowerer.definitions_root(),
                        language.as_ref(),
                    )
                });
        #[cfg(not(feature = "glam-prof"))]
        let next = parser.next_expanded_declarations(
            context,
            lowerer.definitions_root(),
            language.as_ref(),
        );
        let Some(declarations) = next else {
            break;
        };
        for declaration in declarations {
            if let DeclarationKind::Language(declared) = &declaration.kind {
                language = Some(declared.clone());
            }
            #[cfg(feature = "glam-prof")]
            context
                .values()
                .evaluation_profile()
                .time(crate::profiling::Phase::Lower, || {
                    lowerer.lower_declaration(declaration)
                });
            #[cfg(not(feature = "glam-prof"))]
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
    /// Boundaries of definitions in nested namespaces, by namespace path:
    /// the module's with the namespace's, or a declared object's alone.
    nested_reflection: HashMap<String, RuntimeValueRoot>,
    /// The objects this file declares or extends.
    declared_objects: BTreeSet<String>,
    journal: PendingJournal,
    diagnostics: Vec<Diagnostic>,
    parsed_declarations: Vec<Declaration>,
}

impl<'context> ModuleLowerer<'context> {
    pub(in crate::g_syntax) fn new(context: &'context CompileContext) -> Self {
        compiler_values::prepare(context.values());
        let module_reflection = context.values().with_runtime_value_access(|access| {
            compiler_values::reflection_boundary_root(
                &access,
                context.abstract_global_path(&access, "refl"),
                context.final_defs(&access),
            )
        });
        Self {
            context,
            definitions: context.prior_defs_root().clone(),
            module_reflection,
            nested_reflection: HashMap::new(),
            declared_objects: BTreeSet::new(),
            journal: PendingJournal::default(),
            diagnostics: Vec::new(),
            parsed_declarations: Vec::new(),
        }
    }

    pub(in crate::g_syntax) fn lower_declaration(&mut self, declaration: Declaration) {
        let line = declaration.line;
        self.record_journal(&declaration.kind, line);
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
                let nested = match kind {
                    DeclarationKind::Definition(definition) => {
                        self.nested_reflection(&definition.target)
                    }
                    DeclarationKind::Object(object) => {
                        self.declared_objects.insert(object.target.clone());
                        None
                    }
                    DeclarationKind::Extend(extend) => {
                        self.declared_objects.insert(extend.target.clone());
                        None
                    }
                    _ => None,
                };
                let reflection = match &nested {
                    Some(path) => &self.nested_reflection[path],
                    None => &self.module_reflection,
                };
                let result = self.context.values().with_runtime_value_access(|access| {
                    let definitions = self.definitions.clone_core_with(&access);
                    let scope = NameScope::module_with_reflection(
                        &access,
                        self.context,
                        access.duplicate_value(&definitions),
                        ReflectionBoundary {
                            demand: reflection.clone_core_with(&access),
                        },
                    );
                    let resolved = match kind {
                        DeclarationKind::Definition(definition) => resolve_module_definition(
                            &access,
                            definition,
                            line,
                            self.context,
                            definitions,
                            &scope,
                        ),
                        DeclarationKind::Object(object) => {
                            lower_object(&access, object, line, self.context, definitions, &scope)
                        }
                        DeclarationKind::Extend(extend) => {
                            lower_extend(&access, extend, line, self.context, definitions, &scope)
                        }
                        _ => unreachable!("resolved declarations were matched above"),
                    }?;
                    Ok(access.root_runtime_value(lower_resolved_expr_in(&access, resolved)))
                });
                match result {
                    Ok(definitions) => {
                        self.definitions = definitions;
                        Ok(())
                    }
                    Err(diagnostic) => Err(diagnostic),
                }
            }
            DeclarationKind::Language(_)
            | DeclarationKind::Declare(_)
            | DeclarationKind::Assume(_)
            | DeclarationKind::Unknown => Ok(()),
        };
        if let Err(diagnostic) = result {
            self.diagnostics.push(diagnostic);
        }
        self.parsed_declarations.push(declaration);
    }

    /// The namespace path whose boundary a definition of `target` carries,
    /// if it is nested: the module's and the namespace's own, which checks
    /// the namespace's journal, or only a declared object's, as objects are
    /// self-contained. A target within `refl`, `meta` or `spec` carries no
    /// boundary at all.
    fn nested_reflection(&mut self, target: &[SyntaxKeyExpr]) -> Option<String> {
        let names = target
            .iter()
            .map(|key| match key {
                SyntaxKeyExpr::Atom(name) => Some(name.as_str()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let (_, namespace) = names.split_last()?;
        if namespace.is_empty()
            || namespace
                .iter()
                .any(|name| matches!(*name, "refl" | "meta" | "spec"))
        {
            return None;
        }
        let object = (1..=namespace.len())
            .map(|length| namespace[..length].join("."))
            .find(|path| self.declared_objects.contains(path));
        let path = match &object {
            Some(object) => format!("object {object}"),
            None => namespace.join("."),
        };
        if !self.nested_reflection.contains_key(&path) {
            let boundary = self.context.values().with_runtime_value_access(|access| {
                let final_defs = || ResolvedExpr::Provided(self.context.final_defs(&access));
                let boundary = match &object {
                    Some(object) => compiler_values::reflection_boundary_resolved(
                        &access,
                        object_reflection_guard_resolved(
                            &access,
                            path_resolved_in_definitions(&access, object, final_defs()),
                        ),
                        path_resolved_in_definitions(&access, object, final_defs()),
                    ),
                    None => {
                        let namespace = namespace.join(".");
                        let guard = self
                            .context
                            .abstract_global_path(&access, &format!("{namespace}.refl"));
                        apply_builtin_resolved(
                            &access,
                            Builtin::Seq,
                            [
                                ResolvedExpr::Provided(
                                    self.module_reflection.clone_core_with(&access),
                                ),
                                compiler_values::reflection_boundary_resolved(
                                    &access,
                                    ResolvedExpr::Provided(guard),
                                    path_resolved_in_definitions(&access, &namespace, final_defs()),
                                ),
                            ],
                        )
                    }
                };
                access.root_runtime_value(lower_resolved_expr_in(&access, boundary))
            });
            self.nested_reflection.insert(path.clone(), boundary);
        }
        Some(path)
    }

    /// Records what `kind` defines in the journal, writing the stretch
    /// recorded so far before an import.
    fn record_journal(&mut self, kind: &DeclarationKind, line: usize) {
        let static_path = |path: &str| path.split('.').map(str::to_owned).collect::<Vec<_>>();
        let (op, target) = match kind {
            DeclarationKind::Import(import) => {
                self.flush_journal();
                match &import.placement {
                    ImportPlacement::As(target) => (JournalOp::Introduce, static_path(target)),
                    ImportPlacement::At(target) => (JournalOp::Update, static_path(target)),
                    ImportPlacement::Inline => {
                        if let ImportReference::Builtin(name) = &import.reference {
                            self.record_builtin_import(name, line);
                        }
                        return;
                    }
                }
            }
            DeclarationKind::Unique(names) => {
                for name in names {
                    self.journal
                        .record(&[name.as_str()], JournalOp::Introduce, line);
                }
                return;
            }
            DeclarationKind::Declare(paths) | DeclarationKind::Assume(paths) => {
                let op = match kind {
                    DeclarationKind::Declare(_) => JournalOp::Declare,
                    _ => JournalOp::Assume,
                };
                for path in paths {
                    let path = path.split('.').collect::<Vec<_>>();
                    self.journal.record(&path, op, line);
                }
                return;
            }
            DeclarationKind::Definition(definition) if definition.expr.is_some() => {
                let op = match definition.kind {
                    DefinitionKind::Introduce => JournalOp::Introduce,
                    DefinitionKind::Override => JournalOp::Override,
                    DefinitionKind::Update => JournalOp::Update,
                };
                // A computed target keeps its run-time assertion instead.
                let Some(target) = definition
                    .target
                    .iter()
                    .map(|key| match key {
                        SyntaxKeyExpr::Atom(name) => Some(name.clone()),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                else {
                    return;
                };
                (op, target)
            }
            DeclarationKind::Object(object) => (JournalOp::Introduce, static_path(&object.target)),
            DeclarationKind::Extend(extend) => (JournalOp::Override, static_path(&extend.target)),
            _ => return,
        };
        let target = target.iter().map(String::as_str).collect::<Vec<_>>();
        self.journal.record(&target, op, line);
    }

    /// An inline built-in import updates each name it merges.
    fn record_builtin_import(&mut self, name: &str, line: usize) {
        let keys = self.context.values().with_runtime_value_access(|access| {
            match compiler_values::builtin_module(&access, name).map(|module| module.value) {
                Some(Value::Dict(dict)) => dict.iter().map(|(key, _)| key.clone()).collect(),
                _ => Vec::new(),
            }
        });
        for key in keys {
            self.journal.record_key(key, JournalOp::Update, line);
        }
    }

    fn flush_journal(&mut self) {
        let definitions = self.context.values().with_runtime_value_access(|access| {
            let definitions = self.definitions.clone_core_with(&access);
            let definitions = self.journal.flush_in(&access, self.context, definitions);
            access.root_runtime_value(definitions)
        });
        self.definitions = definitions;
    }

    pub(in crate::g_syntax) fn parsed_declarations(&self) -> &[Declaration] {
        &self.parsed_declarations
    }

    pub(in crate::g_syntax) fn definitions_root(&self) -> &RuntimeValueRoot {
        &self.definitions
    }

    pub(in crate::g_syntax) fn finish(
        mut self,
        mut source_diagnostics: Vec<Diagnostic>,
    ) -> LoweredSource {
        self.flush_journal();
        source_diagnostics.extend(check_file_global_local_shadowing(&self.parsed_declarations));
        source_diagnostics.extend(self.diagnostics);
        let definitions_root = self.definitions;
        LoweredSource {
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
    access: &RuntimeValueAccess<'_>,
    definition: &DefinitionDecl,
    line: usize,
    context: &CompileContext,
    definitions: Value,
    scope: &NameScope,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let mut locals = ResolverContext::default();
    lower_definition_resolved(
        access,
        definition,
        line,
        context,
        &ResolvedRoot::Provided(definitions),
        &scope.resolved_in(access),
        true,
        &mut locals,
    )
}
