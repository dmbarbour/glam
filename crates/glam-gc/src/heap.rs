use std::collections::HashMap;
use std::num::NonZeroU64;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::{
    AllocationClass, Mutator, Trace,
    arena::{Arena, RunLocation, RunPublicationError},
    class::{AllocationClassEntry, MetadataIdentity, ObjectMetadata, metadata_for},
    run::{AllocationClassId, RunGeometry},
    thread_cache::{
        AllocationCursor, AllocationLeaseEpoch, ThreadHeapEntry, take_deferred_collection_heaps,
        thread_has_active_mutator, thread_has_any_active_mutator,
    },
};

const INITIAL_RUN_PUBLICATION_ALLOWANCE: usize = crate::arena::RUNS_PER_CHUNK * 7 / 8;
const _: () = assert!(INITIAL_RUN_PUBLICATION_ALLOWANCE != 0);
const _: () = assert!(INITIAL_RUN_PUBLICATION_ALLOWANCE < crate::arena::RUNS_PER_CHUNK);

/// One completed stop-the-world collection handshake.
///
/// C3 reports coordination epochs only. Later phases extend the report with
/// marking, reclamation, and finalization statistics without changing the
/// synchronous completion boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CollectionReport {
    epoch: NonZeroU64,
}

impl CollectionReport {
    /// Returns this heap's monotonically increasing collection epoch.
    #[must_use]
    pub const fn epoch(self) -> u64 {
        self.epoch.get()
    }
}

/// A synchronous collection request which cannot enter the coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionError {
    /// The calling thread already holds a mutator for this heap.
    ActiveMutator,
}

impl std::fmt::Display for CollectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActiveMutator => {
                formatter.write_str("cannot collect while this thread holds the heap's mutator")
            }
        }
    }
}

impl std::error::Error for CollectionError {}

/// One shareable, runtime-local managed-value domain.
///
/// C2C's heap owns canonical allocation classes, typed-run topology, and every
/// arena payload. Collection remains disabled, so payloads remain allocated
/// until provisional terminal heap teardown.
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
        let admission = outer.then(|| self.inner.admit_outer_mutator(prepared.admission_kind()));
        after_admission();
        let thread_entry =
            prepared.activate(self.inner.current_allocation_lease_epoch(), admission);
        let mutator = Mutator::new(&self.inner, thread_entry.cache());
        let result = operation(&mutator);
        drop(mutator);
        drop(thread_entry);

        // A nested cross-heap exit must not synchronously become a collector:
        // doing so could wait on the outer heap while an opposite nesting waits
        // on this one. The last heap region on this thread is a safe servicing
        // boundary for coalesced requests.
        if outer && !thread_has_any_active_mutator() {
            for deferred in take_deferred_collection_heaps() {
                deferred.service_requested_collections();
            }
            self.inner.service_requested_collections();
        }
        result
    }

    /// Records an idempotent, nonblocking full-collection request.
    ///
    /// The request is serviced at a later outer mutator exit or by
    /// [`Heap::collect_full`]. Calling this inside a mutator never waits on the
    /// calling region.
    pub fn request_collection(&self) {
        self.inner.request_collection();
    }

    /// Completes a full stop-the-world collection handshake synchronously.
    ///
    /// C3 performs no tracing or reclamation. It nevertheless establishes the
    /// same request, election, exclusion, and completion boundary which later
    /// phases use for real full collection.
    pub fn collect_full(&self) -> Result<CollectionReport, CollectionError> {
        if thread_has_active_mutator(&self.inner) {
            return Err(CollectionError::ActiveMutator);
        }
        Ok(self.inner.collect_full())
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
    state: Mutex<HeapState>,
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
}

#[cfg(test)]
#[derive(Clone)]
struct AllocationCursorSlowPathHook {
    arrived: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

impl Default for HeapInner {
    fn default() -> Self {
        Self {
            state: Mutex::new(HeapState::default()),
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
        }
    }
}

#[derive(Default)]
struct HeapState {
    #[allow(
        dead_code,
        reason = "C2B.3 run state becomes a live allocator path in C2C"
    )]
    arena: Arena,
    classes_by_metadata: HashMap<MetadataIdentity, AllocationClassId>,
    classes: Vec<AllocationClassEntry>,
    allocation_pressure: AllocationPressure,
    coordinator: MutatorCoordinator,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MutatorCoordinator {
    phase: AdmissionPhase,
    active_outer_mutators: usize,
    collection_requested: bool,
    active_collection: Option<CollectionEpoch>,
    completed_collection_epoch: u64,
    #[cfg(test)]
    blocked_outer_mutators: usize,
    #[cfg(test)]
    blocked_collection_waiters: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AdmissionPhase {
    #[default]
    Ordinary,
    ExclusivePending,
    Exclusive,
    Finalizing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdmissionKind {
    Ordinary,
    Dependent,
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
        let mut state = self
            .heap
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.coordinator.phase, AdmissionPhase::Exclusive);
        assert_eq!(state.coordinator.active_outer_mutators, 0);
        state.coordinator.phase = AdmissionPhase::Ordinary;
        self.heap.admission_changed.notify_all();
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AllocationPressure {
    published_runs: usize,
    collection_requested: bool,
}

impl AllocationPressure {
    fn record_run_publication(&mut self) -> bool {
        let was_requested = self.collection_requested;
        self.published_runs = self.published_runs.saturating_add(1);
        self.collection_requested |= self.published_runs >= INITIAL_RUN_PUBLICATION_ALLOWANCE;
        !was_requested && self.collection_requested
    }
}

impl HeapState {
    fn publish_run(
        &mut self,
        class_index: usize,
        class_id: AllocationClassId,
        geometry: RunGeometry,
    ) -> Result<RunLocation, RunPublicationError> {
        // Reserve the class-pool entry before publishing arena state. After a
        // successful arena publication, all remaining operations are
        // infallible and the pressure event is recorded exactly once.
        self.classes[class_index].reserve_run();
        let location = self.arena.publish_run(class_id, geometry)?;
        let target = self
            .arena
            .run_claim_target(location)
            .expect("published typed run must expose stable claim topology");
        self.classes[class_index].publish_run(target);
        if self.allocation_pressure.record_run_publication() {
            self.coordinator.request_collection();
        }
        Ok(location)
    }
}

impl MutatorCoordinator {
    fn request_collection(&mut self) {
        // A request which arrives before exclusion is fixed can join that
        // collection. Once exclusion is authoritative, the completed trace
        // would not cover subsequent work, so retain a follow-up obligation.
        match self.phase {
            AdmissionPhase::Ordinary | AdmissionPhase::Exclusive | AdmissionPhase::Finalizing => {
                self.collection_requested = true;
            }
            AdmissionPhase::ExclusivePending => {}
        }
    }

    fn request_synchronous_collection(&mut self) -> CollectionEpoch {
        match self.phase {
            AdmissionPhase::Ordinary => {
                self.collection_requested = true;
                CollectionEpoch::after(self.completed_collection_epoch)
            }
            AdmissionPhase::ExclusivePending => self
                .active_collection
                .expect("pending collection must have an epoch"),
            AdmissionPhase::Exclusive => {
                self.collection_requested = true;
                let active = self
                    .active_collection
                    .expect("exclusive collection must have an epoch");
                CollectionEpoch::after(active.get())
            }
            AdmissionPhase::Finalizing => {
                self.collection_requested = true;
                let active = self
                    .active_collection
                    .expect("finalizing collection must have an epoch");
                CollectionEpoch::after(active.get())
            }
        }
    }

    fn elect_collection(&mut self) -> Option<CollectionEpoch> {
        if self.phase != AdmissionPhase::Ordinary || !self.collection_requested {
            return None;
        }
        let epoch = CollectionEpoch::after(self.completed_collection_epoch);
        self.collection_requested = false;
        self.active_collection = Some(epoch);
        self.phase = AdmissionPhase::ExclusivePending;
        Some(epoch)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "C2B.3 run-publication failures become allocator-visible in C2C"
)]
enum PrepareRunError {
    ForeignClass,
    InvalidClass,
    Publication(RunPublicationError),
}

#[derive(Clone, Copy)]
#[allow(
    dead_code,
    reason = "C2B.3 checked slot resolution becomes the access proof in C2C"
)]
struct ResolvedSlot {
    metadata: &'static ObjectMetadata,
    class_id: AllocationClassId,
    geometry: RunGeometry,
    slot_index: usize,
}

#[derive(Clone, Copy)]
#[allow(
    dead_code,
    reason = "C2B.3 run enumeration becomes collector input after allocation starts"
)]
struct ResolvedRun {
    location: RunLocation,
    metadata: &'static ObjectMetadata,
    class_id: AllocationClassId,
    geometry: RunGeometry,
}

impl HeapInner {
    fn admit_outer_mutator(&self, kind: AdmissionKind) -> MutatorAdmission<'_> {
        let mut state = self
            .state
            .lock()
            .expect("heap state should not be poisoned");
        while !admission_is_open(&state.coordinator, kind) {
            #[cfg(test)]
            {
                state.coordinator.blocked_outer_mutators += 1;
                self.admission_changed.notify_all();
            }
            state = self
                .admission_changed
                .wait(state)
                .expect("heap state should not be poisoned");
            #[cfg(test)]
            {
                state.coordinator.blocked_outer_mutators -= 1;
                self.admission_changed.notify_all();
            }
        }
        state.coordinator.active_outer_mutators = state
            .coordinator
            .active_outer_mutators
            .checked_add(1)
            .expect("active mutator count exhausted");
        MutatorAdmission { heap: self }
    }

    fn request_collection(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.coordinator.request_collection();
        self.admission_changed.notify_all();
    }

    fn collect_full(self: &Arc<Self>) -> CollectionReport {
        let target = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let target = state.coordinator.request_synchronous_collection();
            self.admission_changed.notify_all();
            target
        };

        loop {
            let elected = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                loop {
                    if state.coordinator.completed_collection_epoch >= target.get() {
                        return CollectionReport { epoch: target.0 };
                    }
                    if let Some(elected) = state.coordinator.elect_collection() {
                        self.admission_changed.notify_all();
                        break elected;
                    }
                    #[cfg(test)]
                    {
                        state.coordinator.blocked_collection_waiters += 1;
                        self.admission_changed.notify_all();
                    }
                    state = self
                        .admission_changed
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    #[cfg(test)]
                    {
                        state.coordinator.blocked_collection_waiters -= 1;
                        self.admission_changed.notify_all();
                    }
                }
            };
            let mut elected = Some(elected);
            while let Some(epoch) = elected {
                elected = self.run_synthetic_collection(epoch, || {}, |_| {});
            }
        }
    }

    fn service_requested_collections(self: &Arc<Self>) {
        loop {
            let elected = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.coordinator.elect_collection()
            };
            let Some(elected) = elected else {
                return;
            };
            self.admission_changed.notify_all();
            let mut elected = Some(elected);
            while let Some(epoch) = elected {
                elected = self.run_synthetic_collection(epoch, || {}, |_| {});
            }
        }
    }

    fn run_synthetic_collection(
        self: &Arc<Self>,
        epoch: CollectionEpoch,
        exclusive_work: impl FnOnce(),
        finalizer_work: impl for<'mutator> FnOnce(&Mutator<'mutator>),
    ) -> Option<CollectionEpoch> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            state.coordinator.active_collection,
            Some(epoch),
            "collector epoch is no longer authoritative"
        );
        assert_eq!(
            state.coordinator.phase,
            AdmissionPhase::ExclusivePending,
            "elected collector must begin pending"
        );
        while state.coordinator.active_outer_mutators != 0 {
            state = self
                .admission_changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.coordinator.phase = AdmissionPhase::Exclusive;
        self.admission_changed.notify_all();
        drop(state);

        let mut attempt = CollectionAttempt::new(self, epoch);
        exclusive_work();

        // Prepare the TLS record without activating it. Under the heap-state
        // mutex, exclusive authority is then converted directly into one
        // collector-owned mutator obligation. No ordinary entrant can observe
        // a gap in which neither authority is present.
        let prepared = ThreadHeapEntry::prepare(self, self.current_allocation_lease_epoch());
        assert!(
            prepared.is_outer(),
            "collector thread unexpectedly holds a mutator for its target heap"
        );
        let admission = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(state.coordinator.phase, AdmissionPhase::Exclusive);
            assert_eq!(state.coordinator.active_outer_mutators, 0);
            assert_eq!(state.coordinator.active_collection, Some(epoch));
            state.coordinator.phase = AdmissionPhase::Finalizing;
            state.coordinator.active_outer_mutators = 1;
            self.admission_changed.notify_all();
            MutatorAdmission { heap: self }
        };
        let thread_entry =
            prepared.activate(self.current_allocation_lease_epoch(), Some(admission));
        let mutator = Mutator::new(self, thread_entry.cache());
        finalizer_work(&mutator);
        drop(mutator);
        drop(thread_entry);

        attempt.complete()
    }

    fn release_outer_mutator(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.coordinator.active_outer_mutators = state
            .coordinator
            .active_outer_mutators
            .checked_sub(1)
            .expect("active mutator count underflow");
        if state.coordinator.active_outer_mutators == 0 {
            self.admission_changed.notify_all();
        }
    }

    #[cfg(test)]
    fn enter_synthetic_exclusive(&self) -> SyntheticExclusiveAdmission<'_> {
        let mut state = self
            .state
            .lock()
            .expect("heap state should not be poisoned");
        while state.coordinator.phase != AdmissionPhase::Ordinary {
            state = self
                .admission_changed
                .wait(state)
                .expect("heap state should not be poisoned");
        }
        state.coordinator.phase = AdmissionPhase::ExclusivePending;
        self.admission_changed.notify_all();
        while state.coordinator.active_outer_mutators != 0 {
            state = self
                .admission_changed
                .wait(state)
                .expect("heap state should not be poisoned");
        }
        state.coordinator.phase = AdmissionPhase::Exclusive;
        self.admission_changed.notify_all();
        SyntheticExclusiveAdmission { heap: self }
    }

    #[cfg(test)]
    fn coordinator_snapshot(&self) -> MutatorCoordinator {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .coordinator
    }

    #[cfg(test)]
    fn wait_for_blocked_outer_mutators(&self, expected: usize) {
        let mut state = self
            .state
            .lock()
            .expect("heap state should not be poisoned");
        while state.coordinator.blocked_outer_mutators != expected {
            state = self
                .admission_changed
                .wait(state)
                .expect("heap state should not be poisoned");
        }
    }

    #[cfg(test)]
    fn wait_for_admission_phase(&self, expected: AdmissionPhase) {
        let mut state = self
            .state
            .lock()
            .expect("heap state should not be poisoned");
        while state.coordinator.phase != expected {
            state = self
                .admission_changed
                .wait(state)
                .expect("heap state should not be poisoned");
        }
    }

    #[cfg(test)]
    fn wait_for_collection_waiters(&self, expected: usize) {
        let mut state = self
            .state
            .lock()
            .expect("heap state should not be poisoned");
        while state.coordinator.blocked_collection_waiters != expected {
            state = self
                .admission_changed
                .wait(state)
                .expect("heap state should not be poisoned");
        }
    }

    fn current_allocation_lease_epoch(&self) -> AllocationLeaseEpoch {
        AllocationLeaseEpoch::from_raw(self.allocation_lease_epoch.load(Ordering::Acquire))
            .expect("allocation lease epoch must remain nonzero")
    }

    #[cfg(test)]
    fn advance_allocation_lease_epoch(&self) -> AllocationLeaseEpoch {
        let prior = self
            .allocation_lease_epoch
            .try_update(Ordering::AcqRel, Ordering::Acquire, |prior| {
                prior.checked_add(1).filter(|next| *next != 0)
            })
            .expect("allocation lease epoch exhausted");
        AllocationLeaseEpoch::from_raw(prior + 1)
            .expect("advanced allocation lease epoch must remain nonzero")
    }

    pub(crate) fn discover_class<T: Trace>(
        self: &Arc<Self>,
        metadata: &'static ObjectMetadata,
        geometry: RunGeometry,
    ) -> AllocationClass<T> {
        self.discover_class_with(metadata, geometry, || {
            AllocationClassEntry::new(metadata, geometry)
        })
    }

    fn discover_class_with<T: Trace>(
        self: &Arc<Self>,
        metadata: &'static ObjectMetadata,
        geometry: RunGeometry,
        make_candidate: impl FnOnce() -> AllocationClassEntry,
    ) -> AllocationClass<T> {
        let identity = MetadataIdentity::new(metadata);
        {
            let state = self
                .state
                .lock()
                .expect("heap state should not be poisoned");
            if let Some(id) = state.classes_by_metadata.get(&identity).copied() {
                let shared = Arc::clone(
                    state.classes[class_index(id).expect("known class ID must be valid")].shared(),
                );
                return AllocationClass::new(Arc::clone(self), metadata, id, shared);
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

        let mut state = self
            .state
            .lock()
            .expect("heap state should not be poisoned");
        if let Some(id) = state.classes_by_metadata.get(&identity).copied() {
            let shared = Arc::clone(
                state.classes[class_index(id).expect("known class ID must be valid")].shared(),
            );
            return AllocationClass::new(Arc::clone(self), metadata, id, shared);
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

        AllocationClass::new(Arc::clone(self), metadata, next, shared)
    }

    #[allow(
        dead_code,
        reason = "C2B.3 run publication is consumed by the C2C allocator"
    )]
    fn prepare_run<T: Trace>(
        &self,
        class: &AllocationClass<T>,
    ) -> Result<RunLocation, PrepareRunError> {
        if !class.belongs_to(self) {
            return Err(PrepareRunError::ForeignClass);
        }

        let mut state = self
            .state
            .lock()
            .expect("heap state should not be poisoned");
        let index = class_index(class.id()).ok_or(PrepareRunError::InvalidClass)?;
        let entry = state
            .classes
            .get(index)
            .ok_or(PrepareRunError::InvalidClass)?;
        if !std::ptr::eq(entry.metadata(), class.metadata()) {
            return Err(PrepareRunError::InvalidClass);
        }
        let geometry = entry.geometry();

        let location = state
            .publish_run(index, class.id(), geometry)
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

        if let Some(claimed) = class.claim_frontier() {
            return allocation_cursor(class.id(), claimed);
        }

        #[cfg(test)]
        self.allocation_cursor_slow_paths
            .fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        self.pause_before_allocation_cursor_slow_path();
        let mut state = self
            .state
            .lock()
            .expect("heap state should not be poisoned");
        let index = class_index(class.id()).expect("allocation class has an invalid ID");
        let geometry = {
            let entry = state
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
        if let Some(claimed) = class.claim_frontier() {
            #[cfg(test)]
            self.allocation_cursor_locked_recheck_hits
                .fetch_add(1, Ordering::Relaxed);
            return allocation_cursor(class.id(), claimed);
        }

        loop {
            #[cfg(test)]
            self.allocation_cursor_frontier_advance_attempts
                .fetch_add(1, Ordering::Relaxed);
            if let Some(target) = state.classes[index].advance_frontier() {
                if let Some(claimed) = target.claim_allocation_word() {
                    return allocation_cursor(class.id(), claimed);
                }
                continue;
            }

            state
                .publish_run(index, class.id(), geometry)
                .unwrap_or_else(|error| panic!("managed run allocation failed: {error:?}"));
            let target = state.classes[index].activate_last_run();
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
    fn allocation_pressure(&self) -> AllocationPressure {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .allocation_pressure
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

        let mut state = self
            .state
            .lock()
            .expect("heap state should not be poisoned");
        let index = class_index(class.id()).expect("allocation class has an invalid ID");
        let entry = state
            .classes
            .get(index)
            .expect("allocation class is absent from its heap");
        assert!(
            std::ptr::eq(entry.metadata(), class.metadata()),
            "allocation class metadata does not match its heap entry"
        );
        let geometry = entry.geometry();

        let selected = entry.runs().iter().find_map(|run| {
            state
                .arena
                .first_free_slot(run.location)
                .map(|slot_index| (run.location, slot_index))
        });
        let (location, slot_index) = if let Some(selected) = selected {
            selected
        } else {
            let location = state
                .publish_run(index, class.id(), geometry)
                .unwrap_or_else(|error| panic!("managed run allocation failed: {error:?}"));
            (location, 0)
        };

        // This hook exists only to latch that selecting a currently free slot
        // publishes no state. Production initialization below contains no
        // panicking operation between writing `T` and its allocation bit.
        before_initialize();
        state
            .arena
            .initialize_slot(location, class.id(), geometry, slot_index, value)
    }

    #[allow(
        dead_code,
        reason = "C2B.3 metadata resolution becomes the access proof in C2C"
    )]
    fn resolve_slot(&self, address: usize) -> Option<ResolvedSlot> {
        let state = self.state.lock().ok()?;
        resolve_slot_in_state(&state, address)
    }

    #[allow(
        dead_code,
        reason = "C2B.3 run enumeration becomes mark and sweep input later"
    )]
    fn resolved_runs(&self) -> Vec<ResolvedRun> {
        let state = self
            .state
            .lock()
            .expect("heap state should not be poisoned");
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
            let address = pointer.as_ptr().cast::<()>() as usize;
            let state = self
                .state
                .lock()
                .expect("heap state should not be poisoned");
            let resolved = resolve_slot_in_state(&state, address);
            drop(state);
            let metadata = resolved
                .map(|slot| slot.metadata)
                .unwrap_or_else(|| panic!("managed pointer does not belong to this heap"));

            assert!(
                std::ptr::eq(metadata, expected),
                "managed pointer has representation `{}`, not requested `{}`",
                metadata.type_name(),
                expected.type_name()
            );
        }

        #[cfg(not(debug_assertions))]
        let _ = pointer;
    }
}

fn admission_is_open(coordinator: &MutatorCoordinator, kind: AdmissionKind) -> bool {
    match coordinator.phase {
        AdmissionPhase::Ordinary => true,
        AdmissionPhase::ExclusivePending => kind == AdmissionKind::Dependent,
        AdmissionPhase::Exclusive => false,
        AdmissionPhase::Finalizing => {
            !coordinator.collection_requested || kind == AdmissionKind::Dependent
        }
    }
}

struct CollectionAttempt<'heap> {
    heap: &'heap HeapInner,
    epoch: CollectionEpoch,
    completed: bool,
}

impl<'heap> CollectionAttempt<'heap> {
    fn new(heap: &'heap HeapInner, epoch: CollectionEpoch) -> Self {
        Self {
            heap,
            epoch,
            completed: false,
        }
    }

    fn complete(&mut self) -> Option<CollectionEpoch> {
        let mut state = self
            .heap
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.coordinator.phase, AdmissionPhase::Finalizing);
        assert_eq!(state.coordinator.active_collection, Some(self.epoch));
        state.coordinator.completed_collection_epoch = self.epoch.get();
        let next = if state.coordinator.collection_requested {
            let next = CollectionEpoch::after(self.epoch.get());
            state.coordinator.collection_requested = false;
            state.coordinator.active_collection = Some(next);
            state.coordinator.phase = AdmissionPhase::ExclusivePending;
            Some(next)
        } else {
            state.coordinator.active_collection = None;
            state.coordinator.phase = AdmissionPhase::Ordinary;
            None
        };
        self.completed = true;
        self.heap.admission_changed.notify_all();
        next
    }
}

impl Drop for CollectionAttempt<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let mut state = self
            .heap
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.coordinator.active_collection == Some(self.epoch) {
            state.coordinator.active_collection = None;
            state.coordinator.collection_requested = true;
            state.coordinator.phase = AdmissionPhase::Ordinary;
        }
        self.heap.admission_changed.notify_all();
    }
}

fn resolve_slot_in_state(state: &HeapState, address: usize) -> Option<ResolvedSlot> {
    let owner = state.arena.checked_slot_owner(address)?;
    let entry = state.classes.get(class_index(owner.class_id)?)?;
    if entry.geometry() != owner.geometry || !entry.contains_run(owner.location) {
        return None;
    }
    Some(ResolvedSlot {
        metadata: entry.metadata(),
        class_id: owner.class_id,
        geometry: owner.geometry,
        slot_index: owner.slot_index,
    })
}

impl Drop for HeapInner {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for entry in &state.classes {
            let metadata = entry.metadata();
            if !metadata.needs_drop() {
                continue;
            }
            for run in entry.runs() {
                for pointer in state.arena.allocated_slot_pointers(run.location) {
                    // SAFETY: the allocation bitmap is published only after a
                    // value with this run's canonical metadata is initialized.
                    // Final heap ownership is exclusive, and this provisional
                    // teardown visits each allocated slot exactly once before
                    // arena storage is released.
                    unsafe { metadata.drop_in_place(pointer) };
                }
            }
        }
    }
}

#[allow(
    dead_code,
    reason = "C2B.3 dense class lookup becomes allocator and collector input"
)]
fn class_index(id: AllocationClassId) -> Option<usize> {
    usize::try_from(id.get().checked_sub(1)?).ok()
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, mpsc};

    use crate::{
        Trace, UnsupportedLayout, Visitor,
        arena::Arena,
        class::metadata_for,
        run::{AllocationClassId, RunGeometry},
        thread_cache::{
            AllocationCursor, AllocationLeaseEpoch, cache_snapshot, cursor, insert_cursor,
            registry_contains,
        },
    };

    use super::{
        AdmissionPhase, AllocationPressure, Heap, INITIAL_RUN_PUBLICATION_ALLOWANCE,
        PrepareRunError, RunLocation, RunPublicationError, class_index,
    };

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
                .state
                .lock()
                .expect("test arena should not be poisoned");
            let chunk = arena.arena.reserve_chunk().unwrap();
            arena.arena.run_address(chunk, 0).unwrap().address()
        };

        assert!(
            first
                .inner
                .state
                .lock()
                .unwrap()
                .arena
                .find_run(address)
                .is_some()
        );
        assert!(
            second
                .inner
                .state
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
        let expected = heap
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();
        let handles = (0..THREADS)
            .map(|_| {
                let heap = heap.clone();
                std::thread::spawn(move || {
                    heap.with_mutator(|mutator| mutator.allocation_class::<FirstType>())
                        .unwrap()
                })
            })
            .map(|thread| thread.join().expect("class-discovery worker panicked"))
            .collect::<Vec<_>>();

        assert!(handles.iter().all(|class| class.id() == expected.id()));
        assert!(
            handles
                .iter()
                .all(|class| std::ptr::eq(class.metadata(), expected.metadata()))
        );
        assert!(handles.iter().all(|class| class.belongs_to(&heap.inner)));
        assert_eq!(heap.inner.state.lock().unwrap().classes.len(), 1);
    }

    #[test]
    fn class_discovery_waits_for_synthetic_exclusive_admission() {
        let heap = Heap::new();
        let exclusive = heap.inner.enter_synthetic_exclusive();
        let worker = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.with_mutator(|mutator| mutator.allocation_class::<FirstType>())
                    .unwrap()
            }
        });

        heap.inner.wait_for_blocked_outer_mutators(1);
        let blocked = heap.inner.coordinator_snapshot();
        assert_eq!(blocked.phase, AdmissionPhase::Exclusive);
        assert_eq!(blocked.active_outer_mutators, 0);
        assert_eq!(blocked.blocked_outer_mutators, 1);

        drop(exclusive);
        let class = worker.join().expect("class-discovery worker panicked");
        assert!(class.belongs_to(&heap.inner));
        assert_eq!(class.id().get(), 1);
        assert_eq!(heap.inner.state.lock().unwrap().classes.len(), 1);
    }

    #[test]
    fn retained_class_handle_remains_usable_after_discovery_region_exits() {
        let heap = Heap::new();
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();
        assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 0);

        let value = heap.with_mutator(|mutator| mutator.alloc(&class, FirstType { _value: 41 }));
        let resolved = heap
            .inner
            .resolve_slot(value.erase().as_ptr().as_ptr() as usize)
            .expect("retained class handle must allocate into its original heap");
        assert_eq!(resolved.class_id, class.id());
        assert!(std::ptr::eq(resolved.metadata, class.metadata()));
    }

    #[test]
    fn recursive_class_discovery_reuses_admission_while_exclusive_is_pending() {
        let heap = Heap::new();

        let (class, collector) = heap.with_mutator(|_| {
            let collector = std::thread::spawn({
                let heap = heap.clone();
                move || drop(heap.inner.enter_synthetic_exclusive())
            });
            heap.inner
                .wait_for_admission_phase(AdmissionPhase::ExclusivePending);

            let class = heap
                .with_mutator(|mutator| mutator.allocation_class::<SecondType>())
                .unwrap();
            assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 1);
            assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 1);
            (class, collector)
        });

        collector.join().expect("synthetic collector panicked");
        assert!(class.belongs_to(&heap.inner));
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
    fn pending_exclusive_blocks_fresh_outer_admission_until_release() {
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

        let (exclusive_tx, exclusive_rx) = mpsc::channel();
        let (release_exclusive_tx, release_exclusive_rx) = mpsc::channel();
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                let exclusive = heap.inner.enter_synthetic_exclusive();
                exclusive_tx.send(()).unwrap();
                release_exclusive_rx.recv().unwrap();
                drop(exclusive);
            }
        });
        heap.inner
            .wait_for_admission_phase(AdmissionPhase::ExclusivePending);

        let (entered_tx, entered_rx) = mpsc::channel();
        let entrant = std::thread::spawn({
            let heap = heap.clone();
            move || heap.with_mutator(|_| entered_tx.send(()).unwrap())
        });
        heap.inner.wait_for_blocked_outer_mutators(1);

        release_active_tx.send(()).unwrap();
        active.join().expect("active mutator panicked");
        exclusive_rx.recv().unwrap();
        assert_eq!(
            heap.inner.coordinator_snapshot().phase,
            AdmissionPhase::Exclusive
        );
        assert_eq!(heap.inner.coordinator_snapshot().blocked_outer_mutators, 1);
        assert!(entered_rx.try_recv().is_err());

        release_exclusive_tx.send(()).unwrap();
        collector.join().expect("synthetic collector panicked");
        entrant.join().expect("blocked entrant panicked");
        entered_rx.recv().unwrap();
        assert_eq!(
            heap.inner.coordinator_snapshot().phase,
            AdmissionPhase::Ordinary
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
    fn explicit_request_is_nonblocking_and_serviced_at_outer_exit() {
        let heap = Heap::new();

        heap.request_collection();
        let requested = heap.inner.coordinator_snapshot();
        assert_eq!(requested.phase, AdmissionPhase::Ordinary);
        assert!(requested.collection_requested);
        assert_eq!(requested.completed_collection_epoch, 0);

        heap.with_mutator(|_| {
            assert_eq!(
                heap.inner.coordinator_snapshot().completed_collection_epoch,
                0
            );
        });

        let completed = heap.inner.coordinator_snapshot();
        assert_eq!(completed.phase, AdmissionPhase::Ordinary);
        assert!(!completed.collection_requested);
        assert_eq!(completed.completed_collection_epoch, 1);
    }

    #[test]
    fn recursive_request_is_serviced_only_after_the_outer_exit() {
        let heap = Heap::new();

        heap.with_mutator(|_| {
            heap.with_mutator(|_| heap.request_collection());
            let pending = heap.inner.coordinator_snapshot();
            assert!(pending.collection_requested);
            assert_eq!(pending.completed_collection_epoch, 0);
            assert_eq!(pending.active_outer_mutators, 1);
        });

        let completed = heap.inner.coordinator_snapshot();
        assert_eq!(completed.completed_collection_epoch, 1);
        assert_eq!(completed.active_outer_mutators, 0);
    }

    #[test]
    fn synchronous_collection_rejects_a_same_thread_active_mutator() {
        let heap = Heap::new();

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
    }

    #[test]
    fn synchronous_collection_drains_active_mutators_and_blocks_a_fresh_entrant() {
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
        heap.inner
            .wait_for_admission_phase(AdmissionPhase::ExclusivePending);

        let (entered_tx, entered_rx) = mpsc::channel();
        let entrant = std::thread::spawn({
            let heap = heap.clone();
            move || heap.with_mutator(|_| entered_tx.send(()).unwrap())
        });
        heap.inner.wait_for_blocked_outer_mutators(1);
        assert!(entered_rx.try_recv().is_err());

        release_tx.send(()).unwrap();
        active.join().unwrap();
        assert_eq!(collector.join().unwrap().epoch(), 1);
        entrant.join().unwrap();
        entered_rx.recv().unwrap();
    }

    #[test]
    fn synchronous_requesters_coalesce_on_one_pending_collection() {
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

        let first = std::thread::spawn({
            let heap = heap.clone();
            move || heap.collect_full().unwrap()
        });
        heap.inner
            .wait_for_admission_phase(AdmissionPhase::ExclusivePending);
        let second = std::thread::spawn({
            let heap = heap.clone();
            move || heap.collect_full().unwrap()
        });
        heap.inner.wait_for_collection_waiters(1);

        release_tx.send(()).unwrap();
        active.join().unwrap();
        assert_eq!(first.join().unwrap().epoch(), 1);
        assert_eq!(second.join().unwrap().epoch(), 1);
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            1
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
        heap.inner
            .wait_for_admission_phase(AdmissionPhase::ExclusivePending);

        drop(heap);
        release_tx.send(()).unwrap();
        active.join().unwrap();
        assert_eq!(waiter.join().unwrap().epoch(), 1);
    }

    #[test]
    fn reciprocal_dependent_entries_pass_two_pending_collectors() {
        let first = Heap::new();
        let second = Heap::new();
        let active = Arc::new(Barrier::new(3));
        let enter_dependents = Arc::new(Barrier::new(3));
        let dependent_entries = Arc::new(AtomicUsize::new(0));

        let first_then_second = std::thread::spawn({
            let first = first.clone();
            let second = second.clone();
            let active = Arc::clone(&active);
            let enter_dependents = Arc::clone(&enter_dependents);
            let dependent_entries = Arc::clone(&dependent_entries);
            move || {
                first.with_mutator(|_| {
                    active.wait();
                    enter_dependents.wait();
                    second.with_mutator(|_| {
                        dependent_entries.fetch_add(1, Ordering::Relaxed);
                    });
                });
            }
        });
        let second_then_first = std::thread::spawn({
            let first = first.clone();
            let second = second.clone();
            let active = Arc::clone(&active);
            let enter_dependents = Arc::clone(&enter_dependents);
            let dependent_entries = Arc::clone(&dependent_entries);
            move || {
                second.with_mutator(|_| {
                    active.wait();
                    enter_dependents.wait();
                    first.with_mutator(|_| {
                        dependent_entries.fetch_add(1, Ordering::Relaxed);
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
        first
            .inner
            .wait_for_admission_phase(AdmissionPhase::ExclusivePending);
        second
            .inner
            .wait_for_admission_phase(AdmissionPhase::ExclusivePending);

        enter_dependents.wait();
        first_then_second.join().unwrap();
        second_then_first.join().unwrap();
        assert_eq!(dependent_entries.load(Ordering::Relaxed), 2);
        assert_eq!(first_collector.join().unwrap().epoch(), 1);
        assert_eq!(second_collector.join().unwrap().epoch(), 1);
    }

    #[test]
    fn nested_heap_request_is_deferred_until_the_thread_leaves_its_outer_heap() {
        let outer = Heap::new();
        let nested = Heap::new();

        outer.with_mutator(|_| {
            nested.with_mutator(|_| nested.request_collection());
            let pending = nested.inner.coordinator_snapshot();
            assert!(pending.collection_requested);
            assert_eq!(pending.completed_collection_epoch, 0);
        });

        let completed = nested.inner.coordinator_snapshot();
        assert!(!completed.collection_requested);
        assert_eq!(completed.completed_collection_epoch, 1);
    }

    #[test]
    fn caught_nested_unwind_preserves_deferred_collection_service() {
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

        assert_eq!(
            nested
                .inner
                .coordinator_snapshot()
                .completed_collection_epoch,
            1
        );
    }

    #[test]
    fn dependent_entry_waits_once_the_target_collector_is_exclusive() {
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
    fn automatic_pressure_request_is_serviced_once_but_remains_latched() {
        let heap = Heap::new();
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();

        heap.with_mutator(|_| {
            for _ in 0..INITIAL_RUN_PUBLICATION_ALLOWANCE {
                heap.inner.prepare_run(&class).unwrap();
            }
            let state = heap.inner.state.lock().unwrap();
            assert!(state.allocation_pressure.collection_requested);
            assert!(state.coordinator.collection_requested);
            assert_eq!(state.coordinator.completed_collection_epoch, 0);
        });

        let state = heap.inner.state.lock().unwrap();
        assert!(state.allocation_pressure.collection_requested);
        assert!(!state.coordinator.collection_requested);
        assert_eq!(state.coordinator.completed_collection_epoch, 1);
        assert_eq!(state.allocation_pressure.published_runs, 112);
    }

    #[test]
    fn panicking_elected_collection_restores_and_relatches_its_request() {
        let heap = Heap::new();
        heap.request_collection();
        let epoch = {
            let mut state = heap.inner.state.lock().unwrap();
            state.coordinator.elect_collection().unwrap()
        };

        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.inner.run_synthetic_collection(
                epoch,
                || panic!("injected collection panic"),
                |_| {},
            );
        }));
        assert!(panic.is_err());

        let restored = heap.inner.coordinator_snapshot();
        assert_eq!(restored.phase, AdmissionPhase::Ordinary);
        assert!(restored.collection_requested);
        assert_eq!(restored.active_collection, None);
        heap.inner.service_requested_collections();
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            1
        );
    }

    #[test]
    fn finalizer_handoff_installs_one_recursive_current_mutator_without_a_gap() {
        let heap = Heap::new();
        heap.request_collection();
        let epoch = {
            let mut state = heap.inner.state.lock().unwrap();
            state.coordinator.elect_collection().unwrap()
        };

        let next = heap.inner.run_synthetic_collection(
            epoch,
            || {
                let coordinator = heap.inner.coordinator_snapshot();
                assert_eq!(coordinator.phase, AdmissionPhase::Exclusive);
                assert_eq!(coordinator.active_outer_mutators, 0);
            },
            |finalizer_mutator| {
                let coordinator = heap.inner.coordinator_snapshot();
                assert_eq!(coordinator.phase, AdmissionPhase::Finalizing);
                assert_eq!(coordinator.active_outer_mutators, 1);
                assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 1);

                let class = finalizer_mutator.allocation_class::<SecondType>().unwrap();
                heap.with_mutator(|recursive| {
                    assert_eq!(heap.inner.coordinator_snapshot().active_outer_mutators, 1);
                    assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 2);
                    let _ = recursive.alloc(&class, SecondType { _value: 19 });
                });
            },
        );

        assert_eq!(next, None);
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
        let epoch = {
            let mut state = heap.inner.state.lock().unwrap();
            state.coordinator.elect_collection().unwrap()
        };
        let (finalizing_tx, finalizing_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                heap.inner.run_synthetic_collection(
                    epoch,
                    || {},
                    |_| {
                        finalizing_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    },
                )
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
        assert_eq!(collector.join().unwrap(), None);
        assert_eq!(
            heap.inner.coordinator_snapshot().phase,
            AdmissionPhase::Ordinary
        );
    }

    #[test]
    fn request_during_finalization_commits_a_followup_without_an_admission_gap() {
        let heap = Heap::new();
        heap.request_collection();
        let first_epoch = {
            let mut state = heap.inner.state.lock().unwrap();
            state.coordinator.elect_collection().unwrap()
        };
        let (finalizing_tx, finalizing_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                let next = heap.inner.run_synthetic_collection(
                    first_epoch,
                    || {},
                    |_| {
                        finalizing_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    },
                );
                let next = next.expect("finalization request must commit a follow-up");
                assert_eq!(next.get(), 2);
                heap.inner.run_synthetic_collection(next, || {}, |_| {})
            }
        });
        finalizing_rx.recv().unwrap();
        heap.request_collection();

        let (entered_tx, entered_rx) = mpsc::channel();
        let entrant = std::thread::spawn({
            let heap = heap.clone();
            move || heap.with_mutator(|_| entered_tx.send(()).unwrap())
        });
        heap.inner.wait_for_blocked_outer_mutators(1);
        assert!(entered_rx.try_recv().is_err());

        release_tx.send(()).unwrap();
        assert_eq!(collector.join().unwrap(), None);
        entrant.join().unwrap();
        entered_rx.recv().unwrap();
        let completed = heap.inner.coordinator_snapshot();
        assert_eq!(completed.completed_collection_epoch, 2);
        assert_eq!(completed.phase, AdmissionPhase::Ordinary);
    }

    #[test]
    fn request_after_exclusion_belongs_to_the_followup_epoch() {
        let heap = Heap::new();
        heap.request_collection();
        let first_epoch = {
            let mut state = heap.inner.state.lock().unwrap();
            state.coordinator.elect_collection().unwrap()
        };
        let (exclusive_tx, exclusive_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let collector = std::thread::spawn({
            let heap = heap.clone();
            move || {
                let next = heap.inner.run_synthetic_collection(
                    first_epoch,
                    || {
                        exclusive_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    },
                    |_| {},
                );
                let next = next.expect("exclusive request must schedule a follow-up");
                heap.inner.run_synthetic_collection(next, || {}, |_| {})
            }
        });
        exclusive_rx.recv().unwrap();
        heap.request_collection();
        release_tx.send(()).unwrap();

        assert_eq!(collector.join().unwrap(), None);
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            2
        );
    }

    #[test]
    fn panicking_finalizer_retires_its_mutator_and_relatches_collection() {
        let heap = Heap::new();
        heap.request_collection();
        let epoch = {
            let mut state = heap.inner.state.lock().unwrap();
            state.coordinator.elect_collection().unwrap()
        };

        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.inner.run_synthetic_collection(
                epoch,
                || {},
                |_| panic!("injected finalizer panic"),
            );
        }));
        assert!(panic.is_err());

        let restored = heap.inner.coordinator_snapshot();
        assert_eq!(restored.phase, AdmissionPhase::Ordinary);
        assert_eq!(restored.active_outer_mutators, 0);
        assert_eq!(restored.active_collection, None);
        assert!(restored.collection_requested);
        assert_eq!(cache_snapshot(&heap.inner).unwrap().recursive_depth, 0);

        heap.inner.service_requested_collections();
        assert_eq!(
            heap.inner.coordinator_snapshot().completed_collection_epoch,
            1
        );
    }

    #[test]
    fn heaps_share_type_metadata_but_not_class_provenance() {
        let first = Heap::new();
        let second = Heap::new();
        let first_class = first
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();
        let second_class = second
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();

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
        assert!(matches!(
            heap.with_mutator(|mutator| mutator.allocation_class::<()>()),
            Err(UnsupportedLayout::ZeroSized)
        ));
        assert!(matches!(
            heap.with_mutator(|mutator| mutator.allocation_class::<OverflowingSlot>()),
            Err(UnsupportedLayout::ArithmeticOverflow)
        ));

        let first_valid = heap
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();
        assert_eq!(first_valid.id().get(), 1);
        assert_eq!(heap.inner.state.lock().unwrap().classes.len(), 1);
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
        assert!(heap.inner.state.lock().unwrap().classes.is_empty());

        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<SecondType>())
            .unwrap();
        assert_eq!(class.id().get(), 1);
    }

    #[test]
    fn typed_run_headers_resolve_to_exact_class_metadata() {
        let heap = Heap::new();
        let first_class = heap
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();
        let dropping_class = heap
            .with_mutator(|mutator| mutator.allocation_class::<DroppingType>())
            .unwrap();
        let first_run = heap.inner.prepare_run(&first_class).unwrap();
        let dropping_run = heap.inner.prepare_run(&dropping_class).unwrap();

        let (first_address, dropping_address) = {
            let state = heap.inner.state.lock().unwrap();
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
    fn heap_enumerates_every_published_run_from_authoritative_state() {
        let heap = Heap::new();
        let first_class = heap
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();
        let second_class = heap
            .with_mutator(|mutator| mutator.allocation_class::<SecondType>())
            .unwrap();
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
                    == heap.inner.state.lock().unwrap().classes[class_index(run.class_id).unwrap()]
                        .geometry()
        }));
    }

    #[test]
    fn concurrent_run_publication_keeps_one_enumerable_class_pool() {
        const THREADS: usize = 8;

        let heap = Heap::new();
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();
        let runs = (0..THREADS)
            .map(|_| {
                let heap = heap.clone();
                let class = class.clone();
                std::thread::spawn(move || heap.inner.prepare_run(&class).unwrap())
            })
            .map(|thread| thread.join().expect("run-publication worker panicked"))
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(runs.len(), THREADS);
        let resolved = heap.inner.resolved_runs();
        assert_eq!(resolved.len(), THREADS);
        assert!(
            resolved
                .iter()
                .all(|run| run.class_id == class.id() && runs.contains(&run.location))
        );
    }

    #[test]
    fn foreign_class_leaves_both_heaps_run_state_unchanged() {
        let owner = Heap::new();
        let observer = Heap::new();
        let class = owner
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();

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
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<WideSlot>())
            .unwrap();
        #[cfg(miri)]
        let last_index = crate::arena::RUNS_PER_CHUNK;
        #[cfg(not(miri))]
        let last_index = 2 * crate::arena::RUNS_PER_CHUNK;
        let values = heap.with_mutator(|mutator| {
            (0..=last_index)
                .map(|value| {
                    mutator.alloc(
                        &class,
                        WideSlot {
                            value: value as u64,
                        },
                    )
                })
                .collect::<Vec<_>>()
        });

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
            let state = heap.inner.state.lock().unwrap();
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
    }

    #[test]
    fn synchronized_allocation_rejects_a_foreign_class_before_state_changes() {
        let owner = Heap::new();
        let observer = Heap::new();
        let class = owner
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();

        let panic = catch_unwind(AssertUnwindSafe(|| {
            observer.with_mutator(|mutator| {
                let _ = mutator.alloc(&class, FirstType { _value: 1 });
            });
        }));
        assert!(panic.is_err());
        assert!(owner.inner.resolved_runs().is_empty());
        assert!(observer.inner.resolved_runs().is_empty());
    }

    #[test]
    fn unwind_after_slot_selection_does_not_publish_an_allocation() {
        let heap = Heap::new();
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<DropCounter>())
            .unwrap();
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

        let state = match heap.inner.state.lock() {
            Ok(_) => panic!("injected unwind should poison the heap-state mutex"),
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
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<DropCounter>())
            .unwrap();
        heap.with_mutator(|mutator| {
            for _ in 0..ALLOCATIONS {
                let _ = mutator.alloc(&class, DropCounter(Arc::clone(&drops)));
            }
        });

        drop(heap);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(class);
        assert_eq!(drops.load(Ordering::Relaxed), ALLOCATIONS);
    }

    #[test]
    fn cached_allocation_reuses_one_word_without_shared_slow_path() {
        let heap = Heap::new();
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<u64>())
            .unwrap();

        let first = heap.with_mutator(|mutator| {
            (0..32_u64)
                .map(|value| mutator.alloc(&class, value))
                .collect::<Vec<_>>()
        });
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);
        assert_eq!(heap.inner.allocation_cursor_slow_path_count(), 1);

        let second = heap.with_mutator(|mutator| {
            (32..64_u64)
                .map(|value| mutator.alloc(&class, value))
                .collect::<Vec<_>>()
        });
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressure {
                published_runs: 1,
                collection_requested: false,
            }
        );

        let next = heap.with_mutator(|mutator| mutator.alloc(&class, 64_u64));
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 2);
        assert_eq!(heap.inner.allocation_cursor_slow_path_count(), 1);
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressure {
                published_runs: 1,
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
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<u64>())
            .unwrap();
        let barrier = Arc::new(Barrier::new(THREADS));
        let workers = (0..THREADS)
            .map(|thread_index| {
                let heap = heap.clone();
                let class = class.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    heap.with_mutator(|mutator| {
                        (0..VALUES_PER_THREAD)
                            .map(|offset| {
                                let value = thread_index * VALUES_PER_THREAD + offset;
                                mutator.alloc(&class, value as u64)
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
        assert_eq!(heap.inner.allocation_pressure().published_runs, 1);
        assert_eq!(
            class.shared().frontier(),
            Some(RunLocation { chunk: 0, run: 0 })
        );

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
        class: &crate::AllocationClass<u64>,
        threads: usize,
    ) -> Vec<(RunLocation, usize)> {
        let arrived = Arc::new(Barrier::new(threads + 1));
        let release = Arc::new(Barrier::new(threads + 1));
        heap.inner
            .install_allocation_cursor_slow_path_hook(Arc::clone(&arrived), Arc::clone(&release));

        let workers = (0..threads)
            .map(|_| {
                let heap = heap.clone();
                let class = class.clone();
                std::thread::spawn(move || {
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
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<u64>())
            .unwrap();
        let runs = (0..2)
            .map(|_| heap.inner.prepare_run(&class).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(class.shared().frontier(), Some(runs[0]));
        while class.claim_frontier().is_some() {}

        let claims = force_concurrent_exhausted_frontier_claims(&heap, &class, THREADS);
        let unique = claims.iter().copied().collect::<HashSet<_>>();

        assert_eq!(unique.len(), THREADS);
        assert!(claims.iter().all(|(location, _)| *location == runs[1]));
        assert_eq!(class.shared().frontier(), Some(runs[1]));
        assert_eq!(heap.inner.resolved_runs().len(), 2);
        assert_eq!(heap.inner.allocation_pressure().published_runs, 2);
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
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<u64>())
            .unwrap();
        let first = heap.inner.prepare_run(&class).unwrap();
        assert_eq!(class.shared().frontier(), Some(first));
        while class.claim_frontier().is_some() {}

        let claims = force_concurrent_exhausted_frontier_claims(&heap, &class, THREADS);
        let unique = claims.iter().copied().collect::<HashSet<_>>();
        let successor = RunLocation { chunk: 0, run: 1 };

        assert_eq!(unique.len(), THREADS);
        assert!(claims.iter().all(|(location, _)| *location == successor));
        assert_eq!(class.shared().frontier(), Some(successor));
        assert_eq!(heap.inner.resolved_runs().len(), 2);
        assert_eq!(heap.inner.allocation_pressure().published_runs, 2);
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
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<u64>())
            .unwrap();
        let runs = (0..3)
            .map(|_| heap.inner.prepare_run(&class).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(class.shared().frontier(), Some(runs[0]));

        while class.claim_frontier().is_some() {}
        let cursor = heap.inner.claim_allocation_cursor(&class);

        assert_eq!(cursor.location, runs[1]);
        assert_eq!(class.shared().frontier(), Some(runs[1]));
        assert_eq!(heap.inner.allocation_pressure().published_runs, 3);
    }

    #[test]
    fn evicted_and_thread_exit_cursors_leave_their_words_leased() {
        let heap = Heap::new();
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<u64>())
            .unwrap();
        let first = heap.with_mutator(|mutator| mutator.alloc(&class, 1_u64));
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);

        let colliding = AllocationClassId::new(class.id().get() + 64).unwrap();
        heap.with_mutator(|_| insert_cursor(&heap.inner, test_cursor(colliding)));
        let second = heap.with_mutator(|mutator| mutator.alloc(&class, 2_u64));
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 2);

        let worker_heap = heap.clone();
        let worker_class = class.clone();
        let third = std::thread::spawn(move || {
            worker_heap.with_mutator(|mutator| mutator.alloc(&worker_class, 3_u64))
        })
        .join()
        .expect("allocation worker panicked");
        let fourth = std::thread::spawn({
            let heap = heap.clone();
            let class = class.clone();
            move || heap.with_mutator(|mutator| mutator.alloc(&class, 4_u64))
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
        assert_eq!(heap.inner.allocation_pressure().published_runs, 1);
    }

    #[test]
    fn pressure_request_uses_saturating_typed_run_publications() {
        let mut pressure = AllocationPressure::default();
        for _ in 0..INITIAL_RUN_PUBLICATION_ALLOWANCE - 1 {
            pressure.record_run_publication();
        }
        assert_eq!(
            pressure.published_runs,
            INITIAL_RUN_PUBLICATION_ALLOWANCE - 1
        );
        assert!(!pressure.collection_requested);

        pressure.record_run_publication();
        assert_eq!(pressure.published_runs, INITIAL_RUN_PUBLICATION_ALLOWANCE);
        assert!(pressure.collection_requested);

        pressure.published_runs = usize::MAX;
        pressure.record_run_publication();
        assert_eq!(pressure.published_runs, usize::MAX);
        assert!(pressure.collection_requested);
    }

    #[test]
    fn authoritative_run_publication_records_exactly_one_pressure_event() {
        let heap = Heap::new();
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();

        for _ in 0..INITIAL_RUN_PUBLICATION_ALLOWANCE - 1 {
            heap.inner.prepare_run(&class).unwrap();
        }
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressure {
                published_runs: INITIAL_RUN_PUBLICATION_ALLOWANCE - 1,
                collection_requested: false,
            }
        );

        heap.inner.prepare_run(&class).unwrap();
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressure {
                published_runs: INITIAL_RUN_PUBLICATION_ALLOWANCE,
                collection_requested: true,
            }
        );
        assert_eq!(
            heap.inner.resolved_runs().len(),
            INITIAL_RUN_PUBLICATION_ALLOWANCE
        );
    }

    #[test]
    fn failed_run_publication_exposes_no_frontier_or_pressure() {
        let heap = Heap::new();
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();
        let index = class_index(class.id()).unwrap();
        let mut invalid = heap.inner.state.lock().unwrap().classes[index].geometry();
        invalid.slot_count = 0;

        let error = heap
            .inner
            .state
            .lock()
            .unwrap()
            .publish_run(index, class.id(), invalid)
            .unwrap_err();

        assert!(matches!(
            error,
            RunPublicationError::Initialization(
                crate::arena::RunInitializationError::InvalidGeometry
            )
        ));
        assert_eq!(class.shared().frontier(), None);
        assert!(heap.inner.resolved_runs().is_empty());
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressure::default()
        );
    }

    #[test]
    fn cached_preinitialization_unwind_reuses_the_unpublished_slot() {
        let drops = Arc::new(AtomicUsize::new(0));
        let heap = Heap::new();
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<DropCounter>())
            .unwrap();

        let (first, retried) = heap.with_mutator(|mutator| {
            let first = mutator.alloc(&class, DropCounter(Arc::clone(&drops)));
            let panic = catch_unwind(AssertUnwindSafe(|| {
                let _ = mutator.alloc_with_before_initialize(
                    &class,
                    DropCounter(Arc::clone(&drops)),
                    || panic!("injected cached pre-initialization unwind"),
                );
            }));
            assert!(panic.is_err());
            assert_eq!(drops.load(Ordering::Relaxed), 1);
            assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);

            let retried = mutator.alloc(&class, DropCounter(Arc::clone(&drops)));
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
        drop(class);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn terminal_heap_teardown_waits_for_active_owner_regions() {
        let heap = Heap::new();
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<u64>())
            .unwrap();
        let weak = Arc::downgrade(&heap.inner);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn({
            let heap = heap.clone();
            let class = class.clone();
            move || {
                heap.with_mutator(|mutator| {
                    let value = mutator.alloc(&class, 91_u64);
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
        drop(class);
        assert!(weak.upgrade().is_some());
        release_tx.send(()).unwrap();
        assert_eq!(worker.join().expect("owner-region worker panicked"), 91);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn synchronized_allocator_remains_a_correct_test_reference() {
        let heap = Heap::new();
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();
        let pointer = heap
            .inner
            .allocate_synchronized(&class, FirstType { _value: 73 });
        let resolved = heap.inner.resolve_slot(pointer.as_ptr() as usize).unwrap();
        assert_eq!(resolved.class_id, class.id());
        assert!(std::ptr::eq(resolved.metadata, class.metadata()));
        // SAFETY: the synchronized test allocator returned an initialized
        // `FirstType` pointer which remains live until this heap is dropped.
        assert_eq!(unsafe { pointer.as_ref() }._value, 73);
    }

    #[test]
    fn mutator_entries_track_same_heap_recursion_and_separate_heaps() {
        let first = Heap::new();
        let second = Heap::new();
        let class = first
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();

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
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<FirstType>())
            .unwrap();
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
        let class = heap
            .with_mutator(|mutator| mutator.allocation_class::<u64>())
            .unwrap();
        let first = heap.with_mutator(|mutator| mutator.alloc(&class, 1_u64));
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);
        assert_eq!(heap.inner.allocation_pressure().published_runs, 1);

        assert_eq!(Heap::release_current_thread_caches(), 1);
        let second = heap.with_mutator(|mutator| mutator.alloc(&class, 2_u64));

        assert_eq!(heap.inner.allocation_cursor_claim_count(), 2);
        assert_eq!(heap.inner.allocation_pressure().published_runs, 1);
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
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<crate::AllocationClass<FirstType>>();
    };

    const _: fn() = || {
        fn assert_send<T: Send>() {}
        assert_send::<Arena>();
    };
}
