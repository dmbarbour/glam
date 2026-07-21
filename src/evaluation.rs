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

use crate::core::{
    DeferredValueId, LazyCycle, LazyCycleMember, LazyFailure, LazyValue, PromisedValue, Value,
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

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_WAIT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

fn allocate_task_id() -> Result<EvaluationTaskId, Arc<str>> {
    allocate_id(&NEXT_TASK_ID, "evaluation task IDs exhausted").map(EvaluationTaskId)
}

fn allocate_wait_token(session: &Arc<EvaluationSession>) -> Result<EvaluationWaitToken, Arc<str>> {
    Ok(EvaluationWaitToken {
        id: allocate_id(&NEXT_WAIT_ID, "evaluation wait-token IDs exhausted")?,
        owner: Arc::downgrade(session),
    })
}

fn allocate_session_id() -> u64 {
    NEXT_SESSION_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("evaluation session IDs exhausted")
}

fn allocate_id(source: &AtomicU64, exhausted: &'static str) -> Result<NonZeroU64, Arc<str>> {
    source
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map(|id| NonZeroU64::new(id).expect("evaluation IDs start at one"))
        .map_err(|_| Arc::from(exhausted))
}

#[derive(Clone)]
pub(crate) struct EvaluationWaitToken {
    id: NonZeroU64,
    owner: Weak<EvaluationSession>,
}

impl EvaluationWaitToken {
    pub(crate) fn get(&self) -> u64 {
        self.id.get()
    }
}

impl fmt::Debug for EvaluationWaitToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("EvaluationWaitToken")
            .field(&self.id)
            .finish()
    }
}

impl PartialEq for EvaluationWaitToken {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for EvaluationWaitToken {}

impl Hash for EvaluationWaitToken {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
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
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationTaskPoll {
    Pending(EvaluationWaitToken),
    Complete(Value),
    Failed(Arc<str>),
    Cancelled,
    ForeignSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationTaskCancellation {
    Requested,
    Late,
    ForeignSession,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationTaskBlock {
    pub(crate) lazy: Option<EvaluationWaitToken>,
    pub(crate) observed_generation: Option<u64>,
    pub(crate) error: Option<Arc<str>>,
}

pub(crate) enum EvaluationMachinePoll {
    Yielded,
    Blocked(EvaluationTaskBlock),
    Complete(Value),
    Failed(Arc<str>),
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
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<str>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationTaskStatus {
    Launched,
    Blocked,
    Complete(Value),
    Failed(Arc<str>),
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
    NoProgress,
    BudgetExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationSessionRun {
    Complete(EvaluationSessionReport),
    Quiescent(EvaluationSessionReport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationSessionReport {
    pub(crate) failures: Vec<EvaluationTaskFailure>,
    pub(crate) unfinished: Vec<EvaluationUnfinishedTask>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationTaskFailure {
    pub(crate) task: EvaluationTaskId,
    pub(crate) error: Arc<str>,
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
    pub(crate) wait: Option<u64>,
    pub(crate) observed_generation: Option<u64>,
    pub(crate) error: Option<Arc<str>>,
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
    Failed(Arc<str>),
    Cancelled,
}

struct ReflectionTaskRecord {
    id: EvaluationTaskId,
    state: EvaluationTaskState,
    machine: Option<Box<dyn EvaluationTaskMachine>>,
    cancel_requested: bool,
    status_sinks: Vec<Arc<dyn EvaluationTaskStatusSink>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeferredTaskState {
    Dormant,
    Running,
    Blocked(EvaluationTaskBlock),
    Complete(Value),
    Failed(Arc<LazyFailure>),
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

    pub(crate) fn activate(&self, status: Arc<dyn EvaluationTaskStatusSink>) {
        if self.inner.activated.swap(true, Ordering::AcqRel) {
            return;
        }
        self.inner.context.activate_reflection_task(
            &self.inner.handle,
            self.inner.effect.clone(),
            ReflectionTaskKind::Joinable,
            Some(status),
        );
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
    result: Weak<OnceLock<Result<Value, Arc<str>>>>,
}

#[derive(Default)]
struct EvaluationTasks {
    reflection: HashMap<EvaluationWaitToken, ReflectionTaskRecord>,
    reflection_by_id: BTreeMap<EvaluationTaskId, EvaluationWaitToken>,
    ready: VecDeque<EvaluationTaskId>,
    promises: HashMap<EvaluationWaitToken, PromiseRecord>,
    owned_promises: HashMap<EvaluationTaskId, Vec<EvaluationWaitToken>>,
    deferred: HashMap<DeferredValueId, DeferredTaskRecord>,
    deferred_by_wait: HashMap<EvaluationWaitToken, DeferredValueId>,
    deferred_by_task: HashMap<EvaluationTaskId, DeferredValueId>,
}

pub(crate) struct EvaluationSession {
    id: u64,
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
            executor.notify_session_ready(self.id);
        }
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
    originating_task: Option<EvaluationTaskId>,
}

impl EvalContext {
    pub(crate) fn new(session: Arc<EvaluationSession>) -> Self {
        Self {
            session,
            task: Arc::new(OnceLock::new()),
            scheduled_task: false,
            originating_task: None,
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

    pub(crate) fn runs_scheduled_task(&self) -> bool {
        self.scheduled_task
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
        let wait = allocate_wait_token(&self.session)?;
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

    pub(crate) fn register_promise(
        &self,
        result: &Arc<OnceLock<Result<Value, Arc<str>>>>,
    ) -> Result<(EvaluationTaskId, EvaluationWaitToken), Arc<str>> {
        let owner = self.task_id()?;
        let wait = allocate_wait_token(&self.session)?;
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

    pub(crate) fn register_result_promise(
        &self,
        result: &Arc<OnceLock<Result<Value, Arc<str>>>>,
    ) -> Result<(EvaluationTaskId, EvaluationWaitToken), Arc<str>> {
        let owner = self.task_id()?;
        let wait = allocate_wait_token(&self.session)?;
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

    pub(crate) fn fail_unresolved_promises(&self, reason: impl Into<Arc<str>>) {
        let Some(Ok(owner)) = self.task.get() else {
            return;
        };
        let owner = *owner;
        let reason = reason.into();
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let waits = tasks.owned_promises.remove(&owner).unwrap_or_default();
        for wait in waits {
            let Some(promise) = tasks.promises.get(&wait) else {
                continue;
            };
            if let Some(result) = promise.result.upgrade() {
                let _ = result.set(Err(reason.clone()));
            }
        }
    }

    pub(crate) fn release_owned_promise(
        &self,
        owner: EvaluationTaskId,
        wait: &EvaluationWaitToken,
    ) {
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let remove_owner = if let Some(waits) = tasks.owned_promises.get_mut(&owner) {
            waits.retain(|candidate| candidate != wait);
            waits.is_empty()
        } else {
            false
        };
        if remove_owner {
            tasks.owned_promises.remove(&owner);
        }
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
        let wait = allocate_wait_token(&self.session)?;
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
        let wait = allocate_wait_token(&self.session)?;
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
    ) {
        let result = self
            .session
            .reflection_launcher
            .get()
            .ok_or_else(|| Arc::from("evaluation session has no reflection task launcher"))
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
        if let Some(status_sink) = status_sink {
            record.status_sinks.push(status_sink);
        }
        match result {
            Ok(machine) => {
                record.machine = Some(machine);
                record.state = EvaluationTaskState::Queued;
                tasks.ready.push_back(handle.id);
            }
            Err(error) => record.state = EvaluationTaskState::Failed(error),
        }
        self.session.task_changed.notify_all();
        let queued = matches!(
            tasks
                .reflection
                .get(&handle.wait)
                .map(|record| &record.state),
            Some(EvaluationTaskState::Queued)
        );
        let status = tasks
            .reflection
            .get_mut(&handle.wait)
            .and_then(|record| task_status_update(record, Some(&prior)));
        drop(tasks);
        publish_task_status(status);
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
        if tasks
            .reflection
            .get(&handle.wait)
            .is_some_and(|record| matches!(record.state, EvaluationTaskState::Reserved))
        {
            tasks.reflection.remove(&handle.wait);
            tasks.reflection_by_id.remove(&handle.id);
            self.session.task_changed.notify_all();
        }
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
            self.activate_reflection_task(&handle, effect, ReflectionTaskKind::Annotation, None);
            return Ok(handle);
        }

        // Focused evaluator tests and internal clients may intentionally use a
        // bare session. Preserve an inspectable wait record for them; ordinary
        // Assembler sessions always install a launcher.
        let id = allocate_task_id()?;
        let wait = allocate_wait_token(&self.session)?;
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

    pub(crate) fn poll_reflection_task_id(&self, id: EvaluationTaskId) -> EvaluationTaskPoll {
        let wait = {
            let tasks = self
                .session
                .tasks
                .lock()
                .expect("evaluation task registry was poisoned");
            tasks.reflection_by_id.get(&id).cloned()
        };
        wait.map_or(EvaluationTaskPoll::ForeignSession, |wait| {
            self.poll_wait(&wait)
        })
    }

    pub(crate) fn cancel_reflection_task_id(
        &self,
        id: EvaluationTaskId,
    ) -> EvaluationTaskCancellation {
        let (machine, status) = {
            let mut tasks = self
                .session
                .tasks
                .lock()
                .expect("evaluation task registry was poisoned");
            let Some(wait) = tasks.reflection_by_id.get(&id).cloned() else {
                return EvaluationTaskCancellation::ForeignSession;
            };
            let record = tasks
                .reflection
                .get_mut(&wait)
                .expect("task ID index must refer to a task record");
            match record.state {
                EvaluationTaskState::Complete(_)
                | EvaluationTaskState::Failed(_)
                | EvaluationTaskState::Cancelled => return EvaluationTaskCancellation::Late,
                EvaluationTaskState::Running => {
                    record.cancel_requested = true;
                    return EvaluationTaskCancellation::Requested;
                }
                EvaluationTaskState::Dormant
                | EvaluationTaskState::Reserved
                | EvaluationTaskState::Queued
                | EvaluationTaskState::Blocked(_) => {
                    let prior = record.state.clone();
                    record.state = EvaluationTaskState::Cancelled;
                    self.session.task_changed.notify_all();
                    let machine = record.machine.take();
                    let status = task_status_update(record, Some(&prior));
                    (machine, status)
                }
            }
        };
        publish_task_status(status);
        if let Some(mut machine) = machine {
            machine.cancel();
        }
        EvaluationTaskCancellation::Requested
    }

    pub(crate) fn poll_wait(&self, wait: &EvaluationWaitToken) -> EvaluationTaskPoll {
        let Some(owner) = wait.owner.upgrade() else {
            return EvaluationTaskPoll::Failed(Arc::from(
                "reflection task's evaluation session no longer exists",
            ));
        };
        let tasks = owner
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let Some(record) = tasks.reflection.get(wait) else {
            if let Some(deferred) = tasks.deferred_by_wait.get(wait) {
                let record = tasks
                    .deferred
                    .get(deferred)
                    .expect("deferred wait index must refer to a task record");
                return match &record.state {
                    DeferredTaskState::Complete(value) => {
                        EvaluationTaskPoll::Complete(value.clone())
                    }
                    DeferredTaskState::Failed(error) => {
                        EvaluationTaskPoll::Failed(error.legacy_message())
                    }
                    DeferredTaskState::Dormant
                    | DeferredTaskState::Running
                    | DeferredTaskState::Blocked(_) => {
                        if Arc::ptr_eq(&self.session, &owner) {
                            EvaluationTaskPoll::Pending(wait.clone())
                        } else {
                            EvaluationTaskPoll::ForeignSession
                        }
                    }
                };
            }
            let Some(promise) = tasks.promises.get(wait) else {
                return EvaluationTaskPoll::Failed(Arc::from(
                    "evaluation wait token is no longer registered",
                ));
            };
            let Some(result) = promise.result.upgrade() else {
                return EvaluationTaskPoll::Failed(Arc::from("promised value no longer exists"));
            };
            return match result.get() {
                Some(Ok(value)) => EvaluationTaskPoll::Complete(value.clone()),
                Some(Err(error)) => EvaluationTaskPoll::Failed(error.clone()),
                None if Arc::ptr_eq(&self.session, &owner) => {
                    EvaluationTaskPoll::Pending(wait.clone())
                }
                None => EvaluationTaskPoll::ForeignSession,
            };
        };
        match &record.state {
            EvaluationTaskState::Complete(value) => EvaluationTaskPoll::Complete(value.clone()),
            EvaluationTaskState::Failed(error) => EvaluationTaskPoll::Failed(error.clone()),
            EvaluationTaskState::Cancelled => EvaluationTaskPoll::Cancelled,
            EvaluationTaskState::Dormant
            | EvaluationTaskState::Reserved
            | EvaluationTaskState::Queued
            | EvaluationTaskState::Running
            | EvaluationTaskState::Blocked(_) => {
                if Arc::ptr_eq(&self.session, &owner) {
                    EvaluationTaskPoll::Pending(wait.clone())
                } else {
                    EvaluationTaskPoll::ForeignSession
                }
            }
        }
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
        tasks
            .reflection
            .get_mut(&wait)
            .expect("test task must belong to this session")
            .state = EvaluationTaskState::Complete((*crate::core::keys::UNIT_VALUE).clone());
        self.session.task_changed.notify_all();
        drop(tasks);
        while matches!(
            self.pump_wait(&target, 256),
            EvaluationPumpOutcome::BudgetExhausted
        ) {}
    }

    #[cfg(test)]
    pub(crate) fn fail_wait(&self, wait: &EvaluationWaitToken, error: impl Into<Arc<str>>) {
        let target = wait.clone();
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let wait = test_reflection_dependency(&tasks, wait);
        tasks
            .reflection
            .get_mut(&wait)
            .expect("test task must belong to this session")
            .state = EvaluationTaskState::Failed(error.into());
        self.session.task_changed.notify_all();
        drop(tasks);
        while matches!(
            self.pump_wait(&target, 256),
            EvaluationPumpOutcome::BudgetExhausted
        ) {}
    }

    #[cfg(test)]
    pub(crate) fn reflection_task_count(&self) -> usize {
        self.session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned")
            .reflection
            .len()
    }

    #[cfg(test)]
    pub(crate) fn deferred_task_count(&self) -> usize {
        self.session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned")
            .deferred
            .len()
    }

    #[cfg(test)]
    pub(crate) fn lazy_failure(&self, lazy: &LazyValue) -> Option<Arc<LazyFailure>> {
        self.deferred_failure(lazy.id().into())
    }

    #[cfg(test)]
    pub(crate) fn promise_failure(&self, promise: &PromisedValue) -> Option<Arc<LazyFailure>> {
        self.deferred_failure(promise.id().into())
    }

    #[cfg(test)]
    fn deferred_failure(&self, deferred: DeferredValueId) -> Option<Arc<LazyFailure>> {
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
    while seen.insert(wait.clone()) {
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
                return Self::session_run_report(&tasks);
            };

            let poll = claimed.poll(TASK_POLL_QUANTUM);
            let claimed_id = claimed.id();
            let (made_progress, remains_blocked, cancelled, status) =
                self.release_task(claimed, poll);
            publish_task_status(status);
            self.notify_executor_if_ready();
            if let Some(mut machine) = cancelled {
                machine.cancel();
            }
            if remains_blocked {
                attempted_blocked.insert(claimed_id);
            }
            if made_progress {
                attempted_blocked.clear();
            }
        }
    }

    fn session_run_report(tasks: &EvaluationTasks) -> EvaluationSessionRun {
        let mut failures = Vec::new();
        let mut unfinished = Vec::new();
        for (task, wait) in &tasks.reflection_by_id {
            let record = tasks
                .reflection
                .get(wait)
                .expect("task ID index must refer to a task record");
            match &record.state {
                EvaluationTaskState::Complete(_) | EvaluationTaskState::Cancelled => {}
                EvaluationTaskState::Failed(error) => failures.push(EvaluationTaskFailure {
                    task: *task,
                    error: error.clone(),
                }),
                state => {
                    let (state, dependency, dependency_wait, observed_generation, error) =
                        match state {
                            EvaluationTaskState::Dormant => {
                                (EvaluationUnfinishedState::Dormant, None, None, None, None)
                            }
                            EvaluationTaskState::Reserved => {
                                (EvaluationUnfinishedState::Reserved, None, None, None, None)
                            }
                            EvaluationTaskState::Queued => {
                                (EvaluationUnfinishedState::Queued, None, None, None, None)
                            }
                            EvaluationTaskState::Running => {
                                (EvaluationUnfinishedState::Running, None, None, None, None)
                            }
                            EvaluationTaskState::Blocked(block) => (
                                EvaluationUnfinishedState::Blocked,
                                block
                                    .lazy
                                    .as_ref()
                                    .and_then(|wait| producer_for_wait(tasks, wait)),
                                block.lazy.as_ref().map(EvaluationWaitToken::get),
                                block.observed_generation,
                                block.error.clone(),
                            ),
                            EvaluationTaskState::Complete(_)
                            | EvaluationTaskState::Failed(_)
                            | EvaluationTaskState::Cancelled => {
                                unreachable!("terminal task states were handled above")
                            }
                        };
                    unfinished.push(EvaluationUnfinishedTask {
                        task: *task,
                        state,
                        dependency,
                        wait: dependency_wait,
                        observed_generation,
                        error,
                    });
                }
            }
        }
        let report = EvaluationSessionReport {
            failures,
            unfinished,
        };
        if report.unfinished.is_empty() {
            EvaluationSessionRun::Complete(report)
        } else {
            EvaluationSessionRun::Quiescent(report)
        }
    }

    fn pump(
        &self,
        context: &EvalContext,
        target: &EvaluationWaitToken,
        mut step_budget: usize,
    ) -> EvaluationPumpOutcome {
        if !target
            .owner
            .upgrade()
            .is_some_and(|owner| Arc::ptr_eq(&context.session, &owner))
        {
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
                if target_has_running_deferred_producer(&tasks, target)
                    || context.runs_scheduled_task() && target_has_running_producer(&tasks, target)
                {
                    return EvaluationPumpOutcome::NoProgress;
                }
                let prioritized = prioritized_task(&tasks, target, &attempted_blocked);
                prioritized
                    .and_then(|id| claim_task(&mut tasks, id))
                    .or_else(|| claim_ready_task(&mut tasks))
                    .or_else(|| claim_blocked_task(&mut tasks, &attempted_blocked))
            };
            let Some(mut claimed) = claimed else {
                let mut tasks = self
                    .tasks
                    .lock()
                    .expect("evaluation task registry was poisoned");
                if target_has_running_producer(&tasks, target) {
                    if context.runs_scheduled_task()
                        || target_has_running_deferred_producer(&tasks, target)
                    {
                        return EvaluationPumpOutcome::NoProgress;
                    }
                    tasks = self
                        .task_changed
                        .wait(tasks)
                        .expect("evaluation task registry was poisoned");
                    drop(tasks);
                    continue;
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
            let (made_progress, remains_blocked, cancelled, status) =
                self.release_task(claimed, poll);
            publish_task_status(status);
            self.notify_executor_if_ready();
            if let Some(mut machine) = cancelled {
                machine.cancel();
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
        Option<Box<dyn EvaluationTaskMachine>>,
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
        Option<Box<dyn EvaluationTaskMachine>>,
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
            record.state = EvaluationTaskState::Cancelled;
            self.task_changed.notify_all();
            let status = task_status_update(record, Some(&claimed.prior_state));
            return (true, false, Some(claimed.machine), status);
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
        record.state = state;
        if matches!(record.state, EvaluationTaskState::Queued) {
            tasks.ready.push_back(claimed.id);
        }
        let status = tasks
            .reflection
            .get_mut(&claimed.wait)
            .and_then(|record| task_status_update(record, Some(&claimed.prior_state)));
        self.task_changed.notify_all();
        (made_progress, remains_blocked, None, status)
    }

    fn release_deferred_task(
        &self,
        claimed: ClaimedDeferredTask,
        poll: EvaluationMachinePoll,
    ) -> (
        bool,
        bool,
        Option<Box<dyn EvaluationTaskMachine>>,
        Option<TaskStatusUpdate>,
    ) {
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
            record.machine = Some(claimed.machine);
        }

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
            EvaluationMachinePoll::Failed(error) => (
                DeferredTaskState::Failed(Arc::new(LazyFailure::evaluation(error))),
                true,
            ),
            EvaluationMachinePoll::Cancelled => (
                DeferredTaskState::Failed(Arc::new(LazyFailure::evaluation(
                    "deferred evaluation task was cancelled",
                ))),
                true,
            ),
        };
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
        record.state = state;
        record.dependency = dependency;

        if dependency.is_some()
            && let Some(cycle) = deferred_dependency_cycle(&tasks, claimed.deferred)
        {
            poison_deferred_cycle(&mut tasks, &cycle);
            made_progress = true;
        }
        let remains_blocked = tasks
            .deferred
            .get(&claimed.deferred)
            .is_some_and(|record| matches!(record.state, DeferredTaskState::Blocked(_)));
        self.task_changed.notify_all();
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
        let (_, _, cancelled, status) = self.release_task(claimed, poll);
        publish_task_status(status);
        if let Some(mut machine) = cancelled {
            machine.cancel();
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
    if let Some(record) = tasks.reflection.get(wait) {
        return matches!(
            record.state,
            EvaluationTaskState::Complete(_)
                | EvaluationTaskState::Failed(_)
                | EvaluationTaskState::Cancelled
        );
    }
    if let Some(deferred) = tasks.deferred_by_wait.get(wait) {
        return tasks.deferred.get(deferred).is_some_and(|record| {
            matches!(
                record.state,
                DeferredTaskState::Complete(_) | DeferredTaskState::Failed(_)
            )
        });
    }
    tasks
        .promises
        .get(wait)
        .and_then(|promise| promise.result.upgrade())
        .is_some_and(|result| result.get().is_some())
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
            let mut cycle = path[first..].to_vec();
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

/// Installs one shared structured failure in every member of a proven strict
/// deferred cycle. The legacy lazy string cache remains a projection until Spike 4
/// changes the cache result type to `LazyFailure` directly.
fn poison_deferred_cycle(tasks: &mut EvaluationTasks, members: &[DeferredValueId]) {
    let cycle = Arc::new(LazyCycle {
        members: members
            .iter()
            .map(|id| {
                let record = tasks
                    .deferred
                    .get(id)
                    .expect("cycle members must remain registered");
                LazyCycleMember {
                    id: *id,
                    label: record.value.label().clone(),
                }
            })
            .collect(),
    });
    let failure = Arc::new(LazyFailure::DependencyCycle(cycle));
    let legacy = failure.legacy_message();

    for id in members {
        let record = tasks
            .deferred
            .get_mut(id)
            .expect("cycle members must remain registered");
        record.dependency = None;
        record.state = match &record.value {
            DeferredValue::Promise(_) => DeferredTaskState::Failed(failure.clone()),
            DeferredValue::Lazy(lazy) => match lazy.cache(Err(legacy.clone())) {
                Err(error) if Arc::ptr_eq(&error, &legacy) => {
                    DeferredTaskState::Failed(failure.clone())
                }
                Err(error) => DeferredTaskState::Failed(Arc::new(LazyFailure::evaluation(error))),
                Ok(value) => {
                    debug_assert!(
                        false,
                        "a successful concurrent lazy result contradicts a strict dependency cycle"
                    );
                    DeferredTaskState::Complete(value)
                }
            },
        };
    }
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

fn target_has_running_deferred_producer(
    tasks: &EvaluationTasks,
    target: &EvaluationWaitToken,
) -> bool {
    let mut seen = HashSet::new();
    let mut wait = target.clone();
    while let Some(id) = producer_for_wait(tasks, &wait) {
        if !seen.insert(id) {
            return false;
        }
        if let Some(deferred) = tasks.deferred_by_task.get(&id) {
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
            continue;
        }
        let Some(task_wait) = tasks.reflection_by_id.get(&id) else {
            return false;
        };
        let Some(record) = tasks.reflection.get(task_wait) else {
            return false;
        };
        match &record.state {
            EvaluationTaskState::Blocked(block) => {
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
    use std::time::Duration;

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
                EvaluationTaskPoll::Failed(error) => EvaluationMachinePoll::Failed(error),
                EvaluationTaskPoll::Cancelled => EvaluationMachinePoll::Cancelled,
                EvaluationTaskPoll::ForeignSession => EvaluationMachinePoll::Failed(Arc::from(
                    "test dependency unexpectedly belongs to another session",
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
                EvaluationTaskPoll::Pending(wait) => {
                    EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                        lazy: Some(wait),
                        observed_generation: None,
                        error: None,
                    })
                }
                EvaluationTaskPoll::Complete(value) => EvaluationMachinePoll::Complete(value),
                EvaluationTaskPoll::Failed(error) => EvaluationMachinePoll::Failed(error),
                EvaluationTaskPoll::Cancelled => EvaluationMachinePoll::Cancelled,
                EvaluationTaskPoll::ForeignSession => EvaluationMachinePoll::Failed(Arc::from(
                    "test dependency unexpectedly belongs to another session",
                )),
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

    fn dependency_cycle(context: &EvalContext, lazy: &LazyValue) -> Arc<LazyCycle> {
        match context
            .lazy_failure(lazy)
            .expect("test lazy should have a structured failure")
            .as_ref()
        {
            LazyFailure::DependencyCycle(cycle) => cycle.clone(),
            LazyFailure::Evaluation(error) => {
                panic!("expected dependency cycle, got evaluation failure: {error}")
            }
        }
    }

    struct AlwaysBlocked;

    impl EvaluationTaskMachine for AlwaysBlocked {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                lazy: None,
                observed_generation: Some(7),
                error: Some(Arc::from("retryable evaluation error")),
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
            EvaluationMachinePoll::Failed(Arc::from("reasoning failed"))
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
        let cycle = dependency_cycle(&context, &lazy);
        assert_eq!(cycle.members.len(), 1);
        assert_eq!(cycle.members[0].id, lazy.id().into());
        assert_eq!(cycle.members[0].label.as_ref(), "self cycle");
        assert!(matches!(
            context.poll_wait(&wait),
            EvaluationTaskPoll::Failed(error)
                if error.contains("lazy dependency cycle")
        ));
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
        assert_eq!(
            left_thread.join().unwrap(),
            EvaluationPumpOutcome::TargetReady
        );
        assert_eq!(
            right_thread.join().unwrap(),
            EvaluationPumpOutcome::TargetReady
        );

        let left_failure = context.lazy_failure(&left).unwrap();
        let right_failure = context.lazy_failure(&right).unwrap();
        assert!(Arc::ptr_eq(&left_failure, &right_failure));
        let cycle = dependency_cycle(&context, &left);
        assert_eq!(
            cycle
                .members
                .iter()
                .map(|member| member.id)
                .collect::<Vec<_>>(),
            vec![left.id().into(), right.id().into()]
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
        let cycle = dependency_cycle(&context, &first);
        assert_eq!(
            cycle
                .members
                .iter()
                .map(|member| member.id)
                .collect::<Vec<_>>(),
            vec![first.id().into(), second.id().into(), third.id().into()]
        );
        assert!(matches!(
            context.lazy_failure(&upstream).as_deref(),
            Some(LazyFailure::Evaluation(error))
                if error.contains("lazy dependency cycle")
        ));
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
            context.cancel_reflection_task_id(task.id()),
            EvaluationTaskCancellation::Requested
        );
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(
            context.poll_reflection_task(&task),
            EvaluationTaskPoll::Cancelled
        );
        assert_eq!(
            context.cancel_reflection_task_id(task.id()),
            EvaluationTaskCancellation::Late
        );

        let foreign = EvalContext::standalone();
        assert_eq!(
            foreign.cancel_reflection_task_id(task.id()),
            EvaluationTaskCancellation::ForeignSession
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
        assert_eq!(context.reflection_task_count(), 2);
    }

    #[test]
    fn run_until_quiescent_collects_failures_without_short_circuiting() {
        let context = EvalContext::standalone();
        context.schedule_task(|_| Ok(Box::new(Fail))).unwrap();
        context.schedule_task(|_| Ok(Box::new(Complete))).unwrap();

        let EvaluationSessionRun::Complete(report) = context.run_until_quiescent() else {
            panic!("terminal failures do not leave unfinished work");
        };
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].error.as_ref(), "reasoning failed");
        assert!(report.unfinished.is_empty());
    }

    #[test]
    fn run_until_quiescent_reports_stable_blocked_tasks() {
        let context = EvalContext::standalone();
        let task = context
            .schedule_task(|_| Ok(Box::new(AlwaysBlocked)))
            .unwrap();

        let EvaluationSessionRun::Quiescent(report) = context.run_until_quiescent() else {
            panic!("an unchanged blocked task should leave the session quiescent");
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
            report.unfinished[0].error.as_deref(),
            Some("retryable evaluation error")
        );
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
