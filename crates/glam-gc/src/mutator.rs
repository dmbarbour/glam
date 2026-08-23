use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Arc;

use crate::thread_cache::ThreadCacheHandle;
use crate::{Gc, Root, Trace, UnsupportedLayout, heap::HeapInner};
use crate::{class::AllocationClass, class::metadata_for, run::RunGeometry};

/// Scoped authority to access one [`crate::Heap`].
///
/// It is intentionally neither `Send` nor `Sync`. Every operation remains
/// qualified by the heap which created the token; holding another heap's token
/// grants no authority over this heap's allocations.
///
/// A mutator cannot be shared with another thread, even through a scoped
/// thread:
///
/// ```compile_fail
/// use glam_gc::Heap;
///
/// let heap = Heap::new();
/// heap.with_mutator(|mutator| {
///     std::thread::scope(|scope| {
///         scope.spawn(move || {
///             let _ = mutator.allocator::<u64>();
///         });
///     });
/// });
/// ```
pub struct Mutator<'heap> {
    heap: &'heap Arc<HeapInner>,
    cache: ThreadCacheHandle,
    marker: PhantomData<&'heap HeapInner>,
}

impl<'heap> Mutator<'heap> {
    pub(crate) fn new(heap: &'heap Arc<HeapInner>, cache: ThreadCacheHandle) -> Self {
        Self {
            heap,
            cache,
            marker: PhantomData,
        }
    }

    /// Discovers or reuses this heap's allocation class for `T`.
    ///
    /// Process-wide metadata is shared by every heap, while the returned dense
    /// class identity and eventual run pool belong only to this mutator's heap.
    /// Discovery requires mutator admission because it may extend heap-local
    /// class topology. The returned allocator borrows this mutator and cannot
    /// escape its admitted region:
    ///
    /// ```compile_fail
    /// use glam_gc::Heap;
    ///
    /// let heap = Heap::new();
    /// let _allocator = heap.with_mutator(|mutator| {
    ///     mutator.allocator::<u64>().unwrap()
    /// });
    /// ```
    pub fn allocator<T: Trace>(&self) -> Result<Allocator<'_, T>, UnsupportedLayout> {
        let metadata = metadata_for::<T>();
        let geometry = RunGeometry::derive(metadata.layout(), metadata.requested_slot_size())
            .map_err(UnsupportedLayout::from_validated_geometry)?;
        let class = self.heap.discover_class(metadata, geometry);
        Ok(Allocator {
            heap: self.heap.as_ref(),
            cache: &self.cache,
            class,
        })
    }

    /// Constructs and publishes an external root for one managed allocation.
    ///
    /// The value must belong to this mutator's heap and have the canonical
    /// representation for `T`. Publication into the heap's weak root registry
    /// completes before this method returns.
    pub fn root<T: Trace>(&self, value: Gc<T>) -> Root<T> {
        self.heap.register_root(value)
    }

    pub(crate) fn debug_assert_access<T: Trace>(&self, pointer: NonNull<T>) {
        self.heap.debug_assert_access(pointer);
    }

    pub(crate) fn heap(&self) -> &Arc<HeapInner> {
        self.heap
    }
}

/// Typed allocation authority borrowed from one admitted mutator region.
///
/// The heap owns the durable allocation class and run topology. This value
/// retains neither the heap nor any allocation, and safe Rust cannot use it
/// after the mutator which created it leaves scope.
///
/// It also cannot cross a thread boundary, including through a scoped thread:
///
/// ```compile_fail
/// use glam_gc::Heap;
///
/// let heap = Heap::new();
/// heap.with_mutator(|mutator| {
///     let allocator = mutator.allocator::<u64>().unwrap();
///     std::thread::scope(|scope| {
///         scope.spawn(move || {
///             let _ = allocator.alloc(42);
///         });
///     });
/// });
/// ```
#[must_use = "a scoped allocator is required for managed allocation"]
pub struct Allocator<'mutator, T: Trace> {
    heap: &'mutator HeapInner,
    cache: &'mutator ThreadCacheHandle,
    class: AllocationClass<T>,
}

impl<T: Trace> Allocator<'_, T> {
    /// Allocates one value through this scoped heap-local allocation class.
    ///
    /// Zero-sized managed types are unsupported:
    ///
    /// ```compile_fail,E0080
    /// use glam_gc::Heap;
    ///
    /// let heap = Heap::new();
    /// heap.with_mutator(|mutator| {
    ///     let allocator = mutator.allocator::<()>().unwrap();
    ///     let _ = allocator.alloc(());
    /// });
    /// ```
    pub fn alloc(&self, value: T) -> Gc<T> {
        const {
            assert!(
                std::mem::size_of::<T>() != 0,
                "zero-sized managed types are unsupported"
            );
        }

        debug_assert!(
            self.class.belongs_to(self.heap),
            "scoped allocator class does not belong to its heap"
        );
        let value = match self.cache.try_allocate(self.class.id(), value) {
            Ok(pointer) => {
                // SAFETY: the worker-local allocator initialized `T` in its
                // exclusively leased range and published the allocation bit.
                return unsafe { Gc::from_raw(pointer) };
            }
            Err(value) => value,
        };

        let cursor = self.heap.claim_allocation_cursor(&self.class);
        self.cache.install(cursor);
        let pointer = self
            .cache
            .try_allocate(self.class.id(), value)
            .unwrap_or_else(|_| panic!("fresh allocation cursor contains no free slot"));

        // SAFETY: the worker-local allocator initialized `T` in its exclusively
        // leased range and published the allocation bit.
        unsafe { Gc::from_raw(pointer) }
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> crate::run::AllocationClassId {
        self.class.id()
    }

    #[cfg(test)]
    pub(crate) fn metadata(&self) -> &'static crate::class::ObjectMetadata {
        self.class.metadata()
    }

    #[cfg(test)]
    pub(crate) fn belongs_to(&self, heap: &HeapInner) -> bool {
        self.class.belongs_to(heap)
    }

    #[cfg(test)]
    pub(crate) fn alloc_with_before_initialize(
        &self,
        value: T,
        before_initialize: impl FnOnce(),
    ) -> Gc<T> {
        debug_assert!(self.class.belongs_to(self.heap));
        let pointer = self
            .cache
            .try_allocate_with(self.class.id(), value, before_initialize)
            .unwrap_or_else(|_| panic!("test allocation requires a retained cursor"));
        // SAFETY: the retained cursor returns only after initialization and
        // allocation-bit publication, as in the public allocation path.
        unsafe { Gc::from_raw(pointer) }
    }
}

impl<T: Trace> fmt::Debug for Allocator<'_, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Allocator")
            .field("class", &self.class)
            .finish_non_exhaustive()
    }
}
