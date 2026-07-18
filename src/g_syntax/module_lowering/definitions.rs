use super::super::*;
use super::objects::apply_builtin_resolved;

#[derive(Clone, Copy)]
pub(in crate::g_syntax) enum BuiltinAssertion {
    Defined,
    Undefined,
}

/// Shared source and scope state for checking and updating one definition.
pub(in crate::g_syntax) struct DefinitionTargetContext<'a> {
    definitions: &'a ResolvedRoot,
    line: usize,
    compiler: &'a CompileContext,
    scope: &'a NameScope<ResolvedRoot>,
}

impl<'a> DefinitionTargetContext<'a> {
    pub(in crate::g_syntax) fn new(
        definitions: &'a ResolvedRoot,
        line: usize,
        compiler: &'a CompileContext,
        scope: &'a NameScope<ResolvedRoot>,
    ) -> Self {
        Self {
            definitions,
            line,
            compiler,
            scope,
        }
    }

    fn lower_update(
        &self,
        target: &str,
        update: &SyntaxExpr,
        sugar_param_count: usize,
        locals: &mut ResolverContext,
    ) -> Result<ResolvedExpr<Value>, Diagnostic> {
        let prior = definition_target_access_resolved(
            target,
            self.definitions,
            self.line,
            self.compiler,
            self.scope,
            locals,
        )?;
        if sugar_param_count == 0 {
            let update = syntax_expr_to_resolved_in_semantic_scope(
                update,
                self.line,
                self.compiler,
                self.scope,
                locals,
            )?;
            return Ok(ResolvedExpr::apply(update, [prior]));
        }

        let SyntaxExpr::Lambda(params, body) = update else {
            return Err(Diagnostic::error(
                self.line,
                "internal error lowering update definition arguments",
            ));
        };
        if params.len() != sugar_param_count {
            return Err(Diagnostic::error(
                self.line,
                "internal error counting update definition arguments",
            ));
        }

        let base_len = locals.len();
        let parameters = locals.extend_bindings(params.iter().map(String::as_str));
        let lowered = syntax_expr_to_resolved_in_semantic_scope(
            body,
            self.line,
            self.compiler,
            self.scope,
            locals,
        )?;
        locals.truncate(base_len);
        Ok(ResolvedExpr::lambda(
            parameters,
            ResolvedExpr::apply(lowered, [prior]),
        ))
    }

    pub(in crate::g_syntax) fn annotate(
        &self,
        assertion: BuiltinAssertion,
        target: &str,
        value: ResolvedExpr<Value>,
        locals: &mut ResolverContext,
    ) -> Result<ResolvedExpr<Value>, Diagnostic> {
        let tag = match assertion {
            BuiltinAssertion::Defined => "assert_defined",
            BuiltinAssertion::Undefined => "assert_undefined",
        };
        let singleton = |key: &str, value| {
            apply_builtin_resolved(
                Builtin::DictSingleton,
                [
                    ResolvedExpr::Embedded(self.compiler.value_atom(atom_from_str(key))),
                    value,
                ],
                self.compiler,
            )
        };
        let payload = apply_builtin_resolved(
            Builtin::DictUnion,
            [
                singleton(
                    "name",
                    ResolvedExpr::Embedded(self.compiler.value_binary(target)),
                ),
                singleton(
                    "value",
                    definition_target_access_resolved(
                        target,
                        self.definitions,
                        self.line,
                        self.compiler,
                        self.scope,
                        locals,
                    )?,
                ),
            ],
            self.compiler,
        );
        let annotation = singleton(tag, payload);
        Ok(apply_builtin_resolved(
            Builtin::Anno,
            [annotation, value],
            self.compiler,
        ))
    }
}

pub(in crate::g_syntax) fn lower_definition_resolved(
    definition: &DefinitionDecl,
    declaration_text: &str,
    line: usize,
    context: &CompileContext,
    definitions: &ResolvedRoot,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let Some(expr) = &definition.expr else {
        return Ok(definitions.expr());
    };

    let target_scope = definition_target_scope_resolved(scope, definitions.clone());
    let target_context = DefinitionTargetContext::new(definitions, line, context, &target_scope);
    let (assertion, value) = match definition.kind {
        DefinitionKind::Introduce | DefinitionKind::Override => {
            let assertion = match definition.kind {
                DefinitionKind::Introduce => BuiltinAssertion::Undefined,
                DefinitionKind::Override => BuiltinAssertion::Defined,
                DefinitionKind::Update => unreachable!(),
            };
            let value =
                syntax_expr_to_resolved_in_semantic_scope(expr, line, context, scope, locals)?;
            (Some(assertion), value)
        }
        DefinitionKind::Update => (
            None,
            target_context.lower_update(
                &definition.target,
                expr,
                definition_param_count(definition, declaration_text, line)?,
                locals,
            )?,
        ),
    };
    let value =
        decorate_reflection_boundary(&definition.target, line, value, context, scope, locals)?;
    let value = match assertion {
        Some(assertion) => target_context.annotate(assertion, &definition.target, value, locals)?,
        None => value,
    };
    update_definition_target_resolved(
        definitions,
        &definition.target,
        value,
        line,
        context,
        &target_scope,
        locals,
    )
}

fn decorate_reflection_boundary(
    target: &str,
    line: usize,
    value: ResolvedExpr<Value>,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let Some(boundary) = &scope.reflection else {
        return Ok(value);
    };
    let parts = definition_target_parts(target, line)?;
    let Some(SyntaxKeyExpr::Atom(root)) = parts.first() else {
        // Reflection namespaces are intentionally statically recognizable.
        // A computed root might evaluate to `refl`, `meta`, or `spec`, so it
        // cannot safely receive an automatic demand boundary.
        return Ok(value);
    };
    if matches!(root.as_str(), "refl" | "meta" | "spec") {
        return Ok(value);
    }

    Ok(apply_reflection_boundary(value, boundary, context, locals))
}

fn apply_reflection_boundary(
    value: ResolvedExpr<Value>,
    boundary: &ReflectionBoundary<ResolvedRoot>,
    context: &CompileContext,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    let base_len = locals.len();
    let guard_path = || {
        ResolvedExpr::List(vec![
            ResolvedExpr::Embedded(context.value_atom(atom_from_str("heap"))),
            boundary.guard.expr(),
        ])
    };

    let final_refl = ResolvedExpr::Access {
        base: Box::new(boundary.final_defs.expr()),
        path: vec![ResolvedPathPart::Key(name_as_key("refl"))],
    };
    let launch_all = effect_call_resolved("refl_tasks", [final_refl], context, locals);
    let scanner = effect_call_resolved("cut", [launch_all], context, locals);

    let scanner_handle = locals.push_binding("<reflection-scanner-handle>");
    let remember_scanner = effect_call_resolved(
        "set",
        [guard_path(), ResolvedExpr::Local(scanner_handle)],
        context,
        locals,
    );
    let launch_scanner = effect_call_resolved("refl_task", [scanner], context, locals);
    let launch_and_remember = effect_call_resolved(
        "seq",
        [
            launch_scanner,
            ResolvedExpr::lambda(vec![scanner_handle], remember_scanner),
        ],
        context,
        locals,
    );

    let existing = locals.push_binding("<reflection-scanner>");
    let guard_is_empty = ResolvedExpr::apply(
        ResolvedExpr::Embedded(context.value_builtin(Builtin::Equal)),
        [
            ResolvedExpr::Local(existing),
            ResolvedExpr::Embedded(context.empty_dict_value()),
        ],
    );
    let start_if_missing =
        effect_then_resolved(guard_is_empty, launch_and_remember, context, locals);
    let already_started = effect_call_resolved(
        "r",
        [ResolvedExpr::Embedded(context.unit_value())],
        context,
        locals,
    );
    let choose = effect_call_resolved("alt", [start_if_missing, already_started], context, locals);
    let check_guard = effect_call_resolved("get", [guard_path()], context, locals);
    let ensure_scanner = effect_call_resolved(
        "seq",
        [check_guard, ResolvedExpr::lambda(vec![existing], choose)],
        context,
        locals,
    );
    let ensure_scanner = effect_call_resolved("cut", [ensure_scanner], context, locals);
    locals.truncate(base_len);

    let annotation = apply_builtin_resolved(
        Builtin::DictSingleton,
        [
            ResolvedExpr::Embedded(context.value_atom(atom_from_str("refl"))),
            ensure_scanner,
        ],
        context,
    );
    apply_builtin_resolved(Builtin::Anno, [annotation, value], context)
}

pub(in crate::g_syntax) fn definition_target_scope_resolved(
    scope: &NameScope<ResolvedRoot>,
    visible_definitions: ResolvedRoot,
) -> NameScope<ResolvedRoot> {
    if scope.object_final_defs.is_some() {
        return scope.clone();
    }

    let mut scope = scope.clone();
    scope.final_defs = visible_definitions.clone();
    scope.prior_defs = visible_definitions.clone();
    scope.module_final_defs = visible_definitions.clone();
    scope.module_prior_defs = visible_definitions;
    scope
}

pub(in crate::g_syntax) fn update_module_resolved(
    definitions: ResolvedExpr<Value>,
    target: &str,
    value: ResolvedExpr<Value>,
    context: &CompileContext,
) -> ResolvedExpr<Value> {
    apply_builtin_resolved(
        Builtin::DictUpdate,
        [static_path_resolved(target, context), value, definitions],
        context,
    )
}

pub(in crate::g_syntax) fn update_definition_target_resolved(
    definitions: &ResolvedRoot,
    target: &str,
    value: ResolvedExpr<Value>,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    Ok(apply_builtin_resolved(
        Builtin::DictUpdate,
        [
            definition_target_path_resolved(target, line, context, scope, locals)?,
            value,
            definitions.expr(),
        ],
        context,
    ))
}

pub(in crate::g_syntax) fn definition_target_access_resolved(
    target: &str,
    definitions: &ResolvedRoot,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let path = definition_target_parts(target, line)?
        .iter()
        .map(|part| syntax_key_expr_to_resolved_path(part, line, context, scope, locals))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ResolvedExpr::Access {
        base: Box::new(definitions.expr()),
        path,
    })
}

pub(in crate::g_syntax) fn definition_target_path_resolved(
    target: &str,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    syntax_path_resolved(
        &definition_target_parts(target, line)?,
        line,
        context,
        scope,
        locals,
    )
}

pub(in crate::g_syntax) fn syntax_path_resolved(
    parts: &[SyntaxKeyExpr],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let mut result: Option<ResolvedExpr<Value>> = None;
    let mut pending = Vec::new();

    for part in parts {
        match part {
            SyntaxKeyExpr::PathIndex(expr) => {
                let prefix = ResolvedExpr::List(std::mem::take(&mut pending));
                let combined = match result {
                    Some(result) => {
                        apply_builtin_resolved(Builtin::Append, [result, prefix], context)
                    }
                    None => prefix,
                };
                let splice =
                    syntax_expr_to_resolved_in_semantic_scope(expr, line, context, scope, locals)?;
                result = Some(apply_builtin_resolved(
                    Builtin::Append,
                    [combined, splice],
                    context,
                ));
            }
            SyntaxKeyExpr::Atom(name) => pending.push(ResolvedExpr::Embedded(
                context.value_atom(atom_from_str(name)),
            )),
            SyntaxKeyExpr::Index(expr) => pending.push(syntax_expr_to_resolved_in_semantic_scope(
                expr, line, context, scope, locals,
            )?),
        }
    }

    let tail = ResolvedExpr::List(pending);
    Ok(match result {
        Some(result) => apply_builtin_resolved(Builtin::Append, [result, tail], context),
        None => tail,
    })
}

pub(in crate::g_syntax) fn static_path_resolved(
    target: &str,
    context: &CompileContext,
) -> ResolvedExpr<Value> {
    ResolvedExpr::List(
        target
            .split('.')
            .map(|part| ResolvedExpr::Embedded(context.value_atom(atom_from_str(part))))
            .collect::<Vec<_>>(),
    )
}

pub(in crate::g_syntax) fn path_resolved_in_scope(
    target: &str,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &ResolverContext,
) -> ResolvedExpr<Value> {
    let mut parts = target.split('.');
    let Some(first) = parts.next() else {
        return ResolvedExpr::Embedded(context.empty_dict_value());
    };
    let value = lower_name_expr_resolved(first, context, scope, locals);
    let path = parts
        .map(|part| ResolvedPathPart::Key(name_as_key(part)))
        .collect::<Vec<_>>();
    if path.is_empty() {
        value
    } else {
        ResolvedExpr::Access {
            base: Box::new(value),
            path,
        }
    }
}

pub(in crate::g_syntax) fn path_resolved_in_definitions(
    target: &str,
    definitions: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::Access {
        base: Box::new(definitions),
        path: target
            .split('.')
            .map(|part| ResolvedPathPart::Key(name_as_key(part)))
            .collect(),
    }
}

pub(in crate::g_syntax) fn update_module_value(
    definitions: Value,
    target: &str,
    value: Value,
    context: &CompileContext,
) -> Value {
    // Module definitions are ordered updates over the incoming namespace.
    // Ordinary dictionary literals still lower through DictUnion.
    lower_resolved_expr(apply_builtin_resolved(
        Builtin::DictUpdate,
        [
            ResolvedExpr::Embedded(path_value(target, context)),
            ResolvedExpr::Provided(value),
            ResolvedExpr::Provided(definitions),
        ],
        context,
    ))
}

pub(in crate::g_syntax) fn update_module_dict_value(
    definitions: Value,
    item: Value,
    context: &CompileContext,
) -> Value {
    match item {
        Value::Dict(dict) => update_module_dict_entries(definitions, Vec::new(), &dict, context),
        _ => definitions,
    }
}

pub(in crate::g_syntax) fn update_module_dict_entries(
    definitions: Value,
    prefix: Vec<Value>,
    dict: &Dict,
    context: &CompileContext,
) -> Value {
    dict.iter().fold(definitions, |definitions, (key, value)| {
        let mut path = prefix.clone();
        path.push(key_to_value(key, context));
        match value {
            Value::Dict(nested) if !nested.is_empty() => {
                update_module_dict_entries(definitions, path, nested, context)
            }
            _ => lower_resolved_expr(apply_builtin_resolved(
                Builtin::DictUpdate,
                [
                    ResolvedExpr::Embedded(Value::List(crate::core::List::from_values(path))),
                    ResolvedExpr::Provided(value.clone()),
                    ResolvedExpr::Provided(definitions),
                ],
                context,
            )),
        }
    })
}

pub(in crate::g_syntax) fn path_value(target: &str, context: &CompileContext) -> Value {
    Value::List(crate::core::List::from_values(
        target
            .split('.')
            .map(|part| context.value_atom(atom_from_str(part)))
            .collect(),
    ))
}

pub(in crate::g_syntax) fn key_to_value(key: &Key, context: &CompileContext) -> Value {
    match key {
        Key::Atom(atom) => context.value_atom(*atom),
        Key::Number(number) => context.value_number(number.clone()),
        Key::Binary(bytes) => Value::Binary(bytes.clone()),
        Key::AbstractGlobalPath(parts) => {
            context.value_atom(Atom::from_key(&Key::AbstractGlobalPath(parts.clone())))
        }
        Key::List(items) => Value::List(crate::core::List::from_values(
            items
                .iter()
                .map(|item| key_to_value(item, context))
                .collect(),
        )),
        Key::Dict(entries) => {
            context.value_dict(entries.iter().fold(Dict::new_sync(), |dict, (key, value)| {
                dict.insert(key.clone(), key_to_value(value, context))
            }))
        }
    }
}

pub(in crate::g_syntax) fn path_value_in_definitions(
    target: &str,
    definitions: Value,
) -> Result<Value, Diagnostic> {
    let path = target
        .split('.')
        .map(|part| ResolvedPathPart::Key(name_as_key(part)))
        .collect::<Vec<_>>();
    Ok(lower_resolved_expr(ResolvedExpr::Access {
        base: Box::new(ResolvedExpr::Provided(definitions)),
        path,
    }))
}
