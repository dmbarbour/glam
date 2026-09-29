use super::super::*;
use super::definitions::*;

pub(in crate::g_syntax) fn lower_object(
    access: &RuntimeValueAccess<'_>,
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    definitions: Value,
    module_scope: &NameScope<Value>,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let mut locals = ResolverContext::default();
    let scope = module_scope.resolved_in(access);
    let definitions_root = ResolvedRoot::Provided(definitions);
    let name = ResolvedExpr::Embedded(context.abstract_global_path(access, &object.target));
    let object_value = object_decl_resolved_in_scope(
        access,
        object,
        line,
        context,
        scope.duplicate_in(access),
        &mut locals,
        name,
        declared_target_has_reflection(&object.target),
    )?;
    let target_context =
        DefinitionTargetContext::new(access, &definitions_root, line, context, &scope);
    let object_value = target_context.annotate_static(
        BuiltinAssertion::Undefined,
        &object.target,
        object_value,
        &mut locals,
    )?;
    let object_value =
        annotate_definition_context(access, object_value, &object.target, line, context);
    Ok(update_module_resolved(
        access,
        definitions_root.expr(access),
        &object.target,
        object_value,
    ))
}

pub(in crate::g_syntax) fn object_instance_from_parts_value_in(
    access: &RuntimeValueAccess<'_>,
    name: Value,
    deps: Value,
    defs: Value,
) -> Value {
    lower_resolved_expr_in(
        access,
        object_instance_from_parts_resolved(
            access,
            ResolvedExpr::Provided(name),
            ResolvedExpr::Provided(deps),
            ResolvedExpr::Provided(defs),
        ),
    )
}

pub(in crate::g_syntax) fn apply_builtin_resolved(
    _access: &RuntimeValueAccess<'_>,
    builtin: Builtin,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(ResolvedExpr::Embedded(Value::Builtin(builtin)), arguments)
}

pub(in crate::g_syntax) fn object_spec_resolved(
    access: &RuntimeValueAccess<'_>,
    value: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    apply_builtin_resolved(access, Builtin::ObjectSpec, [value])
}

pub(in crate::g_syntax) fn object_instance_from_parts_resolved(
    access: &RuntimeValueAccess<'_>,
    name: ResolvedExpr<Value>,
    deps: ResolvedExpr<Value>,
    defs: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    apply_builtin_resolved(access, Builtin::ObjectInstanceFromParts, [name, deps, defs])
}

#[allow(clippy::too_many_arguments)]
pub(in crate::g_syntax) fn object_decl_resolved_in_scope(
    access: &RuntimeValueAccess<'_>,
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    parent_scope: NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
    name: ResolvedExpr<Value>,
    declared_reflection: bool,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let deps = object_parents_resolved(access, &object.deps, line, context, &parent_scope, locals)?;
    let defs = object_body_defs_resolved_in_scope(
        access,
        &object.body,
        object.alias.as_deref(),
        line,
        context,
        parent_scope.duplicate_in(access),
        locals,
        declared_reflection,
    )?;
    Ok(object_from_parts_resolved(
        access,
        object.realization,
        name,
        ResolvedExpr::List(deps),
        defs,
    ))
}

pub(in crate::g_syntax) fn object_from_parts_resolved(
    access: &RuntimeValueAccess<'_>,
    realization: ObjectRealization,
    name: ResolvedExpr<Value>,
    deps: ResolvedExpr<Value>,
    defs: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    match realization {
        ObjectRealization::Instance => {
            object_instance_from_parts_resolved(access, name, deps, defs)
        }
        ObjectRealization::Abstract => {
            let spec = resolved_record(access, [("name", name), ("deps", deps), ("defs", defs)]);
            resolved_record(access, [("spec", spec)])
        }
    }
}

fn resolved_record(
    access: &RuntimeValueAccess<'_>,
    fields: impl IntoIterator<Item = (&'static str, ResolvedExpr<Value>)>,
) -> ResolvedExpr<Value> {
    let mut fields = fields.into_iter().map(|(name, value)| {
        apply_builtin_resolved(
            access,
            Builtin::DictSingleton,
            [
                ResolvedExpr::Embedded(Value::Atom(atom_from_str(name))),
                value,
            ],
        )
    });
    let Some(first) = fields.next() else {
        return ResolvedExpr::Embedded(Value::Dict(Dict::new_sync()));
    };
    fields.fold(first, |record, field| {
        apply_builtin_resolved(access, Builtin::DictUnion, [record, field])
    })
}

pub(in crate::g_syntax) fn object_parents_resolved(
    access: &RuntimeValueAccess<'_>,
    parents: &[SyntaxExpr],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<Vec<ResolvedExpr<Value>>, Diagnostic> {
    parents
        .iter()
        .map(|parent| {
            let parent = syntax_expr_to_resolved_in_semantic_scope(
                access, parent, line, context, scope, locals,
            )?;
            Ok(object_spec_resolved(access, parent))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(in crate::g_syntax) fn object_body_defs_resolved_in_scope(
    access: &RuntimeValueAccess<'_>,
    body: &[ObjectBodyDefinition],
    alias: Option<&str>,
    _line: usize,
    context: &CompileContext,
    parent_scope: NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
    declared_reflection: bool,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let base_len = locals.len();
    let prior_self = locals.push_internal_binding("<object-prior-self>");
    let final_self = locals.push_internal_binding("<object-final-self>");
    let object_final_defs = ResolvedRoot::Local(final_self);
    let mut bindings = ResolvedBindings::default();
    let reflection_guard = declared_reflection.then(|| {
        bindings.bind(
            access,
            locals,
            "<object-reflection-guard>",
            object_reflection_guard_resolved(access, object_final_defs.expr(access)),
        )
    });
    let reflection_annotator = reflection_guard.map(|guard| {
        bindings.bind(
            access,
            locals,
            "<object-reflection-annotator>",
            compiler_values::reflection_annotator_resolved(
                access,
                guard.expr(access),
                object_final_defs.expr(access),
            ),
        )
    });
    let mut definitions = bindings.bind(
        access,
        locals,
        "<object-visible-defs>",
        remove_object_spec_resolved(access, ResolvedExpr::Local(prior_self)),
    );

    for body_definition in body {
        let scope = object_body_scope_resolved(
            access,
            alias,
            object_final_defs.duplicate_in(access),
            definitions.duplicate_in(access),
            parent_scope.duplicate_in(access),
            reflection_annotator
                .as_ref()
                .map(|root| root.duplicate_in(access)),
        );
        let updated = lower_object_body_item_resolved(
            access,
            body_definition,
            context,
            &definitions,
            &scope,
            locals,
        )?;
        definitions = bindings.bind(
            access,
            locals,
            "<object-visible-defs>",
            remove_object_spec_resolved(access, updated),
        );
    }

    let body = bindings.wrap(access, definitions.expr(access));
    locals.truncate(base_len);
    Ok(ResolvedExpr::lambda(vec![prior_self, final_self], body))
}

pub(in crate::g_syntax) fn lower_object_body_item_resolved(
    access: &RuntimeValueAccess<'_>,
    item: &ObjectBodyDefinition,
    context: &CompileContext,
    definitions: &ResolvedRoot,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    match &item.kind {
        ObjectBodyDefinitionKind::Definition(definition) => lower_definition_resolved(
            access,
            definition,
            item.line,
            context,
            definitions,
            scope,
            locals,
        ),
        ObjectBodyDefinitionKind::Object(object) => lower_nested_object_resolved(
            access,
            object,
            item.line,
            context,
            definitions,
            scope,
            locals,
        ),
        ObjectBodyDefinitionKind::Extend(extend) => lower_nested_extend_resolved(
            access,
            extend,
            item.line,
            context,
            definitions,
            scope,
            locals,
        ),
    }
}

pub(in crate::g_syntax) fn lower_nested_object_resolved(
    access: &RuntimeValueAccess<'_>,
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    definitions: &ResolvedRoot,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let name = hierarchical_object_name_resolved(access, &object.target, line, scope)?;
    let object_value = object_decl_resolved_in_scope(
        access,
        object,
        line,
        context,
        scope.duplicate_in(access),
        locals,
        name,
        scope.reflection.is_some() && declared_target_has_reflection(&object.target),
    )?;
    let target_context = DefinitionTargetContext::new(access, definitions, line, context, scope);
    let object_value = target_context.annotate_static(
        BuiltinAssertion::Undefined,
        &object.target,
        object_value,
        locals,
    )?;
    let object_value =
        annotate_definition_context(access, object_value, &object.target, line, context);
    Ok(update_module_resolved(
        access,
        definitions.expr(access),
        &object.target,
        object_value,
    ))
}

pub(in crate::g_syntax) fn hierarchical_object_name_resolved(
    access: &RuntimeValueAccess<'_>,
    target: &str,
    line: usize,
    scope: &NameScope<ResolvedRoot>,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let Some(host) = &scope.object_final_defs else {
        return Err(Diagnostic::error(
            line,
            "nested object declaration requires an object scope",
        ));
    };
    let parts = ResolvedExpr::List(
        target
            .split('.')
            .map(|part| ResolvedExpr::Embedded(Value::Atom(atom_from_str(part))))
            .collect::<Vec<_>>(),
    );
    Ok(apply_builtin_resolved(
        access,
        Builtin::ObjectLocalName,
        [host.expr(access), parts],
    ))
}

pub(in crate::g_syntax) fn remove_object_spec_resolved(
    access: &RuntimeValueAccess<'_>,
    value: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    apply_builtin_resolved(
        access,
        Builtin::DictUpdate,
        [
            static_path_resolved(access, "spec"),
            ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())),
            value,
        ],
    )
}

fn object_reflection_guard_resolved(
    access: &RuntimeValueAccess<'_>,
    object_final_defs: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    let object_name = ResolvedExpr::Access {
        base: Box::new(object_spec_resolved(access, object_final_defs)),
        path: vec![ResolvedPathPart::Key(name_as_key("name"))],
    };
    ResolvedExpr::List(vec![
        ResolvedExpr::Embedded(access.values().object_reflection_guard()),
        object_name,
    ])
}

fn declared_target_has_reflection(target: &str) -> bool {
    !matches!(target.split('.').next(), Some("refl" | "meta" | "spec"))
}

pub(in crate::g_syntax) fn object_body_scope_resolved(
    access: &RuntimeValueAccess<'_>,
    alias: Option<&str>,
    object_final_defs: ResolvedRoot,
    object_prior_defs: ResolvedRoot,
    parent: NameScope<ResolvedRoot>,
    reflection_annotator: Option<ResolvedRoot>,
) -> NameScope<ResolvedRoot> {
    let object_alias = alias
        .map(local_name_metadata)
        .and_then(|alias| alias.canonical);
    let (final_defs, prior_defs) = if object_alias.is_some() {
        (
            parent.final_defs.duplicate_in(access),
            parent.prior_defs.duplicate_in(access),
        )
    } else {
        (
            object_final_defs.duplicate_in(access),
            object_prior_defs.duplicate_in(access),
        )
    };

    NameScope {
        final_defs,
        prior_defs,
        module_final_defs: parent.module_final_defs.duplicate_in(access),
        module_prior_defs: parent.module_prior_defs.duplicate_in(access),
        object_alias,
        object_final_defs: Some(object_final_defs.duplicate_in(access)),
        object_prior_defs: Some(object_prior_defs),
        reflection: reflection_annotator.map(|annotator| ReflectionBoundary { annotator }),
        parent: Some(Box::new(parent)),
    }
}

pub(in crate::g_syntax) fn lower_extend(
    access: &RuntimeValueAccess<'_>,
    extend: &ObjectExtendDecl,
    line: usize,
    context: &CompileContext,
    definitions: Value,
    module_scope: &NameScope<Value>,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let mut locals = ResolverContext::default();
    let scope = module_scope.resolved_in(access);
    let definitions_root = ResolvedRoot::Provided(definitions);
    extend_object_resolved_in_scope(
        access,
        extend,
        line,
        context,
        &definitions_root,
        &scope,
        &mut locals,
        declared_target_has_reflection(&extend.target),
    )
}

pub(in crate::g_syntax) fn lower_nested_extend_resolved(
    access: &RuntimeValueAccess<'_>,
    extend: &ObjectExtendDecl,
    line: usize,
    context: &CompileContext,
    definitions: &ResolvedRoot,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    extend_object_resolved_in_scope(
        access,
        extend,
        line,
        context,
        definitions,
        scope,
        locals,
        scope.reflection.is_some() && declared_target_has_reflection(&extend.target),
    )
}

#[allow(clippy::too_many_arguments)]
fn extend_object_resolved_in_scope(
    access: &RuntimeValueAccess<'_>,
    extend: &ObjectExtendDecl,
    line: usize,
    context: &CompileContext,
    definitions: &ResolvedRoot,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
    declared_reflection: bool,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let extension_defs = object_body_defs_resolved_in_scope(
        access,
        &extend.body,
        extend.alias.as_deref(),
        line,
        context,
        scope.duplicate_in(access),
        locals,
        declared_reflection,
    )?;
    let prior_object =
        path_resolved_in_definitions(access, &extend.target, definitions.expr(access));
    let prior_spec = object_spec_resolved(access, prior_object);
    let mut bindings = ResolvedBindings::default();
    let prior_spec = bindings.bind(access, locals, "<extended-object-spec>", prior_spec);
    let spec_member = |name| ResolvedExpr::Access {
        base: Box::new(prior_spec.expr(access)),
        path: vec![ResolvedPathPart::Key(name_as_key(name))],
    };
    let prior_defs = spec_member("defs");
    let base = locals.push_internal_binding("<extension-base>");
    let self_value = locals.push_internal_binding("<extension-self>");
    let prior_result = ResolvedExpr::apply(
        prior_defs,
        [ResolvedExpr::Local(base), ResolvedExpr::Local(self_value)],
    );
    let composed_defs = ResolvedExpr::lambda(
        vec![base, self_value],
        ResolvedExpr::apply(
            extension_defs,
            [prior_result, ResolvedExpr::Local(self_value)],
        ),
    );
    let object_value = bindings.wrap(
        access,
        object_from_parts_resolved(
            access,
            extend.realization,
            spec_member("name"),
            spec_member("deps"),
            composed_defs,
        ),
    );
    let target_context = DefinitionTargetContext::new(access, definitions, line, context, scope);
    let object_value = target_context.annotate_static(
        BuiltinAssertion::Defined,
        &extend.target,
        object_value,
        locals,
    )?;
    let object_value =
        annotate_definition_context(access, object_value, &extend.target, line, context);
    Ok(update_module_resolved(
        access,
        definitions.expr(access),
        &extend.target,
        object_value,
    ))
}

pub(in crate::g_syntax) fn extend_object_with_defs_in(
    access: &RuntimeValueAccess<'_>,
    target: &str,
    extension_defs: Value,
    visible_definitions: Value,
) -> Result<Value, Diagnostic> {
    let mut locals = ResolverContext::default();
    let prior_object =
        path_resolved_in_definitions(access, target, ResolvedExpr::Provided(visible_definitions));
    let prior_spec = object_spec_resolved(access, prior_object);
    let mut bindings = ResolvedBindings::default();
    let prior_spec = bindings.bind(access, &mut locals, "<extended-object-spec>", prior_spec);
    let spec_member = |name| ResolvedExpr::Access {
        base: Box::new(prior_spec.expr(access)),
        path: vec![ResolvedPathPart::Key(name_as_key(name))],
    };
    let base = locals.push_internal_binding("<extension-base>");
    let self_value = locals.push_internal_binding("<extension-self>");
    let prior_result = ResolvedExpr::apply(
        spec_member("defs"),
        [ResolvedExpr::Local(base), ResolvedExpr::Local(self_value)],
    );
    let composed_defs = ResolvedExpr::lambda(
        vec![base, self_value],
        ResolvedExpr::apply(
            ResolvedExpr::Provided(extension_defs),
            [prior_result, ResolvedExpr::Local(self_value)],
        ),
    );
    Ok(lower_resolved_expr_in(
        access,
        bindings.wrap(
            access,
            object_from_parts_resolved(
                access,
                ObjectRealization::Instance,
                spec_member("name"),
                spec_member("deps"),
                composed_defs,
            ),
        ),
    ))
}
