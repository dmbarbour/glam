use super::super::super::*;

pub(super) fn eval_fixpoint_builtin(
    context: &EvalContext,
    function: &Value,
) -> Result<Value, EvalError> {
    let function = eval_value(context, function)?;
    if !matches!(function, Value::Function(_) | Value::Net(_)) {
        return Err(EvalError::new("fixpoint builtin requires a function value"));
    }

    let handle = LazyValue::promised("fixpoint");
    let marker = Value::Lazy(handle.clone());
    let value = apply_value(context, function, marker.clone())?;
    handle
        .set(value.clone())
        .map_err(|_| EvalError::new("fixpoint builtin initialized twice"))?;
    Ok(value)
}
