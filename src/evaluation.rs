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

use crate::core::{Dict, Value};

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
pub(crate) struct EvaluationQueryId(NonZeroU64);

impl EvaluationQueryId {
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) fn from_u64(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }
}

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_QUERY_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_WAIT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

fn allocate_task_id() -> Result<EvaluationTaskId, Arc<str>> {
    allocate_id(&NEXT_TASK_ID, "evaluation task IDs exhausted").map(EvaluationTaskId)
}

fn allocate_query_id() -> Result<EvaluationQueryId, Arc<str>> {
    allocate_id(&NEXT_QUERY_ID, "evaluation query IDs exhausted").map(EvaluationQueryId)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EvaluationQueryHandle {
    id: EvaluationQueryId,
}

impl EvaluationQueryHandle {
    pub(crate) fn id(&self) -> EvaluationQueryId {
        self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationQueryPoll {
    Pending,
    Complete(Value),
    ForeignSession,
}

impl EvaluationTaskHandle {
    pub(crate) fn id(&self) -> EvaluationTaskId {
        self.id
    }

    #[allow(dead_code)] // Scheduler-facing inspection, currently used by focused tests.
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
}

#[derive(Clone)]
pub(crate) struct PendingReflectionTask {
    inner: Arc<PendingReflectionTaskInner>,
}

#[derive(Clone)]
pub(crate) struct PendingEvaluationQuery {
    inner: Arc<PendingEvaluationQueryInner>,
}

struct PendingEvaluationQueryInner {
    context: EvalContext,
    handle: EvaluationQueryHandle,
    completed: AtomicBool,
}

impl PendingEvaluationQuery {
    pub(crate) fn handle(&self) -> &EvaluationQueryHandle {
        &self.inner.handle
    }

    pub(crate) fn complete(&self, result: Value) {
        if self.inner.completed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.inner
            .context
            .complete_query(&self.inner.handle, result);
    }
}

impl Drop for PendingEvaluationQueryInner {
    fn drop(&mut self) {
        if !self.completed.load(Ordering::Acquire) {
            self.context.cancel_reserved_query(&self.handle);
        }
    }
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

    pub(crate) fn activate(&self) {
        if self.inner.activated.swap(true, Ordering::AcqRel) {
            return;
        }
        self.inner.context.activate_reflection_task(
            &self.inner.handle,
            self.inner.effect.clone(),
            ReflectionTaskKind::Joinable,
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

struct EvaluationQueryRecord {
    result: Option<Value>,
}

#[derive(Default)]
struct EvaluationTasks {
    reflection: HashMap<EvaluationWaitToken, ReflectionTaskRecord>,
    reflection_by_id: BTreeMap<EvaluationTaskId, EvaluationWaitToken>,
    ready: VecDeque<EvaluationTaskId>,
    promises: HashMap<EvaluationWaitToken, PromiseRecord>,
    owned_promises: HashMap<EvaluationTaskId, Vec<EvaluationWaitToken>>,
    queries: BTreeMap<EvaluationQueryId, EvaluationQueryRecord>,
}

pub(crate) struct EvaluationSession {
    id: u64,
    reflection_environment: Value,
    tasks: Mutex<EvaluationTasks>,
    task_changed: Condvar,
    reflection_launcher: OnceLock<Arc<dyn ReflectionTaskLauncher>>,
    executor: Weak<EvaluationExecutor>,
    lazy_claims: Mutex<HashMap<u64, EvaluationTaskId>>,
    lazy_changed: Condvar,
}

impl Default for EvaluationSession {
    fn default() -> Self {
        Self::with_environment_and_executor(Value::Dict(Dict::new_sync()), Weak::new())
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

    pub(crate) fn with_environment(reflection_environment: Value) -> Self {
        Self::with_environment_and_executor(reflection_environment, Weak::new())
    }

    fn with_environment_and_executor(
        reflection_environment: Value,
        executor: Weak<EvaluationExecutor>,
    ) -> Self {
        Self {
            id: allocate_session_id(),
            reflection_environment,
            tasks: Mutex::new(EvaluationTasks::default()),
            task_changed: Condvar::new(),
            reflection_launcher: OnceLock::new(),
            executor,
            lazy_claims: Mutex::new(HashMap::new()),
            lazy_changed: Condvar::new(),
        }
    }

    pub(crate) fn shared(
        reflection_environment: Value,
        executor: &Arc<EvaluationExecutor>,
    ) -> Arc<Self> {
        let session = Arc::new(Self::with_environment_and_executor(
            reflection_environment,
            Arc::downgrade(executor),
        ));
        executor.register_session(&session);
        session
    }

    fn reflection_environment(&self) -> Value {
        self.reflection_environment.clone()
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

pub(crate) struct LazyEvaluationClaim {
    session: Arc<EvaluationSession>,
    lazy: u64,
    owner: EvaluationTaskId,
}

impl Drop for LazyEvaluationClaim {
    fn drop(&mut self) {
        let mut claims = self
            .session
            .lazy_claims
            .lock()
            .expect("lazy evaluation claim table was poisoned");
        if claims.get(&self.lazy) == Some(&self.owner) {
            claims.remove(&self.lazy);
            self.session.lazy_changed.notify_all();
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
}

impl EvalContext {
    pub(crate) fn new(session: Arc<EvaluationSession>) -> Self {
        Self {
            session,
            task: Arc::new(OnceLock::new()),
        }
    }

    fn for_task(session: Arc<EvaluationSession>, id: EvaluationTaskId) -> Self {
        let task = Arc::new(OnceLock::new());
        task.set(Ok(id))
            .expect("fresh task identity cell must be empty");
        Self { session, task }
    }

    /// Creates a session for internal clients that do not yet run under an
    /// assembler, notably standalone reflection tasks and focused tests.
    pub(crate) fn standalone() -> Self {
        Self::new(Arc::new(EvaluationSession::new()))
    }

    pub(crate) fn standalone_with_environment(environment: Value) -> Self {
        Self::new(Arc::new(EvaluationSession::with_environment(environment)))
    }

    pub(crate) fn reflection_environment(&self) -> Value {
        self.session.reflection_environment()
    }

    pub(crate) fn spark(&self, value: Value) {
        if matches!(value, Value::Lazy(_) | Value::Net(_)) {
            self.session.submit_spark(value);
        }
    }

    pub(crate) fn claim_lazy(&self, lazy: u64) -> Result<LazyEvaluationClaim, Arc<str>> {
        let owner = self.task_id()?;
        let mut claims = self
            .session
            .lazy_claims
            .lock()
            .expect("lazy evaluation claim table was poisoned");
        loop {
            match claims.get(&lazy).copied() {
                None => {
                    claims.insert(lazy, owner);
                    return Ok(LazyEvaluationClaim {
                        session: self.session.clone(),
                        lazy,
                        owner,
                    });
                }
                Some(active) if active == owner => {
                    return Err(Arc::from(format!(
                        "lazy value {lazy} recursively observed itself"
                    )));
                }
                Some(_) => {
                    claims = self
                        .session
                        .lazy_changed
                        .wait(claims)
                        .expect("lazy evaluation claim table was poisoned");
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn install_reflection_launcher(
        &self,
        launcher: Arc<dyn ReflectionTaskLauncher>,
    ) -> Result<(), Arc<str>> {
        self.session.install_reflection_launcher(launcher)
    }

    #[allow(dead_code)] // Used once reflection exposes task spawning.
    pub(crate) fn with_new_task(&self) -> Result<Self, Arc<str>> {
        let context = Self {
            session: self.session.clone(),
            task: Arc::new(OnceLock::new()),
        };
        context.task_id()?;
        Ok(context)
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
        drop(tasks);
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

    pub(crate) fn start_joinable_reflection_task(
        &self,
        effect: Value,
    ) -> Result<EvaluationTaskHandle, Arc<str>> {
        let pending = self.reserve_reflection_task(effect)?;
        let handle = pending.handle().clone();
        pending.activate();
        Ok(handle)
    }

    pub(crate) fn start_reflection_task(
        &self,
        effect: Value,
    ) -> Result<EvaluationTaskHandle, Arc<str>> {
        if self.session.reflection_launcher.get().is_some() {
            let handle = self.reserve_task()?;
            self.activate_reflection_task(&handle, effect, ReflectionTaskKind::Annotation);
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
            },
        );
        let replaced_id = tasks.reflection_by_id.insert(id, wait.clone());
        assert!(
            replaced.is_none() && replaced_id.is_none(),
            "evaluation task identities must be unique"
        );
        Ok(EvaluationTaskHandle { id, wait })
    }

    pub(crate) fn reserve_query(&self) -> Result<PendingEvaluationQuery, Arc<str>> {
        let handle = EvaluationQueryHandle {
            id: allocate_query_id()?,
        };
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let replaced = tasks
            .queries
            .insert(handle.id, EvaluationQueryRecord { result: None });
        assert!(replaced.is_none(), "evaluation query IDs must be unique");
        drop(tasks);
        Ok(PendingEvaluationQuery {
            inner: Arc::new(PendingEvaluationQueryInner {
                context: self.clone(),
                handle,
                completed: AtomicBool::new(false),
            }),
        })
    }

    fn complete_query(&self, handle: &EvaluationQueryHandle, result: Value) {
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        let Some(query) = tasks.queries.get_mut(&handle.id) else {
            return;
        };
        if query.result.is_none() {
            query.result = Some(result);
            self.session.task_changed.notify_all();
        }
    }

    fn cancel_reserved_query(&self, handle: &EvaluationQueryHandle) {
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        if tasks
            .queries
            .get(&handle.id)
            .is_some_and(|query| query.result.is_none())
        {
            tasks.queries.remove(&handle.id);
            self.session.task_changed.notify_all();
        }
    }

    pub(crate) fn poll_query(&self, id: EvaluationQueryId) -> EvaluationQueryPoll {
        let tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        match tasks.queries.get(&id) {
            Some(EvaluationQueryRecord {
                result: Some(result),
            }) => EvaluationQueryPoll::Complete(result.clone()),
            Some(EvaluationQueryRecord { result: None }) => EvaluationQueryPoll::Pending,
            None => EvaluationQueryPoll::ForeignSession,
        }
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
        let machine = {
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
                    record.state = EvaluationTaskState::Cancelled;
                    self.session.task_changed.notify_all();
                    record.machine.take()
                }
            }
        };
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
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        tasks
            .reflection
            .get_mut(wait)
            .expect("test task must belong to this session")
            .state = EvaluationTaskState::Complete((*crate::core::keys::UNIT_VALUE).clone());
    }

    #[cfg(test)]
    pub(crate) fn fail_wait(&self, wait: &EvaluationWaitToken, error: impl Into<Arc<str>>) {
        let mut tasks = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned");
        tasks
            .reflection
            .get_mut(wait)
            .expect("test task must belong to this session")
            .state = EvaluationTaskState::Failed(error.into());
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
    pub(crate) fn evaluation_query_count(&self) -> usize {
        self.session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned")
            .queries
            .len()
    }

    #[cfg(test)]
    pub(crate) fn shares_session_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.session, &other.session)
    }
}

const TASK_POLL_QUANTUM: usize = 64;

struct ClaimedTask {
    id: EvaluationTaskId,
    wait: EvaluationWaitToken,
    prior_state: EvaluationTaskState,
    machine: Box<dyn EvaluationTaskMachine>,
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

            let poll = claimed.machine.poll(TASK_POLL_QUANTUM);
            let claimed_id = claimed.id;
            let (made_progress, remains_blocked, cancelled) = self.release_task(claimed, poll);
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
                    let (state, dependency, dependency_wait, observed_generation) = match state {
                        EvaluationTaskState::Dormant => {
                            (EvaluationUnfinishedState::Dormant, None, None, None)
                        }
                        EvaluationTaskState::Reserved => {
                            (EvaluationUnfinishedState::Reserved, None, None, None)
                        }
                        EvaluationTaskState::Queued => {
                            (EvaluationUnfinishedState::Queued, None, None, None)
                        }
                        EvaluationTaskState::Running => {
                            (EvaluationUnfinishedState::Running, None, None, None)
                        }
                        EvaluationTaskState::Blocked(block) => (
                            EvaluationUnfinishedState::Blocked,
                            block
                                .lazy
                                .as_ref()
                                .and_then(|wait| producer_for_wait(tasks, wait)),
                            block.lazy.as_ref().map(EvaluationWaitToken::get),
                            block.observed_generation,
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
            let poll = claimed.machine.poll(quantum);
            let claimed_id = claimed.id;
            let (made_progress, remains_blocked, cancelled) = self.release_task(claimed, poll);
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
    ) -> (bool, bool, Option<Box<dyn EvaluationTaskMachine>>) {
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
            return (true, false, Some(claimed.machine));
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
        self.task_changed.notify_all();
        (made_progress, remains_blocked, None)
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
        let poll = claimed.machine.poll(TASK_POLL_QUANTUM);
        let (_, _, cancelled) = self.release_task(claimed, poll);
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
        .or_else(|| tasks.promises.get(wait).map(|promise| promise.producer))
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
        let Some(task_wait) = tasks.reflection_by_id.get(&id) else {
            break;
        };
        let Some(record) = tasks.reflection.get(task_wait) else {
            break;
        };
        chain.push(id);
        let EvaluationTaskState::Blocked(block) = &record.state else {
            break;
        };
        let Some(dependency) = &block.lazy else {
            break;
        };
        wait = dependency.clone();
    }

    chain.into_iter().rev().find(|id| {
        let Some(wait) = tasks.reflection_by_id.get(id) else {
            return false;
        };
        let Some(record) = tasks.reflection.get(wait) else {
            return false;
        };
        matches!(record.state, EvaluationTaskState::Queued)
            || matches!(record.state, EvaluationTaskState::Blocked(_))
                && !attempted_blocked.contains(id)
    })
}

fn target_has_running_producer(tasks: &EvaluationTasks, target: &EvaluationWaitToken) -> bool {
    let mut seen = HashSet::new();
    let mut wait = target.clone();
    while let Some(id) = producer_for_wait(tasks, &wait) {
        if !seen.insert(id) {
            return false;
        }
        let Some(task_wait) = tasks.reflection_by_id.get(&id) else {
            return false;
        };
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
    }
    false
}

fn claim_task(tasks: &mut EvaluationTasks, id: EvaluationTaskId) -> Option<ClaimedTask> {
    let wait = tasks.reflection_by_id.get(&id)?.clone();
    let record = tasks.reflection.get_mut(&wait)?;
    if !matches!(
        record.state,
        EvaluationTaskState::Queued | EvaluationTaskState::Blocked(_)
    ) {
        return None;
    }
    let machine = record.machine.take()?;
    let prior_state = std::mem::replace(&mut record.state, EvaluationTaskState::Running);
    Some(ClaimedTask {
        id,
        wait,
        prior_state,
        machine,
    })
}

fn claim_ready_task(tasks: &mut EvaluationTasks) -> Option<ClaimedTask> {
    while let Some(id) = tasks.ready.pop_front() {
        let is_queued = tasks
            .reflection_by_id
            .get(&id)
            .and_then(|wait| tasks.reflection.get(wait))
            .is_some_and(|record| matches!(record.state, EvaluationTaskState::Queued));
        if is_queued && let Some(claimed) = claim_task(tasks, id) {
            return Some(claimed);
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
    claim_task(tasks, id)
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
    }

    #[test]
    fn zero_worker_executor_drops_sparks_without_forcing_them() {
        let executor = EvaluationExecutor::new(0).unwrap();
        let session = EvaluationSession::shared(Value::Dict(Dict::new_sync()), &executor);
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
        let session = EvaluationSession::shared(Value::Dict(Dict::new_sync()), &executor);
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
