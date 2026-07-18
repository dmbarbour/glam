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
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::core::Value;

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

fn allocate_task_id() -> Result<EvaluationTaskId, Arc<str>> {
    allocate_id(&NEXT_TASK_ID, "evaluation task IDs exhausted").map(EvaluationTaskId)
}

fn allocate_wait_token(session: &Arc<EvaluationSession>) -> Result<EvaluationWaitToken, Arc<str>> {
    Ok(EvaluationWaitToken {
        id: allocate_id(&NEXT_WAIT_ID, "evaluation wait-token IDs exhausted")?,
        owner: Arc::downgrade(session),
    })
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

#[derive(Default)]
struct EvaluationTasks {
    reflection: HashMap<EvaluationWaitToken, ReflectionTaskRecord>,
    reflection_by_id: BTreeMap<EvaluationTaskId, EvaluationWaitToken>,
    ready: VecDeque<EvaluationTaskId>,
    promises: HashMap<EvaluationWaitToken, PromiseRecord>,
    owned_promises: HashMap<EvaluationTaskId, Vec<EvaluationWaitToken>>,
}

#[derive(Default)]
pub(crate) struct EvaluationSession {
    tasks: Mutex<EvaluationTasks>,
    reflection_launcher: OnceLock<Arc<dyn ReflectionTaskLauncher>>,
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

    pub(crate) fn install_reflection_launcher(
        &self,
        launcher: Arc<dyn ReflectionTaskLauncher>,
    ) -> Result<(), Arc<str>> {
        self.reflection_launcher
            .set(launcher)
            .map_err(|_| Arc::from("evaluation session already has a reflection task launcher"))
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

    /// Gives queued and coarsely blocked reflection tasks a bounded amount of
    /// cooperative execution without waiting for any particular result.
    pub(crate) fn pump_background(&self, step_budget: usize) {
        self.session.pump_background(step_budget);
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
    fn pump_background(&self, mut step_budget: usize) {
        let mut attempted_blocked = HashSet::new();
        while step_budget > 0 {
            let claimed = {
                let mut tasks = self
                    .tasks
                    .lock()
                    .expect("evaluation task registry was poisoned");
                claim_ready_task(&mut tasks)
                    .or_else(|| claim_blocked_task(&mut tasks, &attempted_blocked))
            };
            let Some(mut claimed) = claimed else {
                break;
            };

            let quantum = step_budget.min(TASK_POLL_QUANTUM);
            step_budget -= quantum;
            let poll = claimed.machine.poll(quantum);
            let claimed_id = claimed.id;
            let (made_progress, remains_blocked, cancelled) = self.release_task(claimed, poll);
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
                return EvaluationPumpOutcome::NoProgress;
            };

            let quantum = step_budget.min(TASK_POLL_QUANTUM);
            step_budget -= quantum;
            let poll = claimed.machine.poll(quantum);
            let claimed_id = claimed.id;
            let (made_progress, remains_blocked, cancelled) = self.release_task(claimed, poll);
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
        (made_progress, remains_blocked, None)
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
}
