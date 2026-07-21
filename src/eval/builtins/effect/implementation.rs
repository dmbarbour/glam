use super::super::super::*;
use crate::core::FixpointComputation;
use crate::list::ListItem;

pub(super) fn eval_fixpoint_builtin(
    context: &EvalContext,
    function: &Value,
) -> Result<Value, EvalError> {
    let function = eval_value(context, function)?;
    if !matches!(function, Value::Function(_) | Value::Net(_)) {
        return Err(EvalError::new("fixpoint builtin requires a function value"));
    }

    Ok(Value::Lazy(LazyValue::computed_fixpoint(
        "fixpoint",
        FixpointComputation::Function(function),
    )))
}

pub(super) fn eval_effect_map_builtin(function: &Value, items: &Value) -> Result<Value, EvalError> {
    Ok(effect_value(Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectMapRun,
        arguments: Arc::from([function.clone(), items.clone(), Value::List(List::empty())]),
    })))
}

pub(super) fn eval_effect_map_run_builtin(
    context: &EvalContext,
    function: &Value,
    items: &Value,
    results: &Value,
    api: &Value,
) -> Result<Value, EvalError> {
    let Value::List(items) = force_value_shell(context, items)? else {
        return Err(EvalError::new("effect map requires a list"));
    };
    let Value::List(results) = force_value_shell(context, results)? else {
        return Err(EvalError::new("effect map internal results must be a list"));
    };
    let Some((item, remaining)) =
        items.try_pop_front(&mut |thunk| force_list_thunk(context, thunk))?
    else {
        return apply_effect_api(context, api, &keys::R, vec![Value::List(results)]);
    };

    let item = match item {
        ListItem::Byte(byte) => Value::Number(Number::from_u8(byte)),
        ListItem::Value(value) => value,
    };
    let operation = apply_value(context, function.clone(), item)?;
    let continuation = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectMapContinue,
        arguments: Arc::from([
            function.clone(),
            Value::List(remaining),
            Value::List(results),
        ]),
    });
    apply_effect_api(context, api, &keys::SEQ, vec![operation, continuation])
}

pub(super) fn eval_effect_map_continue_builtin(
    function: &Value,
    items: &Value,
    results: &Value,
    result: &Value,
) -> Result<Value, EvalError> {
    let Value::List(results) = results else {
        return Err(EvalError::new("effect map internal results must be a list"));
    };
    let results = List::concat(results.clone(), List::from_values(vec![result.clone()]));
    Ok(effect_value(Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectMapRun,
        arguments: Arc::from([function.clone(), items.clone(), Value::List(results)]),
    })))
}

fn apply_effect_api(
    context: &EvalContext,
    api: &Value,
    name: &Key,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    let function = resolve_core_access(
        context,
        std::slice::from_ref(api),
        &[CoreDataKey::Key(name.clone())],
    )?;
    apply_values(context, function, arguments)
}
