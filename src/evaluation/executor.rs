//! Worker ownership and fair selection across related evaluation sessions.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;

use crate::core::Value;

use super::{EvalContext, EvaluationSession};

struct SparkJob {
    session: Weak<EvaluationSession>,
    value: Value,
}

#[derive(Default)]
struct ExecutorQueue {
    stopping: bool,
    sessions: HashMap<u64, Weak<EvaluationSession>>,
    ready_sessions: VecDeque<u64>,
    ready_session_set: HashSet<u64>,
    sparks: VecDeque<SparkJob>,
    prefer_spark: bool,
}

struct EvaluationExecutorInner {
    queue: Mutex<ExecutorQueue>,
    work_available: Condvar,
    worker_count: usize,
}

/// Shared background execution resources for one assembler runtime.
///
/// Sessions retain only a weak reference to the executor. Worker threads own
/// the queue state but not this handle, allowing the last runtime owner to
/// signal shutdown even when a spark diverges. Running divergent sparks are
/// intentionally not forcibly cancelled.
pub(crate) struct EvaluationExecutor {
    inner: Arc<EvaluationExecutorInner>,
    workers: Mutex<Vec<thread::JoinHandle<()>>>,
}

const MAX_EVALUATION_WORKERS: usize = 256;

impl fmt::Debug for EvaluationExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationExecutor")
            .field("worker_count", &self.worker_count())
            .finish_non_exhaustive()
    }
}

impl EvaluationExecutor {
    pub(crate) fn new(worker_count: usize) -> Result<Arc<Self>, Arc<str>> {
        if worker_count > MAX_EVALUATION_WORKERS {
            return Err(Arc::from(format!(
                "worker count {worker_count} exceeds the supported maximum of {MAX_EVALUATION_WORKERS}"
            )));
        }
        let executor = Arc::new(Self {
            inner: Arc::new(EvaluationExecutorInner {
                queue: Mutex::new(ExecutorQueue::default()),
                work_available: Condvar::new(),
                worker_count,
            }),
            workers: Mutex::new(Vec::with_capacity(worker_count)),
        });
        if worker_count == 0 {
            return Ok(executor);
        }

        let mut workers = executor
            .workers
            .lock()
            .expect("evaluation worker registry was poisoned");
        for index in 0..worker_count {
            let inner = executor.inner.clone();
            let worker = thread::Builder::new()
                .name(format!("glam-eval-{index}"))
                .spawn(move || evaluation_worker(inner))
                .map_err(|error| {
                    Arc::<str>::from(format!(
                        "could not start evaluation worker {index}: {error}"
                    ))
                })?;
            workers.push(worker);
        }
        drop(workers);
        Ok(executor)
    }

    pub(crate) fn worker_count(&self) -> usize {
        self.inner.worker_count
    }

    pub(super) fn register_session(&self, session: &Arc<EvaluationSession>) {
        let mut queue = self
            .inner
            .queue
            .lock()
            .expect("evaluation executor queue was poisoned");
        queue
            .sessions
            .insert(session.id.get(), Arc::downgrade(session));
    }

    pub(super) fn notify_session_ready(&self, session: u64) {
        let mut queue = self
            .inner
            .queue
            .lock()
            .expect("evaluation executor queue was poisoned");
        if queue.stopping || !queue.sessions.contains_key(&session) {
            return;
        }
        if queue.ready_session_set.insert(session) {
            queue.ready_sessions.push_back(session);
            self.inner.work_available.notify_one();
        }
    }

    pub(super) fn submit_spark(&self, session: &Arc<EvaluationSession>, value: Value) {
        if self.worker_count() == 0 {
            return;
        }
        let mut queue = self
            .inner
            .queue
            .lock()
            .expect("evaluation executor queue was poisoned");
        if queue.stopping {
            return;
        }
        queue.sparks.push_back(SparkJob {
            session: Arc::downgrade(session),
            value,
        });
        self.inner.work_available.notify_one();
    }
}

impl Drop for EvaluationExecutor {
    fn drop(&mut self) {
        let mut queue = self
            .inner
            .queue
            .lock()
            .expect("evaluation executor queue was poisoned");
        queue.stopping = true;
        queue.sparks.clear();
        self.inner.work_available.notify_all();
        drop(queue);

        // Dropping a JoinHandle detaches its thread. Idle workers observe the
        // stop flag and exit promptly; an actively divergent spark retains
        // only executor internals until the process terminates or it returns.
        self.workers
            .get_mut()
            .expect("evaluation worker registry was poisoned")
            .clear();
    }
}

enum ExecutorWork {
    Reflection(Arc<EvaluationSession>),
    Spark(SparkJob),
    Stop,
}

fn evaluation_worker(inner: Arc<EvaluationExecutorInner>) {
    loop {
        let work = {
            let mut queue = inner
                .queue
                .lock()
                .expect("evaluation executor queue was poisoned");
            'select: loop {
                if queue.prefer_spark
                    && let Some(spark) = queue.sparks.pop_front()
                {
                    queue.prefer_spark = false;
                    break 'select ExecutorWork::Spark(spark);
                }
                if let Some(session) = pop_ready_session(&mut queue) {
                    queue.prefer_spark = true;
                    break 'select ExecutorWork::Reflection(session);
                }
                if let Some(spark) = queue.sparks.pop_front() {
                    queue.prefer_spark = false;
                    break ExecutorWork::Spark(spark);
                }
                if queue.stopping {
                    break ExecutorWork::Stop;
                }
                queue = inner
                    .work_available
                    .wait(queue)
                    .expect("evaluation executor queue was poisoned");
            }
        };

        match work {
            ExecutorWork::Reflection(session) => {
                session.poll_one_ready_task();
            }
            ExecutorWork::Spark(job) => {
                let Some(session) = job.session.upgrade() else {
                    continue;
                };
                let context = EvalContext::new(session);
                let _ = crate::eval::eval_value(&context, &job.value);
            }
            ExecutorWork::Stop => return,
        }
    }
}

fn pop_ready_session(queue: &mut ExecutorQueue) -> Option<Arc<EvaluationSession>> {
    while let Some(session_id) = queue.ready_sessions.pop_front() {
        queue.ready_session_set.remove(&session_id);
        let session = queue.sessions.get(&session_id).and_then(Weak::upgrade);
        if session.is_some() {
            return session;
        }
        queue.sessions.remove(&session_id);
    }
    None
}
