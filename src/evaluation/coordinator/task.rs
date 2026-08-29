//! Specialization-independent task, wait, and status protocol.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use rpds::RedBlackTreeMapSync;

use crate::core::{EvaluationFailure, PromiseAssignment, PromiseCell, PromiseId};
use crate::runtime::{EvaluationRuntimeId, RuntimeMutationAuthority, RuntimeValueRoot};

use super::super::{EvaluationDemandState, RuntimeObservationEpoch, evaluation_failure};
use super::{
    CompletionSubscriptionOutcome, CompletionSubscriptions, CompletionWake,
    EvaluationWorkCoordinator, EvaluationWorkId, ReflectionCancellation, WakeRegistration,
    WorkDependency,
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
    pub(crate) error: Option<Arc<EvaluationFailure>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationWaitPoll {
    Pending(EvaluationWaitToken),
    // The compatibility root still embeds a large `Value`. Keep the poll
    // itself pointer-sized until I4F.2 replaces that interior with a managed
    // root; recursive evaluator drivers carry this enum in several frames.
    Complete(Box<RuntimeValueRoot>),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
    Abandoned,
    Exited,
    Killed(Arc<EvaluationFailure>),
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum InitialTaskDisposition {
    #[default]
    Launch,
    Cancel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PendingTaskPolicy {
    disposition: InitialTaskDisposition,
    acknowledge_error: bool,
}

impl PendingTaskPolicy {
    pub(crate) fn cancel(&mut self) {
        self.disposition = InitialTaskDisposition::Cancel;
    }

    pub(crate) fn acknowledge_error(&mut self) {
        self.acknowledge_error = true;
    }

    pub(crate) fn disposition(self) -> InitialTaskDisposition {
        self.disposition
    }

    pub(crate) fn acknowledges_error(self) -> bool {
        self.acknowledge_error
    }
}

/// A coordinator-facing request to terminalize one effect machine when the
/// runtime client accepts a stable readiness snapshot.
///
/// Exit is deliberately distinct from ordinary completion or failure. Until
/// settlement, it produces neither a task result nor a failure-ledger entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExitIntent {
    Success,
    Error(RuntimeValueRoot),
}

/// The specialization-independent portion of one preterminal exit vote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationExitBlock {
    pub(crate) intent: ExitIntent,
    pub(crate) observed_epoch: Option<RuntimeObservationEpoch>,
}

pub(crate) enum EvaluationMachinePoll {
    Yielded,
    Blocked(EvaluationTaskBlock),
    Exit(EvaluationExitBlock),
    Complete(RuntimeValueRoot),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
}

pub(crate) trait EvaluationTaskMachine: Send {
    fn poll(
        &mut self,
        context: &super::super::EvaluationPollContext,
        step_budget: usize,
    ) -> EvaluationMachinePoll;

    fn cancel(&mut self) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationTaskStatus {
    Launched,
    Blocked,
    Complete(RuntimeValueRoot),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
    Abandoned,
    Exited,
    Killed(Arc<EvaluationFailure>),
}

pub(super) struct TaskTerminalPublisher {
    pub(super) wait: EvaluationWaitToken,
    published_status: EvaluationTaskStatus,
    pub(super) protected_status: Option<TaskStatusPublisher>,
    lifecycle_status: Option<TaskStatusPublisher>,
}

pub(super) type TaskStatusUpdate = (TaskStatusPublisher, EvaluationTaskStatus);

impl TaskTerminalPublisher {
    pub(super) fn new(wait: EvaluationWaitToken) -> Self {
        Self {
            wait,
            published_status: EvaluationTaskStatus::Launched,
            protected_status: None,
            lifecycle_status: None,
        }
    }

    pub(super) fn attach_status(&mut self, publisher: TaskStatusPublisher) {
        assert!(
            self.protected_status.replace(publisher).is_none(),
            "a reflection task may expose only one status query"
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
            .map(|publisher| (publisher, status.clone()))
            .collect()
    }
}

pub(super) fn terminal_task_status(terminal: &EvaluationWaitTerminal) -> EvaluationTaskStatus {
    match terminal {
        EvaluationWaitTerminal::Complete(value) => EvaluationTaskStatus::Complete(value.clone()),
        EvaluationWaitTerminal::Failed(error) => EvaluationTaskStatus::Failed(error.clone()),
        EvaluationWaitTerminal::Cancelled => EvaluationTaskStatus::Cancelled,
        EvaluationWaitTerminal::Abandoned => EvaluationTaskStatus::Abandoned,
        EvaluationWaitTerminal::Exited => EvaluationTaskStatus::Exited,
        EvaluationWaitTerminal::Killed(error) => EvaluationTaskStatus::Killed(error.clone()),
    }
}

type GuardedTaskStatusPublication =
    dyn Fn(&dyn RuntimeMutationAuthority, EvaluationTaskStatus) -> TaskStatusWake + Send + Sync;

#[derive(Clone)]
pub(crate) struct TaskStatusPublisher {
    publish: Arc<GuardedTaskStatusPublication>,
}

impl TaskStatusPublisher {
    pub(crate) fn new(
        publish: impl Fn(&dyn RuntimeMutationAuthority, EvaluationTaskStatus) -> TaskStatusWake
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            publish: Arc::new(publish),
        }
    }

    pub(crate) fn publish_guarded(
        &self,
        mutation: &dyn RuntimeMutationAuthority,
        status: EvaluationTaskStatus,
    ) -> TaskStatusWake {
        (self.publish)(mutation, status)
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

    pub(crate) fn notify(mut self) {
        if let Some(notify) = self.notify.take() {
            notify();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReflectionTaskResultPolicy {
    RequireUnit,
    ReturnValue,
}

pub(crate) type TaskFailureLedger = RedBlackTreeMapSync<EvaluationTaskId, Arc<EvaluationFailure>>;
pub(crate) type RuntimeFailureLedger = RedBlackTreeMapSync<EvaluationSessionId, TaskFailureLedger>;

struct EvaluationWaitState {
    id: NonZeroU64,
    runtime: EvaluationRuntimeId,
    owner_id: EvaluationSessionId,
    producer: EvaluationTaskId,
    terminal: OnceLock<EvaluationWaitTerminal>,
    completion: CompletionSubscriptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationWaitTerminal {
    Complete(RuntimeValueRoot),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
    Abandoned,
    Exited,
    Killed(Arc<EvaluationFailure>),
}

#[derive(Clone)]
pub(crate) struct EvaluationWaitToken(Arc<EvaluationWaitState>);

impl EvaluationWaitToken {
    pub(crate) fn new(
        id: NonZeroU64,
        runtime: EvaluationRuntimeId,
        owner_id: EvaluationSessionId,
        producer: EvaluationTaskId,
        completion: CompletionSubscriptions,
    ) -> Self {
        Self(Arc::new(EvaluationWaitState {
            id,
            runtime,
            owner_id,
            producer,
            terminal: OnceLock::new(),
            completion,
        }))
    }

    pub(crate) fn get(&self) -> u64 {
        self.0.id.get()
    }

    pub(crate) fn owner_id(&self) -> EvaluationSessionId {
        self.0.owner_id
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.0.runtime
    }

    pub(crate) fn producer(&self) -> EvaluationTaskId {
        self.0.producer
    }

    fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        self.0.completion.coordinator()
    }

    pub(crate) fn belongs_to(&self, session: &Arc<EvaluationDemandState>) -> bool {
        self.runtime_id() == session.values.runtime_id() && self.owner_id() == session.id
    }

    pub(crate) fn terminal_poll(&self) -> Option<EvaluationWaitPoll> {
        self.0.terminal.get().map(EvaluationWaitTerminal::to_poll)
    }

    pub(crate) fn publish_terminal(
        &self,
        terminal: EvaluationWaitTerminal,
    ) -> EvaluationWaitTerminal {
        if let EvaluationWaitTerminal::Complete(value) = &terminal {
            debug_assert_eq!(value.runtime_id(), self.runtime_id());
        }
        if let Err(candidate) = self.0.terminal.set(terminal) {
            debug_assert_eq!(
                self.0.terminal.get(),
                Some(&candidate),
                "a wait token received conflicting terminal results"
            );
        }
        self.0
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
            .completion
            .publish_guarded(coordinator, mutation, || {
                Ok::<_, std::convert::Infallible>(self.publish_terminal(terminal))
            })
            .expect("wait terminal publication is infallible")
    }

    pub(crate) fn notify_terminal(&self) {
        debug_assert!(self.0.terminal.get().is_some());
        self.0.completion.notify_published();
    }

    pub(super) fn abandon_deferred_producer(&self) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        let owner = self.owner_id();
        let mut wait = self.clone();
        loop {
            if wait.owner_id() != owner || wait.terminal_poll().is_some() {
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
            .completion
            .subscribe(runtime, registration, || self.0.terminal.get().is_some())
    }

    pub(crate) fn unsubscribe_work(&self, registration: WakeRegistration) -> bool {
        self.0.completion.unsubscribe(registration)
    }

    #[cfg(test)]
    pub(crate) fn exact_subscription_count(&self) -> usize {
        self.0.completion.len()
    }

    #[cfg(test)]
    pub(crate) fn terminal_for_test(&self) -> Option<&EvaluationWaitTerminal> {
        self.0.terminal.get()
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
            Self::Exited => EvaluationWaitPoll::Exited,
            Self::Killed(error) => EvaluationWaitPoll::Killed(error.clone()),
        }
    }
}

impl fmt::Debug for EvaluationWaitToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationWaitToken")
            .field("wait", &self.0.id)
            .field("session", &self.0.owner_id)
            .field("producer", &self.0.producer)
            .field("terminal", &self.0.terminal.get().is_some())
            .finish_non_exhaustive()
    }
}

impl PartialEq for EvaluationWaitToken {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}

impl Eq for EvaluationWaitToken {}

impl Hash for EvaluationWaitToken {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.id.hash(state);
    }
}

/// Assignment-side access to one task-owned promise obligation.
pub(crate) struct PromiseProducerObligation {
    owner: EvaluationTaskId,
    wait: EvaluationWaitToken,
    source: PromiseProducerSource,
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

#[derive(Debug, Clone)]
struct LocalPromiseObligation {
    promise: PromiseId,
    cell: Weak<PromiseCell>,
    wait: EvaluationWaitToken,
}

#[derive(Debug, Default)]
pub(crate) struct LocalPromiseOwner {
    obligations: Mutex<Vec<LocalPromiseObligation>>,
}

impl LocalPromiseOwner {
    fn register(&self, promise: &Arc<PromiseCell>, wait: EvaluationWaitToken) {
        self.obligations
            .lock()
            .expect("local promise obligations were poisoned")
            .push(LocalPromiseObligation {
                promise: promise.id(),
                cell: Arc::downgrade(promise),
                wait,
            });
    }

    pub(crate) fn contains_wait(&self, wait: &EvaluationWaitToken) -> bool {
        self.obligations
            .lock()
            .expect("local promise obligations were poisoned")
            .iter()
            .any(|obligation| obligation.wait == *wait)
    }

    fn complete(&self, promise: PromiseId, wait: &EvaluationWaitToken) {
        let mut obligations = self
            .obligations
            .lock()
            .expect("local promise obligations were poisoned");
        if let Some(index) = obligations
            .iter()
            .position(|obligation| obligation.promise == promise && obligation.wait == *wait)
        {
            obligations.swap_remove(index);
        }
    }

    pub(crate) fn fail_all(&self, failure: Arc<EvaluationFailure>) {
        let obligations = self
            .obligations
            .lock()
            .expect("local promise obligations were poisoned")
            .clone();
        for obligation in obligations {
            if let Some(cell) = obligation.cell.upgrade() {
                let _ = cell.fail(failure.clone());
            } else {
                self.complete(obligation.promise, &obligation.wait);
                obligation
                    .wait
                    .publish_terminal(EvaluationWaitTerminal::Failed(failure.clone()));
                obligation.wait.notify_terminal();
            }
        }
    }
}

pub(crate) enum PromiseProducerPublication {
    Guarded(CompletionWake),
    Detached(EvaluationWaitToken),
}

impl PromiseProducerPublication {
    pub(crate) fn notify(self) {
        match self {
            Self::Guarded(wake) => wake.notify(),
            Self::Detached(wait) => wait.notify_terminal(),
        }
    }
}

impl PromiseProducerObligation {
    pub(crate) fn coordinator_owned(
        owner: EvaluationTaskId,
        wait: EvaluationWaitToken,
        work: EvaluationWorkId,
        promise: PromiseId,
        coordinator: &Arc<EvaluationWorkCoordinator>,
    ) -> Self {
        Self {
            owner,
            wait,
            source: PromiseProducerSource::Coordinator {
                work,
                promise,
                coordinator: Arc::downgrade(coordinator),
            },
        }
    }

    pub(crate) fn local_owned(
        owner: EvaluationTaskId,
        wait: EvaluationWaitToken,
        promise: &Arc<PromiseCell>,
        local_owner: &Arc<LocalPromiseOwner>,
    ) -> Self {
        local_owner.register(promise, wait.clone());
        Self {
            owner,
            wait,
            source: PromiseProducerSource::Local {
                promise: promise.id(),
                owner: Arc::downgrade(local_owner),
            },
        }
    }

    pub(crate) fn owner(&self) -> EvaluationTaskId {
        self.owner
    }

    pub(crate) fn wait(&self) -> &EvaluationWaitToken {
        &self.wait
    }

    pub(crate) fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        match &self.source {
            PromiseProducerSource::Coordinator { coordinator, .. } => coordinator.upgrade(),
            PromiseProducerSource::Local { .. } => None,
        }
    }

    pub(crate) fn publish_assignment_guarded(
        &self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        assignment: &PromiseAssignment,
    ) -> PromiseProducerPublication {
        debug_assert_eq!(coordinator.runtime_id(), self.wait.runtime_id());
        let PromiseProducerSource::Coordinator { work, promise, .. } = self.source else {
            panic!("a task-local promise cannot publish through a coordinator guard");
        };
        coordinator.complete_task_promise_guarded(mutation, work, &self.wait, promise);
        let terminal = promise_assignment_terminal(self.wait.runtime_id(), assignment);
        let (_, wake) = self
            .wait
            .publish_terminal_guarded(coordinator, mutation, terminal);
        PromiseProducerPublication::Guarded(wake)
    }

    pub(crate) fn publish_assignment_detached(
        &self,
        assignment: &PromiseAssignment,
    ) -> PromiseProducerPublication {
        if let PromiseProducerSource::Local { promise, owner } = &self.source
            && let Some(owner) = owner.upgrade()
        {
            owner.complete(*promise, &self.wait);
        }
        let terminal = promise_assignment_terminal(self.wait.runtime_id(), assignment);
        self.wait.publish_terminal(terminal);
        PromiseProducerPublication::Detached(self.wait.clone())
    }
}

fn promise_assignment_terminal(
    runtime: EvaluationRuntimeId,
    assignment: &PromiseAssignment,
) -> EvaluationWaitTerminal {
    match assignment {
        Ok(value) => {
            debug_assert_eq!(value.runtime_id(), runtime);
            EvaluationWaitTerminal::Complete(value.clone())
        }
        Err(error) => EvaluationWaitTerminal::Failed(error.clone()),
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
        debug_assert_eq!(owner_session, wait.owner_id());
        debug_assert_eq!(id, wait.producer());
        Self {
            id,
            work,
            owner_session,
            coordinator: Arc::downgrade(coordinator),
            wait,
        }
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

    #[cfg(test)]
    pub(crate) fn wait(&self) -> &EvaluationWaitToken {
        &self.wait
    }

    pub(crate) fn acknowledge_propagated_failure(&self) {
        self.acknowledge_failure();
    }

    pub(crate) fn acknowledge_failure(&self) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            debug_assert_eq!(coordinator.runtime_id(), self.runtime_id());
            coordinator.acknowledge_task_failure(self.owner_session, self.id);
        }
    }

    pub(crate) fn cancel(&self) -> EvaluationTaskCancellation {
        if self.wait.terminal_poll().is_some() {
            return EvaluationTaskCancellation::Late;
        }
        let Some(coordinator) = self.coordinator.upgrade() else {
            return EvaluationTaskCancellation::Late;
        };
        debug_assert_eq!(coordinator.runtime_id(), self.runtime_id());
        match coordinator.request_reflection_cancellation(self.work) {
            ReflectionCancellation::Requested => EvaluationTaskCancellation::Requested,
            ReflectionCancellation::Late => EvaluationTaskCancellation::Late,
            ReflectionCancellation::Terminalize => {
                coordinator.settle_terminal_work(
                    self.work,
                    EvaluationWaitTerminal::Cancelled,
                    evaluation_failure("reflection fixpoint producer was cancelled"),
                );
                let mut machine = coordinator.retire_reflection(self.work);
                if let Some(machine) = &mut machine {
                    machine.cancel();
                }
                drop(machine);
                EvaluationTaskCancellation::Requested
            }
        }
    }
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

    pub(crate) fn activate_guarded(&self, mutation: &dyn RuntimeMutationAuthority) -> bool {
        self.coordinator
            .activate_reflection_guarded(self.handle.work, mutation)
    }

    pub(crate) fn finish_guarded_activation(&self, activated: bool) {
        self.coordinator.notify_reflection_activation(activated);
    }

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
