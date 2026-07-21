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
    function: Value,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    if arguments.is_empty() {
        return Ok(function);
    }
    let mut function = match function {
        Value::Function(function) => {
            return apply_function_values(context, function, arguments);
        }
        function => function,
    };
    let mut arguments = arguments.into_iter();
    loop {
        let argument = arguments
            .next()
            .expect("non-empty application arguments must have a first value");
        function = apply_value(context, function, argument)?;
        if arguments.as_slice().is_empty() {
            return Ok(function);
        }
        function = match function {
            Value::Function(function_value) => {
                return apply_function_values(context, function_value, arguments.collect());
            }
            function => function,
        };
    }
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
        let stage = attach_function_stage(function.stage().clone(), arguments);
        return Ok(Value::Function(FunctionValue::new(
            stage,
            remaining - supplied,
        )));
    }

    let mut saturating = arguments;
    let rest = saturating.split_off(remaining);
    let result = Value::Lazy(LazyValue::from_function_call(
        function,
        Arc::from(saturating),
    ));
    if rest.is_empty() {
        Ok(result)
    } else {
        apply_values(context, result, rest)
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
    code: &FunctionCode,
    captures: Vec<Value>,
) -> Result<Value, EvalError> {
    if captures.len() != code.capture_count() {
        return Err(EvalError::new("function capture arity mismatch"));
    }
    let stage = if captures.is_empty() {
        NetValue::new(code.runtime().clone())
    } else {
        attach_function_stage(NetValue::new(code.runtime().clone()), captures)
    };
    Ok(Value::Function(FunctionValue::new(stage, code.arity())))
}
