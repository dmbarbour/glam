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
        target: &[SyntaxKeyExpr],
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
        let parameters =
            locals.extend_source_bindings(params.iter().map(String::as_str), self.line)?;
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
                Builtin::DictSingleton,
                [
                    ResolvedExpr::Embedded(Value::Atom(atom_from_str(key))),
                    value,
                ],
            )
        };
        let payload = apply_builtin_resolved(
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
        Ok(apply_builtin_resolved(Builtin::Anno, [annotation, value]))
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
    definition: &DefinitionDecl,
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
                definition.parameters.len(),
                locals,
            )?,
        ),
    };
    let value = decorate_reflection_boundary(&definition.target, value, scope)?;
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

    Ok(apply_reflection_boundary(value, boundary))
}

fn apply_reflection_boundary(
    value: ResolvedExpr<Value>,
    boundary: &ReflectionBoundary<ResolvedRoot>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(boundary.annotator.expr(), [value])
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
) -> ResolvedExpr<Value> {
    apply_builtin_resolved(
        Builtin::DictUpdate,
        [static_path_resolved(target), value, definitions],
    )
}

pub(in crate::g_syntax) fn update_definition_target_resolved(
    definitions: &ResolvedRoot,
    target: &[SyntaxKeyExpr],
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
    ))
}

pub(in crate::g_syntax) fn definition_target_access_resolved(
    target: &[SyntaxKeyExpr],
    definitions: &ResolvedRoot,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let path = target
        .iter()
        .map(|part| syntax_key_expr_to_resolved_path(part, line, context, scope, locals))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ResolvedExpr::Access {
        base: Box::new(definitions.expr()),
        path,
    })
}

pub(in crate::g_syntax) fn definition_target_path_resolved(
    target: &[SyntaxKeyExpr],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    syntax_path_resolved(target, line, context, scope, locals)
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

pub(in crate::g_syntax) fn static_path_resolved(target: &str) -> ResolvedExpr<Value> {
    ResolvedExpr::List(
        target
            .split('.')
            .map(|part| ResolvedExpr::Embedded(Value::Atom(atom_from_str(part))))
            .collect::<Vec<_>>(),
    )
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
) -> Value {
    // Module definitions are ordered updates over the incoming namespace.
    // Ordinary dictionary literals still lower through DictUnion.
    lower_resolved_expr(apply_builtin_resolved(
        Builtin::DictUpdate,
        [
            ResolvedExpr::Embedded(path_value(target)),
            ResolvedExpr::Provided(value),
            ResolvedExpr::Provided(definitions),
        ],
    ))
}

pub(in crate::g_syntax) fn update_module_dict_value(definitions: Value, item: Value) -> Value {
    match item {
        Value::Dict(dict) => update_module_dict_entries(definitions, Vec::new(), &dict),
        _ => definitions,
    }
}

pub(in crate::g_syntax) fn update_module_dict_entries(
    definitions: Value,
    prefix: Vec<Value>,
    dict: &Dict,
) -> Value {
    dict.iter().fold(definitions, |definitions, (key, value)| {
        let mut path = prefix.clone();
        path.push(key_to_value(key));
        match value {
            Value::Dict(nested) if !nested.is_empty() => {
                update_module_dict_entries(definitions, path, nested)
            }
            _ => lower_resolved_expr(apply_builtin_resolved(
                Builtin::DictUpdate,
                [
                    ResolvedExpr::Embedded(Value::List(crate::core::List::from_values(path))),
                    ResolvedExpr::Provided(value.clone()),
                    ResolvedExpr::Provided(definitions),
                ],
            )),
        }
    })
}

pub(in crate::g_syntax) fn path_value(target: &str) -> Value {
    Value::List(crate::core::List::from_values(
        target
            .split('.')
            .map(|part| Value::Atom(atom_from_str(part)))
            .collect(),
    ))
}

pub(in crate::g_syntax) fn key_to_value(key: &Key) -> Value {
    match key {
        Key::Atom(atom) => Value::Atom(*atom),
        Key::Number(number) => Value::Number(number.clone()),
        Key::Binary(bytes) => Value::Binary(bytes.clone()),
        Key::AbstractGlobalPath(parts) => {
            Value::Atom(Atom::from_key(&Key::AbstractGlobalPath(parts.clone())))
        }
        Key::List(items) => Value::List(crate::core::List::from_values(
            items.iter().map(key_to_value).collect(),
        )),
        Key::Dict(entries) => {
            Value::Dict(entries.iter().fold(Dict::new_sync(), |dict, (key, value)| {
                dict.insert(key.clone(), key_to_value(value))
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
