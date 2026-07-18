use super::super::super::*;
use crate::core::FixpointComputation;

pub(super) fn eval_fixpoint_builtin(
    context: &EvalContext,
    function: &Value,
) -> Result<Value, EvalError> {
    let function = eval_value(context, function)?;
    if !matches!(function, Value::Function(_) | Value::Net(_)) {
        return Err(EvalError::new("fixpoint builtin requires a function value"));
    }

    LazyValue::computed_fixpoint("fixpoint", FixpointComputation::Function(function))
        .map(Value::Lazy)
        .map_err(|error| EvalError::new(error.as_ref()))
}
