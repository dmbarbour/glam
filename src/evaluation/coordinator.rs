//! Runtime-owned work coordination independent of worker ownership.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::num::NonZeroU64;
use std::sync::{Arc, Condvar, Mutex, Weak};

use crate::core::{PromisedValue, Value};
use crate::runtime::{
    EvaluationRuntimeId, RuntimeIds, RuntimeMutationAdmission, RuntimeMutationGuard,
    RuntimeValueRoot,
};

use super::{EvaluationSession, EvaluationSessionId, EvaluationWaitToken};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EvaluationWorkId(NonZeroU64);

impl EvaluationWorkId {
    #[cfg(test)]
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct WakeRegistration {
    work: EvaluationWorkId,
    subscription_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum WorkDependencyKey {
    Wait(u64),
    Promise(u64),
    #[cfg(test)]
    Test(u64),
}

pub(super) struct DependencyWakeBatch {
    source: WorkDependencyKey,
    registrations: Vec<WakeRegistration>,
    observed_generation: Option<u64>,
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
    #[cfg(test)]
    Test(TestWorkDependency),
}

impl WorkDependency {
    fn runtime_id(&self) -> EvaluationRuntimeId {
        match self {
            Self::Wait(wait) => wait.runtime_id(),
            Self::Promise(promise) => promise.runtime_id(),
            #[cfg(test)]
            Self::Test(dependency) => dependency.runtime,
        }
    }

    fn key(&self) -> WorkDependencyKey {
        match self {
            Self::Wait(wait) => WorkDependencyKey::Wait(wait.get()),
            Self::Promise(promise) => WorkDependencyKey::Promise(promise.id().get()),
            #[cfg(test)]
            Self::Test(dependency) => WorkDependencyKey::Test(dependency.id.get()),
        }
    }

    fn same_source(&self, other: &Self) -> bool {
        self.runtime_id() == other.runtime_id() && self.key() == other.key()
    }

    fn abandon(self) {
        match self {
            Self::Wait(wait) => {
                if let Some(owner) = wait.owner() {
                    owner.abandon_spark_wait(&wait);
                }
            }
            Self::Promise(_) => {}
            #[cfg(test)]
            Self::Test(_) => {}
        }
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct TestWorkDependency {
    runtime: EvaluationRuntimeId,
    id: NonZeroU64,
}

struct SparkDemand {
    session: Weak<EvaluationSession>,
    value: RuntimeValueRoot,
}

struct WorkRecord {
    id: EvaluationWorkId,
    demand_session: EvaluationSessionId,
    observed_generation: u64,
    subscription_epoch: u64,
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
    blocked_sparks: HashMap<EvaluationSessionId, HashSet<WakeRegistration>>,
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
                    subscription_epoch: 0,
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
    /// progress. Precise promise and wait subscriptions refine this in later
    /// work-boundary phases.
    pub(super) fn notify_spark_disturbance(&self, session: EvaluationSessionId) {
        let mutation = self.admission.mutation_guard();
        let batches = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if !state.sessions.contains_key(&session) {
                None
            } else {
                let generation = state
                    .spark_generations
                    .entry(session)
                    .and_modify(|generation| *generation = generation.wrapping_add(1))
                    .or_insert(1);
                let generation = *generation;
                let blocked = state.blocked_sparks.remove(&session).unwrap_or_default();
                let mut batches: HashMap<WorkDependencyKey, Vec<WakeRegistration>> = HashMap::new();
                for registration in blocked {
                    if let Some(record) = state.work.get(&registration.work)
                        && matches!(record.state, WorkState::Blocked)
                        && record.subscription_epoch == registration.subscription_epoch
                        && let Some(dependency) = record.dependency.as_ref()
                    {
                        batches
                            .entry(dependency.key())
                            .or_default()
                            .push(registration);
                    }
                }
                state.work_generation = state.work_generation.wrapping_add(1);
                Some((generation, batches))
            }
        };
        let disturbed = batches.is_some();
        let mut woke = false;
        if let Some((generation, batches)) = batches {
            for (source, registrations) in batches {
                woke |= self.wake_dependency_batch_guarded(
                    &mutation,
                    DependencyWakeBatch {
                        source,
                        registrations,
                        observed_generation: Some(generation),
                    },
                );
            }
        }
        drop(mutation);
        self.notify_dependency_wake(disturbed || woke);
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
                    record.subscription_epoch = record
                        .subscription_epoch
                        .checked_add(1)
                        .expect("evaluation work subscription epochs exhausted");
                    let registration = WakeRegistration {
                        work: claimed.id,
                        subscription_epoch: record.subscription_epoch,
                    };
                    record.state = WorkState::Blocked;
                    state
                        .blocked_sparks
                        .entry(claimed.demand_session)
                        .or_default()
                        .insert(registration);
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

    /// Queues registrations which still describe the work's current blocked
    /// dependency. The caller already owns this runtime's mutation admission;
    /// dependency publication and the scheduler transition therefore form one
    /// settlement-visible update without nesting component mutexes.
    pub(super) fn wake_dependency_batch_guarded(
        &self,
        _mutation: &RuntimeMutationGuard<'_>,
        batch: DependencyWakeBatch,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let mut changed = false;
        for registration in batch.registrations {
            changed |= queue_current_registration(
                &mut state,
                registration,
                Some(batch.source),
                batch.observed_generation,
            );
        }
        if changed {
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        changed
    }

    pub(super) fn notify_dependency_wake(&self, changed: bool) {
        if changed {
            self.work_available.notify_all();
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

fn queue_current_registration(
    state: &mut WorkCoordinatorState,
    registration: WakeRegistration,
    source: Option<WorkDependencyKey>,
    observed_generation: Option<u64>,
) -> bool {
    let demand_session = {
        let Some(record) = state.work.get_mut(&registration.work) else {
            return false;
        };
        if !matches!(record.state, WorkState::Blocked)
            || record.subscription_epoch != registration.subscription_epoch
            || source.is_some_and(|source| {
                record
                    .dependency
                    .as_ref()
                    .is_none_or(|dependency| dependency.key() != source)
            })
        {
            return false;
        }
        if let Some(generation) = observed_generation {
            record.observed_generation = generation;
        }
        record.state = WorkState::Queued;
        record.demand_session
    };

    if let Some(blocked) = state.blocked_sparks.get_mut(&demand_session) {
        blocked.remove(&registration);
        if blocked.is_empty() {
            state.blocked_sparks.remove(&demand_session);
        }
    }
    queue_spark(state, registration.work);
    true
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
        blocked.remove(&WakeRegistration {
            work: id,
            subscription_epoch: record.subscription_epoch,
        });
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
    use std::sync::{Barrier, OnceLock};
    use std::thread;

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum CompletionSubscriptionOutcome {
        Pending,
        AlreadyTerminal,
    }

    struct CompletionSubscriptions {
        runtime: EvaluationRuntimeId,
        source: WorkDependencyKey,
        coordinator: Weak<EvaluationWorkCoordinator>,
        registrations: Mutex<Vec<WakeRegistration>>,
    }

    impl CompletionSubscriptions {
        fn new(coordinator: &Arc<EvaluationWorkCoordinator>, source: WorkDependencyKey) -> Self {
            Self {
                runtime: coordinator.runtime,
                source,
                coordinator: Arc::downgrade(coordinator),
                registrations: Mutex::new(Vec::new()),
            }
        }

        fn subscribe(
            &self,
            registration: WakeRegistration,
            terminal: impl FnOnce() -> bool,
            before_insert: impl FnOnce(),
        ) -> CompletionSubscriptionOutcome {
            let mut registrations = self
                .registrations
                .lock()
                .expect("test completion subscriber set was poisoned");
            if terminal() {
                return CompletionSubscriptionOutcome::AlreadyTerminal;
            }
            before_insert();
            registrations.push(registration);
            CompletionSubscriptionOutcome::Pending
        }

        fn publish(&self, publish_terminal: impl FnOnce()) {
            let Some(coordinator) = self.coordinator.upgrade() else {
                publish_terminal();
                self.registrations
                    .lock()
                    .expect("test completion subscriber set was poisoned")
                    .clear();
                return;
            };
            assert_eq!(coordinator.runtime, self.runtime);
            let mutation = coordinator.admission.mutation_guard();
            publish_terminal();
            let registrations = std::mem::take(
                &mut *self
                    .registrations
                    .lock()
                    .expect("test completion subscriber set was poisoned"),
            );
            let changed = coordinator.wake_dependency_batch_guarded(
                &mutation,
                DependencyWakeBatch {
                    source: self.source,
                    registrations,
                    observed_generation: None,
                },
            );
            drop(mutation);
            coordinator.notify_dependency_wake(changed);
        }

        fn len(&self) -> usize {
            self.registrations
                .lock()
                .expect("test completion subscriber set was poisoned")
                .len()
        }
    }

    struct TestCompletionSource {
        id: NonZeroU64,
        terminal: OnceLock<()>,
        subscriptions: CompletionSubscriptions,
    }

    impl TestCompletionSource {
        fn new(coordinator: &Arc<EvaluationWorkCoordinator>) -> Arc<Self> {
            let id = coordinator
                .ids
                .evaluation_wait()
                .expect("test completion identity should be available");
            Arc::new(Self {
                id,
                terminal: OnceLock::new(),
                subscriptions: CompletionSubscriptions::new(
                    coordinator,
                    WorkDependencyKey::Test(id.get()),
                ),
            })
        }

        fn runtime_id(&self) -> EvaluationRuntimeId {
            self.subscriptions.runtime
        }

        fn dependency(&self) -> WorkDependency {
            WorkDependency::Test(TestWorkDependency {
                runtime: self.runtime_id(),
                id: self.id,
            })
        }

        fn key(&self) -> WorkDependencyKey {
            self.subscriptions.source
        }

        fn complete(&self) {
            self.subscriptions.publish(|| {
                let _ = self.terminal.set(());
            });
        }

        fn is_terminal(&self) -> bool {
            self.terminal.get().is_some()
        }

        fn subscriber_count(&self) -> usize {
            self.subscriptions.len()
        }

        fn coordinator_is_live(&self) -> bool {
            self.subscriptions.coordinator.upgrade().is_some()
        }
    }

    impl EvaluationWorkCoordinator {
        fn park_claimed_test_spark(
            &self,
            claimed: ClaimedSparkWork,
            source: &TestCompletionSource,
            before_insert: impl FnOnce(),
        ) -> Result<WakeRegistration, Box<ClaimedSparkWork>> {
            if source.runtime_id() != self.runtime {
                return Err(Box::new(claimed));
            }

            let mutation = self.admission.mutation_guard();
            let (registration, obsolete_dependency) = {
                let mut state = self
                    .state
                    .lock()
                    .expect("evaluation work coordinator was poisoned");
                let record = state
                    .work
                    .get(&claimed.id)
                    .expect("claimed test spark work must remain registered");
                assert_eq!(record.id, claimed.id);
                assert_eq!(record.demand_session, claimed.demand_session);
                assert!(matches!(record.state, WorkState::Running));
                assert!(!record.control.cancel_requested);
                assert!(record.control.close_reason.is_none());

                let current_generation = *state
                    .spark_generations
                    .entry(claimed.demand_session)
                    .or_insert(0);
                assert_eq!(current_generation, claimed.observed_generation);

                let current_dependency = source.dependency();
                let (dependency, obsolete_dependency) = if claimed
                    .prior_dependency
                    .as_ref()
                    .is_some_and(|prior| prior.same_source(&current_dependency))
                {
                    drop(current_dependency);
                    (claimed.prior_dependency, None)
                } else {
                    (Some(current_dependency), claimed.prior_dependency)
                };
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("claimed test spark work must remain registered");
                record.demand = Some(claimed.demand);
                record.dependency = dependency;
                record.observed_generation = current_generation;
                record.subscription_epoch = record
                    .subscription_epoch
                    .checked_add(1)
                    .expect("evaluation work subscription epochs exhausted");
                record.state = WorkState::Blocked;
                let registration = WakeRegistration {
                    work: claimed.id,
                    subscription_epoch: record.subscription_epoch,
                };
                state
                    .blocked_sparks
                    .entry(claimed.demand_session)
                    .or_default()
                    .insert(registration);
                state.work_generation = state.work_generation.wrapping_add(1);
                (registration, obsolete_dependency)
            };

            let outcome = source.subscriptions.subscribe(
                registration,
                || source.is_terminal(),
                before_insert,
            );
            let woke = if outcome == CompletionSubscriptionOutcome::AlreadyTerminal {
                self.wake_dependency_batch_guarded(
                    &mutation,
                    DependencyWakeBatch {
                        source: source.key(),
                        registrations: vec![registration],
                        observed_generation: None,
                    },
                )
            } else {
                false
            };
            drop(mutation);
            self.work_available.notify_all();
            self.notify_dependency_wake(woke);
            obsolete_dependency
                .into_iter()
                .for_each(WorkDependency::abandon);
            Ok(registration)
        }

        fn redeliver_test_registration(
            &self,
            source: WorkDependencyKey,
            registration: WakeRegistration,
        ) -> bool {
            let mutation = self.admission.mutation_guard();
            let changed = self.wake_dependency_batch_guarded(
                &mutation,
                DependencyWakeBatch {
                    source,
                    registrations: vec![registration],
                    observed_generation: None,
                },
            );
            drop(mutation);
            self.notify_dependency_wake(changed);
            changed
        }
    }

    fn claimed_test_spark() -> (
        Arc<EvaluationWorkCoordinator>,
        Arc<super::super::EvaluationExecutor>,
        Arc<EvaluationSession>,
        ClaimedSparkWork,
    ) {
        let (coordinator, executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = EvaluationSession::shared(&coordinator);
        coordinator.executor_started(1);
        coordinator.submit_spark(&session, crate::core::keys::unit_value());
        let CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("test spark should be claimable")
        };
        (coordinator, executor, session, claimed)
    }

    fn finish_queued_test_spark(coordinator: &EvaluationWorkCoordinator) {
        let CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("woken test spark should be claimable")
        };
        coordinator.release_spark(claimed, SparkWorkPoll::Complete);
    }

    #[test]
    fn completion_before_subscription_requeues_immediately() {
        let (coordinator, _executor, _session, claimed) = claimed_test_spark();
        let source = TestCompletionSource::new(&coordinator);
        source.complete();

        let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
            panic!("same-runtime completion source should accept the subscription")
        };

        assert_eq!(source.subscriber_count(), 0);
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
        finish_queued_test_spark(&coordinator);
    }

    #[test]
    fn completion_during_subscription_cannot_lose_the_wake() {
        let (coordinator, _executor, _session, claimed) = claimed_test_spark();
        let source = TestCompletionSource::new(&coordinator);
        let started = Arc::new(Barrier::new(2));
        let completer = Arc::new(Mutex::new(None));

        let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, {
            let source = source.clone();
            let started = started.clone();
            let completer = completer.clone();
            move || {
                let completion_source = source.clone();
                let completion_started = started.clone();
                *completer
                    .lock()
                    .expect("completion thread slot was poisoned") =
                    Some(thread::spawn(move || {
                        completion_started.wait();
                        completion_source.complete();
                    }));
                started.wait();
                while !source.is_terminal() {
                    thread::yield_now();
                }
            }
        }) else {
            panic!("same-runtime completion source should accept the subscription")
        };
        completer
            .lock()
            .expect("completion thread slot was poisoned")
            .take()
            .expect("the subscription hook should start a completer")
            .join()
            .expect("test completion thread should not panic");

        assert_eq!(source.subscriber_count(), 0);
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
        finish_queued_test_spark(&coordinator);
    }

    #[test]
    fn completion_after_subscription_requeues_once() {
        let (coordinator, _executor, _session, claimed) = claimed_test_spark();
        let source = TestCompletionSource::new(&coordinator);
        let Ok(registration) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
            panic!("same-runtime completion source should accept the subscription")
        };
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 1));
        assert_eq!(source.subscriber_count(), 1);

        source.complete();
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
        let generation = coordinator.work_generation();
        assert!(!coordinator.redeliver_test_registration(source.key(), registration));
        assert_eq!(coordinator.work_generation(), generation);
        finish_queued_test_spark(&coordinator);
    }

    #[test]
    fn stale_dependency_wake_does_not_requeue_work_blocked_elsewhere() {
        let (coordinator, _executor, session, claimed) = claimed_test_spark();
        let source_a = TestCompletionSource::new(&coordinator);
        let source_b = TestCompletionSource::new(&coordinator);
        let Ok(registration_a) = coordinator.park_claimed_test_spark(claimed, &source_a, || {})
        else {
            panic!("same-runtime completion source should accept the subscription")
        };

        coordinator.notify_spark_disturbance(session.id);
        let CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("broad disturbance should requeue the test spark")
        };
        let Ok(registration_b) = coordinator.park_claimed_test_spark(claimed, &source_b, || {})
        else {
            panic!("same-runtime completion source should accept the subscription")
        };
        assert!(registration_b.subscription_epoch > registration_a.subscription_epoch);

        let generation = coordinator.work_generation();
        source_a.complete();
        assert_eq!(coordinator.work_generation(), generation);
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 1));

        source_b.complete();
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
        finish_queued_test_spark(&coordinator);
    }

    #[test]
    fn repeated_dependency_uses_a_new_epoch_and_queues_only_once() {
        let (coordinator, _executor, session, claimed) = claimed_test_spark();
        let source = TestCompletionSource::new(&coordinator);
        let Ok(first) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
            panic!("same-runtime completion source should accept the subscription")
        };

        coordinator.notify_spark_disturbance(session.id);
        let CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("broad disturbance should requeue the test spark")
        };
        let Ok(second) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
            panic!("same-runtime completion source should accept the subscription")
        };
        assert!(second.subscription_epoch > first.subscription_epoch);
        assert_eq!(source.subscriber_count(), 2);

        let generation = coordinator.work_generation();
        source.complete();
        assert_eq!(coordinator.work_generation(), generation.wrapping_add(1));
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
        assert!(!coordinator.redeliver_test_registration(source.key(), second));
        assert_eq!(coordinator.work_generation(), generation.wrapping_add(1));
        finish_queued_test_spark(&coordinator);
    }

    #[test]
    fn retired_work_makes_late_completion_registrations_harmless() {
        let (coordinator, executor, session, claimed) = claimed_test_spark();
        let source = TestCompletionSource::new(&coordinator);
        let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
            panic!("same-runtime completion source should accept the subscription")
        };

        drop(session);
        assert_eq!(coordinator.retained_spark_count(), 0);
        source.complete();
        assert_eq!(coordinator.retained_spark_count(), 0);
        drop(executor);

        let (coordinator, executor, session, claimed) = claimed_test_spark();
        let source = TestCompletionSource::new(&coordinator);
        let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
            panic!("same-runtime completion source should accept the subscription")
        };

        drop(executor);
        assert_eq!(coordinator.retained_spark_count(), 0);
        source.complete();
        assert_eq!(coordinator.retained_spark_count(), 0);
        drop(session);
    }

    #[test]
    fn completion_source_does_not_retain_its_runtime_coordinator() {
        let source = {
            let (coordinator, executor, session, claimed) = claimed_test_spark();
            let source = TestCompletionSource::new(&coordinator);
            let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, || {})
            else {
                panic!("same-runtime completion source should accept the subscription")
            };
            drop(session);
            drop(executor);
            drop(coordinator);
            source
        };

        assert!(!source.coordinator_is_live());
        source.complete();
        assert!(source.is_terminal());
        assert_eq!(source.subscriber_count(), 0);
    }

    #[test]
    fn foreign_runtime_is_rejected_before_subscription_or_parking() {
        let (coordinator, _executor, _session, claimed) = claimed_test_spark();
        let other_coordinator = EvaluationWorkCoordinator::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
            RuntimeMutationAdmission::new(),
        );
        let source = TestCompletionSource::new(&other_coordinator);

        let Err(claimed) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
            panic!("foreign-runtime completion source must be rejected")
        };
        assert_eq!(source.subscriber_count(), 0);
        assert_eq!(coordinator.spark_work_counts(), (0, 1, 0));
        coordinator.release_spark(*claimed, SparkWorkPoll::Complete);
        assert_eq!(coordinator.retained_spark_count(), 0);
    }

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
