//! Specialization-independent task, wait, and status protocol.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use rpds::RedBlackTreeMapSync;

use crate::core::{
    EvaluationFailure, EvaluationPanic, Key, ManagedPromiseRoot, PromiseAssignment, PromiseId,
    RuntimeValueAccess,
};
use crate::runtime::{
    EvaluationRuntimeId, RuntimeFailureRoot, RuntimeMutationAuthority, RuntimeValueRoot,
};

use super::super::{RuntimeObservationEpoch, evaluation_failure};
use super::{
    CompletionSubscriptionOutcome, CompletionSubscriptions, CompletionWake,
    EvaluationWorkCoordinator, EvaluationWorkId, WakeRegistration, WorkDependency,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct EvaluationTaskId(NonZeroU64);

impl EvaluationTaskId {
    pub(crate) fn from_nonzero(id: NonZeroU64) -> Self {
        Self(id)
    }

    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) fn from_u64(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct EvaluationSessionId(NonZeroU64);

impl EvaluationSessionId {
    pub(crate) fn from_nonzero(id: NonZeroU64) -> Self {
        Self(id)
    }

    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationTaskBlock {
    pub(crate) dependency: Option<WorkDependency>,
    pub(crate) observed_epoch: Option<RuntimeObservationEpoch>,
    pub(crate) error: Option<RuntimeFailureRoot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationWaitPoll {
    Pending(EvaluationWaitToken),
    // A runtime value root is wider than a pointer. Boxing it keeps the poll
    // within two words (asserted below); drivers carry this enum in several
    // frames.
    Complete(Box<RuntimeValueRoot>),
    Failed(RuntimeFailureRoot),
    Cancelled,
    Abandoned,
    Killed(RuntimeFailureRoot),
    /// The producer was interrupted by a panic. Waiters halt instead of
    /// reinstalling work, and never treat this as a semantic failure.
    Panicked(EvaluationPanic),
}

const _: () = assert!(
    std::mem::size_of::<EvaluationWaitPoll>() <= 2 * std::mem::size_of::<usize>(),
    "wait polls must remain small enough for recursive evaluator frames"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationTaskCancellation {
    Requested,
    Late,
}

pub(crate) enum EvaluationMachinePoll {
    /// The poll spent its whole budget; nothing else ends a poll with
    /// budget left unreported (`budget-exhausted-only`).
    BudgetExhausted,
    /// Another thread holds, or is changing, what the poll needs. The poll
    /// ended with budget left, after any wait for that thread, and the work
    /// runs again (`budget-waits-block`).
    Contended,
    Blocked(EvaluationTaskBlock),
    Complete(RuntimeValueRoot),
    Failed(RuntimeFailureRoot),
    Cancelled,
    /// Only the scheduler's poll boundary produces this. `torn` means this
    /// machine itself unwound; otherwise it halted on a panicked dependency
    /// and remains intact.
    Panicked {
        report: EvaluationPanic,
        torn: bool,
    },
}

pub(crate) trait EvaluationTaskMachine: Send {
    fn poll(
        &mut self,
        context: &mut super::super::EvaluationPollContext,
        step_budget: &mut super::super::EvaluationStepBudget,
    ) -> EvaluationMachinePoll;

    fn cancel(&mut self) {}

    /// The child machines the task's current branch runs within, by call,
    /// outermost first (`child-call-frame`), which an interrupt flags with
    /// the task's own (`quiescence-is-analyzed`). Only a reflection task
    /// calls.
    fn calls(&self) -> Vec<Arc<[Key]>> {
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationTaskStatus {
    Launched,
    Blocked,
    Complete(RuntimeValueRoot),
    Failed(RuntimeFailureRoot),
    Cancelled,
    Abandoned,
    Killed(RuntimeFailureRoot),
    Panicked(EvaluationPanic),
}

pub(super) struct TaskTerminalPublisher {
    pub(super) wait: EvaluationWaitToken,
    published_status: EvaluationTaskStatus,
    status_order: u64,
    /// A test's status observer.
    pub(super) protected_status: Option<TaskStatusPublisher>,
    lifecycle_status: Option<TaskStatusPublisher>,
}

pub(super) struct TaskStatusUpdate {
    pub(super) publisher: TaskStatusPublisher,
    pub(super) status: EvaluationTaskStatus,
    order: u64,
}

impl TaskTerminalPublisher {
    pub(super) fn new(wait: EvaluationWaitToken) -> Self {
        Self {
            wait,
            published_status: EvaluationTaskStatus::Launched,
            status_order: 0,
            protected_status: None,
            lifecycle_status: None,
        }
    }

    /// Whether a test's observer or a host lifecycle observes this task.
    pub(super) fn publishes_status(&self) -> bool {
        self.protected_status.is_some() || self.lifecycle_status.is_some()
    }

    /// Whether updating to `status` would publish it to an observer.
    pub(super) fn would_publish(&self, status: &EvaluationTaskStatus) -> bool {
        self.publishes_status() && self.published_status != *status
    }

    #[cfg(test)]
    pub(super) fn attach_status(&mut self, publisher: TaskStatusPublisher) {
        assert!(
            self.protected_status.replace(publisher).is_none(),
            "a reflection task takes one test status observer"
        );
    }

    pub(super) fn attach_lifecycle(&mut self, publisher: TaskStatusPublisher) {
        assert!(
            self.lifecycle_status.replace(publisher).is_none(),
            "a reflection task may expose only one host lifecycle publisher"
        );
    }

    pub(super) fn update_status(
        &mut self,
        status: EvaluationTaskStatus,
        terminal: bool,
    ) -> Vec<TaskStatusUpdate> {
        if self.published_status == status {
            return Vec::new();
        }
        self.published_status = status.clone();
        self.status_order = self
            .status_order
            .checked_add(1)
            .expect("reflection task status publication order exhausted");
        let protected = if terminal {
            self.protected_status.take()
        } else {
            self.protected_status.clone()
        };
        let lifecycle = if terminal {
            self.lifecycle_status.take()
        } else {
            self.lifecycle_status.clone()
        };
        [protected, lifecycle]
            .into_iter()
            .flatten()
            .map(|publisher| TaskStatusUpdate {
                publisher,
                status: status.clone(),
                order: self.status_order,
            })
            .collect()
    }
}

pub(super) fn terminal_task_status(terminal: &EvaluationWaitTerminal) -> EvaluationTaskStatus {
    match terminal {
        EvaluationWaitTerminal::Complete(value) => EvaluationTaskStatus::Complete(value.clone()),
        EvaluationWaitTerminal::Failed(error) => EvaluationTaskStatus::Failed(error.clone()),
        EvaluationWaitTerminal::Cancelled => EvaluationTaskStatus::Cancelled,
        EvaluationWaitTerminal::Abandoned => EvaluationTaskStatus::Abandoned,
        EvaluationWaitTerminal::Killed(error) => EvaluationTaskStatus::Killed(error.clone()),
        EvaluationWaitTerminal::Panicked(report) => EvaluationTaskStatus::Panicked(report.clone()),
    }
}

type GuardedTaskStatusPublication =
    dyn Fn(&dyn RuntimeMutationAuthority, EvaluationTaskStatus) -> TaskStatusWake + Send + Sync;

struct TaskStatusPublisherInner {
    publish: Box<GuardedTaskStatusPublication>,
    /// Last coordinator-assigned status order delivered through this
    /// publisher. The mutex also prevents an older nonterminal callback from
    /// completing after a newer terminal callback.
    ///
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    published_order: Mutex<u64>,
}

#[derive(Clone)]
pub(crate) struct TaskStatusPublisher {
    inner: Arc<TaskStatusPublisherInner>,
}

impl TaskStatusPublisher {
    pub(crate) fn new(
        publish: impl Fn(&dyn RuntimeMutationAuthority, EvaluationTaskStatus) -> TaskStatusWake
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(TaskStatusPublisherInner {
                publish: Box::new(publish),
                published_order: Mutex::new(0),
            }),
        }
    }

    pub(super) fn publish_update_guarded(
        &self,
        mutation: &dyn RuntimeMutationAuthority,
        update: TaskStatusUpdate,
    ) -> TaskStatusWake {
        debug_assert!(Arc::ptr_eq(&self.inner, &update.publisher.inner));
        let mut published_order = self
            .inner
            .published_order
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if update.order <= *published_order {
            return TaskStatusWake::new(|| {});
        }
        let wake = (self.inner.publish)(mutation, update.status);
        *published_order = update.order;
        wake
    }
}

pub(crate) struct TaskStatusWake {
    notify: Option<Box<dyn FnOnce() + Send>>,
}

impl TaskStatusWake {
    pub(crate) fn new(notify: impl FnOnce() + Send + 'static) -> Self {
        Self {
            notify: Some(Box::new(notify)),
        }
    }

    /// Runs the status callback, which may be client code. A panic in it
    /// skips this wake rather than interrupting the settlement that
    /// published the status.
    pub(crate) fn notify(mut self) {
        if let Some(notify) = self.notify.take() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(notify));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReflectionTaskResultPolicy {
    RequireUnit,
    ReturnValue,
}

/// The failures of the reflection tasks whose failure flags their anonymous
/// roots, annotation tasks, by task (`reflection-tasks-are-machine-tasks`).
/// An anonymous root is unrecoverable, so its failure stands, and
/// settlement reports each as a failed machine.
pub(crate) type FailedRoots = RedBlackTreeMapSync<EvaluationTaskId, RuntimeFailureRoot>;

struct EvaluationWaitState {
    id: NonZeroU64,
    values: crate::core::RuntimeValueObserver,
    terminal: OnceLock<EvaluationWaitTerminal>,
    completion: CompletionSubscriptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationWaitTerminal {
    Complete(RuntimeValueRoot),
    Failed(RuntimeFailureRoot),
    Cancelled,
    Abandoned,
    Killed(RuntimeFailureRoot),
    /// The producer was interrupted by a panic: task-layer state, never a
    /// semantic result.
    Panicked(EvaluationPanic),
}

#[derive(Clone)]
pub(crate) struct EvaluationWaitToken(Arc<EvaluationWaitHandle>);

struct EvaluationWaitHandle {
    state: Arc<EvaluationWaitState>,
    _lease: Option<LazyRouteDemandLease>,
}

/// One logical demand on a runtime-owned lazy route. Cloning its wait handle
/// retains this lease but does not register another subscriber. Coordinator
/// records themselves keep an unleased wait handle.
pub(super) struct LazyRouteDemandLease {
    coordinator: Weak<EvaluationWorkCoordinator>,
    work: super::EvaluationWorkId,
}

impl LazyRouteDemandLease {
    pub(super) fn new(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        work: super::EvaluationWorkId,
    ) -> Self {
        Self {
            coordinator: Arc::downgrade(coordinator),
            work,
        }
    }
}

impl Drop for LazyRouteDemandLease {
    fn drop(&mut self) {
        if let Some(coordinator) = self.coordinator.upgrade()
            && !coordinator.runtime_poisoned()
        {
            coordinator.release_lazy_route_demand(self.work);
        }
    }
}

impl EvaluationWaitToken {
    pub(crate) fn new(
        id: NonZeroU64,
        values: &crate::core::CoreValueFactory,
        completion: CompletionSubscriptions,
    ) -> Self {
        Self(Arc::new(EvaluationWaitHandle {
            state: Arc::new(EvaluationWaitState {
                id,
                values: values.runtime_value_observer(),
                terminal: OnceLock::new(),
                completion,
            }),
            _lease: None,
        }))
    }

    pub(super) fn with_lazy_route_lease(&self, lease: LazyRouteDemandLease) -> Self {
        Self(Arc::new(EvaluationWaitHandle {
            state: self.0.state.clone(),
            _lease: Some(lease),
        }))
    }

    pub(crate) fn get(&self) -> u64 {
        self.0.state.id.get()
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.0.state.values.runtime_id()
    }

    pub(crate) fn value_observer(&self) -> &crate::core::RuntimeValueObserver {
        &self.0.state.values
    }

    fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        self.0.state.completion.coordinator()
    }

    pub(crate) fn terminal_poll(&self) -> Option<EvaluationWaitPoll> {
        self.0
            .state
            .terminal
            .get()
            .map(EvaluationWaitTerminal::to_poll)
    }

    pub(crate) fn publish_terminal(
        &self,
        terminal: EvaluationWaitTerminal,
    ) -> EvaluationWaitTerminal {
        match &terminal {
            EvaluationWaitTerminal::Complete(value) => {
                debug_assert_eq!(value.runtime_id(), self.runtime_id());
            }
            EvaluationWaitTerminal::Failed(failure) | EvaluationWaitTerminal::Killed(failure) => {
                debug_assert_eq!(failure.runtime_id(), self.runtime_id());
            }
            EvaluationWaitTerminal::Cancelled
            | EvaluationWaitTerminal::Abandoned
            | EvaluationWaitTerminal::Panicked(_) => {}
        }
        if let Err(candidate) = self.0.state.terminal.set(terminal) {
            debug_assert_eq!(
                self.0.state.terminal.get(),
                Some(&candidate),
                "a wait token received conflicting terminal results"
            );
        }
        self.0
            .state
            .terminal
            .get()
            .expect("terminal publication must initialize the wait cell")
            .clone()
    }

    pub(crate) fn publish_terminal_guarded(
        &self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        terminal: EvaluationWaitTerminal,
    ) -> (EvaluationWaitTerminal, CompletionWake) {
        self.0
            .state
            .completion
            .publish_guarded(coordinator, mutation, || {
                Ok::<_, std::convert::Infallible>(self.publish_terminal(terminal))
            })
            .expect("wait terminal publication is infallible")
    }

    pub(crate) fn notify_terminal(&self) {
        debug_assert!(self.0.state.terminal.get().is_some());
        self.0.state.completion.notify_published();
    }

    pub(super) fn abandon_deferred_producer(&self) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        if coordinator.runtime_poisoned() {
            return;
        }
        let Some(owner) = coordinator.deferred_owner_for_wait(self) else {
            return;
        };
        let mut wait = self.clone();
        loop {
            if wait.terminal_poll().is_some()
                || coordinator.deferred_owner_for_wait(&wait) != Some(owner)
            {
                return;
            }
            let Some(abandoned) = coordinator.abandon_deferred_wait(&wait) else {
                return;
            };
            let terminal = coordinator.settle_terminal_work(
                abandoned.id,
                EvaluationWaitTerminal::Abandoned,
                evaluation_failure("deferred fixpoint producer was abandoned"),
            );
            debug_assert_eq!(wait.terminal_poll(), Some(terminal.to_poll()));
            coordinator.retire_deferred(abandoned.id);
            drop(abandoned.machine);
            let Some(dependency) = abandoned.dependency else {
                return;
            };
            wait = dependency;
        }
    }

    pub(crate) fn subscribe_work(
        &self,
        runtime: EvaluationRuntimeId,
        registration: WakeRegistration,
    ) -> CompletionSubscriptionOutcome {
        self.0
            .state
            .completion
            .subscribe(runtime, registration, || {
                self.0.state.terminal.get().is_some()
            })
    }

    pub(crate) fn unsubscribe_work(&self, registration: WakeRegistration) -> bool {
        self.0.state.completion.unsubscribe(registration)
    }

    pub(crate) fn has_exact_subscriptions(&self) -> bool {
        self.0.state.completion.len() != 0
    }

    #[cfg(test)]
    pub(crate) fn exact_subscription_count(&self) -> usize {
        self.0.state.completion.len()
    }

    #[cfg(test)]
    pub(crate) fn terminal_for_test(&self) -> Option<&EvaluationWaitTerminal> {
        self.0.state.terminal.get()
    }

    #[cfg(test)]
    pub(crate) fn subscribe_test_work(&self) -> CompletionSubscriptionOutcome {
        self.subscribe_work(self.runtime_id(), super::test_wake_registration())
    }
}

impl EvaluationWaitTerminal {
    pub(crate) fn to_poll(&self) -> EvaluationWaitPoll {
        match self {
            Self::Complete(value) => EvaluationWaitPoll::Complete(Box::new(value.clone())),
            Self::Failed(error) => EvaluationWaitPoll::Failed(error.clone()),
            Self::Cancelled => EvaluationWaitPoll::Cancelled,
            Self::Abandoned => EvaluationWaitPoll::Abandoned,
            Self::Killed(error) => EvaluationWaitPoll::Killed(error.clone()),
            Self::Panicked(report) => EvaluationWaitPoll::Panicked(report.clone()),
        }
    }
}

impl fmt::Debug for EvaluationWaitToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationWaitToken")
            .field("wait", &self.0.state.id)
            .field("terminal", &self.0.state.terminal.get().is_some())
            .finish_non_exhaustive()
    }
}

impl PartialEq for EvaluationWaitToken {
    fn eq(&self, other: &Self) -> bool {
        self.0.state.id == other.0.state.id
    }
}

impl Eq for EvaluationWaitToken {}

impl Hash for EvaluationWaitToken {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.state.id.hash(state);
    }
}

/// Assignment-side routing for one task-owned promise obligation.
///
/// The managed promise cell retains this record strongly for stable producer
/// provenance. Its wait-state route is weak because a terminal wait may own
/// rooted result data; the authoritative task/local obligation retains the
/// strong wait until assignment publication removes that external owner.
pub(crate) struct PromiseProducerObligation {
    owner: EvaluationTaskId,
    wait: Weak<EvaluationWaitState>,
    source: PromiseProducerSource,
    /// The panic that interrupted the producer. A panicked producer leaves
    /// its promises unassigned and records the panic here instead, because a
    /// panic is never a semantic result.
    panic: OnceLock<EvaluationPanic>,
}

enum PromiseProducerSource {
    Coordinator {
        work: EvaluationWorkId,
        promise: PromiseId,
        coordinator: Weak<EvaluationWorkCoordinator>,
    },
    Local {
        promise: PromiseId,
        owner: Weak<LocalPromiseOwner>,
    },
}

#[derive(Clone)]
struct LocalPromiseObligation {
    promise: PromiseId,
    root: ManagedPromiseRoot,
    wait: EvaluationWaitToken,
}

#[derive(Default)]
pub(crate) struct LocalPromiseOwner {
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    obligations: Mutex<Vec<LocalPromiseObligation>>,
}

impl std::fmt::Debug for LocalPromiseOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalPromiseOwner")
            .field(
                "obligations",
                &self
                    .obligations
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .len(),
            )
            .finish()
    }
}

impl LocalPromiseOwner {
    pub(crate) fn register(&self, root: ManagedPromiseRoot, wait: EvaluationWaitToken) {
        self.obligations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(LocalPromiseObligation {
                promise: root.id(),
                wait,
                root,
            });
    }

    pub(crate) fn contains_wait(&self, wait: &EvaluationWaitToken) -> bool {
        self.obligations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|obligation| obligation.wait == *wait)
    }

    fn complete(
        &self,
        promise: PromiseId,
        wait: &EvaluationWaitToken,
    ) -> Option<ManagedPromiseRoot> {
        let mut obligations = self
            .obligations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let obligation = obligations
            .iter()
            .position(|obligation| obligation.promise == promise && obligation.wait == *wait)
            .map(|index| obligations.swap_remove(index));
        drop(obligations);
        obligation.map(|obligation| obligation.root)
    }

    /// Fails every local promise through `access`, returning the
    /// publications for the caller to notify once its access closes.
    #[must_use = "promise completion wakes must be delivered after managed access closes"]
    pub(crate) fn fail_all(
        &self,
        access: &RuntimeValueAccess<'_>,
        failure: Arc<EvaluationFailure>,
    ) -> Vec<crate::core::ManagedPromisePublication> {
        let obligations = self
            .obligations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        obligations
            .into_iter()
            .filter_map(|obligation| {
                debug_assert_eq!(
                    obligation.wait.value_observer().runtime_id(),
                    access.runtime_id()
                );
                obligation.root.publish(access, Err(failure.clone())).ok()
            })
            .collect()
    }
}

enum PromiseProducerNotification {
    Guarded(CompletionWake),
    Detached(EvaluationWaitToken),
}

/// Post-publication notification and roots retired by that publication.
///
/// Roots remain here until the caller releases component locks and runtime
/// mutation admission, delivers the wake, and drops this record. The optional
/// snapshot root covers terminal-settlement callers which had to clone an
/// obligation before publishing through its authoritative coordinator entry.
pub(crate) struct PromiseProducerPublication {
    notification: PromiseProducerNotification,
    retired_root: Option<ManagedPromiseRoot>,
    snapshot_root: Option<ManagedPromiseRoot>,
}

impl PromiseProducerPublication {
    fn guarded(wake: CompletionWake, retired_root: ManagedPromiseRoot) -> Self {
        Self {
            notification: PromiseProducerNotification::Guarded(wake),
            retired_root: Some(retired_root),
            snapshot_root: None,
        }
    }

    fn detached(wait: EvaluationWaitToken, retired_root: Option<ManagedPromiseRoot>) -> Self {
        Self {
            notification: PromiseProducerNotification::Detached(wait),
            retired_root,
            snapshot_root: None,
        }
    }

    pub(super) fn retain_snapshot_root(mut self, root: ManagedPromiseRoot) -> Self {
        assert!(
            self.snapshot_root.replace(root).is_none(),
            "a promise publication may retain only one settlement snapshot root"
        );
        self
    }

    pub(crate) fn notify(self) {
        let Self {
            notification,
            retired_root,
            snapshot_root,
        } = self;
        match notification {
            PromiseProducerNotification::Guarded(wake) => wake.notify(),
            PromiseProducerNotification::Detached(wait) => wait.notify_terminal(),
        }
        drop((retired_root, snapshot_root));
    }
}

impl PromiseProducerObligation {
    pub(crate) fn coordinator_owned(
        owner: EvaluationTaskId,
        wait: &EvaluationWaitToken,
        work: EvaluationWorkId,
        promise: PromiseId,
        coordinator: &Arc<EvaluationWorkCoordinator>,
    ) -> Self {
        Self {
            owner,
            wait: Arc::downgrade(&wait.0.state),
            source: PromiseProducerSource::Coordinator {
                work,
                promise,
                coordinator: Arc::downgrade(coordinator),
            },
            panic: OnceLock::new(),
        }
    }

    pub(crate) fn local_owned(
        owner: EvaluationTaskId,
        wait: &EvaluationWaitToken,
        promise: PromiseId,
        local_owner: &Arc<LocalPromiseOwner>,
    ) -> Self {
        Self {
            owner,
            wait: Arc::downgrade(&wait.0.state),
            source: PromiseProducerSource::Local {
                promise,
                owner: Arc::downgrade(local_owner),
            },
            panic: OnceLock::new(),
        }
    }

    pub(crate) fn panic_report(&self) -> Option<&EvaluationPanic> {
        self.panic.get()
    }

    fn record_panic(&self, report: &EvaluationPanic) {
        let _ = self.panic.set(report.clone());
    }

    pub(crate) fn owner(&self) -> EvaluationTaskId {
        self.owner
    }

    pub(crate) fn try_wait(&self) -> Option<EvaluationWaitToken> {
        self.wait.upgrade().map(|state| {
            EvaluationWaitToken(Arc::new(EvaluationWaitHandle {
                state,
                _lease: None,
            }))
        })
    }

    #[cfg(test)]
    pub(crate) fn wait(&self) -> EvaluationWaitToken {
        self.try_wait()
            .expect("an unresolved producer must retain its external wait owner")
    }

    pub(crate) fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        match &self.source {
            PromiseProducerSource::Coordinator { coordinator, .. } => coordinator.upgrade(),
            PromiseProducerSource::Local { .. } => None,
        }
    }

    pub(crate) fn publish_assignment_guarded(
        &self,
        access: &RuntimeValueAccess<'_>,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        assignment: &PromiseAssignment,
    ) -> PromiseProducerPublication {
        let PromiseProducerSource::Coordinator { work, promise, .. } = self.source else {
            panic!("a task-local promise cannot publish through a coordinator guard");
        };
        let wait = self
            .try_wait()
            .expect("an active task promise must retain its external wait owner");
        debug_assert_eq!(coordinator.runtime_id(), wait.runtime_id());
        let retired_root = coordinator
            .complete_task_promise_guarded(mutation, work, &wait, promise)
            .expect("a publishing task promise must retain its producer obligation");
        let terminal = promise_assignment_terminal(access, &wait, assignment);
        let (_, wake) = wait.publish_terminal_guarded(coordinator, mutation, terminal);
        PromiseProducerPublication::guarded(wake, retired_root)
    }

    /// Publishes this producer's panic in place of an assignment.
    ///
    /// The promise stays unassigned: its observers find the recorded panic
    /// and halt, because a panic is never a semantic result.
    pub(super) fn publish_panic_guarded(
        &self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        report: &EvaluationPanic,
    ) -> PromiseProducerPublication {
        let PromiseProducerSource::Coordinator { work, promise, .. } = self.source else {
            panic!("a task-local promise cannot publish through a coordinator guard");
        };
        let wait = self
            .try_wait()
            .expect("an active task promise must retain its external wait owner");
        debug_assert_eq!(coordinator.runtime_id(), wait.runtime_id());
        self.record_panic(report);
        let retired_root = coordinator
            .complete_task_promise_guarded(mutation, work, &wait, promise)
            .expect("a publishing task promise must retain its producer obligation");
        let (_, wake) = wait.publish_terminal_guarded(
            coordinator,
            mutation,
            EvaluationWaitTerminal::Panicked(report.clone()),
        );
        PromiseProducerPublication::guarded(wake, retired_root)
    }

    pub(crate) fn publish_assignment_detached(
        &self,
        access: &RuntimeValueAccess<'_>,
        assignment: &PromiseAssignment,
    ) -> PromiseProducerPublication {
        let wait = self
            .try_wait()
            .expect("an active local promise must retain its external wait owner");
        let retired_root = match &self.source {
            PromiseProducerSource::Local { promise, owner } => owner
                .upgrade()
                .and_then(|owner| owner.complete(*promise, &wait)),
            PromiseProducerSource::Coordinator { .. } => None,
        };
        let terminal = promise_assignment_terminal(access, &wait, assignment);
        wait.publish_terminal(terminal);
        PromiseProducerPublication::detached(wait, retired_root)
    }
}

fn promise_assignment_terminal(
    access: &RuntimeValueAccess<'_>,
    wait: &EvaluationWaitToken,
    assignment: &PromiseAssignment,
) -> EvaluationWaitTerminal {
    assert!(
        access.admits(wait.value_observer()),
        "promise assignment and terminal wait must share one value domain"
    );
    match assignment {
        Ok(value) => EvaluationWaitTerminal::Complete(
            access.root_runtime_value(access.duplicate_value(value)),
        ),
        Err(error) => EvaluationWaitTerminal::Failed(access.root_runtime_failure(error.clone())),
    }
}

#[derive(Clone)]
pub(crate) struct EvaluationTaskHandle {
    id: EvaluationTaskId,
    pub(in crate::evaluation) work: EvaluationWorkId,
    owner_session: EvaluationSessionId,
    coordinator: Weak<EvaluationWorkCoordinator>,
    pub(in crate::evaluation) wait: EvaluationWaitToken,
}

impl EvaluationTaskHandle {
    pub(crate) fn new(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        owner_session: EvaluationSessionId,
        id: EvaluationTaskId,
        work: EvaluationWorkId,
        wait: EvaluationWaitToken,
    ) -> Self {
        debug_assert_eq!(coordinator.runtime_id(), wait.runtime_id());
        Self {
            id,
            work,
            owner_session,
            coordinator: Arc::downgrade(coordinator),
            wait,
        }
    }

    #[cfg(test)]
    pub(crate) fn wait(&self) -> &EvaluationWaitToken {
        &self.wait
    }

    pub(crate) fn id(&self) -> EvaluationTaskId {
        self.id
    }

    pub(crate) fn session_id(&self) -> EvaluationSessionId {
        self.owner_session
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.wait.runtime_id()
    }

    /// The calls the task waits within, where it waits: the coordinator
    /// holds its machine then. A task running a quantum, or ended, waits
    /// within none.
    pub(crate) fn waiting_calls(&self) -> Vec<Arc<[Key]>> {
        self.coordinator
            .upgrade()
            .map(|coordinator| coordinator.waiting_calls(self.work))
            .unwrap_or_default()
    }

    pub(crate) fn cancel(&self) -> EvaluationTaskCancellation {
        if self.wait.terminal_poll().is_some() {
            return EvaluationTaskCancellation::Late;
        }
        let Some(coordinator) = self.coordinator.upgrade() else {
            return EvaluationTaskCancellation::Late;
        };
        debug_assert_eq!(coordinator.runtime_id(), self.runtime_id());
        coordinator.cancel_reflection(self.work)
    }
}

#[cfg(test)]
#[test]
fn evaluation_task_handle_retains_only_identity_wait_and_weak_coordinator_authority() {
    fn fields(handle: &EvaluationTaskHandle) {
        let EvaluationTaskHandle {
            id,
            work,
            owner_session,
            coordinator,
            wait,
        } = handle;
        let _: &EvaluationTaskId = id;
        let _: &EvaluationWorkId = work;
        let _: &EvaluationSessionId = owner_session;
        let _: &Weak<EvaluationWorkCoordinator> = coordinator;
        let _: &EvaluationWaitToken = wait;
    }

    let _ = fields as fn(&EvaluationTaskHandle);
}

/// A fully constructed reflection task retained in the coordinator's
/// `Reserved` state.
pub(crate) struct PreparedEvaluationTask {
    coordinator: Arc<EvaluationWorkCoordinator>,
    pub(super) handle: EvaluationTaskHandle,
}

impl PreparedEvaluationTask {
    pub(crate) fn new(
        coordinator: Arc<EvaluationWorkCoordinator>,
        handle: EvaluationTaskHandle,
    ) -> Self {
        Self {
            coordinator,
            handle,
        }
    }

    pub(crate) fn activate(self) -> EvaluationTaskHandle {
        assert!(
            self.coordinator.activate_reflection(self.handle.work),
            "fresh reflection reservation must activate"
        );
        self.handle
    }

    #[cfg(test)]
    pub(crate) fn into_handle(self) -> EvaluationTaskHandle {
        self.handle
    }
}

impl fmt::Debug for EvaluationTaskHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationTaskHandle")
            .field("task", &self.id.get())
            .field("work", &self.work.get())
            .field("runtime", &self.runtime_id())
            .field("session", &self.session_id().get())
            .finish_non_exhaustive()
    }
}
