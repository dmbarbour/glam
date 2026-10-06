//! Profiling counters and phase timers for `glam-prof` builds.
//!
//! Ordinary builds compile none of this. Counters are relaxed atomics owned
//! by a runtime's value domain: a snapshot is internally race-safe but is not
//! a synchronization point. Exact work counts come from these counters, never
//! from step budgets.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::interaction_net::profiling::InteractionNetProfileSnapshot;

/// Declares a relaxed atomic counter struct and the plain snapshot it reads
/// into, with a JSON writer for the snapshot.
macro_rules! atomic_counts {
    (
        $name:ident => $snapshot:ident {
            $($field:ident),+ $(,)?
        }
    ) => {
        #[derive(Default)]
        struct $name {
            $(pub(super) $field: ::std::sync::atomic::AtomicU64),+
        }

        impl $name {
            fn snapshot(&self) -> $snapshot {
                $snapshot {
                    $($field: self.$field.load(::std::sync::atomic::Ordering::Relaxed)),+
                }
            }
        }

        impl $snapshot {
            /// Writes this snapshot as one JSON object of counters.
            pub fn write_json(&self, out: &mut String) {
                $crate::profiling::write_counters(out, &[$((stringify!($field), self.$field)),+]);
            }
        }
    };
}
pub(crate) use atomic_counts;

/// Reductions charged to the step budget, by kind (decision
/// `reduction-costs-one-budget-unit`). Net rule applications are counted by
/// kind in the interaction-net profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EvaluationReductionCounts {
    pub whnf_delegations: u64,
    pub builtin_steps: u64,
    pub immediate_builtins: u64,
    pub reflection_steps: u64,
}

/// Wall time per phase, summed across threads, in nanoseconds, plus the
/// number of completed collections.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhaseTimes {
    pub parse_ns: u64,
    pub lower_ns: u64,
    pub collect_ns: u64,
    pub collections: u64,
}

atomic_counts!(AtomicEvaluationReductionCounts => EvaluationReductionCounts {
    whnf_delegations,
    builtin_steps,
    immediate_builtins,
    reflection_steps,
});

atomic_counts!(AtomicPhaseTimes => PhaseTimes {
    parse_ns,
    lower_ns,
    collect_ns,
    collections,
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EvaluationReduction {
    WhnfDelegation,
    BuiltinStep,
    ImmediateBuiltin,
    ReflectionStep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Parse,
    Lower,
    Collect,
}

/// Runtime-owned evaluation counters and phase timers.
#[derive(Default)]
pub(crate) struct EvaluationProfile {
    reductions: AtomicEvaluationReductionCounts,
    phases: AtomicPhaseTimes,
}

impl EvaluationProfile {
    pub(crate) fn record_reduction(&self, reduction: EvaluationReduction) {
        let counter = match reduction {
            EvaluationReduction::WhnfDelegation => &self.reductions.whnf_delegations,
            EvaluationReduction::BuiltinStep => &self.reductions.builtin_steps,
            EvaluationReduction::ImmediateBuiltin => &self.reductions.immediate_builtins,
            EvaluationReduction::ReflectionStep => &self.reductions.reflection_steps,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_phase(&self, phase: Phase, elapsed: Duration) {
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        let counter = match phase {
            Phase::Parse => &self.phases.parse_ns,
            Phase::Lower => &self.phases.lower_ns,
            Phase::Collect => {
                self.phases.collections.fetch_add(1, Ordering::Relaxed);
                &self.phases.collect_ns
            }
        };
        counter.fetch_add(nanos, Ordering::Relaxed);
    }

    /// Runs `work`, adding its wall time to `phase`.
    pub(crate) fn time<R>(&self, phase: Phase, work: impl FnOnce() -> R) -> R {
        let start = Instant::now();
        let result = work();
        self.record_phase(phase, start.elapsed());
        result
    }

    pub(crate) fn snapshot(&self) -> (EvaluationReductionCounts, PhaseTimes) {
        (self.reductions.snapshot(), self.phases.snapshot())
    }
}

/// One runtime's profiling counters at a point in time.
#[derive(Clone, Debug)]
pub struct RuntimeProfileSnapshot {
    pub reductions: EvaluationReductionCounts,
    pub phases: PhaseTimes,
    pub net: InteractionNetProfileSnapshot,
    /// Collector telemetry, absent once the heap is poisoned.
    pub heap: Option<glam_gc::HeapMetrics>,
}

impl RuntimeProfileSnapshot {
    /// Writes this snapshot as one JSON object, with a nested object per
    /// counter group.
    pub fn write_json(&self, out: &mut String) {
        out.push('{');
        out.push_str("\"reductions\":");
        self.reductions.write_json(out);
        out.push_str(",\"phases\":");
        self.phases.write_json(out);
        out.push_str(",\"net_reductions\":");
        self.net.reductions.write_json(out);
        out.push_str(",\"net_driver\":");
        self.net.driver.write_json(out);
        out.push_str(",\"coordinator\":");
        let calls = &self.net.coordinator_notifications.calls;
        write_counters(
            out,
            &[
                ("notify_one", calls.notify_one.total()),
                ("notify_all", calls.notify_all.total()),
            ],
        );
        out.push_str(",\"heap\":");
        let Some(heap) = self.heap else {
            out.push_str("null}");
            return;
        };
        write_counters(
            out,
            &[
                (
                    "allocations",
                    heap.class_cache_hits() + heap.class_cache_misses(),
                ),
                ("root_registrations", heap.root_registrations()),
                ("outer_access_regions", heap.outer_mutator_entries()),
                ("recursive_access_regions", heap.recursive_mutator_entries()),
                ("collection_attempts", heap.collection_attempts()),
                ("successful_collections", heap.successful_collections()),
                ("assigned_runs", heap.assigned_runs() as u64),
                ("free_runs", heap.free_runs() as u64),
                ("arena_chunks", heap.arena_chunks() as u64),
                ("allocated_slots", heap.allocated_slots() as u64),
                (
                    "assigned_slot_capacity",
                    heap.assigned_slot_capacity() as u64,
                ),
            ],
        );
        out.push('}');
    }
}

/// Writes `{"name":value,...}`. Names are Rust identifiers, so they need no
/// escaping.
pub(crate) fn write_counters(out: &mut String, counters: &[(&str, u64)]) {
    out.push('{');
    for (index, (name, value)) in counters.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(name);
        out.push_str("\":");
        out.push_str(&value.to_string());
    }
    out.push('}');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_write_as_one_flat_json_object() {
        let mut out = String::new();
        write_counters(&mut out, &[("a", 1), ("b", 22)]);
        assert_eq!(out, "{\"a\":1,\"b\":22}");
        let mut empty = String::new();
        write_counters(&mut empty, &[]);
        assert_eq!(empty, "{}");
    }

    #[test]
    fn phases_accumulate_time_and_count_collections() {
        let profile = EvaluationProfile::default();
        profile.record_phase(Phase::Collect, Duration::from_nanos(5));
        profile.record_phase(Phase::Collect, Duration::from_nanos(7));
        profile.record_reduction(EvaluationReduction::WhnfDelegation);
        let (reductions, phases) = profile.snapshot();
        assert_eq!(phases.collect_ns, 12);
        assert_eq!(phases.collections, 2);
        assert_eq!(reductions.whnf_delegations, 1);
    }
}
