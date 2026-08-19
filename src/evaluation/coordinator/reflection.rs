//! Reflection-task payloads, indexes, and lifecycle-local transitions.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::runtime::{EvaluationRuntimeId, RuntimeMutationAuthority};

use super::super::{EvaluationDemandState, EvaluationFailure, EvaluationTaskBlock};
use super::deferred::promote_deferred_wait_locked;
use super::{
    EvaluationExitBlock, EvaluationSessionId, EvaluationTaskId, EvaluationTaskMachine,
    EvaluationTaskStatus, EvaluationWaitToken, EvaluationWorkCoordinator, EvaluationWorkId,
    ExitIntent, ObservationRegistration, ProducerSettlementObligation, RuntimeFailureLedger,
    SettlementObligations, TaskFailureLedger, TaskStatusPublisher, WakeRegistration,
    WorkCloseReason, WorkControl, WorkCoordinatorState, WorkDependency, WorkKind, WorkRecord,
    WorkState, demand_session_is_closed, prune_closed_session_registration,
    publish_task_block_locked, queue_task, remove_ready_task,
};

impl EvaluationWorkCoordinator {
    #[cfg(test)]
    pub(in crate::evaluation) fn task_failure_is_acknowledged(
        &self,
        task: EvaluationTaskId,
    ) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(work) = state.reflection.by_task.get(&task) else {
            return false;
        };
        state
            .work
            .get(work)
            .is_some_and(|record| reflection_work(record).failure_reporting.acknowledged)
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn task_has_status_publisher(&self, task: EvaluationTaskId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(work) = state.reflection.by_task.get(&task) else {
            return false;
        };
        state.work.get(work).is_some_and(|record| {
            record
                .obligations
                .producer
                .as_ref()
                .and_then(|producer| match producer {
                    ProducerSettlementObligation::ReflectionTask(publisher) => {
                        publisher.protected_status.as_ref()
                    }
                    ProducerSettlementObligation::DeferredClaim { .. } => None,
                })
                .is_some()
        })
    }

    /// Acknowledges a task failure in its immutable producer-owner bucket.
    ///
    /// If the task is still active, this also records the timing-independent
    /// policy which prevents a later failure from entering the ledger. If it
    /// has already failed and retired, removing the persistent entry is
    /// sufficient. Both paths share the coordinator transition with terminal
    /// failure publication, so acknowledgement cannot race between the policy
    /// check and ledger insertion.
    pub(in crate::evaluation) fn acknowledge_task_failure(
        &self,
        owner: EvaluationSessionId,
        task: EvaluationTaskId,
    ) {
        let mutation = self.admission.mutation_guard();
        let changed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let mut changed = false;
            if let Some(work) = state.reflection.by_task.get(&task).copied()
                && let Some(record) = state.work.get_mut(&work)
            {
                let reflection = reflection_work_mut(record);
                if reflection.failure_reporting.owner_session == owner
                    && !reflection.failure_reporting.acknowledged
                {
                    reflection.failure_reporting.acknowledged = true;
                    changed = true;
                }
            }
            changed |= remove_task_failure(&mut state.failures, owner, task);
            remove_task_failure(&mut state.pending_failure_reports, owner, task);
            if changed {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            changed
        };
        drop(mutation);
        if changed {
            self.work_available.notify_all();
        }
    }

    /// Attaches the one protected-query publisher created by a committed
    /// public `.task.new` operation.
    pub(in crate::evaluation) fn attach_reflection_status_publisher(
        &self,
        work: EvaluationWorkId,
        publisher: TaskStatusPublisher,
    ) -> bool {
        let _mutation = self.admission.mutation_guard();
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(record) = state.work.get_mut(&work) else {
            return false;
        };
        record
            .obligations
            .task_publisher_mut()
            .expect("status publishers belong only to reflection tasks")
            .attach_status(publisher);
        true
    }

    /// Attaches one host-owned lifecycle publisher without consuming the
    /// protected `.task.status` publication slot.
    pub(in crate::evaluation) fn attach_reflection_lifecycle_publisher(
        &self,
        work: EvaluationWorkId,
        publisher: TaskStatusPublisher,
    ) -> bool {
        let _mutation = self.admission.mutation_guard();
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(record) = state.work.get_mut(&work) else {
            return false;
        };
        record
            .obligations
            .task_publisher_mut()
            .expect("lifecycle publishers belong only to reflection tasks")
            .attach_lifecycle(publisher);
        true
    }

    pub(in crate::evaluation) fn reserve_reflection(
        &self,
        session: &EvaluationDemandState,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
    ) -> Result<EvaluationWorkId, Arc<str>> {
        self.insert_reflection(session, task, wait, WorkState::Reserved)
    }

    pub(in crate::evaluation) fn register_dormant_reflection(
        &self,
        session: &EvaluationDemandState,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
    ) -> Result<EvaluationWorkId, Arc<str>> {
        self.insert_reflection(session, task, wait, WorkState::Dormant)
    }

    fn insert_reflection(
        &self,
        session: &EvaluationDemandState,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
        initial: WorkState,
    ) -> Result<EvaluationWorkId, Arc<str>> {
        debug_assert_eq!(session.values.runtime_id(), self.runtime);
        debug_assert!(matches!(initial, WorkState::Dormant | WorkState::Reserved));
        let id = EvaluationWorkId(self.ids.evaluation_work());
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if demand_session_is_closed(&state, session.id) {
                return Err(Arc::from("evaluation demand session is closed"));
            }
            let record = WorkRecord {
                id,
                demand_session: session.id,
                subscription_epoch: 0,
                control: WorkControl::default(),
                obligations: SettlementObligations::reflection_task(wait.clone()),
                state: initial,
                kind: WorkKind::Reflection(ReflectionWork {
                    task,
                    failure_reporting: TaskFailureReporting {
                        owner_session: session.id,
                        acknowledged: false,
                    },
                    wait: wait.clone(),
                    machine: None,
                    block: None,
                    exit: None,
                }),
            };
            assert!(state.work.insert(id, record).is_none());
            assert!(state.reflection.by_task.insert(task, id).is_none());
            assert!(state.reflection.by_wait.insert(wait, id).is_none());
            state
                .work_by_session
                .entry(session.id)
                .or_default()
                .insert(id);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_all();
        Ok(id)
    }

    /// Installs a fully constructed machine into its reserved reflection work
    /// record. A concurrent cancellation or owner close may already have
    /// terminalized the reservation; in that case the unused machine is
    /// returned for destruction after runtime locks are released.
    pub(in crate::evaluation) fn install_reflection_machine(
        &self,
        id: EvaluationWorkId,
        machine: Box<dyn EvaluationTaskMachine>,
    ) -> Result<(), Box<dyn EvaluationTaskMachine>> {
        let mutation = self.admission.mutation_guard();
        let mut machine = Some(machine);
        let installed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if let Some(record) = state
                .work
                .get_mut(&id)
                .filter(|record| matches!(record.state, WorkState::Reserved))
            {
                let reflection = reflection_work_mut(record);
                assert!(
                    reflection.machine.is_none(),
                    "reserved reflection work cannot own two machines"
                );
                reflection.machine = machine.take();
                true
            } else {
                false
            }
        };
        drop(mutation);
        if installed {
            Ok(())
        } else {
            Err(machine.expect("uninstalled reflection machine must remain owned"))
        }
    }

    pub(in crate::evaluation) fn activate_reflection(&self, id: EvaluationWorkId) -> bool {
        let mutation = self.admission.mutation_guard();
        let activated = self.activate_reflection_guarded(id, &mutation);
        drop(mutation);
        self.notify_reflection_activation(activated);
        activated
    }

    pub(in crate::evaluation) fn activate_reflection_guarded(
        &self,
        id: EvaluationWorkId,
        _mutation: &dyn RuntimeMutationAuthority,
    ) -> bool {
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get_mut(&id) else {
                return false;
            };
            if !matches!(record.kind, WorkKind::Reflection(_))
                || !matches!(record.state, WorkState::Reserved)
                || reflection_work(record).machine.is_none()
            {
                return false;
            }
            record.state = WorkState::Queued;
            queue_reflection(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
            true
        }
    }

    pub(in crate::evaluation) fn notify_reflection_activation(&self, activated: bool) {
        if activated {
            self.work_available.notify_all();
        }
    }

    pub(in crate::evaluation) fn terminalize_reserved_reflection(
        &self,
        id: EvaluationWorkId,
    ) -> bool {
        self.begin_reflection_terminalization(id, Some(WorkState::Reserved))
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn terminalize_reflection(&self, id: EvaluationWorkId) -> bool {
        self.begin_reflection_terminalization(id, None)
    }

    pub(in crate::evaluation) fn discard_reserved_reflection(&self, id: EvaluationWorkId) -> bool {
        let mutation = self.admission.mutation_guard();
        let discarded = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let reserved = state.work.get(&id).is_some_and(|record| {
                matches!(record.kind, WorkKind::Reflection(_))
                    && matches!(record.state, WorkState::Reserved)
            });
            if !reserved {
                false
            } else {
                assert!(
                    detach_reflection(&mut state, id, false).is_none(),
                    "an uncommitted reflection reservation cannot own a machine"
                );
                state.work_generation = state.work_generation.wrapping_add(1);
                true
            }
        };
        drop(mutation);
        if discarded {
            self.work_available.notify_all();
        }
        discarded
    }

    fn begin_reflection_terminalization(
        &self,
        id: EvaluationWorkId,
        required: Option<WorkState>,
    ) -> bool {
        let mutation = self.admission.mutation_guard();
        let terminalizing = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get_mut(&id) else {
                return false;
            };
            if !matches!(record.kind, WorkKind::Reflection(_))
                || required
                    .as_ref()
                    .is_some_and(|required| record.state != *required)
            {
                return false;
            }
            assert!(
                !matches!(record.state, WorkState::Running),
                "running reflection work requires a release transition"
            );
            record.state = WorkState::Terminalizing;
            state.observation_waiters.remove(&id);
            remove_ready_reflection(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
            true
        };
        drop(mutation);
        if terminalizing {
            self.work_available.notify_all();
        }
        terminalizing
    }

    pub(in crate::evaluation) fn request_reflection_cancellation(
        &self,
        id: EvaluationWorkId,
    ) -> ReflectionCancellation {
        let mutation = self.admission.mutation_guard();
        let outcome = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get_mut(&id) else {
                return ReflectionCancellation::Late;
            };
            if !matches!(record.kind, WorkKind::Reflection(_)) {
                return ReflectionCancellation::Late;
            }
            let outcome = match record.state {
                WorkState::Running => {
                    record
                        .control
                        .close_reason
                        .get_or_insert(WorkCloseReason::ExplicitCancellation);
                    ReflectionCancellation::Requested
                }
                WorkState::Dormant
                | WorkState::Reserved
                | WorkState::Queued
                | WorkState::Blocked
                | WorkState::ExitWaiting => {
                    record
                        .control
                        .close_reason
                        .get_or_insert(WorkCloseReason::ExplicitCancellation);
                    record.state = WorkState::Terminalizing;
                    state.observation_waiters.remove(&id);
                    remove_ready_reflection(&mut state, id);
                    ReflectionCancellation::Terminalize
                }
                WorkState::Terminalizing => ReflectionCancellation::Late,
            };
            if !matches!(outcome, ReflectionCancellation::Late) {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            outcome
        };
        drop(mutation);
        if !matches!(outcome, ReflectionCancellation::Late) {
            self.work_available.notify_all();
        }
        outcome
    }

    pub(in crate::evaluation) fn release_reflection(
        &self,
        claimed: ClaimedReflectionWork,
        poll: ReflectionWorkPoll,
    ) -> ReflectionWorkRelease {
        let ClaimedReflectionWork {
            id,
            task,
            demand_session,
            prior_block,
            mut machine,
        } = claimed;
        let mutation = self.admission.mutation_guard();
        let (mut release, exact_subscription) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let (cancel, abandoned) = {
                let record = state
                    .work
                    .get(&id)
                    .expect("claimed reflection work must remain registered");
                assert_eq!(record.demand_session, demand_session);
                assert_eq!(reflection_work(record).task, task);
                assert!(matches!(record.state, WorkState::Running));
                assert!(
                    reflection_work(record).machine.is_none(),
                    "running reflection work must not retain its claimed machine"
                );
                (
                    matches!(
                        record.control.close_reason,
                        Some(WorkCloseReason::ExplicitCancellation)
                    ),
                    matches!(
                        record.control.close_reason,
                        Some(WorkCloseReason::DemandSessionClosed)
                    ),
                )
            };
            let (state_after, block, exit, made_progress, remains_blocked, terminal) =
                if cancel || abandoned {
                    (WorkState::Terminalizing, None, None, true, false, true)
                } else {
                    match poll {
                        ReflectionWorkPoll::Yielded => {
                            (WorkState::Queued, None, None, true, false, false)
                        }
                        ReflectionWorkPoll::Blocked(block) => {
                            let unchanged = prior_block.as_ref() == Some(&block);
                            (
                                WorkState::Blocked,
                                Some(block),
                                None,
                                !unchanged,
                                true,
                                false,
                            )
                        }
                        ReflectionWorkPoll::Exit(exit) => {
                            (WorkState::ExitWaiting, None, Some(exit), true, true, false)
                        }
                        ReflectionWorkPoll::Terminal => {
                            (WorkState::Terminalizing, None, None, true, false, true)
                        }
                    }
                };
            let exit_waiting = exit.is_some();
            let retain_machine = !terminal
                && exit
                    .as_ref()
                    .is_none_or(|exit| exit.observed_epoch.is_some());
            if retain_machine {
                let record = state
                    .work
                    .get_mut(&id)
                    .expect("released reflection work must remain registered");
                let reflection = reflection_work_mut(record);
                assert!(reflection.machine.is_none());
                reflection.machine = machine.take();
            }
            let exact_subscription = if let Some(block) = block {
                assert!(matches!(state_after, WorkState::Blocked));
                publish_task_block_locked(&mut state, self.runtime, id, block)
            } else if let Some(exit) = exit {
                assert!(matches!(state_after, WorkState::ExitWaiting));
                publish_reflection_exit_locked(&mut state, self.runtime, id, exit);
                None
            } else {
                let record = state
                    .work
                    .get_mut(&id)
                    .expect("claimed reflection work must remain registered");
                let reflection = reflection_work_mut(record);
                reflection.block = None;
                reflection.exit = None;
                record.state = state_after;
                state.observation_waiters.remove(&id);
                None
            };
            if matches!(state_after, WorkState::Queued) {
                queue_reflection(&mut state, id);
            }
            if matches!(state_after, WorkState::Blocked)
                && let Some(wait) = reflection_work(
                    state
                        .work
                        .get(&id)
                        .expect("blocked reflection work must remain registered"),
                )
                .block
                .as_ref()
                .and_then(|block| block.dependency.as_ref())
                .and_then(WorkDependency::producer_wait)
                .cloned()
            {
                promote_deferred_wait_locked(&mut state, &wait);
            }
            state.work_generation = state.work_generation.wrapping_add(1);
            (
                ReflectionWorkRelease {
                    made_progress,
                    remains_blocked,
                    exit_waiting,
                    terminal,
                    cancel,
                    abandoned,
                    machine: None,
                },
                exact_subscription,
            )
        };
        if release.remains_blocked
            && exact_subscription.is_some_and(|(dependency, registration)| {
                self.subscribe_dependency_guarded(&mutation, dependency, registration)
            })
        {
            release.made_progress = true;
            release.remains_blocked = false;
        }
        if release.remains_blocked && self.recheck_observation_wait(id) {
            release.made_progress = true;
            release.remains_blocked = false;
        }
        let status_wakes = if !release.terminal && !release.exit_waiting {
            let status = if release.remains_blocked {
                EvaluationTaskStatus::Blocked
            } else {
                EvaluationTaskStatus::Launched
            };
            let updates = {
                let mut state = self
                    .state
                    .lock()
                    .expect("evaluation work coordinator was poisoned");
                let record = state
                    .work
                    .get_mut(&id)
                    .expect("released reflection work must remain registered");
                record
                    .obligations
                    .task_publisher_mut()
                    .expect("active reflection work must retain its terminal publisher")
                    .update_status(status, false)
            };
            updates
                .into_iter()
                .map(|(publisher, status)| publisher.publish_guarded(&mutation, status))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        drop(mutation);
        self.work_available.notify_all();
        for wake in status_wakes {
            wake.notify();
        }
        release.machine = machine;
        release
    }

    pub(in crate::evaluation) fn retire_reflection(
        &self,
        id: EvaluationWorkId,
    ) -> Option<Box<dyn EvaluationTaskMachine>> {
        let mutation = self.admission.mutation_guard();
        let machine = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get(&id)
                .expect("terminal reflection work must remain registered");
            assert!(matches!(record.state, WorkState::Terminalizing));
            let machine = detach_reflection(&mut state, id, true);
            state.work_generation = state.work_generation.wrapping_add(1);
            machine
        };
        drop(mutation);
        self.work_available.notify_all();
        machine
    }

    pub(in crate::evaluation) fn reflection_snapshots(
        &self,
        session: EvaluationSessionId,
    ) -> Vec<ReflectionWorkSnapshot> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state
            .reflection
            .by_task
            .iter()
            .filter_map(|(task, id)| {
                let record = state.work.get(id)?;
                (record.demand_session == session).then(|| ReflectionWorkSnapshot {
                    task: *task,
                    state: reflection_state(record),
                })
            })
            .collect()
    }
}

pub(super) struct TaskFailureReporting {
    pub(super) owner_session: EvaluationSessionId,
    pub(super) acknowledged: bool,
}

pub(super) struct ReflectionWork {
    pub(super) task: EvaluationTaskId,
    pub(super) failure_reporting: TaskFailureReporting,
    pub(super) wait: EvaluationWaitToken,
    pub(super) machine: Option<Box<dyn EvaluationTaskMachine>>,
    pub(super) block: Option<EvaluationTaskBlock>,
    pub(super) exit: Option<EvaluationExitBlock>,
}

#[derive(Default)]
pub(super) struct ReflectionIndexes {
    pub(super) by_task: BTreeMap<EvaluationTaskId, EvaluationWorkId>,
    pub(super) by_wait: HashMap<EvaluationWaitToken, EvaluationWorkId>,
}

pub(in crate::evaluation) struct ClaimedReflectionWork {
    pub(super) id: EvaluationWorkId,
    pub(super) task: EvaluationTaskId,
    pub(super) demand_session: EvaluationSessionId,
    pub(super) prior_block: Option<EvaluationTaskBlock>,
    pub(super) machine: Option<Box<dyn EvaluationTaskMachine>>,
}

impl ClaimedReflectionWork {
    pub(in crate::evaluation) fn id(&self) -> EvaluationWorkId {
        self.id
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn task(&self) -> EvaluationTaskId {
        self.task
    }

    pub(in crate::evaluation) fn poll(
        &mut self,
        step_budget: usize,
    ) -> super::EvaluationMachinePoll {
        self.machine
            .as_mut()
            .expect("claimed reflection work must retain its detached machine")
            .poll(step_budget)
    }
}

pub(in crate::evaluation) enum ReflectionWorkPoll {
    Yielded,
    Blocked(EvaluationTaskBlock),
    Exit(EvaluationExitBlock),
    Terminal,
}

pub(in crate::evaluation) struct ReflectionWorkRelease {
    pub(in crate::evaluation) made_progress: bool,
    pub(in crate::evaluation) remains_blocked: bool,
    pub(in crate::evaluation) exit_waiting: bool,
    pub(in crate::evaluation) terminal: bool,
    pub(in crate::evaluation) cancel: bool,
    pub(in crate::evaluation) abandoned: bool,
    pub(in crate::evaluation) machine: Option<Box<dyn EvaluationTaskMachine>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::evaluation) enum ReflectionWorkState {
    Dormant,
    Reserved,
    Queued,
    Running,
    Blocked(EvaluationTaskBlock),
    ExitWaiting(EvaluationExitBlock),
    Terminalizing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::evaluation) struct ReflectionWorkSnapshot {
    pub(in crate::evaluation) task: EvaluationTaskId,
    pub(in crate::evaluation) state: ReflectionWorkState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::evaluation) enum ReflectionCancellation {
    Requested,
    Terminalize,
    Late,
}

pub(in crate::evaluation) struct AbandonedReflectionWork {
    pub(in crate::evaluation) id: EvaluationWorkId,
    pub(in crate::evaluation) task: EvaluationTaskId,
    pub(in crate::evaluation) cancel: bool,
}

pub(super) fn reflection_work(record: &WorkRecord) -> &ReflectionWork {
    match &record.kind {
        WorkKind::Reflection(work) => work,
        WorkKind::Spark(_) => panic!("spark work cannot be used as a reflection task"),
        WorkKind::Deferred(_) => panic!("deferred work cannot be used as a reflection task"),
        WorkKind::ClientDemand(_) => {
            panic!("client demand cannot be used as a reflection task")
        }
    }
}

pub(super) fn reflection_work_mut(record: &mut WorkRecord) -> &mut ReflectionWork {
    match &mut record.kind {
        WorkKind::Reflection(work) => work,
        WorkKind::Spark(_) => panic!("spark work cannot be used as a reflection task"),
        WorkKind::Deferred(_) => panic!("deferred work cannot be used as a reflection task"),
        WorkKind::ClientDemand(_) => {
            panic!("client demand cannot be used as a reflection task")
        }
    }
}

pub(super) fn publish_reflection_exit_locked(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    id: EvaluationWorkId,
    exit: EvaluationExitBlock,
) {
    if let ExitIntent::Error(error) = &exit.intent {
        debug_assert_eq!(
            error.runtime_id(),
            runtime,
            "exit error values must belong to the coordinator runtime"
        );
    }
    state.observation_waiters.remove(&id);
    let observed_epoch = exit.observed_epoch;
    let record = state
        .work
        .get_mut(&id)
        .expect("exiting reflection work must remain registered");
    assert!(matches!(record.state, WorkState::Running));
    record.subscription_epoch = record
        .subscription_epoch
        .checked_add(1)
        .expect("evaluation work subscription epochs exhausted");
    let registration = WakeRegistration {
        work: id,
        subscription_epoch: record.subscription_epoch,
    };
    let reflection = reflection_work_mut(record);
    reflection.block = None;
    reflection.exit = Some(exit);
    assert_eq!(
        reflection.machine.is_some(),
        observed_epoch.is_some(),
        "only retryable exit waits retain their sanitized machine"
    );
    record.state = WorkState::ExitWaiting;
    if let Some(observed_epoch) = observed_epoch {
        state.observation_waiters.insert(
            id,
            ObservationRegistration {
                wake: registration,
                observed_epoch,
            },
        );
    }
}

pub(super) fn queue_reflection(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    assert!(matches!(
        state
            .work
            .get(&id)
            .expect("queued reflection work must remain registered")
            .kind,
        WorkKind::Reflection(_)
    ));
    queue_task(state, id);
}

pub(super) fn remove_ready_reflection(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    remove_ready_task(state, id);
}

pub(super) fn claim_reflection(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
) -> Option<ClaimedReflectionWork> {
    let (task, demand_session, prior_block, machine) = {
        let record = state.work.get_mut(&id)?;
        if !matches!(record.kind, WorkKind::Reflection(_))
            || !matches!(record.state, WorkState::Queued)
        {
            return None;
        }
        record.state = WorkState::Running;
        let demand_session = record.demand_session;
        let reflection = reflection_work_mut(record);
        (
            reflection.task,
            demand_session,
            reflection.block.take(),
            reflection
                .machine
                .take()
                .expect("claimable reflection work must retain its machine"),
        )
    };
    state.observation_waiters.remove(&id);
    remove_ready_reflection(state, id);
    Some(ClaimedReflectionWork {
        id,
        task,
        demand_session,
        prior_block,
        machine: Some(machine),
    })
}

pub(super) fn reflection_state(record: &WorkRecord) -> ReflectionWorkState {
    match record.state {
        WorkState::Dormant => ReflectionWorkState::Dormant,
        WorkState::Reserved => ReflectionWorkState::Reserved,
        WorkState::Queued => ReflectionWorkState::Queued,
        WorkState::Running => ReflectionWorkState::Running,
        WorkState::Blocked => ReflectionWorkState::Blocked(
            reflection_work(record)
                .block
                .clone()
                .expect("blocked reflection work must retain its block"),
        ),
        WorkState::ExitWaiting => ReflectionWorkState::ExitWaiting(
            reflection_work(record)
                .exit
                .clone()
                .expect("exit-waiting reflection work must retain its exit summary"),
        ),
        WorkState::Terminalizing => ReflectionWorkState::Terminalizing,
    }
}

pub(super) fn insert_task_failure(
    ledger: &mut RuntimeFailureLedger,
    owner: EvaluationSessionId,
    task: EvaluationTaskId,
    failure: Arc<EvaluationFailure>,
) {
    let mut failures = ledger
        .get(&owner)
        .cloned()
        .unwrap_or_else(TaskFailureLedger::new_sync);
    assert!(
        !failures.contains_key(&task),
        "a task failure may enter its owner ledger only once"
    );
    failures.insert_mut(task, failure);
    ledger.insert_mut(owner, failures);
}

pub(super) fn remove_task_failure(
    ledger: &mut RuntimeFailureLedger,
    owner: EvaluationSessionId,
    task: EvaluationTaskId,
) -> bool {
    let Some(mut failures) = ledger.get(&owner).cloned() else {
        return false;
    };
    if !failures.remove_mut(&task) {
        return false;
    }
    if failures.is_empty() {
        ledger.remove_mut(&owner);
    } else {
        ledger.insert_mut(owner, failures);
    }
    true
}

pub(super) fn detach_reflection(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
    require_settled: bool,
) -> Option<Box<dyn EvaluationTaskMachine>> {
    state.observation_waiters.remove(&id);
    remove_ready_reflection(state, id);
    let mut record = state
        .work
        .remove(&id)
        .expect("retired reflection work must remain registered");
    let discarded = if require_settled {
        assert!(
            record.obligations.is_empty(),
            "reflection work cannot retire before terminal settlement"
        );
        None
    } else {
        assert!(
            record.obligations.owned_promises.is_empty(),
            "an uncommitted reflection reservation cannot own promises"
        );
        record.obligations.take_producer()
    };
    let WorkKind::Reflection(reflection) = record.kind else {
        panic!("reflection retirement must contain reflection work")
    };
    if let Some(obligation) = discarded {
        let ProducerSettlementObligation::ReflectionTask(publisher) = obligation else {
            panic!("reflection work must retain a reflection task-wait obligation")
        };
        assert_eq!(publisher.wait, reflection.wait);
    }
    assert_eq!(
        state.reflection.by_task.remove(&reflection.task),
        Some(id),
        "reflection task index must agree with its work record"
    );
    assert_eq!(
        state.reflection.by_wait.remove(&reflection.wait),
        Some(id),
        "reflection wait index must agree with its work record"
    );
    if let Some(session_work) = state.work_by_session.get_mut(&record.demand_session) {
        session_work.remove(&id);
        if session_work.is_empty() {
            state.work_by_session.remove(&record.demand_session);
        }
    }
    prune_closed_session_registration(state, record.demand_session);
    reflection.machine
}
