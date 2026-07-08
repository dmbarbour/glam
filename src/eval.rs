use std::fmt;
use std::sync::Arc;

use crate::core::{Expr, Key, List, Term, Value};

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
        Expr::Append(left, right) => {
            append_values(eval_expr(left, root_env)?, eval_expr(right, root_env)?)
        }
        Expr::Name(path) => resolve_name(path, root_env),
    }
}

pub fn eval_value(value: &Value, root_env: Option<&Value>) -> Result<Value, EvalError> {
    match value {
        Value::Number(number) => Ok(Value::Number(*number)),
        Value::Binary(bytes) => Ok(Value::Binary(bytes.clone())),
        Value::List(list) => Ok(Value::List(list.clone())),
        Value::Dict(dict) => Ok(Value::Dict(dict.clone())),
        Value::Expr(expr) => eval_expr(expr, root_env),
    }
}

pub fn eval_key(value: &Value, root_env: Option<&Value>) -> Result<Key, EvalError> {
    let value = eval_value(value, root_env)?;
    value_to_key(&value, root_env)
}

fn resolve_name(path: &[crate::core::Key], root_env: Option<&Value>) -> Result<Value, EvalError> {
    let Some(root_env) = root_env else {
        return Err(EvalError::new("name resolution requires a dictionary root"));
    };

    let value = root_env
        .get_key_path(path)
        .ok_or_else(|| EvalError::new(format!("name `{}` is not defined", format_name(path))))?;
    eval_value(value, Some(root_env))
}

fn format_name(path: &[crate::core::Key]) -> String {
    path.iter()
        .map(format_name_part)
        .collect::<Vec<_>>()
        .join(".")
}

fn format_name_part(key: &crate::core::Key) -> String {
    match key {
        crate::core::Key::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        crate::core::Key::Atom(atom) => match atom.key() {
            crate::core::Key::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            other => format!("{other:?}"),
        },
        other => format!("{other:?}"),
    }
}

fn value_to_key(value: &Value, root_env: Option<&Value>) -> Result<Key, EvalError> {
    match value {
        Value::Number(number) => Ok(Key::Number(*number)),
        Value::Binary(bytes) => Ok(Key::Binary(bytes.clone())),
        Value::List(list) => Ok(Key::List(list_to_key_items(list, root_env)?)),
        Value::Dict(dict) => Ok(Key::Dict(Arc::from(
            dict.iter()
                .map(|(key, value)| Ok((key.clone(), eval_key(value, root_env)?)))
                .collect::<Result<Vec<_>, EvalError>>()?,
        ))),
        Value::Expr(_) => Err(EvalError::new(
            "dictionary keys must evaluate to keyable values",
        )),
    }
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
        let expr = Term::Expr(Expr::Append(
            Arc::new(Expr::Value(Value::List(List::from_values(vec![
                Value::Number(1),
                Value::Number(2),
            ])))),
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
            Arc::new(Expr::Append(
                Arc::new(Expr::Value(Value::List(List::from_values(vec![
                    Value::Number(2),
                ])))),
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
        let expr = Term::Expr(Expr::Append(
            Arc::new(Expr::Value(Value::List(List::from_values(vec![
                Value::Number(72),
                Value::Number(105),
            ])))),
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
                    Value::Expr(Arc::new(Expr::Append(
                        Arc::new(Expr::Append(
                            Arc::new(Expr::Name(Arc::from([hello.clone()]))),
                            Arc::new(Expr::Value(Value::binary_from_text(", "))),
                        )),
                        Arc::new(Expr::Append(
                            Arc::new(Expr::Name(Arc::from([world.clone()]))),
                            Arc::new(Expr::Value(Value::binary_from_text("!"))),
                        )),
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
            &Value::Expr(Arc::new(Expr::Name(Arc::from([Key::atom_from_text(
                "missing",
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
}
