//! Runtime-owned work coordination independent of worker ownership.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::num::NonZeroU64;
use std::sync::{Arc, Condvar, Mutex, Weak};

use crate::core::{PromisedValue, Value};
use crate::runtime::{EvaluationRuntimeId, RuntimeIds, RuntimeMutationAdmission, RuntimeValueRoot};

use super::{EvaluationSession, EvaluationSessionId, EvaluationWaitToken};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EvaluationWorkId(NonZeroU64);

impl EvaluationWorkId {
    #[cfg(test)]
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Default)]
struct WorkControl {
    cancel_requested: bool,
    close_reason: Option<WorkCloseReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkCloseReason {
    DemandSessionClosed,
    ExecutorShutdown,
}

/// Reserved now so later task migration does not need another work-record
/// shape. Sparks acquire no settlement obligations in this phase.
#[derive(Default)]
struct SettlementObligations;

enum WorkState {
    Queued,
    Running,
    Blocked,
    Terminalizing,
}

pub(super) enum WorkDependency {
    Wait(EvaluationWaitToken),
    Promise(PromisedValue),
}

impl WorkDependency {
    fn runtime_id(&self) -> EvaluationRuntimeId {
        match self {
            Self::Wait(wait) => wait.runtime_id(),
            Self::Promise(promise) => promise.runtime_id(),
        }
    }

    fn same_source(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Wait(left), Self::Wait(right)) => left == right,
            (Self::Promise(left), Self::Promise(right)) => left == right,
            _ => false,
        }
    }

    fn abandon(self) {
        if let Self::Wait(wait) = self
            && let Some(owner) = wait.owner()
        {
            owner.abandon_spark_wait(&wait);
        }
    }
}

struct SparkDemand {
    session: Weak<EvaluationSession>,
    value: RuntimeValueRoot,
}

struct WorkRecord {
    id: EvaluationWorkId,
    demand_session: EvaluationSessionId,
    observed_generation: u64,
    control: WorkControl,
    obligations: SettlementObligations,
    state: WorkState,
    demand: Option<SparkDemand>,
    dependency: Option<WorkDependency>,
}

pub(super) struct ClaimedSparkWork {
    id: EvaluationWorkId,
    demand_session: EvaluationSessionId,
    observed_generation: u64,
    demand: SparkDemand,
    prior_dependency: Option<WorkDependency>,
}

impl ClaimedSparkWork {
    pub(super) fn session(&self) -> Option<Arc<EvaluationSession>> {
        self.demand.session.upgrade()
    }

    pub(super) fn value(&self) -> &RuntimeValueRoot {
        &self.demand.value
    }
}

pub(super) enum SparkWorkPoll {
    Complete,
    Blocked(WorkDependency),
}

struct SparkRetirement {
    demand: SparkDemand,
    dependencies: Vec<WorkDependency>,
    _obligations: SettlementObligations,
}

impl SparkRetirement {
    fn abandon(self) {
        for dependency in self.dependencies {
            dependency.abandon();
        }
        drop(self.demand);
    }
}

#[derive(Default)]
struct WorkCoordinatorState {
    sessions: HashMap<EvaluationSessionId, Weak<EvaluationSession>>,
    ready_sessions: VecDeque<EvaluationSessionId>,
    ready_session_set: HashSet<EvaluationSessionId>,
    work: HashMap<EvaluationWorkId, WorkRecord>,
    work_by_session: HashMap<EvaluationSessionId, HashSet<EvaluationWorkId>>,
    ready_sparks: VecDeque<EvaluationWorkId>,
    ready_spark_set: HashSet<EvaluationWorkId>,
    blocked_sparks: HashMap<EvaluationSessionId, HashSet<EvaluationWorkId>>,
    spark_generations: HashMap<EvaluationSessionId, u64>,
    spark_workers: usize,
    prefer_spark: bool,
    work_generation: u64,
}

/// Runtime-owned scheduling state shared by serial and worker execution.
///
/// Spark payloads have stable work records here. Reflection and deferred task
/// records deliberately remain session-owned until their later migration.
pub(crate) struct EvaluationWorkCoordinator {
    runtime: EvaluationRuntimeId,
    ids: Arc<RuntimeIds>,
    admission: Arc<RuntimeMutationAdmission>,
    state: Mutex<WorkCoordinatorState>,
    work_available: Condvar,
}

pub(super) enum CoordinatorSelection {
    Reflection(Arc<EvaluationSession>),
    Spark(ClaimedSparkWork),
    None,
}

impl fmt::Debug for EvaluationWorkCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        formatter
            .debug_struct("EvaluationWorkCoordinator")
            .field("runtime", &self.runtime)
            .field("session_count", &state.sessions.len())
            .field("ready_session_count", &state.ready_session_set.len())
            .field("spark_work_count", &state.work.len())
            .field("work_generation", &state.work_generation)
            .finish_non_exhaustive()
    }
}

impl EvaluationWorkCoordinator {
    pub(crate) fn new(
        runtime: EvaluationRuntimeId,
        ids: Arc<RuntimeIds>,
        admission: Arc<RuntimeMutationAdmission>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            ids,
            admission,
            state: Mutex::new(WorkCoordinatorState::default()),
            work_available: Condvar::new(),
        })
    }

    pub(super) fn register_session(&self, session: &Arc<EvaluationSession>) {
        debug_assert_eq!(session.values.runtime_id(), self.runtime);
        self.publish_transition(|state| {
            let replaced = state.sessions.insert(session.id, Arc::downgrade(session));
            assert!(
                replaced.is_none(),
                "evaluation session identities must be unique within a runtime"
            );
        });
    }

    pub(super) fn unregister_session(&self, session: EvaluationSessionId) {
        let mutation = self.admission.mutation_guard();
        let (retired, changed) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let initial_generation = state.work_generation;
            state.sessions.remove(&session);
            state.ready_session_set.remove(&session);
            state
                .ready_sessions
                .retain(|candidate| *candidate != session);

            let work = state
                .work_by_session
                .get(&session)
                .into_iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            let mut retired = Vec::new();
            for id in work {
                let is_running = state
                    .work
                    .get(&id)
                    .is_some_and(|record| matches!(record.state, WorkState::Running));
                if is_running {
                    let record = state
                        .work
                        .get_mut(&id)
                        .expect("indexed running spark work must remain registered");
                    record.control.cancel_requested = true;
                    record.control.close_reason = Some(WorkCloseReason::DemandSessionClosed);
                } else if let Some(record) = detach_spark(&mut state, id) {
                    retired.push(record);
                }
            }
            state.spark_generations.remove(&session);
            state.blocked_sparks.remove(&session);
            state.work_generation = state.work_generation.wrapping_add(1);
            (retired, state.work_generation != initial_generation)
        };
        drop(mutation);
        if changed {
            self.work_available.notify_all();
        }
        for record in retired {
            record.abandon();
        }
    }

    pub(super) fn notify_session_ready(&self, session: EvaluationSessionId) {
        let mutation = self.admission.mutation_guard();
        let changed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if !state.sessions.contains_key(&session) || !state.ready_session_set.insert(session) {
                false
            } else {
                state.ready_sessions.push_back(session);
                state.work_generation = state.work_generation.wrapping_add(1);
                true
            }
        };
        drop(mutation);
        if changed {
            self.work_available.notify_one();
        }
    }

    pub(super) fn submit_spark(&self, session: &Arc<EvaluationSession>, value: Value) {
        debug_assert_eq!(session.values.runtime_id(), self.runtime);
        let id = EvaluationWorkId(self.ids.evaluation_work());
        let demand = SparkDemand {
            session: Arc::downgrade(session),
            value: RuntimeValueRoot::new(&session.values, value),
        };
        let mutation = self.admission.mutation_guard();
        let admitted = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if state.spark_workers == 0 || !state.sessions.contains_key(&session.id) {
                false
            } else {
                let session_id = session.id;
                let observed_generation = *state.spark_generations.entry(session_id).or_insert(0);
                let record = WorkRecord {
                    id,
                    demand_session: session_id,
                    observed_generation,
                    control: WorkControl::default(),
                    obligations: SettlementObligations,
                    state: WorkState::Queued,
                    demand: Some(demand),
                    dependency: None,
                };
                assert!(state.work.insert(id, record).is_none());
                state
                    .work_by_session
                    .entry(session_id)
                    .or_default()
                    .insert(id);
                queue_spark(&mut state, id);
                state.work_generation = state.work_generation.wrapping_add(1);
                true
            }
        };
        drop(mutation);
        if admitted {
            self.work_available.notify_one();
        }
    }

    /// Broadly retries sparks whose demand session observed task or promise
    /// progress. Precise dependency subscriptions replace this in Phase 4.
    pub(super) fn notify_spark_disturbance(&self, session: EvaluationSessionId) {
        let mutation = self.admission.mutation_guard();
        let changed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if !state.sessions.contains_key(&session) {
                false
            } else {
                let generation = state
                    .spark_generations
                    .entry(session)
                    .and_modify(|generation| *generation = generation.wrapping_add(1))
                    .or_insert(1);
                let generation = *generation;
                let blocked = state.blocked_sparks.remove(&session).unwrap_or_default();
                for id in blocked {
                    if let Some(record) = state.work.get_mut(&id)
                        && matches!(record.state, WorkState::Blocked)
                    {
                        record.state = WorkState::Queued;
                        record.observed_generation = generation;
                        queue_spark(&mut state, id);
                    }
                }
                state.work_generation = state.work_generation.wrapping_add(1);
                true
            }
        };
        drop(mutation);
        if changed {
            self.work_available.notify_all();
        }
    }

    pub(super) fn executor_started(&self, worker_count: usize) {
        self.publish_transition(|state| {
            state.spark_workers = worker_count;
        });
    }

    pub(super) fn executor_stopped(&self) {
        let mutation = self.admission.mutation_guard();
        let retired = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            state.spark_workers = 0;
            let ids = state.work.keys().copied().collect::<Vec<_>>();
            let mut retired = Vec::new();
            for id in ids {
                let is_running = state
                    .work
                    .get(&id)
                    .is_some_and(|record| matches!(record.state, WorkState::Running));
                if is_running {
                    let record = state
                        .work
                        .get_mut(&id)
                        .expect("running spark work must remain registered");
                    record.control.cancel_requested = true;
                    record.control.close_reason = Some(WorkCloseReason::ExecutorShutdown);
                } else if let Some(record) = detach_spark(&mut state, id) {
                    retired.push(record);
                }
            }
            state.work_generation = state.work_generation.wrapping_add(1);
            retired
        };
        drop(mutation);
        self.work_available.notify_all();
        for record in retired {
            record.abandon();
        }
    }

    pub(super) fn work_generation(&self) -> u64 {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .work_generation
    }

    pub(super) fn select(&self) -> CoordinatorSelection {
        let mutation = self.admission.mutation_guard();
        let (selection, changed) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let initial_generation = state.work_generation;
            let had_ready_session = !state.ready_sessions.is_empty();
            let had_ready_spark = !state.ready_sparks.is_empty();
            let selection = if state.prefer_spark {
                claim_ready_spark(&mut state)
                    .map(CoordinatorSelection::Spark)
                    .or_else(|| pop_ready_session(&mut state).map(CoordinatorSelection::Reflection))
                    .unwrap_or(CoordinatorSelection::None)
            } else {
                pop_ready_session(&mut state)
                    .map(CoordinatorSelection::Reflection)
                    .or_else(|| claim_ready_spark(&mut state).map(CoordinatorSelection::Spark))
                    .unwrap_or(CoordinatorSelection::None)
            };
            match selection {
                CoordinatorSelection::Reflection(_) => state.prefer_spark = true,
                CoordinatorSelection::Spark(_) => state.prefer_spark = false,
                CoordinatorSelection::None => {}
            }
            if !matches!(selection, CoordinatorSelection::None)
                || had_ready_session
                || had_ready_spark
            {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            (selection, state.work_generation != initial_generation)
        };
        drop(mutation);
        if changed {
            self.work_available.notify_all();
        }
        selection
    }

    pub(super) fn release_spark(&self, claimed: ClaimedSparkWork, poll: SparkWorkPoll) {
        let mutation = self.admission.mutation_guard();
        let (retired, obsolete_dependency, changed) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get(&claimed.id) else {
                drop(state);
                drop(mutation);
                claimed
                    .prior_dependency
                    .into_iter()
                    .for_each(WorkDependency::abandon);
                drop(claimed.demand);
                return;
            };
            assert_eq!(record.id, claimed.id);
            assert_eq!(record.demand_session, claimed.demand_session);
            assert!(matches!(record.state, WorkState::Running));
            let close_requested =
                record.control.cancel_requested || record.control.close_reason.is_some();

            let mut obsolete_dependency = None;
            let dependency = match poll {
                SparkWorkPoll::Complete => None,
                SparkWorkPoll::Blocked(dependency) => Some(dependency),
            };
            let retired = if close_requested {
                let dependency = match (claimed.prior_dependency, dependency) {
                    (Some(prior), Some(current)) if prior.same_source(&current) => {
                        drop(current);
                        Some(prior)
                    }
                    (Some(prior), current) => {
                        obsolete_dependency = Some(prior);
                        current
                    }
                    (None, current) => current,
                };
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("running spark work must remain registered");
                record.demand = Some(claimed.demand);
                record.dependency = dependency;
                record.state = WorkState::Terminalizing;
                detach_spark(&mut state, claimed.id)
            } else if let Some(dependency) = dependency {
                debug_assert_eq!(dependency.runtime_id(), self.runtime);
                let dependency = if claimed
                    .prior_dependency
                    .as_ref()
                    .is_some_and(|prior| prior.same_source(&dependency))
                {
                    drop(dependency);
                    claimed.prior_dependency
                } else {
                    obsolete_dependency = claimed.prior_dependency;
                    Some(dependency)
                };
                let current_generation = *state
                    .spark_generations
                    .entry(claimed.demand_session)
                    .or_insert(0);
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("running spark work must remain registered");
                record.demand = Some(claimed.demand);
                record.dependency = dependency;
                record.observed_generation = current_generation;
                if current_generation != claimed.observed_generation {
                    record.state = WorkState::Queued;
                    queue_spark(&mut state, claimed.id);
                } else {
                    record.state = WorkState::Blocked;
                    state
                        .blocked_sparks
                        .entry(claimed.demand_session)
                        .or_default()
                        .insert(claimed.id);
                }
                None
            } else {
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("running spark work must remain registered");
                record.demand = Some(claimed.demand);
                record.dependency = claimed.prior_dependency;
                record.state = WorkState::Terminalizing;
                detach_spark(&mut state, claimed.id)
            };
            state.work_generation = state.work_generation.wrapping_add(1);
            (retired, obsolete_dependency, true)
        };
        drop(mutation);
        if changed {
            self.work_available.notify_all();
        }
        if let Some(dependency) = obsolete_dependency {
            dependency.abandon();
        }
        if let Some(record) = retired {
            record.abandon();
        }
    }

    pub(super) fn wait_for_change(&self, observed_generation: u64) {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        while state.work_generation == observed_generation {
            state = self
                .work_available
                .wait(state)
                .expect("evaluation work coordinator was poisoned");
        }
    }

    fn publish_transition(&self, transition: impl FnOnce(&mut WorkCoordinatorState)) {
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            transition(&mut state);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn registered_session_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .sessions
            .len()
    }

    #[cfg(test)]
    pub(crate) fn ready_session_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .ready_session_set
            .len()
    }

    #[cfg(test)]
    pub(crate) fn spark_work_counts(&self) -> (usize, usize, usize) {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let mut queued = 0;
        let mut running = 0;
        let mut blocked = 0;
        for record in state.work.values() {
            match record.state {
                WorkState::Queued => queued += 1,
                WorkState::Running => running += 1,
                WorkState::Blocked => blocked += 1,
                WorkState::Terminalizing => {}
            }
        }
        (queued, running, blocked)
    }

    #[cfg(test)]
    pub(crate) fn retained_spark_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .work
            .len()
    }
}

fn queue_spark(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    if state.ready_spark_set.insert(id) {
        state.ready_sparks.push_back(id);
    }
}

fn claim_ready_spark(state: &mut WorkCoordinatorState) -> Option<ClaimedSparkWork> {
    while let Some(id) = state.ready_sparks.pop_front() {
        state.ready_spark_set.remove(&id);
        let Some(record) = state.work.get_mut(&id) else {
            continue;
        };
        if !matches!(record.state, WorkState::Queued) {
            continue;
        }
        record.state = WorkState::Running;
        let demand = record
            .demand
            .take()
            .expect("queued spark work must retain its demand");
        let prior_dependency = record.dependency.take();
        return Some(ClaimedSparkWork {
            id,
            demand_session: record.demand_session,
            observed_generation: record.observed_generation,
            demand,
            prior_dependency,
        });
    }
    None
}

fn detach_spark(state: &mut WorkCoordinatorState, id: EvaluationWorkId) -> Option<SparkRetirement> {
    let record = state.work.get_mut(&id)?;
    assert!(
        !matches!(record.state, WorkState::Running),
        "worker-owned spark work cannot be detached"
    );
    record.state = WorkState::Terminalizing;
    let mut record = state
        .work
        .remove(&id)
        .expect("terminalizing spark work must remain registered");
    state.ready_spark_set.remove(&id);
    state.ready_sparks.retain(|candidate| *candidate != id);
    if let Some(blocked) = state.blocked_sparks.get_mut(&record.demand_session) {
        blocked.remove(&id);
        if blocked.is_empty() {
            state.blocked_sparks.remove(&record.demand_session);
        }
    }
    if let Some(session_work) = state.work_by_session.get_mut(&record.demand_session) {
        session_work.remove(&id);
        if session_work.is_empty() {
            state.work_by_session.remove(&record.demand_session);
        }
    }
    Some(SparkRetirement {
        demand: record
            .demand
            .take()
            .expect("non-running spark work must retain its demand"),
        dependencies: record.dependency.take().into_iter().collect(),
        _obligations: record.obligations,
    })
}

fn pop_ready_session(state: &mut WorkCoordinatorState) -> Option<Arc<EvaluationSession>> {
    while let Some(session_id) = state.ready_sessions.pop_front() {
        state.ready_session_set.remove(&session_id);
        let session = state.sessions.get(&session_id).and_then(Weak::upgrade);
        if session.is_some() {
            return session;
        }
        state.sessions.remove(&session_id);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinator_owns_session_registration_and_ready_selection() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = EvaluationSession::shared(&coordinator);

        assert_eq!(coordinator.registered_session_count(), 1);
        coordinator.notify_session_ready(session.id);
        coordinator.notify_session_ready(session.id);
        assert_eq!(coordinator.ready_session_count(), 1);

        let CoordinatorSelection::Reflection(selected) = coordinator.select() else {
            panic!("the ready session should be selected")
        };
        assert!(Arc::ptr_eq(&selected, &session));
        assert_eq!(coordinator.ready_session_count(), 0);

        drop(selected);
        drop(session);
        assert_eq!(coordinator.registered_session_count(), 0);
    }

    #[test]
    fn coordinator_fairness_alternates_ready_sessions_and_sparks() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = EvaluationSession::shared(&coordinator);
        coordinator.executor_started(1);
        coordinator.submit_spark(&session, crate::core::keys::unit_value());

        coordinator.notify_session_ready(session.id);
        assert!(matches!(
            coordinator.select(),
            CoordinatorSelection::Reflection(_)
        ));

        coordinator.notify_session_ready(session.id);
        let CoordinatorSelection::Spark(spark) = coordinator.select() else {
            panic!("spark should receive the alternating turn")
        };
        coordinator.release_spark(spark, SparkWorkPoll::Complete);
        assert!(matches!(
            coordinator.select(),
            CoordinatorSelection::Reflection(_)
        ));
    }

    #[test]
    fn queued_sparks_are_abandoned_when_their_demand_session_closes() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = EvaluationSession::shared(&coordinator);
        coordinator.executor_started(1);
        coordinator.submit_spark(&session, crate::core::keys::unit_value());
        let [work] = coordinator
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .work
            .keys()
            .copied()
            .collect::<Vec<_>>()[..]
        else {
            panic!("one stable spark work ID should be registered")
        };
        assert_ne!(work.get(), 0);
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));

        coordinator.unregister_session(session.id);

        assert_eq!(coordinator.spark_work_counts(), (0, 0, 0));
        assert_eq!(coordinator.retained_spark_count(), 0);
    }

    #[test]
    fn dropping_executor_does_not_discard_coordinator_session_state() {
        let (coordinator, executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = EvaluationSession::shared(&coordinator);
        drop(executor);

        coordinator.notify_session_ready(session.id);
        assert!(matches!(
            coordinator.select(),
            CoordinatorSelection::Reflection(_)
        ));
        assert_eq!(coordinator.registered_session_count(), 1);
    }
}
