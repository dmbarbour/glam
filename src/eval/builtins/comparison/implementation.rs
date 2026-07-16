use std::cmp::Ordering;

use super::super::super::*;
use super::super::list::{list_like_value, tuple_payload};

pub(super) fn eval_compare_ordering_builtin(
    name: &str,
    left: &Value,
    right: &Value,
    local_env: &[Value],
    predicate: impl FnOnce(Ordering) -> bool,
) -> Result<Value, EvalError> {
    let ordering = compare_ordered_values(left, right, local_env, name)?;
    Ok(condition_effect_value(predicate(ordering)))
}

pub(super) fn eval_compare_equality_builtin(
    name: &str,
    left: &Value,
    right: &Value,
    local_env: &[Value],
    predicate: impl FnOnce(bool) -> bool,
) -> Result<Value, EvalError> {
    let equal = equal_values(left, right, local_env, name)?;
    Ok(condition_effect_value(predicate(equal)))
}

fn condition_effect_value(success: bool) -> Value {
    if success {
        effect_call_expr_value("r", vec![builtin_unit_value()])
    } else {
        effect_call_expr_value("fail", Vec::new())
    }
}

fn effect_call_expr_value(name: &str, arguments: Vec<Value>) -> Value {
    effect_value(Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectCall,
        arguments: Arc::from([
            Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(name))),
            Value::List(List::from_values(arguments)),
        ]),
    }))
}

fn builtin_unit_value() -> Value {
    Value::Atom(crate::core::Atom::from_key(&Key::abstract_global_path([
        "builtin", "unit",
    ])))
}

fn compare_ordered_values(
    left: &Value,
    right: &Value,
    local_env: &[Value],
    name: &str,
) -> Result<Ordering, EvalError> {
    let left = force_value_shell(left)?;
    let right = force_value_shell(right)?;
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => Ok(left.cmp(&right)),
        (Value::Binary(left), Value::Binary(right)) => Ok(left.cmp(&right)),
        (Value::Binary(left), Value::List(right)) => {
            compare_lists_ordered(List::from_bytes(left), right, local_env, name)
        }
        (Value::List(left), Value::Binary(right)) => {
            compare_lists_ordered(left, List::from_bytes(right), local_env, name)
        }
        (Value::List(left), Value::List(right)) => {
            compare_lists_ordered(left, right, local_env, name)
        }
        (Value::Dict(left), Value::Dict(right)) => {
            let Some(left) = tuple_payload(&left) else {
                return Err(EvalError::new(format!(
                    "{name} builtin can only order dictionaries tagged as `tuple`"
                )));
            };
            let Some(right) = tuple_payload(&right) else {
                return Err(EvalError::new(format!(
                    "{name} builtin can only order dictionaries tagged as `tuple`"
                )));
            };
            let left = list_like_value(left, name)?;
            let right = list_like_value(right, name)?;
            compare_lists_ordered(left, right, local_env, name)
        }
        (Value::Builtin(_), _)
        | (_, Value::Builtin(_))
        | (Value::PartialBuiltin(_), _)
        | (_, Value::PartialBuiltin(_))
        | (Value::Function(_), _)
        | (_, Value::Function(_))
        | (Value::Net(_), _)
        | (_, Value::Net(_)) => Err(EvalError::new(format!(
            "{name} builtin cannot compare function values"
        ))),
        (left, right) => Err(EvalError::new(format!(
            "{name} builtin cannot order values {left:?} and {right:?}"
        ))),
    }
}

fn compare_lists_ordered(
    mut left: List,
    mut right: List,
    local_env: &[Value],
    name: &str,
) -> Result<Ordering, EvalError> {
    loop {
        match (pop_list_front(&left)?, pop_list_front(&right)?) {
            (None, None) => return Ok(Ordering::Equal),
            (None, Some(_)) => return Ok(Ordering::Less),
            (Some(_), None) => return Ok(Ordering::Greater),
            (Some((left_head, left_tail)), Some((right_head, right_tail))) => {
                match compare_ordered_values(&left_head, &right_head, local_env, name)? {
                    Ordering::Equal => {
                        left = left_tail;
                        right = right_tail;
                    }
                    ordering => return Ok(ordering),
                }
            }
        }
    }
}

fn equal_values(
    left: &Value,
    right: &Value,
    local_env: &[Value],
    name: &str,
) -> Result<bool, EvalError> {
    let left = force_value_shell(left)?;
    let right = force_value_shell(right)?;
    match (left, right) {
        (Value::Atom(left), Value::Atom(right)) => Ok(left == right),
        (Value::Number(left), Value::Number(right)) => Ok(left == right),
        (Value::Binary(left), Value::Binary(right)) => Ok(left == right),
        (Value::Binary(left), Value::List(right)) => {
            equal_lists(List::from_bytes(left), right, local_env, name)
        }
        (Value::List(left), Value::Binary(right)) => {
            equal_lists(left, List::from_bytes(right), local_env, name)
        }
        (Value::List(left), Value::List(right)) => equal_lists(left, right, local_env, name),
        (Value::Dict(left), Value::Dict(right)) => equal_dicts(&left, &right, local_env, name),
        (Value::Lazy(_), _) | (_, Value::Lazy(_)) => {
            unreachable!("force_value_shell removes suspended values")
        }
        (Value::Builtin(_), _)
        | (_, Value::Builtin(_))
        | (Value::PartialBuiltin(_), _)
        | (_, Value::PartialBuiltin(_))
        | (Value::Function(_), _)
        | (_, Value::Function(_))
        | (Value::Net(_), _)
        | (_, Value::Net(_)) => Err(EvalError::new(format!(
            "{name} builtin cannot compare function values"
        ))),
        (Value::Atom(_), _)
        | (Value::Number(_), _)
        | (Value::Binary(_), _)
        | (Value::List(_), _)
        | (Value::Dict(_), _) => Ok(false),
    }
}

fn equal_lists(
    mut left: List,
    mut right: List,
    local_env: &[Value],
    name: &str,
) -> Result<bool, EvalError> {
    loop {
        match (pop_list_front(&left)?, pop_list_front(&right)?) {
            (None, None) => return Ok(true),
            (None, Some(_)) | (Some(_), None) => return Ok(false),
            (Some((left_head, left_tail)), Some((right_head, right_tail))) => {
                if !equal_values(&left_head, &right_head, local_env, name)? {
                    return Ok(false);
                }
                left = left_tail;
                right = right_tail;
            }
        }
    }
}

fn equal_dicts(
    left: &crate::core::Dict,
    right: &crate::core::Dict,
    local_env: &[Value],
    name: &str,
) -> Result<bool, EvalError> {
    let empty = Value::Dict(crate::core::Dict::new_sync());
    for (key, left_value) in left.iter() {
        let right_value = right.get(key).unwrap_or(&empty);
        if !equal_values(left_value, right_value, local_env, name)? {
            return Ok(false);
        }
    }

    for (key, right_value) in right.iter() {
        if left.contains_key(key) {
            continue;
        }
        if !equal_values(&empty, right_value, local_env, name)? {
            return Ok(false);
        }
    }

    Ok(true)
}
