//! Session-scoped capabilities threaded through semantic evaluation.
//!
//! The runtime supplies task and wait identity, value provenance, and the
//! authoritative reflection-task lifecycle. During the work-boundary
//! transition, the runtime coordinator owns opaque reflection and deferred
//! machines directly with their lifecycle records and the runtime-owned task
//! failure ledger. Demand state retains its serial cooperative pump, while a
//! weak coordinator registration supplies guarded admission without another
//! session-owned task registry. Reflection
//! specializations remain outside this module behind a small type-erased
//! task-machine boundary.

use std::collections::HashSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use rpds::RedBlackTreeMapSync;

use crate::core::{
    CoreValueFactory, EvaluationFailure, LazyCycle, LazyCycleMember, LazyValue, PromiseAssignment,
    PromiseCell, PromiseId, PromisedValue, Value,
};
use crate::core_net::CoreWaitToken;
use crate::runtime::{EvaluationRuntimeId, RuntimeMutationAuthority, RuntimeValueRoot};

mod coordinator;
mod executor;
use coordinator::{
    ClaimedDeferredWork, ClaimedReflectionWork, ClaimedTaskWork, ClientDemandSnapshot,
    DeferredLazyCycleMember, DeferredProducer, DeferredWorkPoll, DeferredWorkReservation,
    EvaluationWorkId, ReflectionCancellation, ReflectionWorkPoll, ReflectionWorkState,
};
pub(crate) use coordinator::{
    CompletionSubscriptionOutcome, CompletionSubscriptions, CompletionWake, EvaluationTaskBlock,
    EvaluationWorkCoordinator, RuntimeCoordinatorReadiness, RuntimeDeadlockWorkSnapshot,
    RuntimeDependencySnapshot, RuntimeExitSnapshot, RuntimeObservationEpoch,
    RuntimeObservationState, RuntimeWorkKindSnapshot, RuntimeWorkStateSnapshot,
    ValidatedRuntimeSettlementPlan, WakeRegistration, WorkDependency,
};
#[cfg(test)]
use coordinator::{ReflectionWorkSnapshot, test_wake_registration};
pub(crate) use executor::EvaluationExecutor;

#[cfg(test)]
pub(crate) fn test_execution_resources(
    worker_count: usize,
) -> Result<(Arc<EvaluationWorkCoordinator>, Arc<EvaluationExecutor>), Arc<str>> {
    let values = CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let admission = crate::runtime::RuntimeMutationAdmission::new();
    let coordinator = EvaluationWorkCoordinator::new_for_test(values, admission);
    let executor = EvaluationExecutor::new(worker_count, &coordinator)?;
    Ok((coordinator, executor))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct EvaluationTaskId(NonZeroU64);

impl EvaluationTaskId {
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) fn from_u64(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct EvaluationSessionId(NonZeroU64);

impl EvaluationSessionId {
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

fn allocate_task_id(values: &CoreValueFactory) -> Result<EvaluationTaskId, Arc<str>> {
    values.ids().evaluation_task().map(EvaluationTaskId)
}

fn allocate_wait_token(
    session: &Arc<EvaluationDemandState>,
    producer: EvaluationTaskId,
) -> Result<EvaluationWaitToken, Arc<str>> {
    let id = session.values.ids().evaluation_wait()?;
    Ok(EvaluationWaitToken(Arc::new(EvaluationWaitState {
        id,
        runtime: session.values.runtime_id(),
        owner_id: session.id,
        producer,
        terminal: OnceLock::new(),
        completion: CompletionSubscriptions::for_wait(
            session.values.runtime_id(),
            id,
            session.values.work_coordinator_binding(),
        ),
    })))
}

fn evaluation_failure(message: impl AsRef<str>) -> Arc<EvaluationFailure> {
    Arc::new(EvaluationFailure::message(message))
}

struct EvaluationWaitState {
    id: NonZeroU64,
    runtime: EvaluationRuntimeId,
    owner_id: EvaluationSessionId,
    producer: EvaluationTaskId,
    terminal: OnceLock<EvaluationWaitTerminal>,
    completion: CompletionSubscriptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvaluationWaitTerminal {
    Complete(RuntimeValueRoot),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
    Abandoned,
    Exited,
    Killed(Arc<EvaluationFailure>),
}

#[derive(Clone)]
pub(crate) struct EvaluationWaitToken(Arc<EvaluationWaitState>);

impl EvaluationWaitToken {
    pub(crate) fn get(&self) -> u64 {
        self.0.id.get()
    }

    pub(crate) fn owner_id(&self) -> EvaluationSessionId {
        self.0.owner_id
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.0.runtime
    }

    pub(crate) fn producer(&self) -> EvaluationTaskId {
        self.0.producer
    }

    fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        self.0.completion.coordinator()
    }

    fn belongs_to(&self, session: &Arc<EvaluationDemandState>) -> bool {
        self.runtime_id() == session.values.runtime_id() && self.owner_id() == session.id
    }

    fn terminal_poll(&self) -> Option<EvaluationWaitPoll> {
        self.0.terminal.get().map(EvaluationWaitTerminal::to_poll)
    }

    fn publish_terminal(&self, terminal: EvaluationWaitTerminal) -> EvaluationWaitTerminal {
        if let EvaluationWaitTerminal::Complete(value) = &terminal {
            debug_assert_eq!(value.runtime_id(), self.runtime_id());
        }
        if let Err(candidate) = self.0.terminal.set(terminal) {
            debug_assert_eq!(
                self.0.terminal.get(),
                Some(&candidate),
                "a wait token received conflicting terminal results"
            );
        }
        self.0
            .terminal
            .get()
            .expect("terminal publication must initialize the wait cell")
            .clone()
    }

    fn publish_terminal_guarded(
        &self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        terminal: EvaluationWaitTerminal,
    ) -> (EvaluationWaitTerminal, coordinator::CompletionWake) {
        self.0
            .completion
            .publish_guarded(coordinator, mutation, || {
                Ok::<_, std::convert::Infallible>(self.publish_terminal(terminal))
            })
            .expect("wait terminal publication is infallible")
    }

    /// Delivers exact completion registrations after the owner registry lock
    /// which published and retired this wait has been released.
    fn notify_terminal(&self) {
        debug_assert!(self.0.terminal.get().is_some());
        self.0.completion.notify_published();
    }

    /// Abandons the transient deferred-producer chain retained solely for a
    /// best-effort spark dependency.
    ///
    /// The wait cell routes through the runtime coordinator rather than its
    /// originating demand state. Owner identity remains scalar provenance;
    /// dropping a task handle or wait cannot retain or recover the owner
    /// session lease.
    fn abandon_deferred_producer(&self) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        let owner = self.owner_id();
        let mut wait = self.clone();
        loop {
            if wait.owner_id() != owner || wait.terminal_poll().is_some() {
                return;
            }
            let Some(abandoned) = coordinator.abandon_deferred_wait(&wait) else {
                return;
            };
            let terminal = coordinator.settle_terminal_work(
                abandoned.id,
                EvaluationWaitTerminal::Abandoned,
                evaluation_failure("deferred fixpoint producer was abandoned"),
            );
            debug_assert_eq!(wait.terminal_poll(), Some(terminal.to_poll()));
            coordinator.retire_deferred(abandoned.id);
            drop(abandoned.machine);
            let Some(dependency) = abandoned.dependency else {
                return;
            };
            wait = dependency;
        }
    }

    pub(crate) fn subscribe_work(
        &self,
        runtime: EvaluationRuntimeId,
        registration: WakeRegistration,
    ) -> CompletionSubscriptionOutcome {
        self.0
            .completion
            .subscribe(runtime, registration, || self.0.terminal.get().is_some())
    }

    pub(crate) fn unsubscribe_work(&self, registration: WakeRegistration) -> bool {
        self.0.completion.unsubscribe(registration)
    }

    #[cfg(test)]
    pub(crate) fn exact_subscription_count(&self) -> usize {
        self.0.completion.len()
    }

    #[cfg(test)]
    fn subscribe_test_work(&self) -> CompletionSubscriptionOutcome {
        self.subscribe_work(self.runtime_id(), test_wake_registration())
    }
}

/// Assignment-side access to one task-owned promise obligation.
///
/// Scheduled tasks place the reciprocal weak promise cell in their
/// coordinator work record. A task-owned promise is assignable only
/// synchronously by its owning machine while that work is `Running`, and no
/// other thread may terminalize running work. Consequently assignment removes
/// this obligation before the owning poll can release the work for terminal
/// settlement. Resolver-owned promises are a distinct ownership model and do
/// not acquire a task obligation.
///
/// Directly driven effect tasks temporarily use a task-local inventory until
/// client demand becomes coordinator work in Phase 10B. Neither form retains
/// a producer session or runtime coordinator.
pub(crate) struct PromiseProducerObligation {
    owner: EvaluationTaskId,
    wait: EvaluationWaitToken,
    source: PromiseProducerSource,
}

enum PromiseProducerSource {
    Coordinator {
        work: EvaluationWorkId,
        promise: PromiseId,
        coordinator: Weak<EvaluationWorkCoordinator>,
    },
    Local {
        promise: PromiseId,
        owner: Weak<LocalPromiseOwner>,
    },
}

#[derive(Debug, Clone)]
struct LocalPromiseObligation {
    promise: PromiseId,
    cell: Weak<PromiseCell>,
    wait: EvaluationWaitToken,
}

#[derive(Debug, Default)]
struct LocalPromiseOwner {
    obligations: Mutex<Vec<LocalPromiseObligation>>,
}

impl LocalPromiseOwner {
    fn register(&self, promise: &Arc<PromiseCell>, wait: EvaluationWaitToken) {
        self.obligations
            .lock()
            .expect("local promise obligations were poisoned")
            .push(LocalPromiseObligation {
                promise: promise.id(),
                cell: Arc::downgrade(promise),
                wait,
            });
    }

    fn contains_wait(&self, wait: &EvaluationWaitToken) -> bool {
        self.obligations
            .lock()
            .expect("local promise obligations were poisoned")
            .iter()
            .any(|obligation| obligation.wait == *wait)
    }

    fn complete(&self, promise: PromiseId, wait: &EvaluationWaitToken) {
        let mut obligations = self
            .obligations
            .lock()
            .expect("local promise obligations were poisoned");
        if let Some(index) = obligations
            .iter()
            .position(|obligation| obligation.promise == promise && obligation.wait == *wait)
        {
            obligations.swap_remove(index);
        }
    }

    fn fail_all(&self, failure: Arc<EvaluationFailure>) {
        let obligations = self
            .obligations
            .lock()
            .expect("local promise obligations were poisoned")
            .clone();
        for obligation in obligations {
            if let Some(cell) = obligation.cell.upgrade() {
                let _ = cell.fail(failure.clone());
            } else {
                self.complete(obligation.promise, &obligation.wait);
                obligation
                    .wait
                    .publish_terminal(EvaluationWaitTerminal::Failed(failure.clone()));
                obligation.wait.notify_terminal();
            }
        }
    }
}

pub(crate) enum PromiseProducerPublication {
    Guarded(coordinator::CompletionWake),
    Detached(EvaluationWaitToken),
}

impl PromiseProducerPublication {
    pub(crate) fn notify(self) {
        match self {
            Self::Guarded(wake) => wake.notify(),
            Self::Detached(wait) => wait.notify_terminal(),
        }
    }
}

impl PromiseProducerObligation {
    pub(crate) fn owner(&self) -> EvaluationTaskId {
        self.owner
    }

    pub(crate) fn wait(&self) -> &EvaluationWaitToken {
        &self.wait
    }

    pub(crate) fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        match &self.source {
            PromiseProducerSource::Coordinator { coordinator, .. } => coordinator.upgrade(),
            PromiseProducerSource::Local { .. } => None,
        }
    }

    pub(crate) fn publish_assignment_guarded(
        &self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        assignment: &PromiseAssignment,
    ) -> PromiseProducerPublication {
        debug_assert_eq!(coordinator.runtime_id(), self.wait.runtime_id());
        let PromiseProducerSource::Coordinator { work, promise, .. } = self.source else {
            panic!("a task-local promise cannot publish through a coordinator guard");
        };
        coordinator.complete_task_promise_guarded(mutation, work, &self.wait, promise);
        let terminal = promise_assignment_terminal(self.wait.runtime_id(), assignment);
        let (_, wake) = self
            .wait
            .publish_terminal_guarded(coordinator, mutation, terminal);
        PromiseProducerPublication::Guarded(wake)
    }

    pub(crate) fn publish_assignment_detached(
        &self,
        assignment: &PromiseAssignment,
    ) -> PromiseProducerPublication {
        if let PromiseProducerSource::Local { promise, owner } = &self.source
            && let Some(owner) = owner.upgrade()
        {
            owner.complete(*promise, &self.wait);
        }
        let terminal = promise_assignment_terminal(self.wait.runtime_id(), assignment);
        self.wait.publish_terminal(terminal);
        PromiseProducerPublication::Detached(self.wait.clone())
    }
}

impl EvaluationWaitTerminal {
    fn to_poll(&self) -> EvaluationWaitPoll {
        match self {
            Self::Complete(value) => EvaluationWaitPoll::Complete(value.as_core().clone()),
            Self::Failed(error) => EvaluationWaitPoll::Failed(error.clone()),
            Self::Cancelled => EvaluationWaitPoll::Cancelled,
            Self::Abandoned => EvaluationWaitPoll::Abandoned,
            Self::Exited => EvaluationWaitPoll::Exited,
            Self::Killed(error) => EvaluationWaitPoll::Killed(error.clone()),
        }
    }
}

impl fmt::Debug for EvaluationWaitToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationWaitToken")
            .field("wait", &self.0.id)
            .field("session", &self.0.owner_id)
            .field("producer", &self.0.producer)
            .field("terminal", &self.0.terminal.get().is_some())
            .finish_non_exhaustive()
    }
}

impl PartialEq for EvaluationWaitToken {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}

impl Eq for EvaluationWaitToken {}

impl Hash for EvaluationWaitToken {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.id.hash(state);
    }
}

#[derive(Clone)]
pub(crate) struct EvaluationTaskHandle {
    id: EvaluationTaskId,
    work: EvaluationWorkId,
    owner_session: EvaluationSessionId,
    coordinator: Weak<EvaluationWorkCoordinator>,
    wait: EvaluationWaitToken,
}

impl EvaluationTaskHandle {
    fn new(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        owner_session: EvaluationSessionId,
        id: EvaluationTaskId,
        work: EvaluationWorkId,
        wait: EvaluationWaitToken,
    ) -> Self {
        debug_assert_eq!(coordinator.runtime_id(), wait.runtime_id());
        debug_assert_eq!(owner_session, wait.owner_id());
        debug_assert_eq!(id, wait.producer());
        Self {
            id,
            work,
            owner_session,
            coordinator: Arc::downgrade(coordinator),
            wait,
        }
    }

    pub(crate) fn id(&self) -> EvaluationTaskId {
        self.id
    }

    pub(crate) fn session_id(&self) -> EvaluationSessionId {
        self.owner_session
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.wait.runtime_id()
    }

    #[cfg(test)]
    pub(crate) fn wait(&self) -> &EvaluationWaitToken {
        &self.wait
    }

    /// Transfers reporting responsibility for a propagated terminal failure
    /// from the task ledger to the consumer of this handle.
    pub(crate) fn acknowledge_propagated_failure(&self) {
        self.acknowledge_failure();
    }

    /// Acknowledges any present or future failure in this task's immutable
    /// producer-owner ledger bucket.
    pub(crate) fn acknowledge_failure(&self) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            debug_assert_eq!(coordinator.runtime_id(), self.runtime_id());
            coordinator.acknowledge_task_failure(self.owner_session, self.id);
        }
    }

    /// Requests cancellation through this task's runtime work record.
    pub(crate) fn cancel(&self) -> EvaluationTaskCancellation {
        if self.wait.terminal_poll().is_some() {
            return EvaluationTaskCancellation::Late;
        }
        let Some(coordinator) = self.coordinator.upgrade() else {
            return EvaluationTaskCancellation::Late;
        };
        debug_assert_eq!(coordinator.runtime_id(), self.runtime_id());
        match coordinator.request_reflection_cancellation(self.work) {
            ReflectionCancellation::Requested => EvaluationTaskCancellation::Requested,
            ReflectionCancellation::Late => EvaluationTaskCancellation::Late,
            ReflectionCancellation::Terminalize => {
                settle_task_work(
                    &coordinator,
                    self.work,
                    EvaluationTaskState::Cancelled,
                    evaluation_failure("reflection fixpoint producer was cancelled"),
                );
                let mut machine = coordinator.retire_reflection(self.work);
                if let Some(machine) = &mut machine {
                    machine.cancel();
                }
                drop(machine);
                EvaluationTaskCancellation::Requested
            }
        }
    }
}

/// A fully constructed reflection task retained in the coordinator's
/// `Reserved` state. Host integration may publish another runtime fact under
/// the same admission authority before making this task runnable.
pub(crate) struct PreparedEvaluationTask {
    coordinator: Arc<EvaluationWorkCoordinator>,
    handle: EvaluationTaskHandle,
}

impl PreparedEvaluationTask {
    pub(crate) fn activate(self) -> EvaluationTaskHandle {
        assert!(
            self.coordinator.activate_reflection(self.handle.work),
            "fresh reflection reservation must activate"
        );
        self.handle
    }

    pub(crate) fn activate_guarded(&self, mutation: &dyn RuntimeMutationAuthority) -> bool {
        self.coordinator
            .activate_reflection_guarded(self.handle.work, mutation)
    }

    pub(crate) fn finish_guarded_activation(&self, activated: bool) {
        self.coordinator.notify_reflection_activation(activated);
    }

    pub(crate) fn into_handle(self) -> EvaluationTaskHandle {
        self.handle
    }
}

impl fmt::Debug for EvaluationTaskHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationTaskHandle")
            .field("task", &self.id.get())
            .field("work", &self.work.get())
            .field("runtime", &self.runtime_id())
            .field("session", &self.session_id().get())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationWaitPoll {
    Pending(EvaluationWaitToken),
    Complete(Value),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
    Abandoned,
    Exited,
    Killed(Arc<EvaluationFailure>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationTaskCancellation {
    Requested,
    Late,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum InitialTaskDisposition {
    #[default]
    Launch,
    Cancel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PendingTaskPolicy {
    disposition: InitialTaskDisposition,
    acknowledge_error: bool,
}

impl PendingTaskPolicy {
    pub(crate) fn cancel(&mut self) {
        self.disposition = InitialTaskDisposition::Cancel;
    }

    pub(crate) fn acknowledge_error(&mut self) {
        self.acknowledge_error = true;
    }
}

/// A coordinator-facing request to terminalize one effect machine when the
/// runtime client accepts a stable readiness snapshot.
///
/// Exit is deliberately distinct from ordinary completion or failure. Until
/// settlement, it produces neither a task result nor a failure-ledger entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExitIntent {
    Success,
    Error(RuntimeValueRoot),
}

/// The specialization-independent portion of one preterminal exit vote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationExitBlock {
    pub(crate) intent: ExitIntent,
    pub(crate) observed_epoch: Option<RuntimeObservationEpoch>,
}

pub(crate) enum EvaluationMachinePoll {
    Yielded,
    Blocked(EvaluationTaskBlock),
    Exit(EvaluationExitBlock),
    Complete(Value),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
}

pub(crate) trait EvaluationTaskMachine: Send {
    fn poll(&mut self, step_budget: usize) -> EvaluationMachinePoll;

    fn cancel(&mut self) {}
}

/// One sealed pure operation retained by runtime-owned client demand.
///
/// Client demand owns only evaluation of a semantic value to WHNF. Host
/// operations construct accessors, annotations, and other computations as
/// ordinary values before admitting them here.
#[derive(Debug)]
pub(crate) struct ClientDemandOperation(RuntimeValueRoot);

impl ClientDemandOperation {
    fn runtime_id(&self) -> EvaluationRuntimeId {
        self.0.runtime_id()
    }

    fn poll(&mut self, context: &EvalContext) -> coordinator::ClientDemandPoll {
        match crate::eval::eval_value(context, self.0.as_core()) {
            Ok(value) => coordinator::ClientDemandPoll::Complete(RuntimeValueRoot::new(
                context.values(),
                value,
            )),
            Err(halt) => client_demand_halt_poll(halt),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientDemandResult {
    Complete(RuntimeValueRoot),
    Failed(Arc<EvaluationFailure>),
    Abandoned,
    Killed(Arc<EvaluationFailure>),
}

struct ClientDemandResultCell {
    result: Mutex<Option<ClientDemandResult>>,
    changed: Condvar,
    #[cfg(test)]
    publish_probe: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ClientDemandResultCell {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(None),
            changed: Condvar::new(),
            #[cfg(test)]
            publish_probe: Mutex::new(None),
        })
    }

    fn publish(&self, result: ClientDemandResult) -> bool {
        let mut current = self
            .result
            .lock()
            .expect("client demand result cell was poisoned");
        if current.is_some() {
            return false;
        }
        *current = Some(result);
        drop(current);
        #[cfg(test)]
        if let Some(probe) = self
            .publish_probe
            .lock()
            .expect("client demand publish probe was poisoned")
            .take()
        {
            probe();
        }
        self.changed.notify_all();
        true
    }

    #[cfg(test)]
    fn set_publish_probe(&self, probe: impl FnOnce() + Send + 'static) {
        *self
            .publish_probe
            .lock()
            .expect("client demand publish probe was poisoned") = Some(Box::new(probe));
    }

    fn poll(&self) -> Option<ClientDemandResult> {
        self.result
            .lock()
            .expect("client demand result cell was poisoned")
            .clone()
    }

    fn wait(&self) -> ClientDemandResult {
        let mut result = self
            .result
            .lock()
            .expect("client demand result cell was poisoned");
        loop {
            if let Some(result) = result.clone() {
                return result;
            }
            result = self
                .changed
                .wait(result)
                .expect("client demand result cell was poisoned");
        }
    }
}

#[derive(Clone)]
pub(super) struct ClientDemandSink {
    cell: Arc<ClientDemandResultCell>,
}

impl ClientDemandSink {
    fn new(cell: Arc<ClientDemandResultCell>) -> Self {
        Self { cell }
    }

    pub(super) fn publish(&self, result: ClientDemandResult) -> bool {
        self.cell.publish(result)
    }
}

/// Rust-side ownership of one asynchronous pure evaluator demand.
///
/// Dropping or explicitly abandoning this handle retires only its consumer
/// registration. The independently owned lazy/promise producer remains
/// available to other consumers.
pub(crate) struct ClientDemandHandle {
    runtime: EvaluationRuntimeId,
    work: EvaluationWorkId,
    coordinator: Weak<EvaluationWorkCoordinator>,
    cell: Arc<ClientDemandResultCell>,
    active: bool,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "retained async handle controls remain internal until a public runtime-client API is selected"
    )
)]
impl ClientDemandHandle {
    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub(crate) fn poll(&self) -> Option<ClientDemandResult> {
        self.cell.poll()
    }

    fn wait(&self) -> ClientDemandResult {
        self.cell.wait()
    }

    pub(crate) fn abandon(mut self) {
        self.abandon_inner();
    }

    fn abandon_inner(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let retired = self
            .coordinator
            .upgrade()
            .is_some_and(|coordinator| coordinator.abandon_client_demand(self.work));
        if !retired {
            let _ = self.cell.publish(ClientDemandResult::Abandoned);
        }
    }

    fn abandon_if_stably_blocked(&mut self, subscription_epoch: u64) -> Option<WorkDependency> {
        if !self.active {
            return None;
        }
        let dependency = self
            .coordinator
            .upgrade()?
            .abandon_blocked_client_demand(self.work, subscription_epoch)?;
        self.active = false;
        Some(dependency)
    }

    #[cfg(test)]
    fn result_cell(&self) -> Weak<ClientDemandResultCell> {
        Arc::downgrade(&self.cell)
    }

    #[cfg(test)]
    fn work(&self) -> EvaluationWorkId {
        self.work
    }

    #[cfg(test)]
    fn set_publish_probe(&self, probe: impl FnOnce() + Send + 'static) {
        self.cell.set_publish_probe(probe);
    }
}

impl Drop for ClientDemandHandle {
    fn drop(&mut self) {
        self.abandon_inner();
    }
}

#[cfg(test)]
struct PendingTestPromiseTask;

#[cfg(test)]
impl EvaluationTaskMachine for PendingTestPromiseTask {
    fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
        EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
            dependency: None,
            observed_epoch: Some(RuntimeObservationEpoch::from_raw(7)),
            error: None,
        })
    }
}

pub(crate) trait ReflectionTaskLauncher: Send + Sync {
    fn build(
        &self,
        context: EvalContext,
        effect: Value,
        result_policy: ReflectionTaskResultPolicy,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>>;
}

/// One immutable, type-erased reflection task host profile.
///
/// The launcher closes over the profile's effect specialization, environment,
/// diagnostic destination, and shared host resources. Runtime-default and
/// current-task profiles use the same representation but have different
/// selection rules.
pub(crate) struct ReflectionTaskProfile {
    launcher: OnceLock<Arc<dyn ReflectionTaskLauncher>>,
}

impl fmt::Debug for ReflectionTaskProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReflectionTaskProfile")
            .field("sealed", &self.launcher.get().is_some())
            .finish()
    }
}

impl ReflectionTaskProfile {
    pub(crate) fn unsealed() -> Self {
        Self {
            launcher: OnceLock::new(),
        }
    }

    pub(crate) fn sealed(launcher: Arc<dyn ReflectionTaskLauncher>) -> Self {
        let profile = Self::unsealed();
        profile
            .seal(launcher)
            .expect("a fresh reflection task profile must be unsealed");
        profile
    }

    pub(crate) fn seal(&self, launcher: Arc<dyn ReflectionTaskLauncher>) -> Result<(), Arc<str>> {
        self.launcher
            .set(launcher)
            .map_err(|_| Arc::from("reflection task profile is already sealed"))
    }

    fn launcher(&self) -> Option<&Arc<dyn ReflectionTaskLauncher>> {
        self.launcher.get()
    }

    pub(crate) fn is_sealed(&self) -> bool {
        self.launcher.get().is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationTaskStatus {
    Launched,
    Blocked,
    Complete(RuntimeValueRoot),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
    Abandoned,
    Exited,
    Killed(Arc<EvaluationFailure>),
}

type GuardedTaskStatusPublication =
    dyn Fn(&dyn RuntimeMutationAuthority, EvaluationTaskStatus) -> TaskStatusWake + Send + Sync;

#[derive(Clone)]
pub(crate) struct TaskStatusPublisher {
    publish: Arc<GuardedTaskStatusPublication>,
}

impl TaskStatusPublisher {
    pub(crate) fn new(
        publish: impl Fn(&dyn RuntimeMutationAuthority, EvaluationTaskStatus) -> TaskStatusWake
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            publish: Arc::new(publish),
        }
    }

    pub(crate) fn publish_guarded(
        &self,
        mutation: &dyn RuntimeMutationAuthority,
        status: EvaluationTaskStatus,
    ) -> TaskStatusWake {
        (self.publish)(mutation, status)
    }
}

pub(crate) struct TaskStatusWake {
    notify: Option<Box<dyn FnOnce() + Send>>,
}

impl TaskStatusWake {
    pub(crate) fn new(notify: impl FnOnce() + Send + 'static) -> Self {
        Self {
            notify: Some(Box::new(notify)),
        }
    }

    pub(crate) fn notify(mut self) {
        if let Some(notify) = self.notify.take() {
            notify();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReflectionTaskResultPolicy {
    RequireUnit,
    ReturnValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationPumpOutcome {
    TargetReady,
    /// The target has a producer currently claimed by another thread.
    Busy,
    NoProgress,
    BudgetExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationSessionRun {
    Complete(EvaluationSessionReport),
    Quiescent(EvaluationSessionReport),
    Deadlocked(EvaluationSessionReport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationSessionReport {
    pub(crate) failures: RedBlackTreeMapSync<EvaluationTaskId, Arc<EvaluationFailure>>,
    pub(crate) unfinished: Vec<EvaluationUnfinishedTask>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EvaluationTaskRegistryCounts {
    pub(crate) reflection_active: usize,
    pub(crate) reflection_terminal: usize,
    pub(crate) reflection_by_id: usize,
    pub(crate) unacknowledged_failures: usize,
    pub(crate) deferred_active: usize,
    pub(crate) deferred_terminal: usize,
    pub(crate) deferred_by_wait: usize,
    pub(crate) deferred_by_task: usize,
    pub(crate) promises_active: usize,
    pub(crate) promises_terminal: usize,
    pub(crate) owned_promise_waits: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationUnfinishedState {
    Dormant,
    Reserved,
    Queued,
    Running,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationUnfinishedTask {
    pub(crate) task: EvaluationTaskId,
    pub(crate) state: EvaluationUnfinishedState,
    pub(crate) dependency: Option<EvaluationTaskId>,
    pub(crate) dependency_session: Option<EvaluationSessionId>,
    pub(crate) wait: Option<u64>,
    pub(crate) observed_epoch: Option<RuntimeObservationEpoch>,
    pub(crate) error: Option<Arc<EvaluationFailure>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvaluationTaskState {
    Complete(RuntimeValueRoot),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
    Abandoned,
    Exited,
    Killed(Arc<EvaluationFailure>),
}

fn evaluation_task_state(terminal: EvaluationWaitTerminal) -> EvaluationTaskState {
    match terminal {
        EvaluationWaitTerminal::Complete(value) => EvaluationTaskState::Complete(value),
        EvaluationWaitTerminal::Failed(error) => EvaluationTaskState::Failed(error),
        EvaluationWaitTerminal::Cancelled => EvaluationTaskState::Cancelled,
        EvaluationWaitTerminal::Abandoned => EvaluationTaskState::Abandoned,
        EvaluationWaitTerminal::Exited => EvaluationTaskState::Exited,
        EvaluationWaitTerminal::Killed(error) => EvaluationTaskState::Killed(error),
    }
}

fn task_wait_terminal(state: &EvaluationTaskState) -> EvaluationWaitTerminal {
    match state {
        EvaluationTaskState::Complete(value) => EvaluationWaitTerminal::Complete(value.clone()),
        EvaluationTaskState::Failed(error) => EvaluationWaitTerminal::Failed(error.clone()),
        EvaluationTaskState::Cancelled => EvaluationWaitTerminal::Cancelled,
        EvaluationTaskState::Abandoned => EvaluationWaitTerminal::Abandoned,
        EvaluationTaskState::Exited => EvaluationWaitTerminal::Exited,
        EvaluationTaskState::Killed(error) => EvaluationWaitTerminal::Killed(error.clone()),
    }
}

fn settle_task_work(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    work: EvaluationWorkId,
    state: EvaluationTaskState,
    promise_failure: Arc<EvaluationFailure>,
) -> EvaluationTaskState {
    evaluation_task_state(coordinator.settle_terminal_work(
        work,
        task_wait_terminal(&state),
        promise_failure,
    ))
}

#[derive(Clone)]
pub(crate) struct PendingReflectionTask {
    inner: Arc<PendingReflectionTaskInner>,
}

struct PendingReflectionTaskInner {
    context: EvalContext,
    handle: EvaluationTaskHandle,
    effect: RuntimeValueRoot,
    activated: AtomicBool,
}

impl PendingReflectionTask {
    pub(crate) fn handle(&self) -> &EvaluationTaskHandle {
        &self.inner.handle
    }

    pub(crate) fn commit(&self, publisher: TaskStatusPublisher, policy: PendingTaskPolicy) {
        if self.inner.activated.swap(true, Ordering::AcqRel) {
            return;
        }
        match policy.disposition {
            InitialTaskDisposition::Launch => {
                self.inner.context.activate_reflection_task(
                    &self.inner.handle,
                    self.inner.effect.as_core().clone(),
                    ReflectionTaskResultPolicy::ReturnValue,
                    self.inner.context.task_profile.clone(),
                    Some(publisher),
                    policy.acknowledge_error,
                );
            }
            InitialTaskDisposition::Cancel => self
                .inner
                .context
                .cancel_pending_reflection_task(&self.inner.handle, publisher),
        }
    }
}

impl Drop for PendingReflectionTaskInner {
    fn drop(&mut self) {
        if !self.activated.load(Ordering::Acquire) {
            self.context.cancel_reserved_task(&self.handle);
        }
    }
}

pub(crate) type TaskFailureLedger = RedBlackTreeMapSync<EvaluationTaskId, Arc<EvaluationFailure>>;
pub(crate) type RuntimeFailureLedger = RedBlackTreeMapSync<EvaluationSessionId, TaskFailureLedger>;

fn promise_assignment_terminal(
    runtime: EvaluationRuntimeId,
    assignment: &PromiseAssignment,
) -> EvaluationWaitTerminal {
    match assignment {
        Ok(value) => {
            debug_assert_eq!(value.runtime_id(), runtime);
            EvaluationWaitTerminal::Complete(value.clone())
        }
        Err(error) => EvaluationWaitTerminal::Failed(error.clone()),
    }
}

pub(crate) struct EvaluationDemandState {
    id: EvaluationSessionId,
    values: CoreValueFactory,
    default_reflection_profile: Arc<ReflectionTaskProfile>,
    require_default_reflection_profile: bool,
    closed: Arc<AtomicBool>,
    coordinator: Weak<EvaluationWorkCoordinator>,
}

impl fmt::Debug for EvaluationDemandState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationDemandState")
            .field("id", &self.id)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl EvaluationDemandState {
    fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        self.coordinator.upgrade()
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn closed_run_report(&self) -> EvaluationSessionRun {
        let failures = self
            .coordinator()
            .map_or_else(TaskFailureLedger::new_sync, |coordinator| {
                coordinator.failure_snapshot(self.id)
            });
        EvaluationSessionRun::Complete(EvaluationSessionReport {
            failures,
            unfinished: Vec::new(),
        })
    }
}

/// External ownership lease for one evaluation demand domain.
///
/// Machine-visible contexts retain [`EvaluationDemandState`], not this lease.
/// The strong coordinator route exists only here so dropping the last owner can
/// close and unregister the demand domain.
pub(crate) struct EvaluationSession {
    demand: Arc<EvaluationDemandState>,
    coordinator: Arc<EvaluationWorkCoordinator>,
}

impl fmt::Debug for EvaluationSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationSession")
            .finish_non_exhaustive()
    }
}

impl Drop for EvaluationSession {
    fn drop(&mut self) {
        self.demand.closed.store(true, Ordering::Release);
        let mut closing = self.coordinator.close_session(self.demand.id);
        for work in std::mem::take(&mut closing.reflection) {
            let failure = evaluation_failure(if work.cancel {
                format!(
                    "promised value's producer task {} was cancelled",
                    work.task.get()
                )
            } else {
                format!(
                    "promised value's producer task {} was abandoned when its evaluation session closed",
                    work.task.get()
                )
            });
            settle_task_work(
                &self.coordinator,
                work.id,
                if work.cancel {
                    EvaluationTaskState::Cancelled
                } else {
                    EvaluationTaskState::Abandoned
                },
                failure,
            );
            let mut machine = self.coordinator.retire_reflection(work.id);
            if work.cancel
                && let Some(machine) = &mut machine
            {
                machine.cancel();
            }
            drop(machine);
        }
        let abandoning_deferred = std::mem::take(&mut closing.deferred)
            .into_iter()
            .inspect(|work| {
                self.coordinator.settle_terminal_work(
                    work.id,
                    EvaluationWaitTerminal::Abandoned,
                    evaluation_failure(format!(
                        "promised value's producer task {} was abandoned when its evaluation session closed",
                        work.task.get()
                    )),
                );
            })
            .collect::<Vec<_>>();
        for work in abandoning_deferred {
            self.coordinator.retire_deferred(work.id);
            drop(work.machine);
        }
        closing.finish();
    }
}

impl EvaluationSession {
    fn with_execution_resources(
        coordinator: Arc<EvaluationWorkCoordinator>,
        values: CoreValueFactory,
    ) -> Arc<Self> {
        Self::with_execution_resources_and_default_profile(
            coordinator,
            values,
            Arc::new(ReflectionTaskProfile::unsealed()),
            false,
        )
    }

    fn with_execution_resources_and_default_profile(
        coordinator: Arc<EvaluationWorkCoordinator>,
        values: CoreValueFactory,
        default_reflection_profile: Arc<ReflectionTaskProfile>,
        require_default_reflection_profile: bool,
    ) -> Arc<Self> {
        let demand = Arc::new(EvaluationDemandState {
            id: EvaluationSessionId(values.ids().evaluation_session()),
            values: values.clone(),
            default_reflection_profile,
            require_default_reflection_profile,
            closed: Arc::new(AtomicBool::new(false)),
            coordinator: Arc::downgrade(&coordinator),
        });
        Arc::new(Self {
            demand,
            coordinator,
        })
    }

    fn isolated(values: CoreValueFactory) -> Arc<Self> {
        let coordinator = values.work_coordinator().unwrap_or_else(|| {
            let candidate = EvaluationWorkCoordinator::new(
                values.runtime_id(),
                values.ids().clone(),
                crate::runtime::RuntimeMutationAdmission::new(),
                RuntimeObservationState::new(),
            );
            values.work_coordinator_or_attach(candidate)
        });
        let session = Self::with_execution_resources(coordinator.clone(), values);
        coordinator.register_demand(&session.demand);
        session
    }

    /// Creates an evaluator owner on a private coordinator which is never
    /// attached to the runtime value factory. Closed bootstrap construction
    /// can reduce ordinary lazy applications without publishing demand or
    /// work into the runtime scheduler being initialized.
    fn private_closed(values: CoreValueFactory) -> Arc<Self> {
        let coordinator = EvaluationWorkCoordinator::new(
            values.runtime_id(),
            values.ids().clone(),
            crate::runtime::RuntimeMutationAdmission::new(),
            RuntimeObservationState::new(),
        );
        let session = Self::with_execution_resources(coordinator.clone(), values);
        coordinator.register_demand(&session.demand);
        session
    }

    #[cfg(test)]
    pub(crate) fn shared(coordinator: &Arc<EvaluationWorkCoordinator>) -> Arc<Self> {
        let session =
            Self::with_execution_resources(coordinator.clone(), coordinator.test_values());
        coordinator.register_demand(&session.demand);
        session
    }

    pub(crate) fn shared_with_default_profile(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        values: CoreValueFactory,
        default_reflection_profile: Arc<ReflectionTaskProfile>,
    ) -> Arc<Self> {
        let session = Self::with_execution_resources_and_default_profile(
            coordinator.clone(),
            values,
            default_reflection_profile,
            true,
        );
        coordinator.register_demand(&session.demand);
        session
    }
}

/// Cheap per-evaluation handle to one shared demand session.
///
/// Narrower provenance can be added to this handle without duplicating the
/// runtime-owned scheduler or reflection state.
#[derive(Debug, Clone)]
pub(crate) struct EvalContext {
    session: Arc<EvaluationDemandState>,
    task_profile: Arc<ReflectionTaskProfile>,
    task: Arc<OnceLock<Result<EvaluationTaskId, Arc<str>>>>,
    local_promise_owner: Option<Arc<LocalPromiseOwner>>,
    scheduled_task: bool,
    waits_for_claimed_tasks: bool,
    originating_task: Option<EvaluationTaskId>,
}

/// Direct client ownership for an isolated demand context.
///
/// The context itself remains machine-safe and owner-free; this wrapper holds
/// the external lease for callers which do not already have a runtime-managed
/// [`EvaluationSession`].
#[derive(Debug, Clone)]
pub(crate) struct OwnedEvalContext {
    context: EvalContext,
    _owner: Arc<EvaluationSession>,
}

impl Deref for OwnedEvalContext {
    type Target = EvalContext;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl OwnedEvalContext {
    pub(crate) fn new(owner: Arc<EvaluationSession>) -> Self {
        let context = EvalContext::new(&owner);
        Self {
            context,
            _owner: owner,
        }
    }

    #[cfg(test)]
    pub(crate) fn into_parts(self) -> (EvalContext, Arc<EvaluationSession>) {
        (self.context, self._owner)
    }
}

impl EvalContext {
    #[cfg(test)]
    pub(crate) fn standalone() -> OwnedEvalContext {
        Self::isolated(crate::core::test_value_factory())
    }

    pub(crate) fn new(session: &Arc<EvaluationSession>) -> Self {
        let task_profile = session.demand.default_reflection_profile.clone();
        Self {
            session: session.demand.clone(),
            task_profile,
            task: Arc::new(OnceLock::new()),
            local_promise_owner: None,
            scheduled_task: false,
            waits_for_claimed_tasks: false,
            originating_task: None,
        }
    }

    fn for_spark(session: Arc<EvaluationDemandState>) -> Self {
        let task_profile = session.default_reflection_profile.clone();
        Self {
            session,
            task_profile,
            task: Arc::new(OnceLock::new()),
            local_promise_owner: None,
            scheduled_task: false,
            waits_for_claimed_tasks: false,
            originating_task: None,
        }
    }

    fn for_client_demand(session: Arc<EvaluationDemandState>) -> Self {
        let task_profile = session.default_reflection_profile.clone();
        Self {
            session,
            task_profile,
            task: Arc::new(OnceLock::new()),
            local_promise_owner: None,
            // A client demand is coordinator-owned and must return a blocked
            // poll instead of retaining this Rust stack while its producer
            // waits.
            scheduled_task: true,
            waits_for_claimed_tasks: false,
            originating_task: None,
        }
    }

    pub(crate) fn with_task_profile(
        session: &Arc<EvaluationSession>,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        Self {
            session: session.demand.clone(),
            task_profile,
            task: Arc::new(OnceLock::new()),
            local_promise_owner: None,
            scheduled_task: false,
            waits_for_claimed_tasks: false,
            originating_task: None,
        }
    }

    pub(crate) fn patient_with_task_profile(
        session: &Arc<EvaluationSession>,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        Self {
            waits_for_claimed_tasks: true,
            ..Self::with_task_profile(session, task_profile)
        }
    }

    fn for_task(
        session: Arc<EvaluationDemandState>,
        id: EvaluationTaskId,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        let task = Arc::new(OnceLock::new());
        task.set(Ok(id))
            .expect("fresh task identity cell must be empty");
        Self {
            session,
            task_profile,
            task,
            local_promise_owner: None,
            scheduled_task: true,
            waits_for_claimed_tasks: false,
            originating_task: Some(id),
        }
    }

    fn for_deferred_task(
        session: Arc<EvaluationDemandState>,
        id: EvaluationTaskId,
        originating_task: Option<EvaluationTaskId>,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        let task = Arc::new(OnceLock::new());
        task.set(Ok(id))
            .expect("fresh deferred task identity cell must be empty");
        Self {
            session,
            task_profile,
            task,
            local_promise_owner: None,
            scheduled_task: true,
            waits_for_claimed_tasks: false,
            originating_task,
        }
    }

    pub(crate) fn values(&self) -> &CoreValueFactory {
        &self.session.values
    }

    fn coordinator_for_admission(&self) -> Result<Arc<EvaluationWorkCoordinator>, Arc<str>> {
        if self.session.is_closed() {
            return Err(Arc::from("evaluation demand session is closed"));
        }
        self.coordinator()
            .ok_or_else(|| Arc::from("evaluation demand coordinator expired"))
    }

    fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        self.session.coordinator()
    }

    pub(crate) fn current_observation_epoch(&self) -> RuntimeObservationEpoch {
        self.coordinator()
            .expect("evaluation demand coordinator expired")
            .current_observation_epoch()
    }

    /// Creates a zero-worker context in an explicitly selected runtime value
    /// domain. This is for pure closed bootstrap construction and focused
    /// tests; production task services use a runtime-registered session.
    pub(crate) fn isolated(values: CoreValueFactory) -> OwnedEvalContext {
        OwnedEvalContext::new(EvaluationSession::isolated(values))
    }

    /// Creates a closed bootstrap evaluator whose private scheduler cannot
    /// affect runtime readiness or launch work through the runtime's sealed
    /// reflection profile.
    pub(crate) fn private_closed(values: CoreValueFactory) -> OwnedEvalContext {
        OwnedEvalContext::new(EvaluationSession::private_closed(values))
    }

    /// Gives a directly driven effect task a private promise inventory.
    /// Scheduled task contexts use their coordinator work record instead.
    pub(crate) fn for_effect_task(mut self) -> Self {
        if !self.scheduled_task && self.local_promise_owner.is_none() {
            self.local_promise_owner = Some(Arc::new(LocalPromiseOwner::default()));
        }
        self
    }

    pub(crate) fn fail_local_promises(&self, failure: Arc<EvaluationFailure>) {
        if let Some(owner) = &self.local_promise_owner {
            owner.fail_all(failure);
        }
    }

    pub(crate) fn spark(&self, value: Value) {
        // A promise names data whose producer or completed assignment may
        // expose useful work. Nets and the remaining variants are already in
        // WHNF; metadata adds one privileged hidden demand.
        if matches!(
            value,
            Value::Lazy(_) | Value::Promised(_) | Value::Metadata(_)
        ) && !self.session.is_closed()
            && let Some(coordinator) = self.coordinator()
        {
            coordinator.submit_spark(self.session.clone(), value);
        }
    }

    pub(crate) fn demand_whnf(
        &self,
        value: RuntimeValueRoot,
    ) -> Result<ClientDemandHandle, Arc<str>> {
        self.admit_client_demand(ClientDemandOperation(value))
    }

    fn admit_client_demand(
        &self,
        operation: ClientDemandOperation,
    ) -> Result<ClientDemandHandle, Arc<str>> {
        let coordinator = self.coordinator_for_admission()?;
        if operation.runtime_id() != coordinator.runtime_id() {
            return Err(Arc::from(
                "client demand operation belongs to another evaluation runtime",
            ));
        }
        let cell = ClientDemandResultCell::new();
        let work = coordinator.admit_client_demand(
            self.session.clone(),
            operation,
            ClientDemandSink::new(cell.clone()),
        )?;
        Ok(ClientDemandHandle {
            runtime: coordinator.runtime_id(),
            work,
            coordinator: Arc::downgrade(&coordinator),
            cell,
            active: true,
        })
    }

    pub(crate) fn evaluate_whnf(
        &self,
        value: &Value,
    ) -> Result<Value, crate::core::EvaluationHalt> {
        let handle = self
            .demand_whnf(RuntimeValueRoot::new(self.values(), value.clone()))
            .map_err(|error| crate::core::EvaluationHalt::new(error.as_ref()))?;
        match self.drive_client_demand(handle)? {
            ClientDemandResult::Complete(value) => Ok(value.into_core()),
            ClientDemandResult::Abandoned => unreachable!(
                "WHNF client demand must return a value or a propagated evaluation failure"
            ),
            ClientDemandResult::Failed(_) | ClientDemandResult::Killed(_) => {
                unreachable!("client failures are returned by drive_client_demand")
            }
        }
    }

    fn drive_client_demand(
        &self,
        mut handle: ClientDemandHandle,
    ) -> Result<ClientDemandResult, crate::core::EvaluationHalt> {
        let coordinator = self
            .coordinator_for_admission()
            .map_err(|error| crate::core::EvaluationHalt::new(error.as_ref()))?;

        loop {
            if let Some(result) = handle.poll() {
                return terminal_client_demand_result(result);
            }
            if let Some(claimed) = coordinator.claim_client_demand(handle.work) {
                coordinator.poll_claimed_client_demand(claimed);
                continue;
            }

            let generation = coordinator.work_generation();
            let Some(snapshot) = coordinator.client_demand_snapshot(handle.work) else {
                // Retirement removes the coordinator record before publishing
                // its sink. Waiting on the cell closes that intentionally
                // tiny handoff without racing a completion notification.
                return terminal_client_demand_result(handle.wait());
            };
            match snapshot {
                ClientDemandSnapshot::Queued => continue,
                ClientDemandSnapshot::Running => {
                    if handle.poll().is_none() && coordinator.work_generation() == generation {
                        coordinator.wait_for_change(generation);
                    }
                }
                ClientDemandSnapshot::Blocked {
                    dependency,
                    subscription_epoch,
                } => {
                    if let Some(wait) = dependency.producer_wait() {
                        if let Some(task) = prioritized_task_for(&coordinator, wait)
                            && let Some(work) = coordinator.claim_task(task)
                        {
                            coordinator.poll_claimed_task(work);
                            continue;
                        }
                        if coordinator.target_has_running_producer(wait) {
                            if handle.poll().is_none()
                                && coordinator.work_generation() == generation
                            {
                                coordinator.wait_for_change(generation);
                            }
                            continue;
                        }
                    }

                    // A dependency chain may be blocked while another task or
                    // worker owns the state transition which will disturb it.
                    // Use the runtime pump's stability boundary before
                    // abandoning this client demand: run unrelated useful
                    // lifecycle work, release parked best-effort sparks, and
                    // wait for worker-owned progress. A client-visible blocked
                    // halt is valid only after none of those routes remains.
                    if coordinator.poll_runtime_work() {
                        continue;
                    }
                    if coordinator.abandon_quiescent_sparks() != 0 {
                        continue;
                    }
                    let runtime = coordinator.runtime_pump_snapshot();
                    if runtime.useful_ready || runtime.abandonable_sparks {
                        continue;
                    }
                    if runtime.progress_owned {
                        if handle.poll().is_none() && coordinator.work_generation() == generation {
                            coordinator.wait_for_change(generation);
                        }
                        continue;
                    }

                    let Some(dependency) = handle.abandon_if_stably_blocked(subscription_epoch)
                    else {
                        continue;
                    };
                    return Err(client_demand_halt(dependency));
                }
            }
        }
    }

    pub(crate) fn runs_scheduled_task(&self) -> bool {
        self.scheduled_task
    }

    pub(crate) fn waits_for_claimed_tasks(&self) -> bool {
        self.waits_for_claimed_tasks
    }

    /// Waits for one scheduler change only while the target has a producer
    /// claimed by another thread.
    ///
    /// Rechecking against the runtime work generation prevents a producer
    /// release between [`Self::pump_wait`] and this call from becoming a lost
    /// wakeup.
    pub(crate) fn wait_for_claimed_task(&self, target: &EvaluationWaitToken) {
        if target.owner_id() != self.session.id {
            return;
        }
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        let generation = coordinator.work_generation();
        if !coordinator.target_has_running_producer(target) {
            return;
        }
        coordinator.wait_for_change(generation);
    }

    /// Waits for one scheduler transition when an exact dependency chain ends
    /// at a task with a coordinator-indexed broad observation.
    ///
    /// This is narrower than treating every live task as future progress: a
    /// pure wait cycle has no observed epoch and remains `NoProgress` for
    /// quiescence analysis.
    pub(crate) fn wait_for_observed_dependency_progress(
        &self,
        target: &EvaluationWaitToken,
    ) -> bool {
        let Some(coordinator) = self.coordinator() else {
            return false;
        };
        let generation = coordinator.work_generation();
        if !coordinator.dependency_observes_runtime(target) {
            return false;
        }
        if coordinator.work_generation() == generation {
            coordinator.wait_for_change(generation);
        }
        true
    }

    pub(crate) fn observes_as_task(&self, task: EvaluationTaskId) -> bool {
        self.originating_task == Some(task)
            || matches!(self.task.get(), Some(Ok(current)) if *current == task)
    }

    pub(crate) fn lazy_task<F>(
        &self,
        lazy: &LazyValue,
        build: F,
    ) -> Result<EvaluationWaitToken, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Box<dyn EvaluationTaskMachine>,
    {
        self.deferred_task(DeferredProducer::Lazy(lazy.clone()), build)
    }

    pub(crate) fn promise_task<F>(
        &self,
        promise: &PromisedValue,
        build: F,
    ) -> Result<EvaluationWaitToken, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Box<dyn EvaluationTaskMachine>,
    {
        self.deferred_task(DeferredProducer::Promise(promise.clone()), build)
    }

    fn deferred_task<F>(
        &self,
        producer: DeferredProducer,
        build: F,
    ) -> Result<EvaluationWaitToken, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Box<dyn EvaluationTaskMachine>,
    {
        let coordinator = self.coordinator_for_admission()?;
        let deferred = producer.id();
        if let Some(wait) = coordinator.deferred_wait(deferred) {
            return Ok(wait);
        }

        let id = allocate_task_id(self.values())?;
        let wait = allocate_wait_token(&self.session, id)?;
        let originating_task = self
            .originating_task
            .or_else(|| self.task.get().and_then(|task| task.as_ref().ok()).copied());
        let machine = build(Self::for_deferred_task(
            self.session.clone(),
            id,
            originating_task,
            self.task_profile.clone(),
        ));
        let work = match coordinator.reserve_deferred(
            &self.session,
            id,
            wait.clone(),
            producer,
            machine,
        )? {
            DeferredWorkReservation::Existing(wait) => return Ok(wait),
            DeferredWorkReservation::New(work) => work,
        };
        let _ = coordinator.activate_deferred(work);
        Ok(wait)
    }

    #[cfg(test)]
    pub(crate) fn install_reflection_launcher(
        &self,
        launcher: Arc<dyn ReflectionTaskLauncher>,
    ) -> Result<(), Arc<str>> {
        self.task_profile.seal(launcher.clone())?;
        if Arc::ptr_eq(&self.task_profile, &self.session.default_reflection_profile) {
            return Ok(());
        }
        self.session.default_reflection_profile.seal(launcher)
    }

    #[cfg(test)]
    pub(crate) fn with_new_task(&self) -> Result<Self, Arc<str>> {
        let context = Self {
            session: self.session.clone(),
            task_profile: self.task_profile.clone(),
            task: Arc::new(OnceLock::new()),
            local_promise_owner: None,
            scheduled_task: false,
            waits_for_claimed_tasks: false,
            originating_task: None,
        };
        let task = context.task_id()?;
        Ok(Self {
            originating_task: Some(task),
            ..context
        })
    }

    #[cfg(test)]
    pub(crate) fn task_owned_promises(
        &self,
        labels: impl IntoIterator<Item = Arc<str>>,
    ) -> Result<(Vec<PromisedValue>, EvaluationTaskHandle, EvalContext), Arc<str>> {
        let labels = labels.into_iter().collect::<Vec<_>>();
        let promises = Arc::new(Mutex::new(None));
        let output = promises.clone();
        let owner_context = Arc::new(Mutex::new(None));
        let owner_output = owner_context.clone();
        let task = self.schedule_task(move |context| {
            let owned = labels
                .into_iter()
                .map(|label| PromisedValue::fixpoint(&context, label))
                .collect::<Result<Vec<_>, _>>()?;
            *output.lock().expect("test promise output was poisoned") = Some(owned);
            *owner_output
                .lock()
                .expect("test promise owner output was poisoned") = Some(context);
            Ok(Box::new(PendingTestPromiseTask))
        })?;
        let promises = promises
            .lock()
            .expect("test promise output was poisoned")
            .take()
            .expect("test task construction must publish its promises");
        let owner_context = owner_context
            .lock()
            .expect("test promise owner output was poisoned")
            .take()
            .expect("test task construction must publish its owner context");
        Ok((promises, task, owner_context))
    }

    #[cfg(test)]
    pub(crate) fn task_owned_promise(
        &self,
        label: impl Into<Arc<str>>,
    ) -> Result<(PromisedValue, EvaluationTaskHandle, EvalContext), Arc<str>> {
        let (mut promises, task, owner) = self.task_owned_promises([label.into()])?;
        Ok((
            promises.pop().expect("one promise was requested"),
            task,
            owner,
        ))
    }

    pub(crate) fn task_id(&self) -> Result<EvaluationTaskId, Arc<str>> {
        self.task
            .get_or_init(|| allocate_task_id(self.values()))
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn session_id(&self) -> EvaluationSessionId {
        self.session.id
    }

    pub(crate) fn register_promise(
        &self,
        promise: &Arc<crate::core::PromiseCell>,
    ) -> Result<PromiseProducerObligation, Arc<str>> {
        if self.session.is_closed() {
            return Err(Arc::from("evaluation demand session is closed"));
        }
        let owner = self.task_id()?;
        let wait = allocate_wait_token(&self.session, owner)?;
        let source = if self.scheduled_task {
            let coordinator = self.coordinator_for_admission()?;
            let work = coordinator.register_task_promise(owner, wait.clone(), promise)?;
            PromiseProducerSource::Coordinator {
                work,
                promise: promise.id(),
                coordinator: Arc::downgrade(&coordinator),
            }
        } else if let Some(local_owner) = &self.local_promise_owner {
            local_owner.register(promise, wait.clone());
            PromiseProducerSource::Local {
                promise: promise.id(),
                owner: Arc::downgrade(local_owner),
            }
        } else {
            return Err(format!(
                "task {} has no active work record for its promise",
                owner.get()
            )
            .into());
        };
        Ok(PromiseProducerObligation {
            owner,
            wait,
            source,
        })
    }

    /// Registers an executable task whose concrete specialization remains
    /// hidden behind [`EvaluationTaskMachine`]. Construction happens outside
    /// the coordinator lock, so host snapshots and evaluator work may safely
    /// use this same session.
    #[cfg(test)]
    pub(crate) fn schedule_machine<F>(
        &self,
        lifecycle: Option<TaskStatusPublisher>,
        build: F,
    ) -> Result<EvaluationTaskHandle, Arc<EvaluationFailure>>
    where
        F: FnOnce(EvalContext) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>>,
    {
        self.prepare_machine(lifecycle, build)
            .map(PreparedEvaluationTask::activate)
    }

    /// Constructs a coordinator-owned task without making it runnable.
    ///
    /// The returned reservation is already a complete runtime root. A host
    /// may use guarded activation to publish another runtime transition in
    /// the same settlement-exclusion interval before exposing either fact.
    pub(crate) fn prepare_machine<F>(
        &self,
        lifecycle: Option<TaskStatusPublisher>,
        build: F,
    ) -> Result<PreparedEvaluationTask, Arc<EvaluationFailure>>
    where
        F: FnOnce(EvalContext) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>>,
    {
        let coordinator = self
            .coordinator_for_admission()
            .map_err(|error| evaluation_failure(error.as_ref()))?;
        let id =
            allocate_task_id(self.values()).map_err(|error| evaluation_failure(error.as_ref()))?;
        let wait = allocate_wait_token(&self.session, id)
            .map_err(|error| evaluation_failure(error.as_ref()))?;
        let context = Self::for_task(self.session.clone(), id, self.task_profile.clone());
        let work = coordinator
            .reserve_reflection(&self.session, id, wait.clone())
            .map_err(|error| evaluation_failure(error.as_ref()))?;
        let machine = match build(context) {
            Ok(machine) => machine,
            Err(error) => {
                // This helper reports construction failure directly to its
                // Rust caller; it never returns a launched task handle whose
                // failure would need runtime reporting.
                coordinator.acknowledge_task_failure(self.session.id, id);
                assert!(
                    coordinator.terminalize_reserved_reflection(work),
                    "failed test task construction must terminalize its reservation"
                );
                coordinator.settle_terminal_work(
                    work,
                    EvaluationWaitTerminal::Failed(error.clone()),
                    error.clone(),
                );
                drop(coordinator.retire_reflection(work));
                return Err(error);
            }
        };
        if let Some(lifecycle) = lifecycle {
            assert!(
                coordinator.attach_reflection_lifecycle_publisher(work, lifecycle),
                "fresh reflection reservation must accept its lifecycle publisher"
            );
        }
        coordinator
            .install_reflection_machine(work, machine)
            .unwrap_or_else(|_| panic!("fresh reflection reservation must accept its machine"));
        Ok(PreparedEvaluationTask {
            handle: EvaluationTaskHandle::new(&coordinator, self.session.id, id, work, wait),
            coordinator,
        })
    }

    #[cfg(test)]
    pub(crate) fn schedule_task<F>(&self, build: F) -> Result<EvaluationTaskHandle, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Result<Box<dyn EvaluationTaskMachine>, Arc<str>>,
    {
        self.schedule_machine(None, |context| {
            build(context)
                .map_err(|error| evaluation_failure(format!("task construction failed: {error}")))
        })
        .map_err(|error| Arc::from(error.to_string()))
    }

    fn reserve_task(&self) -> Result<EvaluationTaskHandle, Arc<str>> {
        let coordinator = self.coordinator_for_admission()?;
        let id = allocate_task_id(self.values())?;
        let wait = allocate_wait_token(&self.session, id)?;
        let work = coordinator.reserve_reflection(&self.session, id, wait.clone())?;
        Ok(EvaluationTaskHandle::new(
            &coordinator,
            self.session.id,
            id,
            work,
            wait,
        ))
    }

    fn activate_reflection_task(
        &self,
        handle: &EvaluationTaskHandle,
        effect: Value,
        result_policy: ReflectionTaskResultPolicy,
        task_profile: Arc<ReflectionTaskProfile>,
        status_publisher: Option<TaskStatusPublisher>,
        error_acknowledged: bool,
    ) {
        let Ok(coordinator) = self.coordinator_for_admission() else {
            return;
        };
        if error_acknowledged {
            coordinator.acknowledge_task_failure(handle.session_id(), handle.id());
        }
        if let Some(status_publisher) = status_publisher
            && !coordinator.attach_reflection_status_publisher(handle.work, status_publisher)
        {
            return;
        }
        let result = task_profile
            .launcher()
            .ok_or_else(|| {
                Arc::new(EvaluationFailure::message(
                    "reflection task profile is not sealed",
                ))
            })
            .and_then(|launcher| {
                launcher.build(
                    Self::for_task(self.session.clone(), handle.id, task_profile.clone()),
                    effect,
                    result_policy,
                )
            });
        match result {
            Ok(machine) => {
                if coordinator
                    .install_reflection_machine(handle.work, machine)
                    .is_ok()
                {
                    // A concurrent cancellation may already own terminal
                    // cleanup; activation then returns false.
                    let _ = coordinator.activate_reflection(handle.work);
                }
            }
            Err(error) => {
                let promise_failure = error.clone();
                if coordinator.terminalize_reserved_reflection(handle.work) {
                    settle_task_work(
                        &coordinator,
                        handle.work,
                        EvaluationTaskState::Failed(error),
                        promise_failure,
                    );
                    drop(coordinator.retire_reflection(handle.work));
                }
            }
        }
    }

    fn cancel_reserved_task(&self, handle: &EvaluationTaskHandle) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        let _ = coordinator.discard_reserved_reflection(handle.work);
    }

    fn cancel_pending_reflection_task(
        &self,
        handle: &EvaluationTaskHandle,
        status_publisher: TaskStatusPublisher,
    ) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        assert!(
            coordinator.attach_reflection_status_publisher(handle.work, status_publisher),
            "a committed pending task must remain reserved"
        );
        let cancellation = coordinator.request_reflection_cancellation(handle.work);
        assert_eq!(
            cancellation,
            ReflectionCancellation::Terminalize,
            "a committed pre-launch cancellation must own its reservation"
        );
        settle_task_work(
            &coordinator,
            handle.work,
            EvaluationTaskState::Cancelled,
            evaluation_failure("reflection fixpoint producer was cancelled"),
        );
        drop(coordinator.retire_reflection(handle.work));
    }

    pub(crate) fn reserve_reflection_task(
        &self,
        effect: Value,
    ) -> Result<PendingReflectionTask, Arc<str>> {
        self.coordinator_for_admission()?;
        if !self.task_profile.is_sealed() {
            return Err(Arc::from(
                "current task has no sealed reflection task profile",
            ));
        }
        Ok(PendingReflectionTask {
            inner: Arc::new(PendingReflectionTaskInner {
                context: self.clone(),
                handle: self.reserve_task()?,
                effect: RuntimeValueRoot::new(self.values(), effect),
                activated: AtomicBool::new(false),
            }),
        })
    }

    pub(crate) fn start_reflection_task(
        &self,
        effect: Value,
        result_policy: ReflectionTaskResultPolicy,
    ) -> Result<EvaluationTaskHandle, Arc<str>> {
        let coordinator = self.coordinator_for_admission()?;
        let default_profile = self.session.default_reflection_profile.clone();
        if default_profile.is_sealed() {
            let handle = self.reserve_task()?;
            self.activate_reflection_task(
                &handle,
                effect,
                result_policy,
                default_profile,
                None,
                false,
            );
            return Ok(handle);
        }

        if self.session.require_default_reflection_profile {
            return Err(Arc::from(
                "evaluation runtime default reflection task profile is not sealed",
            ));
        }

        // Focused evaluator tests and internal clients may intentionally use a
        // bare session. Preserve an inspectable wait record for them; ordinary
        // Assembler sessions always install a launcher.
        let id = allocate_task_id(self.values())?;
        let wait = allocate_wait_token(&self.session, id)?;
        let work = coordinator.register_dormant_reflection(&self.session, id, wait.clone())?;
        Ok(EvaluationTaskHandle::new(
            &coordinator,
            self.session.id,
            id,
            work,
            wait,
        ))
    }

    pub(crate) fn poll_reflection_task(&self, task: &EvaluationTaskHandle) -> EvaluationWaitPoll {
        self.poll_wait(&task.wait)
    }

    /// Parks a host driver until the coordinator changes after a pending task
    /// observation. Exact dependency completion and broad runtime observation
    /// both advance this generation, so the caller need not choose a wake
    /// source and cannot lose a completion between its poll and park.
    pub(crate) fn wait_for_task_change(&self, task: &EvaluationTaskHandle) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        let generation = coordinator.work_generation();
        if matches!(
            self.poll_reflection_task(task),
            EvaluationWaitPoll::Pending(_)
        ) && !coordinator.session_has_ready_task(self.session.id)
            && coordinator.work_generation() == generation
        {
            coordinator.wait_for_change(generation);
        }
    }

    pub(crate) fn has_ready_session_task(&self) -> bool {
        self.coordinator()
            .is_some_and(|coordinator| coordinator.session_has_ready_task(self.session.id))
    }

    #[cfg(test)]
    pub(crate) fn attach_task_status_publisher(
        &self,
        task: &EvaluationTaskHandle,
        publisher: TaskStatusPublisher,
    ) -> bool {
        self.coordinator().is_some_and(|coordinator| {
            coordinator.attach_reflection_status_publisher(task.work, publisher)
        })
    }

    pub(crate) fn acknowledge_task_failure(
        &self,
        owner: EvaluationSessionId,
        task: EvaluationTaskId,
    ) {
        if let Some(coordinator) = self.coordinator() {
            coordinator.acknowledge_task_failure(owner, task);
        }
    }

    pub(crate) fn poll_wait(&self, wait: &EvaluationWaitToken) -> EvaluationWaitPoll {
        if let Some(terminal) = wait.terminal_poll() {
            return terminal;
        }
        if self
            .local_promise_owner
            .as_ref()
            .is_some_and(|owner| owner.contains_wait(wait))
        {
            return EvaluationWaitPoll::Pending(wait.clone());
        }
        if self
            .coordinator()
            .is_some_and(|coordinator| coordinator.producer_for_wait(wait).is_some())
        {
            return EvaluationWaitPoll::Pending(wait.clone());
        }
        if let Some(terminal) = wait.terminal_poll() {
            return terminal;
        }
        EvaluationWaitPoll::Failed(evaluation_failure(
            "evaluation wait token is no longer registered",
        ))
    }

    pub(crate) fn pump_wait(
        &self,
        wait: &EvaluationWaitToken,
        step_budget: usize,
    ) -> EvaluationPumpOutcome {
        let Some(coordinator) = self.coordinator() else {
            return if wait.terminal_poll().is_some() {
                EvaluationPumpOutcome::TargetReady
            } else {
                EvaluationPumpOutcome::NoProgress
            };
        };
        pump_demand(&coordinator, self.session.id, self, wait, step_budget)
    }

    /// Runs every executable task until all are terminal or one complete pass
    /// leaves every unfinished task unchanged.
    pub(crate) fn run_until_quiescent(&self) -> EvaluationSessionRun {
        self.session.run_until_quiescent()
    }

    #[cfg(test)]
    pub(crate) fn complete_wait(&self, wait: &EvaluationWaitToken) {
        self.complete_wait_with_value(wait, crate::core::keys::unit_value());
    }

    #[cfg(test)]
    pub(crate) fn complete_wait_with_value(&self, wait: &EvaluationWaitToken, value: Value) {
        let coordinator = self
            .coordinator()
            .expect("test wait must retain its coordinator");
        let target = wait.clone();
        let wait = test_reflection_dependency(&coordinator, wait);
        let work = coordinator
            .reflection_work_for_wait(&wait)
            .expect("test task must belong to this runtime");
        assert!(coordinator.terminalize_reflection(work));
        settle_task_work(
            &coordinator,
            work,
            EvaluationTaskState::Complete(RuntimeValueRoot::new(&self.session.values, value)),
            evaluation_failure("reflection task completed without fulfilling its fixpoint"),
        );
        drop(coordinator.retire_reflection(work));
        while matches!(
            self.pump_wait(&target, 256),
            EvaluationPumpOutcome::BudgetExhausted
        ) {}
    }

    #[cfg(test)]
    pub(crate) fn fail_wait(&self, wait: &EvaluationWaitToken, error: impl Into<Arc<str>>) {
        self.fail_wait_with_failure(wait, evaluation_failure(error.into()));
    }

    #[cfg(test)]
    pub(crate) fn fail_wait_with_failure(
        &self,
        wait: &EvaluationWaitToken,
        failure: Arc<EvaluationFailure>,
    ) {
        let coordinator = self
            .coordinator()
            .expect("test wait must retain its coordinator");
        let target = wait.clone();
        let wait = test_reflection_dependency(&coordinator, wait);
        let work = coordinator
            .reflection_work_for_wait(&wait)
            .expect("test task must belong to this runtime");
        assert!(coordinator.terminalize_reflection(work));
        settle_task_work(
            &coordinator,
            work,
            EvaluationTaskState::Failed(failure.clone()),
            failure,
        );
        drop(coordinator.retire_reflection(work));
        while matches!(
            self.pump_wait(&target, 256),
            EvaluationPumpOutcome::BudgetExhausted
        ) {}
    }

    #[cfg(test)]
    pub(crate) fn reflection_task_count(&self) -> usize {
        self.task_registry_counts().reflection_active
    }

    #[cfg(test)]
    pub(crate) fn deferred_task_count(&self) -> usize {
        self.task_registry_counts().deferred_active
    }

    #[cfg(test)]
    pub(crate) fn task_registry_counts(&self) -> EvaluationTaskRegistryCounts {
        let coordinator = self
            .coordinator()
            .expect("test demand must retain its coordinator");
        let deferred_counts = coordinator.deferred_counts(self.session.id);
        let promise_count = coordinator.task_promise_count(self.session.id);
        let (reflection_active, reflection_by_id) = coordinator.reflection_counts(self.session.id);
        EvaluationTaskRegistryCounts {
            reflection_active,
            reflection_terminal: 0,
            reflection_by_id,
            unacknowledged_failures: coordinator.failure_snapshot(self.session.id).size(),
            deferred_active: deferred_counts.0,
            deferred_terminal: 0,
            deferred_by_wait: deferred_counts.1,
            deferred_by_task: deferred_counts.2,
            promises_active: promise_count,
            promises_terminal: 0,
            owned_promise_waits: promise_count,
        }
    }

    #[cfg(test)]
    pub(crate) fn lazy_failure(&self, lazy: &LazyValue) -> Option<Arc<EvaluationFailure>> {
        lazy.cached().and_then(Result::err)
    }

    #[cfg(test)]
    pub(crate) fn promise_failure(
        &self,
        promise: &PromisedValue,
    ) -> Option<Arc<EvaluationFailure>> {
        promise.assignment().and_then(Result::err)
    }

    pub(crate) fn lazy_failure_for_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<Arc<EvaluationFailure>> {
        match wait.terminal_poll() {
            Some(EvaluationWaitPoll::Failed(failure)) => Some(failure),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn shares_session_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.session, &other.session)
    }
}

fn terminal_client_demand_result(
    result: ClientDemandResult,
) -> Result<ClientDemandResult, crate::core::EvaluationHalt> {
    match result {
        ClientDemandResult::Failed(failure) | ClientDemandResult::Killed(failure) => {
            Err(crate::core::EvaluationHalt::failure(failure))
        }
        ClientDemandResult::Abandoned => Err(crate::core::EvaluationHalt::new(
            "client evaluation demand was abandoned",
        )),
        complete => Ok(complete),
    }
}

fn client_demand_halt_poll(halt: crate::core::EvaluationHalt) -> coordinator::ClientDemandPoll {
    if let Some(wait) = halt.blocked_on() {
        coordinator::ClientDemandPoll::Blocked(WorkDependency::Wait(wait.0))
    } else if let Some(promise) = halt.unassigned_promise() {
        coordinator::ClientDemandPoll::Blocked(WorkDependency::Promise(promise.clone()))
    } else {
        coordinator::ClientDemandPoll::Failed(halt.into_permanent_failure())
    }
}

fn client_demand_halt(dependency: WorkDependency) -> crate::core::EvaluationHalt {
    match dependency {
        WorkDependency::Wait(wait) => crate::core::EvaluationHalt::blocked(CoreWaitToken(wait)),
        WorkDependency::Promise(promise) => crate::core::EvaluationHalt::unassigned(promise),
        #[cfg(test)]
        WorkDependency::Test(_) => crate::core::EvaluationHalt::new(
            "client evaluation blocked on a synthetic test dependency",
        ),
    }
}

#[cfg(test)]
fn test_reflection_dependency(
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
        let kind = match work {
            ClaimedTaskWork::Reflection(claim) => ClaimedTaskKind::Reflection(claim),
            ClaimedTaskWork::Deferred(claim) => ClaimedTaskKind::Deferred(claim),
        };
        Self { coordinator, kind }
    }

    fn poll(&mut self, step_budget: usize) -> EvaluationMachinePoll {
        match &mut self.kind {
            ClaimedTaskKind::Reflection(task) => task.poll(step_budget),
            ClaimedTaskKind::Deferred(task) => task.poll(step_budget),
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

impl EvaluationDemandState {
    fn run_until_quiescent(&self) -> EvaluationSessionRun {
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

fn prioritized_task_for(
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

fn pump_demand(
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
    let (work_poll, terminal_state) = match poll {
        EvaluationMachinePoll::Yielded => (ReflectionWorkPoll::Yielded, None),
        EvaluationMachinePoll::Blocked(block) => (ReflectionWorkPoll::Blocked(block), None),
        EvaluationMachinePoll::Exit(exit) => (ReflectionWorkPoll::Exit(exit), None),
        EvaluationMachinePoll::Complete(value) => (
            ReflectionWorkPoll::Terminal,
            Some(EvaluationTaskState::Complete(
                RuntimeValueRoot::from_runtime(coordinator.runtime_id(), value),
            )),
        ),
        EvaluationMachinePoll::Failed(error) => (
            ReflectionWorkPoll::Terminal,
            Some(EvaluationTaskState::Failed(error)),
        ),
        EvaluationMachinePoll::Cancelled => (
            ReflectionWorkPoll::Terminal,
            Some(EvaluationTaskState::Cancelled),
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

    let state = if release.cancel {
        EvaluationTaskState::Cancelled
    } else if release.abandoned {
        EvaluationTaskState::Abandoned
    } else {
        terminal_state.expect("terminal reflection poll must carry a terminal result")
    };
    let promise_failure = match &state {
        EvaluationTaskState::Complete(_) => {
            evaluation_failure("reflection task completed without fulfilling its fixpoint")
        }
        EvaluationTaskState::Failed(error) => error.clone(),
        EvaluationTaskState::Cancelled => {
            evaluation_failure("reflection fixpoint producer was cancelled")
        }
        EvaluationTaskState::Abandoned => {
            evaluation_failure("reflection fixpoint producer was abandoned")
        }
        EvaluationTaskState::Exited => {
            evaluation_failure("reflection fixpoint producer exited without a result")
        }
        EvaluationTaskState::Killed(error) => error.clone(),
    };
    settle_task_work(coordinator, work, state, promise_failure);
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
            Some(EvaluationWaitTerminal::Complete(
                RuntimeValueRoot::from_runtime(coordinator.runtime_id(), value),
            )),
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

    fn poll_claimed_task(self: &Arc<Self>, work: ClaimedTaskWork) {
        let mut claimed = ClaimedTask::new(self.clone(), work);
        let poll = claimed.poll(TASK_POLL_QUANTUM);
        let (_, _, released) = claimed.release(poll);
        if let Some(machine) = released {
            machine.finish();
        }
    }

    fn poll_claimed_client_demand(self: &Arc<Self>, mut claimed: coordinator::ClaimedClientDemand) {
        let poll = claimed.poll();
        self.release_client_demand(claimed, poll);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Barrier, mpsc};
    use std::time::{Duration, Instant};

    struct SameRuntimeFixture {
        _assembler: crate::api::Assembler,
        runtime: crate::api::EvaluationRuntime,
    }

    impl SameRuntimeFixture {
        fn new() -> Self {
            let runtime = crate::api::EvaluationRuntime::new(0).expect("test runtime should build");
            let assembler = crate::api::Assembler::builder()
                .evaluation_runtime(runtime.clone())
                .build()
                .expect("test assembler should seal the runtime reflection profile");
            Self {
                _assembler: assembler,
                runtime,
            }
        }

        fn context(&self) -> OwnedEvalContext {
            let session = self
                .runtime
                .new_evaluation_session()
                .expect("same-runtime test session should build");
            debug_assert_eq!(session.demand.values.runtime_id(), self.runtime.id());
            OwnedEvalContext::new(session)
        }
    }

    fn poll_one_runtime_work(coordinator: &Arc<EvaluationWorkCoordinator>) -> bool {
        match coordinator.select() {
            coordinator::CoordinatorSelection::ClientDemand(claimed) => {
                coordinator.poll_claimed_client_demand(claimed);
                true
            }
            coordinator::CoordinatorSelection::Task(work) => {
                coordinator.poll_claimed_task(work);
                true
            }
            coordinator::CoordinatorSelection::Spark(_) => {
                panic!("client demand tests must not manufacture spark work")
            }
            coordinator::CoordinatorSelection::None => false,
        }
    }

    fn poll_runtime_until(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mut complete: impl FnMut() -> bool,
    ) {
        for _ in 0..64 {
            if complete() {
                return;
            }
            assert!(
                poll_one_runtime_work(coordinator),
                "runtime became idle before the expected client result"
            );
        }
        panic!("runtime did not produce the expected client result");
    }

    fn client_demand_publish_lock_probe(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        handle: &ClientDemandHandle,
    ) -> Arc<Mutex<Option<bool>>> {
        let observed = Arc::new(Mutex::new(None));
        let weak_coordinator = Arc::downgrade(coordinator);
        let probe_result = observed.clone();
        handle.set_publish_probe(move || {
            let locks_are_free = weak_coordinator
                .upgrade()
                .is_none_or(|coordinator| coordinator.runtime_locks_are_free());
            *probe_result
                .lock()
                .expect("client demand lock probe was poisoned") = Some(locks_are_free);
        });
        observed
    }

    fn assert_client_demand_published_after_unlock(observed: &Mutex<Option<bool>>) {
        assert_eq!(
            *observed
                .lock()
                .expect("client demand lock probe was poisoned"),
            Some(true),
            "client demand retirement must publish only after coordinator locks and mutation admission are released"
        );
    }

    #[test]
    fn client_demand_completes_whnf_into_its_result_cell() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        let expected = Value::Number(42.into());
        let handle = context
            .demand_whnf(RuntimeValueRoot::new(context.values(), expected.clone()))
            .expect("same-runtime client demand should be admitted");

        assert_eq!(handle.runtime_id(), fixture.runtime.id());
        assert!(handle.poll().is_none());
        assert!(poll_one_runtime_work(&coordinator));
        assert!(matches!(
            handle.poll(),
            Some(ClientDemandResult::Complete(value)) if value.as_core() == &expected
        ));
    }

    #[test]
    fn client_demand_retirement_publishes_after_runtime_unlock() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");

        let completed = context
            .demand_whnf(RuntimeValueRoot::new(
                context.values(),
                context.values().unit(),
            ))
            .expect("unit demand should be admitted");
        let completed_probe = client_demand_publish_lock_probe(&coordinator, &completed);
        assert!(poll_one_runtime_work(&coordinator));
        assert_client_demand_published_after_unlock(&completed_probe);

        let abandoned = context
            .demand_whnf(RuntimeValueRoot::new(
                context.values(),
                context.values().unit(),
            ))
            .expect("abandoned demand should be admitted");
        let abandoned_probe = client_demand_publish_lock_probe(&coordinator, &abandoned);
        abandoned.abandon();
        assert_client_demand_published_after_unlock(&abandoned_probe);

        let promise = PromisedValue::new(context.values(), "stably blocked client input");
        let mut blocked = context
            .demand_whnf(RuntimeValueRoot::new(
                context.values(),
                Value::Promised(promise.clone()),
            ))
            .expect("promise demand should be admitted");
        assert!(poll_one_runtime_work(&coordinator));
        let ClientDemandSnapshot::Blocked {
            subscription_epoch, ..
        } = coordinator
            .client_demand_snapshot(blocked.work())
            .expect("client demand should remain registered")
        else {
            panic!("unassigned promise demand should be stably blocked")
        };
        let blocked_probe = client_demand_publish_lock_probe(&coordinator, &blocked);
        assert!(
            blocked
                .abandon_if_stably_blocked(subscription_epoch)
                .is_some()
        );
        assert_client_demand_published_after_unlock(&blocked_probe);

        let promise = PromisedValue::new(context.values(), "killable client input");
        let killed = context
            .demand_whnf(RuntimeValueRoot::new(
                context.values(),
                Value::Promised(promise),
            ))
            .expect("killable demand should be admitted");
        assert!(poll_one_runtime_work(&coordinator));
        let killed_probe = client_demand_publish_lock_probe(&coordinator, &killed);
        assert!(coordinator.kill_client_demand(
            killed.work(),
            Arc::new(EvaluationFailure::message("forced client disposition")),
        ));
        assert_client_demand_published_after_unlock(&killed_probe);
    }

    #[test]
    fn client_demand_exactly_restarts_after_promise_assignment() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        let promise = PromisedValue::new(context.values(), "client input");
        let handle = context
            .demand_whnf(RuntimeValueRoot::new(
                context.values(),
                Value::Promised(promise.clone()),
            ))
            .expect("promise demand should be admitted");

        assert!(poll_one_runtime_work(&coordinator));
        assert!(handle.poll().is_none());
        assert_eq!(promise.exact_subscription_count(), 1);

        let expected = Value::Number(7.into());
        promise
            .set(expected.clone())
            .expect("host promise should resolve once");
        assert_eq!(promise.exact_subscription_count(), 0);
        assert!(poll_one_runtime_work(&coordinator));
        assert!(matches!(
            handle.poll(),
            Some(ClientDemandResult::Complete(value)) if value.as_core() == &expected
        ));
    }

    #[test]
    fn abandoning_one_client_demand_preserves_another_exact_consumer() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        let promise = PromisedValue::new(context.values(), "shared client input");
        let root = RuntimeValueRoot::new(context.values(), Value::Promised(promise.clone()));
        let abandoned = context
            .demand_whnf(root.clone())
            .expect("first demand should be admitted");
        let survivor = context
            .demand_whnf(root)
            .expect("second demand should be admitted");

        assert!(poll_one_runtime_work(&coordinator));
        assert!(poll_one_runtime_work(&coordinator));
        assert_eq!(promise.exact_subscription_count(), 2);
        abandoned.abandon();
        assert_eq!(promise.exact_subscription_count(), 1);
        assert!(promise.assignment().is_none());

        let expected = Value::Number(11.into());
        promise
            .set(expected.clone())
            .expect("abandoning a consumer must not poison its producer");
        assert!(poll_one_runtime_work(&coordinator));
        assert!(matches!(
            survivor.poll(),
            Some(ClientDemandResult::Complete(value)) if value.as_core() == &expected
        ));
    }

    #[test]
    fn client_demand_can_follow_a_lazy_producer_owned_by_another_session() {
        let fixture = SameRuntimeFixture::new();
        let owner = fixture.context();
        let observer = fixture.context();
        let coordinator = owner.coordinator().expect("coordinator should be live");
        let promise = PromisedValue::new(owner.values(), "cross-session lazy input");
        let lazy = LazyValue::deferred(owner.values(), "cross-session client lazy", {
            let promise = promise.clone();
            move |context| crate::eval::eval_value(context, &Value::Promised(promise.clone()))
        });
        let root = RuntimeValueRoot::new(owner.values(), Value::Lazy(lazy.clone()));
        let owner_demand = owner
            .demand_whnf(root.clone())
            .expect("owner demand should be admitted");

        assert!(poll_one_runtime_work(&coordinator));
        assert!(owner_demand.poll().is_none());
        assert_eq!(promise.exact_subscription_count(), 1);

        let observer_demand = observer
            .demand_whnf(root)
            .expect("same-runtime observer should admit the shared value");
        assert!(poll_one_runtime_work(&coordinator));
        assert!(observer_demand.poll().is_none());
        assert_eq!(
            promise.exact_subscription_count(),
            1,
            "both clients should share the one lazy producer"
        );

        let expected = Value::Number(19.into());
        promise
            .set(expected.clone())
            .expect("cross-session input should resolve once");
        poll_runtime_until(&coordinator, || {
            owner_demand.poll().is_some() && observer_demand.poll().is_some()
        });
        for result in [owner_demand.poll(), observer_demand.poll()] {
            assert!(matches!(
                result,
                Some(ClientDemandResult::Complete(value)) if value.as_core() == &expected
            ));
        }
        assert!(lazy.cached().is_some_and(|result| result.is_ok()));
    }

    #[test]
    fn client_demand_result_cell_releases_after_terminal_handle_drop() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        let handle = context
            .demand_whnf(RuntimeValueRoot::new(
                context.values(),
                context.values().unit(),
            ))
            .expect("unit demand should be admitted");
        let result_cell = handle.result_cell();

        assert!(poll_one_runtime_work(&coordinator));
        assert!(result_cell.upgrade().is_some());
        drop(handle);
        assert!(result_cell.upgrade().is_none());
    }

    #[test]
    fn client_demand_owner_close_and_forced_kill_answer_once() {
        let fixture = SameRuntimeFixture::new();
        let (closed_handle, closed_promise) = {
            let owner = fixture.context();
            let promise = PromisedValue::new(owner.values(), "closing client input");
            let handle = owner
                .demand_whnf(RuntimeValueRoot::new(
                    owner.values(),
                    Value::Promised(promise.clone()),
                ))
                .expect("closing demand should be admitted");
            let coordinator = owner.coordinator().expect("coordinator should be live");
            assert!(poll_one_runtime_work(&coordinator));
            assert_eq!(promise.exact_subscription_count(), 1);
            (handle, promise)
        };
        assert_eq!(closed_promise.exact_subscription_count(), 0);
        assert_eq!(closed_handle.poll(), Some(ClientDemandResult::Abandoned));

        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        let promise = PromisedValue::new(context.values(), "killed client input");
        let handle = context
            .demand_whnf(RuntimeValueRoot::new(
                context.values(),
                Value::Promised(promise.clone()),
            ))
            .expect("killable demand should be admitted");
        assert!(poll_one_runtime_work(&coordinator));
        let failure = Arc::new(EvaluationFailure::message("forced client disposition"));
        assert!(coordinator.kill_client_demand(handle.work(), failure.clone()));
        assert!(!coordinator.kill_client_demand(handle.work(), failure.clone()));
        assert_eq!(promise.exact_subscription_count(), 0);
        assert!(matches!(
            handle.poll(),
            Some(ClientDemandResult::Killed(actual)) if Arc::ptr_eq(&actual, &failure)
        ));
    }

    #[test]
    fn synchronous_whnf_facade_preserves_retryable_promise_behavior() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        let promise = PromisedValue::new(context.values(), "synchronous client input");
        let promised = Value::Promised(promise.clone());

        let halt = context
            .evaluate_whnf(&promised)
            .expect_err("an unassigned host promise must remain retryable");
        assert_eq!(
            halt.unassigned_promise().map(PromisedValue::id),
            Some(promise.id())
        );
        assert_eq!(promise.exact_subscription_count(), 0);
        assert_eq!(coordinator.client_demand_count(), 0);

        let expected = Value::Number(23.into());
        promise
            .set(expected.clone())
            .expect("host promise should remain assignable after stable abandonment");
        assert_eq!(
            context
                .evaluate_whnf(&promised)
                .expect("resolved promise should complete synchronously"),
            expected
        );
        assert_eq!(coordinator.client_demand_count(), 0);
    }

    #[test]
    fn synchronous_client_demand_waits_for_worker_owned_runtime_progress() {
        let fixture = SameRuntimeFixture::new();
        fixture
            .runtime
            .activate_workers(1)
            .expect("test worker should activate");
        let producer = fixture.context();
        let consumer = fixture.context();
        let promise = PromisedValue::new(producer.values(), "worker-resolved client input");
        let expected = Value::Number(31.into());
        let (started, worker_started) = mpsc::channel();
        let (release, worker_release) = mpsc::channel();
        producer
            .schedule_task({
                let promise = promise.clone();
                let expected = expected.clone();
                move |_| {
                    Ok(Box::new(AssignPromiseAfterRelease {
                        promise,
                        value: expected,
                        started: Some(started),
                        release: worker_release,
                    }))
                }
            })
            .expect("promise producer should schedule");
        worker_started
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should claim the promise producer");

        let (completed, client_completed) = mpsc::channel();
        let client = std::thread::spawn(move || {
            completed
                .send(consumer.evaluate_whnf(&Value::Promised(promise)))
                .expect("client result receiver should remain live");
        });
        assert!(
            client_completed
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "client demand must not report a stable block while a worker owns progress"
        );
        release
            .send(())
            .expect("promise producer should remain live");
        assert_eq!(
            client_completed
                .recv_timeout(Duration::from_secs(2))
                .expect("worker completion should wake client demand")
                .expect("resolved client demand should succeed"),
            expected
        );
        client.join().expect("client thread should finish cleanly");
    }

    #[test]
    fn retained_client_handle_waits_across_external_disturbance_without_a_lost_wake() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        let promise = PromisedValue::new(context.values(), "parked client input");
        let handle = context
            .demand_whnf(RuntimeValueRoot::new(
                context.values(),
                Value::Promised(promise.clone()),
            ))
            .expect("retained demand should be admitted");
        assert!(poll_one_runtime_work(&coordinator));

        let parking = Arc::new(Barrier::new(2));
        let waiter = std::thread::spawn({
            let parking = parking.clone();
            move || {
                parking.wait();
                handle.wait()
            }
        });
        parking.wait();
        let expected = Value::Number(29.into());
        promise
            .set(expected.clone())
            .expect("external producer should resolve once");
        assert!(poll_one_runtime_work(&coordinator));
        assert!(matches!(
            waiter.join().expect("client waiter should finish"),
            ClientDemandResult::Complete(value) if value.as_core() == &expected
        ));
        assert_eq!(coordinator.client_demand_count(), 0);

        let already_complete = context
            .demand_whnf(RuntimeValueRoot::new(
                context.values(),
                context.values().unit(),
            ))
            .expect("unit demand should be admitted");
        assert!(poll_one_runtime_work(&coordinator));
        assert!(matches!(
            already_complete.wait(),
            ClientDemandResult::Complete(value) if value.as_core() == &context.values().unit()
        ));
    }

    #[test]
    fn generic_client_demand_resumes_composed_access_and_binary_annotation() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        let intermediate = PromisedValue::new(context.values(), "access intermediate");
        let byte = PromisedValue::new(context.values(), "binary byte");
        let root = Value::Dict(crate::core::Dict::new_sync().insert(
            crate::core::Key::atom_from_text("outer"),
            Value::Promised(intermediate.clone()),
        ));
        let outer = Value::Lazy(LazyValue::from_access(
            context.values(),
            Arc::from([crate::core_net::CoreDataKey::Key(
                crate::core::Key::atom_from_text("outer"),
            )]),
            Arc::from([root]),
        ));
        let member = Value::Lazy(LazyValue::from_access(
            context.values(),
            Arc::from([crate::core_net::CoreDataKey::Key(
                crate::core::Key::atom_from_text("member"),
            )]),
            Arc::from([outer]),
        ));
        let binary = Value::builtin_call(
            context.values(),
            crate::core::Builtin::Anno,
            vec![
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("binary"),
                )),
                member,
            ],
        );
        let handle = context
            .demand_whnf(RuntimeValueRoot::new(context.values(), binary))
            .expect("composed semantic demand should be admitted");

        assert!(poll_one_runtime_work(&coordinator));
        assert!(handle.poll().is_none());
        intermediate
            .set(Value::Dict(crate::core::Dict::new_sync().insert(
                crate::core::Key::atom_from_text("member"),
                Value::List(crate::core::List::from_values(vec![
                    Value::Number(1.into()),
                    Value::Promised(byte.clone()),
                ])),
            )))
            .expect("intermediate dictionary should resolve once");
        while poll_one_runtime_work(&coordinator) {}
        assert!(handle.poll().is_none());
        byte.set(Value::Number(2.into()))
            .expect("binary byte should resolve once");
        while poll_one_runtime_work(&coordinator) {}
        assert!(matches!(
            handle.poll(),
            Some(ClientDemandResult::Complete(value))
                if value.as_core() == &Value::Binary(bytes::Bytes::from_static(&[1, 2]))
        ));
    }

    #[test]
    fn isolated_context_reuses_and_can_replace_the_runtime_coordinator_binding() {
        let values = CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        );
        let first = EvalContext::isolated(values.clone());
        let shared = EvalContext::isolated(values.clone());
        let first_coordinator = first
            .coordinator()
            .expect("first coordinator should be live");
        let shared_coordinator = shared
            .coordinator()
            .expect("shared coordinator should be live");
        assert!(Arc::ptr_eq(&first_coordinator, &shared_coordinator));

        let expired = Arc::downgrade(&first_coordinator);
        drop(first_coordinator);
        drop(shared_coordinator);
        drop(first);
        drop(shared);
        assert!(expired.upgrade().is_none());

        let replacement = EvalContext::isolated(values.clone());
        let replacement_coordinator = replacement
            .coordinator()
            .expect("replacement coordinator should be live");
        assert!(Arc::ptr_eq(
            &replacement_coordinator,
            &values
                .work_coordinator()
                .expect("replacement coordinator should be bound")
        ));
    }

    #[test]
    fn escaped_context_retains_demand_resources_without_retaining_owner_or_coordinator() {
        let (coordinator, executor) =
            test_execution_resources(0).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let owner_weak = Arc::downgrade(&owner);
        let context = EvalContext::new(&owner);
        let demand = Arc::downgrade(&context.session);

        assert_eq!(Arc::strong_count(&owner), 1);
        drop(owner);
        assert!(owner_weak.upgrade().is_none());
        assert!(context.session.is_closed());

        drop(executor);
        drop(coordinator);
        assert!(context.coordinator().is_none());
        assert_eq!(context.values().unit(), crate::core::keys::unit_value());

        let closed_context = context.clone().for_effect_task();
        let error = PromisedValue::fixpoint(&closed_context, "closed demand promise")
            .expect_err("closed demand state must reject new promise admission");
        assert!(error.contains("closed"));

        drop(closed_context);
        drop(context);
        assert!(demand.upgrade().is_none());
    }

    #[test]
    fn owned_context_retains_its_direct_client_lease() {
        let (coordinator, _executor) =
            test_execution_resources(0).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let owner_weak = Arc::downgrade(&owner);
        let context = OwnedEvalContext::new(owner.clone());

        drop(owner);
        assert!(owner_weak.upgrade().is_some());
        assert!(!context.session.is_closed());
        assert_eq!(coordinator.registered_session_count(), 1);

        drop(context);
        assert!(owner_weak.upgrade().is_none());
        assert_eq!(coordinator.registered_session_count(), 0);
    }

    #[test]
    fn guarded_work_admission_rejects_a_closed_demand_without_an_owner_lease() {
        let (coordinator, _executor) =
            test_execution_resources(0).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let demand = owner.demand.clone();
        drop(owner);

        let task = allocate_task_id(&demand.values).expect("test task identity should allocate");
        let wait = allocate_wait_token(&demand, task).expect("test wait identity should allocate");
        let error = coordinator
            .reserve_reflection(&demand, task, wait)
            .expect_err("guarded admission must reject an already-closed demand");
        assert!(error.contains("closed"));
    }

    #[test]
    fn unregistered_nonterminal_wait_is_not_inferred_as_owner_abandonment() {
        let (coordinator, _executor) =
            test_execution_resources(0).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&owner);
        let task = allocate_task_id(context.values()).expect("test task identity should allocate");
        let wait = allocate_wait_token(&context.session, task)
            .expect("unregistered test wait should allocate");

        drop(owner);
        let EvaluationWaitPoll::Failed(failure) = context.poll_wait(&wait) else {
            panic!("an unregistered nonterminal wait must be an invariant failure");
        };
        assert_eq!(
            failure.to_string(),
            "evaluation wait token is no longer registered"
        );
    }

    #[test]
    fn blocked_machine_context_does_not_retain_its_owner_lease() {
        let (coordinator, _executor) =
            test_execution_resources(0).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let owner_weak = Arc::downgrade(&owner);
        let context = EvalContext::new(&owner);
        let task = context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .expect("blocked task should schedule");
        assert_eq!(
            context.pump_wait(task.wait(), 256),
            EvaluationPumpOutcome::NoProgress
        );

        drop(owner);
        assert!(owner_weak.upgrade().is_none());
        assert!(context.session.is_closed());
        assert!(matches!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Abandoned
        ));
        let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
            panic!("a closed demand must report completion without recovering its owner");
        };
        assert!(report.unfinished.is_empty());
        assert_eq!(coordinator.registered_session_count(), 0);
    }

    #[test]
    fn task_handle_acknowledges_terminal_failure_after_owner_lease_closes() {
        let (coordinator, _executor) =
            test_execution_resources(0).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let owner_weak = Arc::downgrade(&owner);
        let owner_id = owner.demand.id;
        let context = EvalContext::new(&owner);
        let demand_weak = Arc::downgrade(&context.session);
        let task = context
            .schedule_task(|_| Ok(Box::new(Fail)))
            .expect("failed task should schedule");

        assert!(matches!(
            context.run_until_quiescent(),
            EvaluationSessionRun::Complete(_)
        ));
        assert!(
            coordinator
                .failure_snapshot(owner_id)
                .contains_key(&task.id())
        );
        assert!(matches!(
            task.wait.terminal_poll(),
            Some(EvaluationWaitPoll::Failed(_))
        ));

        drop(context);
        drop(owner);
        assert!(owner_weak.upgrade().is_none());
        assert!(
            demand_weak.upgrade().is_none(),
            "a terminal task handle must not retain its former demand state"
        );
        assert_eq!(task.runtime_id(), coordinator.runtime_id());
        assert_eq!(task.session_id(), owner_id);

        task.acknowledge_propagated_failure();
        assert!(
            !coordinator
                .failure_snapshot(owner_id)
                .contains_key(&task.id()),
            "the handle reporting identity must route acknowledgement without recovering its owner"
        );
        assert!(matches!(
            task.wait.terminal_poll(),
            Some(EvaluationWaitPoll::Failed(_))
        ));
    }

    #[test]
    fn task_handle_cancellation_is_harmless_after_owner_closure() {
        let (coordinator, _executor) =
            test_execution_resources(0).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&owner);
        let task = context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .expect("blocked task should schedule");
        assert_eq!(
            context.pump_wait(task.wait(), 256),
            EvaluationPumpOutcome::NoProgress
        );

        drop(context);
        drop(owner);
        assert_eq!(
            task.wait.terminal_poll(),
            Some(EvaluationWaitPoll::Abandoned)
        );
        assert_eq!(
            task.cancel(),
            EvaluationTaskCancellation::Late,
            "owner closure must win atomically over a later cancellation request"
        );
    }

    #[test]
    fn running_machine_finishes_its_quantum_after_owner_drop_without_retaining_the_owner() {
        let (coordinator, _executor) =
            test_execution_resources(1).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let owner_weak = Arc::downgrade(&owner);
        let context = EvalContext::new(&owner);
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let task = context
            .schedule_task(move |_| {
                Ok(Box::new(CompleteAfterRelease {
                    started: Some(started_sender),
                    release: release_receiver,
                }))
            })
            .expect("running owner-drop task should schedule");
        started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should claim the task");
        assert_eq!(coordinator.registered_session_count(), 1);

        drop(owner);
        assert!(owner_weak.upgrade().is_none());
        assert!(context.session.is_closed());
        assert_eq!(coordinator.registered_session_count(), 0);
        assert!(
            context
                .schedule_task(|_| Ok(Box::new(Complete)))
                .expect_err("closed demand must reject later task admission")
                .contains("closed")
        );
        assert!(matches!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Pending(_)
        ));

        release_sender
            .send(())
            .expect("running machine should still own its current quantum");
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Pending(_)
        ) && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Abandoned,
            "owner closure must override the completed poll result"
        );
    }

    #[test]
    fn running_deferred_machine_is_coordinator_owned_after_owner_drop() {
        let (coordinator, _executor) =
            test_execution_resources(1).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let owner_weak = Arc::downgrade(&owner);
        let context = EvalContext::new(&owner);
        let lazy = inert_lazy_for(
            context.values(),
            "running coordinator-owned deferred machine",
        );
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let wait = context
            .lazy_task(&lazy, move |_| {
                Box::new(CompleteAfterRelease {
                    started: Some(started_sender),
                    release: release_receiver,
                })
            })
            .expect("running deferred owner-drop task should schedule");
        assert!(
            coordinator.promote_deferred_wait(&wait),
            "the test's explicit demand should make the deferred producer runnable"
        );
        started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should claim the deferred machine");

        drop(owner);
        assert!(owner_weak.upgrade().is_none());
        assert!(context.session.is_closed());
        assert_eq!(coordinator.registered_session_count(), 0);
        assert!(matches!(
            context.poll_wait(&wait),
            EvaluationWaitPoll::Pending(_)
        ));

        release_sender
            .send(())
            .expect("running deferred machine should finish its current quantum");
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(context.poll_wait(&wait), EvaluationWaitPoll::Pending(_))
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(context.poll_wait(&wait), EvaluationWaitPoll::Abandoned);
        while coordinator.deferred_counts(context.session.id) != (0, 0, 0)
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(coordinator.deferred_counts(context.session.id), (0, 0, 0));
    }

    struct Complete;

    impl EvaluationTaskMachine for Complete {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
        }
    }

    struct ExitVote(EvaluationExitBlock);

    impl EvaluationTaskMachine for ExitVote {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Exit(self.0.clone())
        }
    }

    struct ExitUntilObservation {
        context: EvalContext,
        observed: RuntimeObservationEpoch,
    }

    impl EvaluationTaskMachine for ExitUntilObservation {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            if self.context.current_observation_epoch() == self.observed {
                EvaluationMachinePoll::Exit(EvaluationExitBlock {
                    intent: ExitIntent::Success,
                    observed_epoch: Some(self.observed),
                })
            } else {
                EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
            }
        }
    }

    #[derive(Default)]
    struct RecordedStatuses(Mutex<Vec<EvaluationTaskStatus>>);

    impl RecordedStatuses {
        fn publisher(statuses: &Arc<Self>) -> TaskStatusPublisher {
            let statuses = statuses.clone();
            TaskStatusPublisher::new(move |_mutation, status| {
                let statuses = statuses.clone();
                TaskStatusWake::new(move || {
                    statuses
                        .0
                        .lock()
                        .expect("recorded task statuses were poisoned")
                        .push(status);
                })
            })
        }
    }

    #[test]
    fn exit_wait_does_not_publish_task_status_or_failure() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context
            .coordinator()
            .expect("test task must retain its coordinator");
        let message = RuntimeValueRoot::new(context.values(), crate::core::keys::unit_value());
        let task = context
            .schedule_task(move |_| {
                Ok(Box::new(ExitVote(EvaluationExitBlock {
                    intent: ExitIntent::Error(message),
                    observed_epoch: None,
                })))
            })
            .expect("exit-voting task should schedule");
        let statuses = Arc::new(RecordedStatuses::default());
        assert!(
            coordinator.attach_reflection_status_publisher(
                task.work,
                RecordedStatuses::publisher(&statuses),
            )
        );

        let EvaluationSessionRun::Deadlocked(report) = context.run_until_quiescent() else {
            panic!("a permanent exit vote should remain unfinished")
        };
        assert!(report.failures.is_empty());
        assert_eq!(report.unfinished.len(), 1);
        assert_eq!(
            report.unfinished[0].state,
            EvaluationUnfinishedState::Blocked
        );
        assert!(report.unfinished[0].error.is_none());
        assert!(statuses.0.lock().unwrap().is_empty());
        assert!(matches!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Pending(_)
        ));
        assert!(coordinator.failure_snapshot(context.session.id).is_empty());

        task.acknowledge_failure();
        assert!(coordinator.failure_snapshot(context.session.id).is_empty());
        assert!(matches!(
            coordinator
                .reflection_snapshots(context.session.id)
                .as_slice(),
            [ReflectionWorkSnapshot {
                state: ReflectionWorkState::ExitWaiting(EvaluationExitBlock {
                    intent: ExitIntent::Error(_),
                    observed_epoch: None,
                }),
                ..
            }]
        ));

        assert_eq!(task.cancel(), EvaluationTaskCancellation::Requested);
    }

    #[test]
    fn retryable_exit_wake_skips_intermediate_task_statuses() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context
            .coordinator()
            .expect("test task must retain its coordinator");
        let observed = context.current_observation_epoch();
        let task = context
            .schedule_task(|task_context| {
                Ok(Box::new(ExitUntilObservation {
                    context: task_context,
                    observed,
                }))
            })
            .expect("retryable exit task should schedule");
        let statuses = Arc::new(RecordedStatuses::default());
        assert!(
            coordinator.attach_reflection_status_publisher(
                task.work,
                RecordedStatuses::publisher(&statuses),
            )
        );

        assert!(matches!(
            context.run_until_quiescent(),
            EvaluationSessionRun::Deadlocked(_)
        ));
        assert!(statuses.0.lock().unwrap().is_empty());

        coordinator.publish_runtime_observation();
        let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
            panic!("observation wake should resume and complete the exit voter")
        };
        assert!(report.failures.is_empty());
        assert!(report.unfinished.is_empty());
        assert!(matches!(
            statuses.0.lock().unwrap().as_slice(),
            [EvaluationTaskStatus::Complete(_)]
        ));
    }

    #[test]
    fn task_join_dependency_remains_pending_for_exit_waiting_child() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let child = context
            .schedule_task(|_| {
                Ok(Box::new(ExitVote(EvaluationExitBlock {
                    intent: ExitIntent::Success,
                    observed_epoch: None,
                })))
            })
            .expect("exit-voting child should schedule");
        let child_wait = child.wait().clone();
        let parent = context
            .schedule_task(move |task_context| {
                Ok(Box::new(Await {
                    context: task_context,
                    dependency: child_wait,
                }))
            })
            .expect("joining parent should schedule");

        let EvaluationSessionRun::Deadlocked(report) = context.run_until_quiescent() else {
            panic!("a join on an exit-waiting child must remain pending")
        };
        assert!(report.failures.is_empty());
        assert_eq!(report.unfinished.len(), 2);
        assert!(matches!(
            context.poll_reflection_task(&child),
            EvaluationWaitPoll::Pending(_)
        ));
        assert!(matches!(
            context.poll_reflection_task(&parent),
            EvaluationWaitPoll::Pending(_)
        ));
        let crate::api::RuntimeReadiness::Deadlocked(snapshot) = fixture.runtime.readiness() else {
            panic!("strict join on an exit voter should remain a runtime deadlock")
        };
        assert_eq!(snapshot.dispositions().len(), 1);
        assert_eq!(snapshot.dispositions()[0].task_id(), Some(child.id().get()));
        assert_eq!(snapshot.unfinished().len(), 1);
        assert_eq!(snapshot.unfinished()[0].task_id(), Some(parent.id().get()));
        assert!(matches!(
            snapshot.unfinished()[0].dependency(),
            Some(crate::api::RuntimeDependency::TaskWait { task_id, .. })
                if *task_id == child.id().get()
        ));

        assert_eq!(child.cancel(), EvaluationTaskCancellation::Requested);
        let _ = context.run_until_quiescent();
    }

    struct Await {
        context: EvalContext,
        dependency: EvaluationWaitToken,
    }

    impl EvaluationTaskMachine for Await {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            match self.context.poll_wait(&self.dependency) {
                EvaluationWaitPoll::Pending(wait) => {
                    EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                        dependency: Some(WorkDependency::Wait(wait)),
                        observed_epoch: None,
                        error: None,
                    })
                }
                EvaluationWaitPoll::Complete(value) => EvaluationMachinePoll::Complete(value),
                EvaluationWaitPoll::Failed(error) => self
                    .context
                    .lazy_failure_for_wait(&self.dependency)
                    .map(EvaluationMachinePoll::Failed)
                    .unwrap_or(EvaluationMachinePoll::Failed(error)),
                EvaluationWaitPoll::Cancelled => EvaluationMachinePoll::Cancelled,
                EvaluationWaitPoll::Abandoned => EvaluationMachinePoll::Failed(evaluation_failure(
                    "waited-on task was abandoned",
                )),
                EvaluationWaitPoll::Exited => EvaluationMachinePoll::Failed(evaluation_failure(
                    "waited-on task exited without a result",
                )),
                EvaluationWaitPoll::Killed(error) => EvaluationMachinePoll::Failed(error),
            }
        }
    }

    struct AwaitPromise {
        promise: PromisedValue,
    }

    impl EvaluationTaskMachine for AwaitPromise {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            match self.promise.assignment() {
                None => EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    dependency: Some(WorkDependency::Promise(self.promise.clone())),
                    observed_epoch: None,
                    error: None,
                }),
                Some(Ok(value)) => EvaluationMachinePoll::Complete(value),
                Some(Err(error)) => EvaluationMachinePoll::Failed(error),
            }
        }
    }

    struct AwaitCell {
        context: EvalContext,
        dependency: Arc<OnceLock<EvaluationWaitToken>>,
    }

    impl EvaluationTaskMachine for AwaitCell {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            let dependency = self
                .dependency
                .get()
                .expect("test dependency must be installed before polling");
            match self.context.poll_wait(dependency) {
                EvaluationWaitPoll::Pending(wait) => {
                    EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                        dependency: Some(WorkDependency::Wait(wait)),
                        observed_epoch: None,
                        error: None,
                    })
                }
                EvaluationWaitPoll::Complete(value) => EvaluationMachinePoll::Complete(value),
                EvaluationWaitPoll::Failed(error) => self
                    .context
                    .lazy_failure_for_wait(dependency)
                    .map(EvaluationMachinePoll::Failed)
                    .unwrap_or(EvaluationMachinePoll::Failed(error)),
                EvaluationWaitPoll::Cancelled => EvaluationMachinePoll::Cancelled,
                EvaluationWaitPoll::Abandoned => EvaluationMachinePoll::Failed(evaluation_failure(
                    "waited-on task was abandoned",
                )),
                EvaluationWaitPoll::Exited => EvaluationMachinePoll::Failed(evaluation_failure(
                    "waited-on task exited without a result",
                )),
                EvaluationWaitPoll::Killed(error) => EvaluationMachinePoll::Failed(error),
            }
        }
    }

    fn inert_lazy(label: &'static str) -> LazyValue {
        inert_lazy_for(&crate::core::test_value_factory(), label)
    }

    fn inert_lazy_for(values: &CoreValueFactory, label: &'static str) -> LazyValue {
        LazyValue::deferred(values, label, |_| {
            panic!("scheduler cycle fixtures must use their installed test machine")
        })
    }

    fn register_lazy_await(
        context: &EvalContext,
        lazy: &LazyValue,
        dependency: Arc<OnceLock<EvaluationWaitToken>>,
    ) -> EvaluationWaitToken {
        context
            .lazy_task(lazy, move |task_context| {
                Box::new(AwaitCell {
                    context: task_context,
                    dependency,
                })
            })
            .expect("test lazy task should register")
    }

    fn register_promise_await(
        context: &EvalContext,
        promise: &PromisedValue,
        dependency: Arc<OnceLock<EvaluationWaitToken>>,
    ) -> EvaluationWaitToken {
        context
            .promise_task(promise, move |task_context| {
                Box::new(AwaitCell {
                    context: task_context,
                    dependency,
                })
            })
            .expect("test promise task should register")
    }

    fn assert_deferred_task_retired(context: &EvalContext, _lazy: &LazyValue) {
        let counts = context
            .coordinator()
            .expect("test coordinator should be live")
            .deferred_counts(context.session.id);
        assert_eq!(counts, (0, 0, 0), "coordinator indexes must be retired");
    }

    fn dependency_cycle(lazy: &LazyValue) -> Arc<LazyCycle> {
        lazy.cached()
            .and_then(Result::err)
            .expect("test lazy should have a structured failure")
            .dependency_cycle_value()
            .cloned()
            .expect("test lazy failure should be a dependency cycle")
    }

    struct AlwaysBlocked;

    impl EvaluationTaskMachine for AlwaysBlocked {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                dependency: None,
                observed_epoch: Some(RuntimeObservationEpoch::from_raw(7)),
                error: Some(Arc::new(EvaluationFailure::message(
                    "retryable evaluation error",
                ))),
            })
        }
    }

    struct CountedBlocked {
        polls: Arc<Mutex<usize>>,
    }

    impl EvaluationTaskMachine for CountedBlocked {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            *self
                .polls
                .lock()
                .expect("blocked-task poll count was poisoned") += 1;
            EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                dependency: None,
                observed_epoch: Some(RuntimeObservationEpoch::from_raw(7)),
                error: None,
            })
        }
    }

    struct AlwaysYields;

    impl EvaluationTaskMachine for AlwaysYields {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Yielded
        }
    }

    struct RecordPollOrder {
        label: u8,
        polls: Arc<Mutex<Vec<u8>>>,
        yield_once: bool,
    }

    impl EvaluationTaskMachine for RecordPollOrder {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            self.polls
                .lock()
                .expect("task-order trace was poisoned")
                .push(self.label);
            if std::mem::take(&mut self.yield_once) {
                EvaluationMachinePoll::Yielded
            } else {
                EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
            }
        }
    }

    struct Fail;

    impl EvaluationTaskMachine for Fail {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Failed(evaluation_failure("reasoning failed"))
        }
    }

    struct Signal(Option<mpsc::Sender<()>>);

    impl EvaluationTaskMachine for Signal {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            if let Some(signal) = self.0.take() {
                signal.send(()).expect("test receiver should remain open");
            }
            EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
        }
    }

    struct SpawnSignal {
        target: EvalContext,
        signal: Option<mpsc::Sender<()>>,
    }

    impl EvaluationTaskMachine for SpawnSignal {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            let signal = self
                .signal
                .take()
                .expect("spawning test task must be polled only once");
            self.target
                .schedule_task(move |_| Ok(Box::new(Signal(Some(signal)))))
                .expect("runtime pump should permit cross-session task admission");
            EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
        }
    }

    struct CompleteAfterRelease {
        started: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    impl EvaluationTaskMachine for CompleteAfterRelease {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            if let Some(started) = self.started.take() {
                started
                    .send(())
                    .expect("test start receiver should remain open");
            }
            self.release
                .recv_timeout(Duration::from_secs(2))
                .expect("test should release the task");
            EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
        }
    }

    struct AssignPromiseAfterRelease {
        promise: PromisedValue,
        value: Value,
        started: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    impl EvaluationTaskMachine for AssignPromiseAfterRelease {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            if let Some(started) = self.started.take() {
                started
                    .send(())
                    .expect("test start receiver should remain open");
            }
            self.release
                .recv_timeout(Duration::from_secs(2))
                .expect("test should release the promise producer");
            self.promise
                .set(self.value.clone())
                .expect("worker should resolve the host promise once");
            EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
        }
    }

    struct FailAfterRelease {
        started: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    impl EvaluationTaskMachine for FailAfterRelease {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            if let Some(started) = self.started.take() {
                started
                    .send(())
                    .expect("test start receiver should remain open");
            }
            self.release
                .recv_timeout(Duration::from_secs(2))
                .expect("test should release the task");
            EvaluationMachinePoll::Failed(evaluation_failure("acknowledged task failure"))
        }
    }

    struct CancellableAfterRelease {
        started: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
        cancelled: Arc<AtomicBool>,
    }

    impl EvaluationTaskMachine for CancellableAfterRelease {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            if let Some(started) = self.started.take() {
                started
                    .send(())
                    .expect("test start receiver should remain open");
            }
            self.release
                .recv_timeout(Duration::from_secs(2))
                .expect("test should release the task");
            EvaluationMachinePoll::Failed(evaluation_failure(
                "cancellation should replace this poll result",
            ))
        }

        fn cancel(&mut self) {
            self.cancelled.store(true, Ordering::Release);
        }
    }

    struct AssignPromiseThenYield {
        promise: Option<PromisedValue>,
        assigned: Option<mpsc::Sender<()>>,
    }

    impl EvaluationTaskMachine for AssignPromiseThenYield {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            let promise = self
                .promise
                .take()
                .expect("assignment fixture should publish exactly once");
            promise
                .set(crate::core::keys::unit_value())
                .expect("the owning machine should assign its promise once");
            self.assigned
                .take()
                .expect("assignment fixture should signal exactly once")
                .send(())
                .expect("assignment observer should remain live");
            EvaluationMachinePoll::Yielded
        }
    }

    struct CompleteAndSignalDrop {
        dropped: Arc<AtomicBool>,
    }

    impl EvaluationTaskMachine for CompleteAndSignalDrop {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
        }
    }

    impl Drop for CompleteAndSignalDrop {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    struct CompleteAndCheckReflectionDrop {
        context: EvalContext,
        dropped_without_registry_lock: Arc<AtomicBool>,
    }

    struct CompleteAndCheckTerminalPublication {
        status_authoritative: Arc<AtomicBool>,
        status_notified: Arc<AtomicBool>,
        dropped_after_publication: Arc<AtomicBool>,
    }

    impl EvaluationTaskMachine for CompleteAndCheckTerminalPublication {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
        }
    }

    impl Drop for CompleteAndCheckTerminalPublication {
        fn drop(&mut self) {
            self.dropped_after_publication.store(
                self.status_authoritative.load(Ordering::Acquire)
                    && self.status_notified.load(Ordering::Acquire),
                Ordering::Release,
            );
        }
    }

    impl EvaluationTaskMachine for CompleteAndCheckReflectionDrop {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
        }
    }

    impl Drop for CompleteAndCheckReflectionDrop {
        fn drop(&mut self) {
            let runtime_unlocked = self
                .context
                .coordinator()
                .is_none_or(|coordinator| coordinator.runtime_locks_are_free());
            self.dropped_without_registry_lock
                .store(runtime_unlocked, Ordering::Release);
        }
    }

    struct CacheLazyFailure {
        lazy: LazyValue,
        failure: Arc<EvaluationFailure>,
    }

    impl EvaluationTaskMachine for CacheLazyFailure {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            match self.lazy.cache(Err(self.failure.clone())) {
                Ok(value) => EvaluationMachinePoll::Complete(value.into_value()),
                Err(error) => EvaluationMachinePoll::Failed(error),
            }
        }
    }

    struct SpawnOnce {
        context: EvalContext,
        spawned: bool,
    }

    impl EvaluationTaskMachine for SpawnOnce {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            if !self.spawned {
                self.spawned = true;
                self.context
                    .schedule_task(|_| Ok(Box::new(Complete)))
                    .expect("child should schedule while its parent is polled");
            }
            EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
        }
    }

    struct Cancellable {
        cancelled: Arc<AtomicBool>,
    }

    impl EvaluationTaskMachine for Cancellable {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Yielded
        }

        fn cancel(&mut self) {
            self.cancelled.store(true, Ordering::Release);
        }
    }

    #[test]
    fn terminal_waits_retain_runtime_root_provenance() {
        let context = EvalContext::standalone();
        let task = context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("task should schedule");
        assert!(matches!(
            context.pump_wait(&task.wait, 256),
            EvaluationPumpOutcome::TargetReady
        ));
        let Some(EvaluationWaitTerminal::Complete(value)) = task.wait.0.terminal.get() else {
            panic!("completed wait should retain a terminal value")
        };
        assert_eq!(value.runtime_id(), context.values().runtime_id());
        assert!(matches!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Complete(_)
        ));
    }

    #[test]
    fn pump_follows_a_lazy_dependency_to_its_producer() {
        let context = EvalContext::standalone();
        let dependency = context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("dependency should schedule");
        let dependency_wait = dependency.wait.clone();
        let target = context
            .schedule_task(move |task_context| {
                Ok(Box::new(Await {
                    context: task_context,
                    dependency: dependency_wait,
                }))
            })
            .expect("dependent task should schedule");

        assert_eq!(
            context.pump_wait(&target.wait, 256),
            EvaluationPumpOutcome::TargetReady
        );
        assert!(matches!(
            context.poll_reflection_task(&dependency),
            EvaluationWaitPoll::Complete(_)
        ));
        assert!(matches!(
            context.poll_reflection_task(&target),
            EvaluationWaitPoll::Complete(_)
        ));
    }

    #[test]
    fn completed_deferred_tasks_release_their_machines() {
        let context = EvalContext::standalone();
        let lazy = inert_lazy("terminal machine");
        let dropped = Arc::new(AtomicBool::new(false));
        let wait = context
            .lazy_task(&lazy, {
                let dropped = dropped.clone();
                move |_| Box::new(CompleteAndSignalDrop { dropped })
            })
            .expect("test lazy task should register");

        assert_eq!(
            context.pump_wait(&wait, 256),
            EvaluationPumpOutcome::TargetReady
        );
        assert!(
            dropped.load(Ordering::Acquire),
            "a completed deferred task must drop its machine"
        );
        assert_deferred_task_retired(&context, &lazy);
        assert_eq!(
            context.task_registry_counts(),
            EvaluationTaskRegistryCounts {
                reflection_active: 0,
                reflection_terminal: 0,
                reflection_by_id: 0,
                unacknowledged_failures: 0,
                deferred_active: 0,
                deferred_terminal: 0,
                deferred_by_wait: 0,
                deferred_by_task: 0,
                promises_active: 0,
                promises_terminal: 0,
                owned_promise_waits: 0,
            },
            "the terminal deferred task and all indexes must be retired"
        );
    }

    #[test]
    fn redundant_deferred_registration_observes_the_canonical_lazy_cache() {
        let context = EvalContext::standalone();
        let lazy = inert_lazy("redundant registration");
        let failure = evaluation_failure("canonical lazy failure");
        let (build_started_sender, build_started_receiver) = mpsc::channel();
        let (release_build_sender, release_build_receiver) = mpsc::channel();

        let redundant_registration = {
            let context = context.clone();
            let lazy = lazy.clone();
            let failure = failure.clone();
            std::thread::spawn(move || {
                let machine_lazy = lazy.clone();
                context
                    .lazy_task(&lazy, move |_| {
                        build_started_sender
                            .send(())
                            .expect("test build observer should remain open");
                        release_build_receiver
                            .recv_timeout(Duration::from_secs(2))
                            .expect("test should release the redundant registration");
                        Box::new(CacheLazyFailure {
                            lazy: machine_lazy,
                            failure,
                        })
                    })
                    .expect("redundant lazy task should register")
            })
        };
        build_started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("redundant registration should pass its initial lookup");

        let canonical_wait = context
            .lazy_task(&lazy, {
                let lazy = lazy.clone();
                let failure = failure.clone();
                move |_| Box::new(CacheLazyFailure { lazy, failure })
            })
            .expect("canonical lazy task should register");
        assert_eq!(
            context.pump_wait(&canonical_wait, 256),
            EvaluationPumpOutcome::TargetReady
        );
        assert_deferred_task_retired(&context, &lazy);

        release_build_sender
            .send(())
            .expect("redundant registration should still be waiting");
        let redundant_wait = redundant_registration
            .join()
            .expect("redundant registration should not panic");
        assert_ne!(
            canonical_wait, redundant_wait,
            "the raced registration should install a fresh active record"
        );
        assert_eq!(
            context.task_registry_counts().deferred_active,
            1,
            "the redundant task should exist only until it observes the cache"
        );
        assert_eq!(
            context.pump_wait(&redundant_wait, 256),
            EvaluationPumpOutcome::TargetReady
        );
        assert_deferred_task_retired(&context, &lazy);

        let EvaluationWaitPoll::Failed(canonical_failure) = context.poll_wait(&canonical_wait)
        else {
            panic!("canonical wait should retain the lazy failure");
        };
        let EvaluationWaitPoll::Failed(redundant_failure) = context.poll_wait(&redundant_wait)
        else {
            panic!("redundant wait should retain the lazy failure");
        };
        assert!(Arc::ptr_eq(&canonical_failure, &redundant_failure));
        assert!(Arc::ptr_eq(&canonical_failure, &failure));
    }

    #[test]
    fn terminal_reflection_handles_preserve_late_polling_without_records() {
        let context = EvalContext::standalone();
        let complete = context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("completed task should schedule");
        let failed = context
            .schedule_task(|_| Ok(Box::new(Fail)))
            .expect("failed task should schedule");
        let cancelled = context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("cancelled task should schedule");
        for wait in [complete.wait(), failed.wait(), cancelled.wait()] {
            assert_eq!(
                wait.subscribe_test_work(),
                CompletionSubscriptionOutcome::Pending
            );
            assert_eq!(wait.exact_subscription_count(), 1);
        }
        assert_eq!(cancelled.cancel(), EvaluationTaskCancellation::Requested);
        assert_eq!(cancelled.wait().exact_subscription_count(), 0);

        let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
            panic!("terminal tasks should leave no unfinished work");
        };
        assert_eq!(report.failures.size(), 1);
        assert!(report.failures.contains_key(&failed.id()));
        assert_eq!(complete.wait().exact_subscription_count(), 0);
        assert_eq!(failed.wait().exact_subscription_count(), 0);
        assert_eq!(
            complete.wait().subscribe_test_work(),
            CompletionSubscriptionOutcome::AlreadyTerminal,
            "a late exact subscription must observe the immutable terminal"
        );
        assert_eq!(complete.wait().exact_subscription_count(), 0);

        for _ in 0..2 {
            assert!(matches!(
                context.poll_reflection_task(&complete),
                EvaluationWaitPoll::Complete(_)
            ));
            assert!(matches!(
                context.poll_reflection_task(&failed),
                EvaluationWaitPoll::Failed(error)
                    if error.to_string() == "reasoning failed"
            ));
            assert_eq!(
                context.poll_reflection_task(&cancelled),
                EvaluationWaitPoll::Cancelled
            );
        }

        assert_eq!(
            context.task_registry_counts(),
            EvaluationTaskRegistryCounts {
                reflection_active: 0,
                reflection_terminal: 0,
                reflection_by_id: 0,
                unacknowledged_failures: 1,
                deferred_active: 0,
                deferred_terminal: 0,
                deferred_by_wait: 0,
                deferred_by_task: 0,
                promises_active: 0,
                promises_terminal: 0,
                owned_promise_waits: 0,
            },
            "terminal handles retain outcomes while only unacknowledged failures remain indexed"
        );
    }

    #[test]
    fn wait_completion_subscriptions_reject_a_foreign_runtime() {
        let owner_fixture = SameRuntimeFixture::new();
        let foreign_fixture = SameRuntimeFixture::new();
        let owner = owner_fixture.context();
        let task = owner
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .expect("pending task should schedule");

        assert_eq!(
            task.wait()
                .subscribe_work(foreign_fixture.runtime.id(), test_wake_registration()),
            CompletionSubscriptionOutcome::ForeignRuntime
        );
        assert_eq!(task.wait().exact_subscription_count(), 0);
    }

    #[test]
    fn terminal_reflection_machines_drop_after_releasing_runtime_locks() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let dropped_without_registry_lock = Arc::new(AtomicBool::new(false));
        let task = context
            .schedule_task({
                let dropped_without_registry_lock = dropped_without_registry_lock.clone();
                move |task_context| {
                    Ok(Box::new(CompleteAndCheckReflectionDrop {
                        context: task_context,
                        dropped_without_registry_lock,
                    }))
                }
            })
            .expect("reflection task should schedule");

        assert_eq!(
            context.pump_wait(task.wait(), 256),
            EvaluationPumpOutcome::TargetReady
        );
        assert!(
            dropped_without_registry_lock.load(Ordering::Acquire),
            "terminal reflection machine must be destroyed without runtime locks"
        );
        assert_eq!(
            context.task_registry_counts(),
            EvaluationTaskRegistryCounts {
                reflection_active: 0,
                reflection_terminal: 0,
                reflection_by_id: 0,
                unacknowledged_failures: 0,
                deferred_active: 0,
                deferred_terminal: 0,
                deferred_by_wait: 0,
                deferred_by_task: 0,
                promises_active: 0,
                promises_terminal: 0,
                owned_promise_waits: 0,
            },
            "the terminal record must be retired after its machine is dropped"
        );
    }

    #[test]
    fn terminal_status_is_authoritative_and_notified_before_machine_retirement() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context
            .coordinator()
            .expect("test task must retain its coordinator");
        let status_authoritative = Arc::new(AtomicBool::new(false));
        let status_notified = Arc::new(AtomicBool::new(false));
        let dropped_after_publication = Arc::new(AtomicBool::new(false));
        let task = context
            .schedule_task({
                let status_authoritative = status_authoritative.clone();
                let status_notified = status_notified.clone();
                let dropped_after_publication = dropped_after_publication.clone();
                move |_| {
                    Ok(Box::new(CompleteAndCheckTerminalPublication {
                        status_authoritative,
                        status_notified,
                        dropped_after_publication,
                    }))
                }
            })
            .expect("reflection task should schedule");
        let publisher = TaskStatusPublisher::new({
            let coordinator = coordinator.clone();
            let status_authoritative = status_authoritative.clone();
            let status_notified = status_notified.clone();
            move |_mutation, status| {
                assert!(matches!(status, EvaluationTaskStatus::Complete(_)));
                status_authoritative.store(true, Ordering::Release);
                let coordinator = coordinator.clone();
                let status_notified = status_notified.clone();
                TaskStatusWake::new(move || {
                    assert!(
                        coordinator.runtime_locks_are_free(),
                        "status observers must run after runtime mutation admission"
                    );
                    status_notified.store(true, Ordering::Release);
                })
            }
        });
        assert!(coordinator.attach_reflection_status_publisher(task.work, publisher));

        assert_eq!(
            context.pump_wait(task.wait(), 256),
            EvaluationPumpOutcome::TargetReady
        );
        assert!(status_authoritative.load(Ordering::Acquire));
        assert!(status_notified.load(Ordering::Acquire));
        assert!(
            dropped_after_publication.load(Ordering::Acquire),
            "terminal work must not retire its machine before status publication"
        );
    }

    #[test]
    fn terminal_wait_tokens_outlive_their_owner_session() {
        let fixture = SameRuntimeFixture::new();
        let (completed_wait, failed_wait, cancelled_wait) = {
            let owner = fixture.context();
            let completed = owner
                .schedule_task(|_| Ok(Box::new(Complete)))
                .expect("completed task should schedule");
            let failed = owner
                .schedule_task(|_| Ok(Box::new(Fail)))
                .expect("failed task should schedule");
            let cancelled = owner
                .schedule_task(|_| Ok(Box::new(Complete)))
                .expect("cancelled task should schedule");
            assert_eq!(cancelled.cancel(), EvaluationTaskCancellation::Requested);
            assert!(matches!(
                owner.run_until_quiescent(),
                EvaluationSessionRun::Complete(_)
            ));
            (
                completed.wait().clone(),
                failed.wait().clone(),
                cancelled.wait().clone(),
            )
        };
        let deferred_wait = {
            let owner = fixture.context();
            let lazy = LazyValue::deferred(owner.values(), "owner lifetime", |_| {
                panic!("the terminal wait fixture supplies its own task machine")
            });
            let wait = owner
                .lazy_task(&lazy, |_| Box::new(Complete))
                .expect("deferred task should schedule");
            assert_eq!(
                owner.pump_wait(&wait, 256),
                EvaluationPumpOutcome::TargetReady
            );
            wait
        };
        let pending_wait = {
            let owner = fixture.context();
            owner
                .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
                .expect("pending task should schedule")
                .wait()
                .clone()
        };
        let observer = fixture.context();

        assert!(matches!(
            observer.poll_wait(&completed_wait),
            EvaluationWaitPoll::Complete(_)
        ));
        assert!(matches!(
            observer.poll_wait(&deferred_wait),
            EvaluationWaitPoll::Complete(_)
        ));
        assert!(matches!(
            observer.poll_wait(&failed_wait),
            EvaluationWaitPoll::Failed(error) if error.to_string() == "reasoning failed"
        ));
        assert_eq!(
            observer.poll_wait(&cancelled_wait),
            EvaluationWaitPoll::Cancelled
        );
        assert_eq!(
            observer.pump_wait(&completed_wait, 1),
            EvaluationPumpOutcome::TargetReady
        );
        assert_eq!(
            observer.poll_wait(&pending_wait),
            EvaluationWaitPoll::Abandoned
        );
    }

    #[test]
    fn owner_session_drop_publishes_task_abandonment_and_status() {
        let fixture = SameRuntimeFixture::new();
        let statuses = Arc::new(RecordedStatuses::default());
        let task = {
            let owner = fixture.context();
            let task = owner
                .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
                .expect("abandoned task should schedule");
            assert_eq!(
                task.wait().subscribe_test_work(),
                CompletionSubscriptionOutcome::Pending
            );
            assert!(
                owner
                    .coordinator()
                    .expect("scheduled task must retain its coordinator")
                    .attach_reflection_status_publisher(
                        task.work,
                        RecordedStatuses::publisher(&statuses),
                    )
            );
            task
        };
        let observer = fixture.context();

        assert_eq!(
            observer.poll_reflection_task(&task),
            EvaluationWaitPoll::Abandoned
        );
        assert_eq!(task.wait().exact_subscription_count(), 0);
        assert_eq!(
            statuses
                .0
                .lock()
                .expect("recorded task statuses were poisoned")
                .as_slice(),
            [EvaluationTaskStatus::Abandoned]
        );
    }

    #[test]
    fn internal_reflection_tasks_do_not_manufacture_status_queries() {
        let context = EvalContext::standalone();
        let task = context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .expect("internal task should schedule");
        let coordinator = context
            .coordinator()
            .expect("internal task should retain its coordinator");
        assert!(
            !coordinator.task_has_status_publisher(task.id()),
            "a task without a public handle should not allocate a status publisher"
        );
        assert_eq!(task.cancel(), EvaluationTaskCancellation::Requested);
    }

    #[test]
    fn owner_session_drop_exactly_wakes_a_cross_session_task_waiter() {
        let fixture = SameRuntimeFixture::new();
        let observer = fixture.context();
        let (dependency, follower) = {
            let owner = fixture.context();
            let dependency = owner
                .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
                .expect("abandoned dependency should schedule");
            let dependency_wait = dependency.wait.clone();
            let follower = observer
                .schedule_task(move |task_context| {
                    Ok(Box::new(Await {
                        context: task_context,
                        dependency: dependency_wait,
                    }))
                })
                .expect("cross-session follower should schedule");

            let EvaluationSessionRun::Quiescent(report) = observer.run_until_quiescent() else {
                panic!("the live owner should leave its cross-session follower quiescent")
            };
            assert_eq!(report.unfinished.len(), 1);
            assert_eq!(dependency.wait().exact_subscription_count(), 1);
            (dependency, follower)
        };

        assert_eq!(dependency.wait().exact_subscription_count(), 0);
        assert_eq!(
            observer
                .coordinator()
                .expect("observer coordinator should be live")
                .ready_task_count(),
            1
        );
        let EvaluationSessionRun::Complete(report) = observer.run_until_quiescent() else {
            panic!("owner abandonment should wake and terminalize the follower")
        };
        assert!(report.failures.contains_key(&follower.id()));
    }

    #[test]
    fn abandoned_lazy_claim_can_be_reclaimed_without_poisoning_the_lazy() {
        let fixture = SameRuntimeFixture::new();
        let forced = Arc::new(AtomicBool::new(false));
        let (lazy, abandoned_wait, expected) = {
            let owner = fixture.context();
            let expected = owner.values().unit();
            let lazy = LazyValue::deferred(owner.values(), "reclaimable lazy", {
                let forced = forced.clone();
                let expected = expected.clone();
                move |_| {
                    forced.store(true, Ordering::Release);
                    Ok(expected.clone())
                }
            });
            let wait = owner
                .lazy_task(&lazy, |_| Box::new(AlwaysBlocked))
                .expect("first lazy claim should register");
            (lazy, wait, expected)
        };
        let observer = fixture.context();

        assert_eq!(
            observer.poll_wait(&abandoned_wait),
            EvaluationWaitPoll::Abandoned
        );
        assert_eq!(
            crate::eval::eval_value(&observer, &Value::Lazy(lazy.clone()))
                .expect("another session should reclaim the lazy"),
            expected
        );
        assert!(forced.load(Ordering::Acquire));
        assert!(lazy.cached().is_some_and(|result| result.is_ok()));
    }

    #[test]
    fn owner_session_drop_fails_task_promises_but_not_host_promises() {
        let fixture = SameRuntimeFixture::new();
        let task_promise = {
            let owner = fixture.context();
            let (promise, _owner_task, _owner_context) = owner
                .task_owned_promise(Arc::from("abandoned task promise"))
                .expect("task promise should register");
            let wait = promise
                .task()
                .expect("task promise should retain producer provenance")
                .wait();
            assert_eq!(
                wait.subscribe_test_work(),
                CompletionSubscriptionOutcome::Pending
            );
            promise
        };
        let observer = fixture.context();
        let error = task_promise
            .assignment()
            .expect("session closure must assign the task promise")
            .expect_err("an abandoned task promise must fail");
        assert!(error.to_string().contains("was abandoned"));
        assert_eq!(
            task_promise
                .task()
                .expect("task promise should retain producer provenance")
                .wait()
                .exact_subscription_count(),
            0
        );
        assert!(matches!(
            observer.poll_wait(
                task_promise
                    .task()
                    .expect("task promise should retain producer provenance")
                    .wait()
            ),
            EvaluationWaitPoll::Failed(wait_error) if Arc::ptr_eq(&error, &wait_error)
        ));

        let host_promise = {
            let transient_observer = fixture.context();
            PromisedValue::new(transient_observer.values(), "host promise")
        };
        assert!(
            host_promise.assignment().is_none(),
            "dropping an unrelated observer session must not poison a host promise"
        );
        host_promise
            .set(observer.values().unit())
            .expect("the host promise should remain assignable");
        assert!(
            host_promise
                .assignment()
                .is_some_and(|assignment| assignment.is_ok())
        );
    }

    #[test]
    fn owner_session_drop_exactly_wakes_a_task_promise_follower() {
        let fixture = SameRuntimeFixture::new();
        let observer = fixture.context();
        let (promise, lazy) = {
            let owner = fixture.context();
            let (promise, _owner_task, _owner_context) = owner
                .task_owned_promise(Arc::from("abandoned exact promise"))
                .expect("task-owned promise should register");
            let lazy = LazyValue::from_access(
                observer.values(),
                Arc::from([]),
                Arc::from([Value::Promised(promise.clone())]),
            );
            let blocked = crate::eval::eval_value(&observer, &Value::Lazy(lazy.clone()))
                .expect_err("the unresolved task promise should block its follower");
            assert!(blocked.blocked_on().is_some());
            assert_eq!(promise.exact_subscription_count(), 1);
            (promise, lazy)
        };

        assert_eq!(promise.exact_subscription_count(), 0);
        assert!(
            promise
                .assignment()
                .is_some_and(|assignment| assignment.is_err())
        );
        assert!(
            crate::eval::eval_value(&observer, &Value::Lazy(lazy))
                .expect_err("owner abandonment should fail the exact follower")
                .to_string()
                .contains("was abandoned")
        );
    }

    #[test]
    fn task_cancellation_exactly_wakes_its_promise_follower() {
        let fixture = SameRuntimeFixture::new();
        let owner = fixture.context();
        let observer = fixture.context();
        let (promise, owner_task, _owner_context) = owner
            .task_owned_promise(Arc::from("cancelled exact promise"))
            .expect("task-owned promise should register");
        let lazy = LazyValue::from_access(
            observer.values(),
            Arc::from([]),
            Arc::from([Value::Promised(promise.clone())]),
        );

        let blocked = crate::eval::eval_value(&observer, &Value::Lazy(lazy.clone()))
            .expect_err("the unresolved task promise should block its follower");
        assert!(blocked.blocked_on().is_some());
        assert_eq!(promise.exact_subscription_count(), 1);
        assert_eq!(owner_task.cancel(), EvaluationTaskCancellation::Requested);
        assert_eq!(promise.exact_subscription_count(), 0);
        assert!(
            crate::eval::eval_value(&observer, &Value::Lazy(lazy))
                .expect_err("producer cancellation should fail the exact follower")
                .to_string()
                .contains("was cancelled")
        );
    }

    #[test]
    fn task_terminal_surfaces_publish_under_one_mutation_admission() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        let (mut promises, owner_task, _owner_context) = context
            .task_owned_promises([
                Arc::from("live atomic terminal publication"),
                Arc::from("dropped atomic terminal publication"),
            ])
            .expect("task-owned promises should register");
        let dropped_promise = promises.pop().expect("dropped promise should exist");
        let dropped_wait = dropped_promise
            .task()
            .expect("task-owned promise should expose its producer")
            .wait()
            .clone();
        drop(dropped_promise);
        let promise = promises.pop().expect("live promise should exist");
        let task_wait = owner_task.wait().clone();
        let observed = Arc::new(Mutex::new(None));
        let probe_result = observed.clone();
        let weak_coordinator = Arc::downgrade(&coordinator);
        let probed_dropped_wait = dropped_wait.clone();
        coordinator.set_terminal_publication_probe({
            let promise = promise.clone();
            move || {
                let admission_is_held = weak_coordinator
                    .upgrade()
                    .is_some_and(|coordinator| !coordinator.settlement_admission_is_free());
                *probe_result
                    .lock()
                    .expect("terminal publication probe result was poisoned") = Some((
                    task_wait.terminal_poll().is_some(),
                    promise.assignment().is_some(),
                    probed_dropped_wait.terminal_poll().is_some(),
                    admission_is_held,
                ));
            }
        });

        assert_eq!(owner_task.cancel(), EvaluationTaskCancellation::Requested);
        assert_eq!(
            *observed
                .lock()
                .expect("terminal publication probe result was poisoned"),
            Some((true, true, true, true)),
            "task wait, task-owned promise, and mutation admission must expose one atomic terminal surface"
        );
        assert!(matches!(
            context.poll_reflection_task(&owner_task),
            EvaluationWaitPoll::Cancelled
        ));
        assert!(promise.assignment().is_some_and(|result| result.is_err()));
        assert!(matches!(
            dropped_wait.terminal_poll(),
            Some(EvaluationWaitPoll::Failed(_))
        ));
    }

    #[test]
    fn assigned_task_promise_is_removed_before_later_task_terminalization() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let (promise_sender, promise_receiver) = mpsc::channel();
        let (assigned_sender, assigned_receiver) = mpsc::channel();
        let task = context
            .schedule_task(move |task_context| {
                let promise = PromisedValue::fixpoint(&task_context, "assigned task promise")?;
                promise_sender
                    .send(promise.clone())
                    .expect("test should receive its task-owned promise");
                Ok(Box::new(AssignPromiseThenYield {
                    promise: Some(promise),
                    assigned: Some(assigned_sender),
                }))
            })
            .expect("task promise should register");
        let promise = promise_receiver
            .recv()
            .expect("task construction should publish its promise");
        let promise_wait = promise
            .task()
            .expect("task promise should retain producer provenance")
            .wait()
            .clone();

        assert_eq!(
            context.pump_wait(task.wait(), 1),
            EvaluationPumpOutcome::BudgetExhausted
        );
        assigned_receiver
            .recv()
            .expect("the owning machine should publish before yielding");
        assert_eq!(
            context.task_registry_counts().promises_active,
            0,
            "synchronous assignment must remove the producer obligation before returning"
        );
        assert!(matches!(
            context.poll_wait(&promise_wait),
            EvaluationWaitPoll::Complete(value) if value == context.values().unit()
        ));

        assert_eq!(task.cancel(), EvaluationTaskCancellation::Requested);
        assert_eq!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Cancelled
        );
        assert!(matches!(
            context.poll_wait(&promise_wait),
            EvaluationWaitPoll::Complete(value) if value == context.values().unit()
        ));
        assert_eq!(context.task_registry_counts().promises_active, 0);
    }

    #[test]
    fn long_lived_session_retains_only_unacknowledged_terminal_failures() {
        const ITERATIONS: usize = 64;

        let context = EvalContext::standalone();
        let mut completed = Vec::with_capacity(ITERATIONS);
        let mut failed = Vec::with_capacity(ITERATIONS);
        let mut cancelled = Vec::with_capacity(ITERATIONS);
        let mut promises = Vec::with_capacity(ITERATIONS);

        for index in 0..ITERATIONS {
            let complete = context
                .schedule_task(|_| Ok(Box::new(Complete)))
                .expect("successful reflection task should schedule");
            assert_eq!(
                context.pump_wait(complete.wait(), 256),
                EvaluationPumpOutcome::TargetReady
            );
            completed.push(complete);

            let failure = context
                .schedule_task(|_| Ok(Box::new(Fail)))
                .expect("failing reflection task should schedule");
            assert_eq!(
                context.pump_wait(failure.wait(), 256),
                EvaluationPumpOutcome::TargetReady
            );
            failed.push(failure);

            let cancellation = context
                .schedule_task(|_| Ok(Box::new(Complete)))
                .expect("cancelled reflection task should schedule");
            assert_eq!(cancellation.cancel(), EvaluationTaskCancellation::Requested);
            cancelled.push(cancellation);

            let lazy = LazyValue::deferred(
                &crate::core::test_value_factory(),
                format!("successful lazy {index}"),
                |_| Ok(crate::core::keys::unit_value()),
            );
            assert_eq!(
                crate::eval::eval_value(&context, &Value::Lazy(lazy))
                    .expect("successful lazy should evaluate"),
                crate::core::keys::unit_value()
            );

            let lazy = LazyValue::deferred(
                &crate::core::test_value_factory(),
                format!("failed lazy {index}"),
                |_| Err(crate::core::EvaluationHalt::new("long-lived lazy failure")),
            );
            assert!(
                crate::eval::eval_value(&context, &Value::Lazy(lazy)).is_err(),
                "failed lazy should terminate without retaining its task record"
            );

            let (promise, owner_task, _owner_context) = context
                .task_owned_promise(Arc::from(format!("promise {index}")))
                .expect("task-owned promise should register");
            let wait = promise
                .task()
                .expect("task-owned promise should expose its wait")
                .wait()
                .clone();
            if index % 2 == 0 {
                promise
                    .set(crate::core::keys::unit_value())
                    .expect("successful promise should complete once");
            } else {
                promise
                    .fail_message("long-lived promise failure")
                    .expect("failed promise should complete once");
            }
            context.complete_wait(owner_task.wait());
            promises.push(wait);
        }

        let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
            panic!("terminal stress fixtures should leave no unfinished work")
        };
        assert_eq!(report.failures.size(), ITERATIONS);
        assert!(report.unfinished.is_empty());

        let counts = context.task_registry_counts();
        assert_eq!(counts.reflection_active, 0);
        assert_eq!(counts.reflection_terminal, 0);
        assert_eq!(counts.reflection_by_id, 0);
        assert_eq!(counts.unacknowledged_failures, ITERATIONS);
        assert_eq!(counts.deferred_active, 0);
        assert_eq!(counts.deferred_terminal, 0);
        assert_eq!(counts.deferred_by_wait, 0);
        assert_eq!(counts.deferred_by_task, 0);
        assert_eq!(counts.promises_active, 0);
        assert_eq!(counts.promises_terminal, 0);
        assert_eq!(counts.owned_promise_waits, 0);

        assert!(matches!(
            context.poll_reflection_task(&completed[0]),
            EvaluationWaitPoll::Complete(_)
        ));
        assert!(matches!(
            context.poll_reflection_task(&failed[0]),
            EvaluationWaitPoll::Failed(_)
        ));
        assert_eq!(
            context.poll_reflection_task(&cancelled[0]),
            EvaluationWaitPoll::Cancelled
        );
        assert!(matches!(
            context.poll_wait(&promises[0]),
            EvaluationWaitPoll::Complete(_)
        ));
        assert!(matches!(
            context.poll_wait(&promises[1]),
            EvaluationWaitPoll::Failed(_)
        ));

        for task in &failed {
            task.acknowledge_failure();
        }
        assert_eq!(
            context.task_registry_counts().unacknowledged_failures,
            0,
            "acknowledgement should release the only retained terminal session state"
        );
    }

    #[test]
    fn concurrent_waiters_observe_terminal_publication() {
        let (coordinator, _executor) =
            test_execution_resources(1).expect("test executor should start");
        let session = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&session);
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let task = context
            .schedule_task(move |_| {
                Ok(Box::new(CompleteAfterRelease {
                    started: Some(started_sender),
                    release: release_receiver,
                }))
            })
            .expect("test task should schedule");
        started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should claim the task");

        let barrier = Arc::new(std::sync::Barrier::new(9));
        let waiters = (0..8)
            .map(|_| {
                let context = context.clone();
                let wait = task.wait().clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let deadline = Instant::now() + Duration::from_secs(2);
                    loop {
                        match context.poll_wait(&wait) {
                            EvaluationWaitPoll::Pending(_) if Instant::now() < deadline => {
                                std::thread::yield_now();
                            }
                            EvaluationWaitPoll::Pending(_) => {
                                panic!("waiter timed out before terminal publication")
                            }
                            EvaluationWaitPoll::Complete(_) => break,
                            poll => panic!("waiter observed unexpected task state: {poll:?}"),
                        }
                    }
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        release_sender
            .send(())
            .expect("worker release receiver should remain open");
        for waiter in waiters {
            waiter.join().expect("concurrent waiter should not panic");
        }
        assert!(matches!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Complete(_)
        ));
    }

    #[test]
    fn a_lazy_task_that_waits_on_itself_is_poisoned_as_a_cycle() {
        let context = EvalContext::standalone();
        let lazy = inert_lazy("self cycle");
        let dependency = Arc::new(OnceLock::new());
        let wait = register_lazy_await(&context, &lazy, dependency.clone());
        dependency
            .set(wait.clone())
            .expect("self wait should be installed once");

        assert_eq!(
            context.pump_wait(&wait, 256),
            EvaluationPumpOutcome::TargetReady
        );
        let cycle = dependency_cycle(&lazy);
        assert_eq!(cycle.members.len(), 1);
        assert_eq!(cycle.members[0].id, lazy.id());
        assert_eq!(cycle.members[0].label.as_ref(), "self cycle");
        assert!(matches!(
            context.poll_wait(&wait),
            EvaluationWaitPoll::Failed(error)
                if error.to_string().contains("lazy dependency cycle")
        ));
        assert!(
            lazy.source_snapshot().is_none(),
            "cycle poisoning should release the lazy source"
        );
        assert_deferred_task_retired(&context, &lazy);
    }

    #[test]
    fn concurrently_demanded_lazy_tasks_share_one_two_node_cycle_failure() {
        let context = EvalContext::standalone();
        let left = inert_lazy("left");
        let right = inert_lazy("right");
        let left_dependency = Arc::new(OnceLock::new());
        let right_dependency = Arc::new(OnceLock::new());
        let left_wait = register_lazy_await(&context, &left, left_dependency.clone());
        let right_wait = register_lazy_await(&context, &right, right_dependency.clone());
        left_dependency
            .set(right_wait.clone())
            .expect("left dependency should be installed once");
        right_dependency
            .set(left_wait.clone())
            .expect("right dependency should be installed once");

        let barrier = Arc::new(std::sync::Barrier::new(3));
        let left_thread = {
            let context = context.clone();
            let barrier = barrier.clone();
            let wait = left_wait.clone();
            std::thread::spawn(move || {
                barrier.wait();
                context.pump_wait(&wait, 256)
            })
        };
        let right_thread = {
            let context = context.clone();
            let barrier = barrier.clone();
            let wait = right_wait.clone();
            std::thread::spawn(move || {
                barrier.wait();
                context.pump_wait(&wait, 256)
            })
        };
        barrier.wait();
        let left_outcome = left_thread.join().unwrap();
        let right_outcome = right_thread.join().unwrap();
        assert!(matches!(
            left_outcome,
            EvaluationPumpOutcome::TargetReady
                | EvaluationPumpOutcome::Busy
                | EvaluationPumpOutcome::NoProgress
        ));
        assert!(matches!(
            right_outcome,
            EvaluationPumpOutcome::TargetReady
                | EvaluationPumpOutcome::Busy
                | EvaluationPumpOutcome::NoProgress
        ));
        assert!(matches!(
            context.poll_wait(&left_wait),
            EvaluationWaitPoll::Failed(_)
        ));
        assert!(matches!(
            context.poll_wait(&right_wait),
            EvaluationWaitPoll::Failed(_)
        ));

        let left_failure = context.lazy_failure(&left).unwrap();
        let right_failure = context.lazy_failure(&right).unwrap();
        assert!(Arc::ptr_eq(&left_failure, &right_failure));
        assert!(left.source_snapshot().is_none());
        assert!(right.source_snapshot().is_none());
        assert_eq!(left_wait.exact_subscription_count(), 0);
        assert_eq!(right_wait.exact_subscription_count(), 0);
        assert_deferred_task_retired(&context, &left);
        assert_deferred_task_retired(&context, &right);
        let cycle = dependency_cycle(&left);
        assert_eq!(
            cycle
                .members
                .iter()
                .map(|member| member.id)
                .collect::<Vec<_>>(),
            vec![left.id(), right.id()]
        );
    }

    #[test]
    fn two_sessions_share_and_retire_one_pure_lazy_cycle_failure() {
        let fixture = SameRuntimeFixture::new();
        let left_context = fixture.context();
        let right_context = fixture.context();
        let left = inert_lazy_for(left_context.values(), "cross-session left");
        let right = inert_lazy_for(right_context.values(), "cross-session right");
        let left_dependency = Arc::new(OnceLock::new());
        let right_dependency = Arc::new(OnceLock::new());
        let left_wait = register_lazy_await(&left_context, &left, left_dependency.clone());
        let right_wait = register_lazy_await(&right_context, &right, right_dependency.clone());
        left_dependency
            .set(right_wait.clone())
            .expect("left dependency should be installed once");
        right_dependency
            .set(left_wait.clone())
            .expect("right dependency should be installed once");

        assert_eq!(
            left_context.pump_wait(&left_wait, 256),
            EvaluationPumpOutcome::TargetReady
        );
        assert_eq!(
            right_context.pump_wait(&right_wait, 256),
            EvaluationPumpOutcome::TargetReady
        );
        assert!(matches!(
            left_context.poll_wait(&left_wait),
            EvaluationWaitPoll::Failed(_)
        ));
        assert!(matches!(
            right_context.poll_wait(&right_wait),
            EvaluationWaitPoll::Failed(_)
        ));

        let left_failure = left_context
            .lazy_failure(&left)
            .expect("left cycle member should retain its failure");
        let right_failure = right_context
            .lazy_failure(&right)
            .expect("right cycle member should retain its failure");
        assert!(Arc::ptr_eq(&left_failure, &right_failure));
        let cycle = dependency_cycle(&left);
        assert_eq!(
            cycle
                .members
                .iter()
                .map(|member| member.id)
                .collect::<Vec<_>>(),
            vec![left.id(), right.id()]
        );
        assert!(left.source_snapshot().is_none());
        assert!(right.source_snapshot().is_none());
        assert_eq!(left_wait.exact_subscription_count(), 0);
        assert_eq!(right_wait.exact_subscription_count(), 0);
        assert_deferred_task_retired(&left_context, &left);
        assert_deferred_task_retired(&right_context, &right);
    }

    #[test]
    fn a_cross_session_promise_lazy_cycle_remains_unpoisoned() {
        let fixture = SameRuntimeFixture::new();
        let lazy_context = fixture.context();
        let promise_context = fixture.context();
        let lazy = inert_lazy_for(lazy_context.values(), "mixed cross-session lazy");
        let promise = PromisedValue::new(lazy_context.values(), "mixed cross-session promise");
        let lazy_dependency = Arc::new(OnceLock::new());
        let promise_dependency = Arc::new(OnceLock::new());
        let lazy_wait = register_lazy_await(&lazy_context, &lazy, lazy_dependency.clone());
        let promise_wait =
            register_promise_await(&promise_context, &promise, promise_dependency.clone());
        lazy_dependency
            .set(promise_wait.clone())
            .expect("lazy dependency should be installed once");
        promise_dependency
            .set(lazy_wait.clone())
            .expect("promise dependency should be installed once");

        assert_eq!(
            lazy_context.pump_wait(&lazy_wait, 256),
            EvaluationPumpOutcome::NoProgress
        );
        assert_eq!(
            promise_context.pump_wait(&promise_wait, 256),
            EvaluationPumpOutcome::NoProgress
        );
        assert!(lazy.cached().is_none());
        assert!(lazy.source_snapshot().is_some());
        assert!(promise.assignment().is_none());
        assert!(matches!(
            lazy_context.poll_wait(&lazy_wait),
            EvaluationWaitPoll::Pending(_)
        ));
        assert!(matches!(
            promise_context.poll_wait(&promise_wait),
            EvaluationWaitPoll::Pending(_)
        ));
    }

    #[test]
    fn lazy_cycles_are_canonical_and_exclude_upstream_dependents() {
        let context = EvalContext::standalone();
        let upstream = inert_lazy("upstream");
        let first = inert_lazy("first");
        let second = inert_lazy("second");
        let third = inert_lazy("third");

        let upstream_dependency = Arc::new(OnceLock::new());
        let first_dependency = Arc::new(OnceLock::new());
        let second_dependency = Arc::new(OnceLock::new());
        let third_dependency = Arc::new(OnceLock::new());
        let upstream_wait = register_lazy_await(&context, &upstream, upstream_dependency.clone());
        let first_wait = register_lazy_await(&context, &first, first_dependency.clone());
        let second_wait = register_lazy_await(&context, &second, second_dependency.clone());
        let third_wait = register_lazy_await(&context, &third, third_dependency.clone());
        upstream_dependency.set(first_wait.clone()).unwrap();
        first_dependency.set(second_wait.clone()).unwrap();
        second_dependency.set(third_wait.clone()).unwrap();
        third_dependency.set(first_wait).unwrap();

        assert_eq!(
            context.pump_wait(&upstream_wait, 512),
            EvaluationPumpOutcome::TargetReady
        );
        let cycle = dependency_cycle(&first);
        assert_eq!(
            cycle
                .members
                .iter()
                .map(|member| member.id)
                .collect::<Vec<_>>(),
            vec![first.id(), second.id(), third.id()]
        );
        let EvaluationWaitPoll::Failed(upstream_failure) = context.poll_wait(&upstream_wait) else {
            panic!("upstream dependent should receive the cycle failure");
        };
        let cycle_failure = context
            .lazy_failure(&first)
            .expect("cycle member should retain its failure");
        assert!(Arc::ptr_eq(&upstream_failure, &cycle_failure));
    }

    #[test]
    fn a_mixed_lazy_reflection_cycle_remains_quiescent() {
        let context = EvalContext::standalone();
        let lazy = inert_lazy("mixed lazy");
        let lazy_wait_slot = Arc::new(OnceLock::new());
        let reflection = context
            .schedule_task({
                let dependency = lazy_wait_slot.clone();
                move |task_context| {
                    Ok(Box::new(AwaitCell {
                        context: task_context,
                        dependency,
                    }))
                }
            })
            .expect("reflection task should schedule");
        let reflection_wait_slot = Arc::new(OnceLock::new());
        reflection_wait_slot
            .set(reflection.wait.clone())
            .expect("reflection dependency should be installed once");
        let lazy_wait = register_lazy_await(&context, &lazy, reflection_wait_slot);
        lazy_wait_slot
            .set(lazy_wait.clone())
            .expect("lazy dependency should be installed once");

        assert_eq!(
            context.pump_wait(&lazy_wait, 256),
            EvaluationPumpOutcome::NoProgress
        );
        assert!(context.lazy_failure(&lazy).is_none());
        assert!(lazy.cached().is_none());
        assert!(
            lazy.source_snapshot().is_some(),
            "retryable blockage must retain the lazy source"
        );
        assert!(matches!(
            context.poll_wait(&lazy_wait),
            EvaluationWaitPoll::Pending(_)
        ));
        assert!(matches!(
            context.poll_reflection_task(&reflection),
            EvaluationWaitPoll::Pending(_)
        ));
    }

    #[test]
    fn pump_and_quiescence_do_not_repoll_an_unchanged_block() {
        let context = EvalContext::standalone();
        let polls = Arc::new(Mutex::new(0));
        let target = context
            .schedule_task({
                let polls = polls.clone();
                move |_| Ok(Box::new(CountedBlocked { polls }))
            })
            .expect("blocked task should schedule");

        assert_eq!(
            context.pump_wait(&target.wait, 256),
            EvaluationPumpOutcome::NoProgress
        );
        assert_eq!(*polls.lock().unwrap(), 1);
        assert_eq!(
            context.pump_wait(&target.wait, 256),
            EvaluationPumpOutcome::NoProgress
        );
        assert_eq!(*polls.lock().unwrap(), 1);
        assert!(matches!(
            context.run_until_quiescent(),
            EvaluationSessionRun::Deadlocked(_)
        ));
        assert_eq!(*polls.lock().unwrap(), 1);
        assert!(matches!(
            context.poll_reflection_task(&target),
            EvaluationWaitPoll::Pending(_)
        ));
    }

    #[test]
    fn pump_reports_budget_exhaustion_for_runnable_work() {
        let context = EvalContext::standalone();
        let target = context
            .schedule_task(|_| Ok(Box::new(AlwaysYields)))
            .expect("yielding task should schedule");

        assert_eq!(
            context.pump_wait(&target.wait, 1),
            EvaluationPumpOutcome::BudgetExhausted
        );
    }

    #[test]
    fn cancellation_stops_a_queued_task_and_late_requests_are_noops() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let cancelled = Arc::new(AtomicBool::new(false));
        let observed = cancelled.clone();
        let task = context
            .schedule_task(move |_| {
                Ok(Box::new(Cancellable {
                    cancelled: observed,
                }))
            })
            .expect("cancellable task should schedule");
        assert_eq!(task.cancel(), EvaluationTaskCancellation::Requested);
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Cancelled
        );
        assert_eq!(task.cancel(), EvaluationTaskCancellation::Late);

        assert_eq!(
            task.cancel(),
            EvaluationTaskCancellation::Late,
            "terminal cancellation requests must remain harmless"
        );
    }

    #[test]
    fn running_cancellation_waits_for_release_then_wins_over_the_poll_result() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (promise_sender, promise_receiver) = mpsc::channel();
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let task = context
            .schedule_task({
                let cancelled = cancelled.clone();
                move |task_context| {
                    let promise = PromisedValue::fixpoint(
                        &task_context,
                        "promise owned by running cancellable task",
                    )?;
                    promise_sender
                        .send(promise)
                        .expect("running cancellation test must receive its promise");
                    Ok(Box::new(CancellableAfterRelease {
                        started: Some(started_sender),
                        release: release_receiver,
                        cancelled,
                    }))
                }
            })
            .expect("running cancellation fixture should schedule");
        let promise = promise_receiver
            .recv()
            .expect("task construction should publish its owned promise");
        let promise_wait = promise
            .task()
            .expect("task-owned promise should retain producer provenance")
            .wait()
            .clone();
        let worker = {
            let context = context.clone();
            let wait = task.wait().clone();
            std::thread::spawn(move || context.pump_wait(&wait, 256))
        };
        started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should claim the task");

        assert_eq!(task.cancel(), EvaluationTaskCancellation::Requested);
        assert!(matches!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Pending(_)
        ));
        assert!(promise.assignment().is_none());
        assert!(matches!(
            context.poll_wait(&promise_wait),
            EvaluationWaitPoll::Pending(_)
        ));
        assert!(
            !cancelled.load(Ordering::Acquire),
            "the cancellation hook cannot run while the worker owns the machine"
        );

        release_sender
            .send(())
            .expect("running task should still await release");
        assert_eq!(
            worker.join().expect("worker should not panic"),
            EvaluationPumpOutcome::TargetReady
        );
        assert!(cancelled.load(Ordering::Acquire));
        let promise_failure = promise
            .assignment()
            .expect("owner-thread terminalization must settle its unresolved promise")
            .expect_err("cancellation must fail the unresolved task-owned promise");
        assert!(promise_failure.to_string().contains("was cancelled"));
        assert!(matches!(
            context.poll_wait(&promise_wait),
            EvaluationWaitPoll::Failed(wait_failure)
                if Arc::ptr_eq(&promise_failure, &wait_failure)
        ));
        for _ in 0..2 {
            assert_eq!(
                context.poll_reflection_task(&task),
                EvaluationWaitPoll::Cancelled,
                "the cancellation result must remain readable after record retirement"
            );
        }
        let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
            panic!("cancelled running work should leave no unfinished task")
        };
        assert!(
            report.failures.is_empty(),
            "the machine's discarded failure must not enter the reporting ledger"
        );
    }

    #[test]
    fn executor_shutdown_preserves_worker_owned_cancellation_and_task_promise() {
        let (coordinator, executor) =
            test_execution_resources(1).expect("test executor should start");
        let session = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&session);
        let cancelled = Arc::new(AtomicBool::new(false));
        let (promise_sender, promise_receiver) = mpsc::channel();
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let task = context
            .schedule_task({
                let cancelled = cancelled.clone();
                move |task_context| {
                    let promise = PromisedValue::fixpoint(
                        &task_context,
                        "promise owned across executor shutdown",
                    )?;
                    promise_sender
                        .send(promise)
                        .expect("shutdown test must receive its task-owned promise");
                    Ok(Box::new(CancellableAfterRelease {
                        started: Some(started_sender),
                        release: release_receiver,
                        cancelled,
                    }))
                }
            })
            .expect("worker-owned shutdown task should schedule");
        let promise = promise_receiver
            .recv()
            .expect("task construction should publish its promise");
        let promise_wait = promise
            .task()
            .expect("task-owned promise should retain producer provenance")
            .wait()
            .clone();
        started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("a real worker should claim the task");

        assert_eq!(task.cancel(), EvaluationTaskCancellation::Requested);
        drop(executor);
        assert!(matches!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Pending(_)
        ));
        assert!(promise.assignment().is_none());
        assert!(
            !cancelled.load(Ordering::Acquire),
            "executor shutdown must not steal the worker-owned machine"
        );

        release_sender
            .send(())
            .expect("the detached worker should finish its quantum");
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Pending(_)
        ) && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        while !cancelled.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }

        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Cancelled
        );
        let promise_failure = promise
            .assignment()
            .expect("returning worker must settle its task-owned promise")
            .expect_err("cancellation must fail the unresolved promise");
        assert!(promise_failure.to_string().contains("was cancelled"));
        assert!(matches!(
            context.poll_wait(&promise_wait),
            EvaluationWaitPoll::Failed(wait_failure)
                if Arc::ptr_eq(&promise_failure, &wait_failure)
        ));
        assert_eq!(context.task_registry_counts().reflection_active, 0);
    }

    #[test]
    fn serial_ready_tasks_preserve_same_session_fifo_order_across_requeue() {
        let fixture = SameRuntimeFixture::new();
        let first = fixture.context();
        let other = fixture.context();
        let coordinator = first.coordinator().expect("coordinator should be live");
        let polls = Arc::new(Mutex::new(Vec::new()));
        let schedule = |context: &EvalContext, label, yield_once| {
            let polls = polls.clone();
            context
                .schedule_task(move |_| {
                    Ok(Box::new(RecordPollOrder {
                        label,
                        polls,
                        yield_once,
                    }))
                })
                .expect("ordered reflection task should schedule")
        };

        let _first = schedule(&first, 1, true);
        let other_task = schedule(&other, 9, false);
        let _second = schedule(&first, 2, false);
        let _third = schedule(&first, 3, false);
        coordinator.executor_started(1);
        first.spark(first.values().unit());

        let EvaluationSessionRun::Complete(report) = first.run_until_quiescent() else {
            panic!("the first session's ordered tasks should complete")
        };
        assert!(report.failures.is_empty());
        assert_eq!(*polls.lock().unwrap(), [1, 2, 3, 1]);
        assert!(matches!(
            other.poll_reflection_task(&other_task),
            EvaluationWaitPoll::Pending(_)
        ));

        let EvaluationSessionRun::Complete(report) = other.run_until_quiescent() else {
            panic!("the unrelated session's task should remain independently runnable")
        };
        assert!(report.failures.is_empty());
        assert_eq!(*polls.lock().unwrap(), [1, 2, 3, 1, 9]);

        fixture.runtime.pump_until_stable();
        coordinator.executor_stopped();
        assert_eq!(coordinator.retained_spark_count(), 0);
    }

    #[test]
    fn error_acknowledgement_is_timing_independent_and_preserves_task_results() {
        let context = EvalContext::standalone();

        let reserved = context.reserve_task().expect("task should reserve");
        reserved.acknowledge_failure();
        assert!(
            context
                .coordinator()
                .expect("reserved task must retain its coordinator")
                .task_failure_is_acknowledged(reserved.id())
        );
        context.cancel_reserved_task(&reserved);

        let blocked = context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .expect("blocked task should schedule");
        assert_eq!(
            context.pump_wait(blocked.wait(), 256),
            EvaluationPumpOutcome::NoProgress
        );
        blocked.acknowledge_failure();
        assert!(
            context
                .coordinator()
                .expect("blocked task must retain its coordinator")
                .task_failure_is_acknowledged(blocked.id())
        );
        assert_eq!(blocked.cancel(), EvaluationTaskCancellation::Requested);

        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let running = context
            .schedule_task(move |_| {
                Ok(Box::new(FailAfterRelease {
                    started: Some(started_sender),
                    release: release_receiver,
                }))
            })
            .expect("running task should schedule");
        let worker = {
            let context = context.clone();
            let wait = running.wait().clone();
            std::thread::spawn(move || context.pump_wait(&wait, 256))
        };
        started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should claim the running task");
        running.acknowledge_failure();
        release_sender
            .send(())
            .expect("running task should still be waiting");
        assert_eq!(
            worker.join().expect("worker should not panic"),
            EvaluationPumpOutcome::TargetReady
        );
        assert!(matches!(
            context.poll_reflection_task(&running),
            EvaluationWaitPoll::Failed(error)
                if error.to_string() == "acknowledged task failure"
        ));
        let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
            panic!("only terminal tasks should remain")
        };
        assert!(report.failures.is_empty());

        let failed = context
            .schedule_task(|_| Ok(Box::new(Fail)))
            .expect("failing task should schedule");
        let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
            panic!("failing task should terminate")
        };
        assert_eq!(report.failures.size(), 1);
        assert!(report.failures.contains_key(&failed.id()));
        failed.acknowledge_failure();
        failed.acknowledge_failure();
        for _ in 0..2 {
            let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
                panic!("acknowledged task should remain terminal")
            };
            assert!(
                report.failures.is_empty(),
                "acknowledged failure must stay absent from repeated reports"
            );
        }
        assert!(matches!(
            context.poll_reflection_task(&failed),
            EvaluationWaitPoll::Failed(error) if error.to_string() == "reasoning failed"
        ));

        let successful = context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("successful task should schedule");
        assert_eq!(
            context.pump_wait(successful.wait(), 256),
            EvaluationPumpOutcome::TargetReady
        );
        successful.acknowledge_failure();
        assert!(matches!(
            context.poll_reflection_task(&successful),
            EvaluationWaitPoll::Complete(_)
        ));

        let cancelled = context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("cancelled task should schedule");
        assert_eq!(cancelled.cancel(), EvaluationTaskCancellation::Requested);
        cancelled.acknowledge_failure();
        assert_eq!(
            context.poll_reflection_task(&cancelled),
            EvaluationWaitPoll::Cancelled
        );
        let counts = context.task_registry_counts();
        assert_eq!(counts.reflection_active, 0);
        assert_eq!(counts.reflection_terminal, 0);
        assert_eq!(counts.reflection_by_id, 0);
        assert_eq!(
            counts.unacknowledged_failures, 0,
            "acknowledging before or after failure must leave no reporting entry"
        );
    }

    #[test]
    fn runtime_failure_ledger_preserves_owner_buckets_and_persistent_snapshots() {
        let (coordinator, _executor) =
            test_execution_resources(0).expect("test runtime should build");
        let first_owner = EvaluationSession::shared(&coordinator);
        let second_owner = EvaluationSession::shared(&coordinator);
        let first_context = EvalContext::new(&first_owner);
        let second_context = EvalContext::new(&second_owner);

        let first_task = first_context
            .schedule_task(|_| Ok(Box::new(Fail)))
            .expect("first failing task should schedule");
        let second_task = second_context
            .schedule_task(|_| Ok(Box::new(Fail)))
            .expect("second failing task should schedule");
        assert_eq!(
            second_task.id().get(),
            first_task.id().get() + 1,
            "task identities should be adjacent in the shared runtime domain"
        );

        let EvaluationSessionRun::Complete(first_report) = first_context.run_until_quiescent()
        else {
            panic!("the first owner should drain its failing task")
        };
        let EvaluationSessionRun::Complete(second_report) = second_context.run_until_quiescent()
        else {
            panic!("the second owner should drain its failing task")
        };
        assert!(first_report.failures.contains_key(&first_task.id()));
        assert!(!first_report.failures.contains_key(&second_task.id()));
        assert!(second_report.failures.contains_key(&second_task.id()));
        assert!(!second_report.failures.contains_key(&first_task.id()));

        let first_snapshot = first_report.failures.clone();
        let first_owner_id = first_context.session_id();
        let second_owner_id = second_context.session_id();
        let first_task_id = first_task.id();
        let second_task_id = second_task.id();
        drop(first_task);
        drop(first_owner);

        let ledger = coordinator.failure_ledger_snapshot();
        assert_eq!(ledger.size(), 2);
        assert!(
            ledger
                .get(&first_owner_id)
                .is_some_and(|failures| failures.contains_key(&first_task_id)),
            "owner closure and final task-handle drop must not erase its failure bucket"
        );
        assert!(
            ledger
                .get(&second_owner_id)
                .is_some_and(|failures| failures.contains_key(&second_task_id))
        );

        first_context.acknowledge_task_failure(first_owner_id, first_task_id);
        assert!(
            first_snapshot.contains_key(&first_task_id),
            "persistent report snapshots must remain valid after later acknowledgement"
        );
        let ledger = coordinator.failure_ledger_snapshot();
        assert!(
            !ledger.contains_key(&first_owner_id),
            "acknowledging the last owner failure should remove its empty bucket"
        );
        assert!(
            ledger
                .get(&second_owner_id)
                .is_some_and(|failures| failures.contains_key(&second_task_id)),
            "acknowledgement must not disturb another owner bucket"
        );

        drop(second_task);
        drop(second_owner);
        second_context.acknowledge_task_failure(second_owner_id, second_task_id);
        assert!(coordinator.failure_ledger_snapshot().is_empty());
    }

    #[test]
    fn run_until_quiescent_drains_tasks_spawned_during_the_run() {
        let context = EvalContext::standalone();
        context
            .schedule_task(|task_context| {
                Ok(Box::new(SpawnOnce {
                    context: task_context,
                    spawned: false,
                }))
            })
            .unwrap();

        let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
            panic!("finite parent and child tasks should drain");
        };
        assert!(report.failures.is_empty());
        assert!(report.unfinished.is_empty());
        assert_eq!(context.reflection_task_count(), 0);
    }

    #[test]
    fn run_until_quiescent_collects_failures_without_short_circuiting() {
        let context = EvalContext::standalone();
        let failed = context.schedule_task(|_| Ok(Box::new(Fail))).unwrap();
        context.schedule_task(|_| Ok(Box::new(Complete))).unwrap();

        for _ in 0..2 {
            let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
                panic!("terminal failures do not leave unfinished work");
            };
            assert_eq!(report.failures.size(), 1);
            assert_eq!(
                report
                    .failures
                    .get(&failed.id())
                    .expect("failed task should remain in the reporting ledger")
                    .to_string(),
                "reasoning failed"
            );
            assert!(report.unfinished.is_empty());
        }
    }

    #[test]
    fn run_until_quiescent_reports_stable_blocked_tasks() {
        let context = EvalContext::standalone();
        let task = context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .unwrap();

        let EvaluationSessionRun::Deadlocked(report) = context.run_until_quiescent() else {
            panic!("an unchanged local blockage should be diagnosed as deadlock");
        };
        assert!(report.failures.is_empty());
        assert_eq!(report.unfinished.len(), 1);
        assert_eq!(report.unfinished[0].task, task.id());
        assert_eq!(
            report.unfinished[0].state,
            EvaluationUnfinishedState::Blocked
        );
        assert_eq!(
            report.unfinished[0].observed_epoch,
            Some(RuntimeObservationEpoch::from_raw(7))
        );
        assert_eq!(
            report.unfinished[0]
                .error
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("retryable evaluation error")
        );
    }

    #[test]
    fn live_cross_session_dependencies_are_reported_as_quiescent() {
        let fixture = SameRuntimeFixture::new();
        let owner = fixture.context();
        let dependency = owner
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("cross-session dependency should schedule");
        let observer = fixture.context();
        let dependency_wait = dependency.wait.clone();
        let follower = observer
            .schedule_task(move |task_context| {
                Ok(Box::new(Await {
                    context: task_context,
                    dependency: dependency_wait,
                }))
            })
            .expect("cross-session follower should schedule");

        let EvaluationSessionRun::Quiescent(report) = observer.run_until_quiescent() else {
            panic!("a live cross-session dependency should produce resumable quiescence")
        };
        let blocked = report
            .unfinished
            .iter()
            .find(|task| task.task == follower.id())
            .expect("cross-session follower should remain blocked");
        assert_eq!(blocked.dependency, Some(dependency.id()));
        assert_eq!(blocked.dependency_session, Some(dependency.session_id()));
        assert_eq!(blocked.wait, Some(dependency.wait.get()));

        let EvaluationSessionRun::Complete(owner_report) = owner.run_until_quiescent() else {
            panic!("the producer's session should complete independently")
        };
        assert!(owner_report.unfinished.is_empty());
        let EvaluationSessionRun::Complete(observer_report) = observer.run_until_quiescent() else {
            panic!("polling again should observe cross-session task completion")
        };
        assert!(observer_report.unfinished.is_empty());
    }

    #[test]
    fn task_owned_promise_dependency_reports_its_cross_session_producer() {
        let fixture = SameRuntimeFixture::new();
        let owner = fixture.context();
        let (promise, producer, _producer_context) = owner
            .task_owned_promise(Arc::from("reported task promise"))
            .expect("task-owned promise should register");
        let observer = fixture.context();
        let follower = observer
            .schedule_task({
                let promise = promise.clone();
                move |_| Ok(Box::new(AwaitPromise { promise }))
            })
            .expect("promise follower should schedule");

        let EvaluationSessionRun::Quiescent(report) = observer.run_until_quiescent() else {
            panic!("a live cross-session promise producer should remain quiescent")
        };
        let blocked = report
            .unfinished
            .iter()
            .find(|task| task.task == follower.id())
            .expect("the promise follower should remain blocked");
        assert_eq!(blocked.dependency, Some(producer.id()));
        assert_eq!(blocked.dependency_session, Some(owner.session_id()));
        assert_eq!(
            blocked.wait,
            promise.task().map(|producer| producer.wait().get())
        );
        assert_eq!(promise.exact_subscription_count(), 1);

        promise
            .set(observer.values().unit())
            .expect("the task-owned promise should resolve once");
        assert_eq!(promise.exact_subscription_count(), 0);
        let EvaluationSessionRun::Complete(report) = observer.run_until_quiescent() else {
            panic!("the exact promise wake should complete its follower")
        };
        assert!(report.unfinished.is_empty());
    }

    #[test]
    fn resolver_owned_promise_dependency_reports_no_synthetic_producer() {
        let context = EvalContext::standalone();
        let promise = PromisedValue::new(context.values(), "reported resolver promise");
        let follower = context
            .schedule_task({
                let promise = promise.clone();
                move |_| Ok(Box::new(AwaitPromise { promise }))
            })
            .expect("promise follower should schedule");

        let EvaluationSessionRun::Deadlocked(report) = context.run_until_quiescent() else {
            panic!("an unresolved host promise has no runnable producer")
        };
        let blocked = report
            .unfinished
            .iter()
            .find(|task| task.task == follower.id())
            .expect("the promise follower should remain blocked");
        assert_eq!(blocked.dependency, None);
        assert_eq!(blocked.dependency_session, None);
        assert_eq!(blocked.wait, None);
        assert_eq!(promise.exact_subscription_count(), 1);

        promise
            .fail_message("resolver promise failed")
            .expect("the resolver promise should fail once");
        assert_eq!(promise.exact_subscription_count(), 0);
        let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
            panic!("the exact promise wake should terminalize its follower")
        };
        assert!(report.failures.contains_key(&follower.id()));
    }

    #[test]
    fn exact_demand_can_poll_a_same_runtime_cross_session_dependency() {
        let fixture = SameRuntimeFixture::new();
        let owner = fixture.context();
        let dependency = owner
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("cross-session dependency should schedule");
        let observer = fixture.context();
        let dependency_wait = dependency.wait.clone();
        let follower = observer
            .schedule_task(move |task_context| {
                Ok(Box::new(Await {
                    context: task_context,
                    dependency: dependency_wait,
                }))
            })
            .expect("cross-session follower should schedule");

        assert_eq!(
            observer.pump_wait(follower.wait(), 256),
            EvaluationPumpOutcome::TargetReady,
            "exact demand should detach and poll one same-runtime producer through its owner"
        );
        assert!(matches!(
            observer.poll_wait(follower.wait()),
            EvaluationWaitPoll::Complete(_)
        ));
        assert!(matches!(
            owner.poll_wait(dependency.wait()),
            EvaluationWaitPoll::Complete(_)
        ));
    }

    #[test]
    fn exact_dependency_chain_retains_a_broad_observation_wake() {
        let context = EvalContext::standalone();
        let observed = context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .expect("observed dependency should schedule");
        let observed_wait = observed.wait.clone();
        let follower = context
            .schedule_task(move |task_context| {
                Ok(Box::new(Await {
                    context: task_context,
                    dependency: observed_wait,
                }))
            })
            .expect("observed follower should schedule");

        assert_eq!(
            context.pump_wait(follower.wait(), 256),
            EvaluationPumpOutcome::NoProgress
        );
        assert!(
            context
                .coordinator()
                .expect("test demand coordinator should be live")
                .dependency_observes_runtime(follower.wait()),
            "the synchronous facade must distinguish an observation wake from an orphaned wait"
        );
    }

    #[test]
    fn pending_cross_session_task_promise_does_not_spin_a_deferred_retry() {
        let fixture = SameRuntimeFixture::new();
        let owner = fixture.context();
        let (promise, _owner_task, _owner_context) = owner
            .task_owned_promise(Arc::from("pending cross-session task promise"))
            .expect("task promise should register");
        let dependency = promise
            .task()
            .expect("task promise should retain producer provenance")
            .wait()
            .clone();
        let observer = fixture.context();
        assert_ne!(dependency.owner_id(), observer.session_id());

        let lazy = inert_lazy("cross-session promise follower");
        let wait = observer
            .lazy_task(&lazy, move |task_context| {
                Box::new(Await {
                    context: task_context,
                    dependency,
                })
            })
            .expect("deferred follower should register");

        assert_eq!(
            observer.pump_wait(&wait, 256),
            EvaluationPumpOutcome::NoProgress,
            "a live cross-session task promise remains a dependency, not an orphaned wait"
        );
        assert!(matches!(
            observer.poll_wait(&wait),
            EvaluationWaitPoll::Pending(_)
        ));
    }

    #[test]
    fn a_strict_follower_turns_task_abandonment_into_a_failure() {
        let fixture = SameRuntimeFixture::new();
        let dependency_wait = {
            let owner = fixture.context();
            owner
                .schedule_task(|_| Ok(Box::new(Complete)))
                .expect("cross-session dependency should schedule")
                .wait
                .clone()
        };
        let observer = fixture.context();
        assert_eq!(
            observer.poll_wait(&dependency_wait),
            EvaluationWaitPoll::Abandoned
        );
        let follower = observer
            .schedule_task(move |task_context| {
                Ok(Box::new(Await {
                    context: task_context,
                    dependency: dependency_wait,
                }))
            })
            .expect("cross-session follower should schedule");

        let EvaluationSessionRun::Complete(report) = observer.run_until_quiescent() else {
            panic!("a closed producer session cannot leave retryable work")
        };
        let failure = report
            .failures
            .get(&follower.id())
            .expect("the cross-session follower should fail");
        assert!(
            failure.to_string().contains("was abandoned"),
            "strict waiting should turn abandonment into its own reportable failure"
        );
        assert!(report.unfinished.is_empty());
    }

    #[test]
    fn zero_worker_executor_drops_sparks_without_forcing_them() {
        let (coordinator, _executor) = test_execution_resources(0).unwrap();
        let session = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&session);
        let lazy = crate::core::LazyValue::deferred(
            &crate::core::test_value_factory(),
            "unforced spark",
            |_| panic!("zero-worker spark must never be evaluated"),
        );

        context.spark(Value::Lazy(lazy.clone()));

        assert!(lazy.cached().is_none());
        assert_eq!(
            coordinator.retained_spark_count(),
            0,
            "a zero-worker executor must discard sparks before retaining work"
        );
    }

    #[test]
    fn runtime_pump_follows_new_work_into_another_session() {
        let fixture = SameRuntimeFixture::new();
        let source = fixture.context();
        let target = fixture.context();
        let (signal, observed) = mpsc::channel();
        let target_context = EvalContext::clone(&target);
        let parent = source
            .schedule_task(move |_| {
                Ok(Box::new(SpawnSignal {
                    target: target_context,
                    signal: Some(signal),
                }))
            })
            .expect("cross-session spawning task should schedule");

        fixture.runtime.pump_until_stable();

        observed
            .recv_timeout(Duration::from_secs(2))
            .expect("runtime pump should execute the newly admitted target-session task");
        assert!(matches!(
            source.poll_reflection_task(&parent),
            EvaluationWaitPoll::Complete(_)
        ));
        assert_eq!(
            source
                .coordinator()
                .expect("coordinator should remain live")
                .ready_task_count(),
            0
        );
    }

    #[test]
    fn runtime_readiness_is_ready_when_no_work_is_retained() {
        let fixture = SameRuntimeFixture::new();

        let crate::api::RuntimeReadiness::Ready(first) = fixture.runtime.readiness() else {
            panic!("an idle runtime should be ready")
        };
        let crate::api::RuntimeReadiness::Ready(second) = fixture.runtime.readiness() else {
            panic!("an unchanged runtime should remain ready")
        };

        assert!(first.dispositions().is_empty());
        assert_eq!(first.stamp(), second.stamp());
        assert_eq!(first.reflection().root(), second.reflection().root());
        assert_eq!(first.runtime_id(), fixture.runtime.id());
    }

    #[test]
    fn runtime_deadlock_readiness_with_error_is_observational() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .expect("blocked task should schedule");
        fixture.runtime.pump_until_stable();

        let crate::api::RuntimeReadiness::Deadlocked(first) = fixture.runtime.readiness() else {
            panic!("retryable task error should produce a stable deadlock")
        };
        assert_eq!(first.unfinished().len(), 1);
        assert_eq!(
            first.unfinished()[0].blocked_error(),
            Some("retryable evaluation error")
        );
        let generation = context
            .coordinator()
            .expect("fixture coordinator should remain live")
            .work_generation();

        let crate::api::RuntimeReadiness::Deadlocked(second) = fixture.runtime.readiness() else {
            panic!("observing a deadlock must not disturb blocked work")
        };
        assert_eq!(first.stamp(), second.stamp());
        assert_eq!(second.stamp().work_generation(), generation);
        assert_eq!(
            context
                .coordinator()
                .expect("fixture coordinator should remain live")
                .work_generation(),
            generation,
            "projecting the blocked diagnostic must not create evaluation work"
        );

        let report = first
            .kill(crate::api::RuntimeKillReason::Deadlock)
            .settle()
            .expect("an unchanged deadlock snapshot should settle");
        assert_eq!(report.killed_work().len(), 1);
    }

    #[test]
    fn runtime_readiness_retains_exit_dispositions_without_settling_tasks() {
        let fixture = SameRuntimeFixture::new();
        let success_context = fixture.context();
        let error_context = fixture.context();
        let success = success_context
            .schedule_task(|_| {
                Ok(Box::new(ExitVote(EvaluationExitBlock {
                    intent: ExitIntent::Success,
                    observed_epoch: None,
                })))
            })
            .expect("success exit task should schedule");
        let message = RuntimeValueRoot::new(
            error_context.values(),
            Value::binary_from_text("exit message"),
        );
        let error = error_context
            .schedule_task(move |_| {
                Ok(Box::new(ExitVote(EvaluationExitBlock {
                    intent: ExitIntent::Error(message),
                    observed_epoch: None,
                })))
            })
            .expect("error exit task should schedule");

        fixture.runtime.pump_until_stable();
        let crate::api::RuntimeReadiness::Ready(snapshot) = fixture.runtime.readiness() else {
            panic!("two stable exit votes should make the runtime ready")
        };

        assert_eq!(snapshot.dispositions().len(), 2);
        assert!(snapshot.dispositions().iter().any(|disposition| {
            disposition.task_id() == Some(success.id().get())
                && matches!(
                    disposition.kind(),
                    crate::api::RuntimeDispositionKind::ExitSuccess
                )
        }));
        assert!(snapshot.dispositions().iter().any(|disposition| {
            disposition.task_id() == Some(error.id().get())
                && matches!(
                    disposition.kind(),
                    crate::api::RuntimeDispositionKind::ExitError(value)
                        if value.as_binary() == Some(b"exit message".as_slice())
                )
        }));
        assert!(matches!(
            success_context.poll_reflection_task(&success),
            EvaluationWaitPoll::Pending(_)
        ));
        assert!(matches!(
            error_context.poll_reflection_task(&error),
            EvaluationWaitPoll::Pending(_)
        ));
        let crate::api::RuntimeReadiness::Ready(repeated) = fixture.runtime.readiness() else {
            panic!("observing readiness must not settle exit votes")
        };
        assert_eq!(snapshot.stamp(), repeated.stamp());
        assert_eq!(snapshot.dispositions(), repeated.dispositions());
    }

    #[test]
    fn quiescence_validation_is_observational_and_rejects_stale_state() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let crate::api::RuntimeReadiness::Ready(snapshot) = fixture.runtime.readiness() else {
            panic!("idle runtime should produce a ready snapshot")
        };
        let generation = context
            .coordinator()
            .expect("fixture coordinator should remain live")
            .work_generation();
        let validated = snapshot
            .validate_without_settling()
            .expect("unchanged readiness should validate");
        assert_eq!(validated.work_generation, generation);
        assert!(validated.exits.is_empty());
        assert_eq!(
            context
                .coordinator()
                .expect("fixture coordinator should remain live")
                .work_generation(),
            generation,
            "validation must not mutate coordinator state"
        );

        context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("new work should schedule");
        assert_eq!(
            snapshot.validate_without_settling(),
            Err(crate::api::RuntimeSettlementError::RuntimeChanged)
        );
        assert!(matches!(
            snapshot.settle_after_validation(&validated),
            Err(crate::api::RuntimeSettlementError::RuntimeChanged)
        ));
    }

    #[test]
    fn ready_settlement_publishes_exited_once_and_retains_exit_errors() {
        let fixture = SameRuntimeFixture::new();
        let success_context = fixture.context();
        let error_context = fixture.context();
        let statuses = Arc::new(RecordedStatuses::default());
        let success = success_context
            .schedule_task(|_| {
                Ok(Box::new(ExitVote(EvaluationExitBlock {
                    intent: ExitIntent::Success,
                    observed_epoch: None,
                })))
            })
            .expect("success exit task should schedule");
        assert!(
            success_context
                .attach_task_status_publisher(&success, RecordedStatuses::publisher(&statuses),)
        );
        let message = RuntimeValueRoot::new(
            error_context.values(),
            Value::binary_from_text("retained exit failure"),
        );
        let error = error_context
            .schedule_task(move |_| {
                Ok(Box::new(ExitVote(EvaluationExitBlock {
                    intent: ExitIntent::Error(message),
                    observed_epoch: None,
                })))
            })
            .expect("error exit task should schedule");

        fixture.runtime.pump_until_stable();
        let crate::api::RuntimeReadiness::Ready(snapshot) = fixture.runtime.readiness() else {
            panic!("stable exit tasks should be ready")
        };
        let report = snapshot.settle().expect("ready snapshot should settle");

        assert_eq!(
            success_context.poll_reflection_task(&success),
            EvaluationWaitPoll::Exited
        );
        assert_eq!(
            error_context.poll_reflection_task(&error),
            EvaluationWaitPoll::Exited
        );
        assert_eq!(
            statuses
                .0
                .lock()
                .expect("recorded statuses were poisoned")
                .as_slice(),
            [EvaluationTaskStatus::Exited]
        );
        assert_eq!(report.dispositions(), snapshot.dispositions());
        assert!(report.task_failures().is_empty());
        assert!(report.dispositions().iter().any(|disposition| {
            matches!(
                disposition.kind(),
                crate::api::RuntimeDispositionKind::ExitError(value)
                    if value.as_binary() == Some(b"retained exit failure".as_slice())
            )
        }));
        assert!(
            matches!(
                snapshot.settle(),
                Err(crate::api::RuntimeSettlementError::RuntimeChanged)
            ),
            "a settled exit obligation must not publish twice"
        );
        assert!(matches!(
            fixture.runtime.readiness(),
            crate::api::RuntimeReadiness::Ready(_)
        ));
    }

    #[test]
    fn forced_deadlock_settlement_preserves_exits_and_kills_other_participants() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let child = context
            .schedule_task(|_| {
                Ok(Box::new(ExitVote(EvaluationExitBlock {
                    intent: ExitIntent::Success,
                    observed_epoch: None,
                })))
            })
            .expect("exit-voting child should schedule");
        let child_wait = child.wait().clone();
        let parent = context
            .schedule_task(move |task_context| {
                Ok(Box::new(Await {
                    context: task_context,
                    dependency: child_wait,
                }))
            })
            .expect("strict parent should schedule");
        let promise = PromisedValue::new(context.values(), "killed client promise");
        let client = context
            .demand_whnf(RuntimeValueRoot::new(
                context.values(),
                Value::Promised(promise),
            ))
            .expect("client demand should admit");

        fixture.runtime.pump_until_stable();
        let crate::api::RuntimeReadiness::Deadlocked(deadlock) = fixture.runtime.readiness() else {
            panic!("strict join and unresolved client demand should deadlock")
        };
        let killed_details = deadlock.unfinished().to_vec();
        let forced = deadlock.kill(crate::api::RuntimeKillReason::Deadlock);
        assert_eq!(forced.dispositions().len(), 3);
        assert!(forced.dispositions().iter().any(|disposition| {
            disposition.task_id() == Some(child.id().get())
                && matches!(
                    disposition.kind(),
                    crate::api::RuntimeDispositionKind::ExitSuccess
                )
        }));
        assert!(forced.dispositions().iter().any(|disposition| {
            disposition.task_id() == Some(parent.id().get())
                && matches!(
                    disposition.kind(),
                    crate::api::RuntimeDispositionKind::Killed(
                        crate::api::RuntimeKillReason::Deadlock
                    )
                )
        }));
        assert!(forced.dispositions().iter().any(|disposition| {
            disposition.task_id().is_none()
                && matches!(
                    disposition.kind(),
                    crate::api::RuntimeDispositionKind::Killed(
                        crate::api::RuntimeKillReason::Deadlock
                    )
                )
        }));

        let report = forced
            .settle()
            .expect("unchanged forced deadlock should settle");
        assert_eq!(report.killed_work(), killed_details);
        assert!(report.task_failures().is_empty());
        assert_eq!(
            context.poll_reflection_task(&child),
            EvaluationWaitPoll::Exited
        );
        let EvaluationWaitPoll::Killed(parent_failure) = context.poll_reflection_task(&parent)
        else {
            panic!("forced parent should publish a killed terminal")
        };
        assert!(matches!(
            parent_failure.emission_value(),
            Some(Value::Dict(_))
        ));
        assert_eq!(
            parent_failure.to_string(),
            "runtime killed work in a deadlocked settlement"
        );
        let Some(ClientDemandResult::Killed(client_failure)) = client.poll() else {
            panic!("forced client demand should receive a killed result")
        };
        assert_eq!(client_failure, parent_failure);
        assert!(matches!(
            fixture.runtime.readiness(),
            crate::api::RuntimeReadiness::Ready(_)
        ));

        let later = context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("settlement must not seal the runtime");
        fixture.runtime.pump_until_stable();
        assert!(matches!(
            context.poll_reflection_task(&later),
            EvaluationWaitPoll::Complete(_)
        ));
        assert_eq!(report.killed_work(), killed_details);
    }

    #[test]
    fn forced_kill_abandons_a_deferred_lazy_claim_without_poisoning_the_lazy() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let expected = context.values().unit();
        let lazy = LazyValue::deferred(context.values(), "reclaim after forced kill", {
            let expected = expected.clone();
            move |_| Ok(expected.clone())
        });
        let wait = context
            .lazy_task(&lazy, |_| Box::new(AlwaysBlocked))
            .expect("dormant deferred claim should register");

        let crate::api::RuntimeReadiness::Deadlocked(deadlock) = fixture.runtime.readiness() else {
            panic!("dormant deferred work should be a stable anomaly")
        };
        assert!(deadlock.unfinished().iter().any(|work| {
            work.kind() == crate::api::RuntimeWorkKind::DeferredEvaluation
                && work.state() == crate::api::RuntimeWorkState::Dormant
        }));
        let report = deadlock
            .kill(crate::api::RuntimeKillReason::Deadlock)
            .settle()
            .expect("dormant deferred work should be killable");
        assert_eq!(report.killed_work().len(), 1);
        assert!(matches!(
            context.poll_wait(&wait),
            EvaluationWaitPoll::Killed(_)
        ));
        assert!(lazy.cached().is_none());
        assert_deferred_task_retired(&context, &lazy);
        assert_eq!(
            crate::eval::eval_value(&context, &Value::Lazy(lazy.clone()))
                .expect("a later demand should reclaim the lazy source"),
            expected
        );
        assert!(lazy.cached().is_some_and(|result| result.is_ok()));
    }

    #[test]
    fn forced_kill_publishes_task_status_and_fails_owned_promises() {
        struct BlockedWithDropCheck {
            coordinator: Weak<EvaluationWorkCoordinator>,
            dropped_without_runtime_locks: Arc<AtomicBool>,
        }

        impl EvaluationTaskMachine for BlockedWithDropCheck {
            fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
                EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    dependency: None,
                    observed_epoch: Some(RuntimeObservationEpoch::from_raw(7)),
                    error: None,
                })
            }
        }

        impl Drop for BlockedWithDropCheck {
            fn drop(&mut self) {
                let unlocked = self
                    .coordinator
                    .upgrade()
                    .is_none_or(|coordinator| coordinator.runtime_locks_are_free());
                self.dropped_without_runtime_locks
                    .store(unlocked, Ordering::Release);
            }
        }

        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context
            .coordinator()
            .expect("coordinator should remain live");
        let dropped_without_runtime_locks = Arc::new(AtomicBool::new(false));
        let drop_check = dropped_without_runtime_locks.clone();
        let weak_coordinator = Arc::downgrade(&coordinator);
        let promise_output = Arc::new(Mutex::new(None));
        let output = promise_output.clone();
        let task = context
            .schedule_task(move |task_context| {
                let promise = PromisedValue::fixpoint(&task_context, "killed owned promise")?;
                *output.lock().expect("promise output was poisoned") = Some(promise);
                Ok(Box::new(BlockedWithDropCheck {
                    coordinator: weak_coordinator,
                    dropped_without_runtime_locks: drop_check,
                }))
            })
            .expect("blocked promise owner should schedule");
        let promise = promise_output
            .lock()
            .expect("promise output was poisoned")
            .clone()
            .expect("task construction should expose its promise");
        let statuses = Arc::new(RecordedStatuses::default());
        assert!(
            context.attach_task_status_publisher(&task, RecordedStatuses::publisher(&statuses),)
        );

        fixture.runtime.pump_until_stable();
        let crate::api::RuntimeReadiness::Deadlocked(deadlock) = fixture.runtime.readiness() else {
            panic!("blocked promise owner should deadlock")
        };
        deadlock
            .kill(crate::api::RuntimeKillReason::Deadlock)
            .settle()
            .expect("blocked promise owner should settle as killed");

        let EvaluationWaitPoll::Killed(task_failure) = context.poll_reflection_task(&task) else {
            panic!("task wait should expose the killed terminal")
        };
        let promise_failure = promise
            .assignment()
            .expect("owned promise should receive a terminal assignment")
            .expect_err("owned promise should fail when its producer is killed");
        assert_eq!(promise_failure, task_failure);
        assert!(matches!(
            statuses
                .0
                .lock()
                .expect("recorded statuses were poisoned")
                .last(),
            Some(EvaluationTaskStatus::Killed(failure)) if failure == &task_failure
        ));
        assert!(dropped_without_runtime_locks.load(Ordering::Acquire));
    }

    #[test]
    fn forced_deadlock_settlement_rejects_a_stale_busy_runtime() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let blocked = context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .expect("blocked task should schedule");
        fixture.runtime.pump_until_stable();
        let crate::api::RuntimeReadiness::Deadlocked(deadlock) = fixture.runtime.readiness() else {
            panic!("blocked task should deadlock")
        };
        let forced = deadlock.kill(crate::api::RuntimeKillReason::Deadlock);

        context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("new runnable work should disturb the deadlock");
        assert!(matches!(
            fixture.runtime.readiness(),
            crate::api::RuntimeReadiness::Busy
        ));
        assert!(matches!(
            forced.settle(),
            Err(crate::api::RuntimeSettlementError::RuntimeChanged)
        ));
        assert!(matches!(
            context.poll_reflection_task(&blocked),
            EvaluationWaitPoll::Pending(_)
        ));
    }

    #[test]
    fn exit_settlement_fails_owned_promises_and_drops_reusable_machine_after_unlock() {
        struct ExitWithDropCheck {
            coordinator: Weak<EvaluationWorkCoordinator>,
            dropped_without_runtime_locks: Arc<AtomicBool>,
        }

        impl EvaluationTaskMachine for ExitWithDropCheck {
            fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
                EvaluationMachinePoll::Exit(EvaluationExitBlock {
                    intent: ExitIntent::Success,
                    observed_epoch: Some(RuntimeObservationEpoch::from_raw(1)),
                })
            }
        }

        impl Drop for ExitWithDropCheck {
            fn drop(&mut self) {
                let unlocked = self
                    .coordinator
                    .upgrade()
                    .is_none_or(|coordinator| coordinator.runtime_locks_are_free());
                self.dropped_without_runtime_locks
                    .store(unlocked, Ordering::Release);
            }
        }

        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let promise_output = Arc::new(Mutex::new(None));
        let output = promise_output.clone();
        let dropped_without_runtime_locks = Arc::new(AtomicBool::new(false));
        let drop_check = dropped_without_runtime_locks.clone();
        let coordinator = context
            .coordinator()
            .expect("coordinator should remain live");
        let weak_coordinator = Arc::downgrade(&coordinator);
        let task = context
            .schedule_task(move |task_context| {
                let promise = PromisedValue::fixpoint(&task_context, "exit-owned promise")?;
                *output.lock().expect("promise output was poisoned") = Some(promise);
                Ok(Box::new(ExitWithDropCheck {
                    coordinator: weak_coordinator,
                    dropped_without_runtime_locks: drop_check,
                }))
            })
            .expect("exit promise task should schedule");
        let promise = promise_output
            .lock()
            .expect("promise output was poisoned")
            .clone()
            .expect("task construction should expose its promise");

        fixture.runtime.pump_until_stable();
        let crate::api::RuntimeReadiness::Ready(snapshot) = fixture.runtime.readiness() else {
            panic!("exit promise task should become ready")
        };
        snapshot.settle().expect("exit promise task should settle");

        assert_eq!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Exited
        );
        let promise_failure = promise
            .assignment()
            .expect("settlement should terminalize the owned promise")
            .expect_err("an unfulfilled exit-owned promise must fail");
        assert!(
            promise_failure
                .to_string()
                .contains("exited without fulfilling")
        );
        assert!(dropped_without_runtime_locks.load(Ordering::Acquire));
    }

    #[test]
    fn runtime_deadlock_retains_typed_task_and_client_dependencies() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let child = context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .expect("blocked child should schedule");
        let child_wait = child.wait().clone();
        let parent = context
            .schedule_task(move |task_context| {
                Ok(Box::new(Await {
                    context: task_context,
                    dependency: child_wait,
                }))
            })
            .expect("strict joining parent should schedule");
        let promise = PromisedValue::new(context.values(), "deadlocked client promise");
        let client = context
            .demand_whnf(RuntimeValueRoot::new(
                context.values(),
                Value::Promised(promise.clone()),
            ))
            .expect("client demand should admit");

        fixture.runtime.pump_until_stable();
        let crate::api::RuntimeReadiness::Deadlocked(snapshot) = fixture.runtime.readiness() else {
            panic!("blocked task, join, and client demand should deadlock")
        };

        assert!(snapshot.dispositions().is_empty());
        assert_eq!(snapshot.unfinished().len(), 3);
        let parent_work = snapshot
            .unfinished()
            .iter()
            .find(|work| work.task_id() == Some(parent.id().get()))
            .expect("strict join should appear in the deadlock report");
        assert!(matches!(
            parent_work.dependency(),
            Some(crate::api::RuntimeDependency::TaskWait {
                task_id,
                session_id,
                ..
            }) if *task_id == child.id().get() && *session_id == context.session_id().get()
        ));
        let client_work = snapshot
            .unfinished()
            .iter()
            .find(|work| work.kind() == crate::api::RuntimeWorkKind::ClientDemand)
            .expect("blocked client demand should remain a distinct participant");
        assert!(matches!(
            client_work.dependency(),
            Some(crate::api::RuntimeDependency::Promise {
                promise_id,
                producer: None,
            }) if *promise_id == promise.id().get()
        ));
        assert!(snapshot.unfinished().iter().any(|work| {
            work.task_id() == Some(child.id().get())
                && work.observed_epoch() == Some(7)
                && work.state() == crate::api::RuntimeWorkState::Blocked
        }));

        let retained = snapshot.clone();
        promise
            .set(context.values().unit())
            .expect("host promise should resolve once");
        assert!(matches!(
            fixture.runtime.readiness(),
            crate::api::RuntimeReadiness::Busy
        ));
        context.complete_wait(child.wait());
        fixture.runtime.pump_until_stable();
        assert!(matches!(
            fixture.runtime.readiness(),
            crate::api::RuntimeReadiness::Ready(_)
        ));
        assert_eq!(retained.unfinished().len(), 3);
        assert!(matches!(
            client.poll(),
            Some(ClientDemandResult::Complete(_))
        ));
    }

    #[test]
    fn dormant_and_reserved_work_are_reported_as_deadlock_anomalies() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        let lazy = inert_lazy_for(context.values(), "dormant readiness producer");
        let deferred_wait = context
            .lazy_task(&lazy, |_| Box::new(Complete))
            .expect("dormant deferred work should register");
        let task = allocate_task_id(context.values()).expect("task ID should allocate");
        let wait = allocate_wait_token(&context.session, task).expect("wait ID should allocate");
        coordinator
            .reserve_reflection(&context.session, task, wait.clone())
            .expect("reserved reflection work should register");

        let crate::api::RuntimeReadiness::Deadlocked(snapshot) = fixture.runtime.readiness() else {
            panic!("orphaned dormant and reserved records should be visible deadlocks")
        };
        assert!(snapshot.unfinished().iter().any(|work| {
            work.kind() == crate::api::RuntimeWorkKind::DeferredEvaluation
                && work.state() == crate::api::RuntimeWorkState::Dormant
        }));
        assert!(snapshot.unfinished().iter().any(|work| {
            work.task_id() == Some(task.get())
                && work.kind() == crate::api::RuntimeWorkKind::ReflectionTask
                && work.state() == crate::api::RuntimeWorkState::Reserved
        }));
        let report = snapshot
            .kill(crate::api::RuntimeKillReason::Deadlock)
            .settle()
            .expect("anomalous dormant and reserved work should be forcefully settleable");
        assert_eq!(report.killed_work().len(), 2);
        assert!(matches!(
            context.poll_wait(&deferred_wait),
            EvaluationWaitPoll::Killed(_)
        ));
        assert!(matches!(
            context.poll_wait(&wait),
            EvaluationWaitPoll::Killed(_)
        ));
    }

    #[test]
    fn readiness_reports_runnable_and_unclaimed_spark_work_as_busy() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("queued task should schedule");
        assert!(matches!(
            fixture.runtime.readiness(),
            crate::api::RuntimeReadiness::Busy
        ));
        fixture.runtime.pump_until_stable();

        let coordinator = context.coordinator().expect("coordinator should be live");
        coordinator.executor_started(1);
        context.spark(Value::Lazy(inert_lazy_for(
            context.values(),
            "readiness spark",
        )));
        assert!(matches!(
            fixture.runtime.readiness(),
            crate::api::RuntimeReadiness::Busy
        ));
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
        fixture.runtime.pump_until_stable();
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 0));
    }

    #[test]
    fn readiness_reports_terminalizing_work_as_busy_without_mutating_it() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("terminalizing fixture should schedule");
        let ClaimedTaskWork::Reflection(claimed) = coordinator
            .claim_ready_task_for_session(context.session_id())
            .expect("queued reflection work should be claimable")
        else {
            panic!("fixture should claim reflection work")
        };
        let work = claimed.id();
        let mut release = coordinator.release_reflection(claimed, ReflectionWorkPoll::Terminal);
        assert!(release.terminal);

        assert!(matches!(
            fixture.runtime.readiness(),
            crate::api::RuntimeReadiness::Busy
        ));
        assert!(matches!(
            coordinator.runtime_readiness_snapshot(),
            RuntimeCoordinatorReadiness::Busy
        ));

        settle_task_work(
            &coordinator,
            work,
            EvaluationTaskState::Complete(RuntimeValueRoot::new(
                context.values(),
                crate::core::keys::unit_value(),
            )),
            evaluation_failure("terminalizing fixture completed without a fixpoint"),
        );
        let retired = coordinator.retire_reflection(work);
        drop(release.machine.take());
        drop(retired);
    }

    #[test]
    fn runtime_pump_parks_while_a_worker_owns_progress() {
        let fixture = SameRuntimeFixture::new();
        fixture
            .runtime
            .activate_workers(1)
            .expect("test worker should activate");
        let context = fixture.context();
        let (started, worker_started) = mpsc::channel();
        let (release, worker_release) = mpsc::channel();
        context
            .schedule_task(move |_| {
                Ok(Box::new(CompleteAfterRelease {
                    started: Some(started),
                    release: worker_release,
                }))
            })
            .expect("worker-owned task should schedule");
        worker_started
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should claim the task");
        assert!(matches!(
            fixture.runtime.readiness(),
            crate::api::RuntimeReadiness::Busy
        ));

        let runtime = fixture.runtime.clone();
        let (finished, pump_finished) = mpsc::channel();
        let pump = std::thread::spawn(move || {
            runtime.pump_until_stable();
            finished.send(()).expect("pump receiver should remain live");
        });
        assert!(
            pump_finished
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "runtime pump must park rather than report stability or spin past worker-owned work"
        );

        release.send(()).expect("worker should remain live");
        pump_finished
            .recv_timeout(Duration::from_secs(2))
            .expect("worker release should wake the parked runtime pump");
        pump.join().expect("runtime pump should finish cleanly");
    }

    #[test]
    fn runtime_pump_abandons_queued_and_blocked_sparks() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        coordinator.executor_started(1);

        context.spark(Value::Lazy(inert_lazy_for(
            context.values(),
            "queued runtime-pump spark",
        )));
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
        fixture.runtime.pump_until_stable();
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 0));

        let promise = PromisedValue::new(context.values(), "blocked runtime-pump spark");
        context.spark(Value::Promised(promise.clone()));
        park_next_spark(&coordinator);
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 1));
        fixture.runtime.pump_until_stable();
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 0));
        promise
            .set(context.values().unit())
            .expect("retired spark dependency may complete harmlessly");
        assert_eq!(coordinator.retained_spark_count(), 0);
    }

    #[test]
    fn runtime_pump_snapshot_is_observational() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        coordinator.executor_started(1);
        context.spark(Value::Lazy(inert_lazy_for(
            context.values(),
            "snapshot-retained spark",
        )));
        let generation = coordinator.work_generation();

        let snapshot = coordinator.runtime_pump_snapshot();

        assert!(snapshot.abandonable_sparks);
        assert_eq!(coordinator.work_generation(), generation);
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
        assert_eq!(coordinator.abandon_quiescent_sparks(), 1);
    }

    #[test]
    fn spark_abandonment_wakes_useful_work_for_another_pump_pass() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        coordinator.executor_started(1);
        let promise = PromisedValue::new(context.values(), "spark-owned deferred wait");
        let followed = promise.clone();
        let lazy = LazyValue::deferred(context.values(), "spark-owned lazy claim", move |_| {
            Ok(Value::Promised(followed.clone()))
        });
        let wait = context
            .lazy_task(&lazy, {
                let promise = promise.clone();
                move |_| Box::new(AwaitPromise { promise })
            })
            .expect("deferred producer should register");
        assert!(coordinator.promote_deferred_wait(&wait));
        let waiter = context
            .schedule_task({
                let wait = wait.clone();
                move |task_context| {
                    Ok(Box::new(Await {
                        context: task_context,
                        dependency: wait,
                    }))
                }
            })
            .expect("useful waiter should schedule");

        context.spark(Value::Lazy(inert_lazy_for(
            context.values(),
            "manually blocked spark",
        )));
        let claimed = claim_next_spark(&coordinator);
        coordinator.release_spark(
            claimed,
            coordinator::SparkWorkPoll::Blocked(coordinator::WorkDependency::Wait(wait)),
        );

        fixture.runtime.pump_until_stable();

        assert!(matches!(
            context.poll_reflection_task(&waiter),
            EvaluationWaitPoll::Failed(_)
        ));
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 0));
        assert_eq!(coordinator.ready_task_count(), 0);
    }

    #[test]
    fn runtime_pump_does_not_abandon_a_worker_owned_spark() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        coordinator.executor_started(1);
        context.spark(Value::Lazy(inert_lazy_for(
            context.values(),
            "worker-owned runtime-pump spark",
        )));
        let claimed = claim_next_spark(&coordinator);
        assert_eq!(coordinator.spark_work_counts(), (0, 1, 0));
        assert!(matches!(
            fixture.runtime.readiness(),
            crate::api::RuntimeReadiness::Busy
        ));

        let runtime = fixture.runtime.clone();
        let (finished, pump_finished) = mpsc::channel();
        let pump = std::thread::spawn(move || {
            runtime.pump_until_stable();
            finished.send(()).expect("pump receiver should remain live");
        });
        assert!(
            pump_finished
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "running spark must keep runtime pumping active"
        );
        assert_eq!(coordinator.spark_work_counts(), (0, 1, 0));

        coordinator.release_spark(claimed, coordinator::SparkWorkPoll::Complete);
        pump_finished
            .recv_timeout(Duration::from_secs(2))
            .expect("returning spark worker should wake the pump");
        pump.join().expect("runtime pump should finish cleanly");
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 0));
    }

    fn wait_for_spark_work_counts(
        coordinator: &EvaluationWorkCoordinator,
        expected: (usize, usize, usize),
        message: &str,
    ) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while coordinator.spark_work_counts() != expected && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(coordinator.spark_work_counts(), expected, "{message}");
    }

    fn park_next_spark(coordinator: &EvaluationWorkCoordinator) {
        let claimed = claim_next_spark(coordinator);
        let context = EvalContext::for_spark(claimed.demand_session());
        let halt = crate::eval::eval_value(&context, claimed.value().as_core())
            .expect_err("the unresolved promise should park its spark follower");
        let dependency = if let Some(wait) = halt.blocked_on() {
            coordinator::WorkDependency::Wait(wait.0)
        } else if let Some(promise) = halt.unassigned_promise() {
            coordinator::WorkDependency::Promise(promise.clone())
        } else {
            panic!("an unresolved promise should expose a retryable dependency")
        };
        coordinator.release_spark(claimed, coordinator::SparkWorkPoll::Blocked(dependency));
    }

    fn claim_next_spark(coordinator: &EvaluationWorkCoordinator) -> coordinator::ClaimedSparkWork {
        loop {
            match coordinator.select() {
                coordinator::CoordinatorSelection::Spark(claimed) => return claimed,
                coordinator::CoordinatorSelection::Task(work) => {
                    coordinator.requeue_unpolled_task(work);
                }
                coordinator::CoordinatorSelection::ClientDemand(claimed) => {
                    coordinator.requeue_unpolled_client_demand(claimed);
                }
                coordinator::CoordinatorSelection::None => {
                    panic!("the submitted spark should be claimable")
                }
            }
        }
    }

    #[test]
    fn one_promise_completion_wakes_exact_sparks_in_multiple_sessions() {
        let fixture = SameRuntimeFixture::new();
        let left = fixture.context();
        let right = fixture.context();
        let coordinator = left.coordinator().expect("coordinator should be live");
        coordinator.executor_started(2);
        let promise = PromisedValue::new(left.values(), "shared host promise");

        for context in [&left, &right] {
            context.spark(Value::Promised(promise.clone()));
            park_next_spark(&coordinator);
        }

        assert_eq!(promise.exact_subscription_count(), 2);
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 2));
        promise
            .set(left.values().unit())
            .expect("the shared host promise should resolve once");
        assert_eq!(
            coordinator.spark_work_counts(),
            (2, 0, 0),
            "one publication must disturb every session which followed the promise"
        );

        for _ in 0..2 {
            let coordinator::CoordinatorSelection::Spark(claimed) = coordinator.select() else {
                panic!("both disturbed promise sparks should become runnable")
            };
            coordinator.release_spark(claimed, coordinator::SparkWorkPoll::Complete);
        }
    }

    #[test]
    fn promise_completion_wakes_only_sparks_parked_on_that_promise() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        coordinator.executor_started(2);
        let promise_a = PromisedValue::new(context.values(), "promise A");
        let promise_b = PromisedValue::new(context.values(), "promise B");

        context.spark(Value::Promised(promise_a.clone()));
        park_next_spark(&coordinator);
        context.spark(Value::Promised(promise_b.clone()));
        park_next_spark(&coordinator);
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 2));

        promise_a
            .set(context.values().unit())
            .expect("promise A should resolve once");
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 1));
        assert_eq!(promise_b.exact_subscription_count(), 1);

        let coordinator::CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("promise A should wake its own spark")
        };
        coordinator.release_spark(claimed, coordinator::SparkWorkPoll::Complete);
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 1));

        promise_b
            .set(context.values().unit())
            .expect("promise B should resolve once");
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
        let coordinator::CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("promise B should wake its own spark")
        };
        coordinator.release_spark(claimed, coordinator::SparkWorkPoll::Complete);
    }

    #[test]
    fn promise_completion_between_demand_and_subscription_requeues_the_spark() {
        let fixture = SameRuntimeFixture::new();
        let context = fixture.context();
        let coordinator = context.coordinator().expect("coordinator should be live");
        coordinator.executor_started(1);
        let promise = PromisedValue::new(context.values(), "racing promise");
        context.spark(Value::Promised(promise.clone()));

        let coordinator::CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("the promise spark should be claimable")
        };
        let spark_context = EvalContext::for_spark(claimed.demand_session());
        let halt = crate::eval::eval_value(&spark_context, claimed.value().as_core())
            .expect_err("the unresolved promise should halt the spark");
        let dependency = coordinator::WorkDependency::Promise(
            halt.unassigned_promise()
                .expect("the halt should preserve the promise")
                .clone(),
        );
        promise
            .set(context.values().unit())
            .expect("the promise should resolve before subscription");

        coordinator.release_spark(claimed, coordinator::SparkWorkPoll::Blocked(dependency));
        assert_eq!(promise.exact_subscription_count(), 0);
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
        let coordinator::CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("terminal recheck should requeue the spark")
        };
        coordinator.release_spark(claimed, coordinator::SparkWorkPoll::Complete);
    }

    #[test]
    fn wait_completion_wakes_only_its_exact_spark_after_unrelated_task_progress() {
        let (coordinator, _executor) = test_execution_resources(0).unwrap();
        let session = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&session);
        coordinator.executor_started(1);

        let unrelated = context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("unrelated task should schedule");
        let wait_a = context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("wait A producer should schedule");
        let wait_b = context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("wait B producer should schedule");

        let coordinator::CoordinatorSelection::Task(claimed) = coordinator.select() else {
            panic!("the unrelated reflection task should be selected first")
        };
        coordinator.requeue_unpolled_task(claimed);
        for wait in [wait_a.wait(), wait_b.wait()] {
            context.spark(Value::Lazy(LazyValue::deferred(
                context.values(),
                "manually parked wait spark",
                |_| panic!("the coordinator test parks this demand before evaluation"),
            )));
            let claimed = claim_next_spark(&coordinator);
            coordinator.release_spark(
                claimed,
                coordinator::SparkWorkPoll::Blocked(coordinator::WorkDependency::Wait(
                    wait.clone(),
                )),
            );
        }
        assert_eq!(wait_a.wait().exact_subscription_count(), 1);
        assert_eq!(wait_b.wait().exact_subscription_count(), 1);
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 2));

        assert_eq!(
            context.pump_wait(unrelated.wait(), 256),
            EvaluationPumpOutcome::TargetReady
        );
        assert_eq!(
            coordinator.spark_work_counts(),
            (0, 0, 2),
            "unrelated task progress must not retry wait-blocked sparks"
        );

        assert_eq!(
            context.pump_wait(wait_a.wait(), 256),
            EvaluationPumpOutcome::TargetReady
        );
        assert_eq!(wait_a.wait().exact_subscription_count(), 0);
        assert_eq!(wait_b.wait().exact_subscription_count(), 1);
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 1));
        let claimed = claim_next_spark(&coordinator);
        coordinator.release_spark(claimed, coordinator::SparkWorkPoll::Complete);

        assert_eq!(
            context.pump_wait(wait_b.wait(), 256),
            EvaluationPumpOutcome::TargetReady
        );
        assert_eq!(wait_b.wait().exact_subscription_count(), 0);
        assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
        let claimed = claim_next_spark(&coordinator);
        coordinator.release_spark(claimed, coordinator::SparkWorkPoll::Complete);
    }

    #[test]
    fn permanent_spark_failure_retires_without_a_dependency_subscription() {
        let (coordinator, _executor) = test_execution_resources(0).unwrap();
        let session = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&session);
        coordinator.executor_started(1);
        context.spark(Value::error(context.values(), "spark failure"));

        let coordinator::CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("the failing spark should be claimable")
        };
        let halt = crate::eval::demand_strategy_value(&context, claimed.value().as_core())
            .expect_err("the spark fixture should fail permanently");
        assert!(halt.permanent_failure().is_some());
        assert!(halt.blocked_on().is_none());
        assert!(halt.unassigned_promise().is_none());
        coordinator.release_spark(claimed, coordinator::SparkWorkPoll::Complete);

        assert_eq!(coordinator.retained_spark_count(), 0);
    }

    #[test]
    fn closing_a_session_abandons_a_blocked_spark_and_releases_its_lazy_claim() {
        let (coordinator, _executor) = test_execution_resources(1).unwrap();
        let session = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&session);
        let promise = PromisedValue::new(context.values(), "blocked spark assignment");
        let followed_promise = promise.clone();
        let lazy = LazyValue::deferred(context.values(), "reusable spark claim", move |context| {
            crate::eval::eval_value(context, &Value::Promised(followed_promise.clone()))
        });
        context.spark(Value::Lazy(lazy.clone()));
        wait_for_spark_work_counts(
            &coordinator,
            (0, 0, 1),
            "the unresolved lazy demand should park its stable spark record",
        );
        assert!(context.task_registry_counts().deferred_active > 0);

        drop(session);

        wait_for_spark_work_counts(
            &coordinator,
            (0, 0, 0),
            "closing the demand session should immediately abandon blocked sparks",
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while coordinator.deferred_counts(context.session.id).0 != 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(
            coordinator.deferred_counts(context.session.id).0,
            0,
            "spark abandonment or the returning worker must release the reusable deferred claim"
        );
        assert!(lazy.cached().is_none());

        promise
            .set(context.values().unit())
            .expect("host promise should accept its assignment");
        let observer_session = EvaluationSession::shared(&coordinator);
        let observer = EvalContext::patient_with_task_profile(
            &observer_session,
            observer_session.demand.default_reflection_profile.clone(),
        );
        assert_eq!(
            crate::eval::eval_value(&observer, &Value::Lazy(lazy)),
            Ok(context.values().unit()),
            "a later demand must be able to reclaim the abandoned lazy"
        );
    }

    #[test]
    fn closing_a_session_keeps_worker_owned_spark_work_busy_until_release() {
        let (coordinator, _executor) = test_execution_resources(0).unwrap();
        let session = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&session);
        coordinator.executor_started(1);
        let lazy = inert_lazy_for(context.values(), "worker-owned spark");
        context.spark(Value::Lazy(lazy));
        let coordinator::CoordinatorSelection::Spark(claimed) = coordinator.select() else {
            panic!("the test worker should claim the spark before session closure")
        };
        assert_eq!(coordinator.spark_work_counts(), (0, 1, 0));

        let closed_session = session.demand.id;
        drop(session);
        assert!(context.session.is_closed());
        let repeated = coordinator.close_session(closed_session);
        assert!(repeated.reflection.is_empty() && repeated.deferred.is_empty());

        assert_eq!(
            coordinator.spark_work_counts(),
            (0, 1, 0),
            "a close request must retain worker-owned work and its session index"
        );
        coordinator.release_spark(claimed, coordinator::SparkWorkPoll::Complete);
        assert_eq!(coordinator.spark_work_counts(), (0, 0, 0));
    }

    #[test]
    fn executor_shutdown_explicitly_abandons_dependency_blocked_sparks() {
        let (coordinator, executor) = test_execution_resources(1).unwrap();
        let session = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&session);
        context.spark(Value::Promised(PromisedValue::new(
            context.values(),
            "executor shutdown spark",
        )));
        wait_for_spark_work_counts(
            &coordinator,
            (0, 0, 1),
            "the promise spark should park before executor shutdown",
        );

        drop(executor);

        wait_for_spark_work_counts(
            &coordinator,
            (0, 0, 0),
            "executor shutdown must explicitly abandon parked spark records",
        );
        assert_eq!(context.task_registry_counts().deferred_active, 0);
    }

    #[test]
    fn workers_force_sparks_and_poll_ready_reflection_tasks() {
        let (coordinator, _executor) = test_execution_resources(1).unwrap();
        let session = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&session);
        let (spark_sender, spark_receiver) = mpsc::channel();
        let lazy = crate::core::LazyValue::deferred(
            &crate::core::test_value_factory(),
            "worker spark",
            move |_| {
                spark_sender
                    .send(())
                    .expect("spark receiver should remain open");
                Ok(crate::core::keys::unit_value())
            },
        );
        context.spark(Value::Lazy(lazy));
        spark_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should force queued spark");

        let (task_sender, task_receiver) = mpsc::channel();
        context
            .schedule_task(move |_| Ok(Box::new(Signal(Some(task_sender)))))
            .expect("worker task should schedule");
        task_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should poll ready reflection task");
    }
}
