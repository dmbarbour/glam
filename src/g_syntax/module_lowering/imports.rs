use super::super::*;
use super::objects::*;

pub(in crate::g_syntax) fn lower_import(
    import: &ImportDecl,
    line: usize,
    context: &CompileContext,
    access: &RuntimeValueAccess<'_>,
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
            lower_builtin_import(name, &import.placement, line, context, access, definitions)
        }
        ImportReference::Local(request) if import.binary => lower_local_binary_import(
            request,
            &import.placement,
            line,
            context,
            access,
            definitions,
        ),
        ImportReference::Local(request) => {
            lower_local_import(request, &import.placement, context, access, definitions)
        }
    }
}

pub(in crate::g_syntax) fn lower_builtin_import(
    name: &str,
    placement: &ImportPlacement,
    line: usize,
    context: &CompileContext,
    access: &RuntimeValueAccess<'_>,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let module = compiler_values::builtin_module(access, name)
        .ok_or_else(|| Diagnostic::error(line, format!("unknown built-in module `'{name}`")))?;

    *definitions = match placement {
        ImportPlacement::Inline => {
            update_module_dict_value_in(access, access.duplicate_value(definitions), module.value)
        }
        ImportPlacement::As(target) => update_module_value_in(
            access,
            access.duplicate_value(definitions),
            target,
            module_object_value_with_defs_in(access, target, module.definitions, context),
        ),
        ImportPlacement::At(target) => {
            let object = extend_object_with_defs_in(
                access,
                target,
                module.definitions,
                access.duplicate_value(definitions),
            )?;
            update_module_value_in(access, access.duplicate_value(definitions), target, object)
        }
    };

    Ok(())
}

pub(in crate::g_syntax) fn lower_local_import(
    request: &str,
    placement: &ImportPlacement,
    context: &CompileContext,
    access: &RuntimeValueAccess<'_>,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    match placement {
        ImportPlacement::Inline => {
            *definitions = context.import_module_in(
                access,
                request,
                None,
                access.duplicate_value(definitions),
                context.final_defs(access),
            );
        }
        ImportPlacement::As(target) => {
            let prior_defs = import_as_prior_defs_in(
                access,
                target,
                access.duplicate_value(definitions),
                context,
            )?;
            let loaded =
                scoped_local_import_value_in(access, request, target, prior_defs, context)?;
            *definitions = update_module_value_in(
                access,
                access.duplicate_value(definitions),
                target,
                module_object_value_in(access, target, loaded, context),
            );
        }
        ImportPlacement::At(target) => {
            let scoped_prior =
                path_value_in_definitions_in(access, target, access.duplicate_value(definitions))?;
            let loaded =
                scoped_local_import_value_in(access, request, target, scoped_prior, context)?;
            let loaded_defs = constant_object_defs(access, context, loaded);
            let object = extend_object_with_defs_in(
                access,
                target,
                loaded_defs.clone_core_with(access),
                access.duplicate_value(definitions),
            )?;
            *definitions =
                update_module_value_in(access, access.duplicate_value(definitions), target, object);
        }
    };

    Ok(())
}

pub(in crate::g_syntax) fn lower_local_binary_import(
    request: &str,
    placement: &ImportPlacement,
    line: usize,
    context: &CompileContext,
    access: &RuntimeValueAccess<'_>,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let ImportPlacement::As(target) = placement else {
        return Err(Diagnostic::error(
            line,
            "`import ... binary` requires `as name`",
        ));
    };

    let loaded = context.import_binary_in(access, request);
    *definitions =
        update_module_value_in(access, access.duplicate_value(definitions), target, loaded);
    Ok(())
}

fn scoped_local_import_value_in(
    access: &RuntimeValueAccess<'_>,
    request: &str,
    target: &str,
    prior_defs: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let final_defs = path_value_in_definitions_in(access, target, context.final_defs(access))?;
    Ok(context.import_module_in(access, request, Some(target), prior_defs, final_defs))
}

fn import_as_prior_defs_in(
    access: &RuntimeValueAccess<'_>,
    target: &str,
    definitions: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let env = inherited_import_env_object_value_in(access, target, definitions, context)?;
    Ok(update_module_value_in(
        access,
        Value::Dict(Dict::new_sync()),
        "env",
        env,
    ))
}

fn inherited_import_env_object_value_in(
    access: &RuntimeValueAccess<'_>,
    target: &str,
    definitions: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let parent_env = path_value_in_definitions_in(access, "env", definitions)?;
    let name = context.abstract_global_path(access, &format!("{target}.env"));
    let deps = lower_resolved_expr_in(
        access,
        ResolvedExpr::List(vec![object_spec_resolved(
            access,
            ResolvedExpr::Provided(parent_env),
        )]),
    );
    Ok(object_instance_from_parts_value_in(
        access,
        name,
        deps,
        compiler_values::empty_object_defs(access),
    ))
}

fn module_object_value_in(
    access: &RuntimeValueAccess<'_>,
    target: &str,
    module: Value,
    context: &CompileContext,
) -> Value {
    let definitions = constant_object_defs(access, context, module);
    module_object_value_with_defs_in(access, target, definitions.clone_core_with(access), context)
}

fn module_object_value_with_defs_in(
    access: &RuntimeValueAccess<'_>,
    target: &str,
    definitions: Value,
    context: &CompileContext,
) -> Value {
    lower_resolved_expr_in(
        access,
        object_instance_from_parts_resolved(
            access,
            ResolvedExpr::Embedded(context.abstract_global_path(access, target)),
            ResolvedExpr::List(Vec::new()),
            ResolvedExpr::Provided(definitions),
        ),
    )
}

pub(in crate::g_syntax) fn constant_object_defs(
    access: &RuntimeValueAccess<'_>,
    context: &CompileContext,
    value: Value,
) -> crate::runtime::RuntimeValueRoot {
    debug_assert!(access.belongs_to(context.values()));
    compiler_values::constant_object_defs(access, value)
}

pub(in crate::g_syntax) fn lower_unique(
    names: &[String],
    _line: usize,
    context: &CompileContext,
    access: &RuntimeValueAccess<'_>,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    for name in names {
        let value = context.abstract_global_path(access, name);
        *definitions =
            update_module_value_in(access, access.duplicate_value(definitions), name, value);
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::g_syntax) fn builtin_list_module() -> Dict {
    compiler_values::builtin_list_module(&crate::compiler::test_value_factory())
}
