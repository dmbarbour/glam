//! Runtime-owned work coordination independent of worker ownership.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::num::NonZeroU64;
#[cfg(test)]
use std::sync::OnceLock;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

#[cfg(test)]
use crate::core::LazyValue;
#[cfg(test)]
use crate::core::PromisedValue;
use crate::core::{
    CoreValueFactory, EvaluationFailure, ManagedPromiseRoot, PromiseAssignment, PromiseId,
    RuntimeValueAccess,
};
use crate::runtime::{
    EvaluationRuntimeId, RuntimeIds, RuntimeMutationAdmission, RuntimeMutationAuthority,
    RuntimeMutationGuard, RuntimeValueRoot,
};

#[cfg(test)]
use super::EvaluationSession;
use super::{EvaluationDemandState, RuntimeObservationEpoch, RuntimeObservationState};

mod client_demand;
mod completion;
mod deferred;
mod reflection;
#[cfg(test)]
mod registry_inventory;
mod settlement;
mod spark;
mod task;
pub(crate) use client_demand::{
    ClaimedClientDemand, ClientDemandHandle, ClientDemandOperation, ClientDemandPoll,
    ClientDemandResult, ClientDemandSink, ClientDemandSnapshot,
};
use client_demand::{
    ClientDemandRecord, ClientDemandRetirement, detach_client_demand, queue_client_demand,
};
#[cfg(test)]
use completion::DependencyWakeBatch;
pub(crate) use completion::{
    CompletionSubscriptionOutcome, CompletionSubscriptions, CompletionWake, WakeRegistration,
    WorkDependencyKey,
};
pub(super) use deferred::{
    AbandonedDeferredWork, ClaimedDeferredWork, ClaimedLazyRoute, DeferredLazyCycleMember,
    DeferredProducer, DeferredWorkPoll, DeferredWorkReservation,
};
use deferred::{
    DeferredIndexes, DeferredWork, LazyRouteWork, begin_deferred_abandonment, claim_deferred,
    claim_lazy_route, deferred_work_mut,
};
#[cfg(test)]
pub(super) use reflection::ReflectionWorkSnapshot;
#[cfg(test)]
use reflection::reflection_work;
use reflection::{
    AbandonedReflectionWork, ReflectionIndexes, ReflectionWork, claim_reflection,
    insert_task_failure, reflection_work_mut, remove_ready_reflection,
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
use spark::{SparkRetirement, claim_spark, detach_spark};
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
}

#[derive(Clone)]
struct TaskOwnedPromiseObligation {
    promise: PromiseId,
    root: ManagedPromiseRoot,
    producer: Arc<PromiseProducerObligation>,
    wait: EvaluationWaitToken,
    terminal: TaskPromiseTerminalMapper,
}

#[derive(Clone)]
pub(crate) enum TaskPromiseTerminalMapper {
    UnresolvedFailure,
    ReflectionReturnValue,
    ReflectionGate { target: RuntimeValueRoot },
}

impl TaskPromiseTerminalMapper {
    fn assignment(
        &self,
        access: &RuntimeValueAccess<'_>,
        terminal: &EvaluationWaitTerminal,
        unresolved_failure: &Arc<EvaluationFailure>,
    ) -> PromiseAssignment {
        let operation = match self {
            Self::UnresolvedFailure => return Err(unresolved_failure.clone()),
            Self::ReflectionReturnValue => "reflection_task",
            Self::ReflectionGate { .. } => "reflection_annotation",
        };

        match terminal {
            EvaluationWaitTerminal::Complete(value) => match self {
                Self::ReflectionReturnValue => Ok(value.clone_core_with(access)),
                Self::ReflectionGate { target } => Ok(target.clone_core_with(access)),
                Self::UnresolvedFailure => unreachable!("handled above"),
            },
            EvaluationWaitTerminal::Failed(failure) | EvaluationWaitTerminal::Killed(failure) => {
                Err(Arc::new(failure.as_failure().with_context_in(
                    access,
                    crate::diagnostic::evaluation_context_frame(operation),
                )))
            }
            EvaluationWaitTerminal::Cancelled => Err(reflection_terminal_failure(
                access,
                operation,
                match self {
                    Self::ReflectionReturnValue => "reflection result task was cancelled",
                    Self::ReflectionGate { .. } => "reflection annotation task was cancelled",
                    Self::UnresolvedFailure => unreachable!("handled above"),
                },
            )),
            EvaluationWaitTerminal::Abandoned => Err(reflection_terminal_failure(
                access,
                operation,
                "reflection task was abandoned when its evaluation session closed",
            )),
            EvaluationWaitTerminal::Exited => Err(reflection_terminal_failure(
                access,
                operation,
                "reflection task exited without producing a result",
            )),
        }
    }
}

fn reflection_terminal_failure(
    access: &RuntimeValueAccess<'_>,
    operation: &str,
    message: &str,
) -> Arc<EvaluationFailure> {
    Arc::new(EvaluationFailure::message(message).with_context_in(
        access,
        crate::diagnostic::evaluation_context_frame(operation),
    ))
}

impl TaskOwnedPromiseObligation {
    fn publish_terminal_guarded(
        self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        terminal: &EvaluationWaitTerminal,
        unresolved_failure: &Arc<EvaluationFailure>,
    ) -> (PromiseProducerPublication, CompletionWake) {
        let values = coordinator
            .value_observer()
            .upgrade()
            .expect("a registered promise root must retain a live value domain owner");
        let (publication, wake) = values.with_runtime_value_access(|access| {
            let assignment = self
                .terminal
                .assignment(&access, terminal, unresolved_failure);
            self.root
                .publish_guarded(&access, coordinator, mutation, assignment, |assignment| {
                    self.producer
                        .publish_assignment_guarded(coordinator, mutation, assignment)
                })
                .unwrap_or_else(|_| {
                    panic!("a terminalizing task-owned promise must remain unresolved")
                })
        });
        (publication.retain_snapshot_root(self.root), wake)
    }
}

impl SettlementObligations {
    fn reflection_task(wait: EvaluationWaitToken) -> Self {
        Self {
            producer: Some(ProducerSettlementObligation::ReflectionTask(
                TaskTerminalPublisher::new(wait),
            )),
            owned_promises: Vec::new(),
        }
    }

    fn deferred_claim(wait: EvaluationWaitToken, producer: DeferredProducer) -> Self {
        Self {
            producer: Some(ProducerSettlementObligation::DeferredClaim { wait, producer }),
            owned_promises: Vec::new(),
        }
    }

    fn take_producer(&mut self) -> Option<ProducerSettlementObligation> {
        self.producer.take()
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
        self.producer.is_none() && self.owned_promises.is_empty()
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
    Promise(ManagedPromiseRoot),
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
    pub(super) fn producer_wait(&self) -> Option<EvaluationWaitToken> {
        match self {
            Self::Wait(wait) => Some(wait.clone()),
            Self::Promise(promise) => promise.producer().and_then(|task| task.try_wait()),
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

    pub(crate) fn is_terminal(&self) -> bool {
        match self {
            Self::Wait(wait) => wait.terminal_poll().is_some(),
            Self::Promise(promise) => promise.is_terminal(),
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
    LazyRoute(LazyRouteWork),
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
    LazyRoute(ClaimedLazyRoute),
}

impl ClaimedTaskWork {
    pub(in crate::evaluation) fn id(&self) -> EvaluationWorkId {
        match self {
            Self::Reflection(work) => work.id,
            Self::Deferred(work) => work.id,
            Self::LazyRoute(work) => work.id,
        }
    }

    pub(in crate::evaluation) fn demand(&self) -> &ClaimedDemandSession {
        match self {
            Self::Reflection(work) => &work.demand,
            Self::Deferred(work) => &work.demand,
            Self::LazyRoute(work) => &work.demand,
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
    pub(super) fn poll(
        &mut self,
        poll_context: &super::EvaluationPollContext,
        step_budget: &mut super::EvaluationStepBudget,
    ) -> ClientDemandPoll {
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
        operation.poll(poll_context, &context, step_budget)
    }
}

#[derive(Default)]
struct WorkCoordinatorState {
    demand_sessions: HashMap<EvaluationSessionId, Weak<EvaluationDemandState>>,
    failures: RuntimeFailureLedger,
    pending_failure_reports: RuntimeFailureLedger,
    work: HashMap<EvaluationWorkId, WorkRecord>,
    work_by_session: HashMap<EvaluationSessionId, HashSet<EvaluationWorkId>>,
    client_demands: HashMap<EvaluationWorkId, ClientDemandRecord>,
    client_demands_by_session: HashMap<EvaluationSessionId, HashSet<EvaluationWorkId>>,
    ready_tasks: VecDeque<EvaluationWorkId>,
    ready_task_set: HashSet<EvaluationWorkId>,
    background_roots: VecDeque<EvaluationWorkId>,
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
    background_demand: Mutex<Option<Arc<EvaluationDemandState>>>,
    state: Mutex<WorkCoordinatorState>,
    work_available: Condvar,
    #[cfg(test)]
    work_wait_probe: OnceLock<std::sync::mpsc::Sender<()>>,
    #[cfg(test)]
    test_values: Option<CoreValueFactory>,
    #[cfg(test)]
    terminal_publication_probe: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    reflection_release_status_probe: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

pub(super) enum CoordinatorSelection {
    Task(ClaimedTaskWork),
    Spark(ClaimedSparkWork),
    None,
}

pub(super) enum CausalChildSelection {
    Claimed(ClaimedTaskWork),
    Busy,
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
            .field(
                "work_count",
                &(state.work.len() + state.client_demands.len()),
            )
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
            background_demand: Mutex::new(None),
            state: Mutex::new(WorkCoordinatorState::default()),
            work_available: Condvar::new(),
            #[cfg(test)]
            work_wait_probe: OnceLock::new(),
            #[cfg(test)]
            test_values: None,
            #[cfg(test)]
            terminal_publication_probe: Mutex::new(None),
            #[cfg(test)]
            reflection_release_status_probe: Mutex::new(None),
        })
    }

    pub(super) fn background_demand_or_init(
        &self,
        initialize: impl FnOnce() -> Arc<EvaluationDemandState>,
    ) -> Arc<EvaluationDemandState> {
        let mut background = self
            .background_demand
            .lock()
            .expect("evaluation background demand mutex was poisoned");
        if let Some(demand) = background.as_ref() {
            return demand.clone();
        }
        let demand = initialize();
        self.register_demand(&demand);
        *background = Some(demand.clone());
        demand
    }

    pub(super) fn background_demand(&self) -> Option<Arc<EvaluationDemandState>> {
        self.background_demand
            .lock()
            .expect("evaluation background demand mutex was poisoned")
            .clone()
    }

    /// Releases the runtime-owned background demand at runtime teardown.
    ///
    /// Workers retain only a coordinator route while idle. Keeping this
    /// value-domain owner inside that coordinator after the public runtime is
    /// gone would otherwise delay value-domain retirement until an idle
    /// worker observes shutdown.
    pub(crate) fn release_background_demand(&self) {
        let demand = self
            .background_demand
            .lock()
            .expect("evaluation background demand mutex was poisoned")
            .take();
        drop(demand);
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
            background_demand: Mutex::new(None),
            state: Mutex::new(WorkCoordinatorState::default()),
            work_available: Condvar::new(),
            work_wait_probe: OnceLock::new(),
            test_values: Some(values.clone()),
            terminal_publication_probe: Mutex::new(None),
            reflection_release_status_probe: Mutex::new(None),
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
    pub(super) fn set_reflection_release_status_probe(
        &self,
        probe: impl FnOnce() + Send + 'static,
    ) {
        *self
            .reflection_release_status_probe
            .lock()
            .expect("reflection release status probe was poisoned") = Some(Box::new(probe));
    }

    #[cfg(test)]
    fn run_reflection_release_status_probe(&self) {
        let probe = self
            .reflection_release_status_probe
            .lock()
            .expect("reflection release status probe was poisoned")
            .take();
        if let Some(probe) = probe {
            probe();
        }
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
                    WorkKind::LazyRoute(_) => {
                        unreachable!("runtime-owned lazy route entered session closure")
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
                }
            }
            let clients = state
                .client_demands_by_session
                .get(&session)
                .into_iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            for id in clients {
                let Some(record) = state.client_demands.get_mut(&id) else {
                    continue;
                };
                if record.control.close_reason.is_none() {
                    record.control.close_reason = Some(WorkCloseReason::DemandSessionClosed);
                    changed = true;
                }
                if matches!(record.state, WorkState::Running) {
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
    pub(crate) fn scheduler_inventory_for_test(&self) -> (u64, usize, usize) {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        (
            state.work_generation,
            state.demand_sessions.len(),
            state.work.len() + state.client_demands.len(),
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

    pub(super) fn demand_session_has_running_machine(&self, session: EvaluationSessionId) -> bool {
        session_has_running_machine(
            &self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned"),
            session,
        )
    }

    pub(super) fn runtime_has_running_machine(&self) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state
            .work
            .values()
            .any(|record| matches!(record.state, WorkState::Running | WorkState::Terminalizing))
            || state
                .client_demands
                .values()
                .any(|record| matches!(record.state, WorkState::Running | WorkState::Terminalizing))
    }

    /// Selects one executor-worker root.
    ///
    /// Foreground client evaluations are intentionally absent. The client
    /// which owns that record polls it directly and workers begin only from
    /// background spark or reflection roots. A deferred descendant is
    /// claimable only when an exact dependency path from one of those roots
    /// reaches it.
    pub(super) fn select_worker(&self) -> CoordinatorSelection {
        let mutation = self.admission.mutation_guard();
        let (selection, changed) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let initial_generation = state.work_generation;
            let selection = claim_causal_background(&mut state, self.runtime, true);
            if !matches!(selection, CoordinatorSelection::None) {
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
            let selection = claim_causal_background(&mut state, self.runtime, false);
            if !matches!(selection, CoordinatorSelection::None) {
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
                ClaimedTaskWork::LazyRoute(claimed) => {
                    let record = state
                        .work
                        .get_mut(&claimed.id)
                        .expect("unpolled lazy route must remain registered");
                    assert!(matches!(record.state, WorkState::Running));
                    let WorkKind::LazyRoute(route) = &mut record.kind else {
                        unreachable!()
                    };
                    route.block = claimed.prior_block;
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

    /// Claims one reflection root owned by this demand session, or the first
    /// claimable item on that root's exact producer chain. Merely sharing a
    /// session does not grant drain authority, and sparks and foreground
    /// client records are never session-drain roots.
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
            let selection = claim_causal_session_background(&mut state, self.runtime, session);
            if !matches!(selection, CoordinatorSelection::None) {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            match selection {
                CoordinatorSelection::Task(claimed) => Some(claimed),
                CoordinatorSelection::Spark(_) => {
                    unreachable!("a session drain cannot select spark work")
                }
                CoordinatorSelection::None => None,
            }
        };
        drop(mutation);
        if claimed.is_some() {
            self.work_available.notify_all();
        }
        claimed
    }

    /// Mechanical queue-selection hook for coordinator unit tests which are
    /// not exercising session-drain authority. Production session drains must
    /// use `claim_ready_task_for_session` and therefore start from an owned
    /// reflection root.
    #[cfg(test)]
    pub(in crate::evaluation) fn claim_ready_session_machine_for_test(
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

    /// Test-only compatibility selector for fixtures which advance the old
    /// mixed scheduler one quantum at a time.
    ///
    /// Production worker and runtime selectors must use causal root traversal.
    #[cfg(test)]
    pub(in crate::evaluation) fn claim_ready_task_for_test(&self) -> Option<ClaimedTaskWork> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let claimed = claim_ready_task(&mut state, self.runtime, None);
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
    #[cfg(test)]
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
                WorkKind::LazyRoute(_) => None,
                WorkKind::Spark(_) => None,
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

    pub(super) fn claim_work(&self, id: EvaluationWorkId) -> Option<ClaimedTaskWork> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let work = match state.work.get(&id)?.kind {
                WorkKind::Reflection(_) => claim_reflection_task(&mut state, self.runtime, id),
                WorkKind::Deferred(_) => claim_deferred(&mut state, self.runtime, id, false)
                    .map(ClaimedTaskWork::Deferred),
                WorkKind::LazyRoute(_) => claim_lazy_route(&mut state, self.runtime, id, false)
                    .map(ClaimedTaskWork::LazyRoute),
                WorkKind::Spark(_) => None,
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

    /// Claims a launched child (or its exact producer) reachable from the
    /// target/caller task identities. This never scans the same-session ready
    /// queue: launch provenance is a helping route, not a completion wait.
    pub(super) fn claim_causal_child_work(
        &self,
        target: Option<&EvaluationWaitToken>,
        caller_tasks: [Option<EvaluationTaskId>; 2],
    ) -> CausalChildSelection {
        let mutation = self.admission.mutation_guard();
        let selection = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let mut excluded = HashSet::new();
            loop {
                match causal_child_probe_locked(&state, target, caller_tasks, &excluded) {
                    CausalChildProbe::Ready(id) => {
                        let claimed = match state.work.get(&id).map(|record| &record.kind) {
                            Some(WorkKind::Reflection(_)) => {
                                claim_reflection_task(&mut state, self.runtime, id)
                            }
                            Some(WorkKind::Deferred(_)) => {
                                claim_deferred(&mut state, self.runtime, id, false)
                                    .map(ClaimedTaskWork::Deferred)
                            }
                            Some(WorkKind::LazyRoute(_)) => {
                                claim_lazy_route(&mut state, self.runtime, id, false)
                                    .map(ClaimedTaskWork::LazyRoute)
                            }
                            Some(WorkKind::Spark(_)) | None => None,
                        };
                        if let Some(claimed) = claimed {
                            state.work_generation = state.work_generation.wrapping_add(1);
                            break CausalChildSelection::Claimed(claimed);
                        } else {
                            // A demand session can close independently of this
                            // lock. Keep searching other causal children.
                            excluded.insert(id);
                        }
                    }
                    CausalChildProbe::Busy => break CausalChildSelection::Busy,
                    CausalChildProbe::None => break CausalChildSelection::None,
                }
            }
        };
        drop(mutation);
        if matches!(selection, CausalChildSelection::Claimed(_)) {
            self.work_available.notify_all();
        }
        selection
    }

    pub(super) fn has_busy_causal_child(
        &self,
        target: Option<&EvaluationWaitToken>,
        caller_tasks: [Option<EvaluationTaskId>; 2],
    ) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        matches!(
            causal_child_probe_locked(&state, target, caller_tasks, &HashSet::new()),
            CausalChildProbe::Busy
        )
    }

    pub(super) fn work_for_wait(&self, wait: &EvaluationWaitToken) -> Option<EvaluationWorkId> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        work_for_wait_locked(&state, wait)
    }

    #[cfg(test)]
    pub(super) fn producer_for_wait(&self, wait: &EvaluationWaitToken) -> Option<EvaluationTaskId> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        work_for_wait_locked(&state, wait)
            .and_then(|id| state.work.get(&id))
            .and_then(task_for_record)
    }

    pub(super) fn task_origin_for_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<(EvaluationTaskId, EvaluationSessionId)> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let record = state.work.get(&work_for_wait_locked(&state, wait)?)?;
        Some((task_for_record(record)?, record.demand_session))
    }

    pub(super) fn register_task_promise(
        self: &Arc<Self>,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
        root: ManagedPromiseRoot,
    ) -> Result<Arc<PromiseProducerObligation>, Arc<str>> {
        self.register_task_promise_with_terminal(
            task,
            wait,
            root,
            TaskPromiseTerminalMapper::UnresolvedFailure,
        )
    }

    pub(super) fn register_task_promise_with_terminal(
        self: &Arc<Self>,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
        root: ManagedPromiseRoot,
        terminal: TaskPromiseTerminalMapper,
    ) -> Result<Arc<PromiseProducerObligation>, Arc<str>> {
        debug_assert_eq!(wait.runtime_id(), self.runtime);
        let promise = root.id();
        let mutation = self.admission.mutation_guard();
        let producer = {
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
            if !matches!(
                record.state,
                WorkState::Dormant | WorkState::Reserved | WorkState::Running
            ) {
                return Err(Arc::from(
                    "a promise cannot be added after its producer stopped running",
                ));
            }
            let producer = Arc::new(PromiseProducerObligation::coordinator_owned(
                task,
                record.demand_session,
                &wait,
                work,
                promise,
                self,
            ));
            record
                .obligations
                .add_owned_promise(TaskOwnedPromiseObligation {
                    promise,
                    root,
                    producer: producer.clone(),
                    wait: wait.clone(),
                    terminal,
                });
            assert!(
                state.promise_by_wait.insert(wait, work).is_none(),
                "evaluation wait tokens must be unique"
            );
            state.work_generation = state.work_generation.wrapping_add(1);
            producer
        };
        drop(mutation);
        self.work_available.notify_all();
        Ok(producer)
    }

    pub(super) fn complete_task_promise_guarded(
        &self,
        _mutation: &dyn RuntimeMutationAuthority,
        work: EvaluationWorkId,
        wait: &EvaluationWaitToken,
        promise: PromiseId,
    ) -> Option<ManagedPromiseRoot> {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        if state.promise_by_wait.get(wait).copied() != Some(work) {
            return None;
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
        drop(state);
        Some(obligation.root)
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
        for update in status_update {
            debug_assert_eq!(update.status, terminal_task_status(&terminal));
            let publisher = update.publisher.clone();
            status_wakes.push(publisher.publish_update_guarded(&mutation, update));
            status_publishers.push(publisher);
        }
        let mut promise_publications = Vec::with_capacity(promises.len());
        for obligation in promises {
            let (producer, completion) =
                obligation.publish_terminal_guarded(self, &mutation, &terminal, &promise_failure);
            promise_publications.push(producer);
            completion_wakes.push(completion);
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
            WorkKind::LazyRoute(work) => work.block.as_ref(),
            WorkKind::Spark(_) => None,
        }
        .and_then(|block| block.dependency.clone())
    }

    pub(super) fn work_dependency_by_id(&self, id: EvaluationWorkId) -> Option<WorkDependency> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state.work.get(&id).and_then(work_dependency).cloned()
    }

    pub(super) fn work_observed_epoch(
        &self,
        id: EvaluationWorkId,
    ) -> Option<RuntimeObservationEpoch> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state.work.get(&id).and_then(task_observation_epoch)
    }

    pub(super) fn work_is_claimable(&self, id: EvaluationWorkId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state
            .work
            .get(&id)
            .is_some_and(|record| matches!(record.state, WorkState::Dormant | WorkState::Queued))
    }

    pub(super) fn work_is_busy(&self, id: EvaluationWorkId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state.work.get(&id).is_some_and(|record| {
            matches!(
                record.state,
                WorkState::Reserved | WorkState::Running | WorkState::Terminalizing
            )
        })
    }

    #[cfg(test)]
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

    pub(super) fn target_has_running_producer(&self, target: &EvaluationWaitToken) -> bool {
        let mut seen = HashSet::new();
        let mut wait = target.clone();
        while let Some(work) = self.work_for_wait(&wait) {
            if !seen.insert(work) {
                return false;
            }
            if self.work_is_busy(work) {
                return true;
            }
            let Some(dependency) = self.work_dependency_by_id(work) else {
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
            let Some(work) = self.work_for_wait(&wait) else {
                return false;
            };
            if self.work_observed_epoch(work).is_some() {
                return true;
            }
            let Some(dependency) = self.work_dependency_by_id(work) else {
                return false;
            };
            let Some(dependency_wait) = dependency.producer_wait() else {
                return false;
            };
            wait = dependency_wait.clone();
        }
        false
    }

    #[cfg(test)]
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

    /// Reports only progress which a session drain is authorized to await:
    /// its reflection roots and the exact producer chains beneath them.
    pub(super) fn session_background_is_busy(&self, session: EvaluationSessionId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state.background_roots.iter().any(|root| {
            let Some(root_record) = state.work.get(root) else {
                return false;
            };
            if root_record.demand_session != session
                || !matches!(root_record.kind, WorkKind::Reflection(_))
            {
                return false;
            }
            match causal_background_probe_locked(&state, *root) {
                CausalBackgroundProbe::Busy => true,
                CausalBackgroundProbe::Ready(candidate) => {
                    state.work.get(&candidate).is_some_and(|candidate| {
                        session_has_running_machine(&state, candidate.demand_session)
                    })
                }
                CausalBackgroundProbe::None => false,
            }
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
            .client_demands
            .len()
    }

    pub(super) fn wait_for_change(&self, observed_generation: u64) {
        let _ = self.wait_for_change_with_timeout(observed_generation, None);
    }

    /// Waits on the coordinator's observed generation, optionally bounded by
    /// an idle timeout. The predicate is checked under the same mutex as
    /// publication, so a change preceding this call cannot become a lost
    /// wake. The timeout never interrupts an active machine poll.
    pub(super) fn wait_for_change_with_timeout(
        &self,
        observed_generation: u64,
        timeout: Option<Duration>,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        #[cfg(test)]
        if state.work_generation == observed_generation
            && let Some(probe) = self.work_wait_probe.get()
        {
            let _ = probe.send(());
        }
        if let Some(timeout) = timeout {
            let (current, _) = self
                .work_available
                .wait_timeout_while(state, timeout, |state| {
                    state.work_generation == observed_generation
                })
                .expect("evaluation work coordinator was poisoned");
            return current.work_generation != observed_generation;
        }
        while state.work_generation == observed_generation {
            state = self
                .work_available
                .wait(state)
                .expect("evaluation work coordinator was poisoned");
        }
        true
    }

    #[cfg(test)]
    pub(super) fn set_work_wait_probe(&self, probe: std::sync::mpsc::Sender<()>) {
        self.work_wait_probe
            .set(probe)
            .unwrap_or_else(|_| panic!("work wait probe can only be installed once"));
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
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state
            .reflection
            .by_wait
            .get(wait)
            .or_else(|| state.promise_by_wait.get(wait))
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
        WorkKind::LazyRoute(_) => None,
        WorkKind::Spark(_) => None,
    }
}

fn task_block(record: &WorkRecord) -> Option<&EvaluationTaskBlock> {
    match &record.kind {
        WorkKind::Reflection(work) => work.block.as_ref(),
        WorkKind::Deferred(work) => work.block.as_ref(),
        WorkKind::LazyRoute(work) => work.block.as_ref(),
        WorkKind::Spark(_) => None,
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
        WorkKind::LazyRoute(work) => work.block.as_ref()?.dependency.as_ref(),
    }
}

fn work_for_wait_locked(
    state: &WorkCoordinatorState,
    wait: &EvaluationWaitToken,
) -> Option<EvaluationWorkId> {
    state
        .promise_by_wait
        .get(wait)
        .or_else(|| state.deferred.by_wait.get(wait))
        .or_else(|| state.reflection.by_wait.get(wait))
        .copied()
}

/// Finds the first claimable item on one background root's exact producer
/// chain. A queued root runs before its old block is followed; a blocked root
/// can reach a dormant deferred producer without promoting unrelated work.
/// Worker and runtime-pump selectors use this same traversal policy.
fn causal_background_probe_locked(
    state: &WorkCoordinatorState,
    root: EvaluationWorkId,
) -> CausalBackgroundProbe {
    let Some(root_record) = state.work.get(&root) else {
        return CausalBackgroundProbe::None;
    };
    if !matches!(
        root_record.kind,
        WorkKind::Reflection(_) | WorkKind::Spark(_)
    ) {
        return CausalBackgroundProbe::None;
    }
    let mut current = root;
    let mut seen = HashSet::new();
    while seen.insert(current) {
        let Some(record) = state.work.get(&current) else {
            return CausalBackgroundProbe::None;
        };
        match record.state {
            WorkState::Queued => return CausalBackgroundProbe::Ready(current),
            WorkState::Dormant
                if matches!(record.kind, WorkKind::Deferred(_) | WorkKind::LazyRoute(_)) =>
            {
                return CausalBackgroundProbe::Ready(current);
            }
            WorkState::Blocked => {
                let Some(wait) = work_dependency(record).and_then(WorkDependency::producer_wait)
                else {
                    return CausalBackgroundProbe::None;
                };
                let Some(producer) = work_for_wait_locked(state, &wait) else {
                    return CausalBackgroundProbe::None;
                };
                current = producer;
            }
            WorkState::Reserved | WorkState::Running | WorkState::Terminalizing => {
                return CausalBackgroundProbe::Busy;
            }
            WorkState::Dormant | WorkState::ExitWaiting => {
                return CausalBackgroundProbe::None;
            }
        }
    }
    CausalBackgroundProbe::None
}

fn causal_background_candidate_locked(
    state: &WorkCoordinatorState,
    root: EvaluationWorkId,
) -> Option<EvaluationWorkId> {
    match causal_background_probe_locked(state, root) {
        CausalBackgroundProbe::Ready(candidate) => Some(candidate),
        CausalBackgroundProbe::Busy | CausalBackgroundProbe::None => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CausalBackgroundProbe {
    Ready(EvaluationWorkId),
    Busy,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CausalChildProbe {
    Ready(EvaluationWorkId),
    Busy,
    None,
}

/// Walks task-launch edges from the caller and target's exact producer chain.
/// Each launched child may itself block through an exact producer or launch
/// further children. A claimed route remains visible as Busy; it must not be
/// mistaken for stable absence just because its machine was detached.
fn causal_child_probe_locked(
    state: &WorkCoordinatorState,
    target: Option<&EvaluationWaitToken>,
    caller_tasks: [Option<EvaluationTaskId>; 2],
    excluded: &HashSet<EvaluationWorkId>,
) -> CausalChildProbe {
    let mut tasks = VecDeque::new();
    let mut seen_tasks = HashSet::new();
    for task in caller_tasks.into_iter().flatten() {
        if seen_tasks.insert(task) {
            tasks.push_back(task);
        }
    }

    let mut seen_exact = HashSet::new();
    let mut wait = target.cloned();
    while let Some(work) = wait
        .as_ref()
        .and_then(|wait| work_for_wait_locked(state, wait))
    {
        if !seen_exact.insert(work) {
            break;
        }
        let Some(record) = state.work.get(&work) else {
            break;
        };
        if let Some(task) = task_for_record(record)
            && seen_tasks.insert(task)
        {
            tasks.push_back(task);
        }
        let Some(next) = work_dependency(record).and_then(WorkDependency::producer_wait) else {
            break;
        };
        wait = Some(next.clone());
    }

    let mut seen_child_work = HashSet::new();
    let mut busy = false;
    while let Some(parent) = tasks.pop_front() {
        let Some(children) = state.reflection.children_by_parent.get(&parent) else {
            continue;
        };
        for root in children {
            let mut current = *root;
            while seen_child_work.insert(current) {
                let Some(record) = state.work.get(&current) else {
                    break;
                };
                if let Some(task) = task_for_record(record)
                    && seen_tasks.insert(task)
                {
                    tasks.push_back(task);
                }
                match record.state {
                    WorkState::Queued
                        if !excluded.contains(&current)
                            && !demand_session_is_closed(state, record.demand_session) =>
                    {
                        return CausalChildProbe::Ready(current);
                    }
                    WorkState::Dormant
                        if matches!(
                            record.kind,
                            WorkKind::Deferred(_) | WorkKind::LazyRoute(_)
                        ) && !excluded.contains(&current)
                            && !demand_session_is_closed(state, record.demand_session) =>
                    {
                        return CausalChildProbe::Ready(current);
                    }
                    WorkState::Reserved | WorkState::Running | WorkState::Terminalizing => {
                        busy = true;
                        break;
                    }
                    WorkState::Blocked => {
                        let Some(next) = work_dependency(record)
                            .and_then(WorkDependency::producer_wait)
                            .and_then(|wait| work_for_wait_locked(state, &wait))
                        else {
                            break;
                        };
                        current = next;
                    }
                    WorkState::Dormant | WorkState::Queued | WorkState::ExitWaiting => break,
                }
            }
        }
    }
    if busy {
        CausalChildProbe::Busy
    } else {
        CausalChildProbe::None
    }
}

fn claim_causal_background(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    include_sparks: bool,
) -> CoordinatorSelection {
    let preferred_kinds = if state.prefer_spark {
        [true, false]
    } else {
        [false, true]
    };
    for spark_root in preferred_kinds {
        if spark_root && !include_sparks {
            continue;
        }
        for index in 0..state.background_roots.len() {
            let root = state.background_roots[index];
            let Some(root_record) = state.work.get(&root) else {
                continue;
            };
            if matches!(root_record.kind, WorkKind::Spark(_)) != spark_root {
                continue;
            }
            let Some(candidate) = causal_background_candidate_locked(state, root) else {
                continue;
            };
            let Some(record) = state.work.get(&candidate) else {
                continue;
            };
            if session_has_running_machine(state, record.demand_session) {
                continue;
            }
            let claimed = match record.kind {
                WorkKind::Reflection(_) => {
                    claim_reflection_task(state, runtime, candidate).map(CoordinatorSelection::Task)
                }
                WorkKind::Deferred(_) => claim_deferred(state, runtime, candidate, false)
                    .map(ClaimedTaskWork::Deferred)
                    .map(CoordinatorSelection::Task),
                WorkKind::LazyRoute(_) => claim_lazy_route(state, runtime, candidate, false)
                    .map(ClaimedTaskWork::LazyRoute)
                    .map(CoordinatorSelection::Task),
                WorkKind::Spark(_) => {
                    claim_spark(state, runtime, candidate).map(CoordinatorSelection::Spark)
                }
            };
            if let Some(claimed) = claimed {
                let rotated = state
                    .background_roots
                    .remove(index)
                    .expect("selected background root must remain registered");
                state.background_roots.push_back(rotated);
                state.prefer_spark = !spark_root;
                return claimed;
            }
        }
    }
    CoordinatorSelection::None
}

/// Selects from reflection roots owned by one demand session while allowing
/// an exact producer chain to cross session boundaries. This deliberately
/// does not scan the session's ready queue: queue co-location is not causal
/// authority.
fn claim_causal_session_background(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    session: EvaluationSessionId,
) -> CoordinatorSelection {
    for index in 0..state.background_roots.len() {
        let root = state.background_roots[index];
        let Some(root_record) = state.work.get(&root) else {
            continue;
        };
        if root_record.demand_session != session
            || !matches!(root_record.kind, WorkKind::Reflection(_))
        {
            continue;
        }
        let Some(candidate) = causal_background_candidate_locked(state, root) else {
            continue;
        };
        let Some(record) = state.work.get(&candidate) else {
            continue;
        };
        if session_has_running_machine(state, record.demand_session) {
            continue;
        }
        let claimed = match record.kind {
            WorkKind::Reflection(_) => {
                claim_reflection_task(state, runtime, candidate).map(CoordinatorSelection::Task)
            }
            WorkKind::Deferred(_) => claim_deferred(state, runtime, candidate, false)
                .map(ClaimedTaskWork::Deferred)
                .map(CoordinatorSelection::Task),
            WorkKind::LazyRoute(_) => claim_lazy_route(state, runtime, candidate, false)
                .map(ClaimedTaskWork::LazyRoute)
                .map(CoordinatorSelection::Task),
            WorkKind::Spark(_) => None,
        };
        if let Some(claimed) = claimed {
            let rotated = state
                .background_roots
                .remove(index)
                .expect("selected session reflection root must remain registered");
            state.background_roots.push_back(rotated);
            return claimed;
        }
    }
    CoordinatorSelection::None
}

/// Reports progress already latent in one exact producer chain.
///
/// The caller holds runtime mutation admission and the coordinator-state
/// mutex, so a claimable tail, owned poll, terminal dependency, or stale
/// observation cannot disappear between this check and a client retirement.
fn dependency_has_causal_progress_locked(
    state: &WorkCoordinatorState,
    dependency: &WorkDependency,
    current_epoch: RuntimeObservationEpoch,
) -> bool {
    let mut dependency = Some(dependency.clone());
    let mut seen = HashSet::new();
    while let Some(current) = dependency {
        if current.is_terminal() {
            return true;
        }
        let Some(wait) = current.producer_wait() else {
            return false;
        };
        let Some(work) = work_for_wait_locked(state, &wait) else {
            return false;
        };
        if !seen.insert(work) {
            return false;
        }
        let Some(record) = state.work.get(&work) else {
            return false;
        };
        match record.state {
            WorkState::Dormant
            | WorkState::Reserved
            | WorkState::Queued
            | WorkState::Running
            | WorkState::Terminalizing => return true,
            WorkState::Blocked | WorkState::ExitWaiting => {
                if task_observation_epoch(record).is_some_and(|epoch| epoch < current_epoch) {
                    return true;
                }
                dependency = work_dependency(record).cloned();
            }
        }
    }
    false
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
        WorkKind::LazyRoute(work) => work.block = Some(block),
        WorkKind::Spark(_) => panic!("spark work cannot publish a task block"),
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

fn register_background_root(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    debug_assert!(!state.background_roots.contains(&id));
    state.background_roots.push_back(id);
}

fn unregister_background_root(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    if let Some(position) = state.background_roots.iter().position(|root| *root == id) {
        state.background_roots.remove(position);
    }
}

fn remove_ready_task(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    state.ready_task_set.remove(&id);
    state.ready_tasks.retain(|candidate| *candidate != id);
}

/// Returns whether one ordinary machine still owns a semantic poll for this
/// demand session.
///
/// `Terminalizing` is deliberately excluded. Terminal publication has
/// already detached the machine and made its result authoritative; the
/// remaining machine destruction and coordinator retirement are
/// non-semantic cleanup. Treating that tail as an active poll can deadlock a
/// same-session client demand created while unwinding the completed machine.
fn session_has_running_machine(state: &WorkCoordinatorState, session: EvaluationSessionId) -> bool {
    let background = state
        .work_by_session
        .get(&session)
        .into_iter()
        .flatten()
        .filter_map(|id| state.work.get(id))
        .any(|record| {
            matches!(record.state, WorkState::Running) && !matches!(record.kind, WorkKind::Spark(_))
        });
    background
        || state
            .client_demands_by_session
            .get(&session)
            .into_iter()
            .flatten()
            .filter_map(|id| state.client_demands.get(id))
            .any(|record| matches!(record.state, WorkState::Running))
}

#[cfg(test)]
fn claim_ready_task(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    session: Option<EvaluationSessionId>,
) -> Option<ClaimedTaskWork> {
    loop {
        let eligible = |id: &EvaluationWorkId| {
            state
                .work
                .get(id)
                .is_some_and(|record| !session_has_running_machine(state, record.demand_session))
        };
        let position = match session {
            Some(session) => state
                .ready_tasks
                .iter()
                .position(|id| {
                    state.work.get(id).is_some_and(|record| {
                        record.demand_session == session
                            && matches!(record.kind, WorkKind::Reflection(_))
                            && eligible(id)
                    })
                })
                .or_else(|| {
                    state.ready_tasks.iter().position(|id| {
                        state
                            .work
                            .get(id)
                            .is_some_and(|record| record.demand_session == session && eligible(id))
                    })
                })?,
            None => state.ready_tasks.iter().position(eligible)?,
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
            WorkKind::LazyRoute(_) => {
                claim_lazy_route(state, runtime, id, true).map(ClaimedTaskWork::LazyRoute)
            }
            WorkKind::Spark(_) => None,
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
    if let Some(record) = state.client_demands.get_mut(&registration.work) {
        if !matches!(record.state, WorkState::Blocked)
            || record.subscription_epoch != registration.subscription_epoch
            || source.is_some_and(|source| {
                record
                    .work
                    .subscription
                    .as_ref()
                    .is_none_or(|subscription| subscription.dependency.key() != source)
            })
        {
            return false;
        }
        record.state = WorkState::Queued;
        queue_client_demand(state, registration.work);
        return true;
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
        !matches!(record.kind, WorkKind::Spark(_))
    };
    state.observation_waiters.remove(&registration.work);
    if kind {
        queue_task(state, registration.work);
    }
    true
}

#[cfg(test)]
mod tests;
