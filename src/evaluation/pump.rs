//! Cooperative and runtime evaluation pumping.

use std::collections::HashSet;
use std::sync::Arc;

use super::coordinator::{
    self, ClaimedDeferredWork, ClaimedReflectionWork, ClaimedTaskWork, ClientDemandOperation,
    DeferredLazyCycleMember, DeferredWorkPoll, EvaluationMachinePoll, EvaluationSessionId,
    EvaluationTaskId, EvaluationTaskMachine, EvaluationWaitPoll, EvaluationWaitTerminal,
    EvaluationWaitToken, EvaluationWorkCoordinator, EvaluationWorkId, ReflectionWorkPoll,
    ReflectionWorkState, WorkDependency,
};
use super::session::{
    EvalContext, EvaluationSessionReport, EvaluationSessionRun, EvaluationUnfinishedState,
    EvaluationUnfinishedTask, client_demand_halt_poll,
};
use super::{EvaluationDemandState, EvaluationPollContext, evaluation_failure};
use crate::core::{EvaluationFailure, LazyCycle, LazyCycleMember};
use crate::runtime::RuntimeValueRoot;

impl ClientDemandOperation {
    pub(super) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvalContext,
    ) -> coordinator::ClientDemandPoll {
        let evaluator = poll_context.evaluator(context);
        match crate::eval::eval_value_in(&evaluator, self.0.as_core()) {
            Ok(value) => coordinator::ClientDemandPoll::Complete(evaluator.root_value(value)),
            Err(halt) => client_demand_halt_poll(halt),
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
        let Some(producer) = coordinator.producer_for_wait(&wait) else {
            break;
        };
        let Some(dependency) = coordinator.task_dependency(producer) else {
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
    task: EvaluationTaskId,
    session: EvaluationSessionId,
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
}

impl ClaimedTask {
    fn new(coordinator: Arc<EvaluationWorkCoordinator>, work: ClaimedTaskWork) -> Self {
        work.demand().assert_runtime(coordinator.runtime_id());
        let kind = match work {
            ClaimedTaskWork::Reflection(claim) => ClaimedTaskKind::Reflection(claim),
            ClaimedTaskWork::Deferred(claim) => ClaimedTaskKind::Deferred(claim),
        };
        Self { coordinator, kind }
    }

    fn poll(&mut self, step_budget: usize) -> EvaluationMachinePoll {
        let context = EvaluationPollContext::for_claim(self.kind.demand());
        match &mut self.kind {
            ClaimedTaskKind::Reflection(task) => task.poll(&context, step_budget),
            ClaimedTaskKind::Deferred(task) => task.poll(&context, step_budget),
        }
    }

    fn release(self, poll: EvaluationMachinePoll) -> (bool, bool, Option<ReleasedTaskMachine>) {
        match self.kind {
            ClaimedTaskKind::Reflection(task) => {
                release_reflection_task(&self.coordinator, task, poll)
            }
            ClaimedTaskKind::Deferred(task) => release_deferred_task(&self.coordinator, task, poll),
        }
    }
}

impl ClaimedTaskKind {
    fn demand(&self) -> &coordinator::ClaimedDemandSession {
        match self {
            Self::Reflection(task) => task.demand(),
            Self::Deferred(task) => task.demand(),
        }
    }
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

            let poll = claimed.poll(TASK_POLL_QUANTUM);
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
                dependency: dependency.as_ref().map(|dependency| dependency.task),
                dependency_session: dependency.as_ref().map(|dependency| dependency.session),
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
            if !seen.insert(wait.get()) || wait.owner_id() != self.id {
                return Some(ReportedDependency {
                    task: wait.producer(),
                    session: wait.owner_id(),
                    wait: wait.get(),
                    live_cross_session: wait.owner_id() != self.id
                        && coordinator.demand_session_is_open(wait.owner_id()),
                });
            }
            let Some(next) = coordinator.task_dependency(wait.producer()) else {
                return Some(ReportedDependency {
                    task: wait.producer(),
                    session: wait.owner_id(),
                    wait: wait.get(),
                    live_cross_session: false,
                });
            };
            let Some(next_wait) = next.producer_wait() else {
                return Some(ReportedDependency {
                    task: wait.producer(),
                    session: wait.owner_id(),
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
) -> Option<EvaluationTaskId> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut wait = target.clone();
    while let Some(task) = coordinator.producer_for_wait(&wait) {
        if !seen.insert(task) {
            break;
        }
        chain.push(task);
        let Some(dependency) = coordinator.task_dependency(task) else {
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
        .find(|task| coordinator.task_is_claimable(*task))
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
    if !target.belongs_to(&context.session) {
        return EvaluationPumpOutcome::NoProgress;
    }
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
        let prioritized = prioritized_task_for(coordinator, target);
        let claimed = prioritized
            .and_then(|task| coordinator.claim_task(task))
            .or_else(|| coordinator.claim_ready_task_for_session(session));
        let Some(work) = claimed else {
            if coordinator.target_has_running_producer(target) {
                return EvaluationPumpOutcome::Busy;
            }
            if !matches!(context.poll_wait(target), EvaluationWaitPoll::Pending(_)) {
                return EvaluationPumpOutcome::TargetReady;
            }
            return EvaluationPumpOutcome::NoProgress;
        };

        let mut claimed = ClaimedTask::new(coordinator.clone(), work);
        let quantum = step_budget.min(TASK_POLL_QUANTUM);
        step_budget -= quantum;
        let poll = claimed.poll(quantum);
        let (_, _, released) = claimed.release(poll);
        if let Some(machine) = released {
            machine.finish();
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
        EvaluationWaitTerminal::Failed(error) => error.clone(),
        EvaluationWaitTerminal::Cancelled => {
            evaluation_failure("reflection fixpoint producer was cancelled")
        }
        EvaluationWaitTerminal::Abandoned => {
            evaluation_failure("reflection fixpoint producer was abandoned")
        }
        EvaluationWaitTerminal::Exited => {
            evaluation_failure("reflection fixpoint producer exited without a result")
        }
        EvaluationWaitTerminal::Killed(error) => error.clone(),
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
            Some(EvaluationWaitTerminal::Failed(Arc::new(
                EvaluationFailure::message("deferred evaluation task was cancelled"),
            ))),
        ),
    };

    let mut release = coordinator.release_deferred(claimed, work_poll);
    if !release.cycle.is_empty() {
        poison_lazy_cycle(coordinator, std::mem::take(&mut release.cycle));
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
        EvaluationWaitTerminal::Failed(error) => error.clone(),
        EvaluationWaitTerminal::Cancelled => {
            evaluation_failure("evaluation fixpoint producer was cancelled")
        }
        EvaluationWaitTerminal::Abandoned => {
            evaluation_failure("evaluation fixpoint producer was abandoned")
        }
        EvaluationWaitTerminal::Exited => {
            evaluation_failure("evaluation fixpoint producer exited without a result")
        }
        EvaluationWaitTerminal::Killed(error) => error.clone(),
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
    members: Vec<DeferredLazyCycleMember>,
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
    let failure = Arc::new(EvaluationFailure::dependency_cycle(cycle));
    // Make the shared failure authoritative in every lazy before any
    // producer wait wakes. The already-batched `Terminalizing` transition
    // prevents another worker from reclaiming a cycle member meanwhile.
    let mut terminals = members
        .iter()
        .map(|member| {
            let terminal = match member.lazy.cache(Err(failure.clone())) {
                Err(error) => EvaluationWaitTerminal::Failed(error),
                Ok(value) => {
                    debug_assert!(
                        false,
                        "a successful concurrent lazy result contradicts a strict dependency cycle"
                    );
                    EvaluationWaitTerminal::Complete(RuntimeValueRoot::from_runtime(
                        member.wait.runtime_id(),
                        value.into_value(),
                    ))
                }
            };
            (member, terminal)
        })
        .collect::<Vec<_>>();
    for (member, terminal) in &mut terminals {
        *terminal =
            coordinator.settle_terminal_work(member.work, terminal.clone(), failure.clone());
    }
    for (member, terminal) in &terminals {
        debug_assert_eq!(member.wait.terminal_poll(), Some(terminal.to_poll()));
        coordinator.retire_deferred(member.work);
    }
    drop(terminals);
    let machines = members
        .into_iter()
        .map(|member| member.machine)
        .collect::<Vec<_>>();
    drop(machines);
}

impl EvaluationWorkCoordinator {
    pub(crate) fn poll_runtime_work(self: &Arc<Self>) -> bool {
        match self.select_runtime_pump() {
            coordinator::CoordinatorSelection::ClientDemand(claimed) => {
                self.poll_claimed_client_demand(claimed);
                true
            }
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

    pub(super) fn poll_claimed_task(self: &Arc<Self>, work: ClaimedTaskWork) {
        let mut claimed = ClaimedTask::new(self.clone(), work);
        let poll = claimed.poll(TASK_POLL_QUANTUM);
        let (_, _, released) = claimed.release(poll);
        if let Some(machine) = released {
            machine.finish();
        }
    }

    #[cfg(test)]
    pub(super) fn poll_claimed_task_with_probe(
        self: &Arc<Self>,
        work: ClaimedTaskWork,
        probe: impl FnOnce(&EvaluationMachinePoll),
    ) {
        let mut claimed = ClaimedTask::new(self.clone(), work);
        let poll = claimed.poll(TASK_POLL_QUANTUM);
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
        let poll = claimed.poll(&context);
        self.release_client_demand(claimed, poll);
    }

    pub(super) fn poll_claimed_spark(self: &Arc<Self>, claimed: coordinator::ClaimedSparkWork) {
        claimed.assert_runtime(self.runtime_id());
        let poll_context = EvaluationPollContext::for_claim(claimed.demand());
        let context = EvalContext::for_spark(claimed.demand_session());
        let evaluator = poll_context.evaluator(&context);
        let result = crate::eval::demand_strategy_value_in(&evaluator, claimed.value().as_core());
        let poll = match result {
            Ok(()) => coordinator::SparkWorkPoll::Complete,
            Err(halt) => {
                if let Some(wait) = halt.blocked_on() {
                    coordinator::SparkWorkPoll::Blocked(WorkDependency::Wait(wait.0))
                } else if let Some(promise) = halt.unassigned_promise() {
                    coordinator::SparkWorkPoll::Blocked(WorkDependency::Promise(promise.clone()))
                } else {
                    coordinator::SparkWorkPoll::Complete
                }
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
        coordinator.session_machine_is_busy(self.id)
    }
}
