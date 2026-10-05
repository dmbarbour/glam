//! Cooperative and runtime evaluation pumping.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use crate::core::{EvaluationPanic, EvaluationPanicOrigin};

use super::coordinator::{
    self, CausalChildSelection, ClaimedDeferredWork, ClaimedLazyRoute, ClaimedReflectionWork,
    ClaimedTaskWork, ClientDemandOperation, CoordinatorWaiterClass, CoordinatorWaiterOutcome,
    DeferredLazyCycleMember, DeferredWorkPoll, EvaluationMachinePoll, EvaluationSessionId,
    EvaluationTaskId, EvaluationTaskMachine, EvaluationWaitPoll, EvaluationWaitTerminal,
    EvaluationWaitToken, EvaluationWorkCoordinator, EvaluationWorkId, ExactDemandRoute,
    ExactRouteRelease, ExactTargetSelection, ReflectionWorkPoll, ReflectionWorkState,
    WorkDependency,
};
use super::session::{
    EvalContext, EvaluationSessionReport, EvaluationSessionRun, EvaluationUnfinishedState,
    EvaluationUnfinishedTask,
};
use super::{EvaluationDemandState, EvaluationPollContext, evaluation_failure};
use crate::core::{EvaluationFailure, LazyCycle, LazyCycleMember};
use crate::runtime::RuntimeFailureRoot;

impl ClientDemandOperation {
    pub(super) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvalContext,
        step_budget: &mut super::EvaluationStepBudget,
    ) -> coordinator::ClientDemandPoll {
        match super::whnf::poll_computation(
            &mut self.computation,
            poll_context,
            context,
            step_budget,
        ) {
            super::whnf::WhnfOwnerPoll::Ready(value) => {
                coordinator::ClientDemandPoll::Complete(value)
            }
            super::whnf::WhnfOwnerPoll::Pending(dependency) => {
                coordinator::ClientDemandPoll::Blocked(dependency)
            }
            super::whnf::WhnfOwnerPoll::Yielded => coordinator::ClientDemandPoll::Yielded,
            super::whnf::WhnfOwnerPoll::Failed(failure) => {
                coordinator::ClientDemandPoll::Failed(failure)
            }
            super::whnf::WhnfOwnerPoll::External(boundary) => {
                unreachable!("client demand produced an external {boundary:?} boundary")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationPumpOutcome {
    TargetReady,
    /// The target has a producer currently claimed by another thread.
    Busy,
    NoProgress,
    BudgetExhausted,
}

#[cfg(test)]
pub(super) fn test_reflection_dependency(
    coordinator: &EvaluationWorkCoordinator,
    wait: &EvaluationWaitToken,
) -> EvaluationWaitToken {
    let mut wait = wait.clone();
    let mut seen = HashSet::new();
    while seen.insert(wait.get()) {
        let Some(producer) = coordinator.work_for_wait(&wait) else {
            break;
        };
        let Some(dependency) = coordinator.work_dependency_by_id(producer) else {
            break;
        };
        let Some(dependency_wait) = dependency.producer_wait() else {
            break;
        };
        wait = dependency_wait.clone();
    }
    wait
}

const TASK_POLL_QUANTUM: usize = 64;

enum ReleasedTaskMachine {
    DropOnly(Box<dyn EvaluationTaskMachine>),
    Drop {
        machine: Box<dyn EvaluationTaskMachine>,
        retirement: WorkRetirement,
    },
    Cancel {
        machine: Box<dyn EvaluationTaskMachine>,
        retirement: WorkRetirement,
    },
    /// A machine whose own poll panicked. Its state may be torn, so it is
    /// dropped without cancellation, under an unwind boundary.
    DropTorn {
        machine: Box<dyn EvaluationTaskMachine>,
        retirement: WorkRetirement,
    },
}

enum WorkRetirement {
    Reflection(Arc<EvaluationWorkCoordinator>, EvaluationWorkId),
    Deferred(Arc<EvaluationWorkCoordinator>, EvaluationWorkId),
}

impl ReleasedTaskMachine {
    fn finish(self) {
        let retirement = match self {
            Self::DropOnly(machine) => {
                drop(machine);
                return;
            }
            Self::Drop {
                machine,
                retirement,
            } => {
                drop(machine);
                retirement
            }
            Self::Cancel {
                mut machine,
                retirement,
            } => {
                machine.cancel();
                retirement
            }
            Self::DropTorn {
                machine,
                retirement,
            } => {
                drop_torn(machine);
                retirement
            }
        };
        match retirement {
            WorkRetirement::Reflection(coordinator, work) => {
                drop(coordinator.retire_reflection(work));
            }
            WorkRetirement::Deferred(coordinator, work) => {
                coordinator.retire_deferred(work);
            }
        }
    }
}

struct ReportedDependency {
    task: Option<EvaluationTaskId>,
    session: Option<EvaluationSessionId>,
    wait: u64,
    live_cross_session: bool,
}

struct ClaimedTask {
    coordinator: Arc<EvaluationWorkCoordinator>,
    kind: ClaimedTaskKind,
}

enum ClaimedTaskKind {
    Reflection(ClaimedReflectionWork),
    Deferred(ClaimedDeferredWork),
    LazyRoute(ClaimedLazyRoute),
}

impl ClaimedTask {
    fn new(coordinator: Arc<EvaluationWorkCoordinator>, work: ClaimedTaskWork) -> Self {
        work.demand().assert_runtime(coordinator.runtime_id());
        let kind = match work {
            ClaimedTaskWork::Reflection(claim) => ClaimedTaskKind::Reflection(claim),
            ClaimedTaskWork::Deferred(claim) => ClaimedTaskKind::Deferred(claim),
            ClaimedTaskWork::LazyRoute(claim) => ClaimedTaskKind::LazyRoute(claim),
        };
        Self { coordinator, kind }
    }

    /// Polls the claimed machine behind the scheduler's panic boundary.
    ///
    /// A panic during the poll ends this work as `Panicked` instead of
    /// stranding its claim. Work blocked on a panicked dependency also ends
    /// as `Panicked` without polling, because polling would reinstall the
    /// panicked work.
    fn poll(&mut self, step_budget: &mut super::EvaluationStepBudget) -> EvaluationMachinePoll {
        if let Some(report) = self.kind.prior_dependency_panic() {
            return EvaluationMachinePoll::Panicked {
                report,
                torn: false,
            };
        }
        let context = EvaluationPollContext::for_claim(self.kind.demand());
        let kind = &mut self.kind;
        let polled = catch_unwind(AssertUnwindSafe(|| match kind {
            ClaimedTaskKind::Reflection(task) => task.poll(&context, step_budget),
            ClaimedTaskKind::Deferred(task) => task.poll(&context, step_budget),
            ClaimedTaskKind::LazyRoute(route) => route.poll(&context, step_budget),
        }));
        match polled {
            Ok(poll) => halt_blocked_on_panic(poll),
            Err(payload) => EvaluationMachinePoll::Panicked {
                report: EvaluationPanic::from_payload(payload.as_ref(), self.kind.panic_origin()),
                torn: true,
            },
        }
    }

    fn release(
        self,
        poll: EvaluationMachinePoll,
    ) -> (
        bool,
        bool,
        Option<ReleasedTaskMachine>,
        Option<ExactRouteRelease>,
    ) {
        let (poll, spark) = match poll {
            EvaluationMachinePoll::ScheduleSpark(value) => {
                (EvaluationMachinePoll::Yielded, Some(value))
            }
            poll => (poll, None),
        };
        let demand = spark.as_ref().map(|_| self.kind.demand().demand());
        let released = match self.kind {
            ClaimedTaskKind::Reflection(task) => {
                release_reflection_task(&self.coordinator, task, poll)
            }
            ClaimedTaskKind::Deferred(task) => release_deferred_task(&self.coordinator, task, poll),
            ClaimedTaskKind::LazyRoute(route) => release_lazy_route(&self.coordinator, route, poll),
        };
        if let Some(value) = spark {
            self.coordinator.submit_spark_root(
                demand.expect("a spark request must retain its demand session"),
                value,
            );
        }
        released
    }
}

impl ClaimedTaskKind {
    fn demand(&self) -> &coordinator::ClaimedDemandSession {
        match self {
            Self::Reflection(task) => task.demand(),
            Self::Deferred(task) => task.demand(),
            Self::LazyRoute(route) => route.demand(),
        }
    }

    fn prior_dependency_panic(&self) -> Option<EvaluationPanic> {
        match self {
            Self::Reflection(task) => task.prior_dependency_panic(),
            Self::Deferred(task) => task.prior_dependency_panic(),
            Self::LazyRoute(route) => route.prior_dependency_panic(),
        }
    }

    fn panic_origin(&self) -> EvaluationPanicOrigin {
        match self {
            Self::Reflection(task) => EvaluationPanicOrigin::ReflectionTask(task.id().get()),
            Self::Deferred(task) => EvaluationPanicOrigin::DeferredWork(task.id().get()),
            Self::LazyRoute(route) => EvaluationPanicOrigin::LazyRoute(route.id().get()),
        }
    }
}

/// Ends work that blocked on a panicked dependency: a waiter on a panicked
/// producer halts instead of reinstalling the work that panicked.
fn halt_blocked_on_panic(poll: EvaluationMachinePoll) -> EvaluationMachinePoll {
    let EvaluationMachinePoll::Blocked(block) = &poll else {
        return poll;
    };
    match block
        .dependency
        .as_ref()
        .and_then(coordinator::WorkDependency::panic_report)
    {
        Some(report) => EvaluationMachinePoll::Panicked {
            report,
            torn: false,
        },
        None => poll,
    }
}

/// Drops state left by a panicked poll. Its destructor may panic again; that
/// fault was already reported, so the second panic is contained here.
fn drop_torn<T>(value: T) {
    let _ = catch_unwind(AssertUnwindSafe(move || drop(value)));
}

fn release_lazy_route(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    claimed: ClaimedLazyRoute,
    poll: EvaluationMachinePoll,
) -> (
    bool,
    bool,
    Option<ReleasedTaskMachine>,
    Option<ExactRouteRelease>,
) {
    let work = claimed.id();
    let lazy = claimed.lazy().clone();
    let (work_poll, terminal) = match poll {
        EvaluationMachinePoll::Yielded => (DeferredWorkPoll::Yielded, None),
        EvaluationMachinePoll::ScheduleSpark(_) => {
            unreachable!("claimed-task release must externalize spark requests")
        }
        EvaluationMachinePoll::Blocked(block) => (DeferredWorkPoll::Blocked(block), None),
        EvaluationMachinePoll::Exit(_) => unreachable!("lazy route cannot vote to exit"),
        EvaluationMachinePoll::Complete(value) => (
            DeferredWorkPoll::Terminal,
            Some(EvaluationWaitTerminal::Complete(value)),
        ),
        EvaluationMachinePoll::Failed(error) => (
            DeferredWorkPoll::Terminal,
            Some(EvaluationWaitTerminal::Failed(error)),
        ),
        EvaluationMachinePoll::Cancelled => unreachable!("lazy route cannot be canceled as a task"),
        EvaluationMachinePoll::Panicked { report, torn } => (
            DeferredWorkPoll::Terminal,
            Some(if torn {
                record_lazy_panic(coordinator, &lazy, report)
            } else {
                EvaluationWaitTerminal::Panicked(report)
            }),
        ),
    };
    let mut release = coordinator.release_lazy_route(claimed, work_poll);
    let route = release.route.take();
    if !release.cycle.is_empty() {
        poison_lazy_cycle(
            coordinator,
            std::mem::take(&mut release.cycle),
            release.cycle_error.take(),
        );
        return (release.made_progress, false, None, route);
    }
    if !release.terminal {
        return (release.made_progress, release.remains_blocked, None, route);
    }
    let terminal = terminal.expect("terminal lazy route poll must carry a terminal result");
    let failure = match &terminal {
        EvaluationWaitTerminal::Complete(_) => {
            evaluation_failure("lazy route completed without fulfilling its fixpoint")
        }
        EvaluationWaitTerminal::Failed(error) => error.as_failure().clone(),
        EvaluationWaitTerminal::Panicked(report) => {
            coordinator.settle_panicked_work(work, report.clone());
            coordinator.retire_lazy_route(work);
            return (release.made_progress, false, None, route);
        }
        EvaluationWaitTerminal::Cancelled
        | EvaluationWaitTerminal::Abandoned
        | EvaluationWaitTerminal::Exited
        | EvaluationWaitTerminal::Killed(_) => {
            unreachable!("lazy route terminal is complete, failed, or panicked")
        }
    };
    coordinator.settle_terminal_work(work, terminal, failure);
    coordinator.retire_lazy_route(work);
    (release.made_progress, false, None, route)
}

impl EvaluationDemandState {
    pub(super) fn run_until_quiescent(&self) -> EvaluationSessionRun {
        if self.is_closed() {
            return self.closed_run_report();
        }
        let Some(coordinator) = self.coordinator() else {
            return self.closed_run_report();
        };
        loop {
            let mut claimed = loop {
                if self.is_closed() {
                    return self.closed_run_report();
                }
                if let Some(claimed) = self.claim_ready_task(&coordinator) {
                    break claimed;
                }
                let generation = coordinator.work_generation();
                if self.task_is_running(&coordinator) {
                    if coordinator.wait_for_change_for(
                        generation,
                        None,
                        CoordinatorWaiterClass::SessionDrain,
                    ) {
                        coordinator.record_waiter_outcome(
                            CoordinatorWaiterClass::SessionDrain,
                            if coordinator.session_has_ready_task(self.id) {
                                CoordinatorWaiterOutcome::Productive
                            } else if !self.task_is_running(&coordinator) {
                                CoordinatorWaiterOutcome::Relevant
                            } else {
                                CoordinatorWaiterOutcome::Unrelated
                            },
                        );
                    }
                    continue;
                }
                if coordinator.work_generation() != generation
                    && coordinator.session_has_ready_task(self.id)
                {
                    continue;
                }
                return self.session_run_report(&coordinator);
            };

            let mut budget = super::EvaluationStepBudget::new(TASK_POLL_QUANTUM);
            let poll = claimed.poll(&mut budget);
            let (_, _, released, _) = claimed.release(poll);
            if let Some(machine) = released {
                machine.finish();
            }
        }
    }

    fn session_run_report(&self, coordinator: &EvaluationWorkCoordinator) -> EvaluationSessionRun {
        if self.is_closed() {
            return self.closed_run_report();
        }
        let snapshots = coordinator.reflection_snapshots(self.id);
        let failures = coordinator.failure_snapshot(self.id);
        let mut unfinished = Vec::new();
        let mut has_live_cross_session_dependency = false;
        for snapshot in snapshots {
            let (state, block, exit) = match &snapshot.state {
                ReflectionWorkState::Dormant => (EvaluationUnfinishedState::Dormant, None, None),
                ReflectionWorkState::Reserved => (EvaluationUnfinishedState::Reserved, None, None),
                ReflectionWorkState::Queued => (EvaluationUnfinishedState::Queued, None, None),
                ReflectionWorkState::Running => (EvaluationUnfinishedState::Running, None, None),
                ReflectionWorkState::Blocked(block) => {
                    (EvaluationUnfinishedState::Blocked, Some(block), None)
                }
                ReflectionWorkState::ExitWaiting(exit) => {
                    (EvaluationUnfinishedState::Blocked, None, Some(exit))
                }
                ReflectionWorkState::Terminalizing => {
                    (EvaluationUnfinishedState::Running, None, None)
                }
            };
            let dependency = block
                .and_then(|block| block.dependency.as_ref())
                .and_then(|dependency| self.reported_dependency(coordinator, dependency));
            has_live_cross_session_dependency |= dependency
                .as_ref()
                .is_some_and(|dependency| dependency.live_cross_session);
            unfinished.push(EvaluationUnfinishedTask {
                task: snapshot.task,
                state,
                dependency: dependency.as_ref().and_then(|dependency| dependency.task),
                dependency_session: dependency
                    .as_ref()
                    .and_then(|dependency| dependency.session),
                wait: dependency.as_ref().map(|dependency| dependency.wait),
                observed_epoch: block
                    .and_then(|block| block.observed_epoch)
                    .or_else(|| exit.and_then(|exit| exit.observed_epoch)),
                error: block.and_then(|block| block.error.clone()),
            });
        }
        let report = EvaluationSessionReport {
            failures,
            unfinished,
        };
        if self.is_closed() {
            return self.closed_run_report();
        }
        if report.unfinished.is_empty() {
            EvaluationSessionRun::Complete(report)
        } else if has_live_cross_session_dependency {
            EvaluationSessionRun::Quiescent(report)
        } else {
            EvaluationSessionRun::Deadlocked(report)
        }
    }

    fn reported_dependency(
        &self,
        coordinator: &EvaluationWorkCoordinator,
        initial: &WorkDependency,
    ) -> Option<ReportedDependency> {
        let mut wait = initial.producer_wait()?.clone();
        let mut seen = HashSet::new();
        loop {
            coordinator.work_for_wait(&wait)?;
            let Some((task, session)) = coordinator.task_origin_for_wait(&wait) else {
                return Some(ReportedDependency {
                    task: None,
                    session: None,
                    wait: wait.get(),
                    live_cross_session: true,
                });
            };
            if !seen.insert(wait.get()) || session != self.id {
                return Some(ReportedDependency {
                    task: Some(task),
                    session: Some(session),
                    wait: wait.get(),
                    live_cross_session: session != self.id
                        && coordinator.demand_session_is_open(session),
                });
            }
            let Some(next) = coordinator.task_dependency(task) else {
                return Some(ReportedDependency {
                    task: Some(task),
                    session: Some(session),
                    wait: wait.get(),
                    live_cross_session: false,
                });
            };
            let Some(next_wait) = next.producer_wait() else {
                return Some(ReportedDependency {
                    task: Some(task),
                    session: Some(session),
                    wait: wait.get(),
                    live_cross_session: false,
                });
            };
            wait = next_wait.clone();
        }
    }
}

#[cfg(test)]
pub(super) fn pump_demand(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    context: &EvalContext,
    target: &EvaluationWaitToken,
    reservation_allowance: usize,
) -> EvaluationPumpOutcome {
    let mut reservation_allowance = reservation_allowance;
    pump_demand_on_route(
        coordinator,
        context,
        target,
        &mut reservation_allowance,
        &mut ExactDemandRoute::default(),
    )
}

/// Pumps work toward `target`, drawing whole poll quanta from
/// `reservation_allowance`. On return the allowance holds what remains, so
/// the caller can charge the reserved steps to its own budget.
pub(super) fn pump_demand_on_route(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    context: &EvalContext,
    target: &EvaluationWaitToken,
    reservation_allowance: &mut usize,
    route: &mut ExactDemandRoute,
) -> EvaluationPumpOutcome {
    if target.terminal_poll().is_some() {
        return EvaluationPumpOutcome::TargetReady;
    }
    if target.runtime_id() != context.values().runtime_id() {
        return EvaluationPumpOutcome::NoProgress;
    }
    let mut yielded_causal = None;
    loop {
        if !matches!(context.poll_wait(target), EvaluationWaitPoll::Pending(_)) {
            return EvaluationPumpOutcome::TargetReady;
        }
        if *reservation_allowance == 0 {
            return EvaluationPumpOutcome::BudgetExhausted;
        }

        let selection_generation = coordinator.work_generation();
        let exact = coordinator.claim_exact_target_on_route(target, route);
        let (claimed, exact_claim, causal_busy) = match exact {
            ExactTargetSelection::Claimed(exact) => (Some(exact), true, false),
            ExactTargetSelection::Busy => return EvaluationPumpOutcome::Busy,
            ExactTargetSelection::None => {
                let causal = yielded_causal
                    .take()
                    .and_then(|work| coordinator.claim_work(work))
                    .map(CausalChildSelection::Claimed)
                    .unwrap_or_else(|| {
                        coordinator.claim_causal_child_work(Some(target), context.causal_task_ids())
                    });
                match causal {
                    CausalChildSelection::Claimed(child) => (Some(child), false, false),
                    CausalChildSelection::Busy => (None, false, true),
                    CausalChildSelection::None => (None, false, false),
                }
            }
        };
        let Some(work) = claimed else {
            if causal_busy {
                return EvaluationPumpOutcome::Busy;
            }
            if !matches!(context.poll_wait(target), EvaluationWaitPoll::Pending(_)) {
                return EvaluationPumpOutcome::TargetReady;
            }
            if coordinator.work_generation() != selection_generation {
                continue;
            }
            return EvaluationPumpOutcome::NoProgress;
        };

        let work_id = work.id();
        let mut claimed = ClaimedTask::new(coordinator.clone(), work);
        let quantum = (*reservation_allowance).min(TASK_POLL_QUANTUM);
        *reservation_allowance -= quantum;
        let mut budget = super::EvaluationStepBudget::new(quantum);
        let poll = claimed.poll(&mut budget);
        debug_assert_eq!(budget.spent() + budget.remaining(), budget.granted());
        let yielded = matches!(
            poll,
            EvaluationMachinePoll::Yielded | EvaluationMachinePoll::ScheduleSpark(_)
        );
        let (_, _, released, route_release) = claimed.release(poll);
        if exact_claim {
            if let Some(release) = route_release {
                let _handed_off = coordinator.reconcile_exact_route_release(target, route, release);
                #[cfg(any(test, feature = "interaction-net-profiling"))]
                if _handed_off {
                    coordinator.record_exact_route_handoffs(1);
                }
            } else {
                route.invalidate_missing_release();
            }
        }
        if let Some(machine) = released {
            machine.finish();
        }
        if yielded && !exact_claim {
            // Causal launch provenance is not an exact zipper edge. Preserve
            // its immediate continuation within this bounded call only; a
            // later call must rediscover it through the causal-child probe.
            yielded_causal = Some(work_id);
        }
    }
}

fn release_reflection_task(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    claimed: ClaimedReflectionWork,
    poll: EvaluationMachinePoll,
) -> (
    bool,
    bool,
    Option<ReleasedTaskMachine>,
    Option<ExactRouteRelease>,
) {
    let work = claimed.id();
    let mut torn = false;
    let (work_poll, terminal) = match poll {
        EvaluationMachinePoll::Yielded => (ReflectionWorkPoll::Yielded, None),
        EvaluationMachinePoll::ScheduleSpark(_) => {
            unreachable!("claimed-task release must externalize spark requests")
        }
        EvaluationMachinePoll::Blocked(block) => (ReflectionWorkPoll::Blocked(block), None),
        EvaluationMachinePoll::Exit(exit) => (ReflectionWorkPoll::Exit(exit), None),
        EvaluationMachinePoll::Complete(value) => (
            ReflectionWorkPoll::Terminal,
            Some(EvaluationWaitTerminal::Complete(value)),
        ),
        EvaluationMachinePoll::Failed(error) => (
            ReflectionWorkPoll::Terminal,
            Some(EvaluationWaitTerminal::Failed(error)),
        ),
        EvaluationMachinePoll::Cancelled => (
            ReflectionWorkPoll::Terminal,
            Some(EvaluationWaitTerminal::Cancelled),
        ),
        EvaluationMachinePoll::Panicked {
            report,
            torn: own_panic,
        } => {
            torn = own_panic;
            (
                ReflectionWorkPoll::Terminal,
                Some(EvaluationWaitTerminal::Panicked(report)),
            )
        }
    };

    let mut release = coordinator.release_reflection(claimed, work_poll);
    let route = release.route.take();
    if !release.cycle.is_empty() {
        poison_lazy_cycle(
            coordinator,
            std::mem::take(&mut release.cycle),
            release.cycle_error.take(),
        );
    }
    if !release.terminal {
        if !release.exit_waiting {
            debug_assert!(release.machine.is_none());
            return (release.made_progress, release.remains_blocked, None, route);
        }
        let released = release.machine.take().map(ReleasedTaskMachine::DropOnly);
        return (
            release.made_progress,
            release.remains_blocked,
            released,
            route,
        );
    }

    // A machine's own panic outranks cancellation and session closure. A
    // panic propagated from a dependency does not: that machine is intact.
    let terminal = if torn {
        terminal.expect("a panicked reflection poll carries its report")
    } else if release.cancel {
        EvaluationWaitTerminal::Cancelled
    } else if release.abandoned {
        EvaluationWaitTerminal::Abandoned
    } else {
        terminal.expect("terminal reflection poll must carry a terminal result")
    };
    let promise_failure = match &terminal {
        EvaluationWaitTerminal::Complete(_) => Some(evaluation_failure(
            "reflection task completed without fulfilling its fixpoint",
        )),
        EvaluationWaitTerminal::Failed(error) => Some(error.as_failure().clone()),
        EvaluationWaitTerminal::Cancelled => Some(evaluation_failure(
            "reflection fixpoint producer was cancelled",
        )),
        EvaluationWaitTerminal::Abandoned => Some(evaluation_failure(
            "reflection fixpoint producer was abandoned",
        )),
        EvaluationWaitTerminal::Exited => Some(evaluation_failure(
            "reflection fixpoint producer exited without a result",
        )),
        EvaluationWaitTerminal::Killed(error) => Some(error.as_failure().clone()),
        EvaluationWaitTerminal::Panicked(_) => None,
    };
    match (terminal, promise_failure) {
        (EvaluationWaitTerminal::Panicked(report), _) => {
            coordinator.settle_panicked_work(work, report);
        }
        (terminal, Some(promise_failure)) => {
            coordinator.settle_terminal_work(work, terminal, promise_failure);
        }
        (_, None) => unreachable!("only a panic omits the promise failure"),
    }
    let machine = release
        .machine
        .take()
        .expect("terminal reflection release must retain its detached machine");
    let retirement = WorkRetirement::Reflection(coordinator.clone(), work);
    let released = Some(if torn {
        ReleasedTaskMachine::DropTorn {
            machine,
            retirement,
        }
    } else if release.cancel {
        ReleasedTaskMachine::Cancel {
            machine,
            retirement,
        }
    } else {
        ReleasedTaskMachine::Drop {
            machine,
            retirement,
        }
    });
    (release.made_progress, false, released, route)
}

fn release_deferred_task(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    claimed: ClaimedDeferredWork,
    poll: EvaluationMachinePoll,
) -> (
    bool,
    bool,
    Option<ReleasedTaskMachine>,
    Option<ExactRouteRelease>,
) {
    let work = claimed.id();
    let mut torn = false;
    let (work_poll, terminal) = match poll {
        EvaluationMachinePoll::Yielded => (DeferredWorkPoll::Yielded, None),
        EvaluationMachinePoll::ScheduleSpark(_) => {
            unreachable!("claimed-task release must externalize spark requests")
        }
        EvaluationMachinePoll::Blocked(block) => (DeferredWorkPoll::Blocked(block), None),
        EvaluationMachinePoll::Exit(exit) => {
            drop(exit);
            unreachable!("deferred work cannot publish a runtime exit vote")
        }
        EvaluationMachinePoll::Complete(value) => (
            DeferredWorkPoll::Terminal,
            Some(EvaluationWaitTerminal::Complete(value)),
        ),
        EvaluationMachinePoll::Failed(error) => (
            DeferredWorkPoll::Terminal,
            Some(EvaluationWaitTerminal::Failed(error)),
        ),
        EvaluationMachinePoll::Cancelled => (
            DeferredWorkPoll::Terminal,
            Some(EvaluationWaitTerminal::Failed(
                RuntimeFailureRoot::from_observer(
                    &coordinator.value_observer(),
                    Arc::new(EvaluationFailure::message(
                        "deferred evaluation task was cancelled",
                    )),
                ),
            )),
        ),
        EvaluationMachinePoll::Panicked {
            report,
            torn: own_panic,
        } => {
            torn = own_panic;
            (
                DeferredWorkPoll::Terminal,
                Some(EvaluationWaitTerminal::Panicked(report)),
            )
        }
    };

    let mut release = coordinator.release_deferred(claimed, work_poll);
    let route = release.route.take();
    if !release.cycle.is_empty() {
        poison_lazy_cycle(
            coordinator,
            std::mem::take(&mut release.cycle),
            release.cycle_error.take(),
        );
        return (release.made_progress, false, None, route);
    }
    if !release.terminal {
        debug_assert!(release.machine.is_none());
        return (release.made_progress, release.remains_blocked, None, route);
    }

    // A machine's own panic outranks session closure. A panic propagated
    // from a dependency does not: that machine is intact.
    let terminal = if torn {
        terminal.expect("a panicked deferred poll carries its report")
    } else if release.abandoned {
        EvaluationWaitTerminal::Abandoned
    } else {
        terminal.expect("terminal deferred poll must carry a terminal result")
    };
    let promise_failure = match &terminal {
        EvaluationWaitTerminal::Complete(_) => Some(evaluation_failure(
            "evaluation task completed without fulfilling its fixpoint",
        )),
        EvaluationWaitTerminal::Failed(error) => Some(error.as_failure().clone()),
        EvaluationWaitTerminal::Cancelled => Some(evaluation_failure(
            "evaluation fixpoint producer was cancelled",
        )),
        EvaluationWaitTerminal::Abandoned => Some(evaluation_failure(
            "evaluation fixpoint producer was abandoned",
        )),
        EvaluationWaitTerminal::Exited => Some(evaluation_failure(
            "evaluation fixpoint producer exited without a result",
        )),
        EvaluationWaitTerminal::Killed(error) => Some(error.as_failure().clone()),
        EvaluationWaitTerminal::Panicked(_) => None,
    };
    match (terminal, promise_failure) {
        (EvaluationWaitTerminal::Panicked(report), _) => {
            coordinator.settle_panicked_work(work, report);
        }
        (terminal, Some(promise_failure)) => {
            coordinator.settle_terminal_work(work, terminal, promise_failure);
        }
        (_, None) => unreachable!("only a panic omits the promise failure"),
    }
    let machine = release
        .machine
        .take()
        .expect("terminal deferred release must detach its machine");
    let retirement = WorkRetirement::Deferred(coordinator.clone(), work);
    (
        release.made_progress,
        false,
        Some(if torn {
            ReleasedTaskMachine::DropTorn {
                machine,
                retirement,
            }
        } else {
            ReleasedTaskMachine::Drop {
                machine,
                retirement,
            }
        }),
        route,
    )
}

/// Records a panic in a lazy's own evaluation and returns its route terminal.
///
/// The lazy enters the `Panicked` evaluation state, so later observers halt
/// with this report and the work is never replayed. A lazy that cached its
/// result before the panic keeps it, and the route settles with that result.
fn record_lazy_panic(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    lazy: &crate::core::ManagedLazyRoot,
    report: EvaluationPanic,
) -> EvaluationWaitTerminal {
    let values = coordinator
        .value_observer()
        .upgrade()
        .expect("a claimed lazy route must retain a live value domain");
    values.with_runtime_value_access(|access| {
        if lazy.enter_panicked(&access, &report) {
            return EvaluationWaitTerminal::Panicked(report);
        }
        match lazy.access(&access).and_then(|lazy| lazy.cached()) {
            Some(Ok(value)) => EvaluationWaitTerminal::Complete(
                access.root_runtime_value(value.into_value_in(&access)),
            ),
            Some(Err(failure)) => EvaluationWaitTerminal::Failed(
                crate::runtime::RuntimeFailureRoot::new(&values, failure),
            ),
            None => EvaluationWaitTerminal::Panicked(report),
        }
    })
}

fn poison_lazy_cycle(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    mut members: Vec<DeferredLazyCycleMember>,
    cycle_error: Option<String>,
) {
    let cycle = Arc::new(LazyCycle {
        members: members
            .iter()
            .map(|member| LazyCycleMember {
                id: member.lazy.id(),
                label: member.lazy.label().clone(),
            })
            .collect(),
    });
    let failure = cycle_error
        .map(evaluation_failure)
        .unwrap_or_else(|| Arc::new(EvaluationFailure::dependency_cycle(cycle)));
    let values = coordinator
        .value_observer()
        .upgrade()
        .expect("lazy-cycle publication requires its live value domain");
    // Make the shared failure authoritative in every lazy before any
    // producer wait wakes. The already-batched `Terminalizing` transition
    // prevents another worker from reclaiming a cycle member meanwhile.
    let mut terminals = values.with_runtime_value_access(|access| {
        members
            .iter()
            .map(|member| {
                let terminal = match member.lazy.cache(&access, Err(failure.clone())) {
                    Err(error) => EvaluationWaitTerminal::Failed(
                        RuntimeFailureRoot::from_observer(member.wait.value_observer(), error),
                    ),
                    Ok(value) => {
                        debug_assert!(
                            false,
                            "a successful concurrent lazy result contradicts a strict dependency cycle"
                        );
                        EvaluationWaitTerminal::Complete(
                            access.root_runtime_value(value.into_value_in(&access)),
                        )
                    }
                };
                (member, terminal)
            })
            .collect::<Vec<_>>()
    });
    for (member, terminal) in &mut terminals {
        *terminal =
            coordinator.settle_terminal_work(member.work, terminal.clone(), failure.clone());
    }
    for (member, terminal) in &terminals {
        debug_assert_eq!(member.wait.terminal_poll(), Some(terminal.to_poll()));
        if member.route {
            coordinator.retire_lazy_route(member.work);
        } else {
            coordinator.retire_deferred(member.work);
        }
    }
    drop(terminals);
    let blocks = members
        .iter_mut()
        .filter_map(|member| member.retired_block.take())
        .collect::<Vec<_>>();
    let machines = members
        .into_iter()
        .filter_map(|member| member.machine)
        .collect::<Vec<_>>();
    drop(blocks);
    drop(machines);
}

impl EvaluationWorkCoordinator {
    #[cfg(test)]
    pub(crate) fn poll_runtime_work(self: &Arc<Self>) -> bool {
        self.poll_runtime_work_bounded(TASK_POLL_QUANTUM).is_some()
    }

    /// Polls at most one runtime-visible background machine with a bounded
    /// allowance. A zero allowance performs no selection, so merely probing
    /// the bounded public pump cannot perturb queue order.
    pub(crate) fn poll_runtime_work_bounded(self: &Arc<Self>, step_budget: usize) -> Option<usize> {
        if step_budget == 0 {
            return None;
        }
        match self.select_runtime_pump() {
            coordinator::CoordinatorSelection::Task(work) => {
                let (_, spent) = self.poll_claimed_task_bounded(work, step_budget);
                Some(spent)
            }
            coordinator::CoordinatorSelection::Spark(_) => {
                unreachable!("the runtime pump must not claim best-effort spark work")
            }
            coordinator::CoordinatorSelection::None => None,
        }
    }

    pub(super) fn poll_claimed_task(self: &Arc<Self>, work: ClaimedTaskWork) -> bool {
        self.poll_claimed_task_bounded(work, TASK_POLL_QUANTUM).0
    }

    fn poll_claimed_task_bounded(
        self: &Arc<Self>,
        work: ClaimedTaskWork,
        step_budget: usize,
    ) -> (bool, usize) {
        debug_assert_ne!(step_budget, 0);
        let mut claimed = ClaimedTask::new(self.clone(), work);
        let mut budget = super::EvaluationStepBudget::new(step_budget.min(TASK_POLL_QUANTUM));
        let before = budget.remaining();
        let poll = claimed.poll(&mut budget);
        budget.charge_if_unchanged(before);
        let yielded = matches!(
            poll,
            EvaluationMachinePoll::Yielded | EvaluationMachinePoll::ScheduleSpark(_)
        );
        let (_, _, released, _) = claimed.release(poll);
        if let Some(machine) = released {
            machine.finish();
        }
        (yielded, budget.spent())
    }

    #[cfg(test)]
    pub(super) fn poll_claimed_task_with_probe(
        self: &Arc<Self>,
        work: ClaimedTaskWork,
        probe: impl FnOnce(&EvaluationMachinePoll),
    ) {
        let mut claimed = ClaimedTask::new(self.clone(), work);
        let mut budget = super::EvaluationStepBudget::new(TASK_POLL_QUANTUM);
        let poll = claimed.poll(&mut budget);
        probe(&poll);
        let (_, _, released, _) = claimed.release(poll);
        if let Some(machine) = released {
            machine.finish();
        }
    }

    /// Polls a claimed client demand behind the scheduler's panic boundary,
    /// as [`ClaimedTask::poll`] does for task work.
    pub(super) fn poll_claimed_client_demand(
        self: &Arc<Self>,
        mut claimed: coordinator::ClaimedClientDemand,
    ) {
        if let Some(report) = claimed.prior_dependency_panic() {
            self.release_client_demand(claimed, coordinator::ClientDemandPoll::Panicked(report));
            return;
        }
        let context = EvaluationPollContext::for_claim(&claimed.demand);
        let mut budget = super::EvaluationStepBudget::new(TASK_POLL_QUANTUM);
        let polled = catch_unwind(AssertUnwindSafe(|| claimed.poll(&context, &mut budget)));
        drop(context);
        let poll = match polled {
            Ok(coordinator::ClientDemandPoll::Blocked(dependency)) => {
                match dependency.panic_report() {
                    Some(report) => coordinator::ClientDemandPoll::Panicked(report),
                    None => coordinator::ClientDemandPoll::Blocked(dependency),
                }
            }
            Ok(poll) => poll,
            Err(payload) => coordinator::ClientDemandPoll::Panicked(EvaluationPanic::from_payload(
                payload.as_ref(),
                EvaluationPanicOrigin::ClientDemand(claimed.id.get()),
            )),
        };
        self.release_client_demand(claimed, poll);
    }

    pub(super) fn poll_claimed_spark(self: &Arc<Self>, claimed: coordinator::ClaimedSparkWork) {
        let mut claimed = claimed;
        claimed.assert_runtime(self.runtime_id());
        let poll_context = EvaluationPollContext::for_claim(claimed.demand());
        let context = EvalContext::for_spark(claimed.demand_session());
        let mut budget = super::EvaluationStepBudget::new(TASK_POLL_QUANTUM);
        // A spark is speculative and nobody waits on it. When it panics, or
        // blocks on panicked work, it simply retires: the value's real demand
        // reports any panic to its own waiters.
        if claimed.prior_dependency_panicked() {
            drop(context);
            self.release_spark(claimed, coordinator::SparkWorkPoll::Complete);
            return;
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            poll_context.evaluate(&context, |evaluator| {
                claimed.poll(&poll_context, evaluator, &context, &mut budget)
            })
        }));
        let poll = match result {
            Ok(
                crate::eval::strategy_machine::StrategyDemandPoll::Ready
                | crate::eval::strategy_machine::StrategyDemandPoll::Failed,
            )
            | Err(_) => coordinator::SparkWorkPoll::Complete,
            Ok(crate::eval::strategy_machine::StrategyDemandPoll::Pending(dependency)) => {
                if dependency.panic_report().is_some() {
                    coordinator::SparkWorkPoll::Complete
                } else {
                    coordinator::SparkWorkPoll::Blocked(dependency)
                }
            }
            Ok(crate::eval::strategy_machine::StrategyDemandPoll::Yielded) => {
                coordinator::SparkWorkPoll::Yielded
            }
        };
        drop(context);
        self.release_spark(claimed, poll);
    }
}

impl EvaluationDemandState {
    fn claim_ready_task(
        &self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
    ) -> Option<ClaimedTask> {
        let work = coordinator.claim_ready_task_for_session(self.id)?;
        Some(ClaimedTask::new(coordinator.clone(), work))
    }

    fn task_is_running(&self, coordinator: &EvaluationWorkCoordinator) -> bool {
        coordinator.session_background_is_busy(self.id)
    }
}
