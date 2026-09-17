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
