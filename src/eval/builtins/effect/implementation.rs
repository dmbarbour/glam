use super::super::super::*;
use crate::list::ListItem;

pub(super) fn eval_effect_map_builtin(
    function: &Value,
    items: &Value,
) -> Result<Value, EvaluationHalt> {
    Ok(effect_value(Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectMapRun,
        arguments: Arc::from([function.clone(), items.clone(), Value::List(List::empty())]),
    })))
}

pub(super) fn eval_effect_map_run_builtin(
    context: &EvaluatorStepContext<'_>,
    function: &Value,
    items: &Value,
    results: &Value,
    api: &Value,
) -> Result<Value, EvaluationHalt> {
    let Value::List(items) = eval_value_in(context, items)? else {
        return Err(EvaluationHalt::new("effect map requires a list"));
    };
    let Value::List(results) = eval_value_in(context, results)? else {
        return Err(EvaluationHalt::new(
            "effect map internal results must be a list",
        ));
    };
    let Some((item, remaining)) = items.try_pop_front_by(
        &mut |value| context.with_value_access(|access| access.values().duplicate_value(value)),
        &mut |thunk| force_list_thunk_in(context, thunk),
    )?
    else {
        return apply_effect_api(context, api, &keys::R, vec![Value::List(results)]);
    };

    let item = match item {
        ListItem::Byte(byte) => Value::Number(Number::from_u8(byte)),
        ListItem::Value(value) => value,
    };
    let operation = apply_value_in(context, function.clone(), item)?;
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
) -> Result<Value, EvaluationHalt> {
    let Value::List(results) = results else {
        return Err(EvaluationHalt::new(
            "effect map internal results must be a list",
        ));
    };
    let results = List::concat(results.clone(), List::from_values(vec![result.clone()]));
    Ok(effect_value(Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectMapRun,
        arguments: Arc::from([function.clone(), items.clone(), Value::List(results)]),
    })))
}

pub(super) fn apply_effect_api(
    context: &EvaluatorStepContext<'_>,
    api: &Value,
    name: &Key,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    let path: Arc<[CoreDataKey]> = Arc::from([CoreDataKey::Key(name.clone())]);
    let arguments: Arc<[Value]> = arguments.into();
    Ok(Value::Lazy(context.construct_lazy(|access| {
        let selected =
            LazyValue::from_access_in(access, path, Arc::from([access.duplicate_value(api)]));
        if arguments.is_empty() {
            selected
        } else {
            LazyValue::from_application_in(access, Value::Lazy(selected), arguments)
        }
    })))
}
