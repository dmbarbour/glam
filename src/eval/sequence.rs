use super::*;

pub(crate) fn eval_key_path_list(
    context: &EvalContext,
    value: &Value,
) -> Result<Vec<Key>, EvaluationHalt> {
    let value = eval_value(context, value)?;
    let Value::List(list) = value else {
        return Err(EvaluationHalt::new(
            "path-list operand must evaluate to a list value",
        ));
    };

    let items = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |bytes| {
            items
                .borrow_mut()
                .extend(bytes.iter().map(|byte| Key::Number(Number::from_u8(*byte))));
            Ok::<_, EvaluationHalt>(())
        },
        &mut |values| {
            for value in values.iter() {
                let value = eval_value(context, value)?;
                items.borrow_mut().push(value_to_key(context, &value)?);
            }
            Ok(())
        },
        &mut |thunk| force_list_thunk(context, thunk),
    )?;
    Ok(items.into_inner())
}

pub(super) fn list_to_key_items(
    context: &EvalContext,
    list: &List,
) -> Result<Arc<[Key]>, EvaluationHalt> {
    let items = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |bytes| {
            items
                .borrow_mut()
                .extend(bytes.iter().map(|byte| Key::Number(Number::from_u8(*byte))));
            Ok::<_, EvaluationHalt>(())
        },
        &mut |values| {
            for value in values.iter() {
                let value = eval_value(context, value)?;
                items.borrow_mut().push(value_to_key(context, &value)?);
            }
            Ok(())
        },
        &mut |thunk| force_list_thunk(context, thunk),
    )?;
    Ok(Arc::from(items.into_inner()))
}

pub(crate) fn list_to_value_items(
    context: &EvalContext,
    list: &List,
) -> Result<Vec<Value>, EvaluationHalt> {
    let items = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |bytes| {
            items.borrow_mut().extend(
                bytes
                    .iter()
                    .map(|byte| Value::Number(Number::from_u8(*byte))),
            );
            Ok::<_, EvaluationHalt>(())
        },
        &mut |values| {
            items.borrow_mut().extend(values.iter().cloned());
            Ok(())
        },
        &mut |thunk| force_list_thunk(context, thunk),
    )?;
    Ok(items.into_inner())
}

pub(super) fn list_to_binary_bytes(
    context: &EvalContext,
    list: &List,
    subject: &str,
) -> Result<Vec<u8>, EvaluationHalt> {
    let bytes = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |segment| {
            bytes.borrow_mut().extend_from_slice(segment);
            Ok::<_, EvaluationHalt>(())
        },
        &mut |values| {
            for value in values.iter() {
                match eval_value(context, value).map_err(|error| {
                    error.with_context(evaluation_context_frame("binary_extraction"))
                })? {
                    Value::Number(number) => {
                        let byte = number.to_u8_if_integer().ok_or_else(|| {
                            EvaluationHalt::new(format!(
                                "{subject} cannot encode number `{number}` as a byte"
                            ))
                        })?;
                        bytes.borrow_mut().push(byte);
                    }
                    other => {
                        return Err(EvaluationHalt::new(format!(
                            "{subject} requires list items to be byte integers, got {other:?}"
                        )));
                    }
                }
            }
            Ok(())
        },
        &mut |thunk| {
            force_list_thunk(context, thunk)
                .map_err(|error| error.with_context(evaluation_context_frame("binary_extraction")))
        },
    )?;
    Ok(bytes.into_inner())
}

#[cfg(test)]
pub(crate) fn list_output_bytes(
    context: &EvalContext,
    list: &List,
) -> Result<Vec<u8>, EvaluationHalt> {
    list_output_bytes_for(context, list, "`value`")
}

pub(crate) fn list_output_bytes_for(
    context: &EvalContext,
    list: &List,
    subject: &str,
) -> Result<Vec<u8>, EvaluationHalt> {
    list_to_binary_bytes(context, list, subject)
}

pub(crate) fn list_output_bytes_range(
    context: &EvalContext,
    list: &List,
    range: std::ops::Range<usize>,
    subject: &str,
) -> Result<Option<Vec<u8>>, EvaluationHalt> {
    let Some(slice) = list.try_slice(range.start, range.end, &mut |thunk| {
        force_list_thunk(context, thunk)
            .map_err(|error| error.with_context(evaluation_context_frame("binary_extraction")))
    })?
    else {
        return Ok(None);
    };
    list_output_bytes_for(context, &slice, subject).map(Some)
}

pub(super) fn append_values(left: Value, right: Value) -> Result<Value, EvaluationHalt> {
    let left = append_sequence(left)?;
    let right = append_sequence(right)?;
    Ok(Value::List(List::concat(left, right)))
}

pub(super) fn append_sequence(value: Value) -> Result<List, EvaluationHalt> {
    match value {
        Value::Binary(bytes) => Ok(List::from_bytes(bytes)),
        Value::List(list) => Ok(list),
        Value::Lazy(thunk) => Ok(List::from_thunk(thunk.into())),
        Value::Promised(promise) => Ok(List::from_thunk(promise.into())),
        _ => Err(EvaluationHalt::new(
            "append requires list or binary values on both sides",
        )),
    }
}
