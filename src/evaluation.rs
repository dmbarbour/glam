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
use std::sync::{Arc, Mutex, Weak};

use crate::core::Value;

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
    wait: EvaluationWaitToken,
}

impl fmt::Debug for EvaluationTaskHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationTaskHandle")
            .field("task", &self.wait.get())
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
    #[allow(dead_code)] // Retained for the upcoming reflection executor.
    effect: Value,
    state: EvaluationTaskState,
}

#[derive(Debug, Default)]
struct EvaluationTasks {
    reflection: HashMap<EvaluationWaitToken, ReflectionTaskRecord>,
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
}

impl EvalContext {
    pub(crate) fn new(session: Arc<EvaluationSession>) -> Self {
        Self { session }
    }

    /// Creates a session for internal clients that do not yet run under an
    /// assembler, notably standalone reflection tasks and focused tests.
    pub(crate) fn standalone() -> Self {
        Self::new(Arc::new(EvaluationSession::new()))
    }

    pub(crate) fn start_reflection_task(
        &self,
        effect: Value,
    ) -> Result<EvaluationTaskHandle, Arc<str>> {
        static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

        let id = NEXT_TASK_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| Arc::<str>::from("evaluation task IDs exhausted"))?;
        let wait = EvaluationWaitToken {
            id: NonZeroU64::new(id).expect("evaluation task IDs start at one"),
            owner: Arc::downgrade(&self.session),
        };
        let replaced = self
            .session
            .tasks
            .lock()
            .expect("evaluation task registry was poisoned")
            .reflection
            .insert(
                wait.clone(),
                ReflectionTaskRecord {
                    effect,
                    state: EvaluationTaskState::Queued,
                },
            );
        assert!(replaced.is_none(), "evaluation task IDs must be unique");
        Ok(EvaluationTaskHandle { wait })
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
            return EvaluationTaskPoll::Failed(Arc::from(
                "reflection task is no longer registered",
            ));
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
