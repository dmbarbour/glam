use super::super::super::*;

pub(super) fn eval_list_effect_builtin(
    _context: &EvalContext,
    effect: &Value,
) -> Result<Value, EvalError> {
    Ok(Value::List(lazy_run_list_effect(effect.clone())))
}

pub(super) fn eval_list_effect_seq_builtin(
    _context: &EvalContext,
    operation: &Value,
    continuation: &Value,
) -> Result<Value, EvalError> {
    Ok(Value::List(flat_map_list_effect_results(
        lazy_run_list_effect(operation.clone()),
        continuation.clone(),
    )))
}

pub(super) fn eval_list_effect_alt_builtin(
    _context: &EvalContext,
    left: &Value,
    right: &Value,
) -> Result<Value, EvalError> {
    Ok(Value::List(List::concat(
        lazy_run_list_effect(left.clone()),
        lazy_run_list_effect(right.clone()),
    )))
}

pub(super) fn eval_list_effect_cut_builtin(
    _context: &EvalContext,
    operation: &Value,
) -> Result<Value, EvalError> {
    Ok(Value::List(cut_list_effect_results(operation.clone())))
}

pub(super) fn eval_list_effect_fix_builtin(
    context: &EvalContext,
    function: &Value,
) -> Result<Value, EvalError> {
    let function = eval_value(context, function)?;
    let handle = LazyValue::promised("list effect fixpoint");
    let marker = Value::Lazy(handle.clone());
    let operation = apply_value(context, function, marker.clone())?;
    Ok(Value::List(fix_list_effect_results(operation, handle)))
}

fn lazy_run_list_effect(effect: Value) -> List {
    deferred_list("list effect", move |context| {
        run_list_effect_to_list(context, effect.clone())
    })
}

fn run_list_effect_to_list(context: &EvalContext, effect: Value) -> Result<List, EvalError> {
    let effect = force_value_shell(context, &effect)?;
    let Value::Dict(dict) = effect else {
        return Err(EvalError::new(format!(
            "list effect handler requires an effect dictionary, got {effect:?}"
        )));
    };
    let Some(function) = dict
        .get(&*keys::EFF)
        .filter(|function| !is_undefined_dict_value(function))
        .cloned()
    else {
        return Err(EvalError::new(
            "list effect handler requires an `eff` member",
        ));
    };

    let handled = apply_value(context, eval_value(context, &function)?, list_effect_api())?;
    let handled = force_value_shell(context, &handled)?;
    let Value::List(results) = handled else {
        return Err(EvalError::new(format!(
            "list effect handler expected a standard effect result list, got {handled:?}"
        )));
    };
    Ok(results)
}

fn flat_map_list_effect_results(results: List, continuation: Value) -> List {
    deferred_list("list effect seq", move |context| {
        let Some((head, tail)) = pop_list_front(context, &results)? else {
            return Ok(List::empty());
        };
        let continuation = eval_value(context, &continuation)?;
        let next = apply_value(context, continuation.clone(), head)?;
        Ok(List::concat(
            lazy_run_list_effect(next),
            flat_map_list_effect_results(tail, continuation),
        ))
    })
}

fn cut_list_effect_results(operation: Value) -> List {
    deferred_list("list effect cut", move |context| {
        let results = lazy_run_list_effect(operation.clone());
        let Some((head, _)) = pop_list_front(context, &results)? else {
            return Ok(List::empty());
        };
        Ok(List::from_values(vec![head]))
    })
}

fn fix_list_effect_results(operation: Value, handle: LazyValue) -> List {
    deferred_list("list effect fix", move |context| {
        let results = lazy_run_list_effect(operation.clone());
        let Some((head, tail)) = pop_list_front(context, &results)? else {
            handle
                .set(Value::List(List::empty()))
                .map_err(|_| EvalError::new("list effect fix initialized twice"))?;
            return Ok(List::empty());
        };
        handle
            .set(head.clone())
            .map_err(|_| EvalError::new("list effect fix initialized twice"))?;
        Ok(List::concat(List::from_values(vec![head]), tail))
    })
}

fn deferred_list(
    label: &'static str,
    thunk: impl Fn(&EvalContext) -> Result<List, EvalError> + Send + Sync + 'static,
) -> List {
    List::from_thunk(LazyValue::deferred(label, move |context| {
        thunk(context)
            .map(Value::List)
            .map_err(|err| err.to_string())
    }))
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
