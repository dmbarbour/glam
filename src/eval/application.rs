use super::*;

pub(super) fn apply_value(
    context: &EvalContext,
    function: Value,
    argument: Value,
) -> Result<Value, EvaluationHalt> {
    with_direct_evaluator(context, |evaluator| {
        apply_value_in(evaluator, function, argument)
    })
}

pub(super) fn apply_value_in(
    context: &EvaluatorStepContext<'_>,
    function: Value,
    argument: Value,
) -> Result<Value, EvaluationHalt> {
    match function {
        Value::Builtin(builtin) => apply_builtin(context.context(), builtin, Vec::new(), argument),
        Value::PartialBuiltin(call) => apply_builtin(
            context.context(),
            call.builtin,
            call.arguments.iter().cloned().collect(),
            argument,
        ),
        Value::Function(function) => apply_function_values_in(context, function, vec![argument]),
        Value::Dict(dict) => apply_dict_value_in(context, dict, argument),
        Value::Lazy(thunk) => apply_value_in(context, eval_lazy_in(context, &thunk)?, argument),
        Value::Promised(promise) => apply_value_in(
            context,
            eval_value_in(context, &Value::Promised(promise))?,
            argument,
        ),
        value => Err(non_callable_error(&value)),
    }
}

pub(crate) fn apply_values(
    context: &EvalContext,
    function: Value,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    with_direct_evaluator(context, |evaluator| {
        apply_values_in(evaluator, function, arguments)
    })
}

pub(crate) fn apply_values_in(
    context: &EvaluatorStepContext<'_>,
    function: Value,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    if arguments.is_empty() {
        return Ok(function);
    }
    let mut function = match function {
        Value::Function(function) => {
            return apply_function_values_in(context, function, arguments);
        }
        function => function,
    };
    let mut arguments = arguments.into_iter();
    loop {
        let argument = arguments
            .next()
            .expect("non-empty application arguments must have a first value");
        function = apply_value_in(context, function, argument)?;
        if arguments.as_slice().is_empty() {
            return Ok(function);
        }
        function = match function {
            Value::Function(function_value) => {
                return apply_function_values_in(context, function_value, arguments.collect());
            }
            function => function,
        };
    }
}

#[cfg(test)]
pub(super) fn apply_function_values(
    context: &EvalContext,
    function: FunctionValue,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    with_direct_evaluator(context, |evaluator| {
        apply_function_values_in(evaluator, function, arguments)
    })
}

fn apply_function_values_in(
    context: &EvaluatorStepContext<'_>,
    function: FunctionValue,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
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
        context.context().values(),
        function,
        Arc::from(saturating),
    ));
    if rest.is_empty() {
        Ok(result)
    } else {
        apply_values_in(context, result, rest)
    }
}

fn apply_dict_value_in(
    context: &EvaluatorStepContext<'_>,
    dict: crate::core::Dict,
    argument: Value,
) -> Result<Value, EvaluationHalt> {
    if let Some(function) = tagged_payload_in(&dict, context, &keys::EFF)? {
        return Ok(effect_value(apply_effect_function_value(
            function, argument,
        )));
    }

    if let Some(function) = dict.get(&*keys::APPLY)
        && !is_undefined_dict_value(function)
    {
        return apply_value_in(context, eval_value_in(context, function)?, argument);
    }

    Err(non_callable_error(&Value::Dict(dict)))
}

pub(super) fn non_callable_error(value: &Value) -> EvaluationHalt {
    EvaluationHalt::new(format!(
        "application requires a function value, received {}",
        value.diagnostic_kind_name()
    ))
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
) -> Result<Value, EvaluationHalt> {
    if captures.len() != code.capture_count() {
        return Err(EvaluationHalt::new("function capture arity mismatch"));
    }
    let stage = if captures.is_empty() {
        NetValue::new(code.runtime().clone())
    } else {
        attach_function_stage(NetValue::new(code.runtime().clone()), captures)
    };
    Ok(Value::Function(FunctionValue::new(stage, code.arity())))
}
