use std::fmt;
use std::sync::Arc;

use crate::core::{Builtin, Expr, Key, KeyExpr, List, Term, Value};
use crate::number::Number;

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
        Expr::Error(message) => Err(EvalError::new(message.as_ref())),
    }
}

pub fn eval_value(value: &Value, root_env: Option<&Value>) -> Result<Value, EvalError> {
    match value {
        Value::Atom(atom) => Ok(Value::Atom(*atom)),
        Value::Number(number) => Ok(Value::Number(number.clone())),
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
        Value::Atom(atom) => Ok(Key::Atom(*atom)),
        Value::Number(number) => Ok(Key::Number(number.clone())),
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
        Builtin::Add => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("add builtin received the wrong number of arguments")
            })?;
            eval_numeric_builtin("add", &left, &right, root_env, Number::add)
        }
        Builtin::Subtract => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("subtract builtin received the wrong number of arguments")
            })?;
            eval_numeric_builtin("subtract", &left, &right, root_env, Number::sub)
        }
        Builtin::Multiply => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("multiply builtin received the wrong number of arguments")
            })?;
            eval_numeric_builtin("multiply", &left, &right, root_env, Number::mul)
        }
        Builtin::Divide => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("divide builtin received the wrong number of arguments")
            })?;
            eval_numeric_divide_builtin(&left, &right, root_env)
        }
        Builtin::Singleton => {
            let [key, value] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("singleton builtin received the wrong number of arguments")
            })?;
            eval_singleton_builtin(&key, &value, root_env)
        }
        Builtin::DictUnion => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("dict union builtin received the wrong number of arguments")
            })?;
            eval_dict_union_builtin(&left, &right, root_env)
        }
    }
}

fn eval_numeric_builtin(
    name: &str,
    left: &Value,
    right: &Value,
    root_env: Option<&Value>,
    op: impl Fn(&Number, &Number) -> Number,
) -> Result<Value, EvalError> {
    let left = eval_number(left, root_env, name)?;
    let right = eval_number(right, root_env, name)?;
    Ok(Value::Number(op(&left, &right)))
}

fn eval_numeric_divide_builtin(
    left: &Value,
    right: &Value,
    root_env: Option<&Value>,
) -> Result<Value, EvalError> {
    let left = eval_number(left, root_env, "divide")?;
    let right = eval_number(right, root_env, "divide")?;
    let Some(result) = left.checked_div(&right) else {
        return Err(EvalError::new("divide builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}

fn eval_number(
    value: &Value,
    root_env: Option<&Value>,
    builtin_name: &str,
) -> Result<Number, EvalError> {
    let value = eval_value(value, root_env)?;
    let Value::Number(number) = value else {
        return Err(EvalError::new(format!(
            "{builtin_name} builtin requires number values"
        )));
    };
    Ok(number)
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

fn eval_singleton_builtin(
    key: &Value,
    value: &Value,
    root_env: Option<&Value>,
) -> Result<Value, EvalError> {
    let key = eval_key(key, root_env)?;
    let observed = eval_value(value, root_env)?;
    if is_undefined_dict_value(&observed) {
        return Ok(Value::Dict(crate::core::Dict::new_sync()));
    }

    Ok(Value::Dict(
        crate::core::Dict::new_sync().insert(key, value.clone()),
    ))
}

fn eval_dict_union_builtin(
    left: &Value,
    right: &Value,
    root_env: Option<&Value>,
) -> Result<Value, EvalError> {
    let left = eval_value(left, root_env)?;
    let right = eval_value(right, root_env)?;
    let Value::Dict(left_dict) = left else {
        return Err(EvalError::new(
            "dictionary union requires dictionary values",
        ));
    };
    let Value::Dict(right_dict) = right else {
        return Err(EvalError::new(
            "dictionary union requires dictionary values",
        ));
    };

    Ok(Value::Dict(merge_dicts(&left_dict, &right_dict)))
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
    if is_undefined_dict_value(left) {
        right.clone()
    } else if is_undefined_dict_value(right) {
        left.clone()
    } else if matches!((left, right), (Value::Dict(_), Value::Dict(_)))
        || is_expr_value(left)
        || is_expr_value(right)
    {
        // Defer when both sides are concrete dictionaries, or when either side
        // is still an unevaluated expression that may become one.
        builtin_apply2_value(Builtin::DictUnion, left, right)
    } else {
        Value::Expr(Arc::new(Expr::Error(Arc::from(format!(
            "dictionary union is ambiguous at key `{}`",
            format_name_part(key)
        )))))
    }
}

fn value_as_expr(value: &Value) -> Arc<Expr> {
    match value {
        Value::Expr(expr) => expr.clone(),
        _ => Arc::new(Expr::Value(value.clone())),
    }
}

fn builtin_apply2_value(builtin: Builtin, left: &Value, right: &Value) -> Value {
    Value::Expr(Arc::new(Expr::Apply(
        Arc::new(Expr::Apply(
            Arc::new(Expr::Value(Value::Builtin(builtin))),
            value_as_expr(left),
        )),
        value_as_expr(right),
    )))
}

fn is_expr_value(value: &Value) -> bool {
    matches!(value, Value::Expr(_))
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
                .extend(bytes.iter().map(|byte| Key::Number(Number::from_u8(*byte))));
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
                .extend(bytes.iter().map(|byte| Key::Number(Number::from_u8(*byte))));
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
    use crate::number::Number;

    use super::*;

    fn n(value: i64) -> Value {
        Value::Number(value.into())
    }

    fn k(value: i64) -> Key {
        Key::Number(value.into())
    }

    fn builtin2_expr(builtin: Builtin, left: Expr, right: Expr) -> Expr {
        Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Value(Value::Builtin(builtin))),
                Arc::new(left),
            )),
            Arc::new(right),
        )
    }

    fn singleton_expr(key: Value, value: Expr) -> Expr {
        builtin2_expr(Builtin::Singleton, Expr::Value(key), value)
    }

    fn dict_union_expr(left: Expr, right: Expr) -> Expr {
        builtin2_expr(Builtin::DictUnion, left, right)
    }

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
                    n(1),
                    n(2),
                ])))),
            )),
            Arc::new(Expr::Value(Value::List(List::from_values(vec![n(3)])))),
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
        assert_eq!(values, vec![n(1), n(2), n(3)]);
    }

    #[test]
    fn evaluates_mixed_list_segments() {
        let expr = Term::Expr(Expr::List(Arc::from([
            Arc::new(Expr::Value(n(1))),
            Arc::new(Expr::Value(Value::binary_from_text("Hi"))),
            Arc::new(Expr::Apply(
                Arc::new(Expr::Apply(
                    Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                    Arc::new(Expr::Value(Value::List(List::from_values(vec![n(2)])))),
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

        assert_eq!(saw_values, vec![vec![n(1)], vec![n(2)]]);
        assert_eq!(saw_bytes, vec![b"Hi".to_vec(), b"!".to_vec()]);
    }

    #[test]
    fn appends_list_and_binary() {
        let expr = Term::Expr(Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                Arc::new(Expr::Value(Value::List(List::from_values(vec![
                    n(72),
                    n(105),
                ])))),
            )),
            Arc::new(Expr::Value(Value::binary_from_text("!"))),
        ));

        let value = eval_term(&expr).expect("append should evaluate");

        assert!(matches!(value, Value::List(_)));
    }

    #[test]
    fn evaluates_arithmetic_builtins() {
        let expr = Term::Expr(builtin2_expr(
            Builtin::Subtract,
            builtin2_expr(
                Builtin::Add,
                Expr::Value(n(1)),
                builtin2_expr(Builtin::Multiply, Expr::Value(n(2)), Expr::Value(n(3))),
            ),
            builtin2_expr(Builtin::Divide, Expr::Value(n(4)), Expr::Value(n(5))),
        ));

        let value = eval_term(&expr).expect("arithmetic should evaluate");

        assert_eq!(value, Value::Number(Number::parse("31/5").unwrap()));
    }

    #[test]
    fn divide_builtin_rejects_zero() {
        let expr = Term::Expr(builtin2_expr(
            Builtin::Divide,
            Expr::Value(n(1)),
            Expr::Value(n(0)),
        ));

        let err = eval_term(&expr).expect_err("division by zero should fail");

        assert_eq!(err.to_string(), "divide builtin cannot divide by zero");
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
                    bytes.borrow_mut().push(
                        number
                            .to_u8_if_integer()
                            .expect("byte-oriented result should contain byte integers"),
                    );
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
                List::from_values(vec![n(1)]),
                List::from_bytes(Arc::from(&b"Hi"[..])),
            )),
            None,
        )
        .expect("list should evaluate to a key");

        assert_eq!(
            key,
            Key::List(Arc::from([
                k(1),
                Key::Number(Number::from_u8(b'H')),
                Key::Number(Number::from_u8(b'i')),
            ]))
        );
    }

    #[test]
    fn evaluates_expressions_before_key_validation() {
        let key = eval_key(&Value::Expr(Arc::new(Expr::Value(n(1)))), None)
            .expect("expressions should be allowed when they evaluate to keyable values");

        assert_eq!(key, k(1));
    }

    #[test]
    fn dictionaries_remain_lazy_under_eval_value() {
        let value = Value::Dict(crate::core::Dict::new_sync().insert(
            Key::atom_from_text("answer"),
            Value::Expr(Arc::new(Expr::Value(n(42)))),
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
            Key::from_value(&Value::Expr(Arc::new(Expr::Value(n(1))))),
            None
        );
    }

    #[test]
    fn eval_key_forces_nested_dictionary_values() {
        let key = eval_key(
            &Value::Dict(crate::core::Dict::new_sync().insert(
                Key::atom_from_text("answer"),
                Value::Expr(Arc::new(Expr::Value(n(42)))),
            )),
            None,
        )
        .expect("dict key should force nested values");

        assert_eq!(
            key,
            Key::Dict(Arc::from([(Key::atom_from_text("answer"), k(42),)]))
        );
    }

    #[test]
    fn singleton_dict_filters_empty_dictionary_values() {
        let value = eval_term(&Term::Expr(singleton_expr(
            Value::Atom(crate::core::Atom::from_key(
                &crate::core::Key::binary_from_text("gone"),
            )),
            Expr::Value(Value::Dict(crate::core::Dict::new_sync())),
        )))
        .expect("singleton dict should evaluate");

        assert_eq!(value, Value::Dict(crate::core::Dict::new_sync()));
    }

    #[test]
    fn dictionary_unions_merge_nested_dictionaries_transitively() {
        let key = Key::atom_from_text("greeting");
        let hello = Key::atom_from_text("hello");
        let world = Key::atom_from_text("world");

        let expr = Term::Expr(dict_union_expr(
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(
                    key.clone(),
                    Value::Dict(
                        crate::core::Dict::new_sync()
                            .insert(hello.clone(), Value::binary_from_text("Hello")),
                    ),
                ),
            )),
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(
                    key.clone(),
                    Value::Dict(
                        crate::core::Dict::new_sync()
                            .insert(world.clone(), Value::binary_from_text("World")),
                    ),
                ),
            )),
        ));

        let value = eval_term(&expr).expect("dict union should evaluate");
        let greeting = value.get_key_path(&[key]).expect("greeting should exist");
        let Value::Expr(greeting) = greeting else {
            panic!("greeting should stay lazy until demanded");
        };
        let greeting = eval_value(&Value::Expr(greeting.clone()), None)
            .expect("nested dict union should evaluate when demanded");
        let Value::Dict(greeting) = greeting else {
            panic!("greeting should evaluate to a merged dictionary");
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
        let expr = Term::Expr(dict_union_expr(
            singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("greeting"),
                )),
                Expr::Value(Value::binary_from_text("Hello")),
            ),
            singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("greeting"),
                )),
                Expr::Value(Value::Dict(crate::core::Dict::new_sync())),
            ),
        ));

        let value = eval_term(&expr).expect("dict union should evaluate");
        assert_eq!(
            value.get_key_path(&[key]),
            Some(&Value::binary_from_text("Hello"))
        );
    }

    #[test]
    fn dictionary_unions_defer_ambiguous_keys_until_observed() {
        let key = Key::atom_from_text("greeting");
        let expr = Term::Expr(dict_union_expr(
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
            )),
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("World")),
            )),
        ));

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
            Value::Expr(Arc::new(dict_union_expr(
                Expr::Value(Value::Dict(
                    crate::core::Dict::new_sync()
                        .insert(hello.clone(), Value::binary_from_text("Hello")),
                )),
                Expr::Value(Value::Dict(crate::core::Dict::new_sync())),
            ))),
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
        let one = k(1);
        let two = k(2);
        let three = k(3);

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
                            Arc::new(Expr::Value(n(1))),
                            Arc::new(Expr::Value(n(2))),
                        ]))),
                    )),
                    Arc::new(Expr::List(Arc::from([Arc::new(Expr::Value(n(3)))]))),
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
