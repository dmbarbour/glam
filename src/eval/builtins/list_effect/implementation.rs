use super::super::super::*;
use crate::core::ListEffectComputation;

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

pub(super) fn deferred_list(
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
