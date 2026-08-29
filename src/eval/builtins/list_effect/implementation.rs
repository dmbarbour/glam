use super::super::super::*;

pub(super) fn eval_list_effect_builtin(
    context: &EvaluatorStepContext<'_>,
    effect: &Value,
) -> Result<Value, EvaluationHalt> {
    Ok(Value::List(lazy_run_list_effect(
        context.context().values(),
        effect.clone(),
    )))
}

pub(super) fn eval_list_effect_seq_builtin(
    context: &EvaluatorStepContext<'_>,
    operation: &Value,
    continuation: &Value,
) -> Result<Value, EvaluationHalt> {
    Ok(Value::List(flat_map_list_effect_results(
        context.context().values(),
        lazy_run_list_effect(context.context().values(), operation.clone()),
        continuation.clone(),
    )))
}

pub(super) fn eval_list_effect_alt_builtin(
    context: &EvaluatorStepContext<'_>,
    left: &Value,
    right: &Value,
) -> Result<Value, EvaluationHalt> {
    Ok(Value::List(List::concat(
        lazy_run_list_effect(context.context().values(), left.clone()),
        lazy_run_list_effect(context.context().values(), right.clone()),
    )))
}

pub(super) fn eval_list_effect_cut_builtin(
    context: &EvaluatorStepContext<'_>,
    operation: &Value,
) -> Result<Value, EvaluationHalt> {
    Ok(Value::List(cut_list_effect_results(
        context.context().values(),
        operation.clone(),
    )))
}

pub(super) fn eval_list_effect_fix_builtin(
    context: &EvaluatorStepContext<'_>,
    function: &Value,
) -> Result<Value, EvaluationHalt> {
    let function = eval_value_in(context, function)?;
    let handle = PromisedValue::new(context.context().values(), "list effect fixpoint");
    let marker = Value::Promised(handle.clone());
    let operation = apply_value_in(context, function, marker.clone())?;
    Ok(Value::List(fix_list_effect_results(
        context.context().values(),
        operation,
        handle,
    )))
}

fn lazy_run_list_effect(values: &CoreValueFactory, effect: Value) -> List {
    deferred_list(values, "list effect", move |context| {
        run_list_effect_to_list(context, effect.clone())
    })
}

fn run_list_effect_to_list(context: &EvalContext, effect: Value) -> Result<List, EvaluationHalt> {
    let effect = eval_value(context, &effect)?;
    let Value::Dict(dict) = effect else {
        return Err(EvaluationHalt::new(format!(
            "list effect handler requires an effect dictionary, got {effect:?}"
        )));
    };
    let Some(function) = dict
        .get(&*keys::EFF)
        .filter(|function| !is_undefined_dict_value(function))
        .cloned()
    else {
        return Err(EvaluationHalt::new(
            "list effect handler requires an `eff` member",
        ));
    };

    let handled = apply_value(context, eval_value(context, &function)?, list_effect_api())?;
    let handled = eval_value(context, &handled)?;
    let Value::List(results) = handled else {
        return Err(EvaluationHalt::new(format!(
            "list effect handler expected a standard effect result list, got {handled:?}"
        )));
    };
    Ok(results)
}

fn flat_map_list_effect_results(
    values: &CoreValueFactory,
    results: List,
    continuation: Value,
) -> List {
    deferred_list(values, "list effect seq", move |context| {
        let Some((head, tail)) = pop_list_front(context, &results)? else {
            return Ok(List::empty());
        };
        let continuation = eval_value(context, &continuation)?;
        let next = apply_value(context, continuation.clone(), head)?;
        Ok(List::concat(
            lazy_run_list_effect(context.values(), next),
            flat_map_list_effect_results(context.values(), tail, continuation),
        ))
    })
}

fn cut_list_effect_results(values: &CoreValueFactory, operation: Value) -> List {
    deferred_list(values, "list effect cut", move |context| {
        let results = lazy_run_list_effect(context.values(), operation.clone());
        let Some((head, _)) = pop_list_front(context, &results)? else {
            return Ok(List::empty());
        };
        Ok(List::from_values(vec![head]))
    })
}

fn fix_list_effect_results(
    values: &CoreValueFactory,
    operation: Value,
    handle: PromisedValue,
) -> List {
    deferred_list(values, "list effect fix", move |context| {
        let results = lazy_run_list_effect(context.values(), operation.clone());
        let Some((head, tail)) = pop_list_front(context, &results)? else {
            handle
                .set(Value::List(List::empty()))
                .map_err(|_| EvaluationHalt::new("list effect fix initialized twice"))?;
            return Ok(List::empty());
        };
        handle
            .set(head.clone())
            .map_err(|_| EvaluationHalt::new("list effect fix initialized twice"))?;
        Ok(List::concat(List::from_values(vec![head]), tail))
    })
}

fn deferred_list(
    values: &CoreValueFactory,
    label: &'static str,
    thunk: impl Fn(&EvalContext) -> Result<List, EvaluationHalt> + Send + Sync + 'static,
) -> List {
    List::from_thunk(
        LazyValue::deferred(values, label, move |context| {
            thunk(context).map(Value::List)
        })
        .into(),
    )
}

fn list_effect_api() -> Value {
    Value::Dict(
        crate::core::Dict::new_sync()
            .insert(
                (*keys::R).clone(),
                Value::Builtin(Builtin::ListEffectReturn),
            )
            .insert((*keys::SEQ).clone(), Value::Builtin(Builtin::ListEffectSeq))
            .insert((*keys::ALT).clone(), Value::Builtin(Builtin::ListEffectAlt))
            .insert((*keys::FAIL).clone(), Value::List(List::empty()))
            .insert((*keys::CUT).clone(), Value::Builtin(Builtin::ListEffectCut))
            .insert((*keys::FIX).clone(), Value::Builtin(Builtin::ListEffectFix)),
    )
}
