use super::super::super::*;
use super::merge::{merge_dicts, update_dict_path};

pub(super) fn eval_singleton_builtin(
    context: &EvaluatorStepContext<'_>,
    key: &Value,
    value: &Value,
) -> Result<Value, EvaluationHalt> {
    let key = eval_value_in(context, key)?;
    let key = value_to_key_in(context, &key)?;
    if matches!(value, Value::Dict(dict) if dict.is_empty()) {
        return Ok(Value::Dict(crate::core::Dict::new_sync()));
    }

    Ok(Value::Dict(
        crate::core::Dict::new_sync().insert(key, value.clone()),
    ))
}

pub(in crate::eval::builtins) fn eval_dict_union_builtin_in(
    context: &EvaluatorStepContext<'_>,
    left: &Value,
    right: &Value,
) -> Result<Value, EvaluationHalt> {
    let left = eval_value_in(context, left)?;
    let right = eval_value_in(context, right)?;
    let Value::Dict(left_dict) = left else {
        return Err(EvaluationHalt::new(
            "dictionary union requires dictionary values",
        ));
    };
    let Value::Dict(right_dict) = right else {
        return Err(EvaluationHalt::new(
            "dictionary union requires dictionary values",
        ));
    };

    Ok(Value::Dict(merge_dicts(
        context.context().values(),
        &left_dict,
        &right_dict,
    )))
}

pub(super) fn eval_dict_update_builtin(
    context: &EvaluatorStepContext<'_>,
    path: &Value,
    new_value: &Value,
    dict: &Value,
) -> Result<Value, EvaluationHalt> {
    let path = eval_key_path_list_in(context, path)?;
    if path.is_empty() {
        return Err(EvaluationHalt::new(
            "dict update builtin requires a non-empty path",
        ));
    }
    let dict = eval_value_in(context, dict)?;
    let Value::Dict(dict) = dict else {
        return Err(EvaluationHalt::new(
            "dict update builtin requires a dictionary",
        ));
    };
    Ok(Value::Dict(update_dict_path(
        context.context().values(),
        &dict,
        &path,
        new_value.clone(),
    )))
}
