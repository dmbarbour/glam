//! Worker ownership and fair selection across related evaluation sessions.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;

use crate::core::Value;

use super::{EvalContext, EvaluationSession};

struct SparkJob {
    session_id: u64,
    session: Weak<EvaluationSession>,
    value: Value,
    observed_generation: u64,
}

#[derive(Default)]
struct ExecutorQueue {
    stopping: bool,
    sessions: HashMap<u64, Weak<EvaluationSession>>,
    ready_sessions: VecDeque<u64>,
    ready_session_set: HashSet<u64>,
    sparks: VecDeque<SparkJob>,
    blocked_sparks: HashMap<u64, Vec<SparkJob>>,
    spark_generations: HashMap<u64, u64>,
    prefer_spark: bool,
}

struct EvaluationExecutorInner {
    queue: Mutex<ExecutorQueue>,
    work_available: Condvar,
    worker_count: AtomicUsize,
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
    activated: Mutex<bool>,
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
                worker_count: AtomicUsize::new(0),
            }),
            workers: Mutex::new(Vec::with_capacity(worker_count)),
            activated: Mutex::new(false),
        });
        if worker_count != 0 {
            executor.activate_workers(worker_count)?;
        }

        Ok(executor)
    }

    pub(crate) fn activate_workers(self: &Arc<Self>, worker_count: usize) -> Result<(), Arc<str>> {
        if worker_count > MAX_EVALUATION_WORKERS {
            return Err(Arc::from(format!(
                "worker count {worker_count} exceeds the supported maximum of {MAX_EVALUATION_WORKERS}"
            )));
        }
        let mut activated = self
            .activated
            .lock()
            .expect("evaluation worker activation mutex was poisoned");
        if *activated {
            return Err(Arc::from("evaluation workers were already activated"));
        }
        *activated = true;
        if worker_count == 0 {
            return Ok(());
        }

        let mut workers = self
            .workers
            .lock()
            .expect("evaluation worker registry was poisoned");
        for index in 0..worker_count {
            let inner = self.inner.clone();
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
        self.inner
            .worker_count
            .store(worker_count, Ordering::Release);
        drop(workers);
        Ok(())
    }

    pub(crate) fn worker_count(&self) -> usize {
        self.inner.worker_count.load(Ordering::Acquire)
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
        queue.spark_generations.entry(session.id.get()).or_insert(0);
    }

    pub(super) fn unregister_session(&self, session: u64) {
        let mut queue = self
            .inner
            .queue
            .lock()
            .expect("evaluation executor queue was poisoned");
        queue.sessions.remove(&session);
        queue.ready_session_set.remove(&session);
        queue
            .ready_sessions
            .retain(|candidate| *candidate != session);
        queue.sparks.retain(|job| job.session_id != session);
        queue.blocked_sparks.remove(&session);
        queue.spark_generations.remove(&session);
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
        let session_id = session.id.get();
        if !queue.sessions.contains_key(&session_id) {
            return;
        }
        let observed_generation = *queue.spark_generations.entry(session_id).or_insert(0);
        queue.sparks.push_back(SparkJob {
            session_id,
            session: Arc::downgrade(session),
            value,
            observed_generation,
        });
        self.inner.work_available.notify_one();
    }

    pub(super) fn notify_spark_disturbance(&self, session: u64) {
        let mut queue = self
            .inner
            .queue
            .lock()
            .expect("evaluation executor queue was poisoned");
        if queue.stopping || !queue.sessions.contains_key(&session) {
            return;
        }
        let generation = queue
            .spark_generations
            .entry(session)
            .and_modify(|generation| *generation = generation.wrapping_add(1))
            .or_insert(1);
        let generation = *generation;
        let Some(blocked) = queue.blocked_sparks.remove(&session) else {
            return;
        };
        for mut job in blocked {
            job.observed_generation = generation;
            queue.sparks.push_back(job);
        }
        self.inner.work_available.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn blocked_spark_count(&self) -> usize {
        self.inner
            .queue
            .lock()
            .expect("evaluation executor queue was poisoned")
            .blocked_sparks
            .values()
            .map(Vec::len)
            .sum()
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
        queue.blocked_sparks.clear();
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
                let result = crate::eval::demand_strategy_value(&context, &job.value);
                if result.as_ref().is_err_and(|halt| {
                    halt.blocked_on().is_some() || halt.unassigned_promise().is_some()
                }) {
                    park_spark(&inner, job);
                }
            }
            ExecutorWork::Stop => return,
        }
    }
}

fn park_spark(inner: &EvaluationExecutorInner, mut job: SparkJob) {
    let mut queue = inner
        .queue
        .lock()
        .expect("evaluation executor queue was poisoned");
    if queue.stopping || !queue.sessions.contains_key(&job.session_id) {
        return;
    }
    let generation = *queue.spark_generations.entry(job.session_id).or_insert(0);
    if generation != job.observed_generation {
        job.observed_generation = generation;
        queue.sparks.push_back(job);
        inner.work_available.notify_one();
    } else {
        queue
            .blocked_sparks
            .entry(job.session_id)
            .or_default()
            .push(job);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disturbance_racing_with_spark_parking_is_not_lost() {
        let executor = EvaluationExecutor::new(0).expect("test executor should build");
        let session = EvaluationSession::shared(&executor);
        let session_id = session.id.get();
        let job = SparkJob {
            session_id,
            session: Arc::downgrade(&session),
            value: crate::core::keys::unit_value(),
            observed_generation: 0,
        };

        executor.notify_spark_disturbance(session_id);
        park_spark(&executor.inner, job);

        let queue = executor
            .inner
            .queue
            .lock()
            .expect("evaluation executor queue was poisoned");
        assert_eq!(queue.sparks.len(), 1);
        assert!(queue.blocked_sparks.is_empty());
        assert_eq!(queue.sparks[0].observed_generation, 1);
    }
}
