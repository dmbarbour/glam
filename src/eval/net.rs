use super::*;
use crate::interaction_net::{
    ActivePairStep, CursorDependencyDisposition, CursorDependencyResolution, CursorStep,
    DemandEndpoint, FrontierObservation, InterfaceDemand, NetContention, NormalizationBatchLease,
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
    ResumeCursorDependency {
        runtime: crate::core_net::CoreRuntimeNet,
        cursor: crate::interaction_net::NodeId,
        expected_dependency: CursorDependency<CoreSpecialization>,
        disposition: CursorDependencyDisposition,
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
        self.push(NetDriverWork::ResumeCursorDependency {
            runtime: runtime.clone(),
            cursor,
            expected_dependency: dependency.clone(),
            disposition: CursorDependencyDisposition::Progressed,
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

    fn mark_nearest_dependency_stable(&mut self) -> bool {
        let Some(resumption) = self
            .items
            .iter_mut()
            .rev()
            .find(|item| matches!(item, NetDriverWork::ResumeCursorDependency { .. }))
        else {
            return false;
        };
        let NetDriverWork::ResumeCursorDependency { disposition, .. } = resumption else {
            unreachable!()
        };
        *disposition = CursorDependencyDisposition::Stable;
        true
    }

    fn restart_from_request_root(&mut self) -> bool {
        let Some(root) = self
            .items
            .iter()
            .find(|item| matches!(item, NetDriverWork::RequestRoot { .. }))
            .cloned()
        else {
            self.items.clear();
            return false;
        };
        self.items.clear();
        self.items.push(root);
        true
    }

    fn has_request_root(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, NetDriverWork::RequestRoot { .. }))
    }
}

impl NetDriverWork {
    fn runtime(&self) -> &crate::core_net::CoreRuntimeNet {
        match self {
            Self::RequestRoot { runtime, .. }
            | Self::Cursor { runtime, .. }
            | Self::ActivePair { runtime, .. }
            | Self::ResumeCursorDependency { runtime, .. } => runtime,
            Self::ObservedCursor { observation, .. }
            | Self::ObservedActivePair { observation, .. } => observation.source(),
        }
    }
}

enum NetDriverOutcome {
    Progressed,
    Root(InterfaceDemand),
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
                match runtime.poll_interface_demand(interface) {
                    terminal @ (InterfaceDemand::Data
                    | InterfaceDemand::Bind
                    | InterfaceDemand::NormalForm
                    | InterfaceDemand::StableCursor(_)) => {
                        return Ok(NetDriverOutcome::Root(terminal));
                    }
                    InterfaceDemand::Cursor(cursor) => {
                        driver.worklist.push(NetDriverWork::RequestRoot {
                            runtime: runtime.clone(),
                            interface,
                        });
                        driver
                            .worklist
                            .push(NetDriverWork::Cursor { runtime, cursor });
                    }
                    InterfaceDemand::ActivePair(pair) => {
                        driver.worklist.push(NetDriverWork::RequestRoot {
                            runtime: runtime.clone(),
                            interface,
                        });
                        driver
                            .worklist
                            .push(NetDriverWork::ActivePair { runtime, pair });
                    }
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
                CursorStep::Stable => {
                    if !driver.worklist.mark_nearest_dependency_stable() {
                        assert!(
                            driver.worklist.has_request_root(),
                            "stable cursor work must resume an evaluator request root"
                        );
                    }
                }
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
                CursorStep::Stable => {
                    if !driver.worklist.mark_nearest_dependency_stable() {
                        assert!(
                            driver.worklist.has_request_root(),
                            "stable observed cursor work must resume an evaluator request root"
                        );
                    }
                }
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
            NetDriverWork::ResumeCursorDependency {
                runtime,
                cursor,
                expected_dependency,
                disposition,
            } => {
                match runtime.resolve_cursor_dependency(cursor, &expected_dependency, disposition) {
                    CursorDependencyResolution::Resolved => {
                        driver.progressed = true;
                        if disposition == CursorDependencyDisposition::Stable
                            && !driver.worklist.mark_nearest_dependency_stable()
                        {
                            assert!(
                                driver.worklist.has_request_root(),
                                "stable dependency must resume an evaluator request root"
                            );
                        }
                    }
                    CursorDependencyResolution::Disturbed | CursorDependencyResolution::Gone => {
                        driver.progressed = true;
                        driver.worklist.restart_from_request_root();
                    }
                }
            }
        }
    }
    assert!(
        driver.progressed,
        "request driver exhausted without progress or a root result"
    );
    Ok(NetDriverOutcome::Progressed)
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
        match drive_net_work(
            context,
            NetDriverWork::RequestRoot {
                runtime: runtime.clone(),
                interface,
            },
        )? {
            NetDriverOutcome::Progressed => continue,
            NetDriverOutcome::Root(InterfaceDemand::Data) => {
                return Ok(NetInterfaceOutcome::Data);
            }
            NetDriverOutcome::Root(InterfaceDemand::Bind) => {
                return Ok(NetInterfaceOutcome::Bind);
            }
            NetDriverOutcome::Root(
                InterfaceDemand::NormalForm | InterfaceDemand::StableCursor(_),
            ) => {
                return Ok(NetInterfaceOutcome::NormalForm);
            }
            NetDriverOutcome::Root(InterfaceDemand::Cursor(_) | InterfaceDemand::ActivePair(_)) => {
                unreachable!("root driver must dispatch nonterminal demand")
            }
            NetDriverOutcome::Contended(contention) => {
                contention
                    .runtime()
                    .wait_for_disturbance(contention.revisions().disturbance_epoch());
                continue;
            }
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
            let source = source.prepare_copy_source();
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
            NetDriverWork::ResumeCursorDependency {
                runtime: parent_runtime,
                cursor: parent,
                expected_dependency,
                disposition,
            } => {
                assert!(parent_runtime.ptr_eq(&runtime));
                assert_eq!(parent, cursor);
                assert_eq!(expected_dependency, CursorDependency::LocalCursor(cursor));
                assert_eq!(disposition, CursorDependencyDisposition::Progressed);
            }
            _ => panic!("cursor dependency must retain a parent retry"),
        }
        assert!(worklist.pop().is_none());
    }

    #[test]
    fn stable_cursor_dependencies_propagate_through_pairless_layers() {
        let (leaf, _) = crate::interaction_net::SharedRuntimeNet::test_stable_auxiliary();
        let (middle, middle_interface) =
            crate::interaction_net::SharedRuntimeNet::test_copy_layer(leaf);
        let (root, root_interface) =
            crate::interaction_net::SharedRuntimeNet::test_copy_layer(middle.clone());

        assert!(matches!(
            drive_net_work(
                &test_context(),
                NetDriverWork::RequestRoot {
                    runtime: root.clone(),
                    interface: root_interface,
                },
            )
            .unwrap(),
            NetDriverOutcome::Root(InterfaceDemand::StableCursor(_))
        ));
        assert!(matches!(
            root.poll_interface_demand(root_interface),
            crate::interaction_net::InterfaceDemand::StableCursor(_)
        ));
        assert!(matches!(
            middle.poll_interface_demand(middle_interface),
            crate::interaction_net::InterfaceDemand::StableCursor(_)
        ));
    }

    #[test]
    fn stable_cursor_dependencies_propagate_through_mixed_owner_layers() {
        let (leaf, _) = crate::interaction_net::SharedRuntimeNet::test_stable_auxiliary();
        let (middle, middle_interface, middle_cursor) =
            crate::interaction_net::SharedRuntimeNet::test_pair_owned_copy_layer(leaf);
        let (root, root_interface) =
            crate::interaction_net::SharedRuntimeNet::test_copy_layer(middle.clone());

        assert!(matches!(
            drive_net_work(
                &test_context(),
                NetDriverWork::RequestRoot {
                    runtime: root.clone(),
                    interface: root_interface,
                },
            )
            .unwrap(),
            NetDriverOutcome::Root(InterfaceDemand::StableCursor(_))
        ));
        assert!(matches!(
            root.poll_interface_demand(root_interface),
            crate::interaction_net::InterfaceDemand::StableCursor(_)
        ));
        assert_eq!(
            middle.poll_interface_demand(middle_interface),
            crate::interaction_net::InterfaceDemand::StableCursor(middle_cursor)
        );
    }

    #[test]
    fn deep_stable_cursor_dependencies_exceed_the_former_recursion_limit() {
        let (mut source, _) = crate::interaction_net::SharedRuntimeNet::test_stable_auxiliary();
        let mut root_interface = source.with(|net| net.exposed());

        for _ in 0..1_100 {
            (source, root_interface) =
                crate::interaction_net::SharedRuntimeNet::test_copy_layer(source);
        }

        assert!(matches!(
            drive_net_work(
                &test_context(),
                NetDriverWork::RequestRoot {
                    runtime: source.clone(),
                    interface: root_interface,
                },
            )
            .unwrap(),
            NetDriverOutcome::Root(InterfaceDemand::StableCursor(_))
        ));
        assert!(matches!(
            source.poll_interface_demand(root_interface),
            InterfaceDemand::StableCursor(_)
        ));
        assert_eq!(source.active_normalization_batch(), None);
    }

    #[test]
    fn deep_productive_cursor_chain_alternates_pairless_and_pair_owned_layers() {
        let expected = crate::core::test_value_factory().unit();
        let mut leaf = NetBuilder::<CoreSpecialization>::new();
        let data = leaf.data(expected.clone());
        let mut source = leaf.finish(data).instantiate_shared();
        let mut root_interface = source.with(|net| net.exposed());

        for layer in 0..1_100 {
            if layer % 2 == 0 {
                (source, root_interface) =
                    crate::interaction_net::SharedRuntimeNet::test_productive_pair_owned_copy_layer(
                        source,
                    );
            } else {
                (source, root_interface) =
                    crate::interaction_net::SharedRuntimeNet::test_copy_layer(source);
            }
        }

        assert_eq!(
            NormalizationRequest::cursor_whnf(source.clone(), root_interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::Data
        );
        assert_eq!(
            source.with(|net| net.interface_data(root_interface).cloned()),
            Some(expected)
        );
        assert_eq!(source.active_normalization_batch(), None);
    }

    #[test]
    fn stable_root_does_not_reduce_disconnected_or_undemanded_ready_work() {
        let mut disconnected = NetBuilder::<CoreSpecialization>::new();
        let root = disconnected.push(crate::interaction_net::Node::Erase);
        let left = disconnected.push(crate::interaction_net::Node::Erase);
        let right = disconnected.push(crate::interaction_net::Node::Erase);
        disconnected.wire(Port::principal(left), Port::principal(right));
        let disconnected = disconnected
            .finish(Port::principal(root))
            .instantiate_shared();
        let disconnected_interface = disconnected.with(|net| net.exposed());
        let before = disconnected.with(|net| net.active_pairs().collect::<Vec<_>>());
        assert_eq!(
            NormalizationRequest::cursor_whnf(disconnected.clone(), disconnected_interface,)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::NormalForm
        );
        assert_eq!(
            disconnected.with(|net| net.active_pairs().collect::<Vec<_>>()),
            before
        );

        let mut branched = NetBuilder::<CoreSpecialization>::new();
        let root = branched.push_fan();
        let active = branched.push(crate::interaction_net::Node::Bind);
        let erase = branched.push(crate::interaction_net::Node::Erase);
        branched.wire(Port::auxiliary(root, 1), Port::auxiliary(active, 1));
        branched.wire(Port::auxiliary(root, 2), Port::auxiliary(active, 2));
        branched.wire(Port::principal(active), Port::principal(erase));
        let branched = branched.finish(Port::principal(root)).instantiate_shared();
        let branched_interface = branched.with(|net| net.exposed());
        let before = branched.with(|net| net.active_pairs().collect::<Vec<_>>());
        assert_eq!(
            NormalizationRequest::cursor_whnf(branched.clone(), branched_interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::NormalForm
        );
        assert_eq!(
            branched.with(|net| net.active_pairs().collect::<Vec<_>>()),
            before
        );
    }

    #[test]
    fn demanded_ready_pair_runs_before_root_completion() {
        let value = crate::core::test_value_factory().unit();
        let mut net = NetBuilder::<CoreSpecialization>::new();
        let left = net.push(crate::interaction_net::Node::Bind);
        let right = net.push(crate::interaction_net::Node::Bind);
        let left_result = net.data(value.clone());
        let exposed_result = net.data(value.clone());
        let right_result = net.data(value);
        net.wire(Port::principal(left), Port::principal(right));
        net.wire(Port::auxiliary(left, 2), left_result);
        net.wire(Port::auxiliary(right, 1), exposed_result);
        net.wire(Port::auxiliary(right, 2), right_result);
        let runtime = net.finish(Port::auxiliary(left, 1)).instantiate_shared();
        let interface = runtime.with(|net| net.exposed());
        let demanded = runtime.with(|net| {
            let pairs = net.active_pairs().collect::<Vec<_>>();
            assert_eq!(pairs.len(), 1);
            pairs[0]
        });
        assert_eq!(
            NormalizationRequest::cursor_whnf(runtime.clone(), interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::Data
        );
        assert!(!runtime.with(|net| net.contains_active_pair(demanded)));
    }

    #[test]
    fn stable_root_ignores_unrelated_claimed_and_stuck_work() {
        let value = crate::core::test_value_factory().unit();
        let mut claimed = NetBuilder::<CoreSpecialization>::new();
        let root = claimed.push(crate::interaction_net::Node::Erase);
        let bind = claimed.push(crate::interaction_net::Node::Bind);
        let data = claimed.data(value);
        let erase_left = claimed.push(crate::interaction_net::Node::Erase);
        let erase_right = claimed.push(crate::interaction_net::Node::Erase);
        claimed.wire(Port::principal(bind), data);
        claimed.wire(Port::auxiliary(bind, 1), Port::principal(erase_left));
        claimed.wire(Port::auxiliary(bind, 2), Port::principal(erase_right));
        let claimed = claimed.finish(Port::principal(root)).instantiate_shared();
        let interface = claimed.with(|net| net.exposed());
        let pair = claimed.with(|net| net.active_pairs().next().unwrap());
        assert!(matches!(
            claimed.with_optional_mut(|net| net.reduce_pair(pair)),
            Some(Reduction {
                kind: ReductionKind::Call { .. },
                ..
            })
        ));
        assert!(claimed.with(|net| net.pair_is_claimed(pair)));
        assert_eq!(
            NormalizationRequest::cursor_whnf(claimed.clone(), interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::NormalForm
        );
        assert!(claimed.with(|net| net.pair_is_claimed(pair)));

        let mut source = NetBuilder::<CoreSpecialization>::new();
        let data = source.data(crate::core::test_value_factory().unit());
        let source = source.finish(data).instantiate_shared();
        let (claimed_cursor, cursor_interface, cursor) =
            crate::interaction_net::SharedRuntimeNet::test_stable_root_with_claimed_cursor(source);
        assert_eq!(
            NormalizationRequest::cursor_whnf(claimed_cursor.clone(), cursor_interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::NormalForm
        );
        assert!(matches!(
            claimed_cursor.step_cursor(cursor),
            CursorStep::Contended(_)
        ));

        let mut stuck = NetBuilder::<CoreSpecialization>::new();
        let root = stuck.push(crate::interaction_net::Node::Erase);
        let left = stuck.data(crate::core::test_value_factory().unit());
        let right = stuck.data(crate::core::test_value_factory().unit());
        stuck.wire(left, right);
        let stuck = stuck.finish(Port::principal(root)).instantiate_shared();
        let interface = stuck.with(|net| net.exposed());
        let pair = stuck.with(|net| net.active_pairs().next().unwrap());
        assert!(matches!(
            stuck.with_optional_mut(|net| net.reduce_pair(pair)),
            Some(Reduction {
                kind: ReductionKind::Stuck,
                ..
            })
        ));
        assert_eq!(
            NormalizationRequest::cursor_whnf(stuck.clone(), interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::NormalForm
        );
        assert!(stuck.with(|net| net.stuck_reason(pair).is_some()));
    }

    #[test]
    fn demanded_claim_completion_before_wait_registration_is_not_lost() {
        let mut source = NetBuilder::<CoreSpecialization>::new();
        let data = source.data(crate::core::test_value_factory().unit());
        let source = source.finish(data).instantiate_shared();
        let (target, interface) = crate::interaction_net::SharedRuntimeNet::test_copy_layer(source);
        let cursor = match target.poll_interface_demand(interface) {
            InterfaceDemand::Cursor(cursor) => cursor,
            other => panic!("copy root should expose a cursor, received {other:?}"),
        };
        assert!(target.with_mut(|net| net.claim_pairless_cursor_obligation(cursor)));
        let contention = match drive_net_work(
            &test_context(),
            NetDriverWork::RequestRoot {
                runtime: target.clone(),
                interface,
            },
        )
        .unwrap()
        {
            NetDriverOutcome::Contended(contention) => contention,
            _ => panic!("claimed demanded cursor must report contention"),
        };
        assert!(matches!(
            target.advance_claimed_cursor(cursor),
            Some(crate::interaction_net::CursorProgress::Materialized { .. })
        ));
        contention
            .runtime()
            .wait_for_disturbance(contention.revisions().disturbance_epoch());
        assert_eq!(
            NormalizationRequest::cursor_whnf(target, interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::Data
        );
    }

    #[test]
    fn demanded_claimed_pair_remains_request_relative_and_propagates_failure() {
        let value = crate::core::test_value_factory().unit();
        let mut net = NetBuilder::<CoreSpecialization>::new();
        let bind = net.push(crate::interaction_net::Node::Bind);
        let data = net.data(value);
        let erase = net.push(crate::interaction_net::Node::Erase);
        net.wire(Port::principal(bind), data);
        net.wire(Port::auxiliary(bind, 2), Port::principal(erase));
        let runtime = net.finish(Port::auxiliary(bind, 1)).instantiate_shared();
        let interface = runtime.with(|net| net.exposed());
        let pair = runtime.with(|net| net.active_pairs().next().unwrap());
        let reduction = runtime
            .with_optional_mut(|net| net.reduce_pair(pair))
            .expect("demanded call should be claimable");
        let ReductionKind::Call { bind, data } = reduction.kind else {
            panic!("bind-data demand should be a call")
        };
        let call = Call { pair, bind, data };
        let contention = match drive_net_work(
            &test_context(),
            NetDriverWork::RequestRoot {
                runtime: runtime.clone(),
                interface,
            },
        )
        .unwrap()
        {
            NetDriverOutcome::Contended(contention) => contention,
            _ => panic!("claimed demanded pair must report contention"),
        };
        runtime.with_mut(|net| {
            net.fail_claimed_call(call, EvaluationHalt::new("demanded call failed"))
        });
        contention
            .runtime()
            .wait_for_disturbance(contention.revisions().disturbance_epoch());
        let failure = match drive_net_work(
            &test_context(),
            NetDriverWork::RequestRoot { runtime, interface },
        ) {
            Err(failure) => failure,
            Ok(_) => panic!("demanded stuck pair must remain a failure"),
        };
        assert!(failure.to_string().contains("demanded call failed"));
    }

    #[test]
    fn nested_terminal_failure_propagates_through_the_complete_driver() {
        let value = crate::core::test_value_factory().unit();
        let mut source = NetBuilder::<CoreSpecialization>::new();
        let failed_bind = source.push(crate::interaction_net::Node::Bind);
        let failed_data = source.data(value.clone());
        let failed_result = source.data(value.clone());
        source.wire(Port::principal(failed_bind), failed_data);
        source.wire(Port::auxiliary(failed_bind, 2), failed_result);

        let unrelated_left = source.push(crate::interaction_net::Node::Bind);
        let unrelated_right = source.push(crate::interaction_net::Node::Bind);
        source.wire(
            Port::principal(unrelated_left),
            Port::principal(unrelated_right),
        );
        for auxiliary in 1..=2 {
            let left_data = source.data(value.clone());
            let right_data = source.data(value.clone());
            source.wire(Port::auxiliary(unrelated_left, auxiliary), left_data);
            source.wire(Port::auxiliary(unrelated_right, auxiliary), right_data);
        }
        let source = source
            .finish(Port::auxiliary(failed_bind, 1))
            .instantiate_shared();
        let (failed_pair, unrelated_pair) = source.with(|net| {
            let pairs = net.active_pairs().collect::<Vec<_>>();
            let failed_pair = pairs
                .iter()
                .copied()
                .find(|pair| net.call(*pair).is_some())
                .expect("source should contain one callable pair");
            let unrelated_pair = pairs
                .into_iter()
                .find(|pair| *pair != failed_pair)
                .expect("source should contain one unrelated pure pair");
            (failed_pair, unrelated_pair)
        });

        let reduction = source
            .with_optional_mut(|net| net.reduce_pair(failed_pair))
            .expect("nested source call should be claimable");
        let ReductionKind::Call { bind, data } = reduction.kind else {
            panic!("nested source failure should originate in a call");
        };
        source.with_mut(|net| {
            net.fail_claimed_call(
                Call {
                    pair: failed_pair,
                    bind,
                    data,
                },
                EvaluationHalt::new("nested driver failure"),
            );
        });

        let (target, interface) =
            crate::interaction_net::SharedRuntimeNet::test_copy_layer(source.clone());
        let cursor = match target.poll_interface_demand(interface) {
            InterfaceDemand::Cursor(cursor) => cursor,
            demand => panic!("copy root should expose a cursor, got {demand:?}"),
        };
        let CursorStep::Dependency(CursorDependency::SourceFrontier(observation)) =
            target.step_cursor(cursor)
        else {
            panic!("nested source failure should become an exact frontier dependency");
        };
        assert_eq!(
            observation.endpoint(),
            DemandEndpoint::ActivePair(failed_pair)
        );

        assert!(matches!(
            source.with_optional_mut(|net| net.reduce_pair(unrelated_pair)),
            Some(Reduction {
                kind: ReductionKind::BindJoin,
                ..
            })
        ));
        let failure = NormalizationRequest::cursor_whnf(target, interface)
            .drive(&test_context())
            .expect_err("nested terminal failure must propagate through the driver");
        assert!(failure.to_string().contains("nested driver failure"));
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
