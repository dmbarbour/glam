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
    let module = compiler_values::builtin_module(name)
        .ok_or_else(|| Diagnostic::error(line, format!("unknown built-in module `'{name}`")))?;

    *definitions = match placement {
        ImportPlacement::Inline => update_module_dict_value(definitions.clone(), module.value),
        ImportPlacement::As(target) => update_module_value(
            definitions.clone(),
            target,
            module_object_value_with_defs(target, module.definitions, context),
        ),
        ImportPlacement::At(target) => {
            let object = extend_object_with_defs(target, module.definitions, definitions.clone())?;
            update_module_value(definitions.clone(), target, object)
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
            );
        }
        ImportPlacement::At(target) => {
            let scoped_prior = path_value_in_definitions(target, definitions.clone())?;
            let loaded = scoped_local_import_value(request, target, scoped_prior, context)?;
            let object =
                extend_object_with_defs(target, constant_object_defs(loaded), definitions.clone())?;
            *definitions = update_module_value(definitions.clone(), target, object);
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
            "`import ... binary` requires `as name`",
        ));
    };

    let loaded = context.import_binary(request);
    *definitions = update_module_value(definitions.clone(), target, loaded);
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
        Value::Dict(Dict::new_sync()),
        "env",
        env,
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
    )]));
    Ok(object_instance_from_parts_value(
        name,
        deps,
        compiler_values::empty_object_defs(),
    ))
}

pub(in crate::g_syntax) fn module_object_value(
    target: &str,
    module: Value,
    context: &CompileContext,
) -> Value {
    module_object_value_with_defs(target, constant_object_defs(module), context)
}

fn module_object_value_with_defs(
    target: &str,
    definitions: Value,
    context: &CompileContext,
) -> Value {
    lower_resolved_expr(object_instance_from_parts_resolved(
        ResolvedExpr::Embedded(context.abstract_global_path(target)),
        ResolvedExpr::List(Vec::new()),
        ResolvedExpr::Provided(definitions),
    ))
}

pub(in crate::g_syntax) fn constant_object_defs(value: Value) -> Value {
    compiler_values::constant_object_defs(value)
}

pub(in crate::g_syntax) fn lower_unique(
    names: &[String],
    _line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    for name in names {
        let value = context.abstract_global_path(name);
        *definitions = update_module_value(definitions.clone(), name, value);
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::g_syntax) fn builtin_list_module() -> Dict {
    compiler_values::builtin_list_module()
}
