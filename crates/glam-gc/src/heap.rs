use std::collections::HashMap;
use std::num::NonZeroU64;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};

use crate::{
    Mutator, Root, Trace, Visitor,
    arena::{Arena, RunClaimTarget, RunLocation, RunOwner, RunPublicationError},
    class::{
        AllocationClass, AllocationClassEntry, MetadataIdentity, ObjectMetadata, metadata_for,
    },
    root::RootCell,
    run::{AllocationClassId, RunGeometry},
    thread_cache::{
        AllocationCursor, AllocationLeaseEpoch, ThreadHeapEntry, remove_inactive_thread_cache,
        thread_has_any_active_mutator,
    },
    trace::ErasedGc,
};

const FIXED_SURVIVOR_RUN_HEADROOM: usize = crate::arena::RUNS_PER_CHUNK * 7 / 8;
const SURVIVOR_GROWTH_NUMERATOR: usize = 1;
const SURVIVOR_GROWTH_DENOMINATOR: usize = 2;
const _: () = assert!(FIXED_SURVIVOR_RUN_HEADROOM != 0);
const _: () = assert!(FIXED_SURVIVOR_RUN_HEADROOM < crate::arena::RUNS_PER_CHUNK);
const _: () = assert!(SURVIVOR_GROWTH_DENOMINATOR != 0);

/// Scalar results from one completed stop-the-world collection.
///
/// The report is published atomically with its completion epoch. It contains
/// no bitmap, allocation identity, or other retained collection history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CollectionReport {
    epoch: NonZeroU64,
    root_entries: usize,
    traced_objects: usize,
    marked_slots: usize,
    conservatively_retained_slots: usize,
    reclaimed_slots: usize,
    finalized_slots: usize,
    reclaimed_runs: usize,
    #[cfg(test)]
    peak_object_worklist_len: usize,
    #[cfg(test)]
    peak_object_worklist_capacity: usize,
}

impl CollectionReport {
    /// Returns this heap's monotonically increasing collection epoch.
    #[must_use]
    pub const fn epoch(self) -> u64 {
        self.epoch.get()
    }

    /// Returns the number of live external-root registry entries visited.
    ///
    /// Distinct roots to the same allocation count separately; clones of one
    /// root share a registry entry.
    #[must_use]
    pub const fn root_entries(self) -> usize {
        self.root_entries
    }

    /// Returns the number of managed objects whose `Trace` implementation ran.
    #[must_use]
    pub const fn traced_objects(self) -> usize {
        self.traced_objects
    }

    /// Returns the number of distinct managed slots marked reachable.
    #[must_use]
    pub const fn marked_slots(self) -> usize {
        self.marked_slots
    }

    /// Returns slots retained without dispatching their `Trace` implementation.
    ///
    /// This is zero during the mark-only C5 collector. C6 uses it for durable
    /// pending finalizers retained across a recovered destructor panic without
    /// tracing their payloads.
    #[must_use]
    pub const fn conservatively_retained_slots(self) -> usize {
        self.conservatively_retained_slots
    }

    /// Returns the number of managed allocations retired by this collection.
    ///
    /// This includes eagerly swept no-drop allocations and terminal destructor
    /// attempts. [`CollectionReport::finalized_slots`] is the destructor-bearing
    /// subset.
    #[must_use]
    pub const fn reclaimed_slots(self) -> usize {
        self.reclaimed_slots
    }

    /// Returns the number of destructor-bearing allocations terminally retired.
    ///
    /// A destructor which unwinds does not produce a successful collection
    /// report. Its terminal retirement is therefore not reported by a later
    /// collection, while untouched obligations finalized by that later attempt
    /// are counted normally.
    #[must_use]
    pub const fn finalized_slots(self) -> usize {
        self.finalized_slots
    }

    /// Returns the number of emptied runs reset into the heap-wide free pool.
    #[must_use]
    pub const fn reclaimed_runs(self) -> usize {
        self.reclaimed_runs
    }
}

/// Operational finalizer activity currently retained by a heap.
///
/// A run-local finalization attempt claims all of its obligations together.
/// They remain `running` until that run commits, including any successfully
/// destroyed prefix whose allocation bits have not yet been published. A
/// recovered panic returns only the untouched suffix to `queued` activity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeapActivity {
    queued_finalizers: usize,
    running_finalizers: usize,
}

impl HeapActivity {
    /// Returns finalizer obligations not claimed by the current run attempt.
    #[must_use]
    pub const fn queued_finalizers(self) -> usize {
        self.queued_finalizers
    }

    /// Returns finalizer obligations claimed by the current run attempt.
    #[must_use]
    pub const fn running_finalizers(self) -> usize {
        self.running_finalizers
    }

    /// Returns whether no queued or running finalizer obligation remains.
    #[must_use]
    pub const fn is_idle(self) -> bool {
        self.queued_finalizers == 0 && self.running_finalizers == 0
    }
}

/// A synchronous collection request which cannot enter the coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionError {
    /// The calling thread already holds a mutator for some heap.
    ActiveMutator,
    /// A prior collection crossed an irreversible boundary and then panicked.
    Poisoned,
}

impl std::fmt::Display for CollectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActiveMutator => {
                formatter.write_str("cannot collect while this thread holds a mutator")
            }
            Self::Poisoned => formatter.write_str("managed heap is permanently poisoned"),
        }
    }
}

impl std::error::Error for CollectionError {}

/// One shareable, runtime-local managed-value domain.
///
/// The heap owns canonical allocation classes, typed-run topology, mark state,
/// and every arena payload. Full collection eagerly reclaims dead no-drop
/// allocations and runs, and invokes drop-bearing payload destructors under a
/// collector-owned finalizer mutator.
#[derive(Clone, Default)]
pub struct Heap {
    inner: Arc<HeapInner>,
}

impl Heap {
    /// Creates an empty managed heap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Releases every inactive Glam GC cache retained by the calling thread.
    ///
    /// This is optional maintenance for long-lived host threads which outlive
    /// one or more heaps. It never returns leases to a live heap: forgotten
    /// leases remain unavailable until a future full collection. Calling it
    /// while this thread holds a mutator for any heap is a contract violation
    /// and panics before releasing any record.
    pub fn release_current_thread_caches() -> usize {
        crate::thread_cache::release_current_thread_caches()
    }

    /// Runs `operation` inside a scoped mutator region for this heap.
    ///
    /// The outermost entry obtains one coordinator admission obligation.
    /// Recursive same-heap entries reuse that obligation, while entries into
    /// different heaps remain independent.
    ///
    /// # Panics
    ///
    /// Panics if a prior collection crossed an irreversible mutation boundary
    /// and then panicked, permanently poisoning this heap.
    pub fn with_mutator<R>(&self, operation: impl for<'heap> FnOnce(&Mutator<'heap>) -> R) -> R {
        self.with_mutator_after_admission(|| {}, operation)
    }

    fn with_mutator_after_admission<R>(
        &self,
        after_admission: impl FnOnce(),
        operation: impl for<'heap> FnOnce(&Mutator<'heap>) -> R,
    ) -> R {
        let prepared =
            ThreadHeapEntry::prepare(&self.inner, self.inner.current_allocation_lease_epoch());
        let outer = prepared.is_outer();
        let (prepared, admission) = if outer {
            let (admission, collected) = self.inner.admit_outer_mutator();
            let prepared = if collected {
                drop(prepared);
                ThreadHeapEntry::prepare(&self.inner, self.inner.current_allocation_lease_epoch())
            } else {
                prepared
            };
            (prepared, Some(admission))
        } else {
            (prepared, None)
        };
        after_admission();
        let thread_entry =
            prepared.activate(self.inner.current_allocation_lease_epoch(), admission);
        let mutator = Mutator::new(&self.inner, thread_entry.cache());
        let result = operation(&mutator);
        drop(mutator);
        drop(thread_entry);

        result
    }

    /// Records an idempotent, nonblocking full-collection request.
    ///
    /// The request is serviced when a later outer mutator entry finds the heap
    /// idle, or by [`Heap::collect_full`]. Calling this inside a mutator never
    /// waits on the calling region.
    ///
    /// # Panics
    ///
    /// Panics if the heap is permanently poisoned.
    pub fn request_collection(&self) {
        self.inner.request_collection();
    }

    /// Returns a coherent snapshot of this heap's finalizer activity.
    ///
    /// This is operational host information rather than managed-program
    /// semantics. Later runtime integration combines it with worker, task,
    /// diagnostic, and event activity when deciding quiescence.
    ///
    /// # Panics
    ///
    /// Panics if the heap is permanently poisoned. An activity observation
    /// which began before poison publication may complete with its pre-poison
    /// snapshot, like any already-admitted host observation.
    #[must_use]
    pub fn activity(&self) -> HeapActivity {
        self.inner.activity()
    }

    /// Completes a full stop-the-world collection handshake synchronously.
    ///
    /// The collector clears mark state, seeds the stable root registry, traces
    /// the reachable graph non-recursively, and returns the latest completed
    /// scalar report which satisfies this call's requested epoch. It currently
    /// reclaims wholly dead runs with no destructor obligation, clears dead
    /// allocations from retained partial no-drop runs, and invokes exact
    /// drop-required identities outside collector locks under an installed
    /// finalizer mutator. A destructor retires its allocation after either
    /// returning or unwinding to the collector boundary. On unwind, untouched
    /// finalizers remain pending for a later collection before the original
    /// panic resumes.
    /// A concurrent later collection may overtake a waiting caller, and no
    /// unbounded report history is retained.
    ///
    /// # Errors
    ///
    /// Returns [`CollectionError::ActiveMutator`] when the calling thread
    /// already holds a mutator, or [`CollectionError::Poisoned`] after an
    /// irreversible collection panic has made further collection unsafe.
    pub fn collect_full(&self) -> Result<CollectionReport, CollectionError> {
        if self.inner.is_poisoned() {
            return Err(CollectionError::Poisoned);
        }
        if thread_has_any_active_mutator() {
            return Err(CollectionError::ActiveMutator);
        }
        self.inner.collect_full()
    }

    #[cfg(test)]
    fn with_mutator_admission_hook<R>(
        &self,
        after_admission: impl FnOnce(),
        operation: impl for<'heap> FnOnce(&Mutator<'heap>) -> R,
    ) -> R {
        self.with_mutator_after_admission(after_admission, operation)
    }
}

pub(crate) struct HeapInner {
    coordinator: Mutex<MutatorCoordinator>,
    data: Mutex<ManagedData>,
    poisoned: AtomicBool,
    collection_requested: AtomicBool,
    admission_changed: Condvar,
    allocation_lease_epoch: AtomicU64,
    #[cfg(test)]
    allocation_cursor_claims: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    allocation_cursor_slow_paths: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    allocation_cursor_locked_recheck_hits: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    allocation_cursor_frontier_advance_attempts: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    allocation_cursor_slow_path_hook: Mutex<Option<AllocationCursorSlowPathHook>>,
    #[cfg(test)]
    collection_acknowledgement_hook: Mutex<Option<CollectionAcknowledgementHook>>,
    #[cfg(test)]
    panic_after_topology_mutation: AtomicBool,
    #[cfg(test)]
    panic_after_finalizer_terminal_recording: AtomicBool,
    #[cfg(test)]
    poisoned_outer_mutator_releases: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    coordinator_notifications: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
#[derive(Clone)]
struct AllocationCursorSlowPathHook {
    arrived: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

#[cfg(test)]
#[derive(Clone)]
struct CollectionAcknowledgementHook {
    arrived: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

#[cfg(test)]
struct FinalizedWordReleaseHook {
    location: RunLocation,
    word_index: usize,
    arrived: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

impl Default for HeapInner {
    fn default() -> Self {
        Self {
            coordinator: Mutex::new(MutatorCoordinator::default()),
            data: Mutex::new(ManagedData::default()),
            poisoned: AtomicBool::new(false),
            collection_requested: AtomicBool::new(false),
            admission_changed: Condvar::new(),
            allocation_lease_epoch: AtomicU64::new(AllocationLeaseEpoch::INITIAL.get()),
            #[cfg(test)]
            allocation_cursor_claims: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            allocation_cursor_slow_paths: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            allocation_cursor_locked_recheck_hits: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            allocation_cursor_frontier_advance_attempts: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            allocation_cursor_slow_path_hook: Mutex::new(None),
            #[cfg(test)]
            collection_acknowledgement_hook: Mutex::new(None),
            #[cfg(test)]
            panic_after_topology_mutation: AtomicBool::new(false),
            #[cfg(test)]
            panic_after_finalizer_terminal_recording: AtomicBool::new(false),
            #[cfg(test)]
            poisoned_outer_mutator_releases: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            coordinator_notifications: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[derive(Default)]
struct ManagedData {
    arena: Arena,
    classes_by_metadata: HashMap<MetadataIdentity, AllocationClassId>,
    classes: Vec<AllocationClassEntry>,
    retired_no_drop_runs: Vec<RetiredNoDropRun>,
    finalization_batch: FinalizationBatch,
    running_finalizers: usize,
    #[cfg(test)]
    finalized_word_release_hook: Option<FinalizedWordReleaseHook>,
    free_runs: Vec<RunLocation>,
    allocation_pressure: AllocationPressure,
    roots: Vec<Weak<RootCell>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MutatorCoordinator {
    phase: AdmissionPhase,
    active_outer_mutators: usize,
    active_collection: Option<CollectionEpoch>,
    completed_collection_epoch: u64,
    latest_collection_report: Option<CollectionReport>,
    #[cfg(test)]
    blocked_outer_mutators: usize,
    #[cfg(test)]
    blocked_collection_waiters: usize,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CoordinatorSnapshot {
    phase: AdmissionPhase,
    active_outer_mutators: usize,
    collection_requested: bool,
    active_collection: Option<CollectionEpoch>,
    completed_collection_epoch: u64,
    latest_collection_report: Option<CollectionReport>,
    blocked_outer_mutators: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AdmissionPhase {
    #[default]
    Ordinary,
    Exclusive,
    Finalizing,
    Poisoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct CollectionEpoch(NonZeroU64);

impl CollectionEpoch {
    fn after(completed: u64) -> Self {
        let next = completed
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .expect("collection epoch exhausted");
        Self(next)
    }

    const fn get(self) -> u64 {
        self.0.get()
    }
}

/// One outer mutator's coordinator obligation.
///
/// The token borrows the heap already retained by `Heap::with_mutator`; it
/// adds no per-entry shared-owner clone. Dropping it is the only ordinary path
/// which retires the active-mutator count.
pub(crate) struct MutatorAdmission<'heap> {
    heap: &'heap HeapInner,
}

#[cfg(test)]
struct SyntheticExclusiveAdmission<'heap> {
    heap: &'heap HeapInner,
}

impl Drop for MutatorAdmission<'_> {
    fn drop(&mut self) {
        self.heap.release_outer_mutator();
    }
}

#[cfg(test)]
impl Drop for SyntheticExclusiveAdmission<'_> {
    fn drop(&mut self) {
        let mut coordinator = self
            .heap
            .coordinator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(coordinator.phase, AdmissionPhase::Exclusive);
        assert_eq!(coordinator.active_outer_mutators, 0);
        coordinator.phase = AdmissionPhase::Ordinary;
        self.heap.notify_coordinator_waiters();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AllocationPressure {
    assigned_runs: usize,
    high_water_mark: usize,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AllocationPressureSnapshot {
    assigned_runs: usize,
    high_water_mark: usize,
    collection_requested: bool,
}

#[cfg(test)]
impl Default for AllocationPressureSnapshot {
    fn default() -> Self {
        Self {
            assigned_runs: 0,
            high_water_mark: FIXED_SURVIVOR_RUN_HEADROOM,
            collection_requested: false,
        }
    }
}

impl Default for AllocationPressure {
    fn default() -> Self {
        Self {
            assigned_runs: 0,
            high_water_mark: FIXED_SURVIVOR_RUN_HEADROOM,
        }
    }
}

impl AllocationPressure {
    fn record_run_assignment(&mut self) -> bool {
        self.assigned_runs = self.assigned_runs.saturating_add(1);
        self.assigned_runs >= self.high_water_mark
    }

    fn record_run_release(&mut self) {
        self.assigned_runs = self
            .assigned_runs
            .checked_sub(1)
            .expect("released run was absent from assigned occupancy");
    }

    fn publish_survivor_baseline(&mut self, assigned_runs: usize) {
        assert_eq!(
            self.assigned_runs, assigned_runs,
            "pressure occupancy diverged from allocation-class topology"
        );
        self.high_water_mark = survivor_run_high_water_mark(
            assigned_runs,
            SURVIVOR_GROWTH_NUMERATOR,
            SURVIVOR_GROWTH_DENOMINATOR,
        );
    }
}

fn survivor_run_high_water_mark(
    survivors: usize,
    growth_numerator: usize,
    growth_denominator: usize,
) -> usize {
    assert_ne!(growth_denominator, 0, "survivor growth denominator is zero");
    let growth = survivors
        .checked_mul(growth_numerator)
        .and_then(|product| product.checked_add(growth_denominator - 1))
        .map_or(usize::MAX, |rounded| rounded / growth_denominator);
    survivors
        .checked_add(FIXED_SURVIVOR_RUN_HEADROOM)
        .and_then(|target| target.checked_add(growth))
        .unwrap_or(usize::MAX)
}

impl ManagedData {
    fn clear_mark_bitmaps(&mut self) -> usize {
        self.arena.clear_assigned_mark_bitmaps()
    }

    #[cfg(test)]
    fn collector_slot(&self, value: ErasedGc) -> Result<CollectorSlot, CollectorLookupError> {
        collector_slot_in(&self.arena, &self.classes, value)
    }

    #[cfg(test)]
    fn collector_slot_is_marked(&self, slot: CollectorSlot) -> bool {
        self.arena.owner_slot_is_marked(slot.owner)
    }

    fn prepare_swept_allocator_transition(
        &mut self,
        dead_set: &DeadSetPlan,
    ) -> FinalizationBatchPlan {
        assert!(
            self.retired_no_drop_runs.is_empty(),
            "a new collection cannot inherit unfinished no-drop retirement"
        );
        let finalization_batch = FinalizationBatchPlan::new(dead_set);
        self.finalization_batch
            .runs
            .try_reserve(finalization_batch.runs.len())
            .expect("durable finalization batch capacity exhausted");
        for (&location, planned) in &finalization_batch.runs {
            if let Some(existing) = self.finalization_batch.runs.get_mut(&location) {
                assert!(
                    !existing.target.is_detached(),
                    "detached finalization run reappeared in class topology"
                );
                existing
                    .pending_words
                    .try_reserve(planned.run.pending_words.len())
                    .expect("durable finalization word capacity exhausted");
            }
        }
        let retired_count = dead_set
            .dead_runs
            .iter()
            .filter(|run| run.disposition == DeadSlotDisposition::NoDrop && run.live_slots == 0)
            .count();
        let detached_finalizer_count = finalization_batch
            .runs
            .values()
            .filter(|planned| planned.detach_from_index.is_some())
            .count();
        self.retired_no_drop_runs
            .try_reserve(retired_count)
            .expect("retired no-drop run pool capacity exhausted");
        self.free_runs
            .try_reserve(
                retired_count
                    .checked_add(detached_finalizer_count)
                    .expect("free-run reservation count exhausted"),
            )
            .expect("free-run pool capacity exhausted");

        // Validate all stable retained topology and every attempt-local dead
        // record before the first selector is withdrawn. Exclusive authority
        // keeps these facts stable through the subsequent allocation-free
        // mutation/publication window.
        for (index, class) in self.classes.iter().enumerate() {
            let class_id = class_id(index);
            for target in class.runs().iter().map(|run| **run) {
                assert_eq!(target.geometry, class.geometry());
                self.arena.validate_run_target(target, class_id);
            }
        }
        let mut ordered_dead_runs = dead_set.dead_runs.iter().peekable();
        for class in &self.classes {
            for target in class.runs().iter().map(|run| **run) {
                if ordered_dead_runs
                    .peek()
                    .is_some_and(|planned| planned.target == target)
                {
                    ordered_dead_runs.next();
                }
            }
        }
        assert!(
            ordered_dead_runs.next().is_none(),
            "dead-run plan does not preserve authoritative class/run order"
        );
        for run in &dead_set.dead_runs {
            let index = class_index(run.class_id).expect("dead run has an invalid class ID");
            let class = self
                .classes
                .get(index)
                .expect("dead run allocation class is absent");
            assert!(
                std::ptr::eq(class.metadata(), run.metadata),
                "dead run changed allocation metadata"
            );
            assert_eq!(
                class.geometry(),
                run.target.geometry,
                "dead run changed allocation geometry"
            );
            assert!(
                class.contains_target(run.target),
                "dead run is absent from its allocation class"
            );
            assert_eq!(
                class.runs()[run.class_run_index].as_ref(),
                &run.target,
                "dead run changed its ordered class position"
            );
            if let Some(pending) = self.finalization_batch.runs.get(&run.target.location) {
                assert_eq!(pending.target.target(), run.target);
                assert!(!pending.target.is_detached());
                assert_ne!(run.live_slots, 0);
            }
            self.arena.validate_run_target(run.target, run.class_id);
            let dead_words = dead_set
                .dead_words
                .get(run.dead_words.clone())
                .expect("dead run has an invalid word range");
            assert!(dead_words.iter().all(|word| {
                word.dead_mask != 0
                    && word.word_index < run.target.geometry.allocation_bitmap.word_len
            }));
            assert_eq!(
                run.disposition,
                if run.dead_slots == 0 || !run.metadata.needs_drop() {
                    DeadSlotDisposition::NoDrop
                } else {
                    DeadSlotDisposition::DropRequired
                },
                "dead run has the wrong finalization disposition"
            );

            if run.disposition == DeadSlotDisposition::NoDrop && run.live_slots == 0 {
                assert!(
                    self.free_runs
                        .iter()
                        .all(|location| *location != run.target.location),
                    "retired no-drop run was already published for reuse"
                );
            }
        }

        finalization_batch
    }

    fn withdraw_allocator_frontiers(&mut self) {
        for class in &mut self.classes {
            class.withdraw_frontier();
        }
    }

    fn retire_wholly_dead_no_drop_runs(&mut self, dead_set: &DeadSetPlan) -> usize {
        let is_retired = |run: &&DeadRunPlan| {
            run.disposition == DeadSlotDisposition::NoDrop && run.live_slots == 0
        };
        let retired_count = dead_set.dead_runs.iter().filter(is_retired).count();
        for run in dead_set.dead_runs.iter().filter(is_retired) {
            debug_assert_eq!(
                run.live_slots, 0,
                "retired run must contain no live allocation"
            );
            let index = class_index(run.class_id).expect("retired run has an invalid class ID");
            debug_assert!(self.classes[index].frontier_is_withdrawn());
            let target = self.classes[index].retire_withdrawn_run(run.target);
            self.retired_no_drop_runs.push(RetiredNoDropRun {
                target,
                former_class_id: run.class_id,
            });
        }

        retired_count
    }

    fn recycle_retired_no_drop_runs(&mut self) -> usize {
        let recycled_count = self.retired_no_drop_runs.len();

        for retired in std::mem::take(&mut self.retired_no_drop_runs) {
            let target = *retired.target;
            self.arena
                .reset_recyclable_run(target, retired.former_class_id);
            self.allocation_pressure.record_run_release();
            self.free_runs.push(target.location);
        }

        recycled_count
    }

    fn install_finalization_batch(&mut self, mut batch: FinalizationBatchPlan) {
        // Planning reserved durable batch capacity and validated every target
        // before selector withdrawal. This loop only moves stable boxes and
        // appends or merges exact masks into existing capacity.
        for location in batch.install_order {
            let planned = batch
                .runs
                .remove(&location)
                .expect("finalization install order lost its planned run");
            let mut run = planned.run;
            let added_slots = run.pending_slot_count;
            let target = run.target.target();
            assert_eq!(target.location, location);
            if let Some(existing) = self.finalization_batch.runs.get_mut(&location) {
                debug_assert!(planned.detach_from_index.is_none());
                debug_assert!(!existing.target.is_detached());
                debug_assert_eq!(existing.class_id, run.class_id);
                debug_assert!(std::ptr::eq(existing.metadata, run.metadata));
                existing.merge_pending_words(&run.pending_words, run.pending_slot_count);
                self.finalization_batch.pending_slot_count = self
                    .finalization_batch
                    .pending_slot_count
                    .checked_add(added_slots)
                    .expect("finalization batch slot count exhausted");
                continue;
            }

            let index = class_index(run.class_id).expect("finalization run has invalid class ID");
            let class = self
                .classes
                .get_mut(index)
                .expect("finalization run allocation class is absent");
            assert!(class.frontier_is_withdrawn());

            if let Some(former_index) = planned.detach_from_index {
                let (current_index, target) = class.retire_withdrawn_run_at(target);
                assert!(
                    current_index <= former_index,
                    "finalization detachment moved past its retained class position"
                );
                run.target = FinalizationRunTarget::Detached { target };
            } else {
                assert!(class.contains_target(target));
            }

            assert!(
                self.finalization_batch.runs.insert(location, run).is_none(),
                "finalization install duplicated a durable run"
            );
            self.finalization_batch.pending_slot_count = self
                .finalization_batch
                .pending_slot_count
                .checked_add(added_slots)
                .expect("finalization batch slot count exhausted");
        }
        assert!(batch.runs.is_empty());
    }

    fn finalization_dispatch_snapshot(&self) -> Vec<RunLocation> {
        self.finalization_batch.dispatch_snapshot()
    }

    fn prepare_finalization_run(&mut self, location: RunLocation) -> RunFinalizationAttempt {
        assert_eq!(
            self.running_finalizers, 0,
            "cannot claim a second finalization run while one is active"
        );
        let attempt = self
            .finalization_batch
            .prepare_run_attempt(&self.arena, location);
        self.running_finalizers = attempt.work.len();
        attempt
    }

    fn complete_finalization_run(&mut self, attempt: RunFinalizationAttempt) -> bool {
        assert_eq!(
            self.running_finalizers,
            attempt.work.len(),
            "active finalization count diverged from its run attempt"
        );
        let completion = self
            .finalization_batch
            .complete_run_attempt(&self.arena, attempt);
        self.running_finalizers = 0;

        if !completion.detached {
            let index = class_index(completion.class_id)
                .expect("released finalization word has invalid class ID");
            for word in &completion.words {
                if word.remaining_mask != 0 {
                    continue;
                }
                self.arena.release_finalized_allocation_word(
                    completion.target,
                    completion.class_id,
                    word.word_index,
                );
                #[cfg(test)]
                self.pause_after_finalized_word_release(
                    completion.target.location,
                    word.word_index,
                );
                self.classes[index].publish_released_run(completion.target);
            }
        }

        if let Some(completed) = completion.completed_detached_run {
            let target = *completed.target;
            self.arena.reset_recyclable_run(target, completed.class_id);
            self.allocation_pressure.record_run_release();
            self.free_runs.push(target.location);
            true
        } else {
            false
        }
    }

    fn activity(&self) -> HeapActivity {
        let queued_finalizers = self
            .finalization_batch
            .pending_slot_count()
            .checked_sub(self.running_finalizers)
            .expect("running finalizers exceed durable pending obligations");
        HeapActivity {
            queued_finalizers,
            running_finalizers: self.running_finalizers,
        }
    }

    #[cfg(test)]
    fn pause_after_finalized_word_release(&mut self, location: RunLocation, word_index: usize) {
        let matches = self
            .finalized_word_release_hook
            .as_ref()
            .is_some_and(|hook| hook.location == location && hook.word_index == word_index);
        if !matches {
            return;
        }
        let hook = self
            .finalized_word_release_hook
            .take()
            .expect("matching finalized-word hook disappeared");
        hook.arrived.wait();
        hook.release.wait();
    }

    fn sweep_partial_no_drop_runs(&mut self, dead_set: &DeadSetPlan) {
        let is_swept = |run: &&DeadRunPlan| {
            run.disposition == DeadSlotDisposition::NoDrop
                && run.live_slots != 0
                && run.dead_slots != 0
        };
        for run in dead_set.dead_runs.iter().filter(is_swept) {
            debug_assert!(!run.metadata.needs_drop());
            self.arena
                .retain_marked_allocations(run.target, run.class_id);
        }
    }

    fn publish_swept_allocator_view(&mut self) {
        let arena = &self.arena;
        let finalization_batch = &self.finalization_batch;
        for (index, class) in self.classes.iter_mut().enumerate() {
            debug_assert!(class.frontier_is_withdrawn());
            let class_id = class_id(index);
            let mut first_available = None;

            for (run_index, target) in class.runs().iter().enumerate() {
                let has_available =
                    arena.publish_allocation_word_leases(**target, class_id, |word_index| {
                        finalization_batch.reserves_word(**target, word_index)
                    });
                if has_available && first_available.is_none() {
                    first_available = Some(run_index);
                }
            }

            class.publish_swept_frontier(first_available);
        }
    }

    fn publish_survivor_pressure_baseline(&mut self) {
        // Class membership plus collector-owned detached finalization records
        // is the authoritative assigned-run topology. The separately
        // maintained counter makes activation/release pressure cheap;
        // recomputing it at the completed sweep boundary catches accounting
        // drift before publishing a new target. Detachment is not release.
        let assigned_runs = self
            .classes
            .iter()
            .fold(0_usize, |total, class| {
                total
                    .checked_add(class.runs().len())
                    .expect("assigned-run occupancy exhausted")
            })
            .checked_add(self.finalization_batch.detached_run_count())
            .expect("assigned finalization-run occupancy exhausted");
        assert!(
            assigned_runs <= self.arena.run_capacity(),
            "assigned-run occupancy exceeds committed arena capacity"
        );
        self.allocation_pressure
            .publish_survivor_baseline(assigned_runs);
    }

    fn publish_run(
        &mut self,
        class_index: usize,
        class_id: AllocationClassId,
        geometry: RunGeometry,
        collection_requested: &AtomicBool,
    ) -> Result<RunLocation, RunPublicationError> {
        // Reserve the class-pool entry before publishing arena state. After a
        // successful arena publication, all remaining operations are
        // infallible and the pressure event is recorded exactly once.
        self.classes[class_index].reserve_run();
        let location = if let Some(location) = self.free_runs.pop() {
            match self
                .arena
                .initialize_run(location.chunk, location.run, class_id, geometry)
            {
                Ok(()) => location,
                Err(error) => {
                    // Initialization validates before mutating the empty run,
                    // so a rejected retype remains reusable.
                    self.free_runs.push(location);
                    return Err(RunPublicationError::Initialization(error));
                }
            }
        } else {
            self.arena.publish_run(class_id, geometry)?
        };
        let target = self
            .arena
            .run_claim_target(location)
            .expect("published typed run must expose stable claim topology");
        self.classes[class_index].publish_run(target);
        if self.allocation_pressure.record_run_assignment() {
            collection_requested.store(true, Ordering::Release);
        }
        Ok(location)
    }
}

impl MutatorCoordinator {
    fn request_synchronous_collection(&self) -> (CollectionEpoch, bool) {
        match self.phase {
            AdmissionPhase::Ordinary => (
                CollectionEpoch::after(self.completed_collection_epoch),
                true,
            ),
            AdmissionPhase::Exclusive | AdmissionPhase::Finalizing => (
                self.active_collection
                    .expect("active collection phase must have an epoch"),
                false,
            ),
            AdmissionPhase::Poisoned => {
                unreachable!("poisoned heaps reject collection before requesting an epoch")
            }
        }
    }

    fn elect_idle_collection(&mut self, collection_requested: bool) -> Option<CollectionEpoch> {
        if self.phase != AdmissionPhase::Ordinary
            || self.active_outer_mutators != 0
            || !collection_requested
        {
            return None;
        }
        let epoch = CollectionEpoch::after(self.completed_collection_epoch);
        self.active_collection = Some(epoch);
        self.phase = AdmissionPhase::Exclusive;
        Some(epoch)
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrepareRunError {
    ForeignClass,
    InvalidClass,
    Publication(RunPublicationError),
}

#[derive(Clone, Copy)]
struct ResolvedSlot {
    metadata: &'static ObjectMetadata,
    #[cfg(test)]
    class_id: AllocationClassId,
    #[cfg(test)]
    geometry: RunGeometry,
    #[cfg(test)]
    slot_index: usize,
    allocated: bool,
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct ResolvedRun {
    location: RunLocation,
    metadata: &'static ObjectMetadata,
    class_id: AllocationClassId,
    geometry: RunGeometry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CollectorLookupError {
    InvalidAddress,
    InvalidClass,
    InvalidRunTopology,
    Unallocated,
}

impl CollectorLookupError {
    fn raise(self) -> ! {
        let message = match self {
            Self::InvalidAddress => "collector edge does not identify an exact managed slot",
            Self::InvalidClass => "collector edge refers to an absent allocation class",
            Self::InvalidRunTopology => {
                "collector edge refers to a run outside its allocation class"
            }
            Self::Unallocated => "collector edge does not identify an allocated value",
        };
        panic!("{message}")
    }
}

#[derive(Clone, Copy)]
struct CollectorSlot {
    owner: RunOwner,
    metadata: &'static ObjectMetadata,
}

#[derive(Clone, Copy)]
struct TraceWork {
    value: ErasedGc,
    metadata: &'static ObjectMetadata,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MarkSummary {
    root_entries: usize,
    traced_objects: usize,
    marked_slots: usize,
    conservatively_retained_slots: usize,
    #[cfg(test)]
    peak_object_worklist_len: usize,
    #[cfg(test)]
    peak_object_worklist_capacity: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SweepSummary {
    reclaimed_slots: usize,
    reclaimed_runs: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct FinalizationSummary {
    finalized_slots: usize,
    reclaimed_runs: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CollectionSummary {
    mark: MarkSummary,
    reclaimed_slots: usize,
    finalized_slots: usize,
    reclaimed_runs: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeadSlotDisposition {
    NoDrop,
    DropRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeadBitmapWord {
    word_index: usize,
    dead_mask: u64,
}

struct DeadRunPlan {
    target: RunClaimTarget,
    class_run_index: usize,
    class_id: AllocationClassId,
    metadata: &'static ObjectMetadata,
    disposition: DeadSlotDisposition,
    live_slots: usize,
    dead_slots: usize,
    dead_words: Range<usize>,
}

struct RetiredNoDropRun {
    target: Box<RunClaimTarget>,
    former_class_id: AllocationClassId,
}

/// Durable ownership of initialized allocations awaiting Rust destruction.
///
/// Partial runs remain attached to their allocation class while the batch
/// reserves the exact words containing pending slots. Wholly dead runs move
/// their stable boxed target here and leave ordinary class topology. The
/// finalizer consumes these records outside collector locks; retaining them
/// across a panic or terminal teardown keeps root rejection and destruction
/// nonduplicating.
#[derive(Default)]
struct FinalizationBatch {
    runs: HashMap<RunLocation, FinalizationRun>,
    pending_slot_count: usize,
}

struct FinalizationBatchPlan {
    runs: HashMap<RunLocation, PlannedFinalizationRun>,
    install_order: Vec<RunLocation>,
}

struct PlannedFinalizationRun {
    run: FinalizationRun,
    detach_from_index: Option<usize>,
}

#[derive(Clone, Copy)]
struct FinalizationWork {
    word_attempt_index: usize,
    word_index: usize,
    bit: u64,
    owner: RunOwner,
    value: ErasedGc,
    metadata: &'static ObjectMetadata,
}

struct FinalizationCompletion {
    target: RunClaimTarget,
    class_id: AllocationClassId,
    detached: bool,
    words: Vec<FinalizationWordAttempt>,
    completed_detached_run: Option<CompletedDetachedRun>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FinalizationWordAttempt {
    word_index: usize,
    original_mask: u64,
    remaining_mask: u64,
}

struct RunFinalizationAttempt {
    location: RunLocation,
    target: RunClaimTarget,
    class_id: AllocationClassId,
    metadata: &'static ObjectMetadata,
    words: Vec<FinalizationWordAttempt>,
    work: Vec<FinalizationWork>,
    next_work_index: usize,
}

struct CompletedDetachedRun {
    target: Box<RunClaimTarget>,
    class_id: AllocationClassId,
}

struct FinalizationRun {
    target: FinalizationRunTarget,
    class_id: AllocationClassId,
    metadata: &'static ObjectMetadata,
    pending_slot_count: usize,
    pending_words: HashMap<usize, u64>,
}

enum FinalizationRunTarget {
    /// A partial run still selected through its ordinary allocation class.
    Attached(RunClaimTarget),
    /// A wholly dead run owned only by the finalization batch.
    Detached { target: Box<RunClaimTarget> },
}

impl FinalizationRunTarget {
    fn target(&self) -> RunClaimTarget {
        match self {
            Self::Attached(target) => *target,
            Self::Detached { target, .. } => **target,
        }
    }

    fn is_detached(&self) -> bool {
        matches!(self, Self::Detached { .. })
    }
}

impl FinalizationRun {
    fn pending_slot_count(&self) -> usize {
        self.pending_slot_count
    }

    fn contains_owner(&self, owner: RunOwner) -> bool {
        let target = self.target.target();
        if owner.location != target.location
            || owner.run != target.run
            || owner.class_id != self.class_id
            || owner.geometry != target.geometry
        {
            return false;
        }
        let word_index = owner.slot_index / u64::BITS as usize;
        let bit = 1_u64 << (owner.slot_index % u64::BITS as usize);
        self.pending_words
            .get(&word_index)
            .is_some_and(|pending| pending & bit != 0)
    }

    fn visit_pending_slots(&self, mut visit: impl FnMut(RunOwner)) {
        let target = self.target.target();
        for (&word_index, &pending_mask) in &self.pending_words {
            let word_start = word_index
                .checked_mul(u64::BITS as usize)
                .expect("finalization word offset exhausted");
            let mut pending = pending_mask;
            while pending != 0 {
                let bit_index = pending.trailing_zeros() as usize;
                let slot_index = word_start
                    .checked_add(bit_index)
                    .expect("finalization slot index exhausted");
                assert!(slot_index < target.geometry.slot_count);
                visit(RunOwner {
                    location: target.location,
                    run: target.run,
                    class_id: self.class_id,
                    geometry: target.geometry,
                    slot_index,
                });
                pending &= pending - 1;
            }
        }
    }

    fn merge_pending_words(&mut self, additional: &HashMap<usize, u64>, additional_slots: usize) {
        for (&word_index, &additional_mask) in additional {
            assert_ne!(additional_mask, 0);
            let pending = self.pending_words.entry(word_index).or_default();
            assert_eq!(
                *pending & additional_mask,
                0,
                "finalization batch duplicated an existing slot obligation"
            );
            *pending |= additional_mask;
        }
        self.pending_slot_count = self
            .pending_slot_count
            .checked_add(additional_slots)
            .expect("finalization slot count exhausted");
    }
}

impl FinalizationBatchPlan {
    fn new(dead_set: &DeadSetPlan) -> Self {
        let run_count = dead_set
            .dead_runs
            .iter()
            .filter(|run| run.disposition == DeadSlotDisposition::DropRequired)
            .count();
        let mut runs = HashMap::new();
        runs.try_reserve(run_count)
            .expect("finalization run batch capacity exhausted");
        let mut install_order = Vec::new();
        install_order
            .try_reserve_exact(run_count)
            .expect("finalization install-order capacity exhausted");

        for run in dead_set
            .dead_runs
            .iter()
            .filter(|run| run.disposition == DeadSlotDisposition::DropRequired)
        {
            assert!(run.metadata.needs_drop());
            assert_ne!(run.dead_slots, 0);
            let source_words = dead_set
                .dead_words
                .get(run.dead_words.clone())
                .expect("finalization run has an invalid word range");
            let mut pending_words = HashMap::new();
            pending_words
                .try_reserve(source_words.len())
                .expect("finalization word batch capacity exhausted");
            for word in source_words {
                assert_ne!(word.dead_mask, 0);
                assert!(
                    pending_words
                        .insert(word.word_index, word.dead_mask)
                        .is_none(),
                    "finalization plan duplicated a pending word"
                );
            }
            let detach_from_index = (run.live_slots == 0).then(|| {
                let retired_before = dead_set
                    .dead_runs
                    .iter()
                    .filter(|prior| {
                        prior.class_id == run.class_id
                            && prior.class_run_index < run.class_run_index
                            && prior.disposition == DeadSlotDisposition::NoDrop
                            && prior.live_slots == 0
                    })
                    .count();
                run.class_run_index
                    .checked_sub(retired_before)
                    .expect("finalization run position underflowed retired topology")
            });
            let location = run.target.location;
            let planned = PlannedFinalizationRun {
                detach_from_index,
                run: FinalizationRun {
                    target: FinalizationRunTarget::Attached(run.target),
                    class_id: run.class_id,
                    metadata: run.metadata,
                    pending_slot_count: run.dead_slots,
                    pending_words,
                },
            };
            assert_eq!(planned.run.pending_slot_count(), run.dead_slots);
            assert!(
                runs.insert(location, planned).is_none(),
                "finalization plan duplicated a run"
            );
            install_order.push(location);
        }

        Self {
            runs,
            install_order,
        }
    }
}

impl FinalizationBatch {
    fn pending_slot_count(&self) -> usize {
        self.pending_slot_count
    }

    fn detached_run_count(&self) -> usize {
        self.runs
            .values()
            .filter(|run| run.target.is_detached())
            .count()
    }

    fn pending_metadata_at(
        &self,
        arena: &Arena,
        address: usize,
    ) -> Option<&'static ObjectMetadata> {
        let owner = arena.checked_slot_owner(address)?;
        let run = self.runs.get(&owner.location)?;
        run.contains_owner(owner).then_some(run.metadata)
    }

    fn dispatch_snapshot(&self) -> Vec<RunLocation> {
        let mut locations = Vec::new();
        locations
            .try_reserve_exact(self.runs.len())
            .expect("finalization dispatch snapshot capacity exhausted");
        locations.extend(self.runs.keys().copied());
        locations
    }

    fn prepare_run_attempt(&self, arena: &Arena, location: RunLocation) -> RunFinalizationAttempt {
        let run = self
            .runs
            .get(&location)
            .expect("finalization dispatch snapshot lost a run");
        let target = run.target.target();
        assert_eq!(target.location, location);
        let mut words = Vec::new();
        words
            .try_reserve_exact(run.pending_words.len())
            .expect("run finalization word snapshot capacity exhausted");
        let mut work = Vec::new();
        work.try_reserve_exact(run.pending_slot_count)
            .expect("run finalization work capacity exhausted");

        for (&word_index, &pending_mask) in &run.pending_words {
            assert_ne!(pending_mask, 0);
            let word_attempt_index = words.len();
            words.push(FinalizationWordAttempt {
                word_index,
                original_mask: pending_mask,
                remaining_mask: pending_mask,
            });
            let mut pending = pending_mask;
            while pending != 0 {
                let bit_index = pending.trailing_zeros() as usize;
                let bit = 1_u64 << bit_index;
                let slot_index = word_index
                    .checked_mul(u64::BITS as usize)
                    .and_then(|start| start.checked_add(bit_index))
                    .expect("finalization slot index exhausted");
                assert!(slot_index < target.geometry.slot_count);
                let owner = RunOwner {
                    location: target.location,
                    run: target.run,
                    class_id: run.class_id,
                    geometry: target.geometry,
                    slot_index,
                };
                assert!(arena.owner_slot_is_allocated(owner));
                let pointer = arena.owner_slot_pointer(owner);
                work.push(FinalizationWork {
                    word_attempt_index,
                    word_index,
                    bit,
                    owner,
                    value: ErasedGc::new(pointer),
                    metadata: run.metadata,
                });
                pending &= pending - 1;
            }
        }

        assert_eq!(work.len(), run.pending_slot_count);
        RunFinalizationAttempt {
            location,
            target,
            class_id: run.class_id,
            metadata: run.metadata,
            words,
            work,
            next_work_index: 0,
        }
    }

    fn complete_run_attempt(
        &mut self,
        arena: &Arena,
        attempt: RunFinalizationAttempt,
    ) -> FinalizationCompletion {
        let location = attempt.location;
        let target = attempt.target;
        let class_id = attempt.class_id;
        let metadata = attempt.metadata;
        let remaining_slots = attempt.words.iter().fold(0_usize, |total, word| {
            total
                .checked_add(word.remaining_mask.count_ones() as usize)
                .expect("remaining finalization slot count exhausted")
        });

        {
            let run = self
                .runs
                .get(&location)
                .expect("completed finalization lost its run record");
            assert_eq!(run.target.target(), target);
            assert_eq!(run.class_id, class_id);
            assert!(std::ptr::eq(run.metadata, metadata));
            assert_eq!(run.pending_words.len(), attempt.words.len());
            assert_eq!(run.pending_slot_count, attempt.work.len());
            for word in &attempt.words {
                assert_eq!(
                    run.pending_words.get(&word.word_index),
                    Some(&word.original_mask),
                    "finalization run changed while its local attempt executed"
                );
            }
        }

        // Keep every exact pending identity authoritative until all completed
        // allocation bits have been cleared. Root validation shares this heap
        // mutex, so it observes either pending finalization or an unallocated
        // slot.
        for word in &attempt.words {
            let mut completed = word.original_mask & !word.remaining_mask;
            while completed != 0 {
                let bit_index = completed.trailing_zeros() as usize;
                let slot_index = word
                    .word_index
                    .checked_mul(u64::BITS as usize)
                    .and_then(|start| start.checked_add(bit_index))
                    .expect("completed finalization slot index exhausted");
                let owner = RunOwner {
                    location,
                    run: target.run,
                    class_id,
                    geometry: target.geometry,
                    slot_index,
                };
                assert!(
                    arena.clear_owner_allocation(owner),
                    "terminally destroyed allocation was already retired"
                );
                completed &= completed - 1;
            }
        }

        let detached = self
            .runs
            .get(&location)
            .expect("completed finalization lost its run record")
            .target
            .is_detached();
        let completed_slots = attempt
            .work
            .len()
            .checked_sub(remaining_slots)
            .expect("finalization attempt gained pending slots");
        {
            let run = self
                .runs
                .get_mut(&location)
                .expect("completed finalization lost its run record");
            for word in &attempt.words {
                if word.remaining_mask == 0 {
                    assert_eq!(
                        run.pending_words.remove(&word.word_index),
                        Some(word.original_mask)
                    );
                } else {
                    assert_eq!(
                        run.pending_words
                            .insert(word.word_index, word.remaining_mask),
                        Some(word.original_mask)
                    );
                }
            }
            run.pending_slot_count = remaining_slots;
            assert_eq!(run.pending_words.is_empty(), remaining_slots == 0);
        }
        self.pending_slot_count = self
            .pending_slot_count
            .checked_sub(completed_slots)
            .expect("completed finalization exceeded pending batch count");

        let completed_detached_run = if remaining_slots == 0 {
            let run = self
                .runs
                .remove(&location)
                .expect("completed finalization lost its empty run record");
            match run.target {
                FinalizationRunTarget::Attached(attached) => {
                    debug_assert_eq!(attached, target);
                    None
                }
                FinalizationRunTarget::Detached { target, .. } => {
                    Some(CompletedDetachedRun { target, class_id })
                }
            }
        } else {
            None
        };

        FinalizationCompletion {
            target,
            class_id,
            detached,
            words: attempt.words,
            completed_detached_run,
        }
    }

    fn reserves_word(&self, target: RunClaimTarget, word_index: usize) -> bool {
        self.runs.get(&target.location).is_some_and(|run| {
            run.target.target() == target
                && run
                    .pending_words
                    .get(&word_index)
                    .is_some_and(|pending| *pending != 0)
        })
    }

    fn visit_pending_slots(&self, mut visit: impl FnMut(RunOwner)) {
        for run in self.runs.values() {
            run.visit_pending_slots(&mut visit);
        }
    }
}

impl RunFinalizationAttempt {
    fn next_work(&self) -> Option<FinalizationWork> {
        self.work.get(self.next_work_index).copied()
    }

    fn complete_terminal(&mut self, work: FinalizationWork) {
        assert_eq!(
            self.work.get(self.next_work_index).map(|next| next.value),
            Some(work.value),
            "finalization attempt completed work out of order"
        );
        let word = self
            .words
            .get_mut(work.word_attempt_index)
            .expect("finalization work lost its local word snapshot");
        assert_eq!(word.word_index, work.word_index);
        assert_eq!(work.owner.location, self.location);
        assert_eq!(work.owner.run, self.target.run);
        assert_eq!(work.owner.class_id, self.class_id);
        assert_eq!(work.owner.geometry, self.target.geometry);
        assert_eq!(work.owner.slot_index / u64::BITS as usize, work.word_index);
        assert_ne!(word.remaining_mask & work.bit, 0);
        word.remaining_mask &= !work.bit;
        self.next_work_index += 1;
    }
}

#[derive(Default)]
struct DeadSetPlan {
    #[cfg(any(test, debug_assertions))]
    allocated_slots: usize,
    #[cfg(any(test, debug_assertions))]
    live_slots: usize,
    no_drop_dead_slots: usize,
    #[cfg(any(test, debug_assertions))]
    drop_required_dead_slots: usize,
    #[cfg(test)]
    live_runs: usize,
    #[cfg(test)]
    empty_runs: usize,
    #[cfg(test)]
    no_drop_dead_runs: usize,
    #[cfg(test)]
    drop_required_dead_runs: usize,
    dead_runs: Vec<DeadRunPlan>,
    dead_words: Vec<DeadBitmapWord>,
}

struct PostMarkPlan {
    #[cfg(test)]
    summary: MarkSummary,
    dead_set: DeadSetPlan,
}

impl DeadSetPlan {
    fn classify(data: &ManagedData) -> Self {
        let mut plan = Self::default();

        for (class_index, class) in data.classes.iter().enumerate() {
            let class_id = class_id(class_index);
            let metadata = class.metadata();
            let class_disposition = if metadata.needs_drop() {
                DeadSlotDisposition::DropRequired
            } else {
                DeadSlotDisposition::NoDrop
            };

            for (class_run_index, target) in class.runs().iter().map(|run| **run).enumerate() {
                assert_eq!(
                    target.geometry,
                    class.geometry(),
                    "allocation-class run changed geometry"
                );
                let mut live_slots = 0_usize;
                let mut dead_slots = 0_usize;
                let dead_words_start = plan.dead_words.len();
                data.arena.visit_allocation_mark_words(
                    target,
                    class_id,
                    |word_index, allocated, marked| {
                        debug_assert_eq!(marked & allocated, marked);
                        let live = allocated & marked;
                        let dead = allocated & !marked;
                        live_slots = live_slots
                            .checked_add(live.count_ones() as usize)
                            .expect("live-slot count exhausted");
                        dead_slots = dead_slots
                            .checked_add(dead.count_ones() as usize)
                            .expect("dead-slot count exhausted");
                        if dead != 0 {
                            plan.dead_words
                                .try_reserve(1)
                                .expect("collector dead-word plan capacity exhausted");
                            plan.dead_words.push(DeadBitmapWord {
                                word_index,
                                dead_mask: dead,
                            });
                        }
                    },
                );

                #[cfg(any(test, debug_assertions))]
                {
                    plan.allocated_slots = plan
                        .allocated_slots
                        .checked_add(live_slots)
                        .and_then(|slots| slots.checked_add(dead_slots))
                        .expect("allocated-slot count exhausted");
                    plan.live_slots = plan
                        .live_slots
                        .checked_add(live_slots)
                        .expect("live-slot count exhausted");
                }
                #[cfg(test)]
                if live_slots != 0 {
                    plan.live_runs = plan
                        .live_runs
                        .checked_add(1)
                        .expect("live-run count exhausted");
                }
                if live_slots == 0 && dead_slots == 0 {
                    #[cfg(test)]
                    {
                        plan.empty_runs = plan
                            .empty_runs
                            .checked_add(1)
                            .expect("empty-run count exhausted");
                    }
                    plan.dead_runs
                        .try_reserve(1)
                        .expect("collector empty-run plan capacity exhausted");
                    plan.dead_runs.push(DeadRunPlan {
                        target,
                        class_run_index,
                        class_id,
                        metadata,
                        disposition: DeadSlotDisposition::NoDrop,
                        live_slots: 0,
                        dead_slots: 0,
                        dead_words: dead_words_start..plan.dead_words.len(),
                    });
                    continue;
                }
                if dead_slots == 0 {
                    continue;
                }

                match class_disposition {
                    DeadSlotDisposition::NoDrop => {
                        plan.no_drop_dead_slots = plan
                            .no_drop_dead_slots
                            .checked_add(dead_slots)
                            .expect("no-drop dead-slot count exhausted");
                        #[cfg(test)]
                        {
                            plan.no_drop_dead_runs = plan
                                .no_drop_dead_runs
                                .checked_add(1)
                                .expect("no-drop dead-run count exhausted");
                        }
                    }
                    DeadSlotDisposition::DropRequired => {
                        #[cfg(any(test, debug_assertions))]
                        {
                            plan.drop_required_dead_slots = plan
                                .drop_required_dead_slots
                                .checked_add(dead_slots)
                                .expect("drop-required dead-slot count exhausted");
                        }
                        #[cfg(test)]
                        {
                            plan.drop_required_dead_runs = plan
                                .drop_required_dead_runs
                                .checked_add(1)
                                .expect("drop-required dead-run count exhausted");
                        }
                    }
                }
                plan.dead_runs
                    .try_reserve(1)
                    .expect("collector dead-run plan capacity exhausted");
                plan.dead_runs.push(DeadRunPlan {
                    target,
                    class_run_index,
                    class_id,
                    metadata,
                    disposition: class_disposition,
                    live_slots,
                    dead_slots,
                    dead_words: dead_words_start..plan.dead_words.len(),
                });
            }
        }

        #[cfg(debug_assertions)]
        debug_assert_eq!(
            plan.allocated_slots,
            plan.live_slots
                .checked_add(plan.no_drop_dead_slots)
                .and_then(|slots| slots.checked_add(plan.drop_required_dead_slots))
                .expect("classified-slot count exhausted")
        );
        plan
    }
}

#[derive(Default)]
struct MarkAttempt {
    worklist: Vec<TraceWork>,
    root_count: usize,
    marked_slot_count: usize,
    conservatively_retained_slot_count: usize,
    traced_object_count: usize,
    #[cfg(test)]
    panic_before_worklist_push: Option<usize>,
    #[cfg(test)]
    completed_worklist_pushes: usize,
    #[cfg(test)]
    peak_object_worklist_len: usize,
    #[cfg(test)]
    peak_object_worklist_capacity: usize,
}

impl MarkAttempt {
    fn retain_pending_finalizers(&mut self, data: &mut ManagedData) {
        let ManagedData {
            arena,
            finalization_batch,
            ..
        } = data;
        finalization_batch.visit_pending_slots(|owner| {
            assert!(
                arena.owner_slot_is_allocated(owner),
                "pending finalizer lost its allocation bit"
            );
            assert!(
                arena.mark_owner_slot(owner),
                "pending finalizer was duplicated in its durable batch"
            );
            self.marked_slot_count = self
                .marked_slot_count
                .checked_add(1)
                .expect("marked-slot count exhausted");
            self.conservatively_retained_slot_count = self
                .conservatively_retained_slot_count
                .checked_add(1)
                .expect("conservative-retention count exhausted");
        });
    }

    fn reserve_root_capacity(&mut self, root_registry_len: usize) {
        self.worklist
            .try_reserve(root_registry_len)
            .expect("collector root worklist capacity exhausted");
        #[cfg(test)]
        {
            self.peak_object_worklist_capacity = self.worklist.capacity();
        }
    }

    fn discover(
        &mut self,
        data: &mut ManagedData,
        value: ErasedGc,
    ) -> Result<bool, CollectorLookupError> {
        self.discover_in(&mut data.arena, &data.classes, value)
    }

    fn discover_in(
        &mut self,
        arena: &mut Arena,
        classes: &[AllocationClassEntry],
        value: ErasedGc,
    ) -> Result<bool, CollectorLookupError> {
        let slot = collector_slot_in(arena, classes, value)?;
        if !mark_collector_slot_in(arena, classes, slot) {
            return Ok(false);
        }
        self.marked_slot_count = self
            .marked_slot_count
            .checked_add(1)
            .expect("marked-slot count exhausted");
        self.push_work(TraceWork {
            value,
            metadata: slot.metadata,
        });
        Ok(true)
    }

    fn push_work(&mut self, work: TraceWork) {
        if self.worklist.len() == self.worklist.capacity() {
            self.worklist
                .try_reserve(1)
                .expect("collector object worklist capacity exhausted");
        }
        #[cfg(test)]
        if self.panic_before_worklist_push == Some(self.completed_worklist_pushes) {
            panic!(
                "injected worklist panic after {} completed pushes",
                self.completed_worklist_pushes
            );
        }
        self.worklist.push(work);
        #[cfg(test)]
        {
            self.peak_object_worklist_len = self.peak_object_worklist_len.max(self.worklist.len());
            self.peak_object_worklist_capacity = self
                .peak_object_worklist_capacity
                .max(self.worklist.capacity());
            self.completed_worklist_pushes = self
                .completed_worklist_pushes
                .checked_add(1)
                .expect("test worklist-push count exhausted");
        }
    }

    #[cfg(test)]
    fn inject_worklist_panic_after(&mut self, completed_pushes: usize) {
        self.panic_before_worklist_push = Some(completed_pushes);
    }

    fn seed_registered_roots(
        &mut self,
        data: &mut ManagedData,
    ) -> Result<(), CollectorLookupError> {
        let ManagedData {
            arena,
            classes,
            roots,
            ..
        } = data;
        let mut lookup_error = None;
        retain_registered_roots(roots, |value| {
            self.root_count = self
                .root_count
                .checked_add(1)
                .expect("root count exhausted");
            if lookup_error.is_none()
                && let Err(error) = self.discover_in(arena, classes, value)
            {
                lookup_error = Some(error);
            }
        });
        match lookup_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn trace_worklist(&mut self, data: &mut ManagedData) -> Result<(), CollectorLookupError> {
        while let Some(work) = self.worklist.pop() {
            let mut visit = |edge| {
                self.discover(data, edge)
                    .unwrap_or_else(|error| error.raise());
            };
            let mut visitor = Visitor::new(&mut visit);
            // SAFETY: `TraceWork` is constructed only after checked discovery
            // proves that `work.value` identifies one live initialized
            // allocation with this canonical metadata. The attempt retains
            // exclusive collection through this synchronous trace, and drain
            // holds managed data while dispatching, so no topology change can
            // invalidate that proof.
            unsafe { work.metadata.trace(work.value.as_ptr(), &mut visitor) };
            self.traced_object_count = self
                .traced_object_count
                .checked_add(1)
                .expect("traced-object count exhausted");
        }
        Ok(())
    }

    fn finish(self) -> MarkSummary {
        assert!(
            self.worklist.is_empty(),
            "successful mark attempt must drain its complete worklist"
        );
        MarkSummary {
            root_entries: self.root_count,
            traced_objects: self.traced_object_count,
            marked_slots: self.marked_slot_count,
            conservatively_retained_slots: self.conservatively_retained_slot_count,
            #[cfg(test)]
            peak_object_worklist_len: self.peak_object_worklist_len,
            #[cfg(test)]
            peak_object_worklist_capacity: self.peak_object_worklist_capacity,
        }
    }
}

impl HeapInner {
    fn admit_outer_mutator(self: &Arc<Self>) -> (MutatorAdmission<'_>, bool) {
        let elected = {
            let mut coordinator = self
                .coordinator
                .lock()
                .expect("mutator coordinator should not be poisoned");
            loop {
                if self.is_poisoned() {
                    drop(coordinator);
                    panic!("managed heap is permanently poisoned");
                }
                match coordinator.phase {
                    AdmissionPhase::Ordinary => {
                        let requested = self.collection_requested.load(Ordering::Acquire);
                        if let Some(epoch) = coordinator.elect_idle_collection(requested) {
                            self.notify_coordinator_waiters();
                            break Some(epoch);
                        }
                        coordinator.active_outer_mutators = coordinator
                            .active_outer_mutators
                            .checked_add(1)
                            .expect("active mutator count exhausted");
                        break None;
                    }
                    AdmissionPhase::Finalizing => {
                        coordinator.active_outer_mutators = coordinator
                            .active_outer_mutators
                            .checked_add(1)
                            .expect("active mutator count exhausted");
                        break None;
                    }
                    AdmissionPhase::Exclusive => {
                        #[cfg(test)]
                        {
                            coordinator.blocked_outer_mutators += 1;
                            self.notify_coordinator_waiters();
                        }
                        coordinator = self
                            .admission_changed
                            .wait(coordinator)
                            .expect("mutator coordinator should not be poisoned");
                        #[cfg(test)]
                        {
                            coordinator.blocked_outer_mutators -= 1;
                            self.notify_coordinator_waiters();
                        }
                    }
                    AdmissionPhase::Poisoned => {
                        drop(coordinator);
                        panic!("managed heap is permanently poisoned");
                    }
                }
            }
        };

        let Some(epoch) = elected else {
            return (MutatorAdmission { heap: self }, false);
        };
        (
            self.run_synthetic_collection(epoch, true, |_, _| {}, |_| {})
                .expect("entry-elected collection must preserve its mutator admission"),
            true,
        )
    }

    fn request_collection(&self) {
        assert!(!self.is_poisoned(), "managed heap is permanently poisoned");
        self.collection_requested.store(true, Ordering::Release);
        if self.is_poisoned() {
            self.collection_requested.store(false, Ordering::Release);
            panic!("managed heap is permanently poisoned");
        }
    }

    fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    fn poison_collection(&self, epoch: CollectionEpoch) {
        let mut coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Coordinator ownership linearizes poison against new outer mutator
        // admission and synchronous collection. The atomic publication keeps
        // the nonblocking request and terminal-drop paths cheap.
        self.poisoned.store(true, Ordering::Release);
        self.collection_requested.store(false, Ordering::Release);
        if coordinator.active_collection == Some(epoch) {
            coordinator.active_collection = None;
        }
        coordinator.phase = AdmissionPhase::Poisoned;
        self.notify_coordinator_waiters();
    }

    fn activity(&self) -> HeapActivity {
        assert!(!self.is_poisoned(), "managed heap is permanently poisoned");
        self.data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .activity()
    }

    fn notify_coordinator_waiters(&self) {
        #[cfg(test)]
        self.coordinator_notifications
            .fetch_add(1, Ordering::Relaxed);
        self.admission_changed.notify_all();
    }

    fn collect_full(self: &Arc<Self>) -> Result<CollectionReport, CollectionError> {
        let target = {
            let coordinator = self
                .coordinator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.is_poisoned() {
                return Err(CollectionError::Poisoned);
            }
            let (target, request) = coordinator.request_synchronous_collection();
            if request {
                self.collection_requested.store(true, Ordering::Release);
            }
            target
        };

        loop {
            let elected = {
                let mut coordinator = self
                    .coordinator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                loop {
                    if self.is_poisoned() {
                        return Err(CollectionError::Poisoned);
                    }
                    if coordinator.completed_collection_epoch >= target.get() {
                        let report = coordinator
                            .latest_collection_report
                            .expect("completed collection must publish its scalar report");
                        assert_eq!(
                            report.epoch(),
                            coordinator.completed_collection_epoch,
                            "latest report and completed epoch must publish together"
                        );
                        assert!(
                            report.epoch() >= target.get(),
                            "latest report must satisfy the synchronous target epoch"
                        );
                        return Ok(report);
                    }
                    let requested = self.collection_requested.load(Ordering::Acquire);
                    if let Some(elected) = coordinator.elect_idle_collection(requested) {
                        self.notify_coordinator_waiters();
                        break elected;
                    }
                    #[cfg(test)]
                    {
                        coordinator.blocked_collection_waiters += 1;
                        self.notify_coordinator_waiters();
                    }
                    coordinator = self
                        .admission_changed
                        .wait(coordinator)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    #[cfg(test)]
                    {
                        coordinator.blocked_collection_waiters -= 1;
                        self.notify_coordinator_waiters();
                    }
                }
            };
            assert!(
                self.run_synthetic_collection(elected, false, |_, _| {}, |_| {})
                    .is_none(),
                "maintenance collection cannot retain a mutator admission"
            );
        }
    }

    fn run_synthetic_collection<'heap>(
        self: &'heap Arc<Self>,
        epoch: CollectionEpoch,
        continue_as_mutator: bool,
        post_mark_work: impl FnOnce(&PostMarkPlan, &mut ManagedData),
        finalizer_work: impl for<'mutator> FnOnce(&Mutator<'mutator>),
    ) -> Option<MutatorAdmission<'heap>> {
        self.run_synthetic_collection_with_mark_work(
            epoch,
            continue_as_mutator,
            |_, _| {},
            post_mark_work,
            finalizer_work,
        )
    }

    fn run_synthetic_collection_with_mark_work<'heap>(
        self: &'heap Arc<Self>,
        epoch: CollectionEpoch,
        continue_as_mutator: bool,
        mark_work: impl FnOnce(&mut MarkAttempt, &mut ManagedData),
        post_mark_work: impl FnOnce(&PostMarkPlan, &mut ManagedData),
        finalizer_work: impl for<'mutator> FnOnce(&Mutator<'mutator>),
    ) -> Option<MutatorAdmission<'heap>> {
        let coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            coordinator.active_collection,
            Some(epoch),
            "collector epoch is no longer authoritative"
        );
        assert_eq!(
            coordinator.phase,
            AdmissionPhase::Exclusive,
            "elected collector must begin exclusive"
        );
        assert_eq!(coordinator.active_outer_mutators, 0);
        drop(coordinator);

        remove_inactive_thread_cache(self);
        let mut attempt = CollectionAttempt::new(self, epoch);
        let mut mark_attempt = MarkAttempt::default();
        {
            let mut data = self
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            data.clear_mark_bitmaps();
            mark_attempt.retain_pending_finalizers(&mut data);
            mark_work(&mut mark_attempt, &mut data);
        }
        let root_registry_len = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .roots
            .len();
        mark_attempt.reserve_root_capacity(root_registry_len);
        {
            let mut data = self
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            mark_attempt
                .seed_registered_roots(&mut data)
                .unwrap_or_else(|error| error.raise());
            mark_attempt
                .trace_worklist(&mut data)
                .unwrap_or_else(|error| error.raise());
        }
        let mark_summary = mark_attempt.finish();
        let sweep_summary = {
            // Post-mark work is deliberately data-side. Exclusive authority
            // was validated above and keeps this topology stable; the callback
            // must not acquire the sibling coordinator mutex while this guard
            // is held. Its managed-data borrow ends before finalizer admission.
            let mut data = self
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let post_mark = PostMarkPlan {
                #[cfg(test)]
                summary: mark_summary,
                dead_set: DeadSetPlan::classify(&data),
            };
            post_mark_work(&post_mark, &mut data);

            // Complete capacity reservation and topology validation before
            // withdrawing the first selector. Exclusive admission keeps this
            // plan stable through the allocation-free mutation window.
            let (current_epoch, next_epoch) = self.next_allocation_lease_epoch();
            let finalization_batch = data.prepare_swept_allocator_transition(&post_mark.dead_set);
            attempt.begin_topology_mutation();
            data.withdraw_allocator_frontiers();
            #[cfg(test)]
            self.maybe_panic_after_topology_mutation();

            // Wholly dead no-drop run records leave class topology only after
            // every lock-free selector is null. Their side state and headers
            // are then cleared before the locations enter the heap-wide
            // free-run pool.
            let reclaimed_runs = data.retire_wholly_dead_no_drop_runs(&post_mark.dead_set);
            data.install_finalization_batch(finalization_batch);
            data.recycle_retired_no_drop_runs();

            // Only dead allocations in retained partial no-drop runs can be
            // cleared immediately. Drop-bearing words remain intact under
            // finalization-batch ownership.
            data.sweep_partial_no_drop_runs(&post_mark.dead_set);

            // Publish each lease word from the final swept view, keep
            // finalization-bearing words reserved, and select the first
            // eligible retained run in each class. The Release epoch comes
            // last so later outer entries discard every stale cursor before
            // using these selectors.
            data.publish_swept_allocator_view();
            self.publish_allocation_lease_epoch(current_epoch, next_epoch);
            SweepSummary {
                reclaimed_slots: post_mark.dead_set.no_drop_dead_slots,
                reclaimed_runs,
            }
        };
        attempt.publish_allocator_view();

        // Prepare the TLS record without activating it. Under the coordinator
        // mutex, exclusive authority is then converted directly into one
        // collector-owned mutator obligation. No ordinary entrant can observe
        // a gap in which neither authority is present.
        let prepared = ThreadHeapEntry::prepare(self, self.current_allocation_lease_epoch());
        assert!(
            prepared.is_outer(),
            "collector thread unexpectedly holds a mutator for its target heap"
        );
        let admission = {
            let mut coordinator = self
                .coordinator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(coordinator.phase, AdmissionPhase::Exclusive);
            assert_eq!(coordinator.active_outer_mutators, 0);
            assert_eq!(coordinator.active_collection, Some(epoch));
            coordinator.phase = AdmissionPhase::Finalizing;
            coordinator.active_outer_mutators = 1;
            self.notify_coordinator_waiters();
            MutatorAdmission { heap: self }
        };
        let thread_entry =
            prepared.activate(self.current_allocation_lease_epoch(), Some(admission));
        let mutator = Mutator::new(self, thread_entry.cache());
        // Test-only/synthetic work observes the established Finalizing
        // boundary before production destruction begins. A panic here has not
        // invoked any payload destructor and remains safely retryable.
        finalizer_work(&mutator);
        let finalization_summary = self.run_finalization_batch(&mut attempt, &mutator);
        drop(mutator);
        let admission = thread_entry.into_outer_admission();

        attempt.complete(CollectionSummary {
            mark: mark_summary,
            reclaimed_slots: sweep_summary
                .reclaimed_slots
                .checked_add(finalization_summary.finalized_slots)
                .expect("collection reclaimed-slot count exhausted"),
            finalized_slots: finalization_summary.finalized_slots,
            reclaimed_runs: sweep_summary
                .reclaimed_runs
                .checked_add(finalization_summary.reclaimed_runs)
                .expect("collection reclaimed-run count exhausted"),
        });
        if continue_as_mutator {
            Some(admission)
        } else {
            drop(admission);
            None
        }
    }

    fn run_finalization_batch(
        &self,
        collection: &mut CollectionAttempt<'_>,
        mutator: &Mutator<'_>,
    ) -> FinalizationSummary {
        debug_assert!(std::ptr::eq(self, Arc::as_ptr(mutator.heap())));
        let mut summary = FinalizationSummary::default();
        let mut locations = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finalization_dispatch_snapshot();

        while let Some(location) = locations.pop() {
            let mut attempt = self
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .prepare_finalization_run(location);
            let mut panic = None;
            let mut next_work = attempt.next_work();
            let commit = next_work
                .is_some()
                .then(|| FinalizerCommitGuard::new(collection));

            while let Some(work) = next_work {
                // SAFETY: the durable map and local run snapshot were built
                // from an initialized allocated slot with this canonical
                // metadata. The exact word remains finalizer-owned, root/debug
                // access rejects every pending identity, and the installed
                // finalizer mutator prevents another collection. No collector
                // lock is held while user/Rust destruction runs.
                let result = catch_unwind(AssertUnwindSafe(|| unsafe {
                    work.metadata.drop_in_place(work.value.as_ptr())
                }));
                attempt.complete_terminal(work);
                #[cfg(test)]
                self.maybe_panic_after_finalizer_terminal_recording();
                if let Err(payload) = result {
                    panic = Some(payload);
                    break;
                }
                next_work = attempt.next_work();
            }

            let completed_slots = attempt.next_work_index;
            let reclaimed_run = self
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .complete_finalization_run(attempt);
            if let Some(commit) = commit {
                commit.publish();
            }
            summary.finalized_slots = summary
                .finalized_slots
                .checked_add(completed_slots)
                .expect("finalized-slot count exhausted");
            if reclaimed_run {
                summary.reclaimed_runs = summary
                    .reclaimed_runs
                    .checked_add(1)
                    .expect("finalized-run count exhausted");
            }
            if let Some(payload) = panic {
                resume_unwind(payload);
            }
        }
        summary
    }

    fn release_outer_mutator(&self) {
        let mut coordinator = self
            .coordinator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(test)]
        if coordinator.phase == AdmissionPhase::Poisoned {
            self.poisoned_outer_mutator_releases
                .fetch_add(1, Ordering::Relaxed);
        }
        coordinator.active_outer_mutators = coordinator
            .active_outer_mutators
            .checked_sub(1)
            .expect("active mutator count underflow");
        if coordinator.active_outer_mutators == 0 {
            self.notify_coordinator_waiters();
        }
    }

    #[cfg(test)]
    fn enter_synthetic_exclusive(&self) -> SyntheticExclusiveAdmission<'_> {
        let mut coordinator = self
            .coordinator
            .lock()
            .expect("mutator coordinator should not be poisoned");
        while coordinator.phase != AdmissionPhase::Ordinary
            || coordinator.active_outer_mutators != 0
        {
            coordinator = self
                .admission_changed
                .wait(coordinator)
                .expect("mutator coordinator should not be poisoned");
        }
        coordinator.phase = AdmissionPhase::Exclusive;
        self.notify_coordinator_waiters();
        SyntheticExclusiveAdmission { heap: self }
    }

    #[cfg(test)]
    fn coordinator_snapshot(&self) -> CoordinatorSnapshot {
        let coordinator = *self
            .coordinator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        CoordinatorSnapshot {
            phase: coordinator.phase,
            active_outer_mutators: coordinator.active_outer_mutators,
            collection_requested: self.collection_requested.load(Ordering::Acquire),
            active_collection: coordinator.active_collection,
            completed_collection_epoch: coordinator.completed_collection_epoch,
            latest_collection_report: coordinator.latest_collection_report,
            blocked_outer_mutators: coordinator.blocked_outer_mutators,
        }
    }

    #[cfg(test)]
    fn coordinator_notification_count(&self) -> usize {
        self.coordinator_notifications.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn poisoned_outer_mutator_release_count(&self) -> usize {
        self.poisoned_outer_mutator_releases.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn install_collection_acknowledgement_hook(
        &self,
        arrived: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) {
        let mut hook = self
            .collection_acknowledgement_hook
            .lock()
            .expect("collection acknowledgement hook should not be poisoned");
        assert!(
            hook.is_none(),
            "collection acknowledgement hook already installed"
        );
        *hook = Some(CollectionAcknowledgementHook { arrived, release });
    }

    #[cfg(test)]
    fn clear_collection_acknowledgement_hook(&self) {
        self.collection_acknowledgement_hook
            .lock()
            .expect("collection acknowledgement hook should not be poisoned")
            .take();
    }

    #[cfg(test)]
    fn install_finalized_word_release_hook(
        &self,
        location: RunLocation,
        word_index: usize,
        arrived: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) {
        let mut data = self
            .data
            .lock()
            .expect("managed heap data should not be poisoned");
        assert!(
            data.finalized_word_release_hook.is_none(),
            "finalized-word release hook already installed"
        );
        data.finalized_word_release_hook = Some(FinalizedWordReleaseHook {
            location,
            word_index,
            arrived,
            release,
        });
    }

    #[cfg(test)]
    fn inject_panic_after_topology_mutation(&self) {
        assert!(
            !self
                .panic_after_topology_mutation
                .swap(true, Ordering::AcqRel),
            "topology-mutation panic is already armed"
        );
    }

    #[cfg(test)]
    fn maybe_panic_after_topology_mutation(&self) {
        if self
            .panic_after_topology_mutation
            .swap(false, Ordering::AcqRel)
        {
            panic!("injected panic after destructive topology mutation");
        }
    }

    #[cfg(test)]
    fn inject_panic_after_finalizer_terminal_recording(&self) {
        assert!(
            !self
                .panic_after_finalizer_terminal_recording
                .swap(true, Ordering::AcqRel),
            "finalizer-terminal-recording panic is already armed"
        );
    }

    #[cfg(test)]
    fn maybe_panic_after_finalizer_terminal_recording(&self) {
        if self
            .panic_after_finalizer_terminal_recording
            .swap(false, Ordering::AcqRel)
        {
            panic!("injected panic after finalizer terminal recording");
        }
    }

    #[cfg(test)]
    fn pause_after_collection_acknowledgement(&self) {
        let hook = self
            .collection_acknowledgement_hook
            .lock()
            .expect("collection acknowledgement hook should not be poisoned")
            .clone();
        if let Some(hook) = hook {
            hook.arrived.wait();
            hook.release.wait();
        }
    }

    #[cfg(test)]
    fn elect_idle_collection_for_test(&self) -> CollectionEpoch {
        let mut coordinator = self
            .coordinator
            .lock()
            .expect("mutator coordinator should not be poisoned");
        coordinator
            .elect_idle_collection(self.collection_requested.load(Ordering::Acquire))
            .expect("test heap must be idle with a collection requested")
    }

    #[cfg(test)]
    fn wait_for_blocked_outer_mutators(&self, expected: usize) {
        let mut coordinator = self
            .coordinator
            .lock()
            .expect("mutator coordinator should not be poisoned");
        while coordinator.blocked_outer_mutators != expected {
            coordinator = self
                .admission_changed
                .wait(coordinator)
                .expect("mutator coordinator should not be poisoned");
        }
    }

    #[cfg(test)]
    fn wait_for_collection_waiters(&self, expected: usize) {
        let mut coordinator = self
            .coordinator
            .lock()
            .expect("mutator coordinator should not be poisoned");
        while coordinator.blocked_collection_waiters != expected {
            coordinator = self
                .admission_changed
                .wait(coordinator)
                .expect("mutator coordinator should not be poisoned");
        }
    }

    fn current_allocation_lease_epoch(&self) -> AllocationLeaseEpoch {
        AllocationLeaseEpoch::from_raw(self.allocation_lease_epoch.load(Ordering::Acquire))
            .expect("allocation lease epoch must remain nonzero")
    }

    fn next_allocation_lease_epoch(&self) -> (AllocationLeaseEpoch, AllocationLeaseEpoch) {
        let current = self.current_allocation_lease_epoch();
        let next = current
            .get()
            .checked_add(1)
            .and_then(AllocationLeaseEpoch::from_raw)
            .expect("allocation lease epoch exhausted");
        (current, next)
    }

    fn publish_allocation_lease_epoch(
        &self,
        current: AllocationLeaseEpoch,
        next: AllocationLeaseEpoch,
    ) {
        self.allocation_lease_epoch
            .compare_exchange(
                current.get(),
                next.get(),
                Ordering::Release,
                Ordering::Acquire,
            )
            .expect("allocation lease epoch changed outside exclusive collection");
    }

    #[cfg(test)]
    fn advance_allocation_lease_epoch(&self) -> AllocationLeaseEpoch {
        let (current, next) = self.next_allocation_lease_epoch();
        self.publish_allocation_lease_epoch(current, next);
        next
    }

    pub(crate) fn discover_class<T: Trace>(
        &self,
        metadata: &'static ObjectMetadata,
        geometry: RunGeometry,
    ) -> AllocationClass<T> {
        self.discover_class_with(metadata, geometry, || {
            AllocationClassEntry::new(metadata, geometry)
        })
    }

    fn discover_class_with<T: Trace>(
        &self,
        metadata: &'static ObjectMetadata,
        geometry: RunGeometry,
        make_candidate: impl FnOnce() -> AllocationClassEntry,
    ) -> AllocationClass<T> {
        let identity = MetadataIdentity::new(metadata);
        {
            let state = self.data.lock().expect("heap state should not be poisoned");
            if let Some(id) = state.classes_by_metadata.get(&identity).copied() {
                let shared = Arc::clone(
                    state.classes[class_index(id).expect("known class ID must be valid")].shared(),
                );
                return AllocationClass::new(self, metadata, id, shared);
            }
        }

        // As with process metadata, immutable candidate construction remains
        // outside the heap lock. A panic publishes neither a dense ID nor a
        // class-table entry.
        let candidate = make_candidate();
        assert!(
            std::ptr::eq(candidate.metadata(), metadata),
            "allocation-class candidate metadata mismatch"
        );
        assert_eq!(
            candidate.geometry(),
            geometry,
            "allocation-class candidate geometry mismatch"
        );

        let mut state = self.data.lock().expect("heap state should not be poisoned");
        if let Some(id) = state.classes_by_metadata.get(&identity).copied() {
            let shared = Arc::clone(
                state.classes[class_index(id).expect("known class ID must be valid")].shared(),
            );
            return AllocationClass::new(self, metadata, id, shared);
        }

        let next = state
            .classes
            .len()
            .checked_add(1)
            .and_then(|id| u64::try_from(id).ok())
            .and_then(AllocationClassId::new)
            .expect("allocation class ID space exhausted");
        state
            .classes
            .try_reserve(1)
            .expect("allocation-class table capacity exhausted");
        state
            .classes_by_metadata
            .try_reserve(1)
            .expect("allocation-class index capacity exhausted");
        state.classes.push(candidate);
        let shared = Arc::clone(
            state.classes[class_index(next).expect("new class ID must be valid")].shared(),
        );
        let replaced = state.classes_by_metadata.insert(identity, next);
        debug_assert!(replaced.is_none());

        AllocationClass::new(self, metadata, next, shared)
    }

    #[cfg(test)]
    fn prepare_run<T: Trace>(
        &self,
        class: &AllocationClass<T>,
    ) -> Result<RunLocation, PrepareRunError> {
        if !class.belongs_to(self) {
            return Err(PrepareRunError::ForeignClass);
        }

        let mut data = self
            .data
            .lock()
            .expect("managed heap data should not be poisoned");
        let index = class_index(class.id()).ok_or(PrepareRunError::InvalidClass)?;
        let entry = data
            .classes
            .get(index)
            .ok_or(PrepareRunError::InvalidClass)?;
        if !std::ptr::eq(entry.metadata(), class.metadata()) {
            return Err(PrepareRunError::InvalidClass);
        }
        let geometry = entry.geometry();

        let location = data
            .publish_run(index, class.id(), geometry, &self.collection_requested)
            .map_err(PrepareRunError::Publication)?;
        Ok(location)
    }

    #[cfg(test)]
    pub(crate) fn allocate_synchronized<T: Trace>(
        &self,
        class: &AllocationClass<T>,
        value: T,
    ) -> NonNull<T> {
        self.allocate_synchronized_with(class, value, || {})
    }

    pub(crate) fn claim_allocation_cursor<T: Trace>(
        &self,
        class: &AllocationClass<T>,
    ) -> AllocationCursor {
        assert!(
            class.belongs_to(self),
            "allocation class does not belong to this heap"
        );
        #[cfg(test)]
        self.allocation_cursor_claims
            .fetch_add(1, Ordering::Relaxed);

        if let Some(claimed) = class.claim_frontier(self) {
            return allocation_cursor(class.id(), claimed);
        }

        #[cfg(test)]
        self.allocation_cursor_slow_paths
            .fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        self.pause_before_allocation_cursor_slow_path();
        let mut data = self
            .data
            .lock()
            .expect("managed heap data should not be poisoned");
        let index = class_index(class.id()).expect("allocation class has an invalid ID");
        let geometry = {
            let entry = data
                .classes
                .get(index)
                .expect("allocation class is absent from its heap");
            assert!(
                std::ptr::eq(entry.metadata(), class.metadata()),
                "allocation class metadata does not match its heap entry"
            );
            assert!(
                Arc::ptr_eq(entry.shared(), class.shared()),
                "allocation class shared state does not match its heap entry"
            );
            entry.geometry()
        };

        // Another publisher may have advanced the frontier before this thread
        // acquired heap state. Recheck before changing authoritative topology.
        if let Some(claimed) = class.claim_frontier(self) {
            #[cfg(test)]
            self.allocation_cursor_locked_recheck_hits
                .fetch_add(1, Ordering::Relaxed);
            return allocation_cursor(class.id(), claimed);
        }

        loop {
            #[cfg(test)]
            self.allocation_cursor_frontier_advance_attempts
                .fetch_add(1, Ordering::Relaxed);
            if let Some(target) = data.classes[index].advance_frontier() {
                if let Some(claimed) = target.claim_allocation_word() {
                    return allocation_cursor(class.id(), claimed);
                }
                continue;
            }

            data.publish_run(index, class.id(), geometry, &self.collection_requested)
                .unwrap_or_else(|error| panic!("managed run allocation failed: {error:?}"));
            let target = data.classes[index].activate_last_run();
            if let Some(claimed) = target.claim_allocation_word() {
                return allocation_cursor(class.id(), claimed);
            }
        }
    }

    #[cfg(test)]
    fn allocation_cursor_claim_count(&self) -> usize {
        self.allocation_cursor_claims.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn allocation_cursor_slow_path_count(&self) -> usize {
        self.allocation_cursor_slow_paths.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn allocation_cursor_locked_recheck_hit_count(&self) -> usize {
        self.allocation_cursor_locked_recheck_hits
            .load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn allocation_cursor_frontier_advance_attempt_count(&self) -> usize {
        self.allocation_cursor_frontier_advance_attempts
            .load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn install_allocation_cursor_slow_path_hook(
        &self,
        arrived: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) {
        let replaced = self
            .allocation_cursor_slow_path_hook
            .lock()
            .expect("allocation-cursor test hook should not be poisoned")
            .replace(AllocationCursorSlowPathHook { arrived, release });
        assert!(replaced.is_none(), "allocation-cursor test hook is active");
    }

    #[cfg(test)]
    fn clear_allocation_cursor_slow_path_hook(&self) {
        let removed = self
            .allocation_cursor_slow_path_hook
            .lock()
            .expect("allocation-cursor test hook should not be poisoned")
            .take();
        assert!(removed.is_some(), "allocation-cursor test hook is absent");
    }

    #[cfg(test)]
    fn pause_before_allocation_cursor_slow_path(&self) {
        let hook = self
            .allocation_cursor_slow_path_hook
            .lock()
            .expect("allocation-cursor test hook should not be poisoned")
            .clone();
        if let Some(hook) = hook {
            hook.arrived.wait();
            hook.release.wait();
        }
    }

    #[cfg(test)]
    fn allocation_pressure(&self) -> AllocationPressureSnapshot {
        let pressure = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .allocation_pressure;
        AllocationPressureSnapshot {
            assigned_runs: pressure.assigned_runs,
            high_water_mark: pressure.high_water_mark,
            collection_requested: self.collection_requested.load(Ordering::Acquire),
        }
    }

    #[cfg(test)]
    fn allocate_synchronized_with<T: Trace>(
        &self,
        class: &AllocationClass<T>,
        value: T,
        before_initialize: impl FnOnce(),
    ) -> NonNull<T> {
        assert!(
            class.belongs_to(self),
            "allocation class does not belong to this heap"
        );

        {
            let mut data = self
                .data
                .lock()
                .expect("managed heap data should not be poisoned");
            let index = class_index(class.id()).expect("allocation class has an invalid ID");
            let entry = data
                .classes
                .get(index)
                .expect("allocation class is absent from its heap");
            assert!(
                std::ptr::eq(entry.metadata(), class.metadata()),
                "allocation class metadata does not match its heap entry"
            );
            let geometry = entry.geometry();

            let selected = entry.runs().iter().find_map(|run| {
                data.arena
                    .first_free_slot(run.location)
                    .map(|slot_index| (run.location, slot_index))
            });
            let (location, slot_index) = if let Some((location, slot_index)) = selected {
                (location, slot_index)
            } else {
                let location = data
                    .publish_run(index, class.id(), geometry, &self.collection_requested)
                    .unwrap_or_else(|error| panic!("managed run allocation failed: {error:?}"));
                (location, 0)
            };

            // This hook exists only to latch that selecting a currently free
            // slot publishes no state. Production initialization below
            // contains no panicking operation between writing `T` and its
            // allocation bit.
            before_initialize();
            data.arena
                .initialize_slot(location, class.id(), geometry, slot_index, value)
        }
    }

    #[cfg(test)]
    fn resolve_slot(&self, address: usize) -> Option<ResolvedSlot> {
        let state = self.data.lock().ok()?;
        resolve_slot_in_state(&state, address)
    }

    #[cfg(test)]
    fn resolved_runs(&self) -> Vec<ResolvedRun> {
        let state = self.data.lock().expect("heap state should not be poisoned");
        state
            .arena
            .initialized_runs()
            .into_iter()
            .filter_map(|run| {
                let entry = state.classes.get(class_index(run.class_id)?)?;
                if entry.geometry() != run.geometry || !entry.contains_run(run.location) {
                    return None;
                }
                Some(ResolvedRun {
                    location: run.location,
                    metadata: entry.metadata(),
                    class_id: run.class_id,
                    geometry: run.geometry,
                })
            })
            .collect()
    }

    pub(crate) fn debug_assert_access<T: Trace>(&self, pointer: NonNull<T>) {
        #[cfg(debug_assertions)]
        {
            let expected = metadata_for::<T>();
            let value = ErasedGc::new(pointer.cast());
            let state = self.data.lock().expect("heap state should not be poisoned");
            let validation = validate_rootable_in_state(&state, value, expected);
            drop(state);
            if let Err(error) = validation {
                error.raise();
            }
        }

        #[cfg(not(debug_assertions))]
        let _ = pointer;
    }

    pub(crate) fn register_root<T: Trace>(self: &Arc<Self>, value: crate::Gc<T>) -> Root<T> {
        let (root, registration) = Root::candidate(self, value);
        let expected = metadata_for::<T>();
        let value = value.erase();
        let mut state = self.data.lock().expect("heap state should not be poisoned");
        if let Err(error) = validate_rootable_in_state(&state, value, expected) {
            drop(state);
            error.raise();
        }
        state
            .roots
            .try_reserve(1)
            .expect("root registry capacity exhausted");
        state.roots.push(registration);
        root
    }

    #[cfg(test)]
    fn visit_registered_roots(&self, visit: impl FnMut(crate::trace::ErasedGc)) -> usize {
        {
            let coordinator = self
                .coordinator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(
                coordinator.phase,
                AdmissionPhase::Exclusive,
                "root traversal requires exclusive collection authority"
            );
            assert_eq!(coordinator.active_outer_mutators, 0);
        }

        let mut data = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        retain_registered_roots(&mut data.roots, visit)
    }
}

fn retain_registered_roots(
    roots: &mut Vec<Weak<RootCell>>,
    mut visit: impl FnMut(ErasedGc),
) -> usize {
    let mut visited = 0;
    roots.retain(|registration| {
        let Some(cell) = registration.upgrade() else {
            return false;
        };
        visit(cell.value());
        drop(cell);
        visited += 1;
        true
    });
    visited
}

enum RootValidationError {
    ForeignHeap,
    Representation {
        actual: &'static str,
        expected: &'static str,
    },
    Unallocated,
    PendingFinalization,
}

impl RootValidationError {
    fn raise(self) -> ! {
        match self {
            Self::ForeignHeap => panic!("managed pointer does not belong to this heap"),
            Self::Representation { actual, expected } => {
                panic!("managed pointer has representation `{actual}`, not requested `{expected}`")
            }
            Self::Unallocated => panic!("managed pointer does not identify an allocated value"),
            Self::PendingFinalization => {
                panic!("managed pointer is pending finalization and cannot be rooted or accessed")
            }
        }
    }
}

fn validate_rootable_in_state(
    state: &ManagedData,
    value: ErasedGc,
    expected: &'static ObjectMetadata,
) -> Result<(), RootValidationError> {
    let address = value.as_ptr().as_ptr() as usize;
    if let Some(actual) = state
        .finalization_batch
        .pending_metadata_at(&state.arena, address)
    {
        if !std::ptr::eq(actual, expected) {
            return Err(RootValidationError::Representation {
                actual: actual.type_name(),
                expected: expected.type_name(),
            });
        }
        return Err(RootValidationError::PendingFinalization);
    }

    let resolved = resolve_slot_in_state(state, address).ok_or(RootValidationError::ForeignHeap)?;

    if !std::ptr::eq(resolved.metadata, expected) {
        return Err(RootValidationError::Representation {
            actual: resolved.metadata.type_name(),
            expected: expected.type_name(),
        });
    }
    if !resolved.allocated {
        return Err(RootValidationError::Unallocated);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CollectionAttemptState {
    Reversible,
    TopologyMutation,
    AllocatorViewPublished,
    FinalizerCommitPending,
    Poisoned,
    Completed,
}

struct CollectionAttempt<'heap> {
    heap: &'heap HeapInner,
    epoch: CollectionEpoch,
    state: CollectionAttemptState,
}

impl<'heap> CollectionAttempt<'heap> {
    fn new(heap: &'heap HeapInner, epoch: CollectionEpoch) -> Self {
        Self {
            heap,
            epoch,
            state: CollectionAttemptState::Reversible,
        }
    }

    fn begin_topology_mutation(&mut self) {
        assert_eq!(self.state, CollectionAttemptState::Reversible);
        self.state = CollectionAttemptState::TopologyMutation;
    }

    fn publish_allocator_view(&mut self) {
        assert_eq!(self.state, CollectionAttemptState::TopologyMutation);
        self.state = CollectionAttemptState::AllocatorViewPublished;
    }

    fn begin_finalizer_commit(&mut self) {
        assert_eq!(self.state, CollectionAttemptState::AllocatorViewPublished);
        self.state = CollectionAttemptState::FinalizerCommitPending;
    }

    fn publish_finalizer_commit(&mut self) {
        assert_eq!(self.state, CollectionAttemptState::FinalizerCommitPending);
        self.state = CollectionAttemptState::AllocatorViewPublished;
    }

    fn poison(&mut self) {
        // This is an unwind-safety path: its callers establish the
        // irreversible state structurally, and poisoning itself must not add
        // another assertion panic while preserving the original payload.
        self.heap.poison_collection(self.epoch);
        self.state = CollectionAttemptState::Poisoned;
    }

    fn complete(&mut self, summary: CollectionSummary) {
        assert_eq!(self.state, CollectionAttemptState::AllocatorViewPublished);
        {
            // Publish the post-finalization assigned-run baseline and
            // serialize request acknowledgement with run assignment. Pressure
            // raised by finalizer allocations before this lock is coalesced;
            // a later publication survives the clear.
            let mut data = self
                .heap
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                data.activity().is_idle(),
                "successful collection retained finalizer activity"
            );
            data.publish_survivor_pressure_baseline();
            self.heap
                .collection_requested
                .store(false, Ordering::Release);
        }
        #[cfg(test)]
        self.heap.pause_after_collection_acknowledgement();
        {
            let mut coordinator = self
                .heap
                .coordinator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(coordinator.phase, AdmissionPhase::Finalizing);
            assert_eq!(coordinator.active_collection, Some(self.epoch));
            let report = CollectionReport {
                epoch: self.epoch.0,
                root_entries: summary.mark.root_entries,
                traced_objects: summary.mark.traced_objects,
                marked_slots: summary.mark.marked_slots,
                conservatively_retained_slots: summary.mark.conservatively_retained_slots,
                reclaimed_slots: summary.reclaimed_slots,
                finalized_slots: summary.finalized_slots,
                reclaimed_runs: summary.reclaimed_runs,
                #[cfg(test)]
                peak_object_worklist_len: summary.mark.peak_object_worklist_len,
                #[cfg(test)]
                peak_object_worklist_capacity: summary.mark.peak_object_worklist_capacity,
            };
            coordinator.latest_collection_report = Some(report);
            coordinator.completed_collection_epoch = self.epoch.get();
            coordinator.active_collection = None;
            coordinator.phase = AdmissionPhase::Ordinary;
            self.heap.notify_coordinator_waiters();
        }
        self.state = CollectionAttemptState::Completed;
    }
}

impl Drop for CollectionAttempt<'_> {
    fn drop(&mut self) {
        match self.state {
            CollectionAttemptState::Poisoned | CollectionAttemptState::Completed => return,
            CollectionAttemptState::TopologyMutation
            | CollectionAttemptState::FinalizerCommitPending => {
                self.poison();
                return;
            }
            CollectionAttemptState::Reversible | CollectionAttemptState::AllocatorViewPublished => {
            }
        }
        let data = self
            .heap
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.heap.data.clear_poison();
        if self.state == CollectionAttemptState::AllocatorViewPublished {
            // Swept topology and the allocation-lease epoch are already
            // durable. Publish the matching post-attempt pressure baseline
            // before the original panic resumes, but leave the completion
            // epoch and report untouched.
            let mut data = data;
            assert_eq!(data.running_finalizers, 0);
            data.publish_survivor_pressure_baseline();
            drop(data);
        } else {
            drop(data);
        }
        self.heap
            .collection_requested
            .store(true, Ordering::Release);
        let mut coordinator = self
            .heap
            .coordinator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if coordinator.active_collection == Some(self.epoch) {
            coordinator.active_collection = None;
            coordinator.phase = AdmissionPhase::Ordinary;
        }
        self.heap.notify_coordinator_waiters();
    }
}

struct FinalizerCommitGuard<'attempt, 'heap> {
    attempt: &'attempt mut CollectionAttempt<'heap>,
}

impl<'attempt, 'heap> FinalizerCommitGuard<'attempt, 'heap> {
    fn new(attempt: &'attempt mut CollectionAttempt<'heap>) -> Self {
        attempt.begin_finalizer_commit();
        Self { attempt }
    }

    fn publish(self) {
        self.attempt.publish_finalizer_commit();
    }
}

impl Drop for FinalizerCommitGuard<'_, '_> {
    fn drop(&mut self) {
        if self.attempt.state == CollectionAttemptState::FinalizerCommitPending {
            self.attempt.poison();
        }
    }
}

fn resolve_slot_in_state(state: &ManagedData, address: usize) -> Option<ResolvedSlot> {
    let (owner, metadata) = resolve_slot_topology(state, address).ok()?;
    let allocated = state.arena.owner_slot_is_allocated(owner);
    Some(ResolvedSlot {
        metadata,
        #[cfg(test)]
        class_id: owner.class_id,
        #[cfg(test)]
        geometry: owner.geometry,
        #[cfg(test)]
        slot_index: owner.slot_index,
        allocated,
    })
}

fn resolve_slot_topology(
    state: &ManagedData,
    address: usize,
) -> Result<(RunOwner, &'static ObjectMetadata), CollectorLookupError> {
    resolve_slot_topology_in(&state.arena, &state.classes, address)
}

fn resolve_slot_topology_in(
    arena: &Arena,
    classes: &[AllocationClassEntry],
    address: usize,
) -> Result<(RunOwner, &'static ObjectMetadata), CollectorLookupError> {
    let owner = arena
        .checked_slot_owner(address)
        .ok_or(CollectorLookupError::InvalidAddress)?;
    let index = class_index(owner.class_id).ok_or(CollectorLookupError::InvalidClass)?;
    let entry = classes
        .get(index)
        .ok_or(CollectorLookupError::InvalidClass)?;
    if entry.geometry() != owner.geometry || !entry.contains_run(owner.location) {
        return Err(CollectorLookupError::InvalidRunTopology);
    }
    Ok((owner, entry.metadata()))
}

fn collector_slot_in(
    arena: &Arena,
    classes: &[AllocationClassEntry],
    value: ErasedGc,
) -> Result<CollectorSlot, CollectorLookupError> {
    let address = value.as_ptr().as_ptr() as usize;
    let (owner, metadata) = resolve_slot_topology_in(arena, classes, address)?;
    if !arena.owner_slot_is_allocated(owner) {
        return Err(CollectorLookupError::Unallocated);
    }
    Ok(CollectorSlot { owner, metadata })
}

fn mark_collector_slot_in(
    arena: &mut Arena,
    classes: &[AllocationClassEntry],
    slot: CollectorSlot,
) -> bool {
    debug_assert!(std::ptr::eq(
        slot.metadata,
        classes[class_index(slot.owner.class_id)
            .expect("collector slot must retain a valid class ID")]
        .metadata()
    ));
    arena.mark_owner_slot(slot.owner)
}

impl Drop for HeapInner {
    fn drop(&mut self) {
        if self.poisoned.load(Ordering::Acquire) {
            // An irreversible collection panic may leave initialized payloads
            // whose destructor-dispatch status is no longer authoritative.
            // Releasing raw arena storage leaks their Rust-owned resources but
            // cannot invoke one destructor twice.
            return;
        }
        let state = self
            .data
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Detached finalization runs no longer occur in allocation-class
        // topology. They were wholly dead when detached, cannot be allocated
        // from afterward, and terminally retired prefix slots have already
        // cleared their allocation bits. Every remaining allocated slot is
        // therefore one untouched pending destructor obligation.
        for run in state
            .finalization_batch
            .runs
            .values()
            .filter(|run| run.target.is_detached())
        {
            for pointer in state
                .arena
                .allocated_slot_pointers(run.target.target().location)
            {
                // SAFETY: the detached run's remaining allocation bits name
                // initialized payloads with this record's canonical metadata.
                // Detached runs are absent from the following class walk, so
                // terminal teardown invokes each such destructor exactly once.
                unsafe { run.metadata.drop_in_place(pointer) };
            }
        }

        // Attached pending finalizers remain ordinary allocated class members.
        // Terminal teardown has no retry state to preserve, so the class walk
        // can destroy them together with every other remaining allocation and
        // does not need a per-object finalization-index query.
        for entry in &state.classes {
            let metadata = entry.metadata();
            if !metadata.needs_drop() {
                continue;
            }
            for run in entry.runs() {
                for pointer in state.arena.allocated_slot_pointers(run.location) {
                    // SAFETY: the allocation bitmap is published only after a
                    // value with this run's canonical metadata is initialized.
                    // Final heap ownership is exclusive, and detached runs are
                    // absent from class topology, so this walk visits each
                    // attached allocated slot exactly once before arena storage
                    // is released.
                    unsafe { metadata.drop_in_place(pointer) };
                }
            }
        }
    }
}

fn class_index(id: AllocationClassId) -> Option<usize> {
    usize::try_from(id.get().checked_sub(1)?).ok()
}

fn class_id(index: usize) -> AllocationClassId {
    u64::try_from(index)
        .ok()
        .and_then(|index| index.checked_add(1))
        .and_then(AllocationClassId::new)
        .expect("allocation class ID space exhausted")
}

fn allocation_cursor(
    class_id: AllocationClassId,
    claimed: crate::arena::ClaimedAllocationWord,
) -> AllocationCursor {
    AllocationCursor {
        class_id,
        location: claimed.location,
        run: claimed.run,
        geometry: claimed.geometry,
        word_index: claimed.word_index,
        free_mask: claimed.free_mask,
    }
}

#[cfg(test)]
#[expect(unsafe_code, reason = "reviewed C2B allocation-class fixtures")]
mod tests {
    use std::collections::HashSet;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex, mpsc};
    use std::time::Duration;

    use crate::{
        Trace, UnsupportedLayout, Visitor,
        arena::Arena,
        class::metadata_for,
        run::{AllocationClassId, RunGeometry},
        thread_cache::{
            AllocationCursor, AllocationLeaseEpoch, cache_snapshot, cursor, insert_cursor,
            registry_contains, thread_has_any_active_mutator,
        },
        trace::ErasedGc,
    };

    use super::{
        AdmissionPhase, AllocationClass, AllocationPressure, AllocationPressureSnapshot,
        CollectionError, CollectorLookupError, CollectorSlot, DeadBitmapWord, DeadSlotDisposition,
        FIXED_SURVIVOR_RUN_HEADROOM, Heap, HeapActivity, HeapInner, MarkSummary, PrepareRunError,
        RootValidationError, RunLocation, RunPublicationError, SURVIVOR_GROWTH_DENOMINATOR,
        SURVIVOR_GROWTH_NUMERATOR, class_index, resolve_slot_in_state,
        survivor_run_high_water_mark, validate_rootable_in_state,
    };

    fn internal_class<T: Trace>(heap: &Heap) -> AllocationClass<T> {
        let metadata = metadata_for::<T>();
        let geometry = RunGeometry::derive(metadata.layout(), metadata.requested_slot_size())
            .expect("test allocation class must have a supported layout");
        heap.inner.discover_class(metadata, geometry)
    }

    fn allocate<T: Trace>(heap: &Heap, value: T) -> crate::Gc<T> {
        heap.with_mutator(|mutator| mutator.allocator::<T>().unwrap().alloc(value))
    }

    fn collector_slot<T: Trace>(heap: &Heap, value: crate::Gc<T>) -> CollectorSlot {
        heap.inner
            .data
            .lock()
            .unwrap()
            .collector_slot(value.erase())
            .unwrap()
    }

    fn slot_is_marked(heap: &Heap, slot: CollectorSlot) -> bool {
        heap.inner
            .data
            .lock()
            .unwrap()
            .collector_slot_is_marked(slot)
    }

    fn assert_failed_collection_restored(heap: &Heap) {
        assert!(
            !heap.inner.data.is_poisoned(),
            "failed collection must recover the managed-data mutex"
        );
        let restored = heap.inner.coordinator_snapshot();
        assert_eq!(restored.phase, AdmissionPhase::Ordinary);
        assert_eq!(restored.active_collection, None);
        assert_eq!(restored.completed_collection_epoch, 0);
        assert_eq!(restored.latest_collection_report, None);
        assert!(restored.collection_requested);
    }

    #[derive(Debug, Eq, PartialEq)]
    struct ClassificationClassSnapshot {
        class_id: AllocationClassId,
        metadata: usize,
        runs: Vec<RunLocation>,
        frontier: Option<RunLocation>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct ClassificationRunSnapshot {
        class_id: AllocationClassId,
        location: RunLocation,
        geometry: RunGeometry,
        allocations: Vec<usize>,
        side_metadata: Vec<u8>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct ClassificationStateSnapshot {
        classes: Vec<ClassificationClassSnapshot>,
        runs: Vec<ClassificationRunSnapshot>,
        pressure: AllocationPressureSnapshot,
        allocation_lease_epoch: AllocationLeaseEpoch,
    }

    fn classification_state_snapshot(
        heap: &Heap,
        data: &super::ManagedData,
    ) -> ClassificationStateSnapshot {
        let classes = data
            .classes
            .iter()
            .enumerate()
            .map(|(index, class)| {
                let class_id =
                    AllocationClassId::new(u64::try_from(index).unwrap().checked_add(1).unwrap())
                        .unwrap();
                ClassificationClassSnapshot {
                    class_id,
                    metadata: class.metadata() as *const _ as usize,
                    runs: class.runs().iter().map(|run| run.location).collect(),
                    frontier: class.shared().frontier(&heap.inner),
                }
            })
            .collect();
        let runs = data
            .classes
            .iter()
            .enumerate()
            .flat_map(|(index, class)| {
                let class_id =
                    AllocationClassId::new(u64::try_from(index).unwrap().checked_add(1).unwrap())
                        .unwrap();
                class
                    .runs()
                    .iter()
                    .map(move |run| ClassificationRunSnapshot {
                        class_id,
                        location: run.location,
                        geometry: run.geometry,
                        allocations: data
                            .arena
                            .allocated_slot_pointers(run.location)
                            .into_iter()
                            .map(|pointer| pointer.as_ptr() as usize)
                            .collect(),
                        side_metadata: data
                            .arena
                            .run_side_metadata_for_test(run.location, run.geometry),
                    })
            })
            .collect();
        ClassificationStateSnapshot {
            classes,
            runs,
            pressure: AllocationPressureSnapshot {
                assigned_runs: data.allocation_pressure.assigned_runs,
                high_water_mark: data.allocation_pressure.high_water_mark,
                collection_requested: heap.inner.collection_requested.load(Ordering::Acquire),
            },
            allocation_lease_epoch: heap.inner.current_allocation_lease_epoch(),
        }
    }

    fn dead_bitmap_words(slots: &[CollectorSlot]) -> Vec<DeadBitmapWord> {
        let mut words: Vec<DeadBitmapWord> = Vec::new();
        for slot in slots {
            let word_index = slot.owner.slot_index / u64::BITS as usize;
            let bit = 1_u64 << (slot.owner.slot_index % u64::BITS as usize);
            if let Some(word) = words.iter_mut().find(|word| word.word_index == word_index) {
                word.dead_mask |= bit;
            } else {
                words.push(DeadBitmapWord {
                    word_index,
                    dead_mask: bit,
                });
            }
        }
        words.sort_by_key(|word| word.word_index);
        words
    }

    fn allocation_bytes(run: &ClassificationRunSnapshot) -> &[u8] {
        &run.side_metadata[..run.geometry.allocation_bitmap.byte_len()]
    }

    fn lease_words(run: &ClassificationRunSnapshot) -> Vec<u64> {
        let start = run.geometry.allocation_bitmap.byte_len();
        let end = start + run.geometry.lease_bitmap.byte_len();
        run.side_metadata[start..end]
            .chunks_exact(std::mem::size_of::<u64>())
            .map(|word| u64::from_ne_bytes(word.try_into().unwrap()))
            .collect()
    }

    fn mark_bytes(run: &ClassificationRunSnapshot) -> &[u8] {
        let start =
            run.geometry.allocation_bitmap.byte_len() + run.geometry.lease_bitmap.byte_len();
        &run.side_metadata[start..]
    }

    fn panic_string(panic: &(dyn std::any::Any + Send)) -> &str {
        panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&'static str>().copied())
            .expect("test panic must carry a string payload")
    }

    struct FirstType {
        _value: u64,
    }

    // SAFETY: `FirstType` contains no managed edge.
    unsafe impl Trace for FirstType {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct SecondType {
        _value: u64,
    }

    // SAFETY: `SecondType` contains no managed edge.
    unsafe impl Trace for SecondType {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct OverflowingSlot {
        _value: u64,
    }

    struct DroppingType {
        _value: u64,
    }

    impl Drop for DroppingType {
        fn drop(&mut self) {}
    }

    // SAFETY: `DroppingType` contains no managed edge.
    unsafe impl Trace for DroppingType {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct WideSlot {
        value: u64,
    }

    // SAFETY: `WideSlot` contains no managed edge. Its large total slot request
    // makes allocation topology cross run and chunk boundaries with few test
    // values.
    unsafe impl Trace for WideSlot {
        const REQUESTED_SLOT_SIZE: Option<usize> = Some(32 * 1024);

        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct BitmapBoundarySlot {
        _value: u64,
    }

    // SAFETY: `BitmapBoundarySlot` contains no managed edge. Its requested
    // stride gives classification fixtures multiple allocation words per run.
    unsafe impl Trace for BitmapBoundarySlot {
        const REQUESTED_SLOT_SIZE: Option<usize> = Some(512);

        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: `DropCounter` contains no managed edge.
    unsafe impl Trace for DropCounter {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct MutatorContextDrop {
        observed_active_mutator: Arc<Mutex<Vec<bool>>>,
    }

    impl Drop for MutatorContextDrop {
        fn drop(&mut self) {
            self.observed_active_mutator
                .lock()
                .unwrap()
                .push(thread_has_any_active_mutator());
        }
    }

    // SAFETY: this fixture contains only host observation state and has no
    // managed edge. It observes whether its Rust destructor runs inside an
    // already-established mutator region but cannot create one.
    unsafe impl Trace for MutatorContextDrop {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    static TERMINAL_DESTRUCTOR_TEST_LOCK: Mutex<()> = Mutex::new(());
    static TERMINAL_DESTRUCTOR_PANIC_ONCE: AtomicBool = AtomicBool::new(false);
    static TERMINAL_DESTRUCTOR_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

    struct TerminalPanickingDrop {
        _not_zero_sized: u8,
    }

    impl Drop for TerminalPanickingDrop {
        fn drop(&mut self) {
            TERMINAL_DESTRUCTOR_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            if TERMINAL_DESTRUCTOR_PANIC_ONCE.swap(false, Ordering::Relaxed) {
                panic!("injected terminal destructor panic");
            }
        }
    }

    // SAFETY: this fixture contains only immediate values and host
    // coordination state. It has no managed edge.
    unsafe impl Trace for TerminalPanickingDrop {
        const REQUESTED_SLOT_SIZE: Option<usize> = Some(4096);

        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    static TOPOLOGY_POISON_DESTRUCTOR_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    static FINALIZER_COMMIT_POISON_DESTRUCTOR_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

    struct PoisonedHeapDrop {
        attempts: &'static AtomicUsize,
    }

    impl Drop for PoisonedHeapDrop {
        fn drop(&mut self) {
            self.attempts.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: this fixture contains only a static host-observation reference
    // and reports its destructor through that instrumentation. It has no
    // managed edge.
    unsafe impl Trace for PoisonedHeapDrop {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct WideDropCounter(Arc<AtomicUsize>);

    impl Drop for WideDropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: `WideDropCounter` contains no managed edge. Its large total slot
    // request gives finalization fixtures one allocation per run.
    unsafe impl Trace for WideDropCounter {
        const REQUESTED_SLOT_SIZE: Option<usize> = Some(32 * 1024);

        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct AllocatingDrop {
        heap: Heap,
        drops: Arc<AtomicUsize>,
        published: Arc<Mutex<Option<crate::Root<u64>>>>,
        allocate_on_drop: bool,
        panic_after_drop: bool,
    }

    impl Drop for AllocatingDrop {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
            if self.allocate_on_drop {
                self.heap.with_mutator(|mutator| {
                    assert_eq!(
                        self.heap.inner.coordinator_snapshot().phase,
                        AdmissionPhase::Finalizing
                    );
                    assert_eq!(cache_snapshot(&self.heap.inner).unwrap().recursive_depth, 2);
                    let value = mutator.allocator::<u64>().unwrap().alloc(73);
                    let root = mutator.root(value);
                    assert!(self.published.lock().unwrap().replace(root).is_none());
                });
            }
            assert!(
                !self.panic_after_drop,
                "injected allocating finalizer panic"
            );
        }
    }

    // SAFETY: the heap handle and publication channel are ordinary Rust
    // resources, not managed edges. `AllocatingDrop` contains no `Gc<_>`.
    unsafe impl Trace for AllocatingDrop {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct IncrementalReleaseDrop {
        heap: std::sync::Weak<HeapInner>,
        drops: Arc<AtomicUsize>,
        allocate_on_drop: bool,
        published: mpsc::Sender<(crate::Root<IncrementalReleaseDrop>, RunLocation, usize)>,
    }

    impl Drop for IncrementalReleaseDrop {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
            if !self.allocate_on_drop {
                return;
            }

            let heap = Heap {
                inner: self
                    .heap
                    .upgrade()
                    .expect("finalizer test heap disappeared during Drop"),
            };
            heap.with_mutator(|mutator| {
                let replacement = mutator.allocator::<Self>().unwrap().alloc(Self {
                    heap: Arc::downgrade(&heap.inner),
                    drops: Arc::clone(&self.drops),
                    allocate_on_drop: false,
                    published: self.published.clone(),
                });
                let owner = collector_slot(&heap, replacement).owner;
                let root = mutator.root(replacement);
                self.published
                    .send((root, owner.location, owner.slot_index))
                    .expect("finalizer test receiver disappeared");
            });
        }
    }

    // SAFETY: this fixture contains only host coordination and immediate
    // fields. Its weak heap handle is not a managed edge, and its channel
    // publishes only a freshly allocated root to an external test owner.
    unsafe impl Trace for IncrementalReleaseDrop {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct ConcurrentReleaseDrop {
        drops: Arc<AtomicUsize>,
        pause_on_drop: bool,
        released_before: Arc<Barrier>,
        continue_finalization: Arc<Barrier>,
    }

    impl Drop for ConcurrentReleaseDrop {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
            if self.pause_on_drop {
                self.released_before.wait();
                self.continue_finalization.wait();
            }
        }
    }

    // SAFETY: this fixture contains only immediate and host synchronization
    // state. It has no managed edge and never attempts to inspect one in Drop.
    unsafe impl Trace for ConcurrentReleaseDrop {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct SelectivePanickingDrop {
        id: usize,
        panic: bool,
        events: Arc<Mutex<Vec<usize>>>,
    }

    impl Drop for SelectivePanickingDrop {
        fn drop(&mut self) {
            self.events.lock().unwrap().push(self.id);
            if self.panic {
                panic!("injected managed destructor panic");
            }
        }
    }

    // SAFETY: `SelectivePanickingDrop` contains no managed edge. The event log
    // is host coordination state and IDs and policy are immediate values.
    unsafe impl Trace for SelectivePanickingDrop {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    enum GraphEdges {
        Empty,
        One(crate::Gc<GraphNode>),
        Many(Vec<crate::Gc<GraphNode>>),
    }

    impl GraphEdges {
        fn from_vec(mut edges: Vec<crate::Gc<GraphNode>>) -> Self {
            match edges.len() {
                0 => Self::Empty,
                1 => Self::One(edges.pop().expect("single graph edge disappeared")),
                _ => Self::Many(edges),
            }
        }

        fn push(&mut self, target: crate::Gc<GraphNode>) {
            match self {
                Self::Empty => *self = Self::One(target),
                Self::One(first) => *self = Self::Many(vec![*first, target]),
                Self::Many(edges) => edges.push(target),
            }
        }

        fn len(&self) -> usize {
            match self {
                Self::Empty => 0,
                Self::One(_) => 1,
                Self::Many(edges) => edges.len(),
            }
        }

        fn is_empty(&self) -> bool {
            matches!(self, Self::Empty)
        }

        fn visit(&self, visitor: &mut Visitor<'_>) {
            match self {
                Self::Empty => {}
                Self::One(edge) => visitor.visit(*edge),
                Self::Many(edges) => {
                    for edge in edges.iter().copied() {
                        visitor.visit(edge);
                    }
                }
            }
        }
    }

    struct GraphNode {
        edges: Mutex<GraphEdges>,
        traces: Arc<AtomicUsize>,
    }

    // SAFETY: the mutex-protected edge representation contains every managed
    // edge in the node. Exclusive collection excludes mutator updates, and
    // tracing reports the complete synchronized edge snapshot without changing
    // it.
    unsafe impl Trace for GraphNode {
        fn trace(&self, visitor: &mut Visitor<'_>) {
            self.traces.fetch_add(1, Ordering::Relaxed);
            let edges = self.edges.lock().unwrap_or_else(|poison| {
                self.edges.clear_poison();
                poison.into_inner()
            });
            edges.visit(visitor);
        }
    }

    fn graph_node(traces: Arc<AtomicUsize>) -> GraphNode {
        GraphNode {
            edges: Mutex::new(GraphEdges::Empty),
            traces,
        }
    }

    fn graph_node_with_edges(
        traces: Arc<AtomicUsize>,
        edges: Vec<crate::Gc<GraphNode>>,
    ) -> GraphNode {
        GraphNode {
            edges: Mutex::new(GraphEdges::from_vec(edges)),
            traces,
        }
    }

    fn graph_node_with_edge(traces: Arc<AtomicUsize>, edge: crate::Gc<GraphNode>) -> GraphNode {
        GraphNode {
            edges: Mutex::new(GraphEdges::One(edge)),
            traces,
        }
    }

    struct TestRng(u64);

    impl TestRng {
        fn new(seed: u64) -> Self {
            assert_ne!(seed, 0, "test RNG seed must be nonzero");
            Self(seed)
        }

        fn next(&mut self) -> u64 {
            let mut value = self.0;
            value ^= value << 13;
            value ^= value >> 7;
            value ^= value << 17;
            self.0 = value;
            value
        }

        fn index(&mut self, upper_bound: usize) -> usize {
            assert_ne!(upper_bound, 0);
            (self.next() % upper_bound as u64) as usize
        }
    }

    fn reference_reachability(adjacency: &[Vec<usize>], roots: &[usize]) -> Vec<bool> {
        let mut reachable = vec![false; adjacency.len()];
        let mut worklist = roots.to_vec();
        while let Some(node) = worklist.pop() {
            if std::mem::replace(&mut reachable[node], true) {
                continue;
            }
            worklist.extend(adjacency[node].iter().copied());
        }
        reachable
    }

    fn connect_graph_nodes(
        mutator: &crate::Mutator<'_>,
        owner: crate::Gc<GraphNode>,
        target: crate::Gc<GraphNode>,
    ) {
        // SAFETY: both nodes were allocated in this admitted mutator's heap.
        // The closure appends the one reported edge and leaves the node valid
        // if vector growth panics before publication.
        unsafe {
            mutator.with_edge_replacement(owner, None, Some(target), || {
                owner
                    .get_unchecked(mutator)
                    .edges
                    .lock()
                    .expect("graph-node edges should not be poisoned")
                    .push(target);
            });
        }
    }

    struct PanickingTraceNode {
        edges: Vec<crate::Gc<PanickingTraceNode>>,
        panic_after_edges: Option<usize>,
        armed: Arc<AtomicBool>,
        traces: Arc<AtomicUsize>,
    }

    // SAFETY: `edges` contains every managed edge in this immutable test
    // representation. The injected panic leaves the value unchanged and safe
    // to trace again from the beginning, as required by `Trace`.
    unsafe impl Trace for PanickingTraceNode {
        fn trace(&self, visitor: &mut Visitor<'_>) {
            self.traces.fetch_add(1, Ordering::Relaxed);
            for (index, edge) in self.edges.iter().copied().enumerate() {
                if self.armed.load(Ordering::Acquire) && self.panic_after_edges == Some(index) {
                    panic!("injected trace panic after {index} reported edges");
                }
                visitor.visit(edge);
            }
            if self.armed.load(Ordering::Acquire)
                && self.panic_after_edges == Some(self.edges.len())
            {
                panic!(
                    "injected trace panic after {} reported edges",
                    self.edges.len()
                );
            }
        }
    }

    struct InvalidEdgeHolder {
        edge: crate::Gc<GraphNode>,
        traces: Arc<AtomicUsize>,
    }

    // SAFETY: the holder reports its only represented managed edge. Individual
    // C5C fixtures deliberately violate that edge's same-heap/live-slot
    // invariant so checked discovery can prove it rejects the pointer before
    // unsafe trace dispatch.
    unsafe impl Trace for InvalidEdgeHolder {
        fn trace(&self, visitor: &mut Visitor<'_>) {
            self.traces.fetch_add(1, Ordering::Relaxed);
            visitor.visit(self.edge);
        }
    }

    fn invalid_edge_holder(
        heap: &Heap,
        edge: crate::Gc<GraphNode>,
        traces: Arc<AtomicUsize>,
    ) -> crate::Root<InvalidEdgeHolder> {
        heap.with_mutator(|mutator| {
            let holder = mutator
                .allocator::<InvalidEdgeHolder>()
                .unwrap()
                .alloc(InvalidEdgeHolder { edge, traces });
            mutator.root(holder)
        })
    }

    fn assert_reachable_invalid_edge_repeats(
        heap: &Heap,
        expected_message: &str,
        traces: &AtomicUsize,
    ) {
        for expected_traces in 1..=2 {
            let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
                .expect_err("reachable invalid collector edge must panic");
            assert_eq!(panic_string(panic.as_ref()), expected_message);
            assert_eq!(traces.load(Ordering::Relaxed), expected_traces);
            assert_failed_collection_restored(heap);
        }
    }

    fn test_cursor(class_id: AllocationClassId) -> AllocationCursor {
        AllocationCursor {
            class_id,
            location: RunLocation { chunk: 0, run: 0 },
            run: crate::arena::RunAddress::dangling_for_cache_test(),
            geometry: RunGeometry::derive(std::alloc::Layout::new::<u64>(), None).unwrap(),
            word_index: 0,
            free_mask: u64::MAX,
        }
    }

    // SAFETY: `OverflowingSlot` contains no managed edge.
    unsafe impl Trace for OverflowingSlot {
        const REQUESTED_SLOT_SIZE: Option<usize> = Some(usize::MAX);

        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    #[test]
    fn heap_owned_arenas_reject_another_heaps_addresses() {
        let first = Heap::new();
        let second = Heap::new();

        let address = {
            let mut arena = first
                .inner
                .data
                .lock()
                .expect("test arena should not be poisoned");
            let chunk = arena.arena.reserve_chunk().unwrap();
            arena.arena.run_address(chunk, 0).unwrap().address()
        };

        assert!(
            first
                .inner
                .data
                .lock()
                .unwrap()
                .arena
                .find_run(address)
                .is_some()
        );
        assert!(
            second
                .inner
                .data
                .lock()
                .unwrap()
                .arena
                .find_run(address)
                .is_none()
        );
    }

    #[test]
    fn repeated_and_concurrent_class_discovery_returns_one_heap_local_class() {
        const THREADS: usize = 12;

        let heap = Heap::new();
        let expected = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<FirstType>().unwrap();
            (
                allocator.id(),
                allocator.metadata() as *const _ as usize,
                allocator.belongs_to(&heap.inner),
            )
        });
        let identities = (0..THREADS)
            .map(|_| {
                let heap = heap.clone();
                std::thread::spawn(move || {
                    heap.with_mutator(|mutator| {
                        let allocator = mutator.allocator::<FirstType>().unwrap();
                        (
                            allocator.id(),
                            allocator.metadata() as *const _ as usize,
                            allocator.belongs_to(&heap.inner),
                        )
                    })
                })
            })
            .map(|thread| thread.join().expect("class-discovery worker panicked"))
            .collect::<Vec<_>>();

        assert!(identities.iter().all(|identity| *identity == expected));
        assert!(expected.2);
        assert_eq!(heap.inner.data.lock().unwrap().classes.len(), 1);
    }

    #[test]
    fn class_discovery_waits_for_synthetic_exclusive_admission() {
        let heap = Heap::new();
        let exclusive = heap.inner.enter_synthetic_exclusive();
        let worker = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.with_mutator(|mutator| {
                    let allocator = mutator.allocator::<FirstType>().unwrap();
                    (allocator.id(), allocator.belongs_to(&heap.inner))
                })
            }
        });

        heap.inner.wait_for_blocked_outer_mutators(1);
        let blocked = heap.inner.coordinator_snapshot();
        assert_eq!(blocked.phase, AdmissionPhase::Exclusive);
        assert_eq!(blocked.active_outer_mutators, 0);
        assert_eq!(blocked.blocked_outer_mutators, 1);

        drop(exclusive);
        let (class_id, belongs_to_heap) = worker.join().expect("class-discovery worker panicked");
        assert!(belongs_to_heap);
        assert_eq!(class_id.get(), 1);
        assert_eq!(heap.inner.data.lock().unwrap().classes.len(), 1);
    }

    #[test]
    fn repeated_scoped_allocator_discovery_reuses_the_heap_class() {
        let heap = Heap::new();
        let first_id = heap.with_mutator(|mutator| mutator.allocator::<FirstType>().unwrap().id());
        assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 0);

        let (second_id, metadata, value) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<FirstType>().unwrap();
            (
                allocator.id(),
                allocator.metadata(),
                allocator.alloc(FirstType { _value: 41 }),
            )
        });
        let resolved = heap
            .inner
            .resolve_slot(value.erase().as_ptr().as_ptr() as usize)
            .expect("scoped allocator must allocate into its mutator heap");
        assert_eq!(first_id, second_id);
        assert_eq!(resolved.class_id, second_id);
        assert!(std::ptr::eq(resolved.metadata, metadata));
    }

    #[test]
    fn recursive_class_discovery_reuses_admission_while_collection_is_requested() {
        let heap = Heap::new();

        let class_id = heap.with_mutator(|_| {
            heap.request_collection();
            let class_id =
                heap.with_mutator(|mutator| mutator.allocator::<SecondType>().unwrap().id());
            assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 1);
            assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 1);
            assert!(heap.inner.coordinator_snapshot().collection_requested);
            class_id
        });

        assert_eq!(class_id.get(), 1);
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            0
        );
        heap.with_mutator(|_| {});
        assert_eq!(
            heap.inner.coordinator_snapshot().phase,
            AdmissionPhase::Ordinary
        );
    }

    #[test]
    fn prepared_entry_is_inert_until_admission_activates_it() {
        let heap = Heap::new();

        heap.with_mutator_admission_hook(
            || {
                let coordinator = heap.inner.coordinator_snapshot();
                assert_eq!(coordinator.phase, AdmissionPhase::Ordinary);
                assert_eq!(coordinator.active_outer_mutators, 1);
                assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 0);
            },
            |_| {
                assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 1);
                assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 1);
            },
        );

        assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 0);
        assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 0);
    }

    #[test]
    fn panic_between_admission_and_activation_rolls_back_cleanly() {
        let heap = Heap::new();

        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.with_mutator_admission_hook(
                || panic!("injected post-admission panic"),
                |_| unreachable!(),
            );
        }));

        assert!(panic.is_err());
        assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 0);
        assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 0);
        heap.with_mutator(|_| {});
    }

    #[test]
    fn synthetic_exclusive_waits_for_mutator_release_and_observes_prior_work() {
        let heap = Heap::new();
        let published = Arc::new(AtomicUsize::new(0));
        let (active_tx, active_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn({
            let heap = heap.clone();
            let published = Arc::clone(&published);
            move || {
                heap.with_mutator(|_| {
                    published.store(73, Ordering::Relaxed);
                    active_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                });
            }
        });

        active_rx.recv().unwrap();
        assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 1);
        let observer = std::thread::spawn({
            let heap = heap.clone();
            let published = Arc::clone(&published);
            move || {
                let exclusive = heap.inner.enter_synthetic_exclusive();
                let observed = published.load(Ordering::Relaxed);
                drop(exclusive);
                observed
            }
        });

        release_tx.send(()).unwrap();
        worker.join().expect("mutator worker panicked");
        assert_eq!(observer.join().expect("exclusive observer panicked"), 73);
        assert_eq!(
            heap.inner.coordinator_snapshot().phase,
            AdmissionPhase::Ordinary
        );
    }

    #[test]
    fn requested_collection_does_not_block_fresh_entry_while_heap_is_active() {
        let heap = Heap::new();
        let (active_tx, active_rx) = mpsc::channel();
        let (release_active_tx, release_active_rx) = mpsc::channel();
        let active = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.with_mutator(|_| {
                    active_tx.send(()).unwrap();
                    release_active_rx.recv().unwrap();
                });
            }
        });
        active_rx.recv().unwrap();
        heap.request_collection();

        let (entered_tx, entered_rx) = mpsc::channel();
        let entrant = std::thread::spawn({
            let heap = heap.clone();
            move || heap.with_mutator(|_| entered_tx.send(()).unwrap())
        });
        entered_rx.recv().unwrap();
        entrant.join().expect("blocked entrant panicked");

        let requested = heap.inner.coordinator_snapshot();
        assert!(requested.collection_requested);
        assert_eq!(requested.completed_collection_epoch, 0);
        assert_eq!(requested.active_outer_mutators, 1);

        release_active_tx.send(()).unwrap();
        active.join().expect("active mutator panicked");
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            0
        );

        heap.with_mutator(|_| {});
        assert_eq!(
            heap.inner.coordinator_snapshot().phase,
            AdmissionPhase::Ordinary
        );
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            1
        );
    }

    #[test]
    fn panicking_synthetic_exclusive_restores_ordinary_admission() {
        let heap = Heap::new();

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _exclusive = heap.inner.enter_synthetic_exclusive();
            panic!("injected exclusive panic");
        }));

        assert!(panic.is_err());
        assert_eq!(
            heap.inner.coordinator_snapshot().phase,
            AdmissionPhase::Ordinary
        );
        heap.with_mutator(|_| {});
    }

    #[test]
    fn explicit_request_is_nonblocking_and_serviced_before_outer_entry() {
        let heap = Heap::new();

        heap.request_collection();
        let requested = heap.inner.coordinator_snapshot();
        assert_eq!(requested.phase, AdmissionPhase::Ordinary);
        assert!(requested.collection_requested);
        assert_eq!(requested.completed_collection_epoch, 0);

        heap.with_mutator(|_| {
            assert_eq!(
                heap.inner.coordinator_snapshot().completed_collection_epoch,
                1
            );
            assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 1);
        });

        let completed = heap.inner.coordinator_snapshot();
        assert_eq!(completed.phase, AdmissionPhase::Ordinary);
        assert!(!completed.collection_requested);
        assert_eq!(completed.completed_collection_epoch, 1);
        assert_eq!(
            completed
                .latest_collection_report
                .expect("entry-elected collection must publish a report")
                .epoch(),
            1
        );
    }

    #[test]
    fn explicit_request_neither_waits_for_managed_data_nor_notifies_waiters() {
        let heap = Heap::new();
        let data = heap.inner.data.lock().unwrap();
        let notifications = heap.inner.coordinator_notification_count();
        let (returned_tx, returned_rx) = mpsc::channel();
        let requester = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.request_collection();
                returned_tx.send(()).unwrap();
            }
        });

        returned_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("request_collection blocked on managed data");
        assert!(heap.inner.collection_requested.load(Ordering::Acquire));
        assert_eq!(
            heap.inner.coordinator_notification_count(),
            notifications,
            "a request-only transition must not notify coordinator waiters"
        );

        drop(data);
        requester.join().unwrap();
    }

    #[test]
    fn request_and_pressure_before_data_acknowledgement_are_coalesced() {
        let heap = Heap::new();
        let class = internal_class::<FirstType>(&heap);
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();

        assert!(
            heap.inner
                .run_synthetic_collection(
                    epoch,
                    false,
                    |_, _| {},
                    |_| {
                        for _ in 0..FIXED_SURVIVOR_RUN_HEADROOM {
                            heap.inner.prepare_run(&class).unwrap();
                        }
                        heap.request_collection();
                        assert!(heap.inner.allocation_pressure().collection_requested);
                    },
                )
                .is_none()
        );

        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressureSnapshot {
                assigned_runs: FIXED_SURVIVOR_RUN_HEADROOM,
                high_water_mark: survivor_run_high_water_mark(
                    FIXED_SURVIVOR_RUN_HEADROOM,
                    SURVIVOR_GROWTH_NUMERATOR,
                    SURVIVOR_GROWTH_DENOMINATOR,
                ),
                collection_requested: false,
            }
        );
        assert!(!heap.inner.coordinator_snapshot().collection_requested);
    }

    #[test]
    fn external_request_after_data_acknowledgement_remains_pending() {
        let heap = Heap::new();
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        let arrived = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        heap.inner
            .install_collection_acknowledgement_hook(Arc::clone(&arrived), Arc::clone(&release));
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.inner
                    .run_synthetic_collection(epoch, false, |_, _| {}, |_| {})
                    .is_none()
            }
        });

        arrived.wait();
        assert!(!heap.inner.collection_requested.load(Ordering::Acquire));
        let unpublished = heap.inner.coordinator_snapshot();
        assert_eq!(unpublished.completed_collection_epoch, 0);
        assert_eq!(unpublished.latest_collection_report, None);
        heap.request_collection();
        release.wait();
        assert!(collector.join().unwrap());
        heap.inner.clear_collection_acknowledgement_hook();

        let completed = heap.inner.coordinator_snapshot();
        assert_eq!(completed.completed_collection_epoch, 1);
        assert_eq!(
            completed
                .latest_collection_report
                .expect("completed epoch must publish its report")
                .epoch(),
            1
        );
        assert!(completed.collection_requested);
        heap.with_mutator(|_| {});
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            2
        );
    }

    #[test]
    fn entry_root_and_pressure_after_data_acknowledgement_survive_completion() {
        let heap = Heap::new();
        let class = internal_class::<FirstType>(&heap);
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        let arrived = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        heap.inner
            .install_collection_acknowledgement_hook(Arc::clone(&arrived), Arc::clone(&release));
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.inner
                    .run_synthetic_collection(epoch, false, |_, _| {}, |_| {})
                    .is_none()
            }
        });

        arrived.wait();
        let root = heap.with_mutator(|mutator| {
            let value = mutator.allocator::<u64>().unwrap().alloc(42);
            let root = mutator.root(value);
            for _ in 0..FIXED_SURVIVOR_RUN_HEADROOM {
                heap.inner.prepare_run(&class).unwrap();
            }
            root
        });
        assert_eq!(heap.inner.data.lock().unwrap().roots.len(), 1);
        assert!(heap.inner.allocation_pressure().collection_requested);
        release.wait();
        assert!(collector.join().unwrap());
        heap.inner.clear_collection_acknowledgement_hook();

        let completed = heap.inner.coordinator_snapshot();
        assert_eq!(completed.completed_collection_epoch, 1);
        assert!(completed.collection_requested);
        heap.with_mutator(|mutator| assert_eq!(*root.get(mutator), 42));
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            2
        );
    }

    #[test]
    fn recursive_request_survives_outer_exit_until_the_next_entry() {
        let heap = Heap::new();

        heap.with_mutator(|_| {
            heap.with_mutator(|_| heap.request_collection());
            let pending = heap.inner.coordinator_snapshot();
            assert!(pending.collection_requested);
            assert_eq!(pending.completed_collection_epoch, 0);
            assert_eq!(pending.active_outer_mutators, 1);
        });

        let pending = heap.inner.coordinator_snapshot();
        assert!(pending.collection_requested);
        assert_eq!(pending.completed_collection_epoch, 0);
        assert_eq!(pending.active_outer_mutators, 0);

        heap.with_mutator(|_| {
            assert_eq!(
                heap.inner.coordinator_snapshot().completed_collection_epoch,
                1
            );
        });
    }

    #[test]
    fn entry_elected_collection_hands_admission_directly_to_the_mutator() {
        let heap = Heap::new();
        heap.request_collection();

        heap.with_mutator_admission_hook(
            || {
                let coordinator = heap.inner.coordinator_snapshot();
                assert_eq!(coordinator.phase, AdmissionPhase::Ordinary);
                assert_eq!(coordinator.completed_collection_epoch, 1);
                assert_eq!(coordinator.active_outer_mutators, 1);
                assert!(!coordinator.collection_requested);
                assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 0);
            },
            |_| {
                let coordinator = heap.inner.coordinator_snapshot();
                assert_eq!(coordinator.completed_collection_epoch, 1);
                assert_eq!(coordinator.active_outer_mutators, 1);
                assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 1);
            },
        );

        assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 0);
    }

    #[test]
    fn entry_elected_collection_clears_its_inactive_cursor_cache() {
        let heap = Heap::new();
        heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<FirstType>().unwrap();
            let _ = allocator.alloc(FirstType { _value: 1 });
        });
        assert_eq!(cache_snapshot(&heap.inner).unwrap().cursor_count, 1);

        heap.request_collection();
        heap.with_mutator_admission_hook(
            || {
                let cache = cache_snapshot(&heap.inner).unwrap();
                assert_eq!(cache.recursive_depth, 0);
                assert_eq!(cache.cursor_count, 0);
            },
            |mutator| {
                let allocator = mutator.allocator::<FirstType>().unwrap();
                let _ = allocator.alloc(FirstType { _value: 2 });
                assert_eq!(cache_snapshot(&heap.inner).unwrap().cursor_count, 1);
            },
        );
    }

    #[test]
    fn concurrent_idle_entries_elect_exactly_one_requested_collection() {
        const ENTRANTS: usize = 8;

        let heap = Heap::new();
        heap.request_collection();
        let start = Arc::new(Barrier::new(ENTRANTS + 1));
        let entrants = (0..ENTRANTS)
            .map(|_| {
                let heap = heap.clone();
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    heap.with_mutator(|_| {})
                })
            })
            .collect::<Vec<_>>();

        start.wait();
        for entrant in entrants {
            entrant.join().expect("outer entrant panicked");
        }
        let completed = heap.inner.coordinator_snapshot();
        assert_eq!(completed.completed_collection_epoch, 1);
        assert_eq!(completed.active_outer_mutators, 0);
        assert!(!completed.collection_requested);
    }

    #[test]
    fn request_after_collection_completion_remains_pending() {
        let heap = Heap::new();
        heap.request_collection();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let entrant = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.with_mutator(|_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                });
            }
        });

        entered_rx.recv().unwrap();
        let completed = heap.inner.coordinator_snapshot();
        assert_eq!(completed.completed_collection_epoch, 1);
        assert!(!completed.collection_requested);
        heap.request_collection();
        assert!(heap.inner.coordinator_snapshot().collection_requested);

        release_tx.send(()).unwrap();
        entrant.join().unwrap();
        let pending = heap.inner.coordinator_snapshot();
        assert_eq!(pending.completed_collection_epoch, 1);
        assert!(pending.collection_requested);

        heap.with_mutator(|_| {});
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            2
        );
    }

    #[test]
    fn synchronous_collection_rejects_any_same_thread_active_mutator() {
        let heap = Heap::new();
        let other = Heap::new();

        heap.with_mutator(|_| {
            assert_eq!(
                heap.collect_full(),
                Err(super::CollectionError::ActiveMutator)
            );
            let coordinator = heap.inner.coordinator_snapshot();
            assert!(!coordinator.collection_requested);
            assert_eq!(coordinator.phase, AdmissionPhase::Ordinary);
            assert_eq!(coordinator.active_outer_mutators, 1);
        });

        other.with_mutator(|_| {
            assert_eq!(
                heap.collect_full(),
                Err(super::CollectionError::ActiveMutator)
            );
            let coordinator = heap.inner.coordinator_snapshot();
            assert!(!coordinator.collection_requested);
            assert_eq!(coordinator.completed_collection_epoch, 0);
        });
    }

    #[test]
    fn synchronous_collection_waits_without_blocking_a_fresh_entrant() {
        let heap = Heap::new();
        let (active_tx, active_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let active = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.with_mutator(|_| {
                    active_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                });
            }
        });
        active_rx.recv().unwrap();

        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || heap.collect_full().unwrap()
        });
        heap.inner.wait_for_collection_waiters(1);

        let (entered_tx, entered_rx) = mpsc::channel();
        let entrant = std::thread::spawn({
            let heap = heap.clone();
            move || heap.with_mutator(|_| entered_tx.send(()).unwrap())
        });
        entered_rx.recv().unwrap();
        entrant.join().unwrap();
        assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 1);

        release_tx.send(()).unwrap();
        active.join().unwrap();
        assert_eq!(collector.join().unwrap().epoch(), 1);
    }

    #[test]
    fn synchronous_requesters_coalesce_on_one_idle_collection() {
        let heap = Heap::new();
        let root = heap.with_mutator(|mutator| {
            let value = mutator.allocator::<u64>().unwrap().alloc(42);
            mutator.root(value)
        });
        let (active_tx, active_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let active = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.with_mutator(|_| {
                    active_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
            }
        });
        active_rx.recv().unwrap();

        let first = std::thread::spawn({
            let heap = heap.clone();
            move || heap.collect_full().unwrap()
        });
        heap.inner.wait_for_collection_waiters(1);
        let second = std::thread::spawn({
            let heap = heap.clone();
            move || heap.collect_full().unwrap()
        });
        heap.inner.wait_for_collection_waiters(2);

        release_tx.send(()).unwrap();
        active.join().unwrap();
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert_eq!(first, second);
        assert_eq!(first.epoch(), 1);
        assert_eq!(first.root_entries(), 1);
        assert_eq!(first.traced_objects(), 1);
        assert_eq!(first.marked_slots(), 1);
        assert_eq!(first.conservatively_retained_slots(), 0);
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            1
        );
        heap.with_mutator(|mutator| assert_eq!(*root.get(mutator), 42));
    }

    #[test]
    fn synchronous_request_joins_an_already_exclusive_collection() {
        let heap = Heap::new();
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        let (exclusive_tx, exclusive_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.inner
                    .run_synthetic_collection(
                        epoch,
                        false,
                        |_, _| {
                            exclusive_tx.send(()).unwrap();
                            release_rx.recv().unwrap();
                        },
                        |_| {},
                    )
                    .is_none()
            }
        });
        exclusive_rx.recv().unwrap();

        let waiter = std::thread::spawn({
            let heap = heap.clone();
            move || heap.collect_full().unwrap()
        });
        heap.inner.wait_for_collection_waiters(1);
        release_tx.send(()).unwrap();

        assert!(collector.join().unwrap());
        assert_eq!(waiter.join().unwrap().epoch(), 1);
        let completed = heap.inner.coordinator_snapshot();
        assert_eq!(completed.completed_collection_epoch, 1);
        assert!(!completed.collection_requested);
    }

    #[test]
    fn synchronous_request_joins_while_active_collection_waits_for_managed_data() {
        let heap = Heap::new();
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        let data = heap.inner.data.lock().unwrap();
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.inner
                    .run_synthetic_collection(epoch, false, |_, _| {}, |_| {})
                    .is_none()
            }
        });
        let waiter = std::thread::spawn({
            let heap = heap.clone();
            move || heap.collect_full().unwrap()
        });

        heap.inner.wait_for_collection_waiters(1);
        let active = heap.inner.coordinator_snapshot();
        assert_eq!(active.active_collection, Some(epoch));
        assert_eq!(active.phase, AdmissionPhase::Exclusive);
        drop(data);

        assert!(collector.join().unwrap());
        assert_eq!(waiter.join().unwrap().epoch(), epoch.get());
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            epoch.get()
        );
    }

    #[test]
    fn sequential_synchronous_collections_report_monotonic_epochs() {
        let heap = Heap::new();

        assert_eq!(heap.collect_full().unwrap().epoch(), 1);
        assert_eq!(heap.collect_full().unwrap().epoch(), 2);
    }

    #[test]
    fn dropping_the_original_heap_handle_does_not_strand_collection_waiters() {
        let heap = Heap::new();
        let (active_tx, active_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let active = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.with_mutator(|_| {
                    active_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
            }
        });
        active_rx.recv().unwrap();
        let waiter = std::thread::spawn({
            let heap = heap.clone();
            move || heap.collect_full().unwrap()
        });
        heap.inner.wait_for_collection_waiters(1);

        drop(heap);
        release_tx.send(()).unwrap();
        active.join().unwrap();
        assert_eq!(waiter.join().unwrap().epoch(), 1);
    }

    #[test]
    fn reciprocal_nested_entries_pass_two_uncommitted_collection_requests() {
        let first = Heap::new();
        let second = Heap::new();
        let active = Arc::new(Barrier::new(3));
        let enter_nested = Arc::new(Barrier::new(3));
        let nested_entries = Arc::new(AtomicUsize::new(0));

        let first_then_second = std::thread::spawn({
            let first = first.clone();
            let second = second.clone();
            let active = Arc::clone(&active);
            let enter_nested = Arc::clone(&enter_nested);
            let nested_entries = Arc::clone(&nested_entries);
            move || {
                first.with_mutator(|_| {
                    active.wait();
                    enter_nested.wait();
                    second.with_mutator(|_| {
                        nested_entries.fetch_add(1, Ordering::Relaxed);
                    });
                });
            }
        });
        let second_then_first = std::thread::spawn({
            let first = first.clone();
            let second = second.clone();
            let active = Arc::clone(&active);
            let enter_nested = Arc::clone(&enter_nested);
            let nested_entries = Arc::clone(&nested_entries);
            move || {
                second.with_mutator(|_| {
                    active.wait();
                    enter_nested.wait();
                    first.with_mutator(|_| {
                        nested_entries.fetch_add(1, Ordering::Relaxed);
                    });
                });
            }
        });

        active.wait();
        let first_collector = std::thread::spawn({
            let first = first.clone();
            move || first.collect_full().unwrap()
        });
        let second_collector = std::thread::spawn({
            let second = second.clone();
            move || second.collect_full().unwrap()
        });
        first.inner.wait_for_collection_waiters(1);
        second.inner.wait_for_collection_waiters(1);

        enter_nested.wait();
        first_then_second.join().unwrap();
        second_then_first.join().unwrap();
        assert_eq!(nested_entries.load(Ordering::Relaxed), 2);
        assert_eq!(first_collector.join().unwrap().epoch(), 1);
        assert_eq!(second_collector.join().unwrap().epoch(), 1);
    }

    #[test]
    fn nested_heap_request_waits_for_a_later_nested_heap_entry() {
        let outer = Heap::new();
        let nested = Heap::new();

        outer.with_mutator(|_| {
            nested.with_mutator(|_| nested.request_collection());
            let pending = nested.inner.coordinator_snapshot();
            assert!(pending.collection_requested);
            assert_eq!(pending.completed_collection_epoch, 0);
        });

        let pending = nested.inner.coordinator_snapshot();
        assert!(pending.collection_requested);
        assert_eq!(pending.completed_collection_epoch, 0);

        nested.with_mutator(|_| {});
        let completed = nested.inner.coordinator_snapshot();
        assert!(!completed.collection_requested);
        assert_eq!(completed.completed_collection_epoch, 1);
    }

    #[test]
    fn caught_nested_unwind_preserves_request_for_a_later_entry() {
        let outer = Heap::new();
        let nested = Heap::new();

        outer.with_mutator(|_| {
            let panic = catch_unwind(AssertUnwindSafe(|| {
                nested.with_mutator(|_| {
                    nested.request_collection();
                    panic!("injected nested unwind");
                });
            }));
            assert!(panic.is_err());
            assert_eq!(
                nested
                    .inner
                    .coordinator_snapshot()
                    .completed_collection_epoch,
                0
            );
        });

        let pending = nested.inner.coordinator_snapshot();
        assert!(pending.collection_requested);
        assert_eq!(pending.completed_collection_epoch, 0);
        nested.with_mutator(|_| {});
        assert_eq!(
            nested
                .inner
                .coordinator_snapshot()
                .completed_collection_epoch,
            1
        );
    }

    #[test]
    fn cross_heap_entry_waits_while_the_target_collector_is_exclusive() {
        let held = Heap::new();
        let target = Heap::new();
        let (exclusive_tx, exclusive_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let collector = std::thread::spawn({
            let target = target.clone();
            move || {
                let exclusive = target.inner.enter_synthetic_exclusive();
                exclusive_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                drop(exclusive);
            }
        });
        exclusive_rx.recv().unwrap();

        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let entrant = std::thread::spawn({
            let held = held.clone();
            let target = target.clone();
            move || {
                held.with_mutator(|_| {
                    attempt_tx.send(()).unwrap();
                    target.with_mutator(|_| entered_tx.send(()).unwrap());
                });
            }
        });
        attempt_rx.recv().unwrap();
        target.inner.wait_for_blocked_outer_mutators(1);
        assert!(entered_rx.try_recv().is_err());

        release_tx.send(()).unwrap();
        collector.join().unwrap();
        entrant.join().unwrap();
        entered_rx.recv().unwrap();
    }

    #[test]
    fn collector_marks_cross_word_boundaries_and_duplicate_marks_are_inert() {
        let heap = Heap::new();
        let values = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u32>().unwrap();
            (0..65_u32)
                .map(|value| allocator.alloc(value))
                .collect::<Vec<_>>()
        });
        let boundary_values = [values[0], values[63], values[64]];
        let boundary_slots = boundary_values.map(|value| collector_slot(&heap, value));
        assert_eq!(
            boundary_slots.map(|slot| slot.owner.slot_index),
            [0, 63, 64]
        );

        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        assert!(
            heap.inner
                .run_synthetic_collection_with_mark_work(
                    epoch,
                    false,
                    |attempt, data| {
                        for value in boundary_values {
                            assert!(attempt.discover(data, value.erase()).unwrap());
                            assert!(!attempt.discover(data, value.erase()).unwrap());
                        }
                        assert_eq!(attempt.marked_slot_count, 3);
                        assert_eq!(attempt.worklist.len(), 3);
                    },
                    |_, _| {},
                    |_| {},
                )
                .is_none()
        );

        assert!(
            boundary_slots
                .into_iter()
                .all(|slot| slot_is_marked(&heap, slot))
        );
    }

    #[test]
    fn collection_clears_zero_one_and_many_assigned_mark_ranges() {
        let empty = Heap::new();
        empty.request_collection();
        let epoch = empty.inner.elect_idle_collection_for_test();
        assert!(
            empty
                .inner
                .run_synthetic_collection_with_mark_work(
                    epoch,
                    false,
                    |_, data| assert_eq!(data.clear_mark_bitmaps(), 0),
                    |_, _| {},
                    |_| {},
                )
                .is_none()
        );

        let one = Heap::new();
        let one_value = allocate(&one, 1_u64);
        let one_slot = collector_slot(&one, one_value);
        one.request_collection();
        let dirty_epoch = one.inner.elect_idle_collection_for_test();
        assert!(
            one.inner
                .run_synthetic_collection_with_mark_work(
                    dirty_epoch,
                    false,
                    |attempt, data| assert!(attempt.discover(data, one_value.erase()).unwrap()),
                    |_, _| {},
                    |_| {},
                )
                .is_none()
        );
        assert!(slot_is_marked(&one, one_slot));
        one.request_collection();
        let clear_epoch = one.inner.elect_idle_collection_for_test();
        assert!(
            one.inner
                .run_synthetic_collection_with_mark_work(
                    clear_epoch,
                    false,
                    |_, data| assert_eq!(data.clear_mark_bitmaps(), 1),
                    |_, _| {},
                    |_| {},
                )
                .is_none()
        );
        assert!(!slot_is_marked(&one, one_slot));

        let heap = Heap::new();
        let values = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<WideSlot>().unwrap();
            (0..3_u64)
                .map(|value| allocator.alloc(WideSlot { value }))
                .collect::<Vec<_>>()
        });
        let slots = values
            .iter()
            .copied()
            .map(|value| collector_slot(&heap, value))
            .collect::<Vec<_>>();
        assert_eq!(
            slots
                .iter()
                .map(|slot| slot.owner.location)
                .collect::<HashSet<_>>()
                .len(),
            3,
            "wide fixtures must occupy distinct assigned runs"
        );

        heap.request_collection();
        let first = heap.inner.elect_idle_collection_for_test();
        assert!(
            heap.inner
                .run_synthetic_collection_with_mark_work(
                    first,
                    false,
                    |attempt, data| {
                        for value in &values {
                            assert!(attempt.discover(data, value.erase()).unwrap());
                        }
                    },
                    |_, _| {},
                    |_| {},
                )
                .is_none()
        );
        assert!(
            slots
                .iter()
                .copied()
                .all(|slot| slot_is_marked(&heap, slot))
        );

        heap.request_collection();
        let second = heap.inner.elect_idle_collection_for_test();
        assert!(
            heap.inner
                .run_synthetic_collection_with_mark_work(
                    second,
                    false,
                    |_, data| assert_eq!(data.clear_mark_bitmaps(), 3),
                    |_, _| {},
                    |_| {},
                )
                .is_none()
        );
        assert!(
            slots
                .iter()
                .copied()
                .all(|slot| !slot_is_marked(&heap, slot))
        );
    }

    #[test]
    fn mutator_allocation_neither_reads_nor_writes_mark_state() {
        let heap = Heap::new();
        let marked = allocate(&heap, 1_u64);
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        assert!(
            heap.inner
                .run_synthetic_collection_with_mark_work(
                    epoch,
                    false,
                    |attempt, data| assert!(attempt.discover(data, marked.erase()).unwrap()),
                    |_, _| {},
                    |_| {},
                )
                .is_none()
        );
        let marked_slot = collector_slot(&heap, marked);
        assert!(slot_is_marked(&heap, marked_slot));

        let unmarked = allocate(&heap, 2_u64);
        let unmarked_slot = collector_slot(&heap, unmarked);
        assert!(slot_is_marked(&heap, marked_slot));
        assert!(!slot_is_marked(&heap, unmarked_slot));
    }

    #[test]
    fn collector_lookup_recovers_exact_owner_and_canonical_metadata() {
        let heap = Heap::new();
        let value = allocate(&heap, 42_u64);
        let slot = collector_slot(&heap, value);

        assert!(std::ptr::eq(slot.metadata, metadata_for::<u64>()));
        assert_eq!(slot.owner.class_id, internal_class::<u64>(&heap).id());
        assert_eq!(slot.owner.slot_index, 0);
        assert_eq!(
            value.erase().as_ptr().as_ptr() as usize,
            slot.owner.run.address()
                + slot
                    .owner
                    .geometry
                    .slot_offset(slot.owner.slot_index)
                    .unwrap()
        );
    }

    #[test]
    fn collector_lookup_rejects_foreign_interior_unallocated_and_unknown_class_slots() {
        let heap = Heap::new();
        let foreign = Heap::new();
        let value = allocate(&heap, 42_u64);
        let foreign_value = allocate(&foreign, 73_u64);
        let interior = value.erase().as_ptr().map_addr(|address| {
            std::num::NonZeroUsize::new(address.get().checked_add(1).unwrap()).unwrap()
        });

        let class = internal_class::<FirstType>(&heap);
        let empty_run = heap.inner.prepare_run(&class).unwrap();
        let empty = {
            let data = heap.inner.data.lock().unwrap();
            let geometry = RunGeometry::derive(
                metadata_for::<FirstType>().layout(),
                metadata_for::<FirstType>().requested_slot_size(),
            )
            .unwrap();
            let run = data.arena.run_at(empty_run).unwrap();
            ErasedGc::new(
                run.pointer()
                    .map_addr(|address| {
                        std::num::NonZeroUsize::new(
                            address
                                .get()
                                .checked_add(geometry.first_slot_offset)
                                .unwrap(),
                        )
                        .unwrap()
                    })
                    .cast(),
            )
        };

        let (unknown_class, unpublished_run) = {
            let mut data = heap.inner.data.lock().unwrap();
            let chunk = data.arena.reserve_chunk().unwrap();
            let geometry = RunGeometry::derive(std::alloc::Layout::new::<u64>(), None).unwrap();
            let id = AllocationClassId::new(99).unwrap();
            data.arena.initialize_run(chunk, 0, id, geometry).unwrap();
            data.arena
                .initialize_run(chunk, 1, class.id(), geometry)
                .unwrap();
            let unknown_run = data.arena.run_address(chunk, 0).unwrap();
            let unpublished_run = data.arena.run_address(chunk, 1).unwrap();
            (
                ErasedGc::new(
                    unknown_run
                        .pointer()
                        .map_addr(|address| {
                            std::num::NonZeroUsize::new(
                                address
                                    .get()
                                    .checked_add(geometry.first_slot_offset)
                                    .unwrap(),
                            )
                            .unwrap()
                        })
                        .cast(),
                ),
                ErasedGc::new(
                    unpublished_run
                        .pointer()
                        .map_addr(|address| {
                            std::num::NonZeroUsize::new(
                                address
                                    .get()
                                    .checked_add(geometry.first_slot_offset)
                                    .unwrap(),
                            )
                            .unwrap()
                        })
                        .cast(),
                ),
            )
        };

        let data = heap.inner.data.lock().unwrap();
        assert!(matches!(
            data.collector_slot(foreign_value.erase()),
            Err(CollectorLookupError::InvalidAddress)
        ));
        assert!(matches!(
            data.collector_slot(ErasedGc::new(interior)),
            Err(CollectorLookupError::InvalidAddress)
        ));
        assert!(matches!(
            data.collector_slot(empty),
            Err(CollectorLookupError::Unallocated)
        ));
        assert!(matches!(
            data.collector_slot(unknown_class),
            Err(CollectorLookupError::InvalidClass)
        ));
        assert!(matches!(
            data.collector_slot(unpublished_run),
            Err(CollectorLookupError::InvalidRunTopology)
        ));
    }

    #[test]
    fn failed_mark_attempts_leave_scratch_until_a_clean_retry_overwrites_it() {
        for partial_count in [0_usize, 1, 3] {
            let heap = Heap::new();
            let values = heap.with_mutator(|mutator| {
                let allocator = mutator.allocator::<WideSlot>().unwrap();
                (0..3_u64)
                    .map(|value| allocator.alloc(WideSlot { value }))
                    .collect::<Vec<_>>()
            });
            let root = heap.with_mutator(|mutator| mutator.root(values[0]));
            let slots = values
                .iter()
                .copied()
                .map(|value| collector_slot(&heap, value))
                .collect::<Vec<_>>();
            assert_eq!(
                slots
                    .iter()
                    .map(|slot| slot.owner.location)
                    .collect::<HashSet<_>>()
                    .len(),
                3
            );

            heap.request_collection();
            let failed_epoch = heap.inner.elect_idle_collection_for_test();
            let panic = catch_unwind(AssertUnwindSafe(|| {
                heap.inner.run_synthetic_collection_with_mark_work(
                    failed_epoch,
                    false,
                    |attempt, data| {
                        attempt.root_count = 17;
                        attempt.traced_object_count = 23;
                        for value in values.iter().take(partial_count) {
                            assert!(attempt.discover(data, value.erase()).unwrap());
                        }
                        assert_eq!(attempt.marked_slot_count, partial_count);
                        assert_eq!(attempt.worklist.len(), partial_count);
                        panic!("injected mark-attempt panic after {partial_count} marks");
                    },
                    |_, _| {},
                    |_| {},
                );
            }));
            let panic = panic.expect_err("synthetic mark attempt must panic");
            let message = panic
                .downcast_ref::<String>()
                .expect("injected mark panic must retain its String payload");
            assert_eq!(
                message,
                &format!("injected mark-attempt panic after {partial_count} marks")
            );
            assert!(!heap.inner.data.is_poisoned());

            {
                let data = heap.inner.data.lock().unwrap();
                assert_eq!(data.roots.len(), 1);
                for (index, slot) in slots.iter().copied().enumerate() {
                    assert_eq!(
                        data.collector_slot_is_marked(slot),
                        index < partial_count,
                        "failed attempt must leave its physical marks as unpublished scratch"
                    );
                    assert!(data.collector_slot(values[index].erase()).is_ok());
                }
            }
            let restored = heap.inner.coordinator_snapshot();
            assert_eq!(restored.phase, AdmissionPhase::Ordinary);
            assert_eq!(restored.active_collection, None);
            assert_eq!(restored.completed_collection_epoch, 0);
            assert!(restored.collection_requested);

            let retry_epoch = heap.inner.elect_idle_collection_for_test();
            assert_eq!(retry_epoch, failed_epoch);
            assert!(
                heap.inner
                    .run_synthetic_collection_with_mark_work(
                        retry_epoch,
                        false,
                        |attempt, data| {
                            assert_eq!(attempt.root_count, 0);
                            assert_eq!(attempt.marked_slot_count, 0);
                            assert_eq!(attempt.traced_object_count, 0);
                            assert!(attempt.worklist.is_empty());
                            for slot in &slots {
                                assert!(
                                    !data.collector_slot_is_marked(*slot),
                                    "retry must clear every stale partial mark before work"
                                );
                            }
                            assert!(attempt.discover(data, values[2].erase()).unwrap());
                        },
                        |_, _| {},
                        |_| {},
                    )
                    .is_none()
            );

            assert!(
                slot_is_marked(&heap, slots[0]),
                "the retained public root must be discovered after the retry hook"
            );
            assert!(!slot_is_marked(&heap, slots[1]));
            assert!(slot_is_marked(&heap, slots[2]));
            assert_eq!(
                heap.inner.coordinator_snapshot().completed_collection_epoch,
                1
            );
            heap.with_mutator(|mutator| assert_eq!(root.get(mutator).value, 0));
        }
    }

    #[test]
    fn trace_panics_after_partial_edge_reporting_are_recoverable() {
        const EDGE_COUNT: usize = 6;

        for reported_edges in [0_usize, 1, 4] {
            let heap = Heap::new();
            let armed = Arc::new(AtomicBool::new(true));
            let root_traces = Arc::new(AtomicUsize::new(0));
            let leaf_traces = (0..EDGE_COUNT)
                .map(|_| Arc::new(AtomicUsize::new(0)))
                .collect::<Vec<_>>();
            let (root, slots) = heap.with_mutator(|mutator| {
                let allocator = mutator.allocator::<PanickingTraceNode>().unwrap();
                let leaves = leaf_traces
                    .iter()
                    .map(|traces| {
                        allocator.alloc(PanickingTraceNode {
                            edges: Vec::new(),
                            panic_after_edges: None,
                            armed: Arc::clone(&armed),
                            traces: Arc::clone(traces),
                        })
                    })
                    .collect::<Vec<_>>();
                let root_value = allocator.alloc(PanickingTraceNode {
                    edges: leaves.clone(),
                    panic_after_edges: Some(reported_edges),
                    armed: Arc::clone(&armed),
                    traces: Arc::clone(&root_traces),
                });
                let slots = std::iter::once(root_value)
                    .chain(leaves)
                    .map(|value| collector_slot(&heap, value))
                    .collect::<Vec<_>>();
                (mutator.root(root_value), slots)
            });

            let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
                .expect_err("armed trace must panic");
            assert_eq!(
                panic_string(panic.as_ref()),
                format!("injected trace panic after {reported_edges} reported edges")
            );
            assert_failed_collection_restored(&heap);
            assert_eq!(root_traces.load(Ordering::Relaxed), 1);
            assert!(slot_is_marked(&heap, slots[0]));
            for (index, slot) in slots[1..].iter().copied().enumerate() {
                assert_eq!(
                    slot_is_marked(&heap, slot),
                    index < reported_edges,
                    "only edges reported before the trace panic may remain marked"
                );
                assert_eq!(leaf_traces[index].load(Ordering::Relaxed), 0);
            }

            armed.store(false, Ordering::Release);
            assert_eq!(heap.collect_full().unwrap().epoch(), 1);
            assert_eq!(root_traces.load(Ordering::Relaxed), 2);
            assert!(
                slots
                    .iter()
                    .copied()
                    .all(|slot| slot_is_marked(&heap, slot))
            );
            assert!(
                leaf_traces
                    .iter()
                    .all(|traces| traces.load(Ordering::Relaxed) == 1)
            );
            heap.with_mutator(|mutator| assert_eq!(root.get(mutator).edges.len(), EDGE_COUNT));
        }
    }

    #[test]
    fn worklist_publication_panics_leave_retryable_mark_scratch() {
        const EDGE_COUNT: usize = 8;

        for completed_pushes in [0_usize, 1, 5] {
            let heap = Heap::new();
            let root_traces = Arc::new(AtomicUsize::new(0));
            let leaf_traces = (0..EDGE_COUNT)
                .map(|_| Arc::new(AtomicUsize::new(0)))
                .collect::<Vec<_>>();
            let (root, slots) = heap.with_mutator(|mutator| {
                let allocator = mutator.allocator::<GraphNode>().unwrap();
                let leaves = leaf_traces
                    .iter()
                    .map(|traces| allocator.alloc(graph_node(Arc::clone(traces))))
                    .collect::<Vec<_>>();
                let root_value = allocator.alloc(graph_node_with_edges(
                    Arc::clone(&root_traces),
                    leaves.clone(),
                ));
                let slots = std::iter::once(root_value)
                    .chain(leaves)
                    .map(|value| collector_slot(&heap, value))
                    .collect::<Vec<_>>();
                (mutator.root(root_value), slots)
            });

            heap.request_collection();
            let epoch = heap.inner.elect_idle_collection_for_test();
            let panic = catch_unwind(AssertUnwindSafe(|| {
                heap.inner.run_synthetic_collection_with_mark_work(
                    epoch,
                    false,
                    |attempt, _| attempt.inject_worklist_panic_after(completed_pushes),
                    |_, _| {},
                    |_| {},
                );
            }))
            .expect_err("injected worklist publication must panic");
            assert_eq!(
                panic_string(panic.as_ref()),
                format!("injected worklist panic after {completed_pushes} completed pushes")
            );
            assert_failed_collection_restored(&heap);
            assert!(slot_is_marked(&heap, slots[0]));
            for (index, slot) in slots[1..].iter().copied().enumerate() {
                assert_eq!(
                    slot_is_marked(&heap, slot),
                    index < completed_pushes,
                    "the edge marked immediately before the failed publication remains scratch"
                );
            }

            assert_eq!(heap.collect_full().unwrap().epoch(), 1);
            assert_eq!(
                root_traces.load(Ordering::Relaxed),
                if completed_pushes == 0 { 1 } else { 2 },
                "a failed visitor publication retraces the root on retry"
            );
            assert!(
                leaf_traces
                    .iter()
                    .all(|traces| traces.load(Ordering::Relaxed) == 1)
            );
            assert!(
                slots
                    .iter()
                    .copied()
                    .all(|slot| slot_is_marked(&heap, slot))
            );
            heap.with_mutator(|mutator| {
                assert_eq!(root.get(mutator).edges.lock().unwrap().len(), EDGE_COUNT)
            });
        }
    }

    #[test]
    fn live_foreign_edges_fail_repeatedly_without_tracing_the_foreign_value() {
        let owner = Heap::new();
        let foreign = Heap::new();
        let foreign_traces = Arc::new(AtomicUsize::new(0));
        let (foreign_value, foreign_root) = foreign.with_mutator(|mutator| {
            let value = mutator
                .allocator::<GraphNode>()
                .unwrap()
                .alloc(graph_node(Arc::clone(&foreign_traces)));
            (value, mutator.root(value))
        });
        let holder_traces = Arc::new(AtomicUsize::new(0));
        let holder = invalid_edge_holder(&owner, foreign_value, Arc::clone(&holder_traces));

        assert_reachable_invalid_edge_repeats(
            &owner,
            "collector edge does not identify an exact managed slot",
            &holder_traces,
        );
        assert_eq!(foreign_traces.load(Ordering::Relaxed), 0);
        assert_eq!(foreign.collect_full().unwrap().epoch(), 1);
        assert_eq!(foreign_traces.load(Ordering::Relaxed), 1);
        foreign.with_mutator(|mutator| {
            assert!(foreign_root.get(mutator).edges.lock().unwrap().is_empty())
        });

        drop(holder);
        assert_eq!(owner.collect_full().unwrap().epoch(), 1);
        owner.with_mutator(|mutator| {
            let value = mutator.allocator::<u64>().unwrap().alloc(42);
            let root = mutator.root(value);
            assert_eq!(*root.get(mutator), 42);
        });
    }

    #[test]
    fn stale_and_non_slot_edges_fail_before_trace_dispatch() {
        let stale_owner = Heap::new();
        let _owner_reservation = allocate(&stale_owner, 1_u64);
        let stale = {
            let temporary = Heap::new();
            allocate(&temporary, graph_node(Arc::new(AtomicUsize::new(0))))
        };
        let stale_traces = Arc::new(AtomicUsize::new(0));
        let stale_holder = invalid_edge_holder(&stale_owner, stale, Arc::clone(&stale_traces));
        assert_reachable_invalid_edge_repeats(
            &stale_owner,
            "collector edge does not identify an exact managed slot",
            &stale_traces,
        );
        drop(stale_holder);
        assert_eq!(stale_owner.collect_full().unwrap().epoch(), 1);

        let non_slot_owner = Heap::new();
        let valid = allocate(&non_slot_owner, graph_node(Arc::new(AtomicUsize::new(0))));
        let interior = std::ptr::NonNull::new(
            valid
                .erase()
                .as_ptr()
                .as_ptr()
                .cast::<u8>()
                .wrapping_add(1)
                .cast::<GraphNode>(),
        )
        .unwrap();
        // SAFETY: this deliberately invalid handle is never dereferenced. The
        // C5C fixture reports it only to prove collector discovery rejects a
        // non-slot address before trace dispatch.
        let interior = unsafe { crate::Gc::<GraphNode>::from_raw(interior) };
        let non_slot_traces = Arc::new(AtomicUsize::new(0));
        let non_slot_holder =
            invalid_edge_holder(&non_slot_owner, interior, Arc::clone(&non_slot_traces));
        assert_reachable_invalid_edge_repeats(
            &non_slot_owner,
            "collector edge does not identify an exact managed slot",
            &non_slot_traces,
        );
        drop(non_slot_holder);
        assert_eq!(non_slot_owner.collect_full().unwrap().epoch(), 1);

        for heap in [&stale_owner, &non_slot_owner] {
            heap.with_mutator(|mutator| {
                let _ = mutator.allocator::<u64>().unwrap().alloc(1);
            });
        }
    }

    #[test]
    fn unallocated_slot_edges_fail_before_trace_dispatch() {
        let heap = Heap::new();
        let class = internal_class::<GraphNode>(&heap);
        let run = heap.inner.prepare_run(&class).unwrap();
        let pointer = {
            let data = heap.inner.data.lock().unwrap();
            let run = data.arena.run_at(run).unwrap();
            let geometry = RunGeometry::derive(
                metadata_for::<GraphNode>().layout(),
                metadata_for::<GraphNode>().requested_slot_size(),
            )
            .unwrap();
            std::ptr::NonNull::new(
                run.pointer()
                    .as_ptr()
                    .wrapping_add(geometry.first_slot_offset)
                    .cast::<GraphNode>(),
            )
            .unwrap()
        };
        // SAFETY: this deliberately unallocated handle is never dereferenced.
        // The C5C fixture reports it only to prove collector discovery checks
        // the allocation bitmap before trace dispatch.
        let edge = unsafe { crate::Gc::<GraphNode>::from_raw(pointer) };
        let holder_traces = Arc::new(AtomicUsize::new(0));
        let holder = invalid_edge_holder(&heap, edge, Arc::clone(&holder_traces));

        assert_reachable_invalid_edge_repeats(
            &heap,
            "collector edge does not identify an allocated value",
            &holder_traces,
        );
        drop(holder);
        assert_eq!(heap.collect_full().unwrap().epoch(), 1);
        heap.with_mutator(|mutator| {
            let _ = mutator
                .allocator::<GraphNode>()
                .unwrap()
                .alloc(graph_node(Arc::new(AtomicUsize::new(0))));
        });
    }

    #[test]
    fn successful_collection_report_counts_roots_traces_and_distinct_marks() {
        let heap = Heap::new();
        let counters = (0..3)
            .map(|_| Arc::new(AtomicUsize::new(0)))
            .collect::<Vec<_>>();
        let (first_root, second_root, root_alias) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<GraphNode>().unwrap();
            let nodes = counters
                .iter()
                .map(|counter| allocator.alloc(graph_node(Arc::clone(counter))))
                .collect::<Vec<_>>();
            connect_graph_nodes(mutator, nodes[0], nodes[1]);
            connect_graph_nodes(mutator, nodes[0], nodes[1]);
            let first_root = mutator.root(nodes[0]);
            let root_alias = first_root.clone();
            (first_root, mutator.root(nodes[0]), root_alias)
        });

        let first = heap.collect_full().unwrap();
        assert_eq!(first.epoch(), 1);
        assert_eq!(first.root_entries(), 2);
        assert_eq!(first.traced_objects(), 2);
        assert_eq!(first.marked_slots(), 2);
        assert_eq!(first.conservatively_retained_slots(), 0);
        assert_eq!(counters[0].load(Ordering::Relaxed), 1);
        assert_eq!(counters[1].load(Ordering::Relaxed), 1);
        assert_eq!(counters[2].load(Ordering::Relaxed), 0);

        drop(second_root);
        let second = heap.collect_full().unwrap();
        assert_eq!(second.epoch(), 2);
        assert_eq!(second.root_entries(), 1);
        assert_eq!(second.traced_objects(), 2);
        assert_eq!(second.marked_slots(), 2);
        assert_eq!(second.conservatively_retained_slots(), 0);
        assert_eq!(
            heap.inner.coordinator_snapshot().latest_collection_report,
            Some(second)
        );
        heap.with_mutator(|mutator| {
            assert!(std::ptr::eq(
                first_root.get(mutator),
                root_alias.get(mutator)
            ));
        });
    }

    #[test]
    fn c5d_random_graph_marks_match_an_independent_reachability_oracle() {
        #[cfg(miri)]
        const CASES: u64 = 1;
        #[cfg(not(miri))]
        const CASES: u64 = 24;
        #[cfg(miri)]
        const NODE_COUNT: usize = 65;
        #[cfg(not(miri))]
        const NODE_COUNT: usize = 257;
        const CONNECTED_NODE_COUNT: usize = NODE_COUNT - 8;
        const ROOT_INDICES: [usize; 3] =
            [0, CONNECTED_NODE_COUNT / 3, 2 * CONNECTED_NODE_COUNT / 3];

        for case in 1..=CASES {
            let mut rng = TestRng::new(case.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let mut adjacency = vec![Vec::new(); NODE_COUNT];
            for edges in &mut adjacency[..CONNECTED_NODE_COUNT] {
                let edge_count = rng.index(7);
                edges.extend((0..edge_count).map(|_| rng.index(CONNECTED_NODE_COUNT)));
            }
            adjacency[0].extend([0, 1, 1]);
            adjacency[1].push(0);
            let expected = reference_reachability(&adjacency, &ROOT_INDICES);
            let expected_count = expected.iter().filter(|reachable| **reachable).count();

            let heap = Heap::new();
            let counters = (0..NODE_COUNT)
                .map(|_| Arc::new(AtomicUsize::new(0)))
                .collect::<Vec<_>>();
            let (nodes, roots) = heap.with_mutator(|mutator| {
                let allocator = mutator.allocator::<GraphNode>().unwrap();
                let nodes = counters
                    .iter()
                    .map(|counter| allocator.alloc(graph_node(Arc::clone(counter))))
                    .collect::<Vec<_>>();
                for (owner, edges) in adjacency.iter().enumerate() {
                    for target in edges {
                        connect_graph_nodes(mutator, nodes[owner], nodes[*target]);
                    }
                }
                let roots = ROOT_INDICES
                    .iter()
                    .map(|index| mutator.root(nodes[*index]))
                    .collect::<Vec<_>>();
                (nodes, roots)
            });
            let slots = nodes
                .iter()
                .copied()
                .map(|node| collector_slot(&heap, node))
                .collect::<Vec<_>>();

            let report = heap.collect_full().unwrap();
            assert_eq!(report.root_entries(), roots.len(), "case {case}");
            assert_eq!(report.marked_slots(), expected_count, "case {case}");
            assert_eq!(report.traced_objects(), expected_count, "case {case}");
            for index in 0..nodes.len() {
                assert_eq!(
                    slot_is_marked(&heap, slots[index]),
                    expected[index],
                    "case {case}, node {index}"
                );
                assert_eq!(
                    counters[index].load(Ordering::Relaxed),
                    usize::from(expected[index]),
                    "case {case}, node {index}"
                );
            }
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "native complete-run bitmap history is intentionally scale-shaped"
    )]
    fn full_run_sweep_tracks_all_one_and_zero_histories() {
        let metadata = metadata_for::<u64>();
        let geometry = RunGeometry::derive(metadata.layout(), metadata.requested_slot_size())
            .expect("u64 must have supported run geometry");
        let heap = Heap::new();
        let values = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            (0..geometry.slot_count)
                .map(|value| allocator.alloc(value as u64))
                .collect::<Vec<_>>()
        });
        let slots = values
            .iter()
            .copied()
            .map(|value| collector_slot(&heap, value))
            .collect::<Vec<_>>();
        let first_run = slots[0].owner.location;
        assert!(
            slots[..geometry.slot_count]
                .iter()
                .all(|slot| slot.owner.location == first_run)
        );
        let assert_first_run_marks = |expected: &HashSet<usize>| {
            for (index, slot) in slots[..geometry.slot_count].iter().copied().enumerate() {
                assert_eq!(
                    slot_is_marked(&heap, slot),
                    expected.contains(&index),
                    "unexpected mark state at slot {index}"
                );
            }
        };

        let all_roots = heap.with_mutator(|mutator| {
            values[..geometry.slot_count]
                .iter()
                .copied()
                .map(|value| mutator.root(value))
                .collect::<Vec<_>>()
        });
        let all = heap.collect_full().unwrap();
        assert_eq!(all.marked_slots(), geometry.slot_count);
        assert_first_run_marks(&(0..geometry.slot_count).collect());

        let one_index = geometry.slot_count / 2;
        let one_root = all_roots[one_index].clone();
        drop(all_roots);
        let one = heap.collect_full().unwrap();
        assert_eq!(one.marked_slots(), 1);
        assert_first_run_marks(&HashSet::from([one_index]));
        assert_eq!(
            heap.inner
                .data
                .lock()
                .unwrap()
                .arena
                .allocated_slot_pointers(first_run)
                .len(),
            1,
            "partial no-drop sweep must retain exactly the marked allocation"
        );

        drop(one_root);
        let empty_again = heap.collect_full().unwrap();
        assert_eq!(empty_again.marked_slots(), 0);
        assert_first_run_marks(&HashSet::new());
    }

    #[test]
    fn failed_mark_attempt_does_not_publish_or_replace_a_report() {
        let heap = Heap::new();
        let first = heap.collect_full().unwrap();
        assert_eq!(first.epoch(), 1);
        assert_eq!(first.root_entries(), 0);
        assert_eq!(first.traced_objects(), 0);
        assert_eq!(first.marked_slots(), 0);

        let armed = Arc::new(AtomicBool::new(true));
        let traces = Arc::new(AtomicUsize::new(0));
        let root = heap.with_mutator(|mutator| {
            let value =
                mutator
                    .allocator::<PanickingTraceNode>()
                    .unwrap()
                    .alloc(PanickingTraceNode {
                        edges: Vec::new(),
                        panic_after_edges: Some(0),
                        armed: Arc::clone(&armed),
                        traces: Arc::clone(&traces),
                    });
            mutator.root(value)
        });

        let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
            .expect_err("armed trace must prevent report publication");
        assert_eq!(
            panic_string(panic.as_ref()),
            "injected trace panic after 0 reported edges"
        );
        let failed = heap.inner.coordinator_snapshot();
        assert_eq!(failed.phase, AdmissionPhase::Ordinary);
        assert_eq!(failed.active_collection, None);
        assert_eq!(failed.completed_collection_epoch, 1);
        assert_eq!(failed.latest_collection_report, Some(first));
        assert!(failed.collection_requested);

        armed.store(false, Ordering::Release);
        let second = heap.collect_full().unwrap();
        assert_eq!(second.epoch(), 2);
        assert_eq!(second.root_entries(), 1);
        assert_eq!(second.traced_objects(), 1);
        assert_eq!(second.marked_slots(), 1);
        assert_eq!(second.conservatively_retained_slots(), 0);
        assert_eq!(traces.load(Ordering::Relaxed), 2);
        heap.with_mutator(|mutator| assert!(root.get(mutator).edges.is_empty()));
    }

    #[test]
    fn root_seeding_counts_entries_but_enqueues_each_allocation_once() {
        let heap = Heap::new();
        let traces = Arc::new(AtomicUsize::new(0));
        let (value, first_root, second_root, root_alias, dead_root) =
            heap.with_mutator(|mutator| {
                let allocator = mutator.allocator::<GraphNode>().unwrap();
                let value = allocator.alloc(graph_node(Arc::clone(&traces)));
                let dead = allocator.alloc(graph_node(Arc::new(AtomicUsize::new(0))));
                let first_root = mutator.root(value);
                let root_alias = first_root.clone();
                (
                    value,
                    first_root,
                    mutator.root(value),
                    root_alias,
                    mutator.root(dead),
                )
            });
        drop(dead_root);

        let exclusive = heap.inner.enter_synthetic_exclusive();
        let root_registry_len = heap.inner.data.lock().unwrap().roots.len();
        assert_eq!(root_registry_len, 3);
        let mut attempt = super::MarkAttempt::default();
        attempt.reserve_root_capacity(root_registry_len);
        {
            let mut data = heap.inner.data.lock().unwrap();
            data.clear_mark_bitmaps();
            attempt.seed_registered_roots(&mut data).unwrap();

            assert_eq!(data.roots.len(), 2);
            assert_eq!(attempt.root_count, 2);
            assert_eq!(attempt.marked_slot_count, 1);
            assert_eq!(attempt.worklist.len(), 1);
            assert_eq!(attempt.worklist[0].value, value.erase());
            assert!(std::ptr::eq(
                attempt.worklist[0].metadata,
                metadata_for::<GraphNode>()
            ));
            assert_eq!(traces.load(Ordering::Relaxed), 0);

            attempt.trace_worklist(&mut data).unwrap();
            assert!(attempt.worklist.is_empty());
            assert_eq!(attempt.traced_object_count, 1);
        }
        drop(exclusive);

        assert_eq!(traces.load(Ordering::Relaxed), 1);
        assert!(slot_is_marked(&heap, collector_slot(&heap, value)));
        heap.with_mutator(|mutator| {
            assert!(std::ptr::eq(
                first_root.get(mutator),
                second_root.get(mutator)
            ));
            assert!(std::ptr::eq(
                first_root.get(mutator),
                root_alias.get(mutator)
            ));
        });
    }

    #[test]
    fn checked_nonrecursive_marking_handles_cycles_diamonds_and_duplicate_edges() {
        let heap = Heap::new();
        let counters = (0..5)
            .map(|_| Arc::new(AtomicUsize::new(0)))
            .collect::<Vec<_>>();
        let (nodes, root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<GraphNode>().unwrap();
            let nodes = counters
                .iter()
                .map(|counter| allocator.alloc(graph_node(Arc::clone(counter))))
                .collect::<Vec<_>>();
            connect_graph_nodes(mutator, nodes[0], nodes[1]);
            connect_graph_nodes(mutator, nodes[0], nodes[2]);
            connect_graph_nodes(mutator, nodes[0], nodes[1]);
            connect_graph_nodes(mutator, nodes[1], nodes[3]);
            connect_graph_nodes(mutator, nodes[2], nodes[3]);
            connect_graph_nodes(mutator, nodes[3], nodes[0]);
            let root = mutator.root(nodes[0]);
            (nodes, root)
        });
        let slots = nodes
            .iter()
            .copied()
            .map(|node| collector_slot(&heap, node))
            .collect::<Vec<_>>();

        heap.collect_full().unwrap();

        assert_eq!(counters[0].load(Ordering::Relaxed), 1);
        assert_eq!(counters[1].load(Ordering::Relaxed), 1);
        assert_eq!(counters[2].load(Ordering::Relaxed), 1);
        assert_eq!(counters[3].load(Ordering::Relaxed), 1);
        assert_eq!(counters[4].load(Ordering::Relaxed), 0);
        assert!(
            nodes[..4]
                .iter()
                .enumerate()
                .all(|(index, _)| slot_is_marked(&heap, slots[index]))
        );
        assert!(!slot_is_marked(&heap, slots[4]));
        heap.with_mutator(|mutator| assert_eq!(root.get(mutator).edges.lock().unwrap().len(), 3));
    }

    #[test]
    fn checked_nonrecursive_marking_handles_a_deep_chain() {
        #[cfg(miri)]
        const DEPTH: usize = 256;
        #[cfg(not(miri))]
        const DEPTH: usize = 20_000;

        let heap = Heap::new();
        let traces = Arc::new(AtomicUsize::new(0));
        let (last, root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<GraphNode>().unwrap();
            let nodes = (0..DEPTH)
                .map(|_| allocator.alloc(graph_node(Arc::clone(&traces))))
                .collect::<Vec<_>>();
            for pair in nodes.windows(2) {
                connect_graph_nodes(mutator, pair[0], pair[1]);
            }
            (nodes[DEPTH - 1], mutator.root(nodes[0]))
        });

        heap.collect_full().unwrap();

        assert_eq!(traces.load(Ordering::Relaxed), DEPTH);
        assert!(slot_is_marked(&heap, collector_slot(&heap, last)));
        heap.with_mutator(|mutator| {
            assert_eq!(root.get(mutator).traces.load(Ordering::Relaxed), DEPTH)
        });
    }

    #[test]
    fn checked_nonrecursive_marking_handles_wide_shared_spines() {
        const WIDTH: usize = 2_048;
        const TAIL_DEPTH: usize = 64;

        let heap = Heap::new();
        let traces = Arc::new(AtomicUsize::new(0));
        let (shared_tail, root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<GraphNode>().unwrap();
            let tail = (0..TAIL_DEPTH)
                .map(|_| allocator.alloc(graph_node(Arc::clone(&traces))))
                .collect::<Vec<_>>();
            for pair in tail.windows(2) {
                connect_graph_nodes(mutator, pair[0], pair[1]);
            }

            let branches = (0..WIDTH)
                .map(|_| allocator.alloc(graph_node(Arc::clone(&traces))))
                .collect::<Vec<_>>();
            for branch in &branches {
                connect_graph_nodes(mutator, *branch, tail[0]);
            }

            let root_node = allocator.alloc(graph_node(Arc::clone(&traces)));
            for branch in branches {
                connect_graph_nodes(mutator, root_node, branch);
            }
            (tail[TAIL_DEPTH - 1], mutator.root(root_node))
        });

        heap.collect_full().unwrap();

        assert_eq!(traces.load(Ordering::Relaxed), 1 + WIDTH + TAIL_DEPTH);
        assert!(slot_is_marked(&heap, collector_slot(&heap, shared_tail)));
        heap.with_mutator(|mutator| {
            assert_eq!(root.get(mutator).edges.lock().unwrap().len(), WIDTH)
        });
    }

    #[test]
    #[ignore = "C5D.2 scale fixture; run crates/glam-gc/scripts/check-scale.sh"]
    fn c5d_scale_million_node_deep_chain_is_nonrecursive() {
        #[cfg(miri)]
        const NODE_COUNT: usize = 256;
        #[cfg(not(miri))]
        const NODE_COUNT: usize = 1_000_000;

        let heap = Heap::new();
        let traces = Arc::new(AtomicUsize::new(0));
        let (tail, root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<GraphNode>().unwrap();
            let tail = allocator.alloc(graph_node(Arc::clone(&traces)));
            let mut head = tail;
            for _ in 1..NODE_COUNT {
                head = allocator.alloc(graph_node_with_edge(Arc::clone(&traces), head));
            }
            (tail, mutator.root(head))
        });

        let report = heap.collect_full().unwrap();
        assert_eq!(report.root_entries(), 1);
        assert_eq!(report.traced_objects(), NODE_COUNT);
        assert_eq!(report.marked_slots(), NODE_COUNT);
        assert_eq!(traces.load(Ordering::Relaxed), NODE_COUNT);
        assert!(slot_is_marked(&heap, collector_slot(&heap, tail)));
        heap.with_mutator(|mutator| assert_eq!(root.get(mutator).edges.lock().unwrap().len(), 1));
    }

    #[test]
    #[ignore = "C5D.2 scale fixture; run crates/glam-gc/scripts/check-scale.sh"]
    fn c5d_scale_flat_million_edge_array_records_worklist_peak() {
        #[cfg(miri)]
        const EDGE_COUNT: usize = 256;
        #[cfg(not(miri))]
        const EDGE_COUNT: usize = 1_000_000;

        let heap = Heap::new();
        let traces = Arc::new(AtomicUsize::new(0));
        let (last, root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<GraphNode>().unwrap();
            let edges = (0..EDGE_COUNT)
                .map(|_| allocator.alloc(graph_node(Arc::clone(&traces))))
                .collect::<Vec<_>>();
            let last = edges[EDGE_COUNT - 1];
            let root = allocator.alloc(graph_node_with_edges(Arc::clone(&traces), edges));
            (last, mutator.root(root))
        });

        let report = heap.collect_full().unwrap();
        assert_eq!(report.root_entries(), 1);
        assert_eq!(report.traced_objects(), EDGE_COUNT + 1);
        assert_eq!(report.marked_slots(), EDGE_COUNT + 1);
        assert_eq!(traces.load(Ordering::Relaxed), EDGE_COUNT + 1);
        assert!(slot_is_marked(&heap, collector_slot(&heap, last)));
        eprintln!(
            "C5D.2 flat {EDGE_COUNT}-edge worklist peak: len={}, capacity={}",
            report.peak_object_worklist_len, report.peak_object_worklist_capacity
        );
        heap.with_mutator(|mutator| {
            assert_eq!(root.get(mutator).edges.lock().unwrap().len(), EDGE_COUNT)
        });
    }

    #[test]
    fn automatic_pressure_is_acknowledged_by_the_next_idle_entry() {
        let heap = Heap::new();
        let class = internal_class::<FirstType>(&heap);

        heap.with_mutator(|_| {
            for _ in 0..FIXED_SURVIVOR_RUN_HEADROOM {
                heap.inner.prepare_run(&class).unwrap();
            }
            assert!(heap.inner.allocation_pressure().collection_requested);
            let coordinator = heap.inner.coordinator_snapshot();
            assert!(coordinator.collection_requested);
            assert_eq!(coordinator.completed_collection_epoch, 0);
        });

        {
            let pressure = heap.inner.allocation_pressure();
            assert!(pressure.collection_requested);
            assert_eq!(pressure.assigned_runs, FIXED_SURVIVOR_RUN_HEADROOM);
            assert_eq!(pressure.high_water_mark, FIXED_SURVIVOR_RUN_HEADROOM);
            let coordinator = heap.inner.coordinator_snapshot();
            assert!(coordinator.collection_requested);
            assert_eq!(coordinator.completed_collection_epoch, 0);
        }

        heap.with_mutator(|_| {});
        let pressure = heap.inner.allocation_pressure();
        assert!(!pressure.collection_requested);
        assert_eq!(pressure.assigned_runs, 0);
        assert_eq!(pressure.high_water_mark, FIXED_SURVIVOR_RUN_HEADROOM);
        let coordinator = heap.inner.coordinator_snapshot();
        assert!(!coordinator.collection_requested);
        assert_eq!(coordinator.completed_collection_epoch, 1);

        heap.with_mutator(|_| {
            heap.inner.prepare_run(&class).unwrap();
        });
        let pressure = heap.inner.allocation_pressure();
        assert_eq!(pressure.assigned_runs, 1);
        assert_eq!(pressure.high_water_mark, FIXED_SURVIVOR_RUN_HEADROOM);
        assert!(!pressure.collection_requested);
        assert!(!heap.inner.coordinator_snapshot().collection_requested);
        heap.with_mutator(|_| {});
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            1,
            "acknowledged pressure must not collect again immediately"
        );
    }

    #[test]
    fn post_mark_handoff_exposes_summary_and_releases_data_before_finalizing() {
        let heap = Heap::new();
        let value = allocate(&heap, 42_u64);
        let slot = collector_slot(&heap, value);
        let root = heap.with_mutator(|mutator| mutator.root(value));
        let post_mark_seen = AtomicBool::new(false);

        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        assert!(
            heap.inner
                .run_synthetic_collection(
                    epoch,
                    false,
                    |post_mark, data| {
                        assert_eq!(post_mark.summary.root_entries, 1);
                        assert_eq!(post_mark.summary.traced_objects, 1);
                        assert_eq!(post_mark.summary.marked_slots, 1);
                        assert_eq!(post_mark.summary.conservatively_retained_slots, 0);
                        assert!(data.collector_slot_is_marked(slot));
                        post_mark_seen.store(true, Ordering::Release);
                    },
                    |_| {
                        assert!(post_mark_seen.load(Ordering::Acquire));
                        assert!(
                            heap.inner.data.try_lock().is_ok(),
                            "post-mark managed-data access must end before finalization"
                        );
                    },
                )
                .is_none()
        );

        let report = heap
            .inner
            .coordinator_snapshot()
            .latest_collection_report
            .expect("successful post-mark work must publish its report");
        assert_eq!(report.root_entries(), 1);
        assert_eq!(report.traced_objects(), 1);
        assert_eq!(report.marked_slots(), 1);
        heap.with_mutator(|mutator| assert_eq!(*root.get(mutator), 42));
    }

    #[test]
    fn post_mark_dead_set_classifies_exact_live_and_dead_slot_masks() {
        let heap = Heap::new();
        let plain_live = allocate(&heap, 10_u64);
        let plain_dead_a = allocate(&heap, 20_u64);
        let plain_dead_b = allocate(&heap, 30_u64);
        let plain_root = heap.with_mutator(|mutator| mutator.root(plain_live));
        let plain_live_slot = collector_slot(&heap, plain_live);
        let plain_dead_slots = [
            collector_slot(&heap, plain_dead_a),
            collector_slot(&heap, plain_dead_b),
        ];

        let drops = Arc::new(AtomicUsize::new(0));
        let dropping_live = allocate(&heap, DropCounter(Arc::clone(&drops)));
        let dropping_dead = allocate(&heap, DropCounter(Arc::clone(&drops)));
        let dropping_root = heap.with_mutator(|mutator| mutator.root(dropping_live));
        let dropping_live_slot = collector_slot(&heap, dropping_live);
        let dropping_dead_slot = collector_slot(&heap, dropping_dead);

        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        assert!(
            heap.inner
                .run_synthetic_collection(
                    epoch,
                    false,
                    |post_mark, _| {
                        assert_eq!(post_mark.summary.root_entries, 2);
                        assert_eq!(post_mark.summary.traced_objects, 2);
                        assert_eq!(post_mark.summary.marked_slots, 2);

                        let plan = &post_mark.dead_set;
                        assert_eq!(plan.allocated_slots, 5);
                        assert_eq!(plan.live_slots, 2);
                        assert_eq!(plan.no_drop_dead_slots, 2);
                        assert_eq!(plan.drop_required_dead_slots, 1);
                        assert_eq!(plan.live_runs, 2);
                        assert_eq!(plan.no_drop_dead_runs, 1);
                        assert_eq!(plan.drop_required_dead_runs, 1);
                        assert_eq!(plan.dead_runs.len(), 2);

                        let plain = plan
                            .dead_runs
                            .iter()
                            .find(|run| run.class_id == plain_live_slot.owner.class_id)
                            .expect("plain dead run must be classified");
                        assert_eq!(plain.disposition, DeadSlotDisposition::NoDrop);
                        assert_eq!(plain.target.location, plain_live_slot.owner.location);
                        assert_eq!(plain.target.geometry, plain_live_slot.owner.geometry);
                        assert!(std::ptr::eq(plain.metadata, metadata_for::<u64>()));
                        assert_eq!(plain.live_slots, 1);
                        assert_eq!(plain.dead_slots, 2);
                        assert_eq!(
                            plan.dead_words[plain.dead_words.clone()],
                            dead_bitmap_words(&plain_dead_slots)
                        );

                        let dropping = plan
                            .dead_runs
                            .iter()
                            .find(|run| run.class_id == dropping_live_slot.owner.class_id)
                            .expect("drop-required dead run must be classified");
                        assert_eq!(dropping.disposition, DeadSlotDisposition::DropRequired);
                        assert_eq!(dropping.target.location, dropping_live_slot.owner.location);
                        assert_eq!(dropping.target.geometry, dropping_live_slot.owner.geometry);
                        assert!(std::ptr::eq(
                            dropping.metadata,
                            metadata_for::<DropCounter>()
                        ));
                        assert_eq!(dropping.live_slots, 1);
                        assert_eq!(dropping.dead_slots, 1);
                        assert_eq!(
                            plan.dead_words[dropping.dead_words.clone()],
                            dead_bitmap_words(&[dropping_dead_slot])
                        );
                    },
                    |_| {},
                )
                .is_none()
        );

        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(
            !heap
                .inner
                .resolve_slot(plain_dead_a.erase().as_ptr().as_ptr() as usize)
                .unwrap()
                .allocated,
            "partial no-drop sweep must clear a dead allocation bit"
        );
        assert!(
            !heap
                .inner
                .resolve_slot(dropping_dead.erase().as_ptr().as_ptr() as usize)
                .unwrap()
                .allocated,
            "successful finalization must retire the drop-bearing allocation"
        );
        heap.with_mutator(|mutator| {
            assert_eq!(*plain_root.get(mutator), 10);
            let _ = dropping_root.get(mutator);
        });
    }

    #[test]
    fn dead_set_masks_cross_bitmap_words_and_run_boundaries() {
        let heap = Heap::new();
        let metadata = metadata_for::<BitmapBoundarySlot>();
        let geometry = RunGeometry::derive(metadata.layout(), metadata.requested_slot_size())
            .expect("boundary fixture must have supported geometry");
        assert!(geometry.slot_count > u64::BITS as usize);
        let allocation_count = geometry.slot_count + 3;
        let values = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<BitmapBoundarySlot>().unwrap();
            (0..allocation_count)
                .map(|value| {
                    allocator.alloc(BitmapBoundarySlot {
                        _value: value as u64,
                    })
                })
                .collect::<Vec<_>>()
        });
        let slots = values
            .iter()
            .map(|value| collector_slot(&heap, *value))
            .collect::<Vec<_>>();
        let rooted_indices = [
            0,
            u64::BITS as usize - 1,
            u64::BITS as usize,
            geometry.slot_count - 1,
            geometry.slot_count,
        ];
        let roots = heap.with_mutator(|mutator| {
            rooted_indices
                .iter()
                .map(|&index| mutator.root(values[index]))
                .collect::<Vec<_>>()
        });

        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        assert!(
            heap.inner
                .run_synthetic_collection(
                    epoch,
                    false,
                    |post_mark, _| {
                        let plan = &post_mark.dead_set;
                        assert_eq!(plan.allocated_slots, allocation_count);
                        assert_eq!(plan.live_slots, rooted_indices.len());
                        assert_eq!(
                            plan.no_drop_dead_slots,
                            allocation_count - rooted_indices.len()
                        );
                        assert_eq!(plan.drop_required_dead_slots, 0);
                        assert_eq!(plan.live_runs, 2);
                        assert_eq!(plan.no_drop_dead_runs, 2);
                        assert_eq!(plan.dead_runs.len(), 2);

                        for run in &plan.dead_runs {
                            assert_eq!(run.disposition, DeadSlotDisposition::NoDrop);
                            let expected = slots
                                .iter()
                                .enumerate()
                                .filter(|(index, slot)| {
                                    slot.owner.location == run.target.location
                                        && !rooted_indices.contains(index)
                                })
                                .map(|(_, slot)| *slot)
                                .collect::<Vec<_>>();
                            assert_eq!(run.dead_slots, expected.len());
                            assert_eq!(
                                plan.dead_words[run.dead_words.clone()],
                                dead_bitmap_words(&expected)
                            );
                        }
                    },
                    |_| {},
                )
                .is_none()
        );

        for (index, value) in values.iter().enumerate() {
            assert_eq!(
                heap.inner
                    .resolve_slot(value.erase().as_ptr().as_ptr() as usize)
                    .unwrap()
                    .allocated,
                rooted_indices.contains(&index),
                "swept allocation mismatch at boundary fixture slot {index}"
            );
        }

        heap.with_mutator(|mutator| {
            for root in &roots {
                let _ = root.get(mutator);
            }
        });
    }

    #[test]
    fn finalizer_panic_after_partial_no_drop_sweep_preserves_reclaimed_state() {
        let heap = Heap::new();
        let (live, dead) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            (allocator.alloc(10), allocator.alloc(20))
        });
        let live_root = heap.with_mutator(|mutator| mutator.root(live));
        let live_slot = collector_slot(&heap, live);
        let dead_slot = collector_slot(&heap, dead);
        assert_eq!(live_slot.owner.location, dead_slot.owner.location);

        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.inner.run_synthetic_collection(
                epoch,
                false,
                |_, _| {},
                |_| panic!("injected finalizer panic after partial no-drop sweep"),
            );
        }));
        assert!(panic.is_err());
        assert_failed_collection_restored(&heap);

        for (value, expected) in [(live, true), (dead, false)] {
            assert_eq!(
                heap.inner
                    .resolve_slot(value.erase().as_ptr().as_ptr() as usize)
                    .unwrap()
                    .allocated,
                expected
            );
        }
        {
            let data = heap.inner.data.lock().unwrap();
            assert!(data.free_runs.is_empty());
            assert_eq!(
                data.classes[class_index(live_slot.owner.class_id).unwrap()]
                    .runs()
                    .len(),
                1,
                "partial run must remain attached to its allocation class"
            );
        }

        let report = heap.collect_full().unwrap();
        assert_eq!(report.marked_slots(), 1);
        assert!(
            !heap
                .inner
                .resolve_slot(dead.erase().as_ptr().as_ptr() as usize)
                .unwrap()
                .allocated,
            "retry must not restore the swept allocation"
        );
        heap.with_mutator(|mutator| assert_eq!(*live_root.get(mutator), 10));
    }

    #[test]
    fn post_collection_claim_reuses_a_swept_slot_without_lazy_sweep() {
        let heap = Heap::new();
        let (live, dead) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            (allocator.alloc(10), allocator.alloc(20))
        });
        let root = heap.with_mutator(|mutator| mutator.root(live));
        let live_slot = collector_slot(&heap, live);
        let dead_slot = collector_slot(&heap, dead);
        assert_eq!(live_slot.owner.location, dead_slot.owner.location);

        let report = heap.collect_full().unwrap();
        assert_eq!(report.marked_slots(), 1);
        assert!(
            !heap
                .inner
                .resolve_slot(dead.erase().as_ptr().as_ptr() as usize)
                .unwrap()
                .allocated
        );

        let claims_before = heap.inner.allocation_cursor_claim_count();
        let replacement = allocate(&heap, 30_u64);
        let replacement_slot = collector_slot(&heap, replacement);
        assert_eq!(replacement_slot.owner.location, dead_slot.owner.location);
        assert_eq!(
            replacement_slot.owner.slot_index,
            dead_slot.owner.slot_index
        );
        assert_eq!(
            heap.inner.allocation_cursor_claim_count(),
            claims_before + 1
        );
        heap.with_mutator(|mutator| assert_eq!(*root.get(mutator), 10));
    }

    #[test]
    fn partial_drop_runs_release_completed_words_for_reuse() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let values = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<DropCounter>().unwrap();
            (0..66)
                .map(|_| allocator.alloc(DropCounter(Arc::clone(&drops))))
                .collect::<Vec<_>>()
        });
        let roots = heap.with_mutator(|mutator| {
            [0_usize, u64::BITS as usize]
                .into_iter()
                .map(|index| mutator.root(values[index]))
                .collect::<Vec<_>>()
        });
        let first_slot = collector_slot(&heap, values[0]);
        assert_eq!(
            collector_slot(&heap, values[64]).owner.slot_index / u64::BITS as usize,
            1
        );

        let report = heap.collect_full().unwrap();
        assert_eq!(report.marked_slots(), 2);
        assert_eq!(report.reclaimed_slots(), 64);
        assert_eq!(report.finalized_slots(), 64);
        assert_eq!(report.reclaimed_runs(), 0);
        assert_eq!(drops.load(Ordering::Relaxed), 64);
        {
            let data = heap.inner.data.lock().unwrap();
            let state = classification_state_snapshot(&heap, &data);
            let run = state
                .runs
                .iter()
                .find(|run| run.location == first_slot.owner.location)
                .unwrap();
            assert_eq!(lease_words(run)[0] & 0b111, 0);
            assert!(data.finalization_batch.runs.is_empty());
            assert_eq!(
                state.classes[class_index(first_slot.owner.class_id).unwrap()].frontier,
                Some(first_slot.owner.location)
            );
        }

        let replacement = allocate(&heap, DropCounter(Arc::clone(&drops)));
        let replacement_slot = collector_slot(&heap, replacement);
        assert_eq!(replacement_slot.owner.location, first_slot.owner.location);
        assert_eq!(replacement_slot.owner.slot_index, 1);
        heap.with_mutator(|mutator| {
            for root in &roots {
                let _ = root.get(mutator);
            }
        });
    }

    #[test]
    fn finalization_run_commit_delays_word_reuse_by_a_later_destructor() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let (published_tx, published_rx) = mpsc::channel();
        let (values, live_root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<IncrementalReleaseDrop>().unwrap();
            let values = (0..=2 * u64::BITS as usize)
                .map(|index| {
                    allocator.alloc(IncrementalReleaseDrop {
                        heap: Arc::downgrade(&heap.inner),
                        drops: Arc::clone(&drops),
                        allocate_on_drop: index == u64::BITS as usize,
                        published: published_tx.clone(),
                    })
                })
                .collect::<Vec<_>>();
            let live_root = mutator.root(values[2 * u64::BITS as usize]);
            (values, live_root)
        });
        let first_owner = collector_slot(&heap, values[0]).owner;
        assert!(
            values.iter().all(|value| {
                collector_slot(&heap, *value).owner.location == first_owner.location
            })
        );

        let report = heap.collect_full().unwrap();
        assert_eq!(report.marked_slots(), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 2 * u64::BITS as usize);
        let (replacement_root, replacement_location, replacement_slot_index) = published_rx
            .recv()
            .expect("later destructor did not publish its fresh allocation");
        assert_eq!(replacement_location, first_owner.location);
        assert_eq!(replacement_slot_index, 2 * u64::BITS as usize + 1);
        assert_ne!(replacement_slot_index, first_owner.slot_index);
        assert!(
            heap.inner
                .data
                .lock()
                .unwrap()
                .finalization_batch
                .runs
                .is_empty()
        );

        heap.with_mutator(|mutator| {
            let _ = live_root.get(mutator);
            let _ = replacement_root.get(mutator);
        });
        drop(live_root);
        drop(replacement_root);
        heap.collect_full().unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 2 * u64::BITS as usize + 2);
    }

    #[test]
    fn pending_run_keeps_words_reserved_from_an_ordinary_mutator_until_commit() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let released_before = Arc::new(Barrier::new(2));
        let continue_finalization = Arc::new(Barrier::new(2));
        let (values, live_root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<ConcurrentReleaseDrop>().unwrap();
            let values = (0..=2 * u64::BITS as usize)
                .map(|index| {
                    allocator.alloc(ConcurrentReleaseDrop {
                        drops: Arc::clone(&drops),
                        pause_on_drop: index == u64::BITS as usize,
                        released_before: Arc::clone(&released_before),
                        continue_finalization: Arc::clone(&continue_finalization),
                    })
                })
                .collect::<Vec<_>>();
            let live_root = mutator.root(values[2 * u64::BITS as usize]);
            (values, live_root)
        });
        let first_owner = collector_slot(&heap, values[0]).owner;
        assert!(
            values.iter().all(|value| {
                collector_slot(&heap, *value).owner.location == first_owner.location
            })
        );

        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || heap.collect_full().unwrap()
        });
        released_before.wait();
        assert_eq!(
            heap.inner.coordinator_snapshot().phase,
            AdmissionPhase::Finalizing
        );
        assert_eq!(
            heap.activity(),
            HeapActivity {
                queued_finalizers: 0,
                running_finalizers: 2 * u64::BITS as usize,
            }
        );

        let (replacement_root, replacement_owner) = heap.with_mutator(|mutator| {
            assert!(
                catch_unwind(AssertUnwindSafe(|| mutator.root(values[0]))).is_err(),
                "the durable map must reject roots while its local run batch executes"
            );
            let replacement = mutator.allocator::<ConcurrentReleaseDrop>().unwrap().alloc(
                ConcurrentReleaseDrop {
                    drops: Arc::clone(&drops),
                    pause_on_drop: false,
                    released_before: Arc::clone(&released_before),
                    continue_finalization: Arc::clone(&continue_finalization),
                },
            );
            (
                mutator.root(replacement),
                collector_slot(&heap, replacement).owner,
            )
        });
        assert_eq!(replacement_owner.location, first_owner.location);
        assert_eq!(replacement_owner.slot_index, 2 * u64::BITS as usize + 1);
        assert_ne!(replacement_owner.slot_index, first_owner.slot_index);
        {
            let data = heap.inner.data.lock().unwrap();
            let target = data
                .arena
                .run_claim_target(first_owner.location)
                .expect("attached finalization run lost its arena target");
            assert!(data.finalization_batch.reserves_word(target, 0));
            assert!(data.arena.owner_slot_is_allocated(first_owner));
        }

        continue_finalization.wait();
        let report = collector.join().expect("collector worker panicked");
        assert_eq!(report.marked_slots(), 1);
        assert_eq!(report.finalized_slots(), 2 * u64::BITS as usize);
        assert_eq!(report.reclaimed_slots(), 2 * u64::BITS as usize);
        assert_eq!(heap.activity(), HeapActivity::default());
        assert_eq!(drops.load(Ordering::Relaxed), 2 * u64::BITS as usize);
        assert!(
            heap.inner
                .data
                .lock()
                .unwrap()
                .finalization_batch
                .runs
                .is_empty()
        );

        drop(live_root);
        drop(replacement_root);
        heap.collect_full().unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 2 * u64::BITS as usize + 2);
    }

    #[test]
    fn finalized_word_release_publishes_retirement_before_concurrent_claim() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let destructor_arrived = Arc::new(Barrier::new(2));
        let continue_finalization = Arc::new(Barrier::new(2));
        let (values, live_root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<ConcurrentReleaseDrop>().unwrap();
            let values = (0..=2 * u64::BITS as usize)
                .map(|index| {
                    allocator.alloc(ConcurrentReleaseDrop {
                        drops: Arc::clone(&drops),
                        pause_on_drop: index == u64::BITS as usize,
                        released_before: Arc::clone(&destructor_arrived),
                        continue_finalization: Arc::clone(&continue_finalization),
                    })
                })
                .collect::<Vec<_>>();
            let live_root = mutator.root(values[2 * u64::BITS as usize]);
            (values, live_root)
        });
        let first_owner = collector_slot(&heap, values[0]).owner;
        assert!(
            values.iter().all(|value| {
                collector_slot(&heap, *value).owner.location == first_owner.location
            })
        );

        let word_released = Arc::new(Barrier::new(2));
        let publish_frontier = Arc::new(Barrier::new(2));
        heap.inner.install_finalized_word_release_hook(
            first_owner.location,
            0,
            Arc::clone(&word_released),
            Arc::clone(&publish_frontier),
        );

        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || heap.collect_full().unwrap()
        });
        destructor_arrived.wait();

        let (allocator_ready_tx, allocator_ready_rx) = mpsc::channel();
        let (allocate_tx, allocate_rx) = mpsc::channel();
        let (replacement_tx, replacement_rx) = mpsc::channel();
        let allocator_worker = std::thread::spawn({
            let heap = heap.clone();
            let drops = Arc::clone(&drops);
            let destructor_arrived = Arc::clone(&destructor_arrived);
            let continue_finalization = Arc::clone(&continue_finalization);
            move || {
                heap.with_mutator(|mutator| {
                    let allocator = mutator.allocator::<ConcurrentReleaseDrop>().unwrap();
                    allocator_ready_tx.send(()).unwrap();
                    allocate_rx.recv().unwrap();
                    let replacement = allocator.alloc(ConcurrentReleaseDrop {
                        drops,
                        pause_on_drop: false,
                        released_before: destructor_arrived,
                        continue_finalization,
                    });
                    replacement_tx.send(replacement).unwrap();
                });
            }
        });
        allocator_ready_rx.recv().unwrap();

        continue_finalization.wait();
        word_released.wait();
        allocate_tx.send(()).unwrap();
        let replacement = replacement_rx
            .recv()
            .expect("concurrent claimant did not initialize the released word");
        publish_frontier.wait();

        allocator_worker.join().unwrap();
        let report = collector.join().unwrap();
        assert_eq!(report.marked_slots(), 1);
        assert_eq!(report.finalized_slots(), 2 * u64::BITS as usize);
        assert_eq!(drops.load(Ordering::Relaxed), 2 * u64::BITS as usize);

        let replacement_owner = collector_slot(&heap, replacement).owner;
        assert_eq!(replacement_owner.location, first_owner.location);
        assert_eq!(replacement_owner.slot_index, first_owner.slot_index);
        let replacement_root = heap.with_mutator(|mutator| mutator.root(replacement));

        drop(live_root);
        drop(replacement_root);
        heap.collect_full().unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 2 * u64::BITS as usize + 2);
    }

    #[test]
    fn wholly_dead_drop_runs_recycle_without_a_stale_frontier() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let dead = allocate(&heap, WideDropCounter(Arc::clone(&drops)));
        let dead_slot = collector_slot(&heap, dead);
        let class = internal_class::<WideDropCounter>(&heap);

        let report = heap.collect_full().unwrap();
        assert_eq!(report.marked_slots(), 0);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(class.claim_frontier(&heap.inner).is_none());

        let replacement = allocate(&heap, WideDropCounter(Arc::clone(&drops)));
        assert_eq!(
            collector_slot(&heap, replacement).owner.location,
            dead_slot.owner.location
        );
    }

    #[test]
    fn successful_finalization_releases_partial_and_detached_batch_ownership() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let (live, partial_dead, whole_dead, live_root) = heap.with_mutator(|mutator| {
            let partials = mutator.allocator::<DropCounter>().unwrap();
            let live = partials.alloc(DropCounter(Arc::clone(&drops)));
            let partial_dead = partials.alloc(DropCounter(Arc::clone(&drops)));
            let whole_dead = mutator
                .allocator::<WideDropCounter>()
                .unwrap()
                .alloc(WideDropCounter(Arc::clone(&drops)));
            (live, partial_dead, whole_dead, mutator.root(live))
        });
        let live_slot = collector_slot(&heap, live);
        let partial_dead_slot = collector_slot(&heap, partial_dead);
        let whole_dead_slot = collector_slot(&heap, whole_dead);
        assert_eq!(live_slot.owner.location, partial_dead_slot.owner.location);

        let first = heap.collect_full().unwrap();
        assert_eq!(first.marked_slots(), 1);
        assert_eq!(first.conservatively_retained_slots(), 0);
        assert_eq!(first.reclaimed_slots(), 2);
        assert_eq!(first.finalized_slots(), 2);
        assert_eq!(first.reclaimed_runs(), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        {
            let data = heap.inner.data.lock().unwrap();
            assert!(data.finalization_batch.runs.is_empty());
            assert_eq!(data.finalization_batch.pending_slot_count(), 0);
            assert_eq!(data.finalization_batch.detached_run_count(), 0);
            assert!(
                data.classes[class_index(live_slot.owner.class_id).unwrap()]
                    .contains_run(live_slot.owner.location)
            );
            assert!(
                !data.classes[class_index(whole_dead_slot.owner.class_id).unwrap()]
                    .contains_run(whole_dead_slot.owner.location)
            );
            assert!(data.free_runs.contains(&whole_dead_slot.owner.location));
        }

        heap.with_mutator(|mutator| {
            assert!(catch_unwind(AssertUnwindSafe(|| mutator.root(partial_dead))).is_err());
            assert!(catch_unwind(AssertUnwindSafe(|| mutator.root(whole_dead))).is_err());
            #[cfg(debug_assertions)]
            {
                // SAFETY: deliberately attempts access to a completed dead-set
                // identity to verify the debug boundary rejects it before
                // forming a reference.
                assert!(
                    catch_unwind(AssertUnwindSafe(|| unsafe {
                        partial_dead.get_unchecked(mutator)
                    }))
                    .is_err()
                );
            }
            let _ = live_root.get(mutator);
        });

        let second = heap.collect_full().unwrap();
        assert_eq!(second.marked_slots(), 1);
        assert_eq!(second.conservatively_retained_slots(), 0);
        assert_eq!(second.traced_objects(), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        {
            let data = heap.inner.data.lock().unwrap();
            assert!(data.finalization_batch.runs.is_empty());
            assert_eq!(data.finalization_batch.pending_slot_count(), 0);
            assert_eq!(data.finalization_batch.detached_run_count(), 0);
            assert_eq!(data.roots.len(), 1);
        }

        drop(live_root);
        drop(heap);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn later_dead_slots_finalize_after_the_prior_batch_was_released() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let (values, first_root, later_root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<DropCounter>().unwrap();
            let values = (0..3)
                .map(|_| allocator.alloc(DropCounter(Arc::clone(&drops))))
                .collect::<Vec<_>>();
            let first_root = mutator.root(values[0]);
            let later_root = mutator.root(values[2]);
            (values, first_root, later_root)
        });
        let slots = values
            .iter()
            .map(|value| collector_slot(&heap, *value))
            .collect::<Vec<_>>();
        assert!(
            slots
                .iter()
                .all(|slot| slot.owner.location == slots[0].owner.location)
        );

        let first = heap.collect_full().unwrap();
        assert_eq!(first.marked_slots(), 2);
        assert_eq!(first.conservatively_retained_slots(), 0);
        {
            let data = heap.inner.data.lock().unwrap();
            assert!(data.finalization_batch.runs.is_empty());
            assert_eq!(data.finalization_batch.pending_slot_count(), 0);
        }
        assert_eq!(drops.load(Ordering::Relaxed), 1);

        drop(later_root);
        let second = heap.collect_full().unwrap();
        assert_eq!(second.marked_slots(), 1);
        assert_eq!(second.conservatively_retained_slots(), 0);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        {
            let data = heap.inner.data.lock().unwrap();
            assert!(data.finalization_batch.runs.is_empty());
            assert_eq!(data.finalization_batch.pending_slot_count(), 0);
        }

        heap.with_mutator(|mutator| {
            assert!(catch_unwind(AssertUnwindSafe(|| mutator.root(values[2]))).is_err());
            let _ = first_root.get(mutator);
        });
        drop(first_root);
        drop(heap);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn concurrent_finalizing_entrant_cannot_root_a_detached_identity() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let dead = allocate(&heap, WideDropCounter(Arc::clone(&drops)));
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        let finalizing = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let collector = std::thread::spawn({
            let heap = heap.clone();
            let finalizing = Arc::clone(&finalizing);
            let release = Arc::clone(&release);
            move || {
                heap.inner
                    .run_synthetic_collection(
                        epoch,
                        false,
                        |_, _| {},
                        |_| {
                            assert_eq!(
                                heap.inner
                                    .data
                                    .lock()
                                    .unwrap()
                                    .finalization_batch
                                    .pending_slot_count(),
                                1
                            );
                            finalizing.wait();
                            release.wait();
                        },
                    )
                    .is_none()
            }
        });

        finalizing.wait();
        let rejected = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.with_mutator(|mutator| {
                    catch_unwind(AssertUnwindSafe(|| mutator.root(dead))).is_err()
                })
            }
        })
        .join()
        .expect("root-attempt worker panicked outside the checked boundary");
        assert!(rejected);
        assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 1);

        release.wait();
        assert!(collector.join().unwrap());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        drop(heap);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn managed_destructor_runs_outside_locks_with_the_recursive_finalizer_mutator() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let published = Arc::new(Mutex::new(None));
        let dead = allocate(
            &heap,
            AllocatingDrop {
                heap: heap.clone(),
                drops: Arc::clone(&drops),
                published: Arc::clone(&published),
                allocate_on_drop: true,
                panic_after_drop: false,
            },
        );
        let dead_slot = collector_slot(&heap, dead);

        let report = heap.collect_full().unwrap();

        assert_eq!(report.marked_slots(), 0);
        assert_eq!(report.reclaimed_slots(), 1);
        assert_eq!(report.finalized_slots(), 1);
        assert_eq!(report.reclaimed_runs(), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(
            !heap
                .inner
                .data
                .lock()
                .unwrap()
                .arena
                .owner_slot_is_allocated(dead_slot.owner),
            "the allocation bit must retire only after successful Drop"
        );
        let root = published
            .lock()
            .unwrap()
            .take()
            .expect("managed Drop must publish its fresh allocation");
        heap.with_mutator(|mutator| assert_eq!(*root.get(mutator), 73));
        drop(root);
        drop(heap);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn finalizer_run_activation_sets_the_completed_pressure_baseline() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let published = Arc::new(Mutex::new(None));
        let (live, dead, live_root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<AllocatingDrop>().unwrap();
            let live = allocator.alloc(AllocatingDrop {
                heap: heap.clone(),
                drops: Arc::clone(&drops),
                published: Arc::clone(&published),
                allocate_on_drop: false,
                panic_after_drop: false,
            });
            let dead = allocator.alloc(AllocatingDrop {
                heap: heap.clone(),
                drops: Arc::clone(&drops),
                published: Arc::clone(&published),
                allocate_on_drop: true,
                panic_after_drop: false,
            });
            (live, dead, mutator.root(live))
        });
        assert_eq!(
            collector_slot(&heap, live).owner.location,
            collector_slot(&heap, dead).owner.location
        );
        {
            let mut data = heap.inner.data.lock().unwrap();
            assert_eq!(data.allocation_pressure.assigned_runs, 1);
            data.allocation_pressure.high_water_mark = 2;
        }

        let report = heap.collect_full().unwrap();

        assert_eq!(report.reclaimed_slots(), 1);
        assert_eq!(report.finalized_slots(), 1);
        assert_eq!(report.reclaimed_runs(), 0);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(heap.activity(), HeapActivity::default());
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressureSnapshot {
                assigned_runs: 2,
                high_water_mark: survivor_run_high_water_mark(
                    2,
                    SURVIVOR_GROWTH_NUMERATOR,
                    SURVIVOR_GROWTH_DENOMINATOR,
                ),
                collection_requested: false,
            }
        );
        let published_root = published
            .lock()
            .unwrap()
            .take()
            .expect("finalizer did not publish its fresh allocation");
        heap.with_mutator(|mutator| assert_eq!(*published_root.get(mutator), 73));

        drop(published_root);
        drop(live_root);
        heap.collect_full().unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn panicking_finalizer_publishes_pressure_without_a_completion_report() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let published = Arc::new(Mutex::new(None));
        let (live, dead, live_root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<AllocatingDrop>().unwrap();
            let live = allocator.alloc(AllocatingDrop {
                heap: heap.clone(),
                drops: Arc::clone(&drops),
                published: Arc::clone(&published),
                allocate_on_drop: false,
                panic_after_drop: false,
            });
            let dead = allocator.alloc(AllocatingDrop {
                heap: heap.clone(),
                drops: Arc::clone(&drops),
                published: Arc::clone(&published),
                allocate_on_drop: true,
                panic_after_drop: true,
            });
            (live, dead, mutator.root(live))
        });
        assert_eq!(
            collector_slot(&heap, live).owner.location,
            collector_slot(&heap, dead).owner.location
        );
        {
            let mut data = heap.inner.data.lock().unwrap();
            assert_eq!(data.allocation_pressure.assigned_runs, 1);
            data.allocation_pressure.high_water_mark = 2;
        }

        let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
            .expect_err("allocating finalizer must propagate its panic");
        assert_eq!(
            panic_string(panic.as_ref()),
            "injected allocating finalizer panic"
        );
        assert_failed_collection_restored(&heap);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(heap.activity(), HeapActivity::default());
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressureSnapshot {
                assigned_runs: 2,
                high_water_mark: survivor_run_high_water_mark(
                    2,
                    SURVIVOR_GROWTH_NUMERATOR,
                    SURVIVOR_GROWTH_DENOMINATOR,
                ),
                collection_requested: true,
            }
        );
        let published_root = published
            .lock()
            .unwrap()
            .take()
            .expect("panicking finalizer lost its published allocation");
        heap.with_mutator(|mutator| assert_eq!(*published_root.get(mutator), 73));

        drop(published_root);
        drop(live_root);
        let cleanup = heap.collect_full().unwrap();
        assert_eq!(cleanup.reclaimed_slots(), 2);
        assert_eq!(cleanup.finalized_slots(), 1);
        assert_eq!(cleanup.reclaimed_runs(), 2);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn managed_destructor_panic_retires_one_and_defers_the_untouched_batch() {
        let heap = Heap::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let panicking = allocate(
            &heap,
            SelectivePanickingDrop {
                id: 0,
                panic: true,
                events: Arc::clone(&events),
            },
        );
        let deferred = allocate(
            &heap,
            SelectivePanickingDrop {
                id: 1,
                panic: false,
                events: Arc::clone(&events),
            },
        );
        let panicking_slot = collector_slot(&heap, panicking);
        let deferred_slot = collector_slot(&heap, deferred);
        assert_eq!(panicking_slot.owner.location, deferred_slot.owner.location);

        let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
            .expect_err("managed destructor must propagate its panic");
        assert_eq!(
            panic_string(panic.as_ref()),
            "injected managed destructor panic"
        );
        assert_eq!(*events.lock().unwrap(), vec![0]);
        assert_failed_collection_restored(&heap);
        assert_eq!(
            heap.activity(),
            HeapActivity {
                queued_finalizers: 1,
                running_finalizers: 0,
            }
        );
        {
            let data = heap.inner.data.lock().unwrap();
            let metadata = metadata_for::<SelectivePanickingDrop>();
            assert_eq!(data.finalization_batch.pending_slot_count(), 1);
            assert_eq!(data.finalization_batch.detached_run_count(), 1);
            assert!(
                data.finalization_batch
                    .pending_metadata_at(&data.arena, deferred.erase().as_ptr().as_ptr() as usize,)
                    .is_some()
            );
            assert!(
                !data.arena.owner_slot_is_allocated(panicking_slot.owner),
                "a panicking destructor must terminally retire its allocation"
            );
            assert!(data.arena.owner_slot_is_allocated(deferred_slot.owner));
            assert!(matches!(
                validate_rootable_in_state(&data, panicking.erase(), metadata),
                Err(RootValidationError::ForeignHeap | RootValidationError::Unallocated)
            ));
            assert!(matches!(
                validate_rootable_in_state(&data, deferred.erase(), metadata),
                Err(RootValidationError::PendingFinalization)
            ));
        }

        let retry = heap.collect_full().unwrap();
        assert_eq!(retry.epoch(), 1);
        assert_eq!(retry.root_entries(), 0);
        assert_eq!(retry.traced_objects(), 0);
        assert_eq!(retry.marked_slots(), 1);
        assert_eq!(retry.conservatively_retained_slots(), 1);
        assert_eq!(retry.finalized_slots(), 1);
        assert_eq!(retry.reclaimed_slots(), 1);
        assert_eq!(retry.reclaimed_runs(), 1);
        assert_eq!(heap.activity(), HeapActivity::default());
        assert_eq!(*events.lock().unwrap(), vec![0, 1]);
        assert_eq!(
            heap.inner
                .data
                .lock()
                .unwrap()
                .finalization_batch
                .pending_slot_count(),
            0
        );
        heap.with_mutator(|mutator| {
            assert!(catch_unwind(AssertUnwindSafe(|| mutator.root(panicking))).is_err());
            assert!(catch_unwind(AssertUnwindSafe(|| mutator.root(deferred))).is_err());
        });

        drop(heap);
        assert_eq!(*events.lock().unwrap(), vec![0, 1]);
    }

    #[test]
    fn managed_destructor_panic_keeps_an_attached_word_reserved_until_retry() {
        let heap = Heap::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let (panicking, deferred, live_root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<SelectivePanickingDrop>().unwrap();
            let panicking = allocator.alloc(SelectivePanickingDrop {
                id: 0,
                panic: true,
                events: Arc::clone(&events),
            });
            let deferred = allocator.alloc(SelectivePanickingDrop {
                id: 1,
                panic: false,
                events: Arc::clone(&events),
            });
            let live = allocator.alloc(SelectivePanickingDrop {
                id: 2,
                panic: false,
                events: Arc::clone(&events),
            });
            (panicking, deferred, mutator.root(live))
        });
        let panicking_slot = collector_slot(&heap, panicking);
        let deferred_slot = collector_slot(&heap, deferred);
        assert_eq!(panicking_slot.owner.location, deferred_slot.owner.location);
        assert_eq!(
            panicking_slot.owner.slot_index / u64::BITS as usize,
            deferred_slot.owner.slot_index / u64::BITS as usize
        );

        let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
            .expect_err("managed destructor must propagate its panic");
        assert_eq!(
            panic_string(panic.as_ref()),
            "injected managed destructor panic"
        );
        assert_eq!(*events.lock().unwrap(), vec![0]);
        assert_failed_collection_restored(&heap);
        {
            let data = heap.inner.data.lock().unwrap();
            let target = data
                .arena
                .run_claim_target(panicking_slot.owner.location)
                .expect("attached pending run must retain arena topology");
            let word_index = panicking_slot.owner.slot_index / u64::BITS as usize;
            assert_eq!(data.finalization_batch.pending_slot_count(), 1);
            assert_eq!(data.finalization_batch.detached_run_count(), 0);
            assert!(data.finalization_batch.reserves_word(target, word_index));
            assert!(!data.arena.owner_slot_is_allocated(panicking_slot.owner));
            assert!(data.arena.owner_slot_is_allocated(deferred_slot.owner));
        }

        let retry = heap.collect_full().unwrap();
        assert_eq!(retry.epoch(), 1);
        assert_eq!(retry.marked_slots(), 2);
        assert_eq!(retry.conservatively_retained_slots(), 1);
        assert_eq!(*events.lock().unwrap(), vec![0, 1]);
        {
            let data = heap.inner.data.lock().unwrap();
            assert_eq!(data.finalization_batch.pending_slot_count(), 0);
            assert!(data.finalization_batch.runs.is_empty());
        }

        let replacement = allocate(
            &heap,
            SelectivePanickingDrop {
                id: 3,
                panic: false,
                events: Arc::clone(&events),
            },
        );
        let replacement_slot = collector_slot(&heap, replacement);
        assert_eq!(
            replacement_slot.owner.location,
            panicking_slot.owner.location
        );
        assert_eq!(
            replacement_slot.owner.slot_index,
            panicking_slot.owner.slot_index
        );

        drop(live_root);
        drop(heap);
        let mut completed = events.lock().unwrap().clone();
        completed.sort_unstable();
        assert_eq!(completed, vec![0, 1, 2, 3]);
    }

    #[test]
    fn retry_merges_new_same_run_and_same_word_finalizers_without_duplication() {
        let heap = Heap::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let (values, mut roots) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<SelectivePanickingDrop>().unwrap();
            let values = (0..=u64::BITS as usize)
                .map(|id| {
                    allocator.alloc(SelectivePanickingDrop {
                        id,
                        panic: id == 0,
                        events: Arc::clone(&events),
                    })
                })
                .collect::<Vec<_>>();
            let roots = values[2..]
                .iter()
                .map(|value| mutator.root(*value))
                .collect::<Vec<_>>();
            (values, roots)
        });
        let slots = values
            .iter()
            .map(|value| collector_slot(&heap, *value).owner)
            .collect::<Vec<_>>();
        assert!(
            slots
                .iter()
                .all(|owner| owner.location == slots[0].location)
        );
        assert_eq!(slots[1].slot_index / u64::BITS as usize, 0);
        assert_eq!(slots[2].slot_index / u64::BITS as usize, 0);
        assert_eq!(slots[64].slot_index / u64::BITS as usize, 1);

        let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
            .expect_err("the first destructor must interrupt finalization");
        assert_eq!(
            panic_string(panic.as_ref()),
            "injected managed destructor panic"
        );
        assert_eq!(*events.lock().unwrap(), vec![0]);
        assert_failed_collection_restored(&heap);
        assert_eq!(
            heap.inner
                .data
                .lock()
                .unwrap()
                .finalization_batch
                .pending_slot_count(),
            1
        );

        let same_word_root = roots.remove(0);
        let new_word_root = roots
            .pop()
            .expect("the second allocation word must retain a root");
        drop(same_word_root);
        drop(new_word_root);

        let epoch = heap.inner.elect_idle_collection_for_test();
        assert!(
            heap.inner
                .run_synthetic_collection(
                    epoch,
                    false,
                    |post_mark, _| {
                        assert_eq!(post_mark.summary.conservatively_retained_slots, 1);
                    },
                    |_| {
                        let data = heap.inner.data.lock().unwrap();
                        let run = data
                            .finalization_batch
                            .runs
                            .get(&slots[0].location)
                            .expect("retry lost its attached finalization run");
                        assert_eq!(run.pending_slot_count(), 3);
                        assert_eq!(
                            run.pending_words
                                .get(&(slots[1].slot_index / u64::BITS as usize)),
                            Some(
                                &((1_u64 << (slots[1].slot_index % u64::BITS as usize))
                                    | (1_u64 << (slots[2].slot_index % u64::BITS as usize)))
                            )
                        );
                        assert_eq!(
                            run.pending_words
                                .get(&(slots[64].slot_index / u64::BITS as usize)),
                            Some(&(1_u64 << (slots[64].slot_index % u64::BITS as usize)))
                        );
                    },
                )
                .is_none()
        );

        let mut completed = events.lock().unwrap().clone();
        completed.sort_unstable();
        assert_eq!(completed, vec![0, 1, 2, 64]);
        assert!(
            heap.inner
                .data
                .lock()
                .unwrap()
                .finalization_batch
                .runs
                .is_empty()
        );

        drop(roots);
        drop(heap);
    }

    #[test]
    fn repeated_destructor_panics_make_one_terminal_step_per_collection() {
        let heap = Heap::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let values = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<SelectivePanickingDrop>().unwrap();
            (0..3)
                .map(|id| {
                    allocator.alloc(SelectivePanickingDrop {
                        id,
                        panic: true,
                        events: Arc::clone(&events),
                    })
                })
                .collect::<Vec<_>>()
        });
        let slots = values
            .iter()
            .map(|value| collector_slot(&heap, *value))
            .collect::<Vec<_>>();
        assert!(
            slots
                .iter()
                .all(|slot| slot.owner.location == slots[0].owner.location)
        );

        for completed in 1..=values.len() {
            let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
                .expect_err("each pending destructor must propagate its own panic");
            assert_eq!(
                panic_string(panic.as_ref()),
                "injected managed destructor panic"
            );
            assert_eq!(*events.lock().unwrap(), (0..completed).collect::<Vec<_>>());
            assert_failed_collection_restored(&heap);
            let data = heap.inner.data.lock().unwrap();
            assert_eq!(
                data.finalization_batch.pending_slot_count(),
                values.len() - completed
            );
            for (index, slot) in slots.iter().enumerate() {
                assert_eq!(
                    data.arena.owner_slot_is_allocated(slot.owner),
                    index >= completed
                );
            }
        }

        let completed = heap.collect_full().unwrap();
        assert_eq!(completed.epoch(), 1);
        assert_eq!(completed.marked_slots(), 0);
        assert_eq!(completed.conservatively_retained_slots(), 0);
        assert_eq!(*events.lock().unwrap(), vec![0, 1, 2]);
        drop(heap);
        assert_eq!(*events.lock().unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn multiple_whole_finalization_detachments_recycle_without_dispatch_order() {
        let heap = Heap::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let values = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<WideDropCounter>().unwrap();
            (0..3)
                .map(|_| allocator.alloc(WideDropCounter(Arc::clone(&drops))))
                .collect::<Vec<_>>()
        });
        let locations = values
            .iter()
            .map(|value| collector_slot(&heap, *value).owner.location)
            .collect::<Vec<_>>();
        let class_id = collector_slot(&heap, values[1]).owner.class_id;
        let root = heap.with_mutator(|mutator| mutator.root(values[1]));

        heap.collect_full().unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 2);

        let data = heap.inner.data.lock().unwrap();
        assert_eq!(
            data.classes[class_index(class_id).unwrap()]
                .runs()
                .iter()
                .map(|run| run.location)
                .collect::<Vec<_>>(),
            vec![locations[1]]
        );
        assert!(data.finalization_batch.runs.is_empty());
        assert_eq!(
            data.free_runs.iter().copied().collect::<HashSet<_>>(),
            HashSet::from([locations[0], locations[2]])
        );
        drop(data);

        drop(root);
        drop(heap);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn panic_after_dead_set_classification_publishes_no_allocator_change() {
        let heap = Heap::new();
        let live = allocate(&heap, 10_u64);
        let _dead = allocate(&heap, 20_u64);
        let root = heap.with_mutator(|mutator| mutator.root(live));
        let captured = Mutex::new(None);

        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.inner.run_synthetic_collection(
                epoch,
                false,
                |post_mark, data| {
                    assert_eq!(post_mark.dead_set.live_slots, 1);
                    assert_eq!(post_mark.dead_set.no_drop_dead_slots, 1);
                    let snapshot = classification_state_snapshot(&heap, data);
                    *captured.lock().unwrap() = Some(snapshot);
                    panic!("injected panic after dead-set classification");
                },
                |_| {},
            );
        }));
        assert!(panic.is_err());
        assert_failed_collection_restored(&heap);

        let expected = captured
            .lock()
            .unwrap()
            .take()
            .expect("post-mark snapshot must precede the injected panic");
        let data = heap.inner.data.lock().unwrap();
        assert_eq!(classification_state_snapshot(&heap, &data), expected);
        drop(data);

        heap.with_mutator(|mutator| assert_eq!(*root.get(mutator), 10));
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            1
        );
    }

    #[test]
    fn panic_after_destructive_topology_mutation_does_not_reopen_the_heap() {
        TOPOLOGY_POISON_DESTRUCTOR_ATTEMPTS.store(0, Ordering::Relaxed);
        let heap = Heap::new();
        let live = allocate(&heap, 10_u64);
        let _dead = allocate(&heap, 20_u64);
        let root = heap.with_mutator(|mutator| mutator.root(live));
        let _dropping = allocate(
            &heap,
            PoisonedHeapDrop {
                attempts: &TOPOLOGY_POISON_DESTRUCTOR_ATTEMPTS,
            },
        );

        heap.inner.inject_panic_after_topology_mutation();
        let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
            .expect_err("injected topology panic must interrupt collection");
        assert_eq!(
            panic_string(panic.as_ref()),
            "injected panic after destructive topology mutation"
        );

        let entry = catch_unwind(AssertUnwindSafe(|| heap.with_mutator(|_| {})));
        assert!(
            entry.is_err(),
            "an irreversible collection panic must not reopen mutator admission"
        );
        assert_eq!(
            panic_string(entry.unwrap_err().as_ref()),
            "managed heap is permanently poisoned"
        );
        assert_eq!(heap.collect_full(), Err(CollectionError::Poisoned));
        let activity = catch_unwind(AssertUnwindSafe(|| heap.activity()));
        assert_eq!(
            panic_string(activity.unwrap_err().as_ref()),
            "managed heap is permanently poisoned"
        );
        let request = catch_unwind(AssertUnwindSafe(|| heap.request_collection()));
        assert_eq!(
            panic_string(request.unwrap_err().as_ref()),
            "managed heap is permanently poisoned"
        );
        assert!(heap.inner.is_poisoned());
        assert_eq!(
            heap.inner.coordinator_snapshot().phase,
            AdmissionPhase::Poisoned
        );

        drop(root);
        drop(heap);
        assert_eq!(
            TOPOLOGY_POISON_DESTRUCTOR_ATTEMPTS.load(Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn panic_before_finalizer_commit_permanently_poisons_without_redispatch() {
        FINALIZER_COMMIT_POISON_DESTRUCTOR_ATTEMPTS.store(0, Ordering::Relaxed);
        let heap = Heap::new();
        let _dropping = allocate(
            &heap,
            PoisonedHeapDrop {
                attempts: &FINALIZER_COMMIT_POISON_DESTRUCTOR_ATTEMPTS,
            },
        );

        heap.inner.inject_panic_after_finalizer_terminal_recording();
        let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
            .expect_err("injected finalizer-commit panic must interrupt collection");
        assert_eq!(
            panic_string(panic.as_ref()),
            "injected panic after finalizer terminal recording"
        );
        assert_eq!(
            FINALIZER_COMMIT_POISON_DESTRUCTOR_ATTEMPTS.load(Ordering::Relaxed),
            1,
            "the erased destructor must run before the injected commit panic"
        );
        assert_eq!(
            heap.inner.poisoned_outer_mutator_release_count(),
            1,
            "the run-local guard must publish poison before finalizer admission is released"
        );

        let entry = catch_unwind(AssertUnwindSafe(|| heap.with_mutator(|_| {})));
        assert_eq!(
            panic_string(entry.unwrap_err().as_ref()),
            "managed heap is permanently poisoned"
        );
        assert_eq!(heap.collect_full(), Err(CollectionError::Poisoned));
        let activity = catch_unwind(AssertUnwindSafe(|| heap.activity()));
        assert_eq!(
            panic_string(activity.unwrap_err().as_ref()),
            "managed heap is permanently poisoned"
        );
        let request = catch_unwind(AssertUnwindSafe(|| heap.request_collection()));
        assert_eq!(
            panic_string(request.unwrap_err().as_ref()),
            "managed heap is permanently poisoned"
        );
        assert!(heap.inner.is_poisoned());
        assert_eq!(
            heap.inner.coordinator_snapshot().phase,
            AdmissionPhase::Poisoned
        );

        drop(heap);
        assert_eq!(
            FINALIZER_COMMIT_POISON_DESTRUCTOR_ATTEMPTS.load(Ordering::Relaxed),
            1,
            "terminal release must not redispatch an uncertain identity"
        );
    }

    #[test]
    fn irreversible_topology_panic_wakes_waiters_into_permanent_poison() {
        let heap = Heap::new();
        let _value = allocate(&heap, 10_u64);
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        heap.inner.inject_panic_after_topology_mutation();

        let (at_boundary_tx, at_boundary_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                catch_unwind(AssertUnwindSafe(|| {
                    heap.inner.run_synthetic_collection(
                        epoch,
                        false,
                        |_, _| {
                            at_boundary_tx.send(()).unwrap();
                            release_rx.recv().unwrap();
                        },
                        |_| {},
                    );
                }))
            }
        });
        at_boundary_rx.recv().unwrap();

        let mutator_waiter = std::thread::spawn({
            let heap = heap.clone();
            move || catch_unwind(AssertUnwindSafe(|| heap.with_mutator(|_| {})))
        });
        let collection_waiter = std::thread::spawn({
            let heap = heap.clone();
            move || heap.collect_full()
        });
        heap.inner.wait_for_blocked_outer_mutators(1);
        heap.inner.wait_for_collection_waiters(1);

        release_tx.send(()).unwrap();
        let collector_panic = collector
            .join()
            .unwrap()
            .expect_err("collector must propagate the injected topology panic");
        assert_eq!(
            panic_string(collector_panic.as_ref()),
            "injected panic after destructive topology mutation"
        );
        let mutator_panic = mutator_waiter
            .join()
            .unwrap()
            .expect_err("blocked mutator must reject permanent poison");
        assert_eq!(
            panic_string(mutator_panic.as_ref()),
            "managed heap is permanently poisoned"
        );
        assert_eq!(
            collection_waiter.join().unwrap(),
            Err(CollectionError::Poisoned)
        );
    }

    #[test]
    fn successful_post_sweep_publishes_final_leases_frontiers_and_epoch() {
        let heap = Heap::new();
        let plain = allocate(&heap, 10_u64);
        let plain_root = heap.with_mutator(|mutator| mutator.root(plain));
        let drops = Arc::new(AtomicUsize::new(0));
        let dropping = allocate(&heap, DropCounter(Arc::clone(&drops)));
        let dropping_root = heap.with_mutator(|mutator| mutator.root(dropping));

        let before = Mutex::new(None);
        let after = Mutex::new(None);
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        assert!(
            heap.inner
                .run_synthetic_collection(
                    epoch,
                    false,
                    |_, data| {
                        *before.lock().unwrap() = Some(classification_state_snapshot(&heap, data));
                    },
                    |_| {
                        let data = heap.inner.data.lock().unwrap();
                        *after.lock().unwrap() = Some(classification_state_snapshot(&heap, &data));
                    },
                )
                .is_none()
        );

        let before = before.lock().unwrap().take().unwrap();
        let after = after.lock().unwrap().take().unwrap();
        assert_eq!(before.pressure.assigned_runs, 2);
        assert_eq!(before.pressure.high_water_mark, FIXED_SURVIVOR_RUN_HEADROOM);
        assert!(before.pressure.collection_requested);
        assert_eq!(
            after.pressure,
            AllocationPressureSnapshot {
                assigned_runs: 2,
                high_water_mark: FIXED_SURVIVOR_RUN_HEADROOM,
                // The completed request is acknowledged after the finalizer
                // callback returns.
                collection_requested: true,
            }
        );
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressureSnapshot {
                assigned_runs: 2,
                high_water_mark: survivor_run_high_water_mark(
                    2,
                    SURVIVOR_GROWTH_NUMERATOR,
                    SURVIVOR_GROWTH_DENOMINATOR,
                ),
                collection_requested: false,
            }
        );
        assert_eq!(
            after.allocation_lease_epoch.get(),
            before.allocation_lease_epoch.get() + 1
        );
        assert_eq!(before.classes.len(), after.classes.len());
        for (before, after) in before.classes.iter().zip(&after.classes) {
            assert_eq!(before.class_id, after.class_id);
            assert_eq!(before.metadata, after.metadata);
            assert_eq!(before.runs, after.runs);
            assert!(before.frontier.is_some());
            assert_eq!(after.frontier, before.frontier);
        }
        assert_eq!(before.runs.len(), after.runs.len());
        for (before, after) in before.runs.iter().zip(&after.runs) {
            assert_eq!(before.class_id, after.class_id);
            assert_eq!(before.location, after.location);
            assert_eq!(before.geometry, after.geometry);
            assert_eq!(before.allocations, after.allocations);
            assert_eq!(allocation_bytes(before), allocation_bytes(after));
            assert_eq!(mark_bytes(before), mark_bytes(after));
            assert_eq!(
                lease_words(after),
                vec![0; after.geometry.lease_bitmap.word_len],
                "partially occupied runs must publish their directly claimable words"
            );
        }
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        heap.with_mutator(|mutator| {
            assert_eq!(*plain_root.get(mutator), 10);
            let _ = dropping_root.get(mutator);
        });
    }

    #[test]
    fn next_outer_entry_discards_a_stale_cursor_before_claiming_the_rebuilt_view() {
        let heap = Heap::new();
        let (first_tx, first_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let worker = std::thread::spawn({
            let heap = heap.clone();
            move || {
                let first = heap.with_mutator(|mutator| {
                    let value = mutator.allocator::<u64>().unwrap().alloc(10);
                    let root = mutator.root(value);
                    let location = collector_slot(&heap, value).owner.location;
                    (root, location)
                });
                let first_cache = cache_snapshot(&heap.inner).unwrap();
                first_tx.send((first, first_cache)).unwrap();
                resume_rx.recv().unwrap();

                let second_location = heap.with_mutator(|mutator| {
                    let value = mutator.allocator::<u64>().unwrap().alloc(20);
                    collector_slot(&heap, value).owner.location
                });
                second_tx
                    .send((second_location, cache_snapshot(&heap.inner).unwrap()))
                    .unwrap();
            }
        });

        let ((root, first_location), first_cache) = first_rx.recv().unwrap();
        assert_eq!(first_cache.captured_epoch, AllocationLeaseEpoch::INITIAL);
        assert_eq!(first_cache.cursor_count, 1);
        let claims_before_collection = heap.inner.allocation_cursor_claim_count();
        let report = heap.collect_full().unwrap();
        assert_eq!(report.marked_slots(), 1);
        let collection_epoch = heap.inner.current_allocation_lease_epoch();
        assert_ne!(collection_epoch, first_cache.captured_epoch);

        resume_tx.send(()).unwrap();
        let (second_location, second_cache) = second_rx.recv().unwrap();
        worker.join().unwrap();
        assert_eq!(second_location, first_location);
        assert_eq!(second_cache.captured_epoch, collection_epoch);
        assert_eq!(second_cache.cursor_count, 1);
        assert_eq!(
            heap.inner.allocation_cursor_claim_count(),
            claims_before_collection + 1,
            "the worker must claim the rebuilt view instead of using its stale cursor"
        );

        let data = heap.inner.data.lock().unwrap();
        assert_eq!(data.arena.allocated_slot_pointers(first_location).len(), 2);
        drop(data);
        heap.with_mutator(|mutator| assert_eq!(*root.get(mutator), 10));
    }

    #[test]
    fn wholly_dead_no_drop_runs_reset_and_reuse_across_allocation_classes() {
        let heap = Heap::new();
        let wide_class = internal_class::<WideSlot>(&heap);
        let (wide_values, wide_roots) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<WideSlot>().unwrap();
            let values = (0..5_u64)
                .map(|value| allocator.alloc(WideSlot { value }))
                .collect::<Vec<_>>();
            let roots = [0_usize, 2, 4]
                .into_iter()
                .map(|index| mutator.root(values[index]))
                .collect::<Vec<_>>();
            (values, roots)
        });
        let wide_slots = wide_values
            .iter()
            .copied()
            .map(|value| collector_slot(&heap, value))
            .collect::<Vec<_>>();
        let wide_locations = wide_slots
            .iter()
            .map(|slot| slot.owner.location)
            .collect::<Vec<_>>();
        assert_eq!(
            wide_locations.iter().copied().collect::<HashSet<_>>().len(),
            5
        );

        let (plain_live, _plain_dead) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            (allocator.alloc(10), allocator.alloc(20))
        });
        let plain_slot = collector_slot(&heap, plain_live);
        let plain_root = heap.with_mutator(|mutator| mutator.root(plain_live));

        let drops = Arc::new(AtomicUsize::new(0));
        let dropping_dead = allocate(&heap, DropCounter(Arc::clone(&drops)));
        let dropping_slot = collector_slot(&heap, dropping_dead);

        let report = heap.collect_full().unwrap();
        assert_eq!(report.marked_slots(), 4);
        assert_eq!(report.reclaimed_slots(), 4);
        assert_eq!(report.finalized_slots(), 1);
        assert_eq!(report.reclaimed_runs(), 3);
        assert_eq!(drops.load(Ordering::Relaxed), 1);

        let data = heap.inner.data.lock().unwrap();
        let retained = data.classes[class_index(wide_class.id()).unwrap()]
            .runs()
            .iter()
            .map(|run| run.location)
            .collect::<Vec<_>>();
        assert_eq!(
            retained,
            vec![wide_locations[0], wide_locations[2], wide_locations[4]],
            "retirement must preserve the order of surviving class runs"
        );
        assert!(data.classes[class_index(wide_class.id()).unwrap()].frontier_is_withdrawn());

        assert!(data.retired_no_drop_runs.is_empty());
        assert_eq!(
            data.free_runs,
            vec![
                wide_locations[1],
                wide_locations[3],
                dropping_slot.owner.location,
            ],
            "reset locations must enter the one heap-wide free-run pool"
        );
        let free_before_reuse = data.free_runs.iter().copied().collect::<HashSet<_>>();
        for location in &data.free_runs {
            assert!(data.arena.run_is_empty_for_test(*location));
            assert!(data.arena.run_claim_target(*location).is_none());
            assert!(
                data.arena
                    .run_side_metadata_for_test(*location, wide_slots[0].owner.geometry)
                    .iter()
                    .all(|byte| *byte == 0),
                "reset run retained old allocation, lease, or mark state"
            );
        }

        assert_eq!(
            data.classes[class_index(plain_slot.owner.class_id).unwrap()]
                .runs()
                .len(),
            1,
            "a partially live no-drop run must remain in its class"
        );
        assert!(
            !data.classes[class_index(dropping_slot.owner.class_id).unwrap()]
                .contains_run(dropping_slot.owner.location),
            "a wholly dead drop-bearing run must leave ordinary class topology"
        );
        assert!(data.finalization_batch.runs.is_empty());
        for index in [1_usize, 3] {
            assert!(
                resolve_slot_in_state(&data, wide_values[index].erase().as_ptr().as_ptr() as usize)
                    .is_none(),
                "retired no-drop slots must leave published class topology"
            );
        }
        drop(data);

        assert!(wide_class.claim_frontier(&heap.inner).is_none());
        let replacement = allocate(&heap, BitmapBoundarySlot { _value: 99 });
        let replacement_slot = collector_slot(&heap, replacement);
        assert!(
            free_before_reuse.contains(&replacement_slot.owner.location),
            "recycled storage must be preferred over virgin arena capacity"
        );
        assert_ne!(replacement_slot.owner.class_id, wide_class.id());
        {
            let data = heap.inner.data.lock().unwrap();
            assert_eq!(data.free_runs.len(), 2);
            assert!(!data.free_runs.contains(&replacement_slot.owner.location));
            assert!(
                data.classes[class_index(wide_class.id()).unwrap()]
                    .runs()
                    .iter()
                    .all(|run| run.location != replacement_slot.owner.location),
                "old class recovered authority over a retyped run"
            );
            assert!(
                data.classes[class_index(replacement_slot.owner.class_id).unwrap()]
                    .contains_run(replacement_slot.owner.location),
                "new class did not receive the retyped run"
            );
        }
        heap.with_mutator(|mutator| {
            assert_eq!(wide_roots[0].get(mutator).value, 0);
            assert_eq!(wide_roots[1].get(mutator).value, 2);
            assert_eq!(wide_roots[2].get(mutator).value, 4);
            assert_eq!(*plain_root.get(mutator), 10);
        });
    }

    #[test]
    fn completed_sweep_publishes_survivor_assigned_run_baseline() {
        let heap = Heap::new();
        let (values, roots) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<WideSlot>().unwrap();
            let values = (0..4_u64)
                .map(|value| allocator.alloc(WideSlot { value }))
                .collect::<Vec<_>>();
            let roots = [0_usize, 2, 3]
                .into_iter()
                .map(|index| mutator.root(values[index]))
                .collect::<Vec<_>>();
            (values, roots)
        });
        let dead_location = collector_slot(&heap, values[1]).owner.location;

        let report = heap.collect_full().unwrap();

        assert_eq!(report.marked_slots(), 3);
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressureSnapshot {
                assigned_runs: 3,
                high_water_mark: survivor_run_high_water_mark(
                    3,
                    SURVIVOR_GROWTH_NUMERATOR,
                    SURVIVOR_GROWTH_DENOMINATOR,
                ),
                collection_requested: false,
            }
        );
        let data = heap.inner.data.lock().unwrap();
        assert_eq!(data.free_runs, vec![dead_location]);
        assert_eq!(data.arena.run_capacity(), crate::arena::RUNS_PER_CHUNK);
        drop(data);
        heap.with_mutator(|mutator| {
            assert_eq!(roots[0].get(mutator).value, 0);
            assert_eq!(roots[1].get(mutator).value, 2);
            assert_eq!(roots[2].get(mutator).value, 3);
        });
    }

    #[test]
    fn recycled_run_activation_crosses_pressure_target_once() {
        let heap = Heap::new();
        let dead = allocate(&heap, WideSlot { value: 10 });
        let recycled_location = collector_slot(&heap, dead).owner.location;
        heap.collect_full().unwrap();

        {
            let mut data = heap.inner.data.lock().unwrap();
            assert_eq!(data.free_runs, vec![recycled_location]);
            assert_eq!(data.allocation_pressure.assigned_runs, 0);
            data.allocation_pressure.high_water_mark = 1;
        }
        assert!(!heap.inner.allocation_pressure().collection_requested);

        let replacement = allocate(&heap, BitmapBoundarySlot { _value: 20 });
        assert_eq!(
            collector_slot(&heap, replacement).owner.location,
            recycled_location
        );
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressureSnapshot {
                assigned_runs: 1,
                high_water_mark: 1,
                collection_requested: true,
            }
        );
        assert!(heap.inner.data.lock().unwrap().free_runs.is_empty());
    }

    #[test]
    fn finalizer_panic_retains_the_completed_sweep_pressure_baseline() {
        let heap = Heap::new();
        let (live, dead, root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<WideSlot>().unwrap();
            let live = allocator.alloc(WideSlot { value: 10 });
            let dead = allocator.alloc(WideSlot { value: 20 });
            (live, dead, mutator.root(live))
        });
        assert_ne!(
            collector_slot(&heap, live).owner.location,
            collector_slot(&heap, dead).owner.location
        );
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();

        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.inner.run_synthetic_collection(
                epoch,
                false,
                |_, _| {},
                |_| panic!("injected finalizer panic after survivor baseline"),
            );
        }));

        assert!(panic.is_err());
        assert_failed_collection_restored(&heap);
        let pressure = heap.inner.allocation_pressure();
        assert_eq!(pressure.assigned_runs, 1);
        assert_eq!(
            pressure.high_water_mark,
            survivor_run_high_water_mark(1, SURVIVOR_GROWTH_NUMERATOR, SURVIVOR_GROWTH_DENOMINATOR,)
        );
        assert!(pressure.collection_requested);
        heap.with_mutator(|mutator| assert_eq!(root.get(mutator).value, 10));
    }

    #[test]
    fn empty_runs_have_no_finalization_obligation_and_recycle_from_any_class() {
        let heap = Heap::new();
        let plain_class = internal_class::<FirstType>(&heap);
        let dropping_class = internal_class::<DroppingType>(&heap);
        let plain_location = heap.inner.prepare_run(&plain_class).unwrap();
        let dropping_location = heap.inner.prepare_run(&dropping_class).unwrap();
        let (plain_geometry, dropping_geometry) = {
            let data = heap.inner.data.lock().unwrap();
            (
                data.classes[class_index(plain_class.id()).unwrap()].geometry(),
                data.classes[class_index(dropping_class.id()).unwrap()].geometry(),
            )
        };

        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        assert!(
            heap.inner
                .run_synthetic_collection(
                    epoch,
                    false,
                    |post_mark, _| {
                        assert_eq!(post_mark.dead_set.allocated_slots, 0);
                        assert_eq!(post_mark.dead_set.live_slots, 0);
                        assert_eq!(post_mark.dead_set.empty_runs, 2);
                        assert_eq!(post_mark.dead_set.no_drop_dead_slots, 0);
                        assert_eq!(post_mark.dead_set.drop_required_dead_slots, 0);
                        assert_eq!(post_mark.dead_set.dead_runs.len(), 2);
                        assert!(post_mark.dead_set.dead_runs.iter().all(|run| {
                            run.disposition == DeadSlotDisposition::NoDrop
                                && run.live_slots == 0
                                && run.dead_slots == 0
                                && run.dead_words.is_empty()
                        }));
                    },
                    |_| {},
                )
                .is_none()
        );

        let data = heap.inner.data.lock().unwrap();
        assert!(
            data.classes[class_index(plain_class.id()).unwrap()]
                .runs()
                .is_empty()
        );
        assert!(
            data.classes[class_index(dropping_class.id()).unwrap()]
                .runs()
                .is_empty()
        );
        assert!(data.retired_no_drop_runs.is_empty());
        assert_eq!(data.free_runs, vec![plain_location, dropping_location]);
        for (location, geometry) in [
            (plain_location, plain_geometry),
            (dropping_location, dropping_geometry),
        ] {
            assert!(data.arena.run_is_empty_for_test(location));
            assert!(data.arena.run_claim_target(location).is_none());
            assert!(
                data.arena
                    .run_side_metadata_for_test(location, geometry)
                    .iter()
                    .all(|byte| *byte == 0)
            );
        }
    }

    #[test]
    fn finalizer_panic_retains_one_free_run_and_retry_does_not_duplicate_it() {
        let heap = Heap::new();
        let class = internal_class::<WideSlot>(&heap);
        let dead = allocate(&heap, WideSlot { value: 10 });
        let dead_slot = collector_slot(&heap, dead);
        let initial_lease_epoch = heap.inner.current_allocation_lease_epoch();

        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.inner.run_synthetic_collection(
                epoch,
                false,
                |_, _| {},
                |_| panic!("injected finalizer panic after no-drop retirement"),
            );
        }));
        assert!(panic.is_err());
        assert_failed_collection_restored(&heap);
        assert_eq!(
            heap.inner.current_allocation_lease_epoch().get(),
            initial_lease_epoch.get() + 1
        );

        {
            let data = heap.inner.data.lock().unwrap();
            assert!(
                data.classes[class_index(class.id()).unwrap()]
                    .runs()
                    .is_empty()
            );
            assert!(data.retired_no_drop_runs.is_empty());
            assert_eq!(data.free_runs, vec![dead_slot.owner.location]);
            assert!(data.arena.run_is_empty_for_test(dead_slot.owner.location));
            assert!(
                data.arena
                    .run_claim_target(dead_slot.owner.location)
                    .is_none()
            );
            assert!(
                data.arena
                    .run_side_metadata_for_test(dead_slot.owner.location, dead_slot.owner.geometry,)
                    .iter()
                    .all(|byte| *byte == 0)
            );
        }

        let report = heap.collect_full().unwrap();
        assert_eq!(report.marked_slots(), 0);
        {
            let data = heap.inner.data.lock().unwrap();
            assert!(data.retired_no_drop_runs.is_empty());
            assert_eq!(data.free_runs, vec![dead_slot.owner.location]);
            assert!(
                data.classes[class_index(class.id()).unwrap()]
                    .runs()
                    .is_empty()
            );
        }

        let replacement = allocate(&heap, WideSlot { value: 20 });
        let replacement_slot = collector_slot(&heap, replacement);
        assert_eq!(replacement_slot.owner.location, dead_slot.owner.location);
        assert!(heap.inner.data.lock().unwrap().free_runs.is_empty());
    }

    #[test]
    fn panicking_elected_collection_restores_and_relatches_its_request() {
        let heap = Heap::new();
        let initial_lease_epoch = heap.inner.current_allocation_lease_epoch();
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();

        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.inner.run_synthetic_collection(
                epoch,
                false,
                |_, _| panic!("injected collection panic"),
                |_| {},
            );
        }));
        assert!(panic.is_err());

        let restored = heap.inner.coordinator_snapshot();
        assert_eq!(restored.phase, AdmissionPhase::Ordinary);
        assert!(restored.collection_requested);
        assert_eq!(restored.active_collection, None);
        assert_eq!(restored.latest_collection_report, None);
        assert_eq!(
            heap.inner.current_allocation_lease_epoch(),
            initial_lease_epoch,
            "post-mark planning panic precedes allocator invalidation"
        );
        heap.with_mutator(|_| {});
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            1
        );
        assert_eq!(
            heap.inner.current_allocation_lease_epoch().get(),
            initial_lease_epoch.get() + 1
        );
    }

    #[test]
    fn finalizer_handoff_installs_one_recursive_current_mutator_without_a_gap() {
        let heap = Heap::new();
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();

        let admission = heap.inner.run_synthetic_collection(
            epoch,
            false,
            |post_mark, _| assert_eq!(post_mark.summary, MarkSummary::default()),
            |finalizer_mutator| {
                let coordinator = heap.inner.coordinator_snapshot();
                assert_eq!(coordinator.phase, AdmissionPhase::Finalizing);
                assert_eq!(coordinator.active_outer_mutators, 1);
                assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 1);
                assert!(
                    heap.inner
                        .data
                        .lock()
                        .unwrap()
                        .finalization_batch
                        .runs
                        .is_empty(),
                    "the bootstrap deliberately retains the proven no-op handoff for an empty batch"
                );

                let class = finalizer_mutator.allocator::<SecondType>().unwrap();
                heap.with_mutator(|_recursive| {
                    assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 1);
                    assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 2);
                    let _ = class.alloc(SecondType { _value: 19 });
                });
            },
        );

        assert!(admission.is_none());
        let completed = heap.inner.coordinator_snapshot();
        assert_eq!(completed.phase, AdmissionPhase::Ordinary);
        assert_eq!(completed.active_outer_mutators, 0);
        assert_eq!(completed.completed_collection_epoch, 1);
        assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 0);
    }

    #[test]
    fn ordinary_workers_may_enter_while_the_collector_holds_its_finalizer_mutator() {
        let heap = Heap::new();
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();
        let (finalizing_tx, finalizing_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.inner
                    .run_synthetic_collection(
                        epoch,
                        false,
                        |_, _| {},
                        |_| {
                            finalizing_tx.send(()).unwrap();
                            release_rx.recv().unwrap();
                        },
                    )
                    .is_none()
            }
        });
        finalizing_rx.recv().unwrap();

        let (entered_tx, entered_rx) = mpsc::channel();
        let worker = std::thread::spawn({
            let heap = heap.clone();
            move || heap.with_mutator(|_| entered_tx.send(()).unwrap())
        });
        entered_rx.recv().unwrap();
        worker.join().unwrap();
        assert_eq!(
            heap.inner.coordinator_snapshot().active_outer_mutators,
            1,
            "worker exit must leave only the held finalizer mutator"
        );

        release_tx.send(()).unwrap();
        assert!(collector.join().unwrap());
        assert_eq!(
            heap.inner.coordinator_snapshot().phase,
            AdmissionPhase::Ordinary
        );
    }

    #[test]
    fn request_during_finalization_is_coalesced_without_blocking_admission() {
        let heap = Heap::new();
        heap.request_collection();
        let first_epoch = heap.inner.elect_idle_collection_for_test();
        let (finalizing_tx, finalizing_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.inner
                    .run_synthetic_collection(
                        first_epoch,
                        false,
                        |_, _| {},
                        |_| {
                            finalizing_tx.send(()).unwrap();
                            release_rx.recv().unwrap();
                        },
                    )
                    .is_none()
            }
        });
        finalizing_rx.recv().unwrap();
        heap.request_collection();

        let (entered_tx, entered_rx) = mpsc::channel();
        let entrant = std::thread::spawn({
            let heap = heap.clone();
            move || heap.with_mutator(|_| entered_tx.send(()).unwrap())
        });
        entered_rx.recv().unwrap();
        entrant.join().unwrap();

        release_tx.send(()).unwrap();
        assert!(collector.join().unwrap());
        let completed = heap.inner.coordinator_snapshot();
        assert_eq!(completed.completed_collection_epoch, 1);
        assert_eq!(completed.phase, AdmissionPhase::Ordinary);
        assert!(!completed.collection_requested);
    }

    #[test]
    fn request_during_post_mark_work_is_coalesced_into_that_collection() {
        let heap = Heap::new();
        heap.request_collection();
        let first_epoch = heap.inner.elect_idle_collection_for_test();
        let (exclusive_tx, exclusive_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.inner
                    .run_synthetic_collection(
                        first_epoch,
                        false,
                        |_, _| {
                            exclusive_tx.send(()).unwrap();
                            release_rx.recv().unwrap();
                        },
                        |_| {},
                    )
                    .is_none()
            }
        });
        exclusive_rx.recv().unwrap();
        heap.request_collection();
        release_tx.send(()).unwrap();

        assert!(collector.join().unwrap());
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            1
        );
        assert!(!heap.inner.coordinator_snapshot().collection_requested);
    }

    #[test]
    fn panicking_finalizer_retires_its_mutator_and_relatches_collection() {
        let heap = Heap::new();
        let initial_lease_epoch = heap.inner.current_allocation_lease_epoch();
        heap.request_collection();
        let epoch = heap.inner.elect_idle_collection_for_test();

        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.inner.run_synthetic_collection(
                epoch,
                false,
                |_, _| {},
                |_| panic!("injected finalizer panic"),
            );
        }));
        assert!(panic.is_err());

        let restored = heap.inner.coordinator_snapshot();
        assert_eq!(restored.phase, AdmissionPhase::Ordinary);
        assert_eq!(restored.active_outer_mutators, 0);
        assert_eq!(restored.active_collection, None);
        assert_eq!(restored.latest_collection_report, None);
        assert!(restored.collection_requested);
        assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 0);
        assert_eq!(
            heap.inner.current_allocation_lease_epoch().get(),
            initial_lease_epoch.get() + 1,
            "published allocator invalidation is not rolled back by finalizer panic"
        );

        heap.with_mutator(|_| {});
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            1
        );
        assert_eq!(
            heap.inner.current_allocation_lease_epoch().get(),
            initial_lease_epoch.get() + 2
        );
    }

    #[test]
    fn heaps_share_type_metadata_but_not_class_provenance() {
        let first = Heap::new();
        let second = Heap::new();
        let first_class = internal_class::<FirstType>(&first);
        let second_class = internal_class::<FirstType>(&second);

        assert!(std::ptr::eq(
            first_class.metadata(),
            second_class.metadata()
        ));
        assert!(first_class.belongs_to(&first.inner));
        assert!(!first_class.belongs_to(&second.inner));
        assert!(second_class.belongs_to(&second.inner));
        assert!(!second_class.belongs_to(&first.inner));
    }

    #[test]
    fn unsupported_layouts_publish_no_class_or_dense_id() {
        let heap = Heap::new();
        assert!(heap.with_mutator(|mutator| matches!(
            mutator.allocator::<()>(),
            Err(UnsupportedLayout::ZeroSized)
        )));
        assert!(heap.with_mutator(|mutator| matches!(
            mutator.allocator::<OverflowingSlot>(),
            Err(UnsupportedLayout::ArithmeticOverflow)
        )));

        let first_valid_id =
            heap.with_mutator(|mutator| mutator.allocator::<FirstType>().unwrap().id());
        assert_eq!(first_valid_id.get(), 1);
        assert_eq!(heap.inner.data.lock().unwrap().classes.len(), 1);
    }

    #[test]
    fn panicking_class_candidate_publishes_no_partial_entry() {
        let heap = Heap::new();
        let metadata = metadata_for::<SecondType>();
        let geometry =
            crate::run::RunGeometry::derive(metadata.layout(), metadata.requested_slot_size())
                .unwrap();

        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.with_mutator(|_| {
                let _ = heap
                    .inner
                    .discover_class_with::<SecondType>(metadata, geometry, || {
                        panic!("injected class construction panic")
                    });
            });
        }));
        assert!(panic.is_err());
        assert!(heap.inner.data.lock().unwrap().classes.is_empty());

        let class_id = heap.with_mutator(|mutator| mutator.allocator::<SecondType>().unwrap().id());
        assert_eq!(class_id.get(), 1);
    }

    #[test]
    fn typed_run_headers_resolve_to_exact_class_metadata() {
        let heap = Heap::new();
        let first_class = internal_class::<FirstType>(&heap);
        let dropping_class = internal_class::<DroppingType>(&heap);
        let first_run = heap.inner.prepare_run(&first_class).unwrap();
        let dropping_run = heap.inner.prepare_run(&dropping_class).unwrap();

        let (first_address, dropping_address) = {
            let state = heap.inner.data.lock().unwrap();
            let first_geometry = state.classes[class_index(first_class.id()).unwrap()].geometry();
            let dropping_geometry =
                state.classes[class_index(dropping_class.id()).unwrap()].geometry();
            (
                state.arena.run_at(first_run).unwrap().address()
                    + first_geometry.slot_offset(0).unwrap(),
                state.arena.run_at(dropping_run).unwrap().address()
                    + dropping_geometry.slot_offset(1).unwrap(),
            )
        };

        let first = heap.inner.resolve_slot(first_address).unwrap();
        assert!(std::ptr::eq(first.metadata, first_class.metadata()));
        assert_eq!(first.class_id, first_class.id());
        assert_eq!(first.slot_index, 0);
        assert_eq!(first.geometry.slot_stride, std::mem::size_of::<FirstType>());
        assert!(!first.metadata.needs_drop());
        assert_eq!(
            first.metadata.layout(),
            std::alloc::Layout::new::<FirstType>()
        );

        let dropping = heap.inner.resolve_slot(dropping_address).unwrap();
        assert!(std::ptr::eq(dropping.metadata, dropping_class.metadata()));
        assert_eq!(dropping.class_id, dropping_class.id());
        assert_eq!(dropping.slot_index, 1);
        assert!(dropping.metadata.needs_drop());
        assert_eq!(
            dropping.metadata.layout(),
            std::alloc::Layout::new::<DroppingType>()
        );
    }

    #[test]
    fn root_validation_rejects_a_structurally_valid_unallocated_slot() {
        let heap = Heap::new();
        let value = allocate(&heap, 42_u64);
        let unallocated = {
            let state = heap.inner.data.lock().unwrap();
            let address = value.erase().as_ptr().as_ptr() as usize;
            let owner = state.arena.checked_slot_owner(address).unwrap();
            let slot_index = (0..owner.geometry.slot_count)
                .find(|&index| {
                    index != owner.slot_index
                        && !state.arena.owner_slot_is_allocated(crate::arena::RunOwner {
                            slot_index: index,
                            ..owner
                        })
                })
                .expect("test run must contain another unallocated slot");
            let offset = owner
                .geometry
                .slot_offset(slot_index)
                .expect("test slot must have an in-run offset");
            // SAFETY: the selected slot offset was derived from this run's
            // validated geometry and remains inside the live run allocation.
            unsafe { owner.run.pointer().add(offset).cast::<u64>() }
        };

        let state = heap.inner.data.lock().unwrap();
        assert!(matches!(
            validate_rootable_in_state(
                &state,
                ErasedGc::new(unallocated.cast()),
                metadata_for::<u64>()
            ),
            Err(RootValidationError::Unallocated)
        ));
    }

    #[test]
    fn root_registry_publishes_once_per_cell_and_not_per_clone() {
        let heap = Heap::new();
        let value = allocate(&heap, 42_u64);
        let first = heap.with_mutator(|mutator| mutator.root(value));
        let first_clone = first.clone();

        assert_eq!(heap.inner.data.lock().unwrap().roots.len(), 1);

        let second = heap.with_mutator(|mutator| mutator.root(value));
        assert_eq!(heap.inner.data.lock().unwrap().roots.len(), 2);

        drop((first, first_clone, second));
    }

    #[test]
    fn exclusive_root_walk_visits_live_cells_in_order_and_prunes_dead_cells() {
        let heap = Heap::new();
        let values = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            [
                allocator.alloc(11_u64),
                allocator.alloc(22_u64),
                allocator.alloc(33_u64),
            ]
        });
        let [first, middle, last] =
            heap.with_mutator(|mutator| values.map(|value| mutator.root(value)));
        drop(middle);

        let exclusive = heap.inner.enter_synthetic_exclusive();
        let mut visited = Vec::new();
        let count = heap
            .inner
            .visit_registered_roots(|value| visited.push(value.as_ptr().as_ptr() as usize));

        assert_eq!(count, 2);
        assert_eq!(
            visited,
            [values[0], values[2]].map(|value| value.erase().as_ptr().as_ptr() as usize)
        );
        assert_eq!(heap.inner.data.lock().unwrap().roots.len(), 2);

        drop((first, last));
        assert_eq!(heap.inner.visit_registered_roots(|_| {}), 0);
        assert!(heap.inner.data.lock().unwrap().roots.is_empty());
        drop(exclusive);
    }

    #[test]
    fn elected_collection_walks_and_prunes_the_root_registry() {
        let heap = Heap::new();
        let values = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            [allocator.alloc(41_u64), allocator.alloc(42_u64)]
        });
        let live = heap.with_mutator(|mutator| mutator.root(values[0]));
        let dead = heap.with_mutator(|mutator| mutator.root(values[1]));
        drop(dead);
        assert_eq!(heap.inner.data.lock().unwrap().roots.len(), 2);

        assert_eq!(heap.collect_full().unwrap().epoch(), 1);

        assert_eq!(heap.inner.data.lock().unwrap().roots.len(), 1);
        heap.with_mutator(|mutator| assert_eq!(*live.get(mutator), 41));
    }

    #[test]
    fn last_public_drop_after_upgrade_is_passive_and_conservatively_retained() {
        let drops = Arc::new(AtomicUsize::new(0));
        let heap = Heap::new();
        let (dropping_value, plain_value) = heap.with_mutator(|mutator| {
            let dropping_allocator = mutator.allocator::<DropCounter>().unwrap();
            let plain_allocator = mutator.allocator::<u64>().unwrap();
            (
                dropping_allocator.alloc(DropCounter(Arc::clone(&drops))),
                plain_allocator.alloc(42_u64),
            )
        });
        let dropping_root = heap.with_mutator(|mutator| mutator.root(dropping_value));
        let plain_root = heap.with_mutator(|mutator| mutator.root(plain_value));
        let dropping_address = dropping_value.erase().as_ptr().as_ptr() as usize;
        let plain_address = plain_value.erase().as_ptr().as_ptr() as usize;
        let dropping_registration = heap.inner.data.lock().unwrap().roots[0].clone();
        let exclusive = heap.inner.enter_synthetic_exclusive();
        let (upgraded_tx, upgraded_rx) = mpsc::channel();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            upgraded_rx.recv().unwrap();
            drop(dropping_root);
            dropped_tx.send(()).unwrap();
        });

        let mut visited = Vec::new();
        let count = heap.inner.visit_registered_roots(|value| {
            let address = value.as_ptr().as_ptr() as usize;
            visited.push(address);
            if address == dropping_address {
                upgraded_tx.send(()).unwrap();
                dropped_rx.recv().unwrap();
            } else {
                assert_eq!(address, plain_address);
                assert!(
                    dropping_registration.upgrade().is_none(),
                    "the prior temporary upgrade must be released before the next entry"
                );
            }
        });
        dropper.join().expect("root dropper panicked");

        assert_eq!(count, 2);
        assert_eq!(visited, vec![dropping_address, plain_address]);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert_eq!(heap.inner.data.lock().unwrap().roots.len(), 2);

        let mut retained = Vec::new();
        assert_eq!(
            heap.inner
                .visit_registered_roots(|value| retained.push(value.as_ptr().as_ptr() as usize)),
            1
        );
        assert_eq!(retained, vec![plain_address]);
        assert_eq!(heap.inner.data.lock().unwrap().roots.len(), 1);

        drop(exclusive);
        drop(plain_root);
        drop(heap);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn failed_upgrade_cannot_race_root_publication_during_exclusive_collection() {
        let heap = Heap::new();
        let value = allocate(&heap, 42_u64);
        let expired = heap.with_mutator(|mutator| mutator.root(value));
        drop(expired);
        assert_eq!(heap.inner.data.lock().unwrap().roots.len(), 1);
        let exclusive = heap.inner.enter_synthetic_exclusive();
        let worker = std::thread::spawn({
            let heap = heap.clone();
            move || heap.with_mutator(|mutator| mutator.root(value))
        });

        heap.inner.wait_for_blocked_outer_mutators(1);
        assert_eq!(heap.inner.visit_registered_roots(|_| {}), 0);
        assert!(heap.inner.data.lock().unwrap().roots.is_empty());

        drop(exclusive);
        let root = worker.join().expect("root publisher panicked");
        assert_eq!(heap.inner.data.lock().unwrap().roots.len(), 1);
        heap.with_mutator(|mutator| assert_eq!(*root.get(mutator), 42));
    }

    #[test]
    fn failed_root_validation_publishes_no_registry_entry() {
        let owner = Heap::new();
        let observer = Heap::new();
        let value = allocate(&owner, 42_u64);

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = observer.with_mutator(|mutator| mutator.root(value));
        }));

        assert!(panic.is_err());
        assert!(observer.inner.data.lock().unwrap().roots.is_empty());
    }

    #[test]
    fn heap_enumerates_every_published_run_from_authoritative_state() {
        let heap = Heap::new();
        let first_class = internal_class::<FirstType>(&heap);
        let second_class = internal_class::<SecondType>(&heap);
        let first_run = heap.inner.prepare_run(&first_class).unwrap();
        let second_run = heap.inner.prepare_run(&first_class).unwrap();
        let third_run = heap.inner.prepare_run(&second_class).unwrap();

        let runs = heap.inner.resolved_runs();
        assert_eq!(runs.len(), 3);
        assert_eq!(
            runs.iter().map(|run| run.location).collect::<Vec<_>>(),
            vec![first_run, second_run, third_run]
        );
        assert_eq!(
            runs.iter()
                .filter(|run| run.class_id == first_class.id())
                .count(),
            2
        );
        assert!(runs.iter().all(|run| {
            let expected = if run.class_id == first_class.id() {
                first_class.metadata()
            } else {
                second_class.metadata()
            };
            std::ptr::eq(run.metadata, expected)
                && run.geometry
                    == heap.inner.data.lock().unwrap().classes[class_index(run.class_id).unwrap()]
                        .geometry()
        }));
    }

    #[test]
    fn concurrent_run_publication_keeps_one_enumerable_class_pool() {
        const THREADS: usize = 8;

        let heap = Heap::new();
        let class_id = internal_class::<FirstType>(&heap).id();
        let runs = (0..THREADS)
            .map(|_| {
                let heap = heap.clone();
                std::thread::spawn(move || {
                    let class = internal_class::<FirstType>(&heap);
                    heap.inner.prepare_run(&class).unwrap()
                })
            })
            .map(|thread| thread.join().expect("run-publication worker panicked"))
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(runs.len(), THREADS);
        let resolved = heap.inner.resolved_runs();
        assert_eq!(resolved.len(), THREADS);
        assert!(
            resolved
                .iter()
                .all(|run| run.class_id == class_id && runs.contains(&run.location))
        );
    }

    #[test]
    fn foreign_class_leaves_both_heaps_run_state_unchanged() {
        let owner = Heap::new();
        let observer = Heap::new();
        let class = internal_class::<FirstType>(&owner);

        assert_eq!(
            observer.inner.prepare_run(&class),
            Err(PrepareRunError::ForeignClass)
        );
        assert!(owner.inner.resolved_runs().is_empty());
        assert!(observer.inner.resolved_runs().is_empty());

        owner.inner.prepare_run(&class).unwrap();
        assert_eq!(owner.inner.resolved_runs().len(), 1);
    }

    #[test]
    fn synchronized_allocation_crosses_exact_run_and_chunk_boundaries() {
        let heap = Heap::new();
        let class = internal_class::<WideSlot>(&heap);
        #[cfg(miri)]
        let last_index = crate::arena::RUNS_PER_CHUNK;
        #[cfg(not(miri))]
        let last_index = 2 * crate::arena::RUNS_PER_CHUNK;
        let values = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<WideSlot>().unwrap();
            let mut roots = Vec::new();
            let values = (0..=last_index)
                .map(|value| {
                    let managed = allocator.alloc(WideSlot {
                        value: value as u64,
                    });
                    roots.push(mutator.root(managed));
                    managed
                })
                .collect::<Vec<_>>();
            (values, roots)
        });
        let (values, roots) = values;

        #[cfg(miri)]
        let expected = [
            (0, RunLocation { chunk: 0, run: 0 }),
            (
                crate::arena::RUNS_PER_CHUNK - 1,
                RunLocation {
                    chunk: 0,
                    run: crate::arena::RUNS_PER_CHUNK - 1,
                },
            ),
            (
                crate::arena::RUNS_PER_CHUNK,
                RunLocation { chunk: 1, run: 0 },
            ),
        ];
        #[cfg(not(miri))]
        let expected = [
            (0, RunLocation { chunk: 0, run: 0 }),
            (
                crate::arena::RUNS_PER_CHUNK - 1,
                RunLocation {
                    chunk: 0,
                    run: crate::arena::RUNS_PER_CHUNK - 1,
                },
            ),
            (
                crate::arena::RUNS_PER_CHUNK,
                RunLocation { chunk: 1, run: 0 },
            ),
            (
                2 * crate::arena::RUNS_PER_CHUNK,
                RunLocation { chunk: 2, run: 0 },
            ),
        ];
        for (value_index, location) in expected {
            let address = values[value_index].erase().as_ptr().as_ptr() as usize;
            let resolved = heap.inner.resolve_slot(address).unwrap();
            assert_eq!(resolved.class_id, class.id());
            assert_eq!(resolved.slot_index, 0);
            assert!(std::ptr::eq(resolved.metadata, class.metadata()));
            let state = heap.inner.data.lock().unwrap();
            assert_eq!(
                state.arena.checked_slot_owner(address).unwrap().location,
                location
            );
        }

        heap.with_mutator(|mutator| {
            for (index, value) in values.iter().enumerate() {
                // SAFETY: every pointer remains allocated in `heap` with exact
                // `WideSlot` representation until terminal teardown.
                assert_eq!(unsafe { value.get_unchecked(mutator) }.value, index as u64);
            }
        });
        drop(roots);
    }

    #[test]
    fn internal_synchronized_allocation_rejects_a_foreign_class_before_state_changes() {
        let owner = Heap::new();
        let observer = Heap::new();
        let class = internal_class::<FirstType>(&owner);

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = observer
                .inner
                .allocate_synchronized(&class, FirstType { _value: 1 });
        }));
        assert!(panic.is_err());
        assert!(owner.inner.resolved_runs().is_empty());
        assert!(observer.inner.resolved_runs().is_empty());
    }

    #[test]
    fn unwind_after_slot_selection_does_not_publish_an_allocation() {
        let heap = Heap::new();
        let class = internal_class::<DropCounter>(&heap);
        let drops = Arc::new(AtomicUsize::new(0));

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = heap.inner.allocate_synchronized_with(
                &class,
                DropCounter(Arc::clone(&drops)),
                || panic!("injected pre-initialization unwind"),
            );
        }));
        assert!(panic.is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 1);

        let state = match heap.inner.data.lock() {
            Ok(_) => panic!("injected unwind should poison the managed-data mutex"),
            Err(poisoned) => poisoned.into_inner(),
        };
        let runs = state.classes[class_index(class.id()).unwrap()].runs();
        assert_eq!(runs.len(), 1);
        assert!(
            state
                .arena
                .allocated_slot_pointers(runs[0].location)
                .is_empty()
        );
    }

    #[test]
    fn terminal_heap_teardown_drops_each_allocated_payload_exactly_once() {
        const ALLOCATIONS: usize = 130;

        let drops = Arc::new(AtomicUsize::new(0));
        let heap = Heap::new();
        heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<DropCounter>().unwrap();
            for _ in 0..ALLOCATIONS {
                let _ = allocator.alloc(DropCounter(Arc::clone(&drops)));
            }
        });

        drop(heap);
        assert_eq!(drops.load(Ordering::Relaxed), ALLOCATIONS);
    }

    #[test]
    fn only_the_last_heap_facade_starts_terminal_teardown() {
        let drops = Arc::new(AtomicUsize::new(0));
        let heap = Heap::new();
        let other_facade = heap.clone();
        let weak = Arc::downgrade(&heap.inner);
        let _ = allocate(&heap, DropCounter(Arc::clone(&drops)));

        drop(heap);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert!(weak.upgrade().is_some());

        drop(other_facade);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn ordinary_finalization_has_a_mutator_but_terminal_teardown_does_not() {
        let ordinary_observations = Arc::new(Mutex::new(Vec::new()));
        let ordinary = Heap::new();
        let _ = allocate(
            &ordinary,
            MutatorContextDrop {
                observed_active_mutator: Arc::clone(&ordinary_observations),
            },
        );

        ordinary.collect_full().unwrap();
        assert_eq!(*ordinary_observations.lock().unwrap(), vec![true]);

        let terminal_observations = Arc::new(Mutex::new(Vec::new()));
        let terminal = Heap::new();
        let _ = allocate(
            &terminal,
            MutatorContextDrop {
                observed_active_mutator: Arc::clone(&terminal_observations),
            },
        );

        drop(terminal);
        assert_eq!(*terminal_observations.lock().unwrap(), vec![false]);
    }

    #[test]
    fn terminal_teardown_finishes_the_batch_retained_after_finalizer_panic() {
        let heap = Heap::new();
        let weak = Arc::downgrade(&heap.inner);
        let events = Arc::new(Mutex::new(Vec::new()));
        let _panicking = allocate(
            &heap,
            SelectivePanickingDrop {
                id: 0,
                panic: true,
                events: Arc::clone(&events),
            },
        );
        let _deferred = allocate(
            &heap,
            SelectivePanickingDrop {
                id: 1,
                panic: false,
                events: Arc::clone(&events),
            },
        );

        let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
            .expect_err("managed destructor must propagate its panic");
        assert_eq!(
            panic_string(panic.as_ref()),
            "injected managed destructor panic"
        );
        assert_eq!(*events.lock().unwrap(), vec![0]);
        assert_eq!(
            heap.activity(),
            HeapActivity {
                queued_finalizers: 1,
                running_finalizers: 0,
            }
        );

        drop(heap);
        assert_eq!(*events.lock().unwrap(), vec![0, 1]);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn terminal_class_walk_includes_attached_pending_finalizers_once() {
        let heap = Heap::new();
        let weak = Arc::downgrade(&heap.inner);
        let events = Arc::new(Mutex::new(Vec::new()));
        let (panicking, deferred, live, live_root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<SelectivePanickingDrop>().unwrap();
            let panicking = allocator.alloc(SelectivePanickingDrop {
                id: 0,
                panic: true,
                events: Arc::clone(&events),
            });
            let deferred = allocator.alloc(SelectivePanickingDrop {
                id: 1,
                panic: false,
                events: Arc::clone(&events),
            });
            let live = allocator.alloc(SelectivePanickingDrop {
                id: 2,
                panic: false,
                events: Arc::clone(&events),
            });
            (panicking, deferred, live, mutator.root(live))
        });
        let location = collector_slot(&heap, panicking).owner.location;
        assert_eq!(collector_slot(&heap, deferred).owner.location, location);
        assert_eq!(collector_slot(&heap, live).owner.location, location);

        let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
            .expect_err("managed destructor must propagate its panic");
        assert_eq!(
            panic_string(panic.as_ref()),
            "injected managed destructor panic"
        );
        assert_eq!(*events.lock().unwrap(), vec![0]);
        assert_eq!(
            heap.activity(),
            HeapActivity {
                queued_finalizers: 1,
                running_finalizers: 0,
            }
        );
        assert_eq!(
            heap.inner
                .data
                .lock()
                .unwrap()
                .finalization_batch
                .detached_run_count(),
            0
        );

        drop(live_root);
        drop(heap);

        let mut observed = events.lock().unwrap().clone();
        observed.sort_unstable();
        assert_eq!(observed, vec![0, 1, 2]);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn terminal_teardown_visits_mixed_detached_and_attached_topology_once() {
        let _terminal_lock = TERMINAL_DESTRUCTOR_TEST_LOCK.lock().unwrap();
        TERMINAL_DESTRUCTOR_ATTEMPTS.store(0, Ordering::Relaxed);
        TERMINAL_DESTRUCTOR_PANIC_ONCE.store(true, Ordering::Relaxed);
        let heap = Heap::new();
        let weak = Arc::downgrade(&heap.inner);
        let metadata = metadata_for::<TerminalPanickingDrop>();
        let slot_count = RunGeometry::derive(metadata.layout(), metadata.requested_slot_size())
            .expect("terminal topology fixture must have supported geometry")
            .slot_count;

        // Fill one wholly dead run, then place two dead values and one rooted
        // ordinary value in a second run. The finalization dispatch map does
        // not promise order, so every value shares one panic-once token: the
        // first attempted identity retires regardless of which run is chosen.
        let (values, live_root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<TerminalPanickingDrop>().unwrap();
            let values = (0..slot_count + 3)
                .map(|_| allocator.alloc(TerminalPanickingDrop { _not_zero_sized: 0 }))
                .collect::<Vec<_>>();
            let live_root = mutator.root(values[slot_count + 2]);
            (values, live_root)
        });
        let owners = values
            .iter()
            .map(|value| collector_slot(&heap, *value).owner)
            .collect::<Vec<_>>();
        let detached_location = owners[0].location;
        let attached_location = owners[slot_count].location;
        assert_ne!(detached_location, attached_location);
        assert!(
            owners[..slot_count]
                .iter()
                .all(|owner| owner.location == detached_location)
        );
        assert!(
            owners[slot_count..]
                .iter()
                .all(|owner| owner.location == attached_location)
        );

        let panic = catch_unwind(AssertUnwindSafe(|| heap.collect_full()))
            .expect_err("the first managed destructor must propagate its panic");
        assert_eq!(
            panic_string(panic.as_ref()),
            "injected terminal destructor panic"
        );
        assert_eq!(TERMINAL_DESTRUCTOR_ATTEMPTS.load(Ordering::Relaxed), 1);
        assert!(!TERMINAL_DESTRUCTOR_PANIC_ONCE.load(Ordering::Relaxed));

        {
            let data = heap.inner.data.lock().unwrap();
            assert_eq!(data.finalization_batch.runs.len(), 2);
            assert_eq!(data.finalization_batch.detached_run_count(), 1);
            assert_eq!(data.finalization_batch.pending_slot_count(), slot_count + 1);
            assert!(
                data.finalization_batch.runs[&detached_location]
                    .target
                    .is_detached()
            );
            assert!(
                !data.finalization_batch.runs[&attached_location]
                    .target
                    .is_detached()
            );
            assert_eq!(
                owners
                    .iter()
                    .filter(|owner| data.arena.owner_slot_is_allocated(**owner))
                    .count(),
                values.len() - 1,
                "exactly the panicking identity must already be retired"
            );
            assert!(
                data.arena.owner_slot_is_allocated(owners[slot_count + 2]),
                "the rooted ordinary allocation must remain live"
            );
        }

        drop(live_root);
        drop(heap);
        assert_eq!(
            TERMINAL_DESTRUCTOR_ATTEMPTS.load(Ordering::Relaxed),
            values.len(),
            "terminal traversal must attempt every allocation exactly once"
        );
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn terminal_teardown_propagates_the_first_panic_without_continuing() {
        let _terminal_lock = TERMINAL_DESTRUCTOR_TEST_LOCK.lock().unwrap();
        TERMINAL_DESTRUCTOR_ATTEMPTS.store(0, Ordering::Relaxed);
        TERMINAL_DESTRUCTOR_PANIC_ONCE.store(true, Ordering::Relaxed);
        let heap = Heap::new();
        let weak = Arc::downgrade(&heap.inner);
        let root = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<TerminalPanickingDrop>().unwrap();
            let first = allocator.alloc(TerminalPanickingDrop { _not_zero_sized: 0 });
            for _ in 1..3 {
                let _ = allocator.alloc(TerminalPanickingDrop { _not_zero_sized: 0 });
            }
            mutator.root(first)
        });

        let panic = catch_unwind(AssertUnwindSafe(|| drop(heap)))
            .expect_err("terminal destructor must propagate its panic");

        assert_eq!(
            panic_string(panic.as_ref()),
            "injected terminal destructor panic"
        );
        assert_eq!(TERMINAL_DESTRUCTOR_ATTEMPTS.load(Ordering::Relaxed), 1);
        assert!(!TERMINAL_DESTRUCTOR_PANIC_ONCE.load(Ordering::Relaxed));
        assert!(weak.upgrade().is_none());
        drop(root.clone());
        drop(root);
        assert_eq!(TERMINAL_DESTRUCTOR_ATTEMPTS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn forgotten_scoped_allocator_does_not_retain_its_heap() {
        let heap = Heap::new();
        let weak = Arc::downgrade(&heap.inner);

        heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            // This deliberately leaks the allocator's inert 24-byte frontier
            // cell. The sanitizer and Miri harnesses run this fixture
            // separately with leak detection disabled while retaining every
            // other check; see their scripts and `VERIFY.md`.
            std::mem::forget(allocator);
        });
        drop(heap);

        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn cached_allocation_reuses_one_word_without_shared_slow_path() {
        let heap = Heap::new();

        let first = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            (0..32_u64)
                .map(|value| allocator.alloc(value))
                .collect::<Vec<_>>()
        });
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);
        assert_eq!(heap.inner.allocation_cursor_slow_path_count(), 1);

        let second = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            (32..64_u64)
                .map(|value| allocator.alloc(value))
                .collect::<Vec<_>>()
        });
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressureSnapshot {
                assigned_runs: 1,
                high_water_mark: FIXED_SURVIVOR_RUN_HEADROOM,
                collection_requested: false,
            }
        );

        let next = allocate(&heap, 64_u64);
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 2);
        assert_eq!(heap.inner.allocation_cursor_slow_path_count(), 1);
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressureSnapshot {
                assigned_runs: 1,
                high_water_mark: FIXED_SURVIVOR_RUN_HEADROOM,
                collection_requested: false,
            }
        );

        heap.with_mutator(|mutator| {
            for (expected, value) in first.iter().chain(&second).chain([&next]).enumerate() {
                // SAFETY: every pointer belongs to `heap`, remains allocated,
                // and has the `u64` representation requested by `class`.
                assert_eq!(unsafe { *value.get_unchecked(mutator) }, expected as u64);
            }
        });
    }

    #[test]
    fn concurrent_mutators_claim_disjoint_words_in_one_run() {
        const THREADS: usize = 8;
        const VALUES_PER_THREAD: usize = 16;

        let heap = Heap::new();
        let barrier = Arc::new(Barrier::new(THREADS));
        let workers = (0..THREADS)
            .map(|thread_index| {
                let heap = heap.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    heap.with_mutator(|mutator| {
                        let allocator = mutator.allocator::<u64>().unwrap();
                        (0..VALUES_PER_THREAD)
                            .map(|offset| {
                                let value = thread_index * VALUES_PER_THREAD + offset;
                                allocator.alloc(value as u64)
                            })
                            .collect::<Vec<_>>()
                    })
                })
            })
            .collect::<Vec<_>>();
        let values = workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("allocation worker panicked"))
            .collect::<Vec<_>>();

        assert_eq!(heap.inner.allocation_cursor_claim_count(), THREADS);
        let addresses = values
            .iter()
            .map(|value| value.erase().as_ptr().as_ptr() as usize)
            .collect::<HashSet<_>>();
        assert_eq!(addresses.len(), THREADS * VALUES_PER_THREAD);
        let locations = addresses
            .iter()
            .map(|&address| heap.inner.resolve_slot(address).unwrap().slot_index)
            .collect::<HashSet<_>>();
        assert_eq!(locations.len(), THREADS * VALUES_PER_THREAD);
        assert_eq!(heap.inner.resolved_runs().len(), 1);
        assert_eq!(heap.inner.allocation_pressure().assigned_runs, 1);
        let frontier = internal_class::<u64>(&heap).frontier(&heap.inner);
        assert_eq!(frontier, Some(RunLocation { chunk: 0, run: 0 }));

        let observed = heap.with_mutator(|mutator| {
            values
                .iter()
                .map(|value| {
                    // SAFETY: workers returned initialized `u64` allocations
                    // from this heap, which cannot move or die in C2C.
                    unsafe { *value.get_unchecked(mutator) }
                })
                .collect::<HashSet<_>>()
        });
        assert_eq!(observed.len(), THREADS * VALUES_PER_THREAD);
    }

    fn force_concurrent_exhausted_frontier_claims(
        heap: &Heap,
        threads: usize,
    ) -> Vec<(RunLocation, usize)> {
        let arrived = Arc::new(Barrier::new(threads + 1));
        let release = Arc::new(Barrier::new(threads + 1));
        heap.inner
            .install_allocation_cursor_slow_path_hook(Arc::clone(&arrived), Arc::clone(&release));

        let workers = (0..threads)
            .map(|_| {
                let heap = heap.clone();
                std::thread::spawn(move || {
                    let class = internal_class::<u64>(&heap);
                    let cursor = heap.inner.claim_allocation_cursor(&class);
                    (cursor.location, cursor.word_index)
                })
            })
            .collect::<Vec<_>>();

        // Every worker has observed the same exhausted atomic frontier, but
        // none may acquire heap state until the test releases this barrier.
        arrived.wait();
        assert_eq!(heap.inner.allocation_cursor_claim_count(), threads);
        assert_eq!(heap.inner.allocation_cursor_slow_path_count(), threads);
        release.wait();

        let claims = workers
            .into_iter()
            .map(|worker| worker.join().expect("frontier claimant panicked"))
            .collect::<Vec<_>>();
        heap.inner.clear_allocation_cursor_slow_path_hook();
        claims
    }

    #[test]
    fn concurrent_exhausted_frontier_activates_one_prepublished_successor() {
        const THREADS: usize = 8;

        let heap = Heap::new();
        let class = internal_class::<u64>(&heap);
        let runs = (0..2)
            .map(|_| heap.inner.prepare_run(&class).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(class.frontier(&heap.inner), Some(runs[0]));
        while class.claim_frontier(&heap.inner).is_some() {}

        let claims = force_concurrent_exhausted_frontier_claims(&heap, THREADS);
        let unique = claims.iter().copied().collect::<HashSet<_>>();

        assert_eq!(unique.len(), THREADS);
        assert!(claims.iter().all(|(location, _)| *location == runs[1]));
        assert_eq!(class.frontier(&heap.inner), Some(runs[1]));
        assert_eq!(heap.inner.resolved_runs().len(), 2);
        assert_eq!(heap.inner.allocation_pressure().assigned_runs, 2);
        assert_eq!(
            heap.inner.allocation_cursor_locked_recheck_hit_count(),
            THREADS - 1
        );
        assert_eq!(
            heap.inner
                .allocation_cursor_frontier_advance_attempt_count(),
            1
        );
    }

    #[test]
    fn concurrent_exhausted_frontier_publishes_one_successor() {
        const THREADS: usize = 8;

        let heap = Heap::new();
        let class = internal_class::<u64>(&heap);
        let first = heap.inner.prepare_run(&class).unwrap();
        assert_eq!(class.frontier(&heap.inner), Some(first));
        while class.claim_frontier(&heap.inner).is_some() {}

        let claims = force_concurrent_exhausted_frontier_claims(&heap, THREADS);
        let unique = claims.iter().copied().collect::<HashSet<_>>();
        let successor = RunLocation { chunk: 0, run: 1 };

        assert_eq!(unique.len(), THREADS);
        assert!(claims.iter().all(|(location, _)| *location == successor));
        assert_eq!(class.frontier(&heap.inner), Some(successor));
        assert_eq!(heap.inner.resolved_runs().len(), 2);
        assert_eq!(heap.inner.allocation_pressure().assigned_runs, 2);
        assert_eq!(
            heap.inner.allocation_cursor_locked_recheck_hit_count(),
            THREADS - 1
        );
        assert_eq!(
            heap.inner
                .allocation_cursor_frontier_advance_attempt_count(),
            1
        );
    }

    #[test]
    fn exhausted_frontier_advances_through_prepublished_runs() {
        let heap = Heap::new();
        let class = internal_class::<u64>(&heap);
        let runs = (0..3)
            .map(|_| heap.inner.prepare_run(&class).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(class.frontier(&heap.inner), Some(runs[0]));

        while class.claim_frontier(&heap.inner).is_some() {}
        let cursor = heap.inner.claim_allocation_cursor(&class);

        assert_eq!(cursor.location, runs[1]);
        assert_eq!(class.frontier(&heap.inner), Some(runs[1]));
        assert_eq!(heap.inner.allocation_pressure().assigned_runs, 3);
    }

    #[test]
    fn evicted_and_thread_exit_cursors_leave_their_words_leased() {
        let heap = Heap::new();
        let class = internal_class::<u64>(&heap);
        let first = allocate(&heap, 1_u64);
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);

        let colliding = AllocationClassId::new(class.id().get() + 64).unwrap();
        heap.with_mutator(|_| insert_cursor(&heap.inner, test_cursor(colliding)));
        let second = allocate(&heap, 2_u64);
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 2);

        let worker_heap = heap.clone();
        let third = std::thread::spawn(move || allocate(&worker_heap, 3_u64))
            .join()
            .expect("allocation worker panicked");
        let fourth = std::thread::spawn({
            let heap = heap.clone();
            move || allocate(&heap, 4_u64)
        })
        .join()
        .expect("second allocation worker panicked");
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 4);

        let slots = [first, second, third, fourth]
            .map(|value| {
                heap.inner
                    .resolve_slot(value.erase().as_ptr().as_ptr() as usize)
                    .unwrap()
                    .slot_index
            })
            .into_iter()
            .collect::<HashSet<_>>();
        assert_eq!(slots, HashSet::from([0, 64, 128, 192]));
        assert_eq!(heap.inner.allocation_pressure().assigned_runs, 1);
    }

    #[test]
    fn pressure_request_uses_saturating_assigned_run_occupancy() {
        let mut pressure = AllocationPressure::default();
        for _ in 0..FIXED_SURVIVOR_RUN_HEADROOM - 1 {
            assert!(!pressure.record_run_assignment());
        }
        assert_eq!(pressure.assigned_runs, FIXED_SURVIVOR_RUN_HEADROOM - 1);

        assert!(pressure.record_run_assignment());
        assert_eq!(pressure.assigned_runs, FIXED_SURVIVOR_RUN_HEADROOM);

        pressure.assigned_runs = usize::MAX;
        pressure.high_water_mark = usize::MAX;
        assert!(pressure.record_run_assignment());
        assert_eq!(pressure.assigned_runs, usize::MAX);
    }

    #[test]
    fn survivor_pressure_target_rounds_and_saturates() {
        assert_eq!(survivor_run_high_water_mark(0, 1, 2), 112);
        assert_eq!(survivor_run_high_water_mark(1, 1, 2), 114);
        assert_eq!(survivor_run_high_water_mark(2, 1, 2), 115);
        assert_eq!(survivor_run_high_water_mark(4, 1, 2), 118);

        assert_eq!(survivor_run_high_water_mark(1, 2, 3), 114);
        assert_eq!(survivor_run_high_water_mark(2, 2, 3), 116);
        assert_eq!(survivor_run_high_water_mark(3, 2, 3), 117);

        assert_eq!(survivor_run_high_water_mark(usize::MAX, 1, 2), usize::MAX);
        assert_eq!(
            survivor_run_high_water_mark(usize::MAX / 2 + 1, 2, 3),
            usize::MAX
        );
    }

    #[test]
    fn authoritative_run_assignment_records_exactly_one_pressure_event() {
        let heap = Heap::new();
        let class = internal_class::<FirstType>(&heap);

        for _ in 0..FIXED_SURVIVOR_RUN_HEADROOM - 1 {
            heap.inner.prepare_run(&class).unwrap();
        }
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressureSnapshot {
                assigned_runs: FIXED_SURVIVOR_RUN_HEADROOM - 1,
                high_water_mark: FIXED_SURVIVOR_RUN_HEADROOM,
                collection_requested: false,
            }
        );

        heap.inner.prepare_run(&class).unwrap();
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressureSnapshot {
                assigned_runs: FIXED_SURVIVOR_RUN_HEADROOM,
                high_water_mark: FIXED_SURVIVOR_RUN_HEADROOM,
                collection_requested: true,
            }
        );
        assert_eq!(
            heap.inner.resolved_runs().len(),
            FIXED_SURVIVOR_RUN_HEADROOM
        );
    }

    #[test]
    fn failed_run_publication_exposes_no_frontier_or_pressure() {
        let heap = Heap::new();
        let class = internal_class::<FirstType>(&heap);
        let index = class_index(class.id()).unwrap();
        let mut invalid = heap.inner.data.lock().unwrap().classes[index].geometry();
        invalid.slot_count = 0;

        let error = heap
            .inner
            .data
            .lock()
            .unwrap()
            .publish_run(index, class.id(), invalid, &heap.inner.collection_requested)
            .unwrap_err();

        assert!(matches!(
            error,
            RunPublicationError::Initialization(
                crate::arena::RunInitializationError::InvalidGeometry
            )
        ));
        assert_eq!(class.frontier(&heap.inner), None);
        assert!(heap.inner.resolved_runs().is_empty());
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressureSnapshot::default()
        );
    }

    #[test]
    fn cached_preinitialization_unwind_reuses_the_unpublished_slot() {
        let drops = Arc::new(AtomicUsize::new(0));
        let heap = Heap::new();

        let (first, retried) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<DropCounter>().unwrap();
            let first = allocator.alloc(DropCounter(Arc::clone(&drops)));
            let panic = catch_unwind(AssertUnwindSafe(|| {
                let _ = allocator
                    .alloc_with_before_initialize(DropCounter(Arc::clone(&drops)), || {
                        panic!("injected cached pre-initialization unwind")
                    });
            }));
            assert!(panic.is_err());
            assert_eq!(drops.load(Ordering::Relaxed), 1);
            assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);

            let retried = allocator.alloc(DropCounter(Arc::clone(&drops)));
            (first, retried)
        });

        let slots = [first, retried].map(|value| {
            heap.inner
                .resolve_slot(value.erase().as_ptr().as_ptr() as usize)
                .unwrap()
                .slot_index
        });
        assert_eq!(slots, [0, 1]);
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);

        drop(heap);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn terminal_heap_teardown_waits_for_active_owner_regions() {
        let heap = Heap::new();
        let weak = Arc::downgrade(&heap.inner);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.with_mutator(|mutator| {
                    let allocator = mutator.allocator::<u64>().unwrap();
                    let value = allocator.alloc(91_u64);
                    ready_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    // SAFETY: the worker still owns the heap and remains in
                    // its mutator region until after this access.
                    unsafe { *value.get_unchecked(mutator) }
                })
            }
        });

        ready_rx.recv().unwrap();
        drop(heap);
        assert!(weak.upgrade().is_some());
        release_tx.send(()).unwrap();
        assert_eq!(worker.join().expect("owner-region worker panicked"), 91);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn synchronized_allocator_remains_a_correct_test_reference() {
        let heap = Heap::new();
        let class = internal_class::<FirstType>(&heap);
        let pointer = heap
            .inner
            .allocate_synchronized(&class, FirstType { _value: 73 });
        let resolved = heap.inner.resolve_slot(pointer.as_ptr() as usize).unwrap();
        assert_eq!(resolved.class_id, class.id());
        assert!(std::ptr::eq(resolved.metadata, class.metadata()));
        // SAFETY: the synchronized test allocator returned an initialized
        // `FirstType` pointer and no collection can intervene before this
        // immediate observation.
        assert_eq!(unsafe { pointer.as_ref() }._value, 73);
    }

    #[test]
    fn mutator_entries_track_same_heap_recursion_and_separate_heaps() {
        let first = Heap::new();
        let second = Heap::new();
        let class = internal_class::<FirstType>(&first);

        first.with_mutator(|_| {
            assert_eq!(cache_snapshot(&first.inner).unwrap().recursive_depth, 1);
            assert_eq!(first.inner.coordinator_snapshot().active_outer_mutators, 1);
            insert_cursor(&first.inner, test_cursor(class.id()));

            first.with_mutator(|_| {
                let snapshot = cache_snapshot(&first.inner).unwrap();
                assert_eq!(snapshot.recursive_depth, 2);
                assert_eq!(snapshot.cursor_count, 1);
                assert_eq!(first.inner.coordinator_snapshot().active_outer_mutators, 1);
            });
            assert_eq!(cache_snapshot(&first.inner).unwrap().recursive_depth, 1);

            second.with_mutator(|_| {
                assert_eq!(cache_snapshot(&first.inner).unwrap().recursive_depth, 1);
                assert_eq!(cache_snapshot(&second.inner).unwrap().recursive_depth, 1);
                assert_eq!(first.inner.coordinator_snapshot().active_outer_mutators, 1);
                assert_eq!(second.inner.coordinator_snapshot().active_outer_mutators, 1);
            });
            assert_eq!(second.inner.coordinator_snapshot().active_outer_mutators, 0);
            assert_eq!(cache_snapshot(&second.inner).unwrap().recursive_depth, 0);
        });

        let retained = cache_snapshot(&first.inner).unwrap();
        assert_eq!(retained.recursive_depth, 0);
        assert_eq!(retained.cursor_count, 1);
        assert_eq!(first.inner.coordinator_snapshot().active_outer_mutators, 0);
    }

    #[test]
    fn outer_entry_invalidates_the_whole_cache_after_epoch_change() {
        let heap = Heap::new();
        let class = internal_class::<FirstType>(&heap);
        heap.with_mutator(|_| insert_cursor(&heap.inner, test_cursor(class.id())));

        let prior = cache_snapshot(&heap.inner).unwrap();
        assert_eq!(prior.captured_epoch, AllocationLeaseEpoch::INITIAL);
        assert_eq!(prior.cursor_count, 1);
        let advanced = heap.inner.advance_allocation_lease_epoch();
        assert_ne!(advanced, prior.captured_epoch);
        assert_eq!(cache_snapshot(&heap.inner).unwrap().cursor_count, 1);

        heap.with_mutator(|_| {
            let current = cache_snapshot(&heap.inner).unwrap();
            assert_eq!(current.captured_epoch, advanced);
            assert_eq!(current.cursor_count, 0);
        });
    }

    #[test]
    fn bounded_direct_cache_evicts_colliding_class_records_inertly() {
        let heap = Heap::new();
        let first = AllocationClassId::new(1).unwrap();
        let colliding = AllocationClassId::new(65).unwrap();

        heap.with_mutator(|_| {
            insert_cursor(&heap.inner, test_cursor(first));
            assert_eq!(cursor(&heap.inner, first), Some(test_cursor(first)));
            insert_cursor(&heap.inner, test_cursor(colliding));
            assert_eq!(cache_snapshot(&heap.inner).unwrap().cursor_count, 1);
            assert_eq!(cursor(&heap.inner, first), None);
            assert_eq!(cursor(&heap.inner, colliding), Some(test_cursor(colliding)));
        });
    }

    #[test]
    fn dead_heap_tls_identity_remains_weak_until_explicit_release() {
        let _ = Heap::release_current_thread_caches();
        let heap = Heap::new();
        let address = Arc::as_ptr(&heap.inner) as usize;
        let weak = Arc::downgrade(&heap.inner);
        heap.with_mutator(|_| {});
        assert!(registry_contains(address));

        drop(heap);
        assert!(weak.upgrade().is_none());
        assert!(registry_contains(address));

        let other = Heap::new();
        other.with_mutator(|_| {});
        assert!(registry_contains(address));
        assert_eq!(Heap::release_current_thread_caches(), 2);
        assert!(!registry_contains(address));
        assert!(cache_snapshot(&other.inner).is_none());
    }

    #[test]
    fn explicit_cache_release_validates_all_depths_before_mutation() {
        let _ = Heap::release_current_thread_caches();
        let first = Heap::new();
        let second = Heap::new();
        first.with_mutator(|_| {});

        second.with_mutator(|_| {
            let panic = catch_unwind(AssertUnwindSafe(Heap::release_current_thread_caches));
            assert!(panic.is_err());
            assert!(cache_snapshot(&first.inner).is_some());
            assert_eq!(cache_snapshot(&second.inner).unwrap().recursive_depth, 1);
        });

        assert_eq!(Heap::release_current_thread_caches(), 2);
        assert!(cache_snapshot(&first.inner).is_none());
        assert!(cache_snapshot(&second.inner).is_none());
    }

    #[test]
    fn releasing_live_cache_forgets_cursor_without_changing_run_pressure() {
        let _ = Heap::release_current_thread_caches();
        let heap = Heap::new();
        let first = allocate(&heap, 1_u64);
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);
        assert_eq!(heap.inner.allocation_pressure().assigned_runs, 1);

        assert_eq!(Heap::release_current_thread_caches(), 1);
        let second = allocate(&heap, 2_u64);

        assert_eq!(heap.inner.allocation_cursor_claim_count(), 2);
        assert_eq!(heap.inner.allocation_pressure().assigned_runs, 1);
        assert_eq!(
            [first, second].map(|value| {
                heap.inner
                    .resolve_slot(value.erase().as_ptr().as_ptr() as usize)
                    .unwrap()
                    .slot_index
            }),
            [0, 64]
        );
        assert_eq!(Heap::release_current_thread_caches(), 1);
    }

    #[test]
    fn unwinding_user_code_balances_recursive_mutator_depth() {
        let heap = Heap::new();
        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.with_mutator(|_| panic!("injected mutator-region unwind"));
        }));
        assert!(panic.is_err());
        assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 0);
    }

    const _: fn() = || {
        fn assert_send<T: Send>() {}
        assert_send::<Arena>();
    };
}
