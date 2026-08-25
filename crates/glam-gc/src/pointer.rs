use std::fmt;
use std::ptr::NonNull;

use crate::{Mutator, Trace, trace::ErasedGc};

/// A typed, non-rooting pointer to one managed allocation.
///
/// `Gc<T>` carries only the pointer. It does not retain or identify its heap,
/// keep its allocation alive, or permit safe dereference. A collection may
/// reclaim an allocation which is not reachable from a registered root or an
/// explicitly retained collector source. Consequently, a copied `Gc<T>` may
/// become stale even while its heap remains live. Every dereference must be
/// justified independently under matching mutator authority.
///
/// A reference cannot escape the mutator region which authorizes access:
///
/// ```compile_fail
/// use glam_gc::Heap;
///
/// let heap = Heap::new();
/// let escaped = heap.with_mutator(|mutator| {
///     let allocator = mutator.allocator::<u64>().unwrap();
///     let value = allocator.alloc(42_u64);
///     // SAFETY: deliberately attempting to return this reference demonstrates
///     // that the API binds it to the mutator borrow.
///     unsafe { value.get_unchecked(mutator) }
/// });
/// println!("{escaped}");
/// ```
///
/// Nor does the pointer implement `Deref`:
///
/// ```compile_fail
/// use glam_gc::Heap;
///
/// let heap = Heap::new();
/// let value = heap.with_mutator(|mutator| {
///     mutator.allocator::<u64>().unwrap().alloc(42_u64)
/// });
/// let _ = *value;
/// ```
///
/// Address identity deliberately does not implement `Hash`, because moving a
/// managed allocation must not silently invalidate hashed containers:
///
/// ```compile_fail
/// use std::collections::HashSet;
/// use glam_gc::Heap;
///
/// let heap = Heap::new();
/// let value = heap.with_mutator(|mutator| {
///     mutator.allocator::<u64>().unwrap().alloc(42_u64)
/// });
/// let _ = HashSet::from([value]);
/// ```
#[must_use = "a managed pointer does not itself keep its allocation alive"]
#[repr(transparent)]
pub struct Gc<T: Trace> {
    pointer: NonNull<T>,
}

const _: () = assert!(std::mem::size_of::<Gc<u64>>() == std::mem::size_of::<*const u64>());

impl<T: Trace> Gc<T> {
    /// Constructs a managed pointer from an allocator-validated address.
    ///
    /// # Safety
    ///
    /// At construction, `pointer` must identify an initialized, live managed
    /// `T` registered to the allocating heap. Constructing this handle does
    /// not extend that liveness; every later dereference must separately prove
    /// that collection has not reclaimed the allocation.
    pub(crate) unsafe fn from_raw(pointer: NonNull<T>) -> Self {
        Self { pointer }
    }

    /// Returns whether two handles identify the same managed allocation.
    #[must_use]
    pub fn ptr_eq(self, other: Self) -> bool {
        self.pointer == other.pointer
    }

    pub(crate) fn erase(self) -> ErasedGc {
        ErasedGc::new(self.pointer.cast())
    }

    pub(crate) fn debug_assert_owned_by(self, mutator: &Mutator<'_>) {
        mutator.debug_assert_access(self.pointer);
    }
}

impl<T: Trace> Gc<T> {
    /// Borrows the managed value under one heap-qualified mutator token.
    ///
    /// # Safety
    ///
    /// This pointer must be live, belong to `mutator`'s heap, and identify an
    /// initialized value whose representation is exactly `T`. No mutation may
    /// invalidate the returned shared reference during its lifetime.
    ///
    /// Debug and test builds verify heap ownership and representation through
    /// indexed arena/run/class metadata before dereferencing. Those checks are
    /// diagnostics, not the release-build safety proof.
    pub unsafe fn get_unchecked<'access>(&self, mutator: &'access Mutator<'_>) -> &'access T {
        mutator.debug_assert_access(self.pointer);

        // SAFETY: the caller proves liveness, heap ownership, representation,
        // and shared-reference validity. The returned lifetime is bounded by
        // the borrow of the mutator token.
        unsafe { self.pointer.as_ref() }
    }
}

impl<T: Trace> Copy for Gc<T> {}

impl<T: Trace> Clone for Gc<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Trace> PartialEq for Gc<T> {
    fn eq(&self, other: &Self) -> bool {
        self.pointer == other.pointer
    }
}

impl<T: Trace> Eq for Gc<T> {}

impl<T: Trace> fmt::Debug for Gc<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Gc").field(&self.pointer).finish()
    }
}

// SAFETY: a `Gc<T>` grants no access without a non-`Send`, heap-qualified
// mutator. Moving a handle between threads is valid when the eventual shared
// access and destruction of `T` are both thread-safe.
unsafe impl<T: Trace> Send for Gc<T> {}

// SAFETY: sharing a `Gc<T>` grants no access without a non-`Sync`,
// heap-qualified mutator. Shared access to `T` is valid under this bound.
unsafe impl<T: Trace> Sync for Gc<T> {}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use crate::Heap;

    use super::Gc;

    #[test]
    fn pointer_identity_is_all_gc_equality_observes() {
        let heap = Heap::new();
        let (first, alias, equal_value) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let first = allocator.alloc(42_u64);
            (first, first, allocator.alloc(42_u64))
        });

        assert_eq!(first, alias);
        assert!(first.ptr_eq(alias));
        assert_ne!(first, equal_value);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn wrong_representation_fails_before_dereference() {
        let heap = Heap::new();
        let value = heap.with_mutator(|mutator| mutator.allocator::<u64>().unwrap().alloc(42_u64));
        let reinterpreted = Gc::<u32> {
            pointer: value.pointer.cast(),
        };

        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.with_mutator(|mutator| {
                // SAFETY: this deliberately violates the representation
                // precondition to verify the debug check before dereference.
                let _ = unsafe { reinterpreted.get_unchecked(mutator) };
            });
        }))
        .expect_err("wrong-representation access should panic in debug builds");

        let message = if let Some(message) = panic.downcast_ref::<String>() {
            message.as_str()
        } else if let Some(message) = panic.downcast_ref::<&str>() {
            message
        } else {
            "non-string panic"
        };
        assert!(message.contains("not requested `u32`"));
    }
}
