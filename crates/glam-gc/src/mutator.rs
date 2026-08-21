use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

use crate::{Gc, heap::HeapInner};

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
    pub fn alloc<T: Send + Sync + 'static>(&self, value: T) -> Gc<T> {
        let slot = Box::leak(Box::new(PrototypeSlot {
            value,
            _unique_address: 0,
        }));
        let pointer = NonNull::from(&mut slot.value);
        self.heap.register_prototype(pointer);

        // SAFETY: the leaked slot provides a live, aligned, initialized `T`;
        // the immediately preceding registration associates it with this heap
        // and records the same representation.
        unsafe { Gc::from_raw(pointer) }
    }

    pub(crate) fn debug_assert_access<T: 'static>(&self, pointer: NonNull<T>) {
        self.heap.debug_assert_access(pointer);
    }
}

/// The extra byte gives even a zero-sized `T` a distinct leaked allocation.
#[repr(C)]
struct PrototypeSlot<T> {
    value: T,
    _unique_address: u8,
}
