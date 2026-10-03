//! Private deterministic-test-hook home.
//!
//! Hooks are compiled only when the private verification feature is selected.
//! They may observe or pause a production transition, but must not alter its
//! semantic result.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::{
    Gc, Mutator, Trace, UnsupportedLayout, Visitor,
    class::metadata_for,
    run::{RUN_HEADER_SIZE, RUN_SIZE, RunGeometry},
    trace::ErasedGc,
};

/// Private-fixture view of the fixed-run geometry for one representation.
///
/// This supports repository tuning measurements without promoting run layout
/// into the collector's downstream configuration surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GeometryMeasurement {
    run_bytes: usize,
    header_bytes: usize,
    slot_stride: usize,
    slot_count: usize,
    allocation_bitmap_bytes: usize,
    lease_bitmap_bytes: usize,
    mark_bitmap_bytes: usize,
    bitmap_padding_bytes: usize,
    payload_bytes: usize,
    tail_slack_bytes: usize,
}

macro_rules! geometry_accessors {
    ($(($name:ident, $field:ident)),* $(,)?) => {
        impl GeometryMeasurement {
            $(
                #[must_use]
                pub const fn $name(self) -> usize {
                    self.$field
                }
            )*
        }
    };
}

geometry_accessors! {
    (run_bytes, run_bytes),
    (header_bytes, header_bytes),
    (slot_stride, slot_stride),
    (slot_count, slot_count),
    (allocation_bitmap_bytes, allocation_bitmap_bytes),
    (lease_bitmap_bytes, lease_bitmap_bytes),
    (mark_bitmap_bytes, mark_bitmap_bytes),
    (bitmap_padding_bytes, bitmap_padding_bytes),
    (payload_bytes, payload_bytes),
    (tail_slack_bytes, tail_slack_bytes),
}

/// Derives the collector-private fixed-run geometry for a measurement type.
///
/// This private-feature function is not a supported downstream layout API.
pub fn geometry_measurement<T: Trace>() -> Result<GeometryMeasurement, UnsupportedLayout> {
    let metadata = metadata_for::<T>();
    let geometry = RunGeometry::derive(metadata.layout(), metadata.requested_slot_size())
        .map_err(UnsupportedLayout::from_validated_geometry)?;
    let allocation_bitmap_bytes = geometry.allocation_bitmap.byte_len();
    let lease_bitmap_bytes = geometry.lease_bitmap.byte_len();
    let mark_bitmap_bytes = geometry.mark_bitmap.byte_len();
    let bitmap_end = geometry.mark_bitmap.end();
    let payload_bytes = geometry
        .slot_count
        .checked_mul(geometry.slot_stride)
        .expect("validated geometry payload bytes overflowed");
    Ok(GeometryMeasurement {
        run_bytes: RUN_SIZE,
        header_bytes: RUN_HEADER_SIZE,
        slot_stride: geometry.slot_stride,
        slot_count: geometry.slot_count,
        allocation_bitmap_bytes,
        lease_bitmap_bytes,
        mark_bitmap_bytes,
        bitmap_padding_bytes: geometry
            .first_slot_offset
            .checked_sub(bitmap_end)
            .expect("validated geometry overlaps its bitmaps"),
        payload_bytes,
        tail_slack_bytes: RUN_SIZE
            .checked_sub(geometry.first_slot_offset + payload_bytes)
            .expect("validated geometry exceeds its run"),
    })
}

/// Checks the ordinary root-publication predicate without creating a root.
///
/// This private-feature operation exists only to measure finalization lookup
/// costs. It provides no liveness and is not a supported observation API.
#[must_use]
pub fn is_rootable_for_measurement<T: Trace>(mutator: &Mutator<'_>, value: &Gc<T>) -> bool {
    mutator.is_rootable_for_measurement(value)
}

/// One-shot observation that a synchronous collector is blocked by a mutator.
///
/// Reaching this probe proves that the collection target has been reserved and
/// that authoritative coordinator state still contains an active outer
/// mutator. The collector does not wait on the probe itself.
pub struct SynchronousCollectionWaitProbe {
    pub(crate) state: Arc<SynchronousCollectionWaitProbeState>,
}

impl SynchronousCollectionWaitProbe {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(SynchronousCollectionWaitProbeState {
                reached: Mutex::new(false),
                changed: Condvar::new(),
            }),
        }
    }

    /// Waits at most `timeout` for the collector to observe its blocking
    /// mutator and returns whether that authoritative boundary was reached.
    #[must_use]
    pub fn wait_until_reached(&self, timeout: Duration) -> bool {
        let reached = self
            .state
            .reached
            .lock()
            .expect("collection-wait test probe was poisoned");
        let (reached, _) = self
            .state
            .changed
            .wait_timeout_while(reached, timeout, |reached| !*reached)
            .expect("collection-wait test probe was poisoned");
        *reached
    }
}

pub(crate) struct SynchronousCollectionWaitProbeState {
    reached: Mutex<bool>,
    changed: Condvar,
}

impl SynchronousCollectionWaitProbeState {
    pub(crate) fn reach(&self) {
        let mut reached = self
            .reached
            .lock()
            .expect("collection-wait test probe was poisoned");
        *reached = true;
        self.changed.notify_all();
    }
}

/// One-shot pause after a collector has established its Finalizing phase.
///
/// The collector holds its finalizer mutator admission, but no collector
/// component mutex, while it waits for [`Self::release`]. This hook exists
/// only to force request/finalization orderings in repository verification.
pub struct FinalizingPhaseProbe {
    pub(crate) state: Arc<FinalizingPhaseProbeState>,
}

impl FinalizingPhaseProbe {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(FinalizingPhaseProbeState {
                gate: Mutex::new(FinalizingPhaseGate {
                    reached: false,
                    released: false,
                }),
                changed: Condvar::new(),
            }),
        }
    }

    /// Waits until the collector is paused in Finalizing.
    pub fn wait_until_reached(&self) {
        let mut gate = self
            .state
            .gate
            .lock()
            .expect("finalizing-phase test probe was poisoned");
        while !gate.reached {
            gate = self
                .state
                .changed
                .wait(gate)
                .expect("finalizing-phase test probe was poisoned");
        }
    }

    /// Releases the paused collector. Repeated release is harmless.
    pub fn release(&self) {
        self.state.release();
    }
}

impl Drop for FinalizingPhaseProbe {
    fn drop(&mut self) {
        self.state.release();
    }
}

struct FinalizingPhaseGate {
    reached: bool,
    released: bool,
}

pub(crate) struct FinalizingPhaseProbeState {
    gate: Mutex<FinalizingPhaseGate>,
    changed: Condvar,
}

impl FinalizingPhaseProbeState {
    pub(crate) fn reach_and_wait(&self) {
        let mut gate = self
            .gate
            .lock()
            .expect("finalizing-phase test probe was poisoned");
        gate.reached = true;
        self.changed.notify_all();
        while !gate.released {
            gate = self
                .changed
                .wait(gate)
                .expect("finalizing-phase test probe was poisoned");
        }
    }

    fn release(&self) {
        let mut gate = self
            .gate
            .lock()
            .expect("finalizing-phase test probe was poisoned");
        gate.released = true;
        self.changed.notify_all();
    }
}

/// Selects the edge sets traversed by a deterministic mutation probe.
///
/// This is a verification-only approximation of future collector policies;
/// it is not a supported downstream collector configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EdgeTransitionObservation {
    /// Traverse neither side, like the current stop-the-world barrier.
    Neither,
    /// Traverse only edges removed by the mutation, like the provisional SATB
    /// barrier.
    Leaving,
    /// Traverse only newly installed edges.
    Adding,
    /// Traverse both sides.
    Both,
}

impl EdgeTransitionObservation {
    pub(crate) fn leaving(self) -> bool {
        matches!(self, Self::Leaving | Self::Both)
    }

    pub(crate) fn adding(self) -> bool {
        matches!(self, Self::Adding | Self::Both)
    }
}

/// One observed owner-qualified edge-set transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EdgeTransitionRecord {
    leaving_edges: usize,
    adding_edges: usize,
}

impl EdgeTransitionRecord {
    /// Returns the number of leaving edges reported by the selected visitor.
    #[must_use]
    pub const fn leaving_edges(self) -> usize {
        self.leaving_edges
    }

    /// Returns the number of adding edges reported by the selected visitor.
    #[must_use]
    pub const fn adding_edges(self) -> usize {
        self.adding_edges
    }
}

/// Retained observations from one heap-local deterministic mutation probe.
#[derive(Clone)]
pub struct EdgeTransitionProbe {
    pub(crate) state: Arc<EdgeTransitionProbeState>,
}

impl EdgeTransitionProbe {
    pub(crate) fn new(observation: EdgeTransitionObservation) -> Self {
        Self {
            state: Arc::new(EdgeTransitionProbeState {
                observation,
                records: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Returns a stable copy of all transitions observed so far.
    #[must_use]
    pub fn records(&self) -> Vec<EdgeTransitionRecord> {
        self.state
            .records
            .lock()
            .expect("edge-transition test probe was poisoned")
            .clone()
    }
}

pub(crate) struct EdgeTransitionProbeState {
    observation: EdgeTransitionObservation,
    records: Mutex<Vec<EdgeTransitionRecord>>,
}

impl EdgeTransitionProbeState {
    pub(crate) fn observe<Leaving, Adding>(
        &self,
        mutator: &Mutator<'_>,
        leaving: &Leaving,
        adding: &Adding,
    ) where
        Leaving: for<'visit> Fn(&mut Visitor<'visit>),
        Adding: for<'visit> Fn(&mut Visitor<'visit>),
    {
        let leaving_edges = if self.observation.leaving() {
            count_and_validate(mutator, leaving)
        } else {
            0
        };
        let adding_edges = if self.observation.adding() {
            count_and_validate(mutator, adding)
        } else {
            0
        };
        self.records
            .lock()
            .expect("edge-transition test probe was poisoned")
            .push(EdgeTransitionRecord {
                leaving_edges,
                adding_edges,
            });
    }

    pub(crate) fn observe_state_transition<State, Leaving, Adding, Result>(
        &self,
        mutator: &Mutator<'_>,
        state: &mut State,
        leaving: &Leaving,
        adding: &Adding,
        transition: impl FnOnce(&mut State) -> Result,
    ) -> Result
    where
        Leaving: for<'visit> Fn(&State, &mut Visitor<'visit>),
        Adding: for<'visit> Fn(&State, &mut Visitor<'visit>),
    {
        let leaving_edges = if self.observation.leaving() {
            count_state_and_validate(mutator, state, leaving)
        } else {
            0
        };
        let result = transition(state);
        let adding_edges = if self.observation.adding() {
            count_state_and_validate(mutator, state, adding)
        } else {
            0
        };
        self.records
            .lock()
            .expect("edge-transition test probe was poisoned")
            .push(EdgeTransitionRecord {
                leaving_edges,
                adding_edges,
            });
        result
    }
}

fn count_and_validate<Edges>(mutator: &Mutator<'_>, edges: &Edges) -> usize
where
    Edges: for<'visit> Fn(&mut Visitor<'visit>),
{
    let mut count = 0usize;
    let mut visit = |edge: ErasedGc| {
        mutator.assert_observed_edge_for_test(edge);
        count = count.checked_add(1).expect("observed edge count exhausted");
    };
    edges(&mut Visitor::new(&mut visit));
    count
}

fn count_state_and_validate<State, Edges>(
    mutator: &Mutator<'_>,
    state: &State,
    edges: &Edges,
) -> usize
where
    Edges: for<'visit> Fn(&State, &mut Visitor<'visit>),
{
    let mut count = 0usize;
    let mut visit = |edge: ErasedGc| {
        mutator.assert_observed_edge_for_test(edge);
        count = count.checked_add(1).expect("observed edge count exhausted");
    };
    edges(state, &mut Visitor::new(&mut visit));
    count
}
