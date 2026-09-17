use super::*;
use crate::core::RuntimeValueAccess;

#[cfg(test)]
pub(super) fn apply_value(
    context: &EvalContext,
    function: Value,
    argument: Value,
) -> Result<Value, EvaluationHalt> {
    apply_values(context, function, vec![argument])
}

#[cfg(test)]
pub(crate) fn apply_values(
    context: &EvalContext,
    function: Value,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    if arguments.is_empty() {
        return Ok(function);
    }
    Ok(context.values().with_runtime_value_access(|access| {
        Value::Lazy(LazyValue::from_application_in(
            &access,
            function,
            Arc::from(arguments),
        ))
    }))
}

#[cfg(test)]
pub(super) fn apply_function_values(
    context: &EvalContext,
    function: FunctionValue,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    assert!(
        !arguments.is_empty(),
        "function application requires an argument"
    );
    apply_values(context, Value::Function(function), arguments)
}

pub(super) fn non_callable_error(
    _access: &RuntimeValueAccess<'_>,
    value: &Value,
) -> EvaluationHalt {
    EvaluationHalt::new(format!(
        "application requires a function value, received {}",
        value.diagnostic_kind_name()
    ))
}

pub(super) fn apply_effect_function_value(
    _access: &RuntimeValueAccess<'_>,
    function: Value,
    argument: Value,
) -> Value {
    Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectApply,
        arguments: Arc::from([function, argument]),
    })
}

pub(super) fn effect_value(_access: &RuntimeValueAccess<'_>, function: Value) -> Value {
    Value::Dict(crate::core::Dict::new_sync().insert((*keys::EFF).clone(), function))
}
