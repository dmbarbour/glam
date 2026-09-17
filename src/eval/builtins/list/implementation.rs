use super::super::super::*;

pub(super) fn eval_text_lines_builtin(
    context: &EvaluatorStepContext<'_>,
    value: &Value,
) -> Result<Value, EvaluationHalt> {
    let bytes = match eval_value_in(context, value)? {
        Value::Binary(bytes) => bytes,
        Value::List(list) => Bytes::from(list_to_binary_bytes_in(
            context,
            &list,
            "text lines builtin",
        )?),
        _ => {
            return Err(EvaluationHalt::new(
                "text lines builtin requires a binary-compatible list or binary value",
            ));
        }
    };
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(Value::Binary(bytes.slice(start..index)));
            start = index + 1;
        }
    }
    lines.push(Value::Binary(bytes.slice(start..bytes.len())));
    Ok(Value::List(List::from_values(lines)))
}
