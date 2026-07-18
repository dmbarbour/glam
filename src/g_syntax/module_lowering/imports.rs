use super::super::*;
use super::objects::*;

pub(in crate::g_syntax) fn lower_import(
    import: &ImportDecl,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    if let ImportReference::Local(request) = &import.reference {
        crate::compiler::validate_local_source_request(request)
            .map_err(|message| Diagnostic::error(line, message))?;
    }
    match &import.reference {
        ImportReference::Builtin(name) => {
            if import.binary {
                return Err(Diagnostic::error(
                    line,
                    "built-in imports cannot use the `binary` modifier",
                ));
            }
            lower_builtin_import(name, &import.placement, line, context, definitions)
        }
        ImportReference::Local(request) if import.binary => {
            lower_local_binary_import(request, &import.placement, line, context, definitions)
        }
        ImportReference::Local(request) => {
            lower_local_import(request, &import.placement, context, definitions)
        }
    }
}

pub(in crate::g_syntax) fn lower_builtin_import(
    name: &str,
    placement: &ImportPlacement,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let module = builtin_module_value(context, name)
        .ok_or_else(|| Diagnostic::error(line, format!("unknown built-in module `'{name}`")))?;

    *definitions = match placement {
        ImportPlacement::Inline => update_module_dict_value(definitions.clone(), module, context),
        ImportPlacement::As(target) => update_module_value(
            definitions.clone(),
            target,
            module_object_value(target, module, context),
            context,
        ),
        ImportPlacement::At(target) => {
            let object = extend_object_with_defs(
                target,
                constant_object_defs(module),
                context,
                definitions.clone(),
            )?;
            update_module_value(definitions.clone(), target, object, context)
        }
    };

    Ok(())
}

pub(in crate::g_syntax) fn lower_local_import(
    request: &str,
    placement: &ImportPlacement,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    match placement {
        ImportPlacement::Inline => {
            *definitions = context.import_module(
                request,
                None,
                definitions.clone(),
                context.final_defs().clone(),
            );
        }
        ImportPlacement::As(target) => {
            let prior_defs = import_as_prior_defs(target, definitions.clone(), context)?;
            let loaded = scoped_local_import_value(request, target, prior_defs, context)?;
            *definitions = update_module_value(
                definitions.clone(),
                target,
                module_object_value(target, loaded, context),
                context,
            );
        }
        ImportPlacement::At(target) => {
            let scoped_prior = path_value_in_definitions(target, definitions.clone())?;
            let loaded = scoped_local_import_value(request, target, scoped_prior, context)?;
            let object = extend_object_with_defs(
                target,
                constant_object_defs(loaded),
                context,
                definitions.clone(),
            )?;
            *definitions = update_module_value(definitions.clone(), target, object, context);
        }
    };

    Ok(())
}

pub(in crate::g_syntax) fn lower_local_binary_import(
    request: &str,
    placement: &ImportPlacement,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let ImportPlacement::As(target) = placement else {
        return Err(Diagnostic::error(
            line,
            "`import ... binary` requires `as name` in the current spike",
        ));
    };

    let loaded = context.import_binary(request);
    *definitions = update_module_value(definitions.clone(), target, loaded, context);
    Ok(())
}

pub(in crate::g_syntax) fn scoped_local_import_value(
    request: &str,
    target: &str,
    prior_defs: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let final_defs = path_value_in_definitions(target, context.final_defs().clone())?;
    Ok(context.import_module(request, Some(target), prior_defs, final_defs))
}

pub(in crate::g_syntax) fn import_as_prior_defs(
    target: &str,
    definitions: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let env = inherited_import_env_object_value(target, definitions, context)?;
    Ok(update_module_value(
        context.empty_dict_value(),
        "env",
        env,
        context,
    ))
}

pub(in crate::g_syntax) fn inherited_import_env_object_value(
    target: &str,
    definitions: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let parent_env = path_value_in_definitions("env", definitions)?;
    let name = context.abstract_global_path(&format!("{target}.env"));
    let deps = lower_resolved_expr(ResolvedExpr::List(vec![object_spec_resolved(
        ResolvedExpr::Provided(parent_env),
        context,
    )]));
    Ok(object_instance_from_parts_value(
        name,
        deps,
        empty_object_defs(context),
        context,
    ))
}

pub(in crate::g_syntax) fn empty_object_defs(context: &CompileContext) -> Value {
    let mut locals = ResolverContext::default();
    let prior_self = locals.push_binding("<object-prior-self>");
    let final_self = locals.push_binding("<object-final-self>");
    lower_resolved_expr(ResolvedExpr::lambda(
        vec![prior_self, final_self],
        remove_object_spec_resolved(ResolvedExpr::Local(prior_self), context),
    ))
}

pub(in crate::g_syntax) fn module_object_value(
    target: &str,
    module: Value,
    context: &CompileContext,
) -> Value {
    lower_resolved_expr(object_instance_from_parts_resolved(
        ResolvedExpr::Embedded(context.abstract_global_path(target)),
        ResolvedExpr::List(Vec::new()),
        ResolvedExpr::Provided(constant_object_defs(module)),
        context,
    ))
}

pub(in crate::g_syntax) fn constant_object_defs(value: Value) -> Value {
    let mut locals = ResolverContext::default();
    let prior_self = locals.push_binding("<object-prior-self>");
    let final_self = locals.push_binding("<object-final-self>");
    lower_resolved_expr(ResolvedExpr::lambda(
        vec![prior_self, final_self],
        ResolvedExpr::Provided(value),
    ))
}

pub(in crate::g_syntax) fn lower_unique(
    names: &[String],
    _line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    for name in names {
        let value = context.abstract_global_path(name);
        *definitions = update_module_value(definitions.clone(), name, value, context);
    }
    Ok(())
}

pub(in crate::g_syntax) fn builtin_module_value(
    context: &CompileContext,
    name: &str,
) -> Option<Value> {
    match name {
        "math" => Some(context.value_dict(builtin_math_module(context))),
        "list" => Some(context.value_dict(builtin_list_module(context))),
        "std" | "prelude" => Some(context.value_dict(builtin_std_module(context))),
        _ => None,
    }
}

pub(in crate::g_syntax) fn builtin_math_module(context: &CompileContext) -> Dict {
    Dict::new_sync()
        .insert(name_as_key("floor"), context.value_builtin(Builtin::Floor))
        .insert(name_as_key("mod"), context.value_builtin(Builtin::Mod))
}

pub(in crate::g_syntax) fn builtin_list_module(context: &CompileContext) -> Dict {
    Dict::new_sync()
        .insert(name_as_key("slice"), context.value_builtin(Builtin::Slice))
        .insert(
            name_as_key("split"),
            context.value_builtin(Builtin::ListSplit),
        )
        .insert(
            name_as_key("split_end"),
            context.value_builtin(Builtin::ListSplitEnd),
        )
        .insert(name_as_key("map"), context.value_builtin(Builtin::Map))
        .insert(name_as_key("len"), context.value_builtin(Builtin::ListLen))
        .insert(
            name_as_key("head"),
            context.value_builtin(Builtin::ListHead),
        )
        .insert(
            name_as_key("tail"),
            context.value_builtin(Builtin::ListTail),
        )
        .insert(
            name_as_key("pure"),
            context.value_builtin(Builtin::ListEffect),
        )
}

pub(in crate::g_syntax) fn builtin_std_module(context: &CompileContext) -> Dict {
    Dict::new_sync()
        .insert(name_as_key("anno"), context.value_builtin(Builtin::Anno))
        .insert(name_as_key("not"), builtin_not_value(context))
        .insert(name_as_key("could"), builtin_could_value(context))
        .insert(
            name_as_key("math"),
            context.value_dict(builtin_math_module(context)),
        )
        .insert(
            name_as_key("list"),
            context.value_dict(builtin_list_module(context)),
        )
        .insert(
            name_as_key("eff"),
            context.value_dict(Dict::new_sync().insert(
                name_as_key("map"),
                context.value_builtin(Builtin::EffectMap),
            )),
        )
}

pub(in crate::g_syntax) fn builtin_not_value(context: &CompileContext) -> Value {
    let mut locals = ResolverContext::default();
    let condition = locals.push_binding("<not-condition>");
    let fail_operation = lower_effect_expr_resolved("fail", context, &mut locals);
    let true_operation = effect_call_resolved(
        "r",
        [ResolvedExpr::Embedded(context.unit_value())],
        context,
        &mut locals,
    );
    let returned_failure = effect_call_resolved("r", [fail_operation], context, &mut locals);
    let fail_if_condition_succeeds = effect_then_resolved(
        ResolvedExpr::Local(condition),
        returned_failure,
        context,
        &mut locals,
    );
    let succeed_if_condition_fails =
        effect_call_resolved("r", [true_operation], context, &mut locals);
    let alternate = effect_call_resolved(
        "alt",
        [fail_if_condition_succeeds, succeed_if_condition_fails],
        context,
        &mut locals,
    );
    let select_operation = effect_call_resolved("cut", [alternate], context, &mut locals);
    let selected = locals.push_binding("<selected-operation>");
    let run_selected_operation =
        ResolvedExpr::lambda(vec![selected], ResolvedExpr::Local(selected));
    let body = effect_call_resolved(
        "seq",
        [select_operation, run_selected_operation],
        context,
        &mut locals,
    );
    lower_resolved_expr(ResolvedExpr::lambda(vec![condition], body))
}

pub(in crate::g_syntax) fn builtin_could_value(context: &CompileContext) -> Value {
    let not = builtin_not_value(context);
    let mut locals = ResolverContext::default();
    let condition = locals.push_binding("<could-condition>");
    let inner = ResolvedExpr::apply(
        ResolvedExpr::Provided(not.clone()),
        [ResolvedExpr::Local(condition)],
    );
    lower_resolved_expr(ResolvedExpr::lambda(
        vec![condition],
        ResolvedExpr::apply(ResolvedExpr::Provided(not), [inner]),
    ))
}
