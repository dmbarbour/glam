use super::*;
use crate::core::RuntimeValueAccess;
use crate::core_net::{
    CoreActivePairStep as ActivePairStep, CoreCursorDependency as CursorDependency,
    CoreCursorStep as CursorStep, CoreFrontierObservation as FrontierObservation,
    CoreNetContention as NetContention, CorePreparedCopySource, CoreRuntimeNet,
    CoreRuntimeNetAccess,
};
use crate::interaction_net::{
    BlockedCall, BlockedOperatorCall, CursorDependencyDisposition, CursorDependencyResolution,
    DemandEndpoint, InterfaceDemand, RuntimeNet,
};

pub(super) fn attach_net_many_in(
    access: &RuntimeValueAccess<'_>,
    function: NetValue,
    arguments: Vec<Value>,
) -> NetValue {
    let runtime = attached_net_runtime(access, function, arguments);
    NetValue::new(
        access
            .construct_managed_core_net(runtime)
            .expect("managed core-net representation must fit one collector run"),
    )
}

pub(super) fn attached_net_runtime(
    _access: &RuntimeValueAccess<'_>,
    function: NetValue,
    arguments: Vec<Value>,
) -> RuntimeNet<CoreSpecialization> {
    assert!(!arguments.is_empty(), "net attachment requires an argument");
    let mut net = NetBuilder::new();
    let spine = net.bind_spine(arguments.len());
    let function = net.data(Value::Net(function));
    net.wire(spine.input, function);
    for (argument_port, argument) in spine.arguments.into_iter().zip(arguments) {
        let argument = net.data(argument);
        net.wire(argument_port, argument);
    }
    let template = net.finish(spine.result);
    template.instantiate()
}

pub(super) struct NetWhnfMachine {
    request: NormalizationRequest,
    driver: NetDriver,
    operation: Arc<str>,
}

pub(super) enum NetWhnfPoll {
    Ready(Value),
    Yielded,
}

impl NetWhnfMachine {
    pub(super) fn new(
        context: &EvaluatorStepContext<'_>,
        runtime: CoreRuntimeNet,
        interface: Port,
        operation: impl Into<Arc<str>>,
    ) -> Self {
        context
            .with_value_access(|access| Self::new_in(&access, runtime, interface, operation.into()))
    }

    fn new_in(
        access: &crate::evaluation::EvaluationValueAccess<'_>,
        runtime: CoreRuntimeNet,
        interface: Port,
        operation: Arc<str>,
    ) -> Self {
        let request = NormalizationRequest::cursor_whnf_in(&runtime, interface, access);
        let driver = NetDriver::new(&request);
        Self {
            request,
            driver,
            operation,
        }
    }

    pub(super) fn from_function_call(
        access: &crate::evaluation::EvaluationValueAccess<'_>,
        function: &FunctionValue,
        arguments: &[Value],
    ) -> Self {
        let stage = function.duplicate_stage_in(access.values());
        let net = attach_net_many_in(access.values(), stage, arguments.to_vec());
        let runtime = net.into_runtime();
        let exposed = access.net(&runtime).with(|runtime| runtime.exposed());
        Self::new_in(access, runtime, exposed, Arc::from("function call"))
    }

    pub(super) fn poll(
        &mut self,
        context: &EvaluatorStepContext<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> Result<NetWhnfPoll, EvaluationHalt> {
        #[cfg(feature = "interaction-net-profiling")]
        context
            .context()
            .values()
            .record_net_driver(crate::interaction_net::profiling::DriverEvent::MachinePoll);
        match drive_net_driver_work_with_budget_in(context, &mut self.driver, step_budget)? {
            NetDriverOutcome::Progressed => {
                #[cfg(feature = "interaction-net-profiling")]
                context.context().values().record_net_driver(
                    crate::interaction_net::profiling::DriverEvent::RequestRootRestart,
                );
                self.driver.restart_from_request_root();
                Ok(NetWhnfPoll::Yielded)
            }
            #[cfg(all(test, feature = "interaction-net-profiling"))]
            NetDriverOutcome::ProbeBudgetExhausted => Ok(NetWhnfPoll::Yielded),
            NetDriverOutcome::SemanticBudgetExhausted => Ok(NetWhnfPoll::Yielded),
            NetDriverOutcome::Root(InterfaceDemand::Data) => {
                let value = context.with_value_access(|access| {
                    let runtime = CoreRuntimeNet::from_root(&self.request.root, access.values());
                    access.net(&runtime).with(|runtime| {
                        runtime.interface_data(self.request.root_interface).cloned()
                    })
                });
                Ok(NetWhnfPoll::Ready(value.expect(
                    "evaluated interaction-net interface must contain data",
                )))
            }
            NetDriverOutcome::Root(InterfaceDemand::Bind) => Err(EvaluationHalt::new(format!(
                "{} exposed a bind instead of data",
                self.operation
            ))),
            NetDriverOutcome::Root(
                InterfaceDemand::NormalForm | InterfaceDemand::StableCursor(_),
            ) => Err(EvaluationHalt::new(format!(
                "{} reached a non-data normal form",
                self.operation
            ))),
            NetDriverOutcome::Root(InterfaceDemand::Cursor(_) | InterfaceDemand::ActivePair(_)) => {
                unreachable!("root driver must dispatch nonterminal demand")
            }
            NetDriverOutcome::Contended(contention) => {
                contention.wait_for_disturbance();
                Ok(NetWhnfPoll::Yielded)
            }
        }
    }

    #[cfg(test)]
    fn retained_root(&self) -> &crate::core::ManagedCoreNetRoot {
        &self.request.root
    }
}

fn with_core_net_access<R>(
    context: &EvaluatorStepContext<'_>,
    runtime: &CoreRuntimeNet,
    operation: impl FnOnce(CoreRuntimeNetAccess<'_, '_>) -> R,
) -> R {
    context.with_value_access(|access| operation(access.net(runtime)))
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetInterfaceOutcome {
    Data,
    Bind,
    NormalForm,
}

/// Evaluator-owned description of one demanded interaction-net frontier.
/// Shared progress remains in the net's cursor obligations; cloning or
/// dropping this descriptor has no runtime lifecycle effect.
#[derive(Clone)]
struct NormalizationRequest {
    root: crate::core::ManagedCoreNetRoot,
    root_interface: Port,
}

#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
const _: () = assert!(std::mem::size_of::<NormalizationRequest>() == 16);

/// One evaluator-owned unit of cursor-WHNF work. These descriptors carry no
/// claim or lifecycle ownership; authoritative progress remains in the shared
/// runtime nets and can be reconstructed from `RequestRoot` after a retry.
#[derive(Clone)]
enum NetDriverWork {
    RequestRoot {
        root: crate::core::ManagedCoreNetRoot,
        interface: Port,
    },
    Cursor {
        root: crate::core::ManagedCoreNetRoot,
        cursor: crate::interaction_net::NodeId,
    },
    ObservedCursor {
        observation: FrontierObservation,
        cursor: crate::interaction_net::NodeId,
    },
    ActivePair {
        root: crate::core::ManagedCoreNetRoot,
        pair: ActivePairKey,
    },
    ObservedActivePair {
        observation: FrontierObservation,
        pair: ActivePairKey,
    },
    ResumeCursorDependency {
        root: crate::core::ManagedCoreNetRoot,
        cursor: crate::interaction_net::NodeId,
        expected_dependency: CursorDependency,
        disposition: CursorDependencyDisposition,
    },
}

#[derive(Default)]
struct NetDriverWorklist {
    items: Vec<NetDriverWork>,
}

impl NetDriverWorklist {
    fn pop(&mut self) -> Option<NetDriverWork> {
        self.items.pop()
    }

    fn push(&mut self, work: NetDriverWork) {
        self.items.push(work);
    }

    fn follow_cursor_dependency(
        &mut self,
        root: crate::core::ManagedCoreNetRoot,
        cursor: crate::interaction_net::NodeId,
        dependency: CursorDependency,
    ) {
        self.push(NetDriverWork::ResumeCursorDependency {
            root: root.clone(),
            cursor,
            expected_dependency: dependency.clone(),
            disposition: CursorDependencyDisposition::Progressed,
        });
        self.push(match dependency {
            CursorDependency::LocalCursor(cursor) => NetDriverWork::Cursor { root, cursor },
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

    fn mark_nearest_dependency_stable(&mut self) {
        let Some(resumption) = self
            .items
            .iter_mut()
            .rev()
            .find(|item| matches!(item, NetDriverWork::ResumeCursorDependency { .. }))
        else {
            return;
        };
        let NetDriverWork::ResumeCursorDependency { disposition, .. } = resumption else {
            unreachable!()
        };
        *disposition = CursorDependencyDisposition::Stable;
    }

    fn reset(&mut self, root: NetDriverWork) {
        self.items.clear();
        self.items.push(root);
    }
}

impl NetDriverWork {
    fn runtime(&self, access: &RuntimeValueAccess<'_>) -> crate::core_net::CoreRuntimeNet {
        match self {
            Self::RequestRoot { root, .. }
            | Self::Cursor { root, .. }
            | Self::ActivePair { root, .. }
            | Self::ResumeCursorDependency { root, .. } => CoreRuntimeNet::from_root(root, access),
            Self::ObservedCursor { observation, .. }
            | Self::ObservedActivePair { observation, .. } => observation.source(access),
        }
    }
}

enum NetDriverOutcome {
    Progressed,
    Root(InterfaceDemand),
    Contended(NetContention),
    SemanticBudgetExhausted,
    #[cfg(all(test, feature = "interaction-net-profiling"))]
    ProbeBudgetExhausted,
}

struct NetDriver {
    request: NormalizationRequest,
    worklist: NetDriverWorklist,
    progressed: bool,
}

impl NetDriver {
    fn new(request: &NormalizationRequest) -> Self {
        let request = request.clone();
        let mut worklist = NetDriverWorklist::default();
        worklist.push(request.root_work());
        Self {
            request,
            worklist,
            progressed: false,
        }
    }

    fn restart_from_request_root(&mut self) {
        self.worklist.reset(self.request.root_work());
        self.progressed = false;
    }
}

#[cfg(test)]
fn drive_net_work_in(
    context: &EvaluatorStepContext<'_>,
    request: &NormalizationRequest,
) -> Result<NetDriverOutcome, EvaluationHalt> {
    let mut driver = NetDriver::new(request);
    drive_net_driver_work_in(context, &mut driver)
}

#[cfg(test)]
fn drive_net_driver_work_in(
    context: &EvaluatorStepContext<'_>,
    driver: &mut NetDriver,
) -> Result<NetDriverOutcome, EvaluationHalt> {
    drive_net_driver_work_with_budget_in(
        context,
        driver,
        &mut crate::evaluation::EvaluationStepBudget::new(usize::MAX),
    )
}

fn drive_net_driver_work_with_budget_in(
    context: &EvaluatorStepContext<'_>,
    driver: &mut NetDriver,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> Result<NetDriverOutcome, EvaluationHalt> {
    while let Some(work) = driver.worklist.pop() {
        let retained_work = work.clone();
        let outcome = context.with_value_access(|values| {
            let work_runtime = work.runtime(values.values());
            let access = values.net(&work_runtime);
            access.with_normalization_batch(|access| {
                drive_net_batch(driver, &work_runtime, work, access)
            })
        });
        let outcome = match outcome {
            Ok(outcome) => outcome?,
            Err(contention) => {
                // The batch admission did not inspect or change this item.
                // Retain it before handing contention back to the scheduler,
                // just as item-level cursor and active-pair contention does.
                driver.worklist.push(retained_work);
                #[cfg(feature = "interaction-net-profiling")]
                context
                    .context()
                    .values()
                    .record_net_driver(crate::interaction_net::profiling::DriverEvent::Contention);
                return Ok(NetDriverOutcome::Contended(contention));
            }
        };
        match outcome {
            NetBatchOutcome::Continue => {}
            NetBatchOutcome::Driver(outcome) => return Ok(outcome),
            NetBatchOutcome::Semantic { root, pair, step } => {
                if !step_budget.try_consume() {
                    release_unstarted_semantic_step(context, &root, pair, step);
                    driver
                        .worklist
                        .push(NetDriverWork::ActivePair { root, pair });
                    return Ok(NetDriverOutcome::SemanticBudgetExhausted);
                }
                let retained_root = root.clone();
                if let Err(error) =
                    drive_active_pair_semantic_step(context, driver, root, pair, step, step_budget)
                {
                    if error.blocked_on().is_some() || error.unassigned_promise_root().is_some() {
                        driver.worklist.push(NetDriverWork::ActivePair {
                            root: retained_root,
                            pair,
                        });
                    }
                    return Err(error);
                }
                #[cfg(all(test, feature = "interaction-net-profiling"))]
                if context
                    .context()
                    .values()
                    .net_driver_work_item_limit_reached()
                {
                    return Ok(NetDriverOutcome::ProbeBudgetExhausted);
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

/// Restores any claim taken while discovering a semantic active-pair step.
///
/// Budget admission happens after the normalization batch closes because the
/// semantic callback itself must run outside that batch. A call reduction is
/// already claimed by then, so a denied budget must explicitly return it to
/// the runtime before retaining ordinary active-pair work. Blocked retries
/// have not yet been reclaimed and need no mutation.
fn release_unstarted_semantic_step(
    context: &EvaluatorStepContext<'_>,
    root: &crate::core::ManagedCoreNetRoot,
    pair: ActivePairKey,
    step: ActivePairStep,
) {
    let runtime =
        context.with_value_access(|access| CoreRuntimeNet::from_root(root, access.values()));
    match step {
        ActivePairStep::Reduction(reduction) => match reduction.kind {
            ReductionKind::Call { bind, data } => {
                let restored = with_core_net_access(context, &runtime, |runtime| {
                    runtime.release_claimed_call(Call { pair, bind, data })
                });
                assert!(restored, "budget denial must restore the exact call claim");
            }
            ReductionKind::CallableCheckpoint { .. } => {
                let restored = with_core_net_access(context, &runtime, |runtime| {
                    runtime.release_claimed_callable_checkpoint(pair)
                });
                assert!(
                    restored,
                    "budget denial must restore the exact checkpoint claim"
                );
            }
            ReductionKind::OperatorCall { operator, data } => {
                let restored = with_core_net_access(context, &runtime, |runtime| {
                    runtime.release_claimed_operator_call(OperatorCall {
                        pair,
                        operator,
                        data,
                    })
                });
                assert!(
                    restored,
                    "budget denial must restore the exact operator-call claim"
                );
            }
            _ => unreachable!("only semantic reductions leave a normalization batch"),
        },
        ActivePairStep::BlockedCall(_) | ActivePairStep::BlockedOperatorCall(_) => {}
        _ => unreachable!("only semantic work reaches budget admission"),
    }
}

enum NetBatchOutcome {
    Continue,
    Driver(NetDriverOutcome),
    Semantic {
        root: crate::core::ManagedCoreNetRoot,
        pair: ActivePairKey,
        step: ActivePairStep,
    },
}

fn drive_net_batch(
    driver: &mut NetDriver,
    batch_runtime: &CoreRuntimeNet,
    mut work: NetDriverWork,
    access: &CoreRuntimeNetAccess<'_, '_>,
) -> Result<NetBatchOutcome, EvaluationHalt> {
    loop {
        debug_assert!(
            work.runtime(access.values())
                .same_net_in(batch_runtime, access.values())
        );
        if let Some(outcome) = drive_net_work_item(driver, work, access)? {
            return Ok(outcome);
        }
        #[cfg(all(test, feature = "interaction-net-profiling"))]
        if access.driver_work_item_limit_reached() {
            return Ok(NetBatchOutcome::Driver(
                NetDriverOutcome::ProbeBudgetExhausted,
            ));
        }
        let Some(next) = driver.worklist.pop() else {
            assert!(
                driver.progressed,
                "request driver exhausted without progress or a root result"
            );
            return Ok(NetBatchOutcome::Driver(NetDriverOutcome::Progressed));
        };
        if !next
            .runtime(access.values())
            .same_net_in(batch_runtime, access.values())
        {
            driver.worklist.push(next);
            return Ok(NetBatchOutcome::Continue);
        }
        work = next;
    }
}

fn drive_net_work_item(
    driver: &mut NetDriver,
    work: NetDriverWork,
    access: &CoreRuntimeNetAccess<'_, '_>,
) -> Result<Option<NetBatchOutcome>, EvaluationHalt> {
    #[cfg(feature = "interaction-net-profiling")]
    access.record_driver(crate::interaction_net::profiling::DriverEvent::WorkItem);
    match work {
        NetDriverWork::RequestRoot { root, interface } => {
            #[cfg(feature = "interaction-net-profiling")]
            access.record_driver(crate::interaction_net::profiling::DriverEvent::InterfacePoll);
            match access.poll_interface_demand(interface) {
                terminal @ (InterfaceDemand::Data
                | InterfaceDemand::Bind
                | InterfaceDemand::NormalForm
                | InterfaceDemand::StableCursor(_)) => {
                    return Ok(Some(NetBatchOutcome::Driver(NetDriverOutcome::Root(
                        terminal,
                    ))));
                }
                InterfaceDemand::Cursor(cursor) => {
                    driver.worklist.push(NetDriverWork::RequestRoot {
                        root: root.clone(),
                        interface,
                    });
                    driver.worklist.push(NetDriverWork::Cursor { root, cursor });
                }
                InterfaceDemand::ActivePair(pair) => {
                    driver.worklist.push(NetDriverWork::RequestRoot {
                        root: root.clone(),
                        interface,
                    });
                    driver
                        .worklist
                        .push(NetDriverWork::ActivePair { root, pair });
                }
            }
        }
        NetDriverWork::Cursor { root, cursor } => {
            #[cfg(feature = "interaction-net-profiling")]
            access.record_driver(crate::interaction_net::profiling::DriverEvent::CursorStep);
            match access.step_cursor(cursor) {
                CursorStep::Progressed(progress) => {
                    debug_assert_ne!(progress, crate::interaction_net::CursorProgress::Claimed);
                    driver.progressed = true;
                }
                CursorStep::Disturbed | CursorStep::Gone => {
                    #[cfg(feature = "interaction-net-profiling")]
                    access
                        .record_driver(crate::interaction_net::profiling::DriverEvent::Disturbance);
                    driver.progressed = true;
                }
                CursorStep::Dependency(dependency) => {
                    #[cfg(feature = "interaction-net-profiling")]
                    access.record_driver(
                        crate::interaction_net::profiling::DriverEvent::CursorDependency,
                    );
                    driver
                        .worklist
                        .follow_cursor_dependency(root, cursor, dependency);
                }
                CursorStep::Stable => driver.worklist.mark_nearest_dependency_stable(),
                CursorStep::Contended(contention) => {
                    #[cfg(feature = "interaction-net-profiling")]
                    access
                        .record_driver(crate::interaction_net::profiling::DriverEvent::Contention);
                    driver.worklist.push(NetDriverWork::Cursor { root, cursor });
                    return Ok(Some(NetBatchOutcome::Driver(NetDriverOutcome::Contended(
                        contention,
                    ))));
                }
            }
        }
        NetDriverWork::ObservedCursor {
            observation,
            cursor,
        } => {
            #[cfg(feature = "interaction-net-profiling")]
            access.record_driver(crate::interaction_net::profiling::DriverEvent::CursorStep);
            match observation.step_cursor(access, cursor) {
                CursorStep::Progressed(progress) => {
                    debug_assert_ne!(progress, crate::interaction_net::CursorProgress::Claimed);
                    driver.progressed = true;
                }
                CursorStep::Disturbed | CursorStep::Gone => {
                    #[cfg(feature = "interaction-net-profiling")]
                    access
                        .record_driver(crate::interaction_net::profiling::DriverEvent::Disturbance);
                    driver.progressed = true;
                }
                CursorStep::Dependency(dependency) => {
                    #[cfg(feature = "interaction-net-profiling")]
                    access.record_driver(
                        crate::interaction_net::profiling::DriverEvent::CursorDependency,
                    );
                    driver.worklist.follow_cursor_dependency(
                        observation.root().clone(),
                        cursor,
                        dependency,
                    );
                }
                CursorStep::Stable => driver.worklist.mark_nearest_dependency_stable(),
                CursorStep::Contended(contention) => {
                    #[cfg(feature = "interaction-net-profiling")]
                    access
                        .record_driver(crate::interaction_net::profiling::DriverEvent::Contention);
                    driver.worklist.push(NetDriverWork::ObservedCursor {
                        observation,
                        cursor,
                    });
                    return Ok(Some(NetBatchOutcome::Driver(NetDriverOutcome::Contended(
                        contention,
                    ))));
                }
            }
        }
        NetDriverWork::ActivePair { root, pair } => {
            #[cfg(feature = "interaction-net-profiling")]
            access.record_driver(crate::interaction_net::profiling::DriverEvent::ActivePairStep);
            return prepare_active_pair_step(
                driver,
                access,
                root,
                pair,
                access.step_active_pair(pair),
            );
        }
        NetDriverWork::ObservedActivePair { observation, pair } => {
            #[cfg(feature = "interaction-net-profiling")]
            access.record_driver(crate::interaction_net::profiling::DriverEvent::ActivePairStep);
            let step = observation.step_active_pair(access, pair);
            return prepare_active_pair_step(
                driver,
                access,
                observation.root().clone(),
                pair,
                step,
            );
        }
        NetDriverWork::ResumeCursorDependency {
            cursor,
            expected_dependency,
            disposition,
            ..
        } => match access.resolve_cursor_dependency(cursor, &expected_dependency, disposition) {
            CursorDependencyResolution::Resolved => {
                driver.progressed = true;
                if disposition == CursorDependencyDisposition::Stable {
                    driver.worklist.mark_nearest_dependency_stable();
                }
            }
            CursorDependencyResolution::Disturbed | CursorDependencyResolution::Gone => {
                #[cfg(feature = "interaction-net-profiling")]
                access.record_driver(crate::interaction_net::profiling::DriverEvent::Disturbance);
                driver.progressed = true;
                #[cfg(feature = "interaction-net-profiling")]
                access.record_driver(
                    crate::interaction_net::profiling::DriverEvent::RequestRootRestart,
                );
                driver.restart_from_request_root();
            }
        },
    }
    Ok(None)
}

fn prepare_active_pair_step(
    driver: &mut NetDriver,
    access: &CoreRuntimeNetAccess<'_, '_>,
    root: crate::core::ManagedCoreNetRoot,
    pair: ActivePairKey,
    step: ActivePairStep,
) -> Result<Option<NetBatchOutcome>, EvaluationHalt> {
    match step {
        ActivePairStep::Reduction(reduction) => {
            driver.progressed = true;
            if matches!(
                &reduction.kind,
                ReductionKind::Call { .. }
                    | ReductionKind::CallableCheckpoint { .. }
                    | ReductionKind::OperatorCall { .. }
            ) {
                return Ok(Some(NetBatchOutcome::Semantic {
                    root,
                    pair,
                    step: ActivePairStep::Reduction(reduction),
                }));
            }
            match reduction.kind {
                ReductionKind::Stuck => return Err(stuck_pair_error_in(access, pair)),
                ReductionKind::RemoteCursor { cursor, progress } => {
                    debug_assert_ne!(progress, crate::interaction_net::CursorProgress::Claimed);
                    if progress == crate::interaction_net::CursorProgress::Blocked {
                        driver.worklist.push(NetDriverWork::Cursor { root, cursor });
                    }
                }
                _ => {}
            }
        }
        ActivePairStep::Cursor(cursor) => {
            driver.worklist.push(NetDriverWork::Cursor { root, cursor });
        }
        step @ (ActivePairStep::BlockedCall(_)
        | ActivePairStep::BlockedCallableCheckpoint(_)
        | ActivePairStep::BlockedOperatorCall(_)) => {
            return Ok(Some(NetBatchOutcome::Semantic { root, pair, step }));
        }
        ActivePairStep::Stuck => return Err(stuck_pair_error_in(access, pair)),
        ActivePairStep::Contended(contention) => {
            #[cfg(feature = "interaction-net-profiling")]
            access.record_driver(crate::interaction_net::profiling::DriverEvent::Contention);
            driver
                .worklist
                .push(NetDriverWork::ActivePair { root, pair });
            return Ok(Some(NetBatchOutcome::Driver(NetDriverOutcome::Contended(
                contention,
            ))));
        }
        ActivePairStep::Disturbed | ActivePairStep::Gone => driver.progressed = true,
    }
    Ok(None)
}

#[cfg(test)]
fn drive_net_work(
    context: &EvalContext,
    request: &NormalizationRequest,
) -> Result<NetDriverOutcome, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| drive_net_work_in(evaluator, request))
}

fn drive_active_pair_semantic_step(
    context: &EvaluatorStepContext<'_>,
    driver: &mut NetDriver,
    root: crate::core::ManagedCoreNetRoot,
    pair: ActivePairKey,
    step: ActivePairStep,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> Result<(), EvaluationHalt> {
    let runtime =
        context.with_value_access(|access| CoreRuntimeNet::from_root(&root, access.values()));
    assert_semantic_step_is_unbatched(&runtime);
    match step {
        ActivePairStep::Reduction(reduction) => match reduction.kind {
            ReductionKind::Call { bind, data } => {
                let call = Call { pair, bind, data };
                if !progress_exact_core_call_in(context, &runtime, call, step_budget)? {
                    return Err(EvaluationHalt::new("interaction-net call lost its claim"));
                }
                driver
                    .worklist
                    .push(NetDriverWork::ActivePair { root, pair });
            }
            ReductionKind::CallableCheckpoint { .. } => {
                progress_callable_checkpoint(context, &runtime, pair, step_budget)?;
                driver
                    .worklist
                    .push(NetDriverWork::ActivePair { root, pair });
            }
            ReductionKind::OperatorCall { operator, data } => {
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
                    .push(NetDriverWork::ActivePair { root, pair });
            }
            _ => unreachable!("only semantic reductions leave a normalization batch"),
        },
        ActivePairStep::BlockedCall(blocked) => {
            #[cfg(feature = "interaction-net-profiling")]
            context
                .context()
                .values()
                .record_net_driver(crate::interaction_net::profiling::DriverEvent::BlockedRetry);
            match context.context().poll_wait(&blocked.wait.0) {
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
            let Some(claim) = CoreCallClaim::retry(context, &runtime, blocked) else {
                return Err(EvaluationHalt::new(
                    "interaction-net call lost its exact blocked claim",
                ));
            };
            if !progress_core_call_claim(context, claim, step_budget)? {
                return Err(EvaluationHalt::new(
                    "interaction-net call released its retry",
                ));
            }
            driver.progressed = true;
            driver
                .worklist
                .push(NetDriverWork::ActivePair { root, pair });
        }
        ActivePairStep::BlockedCallableCheckpoint(blocked) => {
            #[cfg(feature = "interaction-net-profiling")]
            context
                .context()
                .values()
                .record_net_driver(crate::interaction_net::profiling::DriverEvent::BlockedRetry);
            match context.context().poll_wait(&blocked.wait.0) {
                crate::evaluation::EvaluationWaitPoll::Pending(_) => {
                    return Err(EvaluationHalt::blocked(blocked.wait));
                }
                crate::evaluation::EvaluationWaitPoll::Failed(failure)
                | crate::evaluation::EvaluationWaitPoll::Killed(failure) => {
                    return fail_blocked_callable_checkpoint(context, &runtime, &blocked, failure);
                }
                crate::evaluation::EvaluationWaitPoll::Complete(_)
                | crate::evaluation::EvaluationWaitPoll::Cancelled
                | crate::evaluation::EvaluationWaitPoll::Abandoned
                | crate::evaluation::EvaluationWaitPoll::Exited => {}
            }
            let retried = with_core_net_access(context, &runtime, |runtime| {
                runtime.retry_blocked_callable_checkpoint(&blocked)
            });
            if !retried {
                return Err(EvaluationHalt::new(
                    "interaction-net checkpoint lost its exact blocked generation",
                ));
            }
            driver.progressed = true;
            driver
                .worklist
                .push(NetDriverWork::ActivePair { root, pair });
        }
        ActivePairStep::BlockedOperatorCall(blocked) => {
            #[cfg(feature = "interaction-net-profiling")]
            context
                .context()
                .values()
                .record_net_driver(crate::interaction_net::profiling::DriverEvent::BlockedRetry);
            match context.context().poll_wait(&blocked.wait.0) {
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
            let Some(claim) = CoreOperatorClaim::retry(context, &runtime, blocked) else {
                return Err(EvaluationHalt::new(
                    "interaction-net operator call lost its exact blocked claim",
                ));
            };
            if !progress_core_operator_claim(context, claim)? {
                return Err(EvaluationHalt::new(
                    "interaction-net operator call released its retry",
                ));
            }
            driver.progressed = true;
            driver
                .worklist
                .push(NetDriverWork::ActivePair { root, pair });
        }
        _ => unreachable!("non-semantic active-pair work must remain inside its batch"),
    }
    Ok(())
}

#[cold]
fn fail_blocked_callable_checkpoint(
    context: &EvaluatorStepContext<'_>,
    runtime: &CoreRuntimeNet,
    blocked: &crate::interaction_net::BlockedCallableCheckpoint<crate::core_net::CoreWaitToken>,
    failure: crate::runtime::RuntimeFailureRoot,
) -> Result<(), EvaluationHalt> {
    let failure = EvaluationHalt::failure(failure.into_failure());
    let retired = with_core_net_access(context, runtime, |runtime| {
        runtime.fail_blocked_callable_checkpoint(blocked, failure.clone())
    });
    let Ok(state) = retired else {
        return Err(EvaluationHalt::new(
            "interaction-net checkpoint lost its exact failed generation",
        ));
    };
    drop(state);
    Err(failure)
}

#[cfg(test)]
fn assert_semantic_step_is_unbatched(runtime: &CoreRuntimeNet) {
    let _ = runtime;
    assert!(
        !crate::core_net::thread_has_active_core_normalization_scope(),
        "callable and operator evaluation must begin outside the current normalization scope"
    );
}

#[cfg(not(test))]
fn assert_semantic_step_is_unbatched(_runtime: &CoreRuntimeNet) {}

impl NormalizationRequest {
    #[cfg(test)]
    fn cursor_whnf(
        runtime: &CoreRuntimeNet,
        root_interface: Port,
        context: &EvaluatorStepContext<'_>,
    ) -> Self {
        context.with_value_access(|access| Self::cursor_whnf_in(runtime, root_interface, &access))
    }

    fn cursor_whnf_in(
        runtime: &CoreRuntimeNet,
        root_interface: Port,
        access: &crate::evaluation::EvaluationValueAccess<'_>,
    ) -> Self {
        Self {
            root: runtime.root_in(access.values()),
            root_interface,
        }
    }

    fn root_work(&self) -> NetDriverWork {
        NetDriverWork::RequestRoot {
            root: self.root.clone(),
            interface: self.root_interface,
        }
    }

    #[cfg(test)]
    fn drive_in(
        &self,
        context: &EvaluatorStepContext<'_>,
    ) -> Result<NetInterfaceOutcome, EvaluationHalt> {
        drive_net_interface(context, self)
    }

    #[cfg(test)]
    fn drive(&self, context: &EvalContext) -> Result<NetInterfaceOutcome, EvaluationHalt> {
        super::with_direct_evaluator(context, |evaluator| self.drive_in(evaluator))
    }
}

#[cfg(test)]
fn drive_net_interface(
    context: &EvaluatorStepContext<'_>,
    request: &NormalizationRequest,
) -> Result<NetInterfaceOutcome, EvaluationHalt> {
    drive_net_interface_with_contention_handoff(context, request, || {})
}

#[cfg(test)]
fn drive_net_interface_with_contention_handoff(
    context: &EvaluatorStepContext<'_>,
    request: &NormalizationRequest,
    mut before_handoff: impl FnMut(),
) -> Result<NetInterfaceOutcome, EvaluationHalt> {
    loop {
        match drive_net_work_in(context, request)? {
            NetDriverOutcome::Progressed => continue,
            NetDriverOutcome::SemanticBudgetExhausted => {
                unreachable!("the test-only driver receives an unbounded semantic budget")
            }
            #[cfg(all(test, feature = "interaction-net-profiling"))]
            NetDriverOutcome::ProbeBudgetExhausted => continue,
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
                // The normalization scope has closed before this outcome can
                // escape `drive_net_work_in`. This callback exists so tests
                // can force the handoff schedule at that exact boundary.
                before_handoff();
                contention.wait_for_disturbance();
                continue;
            }
        }
    }
}

impl NetSpecialization for CoreSpecialization {
    type Data = Value;
    type Operator = CoreOperator;
    // Stored cross-net identities carry no operation authority. Core source
    // progression is supplied explicitly by `CoreRuntimeNetAccess`.
    type RuntimeSource = CoreRuntimeNet;
    type WaitToken = crate::core_net::CoreWaitToken;
    type StuckReason = EvaluationHalt;
    type CallableCheckpoint = Box<crate::eval::whnf::NetWhnfState>;
}

pub(super) enum CoreCallable {
    Net(crate::core_net::CoreRuntimeNet),
    Operator(CoreOperator),
}

enum CallDisposition {
    Copy(CorePreparedCopySource),
    Operator(CoreOperator),
    Failed(EvaluationHalt),
    #[allow(
        dead_code,
        reason = "the explicit release disposition is exercised by claim-protocol tests"
    )]
    Release,
}

#[derive(Clone)]
enum CallFallback {
    Ready,
    Blocked(crate::core_net::CoreWaitToken),
}

/// One stack-bound callable claim. It owns the callable clone and the exact
/// state restored by release or unwind, but carries no managed-value access.
#[must_use = "a callable claim must be terminalized or released"]
struct CoreCallClaim<'claim, 'step> {
    context: &'claim EvaluatorStepContext<'step>,
    runtime: &'claim CoreRuntimeNet,
    call: Call,
    callable: crate::runtime::RuntimeValueRoot,
    fallback: Option<CallFallback>,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl<'claim, 'step> CoreCallClaim<'claim, 'step> {
    fn fresh(
        context: &'claim EvaluatorStepContext<'step>,
        runtime: &'claim CoreRuntimeNet,
        call: Call,
    ) -> Option<Self> {
        let callable =
            with_core_net_access(context, runtime, |runtime| runtime.claim_call_rooted(call))?;
        Some(Self {
            context,
            runtime,
            call,
            callable,
            fallback: Some(CallFallback::Ready),
            _thread_bound: std::marker::PhantomData,
        })
    }

    fn retry(
        context: &'claim EvaluatorStepContext<'step>,
        runtime: &'claim CoreRuntimeNet,
        blocked: BlockedCall<crate::core_net::CoreWaitToken>,
    ) -> Option<Self> {
        let fallback = CallFallback::Blocked(blocked.wait.clone());
        let (call, callable) = with_core_net_access(context, runtime, |runtime| {
            runtime.reclaim_blocked_call(&blocked)
        })?;
        Some(Self {
            context,
            runtime,
            call,
            callable,
            fallback: Some(fallback),
            _thread_bound: std::marker::PhantomData,
        })
    }

    fn callable(&self, access: &crate::evaluation::EvaluationValueAccess<'_>) -> Value {
        access.clone_root(&self.callable)
    }

    fn install_checkpoint_in(
        &mut self,
        access: &crate::evaluation::EvaluationValueAccess<'_>,
        state: crate::eval::whnf::NetWhnfState,
    ) -> Result<crate::interaction_net::CallableCheckpointCall, EvaluationHalt> {
        let runtime = access.net(self.runtime);
        match runtime.install_claimed_call_checkpoint(self.call, state) {
            Ok(call) => {
                self.fallback = None;
                Ok(call)
            }
            Err(_state) => Err(EvaluationHalt::new(
                "interaction-net call lost its claim while publishing WHNF state",
            )),
        }
    }

    fn finish(mut self, disposition: CallDisposition) -> Result<bool, EvaluationHalt> {
        let result = match disposition {
            CallDisposition::Copy(source) => {
                with_core_net_access(self.context, self.runtime, |runtime| {
                    runtime.resume_claimed_call_with_copy(self.call, source)
                });
                Ok(true)
            }
            CallDisposition::Operator(operator) => {
                with_core_net_access(self.context, self.runtime, |runtime| {
                    runtime.resume_claimed_call_with_operator(self.call, operator)
                });
                Ok(true)
            }
            CallDisposition::Failed(error) => {
                with_core_net_access(self.context, self.runtime, |runtime| {
                    runtime.fail_claimed_call(self.call, error.clone())
                });
                Err(error)
            }
            CallDisposition::Release => {
                let restored = self.restore_fallback();
                debug_assert!(restored, "released callable claim must remain current");
                Ok(false)
            }
        };
        self.fallback = None;
        result
    }

    fn restore_fallback(&self) -> bool {
        let Some(fallback) = self.fallback.clone() else {
            return true;
        };
        with_core_net_access(self.context, self.runtime, |runtime| match fallback {
            CallFallback::Ready => runtime.release_claimed_call(self.call),
            CallFallback::Blocked(wait) => runtime.restore_blocked_call(self.call, wait),
        })
    }
}

impl Drop for CoreCallClaim<'_, '_> {
    fn drop(&mut self) {
        if self.fallback.is_some() {
            let _ = self.restore_fallback();
        }
    }
}

#[must_use = "a callable checkpoint claim must be published, terminalized, or restored"]
struct CoreCheckpointClaim<'claim, 'scope> {
    access: &'claim crate::evaluation::EvaluationValueAccess<'scope>,
    runtime: CoreRuntimeNetAccess<'claim, 'scope>,
    call: crate::interaction_net::CallableCheckpointCall,
    work: Option<crate::eval::whnf::RegionalWhnfWork>,
}

impl<'claim, 'scope> CoreCheckpointClaim<'claim, 'scope> {
    fn take(
        access: &'claim crate::evaluation::EvaluationValueAccess<'scope>,
        runtime: &'claim CoreRuntimeNet,
        pair: ActivePairKey,
    ) -> Option<Self> {
        let runtime = access.net(runtime);
        let call = runtime.callable_checkpoint(pair)?;
        let state = runtime.take_claimed_callable_checkpoint(call)?;
        Some(Self {
            access,
            runtime,
            call,
            work: Some(state.into_regional(access)),
        })
    }

    fn drive(
        &mut self,
        budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> crate::eval::whnf::RegionalWhnfStatus {
        crate::eval::whnf::drive_regional_in_place(
            self.access,
            &mut self.work,
            budget,
            crate::eval::whnf::reduce_semantic_shell,
        )
    }

    #[cfg(test)]
    fn observation_for_test(&self) -> crate::eval::whnf::NetWhnfObservation {
        self.work
            .as_ref()
            .expect("claimed checkpoint retains its complete regional state")
            .observation_for_test(self.access.values())
    }

    fn publish(mut self) -> Result<crate::interaction_net::CallableCheckpointCall, EvaluationHalt> {
        let work = self
            .work
            .take()
            .expect("claimed checkpoint retains its complete regional state");
        let state = crate::eval::whnf::NetWhnfState::from_regional(self.access, work);
        self.runtime
            .replace_claimed_callable_checkpoint(self.call, state)
            .map_err(|state| {
                drop(state);
                EvaluationHalt::new("interaction-net checkpoint lost its claimed generation")
            })
    }

    fn finish(mut self, callable: CoreCallable) -> Result<(), EvaluationHalt> {
        drop(self.work.take());
        match callable {
            CoreCallable::Net(source) => {
                let source = self.access.net(&source).prepare_copy_source();
                self.runtime
                    .resume_claimed_checkpoint_with_copy(self.call, source);
            }
            CoreCallable::Operator(operator) => self
                .runtime
                .resume_claimed_checkpoint_with_operator(self.call, operator),
        }
        Ok(())
    }

    fn fail(mut self, error: EvaluationHalt) -> Result<(), EvaluationHalt> {
        drop(self.work.take());
        self.runtime
            .fail_claimed_callable_checkpoint(self.call, error)
    }
}

impl Drop for CoreCheckpointClaim<'_, '_> {
    fn drop(&mut self) {
        let Some(work) = self.work.take() else {
            return;
        };
        let state = crate::eval::whnf::NetWhnfState::from_regional(self.access, work);
        if let Err(state) = self
            .runtime
            .restore_claimed_callable_checkpoint(self.call, state)
        {
            drop(state);
        }
    }
}

enum OperatorDisposition {
    Yield(OperatorYield<CoreSpecialization>),
    #[cfg(test)]
    Blocked(crate::core_net::CoreWaitToken),
    Failed(EvaluationHalt),
    #[allow(
        dead_code,
        reason = "the explicit release disposition is exercised by claim-protocol tests"
    )]
    Release,
}

#[derive(Clone)]
enum OperatorFallback {
    Ready,
    Blocked(crate::core_net::CoreWaitToken),
}

/// One stack-bound operator claim. It owns the semantic payload clones and
/// exact replay fallback, but carries no managed-access view.
#[must_use = "an operator claim must be terminalized or released"]
struct CoreOperatorClaim<'claim, 'step> {
    context: &'claim EvaluatorStepContext<'step>,
    runtime: &'claim CoreRuntimeNet,
    call: OperatorCall,
    operator: CoreOperator,
    data: Value,
    fallback: Option<OperatorFallback>,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl<'claim, 'step> CoreOperatorClaim<'claim, 'step> {
    fn fresh(
        context: &'claim EvaluatorStepContext<'step>,
        runtime: &'claim CoreRuntimeNet,
        call: OperatorCall,
    ) -> Option<Self> {
        let (operator, data) = with_core_net_access(context, runtime, |runtime| {
            runtime.claim_operator_call(call)
        })?;
        Some(Self {
            context,
            runtime,
            call,
            operator,
            data,
            fallback: Some(OperatorFallback::Ready),
            _thread_bound: std::marker::PhantomData,
        })
    }

    fn retry(
        context: &'claim EvaluatorStepContext<'step>,
        runtime: &'claim CoreRuntimeNet,
        blocked: BlockedOperatorCall<crate::core_net::CoreWaitToken>,
    ) -> Option<Self> {
        let fallback = OperatorFallback::Blocked(blocked.wait.clone());
        let (call, operator, data) = with_core_net_access(context, runtime, |runtime| {
            runtime.reclaim_blocked_operator_call(&blocked)
        })?;
        Some(Self {
            context,
            runtime,
            call,
            operator,
            data,
            fallback: Some(fallback),
            _thread_bound: std::marker::PhantomData,
        })
    }

    fn parts<'access>(
        &'access self,
        _access: &'access crate::evaluation::EvaluationValueAccess<'_>,
    ) -> (&'access CoreOperator, &'access Value) {
        (&self.operator, &self.data)
    }

    #[cfg(test)]
    fn finish(self, disposition: OperatorDisposition) -> Result<bool, EvaluationHalt> {
        let context = self.context;
        context.with_value_access(|access| self.finish_in(&access, disposition))
    }

    fn finish_in(
        mut self,
        access: &crate::evaluation::EvaluationValueAccess<'_>,
        disposition: OperatorDisposition,
    ) -> Result<bool, EvaluationHalt> {
        let result = match disposition {
            OperatorDisposition::Yield(result) => {
                access
                    .net(self.runtime)
                    .complete_claimed_operator_call(self.call, result);
                Ok(true)
            }
            #[cfg(test)]
            OperatorDisposition::Blocked(wait) => {
                access
                    .net(self.runtime)
                    .block_claimed_operator_call(self.call, wait);
                Ok(true)
            }
            OperatorDisposition::Failed(error) => {
                access
                    .net(self.runtime)
                    .fail_claimed_operator_call(self.call, error.clone());
                Err(error)
            }
            OperatorDisposition::Release => {
                let restored = self.restore_fallback();
                debug_assert!(restored, "released operator claim must remain current");
                Ok(false)
            }
        };
        self.fallback = None;
        result
    }

    fn restore_fallback(&self) -> bool {
        let Some(fallback) = self.fallback.clone() else {
            return true;
        };
        with_core_net_access(self.context, self.runtime, |runtime| match fallback {
            OperatorFallback::Ready => runtime.release_claimed_operator_call(self.call),
            OperatorFallback::Blocked(wait) => {
                runtime.restore_blocked_operator_call(self.call, wait)
            }
        })
    }
}

impl Drop for CoreOperatorClaim<'_, '_> {
    fn drop(&mut self) {
        if self.fallback.is_some() {
            let _ = self.restore_fallback();
        }
    }
}

fn classify_core_callable_in(
    access: &crate::evaluation::EvaluationValueAccess<'_>,
    value: Value,
) -> Result<CoreCallable, EvaluationHalt> {
    match value {
        Value::Net(net) => Ok(CoreCallable::Net(net.into_runtime())),
        Value::Builtin(builtin) => Ok(CoreCallable::Operator(builtin_operator(
            access.values(),
            BuiltinCall::new(builtin),
        ))),
        Value::PartialBuiltin(call) => Ok(CoreCallable::Operator(builtin_operator(
            access.values(),
            call,
        ))),
        value @ (Value::Function(_) | Value::Dict(_)) => Ok(CoreCallable::Operator(
            applicable_operator(access.values(), value),
        )),
        value @ (Value::Atom(_)
        | Value::Number(_)
        | Value::Binary(_)
        | Value::List(_)
        | Value::Metadata(_)
        | Value::Opaque(_)) => Err(non_callable_error(access.values(), &value)),
        Value::Lazy(_) | Value::Promised(_) => {
            unreachable!("callable classification requires WHNF")
        }
    }
}

enum CallableWhnfOutcome {
    Ready(CoreCallable),
    Yielded,
    Boundary {
        call: crate::interaction_net::CallableCheckpointCall,
        request: crate::eval::whnf::RegionalBoundaryRequest,
    },
    Failed(EvaluationHalt),
}

fn drive_original_callable_whnf(
    context: &EvaluatorStepContext<'_>,
    claim: &mut CoreCallClaim<'_, '_>,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> CallableWhnfOutcome {
    context.with_value_access(|access| {
        let callable = claim.callable(&access);
        if !matches!(callable, Value::Lazy(_) | Value::Promised(_)) {
            return match classify_core_callable_in(&access, callable) {
                Ok(callable) => CallableWhnfOutcome::Ready(callable),
                Err(error) => CallableWhnfOutcome::Failed(error),
            };
        }

        let work = crate::eval::whnf::RegionalWhnfWork::from_focus(&access, callable);
        match crate::eval::whnf::drive_regional(
            &access,
            work,
            step_budget,
            crate::eval::whnf::reduce_semantic_shell,
        ) {
            crate::eval::whnf::RegionalWhnfDrive::Ready(value) => {
                match classify_core_callable_in(&access, value) {
                    Ok(callable) => CallableWhnfOutcome::Ready(callable),
                    Err(error) => CallableWhnfOutcome::Failed(error),
                }
            }
            crate::eval::whnf::RegionalWhnfDrive::Boundary { work, request } => {
                let state = crate::eval::whnf::NetWhnfState::from_regional(&access, work);
                match claim.install_checkpoint_in(&access, state) {
                    Ok(call) => CallableWhnfOutcome::Boundary { call, request },
                    Err(error) => CallableWhnfOutcome::Failed(error),
                }
            }
            crate::eval::whnf::RegionalWhnfDrive::Yielded(work) => {
                let state = crate::eval::whnf::NetWhnfState::from_regional(&access, work);
                match claim.install_checkpoint_in(&access, state) {
                    Ok(_) => CallableWhnfOutcome::Yielded,
                    Err(error) => CallableWhnfOutcome::Failed(error),
                }
            }
            crate::eval::whnf::RegionalWhnfDrive::Failed(failure) => {
                CallableWhnfOutcome::Failed(EvaluationHalt::failure(failure))
            }
        }
    })
}

#[cfg(test)]
pub(super) fn classify_core_callable(
    context: &EvalContext,
    value: Value,
) -> Result<CoreCallable, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| {
        evaluator.with_value_access(|access| classify_core_callable_in(&access, value))
    })
}

fn progress_exact_core_call_in(
    context: &EvaluatorStepContext<'_>,
    runtime: &CoreRuntimeNet,
    call: Call,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> Result<bool, EvaluationHalt> {
    let Some(claim) = CoreCallClaim::fresh(context, runtime, call) else {
        return Ok(false);
    };
    progress_core_call_claim(context, claim, step_budget)
}

fn progress_core_call_claim(
    context: &EvaluatorStepContext<'_>,
    claim: CoreCallClaim<'_, '_>,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> Result<bool, EvaluationHalt> {
    progress_core_call_claim_with_boundary_handoff(context, claim, step_budget, || {})
}

fn progress_core_call_claim_with_boundary_handoff(
    context: &EvaluatorStepContext<'_>,
    mut claim: CoreCallClaim<'_, '_>,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
    before_block: impl FnOnce(),
) -> Result<bool, EvaluationHalt> {
    let outcome = drive_original_callable_whnf(context, &mut claim, step_budget);
    let disposition = match outcome {
        CallableWhnfOutcome::Ready(CoreCallable::Net(source)) => {
            let source =
                with_core_net_access(context, &source, |source| source.prepare_copy_source());
            CallDisposition::Copy(source)
        }
        CallableWhnfOutcome::Ready(CoreCallable::Operator(operator)) => {
            CallDisposition::Operator(operator)
        }
        CallableWhnfOutcome::Yielded => return Ok(true),
        CallableWhnfOutcome::Boundary { call, request } => {
            settle_callable_checkpoint_boundary(
                context,
                claim.runtime,
                call,
                request,
                before_block,
            )?;
            return Ok(true);
        }
        CallableWhnfOutcome::Failed(error) => CallDisposition::Failed(error),
    };
    claim.finish(disposition)
}

fn settle_callable_checkpoint_boundary(
    context: &EvaluatorStepContext<'_>,
    runtime: &CoreRuntimeNet,
    call: crate::interaction_net::CallableCheckpointCall,
    request: crate::eval::whnf::RegionalBoundaryRequest,
    before_block: impl FnOnce(),
) -> Result<(), EvaluationHalt> {
    let wait = match callable_boundary_wait(context.context(), request) {
        Ok(wait) => wait,
        Err(error) => {
            let failure = with_core_net_access(context, runtime, |runtime| {
                runtime.fail_published_callable_checkpoint(call, error.clone())
            });
            return if failure.is_ok() {
                Err(error)
            } else {
                // A newer generation or terminal result won the exact-state
                // race. That worker remains authoritative; this stale
                // admission must not publish a competing failure.
                Ok(())
            };
        }
    };
    before_block();
    let result = with_core_net_access(context, runtime, |runtime| {
        runtime.block_callable_checkpoint(call, wait)
    });
    debug_assert!(matches!(
        result,
        crate::interaction_net::CheckpointBlockResult::Blocked
            | crate::interaction_net::CheckpointBlockResult::Disturbed
    ));
    Ok(())
}

fn callable_boundary_wait(
    context: &EvalContext,
    request: crate::eval::whnf::RegionalBoundaryRequest,
) -> Result<crate::core_net::CoreWaitToken, EvaluationHalt> {
    use crate::eval::whnf::{RegionalBoundaryRequest, WhnfDeferredRequest, WhnfDependency};

    let wait = match request {
        RegionalBoundaryRequest::Dependency(WhnfDependency::Wait(wait)) => return Ok(wait),
        RegionalBoundaryRequest::Dependency(WhnfDependency::Promise(promise))
        | RegionalBoundaryRequest::Deferred(WhnfDeferredRequest::PromiseFollow(promise)) => {
            promise_root_wait(context, &promise)
        }
        RegionalBoundaryRequest::Deferred(WhnfDeferredRequest::Lazy(lazy)) => {
            lazy_root_wait(context, &lazy)
        }
        RegionalBoundaryRequest::Deferred(WhnfDeferredRequest::Promise(promise)) => {
            if let Some(producer) = promise.producer()
                && context.observes_as_task(producer.owner())
            {
                return Err(EvaluationHalt::new(format!(
                    "reflection promise {} recursively observed itself in task {}",
                    promise.id().get(),
                    producer.owner().get()
                )));
            }
            promise_root_wait(context, &promise)
        }
        RegionalBoundaryRequest::External(_) => {
            return Err(EvaluationHalt::new(
                "callable WHNF reached an unsupported external boundary",
            ));
        }
    };
    wait.map(crate::core_net::CoreWaitToken)
        .map_err(|error| EvaluationHalt::new(error.as_ref()))
}

enum CheckpointWhnfOutcome {
    Progressed,
    Boundary {
        call: crate::interaction_net::CallableCheckpointCall,
        request: crate::eval::whnf::RegionalBoundaryRequest,
    },
}

fn progress_callable_checkpoint(
    context: &EvaluatorStepContext<'_>,
    runtime: &CoreRuntimeNet,
    pair: ActivePairKey,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> Result<(), EvaluationHalt> {
    let outcome = context.with_value_access(|access| {
        let Some(mut claim) = CoreCheckpointClaim::take(&access, runtime, pair) else {
            return Err(EvaluationHalt::new(
                "interaction-net checkpoint lost its exact claimed state",
            ));
        };
        match claim.drive(step_budget) {
            crate::eval::whnf::RegionalWhnfStatus::Ready(value) => {
                match classify_core_callable_in(&access, value) {
                    Ok(callable) => claim.finish(callable)?,
                    Err(error) => {
                        claim.fail(error.clone())?;
                        return Err(error);
                    }
                }
                Ok(CheckpointWhnfOutcome::Progressed)
            }
            crate::eval::whnf::RegionalWhnfStatus::Boundary(request) => {
                let call = claim.publish()?;
                Ok(CheckpointWhnfOutcome::Boundary { call, request })
            }
            crate::eval::whnf::RegionalWhnfStatus::Yielded => {
                claim.publish()?;
                Ok(CheckpointWhnfOutcome::Progressed)
            }
            crate::eval::whnf::RegionalWhnfStatus::Failed(failure) => {
                let error = EvaluationHalt::failure(failure);
                claim.fail(error.clone())?;
                Err(error)
            }
        }
    })?;

    let CheckpointWhnfOutcome::Boundary { call, request } = outcome else {
        return Ok(());
    };
    settle_callable_checkpoint_boundary(context, runtime, call, request, || {})
}

#[cfg(test)]
fn progress_exact_core_call(
    context: &EvalContext,
    runtime: &CoreRuntimeNet,
    call: Call,
) -> Result<bool, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| {
        progress_exact_core_call_in(
            evaluator,
            runtime,
            call,
            &mut crate::evaluation::EvaluationStepBudget::new(usize::MAX),
        )
    })
}

fn stuck_pair_error_in(
    runtime: &CoreRuntimeNetAccess<'_, '_>,
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
    context: &EvaluatorStepContext<'_>,
    runtime: &CoreRuntimeNet,
    call: OperatorCall,
) -> Result<bool, EvaluationHalt> {
    let Some(claim) = CoreOperatorClaim::fresh(context, runtime, call) else {
        return Ok(false);
    };
    progress_core_operator_claim(context, claim)
}

#[cfg(test)]
fn progress_exact_core_operator_call(
    context: &EvalContext,
    runtime: &CoreRuntimeNet,
    call: OperatorCall,
) -> Result<bool, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| {
        progress_core_operator_call(evaluator, runtime, call)
    })
}

fn progress_core_operator_claim(
    context: &EvaluatorStepContext<'_>,
    claim: CoreOperatorClaim<'_, '_>,
) -> Result<bool, EvaluationHalt> {
    context.with_value_access(|access| {
        let (operator, data) = claim.parts(&access);
        let disposition = match apply_core_operator(&access, operator, data) {
            Ok(result) => OperatorDisposition::Yield(result),
            Err(error) => {
                // Core operator errors already identify the failed semantic
                // operation. Preserve that structured error while retaining
                // the operator itself in the stuck pair for runtime inspection.
                OperatorDisposition::Failed(error)
            }
        };
        claim.finish_in(&access, disposition)
    })
}

pub(super) fn resolve_core_access_in(
    context: &EvaluatorStepContext<'_>,
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
                let value = eval_value_in(context, value)?;
                vec![value_to_key_in(context, &value)?]
            }
            CoreDataKey::PathIndex => eval_key_path_list_in(
                context,
                dynamic
                    .next()
                    .expect("lowered access path index must exist"),
            )?,
        };
        for key in keys {
            let value = eval_value_in(context, &current)?;
            let Value::Dict(dict) = value else {
                return Err(EvaluationHalt::new("value access base is not a dictionary"));
            };
            current = dict
                .get(&key)
                .cloned()
                .unwrap_or_else(|| Value::Dict(crate::core::Dict::new_sync()));
        }
    }
    eval_value_in(context, &current)
}

#[cfg(test)]
mod driver_tests {
    use super::*;
    use crate::interaction_net::RuntimeNet;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn assert_net_driver_bounded_owner_inventory(
        request: &NormalizationRequest,
        work: &NetDriverWork,
        worklist: &NetDriverWorklist,
        driver: &NetDriver,
    ) {
        let NormalizationRequest {
            root,
            root_interface,
        } = request;
        let _: &crate::core::ManagedCoreNetRoot = root;
        let _: &Port = root_interface;

        match work {
            NetDriverWork::RequestRoot { root, interface } => {
                let _: &crate::core::ManagedCoreNetRoot = root;
                let _: &Port = interface;
            }
            NetDriverWork::Cursor { root, cursor } => {
                let _: &crate::core::ManagedCoreNetRoot = root;
                let _: &crate::interaction_net::NodeId = cursor;
            }
            NetDriverWork::ActivePair { root, pair } => {
                let _: &crate::core::ManagedCoreNetRoot = root;
                let _: &ActivePairKey = pair;
            }
            NetDriverWork::ObservedCursor {
                observation,
                cursor,
            } => {
                let _: &FrontierObservation = observation;
                let _: &crate::interaction_net::NodeId = cursor;
            }
            NetDriverWork::ObservedActivePair { observation, pair } => {
                let _: &FrontierObservation = observation;
                let _: &ActivePairKey = pair;
            }
            NetDriverWork::ResumeCursorDependency {
                root,
                cursor,
                expected_dependency,
                disposition,
            } => {
                let _: &crate::core::ManagedCoreNetRoot = root;
                let _: &crate::interaction_net::NodeId = cursor;
                let _: &CursorDependency = expected_dependency;
                let _: &CursorDependencyDisposition = disposition;
            }
        }

        let NetDriverWorklist { items } = worklist;
        let _: &Vec<NetDriverWork> = items;
        let NetDriver {
            request,
            worklist,
            progressed,
        } = driver;
        let _: &NormalizationRequest = request;
        let _: &NetDriverWorklist = worklist;
        let _: &bool = progressed;
    }

    macro_rules! assert_does_not_implement {
        ($module:ident, $type:ty, $trait:path) => {
            mod $module {
                use super::*;

                trait AmbiguousIfImplemented<Discriminator> {
                    fn verify() {}
                }

                struct Implemented;

                impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}
                impl<T: ?Sized + $trait> AmbiguousIfImplemented<Implemented> for T {}

                const _: fn() = || {
                    <$type as AmbiguousIfImplemented<_>>::verify();
                };
            }
        };
    }

    assert_does_not_implement!(
        core_call_claim_is_not_send,
        CoreCallClaim<'static, 'static>,
        Send
    );

    #[test]
    fn net_driver_owner_inventory_is_compile_exhaustive_and_bounded() {
        let _: fn(&NormalizationRequest, &NetDriverWork, &NetDriverWorklist, &NetDriver) =
            assert_net_driver_bounded_owner_inventory;
    }
    assert_does_not_implement!(
        core_call_claim_is_not_sync,
        CoreCallClaim<'static, 'static>,
        Sync
    );
    assert_does_not_implement!(
        core_operator_claim_is_not_send,
        CoreOperatorClaim<'static, 'static>,
        Send
    );
    assert_does_not_implement!(
        core_operator_claim_is_not_sync,
        CoreOperatorClaim<'static, 'static>,
        Sync
    );

    fn instantiate(
        template: crate::core_net::CoreInteractionNet,
    ) -> crate::core_net::CoreRuntimeNet {
        crate::core::test_value_factory().instantiate_core_net(&template)
    }

    fn normalization_request(runtime: &CoreRuntimeNet, interface: Port) -> NormalizationRequest {
        let context = test_context();
        super::with_direct_evaluator(&context, |evaluator| {
            NormalizationRequest::cursor_whnf(runtime, interface, evaluator)
        })
    }

    #[test]
    fn normalization_request_is_an_exact_temporary_net_owner() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let context = EvalContext::isolated(values.clone());
        let baseline = values
            .collect_managed_for_test()
            .expect("the normalization-request fixture should start collectible");
        let request = {
            let runtime = values.instantiate_core_net(&{
                let mut builder = NetBuilder::new();
                let exposed = builder.data(values.unit());
                builder.finish(exposed)
            });
            let exposed = runtime.test_with(&values, RuntimeNet::exposed);
            super::with_direct_evaluator(&context, |evaluator| {
                NormalizationRequest::cursor_whnf(&runtime, exposed, evaluator)
            })
        };

        let retained = values
            .collect_managed_for_test()
            .expect("the normalization request should retain its semantic net");
        assert_eq!(retained.root_entries(), baseline.root_entries() + 1);
        assert_eq!(retained.marked_slots(), baseline.marked_slots() + 1);

        drop(request);
        let retired = values
            .collect_managed_for_test()
            .expect("dropping the normalization request should retire its semantic net");
        assert_eq!(retired.root_entries(), baseline.root_entries());
        assert_eq!(retired.finalized_slots(), 1);
    }

    fn claimed_core_call_in(values: &CoreValueFactory, callable: Value) -> (CoreRuntimeNet, Call) {
        let mut net = NetBuilder::<CoreSpecialization>::new();
        let bind = net.push(crate::interaction_net::Node::Bind);
        let data = net.data(callable);
        let erase = net.push(crate::interaction_net::Node::Erase);
        net.wire(Port::principal(bind), data);
        net.wire(Port::auxiliary(bind, 2), Port::principal(erase));
        let runtime = values.instantiate_core_net(&net.finish(Port::auxiliary(bind, 1)));
        let pair = runtime.test_with(values, |net| net.active_pairs().next().unwrap());
        let reduction = runtime
            .test_with_optional_mut(values, |net| net.reduce_pair(pair))
            .expect("call fixture must be claimable");
        let ReductionKind::Call { bind, data } = reduction.kind else {
            panic!("bind-data fixture must produce a call")
        };
        (runtime, Call { pair, bind, data })
    }

    fn claimed_core_call(callable: Value) -> (CoreRuntimeNet, Call) {
        claimed_core_call_in(&crate::core::test_value_factory(), callable)
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum CurrentCallablePath {
        DirectCopy,
        DirectOperator,
        BlockedDependency,
        Failed,
    }

    fn observe_current_callable_path(runtime: &CoreRuntimeNet, call: Call) -> CurrentCallablePath {
        observe_current_callable_path_in(runtime, &crate::core::test_value_factory(), call)
    }

    fn observe_current_callable_path_in(
        runtime: &CoreRuntimeNet,
        values: &CoreValueFactory,
        call: Call,
    ) -> CurrentCallablePath {
        runtime.test_with(values, |net| {
            if net.blocked_call(call.pair).is_some() {
                return CurrentCallablePath::BlockedDependency;
            }
            if net.blocked_callable_checkpoint(call.pair).is_some() {
                return CurrentCallablePath::BlockedDependency;
            }
            if net.stuck_reason(call.pair).is_some() {
                return CurrentCallablePath::Failed;
            }
            match (net.node(call.bind), net.node(call.data)) {
                (Some(_), None) => CurrentCallablePath::DirectCopy,
                (None, None) => CurrentCallablePath::DirectOperator,
                state => panic!("unrecognized callable-lowering topology: {state:?}"),
            }
        })
    }

    fn checkpoint_observation(
        runtime: &CoreRuntimeNet,
        values: &CoreValueFactory,
        pair: ActivePairKey,
    ) -> (
        crate::interaction_net::CallableCheckpointCall,
        Vec<(usize, usize, usize)>,
    ) {
        runtime.test_with(values, |net| {
            let call = net
                .callable_checkpoint(pair)
                .expect("fixture must retain one callable checkpoint");
            let Some(crate::interaction_net::RuntimeNode::CallableCheckpoint(checkpoint)) =
                net.node(call.checkpoint)
            else {
                panic!("checkpoint token must identify its payload node")
            };
            let state = checkpoint
                .payload
                .as_ref()
                .expect("published checkpoint must retain its dormant state");
            (call, state.container_identities_for_test())
        })
    }

    fn checkpoint_semantic_observation(
        runtime: &CoreRuntimeNet,
        values: &CoreValueFactory,
        pair: ActivePairKey,
    ) -> crate::eval::whnf::NetWhnfObservation {
        runtime.with_test_access(values, |access| {
            access.with(|net| {
                let call = net
                    .callable_checkpoint(pair)
                    .expect("fixture must retain one callable checkpoint");
                let Some(crate::interaction_net::RuntimeNode::CallableCheckpoint(checkpoint)) =
                    net.node(call.checkpoint)
                else {
                    panic!("checkpoint token must identify its payload node")
                };
                checkpoint
                    .payload
                    .as_ref()
                    .expect("published checkpoint must retain its dormant state")
                    .observation_for_test(access.values())
            })
        })
    }

    #[test]
    fn callable_lowering_uses_one_bounded_checkpoint_driver() {
        let source = include_str!("net.rs");
        let claim = source
            .split_once("fn progress_core_call_claim(")
            .expect("claimed-call progress declaration must remain present")
            .1
            .split_once("#[cfg(test)]\nfn progress_exact_core_call(")
            .expect("claimed-call test adapter must remain after progress")
            .0;
        assert_eq!(
            claim
                .matches("drive_original_callable_whnf(context, &mut claim, step_budget)")
                .count(),
            1,
            "the claimed Bind >< Data path must have one bounded callable-WHNF driver"
        );
        assert!(!claim.contains("eval_value_in"));
    }

    #[cfg(feature = "interaction-net-profiling")]
    #[test]
    fn current_callable_profile_counts_only_terminal_call_rewrites() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let context = EvalContext::isolated(values.clone());
        let before = values.interaction_net_profile_snapshot();

        let (runtime, call) = claimed_core_call_in(&values, Value::Builtin(Builtin::Add));
        assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
        let after_builtin = values.interaction_net_profile_snapshot();
        assert_eq!(after_builtin.reductions.call - before.reductions.call, 1);

        let mut source = NetBuilder::<CoreSpecialization>::new();
        let source_data = source.data(values.unit());
        let source = values.instantiate_core_net(&source.finish(source_data));
        let (runtime, call) = claimed_core_call_in(&values, Value::Net(NetValue::new(source)));
        assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
        let after_net = values.interaction_net_profile_snapshot();
        assert_eq!(after_net.reductions.call - after_builtin.reductions.call, 1);

        let promise = PromisedValue::new(&values, "profiled unresolved callable");
        let (runtime, call) = claimed_core_call_in(&values, Value::Promised(promise));
        assert!(progress_exact_core_call(&context, &runtime, call).unwrap());

        let after = values.interaction_net_profile_snapshot();
        assert_eq!(
            after.reductions.call - after_net.reductions.call,
            0,
            "a call blocked on retryable dependency has not committed its semantic rewrite"
        );
        assert_eq!(
            after.reductions.operator_call - before.reductions.operator_call,
            0,
            "callable classification and fused operator installation are not separate reductions"
        );
    }

    fn claimed_core_operator_call(
        operator: CoreOperator,
        data: Value,
    ) -> (CoreRuntimeNet, OperatorCall) {
        let mut net = NetBuilder::<CoreSpecialization>::new();
        let [input, result] = net.operator(operator);
        let data = net.data(data);
        net.wire(input, data);
        let runtime = instantiate(net.finish(result));
        let pair = runtime.test_with(&crate::core::test_value_factory(), |net| {
            net.active_pairs().next().unwrap()
        });
        let reduction = runtime
            .test_with_optional_mut(&crate::core::test_value_factory(), |net| {
                net.reduce_pair(pair)
            })
            .expect("operator fixture must be claimable");
        let ReductionKind::OperatorCall { operator, data } = reduction.kind else {
            panic!("operator-data fixture must produce an operator call")
        };
        (
            runtime,
            OperatorCall {
                pair,
                operator,
                data,
            },
        )
    }

    #[test]
    fn cursor_dependency_work_orders_child_before_parent_retry() {
        let values = crate::core::test_value_factory();
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let data = builder.data(values.unit());
        let runtime = instantiate(builder.finish(data));
        let cursor = runtime.test_with(&values, |net| {
            net.interface_neighbor(net.exposed())
                .expect("closed data net must expose its data node")
                .node()
        });

        let mut worklist = NetDriverWorklist::default();
        let root = values.root_core_net(&runtime);
        let expected_root = root.clone();
        worklist.follow_cursor_dependency(root, cursor, CursorDependency::LocalCursor(cursor));

        values.with_runtime_value_access(|access| {
            match worklist.pop().expect("child work must be present") {
                NetDriverWork::Cursor {
                    root: child_root,
                    cursor: child,
                } => {
                    assert!(child_root.same_root_in(&expected_root, &access));
                    assert_eq!(child, cursor);
                }
                _ => panic!("cursor dependency must schedule its child first"),
            }
            match worklist.pop().expect("parent resumption must be present") {
                NetDriverWork::ResumeCursorDependency {
                    root: parent_root,
                    cursor: parent,
                    expected_dependency,
                    disposition,
                } => {
                    assert!(parent_root.same_root_in(&expected_root, &access));
                    assert_eq!(parent, cursor);
                    assert!(matches!(
                        expected_dependency,
                        CursorDependency::LocalCursor(actual) if actual == cursor
                    ));
                    assert_eq!(disposition, CursorDependencyDisposition::Progressed);
                }
                _ => panic!("cursor dependency must retain a parent retry"),
            }
            assert!(worklist.pop().is_none());
        });
    }

    #[test]
    fn stable_cursor_dependencies_propagate_through_pairless_layers() {
        let (leaf, _) = crate::core_net::CoreRuntimeNet::test_stable_auxiliary(
            &crate::core::test_value_factory(),
        );
        let (middle, middle_interface) = crate::core_net::CoreRuntimeNet::test_copy_layer(
            &crate::core::test_value_factory(),
            leaf,
        );
        let (root, root_interface) = crate::core_net::CoreRuntimeNet::test_copy_layer(
            &crate::core::test_value_factory(),
            middle.duplicate_for_test(&crate::core::test_value_factory()),
        );

        assert!(matches!(
            drive_net_work(
                &test_context(),
                &normalization_request(&root, root_interface),
            )
            .unwrap(),
            NetDriverOutcome::Root(InterfaceDemand::StableCursor(_))
        ));
        assert!(matches!(
            root.test_poll_interface_demand(&crate::core::test_value_factory(), root_interface),
            crate::interaction_net::InterfaceDemand::StableCursor(_)
        ));
        assert!(matches!(
            middle.test_poll_interface_demand(&crate::core::test_value_factory(), middle_interface),
            crate::interaction_net::InterfaceDemand::StableCursor(_)
        ));
    }

    #[test]
    fn stable_cursor_dependencies_propagate_through_mixed_owner_layers() {
        let (leaf, _) = crate::core_net::CoreRuntimeNet::test_stable_auxiliary(
            &crate::core::test_value_factory(),
        );
        let (middle, middle_interface, middle_cursor) =
            crate::core_net::CoreRuntimeNet::test_pair_owned_copy_layer(
                &crate::core::test_value_factory(),
                leaf,
            );
        let (root, root_interface) = crate::core_net::CoreRuntimeNet::test_copy_layer(
            &crate::core::test_value_factory(),
            middle.duplicate_for_test(&crate::core::test_value_factory()),
        );

        assert!(matches!(
            drive_net_work(
                &test_context(),
                &normalization_request(&root, root_interface),
            )
            .unwrap(),
            NetDriverOutcome::Root(InterfaceDemand::StableCursor(_))
        ));
        assert!(matches!(
            root.test_poll_interface_demand(&crate::core::test_value_factory(), root_interface),
            crate::interaction_net::InterfaceDemand::StableCursor(_)
        ));
        assert_eq!(
            middle.test_poll_interface_demand(&crate::core::test_value_factory(), middle_interface),
            crate::interaction_net::InterfaceDemand::StableCursor(middle_cursor)
        );
    }

    #[test]
    fn deep_stable_cursor_dependencies_exceed_the_former_recursion_limit() {
        let (mut source, _) = crate::core_net::CoreRuntimeNet::test_stable_auxiliary(
            &crate::core::test_value_factory(),
        );
        let mut root_interface =
            source.test_with(&crate::core::test_value_factory(), |net| net.exposed());

        for _ in 0..1_100 {
            (source, root_interface) = crate::core_net::CoreRuntimeNet::test_copy_layer(
                &crate::core::test_value_factory(),
                source,
            );
        }

        assert!(matches!(
            drive_net_work(
                &test_context(),
                &normalization_request(&source, root_interface),
            )
            .unwrap(),
            NetDriverOutcome::Root(InterfaceDemand::StableCursor(_))
        ));
        assert!(matches!(
            source.test_poll_interface_demand(&crate::core::test_value_factory(), root_interface),
            InterfaceDemand::StableCursor(_)
        ));
        assert_eq!(
            source.active_normalization_batch(&crate::core::test_value_factory()),
            None
        );
    }

    fn assert_productive_cursor_chain_alternates_pairless_and_pair_owned_layers(layers: usize) {
        let expected = crate::core::test_value_factory().unit();
        let mut leaf = NetBuilder::<CoreSpecialization>::new();
        let data = leaf.data(expected.clone());
        let mut source = instantiate(leaf.finish(data));
        let mut root_interface =
            source.test_with(&crate::core::test_value_factory(), |net| net.exposed());

        for layer in 0..layers {
            if layer % 2 == 0 {
                (source, root_interface) =
                    crate::core_net::CoreRuntimeNet::test_productive_pair_owned_copy_layer(
                        &crate::core::test_value_factory(),
                        source,
                    );
            } else {
                (source, root_interface) = crate::core_net::CoreRuntimeNet::test_copy_layer(
                    &crate::core::test_value_factory(),
                    source,
                );
            }
        }

        assert_eq!(
            normalization_request(&source, root_interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::Data
        );
        assert_eq!(
            source.test_with(&crate::core::test_value_factory(), |net| net
                .interface_data(root_interface)
                .cloned()),
            Some(expected)
        );
        assert_eq!(
            source.active_normalization_batch(&crate::core::test_value_factory()),
            None
        );
    }

    #[test]
    fn productive_cursor_chain_alternates_pairless_and_pair_owned_layers() {
        assert_productive_cursor_chain_alternates_pairless_and_pair_owned_layers(128);
    }

    #[test]
    #[ignore = "cursor stress fixture; run scripts/check-cursor-stress.sh"]
    fn cursor_stress_deep_productive_chain_alternates_pairless_and_pair_owned_layers() {
        assert_productive_cursor_chain_alternates_pairless_and_pair_owned_layers(1_100);
    }

    #[test]
    fn stable_root_does_not_reduce_disconnected_or_undemanded_ready_work() {
        let mut disconnected = NetBuilder::<CoreSpecialization>::new();
        let root = disconnected.push(crate::interaction_net::Node::Erase);
        let left = disconnected.push(crate::interaction_net::Node::Erase);
        let right = disconnected.push(crate::interaction_net::Node::Erase);
        disconnected.wire(Port::principal(left), Port::principal(right));
        let disconnected = instantiate(disconnected.finish(Port::principal(root)));
        let disconnected_interface =
            disconnected.test_with(&crate::core::test_value_factory(), |net| net.exposed());
        let before = disconnected.test_with(&crate::core::test_value_factory(), |net| {
            net.active_pairs().collect::<Vec<_>>()
        });
        assert_eq!(
            normalization_request(&disconnected, disconnected_interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::NormalForm
        );
        assert_eq!(
            disconnected.test_with(&crate::core::test_value_factory(), |net| net
                .active_pairs()
                .collect::<Vec<_>>()),
            before
        );

        let mut branched = NetBuilder::<CoreSpecialization>::new();
        let root = branched.push_fan();
        let active = branched.push(crate::interaction_net::Node::Bind);
        let erase = branched.push(crate::interaction_net::Node::Erase);
        branched.wire(Port::auxiliary(root, 1), Port::auxiliary(active, 1));
        branched.wire(Port::auxiliary(root, 2), Port::auxiliary(active, 2));
        branched.wire(Port::principal(active), Port::principal(erase));
        let branched = instantiate(branched.finish(Port::principal(root)));
        let branched_interface =
            branched.test_with(&crate::core::test_value_factory(), |net| net.exposed());
        let before = branched.test_with(&crate::core::test_value_factory(), |net| {
            net.active_pairs().collect::<Vec<_>>()
        });
        assert_eq!(
            normalization_request(&branched, branched_interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::NormalForm
        );
        assert_eq!(
            branched.test_with(&crate::core::test_value_factory(), |net| net
                .active_pairs()
                .collect::<Vec<_>>()),
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
        let runtime = instantiate(net.finish(Port::auxiliary(left, 1)));
        let interface = runtime.test_with(&crate::core::test_value_factory(), |net| net.exposed());
        let demanded = runtime.test_with(&crate::core::test_value_factory(), |net| {
            let pairs = net.active_pairs().collect::<Vec<_>>();
            assert_eq!(pairs.len(), 1);
            pairs[0]
        });
        assert_eq!(
            normalization_request(&runtime, interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::Data
        );
        assert!(
            !runtime.test_with(&crate::core::test_value_factory(), |net| net
                .contains_active_pair(demanded))
        );
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
        let claimed = instantiate(claimed.finish(Port::principal(root)));
        let interface = claimed.test_with(&crate::core::test_value_factory(), |net| net.exposed());
        let pair = claimed.test_with(&crate::core::test_value_factory(), |net| {
            net.active_pairs().next().unwrap()
        });
        assert!(matches!(
            claimed.test_with_optional_mut(&crate::core::test_value_factory(), |net| net
                .reduce_pair(pair)),
            Some(Reduction {
                kind: ReductionKind::Call { .. },
                ..
            })
        ));
        assert!(
            claimed.test_with(&crate::core::test_value_factory(), |net| net
                .pair_is_claimed(pair))
        );
        assert_eq!(
            normalization_request(&claimed, interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::NormalForm
        );
        assert!(
            claimed.test_with(&crate::core::test_value_factory(), |net| net
                .pair_is_claimed(pair))
        );

        let mut source = NetBuilder::<CoreSpecialization>::new();
        let data = source.data(crate::core::test_value_factory().unit());
        let source = instantiate(source.finish(data));
        let (claimed_cursor, cursor_interface, cursor) =
            crate::core_net::CoreRuntimeNet::test_stable_root_with_claimed_cursor(
                &crate::core::test_value_factory(),
                source,
            );
        assert_eq!(
            normalization_request(&claimed_cursor, cursor_interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::NormalForm
        );
        assert!(matches!(
            claimed_cursor.test_step_cursor(&crate::core::test_value_factory(), cursor),
            CursorStep::Contended(_)
        ));

        let mut stuck = NetBuilder::<CoreSpecialization>::new();
        let root = stuck.push(crate::interaction_net::Node::Erase);
        let left = stuck.data(crate::core::test_value_factory().unit());
        let right = stuck.data(crate::core::test_value_factory().unit());
        stuck.wire(left, right);
        let stuck = instantiate(stuck.finish(Port::principal(root)));
        let interface = stuck.test_with(&crate::core::test_value_factory(), |net| net.exposed());
        let pair = stuck.test_with(&crate::core::test_value_factory(), |net| {
            net.active_pairs().next().unwrap()
        });
        assert!(matches!(
            stuck.test_with_optional_mut(&crate::core::test_value_factory(), |net| net
                .reduce_pair(pair)),
            Some(Reduction {
                kind: ReductionKind::Stuck,
                ..
            })
        ));
        assert_eq!(
            normalization_request(&stuck, interface)
                .drive(&test_context())
                .unwrap(),
            NetInterfaceOutcome::NormalForm
        );
        assert!(stuck.test_with(&crate::core::test_value_factory(), |net| {
            net.stuck_reason(pair).is_some()
        }));
    }

    #[test]
    fn demanded_claim_completion_before_wait_registration_is_not_lost() {
        let context = test_context();
        let mut source = NetBuilder::<CoreSpecialization>::new();
        let data = source.data(crate::core::test_value_factory().unit());
        let source = instantiate(source.finish(data));
        let (target, interface) = crate::core_net::CoreRuntimeNet::test_copy_layer(
            &crate::core::test_value_factory(),
            source,
        );
        let cursor = match target
            .test_poll_interface_demand(&crate::core::test_value_factory(), interface)
        {
            InterfaceDemand::Cursor(cursor) => cursor,
            other => panic!("copy root should expose a cursor, received {other:?}"),
        };
        assert!(
            target
                .test_claim_pairless_cursor_obligation(&crate::core::test_value_factory(), cursor)
        );
        let request = normalization_request(&target, interface);
        let mut driver = NetDriver::new(&request);
        let contention = match crate::eval::with_direct_evaluator(&context, |evaluator| {
            drive_net_driver_work_in(evaluator, &mut driver)
        })
        .unwrap()
        {
            NetDriverOutcome::Contended(contention) => contention,
            _ => panic!("claimed demanded cursor must report contention"),
        };
        assert!(matches!(
            target.test_advance_claimed_cursor(&crate::core::test_value_factory(), cursor),
            Some(crate::interaction_net::CursorProgress::Materialized { .. })
        ));
        contention.wait_for_disturbance();
        let outcome = crate::eval::with_direct_evaluator(&context, |evaluator| {
            loop {
                match drive_net_driver_work_in(evaluator, &mut driver)? {
                    NetDriverOutcome::Progressed => driver.restart_from_request_root(),
                    outcome => return Ok::<_, EvaluationHalt>(outcome),
                }
            }
        })
        .expect("the retained driver must observe the claimed cursor publication");
        assert!(matches!(
            outcome,
            NetDriverOutcome::Root(InterfaceDemand::Data)
        ));
    }

    #[test]
    fn contending_evaluator_hands_off_then_resumes_after_batch_publication() {
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let data = builder.data(crate::core::test_value_factory().unit());
        let runtime = instantiate(builder.finish(data));
        let interface = runtime.test_with(&crate::core::test_value_factory(), |net| net.exposed());

        let values = crate::core::test_value_factory();
        let leader_root = values.with_runtime_value_access(|access| runtime.root_in(&access));
        let (leader_ready_tx, leader_ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let leader = std::thread::spawn(move || {
            let values = crate::core::test_value_factory();
            values.with_runtime_value_access(|values| {
                let leader_runtime = CoreRuntimeNet::from_root(&leader_root, &values);
                let access = leader_runtime.access(&values);
                access
                    .with_normalization_batch(|_| {
                        leader_ready_tx.send(()).unwrap();
                        release_rx
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .expect("test must release the normalization owner");
                    })
                    .expect("the forced leader must acquire the normalization batch");
            });
        });
        leader_ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the normalization owner must publish acquisition");

        let request = normalization_request(&runtime, interface);
        let (registered_tx, registered_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let follower = std::thread::spawn(move || {
            let context = test_context();
            let mut registered_tx = Some(registered_tx);
            let result = crate::eval::with_direct_evaluator(&context, |evaluator| {
                drive_net_interface_with_contention_handoff(evaluator, &request, || {
                    registered_tx
                        .take()
                        .expect("one evaluator handoff should register once")
                        .send(())
                        .unwrap();
                })
            });
            result_tx.send(result).unwrap();
        });

        registered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the evaluator must reach the contention handoff");
        assert!(
            result_rx.try_recv().is_err(),
            "the evaluator must remain handed off until its owner publishes"
        );
        release_tx.send(()).unwrap();
        leader.join().expect("normalization owner must finish");
        assert_eq!(
            result_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("the evaluator must resume after publication")
                .expect("the resumed evaluator must normalize successfully"),
            NetInterfaceOutcome::Data
        );
        follower.join().expect("contending evaluator must finish");
        assert_eq!(
            runtime.active_normalization_batch(&crate::core::test_value_factory()),
            None
        );
    }

    #[test]
    fn persistent_driver_retains_work_across_batch_admission_contention() {
        let context = test_context();
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let data = builder.data(context.values().unit());
        let runtime = instantiate(builder.finish(data));
        let interface = runtime.test_with(context.values(), |net| net.exposed());

        let leader_values = context.values().clone();
        let leader_root =
            leader_values.with_runtime_value_access(|access| runtime.root_in(&access));
        let (leader_ready_tx, leader_ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let leader = std::thread::spawn(move || {
            leader_values.with_runtime_value_access(|access| {
                let runtime = CoreRuntimeNet::from_root(&leader_root, &access);
                runtime
                    .access(&access)
                    .with_normalization_batch(|_| {
                        leader_ready_tx.send(()).unwrap();
                        release_rx
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .expect("test must release the normalization owner");
                    })
                    .expect("the forced leader must acquire the normalization batch");
            });
        });
        leader_ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the normalization owner must publish acquisition");

        let request = normalization_request(&runtime, interface);
        let mut driver = NetDriver::new(&request);
        let contention =
            crate::eval::with_direct_evaluator(
                &context,
                |evaluator| match drive_net_driver_work_in(evaluator, &mut driver)? {
                    NetDriverOutcome::Contended(contention) => Ok::<_, EvaluationHalt>(contention),
                    _ => panic!("the forced batch owner must cause a contention handoff"),
                },
            )
            .expect("batch admission contention is not an evaluation failure");
        assert_eq!(
            driver.worklist.items.len(),
            1,
            "batch admission must retain the exact uninspected work item"
        );

        release_tx.send(()).unwrap();
        leader.join().expect("normalization owner must finish");
        contention.wait_for_disturbance();
        let outcome = crate::eval::with_direct_evaluator(&context, |evaluator| {
            drive_net_driver_work_in(evaluator, &mut driver)
        })
        .expect("the retained work item must resume after publication");
        assert!(matches!(
            outcome,
            NetDriverOutcome::Root(InterfaceDemand::Data)
        ));
    }

    #[test]
    fn semantic_wait_is_published_to_the_net_before_driver_parking() {
        let context = test_context();
        let promise = PromisedValue::new(context.values(), "forced semantic net wait");
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let [application, argument, result] = builder.bind();
        let function = builder.data(Value::Promised(promise));
        let value = builder.data(context.values().unit());
        builder.wire(application, function);
        builder.wire(argument, value);
        let runtime = instantiate(builder.finish(result));
        let interface = runtime.test_with(&crate::core::test_value_factory(), |net| net.exposed());

        let parked = normalization_request(&runtime, interface)
            .drive(&context)
            .expect_err("an unresolved callable promise must park the driver");
        let wait = parked
            .blocked_on()
            .expect("the parked driver must retain its semantic wait");
        let blocked = runtime
            .test_with(&crate::core::test_value_factory(), |net| {
                net.active_pairs()
                    .find_map(|pair| net.blocked_callable_checkpoint(pair))
            })
            .expect("the callable checkpoint must publish Blocked before parking");

        assert_eq!(blocked.wait.0, wait.0);
        assert!(matches!(
            context.poll_wait(&blocked.wait.0),
            crate::evaluation::EvaluationWaitPoll::Pending(_)
        ));
        assert_eq!(
            runtime.active_normalization_batch(&crate::core::test_value_factory()),
            None,
            "the semantic park must retain neither a claim nor a normalization lease"
        );
    }

    #[test]
    fn persistent_driver_requeues_the_exact_active_pair_before_semantic_parking() {
        let context = test_context();
        let promise = PromisedValue::new(context.values(), "persistent semantic net wait");
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let [application, argument, result] = builder.bind();
        let function = builder.data(Value::Promised(promise.clone()));
        let value = builder.data(context.values().unit());
        builder.wire(application, function);
        builder.wire(argument, value);
        let runtime = instantiate(builder.finish(result));
        let interface = runtime.test_with(context.values(), |net| net.exposed());
        let request = normalization_request(&runtime, interface);
        let mut driver = NetDriver::new(&request);

        let parked = match crate::eval::with_direct_evaluator(&context, |evaluator| {
            drive_net_driver_work_in(evaluator, &mut driver)
        }) {
            Err(error) => error,
            Ok(_) => panic!("the unresolved callable promise must park the persistent driver"),
        };
        let wait = parked
            .blocked_on()
            .expect("the parked driver must retain its exact semantic wait");
        let Some(NetDriverWork::ActivePair { root, .. }) = driver.worklist.items.last() else {
            panic!("the exact active pair must be retained for resumption")
        };
        context.values().with_runtime_value_access(|access| {
            assert!(root.same_root_in(&request.root, &access));
        });

        let callable = crate::eval::test_support::closed_function_value_in(
            context.values(),
            1,
            crate::eval::test_support::TestExpr::Value(context.values().unit()),
        );
        crate::core::set_test_promise(context.values(), &promise, callable)
            .expect("the callable promise should accept its assignment");
        assert!(matches!(
            context.pump_wait(&wait.0, 256),
            crate::evaluation::EvaluationPumpOutcome::TargetReady
        ));
        let outcome = crate::eval::with_direct_evaluator(&context, |evaluator| {
            loop {
                match drive_net_driver_work_in(evaluator, &mut driver)? {
                    NetDriverOutcome::Progressed => driver.restart_from_request_root(),
                    outcome => return Ok::<_, EvaluationHalt>(outcome),
                }
            }
        })
        .expect("the same driver must resume after its semantic dependency");
        match outcome {
            NetDriverOutcome::Root(InterfaceDemand::Data) => {}
            NetDriverOutcome::Root(other) => {
                panic!("resumed driver exposed unexpected root demand {other:?}")
            }
            NetDriverOutcome::Contended(_) => {
                panic!("resumed driver unexpectedly remained contended")
            }
            NetDriverOutcome::Progressed => unreachable!("the fixture loop consumes progress"),
            NetDriverOutcome::SemanticBudgetExhausted => {
                unreachable!("the test-only driver receives an unbounded semantic budget")
            }
            #[cfg(all(test, feature = "interaction-net-profiling"))]
            NetDriverOutcome::ProbeBudgetExhausted => {
                panic!("the fixture did not install a profiling work budget")
            }
        }
        assert_eq!(
            runtime.active_normalization_batch(context.values()),
            None,
            "parking and resumption must not retain a normalization lease"
        );
    }

    #[test]
    fn net_whnf_machine_retains_one_root_across_semantic_dependency() {
        let context = test_context();
        let promise = PromisedValue::new(context.values(), "net WHNF semantic wait");
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let [application, argument, result] = builder.bind();
        let function = builder.data(Value::Promised(promise.clone()));
        let value = builder.data(context.values().unit());
        builder.wire(application, function);
        builder.wire(argument, value);
        let runtime = instantiate(builder.finish(result));
        let interface = runtime.test_with(context.values(), |net| net.exposed());
        let mut machine = crate::eval::with_direct_evaluator(&context, |evaluator| {
            NetWhnfMachine::new(evaluator, runtime.clone(), interface, "test net")
        });
        context.values().with_runtime_value_access(|access| {
            assert!(
                machine
                    .retained_root()
                    .same_root_in(&runtime.root_in(&access), &access)
            );
        });

        let mut park_budget = crate::evaluation::EvaluationStepBudget::new(256);
        let parked = match crate::eval::with_direct_evaluator(&context, |evaluator| {
            machine.poll(evaluator, &mut park_budget)
        }) {
            Err(error) => error,
            Ok(_) => panic!("the unresolved callable must park the net-WHNF owner"),
        };
        let wait = parked
            .blocked_on()
            .expect("the owner must expose its exact semantic wait");

        let callable = crate::eval::test_support::closed_function_value_in(
            context.values(),
            1,
            crate::eval::test_support::TestExpr::Value(context.values().unit()),
        );
        crate::core::set_test_promise(context.values(), &promise, callable)
            .expect("the callable promise should accept its assignment");
        assert!(matches!(
            context.pump_wait(&wait.0, 256),
            crate::evaluation::EvaluationPumpOutcome::TargetReady
        ));

        let mut resume_budget = crate::evaluation::EvaluationStepBudget::new(256);
        let value = crate::eval::with_direct_evaluator(&context, |evaluator| {
            loop {
                match machine.poll(evaluator, &mut resume_budget)? {
                    NetWhnfPoll::Ready(value) => return Ok::<_, EvaluationHalt>(value),
                    NetWhnfPoll::Yielded => {}
                }
            }
        })
        .expect("the owner must resume through the same managed net");
        assert!(matches!(value, Value::Lazy(_)));
        assert_eq!(
            crate::eval::eval_value(&context, &value)
                .expect("the returned net payload must retain ordinary lazy demand"),
            context.values().unit()
        );
    }

    fn assigned_callable_machine(context: &EvalContext) -> NetWhnfMachine {
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let [application, argument, result] = builder.bind();
        let function = builder.data(Value::Builtin(Builtin::ListLen));
        let value = builder.data(context.values().unit());
        builder.wire(application, function);
        builder.wire(argument, value);
        let runtime = instantiate(builder.finish(result));
        let interface = runtime.test_with(context.values(), |net| net.exposed());
        crate::eval::with_direct_evaluator(context, |evaluator| {
            NetWhnfMachine::new(evaluator, runtime, interface, "NC1C budget fixture")
        })
    }

    #[test]
    fn net_whnf_machine_borrows_one_outer_semantic_budget() {
        let context = test_context();

        let mut zero_machine = assigned_callable_machine(&context);
        let mut zero = crate::evaluation::EvaluationStepBudget::new(0);
        crate::eval::with_direct_evaluator(&context, |evaluator| {
            assert!(matches!(
                zero_machine.poll(evaluator, &mut zero).unwrap(),
                NetWhnfPoll::Yielded
            ));
            assert!(matches!(
                zero_machine.poll(evaluator, &mut zero).unwrap(),
                NetWhnfPoll::Yielded
            ));
        });
        assert_eq!(zero.spent(), 0);
        assert_eq!(zero.remaining(), 0);

        let mut one_machine = assigned_callable_machine(&context);
        let mut one = crate::evaluation::EvaluationStepBudget::new(1);
        crate::eval::with_direct_evaluator(&context, |evaluator| {
            assert!(matches!(
                one_machine.poll(evaluator, &mut one).unwrap(),
                NetWhnfPoll::Yielded
            ));
            assert_eq!(one.spent(), 1);
            assert_eq!(one.remaining(), 0);
            assert!(matches!(
                one_machine.poll(evaluator, &mut one).unwrap(),
                NetWhnfPoll::Yielded
            ));
            assert_eq!(
                one.spent(),
                1,
                "a retry in one outer poll must not receive a fresh allowance"
            );
        });
        let mut final_one = crate::evaluation::EvaluationStepBudget::new(1);
        crate::eval::with_direct_evaluator(&context, |evaluator| {
            assert!(matches!(
                one_machine.poll(evaluator, &mut final_one).unwrap(),
                NetWhnfPoll::Ready(_)
            ));
        });
        assert_eq!(final_one.spent(), 1);
        assert_eq!(final_one.remaining(), 0);

        let mut many_machine = assigned_callable_machine(&context);
        let mut many = crate::evaluation::EvaluationStepBudget::new(5);
        crate::eval::with_direct_evaluator(&context, |evaluator| {
            assert!(matches!(
                many_machine.poll(evaluator, &mut many).unwrap(),
                NetWhnfPoll::Ready(_)
            ));
            assert_eq!(many.spent(), 2);
            assert_eq!(many.remaining(), 3);
        });
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
        let runtime = instantiate(net.finish(Port::auxiliary(bind, 1)));
        let interface = runtime.test_with(&crate::core::test_value_factory(), |net| net.exposed());
        let pair = runtime.test_with(&crate::core::test_value_factory(), |net| {
            net.active_pairs().next().unwrap()
        });
        let reduction = runtime
            .test_with_optional_mut(&crate::core::test_value_factory(), |net| {
                net.reduce_pair(pair)
            })
            .expect("demanded call should be claimable");
        let ReductionKind::Call { bind, data } = reduction.kind else {
            panic!("bind-data demand should be a call")
        };
        let call = Call { pair, bind, data };
        let request = normalization_request(&runtime, interface);
        let contention = match drive_net_work(&test_context(), &request).unwrap() {
            NetDriverOutcome::Contended(contention) => contention,
            _ => panic!("claimed demanded pair must report contention"),
        };
        runtime.test_with_mut(&crate::core::test_value_factory(), |net| {
            net.fail_claimed_call(call, EvaluationHalt::new("demanded call failed"))
        });
        contention.wait_for_disturbance();
        let failure = match drive_net_work(&test_context(), &request) {
            Err(failure) => failure,
            Ok(_) => panic!("demanded stuck pair must remain a failure"),
        };
        assert!(failure.to_string().contains("demanded call failed"));
    }

    #[test]
    fn progressing_a_claimed_call_publishes_only_its_completion() {
        let mut net = NetBuilder::<CoreSpecialization>::new();
        let bind = net.push(crate::interaction_net::Node::Bind);
        let data = net.data(Value::Builtin(Builtin::Add));
        let erase = net.push(crate::interaction_net::Node::Erase);
        net.wire(Port::principal(bind), data);
        net.wire(Port::auxiliary(bind, 2), Port::principal(erase));
        let runtime = instantiate(net.finish(Port::auxiliary(bind, 1)));
        let pair = runtime.test_with(&crate::core::test_value_factory(), |net| {
            net.active_pairs().next().unwrap()
        });
        let reduction = runtime
            .test_with_optional_mut(&crate::core::test_value_factory(), |net| {
                net.reduce_pair(pair)
            })
            .expect("builtin call should be claimable");
        let ReductionKind::Call { bind, data } = reduction.kind else {
            panic!("bind-data demand should be a call")
        };
        let call = Call { pair, bind, data };
        let before = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1
            .topology_revision();

        assert!(progress_exact_core_call(&test_context(), &runtime, call).unwrap());

        let after = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1
            .topology_revision();
        assert_eq!(
            after,
            before.checked_add(1).expect("revision must not overflow"),
            "reading the claimed payload must be quiet; only completion publishes"
        );
    }

    #[test]
    fn fresh_call_claim_release_restores_ready_work() {
        let context = test_context();
        let (runtime, call) = claimed_core_call(Value::Builtin(Builtin::Add));
        let before = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;

        crate::eval::with_direct_evaluator(&context, |evaluator| {
            let claim = CoreCallClaim::fresh(evaluator, &runtime, call)
                .expect("claimed call must issue its scoped guard");
            assert!(!claim.finish(CallDisposition::Release).unwrap());
        });

        let after = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;
        assert_eq!(after.topology_revision(), before.topology_revision() + 1);
        assert_eq!(after.disturbance_epoch(), before.disturbance_epoch() + 1);
        assert!(matches!(
            runtime.test_with_optional_mut(&crate::core::test_value_factory(), |net| net
                .reduce_pair(call.pair)),
            Some(Reduction {
                kind: ReductionKind::Call { .. },
                ..
            })
        ));
    }

    #[test]
    fn fresh_call_claim_unwind_restores_ready_work() {
        let context = test_context();
        let (runtime, call) = claimed_core_call(Value::Builtin(Builtin::Add));
        let before = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::eval::with_direct_evaluator(&context, |evaluator| {
                let _claim = CoreCallClaim::fresh(evaluator, &runtime, call)
                    .expect("claimed call must issue its scoped guard");
                panic!("forced callable-claim unwind");
            });
        }));

        assert!(unwind.is_err());
        let after = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;
        assert_eq!(after.topology_revision(), before.topology_revision() + 1);
        assert_eq!(after.disturbance_epoch(), before.disturbance_epoch() + 1);
        assert!(matches!(
            runtime.test_with_optional_mut(&crate::core::test_value_factory(), |net| net
                .reduce_pair(call.pair)),
            Some(Reduction {
                kind: ReductionKind::Call { .. },
                ..
            })
        ));
    }

    #[test]
    fn stale_fresh_call_claim_fails_quietly_before_guard_issuance() {
        let context = test_context();
        let (runtime, call) = claimed_core_call(Value::Builtin(Builtin::Add));
        runtime.test_with_mut(&crate::core::test_value_factory(), |net| {
            assert!(net.release_claimed_call(call))
        });
        let before = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;

        crate::eval::with_direct_evaluator(&context, |evaluator| {
            assert!(CoreCallClaim::fresh(evaluator, &runtime, call).is_none());
        });

        assert_eq!(
            runtime
                .test_with_revisions(&crate::core::test_value_factory(), |_| ())
                .1,
            before
        );
    }

    #[test]
    fn retried_checkpoint_claim_release_restores_the_exact_generation() {
        let context = test_context();
        let promise = PromisedValue::new(context.values(), "call-claim wait");
        let (runtime, call) = claimed_core_call(Value::Promised(promise));
        assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
        let blocked = runtime
            .test_with(&crate::core::test_value_factory(), |net| {
                net.blocked_callable_checkpoint(call.pair)
            })
            .expect("unassigned callable promise must block the checkpoint");

        crate::eval::with_direct_evaluator(&context, |evaluator| {
            assert!(with_core_net_access(evaluator, &runtime, |runtime| {
                runtime.retry_blocked_callable_checkpoint(&blocked)
            }));
        });
        runtime
            .test_with_optional_mut(context.values(), |net| net.reduce_pair(call.pair))
            .expect("ready checkpoint must be claimable");
        crate::eval::with_direct_evaluator(&context, |evaluator| {
            evaluator.with_value_access(|access| {
                let claim = CoreCheckpointClaim::take(&access, &runtime, call.pair)
                    .expect("exact checkpoint must issue its scoped claim");
                drop(claim);
            });
        });
        let restored = runtime
            .test_with(context.values(), |net| net.callable_checkpoint(call.pair))
            .expect("release restores the exact ready checkpoint");
        assert_eq!(restored, blocked.call);
        assert!(
            runtime
                .test_with(context.values(), |net| net
                    .blocked_callable_checkpoint(call.pair))
                .is_none()
        );
    }

    #[test]
    fn retried_checkpoint_claim_unwind_restores_the_exact_generation() {
        let context = test_context();
        let promise = PromisedValue::new(context.values(), "unwound call-claim wait");
        let (runtime, call) = claimed_core_call(Value::Promised(promise));
        assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
        let blocked = runtime
            .test_with(&crate::core::test_value_factory(), |net| {
                net.blocked_callable_checkpoint(call.pair)
            })
            .expect("unassigned callable promise must block the checkpoint");

        crate::eval::with_direct_evaluator(&context, |evaluator| {
            assert!(with_core_net_access(evaluator, &runtime, |runtime| {
                runtime.retry_blocked_callable_checkpoint(&blocked)
            }));
        });
        runtime
            .test_with_optional_mut(context.values(), |net| net.reduce_pair(call.pair))
            .expect("ready checkpoint must be claimable");

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::eval::with_direct_evaluator(&context, |evaluator| {
                evaluator.with_value_access(|access| {
                    let _claim = CoreCheckpointClaim::take(&access, &runtime, call.pair)
                        .expect("exact checkpoint must issue its scoped claim");
                    panic!("forced checkpoint-claim unwind");
                });
            });
        }));

        assert!(unwind.is_err());
        let restored = runtime
            .test_with(context.values(), |net| net.callable_checkpoint(call.pair))
            .expect("unwind restores the exact ready checkpoint");
        assert_eq!(restored, blocked.call);
    }

    #[test]
    fn mismatched_blocked_checkpoint_retry_fails_without_disturbance() {
        let context = test_context();
        let promise = PromisedValue::new(context.values(), "current call wait");
        let (runtime, call) = claimed_core_call(Value::Promised(promise));
        assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
        let blocked = runtime
            .test_with(&crate::core::test_value_factory(), |net| {
                net.blocked_callable_checkpoint(call.pair)
            })
            .expect("unassigned callable promise must block the checkpoint");
        let other = PromisedValue::new(context.values(), "unrelated call wait");
        let wrong_wait = crate::core_net::CoreWaitToken(
            promise_wait(&context, &other).expect("unrelated wait must allocate"),
        );
        assert_ne!(wrong_wait, blocked.wait);
        let before = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;

        crate::eval::with_direct_evaluator(&context, |evaluator| {
            let mismatch = crate::interaction_net::BlockedCallableCheckpoint {
                call: blocked.call,
                wait: wrong_wait,
            };
            assert!(!with_core_net_access(evaluator, &runtime, |runtime| {
                runtime.retry_blocked_callable_checkpoint(&mismatch)
            }));
        });

        assert_eq!(
            runtime
                .test_with_revisions(&crate::core::test_value_factory(), |_| ())
                .1,
            before
        );
        assert_eq!(
            runtime
                .test_with(&crate::core::test_value_factory(), |net| net
                    .blocked_callable_checkpoint(call.pair))
                .expect("mismatched retry must preserve the current wait")
                .wait,
            blocked.wait
        );
    }

    #[test]
    fn callable_claim_dispositions_cover_direct_deferred_blocked_and_failed_paths() {
        let context = test_context();

        let partial = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::Add,
            arguments: Arc::from([Value::Number(1.into())]),
        });
        let function = closed_function_value_in(context.values(), 2, TestExpr::Local(0));
        let mut source = NetBuilder::<CoreSpecialization>::new();
        let source_data = source.data(context.values().unit());
        let source = context
            .values()
            .instantiate_core_net(&source.finish(source_data));
        let callable_families = vec![
            (
                "builtin",
                Value::Builtin(Builtin::Add),
                CurrentCallablePath::DirectOperator,
            ),
            (
                "partial builtin",
                partial,
                CurrentCallablePath::DirectOperator,
            ),
            ("function", function, CurrentCallablePath::DirectOperator),
            (
                "dictionary",
                Value::Dict(crate::core::Dict::new_sync()),
                CurrentCallablePath::DirectOperator,
            ),
            (
                "raw net",
                Value::Net(NetValue::new(source)),
                CurrentCallablePath::DirectCopy,
            ),
        ];

        for (name, callable, expected) in callable_families {
            let direct = callable.clone();
            let lazy_result = callable.clone();
            let promise_result = callable;

            let (runtime, call) = claimed_core_call(direct);
            assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
            assert_eq!(
                observe_current_callable_path(&runtime, call),
                expected,
                "{name}"
            );

            let lazy = LazyValue::from_access(
                context.values(),
                Arc::from([]),
                Arc::from([context.values().unit()]),
            );
            crate::core::cache_test_lazy(
                context.values(),
                &lazy,
                Ok(crate::core::EvaluatedValue::try_from(lazy_result)
                    .expect("callable families are already in WHNF")),
            )
            .expect("fresh callable lazy must accept its cached result");
            let (runtime, call) = claimed_core_call(Value::Lazy(lazy));
            assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
            assert_eq!(
                observe_current_callable_path(&runtime, call),
                expected,
                "cached lazy resolving to {name}"
            );

            let promise = PromisedValue::new(context.values(), format!("assigned {name} callable"));
            crate::core::set_test_promise(context.values(), &promise, promise_result)
                .expect("fresh callable promise must accept its assignment");
            let (runtime, call) = claimed_core_call(Value::Promised(promise));
            assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
            assert_eq!(
                observe_current_callable_path(&runtime, call),
                expected,
                "assigned promise resolving to {name}"
            );
        }

        let promise = PromisedValue::new(context.values(), "blocked disposition");
        let (blocked_runtime, blocked_call) = claimed_core_call(Value::Promised(promise));
        assert!(progress_exact_core_call(&context, &blocked_runtime, blocked_call).unwrap());
        assert_eq!(
            observe_current_callable_path(&blocked_runtime, blocked_call),
            CurrentCallablePath::BlockedDependency
        );

        let lazy_dependency = PromisedValue::new(context.values(), "blocked lazy dependency");
        let lazy = LazyValue::from_access(
            context.values(),
            Arc::from([]),
            Arc::from([Value::Promised(lazy_dependency)]),
        );
        let (blocked_runtime, blocked_call) = claimed_core_call(Value::Lazy(lazy));
        assert!(progress_exact_core_call(&context, &blocked_runtime, blocked_call).unwrap());
        assert_eq!(
            observe_current_callable_path(&blocked_runtime, blocked_call),
            CurrentCallablePath::BlockedDependency
        );

        let (failed_runtime, failed_call) = claimed_core_call(context.values().unit());
        let failure = progress_exact_core_call(&context, &failed_runtime, failed_call)
            .expect_err("unit is permanently non-callable");
        assert!(
            failure
                .to_string()
                .contains("application requires a function value")
        );
        assert!(matches!(
            failed_runtime.test_with(&crate::core::test_value_factory(), |net| net.stuck_reason(failed_call.pair).cloned()),
            Some(StuckReason::Specialization(error)) if error == failure
        ));
        assert_eq!(
            observe_current_callable_path(&failed_runtime, failed_call),
            CurrentCallablePath::Failed
        );
    }

    #[test]
    fn callable_checkpoint_resumes_published_focus_without_replay() {
        let context = test_context();
        let terminal = PromisedValue::new(context.values(), "terminal callable focus");
        crate::core::set_test_promise(context.values(), &terminal, Value::Builtin(Builtin::Add))
            .expect("terminal promise accepts its callable result");
        let mut focus = terminal;
        for label in ["third", "second", "first"] {
            let prior = context
                .values()
                .with_runtime_value_access(|access| Value::Promised(focus.duplicate_in(&access)));
            let next = PromisedValue::new(context.values(), format!("{label} callable focus"));
            crate::core::set_test_promise(context.values(), &next, prior)
                .expect("promise accepts its delegated focus");
            focus = next;
        }
        let (runtime, call) = claimed_core_call(Value::Promised(focus));

        super::with_direct_evaluator(&context, |evaluator| {
            let mut budget = crate::evaluation::EvaluationStepBudget::new(2);
            assert!(progress_exact_core_call_in(evaluator, &runtime, call, &mut budget).unwrap());
        });
        assert_eq!(
            checkpoint_observation(&runtime, context.values(), call.pair)
                .0
                .generation,
            0
        );
        assert_eq!(
            runtime.test_with(context.values(), RuntimeNet::callable_checkpoint_count),
            1
        );

        for (budget, expected_generation) in [(2, Some(1)), (1, None)] {
            let reduction = runtime
                .test_with_optional_mut(context.values(), |net| net.reduce_pair(call.pair))
                .expect("published checkpoint must remain runnable");
            assert!(matches!(
                reduction.kind,
                ReductionKind::CallableCheckpoint { .. }
            ));
            super::with_direct_evaluator(&context, |evaluator| {
                let mut budget = crate::evaluation::EvaluationStepBudget::new(budget);
                progress_callable_checkpoint(evaluator, &runtime, call.pair, &mut budget)
            })
            .unwrap();
            match expected_generation {
                Some(generation) => {
                    assert_eq!(
                        checkpoint_observation(&runtime, context.values(), call.pair)
                            .0
                            .generation,
                        generation,
                        "one quantum with multiple WHNF transitions publishes one successor"
                    );
                }
                None => assert_eq!(
                    runtime.test_with(context.values(), RuntimeNet::callable_checkpoint_count),
                    0
                ),
            }
        }
        assert_eq!(
            observe_current_callable_path(&runtime, call),
            CurrentCallablePath::DirectOperator
        );
    }

    #[test]
    fn callable_dependency_completion_before_exact_block_is_not_lost() {
        let context = test_context();
        let promise = PromisedValue::new(context.values(), "checkpoint block race");
        let call_promise = context
            .values()
            .with_runtime_value_access(|access| promise.duplicate_in(&access));
        let (runtime, call) = claimed_core_call(Value::Promised(call_promise));

        super::with_direct_evaluator(&context, |evaluator| {
            let claim = CoreCallClaim::fresh(evaluator, &runtime, call).unwrap();
            let mut budget = crate::evaluation::EvaluationStepBudget::new(usize::MAX);
            progress_core_call_claim_with_boundary_handoff(evaluator, claim, &mut budget, || {
                crate::core::set_test_promise(
                    context.values(),
                    &promise,
                    Value::Builtin(Builtin::Add),
                )
                .expect("completion barrier assigns the observed promise");
            })
        })
        .unwrap();

        let blocked = runtime
            .test_with(context.values(), |net| {
                net.blocked_callable_checkpoint(call.pair)
            })
            .expect("completion-before-block still publishes an exact blocked checkpoint");
        assert!(matches!(
            context.pump_wait(&blocked.wait.0, 256),
            crate::evaluation::EvaluationPumpOutcome::TargetReady
        ));
        assert!(!matches!(
            context.poll_wait(&blocked.wait.0),
            crate::evaluation::EvaluationWaitPoll::Pending(_)
        ));

        super::with_direct_evaluator(&context, |evaluator| {
            assert!(with_core_net_access(evaluator, &runtime, |runtime| {
                runtime.retry_blocked_callable_checkpoint(&blocked)
            }));
            runtime
                .test_with_optional_mut(context.values(), |net| net.reduce_pair(call.pair))
                .expect("completed dependency makes the checkpoint runnable");
            let mut budget = crate::evaluation::EvaluationStepBudget::new(usize::MAX);
            progress_callable_checkpoint(evaluator, &runtime, call.pair, &mut budget)
        })
        .unwrap();
        assert_eq!(
            observe_current_callable_path(&runtime, call),
            CurrentCallablePath::DirectOperator
        );
    }

    #[test]
    fn unsupported_checkpoint_boundary_terminalizes_the_exact_generation() {
        let context = test_context();
        let (runtime, call) = claimed_core_call(context.values().unit());
        let checkpoint = super::with_direct_evaluator(&context, |evaluator| {
            evaluator.with_value_access(|access| {
                let state = crate::eval::whnf::NetWhnfState::from_regional(
                    &access,
                    crate::eval::whnf::RegionalWhnfWork::from_focus(
                        &access,
                        Value::Builtin(Builtin::Add),
                    ),
                );
                let Ok(checkpoint) = access
                    .net(&runtime)
                    .install_claimed_call_checkpoint(call, state)
                else {
                    panic!("claimed call accepts one checkpoint")
                };
                checkpoint
            })
        });

        let failure = super::with_direct_evaluator(&context, |evaluator| {
            settle_callable_checkpoint_boundary(
                evaluator,
                &runtime,
                checkpoint,
                crate::eval::whnf::RegionalBoundaryRequest::External(
                    crate::eval::whnf::WhnfExternalBoundary::Reflection,
                ),
                || {},
            )
        })
        .expect_err("unsupported external boundary must fail the exact checkpoint");
        assert!(
            failure
                .to_string()
                .contains("unsupported external boundary")
        );
        assert_eq!(
            observe_current_callable_path(&runtime, call),
            CurrentCallablePath::Failed
        );
    }

    #[test]
    fn frame_bearing_callable_checkpoint_survives_every_ownership_handoff() {
        let context = test_context();
        let inner = PromisedValue::new(context.values(), "framed checkpoint dependency");
        let inner_value = context
            .values()
            .with_runtime_value_access(|access| Value::Promised(inner.duplicate_in(&access)));
        let outer = PromisedValue::new(context.values(), "framed checkpoint delegate");
        crate::core::set_test_promise(context.values(), &outer, inner_value)
            .expect("outer promise delegates to the unresolved inner promise");
        let (runtime, call) = claimed_core_call(context.values().unit());

        let expected = super::with_direct_evaluator(&context, |evaluator| {
            evaluator.with_value_access(|access| {
                let state = crate::eval::whnf::NetWhnfState::application_checkpoint_for_test(
                    &access,
                    Value::Promised(outer),
                    vec![Value::Number(1.into())],
                );
                let expected = state.container_identities_for_test();
                let Ok(installed) = access
                    .net(&runtime)
                    .install_claimed_call_checkpoint(call, state)
                else {
                    panic!("claimed call accepts the framed checkpoint")
                };
                assert_eq!(installed.generation, 0);
                expected
            })
        });
        assert_eq!(
            checkpoint_observation(&runtime, context.values(), call.pair),
            (
                crate::interaction_net::CallableCheckpointCall {
                    pair: call.pair,
                    bind: call.bind,
                    checkpoint: call.data,
                    generation: 0,
                },
                expected.clone(),
            )
        );

        runtime
            .test_with_optional_mut(context.values(), |net| net.reduce_pair(call.pair))
            .expect("initial checkpoint is runnable");
        super::with_direct_evaluator(&context, |evaluator| {
            let mut budget = crate::evaluation::EvaluationStepBudget::new(1);
            progress_callable_checkpoint(evaluator, &runtime, call.pair, &mut budget)
        })
        .unwrap();
        let (yielded, yielded_identity) =
            checkpoint_observation(&runtime, context.values(), call.pair);
        assert_eq!(yielded.generation, 1);
        assert_eq!(yielded_identity, expected);

        runtime
            .test_with_optional_mut(context.values(), |net| net.reduce_pair(call.pair))
            .expect("yielded checkpoint is runnable");
        super::with_direct_evaluator(&context, |evaluator| {
            let mut budget = crate::evaluation::EvaluationStepBudget::new(usize::MAX);
            progress_callable_checkpoint(evaluator, &runtime, call.pair, &mut budget)
        })
        .unwrap();
        let blocked = runtime
            .test_with(context.values(), |net| {
                net.blocked_callable_checkpoint(call.pair)
            })
            .expect("unresolved inner promise blocks the framed checkpoint");
        assert_eq!(blocked.call.generation, 2);
        let (_, blocked_identity) = checkpoint_observation(&runtime, context.values(), call.pair);
        assert_eq!(blocked_identity, expected);

        crate::core::set_test_promise(context.values(), &inner, Value::Builtin(Builtin::Add))
            .expect("inner promise accepts the eventual callable");
        assert!(matches!(
            context.pump_wait(&blocked.wait.0, 256),
            crate::evaluation::EvaluationPumpOutcome::TargetReady
        ));
        super::with_direct_evaluator(&context, |evaluator| {
            assert!(with_core_net_access(evaluator, &runtime, |runtime| {
                runtime.retry_blocked_callable_checkpoint(&blocked)
            }));
        });

        runtime
            .test_with_optional_mut(context.values(), |net| net.reduce_pair(call.pair))
            .expect("completed dependency makes the checkpoint runnable");
        super::with_direct_evaluator(&context, |evaluator| {
            evaluator.with_value_access(|access| {
                let claim = CoreCheckpointClaim::take(&access, &runtime, call.pair)
                    .expect("exact checkpoint claim moves state into regional ownership");
                assert_eq!(
                    claim
                        .work
                        .as_ref()
                        .expect("claim retains regional work")
                        .container_identities_for_test(),
                    expected
                );
                drop(claim);
            });
        });
        let (restored, restored_identity) =
            checkpoint_observation(&runtime, context.values(), call.pair);
        assert_eq!(restored.generation, blocked.call.generation);
        assert_eq!(restored_identity, expected);

        runtime
            .test_with_optional_mut(context.values(), |net| net.reduce_pair(call.pair))
            .expect("restored checkpoint is runnable");
        super::with_direct_evaluator(&context, |evaluator| {
            let mut budget = crate::evaluation::EvaluationStepBudget::new(usize::MAX);
            progress_callable_checkpoint(evaluator, &runtime, call.pair, &mut budget)
        })
        .unwrap();
        assert_eq!(
            observe_current_callable_path(&runtime, call),
            CurrentCallablePath::DirectOperator
        );
    }

    #[test]
    fn fresh_operator_claim_release_restores_ready_work() {
        let context = test_context();
        let operator = context.values().with_runtime_value_access(|access| {
            builtin_operator(&access, BuiltinCall::new(Builtin::Add))
        });
        let (runtime, call) = claimed_core_operator_call(operator, context.values().unit());
        let before = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;

        crate::eval::with_direct_evaluator(&context, |evaluator| {
            let claim = CoreOperatorClaim::fresh(evaluator, &runtime, call)
                .expect("claimed operator call must issue its scoped guard");
            assert!(!claim.finish(OperatorDisposition::Release).unwrap());
        });

        let after = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;
        assert_eq!(after.topology_revision(), before.topology_revision() + 1);
        assert_eq!(after.disturbance_epoch(), before.disturbance_epoch() + 1);
        assert!(matches!(
            runtime.test_with_optional_mut(&crate::core::test_value_factory(), |net| net
                .reduce_pair(call.pair)),
            Some(Reduction {
                kind: ReductionKind::OperatorCall { .. },
                ..
            })
        ));
    }

    #[test]
    fn fresh_operator_claim_unwind_restores_ready_work() {
        let context = test_context();
        let operator = context.values().with_runtime_value_access(|access| {
            builtin_operator(&access, BuiltinCall::new(Builtin::Add))
        });
        let (runtime, call) = claimed_core_operator_call(operator, context.values().unit());
        let before = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::eval::with_direct_evaluator(&context, |evaluator| {
                let _claim = CoreOperatorClaim::fresh(evaluator, &runtime, call)
                    .expect("claimed operator call must issue its scoped guard");
                panic!("forced operator-claim unwind");
            });
        }));

        assert!(unwind.is_err());
        let after = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;
        assert_eq!(after.topology_revision(), before.topology_revision() + 1);
        assert_eq!(after.disturbance_epoch(), before.disturbance_epoch() + 1);
        assert!(matches!(
            runtime.test_with_optional_mut(&crate::core::test_value_factory(), |net| net
                .reduce_pair(call.pair)),
            Some(Reduction {
                kind: ReductionKind::OperatorCall { .. },
                ..
            })
        ));
    }

    #[test]
    fn stale_fresh_operator_claim_fails_quietly_before_guard_issuance() {
        let context = test_context();
        let operator = context.values().with_runtime_value_access(|access| {
            builtin_operator(&access, BuiltinCall::new(Builtin::Add))
        });
        let (runtime, call) = claimed_core_operator_call(operator, context.values().unit());
        runtime.test_with_mut(&crate::core::test_value_factory(), |net| {
            assert!(net.release_claimed_operator_call(call))
        });
        let before = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;

        crate::eval::with_direct_evaluator(&context, |evaluator| {
            assert!(CoreOperatorClaim::fresh(evaluator, &runtime, call).is_none());
        });

        assert_eq!(
            runtime
                .test_with_revisions(&crate::core::test_value_factory(), |_| ())
                .1,
            before
        );
    }

    #[test]
    fn retried_operator_claim_release_restores_the_exact_wait() {
        let context = test_context();
        let promise = PromisedValue::new(context.values(), "operator-claim wait");
        let wait = crate::core_net::CoreWaitToken(
            promise_wait(&context, &promise).expect("test promise must allocate a wait"),
        );
        let operator = context.values().with_runtime_value_access(|access| {
            builtin_operator(&access, BuiltinCall::new(Builtin::Add))
        });
        let (runtime, call) = claimed_core_operator_call(operator, context.values().unit());
        crate::eval::with_direct_evaluator(&context, |evaluator| {
            CoreOperatorClaim::fresh(evaluator, &runtime, call)
                .expect("ready operator call must be claimable")
                .finish(OperatorDisposition::Blocked(wait.clone()))
                .unwrap();
        });
        let blocked = runtime
            .test_with(&crate::core::test_value_factory(), |net| {
                net.blocked_operator_call(call.pair)
            })
            .expect("unassigned operator promise must block the call");
        let before = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;

        crate::eval::with_direct_evaluator(&context, |evaluator| {
            let claim = CoreOperatorClaim::retry(evaluator, &runtime, blocked.clone())
                .expect("the exact blocked operator wait must be reclaimable");
            assert!(!claim.finish(OperatorDisposition::Release).unwrap());
        });

        let restored = runtime
            .test_with(&crate::core::test_value_factory(), |net| {
                net.blocked_operator_call(call.pair)
            })
            .expect("release must restore the prior blocked operator call");
        assert_eq!(restored.wait, blocked.wait);
        let after = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;
        assert_eq!(after.topology_revision(), before.topology_revision() + 2);
        assert_eq!(after.disturbance_epoch(), before.disturbance_epoch() + 2);
    }

    #[test]
    fn retried_operator_claim_unwind_restores_the_exact_wait() {
        let context = test_context();
        let promise = PromisedValue::new(context.values(), "unwound operator-claim wait");
        let wait = crate::core_net::CoreWaitToken(
            promise_wait(&context, &promise).expect("test promise must allocate a wait"),
        );
        let operator = context.values().with_runtime_value_access(|access| {
            builtin_operator(&access, BuiltinCall::new(Builtin::Add))
        });
        let (runtime, call) = claimed_core_operator_call(operator, context.values().unit());
        crate::eval::with_direct_evaluator(&context, |evaluator| {
            CoreOperatorClaim::fresh(evaluator, &runtime, call)
                .expect("ready operator call must be claimable")
                .finish(OperatorDisposition::Blocked(wait.clone()))
                .unwrap();
        });
        let blocked = runtime
            .test_with(&crate::core::test_value_factory(), |net| {
                net.blocked_operator_call(call.pair)
            })
            .expect("unassigned operator promise must block the call");

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::eval::with_direct_evaluator(&context, |evaluator| {
                let _claim = CoreOperatorClaim::retry(evaluator, &runtime, blocked.clone())
                    .expect("the exact blocked operator wait must be reclaimable");
                panic!("forced retried-operator unwind");
            });
        }));

        assert!(unwind.is_err());
        let restored = runtime
            .test_with(&crate::core::test_value_factory(), |net| {
                net.blocked_operator_call(call.pair)
            })
            .expect("unwind must restore the prior blocked operator call");
        assert_eq!(restored.wait, blocked.wait);
    }

    #[test]
    fn mismatched_blocked_operator_retry_fails_quietly_before_guard_issuance() {
        let context = test_context();
        let promise = PromisedValue::new(context.values(), "current operator wait");
        let wait = crate::core_net::CoreWaitToken(
            promise_wait(&context, &promise).expect("test promise must allocate a wait"),
        );
        let operator = context.values().with_runtime_value_access(|access| {
            builtin_operator(&access, BuiltinCall::new(Builtin::Add))
        });
        let (runtime, call) = claimed_core_operator_call(operator, context.values().unit());
        crate::eval::with_direct_evaluator(&context, |evaluator| {
            CoreOperatorClaim::fresh(evaluator, &runtime, call)
                .expect("ready operator call must be claimable")
                .finish(OperatorDisposition::Blocked(wait.clone()))
                .unwrap();
        });
        let blocked = runtime
            .test_with(&crate::core::test_value_factory(), |net| {
                net.blocked_operator_call(call.pair)
            })
            .expect("unassigned operator promise must block the call");
        let other = PromisedValue::new(context.values(), "unrelated operator wait");
        let wrong_wait = crate::core_net::CoreWaitToken(
            promise_wait(&context, &other).expect("unrelated wait must allocate"),
        );
        assert_ne!(wrong_wait, blocked.wait);
        let before = runtime
            .test_with_revisions(&crate::core::test_value_factory(), |_| ())
            .1;

        crate::eval::with_direct_evaluator(&context, |evaluator| {
            let mismatch = BlockedOperatorCall {
                pair: blocked.pair,
                wait: wrong_wait,
            };
            assert!(CoreOperatorClaim::retry(evaluator, &runtime, mismatch).is_none());
        });

        assert_eq!(
            runtime
                .test_with_revisions(&crate::core::test_value_factory(), |_| ())
                .1,
            before
        );
        assert_eq!(
            runtime
                .test_with(&crate::core::test_value_factory(), |net| net
                    .blocked_operator_call(call.pair))
                .expect("mismatched retry must preserve the current operator wait")
                .wait,
            blocked.wait
        );
    }

    #[test]
    fn operator_claim_dispositions_defer_application_demand_to_whnf() {
        let context = test_context();

        let function = closed_function_value(1, TestExpr::Local(0));
        let data = Value::Number(Number::from(42));
        let data_operator = context
            .values()
            .with_runtime_value_access(|access| applicable_operator(&access, function));
        let (data_runtime, data_call) = claimed_core_operator_call(data_operator, data);
        assert!(progress_exact_core_operator_call(&context, &data_runtime, data_call).unwrap());
        assert!(
            data_runtime.test_with(&crate::core::test_value_factory(), |net| net
                .operator_call(data_call.pair)
                .is_none())
        );

        let operator = context.values().with_runtime_value_access(|access| {
            builtin_operator(&access, BuiltinCall::new(Builtin::Add))
        });
        let (operator_runtime, operator_call) =
            claimed_core_operator_call(operator, Value::Number(Number::from(19)));
        assert!(
            progress_exact_core_operator_call(&context, &operator_runtime, operator_call).unwrap()
        );
        assert!(
            operator_runtime.test_with(&crate::core::test_value_factory(), |net| net
                .operator_call(operator_call.pair)
                .is_none())
        );

        let promise = PromisedValue::new(context.values(), "blocked operator disposition");
        let blocked_operator = context.values().with_runtime_value_access(|access| {
            applicable_operator(&access, Value::Promised(promise))
        });
        let (blocked_runtime, blocked_call) =
            claimed_core_operator_call(blocked_operator, context.values().unit());
        assert!(
            progress_exact_core_operator_call(&context, &blocked_runtime, blocked_call).unwrap()
        );
        assert!(
            blocked_runtime
                .test_with(&crate::core::test_value_factory(), |net| net
                    .operator_call(blocked_call.pair))
                .is_none()
        );
        let blocked = Value::Lazy(LazyValue::from_net_computation(
            context.values(),
            NetValue::new(blocked_runtime),
        ));
        let blocked = eval_value(&context, &blocked)
            .expect_err("the emitted application must retain its promise wait");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());

        let failed_operator = context.values().with_runtime_value_access(|access| {
            applicable_operator(&access, context.values().unit())
        });
        let (failed_runtime, failed_call) =
            claimed_core_operator_call(failed_operator, context.values().unit());
        assert!(progress_exact_core_operator_call(&context, &failed_runtime, failed_call).unwrap());
        let failed = Value::Lazy(LazyValue::from_net_computation(
            context.values(),
            NetValue::new(failed_runtime),
        ));
        let failure = eval_value(&context, &failed).expect_err("unit is permanently non-callable");
        assert!(
            failure
                .to_string()
                .contains("application requires a function value")
        );
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
        let source = instantiate(source.finish(Port::auxiliary(failed_bind, 1)));
        let (failed_pair, unrelated_pair) =
            source.test_with(&crate::core::test_value_factory(), |net| {
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
            .test_with_optional_mut(&crate::core::test_value_factory(), |net| {
                net.reduce_pair(failed_pair)
            })
            .expect("nested source call should be claimable");
        let ReductionKind::Call { bind, data } = reduction.kind else {
            panic!("nested source failure should originate in a call");
        };
        source.test_with_mut(&crate::core::test_value_factory(), |net| {
            net.fail_claimed_call(
                Call {
                    pair: failed_pair,
                    bind,
                    data,
                },
                EvaluationHalt::new("nested driver failure"),
            );
        });

        let (target, interface) = crate::core_net::CoreRuntimeNet::test_copy_layer(
            &crate::core::test_value_factory(),
            source.duplicate_for_test(&crate::core::test_value_factory()),
        );
        let cursor = match target
            .test_poll_interface_demand(&crate::core::test_value_factory(), interface)
        {
            InterfaceDemand::Cursor(cursor) => cursor,
            demand => panic!("copy root should expose a cursor, got {demand:?}"),
        };
        let CursorStep::Dependency(CursorDependency::SourceFrontier(observation)) =
            target.test_step_cursor(&crate::core::test_value_factory(), cursor)
        else {
            panic!("nested source failure should become an exact frontier dependency");
        };
        assert_eq!(
            observation.endpoint(),
            DemandEndpoint::ActivePair(failed_pair)
        );

        assert!(matches!(
            source.test_with_optional_mut(&crate::core::test_value_factory(), |net| net
                .reduce_pair(unrelated_pair)),
            Some(Reduction {
                kind: ReductionKind::BindJoin,
                ..
            })
        ));
        let failure = normalization_request(&target, interface)
            .drive(&test_context())
            .expect_err("nested terminal failure must propagate through the driver");
        assert!(failure.to_string().contains("nested driver failure"));
    }

    fn assert_iterative_cursor_driver_handles_productive_layers(layers: usize) {
        let expected = crate::core::test_value_factory().unit();
        let mut leaf = NetBuilder::<CoreSpecialization>::new();
        let data = leaf.data(expected.clone());
        let leaf = instantiate(leaf.finish(data));
        let mut source = leaf;
        let mut root_interface =
            source.test_with(&crate::core::test_value_factory(), |net| net.exposed());

        for _ in 0..layers {
            (source, root_interface) = crate::core_net::CoreRuntimeNet::test_copy_layer(
                &crate::core::test_value_factory(),
                source,
            );
        }

        let request = normalization_request(&source, root_interface);
        assert_eq!(
            request.drive(&test_context()).unwrap(),
            NetInterfaceOutcome::Data
        );
        assert_eq!(
            source.test_with(&crate::core::test_value_factory(), |net| net
                .interface_data(root_interface)
                .cloned()),
            Some(expected)
        );
        assert_eq!(
            source.active_normalization_batch(&crate::core::test_value_factory()),
            None
        );
    }

    #[test]
    fn iterative_cursor_driver_handles_productive_layers() {
        assert_iterative_cursor_driver_handles_productive_layers(128);
    }

    #[test]
    #[ignore = "cursor stress fixture; run scripts/check-cursor-stress.sh"]
    fn cursor_stress_iterative_driver_exceeds_the_former_recursion_limit() {
        assert_iterative_cursor_driver_handles_productive_layers(1_100);
    }

    #[test]
    fn cursor_driver_releases_each_runtime_before_crossing_to_the_next() {
        let mut leaf = NetBuilder::<CoreSpecialization>::new();
        let data = leaf.data(crate::core::test_value_factory().unit());
        let mut source = instantiate(leaf.finish(data));
        let mut root_interface =
            source.test_with(&crate::core::test_value_factory(), |net| net.exposed());
        let values = crate::core::test_value_factory();
        let mut runtimes = vec![values.with_runtime_value_access(|access| source.root_in(&access))];
        for _ in 0..4 {
            (source, root_interface) =
                crate::core_net::CoreRuntimeNet::test_copy_layer(&values, source);
            runtimes.push(values.with_runtime_value_access(|access| source.root_in(&access)));
        }

        normalization_request(&source, root_interface)
            .drive(&test_context())
            .unwrap();
        assert!(runtimes.iter().all(|root| {
            values.with_runtime_value_access(|access| {
                CoreRuntimeNet::from_root(root, &access)
                    .access(&access)
                    .active_normalization_batch()
                    .is_none()
            })
        }));
    }

    mod nc5_tests {
        include!("net/tests/nc5.rs");
    }
}
