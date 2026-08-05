//! Runtime-owned work coordination independent of worker ownership.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::num::NonZeroU64;
use std::sync::{Arc, Condvar, Mutex, Weak};

#[cfg(test)]
use crate::core::CoreValueFactory;
use crate::core::{PromiseId, PromisedValue, Value};
use crate::runtime::{
    EvaluationRuntimeId, RuntimeIds, RuntimeMutationAdmission, RuntimeMutationGuard,
    RuntimeValueRoot,
};

use super::{
    EvaluationFailure, EvaluationSession, EvaluationSessionId, EvaluationTaskId,
    EvaluationWaitToken,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationTaskBlock {
    pub(crate) lazy: Option<EvaluationWaitToken>,
    pub(crate) observed_generation: Option<u64>,
    pub(crate) error: Option<Arc<EvaluationFailure>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EvaluationWorkId(NonZeroU64);

impl EvaluationWorkId {
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct WakeRegistration {
    work: EvaluationWorkId,
    subscription_epoch: u64,
}

#[cfg(test)]
pub(crate) fn test_wake_registration() -> WakeRegistration {
    WakeRegistration {
        work: EvaluationWorkId(NonZeroU64::MAX),
        subscription_epoch: 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WorkDependencyKey {
    Wait(u64),
    Promise(u64),
    #[cfg(test)]
    Test(u64),
}

pub(crate) struct DependencyWakeBatch {
    source: WorkDependencyKey,
    registrations: Vec<WakeRegistration>,
}

/// Weak, epoch-tagged registrations retained by a one-shot completion source.
///
/// The terminal state remains owned by the source. This component only pairs
/// subscribe-and-recheck with detached coordinator delivery, and therefore
/// cannot retain the runtime or any work record.
pub(crate) struct CompletionSubscriptions {
    runtime: EvaluationRuntimeId,
    source: WorkDependencyKey,
    coordinator: Arc<std::sync::OnceLock<Weak<EvaluationWorkCoordinator>>>,
    registrations: Mutex<Vec<WakeRegistration>>,
}

impl CompletionSubscriptions {
    pub(crate) fn for_promise(
        runtime: EvaluationRuntimeId,
        promise: PromiseId,
        coordinator: Arc<std::sync::OnceLock<Weak<EvaluationWorkCoordinator>>>,
    ) -> Self {
        Self {
            runtime,
            source: WorkDependencyKey::Promise(promise.get()),
            coordinator,
            registrations: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn for_wait(
        runtime: EvaluationRuntimeId,
        wait: NonZeroU64,
        coordinator: Arc<std::sync::OnceLock<Weak<EvaluationWorkCoordinator>>>,
    ) -> Self {
        Self {
            runtime,
            source: WorkDependencyKey::Wait(wait.get()),
            coordinator,
            registrations: Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    fn for_test(coordinator: &Arc<EvaluationWorkCoordinator>, source: WorkDependencyKey) -> Self {
        let binding = Arc::new(std::sync::OnceLock::new());
        binding
            .set(Arc::downgrade(coordinator))
            .expect("test completion coordinator should bind once");
        Self {
            runtime: coordinator.runtime,
            source,
            coordinator: binding,
            registrations: Mutex::new(Vec::new()),
        }
    }

    /// Publishes a source terminal while holding shared runtime mutation
    /// admission, then detaches and delivers every exact wake registration.
    /// External/session wakes returned by `publish_terminal` remain the
    /// caller's responsibility and must run after this method returns.
    pub(crate) fn publish<T, E>(
        &self,
        publish_terminal: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let coordinator = self
            .coordinator
            .get()
            .and_then(Weak::upgrade)
            .filter(|coordinator| coordinator.runtime == self.runtime);
        let mutation = coordinator
            .as_ref()
            .map(|coordinator| coordinator.admission.mutation_guard());
        let result = publish_terminal()?;
        let registrations = std::mem::take(
            &mut *self
                .registrations
                .lock()
                .expect("completion subscriber set was poisoned"),
        );
        let changed = match (&coordinator, &mutation) {
            (Some(coordinator), Some(mutation)) => coordinator.wake_dependency_batch_guarded(
                mutation,
                DependencyWakeBatch {
                    source: self.source,
                    registrations,
                },
            ),
            _ => false,
        };
        drop(mutation);
        if let Some(coordinator) = coordinator {
            coordinator.notify_dependency_wake(changed);
        }
        Ok(result)
    }

    /// Detaches and delivers registrations for a terminal which was already
    /// published under its producer registry lock.
    ///
    /// Session-owned waits use this transitional split until task
    /// terminalization moves under coordinator mutation admission. The
    /// terminal cell is immutable before this call, so subscribe-and-recheck
    /// still cannot lose a wake.
    pub(crate) fn notify_published(&self) {
        self.publish(|| Ok::<_, std::convert::Infallible>(()))
            .expect("published completion notification is infallible");
    }

    pub(crate) fn subscribe(
        &self,
        runtime: EvaluationRuntimeId,
        registration: WakeRegistration,
        terminal: impl FnOnce() -> bool,
    ) -> CompletionSubscriptionOutcome {
        self.subscribe_with(runtime, registration, terminal, || {})
    }

    fn subscribe_with(
        &self,
        runtime: EvaluationRuntimeId,
        registration: WakeRegistration,
        terminal: impl FnOnce() -> bool,
        before_insert: impl FnOnce(),
    ) -> CompletionSubscriptionOutcome {
        if runtime != self.runtime {
            return CompletionSubscriptionOutcome::ForeignRuntime;
        }
        let mut registrations = self
            .registrations
            .lock()
            .expect("completion subscriber set was poisoned");
        if terminal() {
            return CompletionSubscriptionOutcome::AlreadyTerminal;
        }
        before_insert();
        registrations.push(registration);
        CompletionSubscriptionOutcome::Pending
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.registrations
            .lock()
            .expect("completion subscriber set was poisoned")
            .len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionSubscriptionOutcome {
    Pending,
    AlreadyTerminal,
    ForeignRuntime,
}

#[derive(Default)]
struct WorkControl {
    close_reason: Option<WorkCloseReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkCloseReason {
    ExplicitCancellation,
    DemandSessionClosed,
    ExecutorShutdown,
}

/// Reserved now so later task migration does not need another work-record
/// shape. Sparks acquire no settlement obligations in this phase.
#[derive(Default)]
struct SettlementObligations;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkState {
    Dormant,
    Reserved,
    Queued,
    Running,
    Blocked,
    Terminalizing,
}

#[derive(Clone)]
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

    fn subscribe_spark(
        &self,
        runtime: EvaluationRuntimeId,
        registration: WakeRegistration,
    ) -> CompletionSubscriptionOutcome {
        match self {
            Self::Wait(wait) => wait.subscribe_spark(runtime, registration),
            Self::Promise(promise) => promise.subscribe_spark(runtime, registration),
            #[cfg(test)]
            Self::Test(_) => {
                unreachable!("synthetic completion sources install their own subscription")
            }
        }
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

struct SparkWork {
    demand: Option<SparkDemand>,
    dependency: Option<WorkDependency>,
}

struct ReflectionWork {
    task: EvaluationTaskId,
    block: Option<EvaluationTaskBlock>,
}

enum WorkKind {
    Spark(SparkWork),
    Reflection(ReflectionWork),
}

struct WorkRecord {
    id: EvaluationWorkId,
    demand_session: EvaluationSessionId,
    subscription_epoch: u64,
    control: WorkControl,
    obligations: SettlementObligations,
    state: WorkState,
    kind: WorkKind,
}

pub(super) struct ClaimedSparkWork {
    id: EvaluationWorkId,
    demand_session: EvaluationSessionId,
    demand: SparkDemand,
    prior_dependency: Option<WorkDependency>,
}

pub(super) struct ClaimedReflectionWork {
    id: EvaluationWorkId,
    task: EvaluationTaskId,
    demand_session: EvaluationSessionId,
    prior_block: Option<EvaluationTaskBlock>,
}

impl ClaimedReflectionWork {
    pub(super) fn id(&self) -> EvaluationWorkId {
        self.id
    }

    pub(super) fn task(&self) -> EvaluationTaskId {
        self.task
    }
}

pub(super) enum ReflectionWorkPoll {
    Yielded,
    Blocked(EvaluationTaskBlock),
    Terminal,
}

pub(super) struct ReflectionWorkRelease {
    pub(super) made_progress: bool,
    pub(super) remains_blocked: bool,
    pub(super) terminal: bool,
    pub(super) cancel: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReflectionWorkState {
    Dormant,
    Reserved,
    Queued,
    Running,
    Blocked(EvaluationTaskBlock),
    Terminalizing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReflectionWorkSnapshot {
    pub(super) task: EvaluationTaskId,
    pub(super) state: ReflectionWorkState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReflectionCancellation {
    Requested,
    Terminalize,
    Late,
}

pub(super) struct AbandonedReflectionWork {
    pub(super) id: EvaluationWorkId,
    pub(super) task: EvaluationTaskId,
    pub(super) cancel: bool,
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
    ready_reflection: HashMap<EvaluationSessionId, VecDeque<EvaluationWorkId>>,
    ready_reflection_set: HashSet<EvaluationWorkId>,
    reflection_by_task: std::collections::BTreeMap<EvaluationTaskId, EvaluationWorkId>,
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
    #[cfg(test)]
    test_values: Option<CoreValueFactory>,
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
            #[cfg(test)]
            test_values: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        values: CoreValueFactory,
        admission: Arc<RuntimeMutationAdmission>,
    ) -> Arc<Self> {
        let coordinator = Arc::new(Self {
            runtime: values.runtime_id(),
            ids: values.ids().clone(),
            admission,
            state: Mutex::new(WorkCoordinatorState::default()),
            work_available: Condvar::new(),
            test_values: Some(values.clone()),
        });
        values.attach_work_coordinator(&coordinator);
        coordinator
    }

    #[cfg(test)]
    pub(crate) fn test_values(&self) -> CoreValueFactory {
        self.test_values
            .as_ref()
            .expect("synthetic execution resources must install test values")
            .clone()
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
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

            debug_assert!(
                state
                    .work_by_session
                    .get(&session)
                    .into_iter()
                    .flatten()
                    .all(|id| state
                        .work
                        .get(id)
                        .is_none_or(|record| !matches!(record.kind, WorkKind::Reflection(_)))),
                "reflection work must be retired before its machine session unregisters"
            );

            let work = state
                .work_by_session
                .get(&session)
                .into_iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            let mut retired = Vec::new();
            for id in work {
                if !state
                    .work
                    .get(&id)
                    .is_some_and(|record| matches!(record.kind, WorkKind::Spark(_)))
                {
                    continue;
                }
                let is_running = state
                    .work
                    .get(&id)
                    .is_some_and(|record| matches!(record.state, WorkState::Running));
                if is_running {
                    let record = state
                        .work
                        .get_mut(&id)
                        .expect("indexed running spark work must remain registered");
                    record.control.close_reason = Some(WorkCloseReason::DemandSessionClosed);
                } else if state
                    .work
                    .get(&id)
                    .is_some_and(|record| matches!(record.kind, WorkKind::Spark(_)))
                    && let Some(record) = detach_spark(&mut state, id)
                {
                    retired.push(record);
                }
            }
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
            if !state.sessions.contains_key(&session) || state.ready_session_set.contains(&session)
            {
                false
            } else {
                queue_session(&mut state, session);
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
                let record = WorkRecord {
                    id,
                    demand_session: session_id,
                    subscription_epoch: 0,
                    control: WorkControl::default(),
                    obligations: SettlementObligations,
                    state: WorkState::Queued,
                    kind: WorkKind::Spark(SparkWork {
                        demand: Some(demand),
                        dependency: None,
                    }),
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
                if !state
                    .work
                    .get(&id)
                    .is_some_and(|record| matches!(record.kind, WorkKind::Spark(_)))
                {
                    continue;
                }
                let is_running = state
                    .work
                    .get(&id)
                    .is_some_and(|record| matches!(record.state, WorkState::Running));
                if is_running {
                    let record = state
                        .work
                        .get_mut(&id)
                        .expect("running spark work must remain registered");
                    record.control.close_reason = Some(WorkCloseReason::ExecutorShutdown);
                } else if state
                    .work
                    .get(&id)
                    .is_some_and(|record| matches!(record.kind, WorkKind::Spark(_)))
                    && let Some(record) = detach_spark(&mut state, id)
                {
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
        let (retired, obsolete_dependency, exact_subscription) = {
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
            let close_requested = record.control.close_reason.is_some();

            let mut obsolete_dependency = None;
            let mut exact_subscription = None;
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
                let spark = spark_work_mut(record);
                spark.demand = Some(claimed.demand);
                spark.dependency = dependency;
                record.state = WorkState::Terminalizing;
                detach_spark(&mut state, claimed.id)
            } else if let Some(dependency) = dependency {
                if dependency.runtime_id() != self.runtime {
                    obsolete_dependency = claimed.prior_dependency;
                    let record = state
                        .work
                        .get_mut(&claimed.id)
                        .expect("running spark work must remain registered");
                    let spark = spark_work_mut(record);
                    spark.demand = Some(claimed.demand);
                    spark.dependency = Some(dependency);
                    record.state = WorkState::Terminalizing;
                    detach_spark(&mut state, claimed.id)
                } else {
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
                    let record = state
                        .work
                        .get_mut(&claimed.id)
                        .expect("running spark work must remain registered");
                    let spark = spark_work_mut(record);
                    spark.demand = Some(claimed.demand);
                    spark.dependency = dependency;
                    record.subscription_epoch = record
                        .subscription_epoch
                        .checked_add(1)
                        .expect("evaluation work subscription epochs exhausted");
                    let registration = WakeRegistration {
                        work: claimed.id,
                        subscription_epoch: record.subscription_epoch,
                    };
                    record.state = WorkState::Blocked;
                    exact_subscription = Some((
                        spark_work(record)
                            .dependency
                            .as_ref()
                            .expect("blocked spark work must retain its dependency")
                            .clone(),
                        registration,
                    ));
                    None
                }
            } else {
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("running spark work must remain registered");
                let spark = spark_work_mut(record);
                spark.demand = Some(claimed.demand);
                spark.dependency = claimed.prior_dependency;
                record.state = WorkState::Terminalizing;
                detach_spark(&mut state, claimed.id)
            };
            state.work_generation = state.work_generation.wrapping_add(1);
            (retired, obsolete_dependency, exact_subscription)
        };

        if let Some((dependency, registration)) = exact_subscription {
            let source = dependency.key();
            match dependency.subscribe_spark(self.runtime, registration) {
                CompletionSubscriptionOutcome::Pending => {}
                CompletionSubscriptionOutcome::AlreadyTerminal => {
                    self.wake_dependency_batch_guarded(
                        &mutation,
                        DependencyWakeBatch {
                            source,
                            registrations: vec![registration],
                        },
                    );
                }
                CompletionSubscriptionOutcome::ForeignRuntime => {
                    unreachable!("foreign dependencies must be rejected before parking")
                }
            }
        }
        drop(mutation);
        self.work_available.notify_all();
        if let Some(dependency) = obsolete_dependency {
            dependency.abandon();
        }
        if let Some(record) = retired {
            record.abandon();
        }
    }

    pub(super) fn reserve_reflection(
        &self,
        session: &Arc<EvaluationSession>,
        task: EvaluationTaskId,
    ) -> EvaluationWorkId {
        self.insert_reflection(session, task, WorkState::Reserved)
    }

    pub(super) fn register_dormant_reflection(
        &self,
        session: &Arc<EvaluationSession>,
        task: EvaluationTaskId,
    ) -> EvaluationWorkId {
        self.insert_reflection(session, task, WorkState::Dormant)
    }

    fn insert_reflection(
        &self,
        session: &Arc<EvaluationSession>,
        task: EvaluationTaskId,
        initial: WorkState,
    ) -> EvaluationWorkId {
        debug_assert_eq!(session.values.runtime_id(), self.runtime);
        debug_assert!(matches!(initial, WorkState::Dormant | WorkState::Reserved));
        let id = EvaluationWorkId(self.ids.evaluation_work());
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            assert!(
                state.sessions.contains_key(&session.id),
                "reflection work requires a registered demand session"
            );
            let record = WorkRecord {
                id,
                demand_session: session.id,
                subscription_epoch: 0,
                control: WorkControl::default(),
                obligations: SettlementObligations,
                state: initial,
                kind: WorkKind::Reflection(ReflectionWork { task, block: None }),
            };
            assert!(state.work.insert(id, record).is_none());
            assert!(state.reflection_by_task.insert(task, id).is_none());
            state
                .work_by_session
                .entry(session.id)
                .or_default()
                .insert(id);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_all();
        id
    }

    pub(super) fn activate_reflection(&self, id: EvaluationWorkId) -> bool {
        let mutation = self.admission.mutation_guard();
        let activated = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get_mut(&id) else {
                return false;
            };
            if !matches!(record.kind, WorkKind::Reflection(_))
                || !matches!(record.state, WorkState::Reserved)
            {
                return false;
            }
            record.state = WorkState::Queued;
            queue_reflection(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
            true
        };
        drop(mutation);
        if activated {
            self.work_available.notify_all();
        }
        activated
    }

    pub(super) fn terminalize_reserved_reflection(&self, id: EvaluationWorkId) -> bool {
        self.begin_reflection_terminalization(id, Some(WorkState::Reserved))
    }

    #[cfg(test)]
    pub(super) fn terminalize_reflection(&self, id: EvaluationWorkId) -> bool {
        self.begin_reflection_terminalization(id, None)
    }

    pub(super) fn discard_reserved_reflection(&self, id: EvaluationWorkId) -> bool {
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
                detach_reflection(&mut state, id);
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

    pub(super) fn request_reflection_cancellation(
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
            match record.state {
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
                | WorkState::Blocked => {
                    record
                        .control
                        .close_reason
                        .get_or_insert(WorkCloseReason::ExplicitCancellation);
                    record.state = WorkState::Terminalizing;
                    remove_ready_reflection(&mut state, id);
                    ReflectionCancellation::Terminalize
                }
                WorkState::Terminalizing => ReflectionCancellation::Late,
            }
        };
        drop(mutation);
        if !matches!(outcome, ReflectionCancellation::Late) {
            self.work_available.notify_all();
        }
        outcome
    }

    /// Moves every non-running reflection task owned by `session` into the
    /// terminalization handshake. A running task retains the session through
    /// its detached machine context, so observing one here would violate the
    /// session lifetime invariant.
    pub(super) fn abandon_reflection_session(
        &self,
        session: EvaluationSessionId,
    ) -> Vec<AbandonedReflectionWork> {
        let mutation = self.admission.mutation_guard();
        let abandoned = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let ids = state
                .work_by_session
                .get(&session)
                .into_iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            let mut abandoned = Vec::new();
            for id in ids {
                let Some(record) = state.work.get(&id) else {
                    continue;
                };
                if !matches!(record.kind, WorkKind::Reflection(_)) {
                    continue;
                }
                assert!(
                    !matches!(record.state, WorkState::Running),
                    "a detached reflection machine must retain its evaluation session"
                );
                let task = reflection_work(record).task;
                let cancel = matches!(
                    record.control.close_reason,
                    Some(WorkCloseReason::ExplicitCancellation)
                );
                let record = state
                    .work
                    .get_mut(&id)
                    .expect("indexed reflection work must remain registered");
                record.control.close_reason.get_or_insert(if cancel {
                    WorkCloseReason::ExplicitCancellation
                } else {
                    WorkCloseReason::DemandSessionClosed
                });
                record.state = WorkState::Terminalizing;
                remove_ready_reflection(&mut state, id);
                abandoned.push(AbandonedReflectionWork { id, task, cancel });
            }
            if !abandoned.is_empty() {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            abandoned
        };
        drop(mutation);
        if !abandoned.is_empty() {
            self.work_available.notify_all();
        }
        abandoned
    }

    pub(super) fn claim_ready_reflection(
        &self,
        session: EvaluationSessionId,
    ) -> Option<ClaimedReflectionWork> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let claimed = claim_ready_reflection(&mut state, session);
            if claimed.is_some() {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            claimed
        };
        drop(mutation);
        if claimed.is_some() {
            self.work_available.notify_all();
        }
        claimed
    }

    pub(super) fn claim_reflection(&self, id: EvaluationWorkId) -> Option<ClaimedReflectionWork> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let claimed = claim_reflection(&mut state, id);
            if claimed.is_some() {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            claimed
        };
        drop(mutation);
        if claimed.is_some() {
            self.work_available.notify_all();
        }
        claimed
    }

    pub(super) fn claim_blocked_reflection(
        &self,
        session: EvaluationSessionId,
        attempted: &HashSet<EvaluationTaskId>,
    ) -> Option<ClaimedReflectionWork> {
        let id = {
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            state.reflection_by_task.iter().find_map(|(task, id)| {
                let record = state.work.get(id)?;
                (record.demand_session == session
                    && matches!(record.state, WorkState::Blocked)
                    && !attempted.contains(task))
                .then_some(*id)
            })
        }?;
        self.claim_reflection(id)
    }

    pub(super) fn release_reflection(
        &self,
        claimed: ClaimedReflectionWork,
        poll: ReflectionWorkPoll,
    ) -> ReflectionWorkRelease {
        let mutation = self.admission.mutation_guard();
        let release = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let cancel = {
                let record = state
                    .work
                    .get(&claimed.id)
                    .expect("claimed reflection work must remain registered");
                assert_eq!(record.demand_session, claimed.demand_session);
                assert_eq!(reflection_work(record).task, claimed.task);
                assert!(matches!(record.state, WorkState::Running));
                matches!(
                    record.control.close_reason,
                    Some(WorkCloseReason::ExplicitCancellation)
                )
            };
            let (state_after, block, made_progress, remains_blocked, terminal) = if cancel {
                (WorkState::Terminalizing, None, true, false, true)
            } else {
                match poll {
                    ReflectionWorkPoll::Yielded => (WorkState::Queued, None, true, false, false),
                    ReflectionWorkPoll::Blocked(block) => {
                        let unchanged = claimed.prior_block.as_ref() == Some(&block);
                        (WorkState::Blocked, Some(block), !unchanged, true, false)
                    }
                    ReflectionWorkPoll::Terminal => {
                        (WorkState::Terminalizing, None, true, false, true)
                    }
                }
            };
            {
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("claimed reflection work must remain registered");
                reflection_work_mut(record).block = block;
                record.state = state_after;
            }
            if matches!(state_after, WorkState::Queued) {
                queue_reflection(&mut state, claimed.id);
            }
            state.work_generation = state.work_generation.wrapping_add(1);
            ReflectionWorkRelease {
                made_progress,
                remains_blocked,
                terminal,
                cancel,
            }
        };
        drop(mutation);
        self.work_available.notify_all();
        release
    }

    pub(super) fn retire_reflection(&self, id: EvaluationWorkId) {
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get(&id)
                .expect("terminal reflection work must remain registered");
            assert!(matches!(record.state, WorkState::Terminalizing));
            detach_reflection(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_all();
    }

    pub(super) fn reflection_work_id(&self, task: EvaluationTaskId) -> Option<EvaluationWorkId> {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .reflection_by_task
            .get(&task)
            .copied()
    }

    pub(super) fn reflection_dependency(
        &self,
        task: EvaluationTaskId,
    ) -> Option<EvaluationWaitToken> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let id = state.reflection_by_task.get(&task)?;
        let record = state.work.get(id)?;
        reflection_work(record)
            .block
            .as_ref()
            .and_then(|block| block.lazy.clone())
    }

    pub(super) fn reflection_is_claimable(
        &self,
        task: EvaluationTaskId,
        attempted: &HashSet<EvaluationTaskId>,
    ) -> bool {
        if attempted.contains(&task) {
            return false;
        }
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(id) = state.reflection_by_task.get(&task) else {
            return false;
        };
        state
            .work
            .get(id)
            .is_some_and(|record| matches!(record.state, WorkState::Queued | WorkState::Blocked))
    }

    pub(super) fn reflection_is_busy(&self, task: EvaluationTaskId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(id) = state.reflection_by_task.get(&task) else {
            return false;
        };
        state.work.get(id).is_some_and(|record| {
            matches!(record.state, WorkState::Running | WorkState::Terminalizing)
        })
    }

    pub(super) fn reflection_snapshots(
        &self,
        session: EvaluationSessionId,
    ) -> Vec<ReflectionWorkSnapshot> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state
            .reflection_by_task
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
            changed |= queue_current_registration(&mut state, registration, Some(batch.source));
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
            if !matches!(record.kind, WorkKind::Spark(_)) {
                continue;
            }
            match record.state {
                WorkState::Queued => queued += 1,
                WorkState::Running => running += 1,
                WorkState::Blocked => blocked += 1,
                WorkState::Dormant | WorkState::Reserved | WorkState::Terminalizing => {}
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
            .values()
            .filter(|record| matches!(record.kind, WorkKind::Spark(_)))
            .count()
    }
}

fn spark_work(record: &WorkRecord) -> &SparkWork {
    match &record.kind {
        WorkKind::Spark(work) => work,
        WorkKind::Reflection(_) => panic!("reflection work cannot be used as a spark"),
    }
}

fn spark_work_mut(record: &mut WorkRecord) -> &mut SparkWork {
    match &mut record.kind {
        WorkKind::Spark(work) => work,
        WorkKind::Reflection(_) => panic!("reflection work cannot be used as a spark"),
    }
}

fn reflection_work(record: &WorkRecord) -> &ReflectionWork {
    match &record.kind {
        WorkKind::Reflection(work) => work,
        WorkKind::Spark(_) => panic!("spark work cannot be used as a reflection task"),
    }
}

fn reflection_work_mut(record: &mut WorkRecord) -> &mut ReflectionWork {
    match &mut record.kind {
        WorkKind::Reflection(work) => work,
        WorkKind::Spark(_) => panic!("spark work cannot be used as a reflection task"),
    }
}

fn queue_session(state: &mut WorkCoordinatorState, session: EvaluationSessionId) {
    if state.sessions.contains_key(&session) && state.ready_session_set.insert(session) {
        state.ready_sessions.push_back(session);
    }
}

fn queue_reflection(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    let session = state
        .work
        .get(&id)
        .expect("queued reflection work must remain registered")
        .demand_session;
    if state.ready_reflection_set.insert(id) {
        state
            .ready_reflection
            .entry(session)
            .or_default()
            .push_back(id);
    }
    queue_session(state, session);
}

fn remove_ready_reflection(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    state.ready_reflection_set.remove(&id);
    let Some(session) = state.work.get(&id).map(|record| record.demand_session) else {
        return;
    };
    if let Some(ready) = state.ready_reflection.get_mut(&session) {
        ready.retain(|candidate| *candidate != id);
        if ready.is_empty() {
            state.ready_reflection.remove(&session);
            state.ready_session_set.remove(&session);
            state
                .ready_sessions
                .retain(|candidate| *candidate != session);
        }
    }
}

fn claim_ready_reflection(
    state: &mut WorkCoordinatorState,
    session: EvaluationSessionId,
) -> Option<ClaimedReflectionWork> {
    loop {
        let id = state
            .ready_reflection
            .get_mut(&session)
            .and_then(VecDeque::pop_front)?;
        state.ready_reflection_set.remove(&id);
        if state
            .ready_reflection
            .get(&session)
            .is_some_and(VecDeque::is_empty)
        {
            state.ready_reflection.remove(&session);
        }
        if let Some(claimed) = claim_reflection(state, id) {
            if state
                .ready_reflection
                .get(&session)
                .is_some_and(|ready| !ready.is_empty())
            {
                queue_session(state, session);
            }
            return Some(claimed);
        }
    }
}

fn claim_reflection(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
) -> Option<ClaimedReflectionWork> {
    let (task, demand_session, prior_block) = {
        let record = state.work.get_mut(&id)?;
        if !matches!(record.kind, WorkKind::Reflection(_))
            || !matches!(record.state, WorkState::Queued | WorkState::Blocked)
        {
            return None;
        }
        record.state = WorkState::Running;
        let demand_session = record.demand_session;
        let reflection = reflection_work_mut(record);
        (reflection.task, demand_session, reflection.block.take())
    };
    remove_ready_reflection(state, id);
    Some(ClaimedReflectionWork {
        id,
        task,
        demand_session,
        prior_block,
    })
}

fn reflection_state(record: &WorkRecord) -> ReflectionWorkState {
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
        WorkState::Terminalizing => ReflectionWorkState::Terminalizing,
    }
}

fn detach_reflection(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    remove_ready_reflection(state, id);
    let record = state
        .work
        .remove(&id)
        .expect("retired reflection work must remain registered");
    let WorkKind::Reflection(reflection) = record.kind else {
        panic!("reflection retirement must contain reflection work")
    };
    assert_eq!(
        state.reflection_by_task.remove(&reflection.task),
        Some(id),
        "reflection task index must agree with its work record"
    );
    if let Some(session_work) = state.work_by_session.get_mut(&record.demand_session) {
        session_work.remove(&id);
        if session_work.is_empty() {
            state.work_by_session.remove(&record.demand_session);
        }
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
) -> bool {
    {
        let Some(record) = state.work.get_mut(&registration.work) else {
            return false;
        };
        if !matches!(record.state, WorkState::Blocked)
            || record.subscription_epoch != registration.subscription_epoch
            || source.is_some_and(|source| {
                spark_work(record)
                    .dependency
                    .as_ref()
                    .is_none_or(|dependency| dependency.key() != source)
            })
        {
            return false;
        }
        record.state = WorkState::Queued;
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
        let spark = spark_work_mut(record);
        let demand = spark
            .demand
            .take()
            .expect("queued spark work must retain its demand");
        let prior_dependency = spark.dependency.take();
        return Some(ClaimedSparkWork {
            id,
            demand_session: record.demand_session,
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
    let record = state
        .work
        .remove(&id)
        .expect("terminalizing spark work must remain registered");
    state.ready_spark_set.remove(&id);
    state.ready_sparks.retain(|candidate| *candidate != id);
    if let Some(session_work) = state.work_by_session.get_mut(&record.demand_session) {
        session_work.remove(&id);
        if session_work.is_empty() {
            state.work_by_session.remove(&record.demand_session);
        }
    }
    let WorkKind::Spark(mut spark) = record.kind else {
        unreachable!("spark retirement must contain spark work")
    };
    Some(SparkRetirement {
        demand: spark
            .demand
            .take()
            .expect("non-running spark work must retain its demand"),
        dependencies: spark.dependency.take().into_iter().collect(),
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
                subscriptions: CompletionSubscriptions::for_test(
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
            self.subscriptions
                .publish(|| {
                    let _ = self.terminal.set(());
                    Ok::<_, std::convert::Infallible>(())
                })
                .expect("infallible test completion should publish");
        }

        fn is_terminal(&self) -> bool {
            self.terminal.get().is_some()
        }

        fn subscriber_count(&self) -> usize {
            self.subscriptions.len()
        }

        fn coordinator_is_live(&self) -> bool {
            self.subscriptions
                .coordinator
                .get()
                .and_then(Weak::upgrade)
                .is_some()
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
                assert!(record.control.close_reason.is_none());

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
                let spark = spark_work_mut(record);
                spark.demand = Some(claimed.demand);
                spark.dependency = dependency;
                record.subscription_epoch = record
                    .subscription_epoch
                    .checked_add(1)
                    .expect("evaluation work subscription epochs exhausted");
                record.state = WorkState::Blocked;
                let registration = WakeRegistration {
                    work: claimed.id,
                    subscription_epoch: record.subscription_epoch,
                };
                state.work_generation = state.work_generation.wrapping_add(1);
                (registration, obsolete_dependency)
            };

            let outcome = source.subscriptions.subscribe_with(
                self.runtime,
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
        let (coordinator, _executor, _session, claimed) = claimed_test_spark();
        let source_a = TestCompletionSource::new(&coordinator);
        let source_b = TestCompletionSource::new(&coordinator);
        let Ok(registration_a) = coordinator.park_claimed_test_spark(claimed, &source_a, || {})
        else {
            panic!("same-runtime completion source should accept the subscription")
        };

        assert!(coordinator.redeliver_test_registration(source_a.key(), registration_a));
        let CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("the exact source delivery should requeue the test spark")
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
        let (coordinator, _executor, _session, claimed) = claimed_test_spark();
        let source = TestCompletionSource::new(&coordinator);
        let Ok(first) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
            panic!("same-runtime completion source should accept the subscription")
        };

        assert!(coordinator.redeliver_test_registration(source.key(), first));
        let CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("the exact source delivery should requeue the test spark")
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
    fn foreign_promise_dependency_retires_work_without_subscribing() {
        let (coordinator, _executor, _session, claimed) = claimed_test_spark();
        let foreign_values = crate::core::CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        );
        let promise = PromisedValue::new(&foreign_values, "foreign spark promise");

        coordinator.release_spark(
            claimed,
            SparkWorkPoll::Blocked(WorkDependency::Promise(promise.clone())),
        );

        assert_eq!(promise.spark_subscription_count(), 0);
        assert_eq!(coordinator.retained_spark_count(), 0);
    }

    #[test]
    fn foreign_wait_dependency_retires_work_without_subscribing() {
        let (coordinator, _executor, _session, claimed) = claimed_test_spark();
        let (foreign_coordinator, _foreign_executor) = super::super::test_execution_resources(0)
            .expect("foreign execution resources should build");
        let foreign_session = EvaluationSession::shared(&foreign_coordinator);
        let producer = super::super::allocate_task_id(&foreign_session.values)
            .expect("foreign producer identity should allocate");
        let wait = super::super::allocate_wait_token(&foreign_session, producer)
            .expect("foreign wait identity should allocate");

        coordinator.release_spark(
            claimed,
            SparkWorkPoll::Blocked(WorkDependency::Wait(wait.clone())),
        );

        assert_eq!(wait.spark_subscription_count(), 0);
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
    fn coordinator_owns_the_reflection_lifecycle() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = EvaluationSession::shared(&coordinator);
        let task = super::super::allocate_task_id(&session.values)
            .expect("reflection task identity should allocate");
        let work = coordinator.reserve_reflection(&session, task);
        assert_eq!(
            coordinator.reflection_snapshots(session.id),
            vec![ReflectionWorkSnapshot {
                task,
                state: ReflectionWorkState::Reserved,
            }]
        );

        assert!(coordinator.activate_reflection(work));
        assert!(matches!(
            coordinator.reflection_snapshots(session.id).as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Queued,
                ..
            }]
        ));

        let claimed = coordinator
            .claim_ready_reflection(session.id)
            .expect("queued reflection work should be claimable");
        assert_eq!(claimed.id(), work);
        assert!(matches!(
            coordinator.reflection_snapshots(session.id).as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Running,
                ..
            }]
        ));

        let block = EvaluationTaskBlock {
            lazy: None,
            observed_generation: Some(7),
            error: None,
        };
        let release =
            coordinator.release_reflection(claimed, ReflectionWorkPoll::Blocked(block.clone()));
        assert!(release.made_progress);
        assert!(release.remains_blocked);
        assert!(!release.terminal);
        assert_eq!(
            coordinator.reflection_snapshots(session.id),
            vec![ReflectionWorkSnapshot {
                task,
                state: ReflectionWorkState::Blocked(block),
            }]
        );

        let claimed = coordinator
            .claim_blocked_reflection(session.id, &HashSet::new())
            .expect("blocked reflection work should be retryable");
        let release = coordinator.release_reflection(claimed, ReflectionWorkPoll::Terminal);
        assert!(release.terminal);
        assert!(matches!(
            coordinator.reflection_snapshots(session.id).as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Terminalizing,
                ..
            }]
        ));
        coordinator.retire_reflection(work);
        assert!(coordinator.reflection_snapshots(session.id).is_empty());
    }

    #[test]
    fn coordinator_cancels_reflection_reservations_without_polling() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = EvaluationSession::shared(&coordinator);
        let task = super::super::allocate_task_id(&session.values)
            .expect("reflection task identity should allocate");
        let work = coordinator.reserve_reflection(&session, task);

        assert_eq!(
            coordinator.request_reflection_cancellation(work),
            ReflectionCancellation::Terminalize
        );
        assert!(matches!(
            coordinator.reflection_snapshots(session.id).as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Terminalizing,
                ..
            }]
        ));
        coordinator.retire_reflection(work);
        assert_eq!(
            coordinator.request_reflection_cancellation(work),
            ReflectionCancellation::Late
        );
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
