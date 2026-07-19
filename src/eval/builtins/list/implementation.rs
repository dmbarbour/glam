use super::super::super::*;

pub(in crate::eval::builtins) fn list_like_value(
    context: &EvalContext,
    value: Value,
    name: &str,
) -> Result<List, EvalError> {
    match force_value_shell(context, &value)? {
        Value::Binary(bytes) => Ok(List::from_bytes(bytes)),
        Value::List(list) => Ok(list),
        other => Err(EvalError::new(format!(
            "{name} builtin requires tuple payloads to be lists or binaries, got {other:?}"
        ))),
    }
}

pub(super) fn eval_slice_builtin(
    context: &EvalContext,
    start: &Value,
    end: &Value,
    value: &Value,
) -> Result<Value, EvalError> {
    let start = eval_index_number(context, start, "slice")?;
    let end = eval_index_number(context, end, "slice")?;
    if start > end {
        return Err(EvalError::new(
            "slice builtin requires start to be less than or equal to end",
        ));
    }

    match force_value_shell(context, value)? {
        Value::Binary(bytes) => {
            if end > bytes.len() {
                return Err(EvalError::new("slice builtin end is out of bounds"));
            }
            Ok(Value::Binary(bytes.slice(start..end)))
        }
        Value::List(list) => {
            let Some(slice) =
                list.try_slice(start, end, &mut |thunk| force_list_thunk(context, thunk))?
            else {
                return Err(EvalError::new("slice builtin end is out of bounds"));
            };
            Ok(Value::List(slice))
        }
        _ => Err(EvalError::new(
            "slice builtin requires a list or binary value",
        )),
    }
}

pub(super) fn eval_map_builtin(
    context: &EvalContext,
    function: &Value,
    value: &Value,
) -> Result<Value, EvalError> {
    let function = force_value_shell(context, function)?;
    let mapped = match force_value_shell(context, value)? {
        Value::Binary(bytes) => bytes
            .iter()
            .map(|byte| {
                apply_value(
                    context,
                    function.clone(),
                    Value::Number(Number::from_u8(*byte)),
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
        Value::List(list) => list_to_value_items(context, &list)?
            .into_iter()
            .map(|item| apply_value(context, function.clone(), item))
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(EvalError::new(
                "map builtin requires a list or binary value",
            ));
        }
    };

    Ok(Value::List(List::from_values(mapped)))
}

pub(super) fn eval_list_len_builtin(
    context: &EvalContext,
    value: &Value,
) -> Result<Value, EvalError> {
    match force_value_shell(context, value)? {
        Value::Binary(bytes) => Ok(Value::Number(Number::from_usize(bytes.len()))),
        Value::List(list) => Ok(Value::Number(Number::from_usize(
            list.try_len(&mut |thunk| force_list_thunk(context, thunk))?,
        ))),
        _ => Err(EvalError::new(
            "list len builtin requires a list or binary value",
        )),
    }
}

pub(super) fn eval_list_split_builtin(
    context: &EvalContext,
    index: &Value,
    value: &Value,
) -> Result<Value, EvalError> {
    let index = eval_index_number(context, index, "split")?;
    match force_value_shell(context, value)? {
        Value::Binary(bytes) => {
            if index > bytes.len() {
                return Err(EvalError::new("split builtin index is out of bounds"));
            }
            Ok(split_result_value(
                Value::Binary(bytes.slice(0..index)),
                Value::Binary(bytes.slice(index..bytes.len())),
            ))
        }
        Value::List(list) => {
            let Some((left, right)) =
                list.try_split_at(index, &mut |thunk| force_list_thunk(context, thunk))?
            else {
                return Err(EvalError::new("split builtin index is out of bounds"));
            };
            Ok(split_result_value(Value::List(left), Value::List(right)))
        }
        _ => Err(EvalError::new(
            "split builtin requires a list or binary value",
        )),
    }
}

pub(super) fn eval_list_split_end_builtin(
    context: &EvalContext,
    count: &Value,
    value: &Value,
) -> Result<Value, EvalError> {
    let count = eval_index_number(context, count, "split_end")?;
    match force_value_shell(context, value)? {
        Value::Binary(bytes) => {
            if count > bytes.len() {
                return Err(EvalError::new("split_end builtin count is out of bounds"));
            }
            let index = bytes.len() - count;
            Ok(split_result_value(
                Value::Binary(bytes.slice(0..index)),
                Value::Binary(bytes.slice(index..bytes.len())),
            ))
        }
        Value::List(list) => {
            let Some((left, right)) =
                list.try_split_from_end(count, &mut |thunk| force_list_thunk(context, thunk))?
            else {
                return Err(EvalError::new("split_end builtin count is out of bounds"));
            };
            Ok(split_result_value(Value::List(left), Value::List(right)))
        }
        _ => Err(EvalError::new(
            "split_end builtin requires a list or binary value",
        )),
    }
}

pub(super) fn eval_list_head_builtin(
    context: &EvalContext,
    value: &Value,
) -> Result<Value, EvalError> {
    match force_value_shell(context, value)? {
        Value::Binary(bytes) => bytes
            .first()
            .map(|byte| Value::Number(Number::from_u8(*byte)))
            .ok_or_else(|| EvalError::new("list head builtin requires a non-empty list or binary")),
        Value::List(list) => pop_list_front(context, &list)?
            .map(|(head, _)| head)
            .ok_or_else(|| EvalError::new("list head builtin requires a non-empty list or binary")),
        _ => Err(EvalError::new(
            "list head builtin requires a list or binary value",
        )),
    }
}

pub(super) fn eval_list_tail_builtin(
    context: &EvalContext,
    value: &Value,
) -> Result<Value, EvalError> {
    match force_value_shell(context, value)? {
        Value::Binary(bytes) => {
            if bytes.is_empty() {
                Err(EvalError::new(
                    "list tail builtin requires a non-empty list or binary",
                ))
            } else {
                Ok(Value::Binary(bytes.slice(1..bytes.len())))
            }
        }
        Value::List(list) => {
            let Some((_, tail)) = pop_list_front(context, &list)? else {
                return Err(EvalError::new(
                    "list tail builtin requires a non-empty list or binary",
                ));
            };
            Ok(Value::List(tail))
        }
        _ => Err(EvalError::new(
            "list tail builtin requires a list or binary value",
        )),
    }
}

pub(super) fn eval_text_lines_builtin(
    context: &EvalContext,
    value: &Value,
) -> Result<Value, EvalError> {
    let bytes = match force_value_shell(context, value)? {
        Value::Binary(bytes) => bytes,
        Value::List(list) => Bytes::from(
            list_to_binary_bytes(context, &list)
                .map_err(|error| EvalError::new(format!("text lines builtin {error}")))?,
        ),
        _ => {
            return Err(EvalError::new(
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
