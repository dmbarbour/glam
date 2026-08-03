//! Runtime-local identity allocation shared by evaluation subsystems.
//!
//! `EvaluationRuntimeId` remains process-global in the public facade. Every
//! narrower identity is allocated from one of these runtime-owned counters and
//! is therefore interpreted together with its runtime.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct RuntimeIds {
    next_evaluation_session: AtomicU64,
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
