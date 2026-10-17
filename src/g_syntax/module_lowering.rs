use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use super::*;
use crate::core::RuntimeValueMutator;
use crate::runtime::RuntimeValueRoot;

mod definitions;
mod imports;
mod journal;
mod objects;

pub(in crate::g_syntax) use definitions::*;
pub(in crate::g_syntax) use imports::*;
use journal::{JournalOp, PendingJournal};
pub(in crate::g_syntax) use objects::*;

/// Lowers one source under one mutator, the compile entry's
/// (`driver-mutator`): parsing and lowering enter the heap under it, and
/// macro expansion, which evaluates and calls client code, runs with it
/// suspended. The lowerer is built first, since building the compiler-values
/// cache evaluates.
pub(in crate::g_syntax) fn lower_source(source: &[u8], context: &CompileContext) -> LoweredSource {
    let mut parser = parser::StagedSourceParser::new(source);
    let mut lowerer = ModuleLowerer::new(context);
    context.values().with_runtime_value_mutator(|mut mutator| {
        let mut language = None;
        loop {
            #[cfg(feature = "glam-prof")]
            let next =
                context
                    .values()
                    .evaluation_profile()
                    .time(crate::profiling::Phase::Parse, || {
                        parser.next_expanded_declarations(
                            &mut mutator,
                            context,
                            lowerer.definitions_root(),
                            lowerer.file_environment(),
                            language.as_ref(),
                        )
                    });
            #[cfg(not(feature = "glam-prof"))]
            let next = parser.next_expanded_declarations(
                &mut mutator,
                context,
                lowerer.definitions_root(),
                lowerer.file_environment(),
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
                        lowerer.lower_declaration(&mutator, declaration)
                    });
                #[cfg(not(feature = "glam-prof"))]
                lowerer.lower_declaration(&mutator, declaration);
            }
        }
        let macro_uses = parser.file_macro_uses().to_vec();
        let diagnostics = parser.finish(lowerer.parsed_declarations());
        lowerer.finish(&mutator, diagnostics, &macro_uses)
    })
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
    /// The file's `let` names in order, each bound to its share of its
    /// group's value: compiler state, which no module entry holds.
    file_environment: Vec<(String, RuntimeValueRoot)>,
    /// The same names, in the same order, as each declaration's resolver
    /// reads them.
    file_lets: Rc<FileLets>,
    diagnostics: Vec<Diagnostic>,
    parsed_declarations: Vec<Declaration>,
}

impl<'context> ModuleLowerer<'context> {
    pub(in crate::g_syntax) fn new(context: &'context CompileContext) -> Self {
        compiler_values::prepare(context.values());
        let module_reflection = context.values().with_boundary_access(|access| {
            compiler_values::reflection_boundary_root(
                &access,
                context.namespace_name(&access, ""),
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
            file_environment: Vec::new(),
            file_lets: Rc::default(),
            diagnostics: Vec::new(),
            parsed_declarations: Vec::new(),
        }
    }

    pub(in crate::g_syntax) fn lower_declaration(
        &mut self,
        mutator: &RuntimeValueMutator<'_, '_>,
        declaration: Declaration,
    ) {
        let line = declaration.line;
        self.record_journal(mutator, &declaration.kind, line);
        let result = match &declaration.kind {
            DeclarationKind::Import(_) | DeclarationKind::Unique(_) => {
                let (result, definitions) = mutator.with_access(|access| {
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
                        self.nested_reflection(mutator, &definition.target)
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
                let result = mutator.with_access(|access| {
                    let definitions = self.definitions.clone_core_with(&access);
                    let scope = NameScope::module_with_reflection(
                        &access,
                        self.context,
                        access.duplicate_value(&definitions),
                        ReflectionBoundary {
                            demand: reflection.clone_core_with(&access),
                        },
                    );
                    let mut locals = self.file_locals();
                    let resolved = match kind {
                        DeclarationKind::Definition(definition) => resolve_module_definition(
                            &access,
                            definition,
                            line,
                            self.context,
                            definitions,
                            &scope,
                            &mut locals,
                        ),
                        DeclarationKind::Object(object) => lower_object(
                            &access,
                            object,
                            line,
                            self.context,
                            definitions,
                            &scope,
                            &mut locals,
                        ),
                        DeclarationKind::Extend(extend) => lower_extend(
                            &access,
                            extend,
                            line,
                            self.context,
                            definitions,
                            &scope,
                            &mut locals,
                        ),
                        _ => unreachable!("resolved declarations were matched above"),
                    }?;
                    let resolved = self.bind_file_environment(&access, resolved);
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
            DeclarationKind::Let(bindings) => self.lower_file_let(mutator, bindings, line),
            DeclarationKind::Language(_) => {
                self.install_reflection(mutator);
                Ok(())
            }
            DeclarationKind::Declare(_) | DeclarationKind::Assume(_) | DeclarationKind::Unknown => {
                Ok(())
            }
        };
        if let Err(diagnostic) = result {
            self.diagnostics.push(diagnostic);
        }
        self.parsed_declarations.push(declaration);
    }

    /// A resolver context that reads the file's `let` names.
    fn file_locals(&self) -> ResolverContext {
        ResolverContext::with_file_lets(Rc::clone(&self.file_lets))
    }

    /// Binds the file's `let` names that `resolved` reads to their shared
    /// values. A file `let` takes the binding identity of its position.
    fn bind_file_environment(
        &self,
        access: &RuntimeValueAccess<'_>,
        resolved: ResolvedExpr<Value>,
    ) -> ResolvedExpr<Value> {
        if self.file_environment.is_empty() {
            return resolved;
        }
        let (parameters, values): (Vec<_>, Vec<_>) = resolved
            .free_bindings()
            .into_iter()
            .filter_map(|binding| {
                let (_, value) = self
                    .file_environment
                    .get(usize::try_from(binding.local_index()).ok()?)?;
                Some((
                    binding,
                    ResolvedExpr::Provided(value.clone_core_with(access)),
                ))
            })
            .unzip();
        if parameters.is_empty() {
            return resolved;
        }
        ResolvedExpr::apply(ResolvedExpr::lambda(parameters, resolved), values)
    }

    /// Lowers a file's `let` group once, where it stands: against the
    /// module's final self and the definitions visible here, with the
    /// `let`s before it. Its value is one record of the group's bindings,
    /// which the declarations after it share, each name its own entry.
    fn lower_file_let(
        &mut self,
        mutator: &RuntimeValueMutator<'_, '_>,
        bindings: &[(String, SyntaxExpr)],
        line: usize,
    ) -> Result<(), Diagnostic> {
        let entries = mutator.with_access(|access| {
            let definitions = self.definitions.clone_core_with(&access);
            let scope = NameScope::module_with_reflection(
                &access,
                self.context,
                definitions,
                ReflectionBoundary {
                    demand: self.module_reflection.clone_core_with(&access),
                },
            )
            .resolved_in(&access);
            let mut locals = self.file_locals();
            let binders = lower_let_group_resolved(
                &access,
                bindings,
                line,
                self.context,
                &scope,
                &mut locals,
            )?;
            // The group's referable names and their bindings.
            let named = bindings
                .iter()
                .filter_map(|(name, _)| {
                    let canonical = local_name_metadata(name).canonical?;
                    let binding = locals
                        .iter()
                        .rev()
                        .find(|local| local.canonical.as_deref() == Some(canonical.as_str()))
                        .and_then(|local| local.binding)?;
                    Some((name.clone(), canonical, binding))
                })
                .collect::<Vec<_>>();
            let mut record = named
                .iter()
                .map(|(_, canonical, binding)| {
                    apply_builtin_resolved(
                        &access,
                        Builtin::DictSingleton,
                        [
                            ResolvedExpr::Embedded(Value::Atom(atom_from_str(canonical))),
                            ResolvedExpr::Local(*binding),
                        ],
                    )
                })
                .reduce(|left, right| {
                    apply_builtin_resolved(&access, Builtin::DictUnion, [left, right])
                })
                .unwrap_or_else(|| ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())));
            for (parameters, values) in binders.into_iter().rev() {
                record = ResolvedExpr::apply(ResolvedExpr::lambda(parameters, record), values);
            }
            let record = self.bind_file_environment(&access, record);
            let record = access.root_runtime_value(lower_resolved_expr_in(&access, record));
            Ok::<_, Diagnostic>(
                named
                    .into_iter()
                    .map(|(name, canonical, _)| {
                        let entry = ResolvedExpr::Access {
                            base: Box::new(ResolvedExpr::Provided(record.clone_core_with(&access))),
                            path: vec![ResolvedPathPart::Key(name_as_key(&canonical))],
                        };
                        let entry =
                            access.root_runtime_value(lower_resolved_expr_in(&access, entry));
                        (name, entry)
                    })
                    .collect::<Vec<_>>(),
            )
        })?;
        Rc::make_mut(&mut self.file_lets).extend(entries.iter().map(|(name, _)| name.as_str()));
        self.file_environment.extend(entries);
        Ok(())
    }

    /// Installs the module's reflection operation as `meta.refl`, the
    /// scanner, unless the module has one, just after the language
    /// declaration, where a language's version or extensions may later
    /// choose otherwise. The reflection boundary's task runs it.
    fn install_reflection(&mut self, mutator: &RuntimeValueMutator<'_, '_>) {
        let definitions = mutator.with_access(|access| {
            let definitions = self.definitions.clone_core_with(&access);
            let scanner = compiler_values::reflection_scanner_resolved(
                &access,
                compiler_values::reflection_state_key_resolved(
                    &access,
                    ResolvedExpr::Provided(self.context.namespace_name(&access, "")),
                ),
                ResolvedExpr::Provided(self.context.final_defs(&access)),
            );
            let installed = apply_builtin_resolved(
                &access,
                Builtin::Default,
                [
                    scanner,
                    path_resolved_in_definitions(
                        &access,
                        "meta.refl",
                        ResolvedExpr::Provided(access.duplicate_value(&definitions)),
                    ),
                ],
            );
            let definitions = update_module_resolved(
                &access,
                ResolvedExpr::Provided(definitions),
                "meta.refl",
                installed,
            );
            access.root_runtime_value(lower_resolved_expr_in(&access, definitions))
        });
        self.definitions = definitions;
    }

    /// The namespace path whose boundary a definition of `target` carries,
    /// if it is nested: the module's and the namespace's own, which checks
    /// the namespace's journal, or only a declared object's, as objects are
    /// self-contained. A target within `refl`, `meta` or `spec` carries no
    /// boundary at all.
    fn nested_reflection(
        &mut self,
        mutator: &RuntimeValueMutator<'_, '_>,
        target: &[SyntaxKeyExpr],
    ) -> Option<String> {
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
            let boundary = mutator.with_access(|access| {
                let final_defs = || ResolvedExpr::Provided(self.context.final_defs(&access));
                let boundary = match &object {
                    Some(object) => {
                        let object_final =
                            || path_resolved_in_definitions(&access, object, final_defs());
                        let guard = || object_reflection_guard_resolved(&access, object_final());
                        compiler_values::reflection_boundary_resolved(
                            &access,
                            guard(),
                            compiler_values::reflection_scanner_resolved(
                                &access,
                                guard(),
                                object_final(),
                            ),
                        )
                    }
                    None => {
                        let namespace = namespace.join(".");
                        let guard = || {
                            compiler_values::reflection_state_key_resolved(
                                &access,
                                ResolvedExpr::Provided(
                                    self.context.namespace_name(&access, &namespace),
                                ),
                            )
                        };
                        let operation = compiler_values::namespace_reflection_operation_resolved(
                            &access,
                            path_resolved_in_definitions(&access, &namespace, final_defs()),
                        );
                        apply_builtin_resolved(
                            &access,
                            Builtin::Seq,
                            [
                                ResolvedExpr::Provided(
                                    self.module_reflection.clone_core_with(&access),
                                ),
                                compiler_values::reflection_boundary_resolved(
                                    &access,
                                    guard(),
                                    operation,
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
    fn record_journal(
        &mut self,
        mutator: &RuntimeValueMutator<'_, '_>,
        kind: &DeclarationKind,
        line: usize,
    ) {
        let static_path = |path: &str| path.split('.').map(str::to_owned).collect::<Vec<_>>();
        let (op, target) = match kind {
            DeclarationKind::Import(import) => {
                self.flush_journal(mutator);
                match &import.placement {
                    ImportPlacement::As(target) => (JournalOp::Introduce, static_path(target)),
                    ImportPlacement::At(target) => (JournalOp::Update, static_path(target)),
                    ImportPlacement::Inline => {
                        if let ImportReference::Builtin(name) = &import.reference {
                            self.record_builtin_import(mutator, name, line);
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
            DeclarationKind::Language(_) => (JournalOp::Update, static_path("meta.refl")),
            DeclarationKind::Object(object) => (JournalOp::Introduce, static_path(&object.target)),
            DeclarationKind::Extend(extend) => (JournalOp::Override, static_path(&extend.target)),
            _ => return,
        };
        let target = target.iter().map(String::as_str).collect::<Vec<_>>();
        self.journal.record(&target, op, line);
    }

    /// An inline built-in import updates each name it merges.
    fn record_builtin_import(
        &mut self,
        mutator: &RuntimeValueMutator<'_, '_>,
        name: &str,
        line: usize,
    ) {
        let keys = mutator.with_access(|access| {
            match compiler_values::builtin_module(&access, name).map(|module| module.value) {
                Some(Value::Dict(dict)) => dict.iter().map(|(key, _)| key.clone()).collect(),
                _ => Vec::new(),
            }
        });
        for key in keys {
            self.journal.record_key(key, JournalOp::Update, line);
        }
    }

    fn flush_journal(&mut self, mutator: &RuntimeValueMutator<'_, '_>) {
        let definitions = mutator.with_access(|access| {
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

    /// The file's `let` names so far, with their values.
    pub(in crate::g_syntax) fn file_environment(&self) -> &[(String, RuntimeValueRoot)] {
        &self.file_environment
    }

    /// Finishes the file; `macro_uses` are the `let` names its macro
    /// invocations selected.
    pub(in crate::g_syntax) fn finish(
        mut self,
        mutator: &RuntimeValueMutator<'_, '_>,
        mut source_diagnostics: Vec<Diagnostic>,
        macro_uses: &[String],
    ) -> LoweredSource {
        self.flush_journal(mutator);
        source_diagnostics.extend(check_file_global_local_shadowing(&self.parsed_declarations));
        source_diagnostics.extend(warn_unused_file_lets(&self.parsed_declarations, macro_uses));
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
    context.values().with_runtime_value_mutator(|mutator| {
        for declaration in declarations {
            lowerer.lower_declaration(&mutator, declaration);
        }
        lowerer.finish(&mutator, diagnostics, &[])
    })
}

fn resolve_module_definition(
    access: &RuntimeValueAccess<'_>,
    definition: &DefinitionDecl,
    line: usize,
    context: &CompileContext,
    definitions: Value,
    scope: &NameScope,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    lower_definition_resolved(
        access,
        definition,
        line,
        context,
        &ResolvedRoot::Provided(definitions),
        &scope.resolved_in(access),
        true,
        locals,
    )
}
