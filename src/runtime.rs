//! Runtime-local identity allocation shared by evaluation subsystems.
//!
//! `EvaluationRuntimeId` remains process-global. Every narrower identity is
//! allocated from one of these runtime-owned counters and is therefore
//! interpreted together with its runtime.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::core::{CoreValueFactory, Value};

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
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
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
}

pub(crate) struct RuntimeMutationGuard<'a> {
    guard: Option<RwLockReadGuard<'a, ()>>,
    activity: &'a RuntimeActivityState,
}

pub(crate) struct RuntimeSettlementGuard<'a> {
    _guard: RwLockWriteGuard<'a, ()>,
}

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
    generation: Mutex<u64>,
    changed: Condvar,
    #[cfg(test)]
    waits: AtomicU64,
}

impl RuntimeActivityState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            generation: Mutex::new(0),
            changed: Condvar::new(),
            #[cfg(test)]
            waits: AtomicU64::new(0),
        })
    }

    pub(crate) fn current(&self) -> u64 {
        *self
            .generation
            .lock()
            .expect("runtime activity mutex should not be poisoned")
    }

    fn advance(&self) {
        let mut generation = self
            .generation
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        *generation = generation
            .checked_add(1)
            .expect("runtime activity generations exhausted");
        drop(generation);
        self.changed.notify_all();
    }

    pub(crate) fn wait_for_change(&self, observed: u64) {
        #[cfg(test)]
        self.waits.fetch_add(1, Ordering::Relaxed);
        let mut generation = self
            .generation
            .lock()
            .expect("runtime activity mutex should not be poisoned");
        while *generation == observed {
            generation = self
                .changed
                .wait(generation)
                .expect("runtime activity mutex should not be poisoned");
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_count(&self) -> u64 {
        self.waits.load(Ordering::Relaxed)
    }
}

/// One runtime-owned root whose recursive evaluator representation remains
/// private to already-validated internal evaluation.
///
/// Runtime-owned records retain this wrapper rather than a bare core value so
/// provenance cannot be lost when a value crosses a wait, task, cache, or
/// host-event storage boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeValueRoot {
    runtime: EvaluationRuntimeId,
    value: Value,
}

impl RuntimeValueRoot {
    pub(crate) fn new(values: &CoreValueFactory, value: Value) -> Self {
        Self {
            runtime: values.runtime_id(),
            value,
        }
    }

    pub(crate) fn from_runtime(runtime: EvaluationRuntimeId, value: Value) -> Self {
        Self { runtime, value }
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub(crate) fn as_core(&self) -> &Value {
        &self.value
    }

    pub(crate) fn into_core(self) -> Value {
        self.value
    }
}

pub(crate) struct RuntimeIds {
    next_evaluation_session: AtomicU64,
    next_evaluation_work: AtomicU64,
    next_evaluation_task: AtomicU64,
    next_evaluation_wait: AtomicU64,
    next_deferred_value: AtomicU64,
    next_reasoning_session: AtomicU64,
    next_cli_invocation: AtomicU64,
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
            next_cli_invocation: AtomicU64::new(1),
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

    pub(crate) fn cli_invocation(&self) -> NonZeroU64 {
        self.allocate_or_panic(
            &self.next_cli_invocation,
            "CLI invocation IDs exhausted for this evaluation runtime",
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
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map(|id| NonZeroU64::new(id).expect("runtime-local IDs start at one"))
            .map_err(|_| Arc::from(exhausted))
    }

    fn allocate_or_panic(&self, source: &AtomicU64, exhausted: &'static str) -> NonZeroU64 {
        self.allocate(source, exhausted).expect(exhausted)
    }
}
