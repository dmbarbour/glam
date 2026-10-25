//! Cooperative and runtime evaluation pumping.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use crate::core::{EvaluationPanic, EvaluationPanicOrigin};

use super::access::{DeferredWork, run_deferred};
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
use crate::core::RuntimeValueMutator;
use crate::core::{EvaluationFailure, LazyCycle};

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
            super::whnf::WhnfOwnerPoll::BudgetExhausted => {
                coordinator::ClientDemandPoll::BudgetExhausted
            }
            super::whnf::WhnfOwnerPoll::Failed(failure) => {
                coordinator::ClientDemandPoll::Failed(failure)
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

/// The most steps one poll of task or client work takes before its quantum
/// boundary, where a waiting collection joins and other work gets its turn.
/// Measured (`budget-quantum`, 2026-10-22): from 64, a quantum of 256 cut
/// instructions over the profiling workloads by 1.7% and none rose; 1,024
/// and 4,096 cut 2.0 and 2.1% but slowed `sum` by up to 3%, with the same
/// evaluation counters, so a collection's longer wait for its boundary
/// likely cost it.
pub(super) const TASK_POLL_QUANTUM: usize = 256;

/// A poll that reports its budget exhausted has spent all of it: nothing else
/// ends a poll with budget left unreported (`budget-exhausted-only`).
pub(crate) fn assert_spent_when_exhausted(exhausted: bool, budget: &super::EvaluationStepBudget) {
    debug_assert!(
        !exhausted || budget.remaining() == 0,
        "a poll reported its budget exhausted with {} of {} units left",
        budget.remaining(),
        budget.granted()
    );
}

enum ReleasedTaskMachine {
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
    /// Cancels or drops the machine with `mutator` suspended, since that may
    /// run client code, then retires its work.
    fn finish_under(self, mutator: &mut RuntimeValueMutator<'_, '_>) {
        mutator.suspend(|| self.finish());
    }

    fn finish(self) {
        let retirement = match self {
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

/// A claimed poll before its quantum boundary: the poll, and the work it
/// deferred until the driver's mutator is suspended.
struct PolledTask {
    poll: EvaluationMachinePoll,
    deferred: DeferredWork,
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

    /// Polls the claimed machine under the driver's `mutator`, behind the
    /// scheduler's panic boundary. The quantum boundary that follows runs
    /// under the mutator too: [`Self::finish_polled_under`], then
    /// [`Self::release`] (`boundary-release-under-mutator`).
    ///
    /// A panic during the poll ends this work as `Panicked` instead of
    /// stranding its claim. Work blocked on a panicked dependency also ends
    /// as `Panicked` without polling, because polling would reinstall the
    /// panicked work.
    fn poll_under(
        &mut self,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        step_budget: &mut super::EvaluationStepBudget,
    ) -> PolledTask {
        if let Some(report) = self.kind.prior_dependency_panic() {
            return PolledTask {
                poll: EvaluationMachinePoll::Panicked {
                    report,
                    torn: false,
                },
                deferred: Vec::new(),
            };
        }
        let demand = self.kind.demand().demand();
        let kind = &mut self.kind;
        // A reflection task suspends the mutator around its host callbacks
        // and the work it pumps.
        let polled = catch_unwind(AssertUnwindSafe(|| {
            EvaluationPollContext::quantum_under(mutator, demand, |context| match kind {
                ClaimedTaskKind::Reflection(task) => task.poll(context, step_budget),
                ClaimedTaskKind::Deferred(task) => task.poll(context, step_budget),
                ClaimedTaskKind::LazyRoute(route) => route.poll(context, step_budget),
            })
        }));
        // A panic inside one of the poll's suspensions left the mutator
        // without admission.
        mutator.readmit();
        match polled {
            Ok((poll, deferred)) => {
                assert_spent_when_exhausted(
                    matches!(poll, EvaluationMachinePoll::BudgetExhausted),
                    step_budget,
                );
                PolledTask { poll, deferred }
            }
            Err(payload) => PolledTask {
                poll: EvaluationMachinePoll::Panicked {
                    report: EvaluationPanic::from_payload(
                        payload.as_ref(),
                        self.kind.panic_origin(),
                    ),
                    torn: true,
                },
                deferred: Vec::new(),
            },
        }
    }

    /// Returns the poll to release, running the work it deferred, such as a
    /// host call, with `mutator` suspended, when there is any.
    fn finish_polled_under(
        &self,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        polled: PolledTask,
    ) -> EvaluationMachinePoll {
        if polled.deferred.is_empty() {
            return halt_blocked_on_panic(polled.poll);
        }
        mutator.suspend(|| self.finish_polled(polled))
    }

    /// Runs the work the poll deferred, such as a host call, behind the
    /// poll's panic boundary, and returns the poll to release. Runs with no
    /// mutator.
    fn finish_polled(&self, polled: PolledTask) -> EvaluationMachinePoll {
        let PolledTask { poll, deferred } = polled;
        match catch_unwind(AssertUnwindSafe(|| run_deferred(deferred))) {
            Ok(()) => halt_blocked_on_panic(poll),
            Err(payload) => EvaluationMachinePoll::Panicked {
                report: EvaluationPanic::from_payload(payload.as_ref(), self.kind.panic_origin()),
                torn: true,
            },
        }
    }

    /// Polls under `mutator`, then crosses the quantum boundary. Returns
    /// whether the poll yielded.
    fn poll_and_release(
        mut self,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        step_budget: &mut super::EvaluationStepBudget,
    ) -> bool {
        let polled = self.poll_under(mutator, step_budget);
        self.cross_boundary(mutator, polled)
    }

    /// The quantum boundary, under the driver's `mutator`: the work the poll
    /// deferred, the release, and the released machine's disposal, each
    /// suspending it only as it must. Returns whether the poll yielded.
    fn cross_boundary(self, mutator: &mut RuntimeValueMutator<'_, '_>, polled: PolledTask) -> bool {
        let poll = self.finish_polled_under(mutator, polled);
        let yielded = matches!(
            poll,
            EvaluationMachinePoll::BudgetExhausted | EvaluationMachinePoll::Contended
        );
        let (released, _) = self.release(mutator, poll);
        if let Some(machine) = released {
            machine.finish_under(mutator);
        }
        yielded
    }

    /// Releases the polled work under the driver's `mutator`, which a
    /// requested collection, status callbacks and failure diagnostics
    /// suspend or borrow as they need.
    fn release(
        self,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        poll: EvaluationMachinePoll,
    ) -> (Option<ReleasedTaskMachine>, Option<ExactRouteRelease>) {
        // The quantum boundary: the poll's result is rooted and the work is
        // still claimed, so readiness stays `Busy` through any collection
        // here.
        self.coordinator.service_collection_pressure_under(mutator);
        match self.kind {
            ClaimedTaskKind::Reflection(task) => {
                release_reflection_task(mutator, &self.coordinator, task, poll)
            }
            ClaimedTaskKind::Deferred(task) => {
                release_deferred_task(mutator, &self.coordinator, task, poll)
            }
            ClaimedTaskKind::LazyRoute(route) => {
                release_lazy_route(mutator, &self.coordinator, route, poll)
            }
        }
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
    mutator: &mut RuntimeValueMutator<'_, '_>,
    coordinator: &Arc<EvaluationWorkCoordinator>,
    claimed: ClaimedLazyRoute,
    poll: EvaluationMachinePoll,
) -> (Option<ReleasedTaskMachine>, Option<ExactRouteRelease>) {
    let work = claimed.id();
    let lazy = claimed.lazy().clone();
    let (work_poll, terminal) = match poll {
        // Contended work runs again, as work that spent its budget does.
        EvaluationMachinePoll::BudgetExhausted | EvaluationMachinePoll::Contended => {
            (DeferredWorkPoll::Runnable, None)
        }
        EvaluationMachinePoll::Blocked(block) => (DeferredWorkPoll::Blocked(block), None),
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
                mutator.with_access(|access| record_lazy_panic(&access, &lazy, report))
            } else {
                EvaluationWaitTerminal::Panicked(report)
            }),
        ),
    };
    let mut release = coordinator.release_lazy_route(claimed, work_poll);
    let route = release.route.take();
    if !release.cycle.is_empty() {
        poison_lazy_cycle(
            mutator,
            coordinator,
            std::mem::take(&mut release.cycle),
            release.cycle_error.take(),
        );
        return (None, route);
    }
    if !release.terminal {
        return (None, route);
    }
    let terminal = terminal.expect("terminal lazy route poll must carry a terminal result");
    let failure = match &terminal {
        EvaluationWaitTerminal::Complete(_) => {
            evaluation_failure("lazy route completed without fulfilling its fixpoint")
        }
        EvaluationWaitTerminal::Failed(error) => error.as_failure().clone(),
        EvaluationWaitTerminal::Panicked(report) => {
            coordinator.settle_panicked_work_under(mutator, work, report.clone());
            coordinator.retire_lazy_route(work);
            return (None, route);
        }
        EvaluationWaitTerminal::Cancelled
        | EvaluationWaitTerminal::Abandoned
        | EvaluationWaitTerminal::Killed(_) => {
            unreachable!("lazy route terminal is complete, failed, or panicked")
        }
    };
    coordinator.settle_terminal_work_under(mutator, work, terminal, failure);
    coordinator.retire_lazy_route(work);
    (None, route)
}

impl EvaluationDemandState {
    pub(super) fn run_until_quiescent(&self) -> EvaluationSessionRun {
        if self.is_closed() {
            return self.closed_run_report();
        }
        let Some(coordinator) = self.coordinator() else {
            return self.closed_run_report();
        };
        // The drain holds the thread's mutator; its polls borrow it, and its
        // waits and quantum boundaries suspend it (`driver-mutator`).
        let quiescent = self.values.with_runtime_value_mutator(|mut mutator| {
            loop {
                let claimed = loop {
                    if self.is_closed() {
                        return false;
                    }
                    // Read before claiming, so a task a worker requeues after
                    // a failed claim changes the generation compared below.
                    let generation = coordinator.work_generation();
                    if let Some(claimed) = self.claim_ready_task(&coordinator) {
                        break claimed;
                    }
                    // A probe acts as another worker would, releasing
                    // work, so it runs with the drain's mutator suspended.
                    #[cfg(test)]
                    mutator.suspend(|| self.probe_drain(super::SessionDrainPoint::ClaimMissed));
                    if self.task_is_running(&coordinator) {
                        let changed = mutator.suspend(|| {
                            coordinator.wait_for_change_for(
                                generation,
                                None,
                                CoordinatorWaiterClass::SessionDrain,
                            )
                        });
                        if changed {
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
                    #[cfg(test)]
                    mutator.suspend(|| {
                        self.probe_drain(super::SessionDrainPoint::BeforeQuiescence);
                    });
                    // Any change since the claim attempt, such as a worker
                    // requeueing or claiming a task between the checks above,
                    // means the session may not be quiescent: look again.
                    if coordinator.work_generation() != generation {
                        continue;
                    }
                    return true;
                };
                let mut budget = super::EvaluationStepBudget::new(TASK_POLL_QUANTUM);
                claimed.poll_and_release(&mut mutator, &mut budget);
            }
        });
        if quiescent {
            self.session_run_report(&coordinator)
        } else {
            self.closed_run_report()
        }
    }

    fn session_run_report(&self, coordinator: &EvaluationWorkCoordinator) -> EvaluationSessionRun {
        if self.is_closed() {
            return self.closed_run_report();
        }
        let snapshots = coordinator.reflection_snapshots(self.id);
        let mut unfinished = Vec::new();
        let mut has_live_cross_session_dependency = false;
        for snapshot in snapshots {
            let (state, block) = match &snapshot.state {
                ReflectionWorkState::Dormant => (EvaluationUnfinishedState::Dormant, None),
                ReflectionWorkState::Reserved => (EvaluationUnfinishedState::Reserved, None),
                ReflectionWorkState::Queued => (EvaluationUnfinishedState::Queued, None),
                ReflectionWorkState::Running => (EvaluationUnfinishedState::Running, None),
                ReflectionWorkState::Blocked(block) => {
                    (EvaluationUnfinishedState::Blocked, Some(block))
                }
                ReflectionWorkState::Terminalizing => (EvaluationUnfinishedState::Running, None),
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
                observed_epoch: block.and_then(|block| block.observed_epoch),
                error: block.and_then(|block| block.error.clone()),
            });
        }
        let report = EvaluationSessionReport { unfinished };
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
    allowance: usize,
) -> EvaluationPumpOutcome {
    let mut allowance = allowance;
    context.values().with_runtime_value_mutator(|mut mutator| {
        pump_demand_on_route(
            &mut mutator,
            coordinator,
            context,
            target,
            &mut allowance,
            &mut ExactDemandRoute::default(),
        )
    })
}

/// Pumps work toward `target` under `mutator`, lending each poll a quantum
/// from `allowance` and taking back what it did not reduce. On return the
/// allowance holds what remains, so the caller can charge the steps spent to
/// its own budget. The pumped polls borrow the mutator, which is the driver's
/// or the pumping quantum's, and each quantum boundary suspends it.
pub(super) fn pump_demand_on_route(
    mutator: &mut RuntimeValueMutator<'_, '_>,
    coordinator: &Arc<EvaluationWorkCoordinator>,
    context: &EvalContext,
    target: &EvaluationWaitToken,
    allowance: &mut usize,
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
        if *allowance == 0 {
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
        let quantum = (*allowance).min(TASK_POLL_QUANTUM);
        *allowance -= quantum;
        let mut budget = super::EvaluationStepBudget::new(quantum);
        let polled = claimed.poll_under(mutator, &mut budget);
        debug_assert_eq!(budget.spent() + budget.remaining(), budget.granted());
        // The allowance pays for the reductions the poll made; observing
        // and walking the route are free (`reduction-costs-one-budget-unit`),
        // so the rest of the quantum returns.
        *allowance += budget.remaining();
        // The quantum boundary runs under the mutator, which the release
        // suspends only as it must (`boundary-release-under-mutator`).
        let poll = claimed.finish_polled_under(mutator, polled);
        let yielded = matches!(
            poll,
            EvaluationMachinePoll::BudgetExhausted | EvaluationMachinePoll::Contended
        );
        let (released, route_release) = claimed.release(mutator, poll);
        if exact_claim {
            if let Some(release) = route_release {
                let _handed_off = coordinator.reconcile_exact_route_release(target, route, release);
                #[cfg(any(test, feature = "glam-prof"))]
                if _handed_off {
                    coordinator.record_exact_route_handoffs(1);
                }
            } else {
                route.invalidate_missing_release();
            }
        }
        if let Some(machine) = released {
            machine.finish_under(mutator);
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
    mutator: &mut RuntimeValueMutator<'_, '_>,
    coordinator: &Arc<EvaluationWorkCoordinator>,
    claimed: ClaimedReflectionWork,
    poll: EvaluationMachinePoll,
) -> (Option<ReleasedTaskMachine>, Option<ExactRouteRelease>) {
    let work = claimed.id();
    let mut torn = false;
    let (work_poll, terminal) = match poll {
        // Contended work runs again, as work that spent its budget does.
        EvaluationMachinePoll::BudgetExhausted | EvaluationMachinePoll::Contended => {
            (ReflectionWorkPoll::Runnable, None)
        }
        EvaluationMachinePoll::Blocked(block) => (ReflectionWorkPoll::Blocked(block), None),
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

    let mut release = coordinator.release_reflection_under(mutator, claimed, work_poll);
    let route = release.route.take();
    if !release.cycle.is_empty() {
        poison_lazy_cycle(
            mutator,
            coordinator,
            std::mem::take(&mut release.cycle),
            release.cycle_error.take(),
        );
    }
    if !release.terminal {
        debug_assert!(release.machine.is_none());
        return (None, route);
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
        EvaluationWaitTerminal::Killed(error) => Some(error.as_failure().clone()),
        EvaluationWaitTerminal::Panicked(_) => None,
    };
    match (terminal, promise_failure) {
        (EvaluationWaitTerminal::Panicked(report), _) => {
            coordinator.settle_panicked_work_under(mutator, work, report);
        }
        (terminal, Some(promise_failure)) => {
            coordinator.settle_terminal_work_under(mutator, work, terminal, promise_failure);
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
    (released, route)
}

fn release_deferred_task(
    mutator: &mut RuntimeValueMutator<'_, '_>,
    coordinator: &Arc<EvaluationWorkCoordinator>,
    claimed: ClaimedDeferredWork,
    poll: EvaluationMachinePoll,
) -> (Option<ReleasedTaskMachine>, Option<ExactRouteRelease>) {
    let work = claimed.id();
    let mut torn = false;
    let (work_poll, terminal) = match poll {
        // Contended work runs again, as work that spent its budget does.
        EvaluationMachinePoll::BudgetExhausted | EvaluationMachinePoll::Contended => {
            (DeferredWorkPoll::Runnable, None)
        }
        EvaluationMachinePoll::Blocked(block) => (DeferredWorkPoll::Blocked(block), None),
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
            Some(EvaluationWaitTerminal::Failed(mutator.with_access(
                |access| {
                    access.root_runtime_failure(Arc::new(EvaluationFailure::message(
                        "deferred evaluation task was cancelled",
                    )))
                },
            ))),
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
            mutator,
            coordinator,
            std::mem::take(&mut release.cycle),
            release.cycle_error.take(),
        );
        return (None, route);
    }
    if !release.terminal {
        debug_assert!(release.machine.is_none());
        return (None, route);
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
        EvaluationWaitTerminal::Killed(error) => Some(error.as_failure().clone()),
        EvaluationWaitTerminal::Panicked(_) => None,
    };
    match (terminal, promise_failure) {
        (EvaluationWaitTerminal::Panicked(report), _) => {
            coordinator.settle_panicked_work_under(mutator, work, report);
        }
        (terminal, Some(promise_failure)) => {
            coordinator.settle_terminal_work_under(mutator, work, terminal, promise_failure);
        }
        (_, None) => unreachable!("only a panic omits the promise failure"),
    }
    let machine = release
        .machine
        .take()
        .expect("terminal deferred release must detach its machine");
    let retirement = WorkRetirement::Deferred(coordinator.clone(), work);
    (
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
    access: &crate::core::RuntimeValueAccess<'_>,
    lazy: &crate::core::ManagedLazyRoot,
    report: EvaluationPanic,
) -> EvaluationWaitTerminal {
    if lazy.enter_panicked(access, &report) {
        return EvaluationWaitTerminal::Panicked(report);
    }
    match lazy.access(access).and_then(|lazy| lazy.cached()) {
        Some(Ok(value)) => {
            EvaluationWaitTerminal::Complete(access.root_runtime_value(value.into_value_in(access)))
        }
        Some(Err(failure)) => EvaluationWaitTerminal::Failed(access.root_runtime_failure(failure)),
        None => EvaluationWaitTerminal::Panicked(report),
    }
}

fn poison_lazy_cycle(
    mutator: &mut RuntimeValueMutator<'_, '_>,
    coordinator: &Arc<EvaluationWorkCoordinator>,
    mut members: Vec<DeferredLazyCycleMember>,
    cycle_error: Option<String>,
) {
    let cycle = Arc::new(LazyCycle {
        members: members.iter().map(|member| member.lazy.id()).collect(),
    });
    let failure = cycle_error
        .map(evaluation_failure)
        .unwrap_or_else(|| Arc::new(EvaluationFailure::dependency_cycle(cycle)));
    // Make the shared failure authoritative in every lazy before any
    // producer wait wakes. The already-batched `Terminalizing` transition
    // prevents another worker from reclaiming a cycle member meanwhile.
    let mut terminals = mutator.with_access(|access| {
        members
            .iter()
            .map(|member| {
                debug_assert_eq!(member.wait.runtime_id(), access.runtime_id());
                let terminal = match member.lazy.cache(&access, Err(failure.clone())) {
                    Err(error) => EvaluationWaitTerminal::Failed(access.root_runtime_failure(error)),
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
        *terminal = coordinator.settle_terminal_work_under(
            mutator,
            member.work,
            terminal.clone(),
            failure.clone(),
        );
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
    // Dropping a machine may run client code.
    if !machines.is_empty() {
        mutator.suspend(|| drop(machines));
    }
}

impl EvaluationWorkCoordinator {
    #[cfg(test)]
    pub(crate) fn poll_runtime_work(self: &Arc<Self>) -> bool {
        self.with_driver_mutator_for_test(|mutator| {
            self.poll_runtime_work_bounded(mutator, TASK_POLL_QUANTUM)
        })
        .is_some()
    }

    /// Polls at most one runtime-visible background machine with a bounded
    /// allowance, under a driver's `mutator`. A zero allowance performs no
    /// selection, so merely probing the bounded public pump cannot perturb
    /// queue order.
    pub(crate) fn poll_runtime_work_bounded(
        self: &Arc<Self>,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        step_budget: usize,
    ) -> Option<usize> {
        if step_budget == 0 {
            return None;
        }
        match self.select_runtime_pump() {
            coordinator::CoordinatorSelection::Task(work) => {
                Some(self.poll_claimed_task_bounded(mutator, work, step_budget).1)
            }
            coordinator::CoordinatorSelection::Spark(_) => {
                unreachable!("the runtime pump must not claim best-effort spark work")
            }
            coordinator::CoordinatorSelection::None => None,
        }
    }

    /// Polls claimed task work under a driver's `mutator`, crossing its
    /// quantum boundary with the mutator suspended.
    pub(super) fn poll_claimed_task(
        self: &Arc<Self>,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        work: ClaimedTaskWork,
    ) -> bool {
        self.poll_claimed_task_bounded(mutator, work, TASK_POLL_QUANTUM)
            .0
    }

    pub(super) fn poll_claimed_task_bounded(
        self: &Arc<Self>,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        work: ClaimedTaskWork,
        step_budget: usize,
    ) -> (bool, usize) {
        debug_assert_ne!(step_budget, 0);
        let mut claimed = ClaimedTask::new(self.clone(), work);
        let mut budget = super::EvaluationStepBudget::new(step_budget.min(TASK_POLL_QUANTUM));
        let polled = claimed.poll_under(mutator, &mut budget);
        let yielded = claimed.cross_boundary(mutator, polled);
        (yielded, budget.spent())
    }

    #[cfg(test)]
    pub(super) fn poll_claimed_task_with_probe(
        self: &Arc<Self>,
        work: ClaimedTaskWork,
        probe: impl FnOnce(&EvaluationMachinePoll),
    ) {
        let values = self
            .value_observer()
            .upgrade()
            .expect("claimed work must retain its value domain");
        values.with_runtime_value_mutator(|mut mutator| {
            let mut claimed = ClaimedTask::new(self.clone(), work);
            let mut budget = super::EvaluationStepBudget::new(TASK_POLL_QUANTUM);
            let polled = claimed.poll_under(&mut mutator, &mut budget);
            let poll = claimed.finish_polled_under(&mut mutator, polled);
            // A collection may run at the boundary, with the mutator
            // suspended.
            mutator.suspend(|| probe(&poll));
            let (released, _) = claimed.release(&mut mutator, poll);
            if let Some(machine) = released {
                machine.finish_under(&mut mutator);
            }
        });
    }

    /// Polls a claimed client demand under a driver's `mutator`, behind the
    /// scheduler's panic boundary, as [`ClaimedTask::poll_under`] does for
    /// task work, and crosses its quantum boundary under the mutator too.
    #[cfg(test)]
    pub(super) fn poll_claimed_client_demand(
        self: &Arc<Self>,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        claimed: coordinator::ClaimedClientDemand,
    ) {
        self.poll_claimed_client_demand_bounded(mutator, claimed, TASK_POLL_QUANTUM);
    }

    /// As [`Self::poll_claimed_client_demand`], within `step_budget` steps,
    /// returning the reductions it made.
    pub(super) fn poll_claimed_client_demand_bounded(
        self: &Arc<Self>,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        mut claimed: coordinator::ClaimedClientDemand,
        step_budget: usize,
    ) -> usize {
        debug_assert_ne!(step_budget, 0);
        if let Some(report) = claimed.prior_dependency_panic() {
            self.release_client_demand_under(
                mutator,
                claimed,
                coordinator::ClientDemandPoll::Panicked(report),
            );
            return 0;
        }
        let demand = claimed.demand.demand();
        let mut budget = super::EvaluationStepBudget::new(step_budget.min(TASK_POLL_QUANTUM));
        let polled = catch_unwind(AssertUnwindSafe(|| {
            EvaluationPollContext::quantum_under(mutator, demand, |context| {
                claimed.poll(context, &mut budget)
            })
        }));
        mutator.readmit();
        if let Ok((poll, _)) = &polled {
            assert_spent_when_exhausted(
                matches!(poll, coordinator::ClientDemandPoll::BudgetExhausted),
                &budget,
            );
        }
        self.release_polled_client_demand(mutator, claimed, polled);
        budget.spent()
    }

    /// The quantum boundary of a client demand poll, under the driver's
    /// `mutator`: its deferred work, suspended and under the poll's panic
    /// boundary, then collection pressure and the release.
    fn release_polled_client_demand(
        self: &Arc<Self>,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        claimed: coordinator::ClaimedClientDemand,
        polled: std::thread::Result<(coordinator::ClientDemandPoll, DeferredWork)>,
    ) {
        let polled = polled.and_then(|(poll, deferred)| {
            if deferred.is_empty() {
                return Ok(poll);
            }
            mutator
                .suspend(|| catch_unwind(AssertUnwindSafe(|| run_deferred(deferred))))
                .map(|()| poll)
        });
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
        // The quantum boundary, while the demand is still claimed.
        self.service_collection_pressure_under(mutator);
        self.release_client_demand_under(mutator, claimed, poll);
    }

    /// Polls claimed spark work under a driver's `mutator`, crossing its
    /// quantum boundary under the mutator too.
    pub(super) fn poll_claimed_spark(
        self: &Arc<Self>,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        claimed: coordinator::ClaimedSparkWork,
    ) {
        let mut claimed = claimed;
        claimed.assert_runtime(self.runtime_id());
        let demand = claimed.demand().demand();
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
            EvaluationPollContext::quantum_under(mutator, demand, |poll_context| {
                poll_context.evaluate(&context, |evaluator| {
                    claimed.poll(poll_context, evaluator, &context, &mut budget)
                })
            })
        }));
        mutator.readmit();
        if let Ok((poll, _)) = &result {
            assert_spent_when_exhausted(
                matches!(
                    poll,
                    crate::eval::strategy_machine::StrategyDemandPoll::BudgetExhausted
                ),
                &budget,
            );
        }
        let result = result.and_then(|(poll, deferred)| {
            if deferred.is_empty() {
                return Ok(poll);
            }
            mutator
                .suspend(|| catch_unwind(AssertUnwindSafe(|| run_deferred(deferred))))
                .map(|()| poll)
        });
        self.release_polled_spark(mutator, claimed, context, result);
    }

    /// The quantum boundary of a spark poll, under the driver's `mutator`:
    /// collection pressure and the release, after its evaluator context is
    /// gone.
    fn release_polled_spark(
        self: &Arc<Self>,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        claimed: coordinator::ClaimedSparkWork,
        context: EvalContext,
        result: std::thread::Result<crate::eval::strategy_machine::StrategyDemandPoll>,
    ) {
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
            Ok(crate::eval::strategy_machine::StrategyDemandPoll::BudgetExhausted) => {
                coordinator::SparkWorkPoll::Runnable
            }
        };
        drop(context);
        // The quantum boundary, while the spark is still claimed.
        self.service_collection_pressure_under(mutator);
        self.release_spark(claimed, poll);
    }
}

#[cfg(test)]
impl EvaluationWorkCoordinator {
    /// Runs `operation` as a driver would, under a mutator of its own.
    pub(in crate::evaluation) fn with_driver_mutator_for_test<R>(
        &self,
        operation: impl FnOnce(&mut RuntimeValueMutator<'_, '_>) -> R,
    ) -> R {
        self.value_observer()
            .upgrade()
            .expect("test work must retain its value domain")
            .with_runtime_value_mutator(|mut mutator| operation(&mut mutator))
    }

    pub(super) fn poll_runtime_work_bounded_for_test(
        self: &Arc<Self>,
        step_budget: usize,
    ) -> Option<usize> {
        self.with_driver_mutator_for_test(|mutator| {
            self.poll_runtime_work_bounded(mutator, step_budget)
        })
    }

    pub(super) fn poll_claimed_task_for_test(self: &Arc<Self>, work: ClaimedTaskWork) -> bool {
        self.with_driver_mutator_for_test(|mutator| self.poll_claimed_task(mutator, work))
    }

    pub(super) fn poll_claimed_client_demand_for_test(
        self: &Arc<Self>,
        claimed: coordinator::ClaimedClientDemand,
    ) {
        self.with_driver_mutator_for_test(|mutator| {
            self.poll_claimed_client_demand(mutator, claimed);
        });
    }

    pub(super) fn poll_claimed_spark_for_test(
        self: &Arc<Self>,
        claimed: coordinator::ClaimedSparkWork,
    ) {
        self.with_driver_mutator_for_test(|mutator| self.poll_claimed_spark(mutator, claimed));
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

    /// Installs `probe`, which a session drain calls at each
    /// `SessionDrainPoint`.
    #[cfg(test)]
    pub(super) fn set_drain_probe(
        &self,
        probe: impl FnMut(super::SessionDrainPoint) + Send + 'static,
    ) {
        *self.drain_probe.lock().unwrap() = Some(Box::new(probe));
    }

    #[cfg(test)]
    fn probe_drain(&self, point: super::SessionDrainPoint) {
        // Unlocked while it runs, as it may change coordinator state.
        let probe = self.drain_probe.lock().unwrap().take();
        if let Some(mut probe) = probe {
            probe(point);
            *self.drain_probe.lock().unwrap() = Some(probe);
        }
    }
}

/// A session task claimed as a background worker would claim it, so a test
/// can hold it running and yield it back at a chosen point.
#[cfg(test)]
pub(super) struct TestWorkerClaim(ClaimedTask);

#[cfg(test)]
impl TestWorkerClaim {
    pub(super) fn claim(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        session: EvaluationSessionId,
    ) -> Option<Self> {
        coordinator
            .claim_ready_session_machine_for_test(session)
            .map(|work| Self(ClaimedTask::new(coordinator.clone(), work)))
    }

    /// Releases the task unpolled, as a worker that yields, requeueing it.
    pub(super) fn yield_back(self) {
        let coordinator = self.0.coordinator.clone();
        coordinator.with_driver_mutator_for_test(|mutator| {
            let (released, _) = self
                .0
                .release(mutator, EvaluationMachinePoll::BudgetExhausted);
            if let Some(machine) = released {
                machine.finish_under(mutator);
            }
        });
    }
}
