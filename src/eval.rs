use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use bytes::Bytes;

use crate::core::{
    Builtin, BuiltinCall, Closure, DeferredValue, Expr, IVar, Key, KeyExpr, List, Thunk, Value,
};
use crate::core_net::{CoreDataKey, CoreNetData};
use crate::interaction_net::{BlockedCall, NetBuilder, Port, ReductionKind};
use crate::list::ListItem;
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
        Expr::Lambda(lambda) => Ok(Value::Closure(Closure {
            interaction_net: lambda.runtime_with_captures(Arc::from(local_env.to_vec())),
            env: Arc::from(local_env.to_vec()),
            source_body: lambda.body().clone(),
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
        Expr::Deferred(deferred) => deferred
            .force()
            .map_err(|message| EvalError::new(message.as_ref())),
        Expr::Error(message) => Err(EvalError::new(message.as_ref())),
    }
}

pub fn eval_value(value: &Value) -> Result<Value, EvalError> {
    match value {
        Value::Expr(thunk) => eval_thunk(thunk),
        other => Ok(other.clone()),
    }
}

fn eval_thunk(thunk: &Thunk) -> Result<Value, EvalError> {
    if let Some(result) = thunk.cached() {
        return result.map_err(|message| EvalError::new(message.as_ref()));
    }
    let result = if let Some(net) = thunk.net() {
        drive_core_net(net.runtime.clone(), net.interface)
    } else if let Some((path, arguments)) = thunk.access() {
        resolve_core_access(arguments, path)
    } else if let Some(item) = thunk.list_item() {
        eval_thunk(item).map(|value| Value::List(list_literal_segment(value)))
    } else if let Some(call) = thunk.builtin() {
        let mut arguments = call.arguments.iter().cloned().collect::<Vec<_>>();
        let argument = arguments
            .pop()
            .expect("saturated builtin thunk must contain an argument");
        apply_builtin(call.builtin, arguments, argument, &[])
    } else {
        eval_expr(
            thunk
                .expr()
                .expect("expression thunk must have an expression"),
            thunk
                .env()
                .expect("expression thunk must have an environment"),
        )
    }
    .map_err(|error| Arc::<str>::from(error.to_string()));
    thunk
        .cache(result)
        .map_err(|message| EvalError::new(message.as_ref()))
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
        Value::Builtin(_) | Value::PartialBuiltin(_) => Err(EvalError::new(
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
    while matches!(current, Value::Expr(_)) {
        current = eval_value(&current)?;
    }
    Ok(current)
}

fn force_list_thunk(thunk: &Thunk) -> Result<List, EvalError> {
    match force_value_shell(&Value::Expr(thunk.clone()))? {
        Value::Binary(bytes) => Ok(List::from_bytes(bytes)),
        Value::List(list) => Ok(list),
        other => Err(EvalError::new(format!(
            "lazy list chunk must evaluate to a list or binary value, got {other:?}"
        ))),
    }
}

fn pop_list_front(list: &List) -> Result<Option<(Value, List)>, EvalError> {
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

fn eval_apply(function: &Expr, argument: &Expr, local_env: &[Value]) -> Result<Value, EvalError> {
    let function = eval_expr(function, local_env)?;
    let argument = thunk_value(argument, local_env);
    apply_value(function, argument, local_env)
}

fn thunk_value(expr: &Expr, local_env: &[Value]) -> Value {
    match expr {
        Expr::Value(value) => value.clone(),
        _ => Value::Expr(Thunk::new(
            Arc::new(expr.clone()),
            Arc::from(local_env.to_vec()),
        )),
    }
}

fn apply_value(function: Value, argument: Value, local_env: &[Value]) -> Result<Value, EvalError> {
    match function {
        Value::Builtin(builtin) => apply_builtin(builtin, Vec::new(), argument, local_env),
        Value::PartialBuiltin(call) => apply_builtin(
            call.builtin,
            call.arguments.iter().cloned().collect(),
            argument,
            local_env,
        ),
        Value::Closure(closure) => apply_closure(&closure, argument),
        Value::Dict(dict) => apply_dict_value(&dict, argument, local_env),
        Value::Expr(thunk) => apply_value(eval_thunk(&thunk)?, argument, local_env),
        _ => Err(EvalError::new("application requires a function value")),
    }
}

fn apply_dict_value(
    dict: &crate::core::Dict,
    argument: Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    if let Some(function) = singleton_effect_function(dict) {
        return Ok(effect_value(apply_effect_function_value(
            function, argument,
        )));
    }

    if let Some(function) = dict.get(&Key::atom_from_text("apply")) {
        if !is_undefined_dict_value(function) {
            return apply_value(eval_value(function)?, argument, local_env);
        }
    }

    Err(EvalError::new("application requires a function value"))
}

fn singleton_effect_function(dict: &crate::core::Dict) -> Option<Value> {
    let eff_key = Key::atom_from_text("eff");
    let function = dict_effect_function(dict)?;
    if dict
        .iter()
        .all(|(key, value)| *key == eff_key || is_undefined_dict_value(value))
    {
        Some(function.clone())
    } else {
        None
    }
}

fn dict_effect_function(dict: &crate::core::Dict) -> Option<Value> {
    let function = dict.get(&Key::atom_from_text("eff"))?;
    if is_undefined_dict_value(function) {
        None
    } else {
        Some(function.clone())
    }
}

fn apply_effect_function_value(function: Value, argument: Value) -> Value {
    Value::expr(Expr::lambda(Arc::new(Expr::Apply(
        Arc::new(Expr::Apply(
            Arc::new(Expr::Value(function)),
            Arc::new(Expr::Local(0)),
        )),
        Arc::new(Expr::Value(argument)),
    ))))
}

fn effect_value(function: Value) -> Value {
    Value::Dict(crate::core::Dict::new_sync().insert(Key::atom_from_text("eff"), function))
}

fn apply_closure(closure: &Closure, argument: Value) -> Result<Value, EvalError> {
    if net_evaluable_expr(&closure.source_body) {
        return apply_closure_net(closure, argument);
    }
    let mut extended = closure.env.iter().cloned().collect::<Vec<_>>();
    extended.push(argument);
    eval_expr(&closure.source_body, &extended)
}

fn net_evaluable_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Value(_) | Expr::Local(_) | Expr::Error(_) => true,
        Expr::Apply(function, argument) => {
            net_evaluable_expr(function) && net_evaluable_expr(argument)
        }
        Expr::Lambda(lambda) => net_evaluable_expr(lambda.body()),
        Expr::List(items) => items.iter().all(|item| net_evaluable_expr(item)),
        // A copied access can currently expose a demanded local through a
        // second logical-copy boundary. Until that demand is forwarded to the
        // caller-side frontier, evaluating it would pull on the canonical
        // lambda root, which is deliberately unsupplied.
        Expr::Access(_, _) => false,
        Expr::Deferred(_) | Expr::Future(_) => true,
    }
}

fn apply_closure_net(closure: &Closure, argument: Value) -> Result<Value, EvalError> {
    let mut net = NetBuilder::new();
    let [application, argument_port, result] = net.bind();
    let function = net.data(CoreNetData::Value(Value::Closure(closure.clone())));
    let argument = net.data(CoreNetData::Value(argument));
    net.wire(application, function);
    net.wire(argument_port, argument);
    let runtime = net.finish(result).instantiate_shared();
    let exposed = runtime.with(|net| net.exposed());
    drive_core_net(runtime, exposed)
}

fn drive_core_net(
    runtime: crate::core_net::CoreRuntimeNet,
    interface: Port,
) -> Result<Value, EvalError> {
    loop {
        if runtime.with(|net| net.interface_data(interface).is_some()) {
            let data = runtime
                .with_mut(|net| net.take_interface_data(interface))
                .expect("observed interaction-net interface must contain data");
            return observe_core_data(data);
        }

        if let Some(progress) = runtime.with_mut(|net| net.demand_interface(interface)) {
            if !matches!(progress, crate::interaction_net::CursorProgress::Blocked) {
                continue;
            }
            if let Some(source) = runtime.with(|net| net.interface_cursor_source(interface)) {
                if progress_core_net(&source)? {
                    continue;
                }
            }
        }

        let reduction = runtime.with_mut(|net| net.reduce_next());
        if let Some(reduction) = reduction {
            if matches!(reduction.kind, ReductionKind::Stuck) {
                return Err(EvalError::new(
                    "interaction net reached a stuck active pair",
                ));
            }
            continue;
        }

        if progress_core_net(&runtime)? {
            continue;
        }

        let sources = runtime.with(|net| net.blocked_cursor_sources());
        let mut source_progress = false;
        for source in sources {
            source_progress |= progress_core_net(&source)?;
        }
        if source_progress {
            runtime.with_mut(|net| net.wake_blocked_cursors());
            continue;
        }

        let detail = runtime.with(|net| {
            let neighbor = net.interface_neighbor(interface);
            let node = neighbor.and_then(|port| net.node(port.node()));
            let principal_neighbor = neighbor
                .and_then(|port| net.port_neighbor(Port::principal(port.node())));
            let principal_neighbor_node =
                principal_neighbor.and_then(|port| net.node(port.node()));
            format!(
                "neighbor={neighbor:?}, node={node:?}, principal_neighbor={principal_neighbor:?}/{principal_neighbor_node:?}, calls={}, cursors={}, stuck={}",
                net.blocked_calls().len(),
                net.blocked_cursors().len(),
                net.stuck_pairs().len()
            )
        });
        return Err(EvalError::new(format!(
            "interaction net became quiescent before producing a value ({detail})"
        )));
    }
}

fn progress_core_net(runtime: &crate::core_net::CoreRuntimeNet) -> Result<bool, EvalError> {
    if runtime.with_mut(|net| net.reduce_next()).is_some() {
        return Ok(true);
    }
    let calls = runtime.with(|net| net.blocked_calls().iter().copied().collect::<Vec<_>>());
    for call in calls.iter().copied() {
        let callable_captures_lazy_argument = runtime.with(|net| {
            matches!(
                net.call_data(call),
                CoreNetData::Builtin(_)
                    | CoreNetData::Value(Value::Builtin(_))
                    | CoreNetData::Value(Value::PartialBuiltin(_))
                    | CoreNetData::List { .. }
            )
        });
        let argument_is_data = runtime.with(|net| {
            net.call_argument_data(call).is_some_and(|data| {
                net.has_imported_copy()
                    || !matches!(data, CoreNetData::Capture(_) | CoreNetData::Lambda(_))
            })
        });
        if callable_captures_lazy_argument
            && !argument_is_data
            && runtime.with(|net| net.call_argument_cursor_source(call).is_some())
        {
            if let Some(progress) = runtime.with_mut(|net| net.demand_call_argument(call)) {
                if !matches!(progress, crate::interaction_net::CursorProgress::Blocked) {
                    return Ok(true);
                }
            }
        }
        // A builtin may capture an unforced argument only after the call has
        // crossed into a logical copy. Detaching the same call in the
        // canonical lambda runtime would capture its unsupplied root boundary
        // and make the cached reduction depend on a future caller.
        let call_is_instanced =
            callable_captures_lazy_argument && runtime.with(|net| net.has_imported_copy());
        if argument_is_data || call_is_instanced {
            handle_core_call(runtime, call)?;
            return Ok(true);
        }
    }
    for call in calls {
        if let Some(progress) = runtime.with_mut(|net| net.demand_call_argument(call)) {
            if !matches!(progress, crate::interaction_net::CursorProgress::Blocked) {
                return Ok(true);
            }
            if let Some(source) = runtime.with(|net| net.call_argument_cursor_source(call)) {
                if progress_core_net(&source)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn observe_core_data(data: CoreNetData) -> Result<Value, EvalError> {
    match data {
        CoreNetData::Value(value) => eval_value(&value),
        CoreNetData::Builtin(call) if call.arguments.is_empty() => Ok(Value::Builtin(call.builtin)),
        CoreNetData::Builtin(call) => Ok(Value::PartialBuiltin(call)),
        CoreNetData::Lambda(lambda) => Ok(Value::Closure(Closure {
            interaction_net: lambda.interaction_net(),
            env: Arc::from([]),
            source_body: lambda.body().clone(),
        })),
        CoreNetData::List { arity: 0, .. } => Ok(Value::List(List::empty())),
        CoreNetData::Deferred(value) => value
            .force()
            .map_err(|message| EvalError::new(message.as_ref())),
        CoreNetData::Future(value) => value
            .get()
            .cloned()
            .ok_or_else(|| EvalError::new("future was observed before initialization")),
        CoreNetData::Error(message) => Err(EvalError::new(message.as_ref())),
        CoreNetData::Capture(_) | CoreNetData::List { .. } | CoreNetData::Access { .. } => Err(
            EvalError::new("unsupported core data escaped interaction-net evaluation"),
        ),
    }
}

fn handle_core_call(
    runtime: &crate::core_net::CoreRuntimeNet,
    call: BlockedCall,
) -> Result<(), EvalError> {
    let callable = runtime.with(|net| net.call_data(call).clone());
    match callable {
        CoreNetData::Builtin(builtin) => return handle_core_builtin_call(runtime, call, builtin),
        CoreNetData::Value(Value::Builtin(builtin)) => {
            return handle_core_builtin_call(runtime, call, BuiltinCall::new(builtin));
        }
        CoreNetData::Value(Value::PartialBuiltin(builtin)) => {
            return handle_core_builtin_call(runtime, call, builtin);
        }
        CoreNetData::List { arity, arguments } => {
            return handle_core_list_call(runtime, call, arity, arguments);
        }
        CoreNetData::Access { path, arguments } => {
            return handle_core_access_call(runtime, call, path, arguments);
        }
        _ => {}
    }
    let argument = runtime.with(|net| net.call_argument_data(call).cloned());
    let Some(argument) = argument else {
        return Err(EvalError::new(
            "interaction-net call argument did not reduce to embedded data",
        ));
    };
    let argument = core_data_to_lazy_value(argument)?;

    match callable {
        CoreNetData::Value(Value::Closure(closure)) => {
            let mut environment = closure.env.iter().cloned().collect::<Vec<_>>();
            environment.push(argument);
            let map_data = core_copy_mapper(Arc::from(environment));
            runtime.with_mut(|net| {
                net.resume_call_with_copy_map(call, closure.interaction_net.clone(), map_data);
            });
            Ok(())
        }
        CoreNetData::Builtin(_) => unreachable!(),
        CoreNetData::Value(function) => {
            let value = apply_value(function, argument, &[])?;
            complete_core_call(runtime, call, CoreNetData::Value(value));
            Ok(())
        }
        CoreNetData::Lambda(lambda) => {
            let closure = Closure {
                interaction_net: lambda.interaction_net(),
                env: Arc::from([]),
                source_body: lambda.body().clone(),
            };
            let mut environment = vec![argument];
            let map_data = core_copy_mapper(Arc::from(std::mem::take(&mut environment)));
            runtime.with_mut(|net| {
                net.resume_call_with_copy_map(call, closure.interaction_net, map_data);
            });
            Ok(())
        }
        CoreNetData::Capture(_)
        | CoreNetData::List { .. }
        | CoreNetData::Access { .. }
        | CoreNetData::Deferred(_)
        | CoreNetData::Future(_)
        | CoreNetData::Error(_) => Err(EvalError::new(
            "interaction-net data is not callable in this evaluator slice",
        )),
    }
}

fn handle_core_builtin_call(
    runtime: &crate::core_net::CoreRuntimeNet,
    call: BlockedCall,
    mut builtin: BuiltinCall,
) -> Result<(), EvalError> {
    let (frame, argument) = take_core_call_argument(runtime, call)?;
    let mut arguments = builtin.arguments.iter().cloned().collect::<Vec<_>>();
    arguments.push(argument);
    if arguments.len() < builtin.builtin.arity() {
        builtin.arguments = Arc::from(arguments);
        runtime.with_mut(|net| {
            net.complete_interface_with_data(frame.result, CoreNetData::Builtin(builtin));
        });
        return Ok(());
    }
    builtin.arguments = Arc::from(arguments);
    let value = Value::Expr(Thunk::from_builtin(builtin));
    runtime.with_mut(|net| {
        net.complete_interface_with_data(frame.result, CoreNetData::Value(value));
    });
    Ok(())
}

fn handle_core_list_call(
    runtime: &crate::core_net::CoreRuntimeNet,
    call: BlockedCall,
    arity: usize,
    supplied: Arc<[Value]>,
) -> Result<(), EvalError> {
    let (frame, argument) = take_core_call_argument(runtime, call)?;
    let mut arguments = supplied.iter().cloned().collect::<Vec<_>>();
    arguments.push(argument);
    let result = if arguments.len() < arity {
        CoreNetData::List {
            arity,
            arguments: Arc::from(arguments),
        }
    } else {
        let list = arguments.into_iter().fold(List::empty(), |list, value| {
            let segment = match value {
                Value::Binary(bytes) => List::from_bytes(bytes),
                Value::List(list) => list,
                Value::Expr(thunk) => List::from_thunk(Thunk::from_list_item(thunk)),
                other => Value::singleton_list(other),
            };
            List::concat(list, segment)
        });
        CoreNetData::Value(Value::List(list))
    };
    runtime.with_mut(|net| net.complete_interface_with_data(frame.result, result));
    Ok(())
}

fn handle_core_access_call(
    runtime: &crate::core_net::CoreRuntimeNet,
    call: BlockedCall,
    path: Arc<[crate::core_net::CoreDataKey]>,
    supplied: Arc<[Value]>,
) -> Result<(), EvalError> {
    let (frame, argument) = take_core_call_argument(runtime, call)?;
    let mut arguments = supplied.iter().cloned().collect::<Vec<_>>();
    arguments.push(argument);
    let arity = 1 + path
        .iter()
        .filter(|key| !matches!(key, crate::core_net::CoreDataKey::Key(_)))
        .count();
    let result = if arguments.len() < arity {
        CoreNetData::Access {
            path,
            arguments: Arc::from(arguments),
        }
    } else {
        CoreNetData::Value(Value::Expr(Thunk::from_access(path, Arc::from(arguments))))
    };
    runtime.with_mut(|net| net.complete_interface_with_data(frame.result, result));
    Ok(())
}

fn take_core_call_argument(
    runtime: &crate::core_net::CoreRuntimeNet,
    call: BlockedCall,
) -> Result<(crate::interaction_net::CallFrame<CoreNetData>, Value), EvalError> {
    let embedded_argument = runtime.with(|net| net.call_argument_data(call).cloned());
    let frame = runtime.with_mut(|net| net.take_call(call));
    let argument = if let Some(data) = embedded_argument {
        runtime
            .with_mut(|net| net.take_interface_data(frame.argument))
            .expect("observed core argument must remain embedded data");
        core_data_to_lazy_value(data)?
    } else {
        Value::Expr(Thunk::from_net(runtime.clone(), frame.argument))
    };
    Ok((frame, argument))
}

fn resolve_core_access(arguments: &[Value], path: &[CoreDataKey]) -> Result<Value, EvalError> {
    let mut current = arguments
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("interaction-net access is missing its base value"))?;
    let mut dynamic = arguments[1..].iter();
    for part in path {
        let keys = match part {
            CoreDataKey::Key(key) => vec![key.clone()],
            CoreDataKey::Index => {
                let value = dynamic.next().expect("lowered access index must exist");
                let value = force_value_shell(value)?;
                vec![value_to_key(&value, &[])?]
            }
            CoreDataKey::PathIndex => eval_key_path_list(
                dynamic
                    .next()
                    .expect("lowered access path index must exist"),
                &[],
            )?,
        };
        for key in keys {
            let value = force_value_shell(&current)?;
            let Value::Dict(dict) = value else {
                return Err(EvalError::new(
                    "interaction-net access base is not a dictionary",
                ));
            };
            current = dict
                .get(&key)
                .cloned()
                .unwrap_or_else(|| Value::Dict(crate::core::Dict::new_sync()));
        }
    }
    eval_value(&current)
}

fn core_data_to_lazy_value(data: CoreNetData) -> Result<Value, EvalError> {
    match data {
        CoreNetData::Value(value) => Ok(value),
        CoreNetData::Builtin(call) if call.arguments.is_empty() => Ok(Value::Builtin(call.builtin)),
        CoreNetData::Builtin(call) => Ok(Value::PartialBuiltin(call)),
        CoreNetData::Lambda(lambda) => Ok(Value::Closure(Closure {
            interaction_net: lambda.interaction_net(),
            env: Arc::from([]),
            source_body: lambda.body().clone(),
        })),
        CoreNetData::List {
            arity: 0,
            arguments,
        } if arguments.is_empty() => Ok(Value::List(List::empty())),
        CoreNetData::Deferred(value) => Ok(Value::expr(Expr::Deferred(value))),
        CoreNetData::Future(value) => Ok(Value::expr(Expr::Future(value))),
        CoreNetData::Error(message) => Ok(Value::expr(Expr::Error(message))),
        unsupported @ (CoreNetData::Capture(_)
        | CoreNetData::List { .. }
        | CoreNetData::Access { .. }) => Err(EvalError::new(format!(
            "unsupported core data was used as a lazy argument: {unsupported:?}"
        ))),
    }
}

fn complete_core_call(
    runtime: &crate::core_net::CoreRuntimeNet,
    call: BlockedCall,
    result: CoreNetData,
) {
    runtime.with_mut(|net| {
        let frame = net.take_call(call);
        net.take_interface_data(frame.argument)
            .expect("core call argument must remain embedded data");
        net.complete_interface_with_data(frame.result, result);
    });
}

fn core_copy_mapper(
    environment: Arc<[Value]>,
) -> Arc<dyn Fn(&CoreNetData) -> CoreNetData + Send + Sync> {
    Arc::new(move |data| match data {
        CoreNetData::Lambda(lambda) => CoreNetData::Value(Value::Closure(Closure {
            interaction_net: lambda.runtime_with_captures(environment.clone()),
            env: environment.clone(),
            source_body: lambda.body().clone(),
        })),
        CoreNetData::Capture(index) => {
            let value = environment
                .len()
                .checked_sub(index + 1)
                .and_then(|index| environment.get(index))
                .expect("logical-copy capture must exist");
            CoreNetData::Value(value.clone())
        }
        other => other.clone(),
    })
}

fn apply_builtin(
    builtin: Builtin,
    mut args: Vec<Value>,
    argument: Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    args.push(argument);
    if args.len() < builtin.arity() {
        return Ok(Value::PartialBuiltin(BuiltinCall {
            builtin,
            arguments: Arc::from(args),
        }));
    }

    match builtin {
        Builtin::Append => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("append builtin received the wrong number of arguments")
            })?;
            append_values(left, right)
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
        Builtin::Greater => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("greater-than builtin received the wrong number of arguments")
            })?;
            eval_compare_ordering_builtin("greater-than", &left, &right, local_env, |ordering| {
                ordering == Ordering::Greater
            })
        }
        Builtin::GreaterEqual => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new(
                    "greater-than-or-equal builtin received the wrong number of arguments",
                )
            })?;
            eval_compare_ordering_builtin(
                "greater-than-or-equal",
                &left,
                &right,
                local_env,
                |ordering| ordering != Ordering::Less,
            )
        }
        Builtin::Equal => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("equal builtin received the wrong number of arguments")
            })?;
            eval_compare_equality_builtin("equal", &left, &right, local_env, |equal| equal)
        }
        Builtin::NotEqual => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("not-equal builtin received the wrong number of arguments")
            })?;
            eval_compare_equality_builtin("not-equal", &left, &right, local_env, |equal| !equal)
        }
        Builtin::LessEqual => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("less-than-or-equal builtin received the wrong number of arguments")
            })?;
            eval_compare_ordering_builtin(
                "less-than-or-equal",
                &left,
                &right,
                local_env,
                |ordering| ordering != Ordering::Greater,
            )
        }
        Builtin::Less => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("less-than builtin received the wrong number of arguments")
            })?;
            eval_compare_ordering_builtin("less-than", &left, &right, local_env, |ordering| {
                ordering == Ordering::Less
            })
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
        Builtin::ListLen => {
            let [value] = <[Value; 1]>::try_from(args).map_err(|_| {
                EvalError::new("list len builtin received the wrong number of arguments")
            })?;
            eval_list_len_builtin(&value)
        }
        Builtin::ListSplit => {
            let [index, value] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("list split builtin received the wrong number of arguments")
            })?;
            eval_list_split_builtin(&index, &value, local_env)
        }
        Builtin::ListSplitEnd => {
            let [count, value] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("list split_end builtin received the wrong number of arguments")
            })?;
            eval_list_split_end_builtin(&count, &value, local_env)
        }
        Builtin::ListHead => {
            let [value] = <[Value; 1]>::try_from(args).map_err(|_| {
                EvalError::new("list head builtin received the wrong number of arguments")
            })?;
            eval_list_head_builtin(&value)
        }
        Builtin::ListTail => {
            let [value] = <[Value; 1]>::try_from(args).map_err(|_| {
                EvalError::new("list tail builtin received the wrong number of arguments")
            })?;
            eval_list_tail_builtin(&value)
        }
        Builtin::ListEffect => {
            let [effect] = <[Value; 1]>::try_from(args).map_err(|_| {
                EvalError::new("list effect builtin received the wrong number of arguments")
            })?;
            eval_list_effect_builtin(&effect, local_env)
        }
        Builtin::ListEffectReturn => {
            let [value] = <[Value; 1]>::try_from(args).map_err(|_| {
                EvalError::new("list effect return builtin received the wrong number of arguments")
            })?;
            Ok(Value::List(List::from_values(vec![value])))
        }
        Builtin::ListEffectSeq => {
            let [operation, continuation] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("list effect seq builtin received the wrong number of arguments")
            })?;
            eval_list_effect_seq_builtin(&operation, &continuation, local_env)
        }
        Builtin::ListEffectAlt => {
            let [left, right] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("list effect alt builtin received the wrong number of arguments")
            })?;
            eval_list_effect_alt_builtin(&left, &right, local_env)
        }
        Builtin::ListEffectCut => {
            let [operation] = <[Value; 1]>::try_from(args).map_err(|_| {
                EvalError::new("list effect cut builtin received the wrong number of arguments")
            })?;
            eval_list_effect_cut_builtin(&operation, local_env)
        }
        Builtin::ListEffectFix => {
            let [function] = <[Value; 1]>::try_from(args).map_err(|_| {
                EvalError::new("list effect fix builtin received the wrong number of arguments")
            })?;
            eval_list_effect_fix_builtin(&function, local_env)
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
            let [path, new_value, dict] = <[Value; 3]>::try_from(args).map_err(|_| {
                EvalError::new("dict update builtin received the wrong number of arguments")
            })?;
            eval_dict_update_builtin(&path, &new_value, &dict, local_env)
        }
        Builtin::ObjectSpec => {
            let [value] = <[Value; 1]>::try_from(args).map_err(|_| {
                EvalError::new("object spec builtin received the wrong number of arguments")
            })?;
            eval_object_spec_builtin(&value)
        }
        Builtin::ObjectLocalName => {
            let [host, parts] = <[Value; 2]>::try_from(args).map_err(|_| {
                EvalError::new("object local name builtin received the wrong number of arguments")
            })?;
            eval_object_local_name_builtin(&host, &parts)
        }
        Builtin::ObjectInstanceFromParts => {
            let [name, deps, defs] = <[Value; 3]>::try_from(args).map_err(|_| {
                EvalError::new(
                    "object instance from parts builtin received the wrong number of arguments",
                )
            })?;
            eval_object_instance_from_parts_builtin(name, deps, defs, local_env)
        }
        Builtin::ObjectInstance => {
            let [spec] = <[Value; 1]>::try_from(args).map_err(|_| {
                EvalError::new("object instance builtin received the wrong number of arguments")
            })?;
            eval_object_instance_builtin(&spec, local_env)
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

fn eval_compare_ordering_builtin(
    name: &str,
    left: &Value,
    right: &Value,
    local_env: &[Value],
    predicate: impl FnOnce(Ordering) -> bool,
) -> Result<Value, EvalError> {
    let ordering = compare_ordered_values(left, right, local_env, name)?;
    Ok(condition_effect_value(predicate(ordering)))
}

fn eval_compare_equality_builtin(
    name: &str,
    left: &Value,
    right: &Value,
    local_env: &[Value],
    predicate: impl FnOnce(bool) -> bool,
) -> Result<Value, EvalError> {
    let equal = equal_values(left, right, local_env, name)?;
    Ok(condition_effect_value(predicate(equal)))
}

fn condition_effect_value(success: bool) -> Value {
    if success {
        effect_call_expr_value("r", vec![builtin_unit_value()])
    } else {
        effect_call_expr_value("fail", Vec::new())
    }
}

fn effect_call_expr_value(name: &str, arguments: Vec<Value>) -> Value {
    let api_member = Expr::Access(
        Arc::new(Expr::Local(0)),
        Arc::from([KeyExpr::Key(Key::atom_from_text(name))]),
    );
    let body = arguments
        .into_iter()
        .fold(api_member, |function, argument| {
            Expr::Apply(Arc::new(function), Arc::new(Expr::Value(argument)))
        });
    effect_value(Value::expr(Expr::lambda(Arc::new(body))))
}

fn builtin_unit_value() -> Value {
    Value::Atom(crate::core::Atom::from_key(&Key::abstract_global_path([
        "builtin", "unit",
    ])))
}

fn compare_ordered_values(
    left: &Value,
    right: &Value,
    local_env: &[Value],
    name: &str,
) -> Result<Ordering, EvalError> {
    let left = force_value_shell(left)?;
    let right = force_value_shell(right)?;
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => Ok(left.cmp(&right)),
        (Value::Binary(left), Value::Binary(right)) => Ok(left.cmp(&right)),
        (Value::Binary(left), Value::List(right)) => {
            compare_lists_ordered(List::from_bytes(left), right, local_env, name)
        }
        (Value::List(left), Value::Binary(right)) => {
            compare_lists_ordered(left, List::from_bytes(right), local_env, name)
        }
        (Value::List(left), Value::List(right)) => {
            compare_lists_ordered(left, right, local_env, name)
        }
        (Value::Dict(left), Value::Dict(right)) => {
            let Some(left) = tuple_payload(&left) else {
                return Err(EvalError::new(format!(
                    "{name} builtin can only order dictionaries tagged as `tuple`"
                )));
            };
            let Some(right) = tuple_payload(&right) else {
                return Err(EvalError::new(format!(
                    "{name} builtin can only order dictionaries tagged as `tuple`"
                )));
            };
            let left = list_like_value(left, name)?;
            let right = list_like_value(right, name)?;
            compare_lists_ordered(left, right, local_env, name)
        }
        (Value::Builtin(_), _)
        | (_, Value::Builtin(_))
        | (Value::PartialBuiltin(_), _)
        | (_, Value::PartialBuiltin(_))
        | (Value::Closure(_), _)
        | (_, Value::Closure(_)) => Err(EvalError::new(format!(
            "{name} builtin cannot compare function values"
        ))),
        (left, right) => Err(EvalError::new(format!(
            "{name} builtin cannot order values {left:?} and {right:?}"
        ))),
    }
}

fn compare_lists_ordered(
    mut left: List,
    mut right: List,
    local_env: &[Value],
    name: &str,
) -> Result<Ordering, EvalError> {
    loop {
        match (pop_list_front(&left)?, pop_list_front(&right)?) {
            (None, None) => return Ok(Ordering::Equal),
            (None, Some(_)) => return Ok(Ordering::Less),
            (Some(_), None) => return Ok(Ordering::Greater),
            (Some((left_head, left_tail)), Some((right_head, right_tail))) => {
                match compare_ordered_values(&left_head, &right_head, local_env, name)? {
                    Ordering::Equal => {
                        left = left_tail;
                        right = right_tail;
                    }
                    ordering => return Ok(ordering),
                }
            }
        }
    }
}

fn equal_values(
    left: &Value,
    right: &Value,
    local_env: &[Value],
    name: &str,
) -> Result<bool, EvalError> {
    let left = force_value_shell(left)?;
    let right = force_value_shell(right)?;
    match (left, right) {
        (Value::Atom(left), Value::Atom(right)) => Ok(left == right),
        (Value::Number(left), Value::Number(right)) => Ok(left == right),
        (Value::Binary(left), Value::Binary(right)) => Ok(left == right),
        (Value::Binary(left), Value::List(right)) => {
            equal_lists(List::from_bytes(left), right, local_env, name)
        }
        (Value::List(left), Value::Binary(right)) => {
            equal_lists(left, List::from_bytes(right), local_env, name)
        }
        (Value::List(left), Value::List(right)) => equal_lists(left, right, local_env, name),
        (Value::Dict(left), Value::Dict(right)) => equal_dicts(&left, &right, local_env, name),
        (Value::Expr(_), _) | (_, Value::Expr(_)) => {
            unreachable!("force_value_shell removes suspended values")
        }
        (Value::Builtin(_), _)
        | (_, Value::Builtin(_))
        | (Value::PartialBuiltin(_), _)
        | (_, Value::PartialBuiltin(_))
        | (Value::Closure(_), _)
        | (_, Value::Closure(_)) => Err(EvalError::new(format!(
            "{name} builtin cannot compare function values"
        ))),
        (Value::Atom(_), _)
        | (Value::Number(_), _)
        | (Value::Binary(_), _)
        | (Value::List(_), _)
        | (Value::Dict(_), _) => Ok(false),
    }
}

fn equal_lists(
    mut left: List,
    mut right: List,
    local_env: &[Value],
    name: &str,
) -> Result<bool, EvalError> {
    loop {
        match (pop_list_front(&left)?, pop_list_front(&right)?) {
            (None, None) => return Ok(true),
            (None, Some(_)) | (Some(_), None) => return Ok(false),
            (Some((left_head, left_tail)), Some((right_head, right_tail))) => {
                if !equal_values(&left_head, &right_head, local_env, name)? {
                    return Ok(false);
                }
                left = left_tail;
                right = right_tail;
            }
        }
    }
}

fn equal_dicts(
    left: &crate::core::Dict,
    right: &crate::core::Dict,
    local_env: &[Value],
    name: &str,
) -> Result<bool, EvalError> {
    let empty = Value::Dict(crate::core::Dict::new_sync());
    for (key, left_value) in left.iter() {
        let right_value = right.get(key).unwrap_or(&empty);
        if !equal_values(left_value, right_value, local_env, name)? {
            return Ok(false);
        }
    }

    for (key, right_value) in right.iter() {
        if left.contains_key(key) {
            continue;
        }
        if !equal_values(&empty, right_value, local_env, name)? {
            return Ok(false);
        }
    }

    Ok(true)
}

fn tuple_payload(dict: &crate::core::Dict) -> Option<Value> {
    let tuple_key = Key::atom_from_text("tuple");
    let payload = dict.get(&tuple_key)?;
    if is_undefined_dict_value(payload) {
        return None;
    }
    dict.iter()
        .all(|(key, value)| *key == tuple_key || is_undefined_dict_value(value))
        .then(|| payload.clone())
}

fn list_like_value(value: Value, name: &str) -> Result<List, EvalError> {
    match force_value_shell(&value)? {
        Value::Binary(bytes) => Ok(List::from_bytes(bytes)),
        Value::List(list) => Ok(list),
        other => Err(EvalError::new(format!(
            "{name} builtin requires tuple payloads to be lists or binaries, got {other:?}"
        ))),
    }
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
            Ok(Value::Binary(bytes.slice(start..end)))
        }
        Value::List(list) => {
            let Some(slice) = list.try_slice(start, end, &mut force_list_thunk)? else {
                return Err(EvalError::new("slice builtin end is out of bounds"));
            };
            Ok(Value::List(slice))
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

fn eval_list_len_builtin(value: &Value) -> Result<Value, EvalError> {
    match force_value_shell(value)? {
        Value::Binary(bytes) => Ok(Value::Number(Number::from_usize(bytes.len()))),
        Value::List(list) => Ok(Value::Number(Number::from_usize(
            list.try_len(&mut force_list_thunk)?,
        ))),
        _ => Err(EvalError::new(
            "list len builtin requires a list or binary value",
        )),
    }
}

fn eval_list_split_builtin(
    index: &Value,
    value: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let index = eval_index_number(index, local_env, "split")?;
    match force_value_shell(value)? {
        Value::Binary(bytes) => {
            if index > bytes.len() {
                return Err(EvalError::new("split builtin index is out of bounds"));
            }
            Ok(split_result_value(
                Value::Binary(bytes.slice(0..index)),
                Value::Binary(bytes.slice(index..bytes.len())),
            ))
        }
        Value::List(list) => {
            let Some((left, right)) = list.try_split_at(index, &mut force_list_thunk)? else {
                return Err(EvalError::new("split builtin index is out of bounds"));
            };
            Ok(split_result_value(Value::List(left), Value::List(right)))
        }
        _ => Err(EvalError::new(
            "split builtin requires a list or binary value",
        )),
    }
}

fn eval_list_split_end_builtin(
    count: &Value,
    value: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let count = eval_index_number(count, local_env, "split_end")?;
    match force_value_shell(value)? {
        Value::Binary(bytes) => {
            if count > bytes.len() {
                return Err(EvalError::new("split_end builtin count is out of bounds"));
            }
            let index = bytes.len() - count;
            Ok(split_result_value(
                Value::Binary(bytes.slice(0..index)),
                Value::Binary(bytes.slice(index..bytes.len())),
            ))
        }
        Value::List(list) => {
            let Some((left, right)) = list.try_split_from_end(count, &mut force_list_thunk)? else {
                return Err(EvalError::new("split_end builtin count is out of bounds"));
            };
            Ok(split_result_value(Value::List(left), Value::List(right)))
        }
        _ => Err(EvalError::new(
            "split_end builtin requires a list or binary value",
        )),
    }
}

fn eval_list_head_builtin(value: &Value) -> Result<Value, EvalError> {
    match force_value_shell(value)? {
        Value::Binary(bytes) => bytes
            .first()
            .map(|byte| Value::Number(Number::from_u8(*byte)))
            .ok_or_else(|| EvalError::new("list head builtin requires a non-empty list or binary")),
        Value::List(list) => pop_list_front(&list)?
            .map(|(head, _)| head)
            .ok_or_else(|| EvalError::new("list head builtin requires a non-empty list or binary")),
        _ => Err(EvalError::new(
            "list head builtin requires a list or binary value",
        )),
    }
}

fn eval_list_tail_builtin(value: &Value) -> Result<Value, EvalError> {
    match force_value_shell(value)? {
        Value::Binary(bytes) => {
            if bytes.is_empty() {
                Err(EvalError::new(
                    "list tail builtin requires a non-empty list or binary",
                ))
            } else {
                Ok(Value::Binary(bytes.slice(1..bytes.len())))
            }
        }
        Value::List(list) => {
            let Some((_, tail)) = pop_list_front(&list)? else {
                return Err(EvalError::new(
                    "list tail builtin requires a non-empty list or binary",
                ));
            };
            Ok(Value::List(tail))
        }
        _ => Err(EvalError::new(
            "list tail builtin requires a list or binary value",
        )),
    }
}

fn eval_list_effect_builtin(effect: &Value, local_env: &[Value]) -> Result<Value, EvalError> {
    Ok(Value::List(lazy_run_list_effect(
        effect.clone(),
        Arc::from(local_env.to_vec()),
    )))
}

fn eval_list_effect_seq_builtin(
    operation: &Value,
    continuation: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    Ok(Value::List(flat_map_list_effect_results(
        lazy_run_list_effect(operation.clone(), Arc::from(local_env.to_vec())),
        continuation.clone(),
        Arc::from(local_env.to_vec()),
    )))
}

fn eval_list_effect_alt_builtin(
    left: &Value,
    right: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    Ok(Value::List(List::concat(
        lazy_run_list_effect(left.clone(), Arc::from(local_env.to_vec())),
        lazy_run_list_effect(right.clone(), Arc::from(local_env.to_vec())),
    )))
}

fn eval_list_effect_cut_builtin(
    operation: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    Ok(Value::List(cut_list_effect_results(
        operation.clone(),
        Arc::from(local_env.to_vec()),
    )))
}

fn eval_list_effect_fix_builtin(function: &Value, local_env: &[Value]) -> Result<Value, EvalError> {
    let function = eval_value(function)?;
    let handle = IVar::new();
    let marker = Value::expr(Expr::Future(handle.clone()));
    let operation = apply_value(function, marker.clone(), local_env)?;
    Ok(Value::List(fix_list_effect_results(
        operation,
        handle,
        Arc::from(local_env.to_vec()),
    )))
}

fn lazy_run_list_effect(effect: Value, local_env: Arc<[Value]>) -> List {
    deferred_list("list effect", move || {
        run_list_effect_to_list(effect.clone(), local_env.clone())
    })
}

fn run_list_effect_to_list(effect: Value, local_env: Arc<[Value]>) -> Result<List, EvalError> {
    let effect = force_value_shell(&effect)?;
    let Value::Dict(dict) = effect else {
        return Err(EvalError::new(format!(
            "list effect handler requires an effect dictionary, got {effect:?}"
        )));
    };
    let Some(function) = dict_effect_function(&dict) else {
        return Err(EvalError::new(
            "list effect handler requires an `eff` member",
        ));
    };

    let handled = apply_value(eval_value(&function)?, list_effect_api(), &local_env)?;
    let handled = force_value_shell(&handled)?;
    let Value::List(results) = handled else {
        return Err(EvalError::new(format!(
            "list effect handler expected a standard effect result list, got {handled:?}"
        )));
    };
    Ok(results)
}

fn flat_map_list_effect_results(
    results: List,
    continuation: Value,
    local_env: Arc<[Value]>,
) -> List {
    deferred_list("list effect seq", move || {
        let Some((head, tail)) = pop_list_front(&results)? else {
            return Ok(List::empty());
        };
        let continuation = eval_value(&continuation)?;
        let next = apply_value(continuation.clone(), head, &local_env)?;
        Ok(List::concat(
            lazy_run_list_effect(next, local_env.clone()),
            flat_map_list_effect_results(tail, continuation, local_env.clone()),
        ))
    })
}

fn cut_list_effect_results(operation: Value, local_env: Arc<[Value]>) -> List {
    deferred_list("list effect cut", move || {
        let results = lazy_run_list_effect(operation.clone(), local_env.clone());
        let Some((head, _)) = pop_list_front(&results)? else {
            return Ok(List::empty());
        };
        Ok(List::from_values(vec![head]))
    })
}

fn fix_list_effect_results(operation: Value, handle: IVar, local_env: Arc<[Value]>) -> List {
    deferred_list("list effect fix", move || {
        let results = lazy_run_list_effect(operation.clone(), local_env.clone());
        let Some((head, tail)) = pop_list_front(&results)? else {
            handle
                .set(Value::List(List::empty()))
                .map_err(|_| EvalError::new("list effect fix initialized twice"))?;
            return Ok(List::empty());
        };
        handle
            .set(head.clone())
            .map_err(|_| EvalError::new("list effect fix initialized twice"))?;
        Ok(List::concat(List::from_values(vec![head]), tail))
    })
}

fn deferred_list(
    label: &'static str,
    thunk: impl Fn() -> Result<List, EvalError> + Send + Sync + 'static,
) -> List {
    List::from_thunk(Thunk::new(
        Arc::new(Expr::Deferred(Arc::new(DeferredValue::new(
            label,
            move || thunk().map(Value::List).map_err(|err| err.to_string()),
        )))),
        Arc::from([]),
    ))
}

fn list_effect_api() -> Value {
    Value::Dict(
        crate::core::Dict::new_sync()
            .insert(
                Key::atom_from_text("r"),
                Value::Builtin(Builtin::ListEffectReturn),
            )
            .insert(
                Key::atom_from_text("seq"),
                Value::Builtin(Builtin::ListEffectSeq),
            )
            .insert(
                Key::atom_from_text("alt"),
                Value::Builtin(Builtin::ListEffectAlt),
            )
            .insert(Key::atom_from_text("fail"), Value::List(List::empty()))
            .insert(
                Key::atom_from_text("cut"),
                Value::Builtin(Builtin::ListEffectCut),
            )
            .insert(
                Key::atom_from_text("fix"),
                Value::Builtin(Builtin::ListEffectFix),
            ),
    )
}

fn split_result_value(left: Value, right: Value) -> Value {
    Value::Dict(
        crate::core::Dict::new_sync()
            .insert(Key::atom_from_text("left"), left)
            .insert(Key::atom_from_text("right"), right),
    )
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

fn builtin2_expr(builtin: Builtin, left: Expr, right: Expr) -> Expr {
    Expr::Apply(
        Arc::new(Expr::Apply(
            Arc::new(Expr::Value(Value::Builtin(builtin))),
            Arc::new(left),
        )),
        Arc::new(right),
    )
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
    let left = force_value_shell(left)?;
    let right = force_value_shell(right)?;
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
    path: &Value,
    new_value: &Value,
    dict: &Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let path = eval_key_path_list(path, local_env)?;
    if path.is_empty() {
        return Err(EvalError::new(
            "dict update builtin requires a non-empty path",
        ));
    }
    let dict = force_value_shell(dict)?;
    let Value::Dict(dict) = dict else {
        return Err(EvalError::new("dict update builtin requires a dictionary"));
    };
    Ok(Value::Dict(update_dict_path(
        &dict,
        &path,
        new_value.clone(),
    )))
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

fn eval_object_instance_builtin(spec: &Value, local_env: &[Value]) -> Result<Value, EvalError> {
    let spec_dict = object_spec_dict(spec)?;
    let specs = object_application_order(&spec_dict, local_env)?;

    let handle = IVar::new();
    let self_marker = Value::expr(Expr::Future(handle.clone()));
    let mut base = Value::Dict(crate::core::Dict::new_sync());
    for spec in specs {
        let defs = spec
            .get(&Key::atom_from_text("defs"))
            .cloned()
            .unwrap_or_else(default_object_defs_value);
        let mixed = apply_value(eval_value(&defs)?, base, local_env)?;
        let mixed = apply_value(eval_value(&mixed)?, self_marker.clone(), local_env)?;
        let Value::Dict(mixed_dict) = force_value_shell(&mixed)? else {
            return Err(EvalError::new(
                "object definition mixin must produce a dictionary",
            ));
        };
        base = Value::Dict(mixed_dict);
    }

    let Value::Dict(base_dict) = base else {
        return Err(EvalError::new("object base is not a dictionary"));
    };
    let object = Value::Dict(base_dict.insert(Key::atom_from_text("spec"), Value::Dict(spec_dict)));
    handle
        .set(object.clone())
        .map_err(|_| EvalError::new("object instance initialized twice"))?;
    Ok(object)
}

fn eval_object_instance_from_parts_builtin(
    name: Value,
    deps: Value,
    defs: Value,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let spec = crate::core::Dict::new_sync()
        .insert(Key::atom_from_text("name"), name)
        .insert(Key::atom_from_text("deps"), deps)
        .insert(Key::atom_from_text("defs"), defs);
    eval_object_instance_builtin(&Value::Dict(spec), local_env)
}

fn eval_object_spec_builtin(value: &Value) -> Result<Value, EvalError> {
    let value = force_value_shell(value)?;
    let Value::Dict(dict) = value else {
        return Err(EvalError::new(
            "object spec builtin requires an object or dictionary value",
        ));
    };

    if let Some(spec) = dict.get(&Key::atom_from_text("spec")) {
        let spec = force_value_shell(spec)?;
        if !is_undefined_dict_value(&spec) {
            return Ok(spec);
        }
    }

    Ok(dict_object_spec(dict))
}

fn eval_object_local_name_builtin(host: &Value, parts: &Value) -> Result<Value, EvalError> {
    let host_spec = eval_object_spec_builtin(host)?;
    let host_spec = object_spec_dict(&host_spec)?;
    let Some(host_name) = host_spec.get(&Key::atom_from_text("name")).cloned() else {
        return Err(EvalError::new("object specification requires a name"));
    };

    let mut name_parts = vec![eval_value(&host_name)?];
    name_parts.extend(match force_value_shell(parts)? {
        Value::List(parts) => list_to_value_items(&parts)?,
        Value::Dict(dict) if dict.is_empty() => Vec::new(),
        _ => {
            return Err(EvalError::new(
                "object local name builtin requires a list of name parts",
            ));
        }
    });
    Ok(Value::List(List::from_values(name_parts)))
}

fn object_spec_dict(spec: &Value) -> Result<crate::core::Dict, EvalError> {
    let spec = force_value_shell(spec)?;
    let Value::Dict(spec_dict) = spec else {
        return Err(EvalError::new(
            "object instance builtin requires a specification dictionary",
        ));
    };
    Ok(spec_dict)
}

fn dict_object_spec(dict: crate::core::Dict) -> Value {
    let defs = Expr::lambda(Arc::new(Expr::lambda(Arc::new(dict_union_expr(
        Expr::Local(1),
        Expr::Value(Value::Dict(dict)),
    )))));
    let spec = crate::core::Dict::new_sync()
        .insert(
            Key::atom_from_text("name"),
            Value::Dict(crate::core::Dict::new_sync()),
        )
        .insert(Key::atom_from_text("deps"), Value::List(List::empty()))
        .insert(Key::atom_from_text("defs"), Value::expr(defs));
    Value::Dict(spec)
}

fn dict_union_expr(left: Expr, right: Expr) -> Expr {
    builtin2_expr(Builtin::DictUnion, left, right)
}

fn object_application_order(
    spec: &crate::core::Dict,
    local_env: &[Value],
) -> Result<Vec<crate::core::Dict>, EvalError> {
    let mut seen = BTreeMap::new();
    let mut next_anonymous_id = 0;
    let mut linearized =
        object_c3_linearization(spec, local_env, &mut seen, &mut next_anonymous_id)?;
    linearized.reverse();
    Ok(linearized
        .into_iter()
        .map(|entry| entry.spec)
        .collect::<Vec<_>>())
}

#[derive(Clone)]
struct LinearizedObjectSpec {
    spec: crate::core::Dict,
    name: Key,
    anonymous_id: Option<u64>,
}

impl LinearizedObjectSpec {
    fn new(
        spec: crate::core::Dict,
        local_env: &[Value],
        next_anonymous_id: &mut u64,
    ) -> Result<Self, EvalError> {
        let name = object_spec_name(&spec, local_env)?;
        let anonymous_id = if is_anonymous_object_name(&name) {
            let id = *next_anonymous_id;
            *next_anonymous_id += 1;
            Some(id)
        } else {
            None
        };
        Ok(Self {
            spec,
            name,
            anonymous_id,
        })
    }
}

fn object_c3_linearization(
    spec: &crate::core::Dict,
    local_env: &[Value],
    seen: &mut BTreeMap<Key, ()>,
    next_anonymous_id: &mut u64,
) -> Result<Vec<LinearizedObjectSpec>, EvalError> {
    let entry = LinearizedObjectSpec::new(spec.clone(), local_env, next_anonymous_id)?;
    if entry.anonymous_id.is_none() {
        remember_object_spec(&entry.name, spec, seen)?;
    }
    let deps = spec
        .get(&Key::atom_from_text("deps"))
        .cloned()
        .unwrap_or_else(|| Value::List(List::empty()));
    let deps = object_dep_specs(&deps)?;
    let mut sequences: Vec<Vec<LinearizedObjectSpec>> = Vec::new();
    let mut direct_deps = Vec::new();
    let mut saw_named_dep = false;
    for dep_spec in &deps {
        let dep_spec = object_spec_dict(&dep_spec)?;
        let dep_linearization =
            object_c3_linearization(&dep_spec, local_env, seen, next_anonymous_id)?;
        let dep_entry = dep_linearization
            .first()
            .cloned()
            .ok_or_else(|| EvalError::new("object dependency linearization was empty"))?;
        if dep_entry.anonymous_id.is_some() {
            if saw_named_dep {
                return Err(EvalError::new(
                    "anonymous object dependencies must appear before named object dependencies",
                ));
            }
        } else {
            saw_named_dep = true;
        }
        direct_deps.push(dep_entry);
        sequences.push(dep_linearization);
    }
    sequences.push(direct_deps);

    let mut linearized = vec![entry];
    linearized.extend(c3_merge(sequences, local_env)?);
    Ok(linearized)
}

fn c3_merge(
    mut sequences: Vec<Vec<LinearizedObjectSpec>>,
    _local_env: &[Value],
) -> Result<Vec<LinearizedObjectSpec>, EvalError> {
    let mut result = Vec::new();

    loop {
        sequences.retain(|sequence| !sequence.is_empty());
        if sequences.is_empty() {
            return Ok(result);
        }

        let mut selected = None;
        'candidate: for sequence in &sequences {
            let candidate = &sequence[0];
            for other in &sequences {
                if other
                    .iter()
                    .skip(1)
                    .any(|spec| same_linearized_object_spec(spec, candidate))
                {
                    continue 'candidate;
                }
            }
            selected = Some(candidate.clone());
            break;
        }

        let Some(selected_spec) = selected else {
            return Err(EvalError::new(
                "object dependencies have inconsistent C3 linearization",
            ));
        };
        result.push(selected_spec.clone());

        for sequence in &mut sequences {
            if sequence
                .first()
                .is_some_and(|spec| same_linearized_object_spec(spec, &selected_spec))
            {
                sequence.remove(0);
            }
        }
    }
}

fn same_linearized_object_spec(left: &LinearizedObjectSpec, right: &LinearizedObjectSpec) -> bool {
    match (left.anonymous_id, right.anonymous_id) {
        (Some(left), Some(right)) => left == right,
        (None, None) => left.name == right.name,
        _ => false,
    }
}

fn object_spec_name(spec: &crate::core::Dict, local_env: &[Value]) -> Result<Key, EvalError> {
    let Some(name) = spec.get(&Key::atom_from_text("name")) else {
        return Err(EvalError::new("object specification requires a name"));
    };
    let name = eval_value(name)?;
    value_to_key(&name, local_env)
}

fn is_anonymous_object_name(name: &Key) -> bool {
    matches!(name, Key::Dict(entries) if entries.is_empty())
}

fn remember_object_spec(
    name: &Key,
    _spec: &crate::core::Dict,
    seen: &mut BTreeMap<Key, ()>,
) -> Result<(), EvalError> {
    seen.insert(name.clone(), ());
    Ok(())
}

fn object_dep_specs(deps: &Value) -> Result<Vec<Value>, EvalError> {
    match force_value_shell(deps)? {
        Value::List(list) => list_to_value_items(&list),
        Value::Dict(dict) if dict.is_empty() => Ok(Vec::new()),
        _ => Err(EvalError::new(
            "object specification deps must evaluate to a list",
        )),
    }
}

fn default_object_defs_value() -> Value {
    Value::expr(Expr::lambda(Arc::new(Expr::lambda(Arc::new(Expr::Local(
        1,
    ))))))
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
        RecognizedAnnotation::AssertUnit { value } => {
            let value = eval_value(&value)?;
            if is_unit_value(&value) {
                Ok(target.clone())
            } else {
                Ok(annotation_error_value(format!(
                    "`=>>` requires discarded effect results to be unit, got {value:?}"
                )))
            }
        }
        RecognizedAnnotation::Deque => eval_deque_annotation(target),
        RecognizedAnnotation::Binary => eval_binary_annotation(target),
        RecognizedAnnotation::Array => eval_array_annotation(target),
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
    AssertUnit { value: Value },
    Deque,
    Binary,
    Array,
    Invalid(String),
    Unknown(String),
}

fn recognize_annotation(
    annotation: &Value,
    local_env: &[Value],
) -> Result<RecognizedAnnotation, EvalError> {
    let annotation = eval_value(annotation)?;
    if let Value::Atom(atom) = &annotation {
        return Ok(recognize_simple_annotation(atom)
            .unwrap_or_else(|| RecognizedAnnotation::Unknown(format!("{annotation:?}"))));
    }

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
        Key::Atom(atom) if atom_name(atom) == Some("assert_unit") => {
            Ok(match parse_value_annotation(payload, "assert_unit")? {
                ParsedValueAnnotation::Valid { value } => {
                    RecognizedAnnotation::AssertUnit { value }
                }
                ParsedValueAnnotation::Invalid(message) => RecognizedAnnotation::Invalid(message),
            })
        }
        Key::Atom(atom) if payload_is_unit(payload) => Ok(recognize_simple_annotation(atom)
            .unwrap_or_else(|| RecognizedAnnotation::Unknown(format!("{annotation:?}")))),
        _ => Ok(RecognizedAnnotation::Unknown(format!("{annotation:?}"))),
    }
}

fn recognize_simple_annotation(atom: &crate::core::Atom) -> Option<RecognizedAnnotation> {
    match atom_name(atom)? {
        "deque" => Some(RecognizedAnnotation::Deque),
        "binary" => Some(RecognizedAnnotation::Binary),
        "array" => Some(RecognizedAnnotation::Array),
        _ => None,
    }
}

fn payload_is_unit(payload: &Value) -> bool {
    matches!(payload, Value::Dict(dict) if dict.is_empty())
}

enum ParsedAssertion {
    Valid { name: String, defined: bool },
    Invalid(String),
}

enum ParsedValueAnnotation {
    Valid { value: Value },
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

fn parse_value_annotation(
    payload: &Value,
    tag_name: &str,
) -> Result<ParsedValueAnnotation, EvalError> {
    let payload = eval_value(payload)?;
    let Value::Dict(payload) = payload else {
        return Ok(ParsedValueAnnotation::Invalid(format!(
            "invalid `{tag_name}` annotation payload"
        )));
    };

    let Some(value) = payload.get(&Key::atom_from_text("value")) else {
        return Ok(ParsedValueAnnotation::Invalid(format!(
            "invalid `{tag_name}` annotation payload"
        )));
    };

    Ok(ParsedValueAnnotation::Valid {
        value: value.clone(),
    })
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

fn is_unit_value(value: &Value) -> bool {
    matches!(
        value,
        Value::Atom(atom) if atom.key() == &Key::abstract_global_path(["builtin", "unit"])
    )
}

fn annotation_error_value(message: impl Into<String>) -> Value {
    Value::Expr(Thunk::new(
        Arc::new(Expr::Error(Arc::from(message.into()))),
        Arc::from([]),
    ))
}

fn eval_deque_annotation(target: &Value) -> Result<Value, EvalError> {
    match force_value_shell(target)? {
        Value::List(list) => Ok(Value::List(list.try_balanced(&mut force_list_thunk)?)),
        other => Ok(annotation_error_value(format!(
            "`deque` annotation requires a list target, got {other:?}"
        ))),
    }
}

fn eval_binary_annotation(target: &Value) -> Result<Value, EvalError> {
    match force_value_shell(target)? {
        Value::Binary(bytes) => Ok(Value::Binary(bytes)),
        Value::List(list) => match list_to_binary_bytes(&list) {
            Ok(bytes) => Ok(Value::Binary(Bytes::from(bytes))),
            Err(message) => Ok(annotation_error_value(message)),
        },
        other => Ok(annotation_error_value(format!(
            "`binary` annotation requires a list or binary target, got {other:?}"
        ))),
    }
}

fn eval_array_annotation(target: &Value) -> Result<Value, EvalError> {
    match force_value_shell(target)? {
        Value::Binary(bytes) => Ok(Value::List(List::from_values(
            bytes
                .iter()
                .map(|byte| Value::Number(Number::from_u8(*byte)))
                .collect(),
        ))),
        Value::List(list) => Ok(Value::List(List::from_values(list_to_value_items(&list)?))),
        other => Ok(annotation_error_value(format!(
            "`array` annotation requires a list or binary target, got {other:?}"
        ))),
    }
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
        Value::Expr(Thunk::new(
            Arc::new(Expr::Error(Arc::from(format!(
                "dictionary union is ambiguous at key `{}`",
                format_name_part(key)
            )))),
            Arc::from([]),
        ))
    }
}

fn update_dict_path(dict: &crate::core::Dict, path: &[Key], new_value: Value) -> crate::core::Dict {
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
        update_nested_dict_path(head, rest, new_value, prior)
    };

    if is_undefined_dict_value(&next_value) {
        dict.remove(head)
    } else {
        dict.insert(head.clone(), next_value)
    }
}

fn update_nested_dict_path(head: &Key, rest: &[Key], new_value: Value, prior: Value) -> Value {
    match prior {
        Value::Dict(dict) => Value::Dict(update_dict_path(&dict, rest, new_value)),
        Value::Expr(_) => builtin_apply3_value(
            Builtin::DictUpdate,
            &key_path_value(rest),
            &new_value,
            &prior,
        ),
        _ => Value::Expr(Thunk::new(
            Arc::new(Expr::Error(Arc::from(format!(
                "dictionary update path `{}` traverses a non-dictionary value",
                format_name_part(head)
            )))),
            Arc::from([]),
        )),
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

fn value_as_expr(value: &Value) -> Arc<Expr> {
    Arc::new(Expr::Value(value.clone()))
}

fn builtin_apply3_value(builtin: Builtin, first: &Value, second: &Value, third: &Value) -> Value {
    Value::Expr(Thunk::new(
        Arc::new(Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Apply(
                    Arc::new(Expr::Value(Value::Builtin(builtin))),
                    value_as_expr(first),
                )),
                value_as_expr(second),
            )),
            value_as_expr(third),
        )),
        Arc::from([]),
    ))
}

fn is_expr_value(value: &Value) -> bool {
    matches!(value, Value::Expr(_))
}

fn is_error_expr_value(value: &Value) -> bool {
    matches!(value, Value::Expr(thunk) if matches!(thunk.expr().map(Arc::as_ref), Some(Expr::Error(_))))
}

fn is_undefined_dict_value(value: &Value) -> bool {
    is_undefined_value(value)
}

fn expand_key_expr(key: &KeyExpr, local_env: &[Value]) -> Result<Vec<Key>, EvalError> {
    match key {
        KeyExpr::Key(key) => Ok(vec![key.clone()]),
        KeyExpr::Index(expr) => {
            let value = Value::Expr(Thunk::new(expr.clone(), Arc::from(local_env.to_vec())));
            let value = force_value_shell(&value)?;
            Ok(vec![value_to_key(&value, local_env)?])
        }
        KeyExpr::PathIndex(expr) => eval_key_path_list(
            &Value::Expr(Thunk::new(expr.clone(), Arc::from(local_env.to_vec()))),
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
    list.try_for_each_segment(
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
        &mut force_list_thunk,
    )?;
    Ok(items.into_inner())
}

fn list_to_key_items(list: &List, local_env: &[Value]) -> Result<Arc<[Key]>, EvalError> {
    let items = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
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
        &mut force_list_thunk,
    )?;
    Ok(Arc::from(items.into_inner()))
}

fn list_to_value_items(list: &List) -> Result<Vec<Value>, EvalError> {
    let items = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
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
        &mut force_list_thunk,
    )?;
    Ok(items.into_inner())
}

fn list_to_binary_bytes(list: &List) -> Result<Vec<u8>, String> {
    let bytes = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |segment| {
            bytes.borrow_mut().extend_from_slice(segment);
            Ok::<_, String>(())
        },
        &mut |values| {
            for value in values.iter() {
                match force_value_shell(value).map_err(|err| err.to_string())? {
                    Value::Number(number) => {
                        let byte = number.to_u8_if_integer().ok_or_else(|| {
                            format!("`binary` annotation cannot encode number `{number}` as a byte")
                        })?;
                        bytes.borrow_mut().push(byte);
                    }
                    Value::Binary(segment) => bytes.borrow_mut().extend_from_slice(&segment),
                    Value::List(list) => {
                        bytes
                            .borrow_mut()
                            .extend(list_to_binary_bytes(&list)?);
                    }
                    other => {
                        return Err(format!(
                            "`binary` annotation requires list items to be bytes or binaries, got {other:?}"
                        ));
                    }
                }
            }
            Ok(())
        },
        &mut |thunk| force_list_thunk(thunk).map_err(|err| err.to_string()),
    )?;
    Ok(bytes.into_inner())
}

pub fn list_output_bytes(list: &List) -> Result<Vec<u8>, String> {
    let bytes = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |segment| {
            bytes.borrow_mut().extend_from_slice(segment);
            Ok::<_, String>(())
        },
        &mut |segment| {
            for item in segment.iter() {
                let item = eval_value(item).map_err(|err| err.to_string())?;
                let Value::Number(number) = item else {
                    return Err("must contain only integers and binary segments".to_owned());
                };

                let byte = number.to_u8_if_integer().ok_or_else(|| {
                    format!("contains number `{number}` that is not an in-range byte integer")
                })?;
                bytes.borrow_mut().push(byte);
            }
            Ok(())
        },
        &mut |thunk| force_list_thunk(thunk).map_err(|err| err.to_string()),
    )?;
    Ok(bytes.into_inner())
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
        Value::Expr(thunk) => Ok(List::from_thunk(thunk)),
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

    use bytes::Bytes;

    use crate::core::{Dict, Expr, IVar, Lambda, Thunk, Value};
    use crate::number::Number;

    use super::*;

    #[test]
    fn each_lambda_lowers_to_one_shared_interaction_net() {
        let lambda = Arc::new(Lambda::new(Arc::new(Expr::Local(0))));
        let expr = Expr::Lambda(lambda.clone());

        assert!(!lambda.is_lowered());
        let first = eval_closed_expr(&expr).expect("lambda should evaluate to a closure");
        let second = eval_closed_expr(&expr).expect("lambda should reuse its lowered net");
        assert!(lambda.is_lowered());

        let (Value::Closure(first), Value::Closure(second)) = (first, second) else {
            panic!("lambda evaluations should produce closures");
        };
        assert!(first.interaction_net.ptr_eq(&second.interaction_net));
    }

    #[test]
    fn lowering_an_outer_lambda_does_not_lower_unreached_nested_lambdas() {
        let inner = Arc::new(Lambda::new(Arc::new(Expr::Error(Arc::from(
            "unreached body",
        )))));
        let outer = Arc::new(Lambda::new(Arc::new(Expr::Lambda(inner.clone()))));

        let value = eval_closed_expr(&Expr::Lambda(outer.clone()))
            .expect("outer lambda should become a closure");
        assert!(matches!(value, Value::Closure(_)));
        assert!(outer.is_lowered());
        assert!(!inner.is_lowered());
    }

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

    fn dict_update_expr(path: Expr, new_value: Expr, dict: Expr) -> Expr {
        builtin3_expr(Builtin::DictUpdate, path, new_value, dict)
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

    fn key_path_expr(path: Vec<Key>) -> Expr {
        Expr::Value(Value::List(List::from_values(
            path.iter().map(key_value).collect(),
        )))
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
            Value::Expr(thunk) if thunk.env().is_some_and(|env| env.is_empty()) => {
                thunk.expr().unwrap().as_ref().clone()
            }
            _ => Expr::Value(value.clone()),
        }
    }

    fn fixpoint_dict(dict: Dict) -> Expr {
        Expr::Apply(
            Arc::new(Expr::Value(Value::Builtin(Builtin::Fixpoint))),
            Arc::new(Expr::lambda(Arc::new(module_value_expr(&Value::Dict(
                dict,
            ))))),
        )
    }

    fn rooted_expr_value(root: &Value, expr: Expr) -> Value {
        let handle = IVar::new();
        handle
            .set(root.clone())
            .expect("rooted test expression should initialize handle once");
        Value::Expr(Thunk::new(
            Arc::new(expr),
            Arc::from([Value::expr(Expr::Future(handle))]),
        ))
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
    fn append_preserves_lazy_list_chunks_until_observed() {
        let expr = builtin2_expr(
            Builtin::Append,
            Expr::Value(Value::List(List::from_values(vec![n(72)]))),
            builtin2_expr(
                Builtin::Append,
                Expr::Value(Value::binary_from_text("i")),
                Expr::Value(Value::binary_from_text("!")),
            ),
        );

        let value = eval_closed_expr(&expr).expect("append should evaluate lazily");

        let Value::List(list) = value else {
            panic!("append should produce a list");
        };
        assert_eq!(list.known_len(), None);
        assert_eq!(
            list_output_bytes(&list).expect("lazy chunk should force"),
            b"Hi!"
        );
    }

    #[test]
    fn lazy_list_chunks_error_when_they_do_not_evaluate_to_lists() {
        let expr = builtin2_expr(
            Builtin::Append,
            Expr::Value(Value::binary_from_text("Hi")),
            builtin2_expr(Builtin::Add, Expr::Value(n(1)), Expr::Value(n(1))),
        );

        let value = eval_closed_expr(&expr).expect("append should preserve lazy chunk");
        let Value::List(list) = value else {
            panic!("append should produce a list");
        };

        let err = list_output_bytes(&list).expect_err("bad lazy chunk should fail when observed");
        assert!(err.contains("lazy list chunk must evaluate to a list or binary value"));
    }

    #[test]
    fn split_end_does_not_force_lazy_left_branch_when_suffix_is_in_right_branch() {
        let lazy_left = List::from_thunk(Thunk::new(
            Arc::new(Expr::Error(Arc::from("left branch was forced"))),
            Arc::from([]),
        ));
        let list = List::concat(lazy_left, List::from_bytes(Bytes::from_static(b"abc")));
        let split = eval_closed_expr(&builtin2_expr(
            Builtin::ListSplitEnd,
            Expr::Value(n(1)),
            Expr::Value(Value::List(list)),
        ))
        .expect("split_end should not force left branch");

        let Value::Dict(split) = split else {
            panic!("split_end should produce a dictionary");
        };
        let Value::List(suffix) = split
            .get(&Key::atom_from_text("right"))
            .expect("split should include right suffix")
        else {
            panic!("right suffix should be a list");
        };
        assert_eq!(
            list_output_bytes(suffix).expect("right suffix should render"),
            b"c"
        );
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
    fn expression_arguments_share_forced_values() {
        let force_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = force_count.clone();
        let counted = Expr::Deferred(Arc::new(DeferredValue::new("counted", move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(2))
        })));
        let expr = Expr::Apply(
            Arc::new(Expr::lambda(Arc::new(builtin2_expr(
                Builtin::Add,
                Expr::Local(0),
                Expr::Local(0),
            )))),
            Arc::new(counted),
        );

        let value = eval_closed_expr(&expr).expect("lambda body should evaluate");

        assert_eq!(value, n(4));
        assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn equality_errors_when_dictionary_comparison_reaches_functions() {
        let function = Value::expr(Expr::lambda(Arc::new(Expr::Local(0))));
        let left = Value::Dict(Dict::new_sync().insert(Key::atom_from_text("f"), function.clone()));
        let right = Value::Dict(Dict::new_sync().insert(Key::atom_from_text("f"), function));
        let err = eval_closed_expr(&builtin2_expr(
            Builtin::Equal,
            Expr::Value(left),
            Expr::Value(right),
        ))
        .expect_err("function-valued fields should not be equatable");

        assert!(err.to_string().contains("cannot compare function values"));
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
            Expr::lambda(Arc::new(Expr::Apply(
                Arc::new(Expr::Apply(
                    Arc::new(Expr::Value(Value::Builtin(Builtin::Add))),
                    Arc::new(Expr::Local(0)),
                )),
                Arc::new(Expr::Value(n(1))),
            ))),
            Expr::Value(Value::List(List::from_values(vec![n(1), n(2), n(3)]))),
        ))
        .expect("map should evaluate");
        let binary_len = eval_closed_expr(&builtin1_expr(
            Builtin::ListLen,
            Expr::Value(Value::binary_from_text("World!")),
        ))
        .expect("binary len should evaluate");
        let list_len = eval_closed_expr(&builtin1_expr(
            Builtin::ListLen,
            Expr::Value(Value::List(List::concat(
                List::from_values(vec![n(1), n(2)]),
                List::from_bytes(Bytes::from_static(b"Hi")),
            ))),
        ))
        .expect("list len should evaluate");

        assert_eq!(slice, Value::binary_from_text("orl"));
        let Value::List(mapped) = mapped else {
            panic!("map should produce a list");
        };
        let items = list_to_value_items(&mapped).expect("mapped list should be readable");
        assert_eq!(items, vec![n(2), n(3), n(4)]);
        assert_eq!(binary_len, n(6));
        assert_eq!(list_len, n(4));
    }

    #[test]
    fn evaluates_split_and_split_end_builtins() {
        let split = eval_closed_expr(&builtin2_expr(
            Builtin::ListSplit,
            Expr::Value(n(2)),
            Expr::Value(Value::binary_from_text("Hello")),
        ))
        .expect("split should evaluate");
        let split_end = eval_closed_expr(&builtin2_expr(
            Builtin::ListSplitEnd,
            Expr::Value(n(2)),
            Expr::Value(Value::List(List::concat(
                List::from_values(vec![n(1), n(2)]),
                List::from_bytes(Bytes::from_static(b"abc")),
            ))),
        ))
        .expect("split_end should evaluate");

        let Value::Dict(split) = split else {
            panic!("split should return a dictionary");
        };
        assert_eq!(
            split.get(&Key::atom_from_text("left")),
            Some(&Value::binary_from_text("He"))
        );
        assert_eq!(
            split.get(&Key::atom_from_text("right")),
            Some(&Value::binary_from_text("llo"))
        );

        let Value::Dict(split_end) = split_end else {
            panic!("split_end should return a dictionary");
        };
        let Value::List(prefix) = split_end
            .get(&Key::atom_from_text("left"))
            .expect("split_end should include left")
        else {
            panic!("split_end left should be a list");
        };
        let Value::List(suffix) = split_end
            .get(&Key::atom_from_text("right"))
            .expect("split_end should include right")
        else {
            panic!("split_end right should be a list");
        };

        assert_eq!(
            list_to_value_items(prefix).expect("prefix should be readable"),
            vec![n(1), n(2), Value::Number(Number::from_u8(b'a'))]
        );
        assert_eq!(
            list_to_value_items(suffix).expect("suffix should be readable"),
            vec![
                Value::Number(Number::from_u8(b'b')),
                Value::Number(Number::from_u8(b'c'))
            ]
        );
    }

    #[test]
    fn slice_builtin_shares_binary_storage() {
        let bytes = Bytes::from_static(b"Hello");
        let slice = eval_closed_expr(&builtin3_expr(
            Builtin::Slice,
            Expr::Value(n(1)),
            Expr::Value(n(4)),
            Expr::Value(Value::Binary(bytes.clone())),
        ))
        .expect("slice should evaluate");

        let Value::Binary(slice) = slice else {
            panic!("binary slice should remain binary");
        };
        assert_eq!(&slice[..], b"ell");
        assert_eq!(slice.as_ptr(), bytes[1..].as_ptr());
    }

    #[test]
    fn evaluates_lambda_application_lazily() {
        let expr = Expr::Apply(
            Arc::new(Expr::lambda(Arc::new(Expr::Local(0)))),
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
            Arc::new(Expr::lambda(Arc::new(Expr::Apply(
                Arc::new(Expr::lambda(Arc::new(Expr::Apply(
                    Arc::new(Expr::Local(0)),
                    Arc::new(Expr::Value(n(0))),
                )))),
                Arc::new(Expr::lambda(Arc::new(Expr::Local(1)))),
            )))),
            Arc::new(Expr::Value(n(42))),
        );

        let value = eval_closed_expr(&expr).expect("nested closures should evaluate");

        assert_eq!(value, n(42));
    }

    #[test]
    fn net_partial_builtins_escape_and_share_lazy_arguments() {
        let force_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = force_count.clone();
        let argument = Expr::Deferred(Arc::new(DeferredValue::new(
            "partial argument",
            move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(n(40))
            },
        )));
        let make_partial = Expr::lambda(Arc::new(Expr::Apply(
            Arc::new(Expr::Value(Value::Builtin(Builtin::Add))),
            Arc::new(Expr::Local(0)),
        )));
        let partial = eval_closed_expr(&Expr::Apply(Arc::new(make_partial), Arc::new(argument)))
            .expect("a net builtin should escape while retaining its argument lazily");

        assert!(matches!(partial, Value::PartialBuiltin(_)));
        assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(apply_value(partial.clone(), n(2), &[]).unwrap(), n(42));
        assert_eq!(apply_value(partial, n(3), &[]).unwrap(), n(43));
        assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn net_list_literals_retain_computed_items_as_lazy_holes() {
        let expression = Expr::Apply(
            Arc::new(Expr::lambda(Arc::new(Expr::List(Arc::from([Arc::new(
                Expr::Local(0),
            )]))))),
            Arc::new(Expr::Value(n(42))),
        );
        let Value::List(list) = eval_closed_expr(&expression).unwrap() else {
            panic!("net-backed list literal should produce a list");
        };
        let Some((item, tail)) = pop_list_front(&list).unwrap() else {
            panic!("net-backed list literal should contain its argument");
        };
        assert_eq!(item, n(42));
        assert!(pop_list_front(&tail).unwrap().is_none());
    }

    #[test]
    fn dropped_arguments_do_not_prevent_later_locals_from_resolving() {
        let expr = Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::lambda(Arc::new(Expr::lambda(Arc::new(Expr::Local(
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
    fn method_objects_apply_via_apply_member() {
        let method = Value::Dict(Dict::new_sync().insert(
            Key::atom_from_text("apply"),
            Value::expr(Expr::lambda(Arc::new(builtin2_expr(
                Builtin::Add,
                Expr::Local(0),
                Expr::Value(n(1)),
            )))),
        ));
        let value = eval_closed_expr(&Expr::Apply(
            Arc::new(Expr::Value(method)),
            Arc::new(Expr::Value(n(41))),
        ))
        .expect("method object application should evaluate");

        assert_eq!(value, n(42));
    }

    #[test]
    fn effect_values_apply_by_extending_the_effect_function() {
        let effect = effect_value(Value::expr(Expr::lambda(Arc::new(Expr::Access(
            Arc::new(Expr::Local(0)),
            Arc::from([KeyExpr::Key(Key::atom_from_text("op"))]),
        )))));
        let applied = eval_closed_expr(&Expr::Apply(
            Arc::new(Expr::Value(effect)),
            Arc::new(Expr::Value(n(41))),
        ))
        .expect("effect application should evaluate");
        let Value::Dict(effect) = applied else {
            panic!("effect application should produce an effect value");
        };
        let function = effect
            .get(&Key::atom_from_text("eff"))
            .expect("effect should contain an eff function")
            .clone();
        let api = Value::Dict(Dict::new_sync().insert(
            Key::atom_from_text("op"),
            Value::expr(Expr::lambda(Arc::new(builtin2_expr(
                Builtin::Add,
                Expr::Local(0),
                Expr::Value(n(1)),
            )))),
        ));

        let value = apply_value(eval_value(&function).unwrap(), api, &[])
            .expect("extended effect function should evaluate with an API");
        assert_eq!(value, n(42));
    }

    #[test]
    fn effect_application_requires_singleton_eff_tag() {
        let not_singleton = Value::Dict(
            Dict::new_sync()
                .insert(
                    Key::atom_from_text("eff"),
                    Value::expr(Expr::lambda(Arc::new(Expr::Local(0)))),
                )
                .insert(Key::atom_from_text("extra"), n(1)),
        );
        let err = eval_closed_expr(&Expr::Apply(
            Arc::new(Expr::Value(not_singleton)),
            Arc::new(Expr::Value(n(42))),
        ))
        .unwrap_err();

        assert_eq!(err.to_string(), "application requires a function value");
    }

    #[test]
    fn local_dictionary_paths_resolve_without_a_global_root() {
        let dict = Value::Dict(Dict::new_sync().insert(
            Key::atom_from_text("tail"),
            Value::binary_from_text("World"),
        ));
        let expr = Expr::Apply(
            Arc::new(Expr::lambda(Arc::new(Expr::Access(
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

        assert_eq!(
            list_output_bytes(&list).expect("should render resolved list"),
            b"Hello, World!"
        );
    }

    #[test]
    fn evaluates_keyable_values_into_keys() {
        let key = eval_key(&Value::List(List::concat(
            List::from_values(vec![n(1)]),
            List::from_bytes(Bytes::from_static(b"Hi")),
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
            key_path_expr(vec![key.clone()]),
            Expr::Value(Value::binary_from_text("World")),
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
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
            key_path_expr(vec![key.clone(), world.clone()]),
            Expr::Value(Value::binary_from_text("World")),
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(
                    key.clone(),
                    Value::Dict(
                        crate::core::Dict::new_sync()
                            .insert(hello.clone(), Value::binary_from_text("Hello")),
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
            key_path_expr(vec![key.clone()]),
            Expr::Value(Value::Dict(crate::core::Dict::new_sync())),
            Expr::Value(Value::Dict(
                crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
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
    fn list_annotations_rebalance_and_flatten_lists() {
        let deque = eval_closed_expr(&builtin2_expr(
            Builtin::Anno,
            Expr::Value(Value::Atom(crate::core::Atom::from_key(
                &Key::binary_from_text("deque"),
            ))),
            Expr::Value(Value::List(List::concat(
                List::from_bytes(Bytes::from_static(b"Hello")),
                List::from_values(vec![n(33)]),
            ))),
        ))
        .expect("deque annotation should evaluate");
        let Value::List(deque) = deque else {
            panic!("deque annotation should produce a list");
        };
        assert_eq!(deque.len(), 6);

        let binary = eval_closed_expr(&builtin2_expr(
            Builtin::Anno,
            Expr::Value(Value::Atom(crate::core::Atom::from_key(
                &Key::binary_from_text("binary"),
            ))),
            Expr::Value(Value::List(List::concat(
                List::from_values(vec![n(72), n(105)]),
                List::from_bytes(Bytes::from_static(b"!")),
            ))),
        ))
        .expect("binary annotation should evaluate");
        assert_eq!(binary, Value::binary_from_text("Hi!"));

        let array = eval_closed_expr(&builtin2_expr(
            Builtin::Anno,
            Expr::Value(Value::Atom(crate::core::Atom::from_key(
                &Key::binary_from_text("array"),
            ))),
            Expr::Value(Value::binary_from_text("Hi")),
        ))
        .expect("array annotation should evaluate");
        let Value::List(array) = array else {
            panic!("array annotation should produce a list");
        };
        assert_eq!(list_to_value_items(&array).unwrap(), vec![n(72), n(105)]);
    }

    #[test]
    fn list_annotations_return_stuck_errors_for_wrong_targets() {
        let value = eval_closed_expr(&builtin2_expr(
            Builtin::Anno,
            Expr::Value(Value::Atom(crate::core::Atom::from_key(
                &Key::binary_from_text("binary"),
            ))),
            Expr::Value(Value::List(List::from_values(vec![n(300)]))),
        ))
        .expect("annotation should evaluate to a stuck expression");

        assert_eq!(
            eval_value(&value).unwrap_err().to_string(),
            "`binary` annotation cannot encode number `300` as a byte"
        );

        let value = eval_closed_expr(&builtin2_expr(
            Builtin::Anno,
            Expr::Value(Value::Atom(crate::core::Atom::from_key(
                &Key::binary_from_text("deque"),
            ))),
            Expr::Value(n(1)),
        ))
        .expect("annotation should evaluate to a stuck expression");

        assert!(
            eval_value(&value)
                .unwrap_err()
                .to_string()
                .contains("`deque` annotation requires a list target")
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
            Value::PartialBuiltin(call) => {
                assert_eq!(call.builtin, Builtin::Append);
                assert_eq!(call.arguments.len(), 1);
                assert!(matches!(&call.arguments[0], Value::Expr(_)));
            }
            other => panic!("expected partial builtin, got {other:?}"),
        }
    }
}
