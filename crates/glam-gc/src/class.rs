use std::alloc::Layout;
use std::any::{TypeId, type_name};
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, OnceLock};

use crate::{
    Trace, Visitor,
    arena::RunLocation,
    heap::HeapInner,
    run::{AllocationClassId, GeometryError, RunGeometry},
};

type ErasedTrace = for<'visit> unsafe fn(NonNull<()>, &mut Visitor<'visit>);
type ErasedDrop = unsafe fn(NonNull<()>);

/// Canonical process-wide description of one managed Rust representation.
///
/// The winning static address, rather than `TypeId`, is the operational type
/// identity after cold discovery. Heap-local allocation classes later derive
/// fixed-run geometry from this immutable descriptor.
pub(crate) struct ObjectMetadata {
    type_id: TypeId,
    type_name: &'static str,
    layout: Layout,
    requested_slot_size: Option<usize>,
    trace: ErasedTrace,
    drop: Option<ErasedDrop>,
}

impl ObjectMetadata {
    fn for_type<T: Trace>() -> Self {
        const {
            if let Some(requested) = T::REQUESTED_SLOT_SIZE {
                assert!(
                    requested >= std::mem::size_of::<T>(),
                    "Trace::REQUESTED_SLOT_SIZE is a total slot extent and must be at least size_of::<Self>()"
                );
            }
        }

        Self {
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>(),
            layout: Layout::new::<T>(),
            requested_slot_size: T::REQUESTED_SLOT_SIZE,
            trace: trace_erased::<T>,
            drop: std::mem::needs_drop::<T>().then_some(drop_erased::<T> as ErasedDrop),
        }
    }

    pub(crate) fn type_name(&self) -> &'static str {
        self.type_name
    }

    pub(crate) fn layout(&self) -> Layout {
        self.layout
    }

    pub(crate) fn requested_slot_size(&self) -> Option<usize> {
        self.requested_slot_size
    }

    pub(crate) fn needs_drop(&self) -> bool {
        self.drop.is_some()
    }

    pub(crate) unsafe fn trace(&self, pointer: NonNull<()>, visitor: &mut Visitor<'_>) {
        // SAFETY: the caller proves that `pointer` identifies one live,
        // initialized allocation with exactly this canonical metadata.
        unsafe { (self.trace)(pointer, visitor) };
    }

    pub(crate) unsafe fn drop_in_place(&self, pointer: NonNull<()>) {
        let Some(drop) = self.drop else {
            return;
        };
        // SAFETY: the caller proves that `pointer` identifies one live,
        // initialized allocation with exactly this canonical metadata and
        // that its destructor has not already run.
        unsafe { drop(pointer) };
    }
}

#[derive(Clone, Copy)]
pub(crate) struct MetadataIdentity(&'static ObjectMetadata);

impl MetadataIdentity {
    pub(crate) fn new(metadata: &'static ObjectMetadata) -> Self {
        Self(metadata)
    }

    pub(crate) fn metadata(self) -> &'static ObjectMetadata {
        self.0
    }
}

impl PartialEq for MetadataIdentity {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for MetadataIdentity {}

impl Hash for MetadataIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.0, state);
    }
}

/// One reusable typed handle to a heap-local fixed-run allocation class.
///
/// The handle retains its heap domain, but it is not a managed root and does
/// not retain any allocation. C2C consumes it for payload allocation.
#[must_use = "an allocation class is required for managed allocation"]
pub struct AllocationClass<T: Trace> {
    heap: Arc<HeapInner>,
    metadata: &'static ObjectMetadata,
    id: AllocationClassId,
    marker: PhantomData<fn() -> T>,
}

impl<T: Trace> AllocationClass<T> {
    pub(crate) fn new(
        heap: Arc<HeapInner>,
        metadata: &'static ObjectMetadata,
        id: AllocationClassId,
    ) -> Self {
        Self {
            heap,
            metadata,
            id,
            marker: PhantomData,
        }
    }

    pub(crate) fn belongs_to(&self, heap: &HeapInner) -> bool {
        std::ptr::eq(Arc::as_ptr(&self.heap), heap)
    }

    pub(crate) fn metadata(&self) -> &'static ObjectMetadata {
        self.metadata
    }

    pub(crate) fn id(&self) -> AllocationClassId {
        self.id
    }
}

impl<T: Trace> Clone for AllocationClass<T> {
    fn clone(&self) -> Self {
        Self {
            heap: Arc::clone(&self.heap),
            metadata: self.metadata,
            id: self.id,
            marker: PhantomData,
        }
    }
}

impl<T: Trace> fmt::Debug for AllocationClass<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AllocationClass")
            .field("type_name", &self.metadata.type_name())
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// A managed representation which cannot fit the collector's fixed-run
/// geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedLayout {
    ZeroSized,
    ArithmeticOverflow,
    NoSlots,
}

impl fmt::Display for UnsupportedLayout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ZeroSized => "zero-sized managed representations are unsupported",
            Self::ArithmeticOverflow => "managed representation geometry overflows",
            Self::NoSlots => "managed representation does not fit in a collector run",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for UnsupportedLayout {}

impl UnsupportedLayout {
    pub(crate) fn from_validated_geometry(error: GeometryError) -> Self {
        match error {
            GeometryError::ZeroSized => Self::ZeroSized,
            GeometryError::RequestedSlotTooSmall => {
                unreachable!(
                    "canonical object metadata validates its slot request in const evaluation"
                )
            }
            GeometryError::ArithmeticOverflow => Self::ArithmeticOverflow,
            GeometryError::NoSlots => Self::NoSlots,
        }
    }
}

pub(crate) struct AllocationClassEntry {
    metadata: MetadataIdentity,
    geometry: RunGeometry,
    runs: Vec<RunLocation>,
}

impl AllocationClassEntry {
    pub(crate) fn new(metadata: &'static ObjectMetadata, geometry: RunGeometry) -> Self {
        Self {
            metadata: MetadataIdentity::new(metadata),
            geometry,
            runs: Vec::new(),
        }
    }

    pub(crate) fn metadata(&self) -> &'static ObjectMetadata {
        self.metadata.metadata()
    }

    pub(crate) fn geometry(&self) -> RunGeometry {
        self.geometry
    }

    pub(crate) fn runs(&self) -> &[RunLocation] {
        &self.runs
    }

    pub(crate) fn reserve_run(&mut self) {
        self.runs
            .try_reserve(1)
            .expect("allocation-class run pool capacity exhausted");
    }

    pub(crate) fn publish_run(&mut self, run: RunLocation) {
        self.runs.push(run);
    }
}

/// Returns the one process-lifetime descriptor for `T`.
pub(crate) fn metadata_for<T: Trace>() -> &'static ObjectMetadata {
    metadata_for_with::<T>(ObjectMetadata::for_type::<T>)
}

fn metadata_for_with<T: Trace>(
    make_candidate: impl FnOnce() -> ObjectMetadata,
) -> &'static ObjectMetadata {
    let type_id = TypeId::of::<T>();
    let registry = metadata_registry();

    if let Some(metadata) = registry
        .lock()
        .expect("object metadata registry should not be poisoned")
        .get(&type_id)
        .copied()
    {
        return metadata;
    }

    // Candidate construction deliberately happens outside the registry lock.
    // A panic therefore cannot poison the process-wide registry, and losing
    // immutable candidates remain ordinary owned boxes.
    let candidate = Box::new(make_candidate());
    assert_eq!(
        candidate.type_id, type_id,
        "metadata candidate type mismatch"
    );

    let mut registry = registry
        .lock()
        .expect("object metadata registry should not be poisoned");
    if let Some(metadata) = registry.get(&type_id).copied() {
        return metadata;
    }

    let metadata = Box::leak(candidate);
    registry.insert(type_id, metadata);
    metadata
}

fn metadata_registry() -> &'static Mutex<HashMap<TypeId, &'static ObjectMetadata>> {
    static REGISTRY: OnceLock<Mutex<HashMap<TypeId, &'static ObjectMetadata>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

unsafe fn trace_erased<T: Trace>(pointer: NonNull<()>, visitor: &mut Visitor<'_>) {
    // SAFETY: the metadata caller proves that this is a live initialized `T`.
    let value = unsafe { pointer.cast::<T>().as_ref() };
    value.trace(visitor);
}

unsafe fn drop_erased<T: Trace>(pointer: NonNull<()>) {
    // SAFETY: the metadata caller proves that this is a live initialized `T`
    // whose destructor has not already run.
    unsafe { std::ptr::drop_in_place(pointer.cast::<T>().as_ptr()) };
}

#[cfg(test)]
mod tests {
    use std::mem::MaybeUninit;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::{Gc, Heap, Trace, Visitor};

    use super::{ObjectMetadata, metadata_for, metadata_for_with};

    struct Leaf {
        _value: u8,
    }

    // SAFETY: `Leaf` contains no managed edge.
    unsafe impl Trace for Leaf {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct Holder {
        edge: Gc<Leaf>,
    }

    // SAFETY: `edge` is the only managed edge represented by `Holder`.
    unsafe impl Trace for Holder {
        const REQUESTED_SLOT_SIZE: Option<usize> = Some(64);

        fn trace(&self, visitor: &mut Visitor<'_>) {
            self.edge.trace(visitor);
        }
    }

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: `DropProbe` contains no managed edge.
    unsafe impl Trace for DropProbe {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    #[test]
    fn repeated_and_concurrent_discovery_returns_one_metadata_address() {
        const THREADS: usize = 12;

        let expected = metadata_for::<Holder>() as *const ObjectMetadata as usize;
        assert_eq!(
            metadata_for::<Holder>() as *const ObjectMetadata as usize,
            expected
        );

        let addresses = (0..THREADS)
            .map(|_| {
                std::thread::spawn(|| metadata_for::<Holder>() as *const ObjectMetadata as usize)
            })
            .map(|thread| thread.join().expect("metadata worker panicked"))
            .collect::<Vec<_>>();

        assert!(addresses.into_iter().all(|address| address == expected));
        assert_ne!(
            metadata_for::<Leaf>() as *const ObjectMetadata as usize,
            expected
        );
    }

    #[test]
    fn metadata_records_layout_slot_policy_and_drop_mode() {
        let holder = metadata_for::<Holder>();
        assert_eq!(holder.layout(), std::alloc::Layout::new::<Holder>());
        assert_eq!(holder.requested_slot_size(), Some(64));
        assert!(!holder.needs_drop());
        assert_eq!(holder.type_name(), std::any::type_name::<Holder>());

        let dropping = metadata_for::<DropProbe>();
        assert_eq!(dropping.layout(), std::alloc::Layout::new::<DropProbe>());
        assert!(dropping.needs_drop());
    }

    #[test]
    fn erased_trace_dispatch_uses_the_canonical_representation() {
        let heap = Heap::new();
        let leaf_class = heap.allocation_class::<Leaf>().unwrap();
        let holder_class = heap.allocation_class::<Holder>().unwrap();
        heap.with_mutator(|mutator| {
            let edge = mutator.alloc(&leaf_class, Leaf { _value: 1 });
            let holder = mutator.alloc(&holder_class, Holder { edge });
            let mut observed = Vec::new();
            let mut collect = |edge| observed.push(edge);
            let mut visitor = Visitor::new(&mut collect);

            // SAFETY: `holder` is a live initialized arena allocation of
            // `Holder`, and the selected metadata is canonical for `Holder`.
            unsafe {
                metadata_for::<Holder>().trace(holder.erase().as_ptr(), &mut visitor);
            }

            assert_eq!(observed, vec![edge.erase()]);
        });
    }

    #[test]
    fn erased_drop_dispatch_runs_exactly_once_when_requested() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut storage = Box::new(MaybeUninit::new(DropProbe(Arc::clone(&drops))));
        let pointer = std::ptr::NonNull::from(storage.as_mut()).cast::<DropProbe>();

        // SAFETY: `storage` contains one initialized `DropProbe`; its metadata
        // matches, and this is the value's only destructor invocation.
        unsafe { metadata_for::<DropProbe>().drop_in_place(pointer.cast()) };
        assert_eq!(drops.load(Ordering::Relaxed), 1);

        // `MaybeUninit` releases the allocation without dropping the already
        // destroyed payload again.
        drop(storage);
    }

    #[test]
    fn panicking_candidate_construction_publishes_nothing_and_poison_nothing() {
        struct PanickingCandidate;

        // SAFETY: `PanickingCandidate` contains no managed edge.
        unsafe impl Trace for PanickingCandidate {
            fn trace(&self, _visitor: &mut Visitor<'_>) {}
        }

        let panic = catch_unwind(AssertUnwindSafe(|| {
            metadata_for_with::<PanickingCandidate>(|| {
                panic!("injected metadata construction panic")
            });
        }));
        assert!(panic.is_err());

        let metadata = metadata_for::<PanickingCandidate>();
        assert_eq!(
            metadata.type_name(),
            std::any::type_name::<PanickingCandidate>()
        );
    }
}
