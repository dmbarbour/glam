use super::*;

pub(super) fn apply_value(
    function: Value,
    argument: Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    match function {
        Value::Builtin(builtin) => apply_builtin(builtin, Vec::new(), argument, local_env),
        Value::PartialBuiltin(call) => apply_builtin(
            call.builtin,
            call.arguments.iter().cloned().collect(),
            argument,
            local_env,
        ),
        Value::Function(function) => apply_function_values(function, vec![argument]),
        Value::Net(net) => apply_net(net, argument),
        Value::Dict(dict) => apply_dict_value(&dict, argument, local_env),
        Value::Lazy(thunk) => apply_value(eval_lazy(&thunk)?, argument, local_env),
        _ => Err(EvalError::new("application requires a function value")),
    }
}

pub(crate) fn apply_values(
    mut function: Value,
    arguments: Vec<Value>,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match function {
            Value::Function(function_value) => {
                let arguments = std::iter::once(argument).chain(arguments).collect();
                return apply_function_values(function_value, arguments);
            }
            Value::Net(net) => {
                let arguments = std::iter::once(argument).chain(arguments).collect();
                return apply_explicit_net_many(net, arguments);
            }
            other => function = apply_value(other, argument, local_env)?,
        }
    }
    Ok(function)
}

pub(super) fn apply_function_values(
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
        let stage = advance_function_stage(function.stage().clone(), arguments)?;
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
        apply_values(result, rest.to_vec(), &[])
    }
}

pub(super) fn apply_dict_value(
    dict: &crate::core::Dict,
    argument: Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    if let Some(function) = singleton_effect_function(dict) {
        return Ok(effect_value(apply_effect_function_value(
            function, argument,
        )));
    }

    if let Some(function) = dict.get(&Key::atom_from_text("apply"))
        && !is_undefined_dict_value(function)
    {
        return apply_value(eval_value(function)?, argument, local_env);
    }

    Err(EvalError::new("application requires a function value"))
}

pub(super) fn singleton_effect_function(dict: &crate::core::Dict) -> Option<Value> {
    let eff_key = Key::atom_from_text("eff");
    let function = dict_effect_function(dict)?;
    if dict
        .iter()
        .all(|(key, value)| *key == eff_key || is_undefined_dict_value(value))
    {
        Some(function.clone())
    } else {
        None
    }
}

pub(super) fn dict_effect_function(dict: &crate::core::Dict) -> Option<Value> {
    let function = dict.get(&Key::atom_from_text("eff"))?;
    if is_undefined_dict_value(function) {
        None
    } else {
        Some(function.clone())
    }
}

pub(super) fn apply_effect_function_value(function: Value, argument: Value) -> Value {
    Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectApply,
        arguments: Arc::from([function, argument]),
    })
}

pub(super) fn effect_value(function: Value) -> Value {
    Value::Dict(crate::core::Dict::new_sync().insert(Key::atom_from_text("eff"), function))
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
        advance_function_stage(NetValue::new(code.runtime().clone()), captures)?
    };
    Ok(Value::Function(FunctionValue::new(stage, code.arity())))
}
