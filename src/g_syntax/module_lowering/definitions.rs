use super::super::*;
use super::objects::apply_builtin_resolved;

#[derive(Clone, Copy)]
pub(in crate::g_syntax) enum BuiltinAssertion {
    Defined,
    Undefined,
}

/// Shared source and scope state for checking and updating one definition.
pub(in crate::g_syntax) struct DefinitionTargetContext<'a> {
    access: &'a RuntimeValueAccess<'a>,
    definitions: &'a ResolvedRoot,
    line: usize,
    compiler: &'a CompileContext,
    scope: &'a NameScope<ResolvedRoot>,
}

impl<'a> DefinitionTargetContext<'a> {
    pub(in crate::g_syntax) fn new(
        access: &'a RuntimeValueAccess<'a>,
        definitions: &'a ResolvedRoot,
        line: usize,
        compiler: &'a CompileContext,
        scope: &'a NameScope<ResolvedRoot>,
    ) -> Self {
        Self {
            access,
            definitions,
            line,
            compiler,
            scope,
        }
    }

    fn lower_update(
        &self,
        target: &[SyntaxKeyExpr],
        update: &SyntaxExpr,
        sugar_param_count: usize,
        locals: &mut ResolverContext,
    ) -> Result<ResolvedExpr<Value>, Diagnostic> {
        let prior = definition_target_access_resolved(
            self.access,
            target,
            self.definitions,
            self.line,
            self.compiler,
            self.scope,
            locals,
        )?;
        if sugar_param_count == 0 {
            let update = syntax_expr_to_resolved_in_semantic_scope(
                self.access,
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
        let parameters =
            locals.extend_source_bindings(params.iter().map(String::as_str), self.line)?;
        let lowered = syntax_expr_to_resolved_in_semantic_scope(
            self.access,
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
        target: &[SyntaxKeyExpr],
        value: ResolvedExpr<Value>,
        locals: &mut ResolverContext,
    ) -> Result<ResolvedExpr<Value>, Diagnostic> {
        let tag = match assertion {
            BuiltinAssertion::Defined => "assert_defined",
            BuiltinAssertion::Undefined => "assert_undefined",
        };
        let singleton = |key: &str, value| {
            apply_builtin_resolved(
                self.access,
                Builtin::DictSingleton,
                [
                    ResolvedExpr::Embedded(Value::Atom(atom_from_str(key))),
                    value,
                ],
            )
        };
        let payload = apply_builtin_resolved(
            self.access,
            Builtin::DictUnion,
            [
                singleton(
                    "name",
                    ResolvedExpr::Embedded(Value::binary_from_text(&definition_target_name(
                        target,
                    ))),
                ),
                singleton(
                    "value",
                    definition_target_access_resolved(
                        self.access,
                        target,
                        self.definitions,
                        self.line,
                        self.compiler,
                        self.scope,
                        locals,
                    )?,
                ),
            ],
        );
        let annotation = singleton(tag, payload);
        Ok(apply_builtin_resolved(
            self.access,
            Builtin::Anno,
            [annotation, value],
        ))
    }

    pub(in crate::g_syntax) fn annotate_static(
        &self,
        assertion: BuiltinAssertion,
        target: &str,
        value: ResolvedExpr<Value>,
        locals: &mut ResolverContext,
    ) -> Result<ResolvedExpr<Value>, Diagnostic> {
        let target = target
            .split('.')
            .map(|part| SyntaxKeyExpr::Atom(part.to_owned()))
            .collect::<Vec<_>>();
        self.annotate(assertion, &target, value, locals)
    }
}

pub(in crate::g_syntax) fn lower_definition_resolved(
    access: &RuntimeValueAccess<'_>,
    definition: &DefinitionDecl,
    line: usize,
    context: &CompileContext,
    definitions: &ResolvedRoot,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let Some(expr) = &definition.expr else {
        return Ok(definitions.expr(access));
    };

    let target_scope = definition_target_scope_resolved(scope, definitions.clone());
    let target_context =
        DefinitionTargetContext::new(access, definitions, line, context, &target_scope);
    let (assertion, value) = match definition.kind {
        DefinitionKind::Introduce | DefinitionKind::Override => {
            let assertion = match definition.kind {
                DefinitionKind::Introduce => BuiltinAssertion::Undefined,
                DefinitionKind::Override => BuiltinAssertion::Defined,
                DefinitionKind::Update => unreachable!(),
            };
            let value = syntax_expr_to_resolved_in_semantic_scope(
                access, expr, line, context, scope, locals,
            )?;
            (Some(assertion), value)
        }
        DefinitionKind::Update => (
            None,
            target_context.lower_update(
                &definition.target,
                expr,
                definition.parameters.len(),
                locals,
            )?,
        ),
    };
    let value = decorate_reflection_boundary(access, &definition.target, value, scope)?;
    let value = match assertion {
        Some(assertion) => target_context.annotate(assertion, &definition.target, value, locals)?,
        None => value,
    };
    let value = annotate_definition_context(
        access,
        value,
        &definition_target_name(&definition.target),
        line,
        context,
    );
    update_definition_target_resolved(
        access,
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
    access: &RuntimeValueAccess<'_>,
    target: &[SyntaxKeyExpr],
    value: ResolvedExpr<Value>,
    scope: &NameScope<ResolvedRoot>,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let Some(boundary) = &scope.reflection else {
        return Ok(value);
    };
    let Some(SyntaxKeyExpr::Atom(root)) = target.first() else {
        // Reflection namespaces are intentionally statically recognizable.
        // A computed root might evaluate to `refl`, `meta`, or `spec`, so it
        // cannot safely receive an automatic demand boundary.
        return Ok(value);
    };
    if matches!(root.as_str(), "refl" | "meta" | "spec") {
        return Ok(value);
    }

    Ok(apply_reflection_boundary(access, value, boundary))
}

fn apply_reflection_boundary(
    access: &RuntimeValueAccess<'_>,
    value: ResolvedExpr<Value>,
    boundary: &ReflectionBoundary<ResolvedRoot>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(boundary.annotator.expr(access), [value])
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
    access: &RuntimeValueAccess<'_>,
    definitions: ResolvedExpr<Value>,
    target: &str,
    value: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    apply_builtin_resolved(
        access,
        Builtin::DictUpdate,
        [static_path_resolved(access, target), value, definitions],
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::g_syntax) fn update_definition_target_resolved(
    access: &RuntimeValueAccess<'_>,
    definitions: &ResolvedRoot,
    target: &[SyntaxKeyExpr],
    value: ResolvedExpr<Value>,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    Ok(apply_builtin_resolved(
        access,
        Builtin::DictUpdate,
        [
            definition_target_path_resolved(access, target, line, context, scope, locals)?,
            value,
            definitions.expr(access),
        ],
    ))
}

pub(in crate::g_syntax) fn definition_target_access_resolved(
    access: &RuntimeValueAccess<'_>,
    target: &[SyntaxKeyExpr],
    definitions: &ResolvedRoot,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let path = target
        .iter()
        .map(|part| syntax_key_expr_to_resolved_path(access, part, line, context, scope, locals))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ResolvedExpr::Access {
        base: Box::new(definitions.expr(access)),
        path,
    })
}

pub(in crate::g_syntax) fn definition_target_path_resolved(
    access: &RuntimeValueAccess<'_>,
    target: &[SyntaxKeyExpr],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    syntax_path_resolved(access, target, line, context, scope, locals)
}

fn definition_target_name(target: &[SyntaxKeyExpr]) -> String {
    let mut name = String::new();
    for part in target {
        match part {
            SyntaxKeyExpr::Atom(atom) => {
                if !name.is_empty() {
                    name.push('.');
                }
                name.push_str(atom);
            }
            SyntaxKeyExpr::Index(_) => name.push_str(".[computed]"),
            SyntaxKeyExpr::PathIndex(_) => name.push_str(".(computed path)"),
        }
    }
    name
}

pub(in crate::g_syntax) fn annotate_definition_context(
    access: &RuntimeValueAccess<'_>,
    value: ResolvedExpr<Value>,
    definition: &str,
    line: usize,
    context: &CompileContext,
) -> ResolvedExpr<Value> {
    let Some(origin) = context.opaque_origin(access) else {
        return value;
    };
    let compiler_context = Value::Dict(
        Dict::new_sync()
            .insert((*keys::ORIGIN).clone(), origin)
            .insert(
                (*keys::LINE).clone(),
                Value::Number(crate::number::Number::from_usize(line)),
            )
            .insert(
                (*keys::DEFINITION).clone(),
                Value::binary_from_text(definition),
            ),
    );
    let frame = Value::Dict(Dict::new_sync().insert((*keys::G).clone(), compiler_context));
    let annotation = Value::Dict(Dict::new_sync().insert((*keys::CONTEXT).clone(), frame));
    apply_builtin_resolved(
        access,
        Builtin::Anno,
        [ResolvedExpr::Embedded(annotation), value],
    )
}

pub(in crate::g_syntax) fn static_path_resolved(
    _access: &RuntimeValueAccess<'_>,
    target: &str,
) -> ResolvedExpr<Value> {
    ResolvedExpr::List(
        target
            .split('.')
            .map(|part| ResolvedExpr::Embedded(Value::Atom(atom_from_str(part))))
            .collect::<Vec<_>>(),
    )
}

pub(in crate::g_syntax) fn path_resolved_in_definitions(
    _access: &RuntimeValueAccess<'_>,
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

pub(in crate::g_syntax) fn update_module_value_in(
    access: &RuntimeValueAccess<'_>,
    definitions: Value,
    target: &str,
    value: Value,
) -> Value {
    // Module definitions are ordered updates over the incoming namespace.
    // Ordinary dictionary literals still lower through DictUnion.
    lower_resolved_expr_in(
        access,
        apply_builtin_resolved(
            access,
            Builtin::DictUpdate,
            [
                ResolvedExpr::Embedded(path_value(access, target)),
                ResolvedExpr::Provided(value),
                ResolvedExpr::Provided(definitions),
            ],
        ),
    )
}

pub(in crate::g_syntax) fn update_module_dict_value_in(
    access: &RuntimeValueAccess<'_>,
    definitions: Value,
    item: Value,
) -> Value {
    match item {
        Value::Dict(dict) => update_module_dict_entries_in(access, definitions, Vec::new(), &dict),
        _ => definitions,
    }
}

pub(in crate::g_syntax) fn update_module_dict_entries_in(
    access: &RuntimeValueAccess<'_>,
    definitions: Value,
    prefix: Vec<Value>,
    dict: &Dict,
) -> Value {
    dict.iter().fold(definitions, |definitions, (key, value)| {
        let mut path = prefix.clone();
        path.push(key.to_value_with(access.values()));
        match value {
            Value::Dict(nested) if !nested.is_empty() => {
                update_module_dict_entries_in(access, definitions, path, nested)
            }
            _ => lower_resolved_expr_in(
                access,
                apply_builtin_resolved(
                    access,
                    Builtin::DictUpdate,
                    [
                        ResolvedExpr::Embedded(Value::List(crate::core::List::from_values(path))),
                        ResolvedExpr::Provided(value.clone()),
                        ResolvedExpr::Provided(definitions),
                    ],
                ),
            ),
        }
    })
}

pub(in crate::g_syntax) fn path_value(_access: &RuntimeValueAccess<'_>, target: &str) -> Value {
    Value::List(crate::core::List::from_values(
        target
            .split('.')
            .map(|part| Value::Atom(atom_from_str(part)))
            .collect(),
    ))
}

pub(in crate::g_syntax) fn path_value_in_definitions_in(
    access: &RuntimeValueAccess<'_>,
    target: &str,
    definitions: Value,
) -> Result<Value, Diagnostic> {
    let path = target
        .split('.')
        .map(|part| ResolvedPathPart::Key(name_as_key(part)))
        .collect::<Vec<_>>();
    Ok(lower_resolved_expr_in(
        access,
        ResolvedExpr::Access {
            base: Box::new(ResolvedExpr::Provided(definitions)),
            path,
        },
    ))
}
