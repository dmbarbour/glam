//! Compiler-private, syntax-independent observations used by pattern lowering.
//!
//! These operations return standard pass/fail effects. A shape mismatch is
//! `.fail`; forcing failures and blocked evaluation propagate normally.

use super::super::*;
use crate::core::Dict;
use crate::list::ListItem;

pub(super) fn apply(
    context: &EvalContext,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    match builtin {
        Builtin::PatternIsList => {
            let [value] = super::exact(arguments, "pattern-is-list")?;
            pattern_is_list(context, &value)
        }
        Builtin::PatternListTryUncons => {
            let [value] = super::exact(arguments, "pattern-list-try-uncons")?;
            pattern_list_try_uncons(context, &value)
        }
        Builtin::PatternListTryUnsnoc => {
            let [value] = super::exact(arguments, "pattern-list-try-unsnoc")?;
            pattern_list_try_unsnoc(context, &value)
        }
        Builtin::PatternListIsEmpty => {
            let [value] = super::exact(arguments, "pattern-list-is-empty")?;
            pattern_list_is_empty(context, &value)
        }
        Builtin::PatternEqual => {
            let [expected, value] = super::exact(arguments, "pattern-equal")?;
            pattern_equal(context, &expected, &value)
        }
        Builtin::PatternIsDict => {
            let [value] = super::exact(arguments, "pattern-is-dict")?;
            pattern_is_dict(context, &value)
        }
        Builtin::PatternDictTryTake => {
            let [path, value] = super::exact(arguments, "pattern-dict-try-take")?;
            pattern_dict_try_take(context, &path, &value)
        }
        Builtin::PatternDictIsEmpty => {
            let [value] = super::exact(arguments, "pattern-dict-is-empty")?;
            pattern_dict_is_empty(context, &value)
        }
        _ => unreachable!("pattern dispatcher received a non-pattern builtin"),
    }
}

fn pattern_is_list(context: &EvalContext, value: &Value) -> Result<Value, EvalError> {
    Ok(match eval_value(context, value)? {
        Value::Binary(_) | Value::List(_) => pattern_success((*keys::UNIT_VALUE).clone()),
        _ => pattern_failure(),
    })
}

fn pattern_list_try_uncons(context: &EvalContext, value: &Value) -> Result<Value, EvalError> {
    let parts = match eval_value(context, value)? {
        Value::Binary(bytes) => bytes.first().map(|byte| {
            (
                Value::Number(Number::from_u8(*byte)),
                Value::Binary(bytes.slice(1..bytes.len())),
            )
        }),
        Value::List(list) => list
            .try_pop_front(&mut |thunk| force_list_thunk(context, thunk))?
            .map(|(head, tail)| (list_item_value(head), Value::List(tail))),
        _ => None,
    };
    Ok(parts.map_or_else(pattern_failure, |(head, tail)| {
        pattern_success(Value::Dict(
            Dict::new_sync()
                .insert((*keys::HEAD).clone(), head)
                .insert((*keys::TAIL).clone(), tail),
        ))
    }))
}

fn pattern_list_try_unsnoc(context: &EvalContext, value: &Value) -> Result<Value, EvalError> {
    let parts = match eval_value(context, value)? {
        Value::Binary(bytes) => bytes.last().map(|byte| {
            (
                Value::Binary(bytes.slice(0..bytes.len() - 1)),
                Value::Number(Number::from_u8(*byte)),
            )
        }),
        Value::List(list) => list
            .try_pop_back(&mut |thunk| force_list_thunk(context, thunk))?
            .map(|(init, last)| (Value::List(init), list_item_value(last))),
        _ => None,
    };
    Ok(parts.map_or_else(pattern_failure, |(init, last)| {
        pattern_success(Value::Dict(
            Dict::new_sync()
                .insert((*keys::INIT).clone(), init)
                .insert((*keys::LAST).clone(), last),
        ))
    }))
}

fn pattern_list_is_empty(context: &EvalContext, value: &Value) -> Result<Value, EvalError> {
    let empty = match eval_value(context, value)? {
        Value::Binary(bytes) => bytes.is_empty(),
        Value::List(list) => list
            .try_pop_front(&mut |thunk| force_list_thunk(context, thunk))?
            .is_none(),
        _ => false,
    };
    Ok(if empty {
        pattern_success((*keys::UNIT_VALUE).clone())
    } else {
        pattern_failure()
    })
}

fn pattern_equal(
    context: &EvalContext,
    expected: &Value,
    value: &Value,
) -> Result<Value, EvalError> {
    let expected = eval_value(context, expected)?;
    let value = eval_value(context, value)?;
    let equal = match (expected, value) {
        (Value::Atom(expected), Value::Atom(value)) => expected == value,
        (Value::Number(expected), Value::Number(value)) => expected == value,
        (Value::Binary(expected), Value::Binary(value)) => expected == value,
        (Value::Binary(expected), Value::List(value)) => {
            binary_equals_list(context, expected.as_ref(), value)?
        }
        (Value::Atom(_) | Value::Number(_) | Value::Binary(_), _) => false,
        (expected, _) => {
            return Err(EvalError::new(format!(
                "pattern-equal received unsupported compiler literal {expected:?}"
            )));
        }
    };
    Ok(if equal {
        pattern_success((*keys::UNIT_VALUE).clone())
    } else {
        pattern_failure()
    })
}

fn pattern_is_dict(context: &EvalContext, value: &Value) -> Result<Value, EvalError> {
    Ok(match eval_value(context, value)? {
        Value::Dict(_) => pattern_success((*keys::UNIT_VALUE).clone()),
        _ => pattern_failure(),
    })
}

fn pattern_dict_try_take(
    context: &EvalContext,
    path: &Value,
    value: &Value,
) -> Result<Value, EvalError> {
    let path = eval_key_path_list(context, path)?;
    if path.is_empty() {
        return Err(EvalError::new(
            "pattern-dict-try-take received an empty compiler path",
        ));
    }
    let Value::Dict(dict) = eval_value(context, value)? else {
        return Ok(pattern_failure());
    };
    let Some((value, rest)) = take_dict_path(context, &dict, &path)? else {
        return Ok(pattern_failure());
    };
    Ok(pattern_success(Value::Dict(
        Dict::new_sync()
            .insert((*keys::VALUE).clone(), value)
            .insert((*keys::REST).clone(), Value::Dict(rest)),
    )))
}

fn take_dict_path(
    context: &EvalContext,
    dict: &Dict,
    path: &[Key],
) -> Result<Option<(Value, Dict)>, EvalError> {
    let Some((head, tail)) = path.split_first() else {
        return Ok(None);
    };
    let Some(selected) = dict.get(head) else {
        return Ok(None);
    };
    let selected = eval_value(context, selected)?;
    if tail.is_empty() {
        if value_is_logically_undefined(context, &selected)? {
            return Ok(None);
        }
        return Ok(Some((selected, dict.remove(head))));
    }

    let Value::Dict(child) = selected else {
        return Ok(None);
    };
    let Some((value, child_rest)) = take_dict_path(context, &child, tail)? else {
        return Ok(None);
    };
    let rest = if child_rest.is_empty() {
        dict.remove(head)
    } else {
        dict.insert(head.clone(), Value::Dict(child_rest))
    };
    Ok(Some((value, rest)))
}

fn pattern_dict_is_empty(context: &EvalContext, value: &Value) -> Result<Value, EvalError> {
    let empty = match eval_value(context, value)? {
        Value::Dict(dict) => dict_is_logically_empty(context, &dict)?,
        _ => false,
    };
    Ok(if empty {
        pattern_success((*keys::UNIT_VALUE).clone())
    } else {
        pattern_failure()
    })
}

fn value_is_logically_undefined(context: &EvalContext, value: &Value) -> Result<bool, EvalError> {
    match eval_value(context, value)? {
        Value::Dict(dict) => dict_is_logically_empty(context, &dict),
        _ => Ok(false),
    }
}

fn dict_is_logically_empty(context: &EvalContext, dict: &Dict) -> Result<bool, EvalError> {
    for (_, value) in dict.iter() {
        if !value_is_logically_undefined(context, value)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn binary_equals_list(
    context: &EvalContext,
    expected: &[u8],
    mut value: List,
) -> Result<bool, EvalError> {
    let mut index = 0;
    loop {
        let item = value.try_pop_front(&mut |thunk| force_list_thunk(context, thunk))?;
        let Some((item, tail)) = item else {
            return Ok(index == expected.len());
        };
        let Some(expected) = expected.get(index) else {
            return Ok(false);
        };
        let actual = eval_value(context, &list_item_value(item))?;
        if actual != Value::Number(Number::from_u8(*expected)) {
            return Ok(false);
        }
        index += 1;
        value = tail;
    }
}

fn list_item_value(item: ListItem<Value>) -> Value {
    match item {
        ListItem::Byte(byte) => Value::Number(Number::from_u8(byte)),
        ListItem::Value(value) => value,
    }
}

fn pattern_success(value: Value) -> Value {
    effect_call_value(&keys::R, vec![value])
}

fn pattern_failure() -> Value {
    effect_call_value(&keys::FAIL, Vec::new())
}

fn effect_call_value(name: &Key, arguments: Vec<Value>) -> Value {
    let Key::Atom(name) = name else {
        unreachable!("standard effect request names are atom keys");
    };
    effect_value(Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::EffectCall,
        arguments: Arc::from([
            Value::Atom(*name),
            Value::List(List::from_values(arguments)),
        ]),
    }))
}
