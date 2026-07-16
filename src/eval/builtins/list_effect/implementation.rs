use super::super::super::*;

pub(super) fn eval_list_effect_builtin(
    effect: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    Ok(Value::List(lazy_run_list_effect(
        effect.clone(),
        Arc::from(local_env.to_vec()),
    )))
}

pub(super) fn eval_list_effect_seq_builtin(
    operation: &Value,
    continuation: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    Ok(Value::List(flat_map_list_effect_results(
        lazy_run_list_effect(operation.clone(), Arc::from(local_env.to_vec())),
        continuation.clone(),
        Arc::from(local_env.to_vec()),
    )))
}

pub(super) fn eval_list_effect_alt_builtin(
    left: &Value,
    right: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    Ok(Value::List(List::concat(
        lazy_run_list_effect(left.clone(), Arc::from(local_env.to_vec())),
        lazy_run_list_effect(right.clone(), Arc::from(local_env.to_vec())),
    )))
}

pub(super) fn eval_list_effect_cut_builtin(
    operation: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    Ok(Value::List(cut_list_effect_results(
        operation.clone(),
        Arc::from(local_env.to_vec()),
    )))
}

pub(super) fn eval_list_effect_fix_builtin(
    function: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let function = eval_value(function)?;
    let handle = LazyValue::pending("list effect fixpoint");
    let marker = Value::Lazy(handle.clone());
    let operation = apply_value(function, marker.clone(), local_env)?;
    Ok(Value::List(fix_list_effect_results(
        operation,
        handle,
        Arc::from(local_env.to_vec()),
    )))
}

fn lazy_run_list_effect(effect: Value, local_env: Arc<[Value]>) -> List {
    deferred_list("list effect", move || {
        run_list_effect_to_list(effect.clone(), local_env.clone())
    })
}

fn run_list_effect_to_list(effect: Value, local_env: Arc<[Value]>) -> Result<List, EvalError> {
    let effect = force_value_shell(&effect)?;
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

    let handled = apply_value(eval_value(&function)?, list_effect_api(), &local_env)?;
    let handled = force_value_shell(&handled)?;
    let Value::List(results) = handled else {
        return Err(EvalError::new(format!(
            "list effect handler expected a standard effect result list, got {handled:?}"
        )));
    };
    Ok(results)
}

fn flat_map_list_effect_results(
    results: List,
    continuation: Value,
    local_env: Arc<[Value]>,
) -> List {
    deferred_list("list effect seq", move || {
        let Some((head, tail)) = pop_list_front(&results)? else {
            return Ok(List::empty());
        };
        let continuation = eval_value(&continuation)?;
        let next = apply_value(continuation.clone(), head, &local_env)?;
        Ok(List::concat(
            lazy_run_list_effect(next, local_env.clone()),
            flat_map_list_effect_results(tail, continuation, local_env.clone()),
        ))
    })
}

fn cut_list_effect_results(operation: Value, local_env: Arc<[Value]>) -> List {
    deferred_list("list effect cut", move || {
        let results = lazy_run_list_effect(operation.clone(), local_env.clone());
        let Some((head, _)) = pop_list_front(&results)? else {
            return Ok(List::empty());
        };
        Ok(List::from_values(vec![head]))
    })
}

fn fix_list_effect_results(operation: Value, handle: LazyValue, local_env: Arc<[Value]>) -> List {
    deferred_list("list effect fix", move || {
        let results = lazy_run_list_effect(operation.clone(), local_env.clone());
        let Some((head, tail)) = pop_list_front(&results)? else {
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
    thunk: impl Fn() -> Result<List, EvalError> + Send + Sync + 'static,
) -> List {
    List::from_thunk(LazyValue::deferred(label, move || {
        thunk().map(Value::List).map_err(|err| err.to_string())
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
