//! Session-scoped capabilities threaded through semantic evaluation.
//!
//! The runtime supplies task and wait identity, value provenance, and the
//! authoritative reflection-task lifecycle. The runtime coordinator owns
//! opaque reflection and deferred machines with their lifecycle records and
//! task-failure ledger. This facade retains only the demand/profile bridge and
//! crate-private re-exports: sessions own admission policy, the pump
//! orchestrates detached claims, and coordinator children own lifecycle-local
//! state transitions. Reflection specializations remain outside this module
//! behind a small type-erased task-machine boundary.

use std::fmt;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use crate::core::{CoreValueFactory, EvaluationFailure, Value};

#[allow(
    dead_code,
    reason = "I3A establishes scoped authority before I3B migrates production evaluator substeps"
)]
mod access;
#[cfg(test)]
mod access_inventory;
mod coordinator;
mod executor;
mod observation;
mod pump;
mod session;
pub(crate) use access::{EvaluationPollContext, EvaluatorStepContext};
pub(crate) use coordinator::{
    CompletionSubscriptionOutcome, CompletionSubscriptions, CompletionWake, EvaluationExitBlock,
    EvaluationMachinePoll, EvaluationSessionId, EvaluationTaskBlock, EvaluationTaskCancellation,
    EvaluationTaskHandle, EvaluationTaskId, EvaluationTaskMachine, EvaluationTaskStatus,
    EvaluationWaitPoll, EvaluationWaitToken, EvaluationWorkCoordinator, ExitIntent,
    PendingTaskPolicy, PreparedEvaluationTask, PromiseProducerObligation,
    PromiseProducerPublication, ReflectionTaskResultPolicy, RuntimeCoordinatorReadiness,
    RuntimeDeadlockWorkSnapshot, RuntimeDependencySnapshot, RuntimeExitSnapshot,
    RuntimeWorkKindSnapshot, RuntimeWorkStateSnapshot, TaskFailureLedger, TaskStatusPublisher,
    TaskStatusWake, ValidatedRuntimeSettlementPlan, WakeRegistration, WorkDependency,
};
#[cfg(test)]
use coordinator::{ReflectionWorkSnapshot, test_wake_registration};
pub(crate) use executor::EvaluationExecutor;
pub(crate) use observation::{RuntimeObservationEpoch, RuntimeObservationState};
pub(crate) use pump::EvaluationPumpOutcome;
pub(crate) use session::{
    EvalContext, EvaluationSession, EvaluationSessionReport, EvaluationSessionRun,
    PendingReflectionTask, ReflectionTaskReservation,
};
#[cfg(test)]
pub(crate) use session::{EvaluationTaskRegistryCounts, OwnedEvalContext};

#[cfg(test)]
pub(crate) fn test_execution_resources(
    worker_count: usize,
) -> Result<(Arc<EvaluationWorkCoordinator>, Arc<EvaluationExecutor>), Arc<str>> {
    let values = CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let admission = crate::runtime::RuntimeMutationAdmission::new();
    let coordinator = EvaluationWorkCoordinator::new_for_test(values, admission);
    let executor = EvaluationExecutor::new(worker_count, &coordinator)?;
    Ok((coordinator, executor))
}

fn allocate_task_id(values: &CoreValueFactory) -> Result<EvaluationTaskId, Arc<str>> {
    values
        .ids()
        .evaluation_task()
        .map(EvaluationTaskId::from_nonzero)
}

fn allocate_wait_token(
    session: &Arc<EvaluationDemandState>,
    producer: EvaluationTaskId,
) -> Result<EvaluationWaitToken, Arc<str>> {
    let id = session.values.ids().evaluation_wait()?;
    Ok(EvaluationWaitToken::new(
        id,
        session.values.runtime_id(),
        session.id,
        producer,
        CompletionSubscriptions::for_wait(
            session.values.runtime_id(),
            id,
            session.values.work_coordinator_binding(),
        ),
    ))
}

fn evaluation_failure(message: impl AsRef<str>) -> Arc<EvaluationFailure> {
    Arc::new(EvaluationFailure::message(message))
}

#[cfg(test)]
struct PendingTestPromiseTask;

#[cfg(test)]
impl EvaluationTaskMachine for PendingTestPromiseTask {
    fn poll(
        &mut self,
        _context: &crate::evaluation::EvaluationPollContext,
        _step_budget: usize,
    ) -> EvaluationMachinePoll {
        EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
            dependency: None,
            observed_epoch: Some(RuntimeObservationEpoch::from_raw(7)),
            error: None,
        })
    }
}

pub(crate) trait ReflectionTaskLauncher: Send + Sync {
    fn build(
        &self,
        context: EvalContext,
        effect: Value,
        result_policy: ReflectionTaskResultPolicy,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>>;
}

/// One immutable, type-erased reflection task host profile.
///
/// The launcher closes over the profile's effect specialization, environment,
/// diagnostic destination, and shared host resources. Runtime-default and
/// current-task profiles use the same representation but have different
/// selection rules.
pub(crate) struct ReflectionTaskProfile {
    launcher: OnceLock<Arc<dyn ReflectionTaskLauncher>>,
}

impl fmt::Debug for ReflectionTaskProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReflectionTaskProfile")
            .field("sealed", &self.launcher.get().is_some())
            .finish()
    }
}

impl ReflectionTaskProfile {
    pub(crate) fn unsealed() -> Self {
        Self {
            launcher: OnceLock::new(),
        }
    }

    pub(crate) fn sealed(launcher: Arc<dyn ReflectionTaskLauncher>) -> Self {
        let profile = Self::unsealed();
        profile
            .seal(launcher)
            .expect("a fresh reflection task profile must be unsealed");
        profile
    }

    pub(crate) fn seal(&self, launcher: Arc<dyn ReflectionTaskLauncher>) -> Result<(), Arc<str>> {
        self.launcher
            .set(launcher)
            .map_err(|_| Arc::from("reflection task profile is already sealed"))
    }

    fn launcher(&self) -> Option<&Arc<dyn ReflectionTaskLauncher>> {
        self.launcher.get()
    }

    pub(crate) fn is_sealed(&self) -> bool {
        self.launcher.get().is_some()
    }
}

pub(crate) struct EvaluationDemandState {
    id: EvaluationSessionId,
    values: CoreValueFactory,
    default_reflection_profile: Arc<ReflectionTaskProfile>,
    require_default_reflection_profile: bool,
    closed: Arc<AtomicBool>,
    coordinator: Weak<EvaluationWorkCoordinator>,
    #[cfg(test)]
    poll_contexts: AtomicUsize,
}

impl fmt::Debug for EvaluationDemandState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationDemandState")
            .field("id", &self.id)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl EvaluationDemandState {
    fn coordinator(&self) -> Option<Arc<EvaluationWorkCoordinator>> {
        self.coordinator.upgrade()
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn closed_run_report(&self) -> EvaluationSessionRun {
        let failures = self
            .coordinator()
            .map_or_else(TaskFailureLedger::new_sync, |coordinator| {
                coordinator.failure_snapshot(self.id)
            });
        EvaluationSessionRun::Complete(EvaluationSessionReport {
            failures,
            unfinished: Vec::new(),
        })
    }
}
#[cfg(test)]
mod tests;
