//! Compiler-private, syntax-independent observations used by pattern lowering.
//!
//! These operations return standard pass/fail effects. A shape mismatch is
//! `.fail`; forcing failures and blocked evaluation propagate normally.

use super::super::*;
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
