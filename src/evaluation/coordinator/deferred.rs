//! Deferred producer claims and pure-lazy cycle handling.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::core::{DeferredValueId, LazyValue, PromisedValue};

use super::super::{EvaluationDemandState, EvaluationTaskBlock};
use super::{
    EvaluationSessionId, EvaluationTaskId, EvaluationTaskMachine, EvaluationWaitToken,
    EvaluationWorkCoordinator, EvaluationWorkId, SettlementObligations, WorkCloseReason,
    WorkControl, WorkCoordinatorState, WorkDependency, WorkKind, WorkRecord, WorkState,
    demand_session_is_closed, prune_closed_session_registration, publish_task_block_locked,
    queue_task, remove_ready_task,
};

impl EvaluationWorkCoordinator {
    pub(in crate::evaluation) fn reserve_deferred(
        &self,
        session: &EvaluationDemandState,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
        producer: DeferredProducer,
        machine: Box<dyn EvaluationTaskMachine>,
    ) -> Result<DeferredWorkReservation, Arc<str>> {
        debug_assert_eq!(session.values.runtime_id(), self.runtime);
        debug_assert_eq!(wait.runtime_id(), self.runtime);
        let deferred = producer.id();
        let mutation = self.admission.mutation_guard();
        let mut machine = Some(machine);
        let reservation = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if demand_session_is_closed(&state, session.id) {
                return Err(Arc::from("evaluation demand session is closed"));
            }
            if let Some(id) = state.deferred.by_value.get(&deferred).copied() {
                let wait = deferred_work(
                    state
                        .work
                        .get(&id)
                        .expect("indexed deferred work must remain registered"),
                )
                .wait
                .clone();
                DeferredWorkReservation::Existing(wait)
            } else {
                let id = EvaluationWorkId(self.ids.evaluation_work());
                let record = WorkRecord {
                    id,
                    demand_session: session.id,
                    subscription_epoch: 0,
                    control: WorkControl::default(),
                    obligations: SettlementObligations::deferred_claim(
                        wait.clone(),
                        producer.clone(),
                    ),
                    state: WorkState::Dormant,
                    kind: WorkKind::Deferred(DeferredWork {
                        task,
                        wait: wait.clone(),
                        producer,
                        machine: machine.take(),
                        block: None,
                    }),
                };
                assert!(state.work.insert(id, record).is_none());
                assert!(state.deferred.by_task.insert(task, id).is_none());
                assert!(state.deferred.by_wait.insert(wait, id).is_none());
                assert!(state.deferred.by_value.insert(deferred, id).is_none());
                state
                    .work_by_session
                    .entry(session.id)
                    .or_default()
                    .insert(id);
                state.work_generation = state.work_generation.wrapping_add(1);
                DeferredWorkReservation::New
            }
        };
        drop(mutation);
        // A racing producer may have installed the canonical machine while we
        // were constructing this candidate. Dispose the unused candidate only
        // after releasing coordinator state and mutation admission.
        drop(machine);
        if matches!(reservation, DeferredWorkReservation::New) {
            self.work_available.notify_all();
        }
        Ok(reservation)
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn deferred_work_for_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<EvaluationWorkId> {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .deferred
            .by_wait
            .get(wait)
            .copied()
    }

    pub(in crate::evaluation) fn deferred_wait(
        &self,
        producer: DeferredValueId,
    ) -> Option<EvaluationWaitToken> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let work = state.deferred.by_value.get(&producer)?;
        Some(
            deferred_work(
                state
                    .work
                    .get(work)
                    .expect("indexed deferred work must remain registered"),
            )
            .wait
            .clone(),
        )
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn promote_deferred_wait(&self, wait: &EvaluationWaitToken) -> bool {
        let mutation = self.admission.mutation_guard();
        let promoted = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(id) = state.deferred.by_wait.get(wait).copied() else {
                return false;
            };
            let next = state.work.get(&id).map(|record| record.state);
            match next {
                Some(WorkState::Dormant) => {
                    state
                        .work
                        .get_mut(&id)
                        .expect("dormant deferred work must remain registered")
                        .state = WorkState::Queued;
                    queue_deferred(&mut state, id);
                    state.work_generation = state.work_generation.wrapping_add(1);
                    true
                }
                _ => false,
            }
        };
        drop(mutation);
        if promoted {
            self.work_available.notify_all();
        }
        promoted
    }
}

impl EvaluationWorkCoordinator {
    pub(in crate::evaluation) fn release_deferred(
        &self,
        mut claimed: ClaimedDeferredWork,
        poll: DeferredWorkPoll,
    ) -> DeferredWorkRelease {
        let mut machine = Some(
            claimed
                .machine
                .take()
                .expect("released deferred claim must retain its detached machine"),
        );
        let mutation = self.admission.mutation_guard();
        let (mut release, exact_subscription) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            {
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("claimed deferred work must remain registered");
                assert_eq!(record.demand_session, claimed.demand_session);
                assert!(matches!(record.state, WorkState::Running));
                let deferred = deferred_work_mut(record);
                assert_eq!(deferred.task, claimed.task);
                assert_eq!(deferred.producer.id(), claimed.producer);
                assert!(
                    deferred.machine.is_none(),
                    "running deferred work must have detached its machine"
                );
                deferred.machine = machine.take();
            }

            let abandoned = state.work.get(&claimed.id).is_some_and(|record| {
                matches!(
                    record.control.close_reason,
                    Some(WorkCloseReason::DemandSessionClosed)
                )
            });
            let (state_after, block, made_progress, remains_blocked, terminal) = if abandoned {
                (WorkState::Terminalizing, None, true, false, true)
            } else {
                match poll {
                    DeferredWorkPoll::Yielded if claimed.requeue_on_yield => {
                        (WorkState::Queued, None, true, false, false)
                    }
                    DeferredWorkPoll::Yielded => (WorkState::Dormant, None, true, false, false),
                    DeferredWorkPoll::Blocked(block) => {
                        let unchanged = claimed.prior_block.as_ref() == Some(&block);
                        (WorkState::Blocked, Some(block), !unchanged, true, false)
                    }
                    DeferredWorkPoll::Terminal => {
                        (WorkState::Terminalizing, None, true, false, true)
                    }
                }
            };
            let mut exact_subscription = if let Some(block) = block {
                assert!(matches!(state_after, WorkState::Blocked));
                publish_task_block_locked(&mut state, self.runtime, claimed.id, block)
            } else {
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("claimed deferred work must remain registered");
                deferred_work_mut(record).block = None;
                record.state = state_after;
                state.observation_waiters.remove(&claimed.id);
                None
            };
            if matches!(state_after, WorkState::Queued) {
                queue_deferred(&mut state, claimed.id);
            }

            if matches!(state_after, WorkState::Blocked)
                && let Some(wait) = deferred_work(
                    state
                        .work
                        .get(&claimed.id)
                        .expect("blocked deferred work must remain registered"),
                )
                .block
                .as_ref()
                .and_then(|block| block.dependency.as_ref())
                .and_then(WorkDependency::producer_wait)
                .cloned()
            {
                promote_deferred_wait_locked(&mut state, &wait);
            }

            let cycle = if matches!(state_after, WorkState::Blocked) {
                terminalize_pure_lazy_cycle(&mut state, claimed.id)
            } else {
                Vec::new()
            };
            let cycle_terminal = !cycle.is_empty();
            if cycle_terminal {
                exact_subscription = None;
            }
            let machine = if terminal && !cycle_terminal {
                deferred_work_mut(
                    state
                        .work
                        .get_mut(&claimed.id)
                        .expect("terminal deferred work must remain registered"),
                )
                .machine
                .take()
            } else {
                None
            };
            state.work_generation = state.work_generation.wrapping_add(1);
            (
                DeferredWorkRelease {
                    made_progress: made_progress || cycle_terminal,
                    remains_blocked: remains_blocked && !cycle_terminal,
                    terminal: terminal || cycle_terminal,
                    abandoned,
                    cycle,
                    machine,
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
        if release.remains_blocked && self.recheck_observation_wait(claimed.id) {
            release.made_progress = true;
            release.remains_blocked = false;
        }
        drop(mutation);
        self.work_available.notify_all();
        release
    }

    pub(in crate::evaluation) fn retire_deferred(&self, id: EvaluationWorkId) {
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get(&id)
                .expect("terminal deferred work must remain registered");
            assert!(matches!(record.state, WorkState::Terminalizing));
            detach_deferred(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_all();
    }

    pub(in crate::evaluation) fn abandon_deferred_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<AbandonedDeferredWork> {
        let mutation = self.admission.mutation_guard();
        let abandoned = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let id = state.deferred.by_wait.get(wait).copied()?;
            if state.work.get(&id).is_some_and(|record| {
                matches!(record.state, WorkState::Running | WorkState::Terminalizing)
            }) {
                // A running claim still owns the machine. A terminalizing
                // release already took it and owns settlement/retirement.
                return None;
            }
            let abandoned = begin_deferred_abandonment(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
            abandoned
        };
        drop(mutation);
        self.work_available.notify_all();
        Some(abandoned)
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn deferred_counts(
        &self,
        session: EvaluationSessionId,
    ) -> (usize, usize, usize) {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let active = state
            .deferred
            .by_value
            .values()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count();
        let waits = state
            .deferred
            .by_wait
            .values()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count();
        let tasks = state
            .deferred
            .by_task
            .values()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count();
        (active, waits, tasks)
    }
}

#[derive(Clone)]
pub(in crate::evaluation) enum DeferredProducer {
    Lazy(LazyValue),
    Promise(PromisedValue),
}

impl DeferredProducer {
    pub(in crate::evaluation) fn id(&self) -> DeferredValueId {
        match self {
            Self::Lazy(lazy) => lazy.id().into(),
            Self::Promise(promise) => promise.id().into(),
        }
    }
}

pub(super) struct DeferredWork {
    pub(super) task: EvaluationTaskId,
    pub(super) wait: EvaluationWaitToken,
    pub(super) producer: DeferredProducer,
    pub(super) machine: Option<Box<dyn EvaluationTaskMachine>>,
    pub(super) block: Option<EvaluationTaskBlock>,
}

#[derive(Default)]
pub(super) struct DeferredIndexes {
    pub(super) by_task: BTreeMap<EvaluationTaskId, EvaluationWorkId>,
    pub(super) by_wait: HashMap<EvaluationWaitToken, EvaluationWorkId>,
    pub(super) by_value: HashMap<DeferredValueId, EvaluationWorkId>,
}

pub(in crate::evaluation) struct ClaimedDeferredWork {
    pub(super) id: EvaluationWorkId,
    pub(super) task: EvaluationTaskId,
    pub(super) demand_session: EvaluationSessionId,
    pub(super) producer: DeferredValueId,
    pub(super) prior_block: Option<EvaluationTaskBlock>,
    pub(super) requeue_on_yield: bool,
    pub(super) machine: Option<Box<dyn EvaluationTaskMachine>>,
}

impl ClaimedDeferredWork {
    pub(in crate::evaluation) fn id(&self) -> EvaluationWorkId {
        self.id
    }

    pub(in crate::evaluation) fn poll(
        &mut self,
        step_budget: usize,
    ) -> super::EvaluationMachinePoll {
        self.machine
            .as_mut()
            .expect("claimed deferred work must retain its detached machine")
            .poll(step_budget)
    }
}

pub(in crate::evaluation) enum DeferredWorkPoll {
    Yielded,
    Blocked(EvaluationTaskBlock),
    Terminal,
}

pub(in crate::evaluation) struct DeferredLazyCycleMember {
    pub(in crate::evaluation) work: EvaluationWorkId,
    pub(in crate::evaluation) wait: EvaluationWaitToken,
    pub(in crate::evaluation) lazy: LazyValue,
    pub(in crate::evaluation) machine: Box<dyn EvaluationTaskMachine>,
}

pub(in crate::evaluation) struct DeferredWorkRelease {
    pub(in crate::evaluation) made_progress: bool,
    pub(in crate::evaluation) remains_blocked: bool,
    pub(in crate::evaluation) terminal: bool,
    pub(in crate::evaluation) abandoned: bool,
    pub(in crate::evaluation) cycle: Vec<DeferredLazyCycleMember>,
    pub(in crate::evaluation) machine: Option<Box<dyn EvaluationTaskMachine>>,
}

pub(in crate::evaluation) enum DeferredWorkReservation {
    New,
    Existing(EvaluationWaitToken),
}

pub(in crate::evaluation) struct AbandonedDeferredWork {
    pub(in crate::evaluation) id: EvaluationWorkId,
    pub(in crate::evaluation) task: EvaluationTaskId,
    pub(in crate::evaluation) dependency: Option<EvaluationWaitToken>,
    pub(in crate::evaluation) machine: Box<dyn EvaluationTaskMachine>,
}

pub(super) fn deferred_work(record: &WorkRecord) -> &DeferredWork {
    match &record.kind {
        WorkKind::Deferred(work) => work,
        WorkKind::Spark(_) => panic!("spark work cannot be used as a deferred producer"),
        WorkKind::Reflection(_) => {
            panic!("reflection work cannot be used as a deferred producer")
        }
        WorkKind::ClientDemand(_) => {
            panic!("client demand cannot be used as a deferred producer")
        }
    }
}

pub(super) fn deferred_work_mut(record: &mut WorkRecord) -> &mut DeferredWork {
    match &mut record.kind {
        WorkKind::Deferred(work) => work,
        WorkKind::Spark(_) => panic!("spark work cannot be used as a deferred producer"),
        WorkKind::Reflection(_) => {
            panic!("reflection work cannot be used as a deferred producer")
        }
        WorkKind::ClientDemand(_) => {
            panic!("client demand cannot be used as a deferred producer")
        }
    }
}

pub(super) fn queue_deferred(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    assert!(matches!(
        state
            .work
            .get(&id)
            .expect("queued deferred work must remain registered")
            .kind,
        WorkKind::Deferred(_)
    ));
    queue_task(state, id);
}

pub(super) fn remove_ready_deferred(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    remove_ready_task(state, id);
}

pub(super) fn claim_deferred(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
    requeue_on_yield: bool,
) -> Option<ClaimedDeferredWork> {
    let (task, demand_session, producer, prior_block, machine) = {
        let record = state.work.get_mut(&id)?;
        if !matches!(record.kind, WorkKind::Deferred(_))
            || !matches!(record.state, WorkState::Dormant | WorkState::Queued)
        {
            return None;
        }
        record.state = WorkState::Running;
        let demand_session = record.demand_session;
        let deferred = deferred_work_mut(record);
        (
            deferred.task,
            demand_session,
            deferred.producer.id(),
            deferred.block.take(),
            deferred
                .machine
                .take()
                .expect("claimable deferred work must retain its machine"),
        )
    };
    state.observation_waiters.remove(&id);
    remove_ready_deferred(state, id);
    Some(ClaimedDeferredWork {
        id,
        task,
        demand_session,
        producer,
        prior_block,
        requeue_on_yield,
        machine: Some(machine),
    })
}

pub(super) fn promote_deferred_wait_locked(
    state: &mut WorkCoordinatorState,
    wait: &EvaluationWaitToken,
) -> bool {
    let Some(id) = state.deferred.by_wait.get(wait).copied() else {
        return false;
    };
    match state.work.get(&id).map(|record| record.state) {
        Some(WorkState::Dormant) => {
            state
                .work
                .get_mut(&id)
                .expect("dormant deferred work must remain registered")
                .state = WorkState::Queued;
            queue_deferred(state, id);
            true
        }
        _ => false,
    }
}

fn deferred_dependency_cycle(
    state: &WorkCoordinatorState,
    start: EvaluationWorkId,
) -> Option<DeferredDependencyCycle> {
    let mut path: Vec<EvaluationWorkId> = Vec::new();
    let mut promise_edges = Vec::new();
    let mut positions = HashMap::new();
    let mut current = start;
    loop {
        if let Some(first) = positions.insert(current, path.len()) {
            let mut cycle = path.split_off(first);
            let contains_promise = promise_edges.split_off(first).into_iter().any(|edge| edge);
            let canonical = cycle
                .iter()
                .enumerate()
                .min_by_key(|(_, work)| work.get())
                .map(|(position, _)| position)
                .expect("a repeated successor must produce a non-empty cycle");
            cycle.rotate_left(canonical);
            return Some(DeferredDependencyCycle {
                members: cycle,
                contains_promise,
            });
        }
        path.push(current);
        let record = state.work.get(&current)?;
        let dependency = deferred_work(record).block.as_ref()?.dependency.as_ref()?;
        promise_edges.push(matches!(dependency, WorkDependency::Promise(_)));
        let wait = dependency.producer_wait()?;
        current = *state.deferred.by_wait.get(wait)?;
    }
}

struct DeferredDependencyCycle {
    members: Vec<EvaluationWorkId>,
    contains_promise: bool,
}

pub(super) fn terminalize_pure_lazy_cycle(
    state: &mut WorkCoordinatorState,
    start: EvaluationWorkId,
) -> Vec<DeferredLazyCycleMember> {
    let Some(cycle) = deferred_dependency_cycle(state, start) else {
        return Vec::new();
    };
    if cycle.contains_promise {
        return Vec::new();
    }
    let pure_lazy = cycle.members.iter().all(|id| {
        state.work.get(id).is_some_and(|record| {
            matches!(record.state, WorkState::Blocked)
                && matches!(deferred_work(record).producer, DeferredProducer::Lazy(_))
        })
    });
    if !pure_lazy {
        return Vec::new();
    }

    let mut members = Vec::with_capacity(cycle.members.len());
    for id in cycle.members {
        let record = state
            .work
            .get_mut(&id)
            .expect("cycle member must remain registered");
        let deferred = deferred_work_mut(record);
        let DeferredProducer::Lazy(lazy) = &deferred.producer else {
            unreachable!("pure lazy cycle cannot contain a promise")
        };
        let member = DeferredLazyCycleMember {
            work: id,
            wait: deferred.wait.clone(),
            lazy: lazy.clone(),
            machine: deferred
                .machine
                .take()
                .expect("blocked lazy cycle member must retain its machine"),
        };
        deferred.block = None;
        record.state = WorkState::Terminalizing;
        state.observation_waiters.remove(&id);
        remove_ready_deferred(state, id);
        members.push(member);
    }
    members
}

pub(super) fn begin_deferred_abandonment(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
) -> AbandonedDeferredWork {
    let record = state
        .work
        .get_mut(&id)
        .expect("abandoned deferred work must remain registered");
    assert!(matches!(record.kind, WorkKind::Deferred(_)));
    assert!(!matches!(record.state, WorkState::Running));
    let deferred = deferred_work_mut(record);
    let abandoned = AbandonedDeferredWork {
        id,
        task: deferred.task,
        dependency: deferred
            .block
            .take()
            .and_then(|block| block.dependency)
            .and_then(WorkDependency::into_wait),
        machine: deferred
            .machine
            .take()
            .expect("abandoned deferred work must retain its machine"),
    };
    record.state = WorkState::Terminalizing;
    state.observation_waiters.remove(&id);
    remove_ready_deferred(state, id);
    abandoned
}

pub(super) fn detach_deferred(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    state.observation_waiters.remove(&id);
    remove_ready_deferred(state, id);
    let record = state
        .work
        .remove(&id)
        .expect("retired deferred work must remain registered");
    assert!(
        record.obligations.is_empty(),
        "deferred work cannot retire before terminal settlement"
    );
    let WorkKind::Deferred(deferred) = record.kind else {
        panic!("deferred retirement must contain deferred work")
    };
    assert!(
        deferred.machine.is_none(),
        "deferred work cannot retire before detaching its machine"
    );
    assert_eq!(state.deferred.by_task.remove(&deferred.task), Some(id));
    assert_eq!(state.deferred.by_wait.remove(&deferred.wait), Some(id));
    assert_eq!(
        state.deferred.by_value.remove(&deferred.producer.id()),
        Some(id)
    );
    if let Some(session_work) = state.work_by_session.get_mut(&record.demand_session) {
        session_work.remove(&id);
        if session_work.is_empty() {
            state.work_by_session.remove(&record.demand_session);
        }
    }
    prune_closed_session_registration(state, record.demand_session);
}
