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
    Ok(context.values().with_boundary_access(|access| {
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

pub(super) fn non_callable_error(access: &RuntimeValueAccess<'_>, value: &Value) -> EvaluationHalt {
    EvaluationHalt::new(format!(
        "application requires a function value, received {}",
        access.diagnostic_kind_name(value)
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

/// Whether `value`, a predicate's answer as [`predicate_effect`] builds it,
/// holds; `None` for any other value.
pub(super) fn predicate_effect_holds(
    _access: &RuntimeValueAccess<'_>,
    value: &Value,
) -> Option<bool> {
    let Value::Dict(effect) = value else {
        return None;
    };
    if effect.size() != 1 {
        return None;
    }
    let Value::PartialBuiltin(call) = effect.get(&keys::EFF)? else {
        return None;
    };
    if call.builtin != Builtin::EffectCall {
        return None;
    }
    let [Value::Atom(request), _] = call.arguments.as_ref() else {
        return None;
    };
    let request = request.key();
    if *request == Key::binary_from_text("r") {
        Some(true)
    } else if *request == Key::binary_from_text("fail") {
        Some(false)
    } else {
        None
    }
}

/// The effect a predicate answers, as comparisons do: `.r ()` where it
/// holds, and `.fail` where it does not.
pub(super) fn predicate_effect(access: &RuntimeValueAccess<'_>, holds: bool) -> Value {
    let (effect, arguments) = if holds {
        (
            "r",
            vec![Value::Atom(crate::core::Atom::from_key(
                &Key::abstract_global_path(["builtin", "unit"]),
            ))],
        )
    } else {
        ("fail", Vec::new())
    };
    effect_value(
        access,
        Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::EffectCall,
            arguments: Arc::from([
                Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(effect))),
                Value::List(List::from_values(arguments)),
            ]),
        }),
    )
}
