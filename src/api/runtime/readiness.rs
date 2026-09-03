use std::fmt;
use std::sync::Arc;

use rpds::RedBlackTreeMapSync;

use super::{EvaluationRuntime, RuntimeDeliveryFailureSnapshot};
use crate::api::{Diagnostic, Error, ReasoningFailure, Value, Values};
use crate::core::{CoreValueFactory, Dict, EvaluationFailure, Key, Value as CoreValue};
use crate::diagnostic::Severity;
use crate::eval;
#[cfg(test)]
use crate::evaluation::ValidatedRuntimeSettlementPlan;
use crate::evaluation::{
    EvaluationTaskId, ExitIntent, RuntimeDeadlockWorkSnapshot, RuntimeDependencySnapshot,
    RuntimeExitSnapshot, RuntimeObservationEpoch, RuntimeWorkKindSnapshot,
    RuntimeWorkStateSnapshot,
};
use crate::runtime::{EvaluationRuntimeId, RuntimeFailureRoot};

pub(super) fn reasoning_diagnostic(
    values: &CoreValueFactory,
    failure: &EvaluationFailure,
) -> Diagnostic {
    Diagnostic::from_parts(
        values,
        None,
        Severity::Error,
        eval::failure_diagnostic_value_with(values, failure),
        None,
    )
}

/// Projects retryable blocked-work failures without demanding their payloads.
/// Readiness is observational: using the runtime-aware diagnostic projection
/// here would create an isolated evaluation session and change coordinator
/// generations merely by inspecting a deadlock.
fn blocked_reasoning_diagnostic(
    values: &CoreValueFactory,
    failure: &EvaluationFailure,
) -> Diagnostic {
    let emission = match failure.emission_value() {
        Some(CoreValue::Binary(message)) => {
            crate::diagnostic::text_message(None, String::from_utf8_lossy(message))
        }
        Some(emission) => emission.clone(),
        None => crate::diagnostic::text_message(None, failure.to_string()),
    };
    Diagnostic::from_parts(values, None, Severity::Error, emission, None)
}

/// Stable, observational classification of one runtime instant.
#[derive(Clone)]
pub enum RuntimeReadiness {
    Busy,
    Ready(QuiescenceSnapshot),
    Deadlocked(DeadlockSnapshot),
}

impl fmt::Debug for RuntimeReadiness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("Busy"),
            Self::Ready(snapshot) => formatter.debug_tuple("Ready").field(snapshot).finish(),
            Self::Deadlocked(snapshot) => {
                formatter.debug_tuple("Deadlocked").field(snapshot).finish()
            }
        }
    }
}

/// Authoritative revisions captured by a readiness probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeReadinessStamp {
    pub(super) work_generation: u64,
    pub(super) observation_epoch: u64,
}

impl RuntimeReadinessStamp {
    pub fn work_generation(&self) -> u64 {
        self.work_generation
    }

    pub fn observation_epoch(&self) -> u64 {
        self.observation_epoch
    }
}

/// One work disposition proposed by a stable readiness snapshot.
#[derive(Clone, Debug)]
pub struct RuntimeDisposition {
    work_id: u64,
    session_id: u64,
    task_id: Option<u64>,
    kind: RuntimeDispositionKind,
}

impl RuntimeDisposition {
    pub fn work_id(&self) -> u64 {
        self.work_id
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub fn task_id(&self) -> Option<u64> {
        self.task_id
    }

    pub fn kind(&self) -> &RuntimeDispositionKind {
        &self.kind
    }
}

/// Payload of one proposed runtime disposition.
#[derive(Clone, Debug)]
pub enum RuntimeDispositionKind {
    ExitSuccess,
    ExitError(Value),
    Killed(RuntimeKillReason),
}

/// Host-selected reason for forcefully settling stable unfinished work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeKillReason {
    Deadlock,
}

#[derive(Clone)]
pub(super) enum RuntimeSettlementSnapshot {
    Ready {
        exits: Vec<RuntimeExitSnapshot>,
    },
    KilledDeadlock {
        exits: Vec<RuntimeExitSnapshot>,
        unfinished: Vec<RuntimeDeadlockWorkSnapshot>,
        reason: RuntimeKillReason,
    },
}

impl RuntimeSettlementSnapshot {
    pub(super) fn exits(&self) -> &[RuntimeExitSnapshot] {
        match self {
            Self::Ready { exits } | Self::KilledDeadlock { exits, .. } => exits,
        }
    }

    pub(super) fn kills(&self) -> &[RuntimeDeadlockWorkSnapshot] {
        match self {
            Self::Ready { .. } => &[],
            Self::KilledDeadlock { unfinished, .. } => unfinished,
        }
    }

    pub(super) fn kill_reason(&self) -> Option<RuntimeKillReason> {
        match self {
            Self::Ready { .. } => None,
            Self::KilledDeadlock { reason, .. } => Some(*reason),
        }
    }
}

/// Retained proposal for accepting stable exit votes or forcefully settling a
/// retained deadlock.
#[derive(Clone)]
pub struct QuiescenceSnapshot {
    pub(super) runtime: EvaluationRuntime,
    pub(super) stamp: RuntimeReadinessStamp,
    pub(super) dispositions: Vec<RuntimeDisposition>,
    pub(super) reflection: crate::reflection::StoreSnapshot,
    pub(super) settlement: RuntimeSettlementSnapshot,
    pub(super) killed_work: Vec<RuntimeDeadlockWork>,
}

impl fmt::Debug for QuiescenceSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuiescenceSnapshot")
            .field("runtime", &self.runtime.id())
            .field("stamp", &self.stamp)
            .field("dispositions", &self.dispositions)
            .field("killed_work", &self.killed_work.len())
            .finish_non_exhaustive()
    }
}

impl QuiescenceSnapshot {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime.id()
    }

    pub fn stamp(&self) -> RuntimeReadinessStamp {
        self.stamp
    }

    pub fn dispositions(&self) -> &[RuntimeDisposition] {
        &self.dispositions
    }

    pub fn reflection(&self) -> &crate::reflection::StoreSnapshot {
        &self.reflection
    }

    /// Revalidates and accepts every proposed disposition, then returns a
    /// retained report of the settled runtime instant.
    pub fn settle(&self) -> Result<QuiescenceReport, RuntimeSettlementError> {
        self.runtime.settle_quiescence_snapshot(self, None)
    }

    #[cfg(test)]
    pub(crate) fn validate_without_settling(
        &self,
    ) -> Result<ValidatedRuntimeSettlementPlan, RuntimeSettlementError> {
        self.runtime.validate_quiescence_snapshot(self)
    }

    #[cfg(test)]
    pub(crate) fn settle_after_validation(
        &self,
        plan: &ValidatedRuntimeSettlementPlan,
    ) -> Result<QuiescenceReport, RuntimeSettlementError> {
        self.runtime.settle_quiescence_snapshot(self, Some(plan))
    }
}

/// Failure to accept a readiness snapshot because its runtime changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeSettlementError {
    RuntimeChanged,
}

impl fmt::Display for RuntimeSettlementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("runtime changed after its readiness snapshot")
    }
}

impl std::error::Error for RuntimeSettlementError {}

/// Retained evidence collected when a stable runtime snapshot is settled.
pub struct QuiescenceReport {
    pub(super) runtime: EvaluationRuntime,
    pub(super) stamp: RuntimeReadinessStamp,
    pub(super) dispositions: Vec<RuntimeDisposition>,
    pub(super) task_failures: Vec<ReasoningFailure>,
    pub(super) delivery_failures: RuntimeDeliveryFailureSnapshot,
    pub(super) reflection: crate::reflection::StoreSnapshot,
    pub(super) killed_work: Vec<RuntimeDeadlockWork>,
    pub(super) pending_task_failure_reports: Vec<ReasoningFailure>,
    pub(super) pending_delivery_failure_reports: RuntimeDeliveryFailureSnapshot,
    pub(super) pending_exit_error_reports: Vec<RuntimeDisposition>,
    pub(super) pending_killed_work_reports: Vec<RuntimeDeadlockWork>,
}

impl fmt::Debug for QuiescenceReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuiescenceReport")
            .field("runtime", &self.runtime.id())
            .field("stamp", &self.stamp)
            .field("dispositions", &self.dispositions)
            .field("task_failures", &self.task_failures)
            .field("killed_work", &self.killed_work.len())
            .field(
                "delivery_failures",
                &self.delivery_failures.failures().len(),
            )
            .finish_non_exhaustive()
    }
}

impl QuiescenceReport {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime.id()
    }

    pub fn stamp(&self) -> RuntimeReadinessStamp {
        self.stamp
    }

    pub fn dispositions(&self) -> &[RuntimeDisposition] {
        &self.dispositions
    }

    pub fn task_failures(&self) -> &[ReasoningFailure] {
        &self.task_failures
    }

    pub fn delivery_failures(&self) -> &RuntimeDeliveryFailureSnapshot {
        &self.delivery_failures
    }

    pub fn reflection(&self) -> &crate::reflection::StoreSnapshot {
        &self.reflection
    }

    /// Typed blocked work forcefully retired by this settlement.
    pub fn killed_work(&self) -> &[RuntimeDeadlockWork] {
        &self.killed_work
    }

    /// Task failures whose reporting responsibility was committed to this
    /// settlement rather than an earlier one.
    #[doc(hidden)]
    pub fn pending_task_failure_reports(&self) -> &[ReasoningFailure] {
        &self.pending_task_failure_reports
    }

    /// Delivery failures whose reporting responsibility was committed to this
    /// settlement rather than an earlier one.
    #[doc(hidden)]
    pub fn pending_delivery_failure_reports(&self) -> &RuntimeDeliveryFailureSnapshot {
        &self.pending_delivery_failure_reports
    }

    /// Error exits created by this settlement and not yet committed to report
    /// transport.
    #[doc(hidden)]
    pub fn pending_exit_error_reports(&self) -> &[RuntimeDisposition] {
        &self.pending_exit_error_reports
    }

    /// Killed-work records created by this settlement and not yet committed to
    /// report transport.
    #[doc(hidden)]
    pub fn pending_killed_work_reports(&self) -> &[RuntimeDeadlockWork] {
        &self.pending_killed_work_reports
    }

    /// Records that every pending report entry was accepted by the selected
    /// transport. Persistent failure ledgers are unaffected.
    #[doc(hidden)]
    pub fn mark_reports_enqueued(&mut self) {
        self.pending_task_failure_reports.clear();
        self.pending_delivery_failure_reports.failures = RedBlackTreeMapSync::new_sync();
        self.pending_exit_error_reports.clear();
        self.pending_killed_work_reports.clear();
    }
}

/// Kind of unfinished work retained in a deadlock report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeWorkKind {
    ReflectionTask,
    DeferredEvaluation,
    ClientDemand,
    Spark,
}

/// Stable non-runnable state retained in a deadlock report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeWorkState {
    Dormant,
    Reserved,
    Blocked,
}

/// Producer edge for one blocked runtime participant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeDependency {
    TaskWait {
        wait_id: u64,
        task_id: u64,
        session_id: u64,
    },
    Promise {
        promise_id: u64,
        producer: Option<RuntimeTaskWait>,
    },
    Synthetic {
        id: u64,
    },
}

/// Task-producing wait attached to a promise dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeTaskWait {
    wait_id: u64,
    task_id: u64,
    session_id: u64,
}

impl RuntimeTaskWait {
    pub fn wait_id(&self) -> u64 {
        self.wait_id
    }

    pub fn task_id(&self) -> u64 {
        self.task_id
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }
}

/// One unfinished participant retained by a deadlock snapshot.
#[derive(Clone, Debug)]
pub struct RuntimeDeadlockWork {
    work_id: u64,
    session_id: u64,
    task_id: Option<u64>,
    kind: RuntimeWorkKind,
    state: RuntimeWorkState,
    dependency: Option<RuntimeDependency>,
    observed_epoch: Option<u64>,
    blocked_diagnostic: Option<Diagnostic>,
    blocked_failure: Option<RuntimeFailureRoot>,
}

impl RuntimeDeadlockWork {
    pub fn work_id(&self) -> u64 {
        self.work_id
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub fn task_id(&self) -> Option<u64> {
        self.task_id
    }

    pub fn kind(&self) -> RuntimeWorkKind {
        self.kind
    }

    pub fn state(&self) -> RuntimeWorkState {
        self.state
    }

    pub fn dependency(&self) -> Option<&RuntimeDependency> {
        self.dependency.as_ref()
    }

    pub fn observed_epoch(&self) -> Option<u64> {
        self.observed_epoch
    }

    /// Retryable evaluation failure retained at the blocked checkpoint, when
    /// the participant had reached one and its text is immediately available.
    pub fn blocked_error(&self) -> Option<&str> {
        self.blocked_diagnostic.as_ref().and_then(|diagnostic| {
            let message = diagnostic.message();
            (message != "<diagnostic has no immediate text view>").then_some(message)
        })
    }

    /// Structured retryable evaluation failure retained at this blocked
    /// checkpoint, when the participant had reached one.
    pub fn blocked_diagnostic(&self) -> Option<&Diagnostic> {
        self.blocked_diagnostic.as_ref()
    }

    /// Demands a retained blocked failure far enough to project its complete
    /// diagnostic view. Unlike [`Self::blocked_diagnostic`], this may perform
    /// evaluation work and is intended for rendering after settlement.
    #[doc(hidden)]
    pub fn project_blocked_diagnostic(&self, values: &Values) -> Result<Option<Diagnostic>, Error> {
        let Some(failure) = self.blocked_failure.as_ref() else {
            return Ok(None);
        };
        if let Some(diagnostic) = &self.blocked_diagnostic {
            diagnostic.emission.require_runtime(values.runtime)?;
        }
        Ok(Some(reasoning_diagnostic(
            &values.core,
            failure.as_failure(),
        )))
    }

    #[cfg(test)]
    pub(crate) fn blocked_failure_root(&self) -> Option<&RuntimeFailureRoot> {
        self.blocked_failure.as_ref()
    }
}

/// Retained stable evidence that at least one participant cannot progress.
#[derive(Clone)]
pub struct DeadlockSnapshot {
    pub(super) runtime: EvaluationRuntime,
    pub(super) stamp: RuntimeReadinessStamp,
    pub(super) dispositions: Vec<RuntimeDisposition>,
    pub(super) unfinished: Vec<RuntimeDeadlockWork>,
    pub(super) reflection: crate::reflection::StoreSnapshot,
    pub(super) settlement_exits: Vec<RuntimeExitSnapshot>,
    pub(super) settlement_unfinished: Vec<RuntimeDeadlockWorkSnapshot>,
}

impl fmt::Debug for DeadlockSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeadlockSnapshot")
            .field("runtime", &self.runtime.id())
            .field("stamp", &self.stamp)
            .field("dispositions", &self.dispositions)
            .field("unfinished", &self.unfinished)
            .finish_non_exhaustive()
    }
}

impl DeadlockSnapshot {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime.id()
    }

    pub fn stamp(&self) -> RuntimeReadinessStamp {
        self.stamp
    }

    pub fn dispositions(&self) -> &[RuntimeDisposition] {
        &self.dispositions
    }

    pub fn unfinished(&self) -> &[RuntimeDeadlockWork] {
        &self.unfinished
    }

    pub fn reflection(&self) -> &crate::reflection::StoreSnapshot {
        &self.reflection
    }

    /// Derives a forced settlement proposal from this retained deadlock.
    /// The proposal remains observational until [`QuiescenceSnapshot::settle`]
    /// revalidates and accepts it.
    pub fn kill(&self, reason: RuntimeKillReason) -> QuiescenceSnapshot {
        let mut dispositions = self.dispositions.clone();
        dispositions.extend(self.unfinished.iter().map(|work| RuntimeDisposition {
            work_id: work.work_id,
            session_id: work.session_id,
            task_id: work.task_id,
            kind: RuntimeDispositionKind::Killed(reason),
        }));
        QuiescenceSnapshot {
            runtime: self.runtime.clone(),
            stamp: self.stamp,
            dispositions,
            reflection: self.reflection.clone(),
            settlement: RuntimeSettlementSnapshot::KilledDeadlock {
                exits: self.settlement_exits.clone(),
                unfinished: self.settlement_unfinished.clone(),
                reason,
            },
            killed_work: self.unfinished.clone(),
        }
    }
}

pub(super) fn runtime_disposition_from_snapshot(
    snapshot: RuntimeExitSnapshot,
) -> RuntimeDisposition {
    RuntimeDisposition {
        work_id: snapshot.work.get(),
        session_id: snapshot.session.get(),
        task_id: Some(snapshot.task.get()),
        kind: match snapshot.intent {
            ExitIntent::Success => RuntimeDispositionKind::ExitSuccess,
            ExitIntent::Error(message) => RuntimeDispositionKind::ExitError(Value(message)),
        },
    }
}

pub(super) fn runtime_killed_failure(
    values: &CoreValueFactory,
    reason: RuntimeKillReason,
) -> RuntimeFailureRoot {
    let reason = match reason {
        RuntimeKillReason::Deadlock => values.key_value(&Key::atom_from_text("deadlock")),
    };
    let args = Dict::new_sync().insert(Key::atom_from_text("reason"), reason);
    let detail = Dict::new_sync()
        .insert(
            Key::atom_from_text("op"),
            values.key_value(&Key::atom_from_text("kill")),
        )
        .insert(Key::atom_from_text("args"), CoreValue::Dict(args));
    let message = Dict::new_sync()
        .insert(
            (*crate::core::keys::TEXT).clone(),
            CoreValue::binary_from_text("runtime killed work in a deadlocked settlement"),
        )
        .insert((*crate::core::keys::SEVERITY).clone(), values.error());
    RuntimeFailureRoot::new(
        values,
        Arc::new(EvaluationFailure::emission(CoreValue::Dict(
            Dict::new_sync()
                .insert((*crate::core::keys::MSG).clone(), CoreValue::Dict(message))
                .insert(Key::atom_from_text("runtime"), CoreValue::Dict(detail)),
        ))),
    )
}

fn runtime_dependency_from_snapshot(snapshot: RuntimeDependencySnapshot) -> RuntimeDependency {
    match snapshot {
        RuntimeDependencySnapshot::Wait {
            wait,
            producer,
            session,
        } => RuntimeDependency::TaskWait {
            wait_id: wait,
            task_id: producer.get(),
            session_id: session.get(),
        },
        RuntimeDependencySnapshot::Promise { promise, producer } => RuntimeDependency::Promise {
            promise_id: promise,
            producer: producer.map(|(wait_id, task, session)| RuntimeTaskWait {
                wait_id,
                task_id: task.get(),
                session_id: session.get(),
            }),
        },
        #[cfg(test)]
        RuntimeDependencySnapshot::Test(id) => RuntimeDependency::Synthetic { id },
    }
}

pub(super) fn runtime_deadlock_work_from_snapshot(
    values: &CoreValueFactory,
    snapshot: RuntimeDeadlockWorkSnapshot,
) -> RuntimeDeadlockWork {
    let blocked_failure = snapshot.blocked_error;
    RuntimeDeadlockWork {
        work_id: snapshot.work.get(),
        session_id: snapshot.session.get(),
        task_id: snapshot.task.map(EvaluationTaskId::get),
        kind: match snapshot.kind {
            RuntimeWorkKindSnapshot::ReflectionTask => RuntimeWorkKind::ReflectionTask,
            RuntimeWorkKindSnapshot::DeferredEvaluation => RuntimeWorkKind::DeferredEvaluation,
            RuntimeWorkKindSnapshot::ClientDemand => RuntimeWorkKind::ClientDemand,
            RuntimeWorkKindSnapshot::Spark => RuntimeWorkKind::Spark,
        },
        state: match snapshot.state {
            RuntimeWorkStateSnapshot::Dormant => RuntimeWorkState::Dormant,
            RuntimeWorkStateSnapshot::Reserved => RuntimeWorkState::Reserved,
            RuntimeWorkStateSnapshot::Blocked => RuntimeWorkState::Blocked,
        },
        dependency: snapshot.dependency.map(runtime_dependency_from_snapshot),
        observed_epoch: snapshot.observed_epoch.map(RuntimeObservationEpoch::get),
        blocked_diagnostic: blocked_failure
            .as_ref()
            .map(|error| blocked_reasoning_diagnostic(values, error.as_failure())),
        blocked_failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-exhaustive ownership latch for the host-visible blocked-work
    /// failure retained by readiness snapshots and settlement reports.
    fn assert_runtime_deadlock_work_failure_boundary(work: &RuntimeDeadlockWork) {
        let RuntimeDeadlockWork {
            blocked_diagnostic,
            blocked_failure,
            ..
        } = work;
        let _: &Option<Diagnostic> = blocked_diagnostic;
        let _: &Option<crate::runtime::RuntimeFailureRoot> = blocked_failure;
    }

    #[test]
    fn runtime_deadlock_work_failure_boundary_is_complete() {
        let _: fn(&RuntimeDeadlockWork) = assert_runtime_deadlock_work_failure_boundary;
    }
}
