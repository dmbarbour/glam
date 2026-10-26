use std::fmt;
use std::sync::Arc;

use super::EvaluationRuntime;
use crate::api::{Diagnostic, MachinePath, Value, Values};
use crate::core::{CoreValueFactory, Key};
use crate::diagnostic::Severity;
use crate::evaluation::{
    EvaluationTaskId, RuntimeDeadlockWorkSnapshot, RuntimeDependencySnapshot,
    RuntimeObservationEpoch, RuntimeWorkKindSnapshot, RuntimeWorkStateSnapshot,
};
use crate::runtime::{EvaluationRuntimeId, RuntimeFailureRoot, RuntimeValueRoot};

/// The diagnostic for a retained failure: its one composition, shared by
/// every report of it, so repeated reports present the same diagnostic.
pub(super) fn reasoning_diagnostic(
    values: &CoreValueFactory,
    failure: &RuntimeFailureRoot,
) -> Diagnostic {
    Diagnostic::from_parts(
        &Values::from_core_factory(values.clone()),
        None,
        Severity::Error,
        Value::from_runtime_root(failure.diagnostic(values)),
        None,
        None,
        Arc::from(crate::api::diagnostics::FAILURE_SUMMARY),
    )
}

/// A reflection machine in error (`reflection-tasks-are-machine-tasks`): a
/// child a task on which failed or was interrupted, flagged until its parent
/// recovers or clears it, or an annotation task's anonymous root, which its
/// failure flagged for good. An analysis lists each
/// (`quiescence-is-analyzed`).
#[derive(Clone)]
pub struct FailedMachine {
    runtime: EvaluationRuntimeId,
    path: MachinePath,
    diagnostic: Diagnostic,
}

impl FailedMachine {
    pub(super) fn new(
        runtime: EvaluationRuntimeId,
        values: &CoreValueFactory,
        path: Vec<Key>,
        diagnostic: &RuntimeValueRoot,
    ) -> Self {
        Self {
            runtime,
            path: MachinePath::from(path),
            diagnostic: Diagnostic::from_parts(
                &Values::from_core_factory(values.clone()),
                None,
                Severity::Error,
                Value::from_runtime_root(diagnostic.clone()),
                None,
                None,
                Arc::from(crate::api::diagnostics::FAILURE_SUMMARY),
            ),
        }
    }

    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    /// The machine's path from the root. A child of a reflection task with
    /// no machine of its own lies below that task's anonymous root.
    pub fn path(&self) -> &MachinePath {
        &self.path
    }

    /// What flagged the machine: the failure's diagnostic, or the
    /// interrupt's reason.
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }
}

impl fmt::Debug for FailedMachine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FailedMachine")
            .field("runtime", &self.runtime)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Stable, observational classification of one runtime instant.
#[derive(Clone)]
pub enum RuntimeReadiness {
    Busy,
    MaintenanceRequired(RuntimeMaintenanceSnapshot),
    MaintenanceFailed(RuntimeMaintenanceSnapshot),
    Ready(QuiescenceSnapshot),
    Deadlocked(DeadlockSnapshot),
    /// A panic tore the runtime's scheduler, transaction, or exclusive-admission
    /// state. That is a runtime implementation error, never Glam semantics.
    /// The runtime accepts no further work, but dropping it remains safe, and
    /// the client may build a new runtime.
    Poisoned,
}

impl fmt::Debug for RuntimeReadiness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("Busy"),
            Self::MaintenanceRequired(snapshot) => formatter
                .debug_tuple("MaintenanceRequired")
                .field(snapshot)
                .finish(),
            Self::MaintenanceFailed(snapshot) => formatter
                .debug_tuple("MaintenanceFailed")
                .field(snapshot)
                .finish(),
            Self::Ready(snapshot) => formatter.debug_tuple("Ready").field(snapshot).finish(),
            Self::Deadlocked(snapshot) => {
                formatter.debug_tuple("Deadlocked").field(snapshot).finish()
            }
            Self::Poisoned => formatter.write_str("Poisoned"),
        }
    }
}

/// Authoritative revisions captured by a readiness probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeReadinessStamp {
    pub(super) work_generation: u64,
    pub(super) observation_epoch: u64,
    pub(super) gc_maintenance_revision: u64,
}

impl RuntimeReadinessStamp {
    pub fn work_generation(&self) -> u64 {
        self.work_generation
    }

    pub fn observation_epoch(&self) -> u64 {
        self.observation_epoch
    }

    pub fn gc_maintenance_revision(&self) -> u64 {
        self.gc_maintenance_revision
    }
}

/// Actionable managed-heap state retained by a readiness observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMaintenanceState {
    Requested,
    RetryRequired,
    Poisoned,
}

/// Runtime-local managed-heap maintenance observation.
#[derive(Clone)]
pub struct RuntimeMaintenanceSnapshot {
    pub(super) runtime: EvaluationRuntime,
    pub(super) revision: u64,
    pub(super) state: RuntimeMaintenanceState,
    pub(super) failure: Option<RuntimeMaintenanceFailure>,
}

impl fmt::Debug for RuntimeMaintenanceSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeMaintenanceSnapshot")
            .field("runtime", &self.runtime.id())
            .field("revision", &self.revision)
            .field("state", &self.state)
            .field("failure", &self.failure)
            .finish()
    }
}

impl RuntimeMaintenanceSnapshot {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime.id()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn state(&self) -> RuntimeMaintenanceState {
        self.state
    }

    pub fn failure(&self) -> Option<&RuntimeMaintenanceFailure> {
        self.failure.as_ref()
    }

    /// Services the observed runtime's current maintenance obligation.
    ///
    /// The observed revision is revalidated before the activity lease is
    /// admitted. A stale snapshot returns [`RuntimeMaintenanceErrorKind::RuntimeChanged`]
    /// rather than consuming a newer maintenance obligation.
    pub fn service(&self) -> Result<RuntimeMaintenanceReport, RuntimeMaintenanceError> {
        self.runtime
            .service_managed_collection_revision(self.revision)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMaintenanceFailureKind {
    CollectorPanic,
    FinalizerPanic,
    Poisoned,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeMaintenanceFailure {
    id: u64,
    kind: RuntimeMaintenanceFailureKind,
    message: Arc<str>,
}

impl RuntimeMaintenanceFailure {
    pub(super) fn new(id: u64, kind: RuntimeMaintenanceFailureKind, message: Arc<str>) -> Self {
        Self { id, kind, message }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn kind(&self) -> RuntimeMaintenanceFailureKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMaintenanceErrorKind {
    RuntimeChanged,
    ActiveMutator,
    CollectorPanic,
    FinalizerPanic,
    /// The managed heap or the runtime core is permanently poisoned by a
    /// panic. The message says which.
    Poisoned,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeMaintenanceError {
    kind: RuntimeMaintenanceErrorKind,
    message: Arc<str>,
}

impl RuntimeMaintenanceError {
    pub(super) fn new(kind: RuntimeMaintenanceErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> RuntimeMaintenanceErrorKind {
        self.kind
    }
}

impl fmt::Display for RuntimeMaintenanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RuntimeMaintenanceError {}

/// Scalar results from one explicitly serviced managed collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeMaintenanceReport {
    report: glam_gc::CollectionReport,
    statistics: glam_gc::HeapStatistics,
}

impl RuntimeMaintenanceReport {
    pub(super) fn new(
        report: glam_gc::CollectionReport,
        statistics: glam_gc::HeapStatistics,
    ) -> Self {
        Self { report, statistics }
    }

    pub fn epoch(self) -> u64 {
        self.report.epoch()
    }

    pub fn root_entries(self) -> usize {
        self.report.root_entries()
    }

    pub fn traced_objects(self) -> usize {
        self.report.traced_objects()
    }

    pub fn marked_slots(self) -> usize {
        self.report.marked_slots()
    }

    pub fn conservatively_retained_slots(self) -> usize {
        self.report.conservatively_retained_slots()
    }

    pub fn reclaimed_slots(self) -> usize {
        self.report.reclaimed_slots()
    }

    pub fn finalized_slots(self) -> usize {
        self.report.finalized_slots()
    }

    pub fn reclaimed_runs(self) -> usize {
        self.report.reclaimed_runs()
    }

    pub fn assigned_runs(self) -> usize {
        self.statistics.assigned_runs()
    }

    pub fn run_high_water_mark(self) -> usize {
        self.statistics.run_high_water_mark()
    }

    pub fn run_headroom(self) -> usize {
        self.statistics.run_headroom()
    }

    pub fn pending_finalizers(self) -> usize {
        self.statistics.pending_finalizers()
    }
}

/// A stable runtime with no unfinished work, as a readiness probe found it:
/// its stamp and its reflection store. What the runtime holds, an analysis
/// says (`quiescence-is-analyzed`).
#[derive(Clone)]
pub struct QuiescenceSnapshot {
    pub(super) runtime: EvaluationRuntime,
    pub(super) stamp: RuntimeReadinessStamp,
    pub(super) reflection: crate::reflection::StoreSnapshot,
}

impl fmt::Debug for QuiescenceSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuiescenceSnapshot")
            .field("runtime", &self.runtime.id())
            .field("stamp", &self.stamp)
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

    pub fn reflection(&self) -> &crate::reflection::StoreSnapshot {
        &self.reflection
    }
}

/// Kind of unfinished work retained in a deadlock report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeWorkKind {
    ReflectionTask,
    DeferredEvaluation,
    LazyRoute,
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
    LazyWait {
        wait_id: u64,
        lazy_id: u64,
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
    session_id: Option<u64>,
    task_id: Option<u64>,
    lazy_id: Option<u64>,
    kind: RuntimeWorkKind,
    state: RuntimeWorkState,
    dependency: Option<RuntimeDependency>,
    observed_epoch: Option<u64>,
    blocked_diagnostic: Option<Diagnostic>,
}

impl RuntimeDeadlockWork {
    pub fn work_id(&self) -> u64 {
        self.work_id
    }

    pub fn session_id(&self) -> Option<u64> {
        self.session_id
    }

    pub fn lazy_id(&self) -> Option<u64> {
        self.lazy_id
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

    /// Structured retryable evaluation failure retained at this blocked
    /// checkpoint, when the participant had reached one. It is composed, not
    /// evaluated: its renderer evaluates it.
    pub fn blocked_diagnostic(&self) -> Option<&Diagnostic> {
        self.blocked_diagnostic.as_ref()
    }
}

/// Retained stable evidence that at least one participant cannot progress.
#[derive(Clone)]
pub struct DeadlockSnapshot {
    pub(super) runtime: EvaluationRuntime,
    pub(super) stamp: RuntimeReadinessStamp,
    pub(super) unfinished: Vec<RuntimeDeadlockWork>,
    pub(super) reflection: crate::reflection::StoreSnapshot,
}

impl fmt::Debug for DeadlockSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeadlockSnapshot")
            .field("runtime", &self.runtime.id())
            .field("stamp", &self.stamp)
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

    pub fn unfinished(&self) -> &[RuntimeDeadlockWork] {
        &self.unfinished
    }

    pub fn reflection(&self) -> &crate::reflection::StoreSnapshot {
        &self.reflection
    }
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
        RuntimeDependencySnapshot::LazyWait { wait, lazy } => RuntimeDependency::LazyWait {
            wait_id: wait,
            lazy_id: lazy.get(),
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
    RuntimeDeadlockWork {
        work_id: snapshot.work.get(),
        session_id: snapshot.session.map(|session| session.get()),
        task_id: snapshot.task.map(EvaluationTaskId::get),
        lazy_id: snapshot.lazy.map(|lazy| lazy.get()),
        kind: match snapshot.kind {
            RuntimeWorkKindSnapshot::ReflectionTask => RuntimeWorkKind::ReflectionTask,
            RuntimeWorkKindSnapshot::DeferredEvaluation => RuntimeWorkKind::DeferredEvaluation,
            RuntimeWorkKindSnapshot::LazyRoute => RuntimeWorkKind::LazyRoute,
            RuntimeWorkKindSnapshot::Spark => RuntimeWorkKind::Spark,
        },
        state: match snapshot.state {
            RuntimeWorkStateSnapshot::Dormant => RuntimeWorkState::Dormant,
            RuntimeWorkStateSnapshot::Reserved => RuntimeWorkState::Reserved,
            RuntimeWorkStateSnapshot::Blocked => RuntimeWorkState::Blocked,
        },
        dependency: snapshot.dependency.map(runtime_dependency_from_snapshot),
        observed_epoch: snapshot.observed_epoch.map(RuntimeObservationEpoch::get),
        blocked_diagnostic: snapshot
            .blocked_error
            .as_ref()
            .map(|error| reasoning_diagnostic(values, error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ownership latch for the host-visible blocked-work failure retained by
    /// readiness snapshots and analyses: only its composed
    /// diagnostic, never a raw failure.
    fn assert_runtime_deadlock_work_failure_boundary(work: &RuntimeDeadlockWork) {
        let RuntimeDeadlockWork {
            blocked_diagnostic, ..
        } = work;
        let _: &Option<Diagnostic> = blocked_diagnostic;
    }

    #[test]
    fn runtime_deadlock_work_failure_boundary_is_complete() {
        let _: fn(&RuntimeDeadlockWork) = assert_runtime_deadlock_work_failure_boundary;
    }
}
