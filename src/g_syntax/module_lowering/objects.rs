use super::super::*;
use super::definitions::*;
use super::journal::{JournalOp, appended_journal_resolved, journal_bundle_in};

pub(in crate::g_syntax) fn lower_object(
    access: &RuntimeValueAccess<'_>,
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    definitions: Value,
    module_scope: &NameScope<Value>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let scope = module_scope.resolved_in(access);
    let definitions_root = ResolvedRoot::Provided(definitions);
    let name = ResolvedExpr::Embedded(context.abstract_global_path(access, &object.target));
    let object_value = object_decl_resolved_in_scope(
        access,
        object,
        line,
        context,
        scope.duplicate_in(access),
        locals,
        name,
        declared_target_has_reflection(&object.target),
    )?;
    // The module's journal checks the introduction.
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
    let reflection_boundary = reflection_guard.map(|guard| {
        bindings.bind(
            access,
            locals,
            "<object-reflection-boundary>",
            compiler_values::reflection_boundary_resolved(
                access,
                guard.expr(access),
                compiler_values::reflection_scanner_resolved(
                    access,
                    guard.expr(access),
                    object_final_defs.expr(access),
                ),
            ),
        )
    });
    let mut definitions = bindings.bind(
        access,
        locals,
        "<object-visible-defs>",
        remove_object_spec_resolved(access, ResolvedExpr::Local(prior_self)),
    );

    // The object's journal checks its members where its boundary replays
    // it; elsewhere they keep their run-time assertions.
    let journaled = reflection_boundary.is_some();
    let mut journal = Vec::new();
    for body_definition in body {
        journal.extend(object_body_journal_entries(body_definition));
        if matches!(
            body_definition.kind,
            ObjectBodyDefinitionKind::Declare(_) | ObjectBodyDefinitionKind::Assume(_)
        ) {
            continue;
        }
        let scope = object_body_scope_resolved(
            access,
            alias,
            object_final_defs.duplicate_in(access),
            definitions.duplicate_in(access),
            parent_scope.duplicate_in(access),
            reflection_boundary
                .as_ref()
                .map(|root| root.duplicate_in(access)),
            base_len,
        );
        // A `let` group binds here, against the definitions so far, for
        // the members after it.
        if let ObjectBodyDefinitionKind::Let(group) = &body_definition.kind {
            bindings.bind_group(
                access,
                lower_let_group_resolved(
                    access,
                    group,
                    body_definition.line,
                    context,
                    &scope,
                    locals,
                )?,
            );
            continue;
        }
        let updated = lower_object_body_item_resolved(
            access,
            body_definition,
            context,
            &definitions,
            &scope,
            journaled,
            locals,
        )?;
        definitions = bindings.bind(
            access,
            locals,
            "<object-visible-defs>",
            remove_object_spec_resolved(access, updated),
        );
    }
    if !journal.is_empty() {
        let journal = appended_journal_resolved(
            access,
            "meta.journal",
            definitions.expr(access),
            journal_bundle_in(access, context, journal),
        );
        definitions = bindings.bind(
            access,
            locals,
            "<object-journaled-defs>",
            update_module_resolved(access, definitions.expr(access), "meta.journal", journal),
        );
    }

    let body = bindings.wrap(access, definitions.expr(access));
    locals.truncate(base_len);
    Ok(ResolvedExpr::lambda(vec![prior_self, final_self], body))
}

/// What an object body item records in the object's journal, with whole
/// paths: an object's nested namespaces keep no journals of their own.
fn object_body_journal_entries(item: &ObjectBodyDefinition) -> Vec<(JournalOp, Vec<Key>, usize)> {
    let path = |target: &str| target.split('.').map(name_as_key).collect::<Vec<_>>();
    let line = item.line;
    match &item.kind {
        ObjectBodyDefinitionKind::Definition(definition) if definition.expr.is_some() => {
            let op = match definition.kind {
                DefinitionKind::Introduce => JournalOp::Introduce,
                DefinitionKind::Override => JournalOp::Override,
                DefinitionKind::Update => JournalOp::Update,
            };
            // A computed target keeps its run-time assertion instead.
            definition
                .target
                .iter()
                .map(|key| match key {
                    SyntaxKeyExpr::Atom(name) => Some(name_as_key(name)),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .map(|path| vec![(op, path, line)])
                .unwrap_or_default()
        }
        ObjectBodyDefinitionKind::Definition(_) => Vec::new(),
        ObjectBodyDefinitionKind::Object(object) => {
            vec![(JournalOp::Introduce, path(&object.target), line)]
        }
        ObjectBodyDefinitionKind::Extend(extend) => {
            vec![(JournalOp::Override, path(&extend.target), line)]
        }
        ObjectBodyDefinitionKind::Declare(paths) => paths
            .iter()
            .map(|target| (JournalOp::Declare, path(target), line))
            .collect(),
        ObjectBodyDefinitionKind::Assume(paths) => paths
            .iter()
            .map(|target| (JournalOp::Assume, path(target), line))
            .collect(),
        ObjectBodyDefinitionKind::Let(_) => Vec::new(),
    }
}

/// Lowers an object or dict `with` body item. A `journaled` one's object
/// journal is checked by the object's boundary, in place of run-time
/// assertions.
pub(in crate::g_syntax) fn lower_object_body_item_resolved(
    access: &RuntimeValueAccess<'_>,
    item: &ObjectBodyDefinition,
    context: &CompileContext,
    definitions: &ResolvedRoot,
    scope: &NameScope<ResolvedRoot>,
    journaled: bool,
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
            journaled,
            locals,
        ),
        ObjectBodyDefinitionKind::Object(object) => lower_nested_object_resolved(
            access,
            object,
            item.line,
            context,
            definitions,
            scope,
            journaled,
            locals,
        ),
        ObjectBodyDefinitionKind::Extend(extend) => lower_nested_extend_resolved(
            access,
            extend,
            item.line,
            context,
            definitions,
            scope,
            journaled,
            locals,
        ),
        ObjectBodyDefinitionKind::Declare(_) | ObjectBodyDefinitionKind::Assume(_) => {
            Err(Diagnostic::error(
                item.line,
                "`declare` and `assume` belong to modules and object bodies, not dict `with` bodies",
            ))
        }
        ObjectBodyDefinitionKind::Let(_) => {
            unreachable!("a body binds its `let` groups where it lowers its members")
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::g_syntax) fn lower_nested_object_resolved(
    access: &RuntimeValueAccess<'_>,
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    definitions: &ResolvedRoot,
    scope: &NameScope<ResolvedRoot>,
    journaled: bool,
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
    // The object's journal checks the introduction where it is replayed.
    let object_value = if journaled {
        object_value
    } else {
        DefinitionTargetContext::new(access, definitions, line, context, scope).annotate_static(
            BuiltinAssertion::Undefined,
            &object.target,
            object_value,
            locals,
        )?
    };
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

/// The key a declared object's reflection state lives under, by its final
/// spec's name, an abstract path for a declared object, so an object that
/// inherits keys its own: `[spec.name, 'meta, 'refl]:()`.
pub(in crate::g_syntax) fn object_reflection_guard_resolved(
    access: &RuntimeValueAccess<'_>,
    object_final_defs: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    let name = ResolvedExpr::Access {
        base: Box::new(object_spec_resolved(access, object_final_defs)),
        path: vec![ResolvedPathPart::Key(name_as_key("name"))],
    };
    compiler_values::reflection_state_key_resolved(access, name)
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
    reflection_boundary: Option<ResolvedRoot>,
    body_locals: usize,
) -> NameScope<ResolvedRoot> {
    // An object's names are its own unless an alias other than `self`
    // names it; `alias _` names nothing and leaves names to the parent.
    // Localized names hide the locals bound outside the body, below
    // `body_locals`, as they hide the parent's names: both take `^`. A
    // `using` scope localizes the same way.
    let alias = alias.map(local_name_metadata);
    let localized = alias
        .as_ref()
        .is_none_or(|alias| alias.canonical.as_deref() == Some("self"));
    let object_alias = alias.and_then(|alias| alias.canonical);
    let (final_defs, prior_defs) = if localized {
        (
            object_final_defs.duplicate_in(access),
            object_prior_defs.duplicate_in(access),
        )
    } else {
        (
            parent.final_defs.duplicate_in(access),
            parent.prior_defs.duplicate_in(access),
        )
    };
    let local_floor = if localized {
        body_locals
    } else {
        parent.local_floor
    };

    NameScope {
        final_defs,
        prior_defs,
        module_final_defs: parent.module_final_defs.duplicate_in(access),
        module_prior_defs: parent.module_prior_defs.duplicate_in(access),
        object_alias,
        object_final_defs: Some(object_final_defs.duplicate_in(access)),
        object_prior_defs: Some(object_prior_defs),
        local_floor,
        reflection: reflection_boundary.map(|demand| ReflectionBoundary { demand }),
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
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let scope = module_scope.resolved_in(access);
    let definitions_root = ResolvedRoot::Provided(definitions);
    extend_object_resolved_in_scope(
        access,
        extend,
        line,
        context,
        &definitions_root,
        &scope,
        locals,
        declared_target_has_reflection(&extend.target),
        true,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::g_syntax) fn lower_nested_extend_resolved(
    access: &RuntimeValueAccess<'_>,
    extend: &ObjectExtendDecl,
    line: usize,
    context: &CompileContext,
    definitions: &ResolvedRoot,
    scope: &NameScope<ResolvedRoot>,
    journaled: bool,
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
        journaled,
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
    journaled: bool,
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
    // A module's journal checks that the object is defined.
    let object_value = if journaled {
        object_value
    } else {
        DefinitionTargetContext::new(access, definitions, line, context, scope).annotate_static(
            BuiltinAssertion::Defined,
            &extend.target,
            object_value,
            locals,
        )?
    };
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
