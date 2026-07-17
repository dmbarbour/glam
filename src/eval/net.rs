use super::*;

pub(super) fn apply_net(function: NetValue, argument: Value) -> Result<Value, EvalError> {
    apply_explicit_net_many(function, vec![argument])
}

pub(super) fn apply_explicit_net_many(
    function: NetValue,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    assert!(
        !arguments.is_empty(),
        "batched net application requires an argument"
    );
    let net = attach_net_many(Value::Net(function), arguments);
    let runtime = net.into_runtime();
    let exposed = runtime.with(|net| net.exposed());
    finish_explicit_net_application(runtime, exposed)
}

pub(super) fn attach_net_many(function: Value, arguments: Vec<Value>) -> NetValue {
    assert!(!arguments.is_empty(), "net attachment requires an argument");
    let mut net = NetBuilder::new();
    let spine = net.bind_spine(arguments.len());
    let function = net.data(function);
    net.wire(spine.input, function);
    for (argument_port, argument) in spine.arguments.into_iter().zip(arguments) {
        let argument = net.data(argument);
        net.wire(argument_port, argument);
    }
    NetValue::new(net.finish(spine.result).instantiate_shared())
}

pub(super) fn observe_net(net: NetValue) -> Result<Value, EvalError> {
    let runtime = net.runtime().clone();
    let exposed = runtime.with(|runtime| runtime.exposed());
    match drive_net_interface(&runtime, exposed)? {
        NetInterfaceOutcome::Data => {
            let data = runtime
                .with(|runtime| runtime.interface_data(exposed).cloned())
                .expect("observed interaction-net interface must contain data");
            Ok(data)
        }
        NetInterfaceOutcome::Bind | NetInterfaceOutcome::NormalForm => Ok(Value::Net(net)),
    }
}

pub(super) fn extract_net_data(
    runtime: crate::core_net::CoreRuntimeNet,
    interface: Port,
    operation: &str,
) -> Result<Value, EvalError> {
    match drive_net_interface(&runtime, interface)? {
        NetInterfaceOutcome::Data => {
            let data = runtime
                .with(|runtime| runtime.interface_data(interface).cloned())
                .expect("evaluated interaction-net interface must contain data");
            // Extract exactly one Data payload. If it is lazy, the caller may
            // force it after the enclosing net result has been memoized;
            // forcing here can re-enter a productive fixpoint runtime.
            Ok(data)
        }
        NetInterfaceOutcome::Bind => Err(EvalError::new(format!(
            "{operation} exposed a bind instead of data"
        ))),
        NetInterfaceOutcome::NormalForm => Err(EvalError::new(format!(
            "{operation} reached a non-data normal form"
        ))),
    }
}

pub(super) fn finish_explicit_net_application(
    runtime: crate::core_net::CoreRuntimeNet,
    interface: Port,
) -> Result<Value, EvalError> {
    match drive_net_interface(&runtime, interface)? {
        NetInterfaceOutcome::Data => {
            let data = runtime
                .with(|runtime| runtime.interface_data(interface).cloned())
                .expect("applied interaction-net interface must contain data");
            Ok(data)
        }
        NetInterfaceOutcome::Bind => Ok(Value::Net(NetValue::new(runtime))),
        NetInterfaceOutcome::NormalForm => Err(EvalError::new(
            "interaction-net application reached a non-data normal form without exposing a bind",
        )),
    }
}

pub(super) fn evaluate_function_call(
    function: &FunctionValue,
    arguments: &[Value],
) -> Result<Value, EvalError> {
    let net = attach_net_many(Value::Net(function.stage().clone()), arguments.to_vec());
    let runtime = net.into_runtime();
    let exposed = runtime.with(|runtime| runtime.exposed());
    extract_net_data(runtime, exposed, "function call")
}

pub(super) fn advance_function_stage(
    function: NetValue,
    arguments: Vec<Value>,
) -> Result<NetValue, EvalError> {
    let net = attach_net_many(Value::Net(function), arguments);
    let runtime = net.into_runtime();
    let exposed = runtime.with(|runtime| runtime.exposed());
    match drive_net_interface(&runtime, exposed)? {
        NetInterfaceOutcome::Bind => Ok(NetValue::new(runtime)),
        NetInterfaceOutcome::Data => Err(EvalError::new(
            "partial function stage produced data before exposing its next bind",
        )),
        NetInterfaceOutcome::NormalForm => Err(EvalError::new(
            "partial function stage reached a non-data normal form without exposing its next bind",
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetInterfaceOutcome {
    Data,
    Bind,
    NormalForm,
}

fn drive_net_interface(
    runtime: &crate::core_net::CoreRuntimeNet,
    interface: Port,
) -> Result<NetInterfaceOutcome, EvalError> {
    loop {
        if runtime.with(|net| net.interface_data(interface).is_some()) {
            return Ok(NetInterfaceOutcome::Data);
        }

        let exposes_bind = runtime.with(|net| {
            net.interface_neighbor(interface).is_some_and(|port| {
                port.is_principal()
                    && matches!(
                        net.node(port.node()),
                        Some(crate::interaction_net::RuntimeNode::Bind)
                    )
            })
        });
        if exposes_bind {
            return Ok(NetInterfaceOutcome::Bind);
        }

        if let Some(progress) = runtime.with_mut(|net| net.demand_interface(interface)) {
            let cursor = runtime.with(|net| net.interface_cursor(interface));
            let progress = finish_core_cursor_claim(
                runtime,
                cursor.expect("demanded interface cursor must exist"),
                progress,
            );
            if !matches!(progress, crate::interaction_net::CursorProgress::Blocked) {
                continue;
            }
            if let Some(cursor) = cursor
                && progress_cursor_dependency(runtime, cursor, 0)?
            {
                continue;
            }
        }

        if let Some(pair) = runtime.with(|net| net.interface_dependency(interface))
            && let Some(reduction) = runtime.with_mut(|net| net.reduce_pair(pair))
        {
            handle_core_reduction(runtime, reduction)?;
            continue;
        }

        let reduction = runtime.with_mut(|net| net.reduce_next());
        if let Some(reduction) = reduction {
            handle_core_reduction(runtime, reduction)?;
            continue;
        }

        if progress_core_net(runtime)? {
            continue;
        }

        let scheduler_is_empty = runtime.with(|net| net.active_pairs().len() == 0);
        if scheduler_is_empty {
            return Ok(NetInterfaceOutcome::NormalForm);
        }

        let detail = runtime.with(|net| {
            let neighbor = net.interface_neighbor(interface);
            let node = neighbor.and_then(|port| net.node(port.node()));
            let principal_neighbor = neighbor
                .and_then(|port| net.port_neighbor(Port::principal(port.node())));
            let principal_neighbor_node =
                principal_neighbor.and_then(|port| net.node(port.node()));
            let cursor_dependencies = net
                .blocked_cursors()
                .values()
                .map(|blocked| {
                    (
                        blocked.cursor,
                        net.cursor_dependency(blocked.cursor),
                    )
                })
                .collect::<Vec<_>>();
            format!(
                "neighbor={neighbor:?}, node={node:?}, principal_neighbor={principal_neighbor:?}/{principal_neighbor_node:?}, active={}, cursors={cursor_dependencies:?}, stuck={}",
                net.active_pairs().len(),
                net.stuck_pairs().count()
            )
        });
        return Err(EvalError::new(format!(
            "interaction net became quiescent before producing a value ({detail})"
        )));
    }
}

pub(super) fn progress_core_net(
    runtime: &crate::core_net::CoreRuntimeNet,
) -> Result<bool, EvalError> {
    if let Some(reduction) = runtime.with_mut(|net| net.reduce_next()) {
        handle_core_reduction(runtime, reduction)?;
        return Ok(true);
    }
    Ok(false)
}

pub(super) fn progress_cursor_dependency(
    runtime: &crate::core_net::CoreRuntimeNet,
    cursor: crate::interaction_net::NodeId,
    depth: usize,
) -> Result<bool, EvalError> {
    if depth >= 1024 {
        return Err(EvalError::new(
            "interaction-net cursor dependency chain is too deep",
        ));
    }
    let Some(dependency) = runtime.with(|net| net.cursor_dependency(cursor)) else {
        return Ok(false);
    };
    match dependency {
        CursorDependency::LocalCursor(local_cursor) => {
            progress_dependent_cursor(runtime, local_cursor, depth)
        }
        CursorDependency::SourceCursor {
            source,
            cursor: source_cursor,
        } => progress_dependent_cursor(&source, source_cursor, depth),
        CursorDependency::SourcePair { source, pair } => {
            progress_exact_core_pair(&source, pair, depth + 1)
        }
    }
}

pub(super) fn progress_dependent_cursor(
    runtime: &crate::core_net::CoreRuntimeNet,
    cursor: crate::interaction_net::NodeId,
    depth: usize,
) -> Result<bool, EvalError> {
    let progress = runtime.with_mut(|source| source.claim_dependent_cursor(cursor));
    let progress = progress.map(|progress| finish_core_cursor_claim(runtime, cursor, progress));
    match progress {
        Some(crate::interaction_net::CursorProgress::Blocked) => {
            let progressed = progress_cursor_dependency(runtime, cursor, depth + 1)?;
            if progressed {
                runtime.with_mut(|source| source.retry_blocked_cursor(cursor));
            }
            Ok(progressed)
        }
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

pub(super) fn progress_exact_core_pair(
    runtime: &crate::core_net::CoreRuntimeNet,
    pair: ActivePairKey,
    depth: usize,
) -> Result<bool, EvalError> {
    if let Some(reduction) = runtime.with_mut(|net| net.reduce_pair(pair)) {
        handle_core_reduction(runtime, reduction)?;
        return Ok(true);
    }
    if let Some(blocked) = runtime.with(|net| net.blocked_cursor(pair)) {
        let progressed = progress_cursor_dependency(runtime, blocked.cursor, depth)?;
        if progressed {
            runtime.with_mut(|net| net.retry_blocked_cursor(blocked.cursor));
        }
        return Ok(progressed);
    }
    if runtime.with(|net| net.stuck_reason(pair).is_some()) {
        return Err(stuck_pair_error(runtime, pair));
    }
    Ok(false)
}

impl NetSpecialization for CoreSpecialization {
    type Data = Value;
    type Operator = CoreOperator;
    type Error = EvalError;

    fn callable(data: Self::Data) -> Result<Callable<Self>, Self::Error> {
        lower_core_callable(data)
    }

    fn apply_operator(
        operator: &Self::Operator,
        data: &Self::Data,
    ) -> Result<OperatorYield<Self>, Self::Error> {
        apply_core_operator(operator, data)
    }

    fn operator_name(operator: &Self::Operator) -> &str {
        match operator {
            CoreOperator::ApplyArity { .. } => "semantic apply",
            CoreOperator::FunctionCaptures { .. } => "function captures",
            CoreOperator::ComputationCaptures { .. } => "computation captures",
            CoreOperator::Dict { .. } => "dictionary literal",
            CoreOperator::Builtin(_) => "builtin",
            CoreOperator::Applicable(_) => "core applicable",
            CoreOperator::List { .. } => "list literal",
            CoreOperator::Access { .. } => "dictionary access",
            CoreOperator::Request { .. } => "effect request",
        }
    }
}

pub(super) fn lower_core_callable(value: Value) -> Result<Callable<CoreSpecialization>, EvalError> {
    let value = if matches!(value, Value::Lazy(_)) {
        force_value_shell(&value)?
    } else {
        value
    };
    match value {
        Value::Net(net) => Ok(Callable::Net(net.into_runtime())),
        Value::Builtin(builtin) => Ok(Callable::Operator(builtin_operator(BuiltinCall::new(
            builtin,
        )))),
        Value::PartialBuiltin(call) => Ok(Callable::Operator(builtin_operator(call))),
        value @ Value::Dict(_) => Ok(Callable::Operator(applicable_operator(value))),
        Value::Atom(_)
        | Value::Number(_)
        | Value::Binary(_)
        | Value::List(_)
        | Value::Function(_) => Err(EvalError::new("application requires a function value")),
        Value::Lazy(_) => unreachable!("callable value shell must be fully forced"),
    }
}

pub(super) fn progress_exact_core_call(
    runtime: &crate::core_net::CoreRuntimeNet,
    call: Call,
) -> Result<bool, EvalError> {
    runtime.resolve_call(call)
}

pub(super) fn handle_core_reduction(
    runtime: &crate::core_net::CoreRuntimeNet,
    reduction: Reduction,
) -> Result<(), EvalError> {
    match reduction.kind {
        ReductionKind::Stuck => Err(stuck_pair_error(runtime, reduction.pair)),
        ReductionKind::Call { bind, data } => {
            let call = Call {
                pair: reduction.pair,
                bind,
                data,
            };
            if !progress_exact_core_call(runtime, call)? {
                return Err(EvalError::new("interaction-net call lost its claim"));
            }
            Ok(())
        }
        ReductionKind::OperatorCall { operator, data } => {
            let call = OperatorCall {
                pair: reduction.pair,
                operator,
                data,
            };
            if !progress_core_operator_call(runtime, call)? {
                return Err(EvalError::new(
                    "interaction-net operator call lost its claim",
                ));
            }
            Ok(())
        }
        ReductionKind::RemoteCursor { cursor, progress } => {
            let progress = finish_core_cursor_claim(runtime, cursor, progress);
            if progress != crate::interaction_net::CursorProgress::Blocked {
                return Ok(());
            }
            if progress_cursor_dependency(runtime, cursor, 0)? {
                runtime.with_mut(|net| net.retry_blocked_cursor(cursor));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(super) fn finish_core_cursor_claim(
    runtime: &crate::core_net::CoreRuntimeNet,
    cursor: crate::interaction_net::NodeId,
    progress: crate::interaction_net::CursorProgress,
) -> crate::interaction_net::CursorProgress {
    if progress == crate::interaction_net::CursorProgress::Claimed {
        runtime
            .advance_claimed_cursor(cursor)
            .expect("claimed cursor must advance")
    } else {
        progress
    }
}

pub(super) fn stuck_pair_error(
    runtime: &crate::core_net::CoreRuntimeNet,
    pair: ActivePairKey,
) -> EvalError {
    runtime.with(|net| {
        let reason = net.stuck_reason(pair);
        match reason {
            Some(StuckReason::SpecializationError(error)) => EvalError::new(error.as_ref()),
            Some(StuckReason::NoRule) | None => match net.active_pair_nodes(pair) {
                Some((left, right)) => EvalError::new(format!(
                    "interaction net reached a stuck active pair: {:?} >< {:?}",
                    net.node(left),
                    net.node(right)
                )),
                None => EvalError::new("interaction net reached a stale stuck active pair"),
            },
        }
    })
}

pub(super) fn progress_core_operator_call(
    runtime: &crate::core_net::CoreRuntimeNet,
    call: OperatorCall,
) -> Result<bool, EvalError> {
    let (operator, data) = runtime.with(|net| net.operator_call_parts(call));
    match CoreSpecialization::apply_operator(&operator, &data) {
        Ok(result) => runtime.with_mut(|net| {
            net.complete_operator_call(call, result);
        }),
        Err(error) => {
            // Core operator errors already identify the failed semantic
            // operation. Preserve that diagnostic verbatim while retaining
            // the operator itself in the stuck pair for runtime inspection.
            let error: Arc<str> = error.to_string().into();
            runtime.with_mut(|net| {
                net.fail_operator_call(call, error.clone());
            });
            return Err(EvalError::new(error.as_ref()));
        }
    }
    Ok(true)
}

pub(super) fn resolve_core_access(
    arguments: &[Value],
    path: &[CoreDataKey],
) -> Result<Value, EvalError> {
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
                vec![value_to_key(&value)?]
            }
            CoreDataKey::PathIndex => eval_key_path_list(
                dynamic
                    .next()
                    .expect("lowered access path index must exist"),
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
