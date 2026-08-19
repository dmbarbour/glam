//! Exact subscriptions to one-shot runtime completion sources.

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, Weak};

use crate::core::PromiseId;
use crate::runtime::{EvaluationRuntimeId, RuntimeMutationAuthority};

use super::{
    EvaluationWorkCoordinator, EvaluationWorkId, WorkDependency, queue_current_registration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct WakeRegistration {
    pub(super) work: EvaluationWorkId,
    pub(super) subscription_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WorkDependencyKey {
    Wait(u64),
    Promise(u64),
    #[cfg(test)]
    Test(u64),
}

pub(super) struct DependencyWakeBatch {
    pub(super) source: WorkDependencyKey,
    pub(super) registrations: Vec<WakeRegistration>,
}

/// Weak, epoch-tagged registrations retained by a one-shot completion source.
///
/// The terminal state remains owned by the source. This component only pairs
/// subscribe-and-recheck with detached coordinator delivery, and therefore
/// cannot retain the runtime or any work record.
pub(crate) struct CompletionSubscriptions {
    pub(super) runtime: EvaluationRuntimeId,
    pub(super) source: WorkDependencyKey,
    pub(super) coordinator: Arc<Mutex<Weak<EvaluationWorkCoordinator>>>,
    registrations: Mutex<Vec<WakeRegistration>>,
}

/// Scheduler notification detached from an authoritative completion
/// publication.
///
/// The coordinator transition has already happened while runtime mutation
/// admission was held. Keeping the notification separate lets callers release
/// that admission before waking scheduler threads.
#[must_use = "scheduler wakes must be delivered after mutation admission is released"]
pub(crate) struct CompletionWake {
    coordinator: Arc<EvaluationWorkCoordinator>,
    changed: bool,
}

impl CompletionWake {
    pub(crate) fn notify(self) {
        self.coordinator.notify_dependency_wake(self.changed);
    }
}

impl CompletionSubscriptions {
    pub(crate) fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        let coordinator = self
            .coordinator
            .lock()
            .expect("runtime work-coordinator binding was poisoned")
            .clone();
        coordinator
            .upgrade()
            .filter(|coordinator| coordinator.runtime == self.runtime)
    }

    pub(crate) fn for_promise(
        runtime: EvaluationRuntimeId,
        promise: PromiseId,
        coordinator: Arc<Mutex<Weak<EvaluationWorkCoordinator>>>,
    ) -> Self {
        Self {
            runtime,
            source: WorkDependencyKey::Promise(promise.get()),
            coordinator,
            registrations: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn for_wait(
        runtime: EvaluationRuntimeId,
        wait: NonZeroU64,
        coordinator: Arc<Mutex<Weak<EvaluationWorkCoordinator>>>,
    ) -> Self {
        Self {
            runtime,
            source: WorkDependencyKey::Wait(wait.get()),
            coordinator,
            registrations: Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    pub(super) fn for_test(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        source: WorkDependencyKey,
    ) -> Self {
        let binding = Arc::new(Mutex::new(Arc::downgrade(coordinator)));
        Self {
            runtime: coordinator.runtime,
            source,
            coordinator: binding,
            registrations: Mutex::new(Vec::new()),
        }
    }

    /// Publishes a source terminal while holding shared runtime mutation
    /// admission, then detaches and delivers every exact wake registration.
    /// External/session wakes returned by `publish_terminal` remain the
    /// caller's responsibility and must run after this method returns.
    pub(crate) fn publish<T, E>(
        &self,
        publish_terminal: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let coordinator = self.coordinator();
        let Some(coordinator) = coordinator else {
            let result = publish_terminal()?;
            self.registrations
                .lock()
                .expect("completion subscriber set was poisoned")
                .clear();
            return Ok(result);
        };

        let mutation = coordinator.admission.mutation_guard();
        let (result, wake) = self.publish_guarded(&coordinator, &mutation, publish_terminal)?;
        drop(mutation);
        wake.notify();
        Ok(result)
    }

    /// Publishes a terminal using mutation admission already held by the
    /// caller.
    ///
    /// The terminal closure and every resulting coordinator transition become
    /// authoritative before this returns. The returned wake must be delivered
    /// only after the caller releases component locks and mutation admission.
    pub(crate) fn publish_guarded<T, E>(
        &self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        publish_terminal: impl FnOnce() -> Result<T, E>,
    ) -> Result<(T, CompletionWake), E> {
        debug_assert_eq!(coordinator.runtime, self.runtime);

        let result = publish_terminal()?;
        let registrations = std::mem::take(
            &mut *self
                .registrations
                .lock()
                .expect("completion subscriber set was poisoned"),
        );
        let changed = coordinator.wake_dependency_batch_guarded(
            mutation,
            DependencyWakeBatch {
                source: self.source,
                registrations,
            },
        );
        Ok((
            result,
            CompletionWake {
                coordinator: coordinator.clone(),
                changed,
            },
        ))
    }

    /// Detaches and delivers registrations for a terminal which was already
    /// published under its producer registry lock.
    ///
    /// Locally driven waits use this split because they have no coordinator
    /// work record. Coordinator-owned terminals detach and wake registrations
    /// under coordinator mutation admission. The terminal cell is immutable
    /// before this call, so subscribe-and-recheck still cannot lose a wake.
    pub(crate) fn notify_published(&self) {
        self.publish(|| Ok::<_, std::convert::Infallible>(()))
            .expect("published completion notification is infallible");
    }

    pub(crate) fn subscribe(
        &self,
        runtime: EvaluationRuntimeId,
        registration: WakeRegistration,
        terminal: impl FnOnce() -> bool,
    ) -> CompletionSubscriptionOutcome {
        self.subscribe_with_impl(runtime, registration, terminal, || {})
    }

    #[cfg(test)]
    pub(super) fn subscribe_with(
        &self,
        runtime: EvaluationRuntimeId,
        registration: WakeRegistration,
        terminal: impl FnOnce() -> bool,
        before_insert: impl FnOnce(),
    ) -> CompletionSubscriptionOutcome {
        self.subscribe_with_impl(runtime, registration, terminal, before_insert)
    }

    fn subscribe_with_impl(
        &self,
        runtime: EvaluationRuntimeId,
        registration: WakeRegistration,
        terminal: impl FnOnce() -> bool,
        before_insert: impl FnOnce(),
    ) -> CompletionSubscriptionOutcome {
        if runtime != self.runtime {
            return CompletionSubscriptionOutcome::ForeignRuntime;
        }
        let mut registrations = self
            .registrations
            .lock()
            .expect("completion subscriber set was poisoned");
        if terminal() {
            return CompletionSubscriptionOutcome::AlreadyTerminal;
        }
        before_insert();
        registrations.push(registration);
        CompletionSubscriptionOutcome::Pending
    }

    pub(crate) fn unsubscribe(&self, registration: WakeRegistration) -> bool {
        let mut registrations = self
            .registrations
            .lock()
            .expect("completion subscriber set was poisoned");
        let Some(index) = registrations
            .iter()
            .position(|candidate| *candidate == registration)
        else {
            return false;
        };
        registrations.swap_remove(index);
        true
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.registrations
            .lock()
            .expect("completion subscriber set was poisoned")
            .len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionSubscriptionOutcome {
    Pending,
    AlreadyTerminal,
    ForeignRuntime,
}

impl EvaluationWorkCoordinator {
    pub(super) fn subscribe_dependency_guarded(
        &self,
        mutation: &dyn RuntimeMutationAuthority,
        dependency: WorkDependency,
        registration: WakeRegistration,
    ) -> bool {
        let source = dependency.key();
        match dependency.subscribe_work(self.runtime, registration) {
            CompletionSubscriptionOutcome::Pending => false,
            CompletionSubscriptionOutcome::AlreadyTerminal => self.wake_dependency_batch_guarded(
                mutation,
                DependencyWakeBatch {
                    source,
                    registrations: vec![registration],
                },
            ),
            CompletionSubscriptionOutcome::ForeignRuntime => {
                unreachable!("foreign dependencies must be rejected before task publication")
            }
        }
    }

    /// Queues registrations which still describe the work's current blocked
    /// dependency. The caller already owns this runtime's mutation admission;
    /// dependency publication and the scheduler transition therefore form one
    /// settlement-visible update without nesting component mutexes.
    pub(super) fn wake_dependency_batch_guarded(
        &self,
        _mutation: &dyn RuntimeMutationAuthority,
        batch: DependencyWakeBatch,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let mut changed = false;
        for registration in batch.registrations {
            changed |= queue_current_registration(&mut state, registration, Some(batch.source));
        }
        if changed {
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        changed
    }

    pub(super) fn notify_dependency_wake(&self, changed: bool) {
        if changed {
            self.work_available.notify_all();
        }
    }
}
