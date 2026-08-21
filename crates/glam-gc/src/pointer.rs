use std::fmt;
use std::hash::{Hash, Hasher};
use std::ptr::NonNull;

use crate::Mutator;

/// A typed, non-rooting pointer to one managed allocation.
///
/// `Gc<T>` carries only the pointer. It does not retain or identify its heap,
/// keep its allocation alive, or permit safe dereference. C1A prototype
/// allocations happen to be leaked; later phases replace that temporary
/// liveness rule with roots, mutator regions, and collection invariants.
///
/// A reference cannot escape the mutator region which authorizes access:
///
/// ```compile_fail
/// use glam_gc::Heap;
///
/// let heap = Heap::new();
/// let escaped = heap.with_mutator(|mutator| {
///     let value = mutator.alloc(42_u64);
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
/// let value = heap.with_mutator(|mutator| mutator.alloc(42_u64));
/// let _ = *value;
/// ```
#[must_use = "a managed pointer does not itself keep its allocation alive"]
pub struct Gc<T> {
    pointer: NonNull<T>,
}

impl<T> Gc<T> {
    /// Constructs a managed pointer from an allocator-validated address.
    ///
    /// # Safety
    ///
    /// `pointer` must identify an initialized, live managed `T` registered to
    /// the allocating heap. Its address and representation must remain valid
    /// until the collector's liveness rules permit reclamation.
    pub(crate) unsafe fn from_raw(pointer: NonNull<T>) -> Self {
        Self { pointer }
    }

    /// Returns whether two handles identify the same managed allocation.
    #[must_use]
    pub fn ptr_eq(self, other: Self) -> bool {
        self.pointer == other.pointer
    }
}

impl<T: 'static> Gc<T> {
    /// Borrows the managed value under one heap-qualified mutator token.
    ///
    /// # Safety
    ///
    /// This pointer must be live, belong to `mutator`'s heap, and identify an
    /// initialized value whose representation is exactly `T`. No mutation may
    /// invalidate the returned shared reference during its lifetime.
    ///
    /// Debug and test builds verify heap ownership and representation against
    /// C1A's prototype allocation record before dereferencing. Those checks are
    /// diagnostics, not the release-build safety proof.
    pub unsafe fn get_unchecked<'access>(&self, mutator: &'access Mutator<'_>) -> &'access T {
        mutator.debug_assert_access(self.pointer);

        // SAFETY: the caller proves liveness, heap ownership, representation,
        // and shared-reference validity. The returned lifetime is bounded by
        // the borrow of the mutator token.
        unsafe { self.pointer.as_ref() }
    }
}

impl<T> Copy for Gc<T> {}

impl<T> Clone for Gc<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> PartialEq for Gc<T> {
    fn eq(&self, other: &Self) -> bool {
        self.pointer == other.pointer
    }
}

impl<T> Eq for Gc<T> {}

impl<T> Hash for Gc<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.pointer.hash(state);
    }
}

impl<T> fmt::Debug for Gc<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Gc").field(&self.pointer).finish()
    }
}

// SAFETY: a `Gc<T>` grants no access without a non-`Send`, heap-qualified
// mutator. Moving a handle between threads is valid when the eventual shared
// access and destruction of `T` are both thread-safe.
unsafe impl<T: Send + Sync> Send for Gc<T> {}

// SAFETY: sharing a `Gc<T>` grants no access without a non-`Sync`,
// heap-qualified mutator. Shared access to `T` is valid under this bound.
unsafe impl<T: Send + Sync> Sync for Gc<T> {}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use crate::Heap;

    use super::Gc;

    #[test]
    fn pointer_identity_is_all_gc_equality_observes() {
        let heap = Heap::new();
        let (first, alias, equal_value) = heap.with_mutator(|mutator| {
            let first = mutator.alloc(42_u64);
            (first, first, mutator.alloc(42_u64))
        });

        assert_eq!(first, alias);
        assert!(first.ptr_eq(alias));
        assert_ne!(first, equal_value);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn wrong_representation_fails_before_dereference() {
        let heap = Heap::new();
        let value = heap.with_mutator(|mutator| mutator.alloc(42_u64));
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
