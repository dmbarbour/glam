//! Compiler-private assertion gates shared with equivalent annotations.

use super::super::*;

pub(super) fn apply(
    context: &EvaluatorStepContext<'_>,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    let [diagnostic_context, value, target] = super::exact(arguments, "assert_unit")?;
    assert_unit_in(context, Some(&diagnostic_context), &value, &target)
}

pub(in crate::eval) fn assert_unit_in(
    context: &EvaluatorStepContext<'_>,
    diagnostic_context: Option<&Value>,
    value: &Value,
    target: &Value,
) -> Result<Value, EvaluationHalt> {
    let value = eval_value_in(context, value)?;
    if value == context.context().values().unit() {
        return Ok(target.clone());
    }

    let received = value.diagnostic_kind_name();
    let message = match diagnostic_context {
        Some(diagnostic_context) => {
            let diagnostic_context = eval_value_in(context, diagnostic_context)?;
            let Value::Binary(diagnostic_context) = diagnostic_context else {
                return Err(EvaluationHalt::new(
                    "unit assertion diagnostic context must be text",
                ));
            };
            format!(
                "{}: unit expected, received {received}",
                String::from_utf8_lossy(&diagnostic_context)
            )
        }
        None => format!("unit expected, received {received}"),
    };
    Err(EvaluationHalt::new(message))
}
