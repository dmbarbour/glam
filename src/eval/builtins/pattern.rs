//! Compiler-private, syntax-independent observations used by pattern lowering.
//!
//! These operations return standard pass/fail effects. A shape mismatch is
//! `.fail`; forcing failures and blocked evaluation propagate normally.

use super::super::*;
use crate::core::Dict;
use crate::list::ListItem;

pub(super) fn apply(
    context: &EvaluatorStepContext<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    match builtin {
        Builtin::PatternEqual => {
            let [expected, value] = super::exact(arguments, "pattern-equal")?;
            pattern_equal(context, &expected, &value)
        }
        Builtin::PatternDictTryTake => {
            let [path, value] = super::exact(arguments, "pattern-dict-try-take")?;
            pattern_dict_try_take(context, &path, &value, false)
        }
        Builtin::PatternDictTryTakeOptional => {
            let [path, value] = super::exact(arguments, "pattern-dict-try-take-optional")?;
            pattern_dict_try_take(context, &path, &value, true)
        }
        _ => unreachable!("pattern dispatcher received a non-pattern builtin"),
    }
}

fn pattern_equal(
    context: &EvaluatorStepContext<'_>,
    expected: &Value,
    value: &Value,
) -> Result<Value, EvaluationHalt> {
    let expected = eval_value_in(context, expected)?;
    let value = eval_value_in(context, value)?;
    let equal = match (expected, value) {
        (Value::Atom(expected), Value::Atom(value)) => expected == value,
        (Value::Number(expected), Value::Number(value)) => expected == value,
        (Value::Binary(expected), Value::Binary(value)) => expected == value,
        (Value::Binary(expected), Value::List(value)) => {
            binary_equals_list(context, expected.as_ref(), value)?
        }
        (Value::Atom(_) | Value::Number(_) | Value::Binary(_), _) => false,
        (expected, _) => {
            return Err(EvaluationHalt::new(format!(
                "pattern-equal received unsupported compiler literal {expected:?}"
            )));
        }
    };
    Ok(if equal {
        pattern_success(context.context().values().unit())
    } else {
        pattern_failure()
    })
}

fn pattern_dict_try_take(
    context: &EvaluatorStepContext<'_>,
    path: &Value,
    value: &Value,
    optional: bool,
) -> Result<Value, EvaluationHalt> {
    let path = eval_key_path_list_in(context, path)?;
    if path.is_empty() {
        return Err(EvaluationHalt::new(
            "pattern-dict-try-take received an empty compiler path",
        ));
    }
    let Value::Dict(dict) = eval_value_in(context, value)? else {
        return Ok(pattern_failure());
    };
    let (value, rest) = match take_dict_path(context, &dict, &path)? {
        DictPathTake::Found { value, rest } => (value, rest),
        DictPathTake::Absent if optional => (Value::Dict(Dict::new_sync()), dict),
        DictPathTake::Absent | DictPathTake::WrongIntermediateKind => {
            return Ok(pattern_failure());
        }
    };
    Ok(pattern_success(Value::Dict(
        Dict::new_sync()
            .insert((*keys::VALUE).clone(), value)
            .insert((*keys::REST).clone(), Value::Dict(rest)),
    )))
}

enum DictPathTake {
    Found { value: Value, rest: Dict },
    Absent,
    WrongIntermediateKind,
}

fn take_dict_path(
    context: &EvaluatorStepContext<'_>,
    dict: &Dict,
    path: &[Key],
) -> Result<DictPathTake, EvaluationHalt> {
    let Some((head, tail)) = path.split_first() else {
        return Ok(DictPathTake::Absent);
    };
    let Some(selected) = dict.get(head) else {
        return Ok(DictPathTake::Absent);
    };
    let selected = eval_value_in(context, selected)?;
    if tail.is_empty() {
        if value_is_logically_undefined(context, &selected)? {
            return Ok(DictPathTake::Absent);
        }
        return Ok(DictPathTake::Found {
            value: selected,
            rest: dict.remove(head),
        });
    }

    let Value::Dict(child) = selected else {
        return Ok(DictPathTake::WrongIntermediateKind);
    };
    let (value, child_rest) = match take_dict_path(context, &child, tail)? {
        DictPathTake::Found { value, rest } => (value, rest),
        other => return Ok(other),
    };
    let rest = if child_rest.is_empty() {
        dict.remove(head)
    } else {
        dict.insert(head.clone(), Value::Dict(child_rest))
    };
    Ok(DictPathTake::Found { value, rest })
}

fn value_is_logically_undefined(
    context: &EvaluatorStepContext<'_>,
    value: &Value,
) -> Result<bool, EvaluationHalt> {
    match eval_value_in(context, value)? {
        Value::Dict(dict) => dict_is_logically_empty(context, &dict),
        _ => Ok(false),
    }
}

fn dict_is_logically_empty(
    context: &EvaluatorStepContext<'_>,
    dict: &Dict,
) -> Result<bool, EvaluationHalt> {
    for (_, value) in dict.iter() {
        if !value_is_logically_undefined(context, value)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn binary_equals_list(
    context: &EvaluatorStepContext<'_>,
    expected: &[u8],
    mut value: List,
) -> Result<bool, EvaluationHalt> {
    let mut index = 0;
    loop {
        let item = value.try_pop_front_by(
            &mut |value| context.with_value_access(|access| access.values().duplicate_value(value)),
            &mut |thunk| force_list_thunk_in(context, thunk),
        )?;
        let Some((item, tail)) = item else {
            return Ok(index == expected.len());
        };
        let Some(expected) = expected.get(index) else {
            return Ok(false);
        };
        let actual = eval_value_in(context, &list_item_value(item))?;
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
