use std::fmt;
use std::sync::Arc;

use crate::core::{Key, LazyValue, List, Value, keys};
use crate::list::ListItem;
use crate::number::Number;

#[cfg(test)]
use crate::core::FunctionCode;

#[cfg(test)]
use super::application::*;
use super::builtins::{apply_builtin, is_undefined_value};
use super::net::*;
use super::sequence::list_to_key_items;
#[cfg(test)]
use super::sequence::{eval_key_path_list, list_literal_segment};
#[cfg(test)]
use super::test_support::{eval_apply, thunk_value};

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TestExpr {
    Value(Value),
    List(Arc<[Arc<TestExpr>]>),
    Apply(Arc<TestExpr>, Arc<TestExpr>),
    Function {
        code: Arc<FunctionCode>,
        captures: Arc<[Arc<TestExpr>]>,
    },
    Local(usize),
    Access(Arc<TestExpr>, Arc<[TestKey]>),
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum TestKey {
    Key(Key),
    Index(Arc<TestExpr>),
    PathIndex(Arc<TestExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalError {
    message: String,
}

impl EvalError {
    pub(super) fn new(message: impl Into<String>) -> Self {
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

#[cfg(test)]
pub(super) fn eval_closed_expr(expr: &TestExpr) -> Result<Value, EvalError> {
    eval_expr(expr, &[])
}

#[cfg(test)]
pub(super) fn eval_expr(expr: &TestExpr, local_env: &[Value]) -> Result<Value, EvalError> {
    match expr {
        TestExpr::Value(value) => eval_value(value),
        TestExpr::List(items) => {
            let mut list = List::empty();
            for item in items.iter() {
                let value = eval_expr(item, local_env)?;
                list = List::concat(list, list_literal_segment(value));
            }
            Ok(Value::List(list))
        }
        TestExpr::Apply(function, argument) => eval_apply(function, argument, local_env),
        TestExpr::Function { code, captures } => {
            let captures = captures
                .iter()
                .map(|capture| thunk_value(capture, local_env))
                .collect();
            instantiate_function(code, captures)
        }
        TestExpr::Local(index) => eval_local(*index, local_env),
        TestExpr::Access(base, path) => {
            let base = eval_expr(base, local_env)?;
            resolve_key_path(base, path, path, local_env)
        }
    }
}

pub fn eval_value(value: &Value) -> Result<Value, EvalError> {
    match value {
        Value::Lazy(lazy) => eval_lazy(lazy),
        Value::Net(net) => observe_net(net.clone()),
        other => Ok(other.clone()),
    }
}

pub(super) fn eval_lazy(lazy: &LazyValue) -> Result<Value, EvalError> {
    let net_computation = lazy.net_computation();
    let function_call = lazy.function_call();
    let continue_through_result = net_computation.is_some() || function_call.is_some();
    if let Some(result) = lazy.cached() {
        let result = result.map_err(|message| EvalError::new(message.as_ref()))?;
        return if continue_through_result {
            eval_value(&result)
        } else {
            Ok(result)
        };
    }
    let result = if let Some(result) = lazy.force_deferred() {
        result.map_err(|message| EvalError::new(message.as_ref()))
    } else if let Some((path, arguments)) = lazy.access() {
        resolve_core_access(arguments, path)
    } else if let Some(call) = lazy.builtin() {
        let mut arguments = call.arguments.iter().cloned().collect::<Vec<_>>();
        let argument = arguments
            .pop()
            .expect("saturated builtin thunk must contain an argument");
        apply_builtin(call.builtin, arguments, argument, &[])
    } else if let Some((function, arguments)) = function_call.as_ref() {
        evaluate_function_call(function, arguments)
    } else if let Some(net) = net_computation.as_ref() {
        let runtime = net.runtime().clone();
        let exposed = runtime.with(|runtime| runtime.exposed());
        extract_net_data(runtime, exposed, "lazy net computation")
    } else if lazy.is_pending() {
        // TODO(parallel evaluation): an unfulfilled lazy value currently
        // fails fast. Parallel evaluation needs a thunk-level scheduler,
        // including explicit sparks and suspended continuations, rather
        // than a blocking IVar join that can deadlock on cyclic demand.
        return Err(EvalError::new(
            "lazy value was observed before initialization",
        ));
    } else {
        return Err(EvalError::new("lazy value has no producer"));
    }
    .map_err(|error| Arc::<str>::from(error.to_string()));
    let result = lazy
        .cache(result)
        .map_err(|message| EvalError::new(message.as_ref()))?;
    if continue_through_result {
        // A net computation itself has exactly one meaning: extract the
        // exposed Data payload and cache it. Once that has happened, the
        // surrounding computation (including an ordinary function-call
        // stage) may perform the next lazy step without re-entering the
        // source runtime.
        eval_value(&result)
    } else {
        Ok(result)
    }
}

#[cfg(test)]
pub(super) fn eval_key(value: &Value) -> Result<Key, EvalError> {
    let value = force_value_shell(value)?;
    value_to_key(&value, &[])
}

#[cfg(test)]
pub(super) fn format_name(path: &[TestKey]) -> String {
    path.iter()
        .map(format_name_key_expr)
        .collect::<Vec<_>>()
        .join(".")
}

pub(super) fn format_name_part(key: &Key) -> String {
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

#[cfg(test)]
pub(super) fn format_name_key_expr(key: &TestKey) -> String {
    match key {
        TestKey::Key(key) => format_name_part(key),
        TestKey::Index(_) => "[index]".to_owned(),
        TestKey::PathIndex(_) => "(path-index)".to_owned(),
    }
}

#[cfg(test)]
pub(super) fn eval_local(index: usize, local_env: &[Value]) -> Result<Value, EvalError> {
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

pub(super) fn value_to_key(value: &Value, local_env: &[Value]) -> Result<Key, EvalError> {
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
        Value::Builtin(_) | Value::PartialBuiltin(_) | Value::Function(_) | Value::Net(_) => Err(
            EvalError::new("dictionary keys must evaluate to keyable values"),
        ),
        Value::Lazy(_) => Err(EvalError::new(
            "dictionary keys must evaluate to keyable values",
        )),
    }
}

#[cfg(test)]
pub(super) fn resolve_key_path(
    current: Value,
    remaining: &[TestKey],
    full_path: &[TestKey],
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let Some((head, rest)) = remaining.split_first() else {
        return eval_value(&current);
    };

    let expanded = expand_key_expr(head, local_env)?;
    let next = resolve_expanded_keys(current, &expanded, full_path, remaining, local_env)?;
    resolve_key_path(next, rest, full_path, local_env)
}

#[cfg(test)]
pub(super) fn resolve_expanded_keys(
    mut current: Value,
    expanded: &[Key],
    full_path: &[TestKey],
    remaining: &[TestKey],
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

#[cfg(test)]
pub(super) fn force_dict_shell(
    value: &Value,
    _local_env: &[Value],
    full_path: &[TestKey],
    remaining: &[TestKey],
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

pub(super) fn force_value_shell(value: &Value) -> Result<Value, EvalError> {
    let mut current = eval_value(value)?;
    while matches!(current, Value::Lazy(_)) {
        current = eval_value(&current)?;
    }
    Ok(current)
}

pub(super) fn force_list_thunk(thunk: &LazyValue) -> Result<List, EvalError> {
    match force_value_shell(&Value::Lazy(thunk.clone()))? {
        Value::Binary(bytes) => Ok(List::from_bytes(bytes)),
        Value::List(list) => Ok(list),
        other => Err(EvalError::new(format!(
            "lazy list chunk must evaluate to a list or binary value, got {other:?}"
        ))),
    }
}

pub(super) fn pop_list_front(list: &List) -> Result<Option<(Value, List)>, EvalError> {
    Ok(list
        .try_pop_front(&mut force_list_thunk)?
        .map(|(item, tail)| {
            let value = match item {
                ListItem::Byte(byte) => Value::Number(Number::from_u8(byte)),
                ListItem::Value(value) => value,
            };
            (value, tail)
        }))
}

pub(super) fn split_result_value(left: Value, right: Value) -> Value {
    Value::Dict(
        crate::core::Dict::new_sync()
            .insert((*keys::LEFT).clone(), left)
            .insert((*keys::RIGHT).clone(), right),
    )
}

pub(super) fn eval_number(
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

pub(super) fn eval_index_number(
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

pub(super) fn is_lazy_value(value: &Value) -> bool {
    matches!(value, Value::Lazy(_))
}

pub(super) fn is_error_lazy_value(value: &Value) -> bool {
    matches!(value, Value::Lazy(lazy) if lazy.cached().is_some_and(|result| result.is_err()))
}

pub(super) fn is_undefined_dict_value(value: &Value) -> bool {
    is_undefined_value(value)
}

#[cfg(test)]
pub(super) fn expand_key_expr(key: &TestKey, local_env: &[Value]) -> Result<Vec<Key>, EvalError> {
    match key {
        TestKey::Key(key) => Ok(vec![key.clone()]),
        TestKey::Index(expr) => {
            let value = thunk_value(expr, local_env);
            let value = force_value_shell(&value)?;
            Ok(vec![value_to_key(&value, local_env)?])
        }
        TestKey::PathIndex(expr) => eval_key_path_list(&thunk_value(expr, local_env), local_env),
    }
}
