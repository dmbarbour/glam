use super::super::super::*;
use crate::core::ListEffectComputation;

pub(super) fn eval_list_effect_builtin(
    context: &EvaluatorStepContext<'_>,
    effect: &Value,
) -> Result<Value, EvaluationHalt> {
    Ok(Value::List(lazy_run_list_effect(context, effect.clone())))
}

pub(super) fn eval_list_effect_seq_builtin(
    context: &EvaluatorStepContext<'_>,
    operation: &Value,
    continuation: &Value,
) -> Result<Value, EvaluationHalt> {
    Ok(Value::List(flat_map_list_effect_results(
        context,
        lazy_run_list_effect(context, operation.clone()),
        continuation.clone(),
    )))
}

pub(super) fn eval_list_effect_alt_builtin(
    context: &EvaluatorStepContext<'_>,
    left: &Value,
    right: &Value,
) -> Result<Value, EvaluationHalt> {
    Ok(Value::List(List::concat(
        lazy_run_list_effect(context, left.clone()),
        lazy_run_list_effect(context, right.clone()),
    )))
}

pub(super) fn eval_list_effect_cut_builtin(
    context: &EvaluatorStepContext<'_>,
    operation: &Value,
) -> Result<Value, EvaluationHalt> {
    Ok(Value::List(cut_list_effect_results(
        context,
        operation.clone(),
    )))
}

pub(super) fn eval_list_effect_fix_builtin(
    context: &EvaluatorStepContext<'_>,
    function: &Value,
) -> Result<Value, EvaluationHalt> {
    let function = eval_value_in(context, function)?;
    let handle = context.construct_promise("list effect fixpoint");
    let marker = Value::Promised(handle.clone());
    let operation = apply_value_in(context, function, marker.clone())?;
    Ok(Value::List(fix_list_effect_results(
        context, operation, handle,
    )))
}

fn lazy_run_list_effect(context: &EvaluatorStepContext<'_>, effect: Value) -> List {
    deferred_list(
        context,
        "list effect",
        ListEffectComputation::Run { effect },
    )
}

fn flat_map_list_effect_results(
    context: &EvaluatorStepContext<'_>,
    results: List,
    continuation: Value,
) -> List {
    deferred_list(
        context,
        "list effect seq",
        ListEffectComputation::Sequence {
            results,
            continuation,
        },
    )
}

fn cut_list_effect_results(context: &EvaluatorStepContext<'_>, operation: Value) -> List {
    deferred_list(
        context,
        "list effect cut",
        ListEffectComputation::Cut { operation },
    )
}

fn fix_list_effect_results(
    context: &EvaluatorStepContext<'_>,
    operation: Value,
    handle: PromisedValue,
) -> List {
    deferred_list(
        context,
        "list effect fix",
        ListEffectComputation::Fix {
            operation,
            handle: Value::Promised(handle),
        },
    )
}

fn deferred_list(
    context: &EvaluatorStepContext<'_>,
    label: &'static str,
    computation: ListEffectComputation,
) -> List {
    List::from_thunk(
        context
            .construct_lazy(|access| {
                LazyValue::list_effect_computation_in(access, label, computation)
            })
            .into(),
    )
}
