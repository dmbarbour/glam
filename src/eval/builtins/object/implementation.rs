use super::super::super::*;
use crate::core::{FixpointComputation, RuntimeValueAccess};

pub(super) fn eval_object_instance_builtin(
    context: &EvaluatorStepContext<'_>,
    spec: &Value,
) -> Result<Value, EvaluationHalt> {
    Ok(Value::Lazy(context.construct_lazy(|access| {
        LazyValue::computed_fixpoint_in(
            access,
            "object self",
            FixpointComputation::ObjectInstance(spec.clone()),
        )
    })))
}

pub(super) fn eval_object_instance_from_parts_builtin(
    context: &EvaluatorStepContext<'_>,
    name: Value,
    deps: Value,
    defs: Value,
) -> Result<Value, EvaluationHalt> {
    let spec = context
        .with_value_access(|access| object_spec_from_parts(access.values(), name, deps, defs));
    eval_object_instance_builtin(context, &Value::Dict(spec))
}

fn object_spec_from_parts(
    _access: &RuntimeValueAccess<'_>,
    name: Value,
    deps: Value,
    defs: Value,
) -> crate::core::Dict {
    crate::core::Dict::new_sync()
        .insert((*keys::NAME).clone(), name)
        .insert((*keys::DEPS).clone(), deps)
        .insert((*keys::DEFS).clone(), defs)
}

/// Applies an additional stateless definitions mixin through ordinary `with`.
///
/// Objects are re-instantiated and retain the composed definitions in their
/// resulting `spec`. Plain dictionaries use the same fixpoint application but
/// remain dictionaries.
pub(super) fn eval_object_with_defs_builtin(
    context: &EvaluatorStepContext<'_>,
    object: &Value,
    extension_defs: Value,
) -> Result<Value, EvaluationHalt> {
    let object = eval_value_in(context, object)?;
    let Value::Dict(object_dict) = &object else {
        return Err(EvaluationHalt::new(
            "ordinary `with` requires a dictionary or object value",
        ));
    };
    let Some(spec) = object_dict.get(&*keys::SPEC) else {
        let extension = apply_value_in(context, extension_defs, object)?;
        return super::super::apply_builtin_in(context, Builtin::Fixpoint, Vec::new(), extension);
    };
    let spec = eval_value_in(context, spec)?;
    if context.with_value_access(|access| is_undefined_dict_value(access.values(), &spec)) {
        let extension = apply_value_in(context, extension_defs, object)?;
        return super::super::apply_builtin_in(context, Builtin::Fixpoint, Vec::new(), extension);
    }
    let Value::Dict(spec) = spec else {
        return Err(EvaluationHalt::new(
            "object instance builtin requires a specification dictionary",
        ));
    };
    let name = context.with_value_access(|access| object_spec_name_value(access.values(), &spec));
    let deps = spec
        .get(&*keys::DEPS)
        .cloned()
        .unwrap_or_else(|| Value::List(List::empty()));
    let prior_defs = context.with_value_access(|access| {
        spec.get(&*keys::DEFS)
            .cloned()
            .unwrap_or_else(|| default_object_defs_value(access.values()))
    });
    let composed_defs = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::ObjectComposedDefs,
        arguments: Arc::from([prior_defs, extension_defs]),
    });
    eval_object_instance_from_parts_builtin(context, name, deps, composed_defs)
}

pub(super) fn eval_object_composed_defs_builtin(
    context: &EvaluatorStepContext<'_>,
    prior_defs: Value,
    extension_defs: Value,
    base: Value,
    self_value: Value,
) -> Result<Value, EvaluationHalt> {
    let prior = apply_value_in(context, prior_defs, base)?;
    let prior = apply_value_in(context, prior, self_value.clone())?;
    let extended = apply_value_in(context, extension_defs, prior)?;
    apply_value_in(context, extended, self_value)
}

/// Implements the small right-biased record mixin used for assembler-owned
/// diagnostic fields. It is an internal definitions adapter, not the language
/// `with` surface or its assertion policy.
pub(super) fn eval_object_override_defs_builtin(
    context: &EvaluatorStepContext<'_>,
    updates: &Value,
    base: &Value,
) -> Result<Value, EvaluationHalt> {
    let updates = eval_value_in(context, updates)?;
    let base = eval_value_in(context, base)?;
    let (Value::Dict(updates), Value::Dict(base)) = (updates, base) else {
        return Err(EvaluationHalt::new(
            "object override definitions require dictionary values",
        ));
    };
    Ok(Value::Dict(override_dict(context, &base, &updates)?))
}

fn override_dict(
    context: &EvaluatorStepContext<'_>,
    base: &crate::core::Dict,
    updates: &crate::core::Dict,
) -> Result<crate::core::Dict, EvaluationHalt> {
    let mut result = base.clone();
    for (key, update) in updates.iter() {
        let update = match (result.get(key), update) {
            (Some(prior), Value::Dict(update)) => match eval_value_in(context, prior)? {
                Value::Dict(prior) => Value::Dict(override_dict(context, &prior, update)?),
                _ => Value::Dict(update.clone()),
            },
            _ => update.clone(),
        };
        result = result.insert(key.clone(), update);
    }
    Ok(result)
}

pub(super) fn eval_object_from_dict_builtin(
    context: &EvaluatorStepContext<'_>,
    value: &Value,
) -> Result<Value, EvaluationHalt> {
    let value = eval_value_in(context, value)?;
    let Value::Dict(dict) = value else {
        return Err(EvaluationHalt::new(
            "object_from_dict requires a dictionary value",
        ));
    };

    if let Some(spec) = dict.get(&*keys::SPEC) {
        let spec = eval_value_in(context, spec)?;
        if !context.with_value_access(|access| is_undefined_dict_value(access.values(), &spec)) {
            return Err(EvaluationHalt::new(
                "object_from_dict requires a plain dictionary, not an object",
            ));
        }
    }

    let spec = context.with_value_access(|access| dict_object_spec(access.values(), dict));
    eval_object_instance_builtin(context, &spec)
}

fn dict_object_spec(_access: &RuntimeValueAccess<'_>, dict: crate::core::Dict) -> Value {
    let defs = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::ObjectDictDefs,
        arguments: Arc::from([Value::Dict(dict)]),
    });
    let spec = crate::core::Dict::new_sync()
        .insert(
            (*keys::NAME).clone(),
            Value::Dict(crate::core::Dict::new_sync()),
        )
        .insert((*keys::DEPS).clone(), Value::List(List::empty()))
        .insert((*keys::DEFS).clone(), defs);
    Value::Dict(spec)
}

fn object_spec_name_value(_access: &RuntimeValueAccess<'_>, spec: &crate::core::Dict) -> Value {
    spec.get(&*keys::NAME)
        .cloned()
        .unwrap_or_else(|| Value::Dict(crate::core::Dict::new_sync()))
}

fn default_object_defs_value(_access: &RuntimeValueAccess<'_>) -> Value {
    Value::Builtin(Builtin::ObjectDefaultDefs)
}
