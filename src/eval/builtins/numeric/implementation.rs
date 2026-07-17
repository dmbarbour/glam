use super::super::super::*;

pub(super) fn eval_numeric_builtin(
    context: &EvalContext,
    name: &str,
    left: &Value,
    right: &Value,
    op: impl Fn(&Number, &Number) -> Number,
) -> Result<Value, EvalError> {
    let left = eval_number(context, left, name)?;
    let right = eval_number(context, right, name)?;
    Ok(Value::Number(op(&left, &right)))
}

pub(super) fn eval_numeric_divide_builtin(
    context: &EvalContext,
    left: &Value,
    right: &Value,
) -> Result<Value, EvalError> {
    let left = eval_number(context, left, "divide")?;
    let right = eval_number(context, right, "divide")?;
    let Some(result) = left.checked_div(&right) else {
        return Err(EvalError::new("divide builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}

pub(super) fn eval_floor_builtin(context: &EvalContext, value: &Value) -> Result<Value, EvalError> {
    Ok(Value::Number(eval_number(context, value, "floor")?.floor()))
}

pub(super) fn eval_numeric_mod_builtin(
    context: &EvalContext,
    left: &Value,
    right: &Value,
) -> Result<Value, EvalError> {
    let left = eval_number(context, left, "mod")?;
    let right = eval_number(context, right, "mod")?;
    let Some(result) = left.checked_mod(&right) else {
        return Err(EvalError::new("mod builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}
