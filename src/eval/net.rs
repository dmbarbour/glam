use super::*;
use crate::interaction_net::{
    ActivePairStep, CursorStep, DemandEndpoint, FrontierObservation, NetContention,
    NormalizationBatchLease,
};

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
    let request = NormalizationRequest::cursor_whnf(runtime.clone(), interface);
    match request.drive(context)? {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NormalizationMode {
    CursorWhnf,
}

/// Evaluator-owned description of one demanded interaction-net frontier.
/// Shared progress remains in the net's cursor obligations; cloning or
/// dropping this descriptor has no runtime lifecycle effect.
struct NormalizationRequest {
    runtime: crate::core_net::CoreRuntimeNet,
    root_interface: Port,
    mode: NormalizationMode,
}

/// One evaluator-owned unit of cursor-WHNF work. These descriptors carry no
/// claim or lifecycle ownership; authoritative progress remains in the shared
/// runtime nets and can be reconstructed from `RequestRoot` after a retry.
#[derive(Clone)]
enum NetDriverWork {
    RequestRoot {
        runtime: crate::core_net::CoreRuntimeNet,
        interface: Port,
    },
    Cursor {
        runtime: crate::core_net::CoreRuntimeNet,
        cursor: crate::interaction_net::NodeId,
    },
    ObservedCursor {
        observation: FrontierObservation<CoreSpecialization>,
        cursor: crate::interaction_net::NodeId,
    },
    ActivePair {
        runtime: crate::core_net::CoreRuntimeNet,
        pair: ActivePairKey,
    },
    ObservedActivePair {
        observation: FrontierObservation<CoreSpecialization>,
        pair: ActivePairKey,
    },
    RetryCursor {
        runtime: crate::core_net::CoreRuntimeNet,
        cursor: crate::interaction_net::NodeId,
    },
}

#[derive(Default)]
struct NetDriverWorklist {
    items: Vec<NetDriverWork>,
}

impl NetDriverWorklist {
    fn request_root(runtime: crate::core_net::CoreRuntimeNet, interface: Port) -> Self {
        Self {
            items: vec![NetDriverWork::RequestRoot { runtime, interface }],
        }
    }

    fn pop(&mut self) -> Option<NetDriverWork> {
        self.items.pop()
    }

    fn push(&mut self, work: NetDriverWork) {
        self.items.push(work);
    }

    fn follow_cursor_dependency(
        &mut self,
        runtime: crate::core_net::CoreRuntimeNet,
        cursor: crate::interaction_net::NodeId,
        dependency: CursorDependency<CoreSpecialization>,
    ) {
        self.push(NetDriverWork::RetryCursor {
            runtime: runtime.clone(),
            cursor,
        });
        self.push(match dependency {
            CursorDependency::LocalCursor(cursor) => NetDriverWork::Cursor { runtime, cursor },
            CursorDependency::SourceCursor(observation) => {
                let DemandEndpoint::Cursor(cursor) = observation.endpoint() else {
                    unreachable!("source-cursor dependency must expose a cursor")
                };
                NetDriverWork::ObservedCursor {
                    observation,
                    cursor,
                }
            }
            CursorDependency::SourceFrontier(observation) => {
                let DemandEndpoint::ActivePair(pair) = observation.endpoint() else {
                    unreachable!("source-frontier dependency must expose an active pair")
                };
                NetDriverWork::ObservedActivePair { observation, pair }
            }
        });
    }
}

impl NetDriverWork {
    fn runtime(&self) -> &crate::core_net::CoreRuntimeNet {
        match self {
            Self::RequestRoot { runtime, .. }
            | Self::Cursor { runtime, .. }
            | Self::ActivePair { runtime, .. }
            | Self::RetryCursor { runtime, .. } => runtime,
            Self::ObservedCursor { observation, .. }
            | Self::ObservedActivePair { observation, .. } => observation.source(),
        }
    }
}

enum NetDriverOutcome {
    Progressed,
    Stable,
    Contended(NetContention<CoreSpecialization>),
}

struct NetDriver {
    worklist: NetDriverWorklist,
    progressed: bool,
    batch_runtime: Option<crate::core_net::CoreRuntimeNet>,
    batch_lease: Option<NormalizationBatchLease<CoreSpecialization>>,
}

impl NetDriver {
    fn new(initial: NetDriverWork) -> Self {
        let worklist = match initial {
            NetDriverWork::RequestRoot { runtime, interface } => {
                NetDriverWorklist::request_root(runtime, interface)
            }
            work => {
                let mut worklist = NetDriverWorklist::default();
                worklist.push(work);
                worklist
            }
        };
        Self {
            worklist,
            progressed: false,
            batch_runtime: None,
            batch_lease: None,
        }
    }

    fn close_batch(&mut self) {
        if let Some(lease) = self.batch_lease.take() {
            lease.close();
        }
        self.batch_runtime = None;
    }
}

fn drive_net_work(
    context: &EvalContext,
    initial: NetDriverWork,
) -> Result<NetDriverOutcome, EvaluationHalt> {
    let mut driver = NetDriver::new(initial);
    while let Some(work) = driver.worklist.pop() {
        let work_runtime = work.runtime().clone();
        if driver
            .batch_runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.ptr_eq(&work_runtime))
        {
            driver.close_batch();
        }
        if driver.batch_lease.is_none() {
            match work_runtime.try_begin_normalization_batch() {
                Ok(lease) => {
                    driver.batch_runtime = Some(work_runtime);
                    driver.batch_lease = Some(lease);
                }
                Err(contention) => return Ok(NetDriverOutcome::Contended(contention)),
            }
        }
        match work {
            NetDriverWork::RequestRoot { runtime, interface } => {
                runtime.ensure_interface_cursor_obligation(interface);
                if let Some(cursor) = runtime.with(|net| net.interface_cursor(interface)) {
                    driver.worklist.push(NetDriverWork::RequestRoot {
                        runtime: runtime.clone(),
                        interface,
                    });
                    driver
                        .worklist
                        .push(NetDriverWork::Cursor { runtime, cursor });
                } else if let Some(pair) = runtime.with(|net| net.interface_dependency(interface)) {
                    driver.worklist.push(NetDriverWork::RequestRoot {
                        runtime: runtime.clone(),
                        interface,
                    });
                    driver
                        .worklist
                        .push(NetDriverWork::ActivePair { runtime, pair });
                }
            }
            NetDriverWork::Cursor { runtime, cursor } => match runtime.step_cursor(cursor) {
                CursorStep::Progressed(progress) => {
                    debug_assert_ne!(progress, crate::interaction_net::CursorProgress::Claimed);
                    driver.progressed = true;
                }
                CursorStep::Disturbed | CursorStep::Gone => {
                    driver.progressed = true;
                }
                CursorStep::Dependency(dependency) => {
                    driver
                        .worklist
                        .follow_cursor_dependency(runtime, cursor, dependency);
                }
                CursorStep::Stable => return Ok(NetDriverOutcome::Stable),
                CursorStep::Contended(contention) => {
                    return Ok(NetDriverOutcome::Contended(contention));
                }
            },
            NetDriverWork::ObservedCursor {
                observation,
                cursor,
            } => match observation.step_cursor(cursor) {
                CursorStep::Progressed(progress) => {
                    debug_assert_ne!(progress, crate::interaction_net::CursorProgress::Claimed);
                    driver.progressed = true;
                }
                CursorStep::Disturbed | CursorStep::Gone => {
                    driver.progressed = true;
                }
                CursorStep::Dependency(dependency) => {
                    driver.worklist.follow_cursor_dependency(
                        observation.source().clone(),
                        cursor,
                        dependency,
                    );
                }
                CursorStep::Stable => return Ok(NetDriverOutcome::Stable),
                CursorStep::Contended(contention) => {
                    return Ok(NetDriverOutcome::Contended(contention));
                }
            },
            NetDriverWork::ActivePair { runtime, pair } => {
                let step = runtime.step_active_pair(pair);
                if let Some(contention) =
                    drive_active_pair_step(context, &mut driver, runtime, pair, step)?
                {
                    return Ok(NetDriverOutcome::Contended(contention));
                }
            }
            NetDriverWork::ObservedActivePair { observation, pair } => {
                let step = observation.step_active_pair(pair);
                if let Some(contention) = drive_active_pair_step(
                    context,
                    &mut driver,
                    observation.source().clone(),
                    pair,
                    step,
                )? {
                    return Ok(NetDriverOutcome::Contended(contention));
                }
            }
            NetDriverWork::RetryCursor { runtime, cursor } => {
                if runtime.with_mut(|net| net.retry_blocked_cursor(cursor)) {
                    driver.progressed = true;
                }
            }
        }
    }
    Ok(if driver.progressed {
        NetDriverOutcome::Progressed
    } else {
        NetDriverOutcome::Stable
    })
}

fn drive_active_pair_step(
    context: &EvalContext,
    driver: &mut NetDriver,
    runtime: crate::core_net::CoreRuntimeNet,
    pair: ActivePairKey,
    step: ActivePairStep<CoreSpecialization>,
) -> Result<Option<NetContention<CoreSpecialization>>, EvaluationHalt> {
    match step {
        ActivePairStep::Reduction(reduction) => {
            driver.progressed = true;
            match reduction.kind {
                ReductionKind::Stuck => return Err(stuck_pair_error(&runtime, pair)),
                ReductionKind::Call { bind, data } => {
                    driver.close_batch();
                    let call = Call { pair, bind, data };
                    if !progress_exact_core_call(context, &runtime, call)? {
                        return Err(EvaluationHalt::new("interaction-net call lost its claim"));
                    }
                    driver
                        .worklist
                        .push(NetDriverWork::ActivePair { runtime, pair });
                }
                ReductionKind::OperatorCall { operator, data } => {
                    driver.close_batch();
                    let call = OperatorCall {
                        pair,
                        operator,
                        data,
                    };
                    if !progress_core_operator_call(context, &runtime, call)? {
                        return Err(EvaluationHalt::new(
                            "interaction-net operator call lost its claim",
                        ));
                    }
                    driver
                        .worklist
                        .push(NetDriverWork::ActivePair { runtime, pair });
                }
                ReductionKind::RemoteCursor { cursor, progress } => {
                    let progress = finish_core_cursor_claim(&runtime, cursor, progress);
                    if progress == crate::interaction_net::CursorProgress::Blocked {
                        driver
                            .worklist
                            .push(NetDriverWork::Cursor { runtime, cursor });
                    }
                }
                _ => {}
            }
        }
        ActivePairStep::Cursor(cursor) => {
            driver
                .worklist
                .push(NetDriverWork::Cursor { runtime, cursor });
        }
        ActivePairStep::BlockedCall(blocked) => {
            driver.close_batch();
            match context.poll_wait(&blocked.wait.0) {
                crate::evaluation::EvaluationWaitPoll::Pending(_) => {
                    return Err(EvaluationHalt::blocked(blocked.wait));
                }
                crate::evaluation::EvaluationWaitPoll::Complete(_)
                | crate::evaluation::EvaluationWaitPoll::Failed(_)
                | crate::evaluation::EvaluationWaitPoll::Cancelled
                | crate::evaluation::EvaluationWaitPoll::Abandoned
                | crate::evaluation::EvaluationWaitPoll::Exited
                | crate::evaluation::EvaluationWaitPoll::Killed(_) => {}
            }
            let call = runtime
                .with(|net| net.call(pair))
                .expect("blocked core call must remain a Bind >< Data pair");
            let reclaimed = runtime.with_mut(|net| net.retry_blocked_call(call, &blocked.wait));
            assert!(reclaimed, "matching blocked core call must be reclaimable");
            if !progress_exact_core_call(context, &runtime, call)? {
                return Err(EvaluationHalt::new(
                    "interaction-net call lost its reclaimed claim",
                ));
            }
            driver.progressed = true;
            driver
                .worklist
                .push(NetDriverWork::ActivePair { runtime, pair });
        }
        ActivePairStep::BlockedOperatorCall(blocked) => {
            driver.close_batch();
            match context.poll_wait(&blocked.wait.0) {
                crate::evaluation::EvaluationWaitPoll::Pending(_) => {
                    return Err(EvaluationHalt::blocked(blocked.wait));
                }
                crate::evaluation::EvaluationWaitPoll::Complete(_)
                | crate::evaluation::EvaluationWaitPoll::Failed(_)
                | crate::evaluation::EvaluationWaitPoll::Cancelled
                | crate::evaluation::EvaluationWaitPoll::Abandoned
                | crate::evaluation::EvaluationWaitPoll::Exited
                | crate::evaluation::EvaluationWaitPoll::Killed(_) => {}
            }
            let call = runtime
                .with(|net| net.operator_call(pair))
                .expect("blocked core operator call must remain an Operator >< Data pair");
            let reclaimed =
                runtime.with_mut(|net| net.retry_blocked_operator_call(call, &blocked.wait));
            assert!(
                reclaimed,
                "matching blocked core operator call must be reclaimable"
            );
            if !progress_core_operator_call(context, &runtime, call)? {
                return Err(EvaluationHalt::new(
                    "interaction-net operator call lost its reclaimed claim",
                ));
            }
            driver.progressed = true;
            driver
                .worklist
                .push(NetDriverWork::ActivePair { runtime, pair });
        }
        ActivePairStep::Stuck(_) => return Err(stuck_pair_error(&runtime, pair)),
        ActivePairStep::Contended(contention) => return Ok(Some(contention)),
        ActivePairStep::Disturbed | ActivePairStep::Gone => driver.progressed = true,
    }
    Ok(None)
}

impl NormalizationRequest {
    fn cursor_whnf(runtime: crate::core_net::CoreRuntimeNet, root_interface: Port) -> Self {
        Self {
            runtime,
            root_interface,
            mode: NormalizationMode::CursorWhnf,
        }
    }

    fn drive(&self, context: &EvalContext) -> Result<NetInterfaceOutcome, EvaluationHalt> {
        drive_net_interface(context, self)
    }
}

fn drive_net_interface(
    context: &EvalContext,
    request: &NormalizationRequest,
) -> Result<NetInterfaceOutcome, EvaluationHalt> {
    let runtime = &request.runtime;
    let interface = request.root_interface;
    debug_assert_eq!(request.mode, NormalizationMode::CursorWhnf);
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

        if finish_driver_outcome(drive_net_work(
            context,
            NetDriverWork::RequestRoot {
                runtime: runtime.clone(),
                interface,
            },
        )?)? {
            continue;
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
        let ((terminal, scheduler_is_empty, has_in_flight_claims), revisions) = runtime
            .with_revisions(|net| {
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
            runtime.wait_for_disturbance(revisions.disturbance_epoch());
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
            let cursor_obligations = net.cursor_obligations().collect::<Vec<_>>();
            format!(
                "neighbor={neighbor:?}, node={node:?}, principal_neighbor={principal_neighbor:?}/{principal_neighbor_node:?}, active={}, cursors={cursor_dependencies:?}, obligations={cursor_obligations:?}, stuck={}",
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

fn finish_driver_outcome(outcome: NetDriverOutcome) -> Result<bool, EvaluationHalt> {
    match outcome {
        NetDriverOutcome::Progressed => Ok(true),
        NetDriverOutcome::Stable => Ok(false),
        NetDriverOutcome::Contended(contention) => {
            contention
                .runtime()
                .wait_for_disturbance(contention.revisions().disturbance_epoch());
            Ok(true)
        }
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
            if finish_driver_outcome(drive_net_work(
                context,
                NetDriverWork::Cursor {
                    runtime: runtime.clone(),
                    cursor,
                },
            )?)? {
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

#[cfg(test)]
mod driver_tests {
    use super::*;

    #[test]
    fn cursor_dependency_work_orders_child_before_parent_retry() {
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let data = builder.data(crate::core::test_value_factory().unit());
        let runtime = builder.finish(data).instantiate_shared();
        let cursor = runtime.with(|net| {
            net.interface_neighbor(net.exposed())
                .expect("closed data net must expose its data node")
                .node()
        });

        let mut worklist = NetDriverWorklist::default();
        worklist.follow_cursor_dependency(
            runtime.clone(),
            cursor,
            CursorDependency::LocalCursor(cursor),
        );

        match worklist.pop().expect("child work must be present") {
            NetDriverWork::Cursor {
                runtime: child_runtime,
                cursor: child,
            } => {
                assert!(child_runtime.ptr_eq(&runtime));
                assert_eq!(child, cursor);
            }
            _ => panic!("cursor dependency must schedule its child first"),
        }
        match worklist.pop().expect("parent resumption must be present") {
            NetDriverWork::RetryCursor {
                runtime: parent_runtime,
                cursor: parent,
            } => {
                assert!(parent_runtime.ptr_eq(&runtime));
                assert_eq!(parent, cursor);
            }
            _ => panic!("cursor dependency must retain a parent retry"),
        }
        assert!(worklist.pop().is_none());
    }

    #[test]
    fn iterative_cursor_driver_exceeds_the_former_recursion_limit() {
        let expected = crate::core::test_value_factory().unit();
        let mut leaf = NetBuilder::<CoreSpecialization>::new();
        let data = leaf.data(expected.clone());
        let leaf = leaf.finish(data).instantiate_shared();
        let mut source = leaf;
        let mut root_interface = source.with(|net| net.exposed());

        for _ in 0..1_100 {
            (source, root_interface) =
                crate::interaction_net::SharedRuntimeNet::test_copy_layer(source);
        }

        let request = NormalizationRequest::cursor_whnf(source.clone(), root_interface);
        assert_eq!(
            request.drive(&test_context()).unwrap(),
            NetInterfaceOutcome::Data
        );
        assert_eq!(
            source.with(|net| net.interface_data(root_interface).cloned()),
            Some(expected)
        );
        assert_eq!(source.active_normalization_batch(), None);
    }

    #[test]
    fn cursor_driver_releases_each_runtime_before_crossing_to_the_next() {
        let mut leaf = NetBuilder::<CoreSpecialization>::new();
        let data = leaf.data(crate::core::test_value_factory().unit());
        let mut source = leaf.finish(data).instantiate_shared();
        let mut root_interface = source.with(|net| net.exposed());
        let mut runtimes = vec![source.clone()];
        for _ in 0..4 {
            (source, root_interface) =
                crate::interaction_net::SharedRuntimeNet::test_copy_layer(source);
            runtimes.push(source.clone());
        }

        NormalizationRequest::cursor_whnf(source, root_interface)
            .drive(&test_context())
            .unwrap();
        assert!(
            runtimes
                .iter()
                .all(|runtime| runtime.active_normalization_batch().is_none())
        );
    }
}
