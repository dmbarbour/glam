use super::*;
use crate::interaction_net::{DemandEndpoint, FrontierObservation, FrontierObservationStatus};

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

pub(super) fn extract_net_data(
    context: &EvalContext,
    runtime: crate::core_net::CoreRuntimeNet,
    interface: Port,
    operation: &str,
) -> Result<Value, EvaluationHalt> {
    match drive_net_interface(context, &runtime, interface)? {
        NetInterfaceOutcome::Data => {
            let data = runtime
                .with(|runtime| runtime.interface_data(interface).cloned())
                .expect("evaluated interaction-net interface must contain data");
            // Extract exactly one Data payload. If it is lazy, the caller may
            // force it after the enclosing net result has been memoized;
            // forcing here can re-enter a productive fixpoint runtime.
            Ok(data)
        }
        NetInterfaceOutcome::Bind => Err(EvaluationHalt::new(format!(
            "{operation} exposed a bind instead of data"
        ))),
        NetInterfaceOutcome::NormalForm => Err(EvaluationHalt::new(format!(
            "{operation} reached a non-data normal form"
        ))),
    }
}

pub(super) fn evaluate_function_call(
    context: &EvalContext,
    function: &FunctionValue,
    arguments: &[Value],
) -> Result<Value, EvaluationHalt> {
    let net = attach_net_many(Value::Net(function.stage().clone()), arguments.to_vec());
    let runtime = net.into_runtime();
    let exposed = runtime.with(|runtime| runtime.exposed());
    extract_net_data(context, runtime, exposed, "function call")
}

pub(super) fn attach_function_stage(function: NetValue, arguments: Vec<Value>) -> NetValue {
    attach_net_many(Value::Net(function), arguments)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetInterfaceOutcome {
    Data,
    Bind,
    NormalForm,
}

fn drive_net_interface(
    context: &EvalContext,
    runtime: &crate::core_net::CoreRuntimeNet,
    interface: Port,
) -> Result<NetInterfaceOutcome, EvaluationHalt> {
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

        if let Some(progress) = runtime.with_optional_mut(|net| net.demand_interface(interface)) {
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
                && progress_cursor_dependency(context, runtime, cursor, 0)?
            {
                continue;
            }
        }

        if let Some(pair) = runtime.with(|net| net.interface_dependency(interface)) {
            if progress_exact_core_pair(context, runtime, pair, 0, None)? {
                continue;
            }
            if let Some(blocked) = runtime.with(|net| net.blocked_call(pair)) {
                return Err(EvaluationHalt::blocked(blocked.wait));
            }
            if let Some(blocked) = runtime.with(|net| net.blocked_operator_call(pair)) {
                return Err(EvaluationHalt::blocked(blocked.wait));
            }
        }

        let reduction = runtime.with_optional_mut(|net| net.reduce_next());
        if let Some(reduction) = reduction {
            handle_core_reduction(context, runtime, reduction)?;
            continue;
        }

        if progress_core_net(context, runtime)? {
            continue;
        }

        // Recheck the interface in the same locked observation used to declare
        // quiescence. Another evaluator may have completed an in-flight claim
        // after the checks at the top of the loop; observing only the now-empty
        // scheduler would otherwise misclassify its freshly published data as
        // a non-data normal form.
        let ((terminal, scheduler_is_empty, has_in_flight_claims), version) =
            runtime.with_version(|net| {
                let terminal = if net.interface_data(interface).is_some() {
                    Some(NetInterfaceOutcome::Data)
                } else if net.interface_neighbor(interface).is_some_and(|port| {
                    port.is_principal()
                        && matches!(
                            net.node(port.node()),
                            Some(crate::interaction_net::RuntimeNode::Bind)
                        )
                }) {
                    Some(NetInterfaceOutcome::Bind)
                } else {
                    None
                };
                (
                    terminal,
                    net.active_pairs().len() == 0,
                    net.has_in_flight_claims(),
                )
            });
        if let Some(terminal) = terminal {
            return Ok(terminal);
        }
        if scheduler_is_empty && !has_in_flight_claims {
            return Ok(NetInterfaceOutcome::NormalForm);
        }
        if has_in_flight_claims {
            runtime.wait_for_change(version);
            continue;
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
        return Err(EvaluationHalt::new(format!(
            "interaction net became quiescent before producing a value ({detail})"
        )));
    }
}

pub(super) fn progress_core_net(
    context: &EvalContext,
    runtime: &crate::core_net::CoreRuntimeNet,
) -> Result<bool, EvaluationHalt> {
    if let Some(reduction) = runtime.with_optional_mut(|net| net.reduce_next()) {
        handle_core_reduction(context, runtime, reduction)?;
        return Ok(true);
    }
    Ok(false)
}

pub(super) fn progress_cursor_dependency(
    context: &EvalContext,
    runtime: &crate::core_net::CoreRuntimeNet,
    cursor: crate::interaction_net::NodeId,
    depth: usize,
) -> Result<bool, EvaluationHalt> {
    if depth >= 1024 {
        return Err(EvaluationHalt::new(
            "interaction-net cursor dependency chain is too deep",
        ));
    }
    let Some(dependency) = runtime.with(|net| net.cursor_dependency(cursor)) else {
        return Ok(false);
    };
    match dependency {
        CursorDependency::LocalCursor(local_cursor) => {
            progress_dependent_cursor(context, runtime, local_cursor, depth)
        }
        CursorDependency::SourceCursor(observation) => {
            debug_assert!(matches!(observation.endpoint(), DemandEndpoint::Cursor(_)));
            progress_frontier_observation(context, observation, depth)
        }
        CursorDependency::SourceFrontier(observation) => {
            debug_assert!(matches!(
                observation.endpoint(),
                DemandEndpoint::ActivePair(_)
            ));
            progress_frontier_observation(context, observation, depth)
        }
    }
}

fn progress_frontier_observation(
    context: &EvalContext,
    observation: FrontierObservation<CoreSpecialization>,
    depth: usize,
) -> Result<bool, EvaluationHalt> {
    if observation.status() == FrontierObservationStatus::Disturbed {
        return Ok(true);
    }
    match observation.endpoint() {
        DemandEndpoint::Cursor(cursor) => {
            progress_observed_cursor(context, observation, cursor, depth)
        }
        DemandEndpoint::ActivePair(pair) => progress_exact_core_pair(
            context,
            observation.source(),
            pair,
            depth + 1,
            Some(&observation),
        ),
    }
}

fn progress_observed_cursor(
    context: &EvalContext,
    observation: FrontierObservation<CoreSpecialization>,
    cursor: crate::interaction_net::NodeId,
    depth: usize,
) -> Result<bool, EvaluationHalt> {
    let progress = match observation.claim_cursor(cursor) {
        Err(FrontierObservationStatus::Disturbed) => return Ok(true),
        Ok(Some(progress)) => progress,
        Ok(None) => {
            observation.wait_for_disturbance();
            return Ok(true);
        }
        Err(FrontierObservationStatus::Current) => {
            unreachable!("a current observation cannot fail validation")
        }
    };
    let runtime = observation.source();
    let progress = finish_core_cursor_claim(runtime, cursor, progress);
    if progress != crate::interaction_net::CursorProgress::Blocked {
        return Ok(true);
    }
    let progressed = progress_cursor_dependency(context, runtime, cursor, depth + 1)?;
    if progressed {
        runtime.with_mut(|source| source.retry_blocked_cursor(cursor));
    }
    Ok(progressed)
}

pub(super) fn progress_dependent_cursor(
    context: &EvalContext,
    runtime: &crate::core_net::CoreRuntimeNet,
    cursor: crate::interaction_net::NodeId,
    depth: usize,
) -> Result<bool, EvaluationHalt> {
    let progress = runtime.with_optional_mut(|source| source.claim_dependent_cursor(cursor));
    let progress = progress.map(|progress| finish_core_cursor_claim(runtime, cursor, progress));
    match progress {
        Some(crate::interaction_net::CursorProgress::Blocked) => {
            let progressed = progress_cursor_dependency(context, runtime, cursor, depth + 1)?;
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
    context: &EvalContext,
    runtime: &crate::core_net::CoreRuntimeNet,
    pair: ActivePairKey,
    depth: usize,
    observation: Option<&FrontierObservation<CoreSpecialization>>,
) -> Result<bool, EvaluationHalt> {
    let reduction = if let Some(observation) = observation {
        match observation.reduce_pair(pair) {
            Ok(reduction) => reduction,
            Err(FrontierObservationStatus::Disturbed) => return Ok(true),
            Err(FrontierObservationStatus::Current) => {
                unreachable!("a current observation cannot fail validation")
            }
        }
    } else {
        runtime.with_optional_mut(|net| net.reduce_pair(pair))
    };
    if let Some(reduction) = reduction {
        handle_core_reduction(context, runtime, reduction)?;
        return Ok(true);
    }
    let (claimed, version) = runtime.with_version(|net| net.pair_is_claimed(pair));
    if claimed {
        if let Some(observation) = observation {
            observation.wait_for_disturbance();
        } else {
            runtime.wait_for_change(version);
        }
        return Ok(true);
    }
    if !runtime.with(|net| net.contains_active_pair(pair)) {
        // Another evaluator completed this exact source pair between cursor
        // inspection and our claim attempt. Its disappearance is progress:
        // retry the dependent cursor against the updated source frontier.
        return Ok(true);
    }
    if let Some(blocked) = runtime.with(|net| net.blocked_cursor(pair)) {
        let progressed = progress_cursor_dependency(context, runtime, blocked.cursor, depth)?;
        if progressed {
            runtime.with_mut(|net| net.retry_blocked_cursor(blocked.cursor));
        }
        return Ok(progressed);
    }
    if let Some(blocked) = runtime.with(|net| net.blocked_call(pair)) {
        return match context.poll_wait(&blocked.wait.0) {
            crate::evaluation::EvaluationWaitPoll::Pending(_) => Ok(false),
            crate::evaluation::EvaluationWaitPoll::Complete(_)
            | crate::evaluation::EvaluationWaitPoll::Failed(_)
            | crate::evaluation::EvaluationWaitPoll::Cancelled
            | crate::evaluation::EvaluationWaitPoll::Abandoned
            | crate::evaluation::EvaluationWaitPoll::Exited
            | crate::evaluation::EvaluationWaitPoll::Killed(_) => {
                let call = runtime
                    .with(|net| net.call(pair))
                    .expect("blocked core call must remain a Bind >< Data pair");
                let reclaimed = runtime.with_mut(|net| net.retry_blocked_call(call, &blocked.wait));
                assert!(reclaimed, "matching blocked core call must be reclaimable");
                progress_exact_core_call(context, runtime, call)
            }
        };
    }
    if let Some(blocked) = runtime.with(|net| net.blocked_operator_call(pair)) {
        return match context.poll_wait(&blocked.wait.0) {
            crate::evaluation::EvaluationWaitPoll::Pending(_) => Ok(false),
            crate::evaluation::EvaluationWaitPoll::Complete(_)
            | crate::evaluation::EvaluationWaitPoll::Failed(_)
            | crate::evaluation::EvaluationWaitPoll::Cancelled
            | crate::evaluation::EvaluationWaitPoll::Abandoned
            | crate::evaluation::EvaluationWaitPoll::Exited
            | crate::evaluation::EvaluationWaitPoll::Killed(_) => {
                let call = runtime
                    .with(|net| net.operator_call(pair))
                    .expect("blocked core operator call must remain an Operator >< Data pair");
                let reclaimed =
                    runtime.with_mut(|net| net.retry_blocked_operator_call(call, &blocked.wait));
                assert!(
                    reclaimed,
                    "matching blocked core operator call must be reclaimable"
                );
                progress_core_operator_call(context, runtime, call)
            }
        };
    }
    if runtime.with(|net| net.stuck_reason(pair).is_some()) {
        return Err(stuck_pair_error(runtime, pair));
    }
    if observation
        .is_some_and(|observation| observation.status() == FrontierObservationStatus::Disturbed)
    {
        Ok(true)
    } else {
        Ok(false)
    }
}

impl NetSpecialization for CoreSpecialization {
    type Data = Value;
    type Operator = CoreOperator;
    type WaitToken = crate::core_net::CoreWaitToken;
    type StuckReason = EvaluationHalt;
}

pub(super) fn lower_core_callable(
    context: &EvalContext,
    value: Value,
) -> Result<Callable<CoreSpecialization>, EvaluationHalt> {
    let value = if matches!(value, Value::Lazy(_) | Value::Promised(_)) {
        eval_value(context, &value)?
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
        value @ (Value::Atom(_)
        | Value::Number(_)
        | Value::Binary(_)
        | Value::List(_)
        | Value::Function(_)
        | Value::Metadata(_)
        | Value::Opaque(_)) => Err(non_callable_error(&value)),
        Value::Lazy(_) | Value::Promised(_) => {
            unreachable!("callable value shell must be fully forced")
        }
    }
}

pub(super) fn progress_exact_core_call(
    context: &EvalContext,
    runtime: &crate::core_net::CoreRuntimeNet,
    call: Call,
) -> Result<bool, EvaluationHalt> {
    let Some(data) = runtime.with_mut(|runtime| runtime.claim_call(call)) else {
        return Ok(false);
    };
    match lower_core_callable(context, data) {
        Ok(Callable::Net(source)) => {
            runtime.with_mut(|runtime| runtime.resume_claimed_call_with_copy(call, source));
            Ok(true)
        }
        Ok(Callable::Operator(operator)) => {
            runtime.with_mut(|runtime| {
                runtime.resume_claimed_call_with_operator(call, operator);
            });
            Ok(true)
        }
        Err(error) => {
            let error = match retryable_evaluation_wait(context, &error) {
                Ok(Some(wait)) => {
                    runtime.with_mut(|runtime| runtime.block_claimed_call(call, wait));
                    return Ok(true);
                }
                Ok(None) => error,
                Err(error) => error,
            };
            runtime.with_mut(|runtime| runtime.fail_claimed_call(call, error.clone()));
            Err(error)
        }
    }
}

pub(super) fn handle_core_reduction(
    context: &EvalContext,
    runtime: &crate::core_net::CoreRuntimeNet,
    reduction: Reduction,
) -> Result<(), EvaluationHalt> {
    match reduction.kind {
        ReductionKind::Stuck => Err(stuck_pair_error(runtime, reduction.pair)),
        ReductionKind::Call { bind, data } => {
            let call = Call {
                pair: reduction.pair,
                bind,
                data,
            };
            if !progress_exact_core_call(context, runtime, call)? {
                return Err(EvaluationHalt::new("interaction-net call lost its claim"));
            }
            Ok(())
        }
        ReductionKind::OperatorCall { operator, data } => {
            let call = OperatorCall {
                pair: reduction.pair,
                operator,
                data,
            };
            if !progress_core_operator_call(context, runtime, call)? {
                return Err(EvaluationHalt::new(
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
            if progress_cursor_dependency(context, runtime, cursor, 0)? {
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
) -> EvaluationHalt {
    runtime.with(|net| {
        let reason = net.stuck_reason(pair);
        match reason {
            Some(StuckReason::Specialization(error)) => error.clone(),
            Some(StuckReason::NoRule) | None => match net.active_pair_nodes(pair) {
                Some((left, right)) => EvaluationHalt::new(format!(
                    "interaction net reached a stuck active pair: {:?} >< {:?}",
                    net.node(left),
                    net.node(right)
                )),
                None => EvaluationHalt::new("interaction net reached a stale stuck active pair"),
            },
        }
    })
}

pub(super) fn progress_core_operator_call(
    context: &EvalContext,
    runtime: &crate::core_net::CoreRuntimeNet,
    call: OperatorCall,
) -> Result<bool, EvaluationHalt> {
    let (operator, data) = runtime.with(|net| net.operator_call_parts(call));
    match apply_core_operator(context, &operator, &data) {
        Ok(result) => runtime.with_mut(|net| {
            net.complete_operator_call(call, result);
        }),
        Err(error) => {
            let error = match retryable_evaluation_wait(context, &error) {
                Ok(Some(wait)) => {
                    runtime.with_mut(|net| net.block_claimed_operator_call(call, wait));
                    return Ok(true);
                }
                Ok(None) => error,
                Err(error) => error,
            };
            // Core operator errors already identify the failed semantic
            // operation. Preserve that structured error while retaining
            // the operator itself in the stuck pair for runtime inspection.
            runtime.with_mut(|net| {
                net.fail_operator_call(call, error.clone());
            });
            return Err(error);
        }
    }
    Ok(true)
}

fn retryable_evaluation_wait(
    context: &EvalContext,
    error: &EvaluationHalt,
) -> Result<Option<crate::core_net::CoreWaitToken>, EvaluationHalt> {
    if let Some(wait) = error.blocked_on() {
        return Ok(Some(wait));
    }
    let Some(promise) = error.unassigned_promise() else {
        return Ok(None);
    };
    promise_wait(context, promise)
        .map(crate::core_net::CoreWaitToken)
        .map(Some)
        .map_err(|error| EvaluationHalt::new(error.as_ref()))
}

pub(super) fn resolve_core_access(
    context: &EvalContext,
    arguments: &[Value],
    path: &[CoreDataKey],
) -> Result<Value, EvaluationHalt> {
    let mut current = arguments
        .first()
        .cloned()
        .ok_or_else(|| EvaluationHalt::new("value access is missing its base value"))?;
    let mut dynamic = arguments[1..].iter();
    for part in path {
        let keys = match part {
            CoreDataKey::Key(key) => vec![key.clone()],
            CoreDataKey::Index => {
                let value = dynamic.next().expect("lowered access index must exist");
                let value = eval_value(context, value)?;
                vec![value_to_key(context, &value)?]
            }
            CoreDataKey::PathIndex => eval_key_path_list(
                context,
                dynamic
                    .next()
                    .expect("lowered access path index must exist"),
            )?,
        };
        for key in keys {
            let value = eval_value(context, &current)?;
            let Value::Dict(dict) = value else {
                return Err(EvaluationHalt::new("value access base is not a dictionary"));
            };
            current = dict
                .get(&key)
                .cloned()
                .unwrap_or_else(|| Value::Dict(crate::core::Dict::new_sync()));
        }
    }
    eval_value(context, &current)
}
