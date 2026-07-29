//! Session-scoped capabilities threaded through semantic evaluation.
//!
//! The session owns evaluation-task identity, wait-token provenance, and the
//! serial cooperative executor. Reflection specializations remain outside this
//! module behind a small type-erased task-machine boundary.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use rpds::RedBlackTreeMapSync;

use crate::core::{
    DeferredValueId, EvaluationFailure, LazyCycle, LazyCycleMember, LazyValue, PromiseAssignment,
    PromisedValue, Value,
};

mod executor;
pub(crate) use executor::EvaluationExecutor;

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

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_WAIT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

fn allocate_task_id() -> Result<EvaluationTaskId, Arc<str>> {
    allocate_id(&NEXT_TASK_ID, "evaluation task IDs exhausted").map(EvaluationTaskId)
}

fn allocate_wait_token(
    session: &Arc<EvaluationSession>,
    producer: EvaluationTaskId,
) -> Result<EvaluationWaitToken, Arc<str>> {
    Ok(EvaluationWaitToken(Arc::new(EvaluationWaitState {
        id: allocate_id(&NEXT_WAIT_ID, "evaluation wait-token IDs exhausted")?,
        owner_id: session.id,
        owner: Arc::downgrade(session),
        producer,
        terminal: OnceLock::new(),
    })))
}

fn allocate_session_id() -> EvaluationSessionId {
    EvaluationSessionId(
        allocate_id(&NEXT_SESSION_ID, "evaluation session IDs exhausted")
            .expect("evaluation session IDs are process-global"),
    )
}

fn allocate_id(source: &AtomicU64, exhausted: &'static str) -> Result<NonZeroU64, Arc<str>> {
    source
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map(|id| NonZeroU64::new(id).expect("evaluation IDs start at one"))
        .map_err(|_| Arc::from(exhausted))
}

fn evaluation_failure(message: impl AsRef<str>) -> Arc<EvaluationFailure> {
    Arc::new(EvaluationFailure::message(message))
}

struct EvaluationWaitState {
    id: NonZeroU64,
    owner_id: EvaluationSessionId,
    owner: Weak<EvaluationSession>,
    producer: EvaluationTaskId,
    terminal: OnceLock<EvaluationTaskTerminal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvaluationTaskTerminal {
    Complete(Value),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
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

    fn terminal_poll(&self) -> Option<EvaluationTaskPoll> {
        self.0.terminal.get().map(EvaluationTaskTerminal::to_poll)
    }

    fn publish_terminal(&self, terminal: EvaluationTaskTerminal) -> EvaluationTaskTerminal {
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
        let terminal = promise_assignment_terminal(assignment);
        if let Some(owner) = self.owner() {
            owner.complete_promise_wait(self, terminal);
        } else {
            self.publish_terminal(terminal);
        }
    }
}

impl EvaluationTaskTerminal {
    fn to_poll(&self) -> EvaluationTaskPoll {
        match self {
            Self::Complete(value) => EvaluationTaskPoll::Complete(value.clone()),
            Self::Failed(error) => EvaluationTaskPoll::Failed(error.clone()),
            Self::Cancelled => EvaluationTaskPoll::Cancelled,
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
pub(crate) enum EvaluationTaskPoll {
    Pending(EvaluationWaitToken),
    Complete(Value),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationTaskCancellation {
    Requested,
    Late,
    ForeignSession,
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
        kind: ReflectionTaskKind,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationTaskStatus {
    Launched,
    Blocked,
    Complete(Value),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
}

pub(crate) trait EvaluationTaskStatusSink: Send + Sync {
    fn update(&self, status: EvaluationTaskStatus);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReflectionTaskKind {
    Annotation,
    Joinable,
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
    Complete(Value),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
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
    Complete(Value),
    Failed(Arc<EvaluationFailure>),
}

fn publish_reflection_state(
    wait: &EvaluationWaitToken,
    state: EvaluationTaskState,
) -> EvaluationTaskState {
    let terminal = match state {
        EvaluationTaskState::Complete(value) => EvaluationTaskTerminal::Complete(value),
        EvaluationTaskState::Failed(error) => EvaluationTaskTerminal::Failed(error),
        EvaluationTaskState::Cancelled => EvaluationTaskTerminal::Cancelled,
        state => return state,
    };
    match wait.publish_terminal(terminal) {
        EvaluationTaskTerminal::Complete(value) => EvaluationTaskState::Complete(value),
        EvaluationTaskTerminal::Failed(error) => EvaluationTaskState::Failed(error),
        EvaluationTaskTerminal::Cancelled => EvaluationTaskState::Cancelled,
    }
}

fn publish_deferred_state(
    wait: &EvaluationWaitToken,
    state: DeferredTaskState,
) -> DeferredTaskState {
    let terminal = match state {
        DeferredTaskState::Complete(value) => EvaluationTaskTerminal::Complete(value),
        DeferredTaskState::Failed(error) => EvaluationTaskTerminal::Failed(error),
        state => return state,
    };
    match wait.publish_terminal(terminal) {
        EvaluationTaskTerminal::Complete(value) => DeferredTaskState::Complete(value),
        EvaluationTaskTerminal::Failed(error) => DeferredTaskState::Failed(error),
        EvaluationTaskTerminal::Cancelled => {
            unreachable!("a deferred wait cannot publish cancellation")
        }
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
    effect: Value,
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
                    self.inner.effect.clone(),
                    ReflectionTaskKind::Joinable,
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

fn promise_assignment_terminal(assignment: &PromiseAssignment) -> EvaluationTaskTerminal {
    match assignment {
        Ok(value) => EvaluationTaskTerminal::Complete(value.clone()),
        Err(error) => EvaluationTaskTerminal::Failed(error.clone()),
    }
}

fn promise_record_terminal(record: &PromiseRecord) -> Option<EvaluationTaskTerminal> {
    let Some(result) = record.result.upgrade() else {
        return Some(EvaluationTaskTerminal::Failed(evaluation_failure(
            "promised value no longer exists",
        )));
    };
    result.get().map(promise_assignment_terminal)
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
            promise_record_terminal(record).map(|terminal| (wait.clone(), terminal))
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
    tasks: Mutex<EvaluationTasks>,
    task_changed: Condvar,
    reflection_launcher: OnceLock<Arc<dyn ReflectionTaskLauncher>>,
    executor: Weak<EvaluationExecutor>,
}

impl Default for EvaluationSession {
    fn default() -> Self {
        Self::with_executor(Weak::new())
    }
}

impl fmt::Debug for EvaluationSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationSession")
            .finish_non_exhaustive()
    }
}

impl EvaluationSession {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn with_executor(executor: Weak<EvaluationExecutor>) -> Self {
        Self {
            id: allocate_session_id(),
            tasks: Mutex::new(EvaluationTasks::default()),
            task_changed: Condvar::new(),
            reflection_launcher: OnceLock::new(),
            executor,
        }
    }

    pub(crate) fn shared(executor: &Arc<EvaluationExecutor>) -> Arc<Self> {
        let session = Arc::new(Self::with_executor(Arc::downgrade(executor)));
        executor.register_session(&session);
        session
    }

    pub(crate) fn install_reflection_launcher(
        &self,
        launcher: Arc<dyn ReflectionTaskLauncher>,
    ) -> Result<(), Arc<str>> {
        self.reflection_launcher
            .set(launcher)
            .map_err(|_| Arc::from("evaluation session already has a reflection task launcher"))
    }

    fn notify_executor_ready(&self) {
        if let Some(executor) = self.executor.upgrade() {
            executor.notify_session_ready(self.id.get());
        }
    }

    fn complete_promise_wait(&self, wait: &EvaluationWaitToken, terminal: EvaluationTaskTerminal) {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        wait.publish_terminal(terminal);
        retire_promise_wait(&mut tasks, wait);
        self.task_changed.notify_all();
    }

    pub(crate) fn submit_spark(self: &Arc<Self>, value: Value) {
        if let Some(executor) = self.executor.upgrade() {
            executor.submit_spark(self, value);
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
    task: Arc<OnceLock<Result<EvaluationTaskId, Arc<str>>>>,
    scheduled_task: bool,
    waits_for_claimed_tasks: bool,
    originating_task: Option<EvaluationTaskId>,
}

impl EvalContext {
    pub(crate) fn new(session: Arc<EvaluationSession>) -> Self {
        Self {
            session,
            task: Arc::new(OnceLock::new()),
            scheduled_task: false,
            waits_for_claimed_tasks: false,
            originating_task: None,
        }
    }

    pub(crate) fn patient(session: Arc<EvaluationSession>) -> Self {
        Self {
            waits_for_claimed_tasks: true,
            ..Self::new(session)
        }
    }

    fn for_task(session: Arc<EvaluationSession>, id: EvaluationTaskId) -> Self {
        let task = Arc::new(OnceLock::new());
        task.set(Ok(id))
            .expect("fresh task identity cell must be empty");
        Self {
            session,
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
    ) -> Self {
        let task = Arc::new(OnceLock::new());
        task.set(Ok(id))
            .expect("fresh deferred task identity cell must be empty");
        Self {
            session,
            task,
            scheduled_task: true,
            waits_for_claimed_tasks: false,
            originating_task,
        }
    }

    /// Creates a session for internal clients that do not yet run under an
    /// assembler, notably standalone reflection tasks and focused tests.
    pub(crate) fn standalone() -> Self {
        Self::new(Arc::new(EvaluationSession::new()))
    }

    pub(crate) fn spark(&self, value: Value) {
        if matches!(value, Value::Lazy(_) | Value::Promised(_) | Value::Net(_)) {
            self.session.submit_spark(value);
        }
    }

    /// Signals completion of a host-owned promise to pumps of this context's
    /// session. Promise values may be shared across sessions, but completion
    /// deliberately does not discover or wake those foreign observers.
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

        let id = allocate_task_id()?;
        let wait = allocate_wait_token(&self.session, id)?;
        let originating_task = self
            .originating_task
            .or_else(|| self.task.get().and_then(|task| task.as_ref().ok()).copied());
        let machine = build(Self::for_deferred_task(
            self.session.clone(),
            id,
            originating_task,
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
        self.session.install_reflection_launcher(launcher)
    }

    #[cfg(test)]
    pub(crate) fn with_new_task(&self) -> Result<Self, Arc<str>> {
        let context = Self {
            session: self.session.clone(),
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
        self.task.get_or_init(allocate_task_id).clone()
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
                    result
                        .get()
                        .expect("promise assignment must be set after producer failure"),
                )
            } else {
                EvaluationTaskTerminal::Failed(evaluation_failure(
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
        let id = allocate_task_id()?;
        let wait = allocate_wait_token(&self.session, id)?;
        let context = Self::for_task(self.session.clone(), id);
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
        let id = allocate_task_id()?;
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
        kind: ReflectionTaskKind,
        status_sink: Option<Arc<dyn EvaluationTaskStatusSink>>,
        error_acknowledged: bool,
    ) {
        let result = self
            .session
            .reflection_launcher
            .get()
            .ok_or_else(|| {
                Arc::new(EvaluationFailure::message(
                    "evaluation session has no reflection task launcher",
                ))
            })
            .and_then(|launcher| {
                launcher.build(
                    Self::for_task(self.session.clone(), handle.id),
                    effect,
                    kind,
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
        if self.session.reflection_launcher.get().is_none() {
            return Err(Arc::from(
                "evaluation session has no reflection task launcher",
            ));
        }
        Ok(PendingReflectionTask {
            inner: Arc::new(PendingReflectionTaskInner {
                context: self.clone(),
                handle: self.reserve_task()?,
                effect,
                activated: AtomicBool::new(false),
            }),
        })
    }

    pub(crate) fn start_reflection_task(
        &self,
        effect: Value,
    ) -> Result<EvaluationTaskHandle, Arc<str>> {
        if self.session.reflection_launcher.get().is_some() {
            let handle = self.reserve_task()?;
            self.activate_reflection_task(
                &handle,
                effect,
                ReflectionTaskKind::Annotation,
                None,
                false,
            );
            return Ok(handle);
        }

        // Focused evaluator tests and internal clients may intentionally use a
        // bare session. Preserve an inspectable wait record for them; ordinary
        // Assembler sessions always install a launcher.
        let id = allocate_task_id()?;
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

    pub(crate) fn poll_reflection_task(&self, task: &EvaluationTaskHandle) -> EvaluationTaskPoll {
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
            return EvaluationTaskCancellation::ForeignSession;
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
                | EvaluationTaskState::Cancelled => {
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
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        if let Some(record) = tasks.reflection.get_mut(&task.wait) {
            record.error_acknowledged = true;
        } else {
            tasks.unacknowledged_failures.remove_mut(&task.id);
        }
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

    pub(crate) fn poll_wait(&self, wait: &EvaluationWaitToken) -> EvaluationTaskPoll {
        if let Some(terminal) = wait.terminal_poll() {
            return terminal;
        }
        let owner = match wait.owner() {
            Some(owner) => owner,
            None => {
                return wait.terminal_poll().unwrap_or_else(|| {
                    EvaluationTaskPoll::Failed(evaluation_failure(
                        "reflection task's evaluation session no longer exists",
                    ))
                });
            }
        };
        let mut tasks = owner
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        if let Some(terminal) = wait.terminal_poll() {
            return terminal;
        }
        if let Some(terminal) = tasks.promises.get(wait).and_then(promise_record_terminal) {
            let terminal = wait.publish_terminal(terminal);
            retire_promise_wait(&mut tasks, wait);
            owner.task_changed.notify_all();
            return terminal.to_poll();
        }
        if tasks.reflection.contains_key(wait)
            || tasks.deferred_by_wait.contains_key(wait)
            || tasks.promises.contains_key(wait)
        {
            return EvaluationTaskPoll::Pending(wait.clone());
        }
        EvaluationTaskPoll::Failed(evaluation_failure(
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
            EvaluationTaskState::Complete((*crate::core::keys::UNIT_VALUE).clone()),
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
            Some(EvaluationTaskPoll::Failed(failure)) => Some(failure),
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
    live_foreign: bool,
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
        let mut has_live_foreign_dependency = false;
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
                | EvaluationTaskState::Cancelled => {
                    unreachable!("terminal reflection records must be retired")
                }
            };
            let dependency = block
                .and_then(|block| block.lazy.as_ref())
                .map(|wait| self.reported_dependency(tasks, wait));
            has_live_foreign_dependency |= dependency
                .as_ref()
                .is_some_and(|dependency| dependency.live_foreign);
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
        } else if has_live_foreign_dependency {
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
                    live_foreign: wait.owner_id() != self.id && wait.owner().is_some(),
                };
            }
            let Some(next) = task_dependency(tasks, &wait.producer()).cloned() else {
                return ReportedDependency {
                    task: wait.producer(),
                    session: wait.owner_id(),
                    wait: wait.get(),
                    live_foreign: false,
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
            if !matches!(context.poll_wait(target), EvaluationTaskPoll::Pending(_)) {
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
                if !matches!(context.poll_wait(target), EvaluationTaskPoll::Pending(_)) {
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
        match claimed {
            ClaimedTask::Reflection(claimed) => self.release_reflection_task(claimed, poll),
            ClaimedTask::Deferred(claimed) => self.release_deferred_task(claimed, poll),
        }
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
            EvaluationMachinePoll::Complete(value) => {
                (EvaluationTaskState::Complete(value), true, false)
            }
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
            EvaluationMachinePoll::Complete(value) => (DeferredTaskState::Complete(value), true),
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
            | DeferredTaskState::Failed(_) => false,
        })
}

fn wait_is_terminal(tasks: &EvaluationTasks, wait: &EvaluationWaitToken) -> bool {
    if wait.terminal_poll().is_some() {
        return true;
    }
    if let Some(promise) = tasks.promises.get(wait) {
        return promise_record_terminal(promise).is_some();
    }
    if tasks.reflection.contains_key(wait) || tasks.deferred_by_wait.contains_key(wait) {
        return false;
    }

    // A dependency owned by another session cannot be inspected while this
    // session's task registry is locked. Claim its local follower once per
    // scheduler pass; the machine polls the foreign task after releasing this
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
                DeferredTaskState::Complete(value.into_value())
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

    struct Complete;

    impl EvaluationTaskMachine for Complete {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Complete((*crate::core::keys::UNIT_VALUE).clone())
        }
    }

    struct Await {
        context: EvalContext,
        dependency: EvaluationWaitToken,
    }

    impl EvaluationTaskMachine for Await {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            match self.context.poll_wait(&self.dependency) {
                EvaluationTaskPoll::Pending(wait) => {
                    EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                        lazy: Some(wait),
                        observed_generation: None,
                        error: None,
                    })
                }
                EvaluationTaskPoll::Complete(value) => EvaluationMachinePoll::Complete(value),
                EvaluationTaskPoll::Failed(error) => self
                    .context
                    .lazy_failure_for_wait(&self.dependency)
                    .map(EvaluationMachinePoll::Failed)
                    .unwrap_or(EvaluationMachinePoll::Failed(error)),
                EvaluationTaskPoll::Cancelled => EvaluationMachinePoll::Cancelled,
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
                EvaluationTaskPoll::Pending(wait) => {
                    EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                        lazy: Some(wait),
                        observed_generation: None,
                        error: None,
                    })
                }
                EvaluationTaskPoll::Complete(value) => EvaluationMachinePoll::Complete(value),
                EvaluationTaskPoll::Failed(error) => self
                    .context
                    .lazy_failure_for_wait(dependency)
                    .map(EvaluationMachinePoll::Failed)
                    .unwrap_or(EvaluationMachinePoll::Failed(error)),
                EvaluationTaskPoll::Cancelled => EvaluationMachinePoll::Cancelled,
            }
        }
    }

    fn inert_lazy(label: &'static str) -> LazyValue {
        LazyValue::deferred(label, |_| {
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
            EvaluationMachinePoll::Complete((*crate::core::keys::UNIT_VALUE).clone())
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
            EvaluationMachinePoll::Complete((*crate::core::keys::UNIT_VALUE).clone())
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

    struct CompleteAndSignalDrop {
        dropped: Arc<AtomicBool>,
    }

    impl EvaluationTaskMachine for CompleteAndSignalDrop {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Complete((*crate::core::keys::UNIT_VALUE).clone())
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
            EvaluationMachinePoll::Complete((*crate::core::keys::UNIT_VALUE).clone())
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
            EvaluationMachinePoll::Complete((*crate::core::keys::UNIT_VALUE).clone())
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
            EvaluationTaskPoll::Complete(_)
        ));
        assert!(matches!(
            context.poll_reflection_task(&target),
            EvaluationTaskPoll::Complete(_)
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

        let EvaluationTaskPoll::Failed(canonical_failure) = context.poll_wait(&canonical_wait)
        else {
            panic!("canonical wait should retain the lazy failure");
        };
        let EvaluationTaskPoll::Failed(redundant_failure) = context.poll_wait(&redundant_wait)
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
                EvaluationTaskPoll::Complete(_)
            ));
            assert!(matches!(
                context.poll_reflection_task(&failed),
                EvaluationTaskPoll::Failed(error)
                    if error.to_string() == "reasoning failed"
            ));
            assert_eq!(
                context.poll_reflection_task(&cancelled),
                EvaluationTaskPoll::Cancelled
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
        let (completed_wait, failed_wait, cancelled_wait) = {
            let owner = EvalContext::standalone();
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
            let owner = EvalContext::standalone();
            let lazy = inert_lazy("owner lifetime");
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
            let owner = EvalContext::standalone();
            owner
                .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
                .expect("pending task should schedule")
                .wait()
                .clone()
        };
        let observer = EvalContext::standalone();

        assert!(matches!(
            observer.poll_wait(&completed_wait),
            EvaluationTaskPoll::Complete(_)
        ));
        assert!(matches!(
            observer.poll_wait(&deferred_wait),
            EvaluationTaskPoll::Complete(_)
        ));
        assert!(matches!(
            observer.poll_wait(&failed_wait),
            EvaluationTaskPoll::Failed(error) if error.to_string() == "reasoning failed"
        ));
        assert_eq!(
            observer.poll_wait(&cancelled_wait),
            EvaluationTaskPoll::Cancelled
        );
        assert_eq!(
            observer.pump_wait(&completed_wait, 1),
            EvaluationPumpOutcome::TargetReady
        );
        assert!(matches!(
            observer.poll_wait(&pending_wait),
            EvaluationTaskPoll::Failed(error)
                if error.to_string()
                    == "reflection task's evaluation session no longer exists"
        ));
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

            let lazy = LazyValue::deferred(format!("successful lazy {index}"), |_| {
                Ok((*crate::core::keys::UNIT_VALUE).clone())
            });
            assert_eq!(
                crate::eval::eval_value(&context, &Value::Lazy(lazy))
                    .expect("successful lazy should evaluate"),
                (*crate::core::keys::UNIT_VALUE).clone()
            );

            let lazy = LazyValue::deferred(format!("failed lazy {index}"), |_| {
                Err(crate::core::EvaluationHalt::new("long-lived lazy failure"))
            });
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
                    .set((*crate::core::keys::UNIT_VALUE).clone())
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
            EvaluationTaskPoll::Complete(_)
        ));
        assert!(matches!(
            context.poll_reflection_task(&failed[0]),
            EvaluationTaskPoll::Failed(_)
        ));
        assert_eq!(
            context.poll_reflection_task(&cancelled[0]),
            EvaluationTaskPoll::Cancelled
        );
        assert!(matches!(
            context.poll_wait(&promises[0]),
            EvaluationTaskPoll::Complete(_)
        ));
        assert!(matches!(
            context.poll_wait(&promises[1]),
            EvaluationTaskPoll::Failed(_)
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
        let executor = EvaluationExecutor::new(1).expect("test executor should start");
        let session = EvaluationSession::shared(&executor);
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
                            EvaluationTaskPoll::Pending(_) if Instant::now() < deadline => {
                                std::thread::yield_now();
                            }
                            EvaluationTaskPoll::Pending(_) => {
                                panic!("waiter timed out before terminal publication")
                            }
                            EvaluationTaskPoll::Complete(_) => break,
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
            EvaluationTaskPoll::Complete(_)
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
            EvaluationTaskPoll::Failed(error)
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
            EvaluationTaskPoll::Failed(_)
        ));
        assert!(matches!(
            context.poll_wait(&right_wait),
            EvaluationTaskPoll::Failed(_)
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
        let EvaluationTaskPoll::Failed(upstream_failure) = context.poll_wait(&upstream_wait) else {
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
            EvaluationTaskPoll::Pending(_)
        ));
        assert!(matches!(
            context.poll_reflection_task(&reflection),
            EvaluationTaskPoll::Pending(_)
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
            EvaluationTaskPoll::Pending(_)
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
        let context = EvalContext::standalone();
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
            EvaluationTaskPoll::Cancelled
        );
        assert_eq!(
            context.cancel_reflection_task(&task),
            EvaluationTaskCancellation::Late
        );

        let foreign = EvalContext::standalone();
        assert_eq!(
            foreign.cancel_reflection_task(&task),
            EvaluationTaskCancellation::ForeignSession
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
            EvaluationTaskPoll::Failed(error)
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
            EvaluationTaskPoll::Failed(error) if error.to_string() == "reasoning failed"
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
            EvaluationTaskPoll::Complete(_)
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
            EvaluationTaskPoll::Cancelled
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
                .map(|error| error.legacy_message()),
            Some(Arc::from("retryable evaluation error"))
        );
    }

    #[test]
    fn live_foreign_dependencies_are_reported_as_quiescent() {
        let owner = EvalContext::standalone();
        let dependency = owner
            .schedule_task(|_| Ok(Box::new(Complete)))
            .expect("foreign dependency should schedule");
        let observer = EvalContext::standalone();
        let dependency_wait = dependency.wait.clone();
        let follower = observer
            .schedule_task(move |task_context| {
                Ok(Box::new(Await {
                    context: task_context,
                    dependency: dependency_wait,
                }))
            })
            .expect("foreign follower should schedule");

        let EvaluationSessionRun::Quiescent(report) = observer.run_until_quiescent() else {
            panic!("a live foreign dependency should produce resumable quiescence")
        };
        let blocked = report
            .unfinished
            .iter()
            .find(|task| task.task == follower.id())
            .expect("foreign follower should remain blocked");
        assert_eq!(blocked.dependency, Some(dependency.id()));
        assert_eq!(blocked.dependency_session, Some(dependency.session_id()));
        assert_eq!(blocked.wait, Some(dependency.wait.get()));

        let EvaluationSessionRun::Complete(owner_report) = owner.run_until_quiescent() else {
            panic!("foreign producer should complete independently")
        };
        assert!(owner_report.unfinished.is_empty());
        let EvaluationSessionRun::Complete(observer_report) = observer.run_until_quiescent() else {
            panic!("polling again should observe foreign task completion")
        };
        assert!(observer_report.unfinished.is_empty());
    }

    #[test]
    fn a_dropped_foreign_session_turns_its_follower_into_a_failure() {
        let dependency_wait = {
            let owner = EvalContext::standalone();
            owner
                .schedule_task(|_| Ok(Box::new(Complete)))
                .expect("foreign dependency should schedule")
                .wait
                .clone()
        };
        let observer = EvalContext::standalone();
        let follower = observer
            .schedule_task(move |task_context| {
                Ok(Box::new(Await {
                    context: task_context,
                    dependency: dependency_wait,
                }))
            })
            .expect("foreign follower should schedule");

        let EvaluationSessionRun::Complete(report) = observer.run_until_quiescent() else {
            panic!("a dead foreign producer cannot leave retryable work")
        };
        let failure = report
            .failures
            .get(&follower.id())
            .expect("the foreign follower should fail");
        assert_eq!(
            failure.to_string(),
            "reflection task's evaluation session no longer exists"
        );
        assert!(report.unfinished.is_empty());
    }

    #[test]
    fn zero_worker_executor_drops_sparks_without_forcing_them() {
        let executor = EvaluationExecutor::new(0).unwrap();
        let session = EvaluationSession::shared(&executor);
        let context = EvalContext::new(session);
        let lazy = crate::core::LazyValue::deferred("unforced spark", |_| {
            panic!("zero-worker spark must never be evaluated")
        });

        context.spark(Value::Lazy(lazy.clone()));

        assert!(lazy.cached().is_none());
    }

    #[test]
    fn workers_force_sparks_and_poll_ready_reflection_tasks() {
        let executor = EvaluationExecutor::new(1).unwrap();
        let session = EvaluationSession::shared(&executor);
        let context = EvalContext::new(session);
        let (spark_sender, spark_receiver) = mpsc::channel();
        let lazy = crate::core::LazyValue::deferred("worker spark", move |_| {
            spark_sender
                .send(())
                .expect("spark receiver should remain open");
            Ok((*crate::core::keys::UNIT_VALUE).clone())
        });
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
