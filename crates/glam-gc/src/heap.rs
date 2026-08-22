use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{
    AllocationClass, Mutator, Trace, UnsupportedLayout,
    arena::{Arena, RunLocation, RunPublicationError},
    class::{AllocationClassEntry, MetadataIdentity, ObjectMetadata, metadata_for},
    run::{AllocationClassId, RunGeometry},
    thread_cache::{AllocationCursor, AllocationLeaseEpoch, ThreadHeapEntry},
};

const INITIAL_COLLECTION_PRESSURE_BYTES: usize = crate::arena::ARENA_CHUNK_SIZE;

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

    /// Runs `operation` inside a scoped mutator region for this heap.
    ///
    /// C2C still has no collector to exclude. The token is nevertheless
    /// qualified by this heap and is required for every allocation and access.
    pub fn with_mutator<R>(&self, operation: impl for<'heap> FnOnce(&Mutator<'heap>) -> R) -> R {
        let thread_entry =
            ThreadHeapEntry::enter(&self.inner, self.inner.current_allocation_lease_epoch());
        let mutator = Mutator::new(&self.inner, thread_entry.cache());
        operation(&mutator)
    }

    /// Discovers or reuses this heap's allocation class for `T`.
    ///
    /// Process-wide metadata is shared by every heap, while the returned dense
    /// class identity and eventual run pool belong only to this heap.
    pub fn allocation_class<T: Trace>(&self) -> Result<AllocationClass<T>, UnsupportedLayout> {
        let metadata = metadata_for::<T>();
        let geometry = RunGeometry::derive(metadata.layout(), metadata.requested_slot_size())
            .map_err(UnsupportedLayout::from_validated_geometry)?;
        Ok(self.inner.discover_class(metadata, geometry))
    }
}

pub(crate) struct HeapInner {
    state: Mutex<HeapState>,
    allocation_lease_epoch: AtomicU64,
    #[cfg(test)]
    allocation_cursor_claims: std::sync::atomic::AtomicUsize,
}

impl Default for HeapInner {
    fn default() -> Self {
        Self {
            state: Mutex::new(HeapState::default()),
            allocation_lease_epoch: AtomicU64::new(AllocationLeaseEpoch::INITIAL.get()),
            #[cfg(test)]
            allocation_cursor_claims: std::sync::atomic::AtomicUsize::new(0),
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
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AllocationPressure {
    claimed_ranges: usize,
    leased_capacity_bytes: usize,
    collection_requested: bool,
}

impl AllocationPressure {
    fn record_claim(&mut self, claimed_slots: usize, slot_stride: usize) {
        let claimed_bytes = claimed_slots.saturating_mul(slot_stride);
        self.claimed_ranges = self.claimed_ranges.saturating_add(1);
        self.leased_capacity_bytes = self.leased_capacity_bytes.saturating_add(claimed_bytes);
        self.collection_requested |=
            self.leased_capacity_bytes >= INITIAL_COLLECTION_PRESSURE_BYTES;
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

    fn discover_class<T: Trace>(
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
        if let Some(id) = self
            .state
            .lock()
            .expect("heap state should not be poisoned")
            .classes_by_metadata
            .get(&identity)
            .copied()
        {
            return AllocationClass::new(Arc::clone(self), metadata, id);
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
            return AllocationClass::new(Arc::clone(self), metadata, id);
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
        let replaced = state.classes_by_metadata.insert(identity, next);
        debug_assert!(replaced.is_none());

        AllocationClass::new(Arc::clone(self), metadata, next)
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

        // Reserve the class-pool entry before publishing arena state. After a
        // successful arena publication, pushing the location is infallible.
        state.classes[index].reserve_run();
        let location = state
            .arena
            .publish_run(class.id(), geometry)
            .map_err(PrepareRunError::Publication)?;
        state.classes[index].publish_run(location);
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

        let mut state = self
            .state
            .lock()
            .expect("heap state should not be poisoned");
        let index = class_index(class.id()).expect("allocation class has an invalid ID");
        let HeapState {
            arena,
            classes,
            allocation_pressure,
            ..
        } = &mut *state;
        let entry = classes
            .get_mut(index)
            .expect("allocation class is absent from its heap");
        assert!(
            std::ptr::eq(entry.metadata(), class.metadata()),
            "allocation class metadata does not match its heap entry"
        );
        let geometry = entry.geometry();

        let claimed = entry
            .runs()
            .iter()
            .find_map(|&location| arena.claim_allocation_word(location))
            .unwrap_or_else(|| {
                entry.reserve_run();
                let location = arena
                    .publish_run(class.id(), geometry)
                    .unwrap_or_else(|error| panic!("managed run allocation failed: {error:?}"));
                entry.publish_run(location);
                arena
                    .claim_allocation_word(location)
                    .expect("fresh typed run must provide one allocation word")
            });
        assert_eq!(
            claimed.geometry, geometry,
            "claimed allocation range has the wrong geometry"
        );
        allocation_pressure.record_claim(
            claimed.free_mask.count_ones() as usize,
            claimed.geometry.slot_stride,
        );

        AllocationCursor {
            class_id: class.id(),
            location: claimed.location,
            run: claimed.run,
            geometry: claimed.geometry,
            word_index: claimed.word_index,
            free_mask: claimed.free_mask,
        }
    }

    #[cfg(test)]
    fn allocation_cursor_claim_count(&self) -> usize {
        self.allocation_cursor_claims.load(Ordering::Relaxed)
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

        let selected = entry.runs().iter().find_map(|&location| {
            state
                .arena
                .first_free_slot(location)
                .map(|slot_index| (location, slot_index))
        });
        let (location, slot_index) = if let Some(selected) = selected {
            selected
        } else {
            // Reserve the class-pool entry before publishing arena state. The
            // eventual push is then infallible under this same heap-state lock.
            state.classes[index].reserve_run();
            let location = state
                .arena
                .publish_run(class.id(), geometry)
                .unwrap_or_else(|error| panic!("managed run allocation failed: {error:?}"));
            state.classes[index].publish_run(location);
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
                if entry.geometry() != run.geometry || !entry.runs().contains(&run.location) {
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

fn resolve_slot_in_state(state: &HeapState, address: usize) -> Option<ResolvedSlot> {
    let owner = state.arena.checked_slot_owner(address)?;
    let entry = state.classes.get(class_index(owner.class_id)?)?;
    if entry.geometry() != owner.geometry || !entry.runs().contains(&owner.location) {
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
            for &location in entry.runs() {
                for pointer in state.arena.allocated_slot_pointers(location) {
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
        AllocationPressure, Heap, INITIAL_COLLECTION_PRESSURE_BYTES, PrepareRunError, RunLocation,
        class_index,
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
        let expected = heap.allocation_class::<FirstType>().unwrap();
        let handles = (0..THREADS)
            .map(|_| {
                let heap = heap.clone();
                std::thread::spawn(move || heap.allocation_class::<FirstType>().unwrap())
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
    fn heaps_share_type_metadata_but_not_class_provenance() {
        let first = Heap::new();
        let second = Heap::new();
        let first_class = first.allocation_class::<FirstType>().unwrap();
        let second_class = second.allocation_class::<FirstType>().unwrap();

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
            heap.allocation_class::<()>(),
            Err(UnsupportedLayout::ZeroSized)
        ));
        assert!(matches!(
            heap.allocation_class::<OverflowingSlot>(),
            Err(UnsupportedLayout::ArithmeticOverflow)
        ));

        let first_valid = heap.allocation_class::<FirstType>().unwrap();
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
            let _ = heap
                .inner
                .discover_class_with::<SecondType>(metadata, geometry, || {
                    panic!("injected class construction panic")
                });
        }));
        assert!(panic.is_err());
        assert!(heap.inner.state.lock().unwrap().classes.is_empty());

        let class = heap.allocation_class::<SecondType>().unwrap();
        assert_eq!(class.id().get(), 1);
    }

    #[test]
    fn typed_run_headers_resolve_to_exact_class_metadata() {
        let heap = Heap::new();
        let first_class = heap.allocation_class::<FirstType>().unwrap();
        let dropping_class = heap.allocation_class::<DroppingType>().unwrap();
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
        let first_class = heap.allocation_class::<FirstType>().unwrap();
        let second_class = heap.allocation_class::<SecondType>().unwrap();
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
        let class = heap.allocation_class::<FirstType>().unwrap();
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
        let class = owner.allocation_class::<FirstType>().unwrap();

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
        let class = heap.allocation_class::<WideSlot>().unwrap();
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
        let class = owner.allocation_class::<FirstType>().unwrap();

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
        let class = heap.allocation_class::<DropCounter>().unwrap();
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
        assert!(state.arena.allocated_slot_pointers(runs[0]).is_empty());
    }

    #[test]
    fn terminal_heap_teardown_drops_each_allocated_payload_exactly_once() {
        const ALLOCATIONS: usize = 130;

        let drops = Arc::new(AtomicUsize::new(0));
        let heap = Heap::new();
        let class = heap.allocation_class::<DropCounter>().unwrap();
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
        let class = heap.allocation_class::<u64>().unwrap();

        let first = heap.with_mutator(|mutator| {
            (0..32_u64)
                .map(|value| mutator.alloc(&class, value))
                .collect::<Vec<_>>()
        });
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);

        let second = heap.with_mutator(|mutator| {
            (32..64_u64)
                .map(|value| mutator.alloc(&class, value))
                .collect::<Vec<_>>()
        });
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 1);
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressure {
                claimed_ranges: 1,
                leased_capacity_bytes: 64 * std::mem::size_of::<u64>(),
                collection_requested: false,
            }
        );

        let next = heap.with_mutator(|mutator| mutator.alloc(&class, 64_u64));
        assert_eq!(heap.inner.allocation_cursor_claim_count(), 2);
        assert_eq!(
            heap.inner.allocation_pressure(),
            AllocationPressure {
                claimed_ranges: 2,
                leased_capacity_bytes: 128 * std::mem::size_of::<u64>(),
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
        let class = heap.allocation_class::<u64>().unwrap();
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

    #[test]
    fn evicted_and_thread_exit_cursors_leave_their_words_leased() {
        let heap = Heap::new();
        let class = heap.allocation_class::<u64>().unwrap();
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
        assert_eq!(heap.inner.allocation_pressure().claimed_ranges, 4);
        assert_eq!(
            heap.inner.allocation_pressure().leased_capacity_bytes,
            4 * 64 * std::mem::size_of::<u64>()
        );
    }

    #[test]
    fn pressure_request_uses_batched_leased_capacity() {
        let mut pressure = AllocationPressure::default();
        pressure.record_claim(63, 8);
        assert_eq!(pressure.claimed_ranges, 1);
        assert_eq!(pressure.leased_capacity_bytes, 504);
        assert!(!pressure.collection_requested);

        pressure.record_claim(1, INITIAL_COLLECTION_PRESSURE_BYTES);
        assert_eq!(pressure.claimed_ranges, 2);
        assert!(pressure.collection_requested);

        pressure.record_claim(usize::MAX, usize::MAX);
        assert_eq!(pressure.claimed_ranges, 3);
        assert_eq!(pressure.leased_capacity_bytes, usize::MAX);
        assert!(pressure.collection_requested);
    }

    #[test]
    fn cached_preinitialization_unwind_reuses_the_unpublished_slot() {
        let drops = Arc::new(AtomicUsize::new(0));
        let heap = Heap::new();
        let class = heap.allocation_class::<DropCounter>().unwrap();

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
        let class = heap.allocation_class::<u64>().unwrap();
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
        let class = heap.allocation_class::<FirstType>().unwrap();
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
        let class = first.allocation_class::<FirstType>().unwrap();

        first.with_mutator(|_| {
            assert_eq!(cache_snapshot(&first.inner).unwrap().recursive_depth, 1);
            insert_cursor(&first.inner, test_cursor(class.id()));

            first.with_mutator(|_| {
                let snapshot = cache_snapshot(&first.inner).unwrap();
                assert_eq!(snapshot.recursive_depth, 2);
                assert_eq!(snapshot.cursor_count, 1);
            });
            assert_eq!(cache_snapshot(&first.inner).unwrap().recursive_depth, 1);

            second.with_mutator(|_| {
                assert_eq!(cache_snapshot(&first.inner).unwrap().recursive_depth, 1);
                assert_eq!(cache_snapshot(&second.inner).unwrap().recursive_depth, 1);
            });
            assert_eq!(cache_snapshot(&second.inner).unwrap().recursive_depth, 0);
        });

        let retained = cache_snapshot(&first.inner).unwrap();
        assert_eq!(retained.recursive_depth, 0);
        assert_eq!(retained.cursor_count, 1);
    }

    #[test]
    fn outer_entry_invalidates_the_whole_cache_after_epoch_change() {
        let heap = Heap::new();
        let class = heap.allocation_class::<FirstType>().unwrap();
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
    fn dead_heap_tls_identity_is_weak_and_pruned_on_later_entry() {
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
        assert!(!registry_contains(address));
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
