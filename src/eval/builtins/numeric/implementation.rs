use super::super::super::*;
use crate::core::EvaluatedValue;

pub(super) fn eval_numeric_builtin(
    context: &EvaluatorStepContext<'_>,
    name: &str,
    left: &Value,
    right: &Value,
    op: impl Fn(&Number, &Number) -> Number,
) -> Result<Value, EvaluationHalt> {
    let left = number_from_evaluated(
        EvaluatedValue::try_from(eval_value_in(context, left)?)
            .expect("numeric operand demand must reach WHNF"),
        name,
    )?;
    let right = number_from_evaluated(
        EvaluatedValue::try_from(eval_value_in(context, right)?)
            .expect("numeric operand demand must reach WHNF"),
        name,
    )?;
    Ok(Value::Number(op(&left, &right)))
}

pub(super) fn eval_numeric_divide_builtin(
    context: &EvaluatorStepContext<'_>,
    left: &Value,
    right: &Value,
) -> Result<Value, EvaluationHalt> {
    let left = number_from_evaluated(
        EvaluatedValue::try_from(eval_value_in(context, left)?)
            .expect("divide operand demand must reach WHNF"),
        "divide",
    )?;
    let right = number_from_evaluated(
        EvaluatedValue::try_from(eval_value_in(context, right)?)
            .expect("divide operand demand must reach WHNF"),
        "divide",
    )?;
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
        number_from_evaluated(
            EvaluatedValue::try_from(eval_value_in(context, value)?)
                .expect("floor operand demand must reach WHNF"),
            "floor",
        )?
        .floor(),
    ))
}

pub(super) fn eval_numeric_mod_builtin(
    context: &EvaluatorStepContext<'_>,
    left: &Value,
    right: &Value,
) -> Result<Value, EvaluationHalt> {
    let left = number_from_evaluated(
        EvaluatedValue::try_from(eval_value_in(context, left)?)
            .expect("mod operand demand must reach WHNF"),
        "mod",
    )?;
    let right = number_from_evaluated(
        EvaluatedValue::try_from(eval_value_in(context, right)?)
            .expect("mod operand demand must reach WHNF"),
        "mod",
    )?;
    let Some(result) = left.checked_mod(&right) else {
        return Err(EvaluationHalt::new("mod builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}
