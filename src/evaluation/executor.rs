//! Worker ownership and fair selection across one runtime's ready work.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;

use super::coordinator::{CoordinatorSelection, SparkWorkPoll, WorkDependency};
use super::{EvalContext, EvaluationWorkCoordinator};

struct EvaluationExecutorInner {
    coordinator: Weak<EvaluationWorkCoordinator>,
    stopping: AtomicBool,
    worker_count: AtomicUsize,
}

/// Background worker resources attached to one evaluation runtime.
///
/// Stable work records and spark payloads belong to the runtime coordinator.
/// The executor owns only worker activation, shutdown, and thread handles.
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
    pub(crate) fn new(
        worker_count: usize,
        coordinator: &Arc<EvaluationWorkCoordinator>,
    ) -> Result<Arc<Self>, Arc<str>> {
        if worker_count > MAX_EVALUATION_WORKERS {
            return Err(Arc::from(format!(
                "worker count {worker_count} exceeds the supported maximum of {MAX_EVALUATION_WORKERS}"
            )));
        }
        let executor = Arc::new(Self {
            inner: Arc::new(EvaluationExecutorInner {
                coordinator: Arc::downgrade(coordinator),
                stopping: AtomicBool::new(false),
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
        if let Some(coordinator) = self.inner.coordinator.upgrade() {
            coordinator.executor_started(worker_count);
        }
        drop(workers);
        Ok(())
    }

    pub(crate) fn worker_count(&self) -> usize {
        self.inner.worker_count.load(Ordering::Acquire)
    }
}

impl Drop for EvaluationExecutor {
    fn drop(&mut self) {
        self.inner.stopping.store(true, Ordering::Release);
        self.inner.worker_count.store(0, Ordering::Release);
        if let Some(coordinator) = self.inner.coordinator.upgrade() {
            coordinator.executor_stopped();
        }

        // Dropping a JoinHandle detaches its thread. Idle workers observe the
        // coordinator wake and exit promptly. A divergent worker retains its
        // claimed record until it returns, preserving truthful busy state.
        self.workers
            .get_mut()
            .expect("evaluation worker registry was poisoned")
            .clear();
    }
}

fn evaluation_worker(inner: Arc<EvaluationExecutorInner>) {
    loop {
        if inner.stopping.load(Ordering::Acquire) {
            return;
        }
        let Some(coordinator) = inner.coordinator.upgrade() else {
            return;
        };
        let observed_generation = coordinator.work_generation();
        let work = coordinator.select();

        match work {
            CoordinatorSelection::Task(work) => {
                if inner.stopping.load(Ordering::Acquire) {
                    coordinator.requeue_unpolled_task(work);
                    return;
                }
                coordinator.poll_claimed_task(work);
            }
            CoordinatorSelection::Spark(claimed) => {
                if inner.stopping.load(Ordering::Acquire) {
                    coordinator.release_spark(claimed, SparkWorkPoll::Complete);
                    return;
                }
                claimed.assert_runtime(coordinator.runtime_id());
                let context = EvalContext::for_spark(claimed.demand_session());
                let result =
                    crate::eval::demand_strategy_value(&context, claimed.value().as_core());
                let poll = match result {
                    Ok(()) => SparkWorkPoll::Complete,
                    Err(halt) => {
                        if let Some(wait) = halt.blocked_on() {
                            SparkWorkPoll::Blocked(WorkDependency::Wait(wait.0))
                        } else if let Some(promise) = halt.unassigned_promise() {
                            SparkWorkPoll::Blocked(WorkDependency::Promise(promise.clone()))
                        } else {
                            SparkWorkPoll::Complete
                        }
                    }
                };
                drop(context);
                coordinator.release_spark(claimed, poll);
            }
            CoordinatorSelection::ClientDemand(claimed) => {
                if inner.stopping.load(Ordering::Acquire) {
                    coordinator.requeue_unpolled_client_demand(claimed);
                    return;
                }
                coordinator.poll_claimed_client_demand(claimed);
            }
            CoordinatorSelection::None => {
                if inner.stopping.load(Ordering::Acquire) {
                    return;
                }
                coordinator.wait_for_change(observed_generation);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executor_shutdown_wakes_idle_workers_without_owning_the_coordinator() {
        let (coordinator, executor) =
            super::super::test_execution_resources(1).expect("test executor should build");
        let worker_lease = Arc::downgrade(&executor.inner);

        drop(executor);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while worker_lease.upgrade().is_some() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }

        assert!(
            worker_lease.upgrade().is_none(),
            "an idle worker should observe executor shutdown and release its resources"
        );
        assert_eq!(
            Arc::strong_count(&coordinator),
            1,
            "executor workers must retain only a weak coordinator attachment"
        );
    }
}
