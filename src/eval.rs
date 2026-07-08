use std::fmt;

use crate::core::{Expr, List, Term, Value};

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
        Term::Expr(expr) => eval_expr(expr),
    }
}

fn eval_expr(expr: &Expr) -> Result<Value, EvalError> {
    match expr {
        Expr::Value(value) => eval_value(value),
        Expr::List(items) => {
            let mut list = List::empty();
            for item in items.iter() {
                let value = eval_expr(item)?;
                list = List::concat(list, list_literal_segment(value));
            }
            Ok(Value::List(list))
        }
        Expr::Append(left, right) => append_values(eval_expr(left)?, eval_expr(right)?),
    }
}

pub fn eval_value(value: &Value) -> Result<Value, EvalError> {
    match value {
        Value::Number(number) => Ok(Value::Number(*number)),
        Value::Binary(bytes) => Ok(Value::Binary(bytes.clone())),
        Value::List(list) => Ok(Value::List(list.clone())),
        Value::Dict(dict) => dict
            .iter()
            .map(|(key, value)| Ok((key.clone(), eval_value(value)?)))
            .collect::<Result<_, EvalError>>()
            .map(Value::Dict),
        Value::Expr(expr) => eval_expr(expr),
    }
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
                    crate::core::Atom::from_key(&crate::core::Key::text("asm")),
                    crate::core::Atom::from_key(&crate::core::Key::text("result")),
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
}
