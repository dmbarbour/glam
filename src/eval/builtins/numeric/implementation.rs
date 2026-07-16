use super::super::super::*;

pub(super) fn eval_numeric_builtin(
    name: &str,
    left: &Value,
    right: &Value,
    local_env: &[Value],
    op: impl Fn(&Number, &Number) -> Number,
) -> Result<Value, EvalError> {
    let left = eval_number(left, local_env, name)?;
    let right = eval_number(right, local_env, name)?;
    Ok(Value::Number(op(&left, &right)))
}

pub(super) fn eval_numeric_divide_builtin(
    left: &Value,
    right: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let left = eval_number(left, local_env, "divide")?;
    let right = eval_number(right, local_env, "divide")?;
    let Some(result) = left.checked_div(&right) else {
        return Err(EvalError::new("divide builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}

pub(super) fn eval_floor_builtin(value: &Value, local_env: &[Value]) -> Result<Value, EvalError> {
    Ok(Value::Number(
        eval_number(value, local_env, "floor")?.floor(),
    ))
}

pub(super) fn eval_numeric_mod_builtin(
    left: &Value,
    right: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let left = eval_number(left, local_env, "mod")?;
    let right = eval_number(right, local_env, "mod")?;
    let Some(result) = left.checked_mod(&right) else {
        return Err(EvalError::new("mod builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}
