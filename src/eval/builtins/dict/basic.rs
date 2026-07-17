use super::super::super::*;
use super::merge::{merge_dicts, update_dict_path};

pub(super) fn eval_singleton_builtin(
    context: &EvalContext,
    key: &Value,
    value: &Value,
) -> Result<Value, EvalError> {
    let key = eval_value(context, key)?;
    let key = value_to_key(context, &key)?;
    if matches!(value, Value::Dict(dict) if dict.is_empty()) {
        return Ok(Value::Dict(crate::core::Dict::new_sync()));
    }

    Ok(Value::Dict(
        crate::core::Dict::new_sync().insert(key, value.clone()),
    ))
}

pub(in crate::eval::builtins) fn eval_dict_union_builtin(
    context: &EvalContext,
    left: &Value,
    right: &Value,
) -> Result<Value, EvalError> {
    let left = force_value_shell(context, left)?;
    let right = force_value_shell(context, right)?;
    let Value::Dict(left_dict) = left else {
        return Err(EvalError::new(
            "dictionary union requires dictionary values",
        ));
    };
    let Value::Dict(right_dict) = right else {
        return Err(EvalError::new(
            "dictionary union requires dictionary values",
        ));
    };

    Ok(Value::Dict(merge_dicts(&left_dict, &right_dict)))
}

pub(super) fn eval_dict_update_builtin(
    context: &EvalContext,
    path: &Value,
    new_value: &Value,
    dict: &Value,
) -> Result<Value, EvalError> {
    let path = eval_key_path_list(context, path)?;
    if path.is_empty() {
        return Err(EvalError::new(
            "dict update builtin requires a non-empty path",
        ));
    }
    let dict = force_value_shell(context, dict)?;
    let Value::Dict(dict) = dict else {
        return Err(EvalError::new("dict update builtin requires a dictionary"));
    };
    Ok(Value::Dict(update_dict_path(
        &dict,
        &path,
        new_value.clone(),
    )))
}
