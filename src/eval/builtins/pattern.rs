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
    let [value] = super::exact(arguments, "pattern")?;
    match builtin {
        Builtin::PatternIsList => pattern_is_list(context, &value),
        Builtin::PatternListTryUncons => pattern_list_try_uncons(context, &value),
        Builtin::PatternListTryUnsnoc => pattern_list_try_unsnoc(context, &value),
        Builtin::PatternListIsEmpty => pattern_list_is_empty(context, &value),
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
