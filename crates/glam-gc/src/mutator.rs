use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::thread_cache::ThreadCacheHandle;
use crate::{AllocationClass, Gc, Trace, heap::HeapInner};

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
/// let class = heap.allocation_class::<u64>().unwrap();
/// heap.with_mutator(|mutator| {
///     std::thread::scope(|scope| {
///         scope.spawn(move || {
///             let _ = mutator.alloc(&class, 1_u64);
///         });
///     });
/// });
/// ```
pub struct Mutator<'heap> {
    heap: &'heap HeapInner,
    cache: ThreadCacheHandle,
    marker: PhantomData<&'heap HeapInner>,
}

impl<'heap> Mutator<'heap> {
    pub(crate) fn new(heap: &'heap HeapInner, cache: ThreadCacheHandle) -> Self {
        Self {
            heap,
            cache,
            marker: PhantomData,
        }
    }

    /// Allocates one value through its reusable heap-local allocation class.
    ///
    /// Zero-sized managed types are unsupported:
    ///
    /// ```compile_fail,E0080
    /// use glam_gc::Heap;
    ///
    /// let heap = Heap::new();
    /// let class = heap.allocation_class::<()>().unwrap();
    /// heap.with_mutator(|mutator| {
    ///     let _ = mutator.alloc(&class, ());
    /// });
    /// ```
    pub fn alloc<T: Trace>(&self, class: &AllocationClass<T>, value: T) -> Gc<T> {
        const {
            assert!(
                std::mem::size_of::<T>() != 0,
                "zero-sized managed types are unsupported"
            );
        }

        assert!(
            class.belongs_to(self.heap),
            "allocation class does not belong to this heap"
        );
        let value = match self.cache.try_allocate(class.id(), value) {
            Ok(pointer) => {
                // SAFETY: the worker-local allocator initialized `T` in its
                // exclusively leased range and published the allocation bit.
                return unsafe { Gc::from_raw(pointer) };
            }
            Err(value) => value,
        };

        let cursor = self.heap.claim_allocation_cursor(class);
        self.cache.install(cursor);
        let pointer = self
            .cache
            .try_allocate(class.id(), value)
            .unwrap_or_else(|_| panic!("fresh allocation cursor contains no free slot"));

        // SAFETY: the worker-local allocator initialized `T` in its exclusively
        // leased range and published the allocation bit.
        unsafe { Gc::from_raw(pointer) }
    }

    #[cfg(test)]
    pub(crate) fn alloc_with_before_initialize<T: Trace>(
        &self,
        class: &AllocationClass<T>,
        value: T,
        before_initialize: impl FnOnce(),
    ) -> Gc<T> {
        assert!(
            class.belongs_to(self.heap),
            "allocation class does not belong to this heap"
        );
        let pointer = self
            .cache
            .try_allocate_with(class.id(), value, before_initialize)
            .unwrap_or_else(|_| panic!("test allocation requires a retained cursor"));
        // SAFETY: the retained cursor returns only after initialization and
        // allocation-bit publication, as in the public allocation path.
        unsafe { Gc::from_raw(pointer) }
    }

    pub(crate) fn debug_assert_access<T: Trace>(&self, pointer: NonNull<T>) {
        self.heap.debug_assert_access(pointer);
    }
}
