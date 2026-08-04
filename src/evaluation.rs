//! Session-scoped capabilities threaded through semantic evaluation.
//!
//! The runtime supplies task and wait identity and value provenance. During
//! the work-boundary transition, the session still owns active task records,
//! dependency lookup, and its serial cooperative pump. Reflection
//! specializations remain outside this module behind a small type-erased
//! task-machine boundary.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use rpds::RedBlackTreeMapSync;

use crate::core::{
    CoreValueFactory, DeferredValueId, EvaluationFailure, LazyCycle, LazyCycleMember, LazyValue,
    PromiseAssignment, PromisedValue, Value,
};
use crate::runtime::{EvaluationRuntimeId, RuntimeValueRoot};

mod coordinator;
mod executor;
pub(crate) use coordinator::EvaluationWorkCoordinator;
pub(crate) use executor::EvaluationExecutor;

#[cfg(test)]
pub(crate) fn test_execution_resources(
    worker_count: usize,
) -> Result<(Arc<EvaluationWorkCoordinator>, Arc<EvaluationExecutor>), Arc<str>> {
    let values = crate::core::test_value_factory();
    let admission = crate::runtime::RuntimeMutationAdmission::new();
    let coordinator =
        EvaluationWorkCoordinator::new(values.runtime_id(), values.ids().clone(), admission);
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
    session: &Arc<EvaluationSession>,
    producer: EvaluationTaskId,
) -> Result<EvaluationWaitToken, Arc<str>> {
    Ok(EvaluationWaitToken(Arc::new(EvaluationWaitState {
        id: session.values.ids().evaluation_wait()?,
        runtime: session.values.runtime_id(),
        owner_id: session.id,
        owner: Arc::downgrade(session),
        producer,
        terminal: OnceLock::new(),
    })))
}

fn evaluation_failure(message: impl AsRef<str>) -> Arc<EvaluationFailure> {
    Arc::new(EvaluationFailure::message(message))
}

struct EvaluationWaitState {
    id: NonZeroU64,
    runtime: EvaluationRuntimeId,
    owner_id: EvaluationSessionId,
    owner: Weak<EvaluationSession>,
    producer: EvaluationTaskId,
    terminal: OnceLock<EvaluationWaitTerminal>,
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

    fn owner(&self) -> Option<Arc<EvaluationSession>> {
        self.0.owner.upgrade()
    }

    fn belongs_to(&self, session: &Arc<EvaluationSession>) -> bool {
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

    pub(crate) fn publish_promise_assignment(&self, assignment: &PromiseAssignment) {
        let terminal = promise_assignment_terminal(self.runtime_id(), assignment);
        if let Some(owner) = self.owner() {
            owner.complete_promise_wait(self, terminal);
        } else {
            self.publish_terminal(terminal);
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationTaskBlock {
    pub(crate) lazy: Option<EvaluationWaitToken>,
    pub(crate) observed_generation: Option<u64>,
    pub(crate) error: Option<Arc<EvaluationFailure>>,
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
    pub(crate) observed_generation: Option<u64>,
    pub(crate) error: Option<Arc<EvaluationFailure>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvaluationTaskState {
    /// A task registered in an intentionally bare standalone session.
    Dormant,
    Reserved,
    Queued,
    Running,
    Blocked(EvaluationTaskBlock),
    Complete(RuntimeValueRoot),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
    Abandoned,
}

struct ReflectionTaskRecord {
    id: EvaluationTaskId,
    state: EvaluationTaskState,
    machine: Option<Box<dyn EvaluationTaskMachine>>,
    cancel_requested: bool,
    error_acknowledged: bool,
    status_sinks: Vec<Arc<dyn EvaluationTaskStatusSink>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeferredTaskState {
    Dormant,
    Running,
    Blocked(EvaluationTaskBlock),
    Complete(RuntimeValueRoot),
    Failed(Arc<EvaluationFailure>),
    Abandoned,
}

fn publish_reflection_state(
    wait: &EvaluationWaitToken,
    state: EvaluationTaskState,
) -> EvaluationTaskState {
    let terminal = match state {
        EvaluationTaskState::Complete(value) => EvaluationWaitTerminal::Complete(value),
        EvaluationTaskState::Failed(error) => EvaluationWaitTerminal::Failed(error),
        EvaluationTaskState::Cancelled => EvaluationWaitTerminal::Cancelled,
        EvaluationTaskState::Abandoned => EvaluationWaitTerminal::Abandoned,
        state => return state,
    };
    match wait.publish_terminal(terminal) {
        EvaluationWaitTerminal::Complete(value) => EvaluationTaskState::Complete(value),
        EvaluationWaitTerminal::Failed(error) => EvaluationTaskState::Failed(error),
        EvaluationWaitTerminal::Cancelled => EvaluationTaskState::Cancelled,
        EvaluationWaitTerminal::Abandoned => EvaluationTaskState::Abandoned,
    }
}

fn publish_deferred_state(
    wait: &EvaluationWaitToken,
    state: DeferredTaskState,
) -> DeferredTaskState {
    let terminal = match state {
        DeferredTaskState::Complete(value) => EvaluationWaitTerminal::Complete(value),
        DeferredTaskState::Failed(error) => EvaluationWaitTerminal::Failed(error),
        DeferredTaskState::Abandoned => EvaluationWaitTerminal::Abandoned,
        state => return state,
    };
    match wait.publish_terminal(terminal) {
        EvaluationWaitTerminal::Complete(value) => DeferredTaskState::Complete(value),
        EvaluationWaitTerminal::Failed(error) => DeferredTaskState::Failed(error),
        EvaluationWaitTerminal::Cancelled => {
            unreachable!("a deferred wait cannot publish cancellation")
        }
        EvaluationWaitTerminal::Abandoned => DeferredTaskState::Abandoned,
    }
}

#[derive(Clone)]
enum DeferredValue {
    Lazy(LazyValue),
    Promise(PromisedValue),
}

impl DeferredValue {
    fn id(&self) -> DeferredValueId {
        match self {
            Self::Lazy(lazy) => lazy.id().into(),
            Self::Promise(promise) => promise.id().into(),
        }
    }

    fn label(&self) -> &Arc<str> {
        match self {
            Self::Lazy(lazy) => lazy.label(),
            Self::Promise(promise) => promise.label(),
        }
    }
}

struct DeferredTaskRecord {
    id: EvaluationTaskId,
    wait: EvaluationWaitToken,
    value: DeferredValue,
    state: DeferredTaskState,
    /// The strict deferred producer currently preventing this task from
    /// reaching WHNF. External waits remain in `state` but do not enter this
    /// graph.
    dependency: Option<DeferredValueId>,
    machine: Option<Box<dyn EvaluationTaskMachine>>,
}

struct TaskStatusUpdate {
    status: EvaluationTaskStatus,
    sinks: Vec<Arc<dyn EvaluationTaskStatusSink>>,
}

fn task_status(state: &EvaluationTaskState) -> EvaluationTaskStatus {
    match state {
        EvaluationTaskState::Dormant
        | EvaluationTaskState::Reserved
        | EvaluationTaskState::Queued
        | EvaluationTaskState::Running => EvaluationTaskStatus::Launched,
        EvaluationTaskState::Blocked(_) => EvaluationTaskStatus::Blocked,
        EvaluationTaskState::Complete(value) => EvaluationTaskStatus::Complete(value.clone()),
        EvaluationTaskState::Failed(error) => EvaluationTaskStatus::Failed(error.clone()),
        EvaluationTaskState::Cancelled => EvaluationTaskStatus::Cancelled,
        EvaluationTaskState::Abandoned => EvaluationTaskStatus::Abandoned,
    }
}

fn task_status_update(
    record: &mut ReflectionTaskRecord,
    prior: Option<&EvaluationTaskState>,
) -> Option<TaskStatusUpdate> {
    if record.status_sinks.is_empty() {
        return None;
    }
    let status = task_status(&record.state);
    if prior.is_some_and(|prior| task_status(prior) == status) {
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

#[derive(Debug)]
struct PromiseRecord {
    producer: EvaluationTaskId,
    result: Weak<OnceLock<PromiseAssignment>>,
}

#[derive(Default)]
struct EvaluationTasks {
    reflection: HashMap<EvaluationWaitToken, ReflectionTaskRecord>,
    reflection_by_id: BTreeMap<EvaluationTaskId, EvaluationWaitToken>,
    unacknowledged_failures: RedBlackTreeMapSync<EvaluationTaskId, Arc<EvaluationFailure>>,
    ready: VecDeque<EvaluationTaskId>,
    promises: HashMap<EvaluationWaitToken, PromiseRecord>,
    owned_promises: HashMap<EvaluationTaskId, Vec<EvaluationWaitToken>>,
    deferred: HashMap<DeferredValueId, DeferredTaskRecord>,
    deferred_by_wait: HashMap<EvaluationWaitToken, DeferredValueId>,
    deferred_by_task: HashMap<EvaluationTaskId, DeferredValueId>,
}

struct ReflectionTaskTransition {
    retired: Option<ReflectionTaskRecord>,
    status: Option<TaskStatusUpdate>,
}

fn transition_reflection_task(
    tasks: &mut EvaluationTasks,
    wait: &EvaluationWaitToken,
    state: EvaluationTaskState,
    prior: &EvaluationTaskState,
) -> ReflectionTaskTransition {
    let terminal = matches!(
        state,
        EvaluationTaskState::Complete(_)
            | EvaluationTaskState::Failed(_)
            | EvaluationTaskState::Cancelled
            | EvaluationTaskState::Abandoned
    );
    let (unacknowledged_failure, status) = {
        let record = tasks
            .reflection
            .get_mut(wait)
            .expect("transitioned reflection task must remain registered");
        record.state = publish_reflection_state(wait, state);
        let failure = match &record.state {
            EvaluationTaskState::Failed(error) if !record.error_acknowledged => {
                Some((record.id, error.clone()))
            }
            _ => None,
        };
        let status = task_status_update(record, Some(prior));
        (failure, status)
    };
    if let Some((task, failure)) = unacknowledged_failure {
        tasks.unacknowledged_failures.insert_mut(task, failure);
    }
    let retired = terminal.then(|| retire_reflection_task(tasks, wait));
    ReflectionTaskTransition { retired, status }
}

fn retire_reflection_task(
    tasks: &mut EvaluationTasks,
    wait: &EvaluationWaitToken,
) -> ReflectionTaskRecord {
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

fn promise_record_terminal(
    wait: &EvaluationWaitToken,
    record: &PromiseRecord,
) -> Option<EvaluationWaitTerminal> {
    let Some(result) = record.result.upgrade() else {
        return Some(EvaluationWaitTerminal::Failed(evaluation_failure(
            "promised value no longer exists",
        )));
    };
    result
        .get()
        .map(|assignment| promise_assignment_terminal(wait.runtime_id(), assignment))
}

fn retire_promise_wait(
    tasks: &mut EvaluationTasks,
    wait: &EvaluationWaitToken,
) -> Option<PromiseRecord> {
    let record = tasks.promises.remove(wait)?;
    let remove_owner = {
        let waits = tasks
            .owned_promises
            .get_mut(&record.producer)
            .expect("a registered task promise must belong to its producer");
        let index = waits
            .iter()
            .position(|candidate| candidate == wait)
            .expect("the promise owner index must contain its registered wait");
        waits.swap_remove(index);
        waits.is_empty()
    };
    if remove_owner {
        tasks.owned_promises.remove(&record.producer);
    }
    Some(record)
}

fn prune_terminal_promise_waits(tasks: &mut EvaluationTasks) {
    let terminal = tasks
        .promises
        .iter()
        .filter_map(|(wait, record)| {
            promise_record_terminal(wait, record).map(|terminal| (wait.clone(), terminal))
        })
        .collect::<Vec<_>>();
    for (wait, terminal) in terminal {
        wait.publish_terminal(terminal);
        let retired = retire_promise_wait(tasks, &wait);
        debug_assert!(retired.is_some());
    }
}

fn retire_deferred_task(
    tasks: &mut EvaluationTasks,
    deferred: DeferredValueId,
) -> DeferredTaskRecord {
    let record = tasks
        .deferred
        .remove(&deferred)
        .expect("retired deferred task must remain registered");
    assert_eq!(
        tasks.deferred_by_wait.remove(&record.wait),
        Some(deferred),
        "deferred wait index must agree with its task record"
    );
    assert_eq!(
        tasks.deferred_by_task.remove(&record.id),
        Some(deferred),
        "deferred task ID index must agree with its task record"
    );
    record
}

pub(crate) struct EvaluationSession {
    id: EvaluationSessionId,
    values: CoreValueFactory,
    tasks: Mutex<EvaluationTasks>,
    task_changed: Condvar,
    default_reflection_profile: Arc<ReflectionTaskProfile>,
    require_default_reflection_profile: bool,
    coordinator: Weak<EvaluationWorkCoordinator>,
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
        let (mut reflection, deferred, statuses) = {
            let tasks = self
                .tasks
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            // A task-owned promise is a producer obligation, unlike a
            // reusable lazy or host-promise follower. Publish its permanent
            // failure before waking any task waits which may depend on it.
            let promise_waits = tasks.promises.keys().cloned().collect::<Vec<_>>();
            for wait in promise_waits {
                let record = tasks
                    .promises
                    .get(&wait)
                    .expect("collected promise wait must remain registered");
                let terminal = if let Some(result) = record.result.upgrade() {
                    let _ = result.set(Err(evaluation_failure(format!(
                        "promised value's producer task {} was abandoned when its evaluation session closed",
                        record.producer.get()
                    ))));
                    promise_assignment_terminal(
                        wait.runtime_id(),
                        result
                            .get()
                            .expect("abandoned producer must leave a terminal promise assignment"),
                    )
                } else {
                    EvaluationWaitTerminal::Failed(evaluation_failure(
                        "promised value no longer exists",
                    ))
                };
                wait.publish_terminal(terminal);
                let retired = retire_promise_wait(tasks, &wait);
                debug_assert!(retired.is_some());
            }

            let reflection_waits = tasks.reflection.keys().cloned().collect::<Vec<_>>();
            let mut reflection = Vec::with_capacity(reflection_waits.len());
            let mut statuses = Vec::new();
            for wait in reflection_waits {
                let record = tasks
                    .reflection
                    .get(&wait)
                    .expect("collected reflection wait must remain registered");
                let prior = record.state.clone();
                let state = if record.cancel_requested {
                    EvaluationTaskState::Cancelled
                } else {
                    EvaluationTaskState::Abandoned
                };
                let transition = transition_reflection_task(tasks, &wait, state, &prior);
                reflection.push(
                    transition
                        .retired
                        .expect("session shutdown must retire a reflection task"),
                );
                statuses.extend(transition.status);
            }

            let deferred_ids = tasks.deferred.keys().copied().collect::<Vec<_>>();
            let mut deferred = Vec::with_capacity(deferred_ids.len());
            for deferred_id in deferred_ids {
                let record = tasks
                    .deferred
                    .get_mut(&deferred_id)
                    .expect("collected deferred task must remain registered");
                record.state = publish_deferred_state(&record.wait, DeferredTaskState::Abandoned);
                record.dependency = None;
                deferred.push(retire_deferred_task(tasks, deferred_id));
            }
            tasks.ready.clear();
            (reflection, deferred, statuses)
        };

        for status in statuses {
            publish_task_status(Some(status));
        }
        for record in &mut reflection {
            if matches!(record.state, EvaluationTaskState::Cancelled)
                && let Some(machine) = &mut record.machine
            {
                machine.cancel();
            }
        }
        drop(reflection);
        drop(deferred);

        if let Some(coordinator) = self.coordinator.upgrade() {
            coordinator.unregister_session(self.id);
        }
    }
}

impl EvaluationSession {
    fn acknowledge_reflection_task_error(&self, task: &EvaluationTaskHandle) {
        let mut tasks = self
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        if let Some(record) = tasks.reflection.get_mut(&task.wait) {
            record.error_acknowledged = true;
        } else {
            tasks.unacknowledged_failures.remove_mut(&task.id);
        }
    }

    fn with_execution_resources(
        coordinator: Weak<EvaluationWorkCoordinator>,
        values: CoreValueFactory,
    ) -> Self {
        Self {
            id: EvaluationSessionId(values.ids().evaluation_session()),
            values,
            tasks: Mutex::new(EvaluationTasks::default()),
            task_changed: Condvar::new(),
            default_reflection_profile: Arc::new(ReflectionTaskProfile::unsealed()),
            require_default_reflection_profile: false,
            coordinator,
        }
    }

    fn isolated(values: CoreValueFactory) -> Arc<Self> {
        Arc::new(Self::with_execution_resources(Weak::new(), values))
    }

    fn with_execution_resources_and_default_profile(
        coordinator: Weak<EvaluationWorkCoordinator>,
        values: CoreValueFactory,
        default_reflection_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        Self {
            id: EvaluationSessionId(values.ids().evaluation_session()),
            values,
            tasks: Mutex::new(EvaluationTasks::default()),
            task_changed: Condvar::new(),
            default_reflection_profile,
            require_default_reflection_profile: true,
            coordinator,
        }
    }

    #[cfg(test)]
    pub(crate) fn shared(coordinator: &Arc<EvaluationWorkCoordinator>) -> Arc<Self> {
        let session = Arc::new(Self::with_execution_resources(
            Arc::downgrade(coordinator),
            crate::core::test_value_factory(),
        ));
        coordinator.register_session(&session);
        session
    }

    pub(crate) fn shared_with_default_profile(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        values: CoreValueFactory,
        default_reflection_profile: Arc<ReflectionTaskProfile>,
    ) -> Arc<Self> {
        let session = Arc::new(Self::with_execution_resources_and_default_profile(
            Arc::downgrade(coordinator),
            values,
            default_reflection_profile,
        ));
        coordinator.register_session(&session);
        session
    }

    fn notify_executor_ready(&self) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            coordinator.notify_session_ready(self.id);
        }
    }

    fn notify_spark_disturbance(&self) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            coordinator.notify_spark_disturbance(self.id);
        }
    }

    /// Releases a reusable deferred producer claimed only to satisfy an
    /// abandoned spark. Running producers remain task-owned in Phase 3B and
    /// complete through the existing session path.
    fn abandon_spark_wait(&self, wait: &EvaluationWaitToken) {
        let mut wait = wait.clone();
        loop {
            if wait.owner_id() != self.id || wait.terminal_poll().is_some() {
                return;
            }
            let (retired, dependency) = {
                let mut tasks = self
                    .tasks
                    .lock()
                    .expect("evaluation task registry was poisoned");
                let Some(deferred) = tasks.deferred_by_wait.get(&wait).copied() else {
                    return;
                };
                let Some(record) = tasks.deferred.get_mut(&deferred) else {
                    return;
                };
                if matches!(record.state, DeferredTaskState::Running) {
                    return;
                }
                let dependency = match &record.state {
                    DeferredTaskState::Blocked(block) => block.lazy.clone(),
                    _ => None,
                };
                record.state = publish_deferred_state(&wait, DeferredTaskState::Abandoned);
                record.dependency = None;
                let retired = retire_deferred_task(&mut tasks, deferred);
                self.task_changed.notify_all();
                (retired, dependency)
            };
            drop(retired);
            let Some(dependency) = dependency else {
                return;
            };
            wait = dependency;
        }
    }

    fn complete_promise_wait(&self, wait: &EvaluationWaitToken, terminal: EvaluationWaitTerminal) {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        wait.publish_terminal(terminal);
        retire_promise_wait(&mut tasks, wait);
        self.task_changed.notify_all();
    }

    pub(crate) fn submit_spark(self: &Arc<Self>, value: Value) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            coordinator.submit_spark(self, value);
        }
    }
}

/// Cheap per-evaluation handle to one shared assembler session.
///
/// Narrower provenance can be added to this handle without duplicating the
/// session-owned scheduler and reflection state.
#[derive(Debug, Clone)]
pub(crate) struct EvalContext {
    session: Arc<EvaluationSession>,
    task_profile: Arc<ReflectionTaskProfile>,
    task: Arc<OnceLock<Result<EvaluationTaskId, Arc<str>>>>,
    scheduled_task: bool,
    waits_for_claimed_tasks: bool,
    originating_task: Option<EvaluationTaskId>,
}

impl EvalContext {
    #[cfg(test)]
    pub(crate) fn standalone() -> Self {
        Self::isolated(crate::core::test_value_factory())
    }

    pub(crate) fn new(session: Arc<EvaluationSession>) -> Self {
        let task_profile = session.default_reflection_profile.clone();
        Self {
            session,
            task_profile,
            task: Arc::new(OnceLock::new()),
            scheduled_task: false,
            waits_for_claimed_tasks: false,
            originating_task: None,
        }
    }

    pub(crate) fn with_task_profile(
        session: Arc<EvaluationSession>,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        Self {
            session,
            task_profile,
            task: Arc::new(OnceLock::new()),
            scheduled_task: false,
            waits_for_claimed_tasks: false,
            originating_task: None,
        }
    }

    pub(crate) fn patient_with_task_profile(
        session: Arc<EvaluationSession>,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        Self {
            waits_for_claimed_tasks: true,
            ..Self::with_task_profile(session, task_profile)
        }
    }

    fn for_task(
        session: Arc<EvaluationSession>,
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
            scheduled_task: true,
            waits_for_claimed_tasks: false,
            originating_task: Some(id),
        }
    }

    fn for_deferred_task(
        session: Arc<EvaluationSession>,
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
            scheduled_task: true,
            waits_for_claimed_tasks: false,
            originating_task,
        }
    }

    pub(crate) fn values(&self) -> &CoreValueFactory {
        &self.session.values
    }

    /// Creates a zero-worker context in an explicitly selected runtime value
    /// domain. This is for pure closed bootstrap construction and focused
    /// tests; production task services use a runtime-registered session.
    pub(crate) fn isolated(values: CoreValueFactory) -> Self {
        Self::new(EvaluationSession::isolated(values))
    }

    pub(crate) fn spark(&self, value: Value) {
        // A promise names data whose producer or completed assignment may
        // expose useful work. Nets and the remaining variants are already in
        // WHNF; metadata adds one privileged hidden demand.
        if matches!(
            value,
            Value::Lazy(_) | Value::Promised(_) | Value::Metadata(_)
        ) {
            self.session.submit_spark(value);
        }
    }

    /// Signals completion of a host-owned promise to pumps of this context's
    /// session. Promise values may be shared across sessions, but completion
    /// deliberately does not discover or wake observers in other sessions.
    pub(crate) fn notify_promise_changed(&self) {
        // Taking the scheduler lock pairs this notification with condvar waits
        // and prevents a completion between their final check and sleep from
        // being lost. Public resolvers are never stored inside task records;
        // pruning here only collects task-owned waits whose terminal state is
        // already visible.
        let mut tasks = self
            .session
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_terminal_promise_waits(&mut tasks);
        self.session.task_changed.notify_all();
        drop(tasks);
        self.session.notify_spark_disturbance();
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
    /// Rechecking under the scheduler mutex prevents a producer release
    /// between [`Self::pump_wait`] and this call from becoming a lost wakeup.
    pub(crate) fn wait_for_claimed_task(&self, target: &EvaluationWaitToken) {
        if target.owner_id() != self.session.id {
            return;
        }
        let tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        if !target_has_running_producer(&tasks, target) {
            return;
        }
        drop(
            self.session
                .task_changed
                .wait(tasks)
                .expect("evaluation task registry was poisoned"),
        );
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
        self.deferred_task(DeferredValue::Lazy(lazy.clone()), build)
    }

    pub(crate) fn promise_task<F>(
        &self,
        promise: &PromisedValue,
        build: F,
    ) -> Result<EvaluationWaitToken, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Box<dyn EvaluationTaskMachine>,
    {
        self.deferred_task(DeferredValue::Promise(promise.clone()), build)
    }

    fn deferred_task<F>(
        &self,
        value: DeferredValue,
        build: F,
    ) -> Result<EvaluationWaitToken, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Box<dyn EvaluationTaskMachine>,
    {
        let deferred = value.id();
        {
            let tasks = self
                .session
                .tasks
                .lock()
                .expect("evaluation task registry was poisoned");
            if let Some(record) = tasks.deferred.get(&deferred) {
                return Ok(record.wait.clone());
            }
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
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        if let Some(record) = tasks.deferred.get(&deferred) {
            return Ok(record.wait.clone());
        }
        let record = DeferredTaskRecord {
            id,
            wait: wait.clone(),
            value,
            state: DeferredTaskState::Dormant,
            dependency: None,
            machine: Some(machine),
        };
        assert!(
            tasks.deferred.insert(deferred, record).is_none()
                && tasks
                    .deferred_by_wait
                    .insert(wait.clone(), deferred)
                    .is_none()
                && tasks.deferred_by_task.insert(id, deferred).is_none(),
            "deferred task identities must be unique"
        );
        self.session.task_changed.notify_all();
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
        result: &Arc<OnceLock<PromiseAssignment>>,
    ) -> Result<(EvaluationTaskId, EvaluationWaitToken), Arc<str>> {
        let owner = self.task_id()?;
        let wait = allocate_wait_token(&self.session, owner)?;
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let replaced = tasks.promises.insert(
            wait.clone(),
            PromiseRecord {
                producer: owner,
                result: Arc::downgrade(result),
            },
        );
        assert!(replaced.is_none(), "evaluation wait tokens must be unique");
        tasks
            .owned_promises
            .entry(owner)
            .or_default()
            .push(wait.clone());
        Ok((owner, wait))
    }

    pub(crate) fn fail_unresolved_promises(&self, failure: Arc<EvaluationFailure>) {
        let Some(Ok(owner)) = self.task.get() else {
            return;
        };
        let owner = *owner;
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let waits = tasks.owned_promises.remove(&owner).unwrap_or_default();
        for wait in waits {
            let Some(promise) = tasks.promises.remove(&wait) else {
                continue;
            };
            let terminal = if let Some(result) = promise.result.upgrade() {
                let _ = result.set(Err(failure.clone()));
                promise_assignment_terminal(
                    wait.runtime_id(),
                    result
                        .get()
                        .expect("promise assignment must be set after producer failure"),
                )
            } else {
                EvaluationWaitTerminal::Failed(evaluation_failure(
                    "promised value no longer exists",
                ))
            };
            wait.publish_terminal(terminal);
        }
        self.session.task_changed.notify_all();
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
        let id = allocate_task_id(self.values())?;
        let wait = allocate_wait_token(&self.session, id)?;
        let context = Self::for_task(self.session.clone(), id, self.task_profile.clone());
        let machine = build(context)?;
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let replaced = tasks.reflection.insert(
            wait.clone(),
            ReflectionTaskRecord {
                id,
                state: EvaluationTaskState::Queued,
                machine: Some(machine),
                cancel_requested: false,
                error_acknowledged: false,
                status_sinks: Vec::new(),
            },
        );
        let replaced_id = tasks.reflection_by_id.insert(id, wait.clone());
        assert!(
            replaced.is_none() && replaced_id.is_none(),
            "evaluation task identities must be unique"
        );
        tasks.ready.push_back(id);
        self.session.task_changed.notify_all();
        drop(tasks);
        self.session.notify_executor_ready();
        Ok(EvaluationTaskHandle { id, wait })
    }

    fn reserve_task(&self) -> Result<EvaluationTaskHandle, Arc<str>> {
        let id = allocate_task_id(self.values())?;
        let wait = allocate_wait_token(&self.session, id)?;
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let replaced = tasks.reflection.insert(
            wait.clone(),
            ReflectionTaskRecord {
                id,
                state: EvaluationTaskState::Reserved,
                machine: None,
                cancel_requested: false,
                error_acknowledged: false,
                status_sinks: Vec::new(),
            },
        );
        let replaced_id = tasks.reflection_by_id.insert(id, wait.clone());
        assert!(
            replaced.is_none() && replaced_id.is_none(),
            "evaluation task identities must be unique"
        );
        self.session.task_changed.notify_all();
        Ok(EvaluationTaskHandle { id, wait })
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
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let Some(record) = tasks.reflection.get_mut(&handle.wait) else {
            return;
        };
        if !matches!(record.state, EvaluationTaskState::Reserved) {
            return;
        }
        let prior = record.state.clone();
        record.error_acknowledged = error_acknowledged;
        if let Some(status_sink) = status_sink {
            record.status_sinks.push(status_sink);
        }
        let state = match result {
            Ok(machine) => {
                record.machine = Some(machine);
                EvaluationTaskState::Queued
            }
            Err(error) => EvaluationTaskState::Failed(error),
        };
        let queued = matches!(state, EvaluationTaskState::Queued);
        let transition = transition_reflection_task(&mut tasks, &handle.wait, state, &prior);
        if queued {
            tasks.ready.push_back(handle.id);
        }
        self.session.task_changed.notify_all();
        drop(tasks);
        drop(transition.retired);
        publish_task_status(transition.status);
        if queued {
            self.session.notify_executor_ready();
        }
    }

    fn cancel_reserved_task(&self, handle: &EvaluationTaskHandle) {
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let retired = if tasks
            .reflection
            .get(&handle.wait)
            .is_some_and(|record| matches!(record.state, EvaluationTaskState::Reserved))
        {
            self.session.task_changed.notify_all();
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
        let transition = {
            let mut tasks = self
                .session
                .tasks
                .lock()
                .expect("evaluation task registry was poisoned");
            let record = tasks
                .reflection
                .get_mut(&handle.wait)
                .expect("a committed pending task must remain reserved");
            assert!(
                matches!(record.state, EvaluationTaskState::Reserved),
                "only a reserved task may apply its pre-launch policy"
            );
            let prior = record.state.clone();
            record.status_sinks.push(status_sink);
            let transition = transition_reflection_task(
                &mut tasks,
                &handle.wait,
                EvaluationTaskState::Cancelled,
                &prior,
            );
            self.session.task_changed.notify_all();
            transition
        };
        drop(transition.retired);
        publish_task_status(transition.status);
    }

    pub(crate) fn reserve_reflection_task(
        &self,
        effect: Value,
    ) -> Result<PendingReflectionTask, Arc<str>> {
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
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let replaced = tasks.reflection.insert(
            wait.clone(),
            ReflectionTaskRecord {
                id,
                state: EvaluationTaskState::Dormant,
                machine: None,
                cancel_requested: false,
                error_acknowledged: false,
                status_sinks: Vec::new(),
            },
        );
        let replaced_id = tasks.reflection_by_id.insert(id, wait.clone());
        assert!(
            replaced.is_none() && replaced_id.is_none(),
            "evaluation task identities must be unique"
        );
        Ok(EvaluationTaskHandle { id, wait })
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
        let (retired, status) = {
            let mut tasks = self
                .session
                .tasks
                .lock()
                .expect("evaluation task registry was poisoned");
            if task.wait.terminal_poll().is_some() {
                return EvaluationTaskCancellation::Late;
            }
            let Some(record) = tasks.reflection.get_mut(&task.wait) else {
                return EvaluationTaskCancellation::Late;
            };
            match record.state {
                EvaluationTaskState::Complete(_)
                | EvaluationTaskState::Failed(_)
                | EvaluationTaskState::Cancelled
                | EvaluationTaskState::Abandoned => {
                    unreachable!("terminal reflection records must be retired")
                }
                EvaluationTaskState::Running => {
                    record.cancel_requested = true;
                    return EvaluationTaskCancellation::Requested;
                }
                EvaluationTaskState::Dormant
                | EvaluationTaskState::Reserved
                | EvaluationTaskState::Queued
                | EvaluationTaskState::Blocked(_) => {
                    let prior = record.state.clone();
                    let transition = transition_reflection_task(
                        &mut tasks,
                        &task.wait,
                        EvaluationTaskState::Cancelled,
                        &prior,
                    );
                    self.session.task_changed.notify_all();
                    (transition.retired, transition.status)
                }
            }
        };
        publish_task_status(status);
        let mut retired = retired.expect("terminal cancellation must retire its task record");
        if let Some(mut machine) = retired.machine.take() {
            machine.cancel();
        }
        drop(retired);
        EvaluationTaskCancellation::Requested
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
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned")
            .unacknowledged_failures
            .remove_mut(&task);
    }

    pub(crate) fn poll_wait(&self, wait: &EvaluationWaitToken) -> EvaluationWaitPoll {
        if let Some(terminal) = wait.terminal_poll() {
            return terminal;
        }
        let owner = match wait.owner() {
            Some(owner) => owner,
            None => {
                return wait
                    .terminal_poll()
                    .unwrap_or(EvaluationWaitPoll::Abandoned);
            }
        };
        let mut tasks = owner
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        if let Some(terminal) = wait.terminal_poll() {
            return terminal;
        }
        if let Some(terminal) = tasks
            .promises
            .get(wait)
            .and_then(|record| promise_record_terminal(wait, record))
        {
            let terminal = wait.publish_terminal(terminal);
            retire_promise_wait(&mut tasks, wait);
            owner.task_changed.notify_all();
            return terminal.to_poll();
        }
        if tasks.reflection.contains_key(wait)
            || tasks.deferred_by_wait.contains_key(wait)
            || tasks.promises.contains_key(wait)
        {
            return EvaluationWaitPoll::Pending(wait.clone());
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
        self.session.pump(self, wait, step_budget)
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
        let target = wait.clone();
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let wait = test_reflection_dependency(&tasks, wait);
        let prior = tasks
            .reflection
            .get(&wait)
            .expect("test task must belong to this session")
            .state
            .clone();
        let transition = transition_reflection_task(
            &mut tasks,
            &wait,
            EvaluationTaskState::Complete(RuntimeValueRoot::new(&self.session.values, value)),
            &prior,
        );
        self.session.task_changed.notify_all();
        drop(tasks);
        drop(transition.retired);
        publish_task_status(transition.status);
        while matches!(
            self.pump_wait(&target, 256),
            EvaluationPumpOutcome::BudgetExhausted
        ) {}
    }

    #[cfg(test)]
    pub(crate) fn fail_wait(&self, wait: &EvaluationWaitToken, error: impl Into<Arc<str>>) {
        let error = error.into();
        let target = wait.clone();
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let wait = test_reflection_dependency(&tasks, wait);
        let prior = tasks
            .reflection
            .get(&wait)
            .expect("test task must belong to this session")
            .state
            .clone();
        let transition = transition_reflection_task(
            &mut tasks,
            &wait,
            EvaluationTaskState::Failed(evaluation_failure(error.as_ref())),
            &prior,
        );
        self.session.task_changed.notify_all();
        drop(tasks);
        drop(transition.retired);
        publish_task_status(transition.status);
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
        let tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        debug_assert!(tasks.reflection.values().all(|record| !matches!(
            record.state,
            EvaluationTaskState::Complete(_)
                | EvaluationTaskState::Failed(_)
                | EvaluationTaskState::Cancelled
        )));
        debug_assert!(tasks.deferred.values().all(|record| !matches!(
            record.state,
            DeferredTaskState::Complete(_) | DeferredTaskState::Failed(_)
        )));
        let promises_terminal = tasks
            .promises
            .values()
            .filter(|promise| {
                promise
                    .result
                    .upgrade()
                    .is_none_or(|result| result.get().is_some())
            })
            .count();
        EvaluationTaskRegistryCounts {
            reflection_active: tasks.reflection.len(),
            reflection_terminal: 0,
            reflection_by_id: tasks.reflection_by_id.len(),
            unacknowledged_failures: tasks.unacknowledged_failures.size(),
            deferred_active: tasks.deferred.len(),
            deferred_terminal: 0,
            deferred_by_wait: tasks.deferred_by_wait.len(),
            deferred_by_task: tasks.deferred_by_task.len(),
            promises_active: tasks.promises.len() - promises_terminal,
            promises_terminal,
            owned_promise_waits: tasks.owned_promises.values().map(Vec::len).sum(),
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
        self.deferred_failure(promise.id().into())
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
    fn deferred_failure(&self, deferred: DeferredValueId) -> Option<Arc<EvaluationFailure>> {
        let tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let record = tasks.deferred.get(&deferred)?;
        match &record.state {
            DeferredTaskState::Failed(failure) => Some(failure.clone()),
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
    tasks: &EvaluationTasks,
    wait: &EvaluationWaitToken,
) -> EvaluationWaitToken {
    let mut wait = wait.clone();
    let mut seen = HashSet::new();
    while seen.insert(wait.get()) {
        let Some(deferred) = tasks.deferred_by_wait.get(&wait) else {
            break;
        };
        let Some(record) = tasks.deferred.get(deferred) else {
            break;
        };
        let DeferredTaskState::Blocked(block) = &record.state else {
            break;
        };
        let Some(dependency) = &block.lazy else {
            break;
        };
        wait = dependency.clone();
    }
    wait
}

const TASK_POLL_QUANTUM: usize = 64;

struct ClaimedReflectionTask {
    id: EvaluationTaskId,
    wait: EvaluationWaitToken,
    prior_state: EvaluationTaskState,
    machine: Box<dyn EvaluationTaskMachine>,
}

struct ClaimedDeferredTask {
    id: EvaluationTaskId,
    deferred: DeferredValueId,
    prior_state: DeferredTaskState,
    prior_dependency: Option<DeferredValueId>,
    machine: Box<dyn EvaluationTaskMachine>,
}

enum ReleasedTaskMachine {
    Drop(Box<dyn EvaluationTaskMachine>),
    Cancel(Box<dyn EvaluationTaskMachine>),
}

impl ReleasedTaskMachine {
    fn finish(self) {
        match self {
            Self::Drop(machine) => drop(machine),
            Self::Cancel(mut machine) => machine.cancel(),
        }
    }
}

struct ReportedDependency {
    task: EvaluationTaskId,
    session: EvaluationSessionId,
    wait: u64,
    live_cross_session: bool,
}

enum ClaimedTask {
    Reflection(ClaimedReflectionTask),
    Deferred(ClaimedDeferredTask),
}

impl ClaimedTask {
    fn id(&self) -> EvaluationTaskId {
        match self {
            Self::Reflection(task) => task.id,
            Self::Deferred(task) => task.id,
        }
    }

    fn poll(&mut self, step_budget: usize) -> EvaluationMachinePoll {
        match self {
            Self::Reflection(task) => task.machine.poll(step_budget),
            Self::Deferred(task) => task.machine.poll(step_budget),
        }
    }
}

impl EvaluationSession {
    fn run_until_quiescent(&self) -> EvaluationSessionRun {
        let mut attempted_blocked = HashSet::new();
        loop {
            let mut claimed = loop {
                let mut tasks = self
                    .tasks
                    .lock()
                    .expect("evaluation task registry was poisoned");
                if let Some(claimed) = claim_ready_task(&mut tasks)
                    .or_else(|| claim_blocked_task(&mut tasks, &attempted_blocked))
                {
                    break claimed;
                }
                if tasks
                    .reflection
                    .values()
                    .any(|record| matches!(record.state, EvaluationTaskState::Running))
                {
                    drop(
                        self.task_changed
                            .wait(tasks)
                            .expect("evaluation task registry was poisoned"),
                    );
                    continue;
                }
                return self.session_run_report(&tasks);
            };

            let poll = claimed.poll(TASK_POLL_QUANTUM);
            let claimed_id = claimed.id();
            let (made_progress, remains_blocked, released, status) =
                self.release_task(claimed, poll);
            publish_task_status(status);
            self.notify_executor_if_ready();
            if let Some(machine) = released {
                machine.finish();
            }
            if remains_blocked {
                attempted_blocked.insert(claimed_id);
            }
            if made_progress {
                attempted_blocked.clear();
            }
        }
    }

    fn session_run_report(&self, tasks: &EvaluationTasks) -> EvaluationSessionRun {
        let mut unfinished = Vec::new();
        let mut has_live_cross_session_dependency = false;
        for (task, wait) in &tasks.reflection_by_id {
            let record = tasks
                .reflection
                .get(wait)
                .expect("task ID index must refer to a task record");
            let (state, block) = match &record.state {
                EvaluationTaskState::Dormant => (EvaluationUnfinishedState::Dormant, None),
                EvaluationTaskState::Reserved => (EvaluationUnfinishedState::Reserved, None),
                EvaluationTaskState::Queued => (EvaluationUnfinishedState::Queued, None),
                EvaluationTaskState::Running => (EvaluationUnfinishedState::Running, None),
                EvaluationTaskState::Blocked(block) => {
                    (EvaluationUnfinishedState::Blocked, Some(block))
                }
                EvaluationTaskState::Complete(_)
                | EvaluationTaskState::Failed(_)
                | EvaluationTaskState::Cancelled
                | EvaluationTaskState::Abandoned => {
                    unreachable!("terminal reflection records must be retired")
                }
            };
            let dependency = block
                .and_then(|block| block.lazy.as_ref())
                .map(|wait| self.reported_dependency(tasks, wait));
            has_live_cross_session_dependency |= dependency
                .as_ref()
                .is_some_and(|dependency| dependency.live_cross_session);
            unfinished.push(EvaluationUnfinishedTask {
                task: *task,
                state,
                dependency: dependency.as_ref().map(|dependency| dependency.task),
                dependency_session: dependency.as_ref().map(|dependency| dependency.session),
                wait: dependency.as_ref().map(|dependency| dependency.wait),
                observed_generation: block.and_then(|block| block.observed_generation),
                error: block.and_then(|block| block.error.clone()),
            });
        }
        let report = EvaluationSessionReport {
            failures: tasks.unacknowledged_failures.clone(),
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

    fn reported_dependency(
        &self,
        tasks: &EvaluationTasks,
        initial: &EvaluationWaitToken,
    ) -> ReportedDependency {
        let mut wait = initial.clone();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(wait.get()) || wait.owner_id() != self.id {
                return ReportedDependency {
                    task: wait.producer(),
                    session: wait.owner_id(),
                    wait: wait.get(),
                    live_cross_session: wait.owner_id() != self.id && wait.owner().is_some(),
                };
            }
            let Some(next) = task_dependency(tasks, &wait.producer()).cloned() else {
                return ReportedDependency {
                    task: wait.producer(),
                    session: wait.owner_id(),
                    wait: wait.get(),
                    live_cross_session: false,
                };
            };
            wait = next;
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

        let mut attempted_blocked = HashSet::new();
        loop {
            if !matches!(context.poll_wait(target), EvaluationWaitPoll::Pending(_)) {
                return EvaluationPumpOutcome::TargetReady;
            }
            if step_budget == 0 {
                return EvaluationPumpOutcome::BudgetExhausted;
            }

            let claimed = {
                let mut tasks = self
                    .tasks
                    .lock()
                    .expect("evaluation task registry was poisoned");
                if target_has_running_producer(&tasks, target) {
                    return EvaluationPumpOutcome::Busy;
                }
                let prioritized = prioritized_task(&tasks, target, &attempted_blocked);
                prioritized
                    .and_then(|id| claim_task(&mut tasks, id))
                    .or_else(|| claim_ready_task(&mut tasks))
                    .or_else(|| claim_blocked_task(&mut tasks, &attempted_blocked))
            };
            let Some(mut claimed) = claimed else {
                let tasks = self
                    .tasks
                    .lock()
                    .expect("evaluation task registry was poisoned");
                if target_has_running_producer(&tasks, target) {
                    return EvaluationPumpOutcome::Busy;
                }
                drop(tasks);
                if !matches!(context.poll_wait(target), EvaluationWaitPoll::Pending(_)) {
                    return EvaluationPumpOutcome::TargetReady;
                }
                return EvaluationPumpOutcome::NoProgress;
            };

            let quantum = step_budget.min(TASK_POLL_QUANTUM);
            step_budget -= quantum;
            let poll = claimed.poll(quantum);
            let claimed_id = claimed.id();
            let (made_progress, remains_blocked, released, status) =
                self.release_task(claimed, poll);
            publish_task_status(status);
            self.notify_executor_if_ready();
            if let Some(machine) = released {
                machine.finish();
            }
            if remains_blocked {
                attempted_blocked.insert(claimed_id);
            }
            if made_progress {
                // A completed producer or host commit may have made an earlier
                // blocked task runnable. Reconsider it within this same pump.
                attempted_blocked.clear();
            }
        }
    }

    fn release_task(
        &self,
        claimed: ClaimedTask,
        poll: EvaluationMachinePoll,
    ) -> (
        bool,
        bool,
        Option<ReleasedTaskMachine>,
        Option<TaskStatusUpdate>,
    ) {
        let result = match claimed {
            ClaimedTask::Reflection(claimed) => self.release_reflection_task(claimed, poll),
            ClaimedTask::Deferred(claimed) => self.release_deferred_task(claimed, poll),
        };
        if result.0 {
            self.notify_spark_disturbance();
        }
        result
    }

    fn release_reflection_task(
        &self,
        claimed: ClaimedReflectionTask,
        poll: EvaluationMachinePoll,
    ) -> (
        bool,
        bool,
        Option<ReleasedTaskMachine>,
        Option<TaskStatusUpdate>,
    ) {
        let mut tasks = self
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let record = tasks
            .reflection
            .get_mut(&claimed.wait)
            .expect("claimed task must remain registered");
        assert!(
            matches!(record.state, EvaluationTaskState::Running),
            "only a running task may release its machine"
        );
        assert!(record.machine.is_none(), "claimed machine must be absent");
        if record.cancel_requested {
            record.cancel_requested = false;
            let transition = transition_reflection_task(
                &mut tasks,
                &claimed.wait,
                EvaluationTaskState::Cancelled,
                &claimed.prior_state,
            );
            self.task_changed.notify_all();
            drop(tasks);
            drop(transition.retired);
            return (
                true,
                false,
                Some(ReleasedTaskMachine::Cancel(claimed.machine)),
                transition.status,
            );
        }
        record.machine = Some(claimed.machine);

        let (state, made_progress, remains_blocked) = match poll {
            EvaluationMachinePoll::Yielded => (EvaluationTaskState::Queued, true, false),
            EvaluationMachinePoll::Blocked(block) => {
                let unchanged = matches!(
                    &claimed.prior_state,
                    EvaluationTaskState::Blocked(prior) if prior == &block
                );
                (EvaluationTaskState::Blocked(block), !unchanged, true)
            }
            EvaluationMachinePoll::Complete(value) => (
                EvaluationTaskState::Complete(RuntimeValueRoot::new(&self.values, value)),
                true,
                false,
            ),
            EvaluationMachinePoll::Failed(error) => {
                (EvaluationTaskState::Failed(error), true, false)
            }
            EvaluationMachinePoll::Cancelled => (EvaluationTaskState::Cancelled, true, false),
        };
        let queued = matches!(state, EvaluationTaskState::Queued);
        let transition =
            transition_reflection_task(&mut tasks, &claimed.wait, state, &claimed.prior_state);
        if queued {
            tasks.ready.push_back(claimed.id);
        }
        let mut retired = transition.retired;
        let released = retired
            .as_mut()
            .and_then(|record| record.machine.take())
            .map(ReleasedTaskMachine::Drop);
        self.task_changed.notify_all();
        drop(tasks);
        drop(retired);
        (made_progress, remains_blocked, released, transition.status)
    }

    fn release_deferred_task(
        &self,
        claimed: ClaimedDeferredTask,
        poll: EvaluationMachinePoll,
    ) -> (
        bool,
        bool,
        Option<ReleasedTaskMachine>,
        Option<TaskStatusUpdate>,
    ) {
        let (state, mut made_progress) = match poll {
            EvaluationMachinePoll::Yielded => (DeferredTaskState::Dormant, true),
            EvaluationMachinePoll::Blocked(block) => {
                let unchanged = matches!(
                    &claimed.prior_state,
                    DeferredTaskState::Blocked(prior) if prior == &block
                );
                (DeferredTaskState::Blocked(block), !unchanged)
            }
            EvaluationMachinePoll::Complete(value) => (
                DeferredTaskState::Complete(RuntimeValueRoot::new(&self.values, value)),
                true,
            ),
            EvaluationMachinePoll::Failed(error) => (DeferredTaskState::Failed(error), true),
            EvaluationMachinePoll::Cancelled => (
                DeferredTaskState::Failed(Arc::new(EvaluationFailure::message(
                    "deferred evaluation task was cancelled",
                ))),
                true,
            ),
        };
        let retains_machine = matches!(
            &state,
            DeferredTaskState::Dormant | DeferredTaskState::Blocked(_)
        );
        let mut machine = Some(claimed.machine);
        let mut retired_records = Vec::new();
        let mut tasks = self
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        {
            let record = tasks
                .deferred
                .get_mut(&claimed.deferred)
                .expect("claimed deferred task must remain registered");
            assert_eq!(record.id, claimed.id, "deferred task ID index must agree");
            assert!(
                matches!(record.state, DeferredTaskState::Running),
                "only a running deferred task may release its machine"
            );
            assert!(record.machine.is_none(), "claimed machine must be absent");
            if retains_machine {
                record.machine = machine.take();
            }
        }
        debug_assert_eq!(
            machine.is_none(),
            retains_machine,
            "only an active deferred task may retain its machine"
        );
        let dependency = match &state {
            DeferredTaskState::Blocked(block) => block
                .lazy
                .as_ref()
                .and_then(|wait| deferred_for_wait(&tasks, wait)),
            _ => None,
        };
        let record = tasks
            .deferred
            .get_mut(&claimed.deferred)
            .expect("claimed deferred task must remain registered");
        made_progress |= claimed.prior_dependency != dependency;
        record.state = publish_deferred_state(&record.wait, state);
        record.dependency = dependency;

        if dependency.is_some()
            && let Some(cycle) = deferred_dependency_cycle(&tasks, claimed.deferred)
            && let Some(cycle) = pure_lazy_cycle(&tasks, &cycle)
        {
            retired_records.extend(poison_lazy_cycle(&mut tasks, &cycle));
            made_progress = true;
        } else if !retains_machine {
            retired_records.push(retire_deferred_task(&mut tasks, claimed.deferred));
        }
        let remains_blocked = tasks
            .deferred
            .get(&claimed.deferred)
            .is_some_and(|record| matches!(record.state, DeferredTaskState::Blocked(_)));
        self.task_changed.notify_all();
        drop(tasks);
        drop(machine);
        drop(retired_records);
        (made_progress, remains_blocked, None, None)
    }

    fn notify_executor_if_ready(&self) {
        let tasks = self
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let ready = tasks.ready.iter().any(|id| {
            tasks
                .reflection_by_id
                .get(id)
                .and_then(|wait| tasks.reflection.get(wait))
                .is_some_and(|record| matches!(record.state, EvaluationTaskState::Queued))
        });
        drop(tasks);
        if ready {
            self.notify_executor_ready();
        }
    }

    fn poll_one_ready_task(&self) {
        let claimed = {
            let mut tasks = self
                .tasks
                .lock()
                .expect("evaluation task registry was poisoned");
            claim_ready_task(&mut tasks)
        };
        let Some(mut claimed) = claimed else {
            return;
        };
        // Re-advertise remaining ready work before polling so other workers
        // may claim independent tasks from this same session concurrently.
        self.notify_executor_if_ready();
        let poll = claimed.poll(TASK_POLL_QUANTUM);
        let (_, _, released, status) = self.release_task(claimed, poll);
        publish_task_status(status);
        if let Some(machine) = released {
            machine.finish();
        }
        self.notify_executor_if_ready();
    }
}

fn producer_for_wait(
    tasks: &EvaluationTasks,
    wait: &EvaluationWaitToken,
) -> Option<EvaluationTaskId> {
    tasks
        .reflection
        .get(wait)
        .map(|record| record.id)
        .or_else(|| {
            tasks
                .deferred_by_wait
                .get(wait)
                .and_then(|deferred| tasks.deferred.get(deferred))
                .map(|record| record.id)
        })
        .or_else(|| tasks.promises.get(wait).map(|promise| promise.producer))
}

fn task_dependency<'a>(
    tasks: &'a EvaluationTasks,
    id: &EvaluationTaskId,
) -> Option<&'a EvaluationWaitToken> {
    if let Some(wait) = tasks.reflection_by_id.get(id) {
        let record = tasks.reflection.get(wait)?;
        return match &record.state {
            EvaluationTaskState::Blocked(block) => block.lazy.as_ref(),
            _ => None,
        };
    }
    let deferred = tasks.deferred_by_task.get(id)?;
    let record = tasks.deferred.get(deferred)?;
    match &record.state {
        DeferredTaskState::Blocked(block) => block.lazy.as_ref(),
        _ => None,
    }
}

fn task_is_claimable(
    tasks: &EvaluationTasks,
    id: &EvaluationTaskId,
    attempted: &HashSet<EvaluationTaskId>,
) -> bool {
    if attempted.contains(id) {
        return false;
    }
    if let Some(wait) = tasks.reflection_by_id.get(id) {
        return tasks.reflection.get(wait).is_some_and(|record| {
            matches!(
                record.state,
                EvaluationTaskState::Queued | EvaluationTaskState::Blocked(_)
            )
        });
    }
    tasks
        .deferred_by_task
        .get(id)
        .and_then(|deferred| tasks.deferred.get(deferred))
        .is_some_and(|record| match &record.state {
            DeferredTaskState::Dormant => true,
            DeferredTaskState::Blocked(block) => {
                block
                    .lazy
                    .as_ref()
                    .is_some_and(|wait| wait_is_terminal(tasks, wait))
                    || matches!(&record.value, DeferredValue::Promise(promise) if promise.assignment().is_some())
            }
            DeferredTaskState::Running
            | DeferredTaskState::Complete(_)
            | DeferredTaskState::Failed(_)
            | DeferredTaskState::Abandoned => false,
        })
}

fn wait_is_terminal(tasks: &EvaluationTasks, wait: &EvaluationWaitToken) -> bool {
    if wait.terminal_poll().is_some() {
        return true;
    }
    if let Some(promise) = tasks.promises.get(wait) {
        return promise_record_terminal(wait, promise).is_some();
    }
    if tasks.reflection.contains_key(wait) || tasks.deferred_by_wait.contains_key(wait) {
        return false;
    }

    // A dependency owned by another session cannot be inspected while this
    // session's task registry is locked. Claim its local follower once per
    // scheduler pass; the machine polls the cross-session task after releasing this
    // lock and either advances or records the same stable blockage.
    true
}

fn deferred_for_wait(
    tasks: &EvaluationTasks,
    wait: &EvaluationWaitToken,
) -> Option<DeferredValueId> {
    tasks.deferred_by_wait.get(wait).copied()
}

/// Returns the canonical cycle reachable from `start` in the strict deferred
/// dependency graph. The graph is functional, so a successor walk is enough.
fn deferred_dependency_cycle(
    tasks: &EvaluationTasks,
    start: DeferredValueId,
) -> Option<Vec<DeferredValueId>> {
    let mut path = Vec::new();
    let mut positions = HashMap::new();
    let mut current = start;
    loop {
        if let Some(first) = positions.insert(current, path.len()) {
            let mut cycle = path.split_off(first);
            let canonical = cycle
                .iter()
                .enumerate()
                .min_by_key(|(_, lazy)| **lazy)
                .map(|(position, _)| position)
                .expect("a repeated successor must produce a non-empty cycle");
            cycle.rotate_left(canonical);
            return Some(cycle);
        }
        path.push(current);
        current = tasks.deferred.get(&current)?.dependency?;
    }
}

fn pure_lazy_cycle(
    tasks: &EvaluationTasks,
    members: &[DeferredValueId],
) -> Option<Vec<crate::core::LazyId>> {
    members
        .iter()
        .map(|id| {
            let record = tasks
                .deferred
                .get(id)
                .expect("cycle members must remain registered");
            match &record.value {
                DeferredValue::Lazy(lazy) => Some(lazy.id()),
                DeferredValue::Promise(_) => None,
            }
        })
        .collect()
}

/// Installs one shared structured failure in every member of a proven strict
/// lazy cycle. Promise dependencies are retryable scheduler state and must not
/// permanently poison a computed lazy result.
fn poison_lazy_cycle(
    tasks: &mut EvaluationTasks,
    members: &[crate::core::LazyId],
) -> Vec<DeferredTaskRecord> {
    let cycle = Arc::new(LazyCycle {
        members: members
            .iter()
            .map(|id| {
                let record = tasks
                    .deferred
                    .get(&DeferredValueId::Lazy(*id))
                    .expect("cycle members must remain registered");
                LazyCycleMember {
                    id: *id,
                    label: record.value.label().clone(),
                }
            })
            .collect(),
    });
    let failure = Arc::new(EvaluationFailure::dependency_cycle(cycle));

    for id in members {
        let record = tasks
            .deferred
            .get_mut(&DeferredValueId::Lazy(*id))
            .expect("cycle members must remain registered");
        record.dependency = None;
        let DeferredValue::Lazy(lazy) = &record.value else {
            unreachable!("a pure lazy cycle cannot contain a promise")
        };
        let state = match lazy.cache(Err(failure.clone())) {
            Err(error) => DeferredTaskState::Failed(error),
            Ok(value) => {
                debug_assert!(
                    false,
                    "a successful concurrent lazy result contradicts a strict dependency cycle"
                );
                DeferredTaskState::Complete(RuntimeValueRoot::from_runtime(
                    record.wait.runtime_id(),
                    value.into_value(),
                ))
            }
        };
        record.state = publish_deferred_state(&record.wait, state);
    }
    members
        .iter()
        .map(|id| retire_deferred_task(tasks, DeferredValueId::Lazy(*id)))
        .collect()
}

fn prioritized_task(
    tasks: &EvaluationTasks,
    target: &EvaluationWaitToken,
    attempted_blocked: &HashSet<EvaluationTaskId>,
) -> Option<EvaluationTaskId> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut wait = target.clone();
    while let Some(id) = producer_for_wait(tasks, &wait) {
        if !seen.insert(id) {
            break;
        }
        chain.push(id);
        let Some(dependency) = task_dependency(tasks, &id) else {
            break;
        };
        wait = dependency.clone();
    }

    chain
        .into_iter()
        .rev()
        .find(|id| task_is_claimable(tasks, id, attempted_blocked))
}

fn target_has_running_producer(tasks: &EvaluationTasks, target: &EvaluationWaitToken) -> bool {
    let mut seen = HashSet::new();
    let mut wait = target.clone();
    while let Some(id) = producer_for_wait(tasks, &wait) {
        if !seen.insert(id) {
            return false;
        }
        if let Some(task_wait) = tasks.reflection_by_id.get(&id) {
            let Some(record) = tasks.reflection.get(task_wait) else {
                return false;
            };
            match &record.state {
                EvaluationTaskState::Running => return true,
                EvaluationTaskState::Blocked(block) => {
                    let Some(dependency) = &block.lazy else {
                        return false;
                    };
                    wait = dependency.clone();
                }
                _ => return false,
            }
            continue;
        }
        let Some(deferred) = tasks.deferred_by_task.get(&id) else {
            return false;
        };
        let Some(record) = tasks.deferred.get(deferred) else {
            return false;
        };
        match &record.state {
            DeferredTaskState::Running => return true,
            DeferredTaskState::Blocked(block) => {
                let Some(dependency) = &block.lazy else {
                    return false;
                };
                wait = dependency.clone();
            }
            _ => return false,
        }
    }
    false
}

fn claim_task(tasks: &mut EvaluationTasks, id: EvaluationTaskId) -> Option<ClaimedTask> {
    if let Some(wait) = tasks.reflection_by_id.get(&id).cloned() {
        let record = tasks.reflection.get_mut(&wait)?;
        if !matches!(
            record.state,
            EvaluationTaskState::Queued | EvaluationTaskState::Blocked(_)
        ) {
            return None;
        }
        let machine = record.machine.take()?;
        let prior_state = std::mem::replace(&mut record.state, EvaluationTaskState::Running);
        return Some(ClaimedTask::Reflection(ClaimedReflectionTask {
            id,
            wait,
            prior_state,
            machine,
        }));
    }
    let deferred = *tasks.deferred_by_task.get(&id)?;
    let record = tasks.deferred.get_mut(&deferred)?;
    if !matches!(
        record.state,
        DeferredTaskState::Dormant | DeferredTaskState::Blocked(_)
    ) {
        return None;
    }
    let machine = record.machine.take()?;
    // Once a blocked task resumes, its old dependency is no longer a strict
    // prerequisite. Its next poll either completes or records a fresh edge.
    let prior_dependency = record.dependency.take();
    let prior_state = std::mem::replace(&mut record.state, DeferredTaskState::Running);
    Some(ClaimedTask::Deferred(ClaimedDeferredTask {
        id,
        deferred,
        prior_state,
        prior_dependency,
        machine,
    }))
}

fn claim_ready_task(tasks: &mut EvaluationTasks) -> Option<ClaimedTask> {
    while let Some(id) = tasks.ready.pop_front() {
        let is_queued = tasks
            .reflection_by_id
            .get(&id)
            .and_then(|wait| tasks.reflection.get(wait))
            .is_some_and(|record| matches!(record.state, EvaluationTaskState::Queued));
        if is_queued && let Some(ClaimedTask::Reflection(claimed)) = claim_task(tasks, id) {
            return Some(ClaimedTask::Reflection(claimed));
        }
    }
    None
}

fn claim_blocked_task(
    tasks: &mut EvaluationTasks,
    attempted: &HashSet<EvaluationTaskId>,
) -> Option<ClaimedTask> {
    let id = tasks.reflection_by_id.iter().find_map(|(id, wait)| {
        let record = tasks.reflection.get(wait)?;
        (matches!(record.state, EvaluationTaskState::Blocked(_)) && !attempted.contains(id))
            .then_some(*id)
    })?;
    match claim_task(tasks, id) {
        Some(ClaimedTask::Reflection(claimed)) => Some(ClaimedTask::Reflection(claimed)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        fn context(&self) -> EvalContext {
            let session = self
                .runtime
                .new_evaluation_session()
                .expect("same-runtime test session should build");
            debug_assert_eq!(session.values.runtime_id(), self.runtime.id());
            EvalContext::new(session)
        }
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
                        lazy: Some(wait),
                        observed_generation: None,
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
                        lazy: Some(wait),
                        observed_generation: None,
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
        LazyValue::deferred(&crate::core::test_value_factory(), label, |_| {
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

    fn assert_deferred_task_retired(context: &EvalContext, lazy: &LazyValue) {
        let tasks = context
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        assert!(
            !tasks
                .deferred
                .contains_key(&DeferredValueId::Lazy(lazy.id())),
            "terminal deferred records must be removed"
        );
        assert!(
            tasks
                .deferred_by_wait
                .values()
                .all(|id| *id != DeferredValueId::Lazy(lazy.id())),
            "terminal deferred wait indexes must be removed"
        );
        assert!(
            tasks
                .deferred_by_task
                .values()
                .all(|id| *id != DeferredValueId::Lazy(lazy.id())),
            "terminal deferred task ID indexes must be removed"
        );
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
                lazy: None,
                observed_generation: Some(7),
                error: Some(Arc::new(EvaluationFailure::message(
                    "retryable evaluation error",
                ))),
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
            self.dropped_without_registry_lock.store(
                self.context.session.tasks.try_lock().is_ok(),
                Ordering::Release,
            );
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
        assert_eq!(
            context.cancel_reflection_task(&cancelled),
            EvaluationTaskCancellation::Requested
        );

        let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
            panic!("terminal tasks should leave no unfinished work");
        };
        assert_eq!(report.failures.size(), 1);
        assert!(report.failures.contains_key(&failed.id()));

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
    fn terminal_reflection_machines_drop_after_releasing_the_registry_lock() {
        let context = EvalContext::standalone();
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
            "terminal reflection machine must be destroyed without the registry lock"
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
            owner
                .session
                .tasks
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
            let owner = fixture
                .context()
                .with_new_task()
                .expect("promise owner should allocate a task identity");
            PromisedValue::fixpoint(&owner, "abandoned task promise")
                .expect("task promise should register")
        };
        let observer = fixture.context();
        let error = task_promise
            .assignment()
            .expect("session closure must assign the task promise")
            .expect_err("an abandoned task promise must fail");
        assert!(error.to_string().contains("was abandoned"));
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

            let owner = context
                .with_new_task()
                .expect("promise owner should receive a task ID");
            let promise = PromisedValue::fixpoint(&owner, format!("promise {index}"))
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
        let context = EvalContext::new(session);
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
    fn pump_stops_after_rechecking_an_unchanged_block() {
        let context = EvalContext::standalone();
        let target = context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .expect("blocked task should schedule");

        assert_eq!(
            context.pump_wait(&target.wait, 256),
            EvaluationPumpOutcome::NoProgress
        );
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
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let task = context
            .schedule_task({
                let cancelled = cancelled.clone();
                move |_| {
                    Ok(Box::new(CancellableAfterRelease {
                        started: Some(started_sender),
                        release: release_receiver,
                        cancelled,
                    }))
                }
            })
            .expect("running cancellation fixture should schedule");
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
            let tasks = context
                .session
                .tasks
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
            let tasks = context
                .session
                .tasks
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
        assert_eq!(report.unfinished[0].observed_generation, Some(7));
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
        let context = EvalContext::new(session);
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

    #[test]
    fn closing_a_session_abandons_a_blocked_spark_and_releases_its_lazy_claim() {
        let (coordinator, _executor) = test_execution_resources(1).unwrap();
        let session = EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(session.clone());
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
        let observer = EvalContext::new(EvaluationSession::shared(&coordinator));
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
        let context = EvalContext::new(session.clone());
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
        let context = EvalContext::new(session);
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
        let context = EvalContext::new(session);
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
