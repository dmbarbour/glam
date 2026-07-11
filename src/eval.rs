use std::fmt;
use std::sync::Arc;

use crate::core::{Builtin, Closure, Expr, IVar, Key, KeyExpr, List, Thunk, Value};
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

pub fn eval_closed_expr(expr: &Expr) -> Result<Value, EvalError> {
    eval_expr(expr, &[])
}

fn eval_expr(expr: &Expr, local_env: &[Value]) -> Result<Value, EvalError> {
    match expr {
        Expr::Value(value) => eval_value(value),
        Expr::List(items) => {
            let mut list = List::empty();
            for item in items.iter() {
                let value = eval_expr(item, local_env)?;
                list = List::concat(list, list_literal_segment(value));
            }
            Ok(Value::List(list))
        }
        Expr::Apply(function, argument) => eval_apply(function, argument, local_env),
        Expr::Lambda(body) => Ok(Value::Closure(Closure {
            body: body.clone(),
            env: Arc::from(local_env.to_vec()),
        })),
        Expr::Local(index) => eval_local(*index, local_env),
        Expr::Access(base, path) => {
            let base = eval_expr(base, local_env)?;
            resolve_key_path(base, path, path, local_env)
        }
        // TODO: Future should lock down and wait for the value to be initialized, rather than
        // returning an error. At least once we start using parallel evaluation, this will be
        // necessary
        Expr::Future(ivar) => ivar
            .get()
            .cloned()
            .ok_or_else(|| EvalError::new("future was observed before initialization")),
        Expr::Error(message) => Err(EvalError::new(message.as_ref())),
    }
}

pub fn eval_value(value: &Value) -> Result<Value, EvalError> {
    match value {
        Value::Expr(thunk) => eval_expr(thunk.expr.as_ref(), &thunk.env),
        other => Ok(other.clone()),
    }
}

pub fn eval_key(value: &Value) -> Result<Key, EvalError> {
    let value = eval_value(value)?;
    value_to_key(&value, &[])
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
        Key::AbstractGlobalPath(parts) => parts.join("."),
        Key::Atom(atom) => match atom.key() {
            Key::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            Key::AbstractGlobalPath(parts) => parts.join("."),
            other => format!("{other:?}"),
        },
        other => format!("{other:?}"),
    }
}

fn format_name_key_expr(key: &KeyExpr) -> String {
    match key {
        KeyExpr::Key(key) => format_name_part(key),
        KeyExpr::Index(_) => "[index]".to_owned(),
        KeyExpr::PathIndex(_) => "(path-index)".to_owned(),
    }
}

fn eval_local(index: usize, local_env: &[Value]) -> Result<Value, EvalError> {
    let Some(value) = local_env.get(
        local_env
            .len()
            .checked_sub(index + 1)
            .ok_or_else(|| EvalError::new(format!("local `{index}` is out of scope")))?,
    ) else {
        return Err(EvalError::new(format!("local `{index}` is out of scope")));
    };

    eval_value(value)
}

fn value_to_key(value: &Value, local_env: &[Value]) -> Result<Key, EvalError> {
    match value {
        Value::Atom(atom) => Ok(Key::Atom(*atom)),
        Value::Number(number) => Ok(Key::Number(number.clone())),
        Value::Binary(bytes) => Ok(Key::Binary(bytes.clone())),
        Value::List(list) => Ok(Key::List(list_to_key_items(list, local_env)?)),
        Value::Dict(dict) => Ok(Key::Dict(Arc::from(
            dict.iter()
                .map(|(key, value)| {
                    let value = eval_value(value)?;
                    let value = value_to_key(&value, local_env)?;
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
        Value::Closure(_) => Err(EvalError::new(
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
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let Some((head, rest)) = remaining.split_first() else {
        return eval_value(&current);
    };

    let expanded = expand_key_expr(head, local_env)?;
    let next = resolve_expanded_keys(current, &expanded, full_path, remaining, local_env)?;
    resolve_key_path(next, rest, full_path, local_env)
}

fn resolve_expanded_keys(
    mut current: Value,
    expanded: &[Key],
    full_path: &[KeyExpr],
    remaining: &[KeyExpr],
    local_env: &[Value],
) -> Result<Value, EvalError> {
    for key in expanded {
        let dict = force_dict_shell(&current, local_env, full_path, remaining)?;
        current = dict
            .get(key)
            .cloned()
            .unwrap_or_else(|| Value::Dict(crate::core::Dict::new_sync()));
    }
    Ok(current)
}

fn force_dict_shell(
    value: &Value,
    _local_env: &[Value],
    full_path: &[KeyExpr],
    remaining: &[KeyExpr],
) -> Result<crate::core::Dict, EvalError> {
    match force_value_shell(value)? {
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

fn force_value_shell(value: &Value) -> Result<Value, EvalError> {
    let mut current = eval_value(value)?;
    while let Value::Expr(thunk) = current {
        current = eval_value(&Value::Expr(thunk))?;
    }
    Ok(current)
}

fn eval_apply(function: &Expr, argument: &Expr, local_env: &[Value]) -> Result<Value, EvalError> {
    let function = eval_expr(function, local_env)?;
    let argument = thunk_value(argument, local_env);
    apply_value(function, argument, local_env)
}

fn thunk_value(expr: &Expr, local_env: &[Value]) -> Value {
    match expr {
        Expr::Value(value) => value.clone(),
        _ => Value::Expr(Thunk {
            expr: Arc::new(expr.clone()),
            env: Arc::from(local_env.to_vec()),
        }),
    }
}

fn apply_value(function: Value, argument: Value, local_env: &[Value]) -> Result<Value, EvalError> {
    match function {
        Value::Builtin(builtin) => apply_builtin(builtin, Vec::new(), argument, local_env),
        Value::Closure(closure) => apply_closure(&closure, argument),
        Value::Expr(thunk) => {
            if let Some((builtin, args)) = builtin_application_spine(thunk.expr.as_ref()) {
                apply_builtin(builtin, args, argument, local_env)
            } else {
                Ok(Value::Expr(Thunk {
                    expr: Arc::new(Expr::Apply(
                        thunk.expr.clone(),
                        Arc::new(Expr::Value(argument)),
                    )),
                    env: thunk.env.clone(),
                }))
            }
        }
        _ => Err(EvalError::new("application requires a function value")),
    }
}

fn apply_closure(closure: &Closure, argument: Value) -> Result<Value, EvalError> {
    let mut extended = closure.env.iter().cloned().collect::<Vec<_>>();
    extended.push(argument);
    eval_expr(closure.body.as_ref(), &extended)
}

fn apply_builtin(
    builtin: Builtin,
    mut args: Vec<Value>,
    argument: Value,
    local_env: &[Value],
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
            append_values(force_value_shell(&left)?, force_value_shell(&right)?)
        }
        Builtin::Add => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("add builtin received the wrong number of arguments")
            })?;
            eval_numeric_builtin("add", &left, &right, local_env, Number::add)
        }
        Builtin::Subtract => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("subtract builtin received the wrong number of arguments")
            })?;
            eval_numeric_builtin("subtract", &left, &right, local_env, Number::sub)
        }
        Builtin::Multiply => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("multiply builtin received the wrong number of arguments")
            })?;
            eval_numeric_builtin("multiply", &left, &right, local_env, Number::mul)
        }
        Builtin::Divide => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("divide builtin received the wrong number of arguments")
            })?;
            eval_numeric_divide_builtin(&left, &right, local_env)
        }
        Builtin::Fixpoint => {
            let [function] = <[Value; 1]>::try_from(args).map_err(|_| {
                EvalError::new("fixpoint builtin received the wrong number of arguments")
            })?;
            eval_fixpoint_builtin(&function)
        }
        Builtin::Anno => {
            let [annotation, target] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("anno builtin received the wrong number of arguments")
            })?;
            eval_anno_builtin(&annotation, &target, local_env)
        }
        Builtin::MergeDuplicate => {
            let [name, left, right] = <[Value; 3]>::try_from(args).map_err(|_| {
                EvalError::new("merge duplicate builtin received the wrong number of arguments")
            })?;
            eval_merge_duplicate_builtin(&name, &left, &right, local_env)
        }
        Builtin::UpdateDuplicate => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("update duplicate builtin received the wrong number of arguments")
            })?;
            eval_update_duplicate_builtin(&left, &right, local_env)
        }
        Builtin::Floor => {
            let [value] = <[Value; 1]>::try_from(args).map_err(|_| {
                EvalError::new("floor builtin received the wrong number of arguments")
            })?;
            eval_floor_builtin(&value, local_env)
        }
        Builtin::Mod => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("mod builtin received the wrong number of arguments")
            })?;
            eval_numeric_mod_builtin(&left, &right, local_env)
        }
        Builtin::Slice => {
            let [start, end, value] = <[Value; 3]>::try_from(args).map_err(|_| {
                EvalError::new("slice builtin received the wrong number of arguments")
            })?;
            eval_slice_builtin(&start, &end, &value, local_env)
        }
        Builtin::Map => {
            let [function, value] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("map builtin received the wrong number of arguments")
            })?;
            eval_map_builtin(&function, &value, local_env)
        }
        Builtin::DictSingleton => {
            let [key, value] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("singleton builtin received the wrong number of arguments")
            })?;
            eval_singleton_builtin(&key, &value, local_env)
        }
        Builtin::DictUnion => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("dict union builtin received the wrong number of arguments")
            })?;
            eval_dict_union_builtin(&left, &right, local_env)
        }
        Builtin::DictUpdate => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("dict update builtin received the wrong number of arguments")
            })?;
            eval_dict_update_builtin(&left, &right, local_env)
        }
    }
}

fn eval_numeric_builtin(
    name: &str,
    left: &Value,
    right: &Value,
    local_env: &[Value],
    op: impl Fn(&Number, &Number) -> Number,
) -> Result<Value, EvalError> {
    let left = eval_number(left, local_env, name)?;
    let right = eval_number(right, local_env, name)?;
    Ok(Value::Number(op(&left, &right)))
}

fn eval_numeric_divide_builtin(
    left: &Value,
    right: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let left = eval_number(left, local_env, "divide")?;
    let right = eval_number(right, local_env, "divide")?;
    let Some(result) = left.checked_div(&right) else {
        return Err(EvalError::new("divide builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}

fn eval_floor_builtin(value: &Value, local_env: &[Value]) -> Result<Value, EvalError> {
    Ok(Value::Number(
        eval_number(value, local_env, "floor")?.floor(),
    ))
}

fn eval_numeric_mod_builtin(
    left: &Value,
    right: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let left = eval_number(left, local_env, "mod")?;
    let right = eval_number(right, local_env, "mod")?;
    let Some(result) = left.checked_mod(&right) else {
        return Err(EvalError::new("mod builtin cannot divide by zero"));
    };
    Ok(Value::Number(result))
}

fn eval_slice_builtin(
    start: &Value,
    end: &Value,
    value: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let start = eval_index_number(start, local_env, "slice")?;
    let end = eval_index_number(end, local_env, "slice")?;
    if start > end {
        return Err(EvalError::new(
            "slice builtin requires start to be less than or equal to end",
        ));
    }

    match force_value_shell(value)? {
        Value::Binary(bytes) => {
            if end > bytes.len() {
                return Err(EvalError::new("slice builtin end is out of bounds"));
            }
            Ok(Value::Binary(Arc::from(&bytes[start..end])))
        }
        Value::List(list) => {
            let items = list_to_value_items(&list)?;
            if end > items.len() {
                return Err(EvalError::new("slice builtin end is out of bounds"));
            }
            Ok(Value::List(List::from_values(items[start..end].to_vec())))
        }
        _ => Err(EvalError::new(
            "slice builtin requires a list or binary value",
        )),
    }
}

fn eval_map_builtin(
    function: &Value,
    value: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let function = force_value_shell(function)?;
    let mapped = match force_value_shell(value)? {
        Value::Binary(bytes) => bytes
            .iter()
            .map(|byte| {
                apply_value(
                    function.clone(),
                    Value::Number(Number::from_u8(*byte)),
                    local_env,
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
        Value::List(list) => list_to_value_items(&list)?
            .into_iter()
            .map(|item| apply_value(function.clone(), item, local_env))
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(EvalError::new(
                "map builtin requires a list or binary value",
            ));
        }
    };

    Ok(Value::List(List::from_values(mapped)))
}

fn eval_number(
    value: &Value,
    _local_env: &[Value],
    builtin_name: &str,
) -> Result<Number, EvalError> {
    let value = force_value_shell(value)?;
    let Value::Number(number) = value else {
        return Err(EvalError::new(format!(
            "{builtin_name} builtin requires number values"
        )));
    };
    Ok(number)
}

fn eval_index_number(
    value: &Value,
    local_env: &[Value],
    builtin_name: &str,
) -> Result<usize, EvalError> {
    let number = eval_number(value, local_env, builtin_name)?;
    number.to_usize_if_integer().ok_or_else(|| {
        EvalError::new(format!(
            "{builtin_name} builtin requires non-negative integer indices"
        ))
    })
}

fn partial_builtin_value(builtin: Builtin, args: &[Value]) -> Value {
    let expr = args.iter().cloned().fold(
        Expr::Value(Value::Builtin(builtin)),
        |function, argument| Expr::Apply(Arc::new(function), Arc::new(Expr::Value(argument))),
    );
    Value::Expr(Thunk {
        expr: Arc::new(expr),
        env: Arc::from([]),
    })
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
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let key = eval_value(key)?;
    let key = value_to_key(&key, local_env)?;
    if matches!(value, Value::Dict(dict) if dict.is_empty()) {
        return Ok(Value::Dict(crate::core::Dict::new_sync()));
    }

    Ok(Value::Dict(
        crate::core::Dict::new_sync().insert(key, value.clone()),
    ))
}

fn eval_dict_union_builtin(
    left: &Value,
    right: &Value,
    _local_env: &[Value],
) -> Result<Value, EvalError> {
    let left = eval_value(left)?;
    let right = eval_value(right)?;
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

fn eval_dict_update_builtin(
    left: &Value,
    right: &Value,
    _local_env: &[Value],
) -> Result<Value, EvalError> {
    let left = eval_value(left)?;
    let right = eval_value(right)?;
    let Value::Dict(left_dict) = left else {
        return Err(EvalError::new(
            "dictionary update requires dictionary values",
        ));
    };
    let Value::Dict(right_dict) = right else {
        return Err(EvalError::new(
            "dictionary update requires dictionary values",
        ));
    };

    Ok(Value::Dict(update_dicts(&left_dict, &right_dict)))
}

fn eval_fixpoint_builtin(function: &Value) -> Result<Value, EvalError> {
    let function = eval_value(function)?;
    let Value::Closure(function) = function else {
        return Err(EvalError::new("fixpoint builtin requires a lambda value"));
    };

    let handle = IVar::new();
    let marker = Value::expr(Expr::Future(handle.clone()));
    let value = apply_closure(&function, marker.clone())?;
    handle
        .set(value.clone())
        .map_err(|_| EvalError::new("fixpoint builtin initialized twice"))?;
    Ok(value)
}

fn eval_anno_builtin(
    annotation: &Value,
    target: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    match recognize_annotation(annotation, local_env)? {
        RecognizedAnnotation::AssertDefined { name, defined } => {
            if defined {
                Ok(target.clone())
            } else {
                Ok(annotation_error_value(format!(
                    "cannot override `{name}` because it is not defined"
                )))
            }
        }
        RecognizedAnnotation::AssertUndefined { name, defined } => {
            if defined {
                Ok(annotation_error_value(format!(
                    "cannot introduce `{name}` because it is already defined"
                )))
            } else {
                Ok(target.clone())
            }
        }
        RecognizedAnnotation::Invalid(message) => Ok(annotation_error_value(message)),
        RecognizedAnnotation::Unknown(rendered) => {
            eprintln!("warning: unrecognized annotation encountered: {rendered}");
            Ok(target.clone())
        }
    }
}

enum RecognizedAnnotation {
    AssertDefined { name: String, defined: bool },
    AssertUndefined { name: String, defined: bool },
    Invalid(String),
    Unknown(String),
}

fn recognize_annotation(
    annotation: &Value,
    local_env: &[Value],
) -> Result<RecognizedAnnotation, EvalError> {
    let annotation = eval_value(annotation)?;
    let Value::Dict(annotation) = annotation else {
        return Ok(RecognizedAnnotation::Unknown(format!("{annotation:?}")));
    };

    let Some((tag, payload)) = annotation.iter().next() else {
        return Ok(RecognizedAnnotation::Unknown(format!("{annotation:?}")));
    };
    if annotation.iter().nth(1).is_some() {
        return Ok(RecognizedAnnotation::Unknown(format!("{annotation:?}")));
    }

    match tag {
        Key::Atom(atom) if atom_name(atom) == Some("assert_defined") => Ok(
            match parse_assertion_annotation(payload, local_env, "assert_defined")? {
                ParsedAssertion::Valid { name, defined } => {
                    RecognizedAnnotation::AssertDefined { name, defined }
                }
                ParsedAssertion::Invalid(message) => RecognizedAnnotation::Invalid(message),
            },
        ),
        Key::Atom(atom) if atom_name(atom) == Some("assert_undefined") => Ok(
            match parse_assertion_annotation(payload, local_env, "assert_undefined")? {
                ParsedAssertion::Valid { name, defined } => {
                    RecognizedAnnotation::AssertUndefined { name, defined }
                }
                ParsedAssertion::Invalid(message) => RecognizedAnnotation::Invalid(message),
            },
        ),
        _ => Ok(RecognizedAnnotation::Unknown(format!("{annotation:?}"))),
    }
}

enum ParsedAssertion {
    Valid { name: String, defined: bool },
    Invalid(String),
}

fn parse_assertion_annotation(
    payload: &Value,
    local_env: &[Value],
    tag_name: &str,
) -> Result<ParsedAssertion, EvalError> {
    let payload = eval_value(payload)?;
    let Value::Dict(payload) = payload else {
        return Ok(ParsedAssertion::Invalid(format!(
            "invalid `{tag_name}` annotation payload"
        )));
    };

    let Some(name_value) = payload.get(&Key::atom_from_text("name")) else {
        return Ok(ParsedAssertion::Invalid(format!(
            "invalid `{tag_name}` annotation payload"
        )));
    };
    let Some(value) = payload.get(&Key::atom_from_text("value")) else {
        return Ok(ParsedAssertion::Invalid(format!(
            "invalid `{tag_name}` annotation payload"
        )));
    };

    let name = annotation_name(name_value, local_env)?;
    let defined = !is_undefined_value(&eval_value(value)?);
    Ok(ParsedAssertion::Valid { name, defined })
}

fn annotation_name(value: &Value, _local_env: &[Value]) -> Result<String, EvalError> {
    let value = eval_value(value)?;
    Ok(match value {
        Value::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Value::Atom(atom) => atom_name(&atom)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{atom:?}")),
        Value::Number(number) => number.to_string(),
        other => format!("{other:?}"),
    })
}

fn atom_name(atom: &crate::core::Atom) -> Option<&str> {
    match atom.key() {
        Key::Binary(bytes) => std::str::from_utf8(bytes).ok(),
        _ => None,
    }
}

fn is_undefined_value(value: &Value) -> bool {
    matches!(value, Value::Dict(dict) if dict.is_empty())
}

fn annotation_error_value(message: impl Into<String>) -> Value {
    Value::Expr(Thunk {
        expr: Arc::new(Expr::Error(Arc::from(message.into()))),
        env: Arc::from([]),
    })
}

fn eval_merge_duplicate_builtin(
    name: &Value,
    left: &Value,
    right: &Value,
    _local_env: &[Value],
) -> Result<Value, EvalError> {
    let name = eval_value(name)?;
    let name = match name {
        Value::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Value::Atom(atom) => atom_name(&atom)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{atom:?}")),
        other => format!("{other:?}"),
    };
    let left = eval_value(left)?;
    let right = eval_value(right)?;

    if is_undefined_value(&left) {
        return Ok(right);
    }
    if is_undefined_value(&right) {
        return Ok(left);
    }
    if is_error_expr_value(&left) {
        return Ok(left);
    }
    if is_error_expr_value(&right) {
        return Ok(right);
    }

    match (&left, &right) {
        (Value::Dict(left_dict), Value::Dict(right_dict)) => {
            Ok(Value::Dict(merge_dicts(left_dict, right_dict)))
        }
        _ => Ok(annotation_error_value(format!(
            "dictionary union is ambiguous at key `{name}`"
        ))),
    }
}

fn eval_update_duplicate_builtin(
    left: &Value,
    right: &Value,
    _local_env: &[Value],
) -> Result<Value, EvalError> {
    let left = eval_value(left)?;
    let right = eval_value(right)?;

    if is_undefined_value(&right) {
        return Ok(right);
    }
    if is_undefined_value(&left) {
        return Ok(right);
    }

    match (&left, &right) {
        (Value::Dict(left_dict), Value::Dict(right_dict)) => {
            Ok(Value::Dict(update_dicts(left_dict, right_dict)))
        }
        _ => Ok(right),
    }
}

fn merge_dicts(left: &crate::core::Dict, right: &crate::core::Dict) -> crate::core::Dict {
    let (mut merged, updates) = if left.size() >= right.size() {
        (left.clone(), right)
    } else {
        (right.clone(), left)
    };

    for (key, value) in updates.iter() {
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

fn update_dicts(left: &crate::core::Dict, right: &crate::core::Dict) -> crate::core::Dict {
    let mut updated = left.clone();

    for (key, value) in right.iter() {
        let next_value = match updated.get(key) {
            Some(existing) => Some(update_duplicate_dict_value(existing, value)),
            _ if is_undefined_dict_value(value) => None,
            _ => Some(value.clone()),
        };
        updated = match next_value {
            Some(value) if is_undefined_dict_value(&value) => updated.remove(key),
            Some(value) => updated.insert(key.clone(), value),
            None => updated.remove(key),
        };
    }

    updated
}

fn update_duplicate_dict_value(left: &Value, right: &Value) -> Value {
    match (left, right) {
        (Value::Dict(left_dict), Value::Dict(right_dict)) => {
            Value::Dict(update_dicts(left_dict, right_dict))
        }
        _ if is_expr_value(left) || is_expr_value(right) => {
            builtin_apply2_value(Builtin::UpdateDuplicate, left, right)
        }
        _ => right.clone(),
    }
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
        builtin_apply3_value(
            Builtin::MergeDuplicate,
            &Value::binary_from_text(&format_name_part(key)),
            left,
            right,
        )
    } else {
        Value::Expr(Thunk {
            expr: Arc::new(Expr::Error(Arc::from(format!(
                "dictionary union is ambiguous at key `{}`",
                format_name_part(key)
            )))),
            env: Arc::from([]),
        })
    }
}

fn value_as_expr(value: &Value) -> Arc<Expr> {
    Arc::new(Expr::Value(value.clone()))
}

fn builtin_apply3_value(builtin: Builtin, first: &Value, second: &Value, third: &Value) -> Value {
    Value::Expr(Thunk {
        expr: Arc::new(Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Apply(
                    Arc::new(Expr::Value(Value::Builtin(builtin))),
                    value_as_expr(first),
                )),
                value_as_expr(second),
            )),
            value_as_expr(third),
        )),
        env: Arc::from([]),
    })
}

fn builtin_apply2_value(builtin: Builtin, left: &Value, right: &Value) -> Value {
    Value::Expr(Thunk {
        expr: Arc::new(Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Value(Value::Builtin(builtin))),
                value_as_expr(left),
            )),
            value_as_expr(right),
        )),
        env: Arc::from([]),
    })
}

fn is_expr_value(value: &Value) -> bool {
    matches!(value, Value::Expr(_))
}

fn is_error_expr_value(value: &Value) -> bool {
    matches!(value, Value::Expr(thunk) if matches!(thunk.expr.as_ref(), Expr::Error(_)))
}

fn is_undefined_dict_value(value: &Value) -> bool {
    is_undefined_value(value)
}

fn expand_key_expr(key: &KeyExpr, local_env: &[Value]) -> Result<Vec<Key>, EvalError> {
    match key {
        KeyExpr::Key(key) => Ok(vec![key.clone()]),
        KeyExpr::Index(expr) => {
            let value = Value::Expr(Thunk {
                expr: expr.clone(),
                env: Arc::from(local_env.to_vec()),
            });
            let value = eval_value(&value)?;
            Ok(vec![value_to_key(&value, local_env)?])
        }
        KeyExpr::PathIndex(expr) => eval_key_path_list(
            &Value::Expr(Thunk {
                expr: expr.clone(),
                env: Arc::from(local_env.to_vec()),
            }),
            local_env,
        ),
    }
}

fn eval_key_path_list(value: &Value, local_env: &[Value]) -> Result<Vec<Key>, EvalError> {
    let value = eval_value(value)?;
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
                let value = eval_value(value)?;
                items.borrow_mut().push(value_to_key(&value, local_env)?);
            }
            Ok(())
        },
    )?;
    Ok(items.into_inner())
}

fn list_to_key_items(list: &List, local_env: &[Value]) -> Result<Arc<[Key]>, EvalError> {
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
                let value = eval_value(value)?;
                items.borrow_mut().push(value_to_key(&value, local_env)?);
            }
            Ok(())
        },
    )?;
    Ok(Arc::from(items.into_inner()))
}

fn list_to_value_items(list: &List) -> Result<Vec<Value>, EvalError> {
    let items = std::cell::RefCell::new(Vec::new());
    list.for_each_segment(
        &mut |bytes| {
            items.borrow_mut().extend(
                bytes
                    .iter()
                    .map(|byte| Value::Number(Number::from_u8(*byte))),
            );
            Ok::<_, EvalError>(())
        },
        &mut |values| {
            items.borrow_mut().extend(values.iter().cloned());
            Ok(())
        },
    )?;
    Ok(items.into_inner())
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

    use crate::core::{Dict, Expr, IVar, Thunk, Value};
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

    fn builtin1_expr(builtin: Builtin, value: Expr) -> Expr {
        Expr::Apply(
            Arc::new(Expr::Value(Value::Builtin(builtin))),
            Arc::new(value),
        )
    }

    fn builtin3_expr(builtin: Builtin, first: Expr, second: Expr, third: Expr) -> Expr {
        Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Apply(
                    Arc::new(Expr::Value(Value::Builtin(builtin))),
                    Arc::new(first),
                )),
                Arc::new(second),
            )),
            Arc::new(third),
        )
    }

    fn singleton_expr(key: Value, value: Expr) -> Expr {
        builtin2_expr(Builtin::DictSingleton, Expr::Value(key), value)
    }

    fn dict_union_expr(left: Expr, right: Expr) -> Expr {
        builtin2_expr(Builtin::DictUnion, left, right)
    }

    fn dict_update_expr(left: Expr, right: Expr) -> Expr {
        builtin2_expr(Builtin::DictUpdate, left, right)
    }

    fn global_access(path: Vec<KeyExpr>) -> Expr {
        Expr::Access(Arc::new(Expr::Local(0)), Arc::from(path))
    }

    fn key_value(key: &Key) -> Value {
        match key {
            Key::Atom(atom) => Value::Atom(*atom),
            Key::Number(number) => Value::Number(number.clone()),
            Key::Binary(bytes) => Value::Binary(bytes.clone()),
            Key::AbstractGlobalPath(parts) => Value::Atom(crate::core::Atom::from_key(
                &Key::AbstractGlobalPath(parts.clone()),
            )),
            Key::List(items) => {
                Value::List(List::from_values(items.iter().map(key_value).collect()))
            }
            Key::Dict(entries) => Value::Dict(
                entries
                    .iter()
                    .fold(crate::core::Dict::new_sync(), |dict, (key, value)| {
                        dict.insert(key.clone(), key_value(value))
                    }),
            ),
        }
    }

    fn module_value_expr(value: &Value) -> Expr {
        match value {
            Value::Dict(dict) => {
                let mut items = dict.iter();
                let Some((first_key, first_value)) = items.next() else {
                    return Expr::Value(Value::Dict(crate::core::Dict::new_sync()));
                };

                let mut expr = singleton_expr(key_value(first_key), module_value_expr(first_value));
                for (key, value) in items {
                    expr = dict_union_expr(
                        expr,
                        singleton_expr(key_value(key), module_value_expr(value)),
                    );
                }
                expr
            }
            Value::Expr(thunk) if thunk.env.is_empty() => thunk.expr.as_ref().clone(),
            _ => Expr::Value(value.clone()),
        }
    }

    fn fixpoint_dict(dict: Dict) -> Expr {
        Expr::Apply(
            Arc::new(Expr::Value(Value::Builtin(Builtin::Fixpoint))),
            Arc::new(Expr::Lambda(Arc::new(module_value_expr(&Value::Dict(
                dict,
            ))))),
        )
    }

    fn rooted_expr_value(root: &Value, expr: Expr) -> Value {
        let handle = IVar::new();
        handle
            .set(root.clone())
            .expect("rooted test expression should initialize handle once");
        Value::Expr(Thunk {
            expr: Arc::new(expr),
            env: Arc::from([Value::expr(Expr::Future(handle))]),
        })
    }

    #[test]
    fn evaluates_dictionary_terms_to_values() {
        let asm = Dict::new_sync().insert(
            crate::core::Key::atom_from_text("result"),
            Value::binary_from_text("Hello, World!"),
        );
        let root =
            Dict::new_sync().insert(crate::core::Key::atom_from_text("asm"), Value::Dict(asm));

        let value = eval_closed_expr(&fixpoint_dict(root)).expect("term should evaluate");
        let asm = value
            .get_atom_path(&[crate::core::Atom::from_key(
                &crate::core::Key::binary_from_text("asm"),
            )])
            .expect("asm should exist");
        let asm = eval_value(asm).expect("asm binding should evaluate lazily to a dictionary");
        let Value::Dict(asm) = asm else {
            panic!("asm should evaluate to a dictionary");
        };

        assert!(matches!(value, Value::Dict(_)));
        assert_eq!(
            asm.get(&crate::core::Key::atom_from_text("result")),
            Some(&Value::binary_from_text("Hello, World!"))
        );
    }

    #[test]
    fn evaluates_binary_literals() {
        let value = eval_closed_expr(&Expr::Value(Value::binary_from_text("oops")))
            .expect("binary literal should evaluate");

        assert_eq!(value, Value::binary_from_text("oops"));
    }

    #[test]
    fn appends_lists() {
        let expr = Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                Arc::new(Expr::Value(Value::List(List::from_values(vec![
                    n(1),
                    n(2),
                ])))),
            )),
            Arc::new(Expr::Value(Value::List(List::from_values(vec![n(3)])))),
        );

        let value = eval_closed_expr(&expr).expect("append should evaluate");

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
        let expr = Expr::List(Arc::from([
            Arc::new(Expr::Value(n(1))),
            Arc::new(Expr::Value(Value::binary_from_text("Hi"))),
            Arc::new(Expr::Apply(
                Arc::new(Expr::Apply(
                    Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                    Arc::new(Expr::Value(Value::List(List::from_values(vec![n(2)])))),
                )),
                Arc::new(Expr::Value(Value::binary_from_text("!"))),
            )),
        ]));

        let value = eval_closed_expr(&expr).expect("list should evaluate");

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
        let expr = Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                Arc::new(Expr::Value(Value::List(List::from_values(vec![
                    n(72),
                    n(105),
                ])))),
            )),
            Arc::new(Expr::Value(Value::binary_from_text("!"))),
        );

        let value = eval_closed_expr(&expr).expect("append should evaluate");

        assert!(matches!(value, Value::List(_)));
    }

    #[test]
    fn evaluates_arithmetic_builtins() {
        let expr = builtin2_expr(
            Builtin::Subtract,
            builtin2_expr(
                Builtin::Add,
                Expr::Value(n(1)),
                builtin2_expr(Builtin::Multiply, Expr::Value(n(2)), Expr::Value(n(3))),
            ),
            builtin2_expr(Builtin::Divide, Expr::Value(n(4)), Expr::Value(n(5))),
        );

        let value = eval_closed_expr(&expr).expect("arithmetic should evaluate");

        assert_eq!(value, Value::Number(Number::parse("31/5").unwrap()));
    }

    #[test]
    fn evaluates_extended_math_builtins() {
        let floor = eval_closed_expr(&builtin1_expr(
            Builtin::Floor,
            Expr::Value(Value::Number(Number::parse("_7/2").unwrap())),
        ))
        .expect("floor should evaluate");
        let modulus = eval_closed_expr(&builtin2_expr(
            Builtin::Mod,
            Expr::Value(Value::Number(Number::parse("17/5").unwrap())),
            Expr::Value(Value::Number(Number::parse("3/2").unwrap())),
        ))
        .expect("mod should evaluate");

        assert_eq!(floor, Value::Number((-4).into()));
        assert_eq!(modulus, Value::Number(Number::parse("2/5").unwrap()));
    }

    #[test]
    fn evaluates_slice_and_map_builtins() {
        let slice = eval_closed_expr(&builtin3_expr(
            Builtin::Slice,
            Expr::Value(n(1)),
            Expr::Value(n(4)),
            Expr::Value(Value::binary_from_text("World!")),
        ))
        .expect("slice should evaluate");
        let mapped = eval_closed_expr(&builtin2_expr(
            Builtin::Map,
            Expr::Lambda(Arc::new(Expr::Apply(
                Arc::new(Expr::Apply(
                    Arc::new(Expr::Value(Value::Builtin(Builtin::Add))),
                    Arc::new(Expr::Local(0)),
                )),
                Arc::new(Expr::Value(n(1))),
            ))),
            Expr::Value(Value::List(List::from_values(vec![n(1), n(2), n(3)]))),
        ))
        .expect("map should evaluate");

        assert_eq!(slice, Value::binary_from_text("orl"));
        let Value::List(mapped) = mapped else {
            panic!("map should produce a list");
        };
        let items = list_to_value_items(&mapped).expect("mapped list should be readable");
        assert_eq!(items, vec![n(2), n(3), n(4)]);
    }

    #[test]
    fn evaluates_lambda_application_lazily() {
        let expr = Expr::Apply(
            Arc::new(Expr::Lambda(Arc::new(Expr::Local(0)))),
            Arc::new(builtin2_expr(
                Builtin::Add,
                Expr::Value(n(1)),
                Expr::Value(n(2)),
            )),
        );

        let value = eval_closed_expr(&expr).expect("lambda application should evaluate");

        assert_eq!(value, n(3));
    }

    #[test]
    fn closures_capture_outer_locals() {
        let expr = Expr::Apply(
            Arc::new(Expr::Lambda(Arc::new(Expr::Apply(
                Arc::new(Expr::Lambda(Arc::new(Expr::Apply(
                    Arc::new(Expr::Local(0)),
                    Arc::new(Expr::Value(n(0))),
                )))),
                Arc::new(Expr::Lambda(Arc::new(Expr::Local(1)))),
            )))),
            Arc::new(Expr::Value(n(42))),
        );

        let value = eval_closed_expr(&expr).expect("nested closures should evaluate");

        assert_eq!(value, n(42));
    }

    #[test]
    fn dropped_arguments_do_not_prevent_later_locals_from_resolving() {
        let expr = Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Lambda(Arc::new(Expr::Lambda(Arc::new(Expr::Local(
                    0,
                )))))),
                Arc::new(Expr::Value(n(1))),
            )),
            Arc::new(Expr::Value(n(42))),
        );

        let value = eval_closed_expr(&expr).expect("lambda with dropped argument should evaluate");

        assert_eq!(value, n(42));
    }

    #[test]
    fn local_dictionary_paths_resolve_without_a_global_root() {
        let dict = Value::Dict(Dict::new_sync().insert(
            Key::atom_from_text("tail"),
            Value::binary_from_text("World"),
        ));
        let expr = Expr::Apply(
            Arc::new(Expr::Lambda(Arc::new(Expr::Access(
                Arc::new(Expr::Local(0)),
                Arc::from([KeyExpr::Key(Key::atom_from_text("tail"))]),
            )))),
            Arc::new(Expr::Value(dict)),
        );

        let value = eval_closed_expr(&expr).expect("local dictionary path should evaluate");

        assert_eq!(value, Value::binary_from_text("World"));
    }

    #[test]
    fn divide_builtin_rejects_zero() {
        let expr = builtin2_expr(Builtin::Divide, Expr::Value(n(1)), Expr::Value(n(0)));
        let err = eval_closed_expr(&expr).expect_err("division by zero should fail");
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
                    Value::expr(Expr::Apply(
                        Arc::new(Expr::Apply(
                            Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                            Arc::new(Expr::Apply(
                                Arc::new(Expr::Apply(
                                    Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                                    Arc::new(Expr::Apply(
                                        Arc::new(Expr::Apply(
                                            Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                                            Arc::new(global_access(vec![KeyExpr::Key(
                                                hello.clone(),
                                            )])),
                                        )),
                                        Arc::new(Expr::Value(Value::binary_from_text(", "))),
                                    )),
                                )),
                                Arc::new(global_access(vec![KeyExpr::Key(world.clone())])),
                            )),
                        )),
                        Arc::new(Expr::Value(Value::binary_from_text("!"))),
                    )),
                )),
            )
            .insert(hello, Value::binary_from_text("Hello"))
            .insert(world, Value::binary_from_text("World"));

        let value = eval_closed_expr(&fixpoint_dict(root)).expect("term should evaluate");
        let asm_value = value.get_atom_path(&[asm]).expect("asm should exist");
        let asm_value = eval_value(asm_value).expect("asm binding should evaluate");
        let Value::Dict(asm_value) = asm_value else {
            panic!("asm should evaluate to a dictionary");
        };
        let result_value = asm_value
            .get(&crate::core::Key::Atom(result))
            .expect("result should exist");
        let Value::Expr(thunk) = result_value else {
            panic!("resolved result should stay lazy until demanded");
        };
        let resolved = eval_value(&Value::Expr(thunk.clone()))
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
        let key = eval_key(&Value::List(List::concat(
            List::from_values(vec![n(1)]),
            List::from_bytes(Arc::from(&b"Hi"[..])),
        )))
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
        let key = eval_key(&Value::expr(Expr::Value(n(1))))
            .expect("expressions should be allowed when they evaluate to keyable values");

        assert_eq!(key, k(1));
    }

    #[test]
    fn dictionaries_remain_lazy_under_eval_value() {
        let value = Value::Dict(crate::core::Dict::new_sync().insert(
            Key::atom_from_text("answer"),
            Value::expr(Expr::Value(n(42))),
        ));

        let evaluated = eval_value(&value).expect("dict should stay lazy");

        assert_eq!(evaluated, value);
    }

    #[test]
    fn rejects_unevaluable_keys() {
        let root = Value::Dict(crate::core::Dict::new_sync());
        let key = eval_key(&rooted_expr_value(
            &root,
            global_access(vec![KeyExpr::Key(Key::atom_from_text("missing"))]),
        ))
        .expect("missing names should now resolve to empty dictionaries");

        assert_eq!(key, Key::Dict(Arc::from([])));
    }

    #[test]
    fn raw_value_to_key_rejects_expressions() {
        assert_eq!(Key::from_value(&Value::expr(Expr::Value(n(1)))), None);
    }

    #[test]
    fn eval_key_forces_nested_dictionary_values() {
        let key = eval_key(&Value::Dict(crate::core::Dict::new_sync().insert(
            Key::atom_from_text("answer"),
            Value::expr(Expr::Value(n(42))),
        )))
        .expect("dict key should force nested values");

        assert_eq!(
            key,
            Key::Dict(Arc::from([(Key::atom_from_text("answer"), k(42),)]))
        );
    }

    #[test]
    fn eval_key_elides_empty_dictionary_values_from_dict_keys() {
        let empty = eval_key(&Value::Dict(crate::core::Dict::new_sync()))
            .expect("empty dict should be keyable");
        let with_empty_field = eval_key(&Value::Dict(crate::core::Dict::new_sync().insert(
            Key::atom_from_text("key"),
            Value::Dict(crate::core::Dict::new_sync()),
        )))
        .expect("dict with empty field should be keyable");

        assert_eq!(empty, Key::Dict(Arc::from([])));
        assert_eq!(with_empty_field, Key::Dict(Arc::from([])));
    }

    #[test]
    fn singleton_dict_filters_empty_dictionary_values() {
        let value = eval_closed_expr(&singleton_expr(
            Value::Atom(crate::core::Atom::from_key(
                &crate::core::Key::binary_from_text("gone"),
            )),
            Expr::Value(Value::Dict(crate::core::Dict::new_sync())),
        ))
        .expect("singleton dict should evaluate");

        assert_eq!(value, Value::Dict(crate::core::Dict::new_sync()));
    }

    #[test]
    fn dictionary_unions_merge_nested_dictionaries_transitively() {
        let key = Key::atom_from_text("greeting");
        let hello = Key::atom_from_text("hello");
        let world = Key::atom_from_text("world");

        let expr = dict_union_expr(
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
        );

        let value = eval_closed_expr(&expr).expect("dict union should evaluate");
        let greeting = value.get_key_path(&[key]).expect("greeting should exist");
        let Value::Expr(greeting) = greeting else {
            panic!("greeting should stay lazy until demanded");
        };
        let greeting = eval_value(&Value::Expr(greeting.clone()))
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
        let expr = dict_union_expr(
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
        );

        let value = eval_closed_expr(&expr).expect("dict union should evaluate");
        assert_eq!(
            value.get_key_path(&[key]),
            Some(&Value::binary_from_text("Hello"))
        );
    }

    #[test]
    fn dictionary_unions_defer_ambiguous_keys_until_observed() {
        let key = Key::atom_from_text("greeting");
        let expr = dict_union_expr(
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
            )),
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("World")),
            )),
        );

        let value = eval_closed_expr(&expr).expect("outer dict union should stay evaluable");
        let ambiguous = value
            .get_key_path(&[key])
            .expect("ambiguous key should exist");
        let Value::Expr(ambiguous) = ambiguous else {
            panic!("ambiguous duplicate should stay as a stuck expression");
        };

        let err = eval_value(&Value::Expr(ambiguous.clone()))
            .expect_err("ambiguous key should fail only when demanded");

        assert_eq!(
            err.to_string(),
            "dictionary union is ambiguous at key `greeting`"
        );
    }

    #[test]
    fn dictionary_updates_overwrite_duplicate_values() {
        let key = Key::atom_from_text("greeting");
        let expr = dict_update_expr(
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
            )),
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("World")),
            )),
        );

        let value = eval_closed_expr(&expr).expect("dict update should evaluate");

        assert_eq!(
            value.get_key_path(&[key]),
            Some(&Value::binary_from_text("World"))
        );
    }

    #[test]
    fn dictionary_updates_merge_nested_dictionaries_transitively() {
        let key = Key::atom_from_text("greeting");
        let hello = Key::atom_from_text("hello");
        let world = Key::atom_from_text("world");

        let expr = dict_update_expr(
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
        );

        let value = eval_closed_expr(&expr).expect("dict update should evaluate");
        let greeting = value.get_key_path(&[key]).expect("greeting should exist");
        let Value::Dict(greeting) = greeting else {
            panic!("greeting should resolve directly to a dictionary");
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
    fn dictionary_updates_treat_empty_dictionary_values_as_undefined() {
        let key = Key::atom_from_text("greeting");
        let expr = dict_update_expr(
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
            )),
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync()
                    .insert(key.clone(), Value::Dict(crate::core::Dict::new_sync())),
            )),
        );

        let value = eval_closed_expr(&expr).expect("dict update should evaluate");
        assert_eq!(value.get_key_path(&[key]), None);
    }

    #[test]
    fn names_can_traverse_dictionary_union_bindings() {
        let d = Key::atom_from_text("d");
        let hello = Key::atom_from_text("hello");

        let root = crate::core::Dict::new_sync().insert(
            d.clone(),
            Value::expr(dict_union_expr(
                Expr::Value(Value::Dict(
                    crate::core::Dict::new_sync()
                        .insert(hello.clone(), Value::binary_from_text("Hello")),
                )),
                Expr::Value(Value::Dict(crate::core::Dict::new_sync())),
            )),
        );

        let value = eval_closed_expr(&fixpoint_dict(root)).expect("root should evaluate");
        let resolved = eval_value(&rooted_expr_value(
            &value,
            global_access(vec![KeyExpr::Key(d), KeyExpr::Key(hello)]),
        ))
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
        let value = eval_closed_expr(&fixpoint_dict(root)).expect("root should evaluate");
        let resolved = eval_value(&rooted_expr_value(
            &value,
            global_access(vec![
                KeyExpr::Key(foo),
                KeyExpr::PathIndex(Arc::new(Expr::Apply(
                    Arc::new(Expr::Apply(
                        Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                        Arc::new(Expr::List(Arc::from([
                            Arc::new(Expr::Value(n(1))),
                            Arc::new(Expr::Value(n(2))),
                        ]))),
                    )),
                    Arc::new(Expr::List(Arc::from([Arc::new(Expr::Value(n(3)))]))),
                ))),
            ]),
        ))
        .expect("list-valued path segment should expand into multiple lookups");

        assert_eq!(resolved, Value::binary_from_text("World"));
    }

    #[test]
    fn missing_dictionary_members_resolve_to_empty_dictionary() {
        let root = Value::Dict(crate::core::Dict::new_sync().insert(
            Key::atom_from_text("present"),
            Value::Dict(crate::core::Dict::new_sync()),
        ));
        let resolved = eval_value(&rooted_expr_value(
            &root,
            global_access(vec![
                KeyExpr::Key(Key::atom_from_text("present")),
                KeyExpr::Key(Key::atom_from_text("missing")),
            ]),
        ))
        .expect("missing member access should stay evaluable");

        assert_eq!(resolved, Value::Dict(crate::core::Dict::new_sync()));
    }

    #[test]
    fn anno_builtin_preserves_lazy_targets_when_assertions_pass() {
        let root =
            Value::Dict(crate::core::Dict::new_sync().insert(Key::atom_from_text("later"), n(42)));
        let annotation = singleton_expr(
            Value::Atom(crate::core::Atom::from_key(
                &crate::core::Key::binary_from_text("assert_undefined"),
            )),
            dict_union_expr(
                singleton_expr(
                    Value::Atom(crate::core::Atom::from_key(
                        &crate::core::Key::binary_from_text("name"),
                    )),
                    Expr::Value(Value::binary_from_text("missing")),
                ),
                singleton_expr(
                    Value::Atom(crate::core::Atom::from_key(
                        &crate::core::Key::binary_from_text("value"),
                    )),
                    global_access(vec![KeyExpr::Key(Key::atom_from_text("missing"))]),
                ),
            ),
        );

        let value = eval_value(&rooted_expr_value(
            &root,
            Expr::Apply(
                Arc::new(Expr::Apply(
                    Arc::new(Expr::Value(Value::Builtin(Builtin::Anno))),
                    Arc::new(annotation),
                )),
                Arc::new(global_access(vec![KeyExpr::Key(Key::atom_from_text(
                    "later",
                ))])),
            ),
        ))
        .expect("anno should pass through successful assertions");

        let Value::Expr(thunk) = value else {
            panic!("anno should preserve lazy target evaluation");
        };
        let resolved =
            eval_value(&Value::Expr(thunk)).expect("returned target should still evaluate");
        assert_eq!(resolved, n(42));
    }

    #[test]
    fn anno_builtin_returns_stuck_errors_for_failed_assertions() {
        let annotation = singleton_expr(
            Value::Atom(crate::core::Atom::from_key(
                &crate::core::Key::binary_from_text("assert_defined"),
            )),
            dict_union_expr(
                singleton_expr(
                    Value::Atom(crate::core::Atom::from_key(
                        &crate::core::Key::binary_from_text("name"),
                    )),
                    Expr::Value(Value::binary_from_text("foo")),
                ),
                singleton_expr(
                    Value::Atom(crate::core::Atom::from_key(
                        &crate::core::Key::binary_from_text("value"),
                    )),
                    global_access(vec![KeyExpr::Key(Key::atom_from_text("foo"))]),
                ),
            ),
        );

        let value = eval_value(&rooted_expr_value(
            &Value::Dict(crate::core::Dict::new_sync()),
            Expr::Apply(
                Arc::new(Expr::Apply(
                    Arc::new(Expr::Value(Value::Builtin(Builtin::Anno))),
                    Arc::new(annotation),
                )),
                Arc::new(Expr::Value(n(1))),
            ),
        ))
        .expect("failed anno should still produce a stuck value");

        let Value::Expr(thunk) = value else {
            panic!("failed anno should produce a stuck expression");
        };
        let err = eval_value(&Value::Expr(thunk)).expect_err("failed anno should raise on demand");
        assert_eq!(
            err.to_string(),
            "cannot override `foo` because it is not defined"
        );
    }

    #[test]
    fn unknown_annotations_pass_through_targets() {
        let value = eval_closed_expr(&Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Value(Value::Builtin(Builtin::Anno))),
                Arc::new(singleton_expr(
                    Value::Atom(crate::core::Atom::from_key(
                        &crate::core::Key::binary_from_text("mystery"),
                    )),
                    Expr::Value(n(0)),
                )),
            )),
            Arc::new(Expr::Value(n(42))),
        ))
        .expect("unknown annotations should pass through");

        assert_eq!(value, n(42));
    }

    #[test]
    fn builtins_are_curried_and_do_not_force_arguments_early() {
        let partial = eval_closed_expr(&Expr::Apply(
            Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
            Arc::new(global_access(vec![KeyExpr::Key(Key::atom_from_text(
                "missing",
            ))])),
        ))
        .expect("partial builtin application should not force its first argument");

        match partial {
            Value::Expr(thunk) => {
                let Some((builtin, args)) = builtin_application_spine(thunk.expr.as_ref()) else {
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
