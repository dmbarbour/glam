//! Worker ownership and fair selection across one runtime's ready work.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread;

use super::EvaluationWorkCoordinator;
use super::coordinator::{
    CoordinatorSelection, CoordinatorWaiterClass, CoordinatorWaiterOutcome, SparkWorkPoll,
};

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
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    workers: Mutex<Vec<thread::JoinHandle<()>>>,
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
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
            .unwrap_or_else(PoisonError::into_inner);
        if *activated {
            return Err(Arc::from("evaluation workers were already activated"));
        }
        if worker_count == 0 {
            *activated = true;
            return Ok(());
        }

        // Activation is transactional. Every worker is prepared behind a
        // start gate before any may claim work. If one fails to start, the
        // prepared workers exit and are joined, and activation stays
        // retryable. Only a complete set is published and released.
        let gate = Arc::new(WorkerStartGate::default());
        let mut prepared = Vec::with_capacity(worker_count);
        for index in 0..worker_count {
            let inner = self.inner.clone();
            let worker_gate = gate.clone();
            let spawned = spawn_worker(index, move || {
                if worker_gate.wait_for_release() {
                    evaluation_worker(inner);
                }
            });
            match spawned {
                Ok(worker) => prepared.push(worker),
                Err(error) => {
                    gate.open(false);
                    for worker in prepared {
                        let _ = worker.join();
                    }
                    return Err(Arc::from(format!(
                        "could not start evaluation worker {index}: {error}"
                    )));
                }
            }
        }

        *activated = true;
        self.workers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(prepared);
        self.inner
            .worker_count
            .store(worker_count, Ordering::Release);
        if let Some(coordinator) = self.inner.coordinator.upgrade() {
            coordinator.executor_started(worker_count);
        }
        gate.open(true);
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
            if coordinator.runtime_poisoned() {
                // The scheduler state is torn: wake parked workers directly so
                // they observe `stopping` and exit.
                coordinator.wake_parked_workers();
            } else {
                coordinator.executor_stopped();
            }
        }

        // Dropping a JoinHandle detaches its thread. Idle workers observe the
        // coordinator wake and exit promptly. A divergent worker retains its
        // claimed record until it returns, preserving truthful busy state.
        self.workers
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

fn evaluation_worker(inner: Arc<EvaluationExecutorInner>) {
    let _thread_cache_retirement = WorkerThreadCacheRetirement;
    // Every poll runs behind its own panic boundary, so a panic that reaches
    // here came from scheduler code. That tears runtime-core state: the
    // runtime is poisoned and this worker exits.
    let run = catch_unwind(AssertUnwindSafe(|| run_evaluation_worker(&inner)));
    if run.is_err()
        && let Some(coordinator) = inner.coordinator.upgrade()
    {
        coordinator.mark_runtime_poisoned();
    }
}

fn run_evaluation_worker(inner: &EvaluationExecutorInner) {
    let mut released_from_wait = false;
    loop {
        if inner.stopping.load(Ordering::Acquire) {
            return;
        }
        let Some(coordinator) = inner.coordinator.upgrade() else {
            return;
        };
        if coordinator.runtime_poison_marked() {
            return;
        }
        let observed_generation = coordinator.work_generation();
        let work = coordinator.select_worker();
        if released_from_wait {
            coordinator.record_waiter_outcome(
                CoordinatorWaiterClass::Worker,
                if matches!(work, CoordinatorSelection::None) {
                    CoordinatorWaiterOutcome::Unrelated
                } else {
                    CoordinatorWaiterOutcome::Productive
                },
            );
            released_from_wait = false;
        }

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
                coordinator.poll_claimed_spark(claimed);
            }
            CoordinatorSelection::None => {
                if inner.stopping.load(Ordering::Acquire) {
                    return;
                }
                released_from_wait = coordinator.wait_for_change_for(
                    observed_generation,
                    None,
                    CoordinatorWaiterClass::Worker,
                );
            }
        }
    }
}

/// Holds prepared workers until activation either publishes the complete set
/// or abandons it.
#[derive(Default)]
struct WorkerStartGate {
    /// `None` while pending; `Some(true)` releases, `Some(false)` aborts.
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    decision: Mutex<Option<bool>>,
    decided: crate::counted_condvar::CountedCondvar,
}

impl WorkerStartGate {
    fn open(&self, release: bool) {
        *self.decision.lock().unwrap_or_else(PoisonError::into_inner) = Some(release);
        self.decided.notify_all();
    }

    /// Waits for the decision and returns whether the worker may run.
    fn wait_for_release(&self) -> bool {
        let mut decision = self.decision.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(release) = *decision {
                return release;
            }
            decision = self
                .decided
                .wait(decision)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

fn spawn_worker(
    index: usize,
    run: impl FnOnce() + Send + 'static,
) -> std::io::Result<thread::JoinHandle<()>> {
    #[cfg(test)]
    if FAIL_WORKER_SPAWN_AT.with(|fail| fail.get()) == Some(index) {
        return Err(std::io::Error::other("injected worker spawn failure"));
    }
    thread::Builder::new()
        .name(format!("glam-eval-{index}"))
        .spawn(run)
}

#[cfg(test)]
thread_local! {
    /// Injects a spawn failure at one worker index on the activating thread.
    static FAIL_WORKER_SPAWN_AT: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// Retires inactive per-heap allocation cursors when one worker thread ends.
///
/// Ordinary evaluation quantum boundaries deliberately preserve these
/// reusable cursors. Worker termination is the stronger lifecycle boundary:
/// no later quantum can reuse the calling thread's collector state, and the
/// collector recovers any forgotten leases during a full collection.
struct WorkerThreadCacheRetirement;

impl Drop for WorkerThreadCacheRetirement {
    fn drop(&mut self) {
        let _released = glam_gc::Heap::release_current_thread_caches();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_termination_releases_inactive_collector_caches() {
        let _ = glam_gc::Heap::release_current_thread_caches();
        let values = crate::core::CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        );
        values.with_runtime_value_access(|_| {});

        let (_coordinator, executor) =
            super::super::test_execution_resources(0).expect("test executor should build");
        executor.inner.stopping.store(true, Ordering::Release);
        evaluation_worker(executor.inner.clone());

        assert_eq!(
            glam_gc::Heap::release_current_thread_caches(),
            0,
            "worker termination should have retired every inactive collector cache"
        );
    }

    #[test]
    fn failed_worker_spawn_retires_prepared_workers_and_stays_retryable() {
        let (_coordinator, executor) =
            super::super::test_execution_resources(0).expect("test executor should build");

        FAIL_WORKER_SPAWN_AT.with(|fail| fail.set(Some(1)));
        let error = executor
            .activate_workers(3)
            .expect_err("an injected spawn failure must fail activation");
        FAIL_WORKER_SPAWN_AT.with(|fail| fail.set(None));
        assert!(error.contains("could not start evaluation worker 1"));
        // The prepared worker 0 was joined rather than left running, and
        // nothing was published.
        assert_eq!(executor.worker_count(), 0);
        assert!(
            executor
                .workers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );

        executor
            .activate_workers(2)
            .expect("a failed activation must remain retryable");
        assert_eq!(executor.worker_count(), 2);
        assert!(
            executor.activate_workers(1).is_err(),
            "a successful activation is not repeated"
        );
    }

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
