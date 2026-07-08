use std::fmt;
use std::sync::Arc;

use crate::core::{Builtin, Expr, Key, KeyExpr, List, Term, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalError {
    message: String,
}

impl EvalError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EvalError {}

pub fn eval_term(term: &Term) -> Result<Value, EvalError> {
    match term {
        Term::Expr(expr) => {
            let root_env = match expr {
                Expr::Value(Value::Dict(dict)) => Some(Value::Dict(dict.clone())),
                _ => None,
            };
            eval_expr(expr, root_env.as_ref())
        }
    }
}

fn eval_expr(expr: &Expr, root_env: Option<&Value>) -> Result<Value, EvalError> {
    match expr {
        Expr::Value(value) => eval_value(value, root_env),
        Expr::List(items) => {
            let mut list = List::empty();
            for item in items.iter() {
                let value = eval_expr(item, root_env)?;
                list = List::concat(list, list_literal_segment(value));
            }
            Ok(Value::List(list))
        }
        Expr::Apply(function, argument) => eval_apply(function, argument, root_env),
        Expr::Name(path) => resolve_name(path, root_env),
        Expr::SingletonDict { key, value } => eval_singleton_dict(key, value, root_env),
        Expr::DictUnion { items, key_context } => {
            eval_dict_union(items, key_context.as_ref(), root_env)
        }
        Expr::Error(message) => Err(EvalError::new(message.as_ref())),
    }
}

pub fn eval_value(value: &Value, root_env: Option<&Value>) -> Result<Value, EvalError> {
    match value {
        Value::Number(number) => Ok(Value::Number(*number)),
        Value::Binary(bytes) => Ok(Value::Binary(bytes.clone())),
        Value::List(list) => Ok(Value::List(list.clone())),
        Value::Dict(dict) => Ok(Value::Dict(dict.clone())),
        Value::Builtin(builtin) => Ok(Value::Builtin(*builtin)),
        Value::Expr(expr) => eval_expr(expr, root_env),
    }
}

pub fn eval_key(value: &Value, root_env: Option<&Value>) -> Result<Key, EvalError> {
    let value = eval_value(value, root_env)?;
    value_to_key(&value, root_env)
}

fn resolve_name(path: &[KeyExpr], root_env: Option<&Value>) -> Result<Value, EvalError> {
    let Some(root_env) = root_env else {
        return Err(EvalError::new("name resolution requires a dictionary root"));
    };

    resolve_key_path(root_env.clone(), path, path, root_env)
}

fn format_name(path: &[KeyExpr]) -> String {
    path.iter()
        .map(format_name_key_expr)
        .collect::<Vec<_>>()
        .join(".")
}

fn format_name_part(key: &Key) -> String {
    match key {
        Key::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Key::Atom(atom) => match atom.key() {
            Key::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            other => format!("{other:?}"),
        },
        other => format!("{other:?}"),
    }
}

fn format_name_key_expr(key: &KeyExpr) -> String {
    match key {
        KeyExpr::Key(key) => format_name_part(key),
        KeyExpr::Expr(_) => "[expr]".to_owned(),
        KeyExpr::ListExpr(_) => "(list-expr)".to_owned(),
    }
}

fn value_to_key(value: &Value, root_env: Option<&Value>) -> Result<Key, EvalError> {
    match value {
        Value::Number(number) => Ok(Key::Number(*number)),
        Value::Binary(bytes) => Ok(Key::Binary(bytes.clone())),
        Value::List(list) => Ok(Key::List(list_to_key_items(list, root_env)?)),
        Value::Dict(dict) => Ok(Key::Dict(Arc::from(
            dict.iter()
                .map(|(key, value)| {
                    let value = eval_key(value, root_env)?;
                    if matches!(&value, Key::Dict(entries) if entries.is_empty()) {
                        return Ok(None);
                    }
                    Ok(Some((key.clone(), value)))
                })
                .collect::<Result<Vec<_>, EvalError>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>(),
        ))),
        Value::Builtin(_) => Err(EvalError::new(
            "dictionary keys must evaluate to keyable values",
        )),
        Value::Expr(_) => Err(EvalError::new(
            "dictionary keys must evaluate to keyable values",
        )),
    }
}

fn resolve_key_path(
    current: Value,
    remaining: &[KeyExpr],
    full_path: &[KeyExpr],
    root_env: &Value,
) -> Result<Value, EvalError> {
    let Some((head, rest)) = remaining.split_first() else {
        return eval_value(&current, Some(root_env));
    };

    let expanded = expand_key_expr(head, Some(root_env))?;
    let next = resolve_expanded_keys(current, &expanded, full_path, remaining, root_env)?;
    resolve_key_path(next, rest, full_path, root_env)
}

fn resolve_expanded_keys(
    mut current: Value,
    expanded: &[Key],
    full_path: &[KeyExpr],
    remaining: &[KeyExpr],
    root_env: &Value,
) -> Result<Value, EvalError> {
    for key in expanded {
        let dict = force_dict_shell(&current, Some(root_env), full_path, remaining)?;
        current = dict.get(key).cloned().ok_or_else(|| {
            EvalError::new(format!("name `{}` is not defined", format_name(full_path)))
        })?;
    }
    Ok(current)
}

fn force_dict_shell(
    value: &Value,
    root_env: Option<&Value>,
    full_path: &[KeyExpr],
    remaining: &[KeyExpr],
) -> Result<crate::core::Dict, EvalError> {
    match eval_value(value, root_env)? {
        Value::Dict(dict) => Ok(dict),
        _ => {
            let traversed = &full_path[..full_path.len() - remaining.len()];
            let culprit = if traversed.is_empty() {
                full_path
            } else {
                traversed
            };
            Err(EvalError::new(format!(
                "name `{}` is not a dictionary",
                format_name(culprit)
            )))
        }
    }
}

fn eval_singleton_dict(
    key: &KeyExpr,
    value: &Expr,
    root_env: Option<&Value>,
) -> Result<Value, EvalError> {
    let key = eval_key_expr(key, root_env)?;
    let observed = eval_expr(value, root_env)?;
    if is_undefined_dict_value(&observed) {
        return Ok(Value::Dict(crate::core::Dict::new_sync()));
    }

    let stored = match value {
        Expr::Value(value) => value.clone(),
        _ => Value::Expr(Arc::new(value.clone())),
    };
    Ok(Value::Dict(
        crate::core::Dict::new_sync().insert(key, stored),
    ))
}

fn eval_apply(
    function: &Expr,
    argument: &Expr,
    root_env: Option<&Value>,
) -> Result<Value, EvalError> {
    let function = eval_expr(function, root_env)?;
    let argument = thunk_value(argument);
    apply_value(function, argument, root_env)
}

fn thunk_value(expr: &Expr) -> Value {
    match expr {
        Expr::Value(value) => value.clone(),
        _ => Value::Expr(Arc::new(expr.clone())),
    }
}

fn apply_value(
    function: Value,
    argument: Value,
    root_env: Option<&Value>,
) -> Result<Value, EvalError> {
    match function {
        Value::Builtin(builtin) => apply_builtin(builtin, Vec::new(), argument, root_env),
        Value::Expr(expr) => {
            if let Some((builtin, args)) = builtin_application_spine(expr.as_ref()) {
                apply_builtin(builtin, args, argument, root_env)
            } else {
                Ok(Value::Expr(Arc::new(Expr::Apply(
                    expr.clone(),
                    Arc::new(Expr::Value(argument)),
                ))))
            }
        }
        _ => Err(EvalError::new("application requires a function value")),
    }
}

fn apply_builtin(
    builtin: Builtin,
    mut args: Vec<Value>,
    argument: Value,
    root_env: Option<&Value>,
) -> Result<Value, EvalError> {
    args.push(argument);
    if args.len() < builtin.arity() {
        return Ok(partial_builtin_value(builtin, &args));
    }

    match builtin {
        Builtin::Append => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("append builtin received the wrong number of arguments")
            })?;
            append_values(eval_value(&left, root_env)?, eval_value(&right, root_env)?)
        }
    }
}

fn partial_builtin_value(builtin: Builtin, args: &[Value]) -> Value {
    let expr = args.iter().cloned().fold(
        Expr::Value(Value::Builtin(builtin)),
        |function, argument| Expr::Apply(Arc::new(function), Arc::new(Expr::Value(argument))),
    );
    Value::Expr(Arc::new(expr))
}

fn builtin_application_spine(expr: &Expr) -> Option<(Builtin, Vec<Value>)> {
    match expr {
        Expr::Value(Value::Builtin(builtin)) => Some((*builtin, Vec::new())),
        Expr::Apply(function, argument) => {
            let (builtin, mut args) = builtin_application_spine(function.as_ref())?;
            let Expr::Value(argument) = argument.as_ref() else {
                return None;
            };
            args.push(argument.clone());
            Some((builtin, args))
        }
        _ => None,
    }
}

fn eval_dict_union(
    items: &[Arc<Expr>],
    key_context: Option<&Key>,
    root_env: Option<&Value>,
) -> Result<Value, EvalError> {
    let mut merged = crate::core::Dict::new_sync();

    for item in items {
        let value = eval_expr(item, root_env)?;
        let Value::Dict(dict) = value else {
            return Err(EvalError::new(match key_context {
                Some(key) => format!(
                    "dictionary union is ambiguous at key `{}`",
                    format_name_part(key)
                ),
                None => "dictionary union requires dictionary values".to_owned(),
            }));
        };
        merged = merge_dicts(&merged, &dict);
    }

    Ok(Value::Dict(merged))
}

fn merge_dicts(left: &crate::core::Dict, right: &crate::core::Dict) -> crate::core::Dict {
    let mut merged = left.clone();

    for (key, value) in right.iter() {
        let next_value = match merged.get(key) {
            Some(existing) => Some(merge_duplicate_dict_value(key, existing, value)),
            None if is_undefined_dict_value(value) => None,
            None => Some(value.clone()),
        };
        merged = match next_value {
            Some(value) if is_undefined_dict_value(&value) => merged.remove(key),
            Some(value) => merged.insert(key.clone(), value),
            None => merged,
        };
    }

    merged
}

fn merge_duplicate_dict_value(key: &Key, left: &Value, right: &Value) -> Value {
    match (left, right) {
        _ if is_undefined_dict_value(left) => right.clone(),
        _ if is_undefined_dict_value(right) => left.clone(),
        (Value::Dict(left_dict), Value::Dict(right_dict)) => {
            Value::Dict(merge_dicts(left_dict, right_dict))
        }
        _ if value_can_be_dict(left) || value_can_be_dict(right) => {
            Value::Expr(Arc::new(Expr::DictUnion {
                items: Arc::from([value_as_expr(left), value_as_expr(right)]),
                key_context: Some(key.clone()),
            }))
        }
        _ => Value::Expr(Arc::new(Expr::Error(Arc::from(format!(
            "dictionary union is ambiguous at key `{}`",
            format_name_part(key)
        ))))),
    }
}

fn value_as_expr(value: &Value) -> Arc<Expr> {
    match value {
        Value::Expr(expr) => expr.clone(),
        _ => Arc::new(Expr::Value(value.clone())),
    }
}

fn value_can_be_dict(value: &Value) -> bool {
    matches!(value, Value::Dict(_) | Value::Expr(_))
}

fn eval_key_expr(key: &KeyExpr, root_env: Option<&Value>) -> Result<Key, EvalError> {
    match key {
        KeyExpr::Key(key) => Ok(key.clone()),
        KeyExpr::Expr(expr) => eval_key(&Value::Expr(expr.clone()), root_env),
        KeyExpr::ListExpr(_) => Err(EvalError::new(
            "list-valued path segment cannot be used as a singleton key",
        )),
    }
}

fn is_undefined_dict_value(value: &Value) -> bool {
    matches!(value, Value::Dict(dict) if dict.is_empty())
}

fn expand_key_expr(key: &KeyExpr, root_env: Option<&Value>) -> Result<Vec<Key>, EvalError> {
    match key {
        KeyExpr::Key(key) => Ok(vec![key.clone()]),
        KeyExpr::Expr(expr) => Ok(vec![eval_key(&Value::Expr(expr.clone()), root_env)?]),
        KeyExpr::ListExpr(expr) => eval_key_path_list(&Value::Expr(expr.clone()), root_env),
    }
}

fn eval_key_path_list(value: &Value, root_env: Option<&Value>) -> Result<Vec<Key>, EvalError> {
    let value = eval_value(value, root_env)?;
    let Value::List(list) = value else {
        return Err(EvalError::new(
            "path list expression must evaluate to a list value",
        ));
    };

    let items = std::cell::RefCell::new(Vec::new());
    list.for_each_segment(
        &mut |bytes| {
            items
                .borrow_mut()
                .extend(bytes.iter().map(|byte| Key::Number(i64::from(*byte))));
            Ok::<_, EvalError>(())
        },
        &mut |values| {
            for value in values.iter() {
                items.borrow_mut().push(eval_key(value, root_env)?);
            }
            Ok(())
        },
    )?;
    Ok(items.into_inner())
}

fn list_to_key_items(list: &List, root_env: Option<&Value>) -> Result<Arc<[Key]>, EvalError> {
    let items = std::cell::RefCell::new(Vec::new());
    list.for_each_segment(
        &mut |bytes| {
            items
                .borrow_mut()
                .extend(bytes.iter().map(|byte| Key::Number(i64::from(*byte))));
            Ok::<_, EvalError>(())
        },
        &mut |values| {
            for value in values.iter() {
                items.borrow_mut().push(eval_key(value, root_env)?);
            }
            Ok(())
        },
    )?;
    Ok(Arc::from(items.into_inner()))
}

fn append_values(left: Value, right: Value) -> Result<Value, EvalError> {
    let left = append_sequence(left)?;
    let right = append_sequence(right)?;
    Ok(Value::List(List::concat(left, right)))
}

fn append_sequence(value: Value) -> Result<List, EvalError> {
    match value {
        Value::Binary(bytes) => Ok(List::from_bytes(bytes)),
        Value::List(list) => Ok(list),
        _ => Err(EvalError::new(
            "append requires list or binary values on both sides",
        )),
    }
}

fn list_literal_segment(value: Value) -> List {
    match value {
        Value::Binary(bytes) => List::from_bytes(bytes),
        Value::List(list) => list,
        other => Value::singleton_list(other),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::core::{Dict, Expr, Term, Value};

    use super::*;

    #[test]
    fn evaluates_dictionary_terms_to_values() {
        let asm = Dict::new_sync().insert(
            crate::core::Key::atom_from_text("result"),
            Value::binary_from_text("Hello, World!"),
        );
        let root =
            Dict::new_sync().insert(crate::core::Key::atom_from_text("asm"), Value::Dict(asm));

        let value =
            eval_term(&Term::Expr(Expr::Value(Value::Dict(root)))).expect("term should evaluate");

        assert!(matches!(value, Value::Dict(_)));
        assert!(
            value
                .get_atom_path(&[
                    crate::core::Atom::from_key(&crate::core::Key::binary_from_text("asm")),
                    crate::core::Atom::from_key(&crate::core::Key::binary_from_text("result")),
                ])
                .is_some()
        );
    }

    #[test]
    fn evaluates_binary_literals() {
        let value = eval_term(&Term::Expr(Expr::Value(Value::binary_from_text("oops"))))
            .expect("binary literal should evaluate");

        assert_eq!(value, Value::binary_from_text("oops"));
    }

    #[test]
    fn appends_lists() {
        let expr = Term::Expr(Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                Arc::new(Expr::Value(Value::List(List::from_values(vec![
                    Value::Number(1),
                    Value::Number(2),
                ])))),
            )),
            Arc::new(Expr::Value(Value::List(List::from_values(vec![
                Value::Number(3),
            ])))),
        ));

        let value = eval_term(&expr).expect("append should evaluate");

        let Value::List(list) = value else {
            panic!("append should produce a list");
        };
        let mut values = Vec::new();
        list.for_each_segment(&mut |_bytes| Ok::<_, ()>(()), &mut |segment| {
            values.extend(segment.iter().cloned());
            Ok(())
        })
        .expect("should walk list");
        assert_eq!(
            values,
            vec![Value::Number(1), Value::Number(2), Value::Number(3)]
        );
    }

    #[test]
    fn evaluates_mixed_list_segments() {
        let expr = Term::Expr(Expr::List(Arc::from([
            Arc::new(Expr::Value(Value::Number(1))),
            Arc::new(Expr::Value(Value::binary_from_text("Hi"))),
            Arc::new(Expr::Apply(
                Arc::new(Expr::Apply(
                    Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                    Arc::new(Expr::Value(Value::List(List::from_values(vec![
                        Value::Number(2),
                    ])))),
                )),
                Arc::new(Expr::Value(Value::binary_from_text("!"))),
            )),
        ])));

        let value = eval_term(&expr).expect("list should evaluate");

        let Value::List(list) = value else {
            panic!("list expression should produce a list");
        };
        let mut saw_bytes = Vec::new();
        let mut saw_values = Vec::new();
        list.for_each_segment(
            &mut |bytes| {
                saw_bytes.push(bytes.to_vec());
                Ok::<_, ()>(())
            },
            &mut |segment| {
                saw_values.push(segment.iter().cloned().collect::<Vec<_>>());
                Ok(())
            },
        )
        .expect("should walk list");

        assert_eq!(
            saw_values,
            vec![vec![Value::Number(1)], vec![Value::Number(2)]]
        );
        assert_eq!(saw_bytes, vec![b"Hi".to_vec(), b"!".to_vec()]);
    }

    #[test]
    fn appends_list_and_binary() {
        let expr = Term::Expr(Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                Arc::new(Expr::Value(Value::List(List::from_values(vec![
                    Value::Number(72),
                    Value::Number(105),
                ])))),
            )),
            Arc::new(Expr::Value(Value::binary_from_text("!"))),
        ));

        let value = eval_term(&expr).expect("append should evaluate");

        assert!(matches!(value, Value::List(_)));
    }

    #[test]
    fn resolves_names_against_final_root() {
        let hello = crate::core::Key::atom_from_text("hello");
        let world = crate::core::Key::atom_from_text("world");
        let asm = crate::core::Atom::from_key(&crate::core::Key::binary_from_text("asm"));
        let result = crate::core::Atom::from_key(&crate::core::Key::binary_from_text("result"));

        let root = Dict::new_sync()
            .insert(
                crate::core::Key::Atom(asm),
                Value::Dict(Dict::new_sync().insert(
                    crate::core::Key::Atom(result),
                    Value::Expr(Arc::new(Expr::Apply(
                        Arc::new(Expr::Apply(
                            Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                            Arc::new(Expr::Apply(
                                Arc::new(Expr::Apply(
                                    Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                                    Arc::new(Expr::Apply(
                                        Arc::new(Expr::Apply(
                                            Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                                            Arc::new(Expr::Name(Arc::from([KeyExpr::Key(
                                                hello.clone(),
                                            )]))),
                                        )),
                                        Arc::new(Expr::Value(Value::binary_from_text(", "))),
                                    )),
                                )),
                                Arc::new(Expr::Name(Arc::from([KeyExpr::Key(world.clone())]))),
                            )),
                        )),
                        Arc::new(Expr::Value(Value::binary_from_text("!"))),
                    ))),
                )),
            )
            .insert(hello, Value::binary_from_text("Hello"))
            .insert(world, Value::binary_from_text("World"));

        let value =
            eval_term(&Term::Expr(Expr::Value(Value::Dict(root)))).expect("term should evaluate");
        let result_value = value
            .get_atom_path(&[asm, result])
            .expect("result should exist");
        let Value::Expr(expr) = result_value else {
            panic!("resolved result should stay lazy until demanded");
        };
        let resolved = eval_value(&Value::Expr(expr.clone()), Some(&value))
            .expect("result expression should evaluate when demanded");

        let Value::List(list) = resolved else {
            panic!("resolved result should be a list");
        };
        let bytes = std::cell::RefCell::new(Vec::new());
        list.for_each_segment(
            &mut |segment| {
                bytes.borrow_mut().extend_from_slice(segment);
                Ok::<_, ()>(())
            },
            &mut |segment| {
                for item in segment.iter() {
                    let Value::Number(number) = item else {
                        panic!("byte-oriented result should not contain nested values");
                    };
                    bytes.borrow_mut().push(*number as u8);
                }
                Ok(())
            },
        )
        .expect("should walk resolved list");

        assert_eq!(bytes.into_inner(), b"Hello, World!");
    }

    #[test]
    fn evaluates_keyable_values_into_keys() {
        let key = eval_key(
            &Value::List(List::concat(
                List::from_values(vec![Value::Number(1)]),
                List::from_bytes(Arc::from(&b"Hi"[..])),
            )),
            None,
        )
        .expect("list should evaluate to a key");

        assert_eq!(
            key,
            Key::List(Arc::from([
                Key::Number(1),
                Key::Number(i64::from(b'H')),
                Key::Number(i64::from(b'i')),
            ]))
        );
    }

    #[test]
    fn evaluates_expressions_before_key_validation() {
        let key = eval_key(&Value::Expr(Arc::new(Expr::Value(Value::Number(1)))), None)
            .expect("expressions should be allowed when they evaluate to keyable values");

        assert_eq!(key, Key::Number(1));
    }

    #[test]
    fn dictionaries_remain_lazy_under_eval_value() {
        let value = Value::Dict(crate::core::Dict::new_sync().insert(
            Key::atom_from_text("answer"),
            Value::Expr(Arc::new(Expr::Value(Value::Number(42)))),
        ));

        let evaluated = eval_value(&value, None).expect("dict should stay lazy");

        assert_eq!(evaluated, value);
    }

    #[test]
    fn rejects_unevaluable_keys() {
        let err = eval_key(
            &Value::Expr(Arc::new(Expr::Name(Arc::from([KeyExpr::Key(
                Key::atom_from_text("missing"),
            )])))),
            Some(&Value::Dict(crate::core::Dict::new_sync())),
        )
        .expect_err("missing names should not produce keys");

        assert_eq!(err.to_string(), "name `missing` is not defined");
    }

    #[test]
    fn raw_value_to_key_rejects_expressions() {
        assert_eq!(
            Key::from_value(&Value::Expr(Arc::new(Expr::Value(Value::Number(1))))),
            None
        );
    }

    #[test]
    fn eval_key_forces_nested_dictionary_values() {
        let key = eval_key(
            &Value::Dict(crate::core::Dict::new_sync().insert(
                Key::atom_from_text("answer"),
                Value::Expr(Arc::new(Expr::Value(Value::Number(42)))),
            )),
            None,
        )
        .expect("dict key should force nested values");

        assert_eq!(
            key,
            Key::Dict(Arc::from([(
                Key::atom_from_text("answer"),
                Key::Number(42),
            )]))
        );
    }

    #[test]
    fn singleton_dict_filters_empty_dictionary_values() {
        let value = eval_term(&Term::Expr(Expr::SingletonDict {
            key: KeyExpr::Key(Key::atom_from_text("gone")),
            value: Arc::new(Expr::DictUnion {
                items: Arc::from([]),
                key_context: None,
            }),
        }))
        .expect("singleton dict should evaluate");

        assert_eq!(value, Value::Dict(crate::core::Dict::new_sync()));
    }

    #[test]
    fn dictionary_unions_merge_nested_dictionaries_transitively() {
        let key = Key::atom_from_text("greeting");
        let hello = Key::atom_from_text("hello");
        let world = Key::atom_from_text("world");

        let expr = Term::Expr(Expr::DictUnion {
            items: Arc::from([
                Arc::new(Expr::Value(Value::Dict(
                    crate::core::Dict::new_sync().insert(
                        key.clone(),
                        Value::Dict(
                            crate::core::Dict::new_sync()
                                .insert(hello.clone(), Value::binary_from_text("Hello")),
                        ),
                    ),
                ))),
                Arc::new(Expr::Value(Value::Dict(
                    crate::core::Dict::new_sync().insert(
                        key.clone(),
                        Value::Dict(
                            crate::core::Dict::new_sync()
                                .insert(world.clone(), Value::binary_from_text("World")),
                        ),
                    ),
                ))),
            ]),
            key_context: None,
        });

        let value = eval_term(&expr).expect("dict union should evaluate");
        let greeting = value.get_key_path(&[key]).expect("greeting should exist");
        let Value::Dict(greeting) = greeting else {
            panic!("greeting should be a merged dictionary");
        };

        assert_eq!(
            greeting.get(&hello),
            Some(&Value::binary_from_text("Hello"))
        );
        assert_eq!(
            greeting.get(&world),
            Some(&Value::binary_from_text("World"))
        );
    }

    #[test]
    fn dictionary_unions_treat_empty_dictionary_values_as_undefined() {
        let key = Key::atom_from_text("greeting");
        let expr = Term::Expr(Expr::DictUnion {
            items: Arc::from([
                Arc::new(Expr::SingletonDict {
                    key: KeyExpr::Key(key.clone()),
                    value: Arc::new(Expr::Value(Value::binary_from_text("Hello"))),
                }),
                Arc::new(Expr::SingletonDict {
                    key: KeyExpr::Key(key.clone()),
                    value: Arc::new(Expr::DictUnion {
                        items: Arc::from([]),
                        key_context: None,
                    }),
                }),
            ]),
            key_context: None,
        });

        let value = eval_term(&expr).expect("dict union should evaluate");
        assert_eq!(
            value.get_key_path(&[key]),
            Some(&Value::binary_from_text("Hello"))
        );
    }

    #[test]
    fn dictionary_unions_defer_ambiguous_keys_until_observed() {
        let key = Key::atom_from_text("greeting");
        let expr = Term::Expr(Expr::DictUnion {
            items: Arc::from([
                Arc::new(Expr::Value(Value::Dict(
                    crate::core::Dict::new_sync()
                        .insert(key.clone(), Value::binary_from_text("Hello")),
                ))),
                Arc::new(Expr::Value(Value::Dict(
                    crate::core::Dict::new_sync()
                        .insert(key.clone(), Value::binary_from_text("World")),
                ))),
            ]),
            key_context: None,
        });

        let value = eval_term(&expr).expect("outer dict union should stay evaluable");
        let ambiguous = value
            .get_key_path(&[key])
            .expect("ambiguous key should exist");
        let Value::Expr(ambiguous) = ambiguous else {
            panic!("ambiguous duplicate should stay as a stuck expression");
        };

        let err = eval_value(&Value::Expr(ambiguous.clone()), None)
            .expect_err("ambiguous key should fail only when demanded");

        assert_eq!(
            err.to_string(),
            "dictionary union is ambiguous at key `greeting`"
        );
    }

    #[test]
    fn names_can_traverse_dictionary_union_bindings() {
        let d = Key::atom_from_text("d");
        let hello = Key::atom_from_text("hello");

        let root = crate::core::Dict::new_sync().insert(
            d.clone(),
            Value::Expr(Arc::new(Expr::DictUnion {
                items: Arc::from([Arc::new(Expr::Value(Value::Dict(
                    crate::core::Dict::new_sync()
                        .insert(hello.clone(), Value::binary_from_text("Hello")),
                )))]),
                key_context: None,
            })),
        );

        let value =
            eval_term(&Term::Expr(Expr::Value(Value::Dict(root)))).expect("root should evaluate");
        let resolved = eval_value(
            &Value::Expr(Arc::new(Expr::Name(Arc::from([
                KeyExpr::Key(d),
                KeyExpr::Key(hello),
            ])))),
            Some(&value),
        )
        .expect("dotted name should force intermediate dict unions");

        assert_eq!(resolved, Value::binary_from_text("Hello"));
    }

    #[test]
    fn names_can_expand_list_valued_path_segments() {
        let foo = Key::atom_from_text("foo");
        let one = Key::Number(1);
        let two = Key::Number(2);
        let three = Key::Number(3);

        let nested = Value::Dict(
            crate::core::Dict::new_sync().insert(
                one.clone(),
                Value::Dict(
                    crate::core::Dict::new_sync().insert(
                        two.clone(),
                        Value::Dict(
                            crate::core::Dict::new_sync()
                                .insert(three.clone(), Value::binary_from_text("World")),
                        ),
                    ),
                ),
            ),
        );

        let root = crate::core::Dict::new_sync().insert(foo.clone(), nested);
        let value =
            eval_term(&Term::Expr(Expr::Value(Value::Dict(root)))).expect("root should evaluate");
        let resolved = eval_value(
            &Value::Expr(Arc::new(Expr::Name(Arc::from([
                KeyExpr::Key(foo),
                KeyExpr::ListExpr(Arc::new(Expr::Apply(
                    Arc::new(Expr::Apply(
                        Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                        Arc::new(Expr::List(Arc::from([
                            Arc::new(Expr::Value(Value::Number(1))),
                            Arc::new(Expr::Value(Value::Number(2))),
                        ]))),
                    )),
                    Arc::new(Expr::List(Arc::from([Arc::new(Expr::Value(
                        Value::Number(3),
                    ))]))),
                ))),
            ])))),
            Some(&value),
        )
        .expect("list-valued path segment should expand into multiple lookups");

        assert_eq!(resolved, Value::binary_from_text("World"));
    }

    #[test]
    fn builtins_are_curried_and_do_not_force_arguments_early() {
        let partial = eval_term(&Term::Expr(Expr::Apply(
            Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
            Arc::new(Expr::Name(Arc::from([KeyExpr::Key(Key::atom_from_text(
                "missing",
            ))]))),
        )))
        .expect("partial builtin application should not force its first argument");

        match partial {
            Value::Expr(expr) => {
                let Some((builtin, args)) = builtin_application_spine(expr.as_ref()) else {
                    panic!("expected builtin application spine");
                };
                assert_eq!(builtin, Builtin::Append);
                assert_eq!(args.len(), 1);
                assert!(matches!(&args[0], Value::Expr(_)));
            }
            other => panic!("expected partial builtin, got {other:?}"),
        }
    }
}
