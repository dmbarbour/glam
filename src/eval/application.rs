use super::*;

pub(super) fn apply_value(
    context: &EvalContext,
    function: Value,
    argument: Value,
) -> Result<Value, EvalError> {
    match function {
        Value::Builtin(builtin) => apply_builtin(context, builtin, Vec::new(), argument),
        Value::PartialBuiltin(call) => apply_builtin(
            context,
            call.builtin,
            call.arguments.iter().cloned().collect(),
            argument,
        ),
        Value::Function(function) => apply_function_values(context, function, vec![argument]),
        Value::Net(net) => apply_net(context, net, argument),
        Value::Dict(dict) => apply_dict_value(context, &dict, argument),
        Value::Lazy(thunk) => apply_value(context, eval_lazy(context, &thunk)?, argument),
        Value::Promised(promise) => apply_value(
            context,
            eval_value(context, &Value::Promised(promise))?,
            argument,
        ),
        _ => Err(EvalError::new("application requires a function value")),
    }
}

pub(crate) fn apply_values(
    context: &EvalContext,
    mut function: Value,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match function {
            Value::Function(function_value) => {
                let arguments = std::iter::once(argument).chain(arguments).collect();
                return apply_function_values(context, function_value, arguments);
            }
            Value::Net(net) => {
                let arguments = std::iter::once(argument).chain(arguments).collect();
                return apply_explicit_net_many(context, net, arguments);
            }
            other => function = apply_value(context, other, argument)?,
        }
    }
    Ok(function)
}

pub(super) fn apply_function_values(
    context: &EvalContext,
    function: FunctionValue,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    assert!(
        !arguments.is_empty(),
        "function application requires an argument"
    );
    let remaining = function.remaining_arity();
    if arguments.len() < remaining {
        let supplied = arguments.len();
        let stage = advance_function_stage(context, function.stage().clone(), arguments)?;
        return Ok(Value::Function(FunctionValue::new(
            stage,
            remaining - supplied,
        )));
    }

    let (saturating, rest) = arguments.split_at(remaining);
    let result = Value::Lazy(LazyValue::from_function_call(
        function,
        Arc::from(saturating.to_vec()),
    ));
    if rest.is_empty() {
        Ok(result)
    } else {
        apply_values(context, result, rest.to_vec())
    }
}

pub(super) fn apply_dict_value(
    context: &EvalContext,
    dict: &crate::core::Dict,
    argument: Value,
) -> Result<Value, EvalError> {
    if let Some(function) = dict.tagged_payload(context, &keys::EFF)? {
        return Ok(effect_value(apply_effect_function_value(
            function, argument,
        )));
    }

    if let Some(function) = dict.get(&*keys::APPLY)
        && !is_undefined_dict_value(function)
    {
        return apply_value(context, eval_value(context, function)?, argument);
    }

    Err(EvalError::new("application requires a function value"))
}

pub(super) fn apply_effect_function_value(function: Value, argument: Value) -> Value {
    Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectApply,
        arguments: Arc::from([function, argument]),
    })
}

pub(super) fn effect_value(function: Value) -> Value {
    Value::Dict(crate::core::Dict::new_sync().insert((*keys::EFF).clone(), function))
}

pub(super) fn instantiate_function(
    context: &EvalContext,
    code: &FunctionCode,
    captures: Vec<Value>,
) -> Result<Value, EvalError> {
    if captures.len() != code.capture_count() {
        return Err(EvalError::new("function capture arity mismatch"));
    }
    let stage = if captures.is_empty() {
        NetValue::new(code.runtime().clone())
    } else {
        advance_function_stage(context, NetValue::new(code.runtime().clone()), captures)?
    };
    Ok(Value::Function(FunctionValue::new(stage, code.arity())))
}
