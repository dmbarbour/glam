//! Cooperative and runtime evaluation pumping.

use std::collections::HashSet;
use std::sync::Arc;

use super::coordinator::{
    self, CausalChildSelection, ClaimedDeferredWork, ClaimedLazyRoute, ClaimedReflectionWork,
    ClaimedTaskWork, ClientDemandOperation, DeferredLazyCycleMember, DeferredWorkPoll,
    EvaluationMachinePoll, EvaluationSessionId, EvaluationTaskId, EvaluationTaskMachine,
    EvaluationWaitPoll, EvaluationWaitTerminal, EvaluationWaitToken, EvaluationWorkCoordinator,
    EvaluationWorkId, ReflectionWorkPoll, ReflectionWorkState, WorkDependency,
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
        match super::whnf::poll_computation(&mut self.0, poll_context, context, step_budget) {
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
                unreachable!("W2 semantic shell produced an external {boundary:?} boundary")
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

    fn poll(&mut self, step_budget: &mut super::EvaluationStepBudget) -> EvaluationMachinePoll {
        let context = EvaluationPollContext::for_claim(self.kind.demand());
        match &mut self.kind {
            ClaimedTaskKind::Reflection(task) => task.poll(&context, step_budget),
            ClaimedTaskKind::Deferred(task) => task.poll(&context, step_budget),
            ClaimedTaskKind::LazyRoute(route) => route.poll(&context, step_budget),
        }
    }

    fn release(self, poll: EvaluationMachinePoll) -> (bool, bool, Option<ReleasedTaskMachine>) {
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
}

fn release_lazy_route(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    claimed: ClaimedLazyRoute,
    poll: EvaluationMachinePoll,
) -> (bool, bool, Option<ReleasedTaskMachine>) {
    let work = claimed.id();
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
    };
    let mut release = coordinator.release_lazy_route(claimed, work_poll);
    if !release.cycle.is_empty() {
        poison_lazy_cycle(
            coordinator,
            std::mem::take(&mut release.cycle),
            release.cycle_error.take(),
        );
        return (release.made_progress, false, None);
    }
    if !release.terminal {
        return (release.made_progress, release.remains_blocked, None);
    }
    let terminal = terminal.expect("terminal lazy route poll must carry a terminal result");
    let failure = match &terminal {
        EvaluationWaitTerminal::Complete(_) => {
            evaluation_failure("lazy route completed without fulfilling its fixpoint")
        }
        EvaluationWaitTerminal::Failed(error) => error.as_failure().clone(),
        _ => unreachable!("lazy route terminal is complete or failed"),
    };
    coordinator.settle_terminal_work(work, terminal, failure);
    coordinator.retire_lazy_route(work);
    (release.made_progress, false, None)
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
                    coordinator.wait_for_change(generation);
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
            let (_, _, released) = claimed.release(poll);
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

pub(super) fn prioritized_task_for(
    coordinator: &EvaluationWorkCoordinator,
    target: &EvaluationWaitToken,
) -> Option<EvaluationWorkId> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut wait = target.clone();
    while let Some(work) = coordinator.work_for_wait(&wait) {
        if !seen.insert(work) {
            break;
        }
        chain.push(work);
        let Some(dependency) = coordinator.work_dependency_by_id(work) else {
            break;
        };
        let Some(dependency_wait) = dependency.producer_wait() else {
            break;
        };
        wait = dependency_wait.clone();
    }
    chain
        .into_iter()
        .rev()
        .find(|work| coordinator.work_is_claimable(*work))
}

pub(super) fn pump_demand(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    session: EvaluationSessionId,
    context: &EvalContext,
    target: &EvaluationWaitToken,
    mut step_budget: usize,
) -> EvaluationPumpOutcome {
    if target.terminal_poll().is_some() {
        return EvaluationPumpOutcome::TargetReady;
    }
    if target.runtime_id() != context.values().runtime_id() {
        return EvaluationPumpOutcome::NoProgress;
    }
    let mut yielded_exact = None;
    loop {
        if !matches!(context.poll_wait(target), EvaluationWaitPoll::Pending(_)) {
            return EvaluationPumpOutcome::TargetReady;
        }
        if step_budget == 0 {
            return EvaluationPumpOutcome::BudgetExhausted;
        }

        if coordinator.target_has_running_producer(target) {
            return EvaluationPumpOutcome::Busy;
        }
        let prioritized = yielded_exact
            .take()
            .or_else(|| prioritized_task_for(coordinator, target));
        let exact = prioritized.and_then(|work| coordinator.claim_work(work));
        let (claimed, causal_busy) = if let Some(exact) = exact {
            (Some(exact), false)
        } else {
            match coordinator.claim_causal_child_work(target, context.causal_task_ids()) {
                CausalChildSelection::Claimed(child) => (Some(child), false),
                CausalChildSelection::Busy => (None, true),
                CausalChildSelection::None => (None, false),
            }
        };
        let Some(work) = claimed else {
            if causal_busy {
                return EvaluationPumpOutcome::Busy;
            }
            if coordinator.target_has_running_producer(target) {
                return EvaluationPumpOutcome::Busy;
            }
            if coordinator.demand_session_has_running_machine(session) {
                return EvaluationPumpOutcome::Busy;
            }
            if coordinator.dependency_observes_runtime(target)
                && coordinator.runtime_has_running_machine()
            {
                return EvaluationPumpOutcome::Busy;
            }
            if !matches!(context.poll_wait(target), EvaluationWaitPoll::Pending(_)) {
                return EvaluationPumpOutcome::TargetReady;
            }
            return EvaluationPumpOutcome::NoProgress;
        };

        let work_id = work.id();
        let mut claimed = ClaimedTask::new(coordinator.clone(), work);
        let quantum = step_budget.min(TASK_POLL_QUANTUM);
        step_budget -= quantum;
        let mut budget = super::EvaluationStepBudget::new(quantum);
        let poll = claimed.poll(&mut budget);
        debug_assert_eq!(budget.spent() + budget.remaining(), budget.granted());
        let yielded = matches!(
            poll,
            EvaluationMachinePoll::Yielded | EvaluationMachinePoll::ScheduleSpark(_)
        );
        let (_, _, released) = claimed.release(poll);
        if let Some(machine) = released {
            machine.finish();
        }
        if yielded {
            // An exact, dormant producer can consume a quantum immediately
            // before publishing the dependency it discovered. Preserve that
            // continuation only in this bounded demand pump. Globally queuing
            // it would turn speculative demand from an abandoned alternative
            // into unbounded eager evaluation by background workers.
            yielded_exact = Some(work_id);
        }
    }
}

fn release_reflection_task(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    claimed: ClaimedReflectionWork,
    poll: EvaluationMachinePoll,
) -> (bool, bool, Option<ReleasedTaskMachine>) {
    let work = claimed.id();
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
    };

    let mut release = coordinator.release_reflection(claimed, work_poll);
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
            return (release.made_progress, release.remains_blocked, None);
        }
        let released = release.machine.take().map(ReleasedTaskMachine::DropOnly);
        return (release.made_progress, release.remains_blocked, released);
    }

    let terminal = if release.cancel {
        EvaluationWaitTerminal::Cancelled
    } else if release.abandoned {
        EvaluationWaitTerminal::Abandoned
    } else {
        terminal.expect("terminal reflection poll must carry a terminal result")
    };
    let promise_failure = match &terminal {
        EvaluationWaitTerminal::Complete(_) => {
            evaluation_failure("reflection task completed without fulfilling its fixpoint")
        }
        EvaluationWaitTerminal::Failed(error) => error.as_failure().clone(),
        EvaluationWaitTerminal::Cancelled => {
            evaluation_failure("reflection fixpoint producer was cancelled")
        }
        EvaluationWaitTerminal::Abandoned => {
            evaluation_failure("reflection fixpoint producer was abandoned")
        }
        EvaluationWaitTerminal::Exited => {
            evaluation_failure("reflection fixpoint producer exited without a result")
        }
        EvaluationWaitTerminal::Killed(error) => error.as_failure().clone(),
    };
    coordinator.settle_terminal_work(work, terminal, promise_failure);
    let machine = release
        .machine
        .take()
        .expect("terminal reflection release must retain its detached machine");
    let retirement = WorkRetirement::Reflection(coordinator.clone(), work);
    let released = Some(if release.cancel {
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
    (release.made_progress, false, released)
}

fn release_deferred_task(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    claimed: ClaimedDeferredWork,
    poll: EvaluationMachinePoll,
) -> (bool, bool, Option<ReleasedTaskMachine>) {
    let work = claimed.id();
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
    };

    let mut release = coordinator.release_deferred(claimed, work_poll);
    if !release.cycle.is_empty() {
        poison_lazy_cycle(
            coordinator,
            std::mem::take(&mut release.cycle),
            release.cycle_error.take(),
        );
        return (release.made_progress, false, None);
    }
    if !release.terminal {
        debug_assert!(release.machine.is_none());
        return (release.made_progress, release.remains_blocked, None);
    }

    let terminal = if release.abandoned {
        EvaluationWaitTerminal::Abandoned
    } else {
        terminal.expect("terminal deferred poll must carry a terminal result")
    };
    let promise_failure = match &terminal {
        EvaluationWaitTerminal::Complete(_) => {
            evaluation_failure("evaluation task completed without fulfilling its fixpoint")
        }
        EvaluationWaitTerminal::Failed(error) => error.as_failure().clone(),
        EvaluationWaitTerminal::Cancelled => {
            evaluation_failure("evaluation fixpoint producer was cancelled")
        }
        EvaluationWaitTerminal::Abandoned => {
            evaluation_failure("evaluation fixpoint producer was abandoned")
        }
        EvaluationWaitTerminal::Exited => {
            evaluation_failure("evaluation fixpoint producer exited without a result")
        }
        EvaluationWaitTerminal::Killed(error) => error.as_failure().clone(),
    };
    coordinator.settle_terminal_work(work, terminal, promise_failure);
    let machine = release
        .machine
        .take()
        .expect("terminal deferred release must detach its machine");
    (
        release.made_progress,
        false,
        Some(ReleasedTaskMachine::Drop {
            machine,
            retirement: WorkRetirement::Deferred(coordinator.clone(), work),
        }),
    )
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
                            access.root_runtime_value(value.into_value()),
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
    pub(crate) fn poll_runtime_work(self: &Arc<Self>) -> bool {
        match self.select_runtime_pump() {
            coordinator::CoordinatorSelection::Task(work) => {
                self.poll_claimed_task(work);
                true
            }
            coordinator::CoordinatorSelection::Spark(_) => {
                unreachable!("the runtime pump must not claim best-effort spark work")
            }
            coordinator::CoordinatorSelection::None => false,
        }
    }

    pub(super) fn poll_claimed_task(self: &Arc<Self>, work: ClaimedTaskWork) -> bool {
        let mut claimed = ClaimedTask::new(self.clone(), work);
        let mut budget = super::EvaluationStepBudget::new(TASK_POLL_QUANTUM);
        let poll = claimed.poll(&mut budget);
        let yielded = matches!(
            poll,
            EvaluationMachinePoll::Yielded | EvaluationMachinePoll::ScheduleSpark(_)
        );
        let (_, _, released) = claimed.release(poll);
        if let Some(machine) = released {
            machine.finish();
        }
        yielded
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
        let (_, _, released) = claimed.release(poll);
        if let Some(machine) = released {
            machine.finish();
        }
    }

    pub(super) fn poll_claimed_client_demand(
        self: &Arc<Self>,
        mut claimed: coordinator::ClaimedClientDemand,
    ) {
        let context = EvaluationPollContext::for_claim(&claimed.demand);
        let mut budget = super::EvaluationStepBudget::new(TASK_POLL_QUANTUM);
        let poll = claimed.poll(&context, &mut budget);
        self.release_client_demand(claimed, poll);
    }

    pub(super) fn poll_claimed_spark(self: &Arc<Self>, claimed: coordinator::ClaimedSparkWork) {
        let mut claimed = claimed;
        claimed.assert_runtime(self.runtime_id());
        let poll_context = EvaluationPollContext::for_claim(claimed.demand());
        let context = EvalContext::for_spark(claimed.demand_session());
        let mut budget = super::EvaluationStepBudget::new(TASK_POLL_QUANTUM);
        let result = poll_context.evaluate(&context, |evaluator| {
            claimed.poll(&poll_context, evaluator, &context, &mut budget)
        });
        let poll = match result {
            crate::eval::strategy_machine::StrategyDemandPoll::Ready
            | crate::eval::strategy_machine::StrategyDemandPoll::Failed => {
                coordinator::SparkWorkPoll::Complete
            }
            crate::eval::strategy_machine::StrategyDemandPoll::Pending(dependency) => {
                coordinator::SparkWorkPoll::Blocked(dependency)
            }
            crate::eval::strategy_machine::StrategyDemandPoll::Yielded => {
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
        let work = coordinator
            .claim_ready_task_for_session(self.id)
            .or_else(|| coordinator.claim_ready_lazy_route_dependency_for_session(self.id))?;
        Some(ClaimedTask::new(coordinator.clone(), work))
    }

    fn task_is_running(&self, coordinator: &EvaluationWorkCoordinator) -> bool {
        coordinator.session_machine_is_busy(self.id)
    }
}
