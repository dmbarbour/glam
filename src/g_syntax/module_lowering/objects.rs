use super::super::*;
use super::definitions::*;

pub(in crate::g_syntax) fn lower_object(
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let mut locals = ResolverContext::default();
    let scope = NameScope::module(context, definitions.clone()).resolved();
    let definitions_root = ResolvedRoot::Provided(definitions.clone());
    let name = ResolvedExpr::Embedded(context.abstract_global_path(&object.target));
    let object_value = object_decl_resolved_in_scope(
        object,
        line,
        context,
        scope.clone(),
        &mut locals,
        name,
        automatic_reflection_for_declared_target(&object.target, context),
    )?;
    let target_context = DefinitionTargetContext::new(&definitions_root, line, context, &scope);
    let object_value = target_context.annotate(
        BuiltinAssertion::Undefined,
        &object.target,
        object_value,
        &mut locals,
    )?;
    *definitions = lower_resolved_expr(update_module_resolved(
        definitions_root.expr(),
        &object.target,
        object_value,
        context,
    ));
    Ok(())
}

pub(in crate::g_syntax) fn object_instance_from_parts_value(
    name: Value,
    deps: Value,
    defs: Value,
    context: &CompileContext,
) -> Value {
    lower_resolved_expr(object_instance_from_parts_resolved(
        ResolvedExpr::Provided(name),
        ResolvedExpr::Provided(deps),
        ResolvedExpr::Provided(defs),
        context,
    ))
}

pub(in crate::g_syntax) fn apply_builtin_resolved(
    builtin: Builtin,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
    context: &CompileContext,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(context.value_builtin(builtin)),
        arguments,
    )
}

pub(in crate::g_syntax) fn object_spec_resolved(
    value: ResolvedExpr<Value>,
    context: &CompileContext,
) -> ResolvedExpr<Value> {
    apply_builtin_resolved(Builtin::ObjectSpec, [value], context)
}

pub(in crate::g_syntax) fn object_instance_from_parts_resolved(
    name: ResolvedExpr<Value>,
    deps: ResolvedExpr<Value>,
    defs: ResolvedExpr<Value>,
    context: &CompileContext,
) -> ResolvedExpr<Value> {
    apply_builtin_resolved(
        Builtin::ObjectInstanceFromParts,
        [name, deps, defs],
        context,
    )
}

pub(in crate::g_syntax) fn object_decl_resolved_in_scope(
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    parent_scope: NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
    name: ResolvedExpr<Value>,
    automatic_reflection: bool,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let defs = object_body_defs_resolved_in_scope(
        &object.body,
        object.alias.as_deref(),
        line,
        context,
        parent_scope.clone(),
        locals,
        automatic_reflection,
    )?;
    let deps = object
        .deps
        .iter()
        .map(|dep| {
            object_spec_resolved(
                path_resolved_in_scope(dep, context, &parent_scope, locals),
                context,
            )
        })
        .collect::<Vec<_>>();
    Ok(object_instance_from_parts_resolved(
        name,
        ResolvedExpr::List(deps),
        defs,
        context,
    ))
}

pub(in crate::g_syntax) fn object_body_defs_resolved_in_scope(
    body: &[ObjectBodyDefinition],
    alias: Option<&str>,
    _line: usize,
    context: &CompileContext,
    parent_scope: NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
    automatic_reflection: bool,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let base_len = locals.len();
    let prior_self = locals.push_binding("<object-prior-self>");
    let final_self = locals.push_binding("<object-final-self>");
    let object_final_defs = ResolvedRoot::Local(final_self);
    let mut bindings = ResolvedBindings::default();
    let reflection_guard = automatic_reflection.then(|| {
        bindings.bind(
            locals,
            "<object-reflection-guard>",
            object_reflection_guard_resolved(object_final_defs.expr(), context),
        )
    });
    let mut definitions = bindings.bind(
        locals,
        "<object-visible-defs>",
        remove_object_spec_resolved(ResolvedExpr::Local(prior_self), context),
    );

    for body_definition in body {
        let scope = object_body_scope_resolved(
            alias,
            object_final_defs.clone(),
            definitions.clone(),
            parent_scope.clone(),
            reflection_guard.clone(),
        );
        let updated = lower_object_body_item_resolved(
            body_definition,
            context,
            &definitions,
            &scope,
            locals,
        )?;
        definitions = bindings.bind(
            locals,
            "<object-visible-defs>",
            remove_object_spec_resolved(updated, context),
        );
    }

    let body = bindings.wrap(definitions.expr());
    locals.truncate(base_len);
    Ok(ResolvedExpr::lambda(vec![prior_self, final_self], body))
}

pub(in crate::g_syntax) fn lower_object_body_item_resolved(
    item: &ObjectBodyDefinition,
    context: &CompileContext,
    definitions: &ResolvedRoot,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    match &item.kind {
        ObjectBodyDefinitionKind::Definition(definition) => lower_definition_resolved(
            definition,
            item.text.as_str(),
            item.line,
            context,
            definitions,
            scope,
            locals,
        ),
        ObjectBodyDefinitionKind::Object(object) => {
            lower_nested_object_resolved(object, item.line, context, definitions, scope, locals)
        }
    }
}

pub(in crate::g_syntax) fn lower_nested_object_resolved(
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    definitions: &ResolvedRoot,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let name = hierarchical_object_name_resolved(&object.target, line, context, scope)?;
    let object_value = object_decl_resolved_in_scope(
        object,
        line,
        context,
        scope.clone(),
        locals,
        name,
        scope.reflection.is_some()
            && automatic_reflection_for_declared_target(&object.target, context),
    )?;
    let target_context = DefinitionTargetContext::new(definitions, line, context, scope);
    let object_value = target_context.annotate(
        BuiltinAssertion::Undefined,
        &object.target,
        object_value,
        locals,
    )?;
    Ok(update_module_resolved(
        definitions.expr(),
        &object.target,
        object_value,
        context,
    ))
}

pub(in crate::g_syntax) fn hierarchical_object_name_resolved(
    target: &str,
    line: usize,
    context: &CompileContext,
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
            .map(|part| ResolvedExpr::Embedded(context.value_atom(atom_from_str(part))))
            .collect::<Vec<_>>(),
    );
    Ok(apply_builtin_resolved(
        Builtin::ObjectLocalName,
        [host.expr(), parts],
        context,
    ))
}

pub(in crate::g_syntax) fn remove_object_spec_resolved(
    value: ResolvedExpr<Value>,
    context: &CompileContext,
) -> ResolvedExpr<Value> {
    apply_builtin_resolved(
        Builtin::DictUpdate,
        [
            static_path_resolved("spec", context),
            ResolvedExpr::Embedded(context.empty_dict_value()),
            value,
        ],
        context,
    )
}

fn object_reflection_guard_resolved(
    object_final_defs: ResolvedExpr<Value>,
    context: &CompileContext,
) -> ResolvedExpr<Value> {
    let object_name = ResolvedExpr::Access {
        base: Box::new(object_spec_resolved(object_final_defs, context)),
        path: vec![ResolvedPathPart::Key(name_as_key("name"))],
    };
    ResolvedExpr::List(vec![
        ResolvedExpr::Embedded((*keys::OBJECT_REFLECTION_GUARD_VALUE).clone()),
        object_name,
    ])
}

fn automatic_reflection_for_declared_target(target: &str, context: &CompileContext) -> bool {
    context.automatic_reflection_boundaries()
        && !matches!(target.split('.').next(), Some("refl" | "meta" | "spec"))
}

pub(in crate::g_syntax) fn object_body_scope_resolved(
    alias: Option<&str>,
    object_final_defs: ResolvedRoot,
    object_prior_defs: ResolvedRoot,
    parent: NameScope<ResolvedRoot>,
    reflection_guard: Option<ResolvedRoot>,
) -> NameScope<ResolvedRoot> {
    let object_alias = alias
        .map(local_name_metadata)
        .and_then(|alias| alias.canonical);
    let (final_defs, prior_defs) = if object_alias.is_some() {
        (parent.final_defs.clone(), parent.prior_defs.clone())
    } else {
        (object_final_defs.clone(), object_prior_defs.clone())
    };

    NameScope {
        final_defs,
        prior_defs,
        module_final_defs: parent.module_final_defs.clone(),
        module_prior_defs: parent.module_prior_defs.clone(),
        object_alias,
        object_final_defs: Some(object_final_defs.clone()),
        object_prior_defs: Some(object_prior_defs),
        reflection: reflection_guard.map(|guard| ReflectionBoundary {
            final_defs: object_final_defs,
            guard,
        }),
        parent: Some(Box::new(parent)),
    }
}

pub(in crate::g_syntax) fn lower_extend(
    extend: &ObjectExtendDecl,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let mut locals = ResolverContext::default();
    let scope = NameScope::module(context, definitions.clone()).resolved();
    let definitions_root = ResolvedRoot::Provided(definitions.clone());
    let extension_defs = object_body_defs_resolved_in_scope(
        &extend.body,
        extend.alias.as_deref(),
        line,
        context,
        scope.clone(),
        &mut locals,
        automatic_reflection_for_declared_target(&extend.target, context),
    )?;
    let prior_object = path_resolved_in_definitions(&extend.target, definitions_root.expr());
    let prior_spec = object_spec_resolved(prior_object, context);
    let mut bindings = ResolvedBindings::default();
    let prior_spec = bindings.bind(&mut locals, "<extended-object-spec>", prior_spec);
    let spec_member = |name| ResolvedExpr::Access {
        base: Box::new(prior_spec.expr()),
        path: vec![ResolvedPathPart::Key(name_as_key(name))],
    };
    let prior_defs = spec_member("defs");
    let base = locals.push_binding("<extension-base>");
    let self_value = locals.push_binding("<extension-self>");
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
    let object_value = bindings.wrap(object_instance_from_parts_resolved(
        spec_member("name"),
        spec_member("deps"),
        composed_defs,
        context,
    ));
    let target_context = DefinitionTargetContext::new(&definitions_root, line, context, &scope);
    let object_value = target_context.annotate(
        BuiltinAssertion::Defined,
        &extend.target,
        object_value,
        &mut locals,
    )?;
    *definitions = lower_resolved_expr(update_module_resolved(
        definitions_root.expr(),
        &extend.target,
        object_value,
        context,
    ));
    Ok(())
}

pub(in crate::g_syntax) fn extend_object_with_defs(
    target: &str,
    extension_defs: Value,
    context: &CompileContext,
    visible_definitions: Value,
) -> Result<Value, Diagnostic> {
    let mut locals = ResolverContext::default();
    let prior_object =
        path_resolved_in_definitions(target, ResolvedExpr::Provided(visible_definitions));
    let prior_spec = object_spec_resolved(prior_object, context);
    let mut bindings = ResolvedBindings::default();
    let prior_spec = bindings.bind(&mut locals, "<extended-object-spec>", prior_spec);
    let spec_member = |name| ResolvedExpr::Access {
        base: Box::new(prior_spec.expr()),
        path: vec![ResolvedPathPart::Key(name_as_key(name))],
    };
    let base = locals.push_binding("<extension-base>");
    let self_value = locals.push_binding("<extension-self>");
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
    Ok(lower_resolved_expr(bindings.wrap(
        object_instance_from_parts_resolved(
            spec_member("name"),
            spec_member("deps"),
            composed_defs,
            context,
        ),
    )))
}
