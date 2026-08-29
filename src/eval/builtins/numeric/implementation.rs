use super::super::super::*;

pub(super) fn eval_numeric_builtin(
    context: &EvaluatorStepContext<'_>,
    name: &str,
    left: &Value,
    right: &Value,
    op: impl Fn(&Number, &Number) -> Number,
) -> Result<Value, EvaluationHalt> {
    let left = eval_number_in(context, left, name)?;
    let right = eval_number_in(context, right, name)?;
    Ok(Value::Number(op(&left, &right)))
}

pub(super) fn eval_numeric_divide_builtin(
    context: &EvaluatorStepContext<'_>,
    left: &Value,
    right: &Value,
) -> Result<Value, EvaluationHalt> {
    let left = eval_number_in(context, left, "divide")?;
    let right = eval_number_in(context, right, "divide")?;
    let Some(result) = left.checked_div(&right) else {
        return Err(EvaluationHalt::new("divide builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}

pub(super) fn eval_floor_builtin(
    context: &EvaluatorStepContext<'_>,
    value: &Value,
) -> Result<Value, EvaluationHalt> {
    Ok(Value::Number(
        eval_number_in(context, value, "floor")?.floor(),
    ))
}

pub(super) fn eval_numeric_mod_builtin(
    context: &EvaluatorStepContext<'_>,
    left: &Value,
    right: &Value,
) -> Result<Value, EvaluationHalt> {
    let left = eval_number_in(context, left, "mod")?;
    let right = eval_number_in(context, right, "mod")?;
    let Some(result) = left.checked_mod(&right) else {
        return Err(EvaluationHalt::new("mod builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}
