use super::super::super::*;

pub(super) fn eval_fixpoint_builtin(function: &Value) -> Result<Value, EvalError> {
    let function = eval_value(function)?;
    if !matches!(function, Value::Function(_) | Value::Net(_)) {
        return Err(EvalError::new("fixpoint builtin requires a function value"));
    }

    let handle = LazyValue::pending("fixpoint");
    let marker = Value::Lazy(handle.clone());
    let value = apply_value(function, marker.clone())?;
    handle
        .set(value.clone())
        .map_err(|_| EvalError::new("fixpoint builtin initialized twice"))?;
    Ok(value)
}
