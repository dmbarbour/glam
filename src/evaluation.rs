//! Session-scoped capabilities threaded through semantic evaluation.
//!
//! The session currently owns the registry and identity of reflection work.
//! The cooperative executor, heap, diagnostics, and cancellation facilities
//! will join that boundary in later slices.

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::core::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    Complete,
    Failed(Arc<str>),
    Cancelled,
    ForeignSession,
}

#[allow(dead_code)] // The executor will exercise post-queued states in the next slice.
#[derive(Debug)]
enum EvaluationTaskState {
    Queued,
    Running,
    Blocked,
    Complete,
    Failed(Arc<str>),
    Cancelled,
}

#[derive(Debug)]
struct ReflectionTaskRecord {
    #[allow(dead_code)] // Used when the cooperative executor enters the task.
    id: EvaluationTaskId,
    #[allow(dead_code)] // Retained for the upcoming reflection executor.
    effect: Value,
    state: EvaluationTaskState,
}

#[derive(Debug)]
struct PromiseRecord {
    result: Weak<OnceLock<Result<Value, Arc<str>>>>,
}

#[derive(Debug, Default)]
struct EvaluationTasks {
    reflection: HashMap<EvaluationWaitToken, ReflectionTaskRecord>,
    promises: HashMap<EvaluationWaitToken, PromiseRecord>,
    owned_promises: HashMap<EvaluationTaskId, Vec<EvaluationWaitToken>>,
}

#[derive(Debug, Default)]
pub(crate) struct EvaluationSession {
    tasks: Mutex<EvaluationTasks>,
}

impl EvaluationSession {
    pub(crate) fn new() -> Self {
        Self::default()
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

    /// Creates a session for internal clients that do not yet run under an
    /// assembler, notably standalone reflection tasks and focused tests.
    pub(crate) fn standalone() -> Self {
        Self::new(Arc::new(EvaluationSession::new()))
    }

    #[allow(dead_code)] // Used by spawned work once the cooperative executor is connected.
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

    pub(crate) fn start_reflection_task(
        &self,
        effect: Value,
    ) -> Result<EvaluationTaskHandle, Arc<str>> {
        let id = allocate_task_id()?;
        let wait = allocate_wait_token(&self.session)?;
        let replaced = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned")
            .reflection
            .insert(
                wait.clone(),
                ReflectionTaskRecord {
                    id,
                    effect,
                    state: EvaluationTaskState::Queued,
                },
            );
        assert!(replaced.is_none(), "evaluation task IDs must be unique");
        Ok(EvaluationTaskHandle { id, wait })
    }

    pub(crate) fn poll_reflection_task(&self, task: &EvaluationTaskHandle) -> EvaluationTaskPoll {
        self.poll_wait(&task.wait)
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
                Some(Ok(_)) => EvaluationTaskPoll::Complete,
                Some(Err(error)) => EvaluationTaskPoll::Failed(error.clone()),
                None if Arc::ptr_eq(&self.session, &owner) => {
                    EvaluationTaskPoll::Pending(wait.clone())
                }
                None => EvaluationTaskPoll::ForeignSession,
            };
        };
        match &record.state {
            EvaluationTaskState::Complete => EvaluationTaskPoll::Complete,
            EvaluationTaskState::Failed(error) => EvaluationTaskPoll::Failed(error.clone()),
            EvaluationTaskState::Cancelled => EvaluationTaskPoll::Cancelled,
            EvaluationTaskState::Queued
            | EvaluationTaskState::Running
            | EvaluationTaskState::Blocked => {
                if Arc::ptr_eq(&self.session, &owner) {
                    EvaluationTaskPoll::Pending(wait.clone())
                } else {
                    EvaluationTaskPoll::ForeignSession
                }
            }
        }
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
            .state = EvaluationTaskState::Complete;
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
