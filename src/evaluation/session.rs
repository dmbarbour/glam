//! Evaluation demand sessions and machine-visible evaluation contexts.

use std::fmt;
use std::ops::Deref;
#[cfg(test)]
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crate::core::{Builtin, CoreValueFactory, EvaluationFailure, LazyValue, PromisedValue, Value};
use crate::core_net::CoreWaitToken;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::coordinator::{
    self, ClientDemandHandle, ClientDemandOperation, ClientDemandResult, ClientDemandSink,
    ClientDemandSnapshot, DeferredProducer, DeferredWorkReservation, EvaluationSessionId,
    EvaluationTaskHandle, EvaluationTaskId, EvaluationTaskMachine, EvaluationWaitPoll,
    EvaluationWaitTerminal, EvaluationWaitToken, EvaluationWorkCoordinator, InitialTaskDisposition,
    LocalPromiseOwner, PendingTaskPolicy, PreparedEvaluationTask, PromiseProducerObligation,
    ReflectionCancellation, ReflectionTaskResultPolicy, TaskFailureLedger, TaskStatusPublisher,
    WorkDependency,
};
#[cfg(test)]
use super::pump::test_reflection_dependency;
use super::pump::{EvaluationPumpOutcome, prioritized_task_for, pump_demand};
use super::{
    EvaluationDemandState, EvaluationPollContext, EvaluatorStepContext, ReflectionTaskProfile,
    RuntimeObservationEpoch, RuntimeObservationState, allocate_task_id, allocate_wait_token,
    evaluation_failure,
};
#[cfg(test)]
use super::{PendingTestPromiseTask, ReflectionTaskLauncher};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationSessionRun {
    Complete(EvaluationSessionReport),
    Quiescent(EvaluationSessionReport),
    Deadlocked(EvaluationSessionReport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationSessionReport {
    pub(crate) failures: TaskFailureLedger,
    pub(crate) unfinished: Vec<EvaluationUnfinishedTask>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EvaluationTaskRegistryCounts {
    pub(crate) reflection_active: usize,
    pub(crate) reflection_terminal: usize,
    pub(crate) reflection_by_id: usize,
    pub(crate) unacknowledged_failures: usize,
    pub(crate) deferred_active: usize,
    pub(crate) deferred_terminal: usize,
    pub(crate) deferred_by_wait: usize,
    pub(crate) deferred_by_task: usize,
    pub(crate) promises_active: usize,
    pub(crate) promises_terminal: usize,
    pub(crate) owned_promise_waits: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationUnfinishedState {
    Dormant,
    Reserved,
    Queued,
    Running,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationUnfinishedTask {
    pub(crate) task: EvaluationTaskId,
    pub(crate) state: EvaluationUnfinishedState,
    pub(crate) dependency: Option<EvaluationTaskId>,
    pub(crate) dependency_session: Option<EvaluationSessionId>,
    pub(crate) wait: Option<u64>,
    pub(crate) observed_epoch: Option<RuntimeObservationEpoch>,
    pub(crate) error: Option<RuntimeFailureRoot>,
}

#[derive(Clone)]
pub(crate) struct PendingReflectionTask {
    inner: Arc<PendingReflectionTaskInner>,
}

/// One lazily activated `anno refl:...` task reservation.
///
/// Pure evaluation may discover this reservation and retain its stable wait,
/// but launcher construction belongs to the evaluator-step boundary. Every
/// observer may request activation; the first request owns construction and
/// later requests are inexpensive no-ops.
#[derive(Clone)]
pub(crate) struct ReflectionTaskReservation {
    inner: Arc<ReflectionTaskReservationInner>,
}

struct ReflectionTaskReservationInner {
    context: EvalContext,
    handle: EvaluationTaskHandle,
    activation: Option<ReflectionTaskActivation>,
    activated: AtomicBool,
}

struct ReflectionTaskActivation {
    effect: RuntimeValueRoot,
    result_policy: ReflectionTaskResultPolicy,
    task_profile: Arc<super::ReflectionTaskProfile>,
}

impl ReflectionTaskReservation {
    pub(crate) fn handle(&self) -> &EvaluationTaskHandle {
        &self.inner.handle
    }

    pub(crate) fn activate(&self) {
        let Some(activation) = &self.inner.activation else {
            return;
        };
        if self.inner.activated.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.inner.handle.wait.terminal_poll().is_some() {
            return;
        }
        self.inner.context.activate_reflection_task(
            &self.inner.handle,
            &activation.effect,
            activation.result_policy,
            activation.task_profile.clone(),
            None,
            false,
        );
    }
}

impl Drop for ReflectionTaskReservationInner {
    fn drop(&mut self) {
        if self.activation.is_some() && !self.activated.load(Ordering::Acquire) {
            self.context.cancel_reserved_task(&self.handle);
        }
    }
}

struct PendingReflectionTaskInner {
    context: EvalContext,
    handle: EvaluationTaskHandle,
    effect: RuntimeValueRoot,
    activated: AtomicBool,
}

impl PendingReflectionTask {
    pub(crate) fn handle(&self) -> &EvaluationTaskHandle {
        &self.inner.handle
    }

    pub(crate) fn commit(&self, publisher: TaskStatusPublisher, policy: PendingTaskPolicy) {
        if self.inner.activated.swap(true, Ordering::AcqRel) {
            return;
        }
        match policy.disposition() {
            InitialTaskDisposition::Launch => {
                self.inner.context.activate_reflection_task(
                    &self.inner.handle,
                    &self.inner.effect,
                    ReflectionTaskResultPolicy::ReturnValue,
                    self.inner.context.task_profile.clone(),
                    Some(publisher),
                    policy.acknowledges_error(),
                );
            }
            InitialTaskDisposition::Cancel => self
                .inner
                .context
                .cancel_pending_reflection_task(&self.inner.handle, publisher),
        }
    }
}

impl Drop for PendingReflectionTaskInner {
    fn drop(&mut self) {
        if !self.activated.load(Ordering::Acquire) {
            self.context.cancel_reserved_task(&self.handle);
        }
    }
}

/// External ownership lease for one evaluation demand domain.
///
/// Machine-visible contexts retain [`EvaluationDemandState`], not this lease.
/// The strong coordinator route exists only here so dropping the last owner can
/// close and unregister the demand domain.
pub(crate) struct EvaluationSession {
    pub(super) demand: Arc<EvaluationDemandState>,
    pub(super) coordinator: Arc<EvaluationWorkCoordinator>,
}

impl fmt::Debug for EvaluationSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationSession")
            .finish_non_exhaustive()
    }
}

impl Drop for EvaluationSession {
    fn drop(&mut self) {
        self.demand.closed.store(true, Ordering::Release);
        let mut closing = self.coordinator.close_session(self.demand.id);
        for work in std::mem::take(&mut closing.reflection) {
            let failure = evaluation_failure(if work.cancel {
                format!(
                    "promised value's producer task {} was cancelled",
                    work.task.get()
                )
            } else {
                format!(
                    "promised value's producer task {} was abandoned when its evaluation session closed",
                    work.task.get()
                )
            });
            self.coordinator.settle_terminal_work(
                work.id,
                if work.cancel {
                    EvaluationWaitTerminal::Cancelled
                } else {
                    EvaluationWaitTerminal::Abandoned
                },
                failure,
            );
            let mut machine = self.coordinator.retire_reflection(work.id);
            if work.cancel
                && let Some(machine) = &mut machine
            {
                machine.cancel();
            }
            drop(machine);
        }
        let abandoning_deferred = std::mem::take(&mut closing.deferred)
            .into_iter()
            .inspect(|work| {
                self.coordinator.settle_terminal_work(
                    work.id,
                    EvaluationWaitTerminal::Abandoned,
                    evaluation_failure(format!(
                        "promised value's producer task {} was abandoned when its evaluation session closed",
                        work.task.get()
                    )),
                );
            })
            .collect::<Vec<_>>();
        for work in abandoning_deferred {
            self.coordinator.retire_deferred(work.id);
            drop(work.machine);
        }
        closing.finish();
    }
}

impl EvaluationSession {
    fn with_execution_resources(
        coordinator: Arc<EvaluationWorkCoordinator>,
        values: CoreValueFactory,
    ) -> Arc<Self> {
        Self::with_execution_resources_and_default_profile(
            coordinator,
            values,
            Arc::new(ReflectionTaskProfile::unsealed()),
            false,
        )
    }

    fn with_execution_resources_and_default_profile(
        coordinator: Arc<EvaluationWorkCoordinator>,
        values: CoreValueFactory,
        default_reflection_profile: Arc<ReflectionTaskProfile>,
        require_default_reflection_profile: bool,
    ) -> Arc<Self> {
        let demand = Arc::new(EvaluationDemandState {
            id: EvaluationSessionId::from_nonzero(values.ids().evaluation_session()),
            values: values.clone(),
            default_reflection_profile,
            require_default_reflection_profile,
            closed: Arc::new(AtomicBool::new(false)),
            coordinator: Arc::downgrade(&coordinator),
            #[cfg(test)]
            poll_contexts: AtomicUsize::new(0),
        });
        Arc::new(Self {
            demand,
            coordinator,
        })
    }

    fn isolated(values: CoreValueFactory) -> Arc<Self> {
        let coordinator = values.work_coordinator().unwrap_or_else(|| {
            let candidate = EvaluationWorkCoordinator::new(
                &values,
                crate::runtime::RuntimeMutationAdmission::new(),
                RuntimeObservationState::new(),
            );
            values.work_coordinator_or_attach(candidate)
        });
        let session = Self::with_execution_resources(coordinator.clone(), values);
        coordinator.register_demand(&session.demand);
        session
    }

    /// Creates an evaluator owner on a private coordinator which is never
    /// attached to the runtime value factory. Closed bootstrap construction
    /// can reduce ordinary lazy applications without publishing demand or
    /// work into the runtime scheduler being initialized.
    fn private_closed(values: CoreValueFactory) -> Arc<Self> {
        let coordinator = EvaluationWorkCoordinator::new(
            &values,
            crate::runtime::RuntimeMutationAdmission::new(),
            RuntimeObservationState::new(),
        );
        let session = Self::with_execution_resources(coordinator.clone(), values);
        coordinator.register_demand(&session.demand);
        session
    }

    #[cfg(test)]
    pub(crate) fn shared(coordinator: &Arc<EvaluationWorkCoordinator>) -> Arc<Self> {
        let session =
            Self::with_execution_resources(coordinator.clone(), coordinator.test_values());
        coordinator.register_demand(&session.demand);
        session
    }

    #[cfg(test)]
    pub(crate) fn shared_with_values(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        values: CoreValueFactory,
    ) -> Arc<Self> {
        assert_eq!(
            coordinator.runtime_id(),
            values.runtime_id(),
            "test session values must belong to the coordinator runtime"
        );
        let session = Self::with_execution_resources(coordinator.clone(), values);
        coordinator.register_demand(&session.demand);
        session
    }

    pub(crate) fn shared_with_default_profile(
        coordinator: &Arc<EvaluationWorkCoordinator>,
        values: CoreValueFactory,
        default_reflection_profile: Arc<ReflectionTaskProfile>,
    ) -> Arc<Self> {
        let session = Self::with_execution_resources_and_default_profile(
            coordinator.clone(),
            values,
            default_reflection_profile,
            true,
        );
        coordinator.register_demand(&session.demand);
        session
    }
}

/// Cheap per-evaluation handle to one shared demand session.
///
/// Narrower provenance can be added to this handle without duplicating the
/// runtime-owned scheduler or reflection state.
#[derive(Debug, Clone)]
pub(crate) struct EvalContext {
    pub(super) session: Arc<EvaluationDemandState>,
    task_profile: Arc<ReflectionTaskProfile>,
    task: Arc<OnceLock<Result<EvaluationTaskId, Arc<str>>>>,
    local_promise_owner: Option<Arc<LocalPromiseOwner>>,
    scheduled_task: bool,
    waits_for_claimed_tasks: bool,
    originating_task: Option<EvaluationTaskId>,
    #[cfg(test)]
    claimed_task_wait_probe: Option<std::sync::mpsc::Sender<()>>,
}

/// Direct client ownership for an isolated demand context.
///
/// The context itself remains machine-safe and owner-free; this wrapper holds
/// the external lease for callers which do not already have a runtime-managed
/// [`EvaluationSession`].
#[derive(Debug, Clone)]
pub(crate) struct OwnedEvalContext {
    context: EvalContext,
    _owner: Arc<EvaluationSession>,
}

impl Deref for OwnedEvalContext {
    type Target = EvalContext;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl OwnedEvalContext {
    pub(crate) fn new(owner: Arc<EvaluationSession>) -> Self {
        let context = EvalContext::new(&owner);
        Self {
            context,
            _owner: owner,
        }
    }

    #[cfg(test)]
    pub(crate) fn into_parts(self) -> (EvalContext, Arc<EvaluationSession>) {
        (self.context, self._owner)
    }
}

impl EvalContext {
    #[cfg(test)]
    pub(crate) fn standalone() -> OwnedEvalContext {
        Self::isolated(crate::core::test_value_factory())
    }

    pub(crate) fn new(session: &Arc<EvaluationSession>) -> Self {
        let task_profile = session.demand.default_reflection_profile.clone();
        Self {
            session: session.demand.clone(),
            task_profile,
            task: Arc::new(OnceLock::new()),
            local_promise_owner: None,
            scheduled_task: false,
            waits_for_claimed_tasks: false,
            originating_task: None,
            #[cfg(test)]
            claimed_task_wait_probe: None,
        }
    }

    pub(super) fn for_spark(session: Arc<EvaluationDemandState>) -> Self {
        let task_profile = session.default_reflection_profile.clone();
        Self {
            session,
            task_profile,
            task: Arc::new(OnceLock::new()),
            local_promise_owner: None,
            scheduled_task: false,
            waits_for_claimed_tasks: false,
            originating_task: None,
            #[cfg(test)]
            claimed_task_wait_probe: None,
        }
    }

    pub(super) fn for_client_demand(session: Arc<EvaluationDemandState>) -> Self {
        let task_profile = session.default_reflection_profile.clone();
        Self {
            session,
            task_profile,
            task: Arc::new(OnceLock::new()),
            local_promise_owner: None,
            // A client demand is coordinator-owned and must return a blocked
            // poll instead of retaining this Rust stack while its producer
            // waits.
            scheduled_task: true,
            waits_for_claimed_tasks: false,
            originating_task: None,
            #[cfg(test)]
            claimed_task_wait_probe: None,
        }
    }

    pub(crate) fn with_task_profile(
        session: &Arc<EvaluationSession>,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        Self {
            session: session.demand.clone(),
            task_profile,
            task: Arc::new(OnceLock::new()),
            local_promise_owner: None,
            scheduled_task: false,
            waits_for_claimed_tasks: false,
            originating_task: None,
            #[cfg(test)]
            claimed_task_wait_probe: None,
        }
    }

    pub(crate) fn patient_with_task_profile(
        session: &Arc<EvaluationSession>,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        Self {
            waits_for_claimed_tasks: true,
            ..Self::with_task_profile(session, task_profile)
        }
    }

    fn for_task(
        session: Arc<EvaluationDemandState>,
        id: EvaluationTaskId,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        let task = Arc::new(OnceLock::new());
        task.set(Ok(id))
            .expect("fresh task identity cell must be empty");
        Self {
            session,
            task_profile,
            task,
            local_promise_owner: None,
            scheduled_task: true,
            waits_for_claimed_tasks: false,
            originating_task: Some(id),
            #[cfg(test)]
            claimed_task_wait_probe: None,
        }
    }

    fn for_deferred_task(
        session: Arc<EvaluationDemandState>,
        id: EvaluationTaskId,
        originating_task: Option<EvaluationTaskId>,
        task_profile: Arc<ReflectionTaskProfile>,
    ) -> Self {
        let task = Arc::new(OnceLock::new());
        task.set(Ok(id))
            .expect("fresh deferred task identity cell must be empty");
        Self {
            session,
            task_profile,
            task,
            local_promise_owner: None,
            scheduled_task: true,
            waits_for_claimed_tasks: false,
            originating_task,
            #[cfg(test)]
            claimed_task_wait_probe: None,
        }
    }

    pub(crate) fn values(&self) -> &CoreValueFactory {
        &self.session.values
    }

    /// Projects one compatibility root for a callback which must run after
    /// managed access has ended.
    fn clone_root(&self, root: &RuntimeValueRoot) -> Value {
        self.values().with_runtime_value_access(|access| {
            assert_eq!(
                root.runtime_id(),
                self.values().runtime_id(),
                "runtime root and evaluation context must share one value domain"
            );
            root.clone_core_with(&access)
        })
    }

    fn coordinator_for_admission(&self) -> Result<Arc<EvaluationWorkCoordinator>, Arc<str>> {
        if self.session.is_closed() {
            return Err(Arc::from("evaluation demand session is closed"));
        }
        self.coordinator()
            .ok_or_else(|| Arc::from("evaluation demand coordinator expired"))
    }

    pub(super) fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        self.session.coordinator()
    }

    pub(crate) fn current_observation_epoch(&self) -> RuntimeObservationEpoch {
        self.coordinator()
            .expect("evaluation demand coordinator expired")
            .current_observation_epoch()
    }

    /// Creates a zero-worker context in an explicitly selected runtime value
    /// domain. This is for pure closed bootstrap construction and focused
    /// tests; production task services use a runtime-registered session.
    pub(crate) fn isolated(values: CoreValueFactory) -> OwnedEvalContext {
        OwnedEvalContext::new(EvaluationSession::isolated(values))
    }

    /// Creates a closed bootstrap evaluator whose private scheduler cannot
    /// affect runtime readiness or launch work through the runtime's sealed
    /// reflection profile.
    pub(crate) fn private_closed(values: CoreValueFactory) -> OwnedEvalContext {
        OwnedEvalContext::new(EvaluationSession::private_closed(values))
    }

    /// Gives a directly driven effect task a private promise inventory.
    /// Scheduled task contexts use their coordinator work record instead.
    pub(crate) fn for_effect_task(mut self) -> Self {
        if !self.scheduled_task && self.local_promise_owner.is_none() {
            self.local_promise_owner = Some(Arc::new(LocalPromiseOwner::default()));
        }
        self
    }

    pub(crate) fn fail_local_promises(&self, failure: Arc<EvaluationFailure>) {
        if let Some(owner) = &self.local_promise_owner {
            owner.fail_all(failure);
        }
    }

    pub(crate) fn spark(&self, value: Value) {
        // A promise names data whose producer or completed assignment may
        // expose useful work. Nets and the remaining variants are already in
        // WHNF; metadata adds one privileged hidden demand.
        if matches!(
            value,
            Value::Lazy(_) | Value::Promised(_) | Value::Metadata(_)
        ) && !self.session.is_closed()
            && let Some(coordinator) = self.coordinator()
        {
            coordinator.submit_spark(self.session.clone(), value);
        }
    }

    pub(crate) fn demand_whnf(
        &self,
        value: RuntimeValueRoot,
    ) -> Result<ClientDemandHandle, Arc<str>> {
        self.admit_client_demand(ClientDemandOperation::new(value))
    }

    fn admit_client_demand(
        &self,
        operation: ClientDemandOperation,
    ) -> Result<ClientDemandHandle, Arc<str>> {
        let coordinator = self.coordinator_for_admission()?;
        if operation.runtime_id() != coordinator.runtime_id() {
            return Err(Arc::from(
                "client demand operation belongs to another evaluation runtime",
            ));
        }
        let (sink, cell) = ClientDemandSink::pair();
        let work = coordinator.admit_client_demand(self.session.clone(), operation, sink)?;
        Ok(ClientDemandHandle::new(
            coordinator.runtime_id(),
            work,
            &coordinator,
            cell,
        ))
    }

    pub(crate) fn evaluate_whnf(
        &self,
        value: &Value,
    ) -> Result<Value, crate::core::EvaluationHalt> {
        let handle = self
            .demand_whnf(RuntimeValueRoot::new(self.values(), value.clone()))
            .map_err(|error| crate::core::EvaluationHalt::new(error.as_ref()))?;
        match self.drive_client_demand(handle)? {
            ClientDemandResult::Complete(value) => {
                let poll = EvaluationPollContext::for_context(self);
                Ok(poll.evaluate(self, |evaluator| evaluator.project_root(&value)))
            }
            ClientDemandResult::Abandoned => unreachable!(
                "WHNF client demand must return a value or a propagated evaluation failure"
            ),
            ClientDemandResult::Failed(_) | ClientDemandResult::Killed(_) => {
                unreachable!("client failures are returned by drive_client_demand")
            }
        }
    }

    /// Constructs one semantic builtin call inside a callback-free value
    /// access region. The returned value is durable, but the access carrier
    /// and its mutator cannot escape the higher-ranked callback.
    pub(crate) fn compose_builtin(&self, builtin: Builtin, arguments: Vec<Value>) -> Value {
        self.values().with_runtime_value_access(|_access| {
            Value::builtin_call(self.values(), builtin, arguments)
        })
    }

    /// Composes and demands a builtin call without carrying managed access
    /// through scheduler pumping, waits, reflection, or host callbacks.
    pub(crate) fn evaluate_builtin_whnf(
        &self,
        builtin: Builtin,
        arguments: Vec<Value>,
    ) -> Result<Value, crate::core::EvaluationHalt> {
        let value = self.compose_builtin(builtin, arguments);
        self.evaluate_whnf(&value)
    }

    fn drive_client_demand(
        &self,
        mut handle: ClientDemandHandle,
    ) -> Result<ClientDemandResult, crate::core::EvaluationHalt> {
        let coordinator = self
            .coordinator_for_admission()
            .map_err(|error| crate::core::EvaluationHalt::new(error.as_ref()))?;

        loop {
            if let Some(result) = handle.poll() {
                return terminal_client_demand_result(result);
            }
            if let Some(claimed) = coordinator.claim_client_demand(handle.work) {
                coordinator.poll_claimed_client_demand(claimed);
                continue;
            }

            let generation = coordinator.work_generation();
            let Some(snapshot) = coordinator.client_demand_snapshot(handle.work) else {
                // Retirement removes the coordinator record before publishing
                // its sink. Waiting on the cell closes that intentionally
                // tiny handoff without racing a completion notification.
                return terminal_client_demand_result(handle.wait());
            };
            match snapshot {
                ClientDemandSnapshot::Queued => continue,
                ClientDemandSnapshot::Running => {
                    if handle.poll().is_none() && coordinator.work_generation() == generation {
                        coordinator.wait_for_change(generation);
                    }
                }
                ClientDemandSnapshot::Blocked {
                    dependency,
                    subscription_epoch,
                } => {
                    if let Some(wait) = dependency.producer_wait() {
                        if let Some(task) = prioritized_task_for(&coordinator, &wait)
                            && let Some(work) = coordinator.claim_task(task)
                        {
                            coordinator.poll_claimed_task(work);
                            continue;
                        }
                        if coordinator.target_has_running_producer(&wait) {
                            if handle.poll().is_none()
                                && coordinator.work_generation() == generation
                            {
                                coordinator.wait_for_change(generation);
                            }
                            continue;
                        }
                    }

                    // A dependency chain may be blocked while another task or
                    // worker owns the state transition which will disturb it.
                    // Use the runtime pump's stability boundary before
                    // abandoning this client demand: run unrelated useful
                    // lifecycle work, release parked best-effort sparks, and
                    // wait for worker-owned progress. A client-visible blocked
                    // halt is valid only after none of those routes remains.
                    if coordinator.poll_runtime_work() {
                        continue;
                    }
                    if coordinator.abandon_quiescent_sparks() != 0 {
                        continue;
                    }
                    let runtime = coordinator.runtime_pump_snapshot();
                    if runtime.useful_ready || runtime.abandonable_sparks {
                        continue;
                    }
                    if runtime.progress_owned {
                        if handle.poll().is_none() && coordinator.work_generation() == generation {
                            coordinator.wait_for_change(generation);
                        }
                        continue;
                    }

                    let Some(dependency) = handle.abandon_if_stably_blocked(subscription_epoch)
                    else {
                        continue;
                    };
                    return Err(client_demand_halt(dependency));
                }
            }
        }
    }

    pub(crate) fn runs_scheduled_task(&self) -> bool {
        self.scheduled_task
    }

    #[cfg(test)]
    pub(crate) fn poll_context_count(&self) -> usize {
        self.session.poll_contexts.load(Ordering::Acquire)
    }

    pub(crate) fn waits_for_claimed_tasks(&self) -> bool {
        self.waits_for_claimed_tasks
    }

    #[cfg(test)]
    pub(crate) fn with_claimed_task_wait_probe(
        mut self,
        probe: std::sync::mpsc::Sender<()>,
    ) -> Self {
        self.claimed_task_wait_probe = Some(probe);
        self
    }

    /// Waits for one scheduler change only while the target has a producer
    /// claimed by another thread.
    ///
    /// Rechecking against the runtime work generation prevents a producer
    /// release between [`Self::pump_wait`] and this call from becoming a lost
    /// wakeup.
    pub(crate) fn wait_for_claimed_task(&self, target: &EvaluationWaitToken) {
        if target.owner_id() != self.session.id {
            return;
        }
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        let generation = coordinator.work_generation();
        if !coordinator.target_has_running_producer(target) {
            return;
        }
        #[cfg(test)]
        if let Some(probe) = &self.claimed_task_wait_probe {
            let _ = probe.send(());
        }
        coordinator.wait_for_change(generation);
    }

    /// Waits for one scheduler transition when an exact dependency chain ends
    /// at a task with a coordinator-indexed broad observation.
    ///
    /// This is narrower than treating every live task as future progress: a
    /// pure wait cycle has no observed epoch and remains `NoProgress` for
    /// quiescence analysis.
    pub(crate) fn wait_for_observed_dependency_progress(
        &self,
        target: &EvaluationWaitToken,
    ) -> bool {
        let Some(coordinator) = self.coordinator() else {
            return false;
        };
        let generation = coordinator.work_generation();
        if !coordinator.dependency_observes_runtime(target) {
            return false;
        }
        if coordinator.work_generation() == generation {
            coordinator.wait_for_change(generation);
        }
        true
    }

    pub(crate) fn observes_as_task(&self, task: EvaluationTaskId) -> bool {
        self.originating_task == Some(task)
            || matches!(self.task.get(), Some(Ok(current)) if *current == task)
    }

    pub(crate) fn lazy_task<F>(
        &self,
        lazy: &LazyValue,
        build: F,
    ) -> Result<EvaluationWaitToken, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Box<dyn EvaluationTaskMachine>,
    {
        self.deferred_task(DeferredProducer::Lazy(lazy.root()), build)
    }

    pub(crate) fn promise_task<F>(
        &self,
        promise: &PromisedValue,
        build: F,
    ) -> Result<EvaluationWaitToken, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Box<dyn EvaluationTaskMachine>,
    {
        self.deferred_task(DeferredProducer::Promise(promise.root()), build)
    }

    fn deferred_task<F>(
        &self,
        producer: DeferredProducer,
        build: F,
    ) -> Result<EvaluationWaitToken, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Box<dyn EvaluationTaskMachine>,
    {
        let coordinator = self.coordinator_for_admission()?;
        let deferred = producer.id();
        if let Some(wait) = coordinator.deferred_wait(deferred) {
            return Ok(wait);
        }

        let id = allocate_task_id(self.values())?;
        let wait = allocate_wait_token(&self.session, id)?;
        let originating_task = self
            .originating_task
            .or_else(|| self.task.get().and_then(|task| task.as_ref().ok()).copied());
        let machine = build(Self::for_deferred_task(
            self.session.clone(),
            id,
            originating_task,
            self.task_profile.clone(),
        ));
        match coordinator.reserve_deferred(&self.session, id, wait.clone(), producer, machine)? {
            DeferredWorkReservation::Existing(wait) => return Ok(wait),
            DeferredWorkReservation::New => {}
        }
        Ok(wait)
    }

    #[cfg(test)]
    pub(crate) fn install_reflection_launcher(
        &self,
        launcher: Arc<dyn ReflectionTaskLauncher>,
    ) -> Result<(), Arc<str>> {
        self.task_profile.seal(launcher.clone())?;
        if Arc::ptr_eq(&self.task_profile, &self.session.default_reflection_profile) {
            return Ok(());
        }
        self.session.default_reflection_profile.seal(launcher)
    }

    #[cfg(test)]
    pub(crate) fn with_new_task(&self) -> Result<Self, Arc<str>> {
        let context = Self {
            session: self.session.clone(),
            task_profile: self.task_profile.clone(),
            task: Arc::new(OnceLock::new()),
            local_promise_owner: None,
            scheduled_task: false,
            waits_for_claimed_tasks: false,
            originating_task: None,
            claimed_task_wait_probe: None,
        };
        let task = context.task_id()?;
        Ok(Self {
            originating_task: Some(task),
            ..context
        })
    }

    #[cfg(test)]
    pub(crate) fn task_owned_promises(
        &self,
        labels: impl IntoIterator<Item = Arc<str>>,
    ) -> Result<(Vec<PromisedValue>, EvaluationTaskHandle, EvalContext), Arc<str>> {
        let labels = labels.into_iter().collect::<Vec<_>>();
        let promises = Arc::new(Mutex::new(None));
        let output = promises.clone();
        let owner_context = Arc::new(Mutex::new(None));
        let owner_output = owner_context.clone();
        let task = self.schedule_task(move |context| {
            let owned = labels
                .into_iter()
                .map(|label| PromisedValue::fixpoint(&context, label))
                .collect::<Result<Vec<_>, _>>()?;
            *output.lock().expect("test promise output was poisoned") = Some(owned);
            *owner_output
                .lock()
                .expect("test promise owner output was poisoned") = Some(context);
            Ok(Box::new(PendingTestPromiseTask))
        })?;
        let promises = promises
            .lock()
            .expect("test promise output was poisoned")
            .take()
            .expect("test task construction must publish its promises");
        let owner_context = owner_context
            .lock()
            .expect("test promise owner output was poisoned")
            .take()
            .expect("test task construction must publish its owner context");
        Ok((promises, task, owner_context))
    }

    #[cfg(test)]
    pub(crate) fn task_owned_promise(
        &self,
        label: impl Into<Arc<str>>,
    ) -> Result<(PromisedValue, EvaluationTaskHandle, EvalContext), Arc<str>> {
        let (mut promises, task, owner) = self.task_owned_promises([label.into()])?;
        Ok((
            promises.pop().expect("one promise was requested"),
            task,
            owner,
        ))
    }

    pub(crate) fn task_id(&self) -> Result<EvaluationTaskId, Arc<str>> {
        self.task
            .get_or_init(|| allocate_task_id(self.values()))
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn session_id(&self) -> EvaluationSessionId {
        self.session.id
    }

    pub(crate) fn register_promise(
        &self,
        promise: &PromisedValue,
    ) -> Result<Arc<PromiseProducerObligation>, Arc<str>> {
        if self.session.is_closed() {
            return Err(Arc::from("evaluation demand session is closed"));
        }
        let owner = self.task_id()?;
        let wait = allocate_wait_token(&self.session, owner)?;
        if self.scheduled_task {
            let coordinator = self.coordinator_for_admission()?;
            coordinator.register_task_promise(owner, wait, promise)
        } else if let Some(local_owner) = &self.local_promise_owner {
            let producer = Arc::new(PromiseProducerObligation::local_owned(
                owner,
                &wait,
                promise.id(),
                local_owner,
            ));
            local_owner.register(promise.root(), wait);
            Ok(producer)
        } else {
            Err(format!(
                "task {} has no active work record for its promise",
                owner.get()
            )
            .into())
        }
    }

    /// Registers an executable task whose concrete specialization remains
    /// hidden behind [`EvaluationTaskMachine`]. Construction happens outside
    /// the coordinator lock, so host snapshots and evaluator work may safely
    /// use this same session.
    #[cfg(test)]
    pub(crate) fn schedule_machine<F>(
        &self,
        lifecycle: Option<TaskStatusPublisher>,
        build: F,
    ) -> Result<EvaluationTaskHandle, Arc<EvaluationFailure>>
    where
        F: FnOnce(EvalContext) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>>,
    {
        self.prepare_machine(lifecycle, build)
            .map(PreparedEvaluationTask::activate)
    }

    /// Constructs a coordinator-owned task without making it runnable.
    ///
    /// The returned reservation is already a complete runtime root. A host
    /// may use guarded activation to publish another runtime transition in
    /// the same settlement-exclusion interval before exposing either fact.
    pub(crate) fn prepare_machine<F>(
        &self,
        lifecycle: Option<TaskStatusPublisher>,
        build: F,
    ) -> Result<PreparedEvaluationTask, Arc<EvaluationFailure>>
    where
        F: FnOnce(EvalContext) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>>,
    {
        let coordinator = self
            .coordinator_for_admission()
            .map_err(|error| evaluation_failure(error.as_ref()))?;
        let id =
            allocate_task_id(self.values()).map_err(|error| evaluation_failure(error.as_ref()))?;
        let wait = allocate_wait_token(&self.session, id)
            .map_err(|error| evaluation_failure(error.as_ref()))?;
        let context = Self::for_task(self.session.clone(), id, self.task_profile.clone());
        let work = coordinator
            .reserve_reflection(&self.session, id, wait.clone())
            .map_err(|error| evaluation_failure(error.as_ref()))?;
        let machine = match build(context) {
            Ok(machine) => machine,
            Err(error) => {
                // This helper reports construction failure directly to its
                // Rust caller; it never returns a launched task handle whose
                // failure would need runtime reporting.
                coordinator.acknowledge_task_failure(self.session.id, id);
                assert!(
                    coordinator.terminalize_reserved_reflection(work),
                    "failed test task construction must terminalize its reservation"
                );
                coordinator.settle_terminal_work(
                    work,
                    EvaluationWaitTerminal::Failed(RuntimeFailureRoot::new(
                        self.values(),
                        error.clone(),
                    )),
                    error.clone(),
                );
                drop(coordinator.retire_reflection(work));
                return Err(error);
            }
        };
        if let Some(lifecycle) = lifecycle {
            assert!(
                coordinator.attach_reflection_lifecycle_publisher(work, lifecycle),
                "fresh reflection reservation must accept its lifecycle publisher"
            );
        }
        coordinator
            .install_reflection_machine(work, machine)
            .unwrap_or_else(|_| panic!("fresh reflection reservation must accept its machine"));
        Ok(PreparedEvaluationTask::new(
            coordinator.clone(),
            EvaluationTaskHandle::new(&coordinator, self.session.id, id, work, wait),
        ))
    }

    #[cfg(test)]
    pub(crate) fn schedule_task<F>(&self, build: F) -> Result<EvaluationTaskHandle, Arc<str>>
    where
        F: FnOnce(EvalContext) -> Result<Box<dyn EvaluationTaskMachine>, Arc<str>>,
    {
        self.schedule_machine(None, |context| {
            build(context)
                .map_err(|error| evaluation_failure(format!("task construction failed: {error}")))
        })
        .map_err(|error| Arc::from(error.to_string()))
    }

    pub(super) fn reserve_task(&self) -> Result<EvaluationTaskHandle, Arc<str>> {
        let coordinator = self.coordinator_for_admission()?;
        let id = allocate_task_id(self.values())?;
        let wait = allocate_wait_token(&self.session, id)?;
        let work = coordinator.reserve_reflection(&self.session, id, wait.clone())?;
        Ok(EvaluationTaskHandle::new(
            &coordinator,
            self.session.id,
            id,
            work,
            wait,
        ))
    }

    fn activate_reflection_task(
        &self,
        handle: &EvaluationTaskHandle,
        effect: &RuntimeValueRoot,
        result_policy: ReflectionTaskResultPolicy,
        task_profile: Arc<ReflectionTaskProfile>,
        status_publisher: Option<TaskStatusPublisher>,
        error_acknowledged: bool,
    ) {
        let Ok(coordinator) = self.coordinator_for_admission() else {
            return;
        };
        if error_acknowledged {
            coordinator.acknowledge_task_failure(handle.session_id(), handle.id());
        }
        if let Some(status_publisher) = status_publisher
            && !coordinator.attach_reflection_status_publisher(handle.work, status_publisher)
        {
            return;
        }
        // Clone under matching managed access, then release that access before
        // the launcher invokes reflection-owned construction callbacks.
        let effect = self.clone_root(effect);
        let result = task_profile
            .launcher()
            .ok_or_else(|| {
                Arc::new(EvaluationFailure::message(
                    "reflection task profile is not sealed",
                ))
            })
            .and_then(|launcher| {
                launcher.build(
                    Self::for_task(self.session.clone(), handle.id(), task_profile.clone()),
                    effect,
                    result_policy,
                )
            });
        match result {
            Ok(machine) => {
                if coordinator
                    .install_reflection_machine(handle.work, machine)
                    .is_ok()
                {
                    // A concurrent cancellation may already own terminal
                    // cleanup; activation then returns false.
                    let _ = coordinator.activate_reflection(handle.work);
                }
            }
            Err(error) => {
                let promise_failure = error.clone();
                if coordinator.terminalize_reserved_reflection(handle.work) {
                    coordinator.settle_terminal_work(
                        handle.work,
                        EvaluationWaitTerminal::Failed(RuntimeFailureRoot::new(
                            self.values(),
                            error,
                        )),
                        promise_failure,
                    );
                    drop(coordinator.retire_reflection(handle.work));
                }
            }
        }
    }

    pub(super) fn cancel_reserved_task(&self, handle: &EvaluationTaskHandle) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        let _ = coordinator.discard_reserved_reflection(handle.work);
    }

    fn cancel_pending_reflection_task(
        &self,
        handle: &EvaluationTaskHandle,
        status_publisher: TaskStatusPublisher,
    ) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        assert!(
            coordinator.attach_reflection_status_publisher(handle.work, status_publisher),
            "a committed pending task must remain reserved"
        );
        let cancellation = coordinator.request_reflection_cancellation(handle.work);
        assert_eq!(
            cancellation,
            ReflectionCancellation::Terminalize,
            "a committed pre-launch cancellation must own its reservation"
        );
        coordinator.settle_terminal_work(
            handle.work,
            EvaluationWaitTerminal::Cancelled,
            evaluation_failure("reflection fixpoint producer was cancelled"),
        );
        drop(coordinator.retire_reflection(handle.work));
    }

    pub(crate) fn reserve_reflection_task(
        &self,
        effect: Value,
    ) -> Result<PendingReflectionTask, Arc<str>> {
        self.coordinator_for_admission()?;
        if !self.task_profile.is_sealed() {
            return Err(Arc::from(
                "current task has no sealed reflection task profile",
            ));
        }
        Ok(PendingReflectionTask {
            inner: Arc::new(PendingReflectionTaskInner {
                context: self.clone(),
                handle: self.reserve_task()?,
                effect: RuntimeValueRoot::new(self.values(), effect),
                activated: AtomicBool::new(false),
            }),
        })
    }

    pub(crate) fn reserve_reflection_activation(
        &self,
        effect: Value,
        result_policy: ReflectionTaskResultPolicy,
    ) -> Result<ReflectionTaskReservation, Arc<str>> {
        let coordinator = self.coordinator_for_admission()?;
        let default_profile = self.session.default_reflection_profile.clone();
        if default_profile.is_sealed() {
            let handle = self.reserve_task()?;
            return Ok(ReflectionTaskReservation {
                inner: Arc::new(ReflectionTaskReservationInner {
                    context: self.clone(),
                    handle,
                    activation: Some(ReflectionTaskActivation {
                        effect: RuntimeValueRoot::new(self.values(), effect),
                        result_policy,
                        task_profile: default_profile,
                    }),
                    activated: AtomicBool::new(false),
                }),
            });
        }

        if self.session.require_default_reflection_profile {
            return Err(Arc::from(
                "evaluation runtime default reflection task profile is not sealed",
            ));
        }

        // Focused evaluator tests and internal clients may intentionally use a
        // bare session. Preserve an inspectable wait record for them; ordinary
        // Assembler sessions always install a launcher.
        let id = allocate_task_id(self.values())?;
        let wait = allocate_wait_token(&self.session, id)?;
        let work = coordinator.register_dormant_reflection(&self.session, id, wait.clone())?;
        Ok(ReflectionTaskReservation {
            inner: Arc::new(ReflectionTaskReservationInner {
                context: self.clone(),
                handle: EvaluationTaskHandle::new(&coordinator, self.session.id, id, work, wait),
                activation: None,
                activated: AtomicBool::new(true),
            }),
        })
    }

    pub(crate) fn poll_reflection_task(&self, task: &EvaluationTaskHandle) -> EvaluationWaitPoll {
        self.poll_wait(&task.wait)
    }

    /// Parks a host driver until the coordinator changes after a pending task
    /// observation. Exact dependency completion and broad runtime observation
    /// both advance this generation, so the caller need not choose a wake
    /// source and cannot lose a completion between its poll and park.
    pub(crate) fn wait_for_task_change(&self, task: &EvaluationTaskHandle) {
        let Some(coordinator) = self.coordinator() else {
            return;
        };
        let generation = coordinator.work_generation();
        if matches!(
            self.poll_reflection_task(task),
            EvaluationWaitPoll::Pending(_)
        ) && !coordinator.session_has_ready_task(self.session.id)
            && coordinator.work_generation() == generation
        {
            coordinator.wait_for_change(generation);
        }
    }

    pub(crate) fn has_ready_session_task(&self) -> bool {
        self.coordinator()
            .is_some_and(|coordinator| coordinator.session_has_ready_task(self.session.id))
    }

    #[cfg(test)]
    pub(crate) fn attach_task_status_publisher(
        &self,
        task: &EvaluationTaskHandle,
        publisher: TaskStatusPublisher,
    ) -> bool {
        self.coordinator().is_some_and(|coordinator| {
            coordinator.attach_reflection_status_publisher(task.work, publisher)
        })
    }

    pub(crate) fn acknowledge_task_failure(
        &self,
        owner: EvaluationSessionId,
        task: EvaluationTaskId,
    ) {
        if let Some(coordinator) = self.coordinator() {
            coordinator.acknowledge_task_failure(owner, task);
        }
    }

    pub(crate) fn poll_wait(&self, wait: &EvaluationWaitToken) -> EvaluationWaitPoll {
        if let Some(terminal) = wait.terminal_poll() {
            return terminal;
        }
        if self
            .local_promise_owner
            .as_ref()
            .is_some_and(|owner| owner.contains_wait(wait))
        {
            return EvaluationWaitPoll::Pending(wait.clone());
        }
        if self
            .coordinator()
            .is_some_and(|coordinator| coordinator.producer_for_wait(wait).is_some())
        {
            return EvaluationWaitPoll::Pending(wait.clone());
        }
        if let Some(terminal) = wait.terminal_poll() {
            return terminal;
        }
        EvaluationWaitPoll::Failed(RuntimeFailureRoot::new(
            self.values(),
            evaluation_failure("evaluation wait token is no longer registered"),
        ))
    }

    pub(crate) fn pump_wait(
        &self,
        wait: &EvaluationWaitToken,
        step_budget: usize,
    ) -> EvaluationPumpOutcome {
        let Some(coordinator) = self.coordinator() else {
            return if wait.terminal_poll().is_some() {
                EvaluationPumpOutcome::TargetReady
            } else {
                EvaluationPumpOutcome::NoProgress
            };
        };
        pump_demand(&coordinator, self.session.id, self, wait, step_budget)
    }

    /// Runs every executable task until all are terminal or one complete pass
    /// leaves every unfinished task unchanged.
    pub(crate) fn run_until_quiescent(&self) -> EvaluationSessionRun {
        self.session.run_until_quiescent()
    }

    #[cfg(test)]
    pub(crate) fn complete_wait(&self, wait: &EvaluationWaitToken) {
        self.complete_wait_with_value(wait, crate::core::keys::unit_value());
    }

    #[cfg(test)]
    pub(crate) fn complete_wait_with_value(&self, wait: &EvaluationWaitToken, value: Value) {
        let coordinator = self
            .coordinator()
            .expect("test wait must retain its coordinator");
        let target = wait.clone();
        let wait = test_reflection_dependency(&coordinator, wait);
        let work = coordinator
            .reflection_work_for_wait(&wait)
            .expect("test task must belong to this runtime");
        assert!(coordinator.terminalize_reflection(work));
        coordinator.settle_terminal_work(
            work,
            EvaluationWaitTerminal::Complete(RuntimeValueRoot::new(&self.session.values, value)),
            evaluation_failure("reflection task completed without fulfilling its fixpoint"),
        );
        drop(coordinator.retire_reflection(work));
        while matches!(
            self.pump_wait(&target, 256),
            EvaluationPumpOutcome::BudgetExhausted
        ) {}
    }

    #[cfg(test)]
    pub(crate) fn fail_wait(&self, wait: &EvaluationWaitToken, error: impl Into<Arc<str>>) {
        self.fail_wait_with_failure(wait, evaluation_failure(error.into()));
    }

    #[cfg(test)]
    pub(crate) fn fail_wait_with_failure(
        &self,
        wait: &EvaluationWaitToken,
        failure: Arc<EvaluationFailure>,
    ) {
        let coordinator = self
            .coordinator()
            .expect("test wait must retain its coordinator");
        let target = wait.clone();
        let wait = test_reflection_dependency(&coordinator, wait);
        let work = coordinator
            .reflection_work_for_wait(&wait)
            .expect("test task must belong to this runtime");
        assert!(coordinator.terminalize_reflection(work));
        coordinator.settle_terminal_work(
            work,
            EvaluationWaitTerminal::Failed(RuntimeFailureRoot::new(self.values(), failure.clone())),
            failure,
        );
        drop(coordinator.retire_reflection(work));
        while matches!(
            self.pump_wait(&target, 256),
            EvaluationPumpOutcome::BudgetExhausted
        ) {}
    }

    #[cfg(test)]
    pub(crate) fn reflection_task_count(&self) -> usize {
        self.task_registry_counts().reflection_active
    }

    #[cfg(test)]
    pub(crate) fn deferred_task_count(&self) -> usize {
        self.task_registry_counts().deferred_active
    }

    #[cfg(test)]
    pub(crate) fn task_registry_counts(&self) -> EvaluationTaskRegistryCounts {
        let coordinator = self
            .coordinator()
            .expect("test demand must retain its coordinator");
        let deferred_counts = coordinator.deferred_counts(self.session.id);
        let promise_count = coordinator.task_promise_count(self.session.id);
        let (reflection_active, reflection_by_id) = coordinator.reflection_counts(self.session.id);
        EvaluationTaskRegistryCounts {
            reflection_active,
            reflection_terminal: 0,
            reflection_by_id,
            unacknowledged_failures: coordinator.failure_snapshot(self.session.id).size(),
            deferred_active: deferred_counts.0,
            deferred_terminal: 0,
            deferred_by_wait: deferred_counts.1,
            deferred_by_task: deferred_counts.2,
            promises_active: promise_count,
            promises_terminal: 0,
            owned_promise_waits: promise_count,
        }
    }

    #[cfg(test)]
    pub(crate) fn lazy_failure(&self, lazy: &LazyValue) -> Option<Arc<EvaluationFailure>> {
        lazy.cached().and_then(Result::err)
    }

    #[cfg(test)]
    pub(crate) fn promise_failure(
        &self,
        promise: &PromisedValue,
    ) -> Option<Arc<EvaluationFailure>> {
        promise.assignment().and_then(Result::err)
    }

    pub(crate) fn lazy_failure_for_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<Arc<EvaluationFailure>> {
        match wait.terminal_poll() {
            Some(EvaluationWaitPoll::Failed(failure)) => Some(failure.into_failure()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn shares_session_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.session, &other.session)
    }
}

fn terminal_client_demand_result(
    result: ClientDemandResult,
) -> Result<ClientDemandResult, crate::core::EvaluationHalt> {
    match result {
        ClientDemandResult::Failed(failure) | ClientDemandResult::Killed(failure) => {
            Err(crate::core::EvaluationHalt::failure(failure.into_failure()))
        }
        ClientDemandResult::Abandoned => Err(crate::core::EvaluationHalt::new(
            "client evaluation demand was abandoned",
        )),
        complete => Ok(complete),
    }
}

pub(super) fn client_demand_halt_poll(
    context: &EvaluatorStepContext<'_>,
    halt: crate::core::EvaluationHalt,
) -> coordinator::ClientDemandPoll {
    if let Some(wait) = halt.blocked_on() {
        coordinator::ClientDemandPoll::Blocked(WorkDependency::Wait(wait.0))
    } else if let Some(promise) = halt.unassigned_promise() {
        coordinator::ClientDemandPoll::Blocked(WorkDependency::Promise(promise.root()))
    } else {
        coordinator::ClientDemandPoll::Failed(context.root_failure(halt.into_permanent_failure()))
    }
}

fn client_demand_halt(dependency: WorkDependency) -> crate::core::EvaluationHalt {
    match dependency {
        WorkDependency::Wait(wait) => crate::core::EvaluationHalt::blocked(CoreWaitToken(wait)),
        WorkDependency::Promise(promise) => {
            crate::core::EvaluationHalt::unassigned(PromisedValue::from_root(&promise))
        }
        #[cfg(test)]
        WorkDependency::Test(_) => crate::core::EvaluationHalt::new(
            "client evaluation blocked on a synthetic test dependency",
        ),
    }
}
