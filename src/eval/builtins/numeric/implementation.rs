use super::super::super::*;

pub(super) fn eval_numeric_builtin(
    name: &str,
    left: &Value,
    right: &Value,
    op: impl Fn(&Number, &Number) -> Number,
) -> Result<Value, EvalError> {
    let left = eval_number(left, name)?;
    let right = eval_number(right, name)?;
    Ok(Value::Number(op(&left, &right)))
}

pub(super) fn eval_numeric_divide_builtin(left: &Value, right: &Value) -> Result<Value, EvalError> {
    let left = eval_number(left, "divide")?;
    let right = eval_number(right, "divide")?;
    let Some(result) = left.checked_div(&right) else {
        return Err(EvalError::new("divide builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}

pub(super) fn eval_floor_builtin(value: &Value) -> Result<Value, EvalError> {
    Ok(Value::Number(eval_number(value, "floor")?.floor()))
}

pub(super) fn eval_numeric_mod_builtin(left: &Value, right: &Value) -> Result<Value, EvalError> {
    let left = eval_number(left, "mod")?;
    let right = eval_number(right, "mod")?;
    let Some(result) = left.checked_mod(&right) else {
        return Err(EvalError::new("mod builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}
