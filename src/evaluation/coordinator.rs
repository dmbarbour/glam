//! Runtime-owned work coordination independent of worker ownership.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::num::NonZeroU64;
use std::sync::{Arc, Condvar, Mutex, Weak};

#[cfg(test)]
use crate::core::CoreValueFactory;
use crate::core::{DeferredValueId, LazyValue, PromiseCell, PromiseId, PromisedValue, Value};
use crate::runtime::{
    EvaluationRuntimeId, RuntimeIds, RuntimeMutationAdmission, RuntimeMutationAuthority,
    RuntimeMutationGuard, RuntimeValueRoot,
};

#[cfg(test)]
use super::EvaluationSession;
use super::{
    ClientDemandOperation, ClientDemandResult, ClientDemandSink, EvaluationDemandState,
    EvaluationExitBlock, EvaluationFailure, EvaluationMachinePoll, EvaluationSessionId,
    EvaluationTaskId, EvaluationTaskMachine, EvaluationTaskStatus, EvaluationWaitTerminal,
    EvaluationWaitToken, ExitIntent, RuntimeFailureLedger, TaskFailureLedger, TaskStatusPublisher,
    TaskStatusWake,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationTaskBlock {
    pub(crate) dependency: Option<WorkDependency>,
    pub(crate) observed_epoch: Option<RuntimeObservationEpoch>,
    pub(crate) error: Option<Arc<EvaluationFailure>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EvaluationWorkId(NonZeroU64);

impl EvaluationWorkId {
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

/// Runtime-wide semantic-state revision observed by retryable evaluation.
///
/// Scheduler queue churn uses a separate work generation and never advances
/// this value. Epochs begin at one so `Option<RuntimeObservationEpoch>` can use
/// the zero niche for absence without increasing the block representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RuntimeObservationEpoch(NonZeroU64);

impl RuntimeObservationEpoch {
    pub(crate) fn from_raw(epoch: u64) -> Self {
        Self(NonZeroU64::new(epoch).expect("runtime observation epochs must be nonzero"))
    }

    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

pub(crate) struct RuntimeObservationState {
    epoch: Mutex<RuntimeObservationEpoch>,
    changed: Condvar,
}

impl RuntimeObservationState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: Mutex::new(RuntimeObservationEpoch::from_raw(1)),
            changed: Condvar::new(),
        })
    }

    pub(crate) fn current(&self) -> RuntimeObservationEpoch {
        *self
            .epoch
            .lock()
            .expect("runtime observation mutex should not be poisoned")
    }

    pub(crate) fn advance(&self) -> RuntimeObservationEpoch {
        let mut epoch = self
            .epoch
            .lock()
            .expect("runtime observation mutex should not be poisoned");
        *epoch = RuntimeObservationEpoch::from_raw(
            epoch
                .get()
                .checked_add(1)
                .expect("runtime observation epochs exhausted"),
        );
        *epoch
    }

    pub(crate) fn notify_all(&self) {
        self.changed.notify_all();
    }

    pub(crate) fn wait_for_change(&self, observed: RuntimeObservationEpoch) {
        let mut epoch = self
            .epoch
            .lock()
            .expect("runtime observation mutex should not be poisoned");
        while *epoch == observed {
            epoch = self
                .changed
                .wait(epoch)
                .expect("runtime observation mutex should not be poisoned");
        }
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
    coordinator: Arc<Mutex<Weak<EvaluationWorkCoordinator>>>,
    registrations: Mutex<Vec<WakeRegistration>>,
}

/// Scheduler notification detached from an authoritative completion
/// publication.
///
/// The coordinator transition has already happened while runtime mutation
/// admission was held. Keeping the notification separate lets callers release
/// that admission before waking scheduler threads.
#[must_use = "scheduler wakes must be delivered after mutation admission is released"]
pub(crate) struct CompletionWake {
    coordinator: Arc<EvaluationWorkCoordinator>,
    changed: bool,
}

impl CompletionWake {
    pub(crate) fn notify(self) {
        self.coordinator.notify_dependency_wake(self.changed);
    }
}

impl CompletionSubscriptions {
    pub(crate) fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        let coordinator = self
            .coordinator
            .lock()
            .expect("runtime work-coordinator binding was poisoned")
            .clone();
        coordinator
            .upgrade()
            .filter(|coordinator| coordinator.runtime == self.runtime)
    }

    pub(crate) fn for_promise(
        runtime: EvaluationRuntimeId,
        promise: PromiseId,
        coordinator: Arc<Mutex<Weak<EvaluationWorkCoordinator>>>,
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
        coordinator: Arc<Mutex<Weak<EvaluationWorkCoordinator>>>,
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
        let binding = Arc::new(Mutex::new(Arc::downgrade(coordinator)));
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
        let coordinator = self.coordinator();
        let Some(coordinator) = coordinator else {
            let result = publish_terminal()?;
            self.registrations
                .lock()
                .expect("completion subscriber set was poisoned")
                .clear();
            return Ok(result);
        };

        let mutation = coordinator.admission.mutation_guard();
        let (result, wake) = self.publish_guarded(&coordinator, &mutation, publish_terminal)?;
        drop(mutation);
        wake.notify();
        Ok(result)
    }

    /// Publishes a terminal using mutation admission already held by the
    /// caller.
    ///
    /// The terminal closure and every resulting coordinator transition become
    /// authoritative before this returns. The returned wake must be delivered
    /// only after the caller releases component locks and mutation admission.
    pub(crate) fn publish_guarded<T, E>(
        &self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        publish_terminal: impl FnOnce() -> Result<T, E>,
    ) -> Result<(T, CompletionWake), E> {
        debug_assert_eq!(coordinator.runtime, self.runtime);

        let result = publish_terminal()?;
        let registrations = std::mem::take(
            &mut *self
                .registrations
                .lock()
                .expect("completion subscriber set was poisoned"),
        );
        let changed = coordinator.wake_dependency_batch_guarded(
            mutation,
            DependencyWakeBatch {
                source: self.source,
                registrations,
            },
        );
        Ok((
            result,
            CompletionWake {
                coordinator: coordinator.clone(),
                changed,
            },
        ))
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

    pub(crate) fn unsubscribe(&self, registration: WakeRegistration) -> bool {
        let mut registrations = self
            .registrations
            .lock()
            .expect("completion subscriber set was poisoned");
        let Some(index) = registrations
            .iter()
            .position(|candidate| *candidate == registration)
        else {
            return false;
        };
        registrations.swap_remove(index);
        true
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

struct TaskTerminalPublisher {
    wait: EvaluationWaitToken,
    published_status: EvaluationTaskStatus,
    protected_status: Option<TaskStatusPublisher>,
}

impl TaskTerminalPublisher {
    fn new(wait: EvaluationWaitToken) -> Self {
        Self {
            wait,
            published_status: EvaluationTaskStatus::Launched,
            protected_status: None,
        }
    }

    fn attach_status(&mut self, publisher: TaskStatusPublisher) {
        assert!(
            self.protected_status.replace(publisher).is_none(),
            "a reflection task may expose only one status query"
        );
    }

    fn update_status(
        &mut self,
        status: EvaluationTaskStatus,
        terminal: bool,
    ) -> Option<(TaskStatusPublisher, EvaluationTaskStatus)> {
        if self.published_status == status {
            return None;
        }
        self.published_status = status.clone();
        let publisher = if terminal {
            self.protected_status.take()
        } else {
            self.protected_status.clone()
        }?;
        Some((publisher, status))
    }
}

fn terminal_task_status(terminal: &EvaluationWaitTerminal) -> EvaluationTaskStatus {
    match terminal {
        EvaluationWaitTerminal::Complete(value) => EvaluationTaskStatus::Complete(value.clone()),
        EvaluationWaitTerminal::Failed(error) => EvaluationTaskStatus::Failed(error.clone()),
        EvaluationWaitTerminal::Cancelled => EvaluationTaskStatus::Cancelled,
        EvaluationWaitTerminal::Abandoned => EvaluationTaskStatus::Abandoned,
        EvaluationWaitTerminal::Exited => EvaluationTaskStatus::Exited,
        EvaluationWaitTerminal::Killed(error) => EvaluationTaskStatus::Killed(error.clone()),
    }
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

struct SparkDemand {
    session: Arc<EvaluationDemandState>,
    value: RuntimeValueRoot,
}

struct SparkWork {
    demand: Option<SparkDemand>,
    dependency: Option<WorkDependency>,
}

struct ClientDemandSubscription {
    dependency: WorkDependency,
    registration: WakeRegistration,
}

impl ClientDemandSubscription {
    fn unsubscribe(self) {
        let _ = self.dependency.unsubscribe_work(self.registration);
    }
}

struct ClientDemandWork {
    demand: Arc<EvaluationDemandState>,
    operation: Option<ClientDemandOperation>,
    subscription: Option<ClientDemandSubscription>,
}

struct TaskFailureReporting {
    owner_session: EvaluationSessionId,
    acknowledged: bool,
}

struct ReflectionWork {
    task: EvaluationTaskId,
    failure_reporting: TaskFailureReporting,
    wait: EvaluationWaitToken,
    machine: Option<Box<dyn EvaluationTaskMachine>>,
    block: Option<EvaluationTaskBlock>,
    exit: Option<EvaluationExitBlock>,
}

#[derive(Clone)]
pub(super) enum DeferredProducer {
    Lazy(LazyValue),
    Promise(PromisedValue),
}

impl DeferredProducer {
    pub(super) fn id(&self) -> DeferredValueId {
        match self {
            Self::Lazy(lazy) => lazy.id().into(),
            Self::Promise(promise) => promise.id().into(),
        }
    }
}

struct DeferredWork {
    task: EvaluationTaskId,
    wait: EvaluationWaitToken,
    producer: DeferredProducer,
    machine: Option<Box<dyn EvaluationTaskMachine>>,
    block: Option<EvaluationTaskBlock>,
    demanded_while_reserved: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservationRegistration {
    wake: WakeRegistration,
    observed_epoch: RuntimeObservationEpoch,
}

pub(super) struct ClaimedSparkWork {
    id: EvaluationWorkId,
    demand_session: EvaluationSessionId,
    demand: SparkDemand,
    prior_dependency: Option<WorkDependency>,
}

pub(super) struct ClaimedClientDemand {
    id: EvaluationWorkId,
    demand: Arc<EvaluationDemandState>,
    operation: Option<ClientDemandOperation>,
    prior_subscription: Option<ClientDemandSubscription>,
}

pub(super) enum ClientDemandPoll {
    Complete(RuntimeValueRoot),
    Failed(Arc<EvaluationFailure>),
    Blocked(WorkDependency),
}

pub(super) enum ClientDemandSnapshot {
    Queued,
    Running,
    Blocked {
        dependency: WorkDependency,
        subscription_epoch: u64,
    },
}

struct ClientDemandRetirement {
    sink: ClientDemandSink,
    operation: ClientDemandOperation,
    subscription: Option<ClientDemandSubscription>,
    result: ClientDemandResult,
}

impl ClientDemandRetirement {
    fn finish(self) {
        if let Some(subscription) = self.subscription {
            subscription.unsubscribe();
        }
        let _ = self.sink.publish(self.result);
        drop(self.operation);
    }
}

pub(super) struct ClaimedReflectionWork {
    id: EvaluationWorkId,
    task: EvaluationTaskId,
    demand_session: EvaluationSessionId,
    prior_block: Option<EvaluationTaskBlock>,
    machine: Option<Box<dyn EvaluationTaskMachine>>,
}

pub(super) struct ClaimedDeferredWork {
    id: EvaluationWorkId,
    task: EvaluationTaskId,
    demand_session: EvaluationSessionId,
    producer: DeferredValueId,
    prior_block: Option<EvaluationTaskBlock>,
    requeue_on_yield: bool,
    machine: Option<Box<dyn EvaluationTaskMachine>>,
}

pub(super) enum ClaimedTaskWork {
    Reflection(ClaimedReflectionWork),
    Deferred(ClaimedDeferredWork),
}

impl ClaimedDeferredWork {
    pub(super) fn id(&self) -> EvaluationWorkId {
        self.id
    }

    pub(super) fn poll(&mut self, step_budget: usize) -> EvaluationMachinePoll {
        self.machine
            .as_mut()
            .expect("claimed deferred work must retain its detached machine")
            .poll(step_budget)
    }
}

pub(super) enum DeferredWorkPoll {
    Yielded,
    Blocked(EvaluationTaskBlock),
    Terminal,
}

pub(super) struct DeferredLazyCycleMember {
    pub(super) work: EvaluationWorkId,
    pub(super) wait: EvaluationWaitToken,
    pub(super) lazy: LazyValue,
    pub(super) machine: Box<dyn EvaluationTaskMachine>,
}

pub(super) struct DeferredWorkRelease {
    pub(super) made_progress: bool,
    pub(super) remains_blocked: bool,
    pub(super) terminal: bool,
    pub(super) abandoned: bool,
    pub(super) cycle: Vec<DeferredLazyCycleMember>,
    pub(super) machine: Option<Box<dyn EvaluationTaskMachine>>,
}

pub(super) enum DeferredWorkReservation {
    New(EvaluationWorkId),
    Existing(EvaluationWaitToken),
}

pub(super) struct AbandonedDeferredWork {
    pub(super) id: EvaluationWorkId,
    pub(super) task: EvaluationTaskId,
    pub(super) dependency: Option<EvaluationWaitToken>,
    pub(super) machine: Box<dyn EvaluationTaskMachine>,
}

impl ClaimedReflectionWork {
    pub(super) fn id(&self) -> EvaluationWorkId {
        self.id
    }

    #[cfg(test)]
    pub(super) fn task(&self) -> EvaluationTaskId {
        self.task
    }

    pub(super) fn poll(&mut self, step_budget: usize) -> EvaluationMachinePoll {
        self.machine
            .as_mut()
            .expect("claimed reflection work must retain its detached machine")
            .poll(step_budget)
    }
}

pub(super) enum ReflectionWorkPoll {
    Yielded,
    Blocked(EvaluationTaskBlock),
    Exit(EvaluationExitBlock),
    Terminal,
}

pub(super) struct ReflectionWorkRelease {
    pub(super) made_progress: bool,
    pub(super) remains_blocked: bool,
    pub(super) exit_waiting: bool,
    pub(super) terminal: bool,
    pub(super) cancel: bool,
    pub(super) abandoned: bool,
    pub(super) machine: Option<Box<dyn EvaluationTaskMachine>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReflectionWorkState {
    Dormant,
    Reserved,
    Queued,
    Running,
    Blocked(EvaluationTaskBlock),
    ExitWaiting(EvaluationExitBlock),
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

impl ClaimedSparkWork {
    pub(super) fn demand_session(&self) -> Arc<EvaluationDemandState> {
        self.demand.session.clone()
    }

    pub(super) fn value(&self) -> &RuntimeValueRoot {
        &self.demand.value
    }
}

impl ClaimedClientDemand {
    pub(super) fn poll(&mut self) -> ClientDemandPoll {
        let context = super::EvalContext::for_client_demand(self.demand.clone());
        let operation = self
            .operation
            .as_mut()
            .expect("claimed client demand must retain its operation");
        operation.poll(&context)
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
    demand_sessions: HashMap<EvaluationSessionId, Weak<EvaluationDemandState>>,
    failures: RuntimeFailureLedger,
    work: HashMap<EvaluationWorkId, WorkRecord>,
    work_by_session: HashMap<EvaluationSessionId, HashSet<EvaluationWorkId>>,
    ready_tasks: VecDeque<EvaluationWorkId>,
    ready_task_set: HashSet<EvaluationWorkId>,
    ready_sparks: VecDeque<EvaluationWorkId>,
    ready_spark_set: HashSet<EvaluationWorkId>,
    ready_client_demands: VecDeque<EvaluationWorkId>,
    ready_client_demand_set: HashSet<EvaluationWorkId>,
    reflection_by_task: std::collections::BTreeMap<EvaluationTaskId, EvaluationWorkId>,
    reflection_by_wait: HashMap<EvaluationWaitToken, EvaluationWorkId>,
    deferred_by_task: std::collections::BTreeMap<EvaluationTaskId, EvaluationWorkId>,
    deferred_by_wait: HashMap<EvaluationWaitToken, EvaluationWorkId>,
    deferred_by_value: HashMap<DeferredValueId, EvaluationWorkId>,
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
    ids: Arc<RuntimeIds>,
    admission: Arc<RuntimeMutationAdmission>,
    observations: Arc<RuntimeObservationState>,
    state: Mutex<WorkCoordinatorState>,
    work_available: Condvar,
    #[cfg(test)]
    test_values: Option<CoreValueFactory>,
}

pub(super) enum CoordinatorSelection {
    Task(ClaimedTaskWork),
    Spark(ClaimedSparkWork),
    ClientDemand(ClaimedClientDemand),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimePumpSnapshot {
    pub(crate) useful_ready: bool,
    pub(crate) progress_owned: bool,
    pub(crate) abandonable_sparks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeCoordinatorReadiness {
    Busy,
    Ready {
        work_generation: u64,
        exits: Vec<RuntimeExitSnapshot>,
    },
    Deadlocked {
        work_generation: u64,
        exits: Vec<RuntimeExitSnapshot>,
        unfinished: Vec<RuntimeDeadlockWorkSnapshot>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeExitSnapshot {
    pub(crate) work: EvaluationWorkId,
    pub(crate) session: EvaluationSessionId,
    pub(crate) task: EvaluationTaskId,
    pub(crate) intent: ExitIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeWorkKindSnapshot {
    ReflectionTask,
    DeferredEvaluation,
    ClientDemand,
    Spark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeWorkStateSnapshot {
    Dormant,
    Reserved,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeDependencySnapshot {
    Wait {
        wait: u64,
        producer: EvaluationTaskId,
        session: EvaluationSessionId,
    },
    Promise {
        promise: u64,
        producer: Option<(u64, EvaluationTaskId, EvaluationSessionId)>,
    },
    #[cfg(test)]
    Test(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeDeadlockWorkSnapshot {
    pub(crate) work: EvaluationWorkId,
    pub(crate) session: EvaluationSessionId,
    pub(crate) task: Option<EvaluationTaskId>,
    pub(crate) kind: RuntimeWorkKindSnapshot,
    pub(crate) state: RuntimeWorkStateSnapshot,
    pub(crate) dependency: Option<RuntimeDependencySnapshot>,
    pub(crate) observed_epoch: Option<RuntimeObservationEpoch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedRuntimeSettlementPlan {
    pub(crate) work_generation: u64,
    pub(crate) exits: Vec<RuntimeExitSnapshot>,
    pub(crate) kills: Vec<RuntimeDeadlockWorkSnapshot>,
}

struct SelectedTaskSettlement {
    work: EvaluationWorkId,
    producer: Option<ProducerSettlementObligation>,
    status_update: Option<(TaskStatusPublisher, EvaluationTaskStatus)>,
    promises: Vec<TaskOwnedPromiseObligation>,
    machine: Option<Box<dyn EvaluationTaskMachine>>,
    block: Option<EvaluationTaskBlock>,
    exit: Option<EvaluationExitBlock>,
    terminal: EvaluationWaitTerminal,
    promise_failure: Arc<EvaluationFailure>,
}

/// Resources detached by one successful exit settlement.
///
/// All semantic terminal cells are authoritative before this value is
/// returned. Its notifications and potentially value-owning drops are delayed
/// until the caller has released exclusive settlement admission.
pub(crate) struct RuntimeSettlementRelease {
    coordinator: Arc<EvaluationWorkCoordinator>,
    producers: Vec<ProducerSettlementObligation>,
    machines: Vec<Box<dyn EvaluationTaskMachine>>,
    blocks: Vec<EvaluationTaskBlock>,
    exits: Vec<EvaluationExitBlock>,
    terminals: Vec<EvaluationWaitTerminal>,
    client_demands: Vec<ClientDemandRetirement>,
    completion_wakes: Vec<CompletionWake>,
    status_wakes: Vec<TaskStatusWake>,
    status_publishers: Vec<TaskStatusPublisher>,
    promise_publications: Vec<super::PromiseProducerPublication>,
}

impl RuntimeSettlementRelease {
    pub(crate) fn finish(self) {
        let Self {
            coordinator,
            producers,
            machines,
            blocks,
            exits,
            terminals,
            client_demands,
            completion_wakes,
            status_wakes,
            status_publishers,
            promise_publications,
        } = self;
        for retirement in client_demands {
            retirement.finish();
        }
        for wake in completion_wakes {
            wake.notify();
        }
        for publication in promise_publications {
            publication.notify();
        }
        for wake in status_wakes {
            wake.notify();
        }
        coordinator.work_available.notify_all();
        coordinator.admission.notify_settlement();
        drop(producers);
        drop(machines);
        drop(blocks);
        drop(exits);
        drop(terminals);
        drop(status_publishers);
    }
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
        runtime: EvaluationRuntimeId,
        ids: Arc<RuntimeIds>,
        admission: Arc<RuntimeMutationAdmission>,
        observations: Arc<RuntimeObservationState>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            ids,
            admission,
            observations,
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
            observations: RuntimeObservationState::new(),
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

    pub(crate) fn failure_ledger_snapshot(&self) -> RuntimeFailureLedger {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .failures
            .clone()
    }

    #[cfg(test)]
    pub(super) fn task_failure_is_acknowledged(&self, task: EvaluationTaskId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(work) = state.reflection_by_task.get(&task) else {
            return false;
        };
        state
            .work
            .get(work)
            .is_some_and(|record| reflection_work(record).failure_reporting.acknowledged)
    }

    #[cfg(test)]
    pub(super) fn task_has_status_publisher(&self, task: EvaluationTaskId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(work) = state.reflection_by_task.get(&task) else {
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
    pub(super) fn acknowledge_task_failure(
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
            if let Some(work) = state.reflection_by_task.get(&task).copied()
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
    pub(super) fn attach_reflection_status_publisher(
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

    /// Advances one task's protected-query status under one runtime mutation
    /// admission without retaining its role host in the coordinator record.
    /// External notification happens only after mutation admission is
    /// released.
    pub(super) fn update_reflection_status(
        &self,
        work: EvaluationWorkId,
        status: EvaluationTaskStatus,
    ) {
        let mutation = self.admission.mutation_guard();
        let update = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get_mut(&work)
                .expect("reported reflection work must remain registered");
            record
                .obligations
                .task_publisher_mut()
                .expect("active reflection work must retain its terminal publisher")
                .update_status(status, false)
        };
        let wake = update.map(|(publisher, status)| publisher.publish_guarded(&mutation, status));
        drop(mutation);
        if let Some(wake) = wake {
            wake.notify();
        }
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

    pub(super) fn admit_client_demand(
        &self,
        demand: Arc<EvaluationDemandState>,
        operation: ClientDemandOperation,
        sink: ClientDemandSink,
    ) -> Result<EvaluationWorkId, Arc<str>> {
        debug_assert_eq!(demand.values.runtime_id(), self.runtime);
        if operation.runtime_id() != self.runtime {
            return Err(Arc::from(
                "client demand operation belongs to another evaluation runtime",
            ));
        }
        let id = EvaluationWorkId(self.ids.evaluation_work());
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if demand_session_is_closed(&state, demand.id) {
                return Err(Arc::from("evaluation demand session is closed"));
            }
            let session = demand.id;
            let record = WorkRecord {
                id,
                demand_session: session,
                subscription_epoch: 0,
                control: WorkControl::default(),
                obligations: SettlementObligations::client_demand(sink),
                state: WorkState::Queued,
                kind: WorkKind::ClientDemand(ClientDemandWork {
                    demand,
                    operation: Some(operation),
                    subscription: None,
                }),
            };
            assert!(state.work.insert(id, record).is_none());
            state.work_by_session.entry(session).or_default().insert(id);
            queue_client_demand(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_one();
        Ok(id)
    }

    pub(super) fn submit_spark(&self, session: Arc<EvaluationDemandState>, value: Value) {
        debug_assert_eq!(session.values.runtime_id(), self.runtime);
        let id = EvaluationWorkId(self.ids.evaluation_work());
        let demand = SparkDemand {
            session: session.clone(),
            value: RuntimeValueRoot::new(&session.values, value),
        };
        let mutation = self.admission.mutation_guard();
        let admitted = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if state.spark_workers == 0 || demand_session_is_closed(&state, session.id) {
                false
            } else {
                let session_id = session.id;
                let record = WorkRecord {
                    id,
                    demand_session: session_id,
                    subscription_epoch: 0,
                    control: WorkControl::default(),
                    obligations: SettlementObligations::default(),
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
            let selection = claim_ready_client_demand(&mut state)
                .map(CoordinatorSelection::ClientDemand)
                .unwrap_or_else(|| {
                    if state.prefer_spark {
                        claim_ready_spark(&mut state)
                            .map(CoordinatorSelection::Spark)
                            .or_else(|| {
                                claim_ready_task(&mut state, None).map(CoordinatorSelection::Task)
                            })
                            .unwrap_or(CoordinatorSelection::None)
                    } else {
                        claim_ready_task(&mut state, None)
                            .map(CoordinatorSelection::Task)
                            .or_else(|| {
                                claim_ready_spark(&mut state).map(CoordinatorSelection::Spark)
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
            let selection = claim_ready_client_demand(&mut state)
                .map(CoordinatorSelection::ClientDemand)
                .or_else(|| claim_ready_task(&mut state, None).map(CoordinatorSelection::Task))
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

    /// Inspects scheduler activity while the caller holds settlement
    /// admission exclusively. This method itself is observational.
    pub(crate) fn runtime_pump_snapshot(&self) -> RuntimePumpSnapshot {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        RuntimePumpSnapshot {
            useful_ready: state.work.values().any(|record| {
                matches!(
                    record.kind,
                    WorkKind::Reflection(_) | WorkKind::Deferred(_) | WorkKind::ClientDemand(_)
                ) && matches!(record.state, WorkState::Queued)
            }),
            progress_owned: state.work.values().any(|record| {
                matches!(record.state, WorkState::Running | WorkState::Terminalizing)
            }),
            abandonable_sparks: state.work.values().any(|record| {
                matches!(record.kind, WorkKind::Spark(_))
                    && matches!(record.state, WorkState::Queued | WorkState::Blocked)
            }),
        }
    }

    /// Classifies all retained work while the caller holds settlement
    /// admission exclusively. No queue, work record, or generation is changed.
    pub(crate) fn runtime_readiness_snapshot(&self) -> RuntimeCoordinatorReadiness {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        runtime_readiness_locked(&state)
    }

    /// Revalidates one proposed ready disposition set without changing work.
    pub(crate) fn validate_runtime_settlement(
        &self,
        work_generation: u64,
        exits: &[RuntimeExitSnapshot],
        kills: &[RuntimeDeadlockWorkSnapshot],
    ) -> Option<ValidatedRuntimeSettlementPlan> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        if !settlement_dispositions_match_locked(&state, work_generation, exits, kills) {
            return None;
        }
        Some(ValidatedRuntimeSettlementPlan {
            work_generation,
            exits: exits.to_vec(),
            kills: kills.to_vec(),
        })
    }

    /// Revalidates and atomically publishes every proposed disposition while
    /// the caller holds exclusive settlement admission.
    pub(crate) fn publish_runtime_settlement(
        self: &Arc<Self>,
        mutation: &dyn RuntimeMutationAuthority,
        plan: &ValidatedRuntimeSettlementPlan,
        kill_failure: Option<Arc<EvaluationFailure>>,
    ) -> Option<RuntimeSettlementRelease> {
        if plan.kills.is_empty() != kill_failure.is_none() {
            return None;
        }
        let exit_promise_failure = Arc::new(EvaluationFailure::message(
            "reflection task exited without fulfilling its promised value",
        ));
        let (mut selected, client_demands) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if !settlement_dispositions_match_locked(
                &state,
                plan.work_generation,
                &plan.exits,
                &plan.kills,
            ) {
                return None;
            }

            let mut selected = Vec::with_capacity(plan.exits.len() + plan.kills.len());
            let mut client_demands = Vec::new();
            for proposed in &plan.exits {
                let (producer, status_update, promises, machine, exit) = {
                    let record = state
                        .work
                        .get_mut(&proposed.work)
                        .expect("validated exit work must remain registered");
                    assert!(matches!(record.state, WorkState::ExitWaiting));
                    let reflection = reflection_work_mut(record);
                    assert_eq!(reflection.task, proposed.task);
                    let exit = reflection
                        .exit
                        .take()
                        .expect("exit-waiting work must retain its exit summary");
                    assert_eq!(exit.intent, proposed.intent);
                    let machine = reflection.machine.take();
                    let mut producer = record
                        .obligations
                        .take_producer()
                        .expect("exit work must retain its producer obligation");
                    let status_update = match &mut producer {
                        ProducerSettlementObligation::ReflectionTask(publisher) => {
                            publisher.update_status(EvaluationTaskStatus::Exited, true)
                        }
                        ProducerSettlementObligation::DeferredClaim { .. } => {
                            panic!("only reflection work may publish an exit disposition")
                        }
                    };
                    let promises = record.obligations.owned_promises.clone();
                    record.state = WorkState::Terminalizing;
                    (producer, status_update, promises, machine, exit)
                };
                state.observation_waiters.remove(&proposed.work);
                selected.push(SelectedTaskSettlement {
                    work: proposed.work,
                    producer: Some(producer),
                    status_update,
                    promises,
                    machine,
                    block: None,
                    exit: Some(exit),
                    terminal: EvaluationWaitTerminal::Exited,
                    promise_failure: exit_promise_failure.clone(),
                });
            }

            for proposed in &plan.kills {
                if matches!(
                    state
                        .work
                        .get(&proposed.work)
                        .expect("validated killed work must remain registered")
                        .kind,
                    WorkKind::ClientDemand(_)
                ) {
                    client_demands.push(detach_client_demand(
                        &mut state,
                        proposed.work,
                        None,
                        None,
                        ClientDemandResult::Killed(
                            kill_failure
                                .as_ref()
                                .expect("forced settlement must retain its failure")
                                .clone(),
                        ),
                    ));
                    continue;
                }

                let (producer, status_update, promises, machine, block) = {
                    let record = state
                        .work
                        .get_mut(&proposed.work)
                        .expect("validated killed work must remain registered");
                    assert!(matches!(
                        record.state,
                        WorkState::Dormant | WorkState::Reserved | WorkState::Blocked
                    ));
                    let (machine, block) = match &mut record.kind {
                        WorkKind::Reflection(reflection) => {
                            (reflection.machine.take(), reflection.block.take())
                        }
                        WorkKind::Deferred(deferred) => (
                            Some(
                                deferred
                                    .machine
                                    .take()
                                    .expect("stable deferred work must retain its machine"),
                            ),
                            deferred.block.take(),
                        ),
                        WorkKind::Spark(_) => {
                            unreachable!("stable deadlock cannot retain best-effort spark work")
                        }
                        WorkKind::ClientDemand(_) => unreachable!("handled above"),
                    };
                    let mut producer = record
                        .obligations
                        .take_producer()
                        .expect("killed task work must retain its producer obligation");
                    let killed = EvaluationTaskStatus::Killed(
                        kill_failure
                            .as_ref()
                            .expect("forced settlement must retain its failure")
                            .clone(),
                    );
                    let status_update = match &mut producer {
                        ProducerSettlementObligation::ReflectionTask(publisher) => {
                            publisher.update_status(killed, true)
                        }
                        ProducerSettlementObligation::DeferredClaim { .. } => None,
                    };
                    let promises = record.obligations.owned_promises.clone();
                    record.state = WorkState::Terminalizing;
                    (producer, status_update, promises, machine, block)
                };
                state.observation_waiters.remove(&proposed.work);
                let failure = kill_failure
                    .as_ref()
                    .expect("forced settlement must retain its failure")
                    .clone();
                selected.push(SelectedTaskSettlement {
                    work: proposed.work,
                    producer: Some(producer),
                    status_update,
                    promises,
                    machine,
                    block,
                    exit: None,
                    terminal: EvaluationWaitTerminal::Killed(failure.clone()),
                    promise_failure: failure,
                });
            }
            if !selected.is_empty() || !client_demands.is_empty() {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            (selected, client_demands)
        };

        let mut completion_wakes = Vec::new();
        let mut status_wakes = Vec::new();
        let mut status_publishers = Vec::new();
        let mut promise_publications = Vec::new();
        for selected in &mut selected {
            let wait = match selected
                .producer
                .as_ref()
                .expect("selected task must retain its producer")
            {
                ProducerSettlementObligation::ReflectionTask(publisher) => &publisher.wait,
                ProducerSettlementObligation::DeferredClaim { wait, .. } => wait,
            };
            let (_, wake) =
                wait.publish_terminal_guarded(self, mutation, selected.terminal.clone());
            completion_wakes.push(wake);
            if let Some((publisher, status)) = selected.status_update.take() {
                debug_assert_eq!(status, terminal_task_status(&selected.terminal));
                status_wakes.push(publisher.publish_guarded(mutation, status));
                status_publishers.push(publisher);
            }
            for obligation in &selected.promises {
                if let Some(promise) = obligation.cell.upgrade() {
                    let publication = promise.publish_guarded(
                        self,
                        mutation,
                        Err(selected.promise_failure.clone()),
                    );
                    let (producer, completion) = publication.unwrap_or_else(|_| {
                        panic!("an exit-owned promise must remain unresolved until settlement")
                    });
                    promise_publications.push(producer);
                    completion_wakes.push(completion);
                } else {
                    assert!(self.complete_task_promise_guarded(
                        mutation,
                        selected.work,
                        &obligation.wait,
                        obligation.promise,
                    ));
                    let (_, wake) = obligation.wait.publish_terminal_guarded(
                        self,
                        mutation,
                        EvaluationWaitTerminal::Failed(selected.promise_failure.clone()),
                    );
                    completion_wakes.push(wake);
                }
            }
        }

        let (machines, blocks, exits, terminals) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let machines = selected
                .iter_mut()
                .filter_map(|selected| {
                    let record = state
                        .work
                        .get(&selected.work)
                        .expect("settled exit work must remain registered");
                    assert!(record.obligations.is_empty());
                    match record.kind {
                        WorkKind::Reflection(_) => {
                            let detached = detach_reflection(&mut state, selected.work, true);
                            assert!(
                                detached.is_none(),
                                "settlement must detach a reflection machine before retirement"
                            );
                        }
                        WorkKind::Deferred(_) => detach_deferred(&mut state, selected.work),
                        WorkKind::Spark(_) | WorkKind::ClientDemand(_) => {
                            unreachable!("selected task settlement must contain task work")
                        }
                    }
                    selected.machine.take()
                })
                .collect::<Vec<_>>();
            if !selected.is_empty() {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            let blocks = selected
                .iter_mut()
                .filter_map(|selected| selected.block.take())
                .collect();
            let exits = selected
                .iter_mut()
                .filter_map(|selected| selected.exit.take())
                .collect();
            let terminals = selected
                .iter()
                .map(|selected| selected.terminal.clone())
                .collect();
            (machines, blocks, exits, terminals)
        };

        Some(RuntimeSettlementRelease {
            coordinator: self.clone(),
            producers: selected
                .iter_mut()
                .map(|selected| {
                    selected
                        .producer
                        .take()
                        .expect("settled task must retain its detached producer")
                })
                .collect(),
            machines,
            blocks,
            exits,
            terminals,
            client_demands,
            completion_wakes,
            status_wakes,
            status_publishers,
            promise_publications,
        })
    }
}

fn runtime_readiness_locked(state: &WorkCoordinatorState) -> RuntimeCoordinatorReadiness {
    let mut exits = Vec::new();
    let mut unfinished = Vec::new();

    for record in state.work.values() {
        if matches!(
            record.state,
            WorkState::Queued | WorkState::Running | WorkState::Terminalizing
        ) || matches!(record.kind, WorkKind::Spark(_))
        {
            return RuntimeCoordinatorReadiness::Busy;
        }

        if matches!(record.state, WorkState::ExitWaiting) {
            let reflection = reflection_work(record);
            exits.push(RuntimeExitSnapshot {
                work: record.id,
                session: record.demand_session,
                task: reflection.task,
                intent: reflection
                    .exit
                    .as_ref()
                    .expect("exit-waiting reflection work must retain its exit summary")
                    .intent
                    .clone(),
            });
            continue;
        }

        let state_snapshot = match record.state {
            WorkState::Dormant => RuntimeWorkStateSnapshot::Dormant,
            WorkState::Reserved => RuntimeWorkStateSnapshot::Reserved,
            WorkState::Blocked => RuntimeWorkStateSnapshot::Blocked,
            WorkState::Queued
            | WorkState::Running
            | WorkState::ExitWaiting
            | WorkState::Terminalizing => unreachable!("handled above"),
        };
        unfinished.push(RuntimeDeadlockWorkSnapshot {
            work: record.id,
            session: record.demand_session,
            task: task_for_record(record),
            kind: runtime_work_kind(record),
            state: state_snapshot,
            dependency: work_dependency(record).map(runtime_dependency_snapshot),
            observed_epoch: task_observation_epoch(record),
        });
    }

    exits.sort_by_key(|exit| exit.work.get());
    if unfinished.is_empty() {
        RuntimeCoordinatorReadiness::Ready {
            work_generation: state.work_generation,
            exits,
        }
    } else {
        unfinished.sort_by_key(|work| work.work.get());
        RuntimeCoordinatorReadiness::Deadlocked {
            work_generation: state.work_generation,
            exits,
            unfinished,
        }
    }
}

fn settlement_dispositions_match_locked(
    state: &WorkCoordinatorState,
    work_generation: u64,
    exits: &[RuntimeExitSnapshot],
    kills: &[RuntimeDeadlockWorkSnapshot],
) -> bool {
    match runtime_readiness_locked(state) {
        RuntimeCoordinatorReadiness::Ready {
            work_generation: current_generation,
            exits: current_exits,
        } => kills.is_empty() && current_generation == work_generation && current_exits == exits,
        RuntimeCoordinatorReadiness::Deadlocked {
            work_generation: current_generation,
            exits: current_exits,
            unfinished,
        } => {
            !kills.is_empty()
                && current_generation == work_generation
                && current_exits == exits
                && unfinished == kills
        }
        RuntimeCoordinatorReadiness::Busy => false,
    }
}

impl EvaluationWorkCoordinator {
    /// Removes every queued or blocked best-effort spark without touching a
    /// worker-owned record. Detached dependencies and values are released only
    /// after coordinator and mutation locks have been dropped.
    pub(crate) fn abandon_quiescent_sparks(&self) -> usize {
        let mutation = self.admission.mutation_guard();
        let retired = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let ids = state
                .work
                .iter()
                .filter_map(|(id, record)| {
                    (matches!(record.kind, WorkKind::Spark(_))
                        && matches!(record.state, WorkState::Queued | WorkState::Blocked))
                    .then_some(*id)
                })
                .collect::<Vec<_>>();
            let retired = ids
                .into_iter()
                .filter_map(|id| detach_spark(&mut state, id))
                .collect::<Vec<_>>();
            if !retired.is_empty() {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            retired
        };
        let count = retired.len();
        drop(mutation);
        if count != 0 {
            self.work_available.notify_all();
        }
        for record in retired {
            record.abandon();
        }
        count
    }

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

    pub(super) fn requeue_unpolled_client_demand(&self, mut claimed: ClaimedClientDemand) {
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get_mut(&claimed.id)
                .expect("unpolled client demand must remain registered");
            assert!(matches!(record.state, WorkState::Running));
            let client = client_demand_work_mut(record);
            assert!(
                client.operation.is_none(),
                "running client demand must have detached its operation"
            );
            client.operation = claimed.operation.take();
            client.subscription = claimed.prior_subscription.take();
            record.state = WorkState::Queued;
            queue_client_demand(&mut state, claimed.id);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_all();
    }

    pub(super) fn claim_client_demand(&self, id: EvaluationWorkId) -> Option<ClaimedClientDemand> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let claimed = claim_client_demand(&mut state, id);
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

    pub(super) fn client_demand_snapshot(
        &self,
        id: EvaluationWorkId,
    ) -> Option<ClientDemandSnapshot> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let record = state.work.get(&id)?;
        let WorkKind::ClientDemand(client) = &record.kind else {
            return None;
        };
        match record.state {
            WorkState::Queued => Some(ClientDemandSnapshot::Queued),
            WorkState::Running => Some(ClientDemandSnapshot::Running),
            WorkState::Blocked => {
                let subscription = client
                    .subscription
                    .as_ref()
                    .expect("blocked client demand must retain its exact subscription");
                Some(ClientDemandSnapshot::Blocked {
                    dependency: subscription.dependency.clone(),
                    subscription_epoch: subscription.registration.subscription_epoch,
                })
            }
            WorkState::Dormant
            | WorkState::Reserved
            | WorkState::ExitWaiting
            | WorkState::Terminalizing => {
                unreachable!("client demand entered an unsupported work state")
            }
        }
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
            if state
                .work
                .get(&claimed.id)
                .is_some_and(|record| matches!(record.state, WorkState::Blocked))
                && let Some(wait) = state
                    .work
                    .get(&claimed.id)
                    .and_then(|record| spark_work(record).dependency.as_ref())
                    .and_then(WorkDependency::producer_wait)
                    .cloned()
            {
                promote_deferred_wait_locked(&mut state, &wait);
            }
            state.work_generation = state.work_generation.wrapping_add(1);
            (retired, obsolete_dependency, exact_subscription)
        };

        if let Some((dependency, registration)) = exact_subscription {
            self.subscribe_dependency_guarded(&mutation, dependency, registration);
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

    pub(super) fn release_client_demand(
        &self,
        mut claimed: ClaimedClientDemand,
        poll: ClientDemandPoll,
    ) {
        let mutation = self.admission.mutation_guard();
        let (retirement, obsolete_subscription, exact_subscription) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let close_requested = {
                let record = state
                    .work
                    .get(&claimed.id)
                    .expect("claimed client demand must remain registered");
                assert_eq!(record.demand_session, claimed.demand.id);
                assert!(matches!(record.state, WorkState::Running));
                assert!(matches!(record.kind, WorkKind::ClientDemand(_)));
                record.control.close_reason
            };
            let mut obsolete_subscription = None;
            let mut exact_subscription = None;
            let retirement = if close_requested.is_some() {
                debug_assert!(matches!(
                    close_requested,
                    Some(
                        WorkCloseReason::ClientDemandAbandoned
                            | WorkCloseReason::DemandSessionClosed
                    )
                ));
                Some(detach_client_demand(
                    &mut state,
                    claimed.id,
                    claimed.operation.take(),
                    claimed.prior_subscription.take(),
                    ClientDemandResult::Abandoned,
                ))
            } else {
                match poll {
                    ClientDemandPoll::Complete(value) => {
                        debug_assert_eq!(value.runtime_id(), self.runtime);
                        Some(detach_client_demand(
                            &mut state,
                            claimed.id,
                            claimed.operation.take(),
                            claimed.prior_subscription.take(),
                            ClientDemandResult::Complete(value),
                        ))
                    }
                    ClientDemandPoll::Failed(failure) => Some(detach_client_demand(
                        &mut state,
                        claimed.id,
                        claimed.operation.take(),
                        claimed.prior_subscription.take(),
                        ClientDemandResult::Failed(failure),
                    )),
                    ClientDemandPoll::Blocked(dependency)
                        if dependency.runtime_id() != self.runtime =>
                    {
                        Some(detach_client_demand(
                            &mut state,
                            claimed.id,
                            claimed.operation.take(),
                            claimed.prior_subscription.take(),
                            ClientDemandResult::Failed(Arc::new(EvaluationFailure::message(
                                "client demand blocked on another evaluation runtime",
                            ))),
                        ))
                    }
                    ClientDemandPoll::Blocked(dependency) => {
                        obsolete_subscription = claimed.prior_subscription.take();
                        let record = state
                            .work
                            .get_mut(&claimed.id)
                            .expect("blocked client demand must remain registered");
                        record.subscription_epoch = record
                            .subscription_epoch
                            .checked_add(1)
                            .expect("evaluation work subscription epochs exhausted");
                        let registration = WakeRegistration {
                            work: claimed.id,
                            subscription_epoch: record.subscription_epoch,
                        };
                        let client = client_demand_work_mut(record);
                        assert!(client.operation.is_none());
                        client.operation = claimed.operation.take();
                        client.subscription = Some(ClientDemandSubscription {
                            dependency: dependency.clone(),
                            registration,
                        });
                        record.state = WorkState::Blocked;
                        exact_subscription = Some((dependency.clone(), registration));
                        if let Some(wait) = dependency.producer_wait() {
                            promote_deferred_wait_locked(&mut state, wait);
                        }
                        None
                    }
                }
            };
            state.work_generation = state.work_generation.wrapping_add(1);
            (retirement, obsolete_subscription, exact_subscription)
        };
        if let Some(subscription) = obsolete_subscription {
            subscription.unsubscribe();
        }
        let woke = exact_subscription.is_some_and(|(dependency, registration)| {
            self.subscribe_dependency_guarded(&mutation, dependency, registration)
        });
        if let Some(retirement) = retirement {
            retirement.finish();
        }
        drop(mutation);
        self.work_available.notify_all();
        self.notify_dependency_wake(woke);
    }

    pub(super) fn abandon_client_demand(&self, id: EvaluationWorkId) -> bool {
        let mutation = self.admission.mutation_guard();
        let (accepted, retirement) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get_mut(&id) else {
                return false;
            };
            if !matches!(record.kind, WorkKind::ClientDemand(_)) {
                return false;
            }
            if matches!(record.state, WorkState::Terminalizing) {
                return true;
            }
            if matches!(record.state, WorkState::Running) {
                record
                    .control
                    .close_reason
                    .get_or_insert(WorkCloseReason::ClientDemandAbandoned);
                state.work_generation = state.work_generation.wrapping_add(1);
                (true, None)
            } else {
                let retirement =
                    detach_client_demand(&mut state, id, None, None, ClientDemandResult::Abandoned);
                state.work_generation = state.work_generation.wrapping_add(1);
                (true, Some(retirement))
            }
        };
        if let Some(retirement) = retirement {
            retirement.finish();
        }
        drop(mutation);
        self.work_available.notify_all();
        accepted
    }

    pub(super) fn abandon_blocked_client_demand(
        &self,
        id: EvaluationWorkId,
        subscription_epoch: u64,
    ) -> Option<WorkDependency> {
        let mutation = self.admission.mutation_guard();
        let (dependency, registration) = {
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state.work.get(&id)?;
            if !matches!(record.state, WorkState::Blocked) {
                return None;
            }
            let WorkKind::ClientDemand(client) = &record.kind else {
                return None;
            };
            let subscription = client
                .subscription
                .as_ref()
                .expect("blocked client demand must retain its exact subscription");
            if subscription.registration.subscription_epoch != subscription_epoch {
                return None;
            }
            (subscription.dependency.clone(), subscription.registration)
        };

        // Completion becomes authoritative before its exact subscriber is
        // delivered. Recheck the source outside the coordinator lock so a
        // publisher in that narrow handoff cannot be mistaken for stable
        // blocking. A later completion linearizes after abandonment.
        if dependency.is_terminal() {
            let queued = {
                let mut state = self
                    .state
                    .lock()
                    .expect("evaluation work coordinator was poisoned");
                let queued =
                    queue_current_registration(&mut state, registration, Some(dependency.key()));
                if queued {
                    state.work_generation = state.work_generation.wrapping_add(1);
                }
                queued
            };
            drop(mutation);
            if queued {
                self.work_available.notify_all();
            }
            return None;
        }

        let (dependency, retirement) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state.work.get(&id)?;
            if !matches!(record.state, WorkState::Blocked) {
                return None;
            }
            let WorkKind::ClientDemand(client) = &record.kind else {
                return None;
            };
            let subscription = client
                .subscription
                .as_ref()
                .expect("blocked client demand must retain its exact subscription");
            if subscription.registration.subscription_epoch != subscription_epoch {
                return None;
            }
            let retirement =
                detach_client_demand(&mut state, id, None, None, ClientDemandResult::Abandoned);
            state.work_generation = state.work_generation.wrapping_add(1);
            (dependency, retirement)
        };
        retirement.finish();
        drop(mutation);
        self.work_available.notify_all();
        Some(dependency)
    }

    #[cfg(test)]
    pub(super) fn kill_client_demand(
        &self,
        id: EvaluationWorkId,
        failure: Arc<EvaluationFailure>,
    ) -> bool {
        let mutation = self.admission.mutation_guard();
        let retirement = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get(&id) else {
                return false;
            };
            if !matches!(record.kind, WorkKind::ClientDemand(_))
                || matches!(record.state, WorkState::Running | WorkState::Terminalizing)
            {
                return false;
            }
            let retirement = detach_client_demand(
                &mut state,
                id,
                None,
                None,
                ClientDemandResult::Killed(failure),
            );
            state.work_generation = state.work_generation.wrapping_add(1);
            retirement
        };
        retirement.finish();
        drop(mutation);
        self.work_available.notify_all();
        true
    }

    pub(super) fn reserve_reflection(
        &self,
        session: &EvaluationDemandState,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
    ) -> Result<EvaluationWorkId, Arc<str>> {
        self.insert_reflection(session, task, wait, WorkState::Reserved)
    }

    pub(super) fn register_dormant_reflection(
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
            assert!(state.reflection_by_task.insert(task, id).is_none());
            assert!(state.reflection_by_wait.insert(wait, id).is_none());
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
    pub(super) fn install_reflection_machine(
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
                || reflection_work(record).machine.is_none()
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
            let claimed = claim_ready_task(&mut state, Some(session));
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
                .reflection_by_task
                .get(&task)
                .or_else(|| state.deferred_by_task.get(&task))
                .copied()?;
            let work = match state.work.get(&id)?.kind {
                WorkKind::Reflection(_) => claim_reflection_task(&mut state, id),
                WorkKind::Deferred(_) => {
                    claim_deferred(&mut state, id, false).map(ClaimedTaskWork::Deferred)
                }
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

    pub(super) fn release_reflection(
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
        drop(mutation);
        self.work_available.notify_all();
        release.machine = machine;
        release
    }

    pub(super) fn retire_reflection(
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

    pub(super) fn reserve_deferred(
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
            if let Some(id) = state.deferred_by_value.get(&deferred).copied() {
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
                    state: WorkState::Reserved,
                    kind: WorkKind::Deferred(DeferredWork {
                        task,
                        wait: wait.clone(),
                        producer,
                        machine: machine.take(),
                        block: None,
                        demanded_while_reserved: false,
                    }),
                };
                assert!(state.work.insert(id, record).is_none());
                assert!(state.deferred_by_task.insert(task, id).is_none());
                assert!(state.deferred_by_wait.insert(wait, id).is_none());
                assert!(state.deferred_by_value.insert(deferred, id).is_none());
                state
                    .work_by_session
                    .entry(session.id)
                    .or_default()
                    .insert(id);
                state.work_generation = state.work_generation.wrapping_add(1);
                DeferredWorkReservation::New(id)
            }
        };
        drop(mutation);
        // A racing producer may have installed the canonical machine while we
        // were constructing this candidate. Dispose the unused candidate only
        // after releasing coordinator state and mutation admission.
        drop(machine);
        if matches!(reservation, DeferredWorkReservation::New(_)) {
            self.work_available.notify_all();
        }
        Ok(reservation)
    }

    pub(super) fn deferred_wait(&self, producer: DeferredValueId) -> Option<EvaluationWaitToken> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let work = state.deferred_by_value.get(&producer)?;
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

    /// Finishes the temporary coordinator-first installation handshake.
    /// Demand observed while the session installed its machine is preserved
    /// and makes the producer worker-ready immediately.
    pub(super) fn activate_deferred(&self, id: EvaluationWorkId) -> bool {
        let mutation = self.admission.mutation_guard();
        let activated = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let demanded = {
                let Some(record) = state.work.get_mut(&id) else {
                    return false;
                };
                assert!(matches!(record.kind, WorkKind::Deferred(_)));
                if !matches!(record.state, WorkState::Reserved) {
                    return false;
                }
                let demanded = deferred_work(record).demanded_while_reserved;
                record.state = if demanded {
                    WorkState::Queued
                } else {
                    WorkState::Dormant
                };
                demanded
            };
            if demanded {
                queue_deferred(&mut state, id);
            }
            state.work_generation = state.work_generation.wrapping_add(1);
            true
        };
        drop(mutation);
        self.work_available.notify_all();
        activated
    }

    #[cfg(test)]
    pub(super) fn promote_deferred_wait(&self, wait: &EvaluationWaitToken) -> bool {
        let mutation = self.admission.mutation_guard();
        let promoted = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(id) = state.deferred_by_wait.get(wait).copied() else {
                return false;
            };
            let next = state.work.get(&id).map(|record| record.state);
            match next {
                Some(WorkState::Reserved) => {
                    deferred_work_mut(
                        state
                            .work
                            .get_mut(&id)
                            .expect("reserved deferred work must remain registered"),
                    )
                    .demanded_while_reserved = true;
                    state.work_generation = state.work_generation.wrapping_add(1);
                    true
                }
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

    pub(super) fn release_deferred(
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

    pub(super) fn retire_deferred(&self, id: EvaluationWorkId) {
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

    pub(super) fn abandon_deferred_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<AbandonedDeferredWork> {
        let mutation = self.admission.mutation_guard();
        let abandoned = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let id = state.deferred_by_wait.get(wait).copied()?;
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

    pub(super) fn producer_for_wait(&self, wait: &EvaluationWaitToken) -> Option<EvaluationTaskId> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        if let Some(id) = state.promise_by_wait.get(wait) {
            return state.work.get(id).and_then(task_for_record);
        }
        if let Some(id) = state.deferred_by_wait.get(wait) {
            return state.work.get(id).map(|record| deferred_work(record).task);
        }
        state
            .reflection_by_wait
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
                .reflection_by_task
                .get(&task)
                .or_else(|| state.deferred_by_task.get(&task))
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
        let (producer, status_update) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let (producer, failure, status_update) = {
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
                    ProducerSettlementObligation::DeferredClaim { .. } => None,
                };
                (producer, failure, status_update)
            };
            if let Some((owner, task, failure)) = failure {
                insert_task_failure(&mut state.failures, owner, task, failure);
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            (producer, status_update)
        };
        let wait = match &producer {
            ProducerSettlementObligation::ReflectionTask(publisher) => &publisher.wait,
            ProducerSettlementObligation::DeferredClaim { wait, producer } => {
                let _producer = producer.id();
                wait
            }
        };
        let (terminal, wake) = wait.publish_terminal_guarded(self, &mutation, terminal);
        let status_wake = status_update.map(|(publisher, status)| {
            debug_assert_eq!(status, terminal_task_status(&terminal));
            publisher.publish_guarded(&mutation, status)
        });
        drop(mutation);

        // A task-owned promise is assigned synchronously by its owning machine
        // while this work is Running. Cancellation from another thread only
        // records a request; terminalization happens when that same poll
        // releases the machine. Therefore an assignment observed here has
        // already removed its dynamic obligation. A promise with an
        // independently usable resolver is resolver-owned instead and must not
        // enter this inventory.
        self.fail_task_promises(work, promise_failure);
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

        // The deferred producer clone, exact wakes, and any values they
        // release are all disposed after coordinator/component locks and
        // mutation admission have been released.
        drop(producer);
        wake.notify();
        if let Some(status_wake) = status_wake {
            status_wake.notify();
        }
        terminal
    }

    fn fail_task_promises(
        self: &Arc<Self>,
        work: EvaluationWorkId,
        failure: Arc<EvaluationFailure>,
    ) {
        let mutation = self.admission.mutation_guard();
        let obligations = {
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get(&work) else {
                return;
            };
            record.obligations.owned_promises.clone()
        };
        drop(mutation);
        for obligation in obligations {
            if let Some(promise) = obligation.cell.upgrade() {
                let _ = promise.fail(failure.clone());
            } else {
                let mutation = self.admission.mutation_guard();
                assert!(self.complete_task_promise_guarded(
                    &mutation,
                    work,
                    &obligation.wait,
                    obligation.promise,
                ));
                let (_, wake) = obligation.wait.publish_terminal_guarded(
                    self,
                    &mutation,
                    EvaluationWaitTerminal::Failed(failure.clone()),
                );
                drop(mutation);
                wake.notify();
            }
        }
    }

    pub(super) fn task_dependency(&self, task: EvaluationTaskId) -> Option<WorkDependency> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let id = state
            .reflection_by_task
            .get(&task)
            .or_else(|| state.deferred_by_task.get(&task))?;
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
            .reflection_by_task
            .get(&task)
            .or_else(|| state.deferred_by_task.get(&task))?;
        state.work.get(id).and_then(task_observation_epoch)
    }

    pub(super) fn task_is_claimable(&self, task: EvaluationTaskId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let id = state
            .reflection_by_task
            .get(&task)
            .or_else(|| state.deferred_by_task.get(&task));
        id.and_then(|id| state.work.get(id))
            .is_some_and(|record| matches!(record.state, WorkState::Dormant | WorkState::Queued))
    }

    pub(super) fn task_is_busy(&self, task: EvaluationTaskId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let id = state
            .reflection_by_task
            .get(&task)
            .or_else(|| state.deferred_by_task.get(&task));
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
    pub(super) fn deferred_counts(&self, session: EvaluationSessionId) -> (usize, usize, usize) {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let active = state
            .deferred_by_value
            .values()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count();
        let waits = state
            .deferred_by_wait
            .values()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count();
        let tasks = state
            .deferred_by_task
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

    fn subscribe_dependency_guarded(
        &self,
        mutation: &dyn RuntimeMutationAuthority,
        dependency: WorkDependency,
        registration: WakeRegistration,
    ) -> bool {
        let source = dependency.key();
        match dependency.subscribe_work(self.runtime, registration) {
            CompletionSubscriptionOutcome::Pending => false,
            CompletionSubscriptionOutcome::AlreadyTerminal => self.wake_dependency_batch_guarded(
                mutation,
                DependencyWakeBatch {
                    source,
                    registrations: vec![registration],
                },
            ),
            CompletionSubscriptionOutcome::ForeignRuntime => {
                unreachable!("foreign dependencies must be rejected before task publication")
            }
        }
    }

    /// Queues registrations which still describe the work's current blocked
    /// dependency. The caller already owns this runtime's mutation admission;
    /// dependency publication and the scheduler transition therefore form one
    /// settlement-visible update without nesting component mutexes.
    pub(super) fn wake_dependency_batch_guarded(
        &self,
        _mutation: &dyn RuntimeMutationAuthority,
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
            .reflection_by_wait
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
            .reflection_by_task
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

fn spark_work(record: &WorkRecord) -> &SparkWork {
    match &record.kind {
        WorkKind::Spark(work) => work,
        WorkKind::Reflection(_) => panic!("reflection work cannot be used as a spark"),
        WorkKind::Deferred(_) => panic!("deferred work cannot be used as a spark"),
        WorkKind::ClientDemand(_) => panic!("client demand cannot be used as a spark"),
    }
}

fn spark_work_mut(record: &mut WorkRecord) -> &mut SparkWork {
    match &mut record.kind {
        WorkKind::Spark(work) => work,
        WorkKind::Reflection(_) => panic!("reflection work cannot be used as a spark"),
        WorkKind::Deferred(_) => panic!("deferred work cannot be used as a spark"),
        WorkKind::ClientDemand(_) => panic!("client demand cannot be used as a spark"),
    }
}

fn reflection_work(record: &WorkRecord) -> &ReflectionWork {
    match &record.kind {
        WorkKind::Reflection(work) => work,
        WorkKind::Spark(_) => panic!("spark work cannot be used as a reflection task"),
        WorkKind::Deferred(_) => panic!("deferred work cannot be used as a reflection task"),
        WorkKind::ClientDemand(_) => {
            panic!("client demand cannot be used as a reflection task")
        }
    }
}

fn reflection_work_mut(record: &mut WorkRecord) -> &mut ReflectionWork {
    match &mut record.kind {
        WorkKind::Reflection(work) => work,
        WorkKind::Spark(_) => panic!("spark work cannot be used as a reflection task"),
        WorkKind::Deferred(_) => panic!("deferred work cannot be used as a reflection task"),
        WorkKind::ClientDemand(_) => {
            panic!("client demand cannot be used as a reflection task")
        }
    }
}

fn deferred_work(record: &WorkRecord) -> &DeferredWork {
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

fn deferred_work_mut(record: &mut WorkRecord) -> &mut DeferredWork {
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

fn client_demand_work_mut(record: &mut WorkRecord) -> &mut ClientDemandWork {
    match &mut record.kind {
        WorkKind::ClientDemand(work) => work,
        WorkKind::Spark(_) => panic!("spark work cannot be used as a client demand"),
        WorkKind::Reflection(_) => {
            panic!("reflection work cannot be used as a client demand")
        }
        WorkKind::Deferred(_) => panic!("deferred work cannot be used as a client demand"),
    }
}

fn task_for_record(record: &WorkRecord) -> Option<EvaluationTaskId> {
    match &record.kind {
        WorkKind::Reflection(work) => Some(work.task),
        WorkKind::Deferred(work) => Some(work.task),
        WorkKind::Spark(_) | WorkKind::ClientDemand(_) => None,
    }
}

fn runtime_work_kind(record: &WorkRecord) -> RuntimeWorkKindSnapshot {
    match record.kind {
        WorkKind::Reflection(_) => RuntimeWorkKindSnapshot::ReflectionTask,
        WorkKind::Deferred(_) => RuntimeWorkKindSnapshot::DeferredEvaluation,
        WorkKind::ClientDemand(_) => RuntimeWorkKindSnapshot::ClientDemand,
        WorkKind::Spark(_) => RuntimeWorkKindSnapshot::Spark,
    }
}

fn runtime_dependency_snapshot(dependency: &WorkDependency) -> RuntimeDependencySnapshot {
    match dependency {
        WorkDependency::Wait(wait) => RuntimeDependencySnapshot::Wait {
            wait: wait.get(),
            producer: wait.producer(),
            session: wait.owner_id(),
        },
        WorkDependency::Promise(promise) => RuntimeDependencySnapshot::Promise {
            promise: promise.id().get(),
            producer: dependency
                .producer_wait()
                .map(|wait| (wait.get(), wait.producer(), wait.owner_id())),
        },
        #[cfg(test)]
        WorkDependency::Test(dependency) => RuntimeDependencySnapshot::Test(dependency.id.get()),
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

fn publish_reflection_exit_locked(
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

fn queue_reflection(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
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

fn queue_deferred(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
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

fn queue_task(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    if state.ready_task_set.insert(id) {
        state.ready_tasks.push_back(id);
    }
}

fn remove_ready_reflection(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    remove_ready_task(state, id);
}

fn remove_ready_deferred(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    remove_ready_task(state, id);
}

fn remove_ready_task(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    state.ready_task_set.remove(&id);
    state.ready_tasks.retain(|candidate| *candidate != id);
}

fn claim_ready_task(
    state: &mut WorkCoordinatorState,
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
            WorkKind::Reflection(_) => claim_reflection_task(state, id),
            WorkKind::Deferred(_) => claim_deferred(state, id, true).map(ClaimedTaskWork::Deferred),
            WorkKind::Spark(_) | WorkKind::ClientDemand(_) => None,
        };
        if let Some(claimed) = claimed {
            return Some(claimed);
        }
    }
}

fn claim_reflection_task(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
) -> Option<ClaimedTaskWork> {
    claim_reflection(state, id).map(ClaimedTaskWork::Reflection)
}

fn claim_reflection(
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

fn claim_deferred(
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
        WorkState::ExitWaiting => ReflectionWorkState::ExitWaiting(
            reflection_work(record)
                .exit
                .clone()
                .expect("exit-waiting reflection work must retain its exit summary"),
        ),
        WorkState::Terminalizing => ReflectionWorkState::Terminalizing,
    }
}

fn insert_task_failure(
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

fn remove_task_failure(
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

fn detach_reflection(
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
        state.reflection_by_task.remove(&reflection.task),
        Some(id),
        "reflection task index must agree with its work record"
    );
    assert_eq!(
        state.reflection_by_wait.remove(&reflection.wait),
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

fn promote_deferred_wait_locked(
    state: &mut WorkCoordinatorState,
    wait: &EvaluationWaitToken,
) -> bool {
    let Some(id) = state.deferred_by_wait.get(wait).copied() else {
        return false;
    };
    match state.work.get(&id).map(|record| record.state) {
        Some(WorkState::Reserved) => {
            deferred_work_mut(
                state
                    .work
                    .get_mut(&id)
                    .expect("reserved deferred work must remain registered"),
            )
            .demanded_while_reserved = true;
            true
        }
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
        current = *state.deferred_by_wait.get(wait)?;
    }
}

struct DeferredDependencyCycle {
    members: Vec<EvaluationWorkId>,
    contains_promise: bool,
}

fn terminalize_pure_lazy_cycle(
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

fn begin_deferred_abandonment(
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

fn detach_deferred(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
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
    assert_eq!(state.deferred_by_task.remove(&deferred.task), Some(id));
    assert_eq!(state.deferred_by_wait.remove(&deferred.wait), Some(id));
    assert_eq!(
        state.deferred_by_value.remove(&deferred.producer.id()),
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

fn queue_spark(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    if state.ready_spark_set.insert(id) {
        state.ready_sparks.push_back(id);
    }
}

fn queue_client_demand(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    if state.ready_client_demand_set.insert(id) {
        state.ready_client_demands.push_back(id);
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

fn claim_ready_client_demand(state: &mut WorkCoordinatorState) -> Option<ClaimedClientDemand> {
    while let Some(id) = state.ready_client_demands.pop_front() {
        state.ready_client_demand_set.remove(&id);
        if let Some(claimed) = claim_client_demand(state, id) {
            return Some(claimed);
        }
    }
    None
}

fn claim_client_demand(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
) -> Option<ClaimedClientDemand> {
    let record = state.work.get_mut(&id)?;
    if !matches!(record.state, WorkState::Queued)
        || !matches!(record.kind, WorkKind::ClientDemand(_))
    {
        return None;
    }
    state.ready_client_demand_set.remove(&id);
    state
        .ready_client_demands
        .retain(|candidate| *candidate != id);
    record.state = WorkState::Running;
    let client = client_demand_work_mut(record);
    let demand = client.demand.clone();
    let operation = client
        .operation
        .take()
        .expect("queued client demand must retain its operation");
    let prior_subscription = client.subscription.take();
    Some(ClaimedClientDemand {
        id,
        demand,
        operation: Some(operation),
        prior_subscription,
    })
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
    prune_closed_session_registration(state, record.demand_session);
    let WorkKind::Spark(mut spark) = record.kind else {
        unreachable!("spark retirement must contain spark work")
    };
    assert!(
        record.obligations.is_empty(),
        "spark work must not acquire producer settlement obligations"
    );
    Some(SparkRetirement {
        demand: spark
            .demand
            .take()
            .expect("non-running spark work must retain its demand"),
        dependencies: spark.dependency.take().into_iter().collect(),
        _obligations: record.obligations,
    })
}

fn detach_client_demand(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
    claimed_operation: Option<ClientDemandOperation>,
    claimed_subscription: Option<ClientDemandSubscription>,
    result: ClientDemandResult,
) -> ClientDemandRetirement {
    state.ready_client_demand_set.remove(&id);
    state
        .ready_client_demands
        .retain(|candidate| *candidate != id);
    state.observation_waiters.remove(&id);
    let mut record = state
        .work
        .remove(&id)
        .expect("retired client demand must remain registered");
    assert!(
        !matches!(record.state, WorkState::Running) || claimed_operation.is_some(),
        "worker-owned client demand requires its claimed operation at retirement"
    );
    let WorkKind::ClientDemand(mut client) = record.kind else {
        panic!("client-demand retirement must contain client work")
    };
    let operation = match (claimed_operation, client.operation.take()) {
        (Some(operation), None) | (None, Some(operation)) => operation,
        (Some(_), Some(_)) => panic!("client demand operation cannot have two owners"),
        (None, None) => panic!("client demand retirement must retain its operation"),
    };
    let subscription = match (claimed_subscription, client.subscription.take()) {
        (Some(subscription), None) | (None, Some(subscription)) => Some(subscription),
        (None, None) => None,
        (Some(_), Some(_)) => panic!("client demand subscription cannot have two owners"),
    };
    let sink = record
        .obligations
        .take_client_sink()
        .expect("client demand must retain its result sink until retirement");
    assert!(
        record.obligations.is_empty(),
        "client demand retirement must consume every settlement obligation"
    );
    if let Some(session_work) = state.work_by_session.get_mut(&record.demand_session) {
        session_work.remove(&id);
        if session_work.is_empty() {
            state.work_by_session.remove(&record.demand_session);
        }
    }
    prune_closed_session_registration(state, record.demand_session);
    ClientDemandRetirement {
        sink,
        operation,
        subscription,
        result,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier, OnceLock};
    use std::thread;

    use super::*;

    /// Real external ownership beside the machine-facing demand record.
    ///
    /// Coordinator tests intentionally bypass `EvalContext`, but should still
    /// make the production ownership boundary explicit instead of treating an
    /// `EvaluationSession` owner as if it were demand state.
    struct TestDemand {
        owner: Arc<EvaluationSession>,
        demand: Arc<EvaluationDemandState>,
    }

    impl TestDemand {
        fn new(coordinator: &Arc<EvaluationWorkCoordinator>) -> Self {
            let owner = EvaluationSession::shared(coordinator);
            let demand = owner.demand.clone();
            Self { owner, demand }
        }

        fn context(&self) -> super::super::EvalContext {
            super::super::EvalContext::new(&self.owner)
        }
    }

    #[test]
    fn observation_epochs_are_nonzero_and_option_niche_optimized() {
        assert_eq!(
            std::mem::size_of::<Option<RuntimeObservationEpoch>>(),
            std::mem::size_of::<u64>()
        );

        let observations = RuntimeObservationState::new();
        assert_eq!(observations.current().get(), 1);
        assert_eq!(observations.advance().get(), 2);
    }

    #[test]
    fn task_block_dependency_identity_includes_the_runtime() {
        let id = NonZeroU64::new(17).expect("test dependency identity must be nonzero");
        let dependency = WorkDependency::Test(TestWorkDependency {
            runtime: crate::runtime::allocate_evaluation_runtime_id(),
            id,
        });
        let same_dependency = dependency.clone();
        let foreign_dependency = WorkDependency::Test(TestWorkDependency {
            runtime: crate::runtime::allocate_evaluation_runtime_id(),
            id,
        });

        assert_eq!(dependency, same_dependency);
        assert_ne!(dependency, foreign_dependency);
        assert_eq!(
            EvaluationTaskBlock {
                dependency: Some(dependency),
                observed_epoch: None,
                error: None,
            },
            EvaluationTaskBlock {
                dependency: Some(same_dependency),
                observed_epoch: None,
                error: None,
            }
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(
        expected = "published task block dependency must belong to its coordinator runtime"
    )]
    fn task_block_publication_asserts_the_runtime_invariant() {
        let runtime = crate::runtime::allocate_evaluation_runtime_id();
        let foreign_runtime = crate::runtime::allocate_evaluation_runtime_id();
        let block = EvaluationTaskBlock {
            dependency: Some(WorkDependency::Test(TestWorkDependency {
                runtime: foreign_runtime,
                id: NonZeroU64::new(23).expect("test dependency identity must be nonzero"),
            })),
            observed_epoch: None,
            error: None,
        };

        debug_assert_task_block_runtime(runtime, &block);
    }

    #[test]
    fn promise_dependency_projects_only_a_task_owned_producer_wait() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let context = session.context().for_effect_task();
        let resolver_owned = PromisedValue::new(context.values(), "resolver-owned promise");
        let task_owned = PromisedValue::fixpoint(&context, "task-owned promise")
            .expect("the local task should own its promise");

        assert!(
            WorkDependency::Promise(resolver_owned)
                .producer_wait()
                .is_none()
        );
        assert_eq!(
            WorkDependency::Promise(task_owned.clone()).producer_wait(),
            task_owned.task().map(|task| task.wait())
        );
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

        fn complete_guarded(
            &self,
            coordinator: &Arc<EvaluationWorkCoordinator>,
            mutation: &RuntimeMutationGuard<'_>,
        ) -> CompletionWake {
            let ((), wake) = self
                .subscriptions
                .publish_guarded(coordinator, mutation, || {
                    let _ = self.terminal.set(());
                    Ok::<_, std::convert::Infallible>(())
                })
                .expect("infallible guarded test completion should publish");
            wake
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
                .lock()
                .expect("test work-coordinator binding was poisoned")
                .upgrade()
                .is_some()
        }
    }

    impl EvaluationWorkCoordinator {
        fn park_claimed_test_reflection(
            self: &Arc<Self>,
            mut claimed: ClaimedReflectionWork,
            source: &TestCompletionSource,
            before_insert: impl FnOnce(),
        ) -> WakeRegistration {
            assert_eq!(source.runtime_id(), self.runtime);
            let mutation = self.admission.mutation_guard();
            let (dependency, registration) = {
                let mut state = self
                    .state
                    .lock()
                    .expect("evaluation work coordinator was poisoned");
                let record = state
                    .work
                    .get(&claimed.id)
                    .expect("claimed test reflection work must remain registered");
                assert_eq!(record.demand_session, claimed.demand_session);
                assert_eq!(reflection_work(record).task, claimed.task);
                assert!(matches!(record.state, WorkState::Running));
                let reflection = reflection_work_mut(
                    state
                        .work
                        .get_mut(&claimed.id)
                        .expect("claimed test reflection work must remain registered"),
                );
                assert!(reflection.machine.is_none());
                reflection.machine = claimed.machine.take();
                let exact = publish_task_block_locked(
                    &mut state,
                    self.runtime,
                    claimed.id,
                    EvaluationTaskBlock {
                        dependency: Some(source.dependency()),
                        observed_epoch: None,
                        error: None,
                    },
                )
                .expect("the synthetic task block should retain its dependency");
                state.work_generation = state.work_generation.wrapping_add(1);
                exact
            };
            assert!(dependency.same_source(&source.dependency()));
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
            self.notify_dependency_wake(woke);
            registration
        }

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
        TestDemand,
        ClaimedSparkWork,
    ) {
        let (coordinator, executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        coordinator.executor_started(1);
        coordinator.submit_spark(session.demand.clone(), crate::core::keys::unit_value());
        let CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("test spark should be claimable")
        };
        (coordinator, executor, session, claimed)
    }

    fn publish_test_observation(
        coordinator: &EvaluationWorkCoordinator,
    ) -> RuntimeObservationEpoch {
        let mutation = coordinator.admission.mutation_guard();
        let epoch = coordinator.observations.advance();
        let changed = coordinator.publish_runtime_observation_guarded(&mutation, epoch);
        drop(mutation);
        coordinator.observations.notify_all();
        coordinator.notify_runtime_observation(changed);
        epoch
    }

    struct TestTaskMachine;

    impl EvaluationTaskMachine for TestTaskMachine {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            panic!("coordinator lifecycle tests drive deferred polls explicitly")
        }
    }

    fn activate_test_reflection(coordinator: &EvaluationWorkCoordinator, work: EvaluationWorkId) {
        coordinator
            .install_reflection_machine(work, Box::new(TestTaskMachine))
            .unwrap_or_else(|_| panic!("reserved test reflection must accept its machine"));
        assert!(coordinator.activate_reflection(work));
    }

    struct CheckDeferredDropLocks {
        coordinator: Weak<EvaluationWorkCoordinator>,
        dropped_without_runtime_locks: Arc<AtomicBool>,
    }

    impl EvaluationTaskMachine for CheckDeferredDropLocks {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            panic!("the coordinator test drives this machine's terminal poll")
        }
    }

    impl Drop for CheckDeferredDropLocks {
        fn drop(&mut self) {
            let unlocked = self.coordinator.upgrade().is_none_or(|coordinator| {
                let state_unlocked = coordinator.state.try_lock().is_ok();
                let admission_unlocked = coordinator.admission.try_settlement_guard().is_some();
                state_unlocked && admission_unlocked
            });
            self.dropped_without_runtime_locks
                .store(unlocked, Ordering::Release);
        }
    }

    fn reserve_ready_test_reflection(
        coordinator: &EvaluationWorkCoordinator,
        session: &TestDemand,
    ) -> (EvaluationTaskId, EvaluationWorkId) {
        let task = super::super::allocate_task_id(&session.demand.values)
            .expect("reflection task identity should allocate");
        let wait = super::super::allocate_wait_token(&session.demand, task)
            .expect("reflection wait identity should allocate");
        let work = coordinator
            .reserve_reflection(&session.demand, task, wait)
            .expect("open test session should reserve reflection work");
        activate_test_reflection(coordinator, work);
        (task, work)
    }

    fn claim_ready_test_reflection(
        coordinator: &EvaluationWorkCoordinator,
        session: EvaluationSessionId,
    ) -> ClaimedReflectionWork {
        let ClaimedTaskWork::Reflection(claimed) = coordinator
            .claim_ready_task_for_session(session)
            .expect("queued reflection work should be claimable")
        else {
            panic!("queued reflection work should preserve its kind")
        };
        claimed
    }

    fn settle_test_reflection(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        work: EvaluationWorkId,
    ) {
        coordinator.settle_terminal_work(
            work,
            EvaluationWaitTerminal::Cancelled,
            Arc::new(EvaluationFailure::message("test reflection settlement")),
        );
        drop(coordinator.retire_reflection(work));
    }

    fn settle_test_deferred(coordinator: &Arc<EvaluationWorkCoordinator>, work: EvaluationWorkId) {
        coordinator.settle_terminal_work(
            work,
            EvaluationWaitTerminal::Abandoned,
            Arc::new(EvaluationFailure::message("test deferred settlement")),
        );
        coordinator.retire_deferred(work);
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
    fn task_completion_during_subscription_cannot_lose_the_wake() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let session_id = session.demand.id;
        let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
        let claimed = claim_ready_test_reflection(&coordinator, session_id);
        let source = TestCompletionSource::new(&coordinator);
        let started = Arc::new(Barrier::new(2));
        let completer = Arc::new(Mutex::new(None));

        coordinator.park_claimed_test_reflection(claimed, &source, {
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
        });
        completer
            .lock()
            .expect("completion thread slot was poisoned")
            .take()
            .expect("the subscription hook should start a completer")
            .join()
            .expect("test completion thread should not panic");

        assert_eq!(source.subscriber_count(), 0);
        assert_eq!(coordinator.ready_task_count(), 1);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        assert!(
            coordinator
                .release_reflection(claimed, ReflectionWorkPoll::Terminal)
                .terminal
        );
        settle_test_reflection(&coordinator, work);
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
    fn guarded_completion_defers_scheduler_notification_until_admission_is_released() {
        let (coordinator, _executor, _session, claimed) = claimed_test_spark();
        let source = TestCompletionSource::new(&coordinator);
        let Ok(_registration) = coordinator.park_claimed_test_spark(claimed, &source, || {}) else {
            panic!("same-runtime completion source should accept the subscription")
        };
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 1));

        let mutation = coordinator.admission.mutation_guard();
        let wake = source.complete_guarded(&coordinator, &mutation);
        assert!(source.is_terminal());
        assert_eq!(source.subscriber_count(), 0);
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));

        drop(mutation);
        wake.notify();
        finish_queued_test_spark(&coordinator);
    }

    #[test]
    fn static_producer_obligations_are_taken_once() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let task = super::super::allocate_task_id(&session.demand.values)
            .expect("obligation task identity should allocate");
        let wait = super::super::allocate_wait_token(&session.demand, task)
            .expect("obligation wait identity should allocate");

        let mut reflection = SettlementObligations::reflection_task(wait.clone());
        let Some(ProducerSettlementObligation::ReflectionTask(publisher)) =
            reflection.take_producer()
        else {
            panic!("reflection inventory should contain its task wait")
        };
        assert_eq!(publisher.wait, wait);
        assert!(reflection.take_producer().is_none());

        let lazy = LazyValue::deferred(&session.demand.values, "static obligation", |_| {
            panic!("static obligation test never evaluates its synthetic lazy")
        });
        let producer = DeferredProducer::Lazy(lazy);
        let mut deferred = SettlementObligations::deferred_claim(wait.clone(), producer.clone());
        let Some(ProducerSettlementObligation::DeferredClaim {
            wait: obligation_wait,
            producer: obligation_producer,
        }) = deferred.take_producer()
        else {
            panic!("deferred inventory should contain its wait and claim")
        };
        assert_eq!(obligation_wait, wait);
        assert_eq!(obligation_producer.id(), producer.id());
        assert!(deferred.take_producer().is_none());
    }

    #[test]
    fn terminal_settlement_publishes_once_before_reporting_retirement() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let task = super::super::allocate_task_id(&session.demand.values)
            .expect("settlement task identity should allocate");
        let wait = super::super::allocate_wait_token(&session.demand, task)
            .expect("settlement wait identity should allocate");
        let work = coordinator
            .reserve_reflection(&session.demand, task, wait.clone())
            .expect("open test session should reserve reflection work");
        activate_test_reflection(&coordinator, work);
        assert!(coordinator.terminalize_reflection(work));

        let terminal = coordinator.settle_terminal_work(
            work,
            EvaluationWaitTerminal::Cancelled,
            Arc::new(EvaluationFailure::message("settled test producer")),
        );
        assert_eq!(terminal, EvaluationWaitTerminal::Cancelled);
        assert_eq!(
            wait.terminal_poll(),
            Some(super::super::EvaluationWaitPoll::Cancelled)
        );
        assert!(matches!(
            coordinator
                .reflection_snapshots(session.demand.id)
                .as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Terminalizing,
                ..
            }]
        ));

        drop(coordinator.retire_reflection(work));
        assert!(
            coordinator
                .reflection_snapshots(session.demand.id)
                .is_empty()
        );
    }

    #[test]
    fn session_close_does_not_steal_an_already_terminalizing_claims_settlement() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let session_id = session.demand.id;
        let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
        let claimed = claim_ready_test_reflection(&coordinator, session_id);

        let release = coordinator.release_reflection(claimed, ReflectionWorkPoll::Terminal);
        assert!(release.terminal);
        drop(session);
        assert!(matches!(
            coordinator.reflection_snapshots(session_id).as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Terminalizing,
                ..
            }]
        ));

        settle_test_reflection(&coordinator, work);
        assert_eq!(coordinator.registered_session_count(), 0);
    }

    #[test]
    fn session_close_preserves_an_earlier_running_task_cancellation() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let session_id = session.demand.id;
        let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
        let claimed = claim_ready_test_reflection(&coordinator, session_id);

        assert_eq!(
            coordinator.request_reflection_cancellation(work),
            ReflectionCancellation::Requested
        );
        drop(session);
        let release = coordinator.release_reflection(claimed, ReflectionWorkPoll::Terminal);
        assert!(release.terminal);
        assert!(release.cancel);
        assert!(!release.abandoned);

        settle_test_reflection(&coordinator, work);
        assert_eq!(coordinator.registered_session_count(), 0);
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
            RuntimeObservationState::new(),
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

        assert_eq!(promise.exact_subscription_count(), 0);
        assert_eq!(coordinator.retained_spark_count(), 0);
    }

    #[test]
    fn foreign_wait_dependency_retires_work_without_subscribing() {
        let (coordinator, _executor, _session, claimed) = claimed_test_spark();
        let (foreign_coordinator, _foreign_executor) = super::super::test_execution_resources(0)
            .expect("foreign execution resources should build");
        let foreign_session = TestDemand::new(&foreign_coordinator);
        let producer = super::super::allocate_task_id(&foreign_session.demand.values)
            .expect("foreign producer identity should allocate");
        let wait = super::super::allocate_wait_token(&foreign_session.demand, producer)
            .expect("foreign wait identity should allocate");

        coordinator.release_spark(
            claimed,
            SparkWorkPoll::Blocked(WorkDependency::Wait(wait.clone())),
        );

        assert_eq!(wait.exact_subscription_count(), 0);
        assert_eq!(coordinator.retained_spark_count(), 0);
    }

    #[test]
    fn coordinator_selects_exact_ready_work_without_a_session_queue() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let task = super::super::allocate_task_id(&session.demand.values)
            .expect("reflection task identity should allocate");
        let wait = super::super::allocate_wait_token(&session.demand, task)
            .expect("reflection wait identity should allocate");
        let work = coordinator
            .reserve_reflection(&session.demand, task, wait)
            .expect("open test session should reserve reflection work");
        activate_test_reflection(&coordinator, work);

        assert_eq!(coordinator.registered_session_count(), 1);
        assert_eq!(coordinator.ready_task_count(), 1);

        let CoordinatorSelection::Task(ClaimedTaskWork::Reflection(claimed)) = coordinator.select()
        else {
            panic!("the exact ready task should be selected")
        };
        assert_eq!(coordinator.ready_task_count(), 0);

        coordinator.requeue_unpolled_task(ClaimedTaskWork::Reflection(claimed));
        assert!(coordinator.terminalize_reflection(work));
        settle_test_reflection(&coordinator, work);
        drop(session);
        assert_eq!(coordinator.registered_session_count(), 0);
    }

    #[test]
    fn serial_ready_selection_filters_exact_work_by_demand_session() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let left = TestDemand::new(&coordinator);
        let right = TestDemand::new(&coordinator);
        let (_, left_work) = reserve_ready_test_reflection(&coordinator, &left);
        let (right_task, right_work) = reserve_ready_test_reflection(&coordinator, &right);

        let right_claim = claim_ready_test_reflection(&coordinator, right.demand.id);
        assert_eq!(right_claim.task(), right_task);
        let release = coordinator.release_reflection(right_claim, ReflectionWorkPoll::Terminal);
        assert!(release.terminal);
        settle_test_reflection(&coordinator, right_work);

        let left_claim = claim_ready_test_reflection(&coordinator, left.demand.id);
        let release = coordinator.release_reflection(left_claim, ReflectionWorkPoll::Terminal);
        assert!(release.terminal);
        settle_test_reflection(&coordinator, left_work);
    }

    #[test]
    fn coordinator_owns_the_reflection_lifecycle() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let task = super::super::allocate_task_id(&session.demand.values)
            .expect("reflection task identity should allocate");
        let wait = super::super::allocate_wait_token(&session.demand, task)
            .expect("reflection wait identity should allocate");
        let work = coordinator
            .reserve_reflection(&session.demand, task, wait)
            .expect("open test session should reserve reflection work");
        assert_eq!(
            coordinator.reflection_snapshots(session.demand.id),
            vec![ReflectionWorkSnapshot {
                task,
                state: ReflectionWorkState::Reserved,
            }]
        );

        activate_test_reflection(&coordinator, work);
        assert!(matches!(
            coordinator
                .reflection_snapshots(session.demand.id)
                .as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Queued,
                ..
            }]
        ));

        let ClaimedTaskWork::Reflection(claimed) = coordinator
            .claim_ready_task_for_session(session.demand.id)
            .expect("queued reflection work should be claimable")
        else {
            panic!("queued reflection work should preserve its kind")
        };
        assert_eq!(claimed.id(), work);
        assert!(
            coordinator.claim_task(task).is_none(),
            "a running reflection work record must grant only one machine claim"
        );
        assert!(matches!(
            coordinator
                .reflection_snapshots(session.demand.id)
                .as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Running,
                ..
            }]
        ));

        let observed = coordinator.observations.current();
        let block = EvaluationTaskBlock {
            dependency: None,
            observed_epoch: Some(observed),
            error: None,
        };
        let release =
            coordinator.release_reflection(claimed, ReflectionWorkPoll::Blocked(block.clone()));
        assert!(release.made_progress);
        assert!(release.remains_blocked);
        assert!(!release.terminal);
        assert_eq!(
            coordinator.reflection_snapshots(session.demand.id),
            vec![ReflectionWorkSnapshot {
                task,
                state: ReflectionWorkState::Blocked(block),
            }]
        );

        assert!(publish_test_observation(&coordinator) > observed);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        let release = coordinator.release_reflection(claimed, ReflectionWorkPoll::Terminal);
        assert!(release.terminal);
        assert!(matches!(
            coordinator
                .reflection_snapshots(session.demand.id)
                .as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Terminalizing,
                ..
            }]
        ));
        settle_test_reflection(&coordinator, work);
        assert!(
            coordinator
                .reflection_snapshots(session.demand.id)
                .is_empty()
        );
    }

    #[test]
    fn coordinator_rejects_a_block_without_an_exact_or_broad_wake() {
        let mut state = WorkCoordinatorState::default();
        let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            publish_task_block_locked(
                &mut state,
                crate::runtime::allocate_evaluation_runtime_id(),
                EvaluationWorkId(NonZeroU64::new(1).expect("test work identity should be nonzero")),
                EvaluationTaskBlock {
                    dependency: None,
                    observed_epoch: None,
                    error: None,
                },
            )
        }));
        assert!(rejected.is_err());
    }

    #[test]
    fn observation_published_before_block_registration_requeues_on_recheck() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let observed = coordinator.observations.current();
        let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);

        assert!(publish_test_observation(&coordinator) > observed);
        let release = coordinator.release_reflection(
            claimed,
            ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                dependency: None,
                observed_epoch: Some(observed),
                error: None,
            }),
        );
        assert!(release.made_progress);
        assert!(!release.remains_blocked);
        assert_eq!(coordinator.ready_task_count(), 1);

        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        assert!(
            coordinator
                .release_reflection(claimed, ReflectionWorkPoll::Terminal)
                .terminal
        );
        settle_test_reflection(&coordinator, work);
    }

    #[test]
    fn exact_wait_completion_requeues_only_its_cross_session_task() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let producer = TestDemand::new(&coordinator);
        let observer = TestDemand::new(&coordinator);
        let dependency_task = super::super::allocate_task_id(&producer.demand.values)
            .expect("dependency task identity should allocate");
        let dependency = super::super::allocate_wait_token(&producer.demand, dependency_task)
            .expect("dependency wait identity should allocate");
        let unrelated_task = super::super::allocate_task_id(&producer.demand.values)
            .expect("unrelated task identity should allocate");
        let unrelated = super::super::allocate_wait_token(&producer.demand, unrelated_task)
            .expect("unrelated wait identity should allocate");
        let (_, work) = reserve_ready_test_reflection(&coordinator, &observer);
        let claimed = claim_ready_test_reflection(&coordinator, observer.demand.id);

        let release = coordinator.release_reflection(
            claimed,
            ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                dependency: Some(WorkDependency::Wait(dependency.clone())),
                observed_epoch: None,
                error: None,
            }),
        );
        assert!(release.remains_blocked);
        assert_eq!(dependency.exact_subscription_count(), 1);
        assert_eq!(coordinator.ready_task_count(), 0);

        unrelated.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
            &producer.demand.values,
            crate::core::keys::unit_value(),
        )));
        unrelated.notify_terminal();
        assert_eq!(coordinator.ready_task_count(), 0);
        dependency.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
            &producer.demand.values,
            crate::core::keys::unit_value(),
        )));
        dependency.notify_terminal();
        assert_eq!(dependency.exact_subscription_count(), 0);
        assert_eq!(coordinator.ready_task_count(), 1);

        let claimed = claim_ready_test_reflection(&coordinator, observer.demand.id);
        assert!(
            coordinator
                .release_reflection(claimed, ReflectionWorkPoll::Terminal)
                .terminal
        );
        settle_test_reflection(&coordinator, work);
    }

    #[test]
    fn a_task_reblocked_on_another_wait_ignores_its_prior_terminal_source() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let task_a = super::super::allocate_task_id(&session.demand.values)
            .expect("wait A task identity should allocate");
        let wait_a = super::super::allocate_wait_token(&session.demand, task_a)
            .expect("wait A identity should allocate");
        let task_b = super::super::allocate_task_id(&session.demand.values)
            .expect("wait B task identity should allocate");
        let wait_b = super::super::allocate_wait_token(&session.demand, task_b)
            .expect("wait B identity should allocate");
        let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);

        assert!(
            coordinator
                .release_reflection(
                    claimed,
                    ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                        dependency: Some(WorkDependency::Wait(wait_a.clone())),
                        observed_epoch: None,
                        error: None,
                    }),
                )
                .remains_blocked
        );
        assert_eq!(wait_a.exact_subscription_count(), 1);
        wait_a.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
            &session.demand.values,
            crate::core::keys::unit_value(),
        )));
        wait_a.notify_terminal();
        assert_eq!(wait_a.exact_subscription_count(), 0);
        assert_eq!(coordinator.ready_task_count(), 1);

        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        assert!(
            coordinator
                .release_reflection(
                    claimed,
                    ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                        dependency: Some(WorkDependency::Wait(wait_b.clone())),
                        observed_epoch: None,
                        error: None,
                    }),
                )
                .remains_blocked
        );
        assert_eq!(wait_b.exact_subscription_count(), 1);
        assert_eq!(coordinator.ready_task_count(), 0);

        // Re-notifying the prior terminal source cannot revive work whose
        // subscription epoch now names wait B.
        wait_a.notify_terminal();
        assert_eq!(coordinator.ready_task_count(), 0);
        wait_b.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
            &session.demand.values,
            crate::core::keys::unit_value(),
        )));
        wait_b.notify_terminal();
        assert_eq!(coordinator.ready_task_count(), 1);

        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        assert!(
            coordinator
                .release_reflection(claimed, ReflectionWorkPoll::Terminal)
                .terminal
        );
        settle_test_reflection(&coordinator, work);
    }

    #[test]
    fn exact_and_broad_task_wakes_share_one_block_epoch() {
        for exact_wins in [true, false] {
            let (coordinator, _executor) = super::super::test_execution_resources(0)
                .expect("test execution resources should build");
            let session = TestDemand::new(&coordinator);
            let observed = coordinator.observations.current();
            let dependency_task = super::super::allocate_task_id(&session.demand.values)
                .expect("dependency task identity should allocate");
            let dependency = super::super::allocate_wait_token(&session.demand, dependency_task)
                .expect("dependency wait identity should allocate");
            let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
            let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);

            assert!(
                coordinator
                    .release_reflection(
                        claimed,
                        ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                            dependency: Some(WorkDependency::Wait(dependency.clone())),
                            observed_epoch: Some(observed),
                            error: None,
                        }),
                    )
                    .remains_blocked
            );
            assert_eq!(dependency.exact_subscription_count(), 1);

            let complete = || {
                dependency.publish_terminal(EvaluationWaitTerminal::Complete(
                    RuntimeValueRoot::new(&session.demand.values, crate::core::keys::unit_value()),
                ));
                dependency.notify_terminal();
            };
            if exact_wins {
                complete();
                publish_test_observation(&coordinator);
            } else {
                publish_test_observation(&coordinator);
                complete();
            }
            assert_eq!(dependency.exact_subscription_count(), 0);
            assert_eq!(coordinator.ready_task_count(), 1);

            let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
            assert!(
                coordinator
                    .release_reflection(claimed, ReflectionWorkPoll::Terminal)
                    .terminal
            );
            settle_test_reflection(&coordinator, work);
        }
    }

    #[test]
    fn retired_task_makes_a_late_exact_wait_wake_harmless() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let dependency_task = super::super::allocate_task_id(&session.demand.values)
            .expect("dependency task identity should allocate");
        let dependency = super::super::allocate_wait_token(&session.demand, dependency_task)
            .expect("dependency wait identity should allocate");
        let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);

        assert!(
            coordinator
                .release_reflection(
                    claimed,
                    ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                        dependency: Some(WorkDependency::Wait(dependency.clone())),
                        observed_epoch: None,
                        error: None,
                    }),
                )
                .remains_blocked
        );
        assert_eq!(
            coordinator.request_reflection_cancellation(work),
            ReflectionCancellation::Terminalize
        );
        settle_test_reflection(&coordinator, work);
        assert_eq!(dependency.exact_subscription_count(), 1);

        dependency.publish_terminal(EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(
            &session.demand.values,
            crate::core::keys::unit_value(),
        )));
        dependency.notify_terminal();
        assert_eq!(dependency.exact_subscription_count(), 0);
        assert_eq!(coordinator.ready_task_count(), 0);
    }

    #[test]
    fn observation_published_after_block_registration_requeues_exact_work() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let observed = coordinator.observations.current();
        let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        let dependency_task = super::super::allocate_task_id(&session.demand.values)
            .expect("dependency task identity should allocate");
        let dependency = super::super::allocate_wait_token(&session.demand, dependency_task)
            .expect("dependency wait identity should allocate");

        let release = coordinator.release_reflection(
            claimed,
            ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                dependency: Some(WorkDependency::Wait(dependency.clone())),
                observed_epoch: Some(observed),
                error: None,
            }),
        );
        assert!(release.remains_blocked);
        assert_eq!(coordinator.ready_task_count(), 0);
        assert!(matches!(
            coordinator.reflection_snapshots(session.demand.id).as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Blocked(EvaluationTaskBlock {
                    dependency: Some(WorkDependency::Wait(wait)),
                    observed_epoch: Some(epoch),
                    ..
                }),
                ..
            }] if wait == &dependency && *epoch == observed
        ));

        publish_test_observation(&coordinator);
        assert_eq!(coordinator.ready_task_count(), 1);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        assert!(
            coordinator
                .release_reflection(claimed, ReflectionWorkPoll::Terminal)
                .terminal
        );
        settle_test_reflection(&coordinator, work);
    }

    #[test]
    fn permanent_exit_wait_retains_only_its_summary_and_obligations() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let (task, work) = reserve_ready_test_reflection(&coordinator, &session);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        let message =
            RuntimeValueRoot::new(&session.demand.values, crate::core::keys::unit_value());

        let mut release = coordinator.release_reflection(
            claimed,
            ReflectionWorkPoll::Exit(EvaluationExitBlock {
                intent: ExitIntent::Error(message.clone()),
                observed_epoch: None,
            }),
        );
        assert!(release.exit_waiting);
        assert!(release.remains_blocked);
        assert!(!release.terminal);
        assert!(release.machine.is_some());
        {
            let state = coordinator
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get(&work)
                .expect("exit-waiting work must remain registered");
            let reflection = reflection_work(record);
            assert!(matches!(record.state, WorkState::ExitWaiting));
            assert!(reflection.machine.is_none());
            assert!(reflection.block.is_none());
            assert_eq!(
                reflection.exit,
                Some(EvaluationExitBlock {
                    intent: ExitIntent::Error(message),
                    observed_epoch: None,
                })
            );
            assert!(record.obligations.producer.is_some());
            assert!(!state.observation_waiters.contains_key(&work));
            assert!(state.failures.is_empty());
        }
        drop(release.machine.take());

        coordinator.acknowledge_task_failure(session.demand.id, task);
        assert!(coordinator.failure_ledger_snapshot().is_empty());
        assert!(matches!(
            coordinator
                .reflection_snapshots(session.demand.id)
                .as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::ExitWaiting(EvaluationExitBlock {
                    intent: ExitIntent::Error(_),
                    observed_epoch: None,
                }),
                ..
            }]
        ));

        assert_eq!(
            coordinator.request_reflection_cancellation(work),
            ReflectionCancellation::Terminalize
        );
        settle_test_reflection(&coordinator, work);
    }

    #[test]
    fn retryable_exit_wait_requeues_after_runtime_observation() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let observed = coordinator.current_observation_epoch();
        let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);

        let release = coordinator.release_reflection(
            claimed,
            ReflectionWorkPoll::Exit(EvaluationExitBlock {
                intent: ExitIntent::Success,
                observed_epoch: Some(observed),
            }),
        );
        assert!(release.exit_waiting);
        assert!(release.remains_blocked);
        assert!(release.machine.is_none());
        assert!(matches!(
            coordinator.reflection_snapshots(session.demand.id).as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::ExitWaiting(EvaluationExitBlock {
                    intent: ExitIntent::Success,
                    observed_epoch: Some(epoch),
                }),
                ..
            }] if *epoch == observed
        ));

        publish_test_observation(&coordinator);
        assert_eq!(coordinator.ready_task_count(), 1);
        assert!(matches!(
            coordinator
                .reflection_snapshots(session.demand.id)
                .as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Queued,
                ..
            }]
        ));

        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        assert!(
            coordinator
                .release_reflection(claimed, ReflectionWorkPoll::Terminal)
                .terminal
        );
        settle_test_reflection(&coordinator, work);
    }

    #[test]
    fn observation_published_during_registration_is_caught_by_recheck() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let observed = coordinator.observations.current();
        let (_, work) = reserve_ready_test_reflection(&coordinator, &session);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        let mut epoch = coordinator
            .observations
            .epoch
            .lock()
            .expect("runtime observation mutex should not be poisoned");
        let releasing = {
            let coordinator = coordinator.clone();
            thread::spawn(move || {
                coordinator.release_reflection(
                    claimed,
                    ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                        dependency: None,
                        observed_epoch: Some(observed),
                        error: None,
                    }),
                )
            })
        };

        while !coordinator
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .observation_waiters
            .contains_key(&work)
        {
            thread::yield_now();
        }
        epoch.0 = epoch
            .0
            .checked_add(1)
            .expect("test observation epoch should advance");
        drop(epoch);

        let release = releasing
            .join()
            .expect("observation release thread should finish");
        assert!(release.made_progress);
        assert!(!release.remains_blocked);
        assert_eq!(coordinator.ready_task_count(), 1);
        let claimed = claim_ready_test_reflection(&coordinator, session.demand.id);
        assert!(
            coordinator
                .release_reflection(claimed, ReflectionWorkPoll::Terminal)
                .terminal
        );
        settle_test_reflection(&coordinator, work);
    }

    #[test]
    fn coordinator_cancels_reflection_reservations_without_polling() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let task = super::super::allocate_task_id(&session.demand.values)
            .expect("reflection task identity should allocate");
        let wait = super::super::allocate_wait_token(&session.demand, task)
            .expect("reflection wait identity should allocate");
        let work = coordinator
            .reserve_reflection(&session.demand, task, wait)
            .expect("open test session should reserve reflection work");

        assert_eq!(
            coordinator.request_reflection_cancellation(work),
            ReflectionCancellation::Terminalize
        );
        assert!(matches!(
            coordinator
                .reflection_snapshots(session.demand.id)
                .as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::Terminalizing,
                ..
            }]
        ));
        settle_test_reflection(&coordinator, work);
        assert_eq!(
            coordinator.request_reflection_cancellation(work),
            ReflectionCancellation::Late
        );
    }

    #[test]
    fn coordinator_fairness_alternates_ready_tasks_and_sparks() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        coordinator.executor_started(1);
        coordinator.submit_spark(session.demand.clone(), crate::core::keys::unit_value());
        let task = super::super::allocate_task_id(&session.demand.values)
            .expect("reflection task identity should allocate");
        let wait = super::super::allocate_wait_token(&session.demand, task)
            .expect("reflection wait identity should allocate");
        let work = coordinator
            .reserve_reflection(&session.demand, task, wait)
            .expect("open test session should reserve reflection work");
        activate_test_reflection(&coordinator, work);

        let CoordinatorSelection::Task(claimed) = coordinator.select() else {
            panic!("task work should receive the first turn")
        };
        coordinator.requeue_unpolled_task(claimed);

        let CoordinatorSelection::Spark(spark) = coordinator.select() else {
            panic!("spark should receive the alternating turn")
        };
        coordinator.release_spark(spark, SparkWorkPoll::Complete);
        let CoordinatorSelection::Task(claimed) = coordinator.select() else {
            panic!("task work should receive the next alternating turn")
        };
        coordinator.requeue_unpolled_task(claimed);
        assert!(coordinator.terminalize_reflection(work));
        settle_test_reflection(&coordinator, work);
    }

    #[test]
    fn queued_sparks_are_abandoned_when_their_demand_session_closes() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        coordinator.executor_started(1);
        coordinator.submit_spark(session.demand.clone(), crate::core::keys::unit_value());
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

        drop(session);

        assert_eq!(coordinator.spark_work_counts(), (0, 0, 0));
        assert_eq!(coordinator.retained_spark_count(), 0);
    }

    #[test]
    fn coordinator_owns_dormant_deferred_promotion_and_release() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let task = super::super::allocate_task_id(&session.demand.values)
            .expect("deferred task identity should allocate");
        let wait = super::super::allocate_wait_token(&session.demand, task)
            .expect("deferred wait identity should allocate");
        let lazy = LazyValue::deferred(
            &session.demand.values,
            "coordinator deferred lifecycle",
            |_| panic!("coordinator lifecycle test never evaluates its synthetic lazy"),
        );
        let DeferredWorkReservation::New(work) = coordinator
            .reserve_deferred(
                &session.demand,
                task,
                wait.clone(),
                DeferredProducer::Lazy(lazy),
                Box::new(TestTaskMachine),
            )
            .expect("open test session should reserve deferred work")
        else {
            panic!("fresh deferred work should reserve a canonical record")
        };

        assert!(
            coordinator
                .claim_ready_task_for_session(session.demand.id)
                .is_none()
        );
        assert!(coordinator.promote_deferred_wait(&wait));
        assert!(coordinator.activate_deferred(work));
        let ClaimedTaskWork::Deferred(claimed) = coordinator
            .claim_ready_task_for_session(session.demand.id)
            .expect("demand observed during installation should queue the producer")
        else {
            panic!("queued deferred work should preserve its kind")
        };
        let dependency_task = super::super::allocate_task_id(&session.demand.values)
            .expect("dependency task identity should allocate");
        let dependency = super::super::allocate_wait_token(&session.demand, dependency_task)
            .expect("dependency wait identity should allocate");
        dependency.publish_terminal(super::super::EvaluationWaitTerminal::Complete(
            RuntimeValueRoot::new(&session.demand.values, crate::core::keys::unit_value()),
        ));
        let release = coordinator.release_deferred(
            claimed,
            DeferredWorkPoll::Blocked(EvaluationTaskBlock {
                dependency: Some(WorkDependency::Wait(dependency)),
                observed_epoch: None,
                error: None,
            }),
        );
        assert!(!release.remains_blocked);
        assert!(!release.terminal);

        let ClaimedTaskWork::Deferred(claimed) = coordinator
            .claim_ready_task_for_session(session.demand.id)
            .expect("a terminal dependency should immediately requeue the producer")
        else {
            panic!("the requeued producer should preserve its deferred kind")
        };
        let release = coordinator.release_deferred(claimed, DeferredWorkPoll::Yielded);
        assert!(release.made_progress);
        assert!(!release.remains_blocked);
        let ClaimedTaskWork::Deferred(claimed) = coordinator
            .claim_ready_task_for_session(session.demand.id)
            .expect("a yielded queued demand should remain ready")
        else {
            panic!("the yielded producer should preserve its deferred kind")
        };
        let release = coordinator.release_deferred(claimed, DeferredWorkPoll::Terminal);
        assert!(release.terminal);
        settle_test_deferred(&coordinator, work);
    }

    #[test]
    fn deferred_claim_excludes_competitors_and_releases_its_machine_outside_runtime_locks() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let task = super::super::allocate_task_id(&session.demand.values)
            .expect("deferred task identity should allocate");
        let wait = super::super::allocate_wait_token(&session.demand, task)
            .expect("deferred wait identity should allocate");
        let lazy = LazyValue::deferred(
            &session.demand.values,
            "coordinator machine ownership",
            |_| panic!("coordinator ownership test never evaluates its synthetic lazy"),
        );
        let dropped_without_runtime_locks = Arc::new(AtomicBool::new(false));
        let DeferredWorkReservation::New(work) = coordinator
            .reserve_deferred(
                &session.demand,
                task,
                wait.clone(),
                DeferredProducer::Lazy(lazy),
                Box::new(CheckDeferredDropLocks {
                    coordinator: Arc::downgrade(&coordinator),
                    dropped_without_runtime_locks: dropped_without_runtime_locks.clone(),
                }),
            )
            .expect("open test session should reserve deferred work")
        else {
            panic!("fresh deferred work should reserve a canonical record")
        };
        assert!(coordinator.activate_deferred(work));
        assert!(coordinator.promote_deferred_wait(&wait));
        let ClaimedTaskWork::Deferred(claimed) = coordinator
            .claim_ready_task_for_session(session.demand.id)
            .expect("promoted deferred work should be claimable")
        else {
            panic!("claimed work should preserve its deferred kind")
        };
        assert!(
            coordinator
                .claim_ready_task_for_session(session.demand.id)
                .is_none(),
            "a detached deferred machine must exclude a competing claim"
        );

        let mut release = coordinator.release_deferred(claimed, DeferredWorkPoll::Terminal);
        assert!(release.terminal);
        coordinator.settle_terminal_work(
            work,
            EvaluationWaitTerminal::Abandoned,
            Arc::new(EvaluationFailure::message("test deferred settlement")),
        );
        let machine = release
            .machine
            .take()
            .expect("terminal deferred release must return its machine");
        drop(machine);
        assert!(dropped_without_runtime_locks.load(Ordering::Acquire));
        coordinator.retire_deferred(work);
    }

    #[test]
    fn outer_block_promotes_one_canonical_deferred_producer() {
        let (coordinator, _executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let producer_session = TestDemand::new(&coordinator);
        let observer_session = TestDemand::new(&coordinator);
        let producer_task = super::super::allocate_task_id(&producer_session.demand.values)
            .expect("producer task identity should allocate");
        let producer_wait =
            super::super::allocate_wait_token(&producer_session.demand, producer_task)
                .expect("producer wait identity should allocate");
        let lazy = LazyValue::deferred(
            &producer_session.demand.values,
            "cross-session canonical producer",
            |_| panic!("coordinator promotion test does not evaluate its lazy"),
        );
        let DeferredWorkReservation::New(producer_work) = coordinator
            .reserve_deferred(
                &producer_session.demand,
                producer_task,
                producer_wait.clone(),
                DeferredProducer::Lazy(lazy.clone()),
                Box::new(TestTaskMachine),
            )
            .expect("open producer session should reserve deferred work")
        else {
            panic!("first demand should reserve the canonical producer")
        };
        assert!(coordinator.activate_deferred(producer_work));

        let duplicate_task = super::super::allocate_task_id(&observer_session.demand.values)
            .expect("duplicate task identity should allocate");
        let duplicate_wait =
            super::super::allocate_wait_token(&observer_session.demand, duplicate_task)
                .expect("duplicate wait identity should allocate");
        let DeferredWorkReservation::Existing(canonical_wait) = coordinator
            .reserve_deferred(
                &observer_session.demand,
                duplicate_task,
                duplicate_wait,
                DeferredProducer::Lazy(lazy),
                Box::new(TestTaskMachine),
            )
            .expect("open observer session should reuse deferred work")
        else {
            panic!("a racing demand must reuse the canonical producer")
        };
        assert_eq!(canonical_wait, producer_wait);

        let observer_task = super::super::allocate_task_id(&observer_session.demand.values)
            .expect("observer task identity should allocate");
        let observer_wait =
            super::super::allocate_wait_token(&observer_session.demand, observer_task)
                .expect("observer wait identity should allocate");
        let observer_work = coordinator
            .reserve_reflection(&observer_session.demand, observer_task, observer_wait)
            .expect("open observer session should reserve reflection work");
        activate_test_reflection(&coordinator, observer_work);
        let ClaimedTaskWork::Reflection(claimed) = coordinator
            .claim_ready_task_for_session(observer_session.demand.id)
            .expect("observer reflection work should be ready")
        else {
            panic!("observer work should preserve its reflection kind")
        };
        let release = coordinator.release_reflection(
            claimed,
            ReflectionWorkPoll::Blocked(EvaluationTaskBlock {
                dependency: Some(WorkDependency::Wait(producer_wait)),
                observed_epoch: None,
                error: None,
            }),
        );
        assert!(release.remains_blocked);
        let ClaimedTaskWork::Deferred(producer) = coordinator
            .claim_ready_task_for_session(producer_session.demand.id)
            .expect("publishing the outer dependency should promote its dormant producer")
        else {
            panic!("promoted producer should preserve its deferred kind")
        };
        let release = coordinator.release_deferred(producer, DeferredWorkPoll::Terminal);
        assert!(release.terminal);
        settle_test_deferred(&coordinator, producer_work);
        assert!(coordinator.terminalize_reflection(observer_work));
        settle_test_reflection(&coordinator, observer_work);
    }

    #[test]
    fn dropping_executor_does_not_discard_coordinator_session_state() {
        let (coordinator, executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = TestDemand::new(&coordinator);
        let task = super::super::allocate_task_id(&session.demand.values)
            .expect("reflection task identity should allocate");
        let wait = super::super::allocate_wait_token(&session.demand, task)
            .expect("reflection wait identity should allocate");
        let work = coordinator
            .reserve_reflection(&session.demand, task, wait)
            .expect("open test session should reserve reflection work");
        activate_test_reflection(&coordinator, work);
        drop(executor);

        let CoordinatorSelection::Task(claimed) = coordinator.select() else {
            panic!("dropping the executor must preserve ready task work")
        };
        coordinator.requeue_unpolled_task(claimed);
        assert_eq!(coordinator.registered_session_count(), 1);
        assert!(coordinator.terminalize_reflection(work));
        settle_test_reflection(&coordinator, work);
    }
}
