//! Runtime-local identity allocation shared by evaluation subsystems.
//!
//! `EvaluationRuntimeId` remains process-global. Every narrower identity is
//! allocated from one of these runtime-owned counters and is therefore
//! interpreted together with its runtime.

use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use glam_gc::HeapMaintenanceSnapshot;

use crate::core::{CoreValueFactory, EvaluationFailure, PreparedRuntimeValueRoot, Value};

static NEXT_EVALUATION_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

/// Process-unique identity of one evaluation runtime.
///
/// Numeric IDs are diagnostic provenance, not transferable authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvaluationRuntimeId(NonZeroU64);

impl EvaluationRuntimeId {
    pub fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) fn from_u64(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }
}

pub(crate) fn allocate_evaluation_runtime_id() -> EvaluationRuntimeId {
    let id = NEXT_EVALUATION_RUNTIME_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("evaluation runtime IDs exhausted");
    EvaluationRuntimeId::from_u64(id).expect("evaluation runtime IDs start at one")
}

/// Shared admission boundary for runtime-owned state publication.
///
/// Ordinary component transitions hold a shared guard through their complete
/// authoritative publication sequence. A future readiness snapshot or
/// settlement takes the exclusive guard, so it cannot observe only part of a
/// transition. Component locks are still acquired and released separately.
pub(crate) struct RuntimeMutationAdmission {
    gate: RwLock<()>,
    activity: Arc<RuntimeActivityState>,
}

impl RuntimeMutationAdmission {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            gate: RwLock::new(()),
            activity: RuntimeActivityState::new(),
        })
    }

    pub(crate) fn activity(&self) -> Arc<RuntimeActivityState> {
        self.activity.clone()
    }

    pub(crate) fn mutation_guard(&self) -> RuntimeMutationGuard<'_> {
        RuntimeMutationGuard {
            guard: Some(
                self.gate
                    .read()
                    .expect("runtime settlement gate should not be poisoned"),
            ),
            activity: &self.activity,
        }
    }

    pub(crate) fn try_settlement_guard(&self) -> Option<RuntimeSettlementGuard<'_>> {
        self.gate
            .try_write()
            .ok()
            .map(|guard| RuntimeSettlementGuard { _guard: guard })
    }

    pub(crate) fn settlement_guard(&self) -> RuntimeSettlementGuard<'_> {
        RuntimeSettlementGuard {
            _guard: self
                .gate
                .write()
                .expect("runtime settlement gate should not be poisoned"),
        }
    }

    /// Registers one potentially collecting runtime operation before it may
    /// enter the managed heap.
    pub(crate) fn begin_gc_activity(self: &Arc<Self>) -> RuntimeGcActivityLease {
        let mutation = self.mutation_guard();
        self.activity.begin_gc_activity();
        drop(mutation);
        RuntimeGcActivityLease {
            admission: self.clone(),
            active: true,
        }
    }

    pub(crate) fn begin_gc_activity_for_snapshot(
        self: &Arc<Self>,
        revision: u64,
    ) -> Option<RuntimeGcActivityLease> {
        let mutation = self.mutation_guard();
        let admitted = self.activity.begin_gc_activity_for_snapshot(revision);
        drop(mutation);
        admitted.then(|| RuntimeGcActivityLease {
            admission: self.clone(),
            active: true,
        })
    }

    pub(crate) fn gc_maintenance_snapshot(
        &self,
        _settlement: &RuntimeSettlementGuard<'_>,
    ) -> RuntimeGcMaintenanceSnapshot {
        self.activity.gc_maintenance_snapshot()
    }

    pub(crate) fn record_gc_request(&self, _mutation: &RuntimeMutationGuard<'_>) {
        self.activity.record_gc_request();
    }

    /// Promotes collector-local pressure into one authoritative runtime
    /// maintenance request while exclusive settlement admission is held.
    ///
    /// The heap remains permanently `NoAuto`; this publication only makes an
    /// already-latched collector request visible to runtime readiness.
    pub(crate) fn promote_gc_pressure_request(
        &self,
        _settlement: &RuntimeSettlementGuard<'_>,
        heap: glam_gc::HeapMaintenanceSnapshot,
    ) -> bool {
        let pressure_requested = heap
            .statistics()
            .is_some_and(glam_gc::HeapStatistics::collection_requested);
        pressure_requested && self.activity.promote_gc_pressure_request()
    }

    pub(crate) fn record_gc_request_failure(
        &self,
        _mutation: &RuntimeMutationGuard<'_>,
        outcome: RuntimeGcLeaseOutcome,
    ) {
        self.activity.record_gc_request_failure(outcome);
    }

    pub(crate) fn gc_failure_ledgers_for_settlement(
        &self,
        _settlement: &RuntimeSettlementGuard<'_>,
    ) -> (
        Vec<RuntimeGcMaintenanceFailure>,
        Vec<RuntimeGcMaintenanceFailure>,
        u64,
    ) {
        self.activity.gc_failure_ledgers_for_settlement()
    }

    fn finish_gc_activity(&self, outcome: RuntimeGcLeaseOutcome) {
        let mutation = self.mutation_guard();
        self.activity.finish_gc_activity(outcome);
        drop(mutation);
    }

    /// Wakes runtime clients after an exclusive settlement publication.
    pub(crate) fn notify_settlement(&self) {
        self.activity.advance();
    }
}

pub(crate) struct RuntimeMutationGuard<'a> {
    guard: Option<RwLockReadGuard<'a, ()>>,
    activity: &'a RuntimeActivityState,
}

pub(crate) struct RuntimeSettlementGuard<'a> {
    _guard: RwLockWriteGuard<'a, ()>,
}

/// Runtime-owned disposition of managed-heap maintenance.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RuntimeGcMaintenanceDisposition {
    #[default]
    Idle,
    RetryRequired,
    Poisoned,
}

/// Rust-side category retained for one failed maintenance attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeGcMaintenanceFailureKind {
    CollectorPanic,
    FinalizerPanic,
    Poisoned,
}

/// Durable host failure which never requires entering the managed heap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeGcMaintenanceFailure {
    pub(crate) id: u64,
    pub(crate) kind: RuntimeGcMaintenanceFailureKind,
    pub(crate) message: Arc<str>,
}

/// Authoritative GC state observed only while settlement admission is held.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeGcMaintenanceSnapshot {
    pub(crate) active_leases: usize,
    pub(crate) revision: u64,
    pub(crate) explicit_request: bool,
    pub(crate) disposition: RuntimeGcMaintenanceDisposition,
    pub(crate) latest_failure: Option<RuntimeGcMaintenanceFailure>,
}

#[derive(Debug)]
pub(crate) struct RuntimeGcLeaseOutcome {
    pub(crate) heap: HeapMaintenanceSnapshot,
    pub(crate) failure: Option<(RuntimeGcMaintenanceFailureKind, Arc<str>)>,
}

impl RuntimeGcLeaseOutcome {
    pub(crate) fn success(heap: HeapMaintenanceSnapshot) -> Self {
        Self {
            heap,
            failure: None,
        }
    }

    pub(crate) fn no_collection(heap: HeapMaintenanceSnapshot) -> Self {
        Self {
            heap,
            failure: None,
        }
    }

    pub(crate) fn failure(
        heap: HeapMaintenanceSnapshot,
        kind: RuntimeGcMaintenanceFailureKind,
        message: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            heap,
            failure: Some((kind, message.into())),
        }
    }
}

/// Owned runtime activity obligation for one potentially collecting entry.
///
/// The ordinary path explicitly publishes its collector snapshot. Dropping an
/// unfinished lease is a conservative unwind fallback: readiness remains
/// actionable rather than silently accepting an unknown heap disposition.
pub(crate) struct RuntimeGcActivityLease {
    admission: Arc<RuntimeMutationAdmission>,
    active: bool,
}

impl RuntimeGcActivityLease {
    pub(crate) fn finish(mut self, outcome: RuntimeGcLeaseOutcome) {
        self.admission.finish_gc_activity(outcome);
        self.active = false;
    }
}

impl Drop for RuntimeGcActivityLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mutation = self.admission.mutation_guard();
        self.admission.activity.abandon_gc_activity();
        drop(mutation);
    }
}

mod mutation_authority {
    pub trait Sealed {}
}

/// Proof that the caller owns either shared mutation admission or exclusive
/// settlement admission for this runtime.
///
/// Publication APIs use this sealed trait only as an authority token. They do
/// not acquire admission themselves, which lets ordinary commits and atomic
/// settlement share the same terminal-publication paths.
pub(crate) trait RuntimeMutationAuthority: mutation_authority::Sealed {}

impl mutation_authority::Sealed for RuntimeMutationGuard<'_> {}
impl RuntimeMutationAuthority for RuntimeMutationGuard<'_> {}
impl mutation_authority::Sealed for RuntimeSettlementGuard<'_> {}
impl RuntimeMutationAuthority for RuntimeSettlementGuard<'_> {}

impl Drop for RuntimeMutationGuard<'_> {
    fn drop(&mut self) {
        drop(self.guard.take());
        // This is deliberately conservative: the activity generation is only
        // a parking aid, so an unchanged guarded pass may produce a harmless
        // extra wake. Semantic observation and readiness use their own
        // authoritative generations.
        self.activity.advance();
    }
}

/// Non-authoritative notification state used only to park a runtime pump.
///
/// Every guarded runtime transition advances this generation after releasing
/// the admission lock. A waiter snapshots it after its own guarded inspection,
/// rechecks authoritative state, then sleeps only while the snapshot remains
/// current. Transactions and readiness must never validate this generation.
pub(crate) struct RuntimeActivityState {
    state: Mutex<RuntimeActivityData>,
    changed: Condvar,
    #[cfg(test)]
    waits: AtomicU64,
}

#[derive(Default)]
struct RuntimeActivityData {
    generation: u64,
    gc: RuntimeGcMaintenanceState,
}

#[derive(Default)]
struct RuntimeGcMaintenanceState {
    active_leases: usize,
    revision: u64,
    explicit_request: bool,
    disposition: RuntimeGcMaintenanceDisposition,
    next_failure_id: u64,
    completed_collection_epoch: u64,
    failures: Vec<RuntimeGcMaintenanceFailure>,
    pending_failure_reports: Vec<RuntimeGcMaintenanceFailure>,
}

impl RuntimeActivityState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(RuntimeActivityData::default()),
            changed: Condvar::new(),
            #[cfg(test)]
            waits: AtomicU64::new(0),
        })
    }

    pub(crate) fn current(&self) -> u64 {
        self.state
            .lock()
            .expect("runtime activity mutex should not be poisoned")
            .generation
    }

    fn advance(&self) {
        let mut state = self
            .state
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        state.generation = state
            .generation
            .checked_add(1)
            .expect("runtime activity generations exhausted");
        drop(state);
        self.changed.notify_all();
    }

    pub(crate) fn wait_for_change(&self, observed: u64) {
        #[cfg(test)]
        self.waits.fetch_add(1, Ordering::Relaxed);
        let mut state = self
            .state
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        while state.generation == observed {
            state = self
                .changed
                .wait(state)
                .expect("runtime activity mutex should not be poisoned");
        }
    }

    fn begin_gc_activity(&self) {
        let mut state = self
            .state
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        state.gc.active_leases = state
            .gc
            .active_leases
            .checked_add(1)
            .expect("runtime GC activity lease count exhausted");
        state.gc.advance_revision();
    }

    fn begin_gc_activity_for_snapshot(&self, revision: u64) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        if state.gc.revision != revision
            || state.gc.active_leases != 0
            || !(state.gc.explicit_request
                || state.gc.disposition == RuntimeGcMaintenanceDisposition::RetryRequired)
        {
            return false;
        }
        state.gc.active_leases = state
            .gc
            .active_leases
            .checked_add(1)
            .expect("runtime GC activity lease count exhausted");
        state.gc.advance_revision();
        true
    }

    fn finish_gc_activity(&self, outcome: RuntimeGcLeaseOutcome) {
        let mut state = self
            .state
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        state.gc.retire_lease();
        state.gc.publish_outcome(outcome);
        state.gc.advance_revision();
    }

    fn abandon_gc_activity(&self) {
        let mut state = self
            .state
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        state.gc.retire_lease();
        state.gc.disposition = RuntimeGcMaintenanceDisposition::RetryRequired;
        state.gc.advance_revision();
    }

    fn record_gc_request(&self) {
        let mut state = self
            .state
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        if !state.gc.explicit_request {
            state.gc.explicit_request = true;
            state.gc.advance_revision();
        }
    }

    fn promote_gc_pressure_request(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        if state.gc.active_leases != 0
            || state.gc.explicit_request
            || state.gc.disposition != RuntimeGcMaintenanceDisposition::Idle
        {
            return false;
        }
        state.gc.explicit_request = true;
        state.gc.advance_revision();
        true
    }

    fn record_gc_request_failure(&self, outcome: RuntimeGcLeaseOutcome) {
        let mut state = self
            .state
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        state.gc.explicit_request = true;
        state.gc.publish_outcome(outcome);
        state.gc.advance_revision();
    }

    fn gc_failure_ledgers_for_settlement(
        &self,
    ) -> (
        Vec<RuntimeGcMaintenanceFailure>,
        Vec<RuntimeGcMaintenanceFailure>,
        u64,
    ) {
        let mut state = self
            .state
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        let pending = std::mem::take(&mut state.gc.pending_failure_reports);
        if !pending.is_empty() {
            state.gc.advance_revision();
        }
        (state.gc.failures.clone(), pending, state.gc.revision)
    }

    fn gc_maintenance_snapshot(&self) -> RuntimeGcMaintenanceSnapshot {
        let state = self
            .state
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        state.gc.snapshot()
    }

    #[cfg(test)]
    pub(crate) fn wait_count(&self) -> u64 {
        self.waits.load(Ordering::Relaxed)
    }
}

impl RuntimeGcMaintenanceState {
    fn advance_revision(&mut self) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("runtime GC maintenance revision exhausted");
    }

    fn retire_lease(&mut self) {
        self.active_leases = self
            .active_leases
            .checked_sub(1)
            .expect("runtime GC activity lease retired twice");
    }

    fn publish_outcome(&mut self, outcome: RuntimeGcLeaseOutcome) {
        let was_poisoned = self.disposition == RuntimeGcMaintenanceDisposition::Poisoned;
        match outcome.heap {
            HeapMaintenanceSnapshot::Poisoned => {
                self.disposition = RuntimeGcMaintenanceDisposition::Poisoned;
            }
            HeapMaintenanceSnapshot::Usable(statistics) => {
                let observed_epoch = statistics.completed_collection_epoch();
                if observed_epoch > self.completed_collection_epoch {
                    self.completed_collection_epoch = observed_epoch;
                    self.explicit_request = false;
                    self.disposition =
                        if outcome.failure.is_some() || statistics.pending_finalizers() != 0 {
                            RuntimeGcMaintenanceDisposition::RetryRequired
                        } else {
                            RuntimeGcMaintenanceDisposition::Idle
                        };
                } else if observed_epoch == self.completed_collection_epoch
                    && (outcome.failure.is_some() || statistics.pending_finalizers() != 0)
                {
                    self.disposition = RuntimeGcMaintenanceDisposition::RetryRequired;
                }
                // An older attempt may publish historical failure data after
                // a later successful collection. It must not resurrect retry
                // work already consumed by the later epoch. Likewise, an idle
                // observation of an already-published epoch cannot consume a
                // request which may have linearized after that collection.
            }
        }
        if let Some((kind, message)) = outcome.failure
            && !(was_poisoned && kind == RuntimeGcMaintenanceFailureKind::Poisoned)
        {
            self.push_failure(kind, message);
        }
    }

    fn push_failure(&mut self, kind: RuntimeGcMaintenanceFailureKind, message: Arc<str>) {
        self.next_failure_id = self
            .next_failure_id
            .checked_add(1)
            .expect("runtime GC maintenance failure IDs exhausted");
        let failure = RuntimeGcMaintenanceFailure {
            id: self.next_failure_id,
            kind,
            message,
        };
        self.failures.push(failure.clone());
        self.pending_failure_reports.push(failure);
    }

    fn snapshot(&self) -> RuntimeGcMaintenanceSnapshot {
        RuntimeGcMaintenanceSnapshot {
            active_leases: self.active_leases,
            revision: self.revision,
            explicit_request: self.explicit_request,
            disposition: self.disposition,
            latest_failure: self.failures.last().cloned(),
        }
    }
}

/// One runtime-owned root whose recursive evaluator representation remains
/// private to already-validated internal evaluation.
///
/// Runtime-owned records retain this wrapper rather than a bare core value so
/// provenance cannot be lost when a value crosses a wait, task, cache, or
/// host-event storage boundary.
#[derive(Clone)]
pub(crate) struct RuntimeValueRoot {
    value: PreparedRuntimeValueRoot,
}

impl RuntimeValueRoot {
    /// Test-fixture convenience which keeps production construction on the
    /// access-qualified publication surface.
    #[cfg(test)]
    pub(crate) fn new(values: &CoreValueFactory, value: Value) -> Self {
        values.with_runtime_value_access(|access| access.root_runtime_value(value))
    }

    fn new_from_access(
        observer: crate::core::RuntimeValueObserver,
        access: &crate::core::RuntimeValueAccess<'_>,
        value: Value,
    ) -> Self {
        Self {
            value: PreparedRuntimeValueRoot::prepare_with_access(observer, access, value),
        }
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.value.runtime_id()
    }

    pub(crate) fn value_observer(&self) -> crate::core::RuntimeValueObserver {
        self.value.observer().clone()
    }

    /// Clones the compatibility core representation under matching admitted
    /// value-domain authority.
    ///
    /// This is the bounded semantic projection used by evaluator/compiler
    /// regions. Durable storage must retain this root rather than the returned
    /// compatibility shell.
    pub(crate) fn clone_core_with(&self, access: &crate::core::RuntimeValueAccess<'_>) -> Value {
        self.with_core(access, |value| access.duplicate_value(value))
            .expect("runtime root and managed access must share one value domain")
    }

    pub(crate) fn with_core<R>(
        &self,
        access: &crate::core::RuntimeValueAccess<'_>,
        operation: impl FnOnce(&Value) -> R,
    ) -> Option<R> {
        self.value.with_value(access, operation)
    }

    #[cfg(test)]
    pub(crate) fn clone_core_for_test(&self) -> Value {
        let values = self
            .value
            .observer()
            .upgrade()
            .expect("test root observation requires its live value domain");
        values.with_runtime_value_access(|access| self.clone_core_with(&access))
    }

    /// Compares this retained value without projecting an unrooted shell
    /// between access regions.
    #[cfg(test)]
    pub(crate) fn same_representation_for_test(
        &self,
        values: &CoreValueFactory,
        expected: &Value,
    ) -> bool {
        values.with_runtime_value_access(|access| {
            self.with_core(&access, |actual| {
                access.same_representation(actual, expected)
            })
            .expect("test root and value authority must share one value domain")
        })
    }

    #[cfg(test)]
    #[track_caller]
    pub(crate) fn assert_same_representation_for_test(
        &self,
        values: &CoreValueFactory,
        expected: &Value,
    ) {
        assert!(
            self.same_representation_for_test(values, expected),
            "runtime root did not retain the expected semantic representation"
        );
    }
}

impl crate::core::RuntimeValueAccess<'_> {
    /// Publishes one containing-value root through this admitted region.
    ///
    /// Every managed edge reachable from `value` may remain unpublished while
    /// the region is active. Registering the outer root here transfers its
    /// liveness to the collector before the region ends.
    pub(crate) fn root_runtime_value(&self, value: Value) -> RuntimeValueRoot {
        RuntimeValueRoot::new_from_access(self.values().runtime_value_observer(), self, value)
    }

    /// Publishes one structured evaluation failure while this admitted region
    /// still protects every direct semantic value carried by it.
    pub(crate) fn root_runtime_failure(
        &self,
        failure: Arc<EvaluationFailure>,
    ) -> RuntimeFailureRoot {
        RuntimeFailureRoot(Arc::new(RuntimeFailureRootInner {
            values: self.values().runtime_value_observer(),
            value_roots: RuntimeFailureRoot::root_direct_values(self, &failure),
            failure,
        }))
    }
}

impl PartialEq for RuntimeValueRoot {
    fn eq(&self, other: &Self) -> bool {
        self.value.same_representation(&other.value)
    }
}

impl Eq for RuntimeValueRoot {}

impl fmt::Debug for RuntimeValueRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RuntimeValueRoot")
    }
}

/// One runtime-owned failure whose direct semantic values remain rooted while
/// the compatibility failure representation is retained.
///
/// The existing `Arc<EvaluationFailure>` remains the canonical shared failure
/// identity. The parallel roots are deliberately shallow: recursive edges are
/// owned by the root for each direct emission or context value. I6C retained
/// this compatibility shell after proving that its external owner is not
/// managed-reachable and retires with its report/coordinator owner.
#[derive(Clone)]
pub(crate) struct RuntimeFailureRoot(Arc<RuntimeFailureRootInner>);

impl PartialEq for RuntimeFailureRoot {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for RuntimeFailureRoot {}

struct RuntimeFailureRootInner {
    values: crate::core::RuntimeValueObserver,
    failure: Arc<EvaluationFailure>,
    #[allow(
        dead_code,
        reason = "the I6C-audited compatibility root retains direct failure values for its external owner"
    )]
    value_roots: Box<[RuntimeValueRoot]>,
}

impl std::fmt::Debug for RuntimeFailureRoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RuntimeFailureRoot(..)")
    }
}

impl RuntimeFailureRoot {
    pub(crate) fn new(values: &CoreValueFactory, failure: Arc<EvaluationFailure>) -> Self {
        values.with_runtime_value_access(|access| access.root_runtime_failure(failure))
    }

    pub(crate) fn from_observer(
        observer: &crate::core::RuntimeValueObserver,
        failure: Arc<EvaluationFailure>,
    ) -> Self {
        let values = observer
            .upgrade()
            .expect("failure publication requires its live value domain");
        Self::new(&values, failure)
    }

    fn root_direct_values(
        access: &crate::core::RuntimeValueAccess<'_>,
        failure: &EvaluationFailure,
    ) -> Box<[RuntimeValueRoot]> {
        let mut value_roots = Vec::new();
        if let Some(value) = failure.emission_value_in(access) {
            value_roots.push(access.root_runtime_value(access.duplicate_value(value)));
        }
        value_roots.extend(
            failure
                .contexts_in(access)
                .iter()
                .map(|value| access.root_runtime_value(access.duplicate_value(value))),
        );
        value_roots.into_boxed_slice()
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.0.values.runtime_id()
    }

    pub(crate) fn value_observer(&self) -> &crate::core::RuntimeValueObserver {
        &self.0.values
    }

    pub(crate) fn as_failure(&self) -> &Arc<EvaluationFailure> {
        &self.0.failure
    }

    pub(crate) fn into_failure(self) -> Arc<EvaluationFailure> {
        self.0.failure.clone()
    }

    #[cfg(test)]
    pub(crate) fn direct_value_roots(&self) -> &[RuntimeValueRoot] {
        &self.0.value_roots
    }

    #[cfg(test)]
    pub(crate) fn shares_root_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Display for RuntimeFailureRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.failure.fmt(formatter)
    }
}

pub(crate) struct RuntimeIds {
    next_evaluation_session: AtomicU64,
    next_evaluation_work: AtomicU64,
    next_evaluation_task: AtomicU64,
    next_evaluation_wait: AtomicU64,
    next_deferred_value: AtomicU64,
    next_reasoning_session: AtomicU64,
    next_input_endpoint: AtomicU64,
    next_output_endpoint: AtomicU64,
    next_delivery: AtomicU64,
}

impl RuntimeIds {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            next_evaluation_session: AtomicU64::new(1),
            next_evaluation_work: AtomicU64::new(1),
            next_evaluation_task: AtomicU64::new(1),
            next_evaluation_wait: AtomicU64::new(1),
            next_deferred_value: AtomicU64::new(1),
            next_reasoning_session: AtomicU64::new(1),
            next_input_endpoint: AtomicU64::new(1),
            next_output_endpoint: AtomicU64::new(1),
            next_delivery: AtomicU64::new(1),
        })
    }

    #[cfg(test)]
    pub(crate) fn compiler_test_values() -> Arc<Self> {
        let ids = Self::new();
        ids.next_deferred_value
            .store(1_u64 << 63, Ordering::Relaxed);
        ids
    }

    pub(crate) fn evaluation_session(&self) -> NonZeroU64 {
        self.allocate_or_panic(
            &self.next_evaluation_session,
            "evaluation session IDs exhausted for this evaluation runtime",
        )
    }

    pub(crate) fn evaluation_work(&self) -> NonZeroU64 {
        self.allocate_or_panic(
            &self.next_evaluation_work,
            "evaluation work IDs exhausted for this evaluation runtime",
        )
    }

    pub(crate) fn evaluation_task(&self) -> Result<NonZeroU64, Arc<str>> {
        self.allocate(
            &self.next_evaluation_task,
            "evaluation task IDs exhausted for this evaluation runtime",
        )
    }

    pub(crate) fn evaluation_wait(&self) -> Result<NonZeroU64, Arc<str>> {
        self.allocate(
            &self.next_evaluation_wait,
            "evaluation wait-token IDs exhausted for this evaluation runtime",
        )
    }

    pub(crate) fn deferred_value(&self) -> NonZeroU64 {
        self.allocate_or_panic(
            &self.next_deferred_value,
            "deferred value IDs exhausted for this evaluation runtime",
        )
    }

    pub(crate) fn reasoning_session(&self) -> NonZeroU64 {
        self.allocate_or_panic(
            &self.next_reasoning_session,
            "reasoning session IDs exhausted for this evaluation runtime",
        )
    }

    pub(crate) fn input_endpoint(&self) -> Result<NonZeroU64, Arc<str>> {
        self.allocate(
            &self.next_input_endpoint,
            "runtime input endpoint IDs exhausted",
        )
    }

    pub(crate) fn output_endpoint(&self) -> Result<NonZeroU64, Arc<str>> {
        self.allocate(
            &self.next_output_endpoint,
            "runtime output endpoint IDs exhausted",
        )
    }

    pub(crate) fn delivery(&self) -> Result<NonZeroU64, Arc<str>> {
        self.allocate(&self.next_delivery, "runtime delivery IDs exhausted")
    }

    #[cfg(test)]
    pub(crate) fn exhaust_input_endpoints(&self) {
        self.next_input_endpoint.store(u64::MAX, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn exhaust_output_endpoints(&self) {
        self.next_output_endpoint.store(u64::MAX, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn exhaust_deliveries(&self) {
        self.next_delivery.store(u64::MAX, Ordering::Relaxed);
    }

    fn allocate(
        &self,
        source: &AtomicU64,
        exhausted: &'static str,
    ) -> Result<NonZeroU64, Arc<str>> {
        source
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map(|id| NonZeroU64::new(id).expect("runtime-local IDs start at one"))
            .map_err(|_| Arc::from(exhausted))
    }

    fn allocate_or_panic(&self, source: &AtomicU64, exhausted: &'static str) -> NonZeroU64 {
        self.allocate(source, exhausted).expect(exhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CoreValueFactory, LazyValue, PromisedValue, Value, test_value_factory};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn usable_empty_heap() -> HeapMaintenanceSnapshot {
        glam_gc::Heap::new_with_policy(glam_gc::CollectionPolicy::NoAuto).maintenance_snapshot()
    }

    #[test]
    fn gc_activity_leases_are_authoritative_until_the_last_retirement() {
        let admission = RuntimeMutationAdmission::new();
        let first = admission.begin_gc_activity();
        let second = admission.begin_gc_activity();

        let settlement = admission.settlement_guard();
        let active = admission.gc_maintenance_snapshot(&settlement);
        assert_eq!(active.active_leases, 2);
        assert_eq!(active.revision, 2);
        drop(settlement);

        first.finish(RuntimeGcLeaseOutcome::success(usable_empty_heap()));
        let settlement = admission.settlement_guard();
        let one_left = admission.gc_maintenance_snapshot(&settlement);
        assert_eq!(one_left.active_leases, 1);
        assert_eq!(one_left.revision, 3);
        drop(settlement);

        second.finish(RuntimeGcLeaseOutcome::success(usable_empty_heap()));
        let settlement = admission.settlement_guard();
        let idle = admission.gc_maintenance_snapshot(&settlement);
        assert_eq!(idle.active_leases, 0);
        assert_eq!(idle.revision, 4);
        assert_eq!(idle.disposition, RuntimeGcMaintenanceDisposition::Idle);
    }

    #[test]
    fn exclusive_readiness_admission_orders_before_gc_lease_registration() {
        let admission = RuntimeMutationAdmission::new();
        let settlement = admission.settlement_guard();
        let worker_admission = admission.clone();
        let (registered_tx, registered_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let lease = worker_admission.begin_gc_activity();
            registered_tx.send(()).unwrap();
            lease
        });
        assert!(
            registered_rx
                .recv_timeout(Duration::from_millis(25))
                .is_err(),
            "exclusive readiness admission must exclude lease publication"
        );
        assert_eq!(
            admission.gc_maintenance_snapshot(&settlement).active_leases,
            0
        );

        drop(settlement);
        registered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("lease should register after readiness releases admission");
        let lease = worker.join().unwrap();
        let settlement = admission.settlement_guard();
        assert_eq!(
            admission.gc_maintenance_snapshot(&settlement).active_leases,
            1
        );
        drop(settlement);
        lease.finish(RuntimeGcLeaseOutcome::success(usable_empty_heap()));
    }

    #[test]
    fn abandoned_gc_activity_is_actionable_and_wakes_a_parked_observer() {
        let admission = RuntimeMutationAdmission::new();
        let activity = admission.activity();
        let lease = admission.begin_gc_activity();
        let observed = activity.current();
        let (woke_tx, woke_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            activity.wait_for_change(observed);
            woke_tx.send(()).unwrap();
        });

        drop(lease);

        woke_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("lease retirement must wake a parked runtime observer");
        waiter.join().unwrap();
        let settlement = admission.settlement_guard();
        let snapshot = admission.gc_maintenance_snapshot(&settlement);
        assert_eq!(snapshot.active_leases, 0);
        assert_eq!(
            snapshot.disposition,
            RuntimeGcMaintenanceDisposition::RetryRequired
        );
    }

    #[test]
    fn older_failed_outcome_cannot_resurrect_retry_after_later_success() {
        let heap = glam_gc::Heap::new_with_policy(glam_gc::CollectionPolicy::NoAuto);
        heap.collect_full().unwrap();
        let older = heap.maintenance_snapshot();
        heap.collect_full().unwrap();
        let newer = heap.maintenance_snapshot();
        let mut state = RuntimeGcMaintenanceState::default();

        state.publish_outcome(RuntimeGcLeaseOutcome::success(newer));
        state.publish_outcome(RuntimeGcLeaseOutcome::failure(
            older,
            RuntimeGcMaintenanceFailureKind::CollectorPanic,
            "older attempt failed",
        ));

        assert_eq!(state.disposition, RuntimeGcMaintenanceDisposition::Idle);
        assert_eq!(state.completed_collection_epoch, 2);
        assert_eq!(state.failures.len(), 1);
    }

    #[test]
    fn runtime_failure_root_preserves_identity_and_direct_value_occurrences() {
        let values = test_value_factory();
        let repeated = Value::binary_from_text("failure root sentinel");
        let failure = Arc::new(values.with_runtime_value_access(|access| {
            EvaluationFailure::emission_in(&access, access.duplicate_value(&repeated))
                .with_context_in(&access, access.duplicate_value(&repeated))
                .with_context_in(&access, access.duplicate_value(&repeated))
        }));

        let root = RuntimeFailureRoot::new(&values, failure.clone());

        assert_eq!(root.runtime_id(), values.runtime_id());
        assert!(Arc::ptr_eq(root.as_failure(), &failure));
        assert_eq!(root.direct_value_roots().len(), 3);
        assert!(
            root.direct_value_roots()
                .iter()
                .all(|value| value.runtime_id() == values.runtime_id())
        );
        assert!(root.direct_value_roots().iter().all(|value| {
            values.same_representation_for_test(&value.clone_core_for_test(), &repeated)
        }));
        assert!(Arc::ptr_eq(&root.clone().into_failure(), &failure));
        assert_eq!(
            std::mem::size_of::<RuntimeFailureRoot>(),
            std::mem::size_of::<usize>(),
            "durable failure roots should remain one shared pointer"
        );
    }

    #[test]
    fn runtime_failure_root_can_be_published_from_known_runtime_provenance() {
        let values = test_value_factory();
        let failure = Arc::new(EvaluationFailure::message("known runtime failure"));

        let root = RuntimeFailureRoot::new(&values, failure.clone());

        assert_eq!(root.runtime_id(), values.runtime_id());
        assert!(Arc::ptr_eq(root.as_failure(), &failure));
        assert_eq!(root.direct_value_roots().len(), 0);
    }

    #[test]
    fn runtime_failure_root_does_not_force_or_recursively_visit_values() {
        let values = test_value_factory();
        let forced = Arc::new(AtomicBool::new(false));
        let forced_by_thunk = forced.clone();
        let lazy = Value::Lazy(LazyValue::semantic_thunk(
            &values,
            "runtime failure root sentinel",
            move |_| {
                forced_by_thunk.store(true, Ordering::Release);
                panic!("failure-root construction must not evaluate a direct value")
            },
        ));
        let failure = Arc::new(values.with_runtime_value_access(|access| {
            EvaluationFailure::emission_in(&access, access.duplicate_value(&lazy))
        }));

        let root = RuntimeFailureRoot::new(&values, failure);

        assert_eq!(root.direct_value_roots().len(), 1);
        values.assert_same_representation_for_test(
            &root.direct_value_roots()[0].clone_core_for_test(),
            &lazy,
        );
        assert!(!forced.load(Ordering::Acquire));
    }

    #[test]
    fn runtime_failure_root_alone_retains_and_releases_its_managed_values() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let baseline = values
            .collect_managed_for_test()
            .expect("the failure-root fixture should start collectible");
        let (promise_root, failure) = values.with_runtime_value_access(|access| {
            let promise_root = access
                .construct_rooted_managed_promise("failure root lifecycle")
                .expect("the managed promise cell should fit a run");
            let promise = PromisedValue::from_root(&promise_root, &access);
            (
                promise_root,
                Arc::new(EvaluationFailure::emission_in(
                    &access,
                    Value::Promised(promise),
                )),
            )
        });
        let failure_root = RuntimeFailureRoot::new(&values, failure);
        drop(promise_root);

        let live = values
            .collect_managed_for_test()
            .expect("the durable failure root should retain its direct value graph");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 2);

        drop(failure_root);
        let dead = values
            .collect_managed_for_test()
            .expect("retiring the durable failure root should release its graph");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 2);
    }
}
