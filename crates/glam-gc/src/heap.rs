use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use crate::{
    AllocationClass, Mutator, Trace, UnsupportedLayout,
    arena::{Arena, RunLocation, RunPublicationError},
    class::{AllocationClassEntry, MetadataIdentity, ObjectMetadata, metadata_for},
    run::{AllocationClassId, RunGeometry},
};

/// One shareable, runtime-local managed-value domain.
///
/// C2B's heap owns canonical allocation classes and typed-run topology plus
/// C1A's debug/test prototype allocation registry. Prototype payloads remain
/// deliberately leaked outside those arenas, so the heap still has no managed
/// payload allocator, reclamation policy, or collector coordination state.
#[derive(Clone, Default)]
pub struct Heap {
    inner: Arc<HeapInner>,
}

impl Heap {
    /// Creates an empty prototype heap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `operation` inside a scoped mutator region for this heap.
    ///
    /// C2B still has no collector to exclude. The token is nevertheless
    /// qualified by this heap and is required for every prototype allocation
    /// and access.
    pub fn with_mutator<R>(&self, operation: impl for<'heap> FnOnce(&Mutator<'heap>) -> R) -> R {
        let mutator = Mutator::new(&self.inner);
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

#[derive(Default)]
pub(crate) struct HeapInner {
    state: Mutex<HeapState>,
    #[cfg(debug_assertions)]
    prototype_allocations: Mutex<Vec<PrototypeAllocationRecord>>,
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

    #[allow(
        dead_code,
        reason = "C2B.3 metadata resolution becomes the access proof in C2C"
    )]
    fn resolve_slot(&self, address: usize) -> Option<ResolvedSlot> {
        let state = self.state.lock().ok()?;
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

    pub(crate) fn register_prototype<T: Trace>(&self, pointer: NonNull<T>) {
        let metadata = metadata_for::<T>();

        #[cfg(debug_assertions)]
        self.prototype_allocations
            .lock()
            .expect("prototype allocation registry should not be poisoned")
            .push(PrototypeAllocationRecord {
                address: pointer.as_ptr().cast::<()>() as usize,
                metadata,
            });

        #[cfg(not(debug_assertions))]
        let _ = (pointer, metadata);
    }

    pub(crate) fn debug_assert_access<T: Trace>(&self, pointer: NonNull<T>) {
        #[cfg(debug_assertions)]
        {
            let expected = metadata_for::<T>();
            let address = pointer.as_ptr().cast::<()>() as usize;
            let allocations = self
                .prototype_allocations
                .lock()
                .expect("prototype allocation registry should not be poisoned");
            let record = allocations
                .iter()
                .find(|record| record.address == address)
                .copied();
            drop(allocations);
            let record =
                record.unwrap_or_else(|| panic!("managed pointer does not belong to this heap"));

            assert!(
                std::ptr::eq(record.metadata, expected),
                "managed pointer has representation `{}`, not requested `{}`",
                record.metadata.type_name(),
                expected.type_name()
            );
        }

        #[cfg(not(debug_assertions))]
        let _ = pointer;
    }
}

#[allow(
    dead_code,
    reason = "C2B.3 dense class lookup becomes allocator and collector input"
)]
fn class_index(id: AllocationClassId) -> Option<usize> {
    usize::try_from(id.get().checked_sub(1)?).ok()
}

#[cfg(debug_assertions)]
#[derive(Clone, Copy)]
struct PrototypeAllocationRecord {
    address: usize,
    metadata: &'static ObjectMetadata,
}

#[cfg(test)]
#[expect(unsafe_code, reason = "reviewed C2B allocation-class fixtures")]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use crate::{Trace, UnsupportedLayout, Visitor, arena::Arena, class::metadata_for};

    use super::{Heap, PrepareRunError, class_index};

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

    const _: fn() = || {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<crate::AllocationClass<FirstType>>();
    };

    const _: fn() = || {
        fn assert_send<T: Send>() {}
        assert_send::<Arena>();
    };
}
