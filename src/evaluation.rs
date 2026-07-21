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

use crate::core::{LazyId, LazyValue, Value};

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
enum LazyTaskState {
    Dormant,
    Running,
    Blocked(EvaluationTaskBlock),
    Complete(Value),
    Failed(Arc<str>),
}

struct LazyTaskRecord {
    id: EvaluationTaskId,
    wait: EvaluationWaitToken,
    state: LazyTaskState,
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
    lazy: HashMap<LazyId, LazyTaskRecord>,
    lazy_by_wait: HashMap<EvaluationWaitToken, LazyId>,
    lazy_by_id: HashMap<EvaluationTaskId, LazyId>,
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

    fn for_lazy_task(
        session: Arc<EvaluationSession>,
        id: EvaluationTaskId,
        originating_task: Option<EvaluationTaskId>,
    ) -> Self {
        let task = Arc::new(OnceLock::new());
        task.set(Ok(id))
            .expect("fresh lazy task identity cell must be empty");
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
        if matches!(value, Value::Lazy(_) | Value::Net(_)) {
            self.session.submit_spark(value);
        }
    }

    pub(crate) fn runs_scheduled_task(&self) -> bool {
        self.scheduled_task
    }

    pub(crate) fn is_current_lazy(&self, lazy: LazyId) -> bool {
        if !self.scheduled_task {
            return false;
        }
        let Some(Ok(task)) = self.task.get() else {
            return false;
        };
        self.session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned")
            .lazy_by_id
            .get(task)
            .is_some_and(|current| *current == lazy)
    }

    pub(crate) fn observes_as_task(&self, task: EvaluationTaskId) -> bool {
        self.originating_task == Some(task)
            || matches!(self.task.get(), Some(Ok(current)) if *current == task)
    }

    pub(crate) fn wait_depends_on_lazy(&self, wait: &EvaluationWaitToken, target: LazyId) -> bool {
        let tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let mut wait = wait.clone();
        let mut seen = HashSet::new();
        while seen.insert(wait.clone()) {
            if tasks.lazy_by_wait.get(&wait) == Some(&target) {
                return true;
            }
            let Some(producer) = producer_for_wait(&tasks, &wait) else {
                return false;
            };
            let Some(dependency) = task_dependency(&tasks, &producer) else {
                return false;
            };
            wait = dependency.clone();
        }
        false
    }

    pub(crate) fn lazy_task<F>(
        &self,
        lazy: &LazyValue,
        build: F,
    ) -> Result<EvaluationWaitToken, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Box<dyn EvaluationTaskMachine>,
    {
        {
            let tasks = self
                .session
                .tasks
                .lock()
                .expect("evaluation task registry was poisoned");
            if let Some(record) = tasks.lazy.get(&lazy.id()) {
                return Ok(record.wait.clone());
            }
        }

        let id = allocate_task_id()?;
        let wait = allocate_wait_token(&self.session)?;
        let originating_task = self
            .originating_task
            .or_else(|| self.task.get().and_then(|task| task.as_ref().ok()).copied());
        let machine = build(Self::for_lazy_task(
            self.session.clone(),
            id,
            originating_task,
        ));
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        if let Some(record) = tasks.lazy.get(&lazy.id()) {
            return Ok(record.wait.clone());
        }
        let record = LazyTaskRecord {
            id,
            wait: wait.clone(),
            state: LazyTaskState::Dormant,
            machine: Some(machine),
        };
        assert!(
            tasks.lazy.insert(lazy.id(), record).is_none()
                && tasks.lazy_by_wait.insert(wait.clone(), lazy.id()).is_none()
                && tasks.lazy_by_id.insert(id, lazy.id()).is_none(),
            "lazy task identities must be unique"
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
            if let Some(lazy) = tasks.lazy_by_wait.get(wait) {
                let record = tasks
                    .lazy
                    .get(lazy)
                    .expect("lazy wait index must refer to a task record");
                return match &record.state {
                    LazyTaskState::Complete(value) => EvaluationTaskPoll::Complete(value.clone()),
                    LazyTaskState::Failed(error) => EvaluationTaskPoll::Failed(error.clone()),
                    LazyTaskState::Dormant | LazyTaskState::Running | LazyTaskState::Blocked(_) => {
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
    pub(crate) fn lazy_task_count(&self) -> usize {
        self.session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned")
            .lazy
            .len()
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
        let Some(lazy) = tasks.lazy_by_wait.get(&wait) else {
            break;
        };
        let Some(record) = tasks.lazy.get(lazy) else {
            break;
        };
        let LazyTaskState::Blocked(block) = &record.state else {
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

struct ClaimedLazyTask {
    id: EvaluationTaskId,
    lazy: LazyId,
    prior_state: LazyTaskState,
    machine: Box<dyn EvaluationTaskMachine>,
}

enum ClaimedTask {
    Reflection(ClaimedReflectionTask),
    Lazy(ClaimedLazyTask),
}

impl ClaimedTask {
    fn id(&self) -> EvaluationTaskId {
        match self {
            Self::Reflection(task) => task.id,
            Self::Lazy(task) => task.id,
        }
    }

    fn poll(&mut self, step_budget: usize) -> EvaluationMachinePoll {
        match self {
            Self::Reflection(task) => task.machine.poll(step_budget),
            Self::Lazy(task) => task.machine.poll(step_budget),
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
                if target_has_running_lazy_producer(&tasks, target)
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
                        || target_has_running_lazy_producer(&tasks, target)
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
            ClaimedTask::Lazy(claimed) => self.release_lazy_task(claimed, poll),
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

    fn release_lazy_task(
        &self,
        claimed: ClaimedLazyTask,
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
            .lazy
            .get_mut(&claimed.lazy)
            .expect("claimed lazy task must remain registered");
        assert_eq!(record.id, claimed.id, "lazy task ID index must agree");
        assert!(
            matches!(record.state, LazyTaskState::Running),
            "only a running lazy task may release its machine"
        );
        assert!(record.machine.is_none(), "claimed machine must be absent");
        record.machine = Some(claimed.machine);

        let (state, made_progress, remains_blocked) = match poll {
            EvaluationMachinePoll::Yielded => (LazyTaskState::Dormant, true, false),
            EvaluationMachinePoll::Blocked(block) => {
                let unchanged = matches!(
                    &claimed.prior_state,
                    LazyTaskState::Blocked(prior) if prior == &block
                );
                (LazyTaskState::Blocked(block), !unchanged, true)
            }
            EvaluationMachinePoll::Complete(value) => (LazyTaskState::Complete(value), true, false),
            EvaluationMachinePoll::Failed(error) => (LazyTaskState::Failed(error), true, false),
            EvaluationMachinePoll::Cancelled => (
                LazyTaskState::Failed(Arc::from("lazy evaluation task was cancelled")),
                true,
                false,
            ),
        };
        record.state = state;
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
                .lazy_by_wait
                .get(wait)
                .and_then(|lazy| tasks.lazy.get(lazy))
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
    let lazy = tasks.lazy_by_id.get(id)?;
    let record = tasks.lazy.get(lazy)?;
    match &record.state {
        LazyTaskState::Blocked(block) => block.lazy.as_ref(),
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
        .lazy_by_id
        .get(id)
        .and_then(|lazy| tasks.lazy.get(lazy))
        .is_some_and(|record| match &record.state {
            LazyTaskState::Dormant => true,
            LazyTaskState::Blocked(block) => block
                .lazy
                .as_ref()
                .is_some_and(|wait| wait_is_terminal(tasks, wait)),
            LazyTaskState::Running | LazyTaskState::Complete(_) | LazyTaskState::Failed(_) => false,
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
    if let Some(lazy) = tasks.lazy_by_wait.get(wait) {
        return tasks.lazy.get(lazy).is_some_and(|record| {
            matches!(
                record.state,
                LazyTaskState::Complete(_) | LazyTaskState::Failed(_)
            )
        });
    }
    tasks
        .promises
        .get(wait)
        .and_then(|promise| promise.result.upgrade())
        .is_some_and(|result| result.get().is_some())
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
        let Some(lazy) = tasks.lazy_by_id.get(&id) else {
            return false;
        };
        let Some(record) = tasks.lazy.get(lazy) else {
            return false;
        };
        match &record.state {
            LazyTaskState::Running => return true,
            LazyTaskState::Blocked(block) => {
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

fn target_has_running_lazy_producer(tasks: &EvaluationTasks, target: &EvaluationWaitToken) -> bool {
    let mut seen = HashSet::new();
    let mut wait = target.clone();
    while let Some(id) = producer_for_wait(tasks, &wait) {
        if !seen.insert(id) {
            return false;
        }
        if let Some(lazy) = tasks.lazy_by_id.get(&id) {
            let Some(record) = tasks.lazy.get(lazy) else {
                return false;
            };
            match &record.state {
                LazyTaskState::Running => return true,
                LazyTaskState::Blocked(block) => {
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
    let lazy = *tasks.lazy_by_id.get(&id)?;
    let record = tasks.lazy.get_mut(&lazy)?;
    if !matches!(
        record.state,
        LazyTaskState::Dormant | LazyTaskState::Blocked(_)
    ) {
        return None;
    }
    let machine = record.machine.take()?;
    let prior_state = std::mem::replace(&mut record.state, LazyTaskState::Running);
    Some(ClaimedTask::Lazy(ClaimedLazyTask {
        id,
        lazy,
        prior_state,
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
