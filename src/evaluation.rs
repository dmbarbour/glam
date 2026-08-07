//! Session-scoped capabilities threaded through semantic evaluation.
//!
//! The runtime supplies task and wait identity, value provenance, and the
//! authoritative reflection-task lifecycle. During the work-boundary
//! transition, the runtime coordinator owns opaque reflection and deferred
//! machines directly with their lifecycle records. A session reporting store
//! retains only acknowledgement and status plumbing. Demand state retains its
//! serial cooperative pump. Reflection specializations remain outside this
//! module behind a small type-erased task-machine boundary.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use rpds::RedBlackTreeMapSync;

use crate::core::{
    CoreValueFactory, EvaluationFailure, LazyCycle, LazyCycleMember, LazyValue, PromiseAssignment,
    PromiseCell, PromiseId, PromisedValue, Value,
};
use crate::runtime::{EvaluationRuntimeId, RuntimeValueRoot};

mod coordinator;
mod executor;
#[cfg(test)]
use coordinator::test_wake_registration;
use coordinator::{
    ClaimedDeferredWork, ClaimedReflectionWork, ClaimedTaskWork, DeferredLazyCycleMember,
    DeferredProducer, DeferredWorkPoll, DeferredWorkReservation, EvaluationWorkId,
    ReflectionCancellation, ReflectionWorkPoll, ReflectionWorkState,
};
pub(crate) use coordinator::{
    CompletionSubscriptionOutcome, CompletionSubscriptions, EvaluationTaskBlock,
    EvaluationWorkCoordinator, RuntimeObservationEpoch, RuntimeObservationState, WakeRegistration,
    WorkDependency,
};
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

trait DemandStateRef {
    fn demand_state(&self) -> &Arc<EvaluationDemandState>;
}

impl DemandStateRef for Arc<EvaluationDemandState> {
    fn demand_state(&self) -> &Arc<EvaluationDemandState> {
        self
    }
}

impl DemandStateRef for Arc<EvaluationSession> {
    fn demand_state(&self) -> &Arc<EvaluationDemandState> {
        &self.demand
    }
}

fn allocate_wait_token(
    session: &impl DemandStateRef,
    producer: EvaluationTaskId,
) -> Result<EvaluationWaitToken, Arc<str>> {
    let session = session.demand_state();
    let id = session.values.ids().evaluation_wait()?;
    Ok(EvaluationWaitToken(Arc::new(EvaluationWaitState {
        id,
        runtime: session.values.runtime_id(),
        owner_id: session.id,
        owner: Arc::downgrade(session),
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
    owner: Weak<EvaluationDemandState>,
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

    fn owner(&self) -> Option<Arc<EvaluationDemandState>> {
        self.0.owner.upgrade()
    }

    fn belongs_to(&self, session: &Arc<EvaluationDemandState>) -> bool {
        self.owner()
            .is_some_and(|owner| Arc::ptr_eq(session, &owner))
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
        mutation: &crate::runtime::RuntimeMutationGuard<'_>,
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

    pub(crate) fn subscribe_work(
        &self,
        runtime: EvaluationRuntimeId,
        registration: WakeRegistration,
    ) -> CompletionSubscriptionOutcome {
        self.0
            .completion
            .subscribe(runtime, registration, || self.0.terminal.get().is_some())
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
        mutation: &crate::runtime::RuntimeMutationGuard<'_>,
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
    wait: EvaluationWaitToken,
}

impl EvaluationTaskHandle {
    pub(crate) fn id(&self) -> EvaluationTaskId {
        self.id
    }

    pub(crate) fn session_id(&self) -> EvaluationSessionId {
        self.wait.owner_id()
    }

    #[cfg(test)]
    pub(crate) fn wait(&self) -> &EvaluationWaitToken {
        &self.wait
    }

    /// Transfers reporting responsibility for a propagated terminal failure
    /// from the task ledger to the consumer of this handle.
    pub(crate) fn acknowledge_propagated_failure(&self) {
        let Some(owner) = self.wait.owner() else {
            return;
        };
        owner.acknowledge_reflection_task_error(self);
    }
}

impl fmt::Debug for EvaluationTaskHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationTaskHandle")
            .field("task", &self.id.get())
            .field("work", &self.work.get())
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationTaskCancellation {
    Requested,
    Late,
    NotOwnerSession,
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

pub(crate) enum EvaluationMachinePoll {
    Yielded,
    Blocked(EvaluationTaskBlock),
    Complete(Value),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
}

pub(crate) trait EvaluationTaskMachine: Send {
    fn poll(&mut self, step_budget: usize) -> EvaluationMachinePoll;

    fn cancel(&mut self) {}
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
}

pub(crate) trait EvaluationTaskStatusSink: Send + Sync {
    fn update(&self, status: EvaluationTaskStatus);
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
}

struct ReflectionTaskReportingRecord {
    id: EvaluationTaskId,
    work: EvaluationWorkId,
    error_acknowledged: bool,
    published_status: EvaluationTaskStatus,
    status_sinks: Vec<Arc<dyn EvaluationTaskStatusSink>>,
}

fn evaluation_task_state(terminal: EvaluationWaitTerminal) -> EvaluationTaskState {
    match terminal {
        EvaluationWaitTerminal::Complete(value) => EvaluationTaskState::Complete(value),
        EvaluationWaitTerminal::Failed(error) => EvaluationTaskState::Failed(error),
        EvaluationWaitTerminal::Cancelled => EvaluationTaskState::Cancelled,
        EvaluationWaitTerminal::Abandoned => EvaluationTaskState::Abandoned,
    }
}

fn task_wait_terminal(state: &EvaluationTaskState) -> EvaluationWaitTerminal {
    match state {
        EvaluationTaskState::Complete(value) => EvaluationWaitTerminal::Complete(value.clone()),
        EvaluationTaskState::Failed(error) => EvaluationWaitTerminal::Failed(error.clone()),
        EvaluationTaskState::Cancelled => EvaluationWaitTerminal::Cancelled,
        EvaluationTaskState::Abandoned => EvaluationWaitTerminal::Abandoned,
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

struct TaskStatusUpdate {
    status: EvaluationTaskStatus,
    sinks: Vec<Arc<dyn EvaluationTaskStatusSink>>,
}

fn task_status(state: &EvaluationTaskState) -> EvaluationTaskStatus {
    match state {
        EvaluationTaskState::Complete(value) => EvaluationTaskStatus::Complete(value.clone()),
        EvaluationTaskState::Failed(error) => EvaluationTaskStatus::Failed(error.clone()),
        EvaluationTaskState::Cancelled => EvaluationTaskStatus::Cancelled,
        EvaluationTaskState::Abandoned => EvaluationTaskStatus::Abandoned,
    }
}

fn task_status_update(
    record: &mut ReflectionTaskReportingRecord,
    status: EvaluationTaskStatus,
) -> Option<TaskStatusUpdate> {
    if record.status_sinks.is_empty() {
        record.published_status = status;
        return None;
    }
    if record.published_status == status {
        return None;
    }
    let terminal = matches!(
        status,
        EvaluationTaskStatus::Complete(_)
            | EvaluationTaskStatus::Failed(_)
            | EvaluationTaskStatus::Cancelled
            | EvaluationTaskStatus::Abandoned
    );
    let sinks = if terminal {
        std::mem::take(&mut record.status_sinks)
    } else {
        record.status_sinks.clone()
    };
    record.published_status = status.clone();
    Some(TaskStatusUpdate { status, sinks })
}

fn publish_task_status(update: Option<TaskStatusUpdate>) {
    let Some(update) = update else {
        return;
    };
    for sink in update.sinks {
        sink.update(update.status.clone());
    }
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

    pub(crate) fn commit(
        &self,
        status: Arc<dyn EvaluationTaskStatusSink>,
        policy: PendingTaskPolicy,
    ) {
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
                    Some(status),
                    policy.acknowledge_error,
                );
            }
            InitialTaskDisposition::Cancel => self
                .inner
                .context
                .cancel_pending_reflection_task(&self.inner.handle, status),
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

#[derive(Default)]
struct SessionTaskReportingState {
    reflection: HashMap<EvaluationWaitToken, ReflectionTaskReportingRecord>,
    reflection_by_id: BTreeMap<EvaluationTaskId, EvaluationWaitToken>,
}

type SessionFailureLedger = RedBlackTreeMapSync<EvaluationTaskId, Arc<EvaluationFailure>>;

/// Transitional task reporting state for one demand session.
///
/// This store contains acknowledgement policy, published status, and status
/// sinks only. Executable machines belong to coordinator work records. The
/// coordinator retains the store while an indexed reflection task still has a
/// reporting tail, without recovering the external [`EvaluationSession`]
/// owner lease.
pub(super) struct SessionTaskReportingStore {
    id: EvaluationSessionId,
    state: Mutex<SessionTaskReportingState>,
    failures: Arc<Mutex<SessionFailureLedger>>,
    closed: Arc<AtomicBool>,
}

impl fmt::Debug for SessionTaskReportingStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionTaskReportingStore")
            .field("id", &self.id)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl SessionTaskReportingStore {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

struct ReflectionTaskTransition {
    retired: Option<ReflectionTaskReportingRecord>,
    status: Option<TaskStatusUpdate>,
}

fn transition_reflection_task(
    tasks: &mut SessionTaskReportingState,
    failures: &Mutex<SessionFailureLedger>,
    wait: &EvaluationWaitToken,
    state: EvaluationTaskState,
) -> ReflectionTaskTransition {
    let (unacknowledged_failure, status) = {
        let record = tasks
            .reflection
            .get_mut(wait)
            .expect("transitioned reflection task must remain registered");
        let failure = match &state {
            EvaluationTaskState::Failed(error) if !record.error_acknowledged => {
                Some((record.id, error.clone()))
            }
            _ => None,
        };
        let status = task_status_update(record, task_status(&state));
        (failure, status)
    };
    if let Some((task, failure)) = unacknowledged_failure {
        failures
            .lock()
            .expect("evaluation failure ledger was poisoned")
            .insert_mut(task, failure);
    }
    let retired = Some(retire_reflection_task(tasks, wait));
    ReflectionTaskTransition { retired, status }
}

fn update_reflection_task_status(
    tasks: &mut SessionTaskReportingState,
    wait: &EvaluationWaitToken,
    status: EvaluationTaskStatus,
) -> Option<TaskStatusUpdate> {
    let record = tasks
        .reflection
        .get_mut(wait)
        .expect("active reflection task must remain registered");
    task_status_update(record, status)
}

fn retire_reflection_task(
    tasks: &mut SessionTaskReportingState,
    wait: &EvaluationWaitToken,
) -> ReflectionTaskReportingRecord {
    let record = tasks
        .reflection
        .remove(wait)
        .expect("retired reflection task must remain registered");
    assert_eq!(
        tasks.reflection_by_id.remove(&record.id),
        Some(wait.clone()),
        "reflection task ID index must agree with its task record"
    );
    record
}

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
    reporting: Weak<SessionTaskReportingStore>,
    failures: Arc<Mutex<SessionFailureLedger>>,
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

    fn reporting_store(&self) -> Option<Arc<SessionTaskReportingStore>> {
        self.reporting.upgrade()
    }

    fn acknowledge_reflection_task_error(&self, task: &EvaluationTaskHandle) {
        if let Some(reporting) = self.reporting_store() {
            let mut tasks = reporting
                .state
                .lock()
                .expect("evaluation task registry was poisoned");
            if let Some(record) = tasks.reflection.get_mut(&task.wait) {
                record.error_acknowledged = true;
            }
        }
        self.failures
            .lock()
            .expect("evaluation failure ledger was poisoned")
            .remove_mut(&task.id);
    }

    fn abandon_spark_wait(&self, wait: &EvaluationWaitToken) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        let mut wait = wait.clone();
        loop {
            if wait.owner_id() != self.id || wait.terminal_poll().is_some() {
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

    fn closed_run_report(&self) -> EvaluationSessionRun {
        let failures = self
            .failures
            .lock()
            .expect("evaluation failure ledger was poisoned")
            .clone();
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
    reporting: Arc<SessionTaskReportingStore>,
    coordinator: Arc<EvaluationWorkCoordinator>,
}

impl Deref for EvaluationSession {
    type Target = EvaluationDemandState;

    fn deref(&self) -> &Self::Target {
        &self.demand
    }
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
        let abandoning = self.coordinator.abandon_reflection_session(self.id);
        let abandoning = abandoning
            .into_iter()
            .map(|work| {
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
                let state = settle_task_work(
                    &self.coordinator,
                    work.id,
                    if work.cancel {
                        EvaluationTaskState::Cancelled
                    } else {
                        EvaluationTaskState::Abandoned
                    },
                    failure,
                );
                (work.id, (work.task, work.cancel, state))
            })
            .collect::<HashMap<_, _>>();
        let abandoning_deferred = self
            .coordinator
            .abandon_deferred_session(self.id)
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
        let (reflection, statuses) = {
            let mut tasks = self
                .reporting
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            let reflection_waits = tasks.reflection.keys().cloned().collect::<Vec<_>>();
            let mut reflection = Vec::with_capacity(reflection_waits.len());
            let mut statuses = Vec::new();
            for wait in reflection_waits {
                let record = tasks
                    .reflection
                    .get(&wait)
                    .expect("collected reflection wait must remain registered");
                let Some((task, cancel, state)) = abandoning.get(&record.work).cloned() else {
                    // Another operation owns this record's terminal tail. A
                    // running claim retains its detached machine until it
                    // settles and retires the work. In either case the
                    // coordinator retains this reporting store until that
                    // owner retires the record.
                    continue;
                };
                assert_eq!(record.id, task);
                let work = record.work;
                let transition =
                    transition_reflection_task(&mut tasks, &self.reporting.failures, &wait, state);
                reflection.push((
                    work,
                    cancel,
                    transition
                        .retired
                        .expect("session shutdown must retire a reflection task"),
                ));
                statuses.extend(transition.status);
            }

            (reflection, statuses)
        };

        for status in statuses {
            publish_task_status(Some(status));
        }
        for (work, cancel, record) in reflection {
            let mut machine = self.coordinator.retire_reflection(work);
            if cancel && let Some(machine) = &mut machine {
                machine.cancel();
            }
            drop(machine);
            drop(record);
        }
        for work in abandoning_deferred {
            self.coordinator.retire_deferred(work.id);
            drop(work.machine);
        }

        self.coordinator.unregister_session(self.id);
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
            reporting: Weak::new(),
            failures: Arc::new(Mutex::new(SessionFailureLedger::new_sync())),
            default_reflection_profile,
            require_default_reflection_profile,
            closed: Arc::new(AtomicBool::new(false)),
            coordinator: Arc::downgrade(&coordinator),
        });
        let reporting = Arc::new(SessionTaskReportingStore {
            id: demand.id,
            state: Mutex::new(SessionTaskReportingState::default()),
            failures: demand.failures.clone(),
            closed: demand.closed.clone(),
        });
        // Construction is private and the demand state has not escaped yet.
        // Install only a weak route back to the sibling reporting store so
        // resident machine contexts cannot complete an ownership cycle.
        let mut demand = demand;
        Arc::get_mut(&mut demand)
            .expect("fresh demand state must be uniquely owned")
            .reporting = Arc::downgrade(&reporting);
        Arc::new(Self {
            demand,
            reporting,
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
        coordinator.register_session(&session);
        session
    }

    #[cfg(test)]
    pub(crate) fn shared(coordinator: &Arc<EvaluationWorkCoordinator>) -> Arc<Self> {
        let session =
            Self::with_execution_resources(coordinator.clone(), coordinator.test_values());
        coordinator.register_session(&session);
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
        coordinator.register_session(&session);
        session
    }

    pub(crate) fn submit_spark(self: &Arc<Self>, value: Value) {
        self.coordinator.submit_spark(self, value);
    }
}

/// Cheap per-evaluation handle to one shared demand session.
///
/// Narrower provenance can be added to this handle without duplicating the
/// runtime-owned scheduler or reflection state.
#[derive(Debug, Clone)]
pub(crate) struct EvalContext {
    session: Arc<EvaluationDemandState>,
    owner: Weak<EvaluationSession>,
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
    fn reporting_store(&self) -> Result<Arc<SessionTaskReportingStore>, Arc<str>> {
        self.session
            .reporting_store()
            .ok_or_else(|| Arc::from("evaluation session task reporting store is closed"))
    }

    #[cfg(test)]
    pub(crate) fn standalone() -> OwnedEvalContext {
        Self::isolated(crate::core::test_value_factory())
    }

    pub(crate) fn new(session: &Arc<EvaluationSession>) -> Self {
        let task_profile = session.default_reflection_profile.clone();
        Self {
            session: session.demand.clone(),
            owner: Arc::downgrade(session),
            task_profile,
            task: Arc::new(OnceLock::new()),
            local_promise_owner: None,
            scheduled_task: false,
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
            owner: Arc::downgrade(session),
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
        owner: Weak<EvaluationSession>,
        id: EvaluationTaskId,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        let task = Arc::new(OnceLock::new());
        task.set(Ok(id))
            .expect("fresh task identity cell must be empty");
        Self {
            session,
            owner,
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
        owner: Weak<EvaluationSession>,
        id: EvaluationTaskId,
        originating_task: Option<EvaluationTaskId>,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        let task = Arc::new(OnceLock::new());
        task.set(Ok(id))
            .expect("fresh deferred task identity cell must be empty");
        Self {
            session,
            owner,
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

    fn owner(&self) -> Result<Arc<EvaluationSession>, Arc<str>> {
        if self.session.is_closed() {
            return Err(Arc::from("evaluation demand session is closed"));
        }
        self.owner
            .upgrade()
            .ok_or_else(|| Arc::from("evaluation demand session owner was dropped"))
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
        ) && let Ok(owner) = self.owner()
        {
            owner.submit_spark(value);
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
        let Ok(owner) = self.owner() else {
            return;
        };
        let generation = owner.coordinator.work_generation();
        if !owner.target_has_running_producer(target) {
            return;
        }
        owner.coordinator.wait_for_change(generation);
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
        let Ok(owner) = self.owner() else {
            return false;
        };
        let generation = owner.coordinator.work_generation();
        if !owner.dependency_observes_runtime(target) {
            return false;
        }
        if owner.coordinator.work_generation() == generation {
            owner.coordinator.wait_for_change(generation);
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
        let owner = self.owner()?;
        let deferred = producer.id();
        if let Some(wait) = owner.coordinator.deferred_wait(deferred) {
            return Ok(wait);
        }

        let id = allocate_task_id(self.values())?;
        let wait = allocate_wait_token(&self.session, id)?;
        let originating_task = self
            .originating_task
            .or_else(|| self.task.get().and_then(|task| task.as_ref().ok()).copied());
        let machine = build(Self::for_deferred_task(
            self.session.clone(),
            self.owner.clone(),
            id,
            originating_task,
            self.task_profile.clone(),
        ));
        let work =
            match owner
                .coordinator
                .reserve_deferred(&owner, id, wait.clone(), producer, machine)
            {
                DeferredWorkReservation::Existing(wait) => return Ok(wait),
                DeferredWorkReservation::New(work) => work,
            };
        owner.coordinator.activate_deferred(work);
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
            owner: self.owner.clone(),
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

    pub(crate) fn session_id(&self) -> EvaluationSessionId {
        self.session.id
    }

    pub(crate) fn register_promise(
        &self,
        promise: &Arc<crate::core::PromiseCell>,
    ) -> Result<PromiseProducerObligation, Arc<str>> {
        let session_owner = self.owner()?;
        let owner = self.task_id()?;
        let wait = allocate_wait_token(&self.session, owner)?;
        let source = if self.scheduled_task {
            let work =
                session_owner
                    .coordinator
                    .register_task_promise(owner, wait.clone(), promise)?;
            PromiseProducerSource::Coordinator {
                work,
                promise: promise.id(),
                coordinator: Arc::downgrade(&session_owner.coordinator),
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
    /// hidden behind [`EvaluationTaskMachine`]. Construction happens before
    /// the task registry is locked, so host snapshots and evaluator work may
    /// safely use this same session.
    #[cfg(test)]
    pub(crate) fn schedule_task<F>(&self, build: F) -> Result<EvaluationTaskHandle, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Result<Box<dyn EvaluationTaskMachine>, Arc<str>>,
    {
        let owner = self.owner()?;
        let reporting = self.reporting_store()?;
        let id = allocate_task_id(self.values())?;
        let wait = allocate_wait_token(&self.session, id)?;
        let context = Self::for_task(
            self.session.clone(),
            self.owner.clone(),
            id,
            self.task_profile.clone(),
        );
        let work = owner
            .coordinator
            .reserve_reflection(&owner, id, wait.clone());
        let machine = match build(context) {
            Ok(machine) => machine,
            Err(error) => {
                let failure = evaluation_failure(format!("task construction failed: {error}"));
                assert!(
                    owner.coordinator.terminalize_reserved_reflection(work),
                    "failed test task construction must terminalize its reservation"
                );
                owner.coordinator.settle_terminal_work(
                    work,
                    EvaluationWaitTerminal::Failed(failure.clone()),
                    failure,
                );
                drop(owner.coordinator.retire_reflection(work));
                return Err(error);
            }
        };
        let mut tasks = reporting
            .state
            .lock()
            .expect("evaluation task registry was poisoned");
        let replaced = tasks.reflection.insert(
            wait.clone(),
            ReflectionTaskReportingRecord {
                id,
                work,
                error_acknowledged: false,
                published_status: EvaluationTaskStatus::Launched,
                status_sinks: Vec::new(),
            },
        );
        let replaced_id = tasks.reflection_by_id.insert(id, wait.clone());
        assert!(
            replaced.is_none() && replaced_id.is_none(),
            "evaluation task identities must be unique"
        );
        drop(tasks);
        owner
            .coordinator
            .install_reflection_machine(work, machine)
            .unwrap_or_else(|_| panic!("fresh reflection reservation must accept its machine"));
        assert!(
            owner.coordinator.activate_reflection(work),
            "fresh reflection reservation must activate"
        );
        Ok(EvaluationTaskHandle { id, work, wait })
    }

    fn reserve_task(&self) -> Result<EvaluationTaskHandle, Arc<str>> {
        let owner = self.owner()?;
        let reporting = self.reporting_store()?;
        let id = allocate_task_id(self.values())?;
        let wait = allocate_wait_token(&self.session, id)?;
        let work = owner
            .coordinator
            .reserve_reflection(&owner, id, wait.clone());
        let mut tasks = reporting
            .state
            .lock()
            .expect("evaluation task registry was poisoned");
        let replaced = tasks.reflection.insert(
            wait.clone(),
            ReflectionTaskReportingRecord {
                id,
                work,
                error_acknowledged: false,
                published_status: EvaluationTaskStatus::Launched,
                status_sinks: Vec::new(),
            },
        );
        let replaced_id = tasks.reflection_by_id.insert(id, wait.clone());
        assert!(
            replaced.is_none() && replaced_id.is_none(),
            "evaluation task identities must be unique"
        );
        Ok(EvaluationTaskHandle { id, work, wait })
    }

    fn activate_reflection_task(
        &self,
        handle: &EvaluationTaskHandle,
        effect: Value,
        result_policy: ReflectionTaskResultPolicy,
        task_profile: Arc<ReflectionTaskProfile>,
        status_sink: Option<Arc<dyn EvaluationTaskStatusSink>>,
        error_acknowledged: bool,
    ) {
        let Ok(owner) = self.owner() else {
            return;
        };
        let Ok(reporting) = self.reporting_store() else {
            return;
        };
        let coordinator = &owner.coordinator;
        let result = task_profile
            .launcher()
            .ok_or_else(|| {
                Arc::new(EvaluationFailure::message(
                    "reflection task profile is not sealed",
                ))
            })
            .and_then(|launcher| {
                launcher.build(
                    Self::for_task(
                        self.session.clone(),
                        self.owner.clone(),
                        handle.id,
                        task_profile.clone(),
                    ),
                    effect,
                    result_policy,
                )
            });
        match result {
            Ok(machine) => {
                {
                    let mut tasks = reporting
                        .state
                        .lock()
                        .expect("evaluation task registry was poisoned");
                    let Some(record) = tasks.reflection.get_mut(&handle.wait) else {
                        return;
                    };
                    assert_eq!(record.work, handle.work);
                    record.error_acknowledged = error_acknowledged;
                    if let Some(status_sink) = status_sink {
                        record.status_sinks.push(status_sink);
                    }
                }
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
                {
                    let mut tasks = reporting
                        .state
                        .lock()
                        .expect("evaluation task registry was poisoned");
                    let Some(record) = tasks.reflection.get_mut(&handle.wait) else {
                        return;
                    };
                    assert_eq!(record.work, handle.work);
                    record.error_acknowledged = error_acknowledged;
                    if let Some(status_sink) = status_sink {
                        record.status_sinks.push(status_sink);
                    }
                }
                if coordinator.terminalize_reserved_reflection(handle.work) {
                    let state = settle_task_work(
                        coordinator,
                        handle.work,
                        EvaluationTaskState::Failed(error),
                        promise_failure,
                    );
                    let transition = {
                        let mut tasks = reporting
                            .state
                            .lock()
                            .expect("evaluation task registry was poisoned");
                        transition_reflection_task(
                            &mut tasks,
                            &reporting.failures,
                            &handle.wait,
                            state,
                        )
                    };
                    publish_task_status(transition.status);
                    drop(coordinator.retire_reflection(handle.work));
                    drop(transition.retired);
                }
            }
        }
    }

    fn cancel_reserved_task(&self, handle: &EvaluationTaskHandle) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        if !coordinator.discard_reserved_reflection(handle.work) {
            return;
        }
        let Some(reporting) = self.session.reporting_store() else {
            return;
        };
        let mut tasks = reporting
            .state
            .lock()
            .expect("evaluation task registry was poisoned");
        let retired = if tasks.reflection.contains_key(&handle.wait) {
            Some(retire_reflection_task(&mut tasks, &handle.wait))
        } else {
            None
        };
        drop(tasks);
        drop(retired);
    }

    fn cancel_pending_reflection_task(
        &self,
        handle: &EvaluationTaskHandle,
        status_sink: Arc<dyn EvaluationTaskStatusSink>,
    ) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        let Some(reporting) = self.session.reporting_store() else {
            return;
        };
        {
            let mut tasks = reporting
                .state
                .lock()
                .expect("evaluation task registry was poisoned");
            let record = tasks
                .reflection
                .get_mut(&handle.wait)
                .expect("a committed pending task must remain reserved");
            assert_eq!(record.work, handle.work);
            record.status_sinks.push(status_sink);
        }
        let cancellation = coordinator.request_reflection_cancellation(handle.work);
        assert_eq!(
            cancellation,
            ReflectionCancellation::Terminalize,
            "a committed pre-launch cancellation must own its reservation"
        );
        let state = settle_task_work(
            &coordinator,
            handle.work,
            EvaluationTaskState::Cancelled,
            evaluation_failure("reflection fixpoint producer was cancelled"),
        );
        let transition = {
            let mut tasks = reporting
                .state
                .lock()
                .expect("evaluation task registry was poisoned");
            transition_reflection_task(&mut tasks, &reporting.failures, &handle.wait, state)
        };
        publish_task_status(transition.status);
        drop(coordinator.retire_reflection(handle.work));
        drop(transition.retired);
    }

    pub(crate) fn reserve_reflection_task(
        &self,
        effect: Value,
    ) -> Result<PendingReflectionTask, Arc<str>> {
        self.owner()?;
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
        let owner = self.owner()?;
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
        let reporting = self.reporting_store()?;
        let wait = allocate_wait_token(&self.session, id)?;
        let work = owner
            .coordinator
            .register_dormant_reflection(&owner, id, wait.clone());
        let mut tasks = reporting
            .state
            .lock()
            .expect("evaluation task registry was poisoned");
        let replaced = tasks.reflection.insert(
            wait.clone(),
            ReflectionTaskReportingRecord {
                id,
                work,
                error_acknowledged: false,
                published_status: EvaluationTaskStatus::Launched,
                status_sinks: Vec::new(),
            },
        );
        let replaced_id = tasks.reflection_by_id.insert(id, wait.clone());
        assert!(
            replaced.is_none() && replaced_id.is_none(),
            "evaluation task identities must be unique"
        );
        Ok(EvaluationTaskHandle { id, work, wait })
    }

    pub(crate) fn poll_reflection_task(&self, task: &EvaluationTaskHandle) -> EvaluationWaitPoll {
        self.poll_wait(&task.wait)
    }

    pub(crate) fn owns_task(&self, task: &EvaluationTaskHandle) -> bool {
        task.session_id() == self.session.id
    }

    pub(crate) fn cancel_reflection_task(
        &self,
        task: &EvaluationTaskHandle,
    ) -> EvaluationTaskCancellation {
        if !self.owns_task(task) {
            return EvaluationTaskCancellation::NotOwnerSession;
        }
        if task.wait.terminal_poll().is_some() {
            return EvaluationTaskCancellation::Late;
        }
        let Some(coordinator) = self.coordinator() else {
            return EvaluationTaskCancellation::Late;
        };
        match coordinator.request_reflection_cancellation(task.work) {
            ReflectionCancellation::Requested => EvaluationTaskCancellation::Requested,
            ReflectionCancellation::Late => EvaluationTaskCancellation::Late,
            ReflectionCancellation::Terminalize => {
                let Some(reporting) = self.session.reporting_store() else {
                    return EvaluationTaskCancellation::Late;
                };
                let state = settle_task_work(
                    &coordinator,
                    task.work,
                    EvaluationTaskState::Cancelled,
                    evaluation_failure("reflection fixpoint producer was cancelled"),
                );
                let transition = {
                    let mut tasks = reporting
                        .state
                        .lock()
                        .expect("evaluation task registry was poisoned");
                    if !tasks.reflection.contains_key(&task.wait) {
                        return EvaluationTaskCancellation::Late;
                    }
                    transition_reflection_task(&mut tasks, &reporting.failures, &task.wait, state)
                };
                let retired = transition
                    .retired
                    .expect("terminal cancellation must retire its task record");
                let mut machine = coordinator.retire_reflection(task.work);
                if let Some(machine) = &mut machine {
                    machine.cancel();
                }
                publish_task_status(transition.status);
                drop(machine);
                drop(retired);
                EvaluationTaskCancellation::Requested
            }
        }
    }

    /// Acknowledges any present or future failure of a local reflection task.
    ///
    /// Acknowledgement affects reasoning reports only. The task's terminal
    /// result and transactional status query remain unchanged.
    pub(crate) fn acknowledge_reflection_task_error(&self, task: &EvaluationTaskHandle) -> bool {
        if !self.owns_task(task) {
            return false;
        }
        self.session.acknowledge_reflection_task_error(task);
        true
    }

    pub(crate) fn acknowledge_task_failure(&self, task: EvaluationTaskId) {
        self.session
            .failures
            .lock()
            .expect("evaluation failure ledger was poisoned")
            .remove_mut(&task);
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
        if wait.owner().is_none_or(|owner| owner.is_closed()) {
            return EvaluationWaitPoll::Abandoned;
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
        let Ok(owner) = self.owner() else {
            return if wait.terminal_poll().is_some() {
                EvaluationPumpOutcome::TargetReady
            } else {
                EvaluationPumpOutcome::NoProgress
            };
        };
        owner.pump(self, wait, step_budget)
    }

    /// Runs every executable task until all are terminal or one complete pass
    /// leaves every unfinished task unchanged.
    pub(crate) fn run_until_quiescent(&self) -> EvaluationSessionRun {
        self.owner.upgrade().map_or_else(
            || self.session.closed_run_report(),
            |owner| owner.run_until_quiescent(),
        )
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
        let reporting = self
            .session
            .reporting_store()
            .expect("test task must retain its reporting store");
        let tasks = reporting
            .state
            .lock()
            .expect("evaluation task registry was poisoned");
        let work = tasks
            .reflection
            .get(&wait)
            .expect("test task must belong to this session")
            .work;
        drop(tasks);
        assert!(coordinator.terminalize_reflection(work));
        let state = settle_task_work(
            &coordinator,
            work,
            EvaluationTaskState::Complete(RuntimeValueRoot::new(&self.session.values, value)),
            evaluation_failure("reflection task completed without fulfilling its fixpoint"),
        );
        let mut tasks = reporting
            .state
            .lock()
            .expect("evaluation task registry was poisoned");
        let transition = transition_reflection_task(&mut tasks, &reporting.failures, &wait, state);
        drop(tasks);
        publish_task_status(transition.status);
        drop(coordinator.retire_reflection(work));
        drop(transition.retired);
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
        let reporting = self
            .session
            .reporting_store()
            .expect("test task must retain its reporting store");
        let tasks = reporting
            .state
            .lock()
            .expect("evaluation task registry was poisoned");
        let work = tasks
            .reflection
            .get(&wait)
            .expect("test task must belong to this session")
            .work;
        drop(tasks);
        assert!(coordinator.terminalize_reflection(work));
        let state = settle_task_work(
            &coordinator,
            work,
            EvaluationTaskState::Failed(failure.clone()),
            failure,
        );
        let mut tasks = reporting
            .state
            .lock()
            .expect("evaluation task registry was poisoned");
        let transition = transition_reflection_task(&mut tasks, &reporting.failures, &wait, state);
        drop(tasks);
        publish_task_status(transition.status);
        drop(coordinator.retire_reflection(work));
        drop(transition.retired);
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
        let reporting = self
            .session
            .reporting_store()
            .expect("test demand must retain its reporting store");
        let tasks = reporting
            .state
            .lock()
            .expect("evaluation task registry was poisoned");
        EvaluationTaskRegistryCounts {
            reflection_active: tasks.reflection.len(),
            reflection_terminal: 0,
            reflection_by_id: tasks.reflection_by_id.len(),
            unacknowledged_failures: self
                .session
                .failures
                .lock()
                .expect("evaluation failure ledger was poisoned")
                .size(),
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
    Reflection {
        reporting: Arc<SessionTaskReportingStore>,
        task: ClaimedReflectionWork,
    },
    Deferred(ClaimedDeferredWork),
}

impl ClaimedTask {
    fn new(coordinator: Arc<EvaluationWorkCoordinator>, work: ClaimedTaskWork) -> Self {
        let kind = match work {
            ClaimedTaskWork::Reflection { reporting, claim } => ClaimedTaskKind::Reflection {
                reporting,
                task: claim,
            },
            ClaimedTaskWork::Deferred(claim) => ClaimedTaskKind::Deferred(claim),
        };
        Self { coordinator, kind }
    }

    fn poll(&mut self, step_budget: usize) -> EvaluationMachinePoll {
        match &mut self.kind {
            ClaimedTaskKind::Reflection { task, .. } => task.poll(step_budget),
            ClaimedTaskKind::Deferred(task) => task.poll(step_budget),
        }
    }

    fn release(
        self,
        poll: EvaluationMachinePoll,
    ) -> (
        bool,
        bool,
        Option<ReleasedTaskMachine>,
        Option<TaskStatusUpdate>,
    ) {
        match self.kind {
            ClaimedTaskKind::Reflection { reporting, task } => {
                release_reflection_task(&self.coordinator, &reporting, task, poll)
            }
            ClaimedTaskKind::Deferred(task) => release_deferred_task(&self.coordinator, task, poll),
        }
    }
}

impl EvaluationSession {
    fn run_until_quiescent(&self) -> EvaluationSessionRun {
        loop {
            let mut claimed = loop {
                if let Some(claimed) = self.claim_ready_task() {
                    break claimed;
                }
                let generation = self.coordinator.work_generation();
                if self.task_is_running() {
                    self.coordinator.wait_for_change(generation);
                    continue;
                }
                if self.coordinator.work_generation() != generation
                    && self.coordinator.session_has_ready_task(self.id)
                {
                    continue;
                }
                return self.session_run_report();
            };

            let poll = claimed.poll(TASK_POLL_QUANTUM);
            let (_, _, released, status) = claimed.release(poll);
            publish_task_status(status);
            if let Some(machine) = released {
                machine.finish();
            }
        }
    }

    fn session_run_report(&self) -> EvaluationSessionRun {
        let snapshots = self.coordinator.reflection_snapshots(self.id);
        let failures = self
            .failures
            .lock()
            .expect("evaluation failure ledger was poisoned")
            .clone();
        let mut unfinished = Vec::new();
        let mut has_live_cross_session_dependency = false;
        for snapshot in snapshots {
            let (state, block) = match &snapshot.state {
                ReflectionWorkState::Dormant => (EvaluationUnfinishedState::Dormant, None),
                ReflectionWorkState::Reserved => (EvaluationUnfinishedState::Reserved, None),
                ReflectionWorkState::Queued => (EvaluationUnfinishedState::Queued, None),
                ReflectionWorkState::Running => (EvaluationUnfinishedState::Running, None),
                ReflectionWorkState::Blocked(block) => {
                    (EvaluationUnfinishedState::Blocked, Some(block))
                }
                ReflectionWorkState::Terminalizing => (EvaluationUnfinishedState::Running, None),
            };
            let dependency = block
                .and_then(|block| block.dependency.as_ref())
                .and_then(|dependency| self.reported_dependency(dependency));
            has_live_cross_session_dependency |= dependency
                .as_ref()
                .is_some_and(|dependency| dependency.live_cross_session);
            unfinished.push(EvaluationUnfinishedTask {
                task: snapshot.task,
                state,
                dependency: dependency.as_ref().map(|dependency| dependency.task),
                dependency_session: dependency.as_ref().map(|dependency| dependency.session),
                wait: dependency.as_ref().map(|dependency| dependency.wait),
                observed_epoch: block.and_then(|block| block.observed_epoch),
                error: block.and_then(|block| block.error.clone()),
            });
        }
        let report = EvaluationSessionReport {
            failures,
            unfinished,
        };
        if report.unfinished.is_empty() {
            EvaluationSessionRun::Complete(report)
        } else if has_live_cross_session_dependency {
            EvaluationSessionRun::Quiescent(report)
        } else {
            EvaluationSessionRun::Deadlocked(report)
        }
    }

    fn reported_dependency(&self, initial: &WorkDependency) -> Option<ReportedDependency> {
        let mut wait = initial.producer_wait()?.clone();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(wait.get()) || wait.owner_id() != self.id {
                return Some(ReportedDependency {
                    task: wait.producer(),
                    session: wait.owner_id(),
                    wait: wait.get(),
                    live_cross_session: wait.owner_id() != self.id
                        && wait.owner().is_some_and(|owner| !owner.is_closed()),
                });
            }
            let Some(next) = self.task_dependency(wait.producer()) else {
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

    fn pump(
        &self,
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

            if self.target_has_running_producer(target) {
                return EvaluationPumpOutcome::Busy;
            }
            let prioritized = self.prioritized_task(target);
            let claimed = prioritized
                .and_then(|id| self.claim_task(id))
                .or_else(|| self.claim_ready_task());
            let Some(mut claimed) = claimed else {
                if self.target_has_running_producer(target) {
                    return EvaluationPumpOutcome::Busy;
                }
                if !matches!(context.poll_wait(target), EvaluationWaitPoll::Pending(_)) {
                    return EvaluationPumpOutcome::TargetReady;
                }
                return EvaluationPumpOutcome::NoProgress;
            };

            let quantum = step_budget.min(TASK_POLL_QUANTUM);
            step_budget -= quantum;
            let poll = claimed.poll(quantum);
            let (_, _, released, status) = claimed.release(poll);
            publish_task_status(status);
            if let Some(machine) = released {
                machine.finish();
            }
        }
    }
}

fn release_reflection_task(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    reporting: &SessionTaskReportingStore,
    claimed: ClaimedReflectionWork,
    poll: EvaluationMachinePoll,
) -> (
    bool,
    bool,
    Option<ReleasedTaskMachine>,
    Option<TaskStatusUpdate>,
) {
    let work = claimed.id();
    let wait = {
        let tasks = reporting
            .state
            .lock()
            .expect("evaluation task reporting store was poisoned");
        tasks
            .reflection_by_id
            .get(&claimed.task())
            .expect("claimed reflection work must retain its task lookup")
            .clone()
    };
    let (work_poll, terminal_state) = match poll {
        EvaluationMachinePoll::Yielded => (ReflectionWorkPoll::Yielded, None),
        EvaluationMachinePoll::Blocked(block) => (ReflectionWorkPoll::Blocked(block), None),
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
        debug_assert!(release.machine.is_none());
        let status = {
            let mut tasks = reporting
                .state
                .lock()
                .expect("evaluation task reporting store was poisoned");
            update_reflection_task_status(
                &mut tasks,
                &wait,
                if release.remains_blocked {
                    EvaluationTaskStatus::Blocked
                } else {
                    EvaluationTaskStatus::Launched
                },
            )
        };
        return (release.made_progress, release.remains_blocked, None, status);
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
    };
    let state = settle_task_work(coordinator, work, state, promise_failure);
    let transition = {
        let mut tasks = reporting
            .state
            .lock()
            .expect("evaluation task reporting store was poisoned");
        transition_reflection_task(&mut tasks, &reporting.failures, &wait, state)
    };
    let retired = transition
        .retired
        .expect("terminal reflection transition must retire its reporting record");
    assert_eq!(retired.work, work);
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
    drop(retired);
    (release.made_progress, false, released, transition.status)
}

fn release_deferred_task(
    coordinator: &Arc<EvaluationWorkCoordinator>,
    claimed: ClaimedDeferredWork,
    poll: EvaluationMachinePoll,
) -> (
    bool,
    bool,
    Option<ReleasedTaskMachine>,
    Option<TaskStatusUpdate>,
) {
    let work = claimed.id();
    let (work_poll, terminal) = match poll {
        EvaluationMachinePoll::Yielded => (DeferredWorkPoll::Yielded, None),
        EvaluationMachinePoll::Blocked(block) => (DeferredWorkPoll::Blocked(block), None),
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
        return (release.made_progress, false, None, None);
    }
    if !release.terminal {
        debug_assert!(release.machine.is_none());
        return (release.made_progress, release.remains_blocked, None, None);
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
        None,
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
    fn poll_claimed_task(self: &Arc<Self>, work: ClaimedTaskWork) {
        let mut claimed = ClaimedTask::new(self.clone(), work);
        let poll = claimed.poll(TASK_POLL_QUANTUM);
        let (_, _, released, status) = claimed.release(poll);
        publish_task_status(status);
        if let Some(machine) = released {
            machine.finish();
        }
    }
}

impl EvaluationSession {
    fn claim_task(&self, id: EvaluationTaskId) -> Option<ClaimedTask> {
        let work = self.coordinator.claim_task(id)?;
        Some(ClaimedTask::new(self.coordinator.clone(), work))
    }

    fn claim_ready_task(&self) -> Option<ClaimedTask> {
        let work = self.coordinator.claim_ready_task_for_session(self.id)?;
        Some(ClaimedTask::new(self.coordinator.clone(), work))
    }

    fn producer_for_wait(&self, wait: &EvaluationWaitToken) -> Option<EvaluationTaskId> {
        self.coordinator.producer_for_wait(wait)
    }

    fn task_dependency(&self, id: EvaluationTaskId) -> Option<WorkDependency> {
        self.coordinator.task_dependency(id)
    }

    fn task_is_claimable(&self, id: EvaluationTaskId) -> bool {
        self.coordinator.task_is_claimable(id)
    }

    fn prioritized_task(&self, target: &EvaluationWaitToken) -> Option<EvaluationTaskId> {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut wait = target.clone();
        while let Some(id) = self.producer_for_wait(&wait) {
            if !seen.insert(id) {
                break;
            }
            chain.push(id);
            let Some(dependency) = self.task_dependency(id) else {
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
            .find(|id| self.task_is_claimable(*id))
    }

    fn target_has_running_producer(&self, target: &EvaluationWaitToken) -> bool {
        let mut seen = HashSet::new();
        let mut wait = target.clone();
        while let Some(id) = self.producer_for_wait(&wait) {
            if !seen.insert(id) {
                return false;
            }
            if self.coordinator.task_is_busy(id) {
                return true;
            }
            let Some(dependency) = self.coordinator.task_dependency(id) else {
                return false;
            };
            let Some(dependency_wait) = dependency.producer_wait() else {
                return false;
            };
            wait = dependency_wait.clone();
        }
        false
    }

    fn dependency_observes_runtime(&self, target: &EvaluationWaitToken) -> bool {
        let mut seen = HashSet::new();
        let mut wait = target.clone();
        while seen.insert(wait.get()) {
            let Some(task) = self.producer_for_wait(&wait) else {
                return false;
            };
            if self.coordinator.task_observed_epoch(task).is_some() {
                return true;
            }
            let Some(dependency) = self.coordinator.task_dependency(task) else {
                return false;
            };
            let Some(dependency_wait) = dependency.producer_wait() else {
                return false;
            };
            wait = dependency_wait.clone();
        }
        false
    }

    fn task_is_running(&self) -> bool {
        self.coordinator.session_machine_is_busy(self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Condvar;
    use std::sync::mpsc;
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
            debug_assert_eq!(session.values.runtime_id(), self.runtime.id());
            OwnedEvalContext::new(session)
        }
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
    fn blocked_machine_context_does_not_retain_its_owner_lease() {
        let (coordinator, _executor) =
            test_execution_resources(0).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let owner_weak = Arc::downgrade(&owner);
        let reporting_weak = Arc::downgrade(&owner.reporting);
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
        assert!(
            reporting_weak.upgrade().is_none(),
            "blocked owner closure must retire the task reporting store"
        );
    }

    #[test]
    fn running_machine_finishes_its_quantum_after_owner_drop_without_retaining_the_owner() {
        let (coordinator, _executor) =
            test_execution_resources(1).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let owner_weak = Arc::downgrade(&owner);
        let reporting_weak = Arc::downgrade(&owner.reporting);
        let owner_session = owner.id;
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
        assert!(Arc::ptr_eq(
            &coordinator
                .registered_task_reporting_store(owner_session)
                .expect("running work must keep its reporting store registered"),
            &owner.reporting,
        ));

        drop(owner);
        assert!(owner_weak.upgrade().is_none());
        assert!(context.session.is_closed());
        assert!(reporting_weak.upgrade().is_some());
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
        while reporting_weak.upgrade().is_some() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            reporting_weak.upgrade().is_none(),
            "terminal release must retire the closed session's reporting store"
        );
    }

    #[test]
    fn running_deferred_machine_is_coordinator_owned_after_owner_drop() {
        let (coordinator, _executor) =
            test_execution_resources(1).expect("test execution resources should build");
        let owner = EvaluationSession::shared(&coordinator);
        let owner_weak = Arc::downgrade(&owner);
        let reporting_weak = Arc::downgrade(&owner.reporting);
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
        assert!(
            reporting_weak.upgrade().is_none(),
            "deferred work must not retain the task reporting store"
        );
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

    #[derive(Default)]
    struct RecordedStatuses(Mutex<Vec<EvaluationTaskStatus>>);

    impl EvaluationTaskStatusSink for RecordedStatuses {
        fn update(&self, status: EvaluationTaskStatus) {
            self.0
                .lock()
                .expect("recorded task statuses were poisoned")
                .push(status);
        }
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

    impl EvaluationTaskMachine for CompleteAndCheckReflectionDrop {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
        }
    }

    impl Drop for CompleteAndCheckReflectionDrop {
        fn drop(&mut self) {
            let reporting_unlocked = self
                .context
                .session
                .reporting_store()
                .is_none_or(|reporting| reporting.state.try_lock().is_ok());
            let runtime_unlocked = self
                .context
                .coordinator()
                .is_none_or(|coordinator| coordinator.runtime_locks_are_free());
            self.dropped_without_registry_lock
                .store(reporting_unlocked && runtime_unlocked, Ordering::Release);
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
        assert_eq!(
            context.cancel_reflection_task(&cancelled),
            EvaluationTaskCancellation::Requested
        );
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
    fn terminal_reflection_machines_drop_after_releasing_runtime_and_reporting_locks() {
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
            "terminal reflection machine must be destroyed without runtime or reporting locks"
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
            assert_eq!(
                owner.cancel_reflection_task(&cancelled),
                EvaluationTaskCancellation::Requested
            );
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
            owner
                .session
                .reporting_store()
                .expect("scheduled task must retain its reporting store")
                .state
                .lock()
                .expect("evaluation task registry was poisoned")
                .reflection
                .get_mut(task.wait())
                .expect("scheduled task must remain registered")
                .status_sinks
                .push(statuses.clone());
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
        assert_eq!(
            owner.cancel_reflection_task(&owner_task),
            EvaluationTaskCancellation::Requested
        );
        assert_eq!(promise.exact_subscription_count(), 0);
        assert!(
            crate::eval::eval_value(&observer, &Value::Lazy(lazy))
                .expect_err("producer cancellation should fail the exact follower")
                .to_string()
                .contains("was cancelled")
        );
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

        assert_eq!(
            context.cancel_reflection_task(&task),
            EvaluationTaskCancellation::Requested
        );
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
            assert_eq!(
                context.cancel_reflection_task(&cancellation),
                EvaluationTaskCancellation::Requested
            );
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
            assert!(context.acknowledge_reflection_task_error(task));
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
        assert_eq!(
            context.cancel_reflection_task(&task),
            EvaluationTaskCancellation::Requested
        );
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(
            context.poll_reflection_task(&task),
            EvaluationWaitPoll::Cancelled
        );
        assert_eq!(
            context.cancel_reflection_task(&task),
            EvaluationTaskCancellation::Late
        );

        let non_owner = fixture.context();
        assert_eq!(
            non_owner.cancel_reflection_task(&task),
            EvaluationTaskCancellation::NotOwnerSession
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

        assert_eq!(
            context.cancel_reflection_task(&task),
            EvaluationTaskCancellation::Requested
        );
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
    fn error_acknowledgement_is_timing_independent_and_preserves_task_results() {
        let context = EvalContext::standalone();

        let reserved = context.reserve_task().expect("task should reserve");
        assert!(context.acknowledge_reflection_task_error(&reserved));
        {
            let reporting = context
                .session
                .reporting_store()
                .expect("reserved task must retain its reporting store");
            let tasks = reporting
                .state
                .lock()
                .expect("evaluation task registry was poisoned");
            assert!(
                tasks
                    .reflection
                    .get(reserved.wait())
                    .expect("reserved task should remain registered")
                    .error_acknowledged
            );
        }
        context.cancel_reserved_task(&reserved);

        let blocked = context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .expect("blocked task should schedule");
        assert_eq!(
            context.pump_wait(blocked.wait(), 256),
            EvaluationPumpOutcome::NoProgress
        );
        assert!(context.acknowledge_reflection_task_error(&blocked));
        {
            let reporting = context
                .session
                .reporting_store()
                .expect("blocked task must retain its reporting store");
            let tasks = reporting
                .state
                .lock()
                .expect("evaluation task registry was poisoned");
            assert!(
                tasks
                    .reflection
                    .get(blocked.wait())
                    .expect("blocked task should remain registered")
                    .error_acknowledged
            );
        }
        assert_eq!(
            context.cancel_reflection_task(&blocked),
            EvaluationTaskCancellation::Requested
        );

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
        assert!(context.acknowledge_reflection_task_error(&running));
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
        assert!(context.acknowledge_reflection_task_error(&failed));
        assert!(context.acknowledge_reflection_task_error(&failed));
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
        assert!(context.acknowledge_reflection_task_error(&successful));
        assert!(matches!(
            context.poll_reflection_task(&successful),
            EvaluationWaitPoll::Complete(_)
        ));

        let cancelled = context
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("cancelled task should schedule");
        assert_eq!(
            context.cancel_reflection_task(&cancelled),
            EvaluationTaskCancellation::Requested
        );
        assert!(context.acknowledge_reflection_task_error(&cancelled));
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
                .owner()
                .expect("test demand owner should be live")
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
        let session = claimed
            .session()
            .expect("the manually claimed spark should retain its demand session");
        let halt = crate::eval::eval_value(&EvalContext::new(&session), claimed.value().as_core())
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
        let session = claimed
            .session()
            .expect("the manually claimed spark should retain its demand session");
        let halt = crate::eval::eval_value(&EvalContext::new(&session), claimed.value().as_core())
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

        coordinator.unregister_session(session.id);

        wait_for_spark_work_counts(
            &coordinator,
            (0, 0, 0),
            "closing the demand session should immediately abandon blocked sparks",
        );
        assert_eq!(
            context.task_registry_counts().deferred_active,
            0,
            "spark abandonment must release the reusable deferred claim"
        );
        assert!(lazy.cached().is_none());

        promise
            .set(context.values().unit())
            .expect("host promise should accept its assignment");
        let observer_session = EvaluationSession::shared(&coordinator);
        let observer = EvalContext::patient_with_task_profile(
            &observer_session,
            observer_session.default_reflection_profile.clone(),
        );
        assert_eq!(
            crate::eval::eval_value(&observer, &Value::Lazy(lazy)),
            Ok(context.values().unit()),
            "a later demand must be able to reclaim the abandoned lazy"
        );
    }

    #[test]
    fn closing_a_session_keeps_worker_owned_spark_work_busy_until_release() {
        let (coordinator, _executor) = test_execution_resources(1).unwrap();
        let session = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&session);
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let producer_release = release.clone();
        let (started_sender, started_receiver) = mpsc::channel();
        let lazy = LazyValue::deferred(context.values(), "worker-owned spark", move |context| {
            started_sender
                .send(())
                .expect("test should still await the worker-owned spark");
            let (lock, changed) = &*producer_release;
            let mut released = lock.lock().expect("spark release lock was poisoned");
            while !*released {
                released = changed
                    .wait(released)
                    .expect("spark release lock was poisoned");
            }
            Ok(context.values().unit())
        });
        context.spark(Value::Lazy(lazy));
        started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should own the spark before session closure");
        assert_eq!(coordinator.spark_work_counts(), (0, 1, 0));

        coordinator.unregister_session(session.id);
        coordinator.unregister_session(session.id);

        assert_eq!(
            coordinator.spark_work_counts(),
            (0, 1, 0),
            "a close request must retain worker-owned work and its session index"
        );
        let (lock, changed) = &*release;
        *lock.lock().expect("spark release lock was poisoned") = true;
        changed.notify_all();
        wait_for_spark_work_counts(
            &coordinator,
            (0, 0, 0),
            "the returning worker should apply the saved close request exactly once",
        );
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
