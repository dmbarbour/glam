//! Runtime-owned work coordination independent of worker ownership.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::num::NonZeroU64;
use std::sync::{Arc, Condvar, Mutex, Weak};

#[cfg(test)]
use crate::core::LazyValue;
use crate::core::{CoreValueFactory, PromiseCell, PromiseId, PromisedValue};
#[cfg(test)]
use crate::runtime::RuntimeValueRoot;
use crate::runtime::{
    EvaluationRuntimeId, RuntimeFailureRoot, RuntimeIds, RuntimeMutationAdmission,
    RuntimeMutationAuthority, RuntimeMutationGuard,
};

#[cfg(test)]
use super::EvaluationSession;
use super::{
    EvaluationDemandState, EvaluationFailure, RuntimeObservationEpoch, RuntimeObservationState,
};

mod client_demand;
mod completion;
mod deferred;
mod reflection;
mod settlement;
mod spark;
mod task;
pub(crate) use client_demand::{
    ClaimedClientDemand, ClientDemandHandle, ClientDemandOperation, ClientDemandPoll,
    ClientDemandResult, ClientDemandSink, ClientDemandSnapshot, ClientDemandWork,
};
use client_demand::{
    ClientDemandRetirement, claim_ready_client_demand, detach_client_demand, queue_client_demand,
};
#[cfg(test)]
use completion::DependencyWakeBatch;
pub(crate) use completion::{
    CompletionSubscriptionOutcome, CompletionSubscriptions, CompletionWake, WakeRegistration,
    WorkDependencyKey,
};
pub(super) use deferred::{
    AbandonedDeferredWork, ClaimedDeferredWork, DeferredLazyCycleMember, DeferredProducer,
    DeferredWorkPoll, DeferredWorkReservation,
};
use deferred::{
    DeferredIndexes, DeferredWork, begin_deferred_abandonment, claim_deferred, deferred_work,
    deferred_work_mut,
};
#[cfg(test)]
pub(super) use reflection::ReflectionWorkSnapshot;
use reflection::{
    AbandonedReflectionWork, ReflectionIndexes, ReflectionWork, claim_reflection,
    insert_task_failure, reflection_work, reflection_work_mut, remove_ready_reflection,
};
pub(super) use reflection::{
    ClaimedReflectionWork, ReflectionCancellation, ReflectionWorkPoll, ReflectionWorkState,
};
pub(crate) use settlement::{
    RuntimeCoordinatorReadiness, RuntimeDeadlockWorkSnapshot, RuntimeDependencySnapshot,
    RuntimeExitSnapshot, RuntimeWorkKindSnapshot, RuntimeWorkStateSnapshot,
    ValidatedRuntimeSettlementPlan,
};
#[cfg(test)]
use spark::spark_work_mut;
pub(crate) use spark::{ClaimedSparkWork, SparkWork, SparkWorkPoll};
use spark::{SparkRetirement, claim_ready_spark, detach_spark, queue_spark};
pub(crate) use task::{
    EvaluationExitBlock, EvaluationMachinePoll, EvaluationSessionId, EvaluationTaskBlock,
    EvaluationTaskCancellation, EvaluationTaskHandle, EvaluationTaskId, EvaluationTaskMachine,
    EvaluationTaskStatus, EvaluationWaitPoll, EvaluationWaitTerminal, EvaluationWaitToken,
    ExitIntent, InitialTaskDisposition, LocalPromiseOwner, PendingTaskPolicy,
    PreparedEvaluationTask, PromiseProducerObligation, PromiseProducerPublication,
    ReflectionTaskResultPolicy, RuntimeFailureLedger, TaskFailureLedger, TaskStatusPublisher,
    TaskStatusWake,
};
use task::{TaskStatusUpdate, TaskTerminalPublisher, terminal_task_status};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EvaluationWorkId(NonZeroU64);

impl EvaluationWorkId {
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

#[cfg(test)]
pub(crate) fn test_wake_registration() -> WakeRegistration {
    WakeRegistration {
        work: EvaluationWorkId(NonZeroU64::MAX),
        subscription_epoch: 0,
    }
}

#[derive(Default)]
struct WorkControl {
    close_reason: Option<WorkCloseReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkCloseReason {
    ExplicitCancellation,
    ClientDemandAbandoned,
    DemandSessionClosed,
    ExecutorShutdown,
}

enum ProducerSettlementObligation {
    ReflectionTask(TaskTerminalPublisher),
    DeferredClaim {
        wait: EvaluationWaitToken,
        producer: DeferredProducer,
    },
}

/// Producer state which must be disposed before a work record retires.
///
/// Ordinary terminalization consumes the static producer entry once, publishes
/// every task terminal surface, then settles dynamically registered promises
/// before the work record may retire.
#[derive(Default)]
struct SettlementObligations {
    producer: Option<ProducerSettlementObligation>,
    owned_promises: Vec<TaskOwnedPromiseObligation>,
    client_sink: Option<ClientDemandSink>,
}

#[derive(Clone)]
struct TaskOwnedPromiseObligation {
    promise: PromiseId,
    cell: Weak<PromiseCell>,
    wait: EvaluationWaitToken,
}

impl SettlementObligations {
    fn reflection_task(wait: EvaluationWaitToken) -> Self {
        Self {
            producer: Some(ProducerSettlementObligation::ReflectionTask(
                TaskTerminalPublisher::new(wait),
            )),
            owned_promises: Vec::new(),
            client_sink: None,
        }
    }

    fn deferred_claim(wait: EvaluationWaitToken, producer: DeferredProducer) -> Self {
        Self {
            producer: Some(ProducerSettlementObligation::DeferredClaim { wait, producer }),
            owned_promises: Vec::new(),
            client_sink: None,
        }
    }

    fn client_demand(sink: ClientDemandSink) -> Self {
        Self {
            producer: None,
            owned_promises: Vec::new(),
            client_sink: Some(sink),
        }
    }

    fn take_producer(&mut self) -> Option<ProducerSettlementObligation> {
        self.producer.take()
    }

    fn take_client_sink(&mut self) -> Option<ClientDemandSink> {
        self.client_sink.take()
    }

    fn task_publisher_mut(&mut self) -> Option<&mut TaskTerminalPublisher> {
        match self.producer.as_mut()? {
            ProducerSettlementObligation::ReflectionTask(publisher) => Some(publisher),
            ProducerSettlementObligation::DeferredClaim { .. } => None,
        }
    }

    fn add_owned_promise(&mut self, obligation: TaskOwnedPromiseObligation) {
        self.owned_promises.push(obligation);
    }

    fn take_owned_promise(
        &mut self,
        wait: &EvaluationWaitToken,
        promise: PromiseId,
    ) -> Option<TaskOwnedPromiseObligation> {
        let index = self
            .owned_promises
            .iter()
            .position(|obligation| obligation.wait == *wait && obligation.promise == promise)?;
        Some(self.owned_promises.swap_remove(index))
    }

    fn is_empty(&self) -> bool {
        self.producer.is_none() && self.owned_promises.is_empty() && self.client_sink.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkState {
    Dormant,
    Reserved,
    Queued,
    Running,
    Blocked,
    ExitWaiting,
    Terminalizing,
}

#[derive(Clone)]
pub(crate) enum WorkDependency {
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

    /// The producer wait through which scheduler graph traversal can continue.
    ///
    /// Resolver-owned promises have no producer edge. Task-owned promises
    /// project through the producer obligation while retaining the promise as
    /// the exact completion source in the machine block.
    pub(super) fn producer_wait(&self) -> Option<&EvaluationWaitToken> {
        match self {
            Self::Wait(wait) => Some(wait),
            Self::Promise(promise) => promise.task().map(|task| task.wait()),
            #[cfg(test)]
            Self::Test(_) => None,
        }
    }

    pub(super) fn into_wait(self) -> Option<EvaluationWaitToken> {
        match self {
            Self::Wait(wait) => Some(wait),
            Self::Promise(_) => None,
            #[cfg(test)]
            Self::Test(_) => None,
        }
    }

    fn subscribe_work(
        &self,
        runtime: EvaluationRuntimeId,
        registration: WakeRegistration,
    ) -> CompletionSubscriptionOutcome {
        match self {
            Self::Wait(wait) => wait.subscribe_work(runtime, registration),
            Self::Promise(promise) => promise.subscribe_work(runtime, registration),
            #[cfg(test)]
            Self::Test(_) => {
                unreachable!("synthetic completion sources install their own subscription")
            }
        }
    }

    fn unsubscribe_work(&self, registration: WakeRegistration) -> bool {
        match self {
            Self::Wait(wait) => wait.unsubscribe_work(registration),
            Self::Promise(promise) => promise.unsubscribe_work(registration),
            #[cfg(test)]
            Self::Test(_) => false,
        }
    }

    fn is_terminal(&self) -> bool {
        match self {
            Self::Wait(wait) => wait.terminal_poll().is_some(),
            Self::Promise(promise) => promise.assignment().is_some(),
            #[cfg(test)]
            Self::Test(_) => false,
        }
    }

    fn abandon(self) {
        match self {
            Self::Wait(wait) => wait.abandon_deferred_producer(),
            Self::Promise(_) => {}
            #[cfg(test)]
            Self::Test(_) => {}
        }
    }
}

impl PartialEq for WorkDependency {
    fn eq(&self, other: &Self) -> bool {
        self.same_source(other)
    }
}

impl Eq for WorkDependency {}

impl fmt::Debug for WorkDependency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wait(wait) => formatter.debug_tuple("Wait").field(wait).finish(),
            Self::Promise(promise) => formatter.debug_tuple("Promise").field(promise).finish(),
            #[cfg(test)]
            Self::Test(dependency) => formatter.debug_tuple("Test").field(dependency).finish(),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct TestWorkDependency {
    runtime: EvaluationRuntimeId,
    id: NonZeroU64,
}

enum WorkKind {
    Spark(SparkWork),
    Reflection(ReflectionWork),
    Deferred(DeferredWork),
    ClientDemand(ClientDemandWork),
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

/// Temporary strong route from one detached work claim to its demand domain.
///
/// The coordinator registry remains weak. A claim upgrades that registry only
/// while its machine or operation is detached, so later scheduler admission
/// has one authoritative source for the matching value domain without adding
/// another durable coordinator-to-domain edge.
pub(super) struct ClaimedDemandSession {
    demand: Arc<EvaluationDemandState>,
}

impl ClaimedDemandSession {
    fn registered(
        state: &WorkCoordinatorState,
        session: EvaluationSessionId,
        runtime: EvaluationRuntimeId,
    ) -> Option<Self> {
        let demand = state.demand_sessions.get(&session)?.upgrade()?;
        if demand.is_closed() {
            return None;
        }
        if demand.id != session || demand.values.runtime_id() != runtime {
            return None;
        }
        Some(Self { demand })
    }

    pub(in crate::evaluation) fn id(&self) -> EvaluationSessionId {
        self.demand.id
    }

    pub(in crate::evaluation) fn demand(&self) -> Arc<EvaluationDemandState> {
        self.demand.clone()
    }

    pub(in crate::evaluation) fn values(&self) -> &CoreValueFactory {
        &self.demand.values
    }

    pub(in crate::evaluation) fn assert_runtime(&self, runtime: EvaluationRuntimeId) {
        assert_eq!(
            self.values().runtime_id(),
            runtime,
            "claimed demand session must match the polling coordinator runtime"
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservationRegistration {
    wake: WakeRegistration,
    observed_epoch: RuntimeObservationEpoch,
}

pub(super) enum ClaimedTaskWork {
    Reflection(ClaimedReflectionWork),
    Deferred(ClaimedDeferredWork),
}

impl ClaimedTaskWork {
    pub(in crate::evaluation) fn demand(&self) -> &ClaimedDemandSession {
        match self {
            Self::Reflection(work) => &work.demand,
            Self::Deferred(work) => &work.demand,
        }
    }
}

pub(super) struct SessionClosureWork {
    pub(super) reflection: Vec<AbandonedReflectionWork>,
    pub(super) deferred: Vec<AbandonedDeferredWork>,
    retired_sparks: Vec<SparkRetirement>,
    client_demands: Vec<ClientDemandRetirement>,
}

impl SessionClosureWork {
    fn finish_sparks(&mut self) {
        for record in self.retired_sparks.drain(..) {
            record.abandon();
        }
    }

    fn finish_client_demands(&mut self) {
        for record in self.client_demands.drain(..) {
            record.finish();
        }
    }

    pub(super) fn finish(mut self) {
        self.finish_sparks();
        self.finish_client_demands();
    }
}

impl Drop for SessionClosureWork {
    fn drop(&mut self) {
        self.finish_sparks();
        self.finish_client_demands();
    }
}

impl ClaimedClientDemand {
    pub(super) fn poll(&mut self, poll_context: &super::EvaluationPollContext) -> ClientDemandPoll {
        assert_eq!(
            self.operation
                .as_ref()
                .expect("claimed client demand must retain its operation")
                .runtime_id(),
            self.demand.values().runtime_id(),
            "client-demand operation must match its claimed demand session"
        );
        let context = super::EvalContext::for_client_demand(self.demand.demand());
        let operation = self
            .operation
            .as_mut()
            .expect("claimed client demand must retain its operation");
        operation.poll(poll_context, &context)
    }
}

#[derive(Default)]
struct WorkCoordinatorState {
    demand_sessions: HashMap<EvaluationSessionId, Weak<EvaluationDemandState>>,
    failures: RuntimeFailureLedger,
    pending_failure_reports: RuntimeFailureLedger,
    work: HashMap<EvaluationWorkId, WorkRecord>,
    work_by_session: HashMap<EvaluationSessionId, HashSet<EvaluationWorkId>>,
    ready_tasks: VecDeque<EvaluationWorkId>,
    ready_task_set: HashSet<EvaluationWorkId>,
    ready_sparks: VecDeque<EvaluationWorkId>,
    ready_spark_set: HashSet<EvaluationWorkId>,
    ready_client_demands: VecDeque<EvaluationWorkId>,
    ready_client_demand_set: HashSet<EvaluationWorkId>,
    reflection: ReflectionIndexes,
    deferred: DeferredIndexes,
    promise_by_wait: HashMap<EvaluationWaitToken, EvaluationWorkId>,
    observation_waiters: HashMap<EvaluationWorkId, ObservationRegistration>,
    spark_workers: usize,
    prefer_spark: bool,
    work_generation: u64,
}

/// Runtime-owned scheduling state shared by serial and worker execution.
///
/// Spark payloads and reflection/deferred lifecycle records, including their
/// claimable machine slots, have stable work records here. Session reporting
/// registrations retain only weak demand-state liveness and closure state.
pub(crate) struct EvaluationWorkCoordinator {
    runtime: EvaluationRuntimeId,
    #[allow(
        dead_code,
        reason = "I4F.2d.1 installs weak publication authority before the I4F.2d.2 root switch"
    )]
    values: crate::core::RuntimeValueObserver,
    ids: Arc<RuntimeIds>,
    admission: Arc<RuntimeMutationAdmission>,
    observations: Arc<RuntimeObservationState>,
    state: Mutex<WorkCoordinatorState>,
    work_available: Condvar,
    #[cfg(test)]
    test_values: Option<CoreValueFactory>,
    #[cfg(test)]
    terminal_publication_probe: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

pub(super) enum CoordinatorSelection {
    Task(ClaimedTaskWork),
    Spark(ClaimedSparkWork),
    ClientDemand(ClaimedClientDemand),
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
            .field("session_count", &state.demand_sessions.len())
            .field("ready_task_count", &state.ready_task_set.len())
            .field("work_count", &state.work.len())
            .field("work_generation", &state.work_generation)
            .finish_non_exhaustive()
    }
}

impl EvaluationWorkCoordinator {
    pub(crate) fn new(
        values: &CoreValueFactory,
        admission: Arc<RuntimeMutationAdmission>,
        observations: Arc<RuntimeObservationState>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime: values.runtime_id(),
            values: values.runtime_value_observer(),
            ids: values.ids().clone(),
            admission,
            observations,
            state: Mutex::new(WorkCoordinatorState::default()),
            work_available: Condvar::new(),
            #[cfg(test)]
            test_values: None,
            #[cfg(test)]
            terminal_publication_probe: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        values: CoreValueFactory,
        admission: Arc<RuntimeMutationAdmission>,
    ) -> Arc<Self> {
        let coordinator = Arc::new(Self {
            runtime: values.runtime_id(),
            values: values.runtime_value_observer(),
            ids: values.ids().clone(),
            admission,
            observations: RuntimeObservationState::new(),
            state: Mutex::new(WorkCoordinatorState::default()),
            work_available: Condvar::new(),
            test_values: Some(values.clone()),
            terminal_publication_probe: Mutex::new(None),
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

    #[allow(
        dead_code,
        reason = "I4F.2d.1 installs weak publication authority before the I4F.2d.2 root switch"
    )]
    pub(crate) fn value_observer(&self) -> crate::core::RuntimeValueObserver {
        self.values.clone()
    }

    #[cfg(test)]
    pub(crate) fn shared_mutation_admission(&self) -> Arc<RuntimeMutationAdmission> {
        self.admission.clone()
    }

    #[cfg(test)]
    pub(crate) fn shared_observations(&self) -> Arc<RuntimeObservationState> {
        self.observations.clone()
    }

    #[cfg(test)]
    pub(super) fn runtime_locks_are_free(&self) -> bool {
        self.state.try_lock().is_ok() && self.admission.try_settlement_guard().is_some()
    }

    #[cfg(test)]
    pub(super) fn settlement_admission_is_free(&self) -> bool {
        self.admission.try_settlement_guard().is_some()
    }

    #[cfg(test)]
    pub(super) fn set_terminal_publication_probe(&self, probe: impl FnOnce() + Send + 'static) {
        *self
            .terminal_publication_probe
            .lock()
            .expect("terminal publication probe was poisoned") = Some(Box::new(probe));
    }

    #[cfg(test)]
    fn run_terminal_publication_probe(&self) {
        if let Some(probe) = self
            .terminal_publication_probe
            .lock()
            .expect("terminal publication probe was poisoned")
            .take()
        {
            probe();
        }
    }

    pub(crate) fn current_observation_epoch(&self) -> RuntimeObservationEpoch {
        self.observations.current()
    }

    /// Returns one persistent owner bucket from the runtime failure ledger.
    pub(super) fn failure_snapshot(&self, session: EvaluationSessionId) -> TaskFailureLedger {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .failures
            .get(&session)
            .cloned()
            .unwrap_or_else(TaskFailureLedger::new_sync)
    }

    #[cfg(test)]
    pub(crate) fn failure_ledger_snapshot(&self) -> RuntimeFailureLedger {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .failures
            .clone()
    }

    /// Captures the persistent failure ledger and commits every not-yet-
    /// reported failure to the current settlement report.
    ///
    /// The persistent ledger remains authoritative until explicit
    /// acknowledgement. Only the separate reporting obligations move into the
    /// report, so later settlements do not ask a presentation layer to
    /// remember which failures it has already rendered.
    pub(crate) fn failure_ledgers_for_settlement(
        &self,
    ) -> (RuntimeFailureLedger, RuntimeFailureLedger) {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let pending = std::mem::replace(
            &mut state.pending_failure_reports,
            RuntimeFailureLedger::new_sync(),
        );
        (state.failures.clone(), pending)
    }

    #[cfg(test)]
    pub(crate) fn publish_runtime_observation(&self) {
        let mutation = self.admission.mutation_guard();
        let epoch = self.observations.advance();
        let changed = self.publish_runtime_observation_guarded(&mutation, epoch);
        drop(mutation);
        self.observations.notify_all();
        self.notify_runtime_observation(changed);
    }

    pub(crate) fn mutation_guard(&self) -> RuntimeMutationGuard<'_> {
        self.admission.mutation_guard()
    }

    pub(super) fn register_demand(&self, demand: &Arc<EvaluationDemandState>) {
        debug_assert_eq!(demand.values.runtime_id(), self.runtime);
        self.publish_transition(|state| {
            let replaced = state
                .demand_sessions
                .insert(demand.id, Arc::downgrade(demand));
            assert!(
                replaced.is_none(),
                "evaluation session identities must be unique within a runtime"
            );
        });
    }

    /// Closes one demand session in a single guarded coordinator transition.
    ///
    /// Non-running task work enters terminalization immediately. Running work
    /// retains its exclusive claim and its first close reason until release.
    /// Spark dependencies are abandoned only after runtime locks and mutation
    /// admission have been released.
    pub(super) fn close_session(&self, session: EvaluationSessionId) -> SessionClosureWork {
        let mutation = self.admission.mutation_guard();
        let (reflection, deferred, retired_sparks, client_demands, changed) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let initial_generation = state.work_generation;
            let work = state
                .work_by_session
                .get(&session)
                .into_iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            let mut reflection = Vec::new();
            let mut deferred = Vec::new();
            let mut retired_sparks = Vec::new();
            let mut client_demands = Vec::new();
            let mut changed = false;
            for id in work {
                let Some(record) = state.work.get(&id) else {
                    continue;
                };
                if matches!(record.state, WorkState::Terminalizing) {
                    // The operation which published terminalization owns its
                    // producer settlement and retirement tail.
                    continue;
                }
                let running = matches!(record.state, WorkState::Running);
                match &record.kind {
                    WorkKind::Reflection(reflection_work) => {
                        let task = reflection_work.task;
                        let cancel = matches!(
                            record.control.close_reason,
                            Some(WorkCloseReason::ExplicitCancellation)
                        );
                        let record = state
                            .work
                            .get_mut(&id)
                            .expect("indexed reflection work must remain registered");
                        if record.control.close_reason.is_none() {
                            debug_assert!(!cancel);
                            record.control.close_reason =
                                Some(WorkCloseReason::DemandSessionClosed);
                            changed = true;
                        }
                        if running {
                            continue;
                        }
                        record.state = WorkState::Terminalizing;
                        state.observation_waiters.remove(&id);
                        remove_ready_reflection(&mut state, id);
                        reflection.push(AbandonedReflectionWork { id, task, cancel });
                        changed = true;
                    }
                    WorkKind::Deferred(_) => {
                        let record = state
                            .work
                            .get_mut(&id)
                            .expect("indexed deferred work must remain registered");
                        if record.control.close_reason.is_none() {
                            record.control.close_reason =
                                Some(WorkCloseReason::DemandSessionClosed);
                            changed = true;
                        }
                        if running {
                            continue;
                        }
                        deferred.push(begin_deferred_abandonment(&mut state, id));
                        changed = true;
                    }
                    WorkKind::Spark(_) => {
                        if running {
                            let record = state
                                .work
                                .get_mut(&id)
                                .expect("indexed running spark work must remain registered");
                            if record.control.close_reason.is_none() {
                                record.control.close_reason =
                                    Some(WorkCloseReason::DemandSessionClosed);
                                changed = true;
                            }
                        } else if let Some(record) = detach_spark(&mut state, id) {
                            retired_sparks.push(record);
                            changed = true;
                        }
                    }
                    WorkKind::ClientDemand(_) => {
                        let record = state
                            .work
                            .get_mut(&id)
                            .expect("indexed client demand must remain registered");
                        if record.control.close_reason.is_none() {
                            record.control.close_reason =
                                Some(WorkCloseReason::DemandSessionClosed);
                            changed = true;
                        }
                        if running {
                            continue;
                        }
                        client_demands.push(detach_client_demand(
                            &mut state,
                            id,
                            None,
                            None,
                            ClientDemandResult::Abandoned,
                        ));
                        changed = true;
                    }
                }
            }
            changed |= prune_closed_session_registration(&mut state, session);
            if changed {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            (
                reflection,
                deferred,
                retired_sparks,
                client_demands,
                state.work_generation != initial_generation,
            )
        };
        drop(mutation);
        if changed {
            self.work_available.notify_all();
        }
        SessionClosureWork {
            reflection,
            deferred,
            retired_sparks,
            client_demands,
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

    #[cfg(test)]
    pub(crate) fn cache_builder_scheduler_snapshot(&self) -> (u64, usize, usize) {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        (
            state.work_generation,
            state.demand_sessions.len(),
            state.work.len(),
        )
    }

    pub(super) fn session_has_ready_task(&self, session: EvaluationSessionId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state.ready_task_set.iter().any(|id| {
            state
                .work
                .get(id)
                .is_some_and(|record| record.demand_session == session)
        })
    }

    pub(super) fn select(&self) -> CoordinatorSelection {
        let mutation = self.admission.mutation_guard();
        let (selection, changed) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let initial_generation = state.work_generation;
            let had_ready_task = !state.ready_tasks.is_empty();
            let had_ready_spark = !state.ready_sparks.is_empty();
            let had_ready_client = !state.ready_client_demands.is_empty();
            let selection = claim_ready_client_demand(&mut state, self.runtime)
                .map(CoordinatorSelection::ClientDemand)
                .unwrap_or_else(|| {
                    if state.prefer_spark {
                        claim_ready_spark(&mut state, self.runtime)
                            .map(CoordinatorSelection::Spark)
                            .or_else(|| {
                                claim_ready_task(&mut state, self.runtime, None)
                                    .map(CoordinatorSelection::Task)
                            })
                            .unwrap_or(CoordinatorSelection::None)
                    } else {
                        claim_ready_task(&mut state, self.runtime, None)
                            .map(CoordinatorSelection::Task)
                            .or_else(|| {
                                claim_ready_spark(&mut state, self.runtime)
                                    .map(CoordinatorSelection::Spark)
                            })
                            .unwrap_or(CoordinatorSelection::None)
                    }
                });
            match selection {
                CoordinatorSelection::Task(_) => state.prefer_spark = true,
                CoordinatorSelection::Spark(_) => state.prefer_spark = false,
                CoordinatorSelection::ClientDemand(_) | CoordinatorSelection::None => {}
            }
            if !matches!(selection, CoordinatorSelection::None)
                || had_ready_task
                || had_ready_spark
                || had_ready_client
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

    /// Claims one lifecycle-bearing work item for the host runtime pump.
    ///
    /// Unlike worker selection, this deliberately ignores sparks. Sparks are
    /// best-effort hints which only workers execute; the host pump normalizes
    /// any unclaimed spark records separately once useful work is quiescent.
    pub(super) fn select_runtime_pump(&self) -> CoordinatorSelection {
        let mutation = self.admission.mutation_guard();
        let (selection, changed) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let initial_generation = state.work_generation;
            let had_ready_task = !state.ready_tasks.is_empty();
            let had_ready_client = !state.ready_client_demands.is_empty();
            let selection = claim_ready_client_demand(&mut state, self.runtime)
                .map(CoordinatorSelection::ClientDemand)
                .or_else(|| {
                    claim_ready_task(&mut state, self.runtime, None).map(CoordinatorSelection::Task)
                })
                .unwrap_or(CoordinatorSelection::None);
            if !matches!(selection, CoordinatorSelection::None)
                || had_ready_task
                || had_ready_client
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
}

impl EvaluationWorkCoordinator {
    /// Restores coordinator-claimed task work which was selected but not
    /// polled. This is used only when an executor begins shutdown between
    /// selection and polling. Both task kinds return their detached machine to
    /// the coordinator record before becoming claimable again.
    pub(super) fn requeue_unpolled_task(&self, claimed: ClaimedTaskWork) {
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let id = match claimed {
                ClaimedTaskWork::Reflection(mut claim) => {
                    let record = state
                        .work
                        .get_mut(&claim.id)
                        .expect("unpolled reflection work must remain registered");
                    assert!(matches!(record.state, WorkState::Running));
                    let reflection = reflection_work_mut(record);
                    assert!(
                        reflection.machine.is_none(),
                        "running reflection work must have detached its machine"
                    );
                    reflection.machine = claim.machine.take();
                    reflection.block = claim.prior_block;
                    record.state = WorkState::Queued;
                    claim.id
                }
                ClaimedTaskWork::Deferred(mut claimed) => {
                    let record = state
                        .work
                        .get_mut(&claimed.id)
                        .expect("unpolled deferred work must remain registered");
                    assert!(matches!(record.state, WorkState::Running));
                    let deferred = deferred_work_mut(record);
                    assert!(
                        deferred.machine.is_none(),
                        "running deferred work must have detached its machine"
                    );
                    deferred.machine = claimed.machine.take();
                    deferred.block = claimed.prior_block;
                    record.state = WorkState::Queued;
                    claimed.id
                }
            };
            queue_task(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_all();
    }

    pub(super) fn claim_ready_task_for_session(
        &self,
        session: EvaluationSessionId,
    ) -> Option<ClaimedTaskWork> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let claimed = claim_ready_task(&mut state, self.runtime, Some(session));
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

    /// Claims one exact task dependency and detaches its opaque machine from
    /// the coordinator record. All reporting identity remains in the stable
    /// work record while the machine is claimed.
    pub(super) fn claim_task(&self, task: EvaluationTaskId) -> Option<ClaimedTaskWork> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let id = state
                .reflection
                .by_task
                .get(&task)
                .or_else(|| state.deferred.by_task.get(&task))
                .copied()?;
            let work = match state.work.get(&id)?.kind {
                WorkKind::Reflection(_) => claim_reflection_task(&mut state, self.runtime, id),
                WorkKind::Deferred(_) => claim_deferred(&mut state, self.runtime, id, false)
                    .map(ClaimedTaskWork::Deferred),
                WorkKind::Spark(_) | WorkKind::ClientDemand(_) => None,
            }?;
            state.work_generation = state.work_generation.wrapping_add(1);
            Some(work)
        };
        drop(mutation);
        if claimed.is_some() {
            self.work_available.notify_all();
        }
        claimed
    }

    pub(super) fn producer_for_wait(&self, wait: &EvaluationWaitToken) -> Option<EvaluationTaskId> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        if let Some(id) = state.promise_by_wait.get(wait) {
            return state.work.get(id).and_then(task_for_record);
        }
        if let Some(id) = state.deferred.by_wait.get(wait) {
            return state.work.get(id).map(|record| deferred_work(record).task);
        }
        state
            .reflection
            .by_wait
            .get(wait)
            .and_then(|id| state.work.get(id))
            .map(|record| reflection_work(record).task)
    }

    pub(super) fn register_task_promise(
        &self,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
        promise: &Arc<PromiseCell>,
    ) -> Result<EvaluationWorkId, Arc<str>> {
        debug_assert_eq!(wait.runtime_id(), self.runtime);
        let mutation = self.admission.mutation_guard();
        let work = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let work = state
                .reflection
                .by_task
                .get(&task)
                .or_else(|| state.deferred.by_task.get(&task))
                .copied()
                .ok_or_else(|| {
                    Arc::<str>::from(format!(
                        "task {} has no active work record for its promise",
                        task.get()
                    ))
                })?;
            let record = state
                .work
                .get_mut(&work)
                .expect("indexed promise producer work must remain registered");
            if record.control.close_reason.is_some() {
                return Err(Arc::from("evaluation demand session is closed"));
            }
            if !matches!(record.state, WorkState::Reserved | WorkState::Running) {
                return Err(Arc::from(
                    "a promise cannot be added after its producer stopped running",
                ));
            }
            record
                .obligations
                .add_owned_promise(TaskOwnedPromiseObligation {
                    promise: promise.id(),
                    cell: Arc::downgrade(promise),
                    wait: wait.clone(),
                });
            assert!(
                state.promise_by_wait.insert(wait, work).is_none(),
                "evaluation wait tokens must be unique"
            );
            state.work_generation = state.work_generation.wrapping_add(1);
            work
        };
        drop(mutation);
        self.work_available.notify_all();
        Ok(work)
    }

    pub(super) fn complete_task_promise_guarded(
        &self,
        _mutation: &dyn RuntimeMutationAuthority,
        work: EvaluationWorkId,
        wait: &EvaluationWaitToken,
        promise: PromiseId,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        if state.promise_by_wait.get(wait).copied() != Some(work) {
            return false;
        }
        let record = state
            .work
            .get_mut(&work)
            .expect("indexed promise producer work must remain registered");
        let obligation = record
            .obligations
            .take_owned_promise(wait, promise)
            .expect("promise wait index must agree with its producer obligation");
        debug_assert_eq!(obligation.promise, promise);
        assert_eq!(state.promise_by_wait.remove(wait), Some(work));
        state.work_generation = state.work_generation.wrapping_add(1);
        true
    }

    /// Consumes one terminalizing work record's producer obligation and
    /// publishes its failure-ledger decision, wait terminal, and protected
    /// status query under one runtime mutation admission before the record may
    /// retire.
    pub(super) fn settle_terminal_work(
        self: &Arc<Self>,
        work: EvaluationWorkId,
        terminal: EvaluationWaitTerminal,
        promise_failure: Arc<EvaluationFailure>,
    ) -> EvaluationWaitTerminal {
        let mutation = self.admission.mutation_guard();
        let (producer, status_update, promises) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let (producer, failure, status_update, promises) = {
                let record = state
                    .work
                    .get_mut(&work)
                    .expect("terminalizing work must remain registered");
                assert!(matches!(record.state, WorkState::Terminalizing));
                let failure = match (&record.kind, &terminal) {
                    (WorkKind::Reflection(reflection), EvaluationWaitTerminal::Failed(error))
                        if !reflection.failure_reporting.acknowledged =>
                    {
                        Some((
                            reflection.failure_reporting.owner_session,
                            reflection.task,
                            error.clone(),
                        ))
                    }
                    _ => None,
                };
                let mut producer = record
                    .obligations
                    .take_producer()
                    .expect("work producer obligations must be consumed exactly once");
                let status_update = match &mut producer {
                    ProducerSettlementObligation::ReflectionTask(publisher) => {
                        publisher.update_status(terminal_task_status(&terminal), true)
                    }
                    ProducerSettlementObligation::DeferredClaim { .. } => Vec::new(),
                };
                let promises = record.obligations.owned_promises.clone();
                (producer, failure, status_update, promises)
            };
            if let Some((owner, task, failure)) = failure {
                insert_task_failure(&mut state.failures, owner, task, failure.clone());
                insert_task_failure(&mut state.pending_failure_reports, owner, task, failure);
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            (producer, status_update, promises)
        };
        let wait = match &producer {
            ProducerSettlementObligation::ReflectionTask(publisher) => &publisher.wait,
            ProducerSettlementObligation::DeferredClaim { wait, producer } => {
                let _producer = producer.id();
                wait
            }
        };
        let (terminal, wake) = wait.publish_terminal_guarded(self, &mutation, terminal);
        let mut completion_wakes = vec![wake];
        let mut status_wakes = Vec::with_capacity(status_update.len());
        let mut status_publishers = Vec::with_capacity(status_update.len());
        for (publisher, status) in status_update {
            debug_assert_eq!(status, terminal_task_status(&terminal));
            status_wakes.push(publisher.publish_guarded(&mutation, status));
            status_publishers.push(publisher);
        }
        let mut promise_publications = Vec::with_capacity(promises.len());
        for obligation in promises {
            if let Some(promise) = obligation.cell.upgrade() {
                let (producer, completion) = promise
                    .publish_guarded(self, &mutation, Err(promise_failure.clone()))
                    .unwrap_or_else(|_| {
                        panic!(
                            "a terminalizing task-owned promise must remain unresolved until settlement"
                        )
                    });
                promise_publications.push(producer);
                completion_wakes.push(completion);
            } else {
                assert!(self.complete_task_promise_guarded(
                    &mutation,
                    work,
                    &obligation.wait,
                    obligation.promise,
                ));
                let (_, wake) = obligation.wait.publish_terminal_guarded(
                    self,
                    &mutation,
                    EvaluationWaitTerminal::Failed(RuntimeFailureRoot::from_runtime(
                        obligation.wait.runtime_id(),
                        promise_failure.clone(),
                    )),
                );
                completion_wakes.push(wake);
            }
        }
        {
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get(&work)
                .expect("settled work must remain registered for reporting cleanup");
            assert!(
                record.obligations.is_empty(),
                "terminal settlement must consume every work obligation"
            );
        }

        #[cfg(test)]
        self.run_terminal_publication_probe();
        drop(mutation);

        // The deferred producer clone, exact wakes, status publishers, and any
        // values they release are disposed only after coordinator/component
        // locks and mutation admission have been released.
        drop(producer);
        // Deliver lifecycle/status callbacks before waking parked completion
        // subscribers. The terminal cells are already authoritative, so a
        // direct poller may observe them before either notification; callback
        // completion is not part of the semantic terminal state.
        for status_wake in status_wakes {
            status_wake.notify();
        }
        for wake in completion_wakes {
            wake.notify();
        }
        for publication in promise_publications {
            publication.notify();
        }
        drop(status_publishers);
        terminal
    }

    pub(super) fn task_dependency(&self, task: EvaluationTaskId) -> Option<WorkDependency> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let id = state
            .reflection
            .by_task
            .get(&task)
            .or_else(|| state.deferred.by_task.get(&task))?;
        let record = state.work.get(id)?;
        match &record.kind {
            WorkKind::Reflection(work) => work.block.as_ref(),
            WorkKind::Deferred(work) => work.block.as_ref(),
            WorkKind::Spark(_) | WorkKind::ClientDemand(_) => None,
        }
        .and_then(|block| block.dependency.clone())
    }

    pub(super) fn task_observed_epoch(
        &self,
        task: EvaluationTaskId,
    ) -> Option<RuntimeObservationEpoch> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let id = state
            .reflection
            .by_task
            .get(&task)
            .or_else(|| state.deferred.by_task.get(&task))?;
        state.work.get(id).and_then(task_observation_epoch)
    }

    pub(super) fn task_is_claimable(&self, task: EvaluationTaskId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let id = state
            .reflection
            .by_task
            .get(&task)
            .or_else(|| state.deferred.by_task.get(&task));
        id.and_then(|id| state.work.get(id))
            .is_some_and(|record| matches!(record.state, WorkState::Dormant | WorkState::Queued))
    }

    pub(super) fn task_is_busy(&self, task: EvaluationTaskId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let id = state
            .reflection
            .by_task
            .get(&task)
            .or_else(|| state.deferred.by_task.get(&task));
        id.and_then(|id| state.work.get(id)).is_some_and(|record| {
            matches!(
                record.state,
                WorkState::Reserved | WorkState::Running | WorkState::Terminalizing
            )
        })
    }

    pub(super) fn target_has_running_producer(&self, target: &EvaluationWaitToken) -> bool {
        let mut seen = HashSet::new();
        let mut wait = target.clone();
        while let Some(task) = self.producer_for_wait(&wait) {
            if !seen.insert(task) {
                return false;
            }
            if self.task_is_busy(task) {
                return true;
            }
            let Some(dependency) = self.task_dependency(task) else {
                return false;
            };
            let Some(dependency_wait) = dependency.producer_wait() else {
                return false;
            };
            wait = dependency_wait.clone();
        }
        false
    }

    pub(super) fn dependency_observes_runtime(&self, target: &EvaluationWaitToken) -> bool {
        let mut seen = HashSet::new();
        let mut wait = target.clone();
        while seen.insert(wait.get()) {
            let Some(task) = self.producer_for_wait(&wait) else {
                return false;
            };
            if self.task_observed_epoch(task).is_some() {
                return true;
            }
            let Some(dependency) = self.task_dependency(task) else {
                return false;
            };
            let Some(dependency_wait) = dependency.producer_wait() else {
                return false;
            };
            wait = dependency_wait.clone();
        }
        false
    }

    pub(super) fn session_machine_is_busy(&self, session: EvaluationSessionId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state
            .work_by_session
            .get(&session)
            .into_iter()
            .flatten()
            .filter_map(|id| state.work.get(id))
            .any(|record| {
                matches!(record.kind, WorkKind::Reflection(_) | WorkKind::Deferred(_))
                    && matches!(record.state, WorkState::Running | WorkState::Terminalizing)
            })
    }

    #[cfg(test)]
    pub(super) fn task_promise_count(&self, session: EvaluationSessionId) -> usize {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state
            .promise_by_wait
            .values()
            .filter(|work| {
                state
                    .work
                    .get(work)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count()
    }

    #[cfg(test)]
    pub(super) fn client_demand_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .work
            .values()
            .filter(|record| matches!(record.kind, WorkKind::ClientDemand(_)))
            .count()
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

    /// Queues every blocked task whose retained retry checkpoint predates a
    /// newly published semantic-state epoch. The caller retains shared
    /// runtime mutation admission across epoch publication and this pass.
    pub(crate) fn publish_runtime_observation_guarded(
        &self,
        _mutation: &dyn RuntimeMutationAuthority,
        epoch: RuntimeObservationEpoch,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let registrations = state
            .observation_waiters
            .values()
            .copied()
            .filter(|registration| registration.observed_epoch < epoch)
            .collect::<Vec<_>>();
        let mut changed = false;
        for registration in registrations {
            changed |= queue_current_observation(&mut state, registration, epoch);
        }
        if changed {
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        changed
    }

    pub(crate) fn notify_runtime_observation(&self, changed: bool) {
        if changed {
            self.work_available.notify_all();
        }
    }

    /// Completes subscribe-and-recheck after a task publishes a blocked
    /// observation registration. Runtime mutation admission remains held by
    /// the caller, so a publisher either precedes this recheck or observes the
    /// installed registration itself.
    fn recheck_observation_wait(&self, id: EvaluationWorkId) -> bool {
        let current_epoch = self.observations.current();
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(registration) = state.observation_waiters.get(&id).copied() else {
            return false;
        };
        let changed = queue_current_observation(&mut state, registration, current_epoch);
        if changed {
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        changed
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

    pub(super) fn demand_session_is_open(&self, session: EvaluationSessionId) -> bool {
        let demand = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .demand_sessions
            .get(&session)
            .cloned();
        demand
            .and_then(|demand| demand.upgrade())
            .is_some_and(|demand| !demand.is_closed())
    }

    #[cfg(test)]
    pub(crate) fn registered_session_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .demand_sessions
            .len()
    }

    #[cfg(test)]
    pub(super) fn reflection_work_for_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<EvaluationWorkId> {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .reflection
            .by_wait
            .get(wait)
            .copied()
    }

    #[cfg(test)]
    pub(super) fn reflection_counts(&self, session: EvaluationSessionId) -> (usize, usize) {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let active = state
            .work_by_session
            .get(&session)
            .into_iter()
            .flatten()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| matches!(record.kind, WorkKind::Reflection(_)))
            })
            .count();
        let indexed = state
            .reflection
            .by_task
            .values()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count();
        (active, indexed)
    }

    #[cfg(test)]
    pub(crate) fn ready_task_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .ready_task_set
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
                WorkState::Dormant
                | WorkState::Reserved
                | WorkState::ExitWaiting
                | WorkState::Terminalizing => {}
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

fn task_for_record(record: &WorkRecord) -> Option<EvaluationTaskId> {
    match &record.kind {
        WorkKind::Reflection(work) => Some(work.task),
        WorkKind::Deferred(work) => Some(work.task),
        WorkKind::Spark(_) | WorkKind::ClientDemand(_) => None,
    }
}

fn task_block(record: &WorkRecord) -> Option<&EvaluationTaskBlock> {
    match &record.kind {
        WorkKind::Reflection(work) => work.block.as_ref(),
        WorkKind::Deferred(work) => work.block.as_ref(),
        WorkKind::Spark(_) | WorkKind::ClientDemand(_) => None,
    }
}

fn task_observation_epoch(record: &WorkRecord) -> Option<RuntimeObservationEpoch> {
    match (&record.state, &record.kind) {
        (WorkState::Blocked, _) => task_block(record).and_then(|block| block.observed_epoch),
        (WorkState::ExitWaiting, WorkKind::Reflection(work)) => {
            work.exit.as_ref().and_then(|exit| exit.observed_epoch)
        }
        _ => None,
    }
}

fn work_dependency(record: &WorkRecord) -> Option<&WorkDependency> {
    match &record.kind {
        WorkKind::Spark(work) => work.dependency.as_ref(),
        WorkKind::Reflection(work) => work.block.as_ref()?.dependency.as_ref(),
        WorkKind::Deferred(work) => work.block.as_ref()?.dependency.as_ref(),
        WorkKind::ClientDemand(work) => work
            .subscription
            .as_ref()
            .map(|subscription| &subscription.dependency),
    }
}

fn debug_assert_task_block_runtime(runtime: EvaluationRuntimeId, block: &EvaluationTaskBlock) {
    if let Some(dependency) = &block.dependency {
        debug_assert_eq!(
            dependency.runtime_id(),
            runtime,
            "published task block dependency must belong to its coordinator runtime"
        );
    }
}

fn publish_task_block_locked(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    id: EvaluationWorkId,
    block: EvaluationTaskBlock,
) -> Option<(WorkDependency, WakeRegistration)> {
    debug_assert_task_block_runtime(runtime, &block);
    assert!(
        block.dependency.is_some() || block.observed_epoch.is_some(),
        "blocked task work must publish an exact dependency or observed runtime epoch"
    );
    state.observation_waiters.remove(&id);
    let dependency = block.dependency.clone();
    let observed_epoch = block.observed_epoch;
    let record = state
        .work
        .get_mut(&id)
        .expect("blocked task work must remain registered");
    assert!(matches!(record.state, WorkState::Running));
    record.subscription_epoch = record
        .subscription_epoch
        .checked_add(1)
        .expect("evaluation work subscription epochs exhausted");
    let registration = WakeRegistration {
        work: id,
        subscription_epoch: record.subscription_epoch,
    };
    match &mut record.kind {
        WorkKind::Reflection(work) => work.block = Some(block),
        WorkKind::Deferred(work) => work.block = Some(block),
        WorkKind::Spark(_) => panic!("spark work cannot publish a task block"),
        WorkKind::ClientDemand(_) => panic!("client demand cannot publish a task block"),
    }
    record.state = WorkState::Blocked;
    if let Some(observed_epoch) = observed_epoch {
        state.observation_waiters.insert(
            id,
            ObservationRegistration {
                wake: registration,
                observed_epoch,
            },
        );
    }
    dependency.map(|dependency| (dependency, registration))
}

fn queue_current_observation(
    state: &mut WorkCoordinatorState,
    registration: ObservationRegistration,
    current_epoch: RuntimeObservationEpoch,
) -> bool {
    let id = registration.wake.work;
    let valid = state.work.get(&id).is_some_and(|record| {
        matches!(record.state, WorkState::Blocked | WorkState::ExitWaiting)
            && record.subscription_epoch == registration.wake.subscription_epoch
            && task_observation_epoch(record)
                .is_some_and(|observed| observed == registration.observed_epoch)
    });
    if !valid {
        if state.observation_waiters.get(&id) == Some(&registration) {
            state.observation_waiters.remove(&id);
        }
        return false;
    }
    if registration.observed_epoch >= current_epoch {
        return false;
    }
    state.observation_waiters.remove(&id);
    let record = state
        .work
        .get_mut(&id)
        .expect("validated observation work must remain registered");
    if matches!(record.state, WorkState::ExitWaiting) {
        let reflection = reflection_work_mut(record);
        assert!(
            reflection.machine.is_some(),
            "retryable exit work must retain its sanitized machine"
        );
        reflection.exit = None;
    }
    record.state = WorkState::Queued;
    queue_task(state, id);
    true
}

fn queue_task(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    if state.ready_task_set.insert(id) {
        state.ready_tasks.push_back(id);
    }
}

fn remove_ready_task(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    state.ready_task_set.remove(&id);
    state.ready_tasks.retain(|candidate| *candidate != id);
}

fn claim_ready_task(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    session: Option<EvaluationSessionId>,
) -> Option<ClaimedTaskWork> {
    loop {
        let position = match session {
            Some(session) => state
                .ready_tasks
                .iter()
                .position(|id| {
                    state.work.get(id).is_some_and(|record| {
                        record.demand_session == session
                            && matches!(record.kind, WorkKind::Reflection(_))
                    })
                })
                .or_else(|| {
                    state.ready_tasks.iter().position(|id| {
                        state
                            .work
                            .get(id)
                            .is_some_and(|record| record.demand_session == session)
                    })
                })?,
            None => 0,
        };
        let id = state.ready_tasks.remove(position)?;
        state.ready_task_set.remove(&id);
        let Some(record) = state.work.get(&id) else {
            continue;
        };
        let claimed = match &record.kind {
            WorkKind::Reflection(_) => claim_reflection_task(state, runtime, id),
            WorkKind::Deferred(_) => {
                claim_deferred(state, runtime, id, true).map(ClaimedTaskWork::Deferred)
            }
            WorkKind::Spark(_) | WorkKind::ClientDemand(_) => None,
        };
        if let Some(claimed) = claimed {
            return Some(claimed);
        }
    }
}

fn claim_reflection_task(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    id: EvaluationWorkId,
) -> Option<ClaimedTaskWork> {
    claim_reflection(state, runtime, id).map(ClaimedTaskWork::Reflection)
}

fn demand_session_is_closed(state: &WorkCoordinatorState, session: EvaluationSessionId) -> bool {
    state
        .demand_sessions
        .get(&session)
        .and_then(Weak::upgrade)
        .is_none_or(|demand| demand.is_closed())
}

fn prune_closed_session_registration(
    state: &mut WorkCoordinatorState,
    session: EvaluationSessionId,
) -> bool {
    if demand_session_is_closed(state, session) {
        state.demand_sessions.remove(&session);
        true
    } else {
        false
    }
}

fn queue_current_registration(
    state: &mut WorkCoordinatorState,
    registration: WakeRegistration,
    source: Option<WorkDependencyKey>,
) -> bool {
    enum ReadyQueue {
        Spark,
        ClientDemand,
        Task,
    }

    let kind = {
        let Some(record) = state.work.get_mut(&registration.work) else {
            return false;
        };
        if !matches!(record.state, WorkState::Blocked)
            || record.subscription_epoch != registration.subscription_epoch
            || source.is_some_and(|source| {
                work_dependency(record).is_none_or(|dependency| dependency.key() != source)
            })
        {
            return false;
        }
        record.state = WorkState::Queued;
        match record.kind {
            WorkKind::Spark(_) => ReadyQueue::Spark,
            WorkKind::ClientDemand(_) => ReadyQueue::ClientDemand,
            WorkKind::Reflection(_) | WorkKind::Deferred(_) => ReadyQueue::Task,
        }
    };
    state.observation_waiters.remove(&registration.work);
    match kind {
        ReadyQueue::Spark => queue_spark(state, registration.work),
        ReadyQueue::ClientDemand => queue_client_demand(state, registration.work),
        ReadyQueue::Task => queue_task(state, registration.work),
    }
    true
}

#[cfg(test)]
mod tests;
