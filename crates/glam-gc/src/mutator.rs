use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

use crate::{Gc, Trace, heap::HeapInner};

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
///             let _ = mutator.alloc(1_u64);
///         });
///     });
/// });
/// ```
pub struct Mutator<'heap> {
    heap: &'heap HeapInner,
    marker: PhantomData<(&'heap HeapInner, Rc<()>)>,
}

impl<'heap> Mutator<'heap> {
    pub(crate) fn new(heap: &'heap HeapInner) -> Self {
        Self {
            heap,
            marker: PhantomData,
        }
    }

    /// Allocates one value through C1A's deliberately leaking prototype path.
    ///
    /// This is not the collector allocator. It exists only to verify pointer,
    /// lifetime, heap-authority, and thread-sharing contracts before C2.
    ///
    /// Zero-sized managed types are unsupported:
    ///
    /// ```compile_fail,E0080
    /// use glam_gc::Heap;
    ///
    /// let heap = Heap::new();
    /// heap.with_mutator(|mutator| {
    ///     let _ = mutator.alloc(());
    /// });
    /// ```
    pub fn alloc<T: Trace>(&self, value: T) -> Gc<T> {
        const {
            assert!(
                std::mem::size_of::<T>() != 0,
                "zero-sized managed types are unsupported"
            );
        }

        let value = Box::leak(Box::new(value));
        let pointer = NonNull::from(value);
        self.heap.register_prototype(pointer);

        // SAFETY: the leaked box provides a live, aligned, initialized `T`; the
        // immediately preceding registration associates it with this heap and
        // records the same representation.
        unsafe { Gc::from_raw(pointer) }
    }

    pub(crate) fn debug_assert_access<T: Trace>(&self, pointer: NonNull<T>) {
        self.heap.debug_assert_access(pointer);
    }
}
