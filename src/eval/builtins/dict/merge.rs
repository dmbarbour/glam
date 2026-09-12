use super::super::super::*;
use super::super::annotation::{annotation_error_value, atom_name, is_undefined_value};

pub(super) fn eval_merge_duplicate_builtin(
    context: &EvaluatorStepContext<'_>,
    name: &Value,
    left: &Value,
    right: &Value,
) -> Result<Value, EvaluationHalt> {
    let name = eval_value_in(context, name)?;
    let name = match name {
        Value::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Value::Atom(atom) => atom_name(&atom)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{atom:?}")),
        other => format!("{other:?}"),
    };
    let left = eval_value_in(context, left)?;
    let right = eval_value_in(context, right)?;

    if is_undefined_value(&left) {
        return Ok(right);
    }
    if is_undefined_value(&right) {
        return Ok(left);
    }
    if is_error_lazy_value(context, &left) {
        return Ok(left);
    }
    if is_error_lazy_value(context, &right) {
        return Ok(right);
    }

    match (&left, &right) {
        (Value::Dict(left_dict), Value::Dict(right_dict)) => {
            Ok(Value::Dict(merge_dicts(context, left_dict, right_dict)))
        }
        _ => Ok(annotation_error_value(
            context,
            format!("dictionary union is ambiguous at key `{name}`"),
        )),
    }
}

pub(super) fn merge_dicts(
    context: &EvaluatorStepContext<'_>,
    left: &crate::core::Dict,
    right: &crate::core::Dict,
) -> crate::core::Dict {
    let (mut merged, updates) = if left.size() >= right.size() {
        (left.clone(), right)
    } else {
        (right.clone(), left)
    };

    for (key, value) in updates.iter() {
        let next_value = match merged.get(key) {
            Some(existing) => Some(merge_duplicate_dict_value(context, key, existing, value)),
            None if context
                .with_value_access(|access| is_undefined_dict_value(access.values(), value)) =>
            {
                None
            }
            None => Some(value.clone()),
        };
        merged = match next_value {
            Some(value)
                if context.with_value_access(|access| {
                    is_undefined_dict_value(access.values(), &value)
                }) =>
            {
                merged.remove(key)
            }
            Some(value) => merged.insert(key.clone(), value),
            None => merged,
        };
    }

    merged
}

fn merge_duplicate_dict_value(
    context: &EvaluatorStepContext<'_>,
    key: &Key,
    left: &Value,
    right: &Value,
) -> Value {
    if context.with_value_access(|access| is_undefined_dict_value(access.values(), left)) {
        right.clone()
    } else if context.with_value_access(|access| is_undefined_dict_value(access.values(), right)) {
        left.clone()
    } else if matches!((left, right), (Value::Dict(_), Value::Dict(_)))
        || context.with_value_access(|access| is_deferred_value(access.values(), left))
        || context.with_value_access(|access| is_deferred_value(access.values(), right))
    {
        builtin_apply3_value(
            context,
            Builtin::MergeDuplicate,
            &Value::binary_from_text(&format_name_part(key)),
            left,
            right,
        )
    } else {
        Value::Lazy(context.construct_lazy(|access| {
            LazyValue::error_in(
                access,
                format!(
                    "dictionary union is ambiguous at key `{}`",
                    format_name_part(key)
                ),
            )
        }))
    }
}

pub(super) fn update_dict_path(
    context: &EvaluatorStepContext<'_>,
    dict: &crate::core::Dict,
    path: &[Key],
    new_value: Value,
) -> crate::core::Dict {
    let Some((head, rest)) = path.split_first() else {
        return dict.clone();
    };

    let next_value = if rest.is_empty() {
        new_value
    } else {
        let prior = dict
            .get(head)
            .cloned()
            .unwrap_or_else(|| Value::Dict(crate::core::Dict::new_sync()));
        update_nested_dict_path(context, head, rest, new_value, prior)
    };

    if context.with_value_access(|access| is_undefined_dict_value(access.values(), &next_value)) {
        dict.remove(head)
    } else {
        dict.insert(head.clone(), next_value)
    }
}

fn update_nested_dict_path(
    context: &EvaluatorStepContext<'_>,
    head: &Key,
    rest: &[Key],
    new_value: Value,
    prior: Value,
) -> Value {
    match prior {
        Value::Dict(dict) => Value::Dict(update_dict_path(context, &dict, rest, new_value)),
        Value::Lazy(_) | Value::Promised(_) => builtin_apply3_value(
            context,
            Builtin::DictUpdate,
            &key_path_value(rest),
            &new_value,
            &prior,
        ),
        _ => Value::Lazy(context.construct_lazy(|access| {
            LazyValue::error_in(
                access,
                format!(
                    "dictionary update path `{}` traverses a non-dictionary value",
                    format_name_part(head)
                ),
            )
        })),
    }
}

fn key_path_value(path: &[Key]) -> Value {
    Value::List(List::from_values(path.iter().map(key_value).collect()))
}

fn key_value(key: &Key) -> Value {
    match key {
        Key::Atom(atom) => Value::Atom(*atom),
        Key::Number(number) => Value::Number(number.clone()),
        Key::Binary(bytes) => Value::Binary(bytes.clone()),
        Key::AbstractGlobalPath(parts) => Value::Atom(crate::core::Atom::from_key(
            &Key::AbstractGlobalPath(parts.clone()),
        )),
        Key::List(items) => Value::List(List::from_values(items.iter().map(key_value).collect())),
        Key::Dict(entries) => Value::Dict(
            entries
                .iter()
                .fold(crate::core::Dict::new_sync(), |dict, (key, value)| {
                    dict.insert(key.clone(), key_value(value))
                }),
        ),
    }
}

fn builtin_apply3_value(
    context: &EvaluatorStepContext<'_>,
    builtin: Builtin,
    first: &Value,
    second: &Value,
    third: &Value,
) -> Value {
    Value::Lazy(context.construct_lazy(|access| {
        LazyValue::from_builtin_in(
            access,
            BuiltinCall {
                builtin,
                arguments: Arc::from([first.clone(), second.clone(), third.clone()]),
            },
        )
    }))
}
