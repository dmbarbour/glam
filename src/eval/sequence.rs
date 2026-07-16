use super::*;

pub(super) fn eval_key_path_list(
    value: &Value,
    local_env: &[Value],
) -> Result<Vec<Key>, EvalError> {
    let value = eval_value(value)?;
    let Value::List(list) = value else {
        return Err(EvalError::new(
            "path list expression must evaluate to a list value",
        ));
    };

    let items = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |bytes| {
            items
                .borrow_mut()
                .extend(bytes.iter().map(|byte| Key::Number(Number::from_u8(*byte))));
            Ok::<_, EvalError>(())
        },
        &mut |values| {
            for value in values.iter() {
                let value = eval_value(value)?;
                items.borrow_mut().push(value_to_key(&value, local_env)?);
            }
            Ok(())
        },
        &mut force_list_thunk,
    )?;
    Ok(items.into_inner())
}

pub(super) fn list_to_key_items(list: &List, local_env: &[Value]) -> Result<Arc<[Key]>, EvalError> {
    let items = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |bytes| {
            items
                .borrow_mut()
                .extend(bytes.iter().map(|byte| Key::Number(Number::from_u8(*byte))));
            Ok::<_, EvalError>(())
        },
        &mut |values| {
            for value in values.iter() {
                let value = eval_value(value)?;
                items.borrow_mut().push(value_to_key(&value, local_env)?);
            }
            Ok(())
        },
        &mut force_list_thunk,
    )?;
    Ok(Arc::from(items.into_inner()))
}

pub(super) fn list_to_value_items(list: &List) -> Result<Vec<Value>, EvalError> {
    let items = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |bytes| {
            items.borrow_mut().extend(
                bytes
                    .iter()
                    .map(|byte| Value::Number(Number::from_u8(*byte))),
            );
            Ok::<_, EvalError>(())
        },
        &mut |values| {
            items.borrow_mut().extend(values.iter().cloned());
            Ok(())
        },
        &mut force_list_thunk,
    )?;
    Ok(items.into_inner())
}

pub(super) fn list_to_binary_bytes(list: &List) -> Result<Vec<u8>, String> {
    let bytes = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |segment| {
            bytes.borrow_mut().extend_from_slice(segment);
            Ok::<_, String>(())
        },
        &mut |values| {
            for value in values.iter() {
                match force_value_shell(value).map_err(|err| err.to_string())? {
                    Value::Number(number) => {
                        let byte = number.to_u8_if_integer().ok_or_else(|| {
                            format!("`binary` annotation cannot encode number `{number}` as a byte")
                        })?;
                        bytes.borrow_mut().push(byte);
                    }
                    Value::Binary(segment) => bytes.borrow_mut().extend_from_slice(&segment),
                    Value::List(list) => {
                        bytes
                            .borrow_mut()
                            .extend(list_to_binary_bytes(&list)?);
                    }
                    other => {
                        return Err(format!(
                            "`binary` annotation requires list items to be bytes or binaries, got {other:?}"
                        ));
                    }
                }
            }
            Ok(())
        },
        &mut |thunk| force_list_thunk(thunk).map_err(|err| err.to_string()),
    )?;
    Ok(bytes.into_inner())
}

pub fn list_output_bytes(list: &List) -> Result<Vec<u8>, String> {
    let bytes = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |segment| {
            bytes.borrow_mut().extend_from_slice(segment);
            Ok::<_, String>(())
        },
        &mut |segment| {
            for item in segment.iter() {
                let item = force_value_shell(item).map_err(|err| err.to_string())?;
                let Value::Number(number) = item else {
                    return Err("must contain only integers and binary segments".to_owned());
                };

                let byte = number.to_u8_if_integer().ok_or_else(|| {
                    format!("contains number `{number}` that is not an in-range byte integer")
                })?;
                bytes.borrow_mut().push(byte);
            }
            Ok(())
        },
        &mut |thunk| force_list_thunk(thunk).map_err(|err| err.to_string()),
    )?;
    Ok(bytes.into_inner())
}

pub(crate) fn list_output_bytes_range(
    list: &List,
    range: std::ops::Range<usize>,
) -> Result<Option<Vec<u8>>, String> {
    let Some(slice) = list
        .try_slice(range.start, range.end, &mut force_list_thunk)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    list_output_bytes(&slice).map(Some)
}

pub(super) fn append_values(left: Value, right: Value) -> Result<Value, EvalError> {
    let left = append_sequence(left)?;
    let right = append_sequence(right)?;
    Ok(Value::List(List::concat(left, right)))
}

pub(super) fn append_sequence(value: Value) -> Result<List, EvalError> {
    match value {
        Value::Binary(bytes) => Ok(List::from_bytes(bytes)),
        Value::List(list) => Ok(list),
        Value::Lazy(thunk) => Ok(List::from_thunk(thunk)),
        _ => Err(EvalError::new(
            "append requires list or binary values on both sides",
        )),
    }
}

pub(super) fn list_literal_segment(value: Value) -> List {
    match value {
        Value::Binary(bytes) => List::from_bytes(bytes),
        Value::List(list) => list,
        other => Value::singleton_list(other),
    }
}
