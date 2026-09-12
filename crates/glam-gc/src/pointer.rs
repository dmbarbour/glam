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

    /// Duplicates this persistent edge under matching mutator authority.
    ///
    /// This is a non-rooting pointer operation. The returned edge must be
    /// installed beneath a traced owner or transferred into a registered root
    /// before its independent liveness proof ends. Debug builds validate heap
    /// ownership and the canonical `T` representation before copying it.
    #[must_use = "a duplicated edge must be installed beneath a traced owner or registered root"]
    #[inline(always)]
    pub fn duplicate_in(&self, mutator: &Mutator<'_>) -> Self {
        mutator.debug_assert_access(self.pointer);

        // SAFETY: this copies an allocator-validated address only after the
        // matching admitted mutator has re-established the available heap and
        // representation proof. It does not extend allocation liveness.
        unsafe { Self::from_raw(self.pointer) }
    }

    /// Returns whether two handles identify the same managed allocation under
    /// matching mutator authority.
    ///
    /// Debug builds validate both handles against the mutator's heap and
    /// canonical `T` representation before comparing their addresses.
    #[must_use]
    #[inline(always)]
    pub fn same_allocation_in(&self, other: &Self, mutator: &Mutator<'_>) -> bool {
        mutator.debug_assert_access(self.pointer);
        mutator.debug_assert_access(other.pointer);
        self.pointer == other.pointer
    }

    /// Transitional unqualified address comparison.
    ///
    /// New code uses [`Gc::same_allocation_in`]. This compatibility operation
    /// remains only until the P4 standard-trait cutover.
    #[must_use]
    pub fn ptr_eq(self, other: Self) -> bool {
        self.pointer == other.pointer
    }

    pub(crate) fn erase(&self) -> ErasedGc {
        ErasedGc::new(self.pointer.cast())
    }

    #[cfg(debug_assertions)]
    pub(crate) fn debug_assert_owned_by(&self, mutator: &Mutator<'_>) {
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

    use crate::{Heap, Trace, Visitor};

    use super::Gc;

    const PENDING_STANDARD_TRAIT_CUTOVER: &[&str] = &[
        "impl<T: Trace> Copy for Gc<T>",
        "impl<T: Trace> Clone for Gc<T>",
        "impl<T: Trace> PartialEq for Gc<T>",
        "impl<T: Trace> Eq for Gc<T>",
        "impl<T: Trace> fmt::Debug for Gc<T>",
    ];

    #[test]
    fn persistent_edge_standard_trait_cutover_is_explicitly_pending() {
        let source = include_str!("pointer.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("pointer module must keep one test boundary")
            .0;

        for pending in PENDING_STANDARD_TRAIT_CUTOVER {
            assert_eq!(
                production.matches(pending).count(),
                1,
                "the transitional `{pending}` surface changed; complete or update the P4 cutover latch"
            );
        }
    }

    #[test]
    fn pointer_identity_is_all_gc_equality_observes() {
        let heap = Heap::new();
        heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let first = allocator.alloc(42_u64);
            let alias = first.duplicate_in(mutator);
            let equal_value = allocator.alloc(42_u64);

            assert!(first.same_allocation_in(&alias, mutator));
            assert!(!first.same_allocation_in(&equal_value, mutator));
        });
    }

    #[test]
    fn pointer_duplication_and_identity_register_no_roots() {
        let heap = Heap::new();
        heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let first = allocator.alloc(42_u64);
            let alias = first.duplicate_in(mutator);
            let equal_value = allocator.alloc(42_u64);

            assert_eq!(heap.root_registrations_for_verification(), 0);
            assert!(first.same_allocation_in(&alias, mutator));
            assert!(!first.same_allocation_in(&equal_value, mutator));
            assert_eq!(heap.root_registrations_for_verification(), 0);
        });
    }

    #[test]
    fn explicit_identity_distinguishes_equal_payload_allocations() {
        let heap = Heap::new();
        heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let first = allocator.alloc(42_u64);
            let alias = first.duplicate_in(mutator);
            let equal_value = allocator.alloc(42_u64);

            assert!(first.same_allocation_in(&alias, mutator));
            assert!(!first.same_allocation_in(&equal_value, mutator));
        });
    }

    #[test]
    fn explicit_duplicate_can_transfer_into_a_registered_root() {
        let heap = Heap::new();
        let (value, root) = heap.with_mutator(|mutator| {
            let value = mutator.allocator::<u64>().unwrap().alloc(42_u64);
            let rooted = mutator.root(value.duplicate_in(mutator));
            (value, rooted)
        });

        assert_eq!(heap.root_registrations_for_verification(), 1);
        let report = heap.collect_full().unwrap();
        assert_eq!(report.root_entries(), 1);
        assert_eq!(report.marked_slots(), 1);
        heap.with_mutator(|mutator| {
            assert!(value.same_allocation_in(&root.as_gc(mutator), mutator));
            assert_eq!(*root.get(mutator), 42);
        });
    }

    struct Holder {
        edge: Gc<u64>,
    }

    // SAFETY: `edge` is the holder's sole managed edge and is reported once.
    unsafe impl Trace for Holder {
        fn trace(&self, visitor: &mut Visitor<'_>) {
            visitor.visit(&self.edge);
        }
    }

    #[test]
    fn explicit_duplicate_can_install_below_a_traced_owner() {
        let heap = Heap::new();
        let (target, owner) = heap.with_mutator(|mutator| {
            let target = mutator.allocator::<u64>().unwrap().alloc(73_u64);
            let owner = mutator.allocator::<Holder>().unwrap().alloc(Holder {
                edge: target.duplicate_in(mutator),
            });
            (target, mutator.root(owner))
        });

        assert_eq!(heap.root_registrations_for_verification(), 1);
        let report = heap.collect_full().unwrap();
        assert_eq!(report.root_entries(), 1);
        assert_eq!(report.marked_slots(), 2);
        heap.with_mutator(|mutator| {
            let installed = &owner.get(mutator).edge;
            assert!(target.same_allocation_in(installed, mutator));
            // SAFETY: the rooted holder traces `installed`, and the matching
            // mutator excludes collection during this read.
            assert_eq!(unsafe { *installed.get_unchecked(mutator) }, 73);
        });
    }

    #[cfg(debug_assertions)]
    #[test]
    fn explicit_operations_reject_a_wrong_heap() {
        let owner = Heap::new();
        let observer = Heap::new();
        let value = owner.with_mutator(|mutator| {
            let value = mutator.allocator::<u64>().unwrap().alloc(42_u64);
            (value.duplicate_in(mutator), mutator.root(value))
        });

        let duplicate_panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = observer.with_mutator(|mutator| value.0.duplicate_in(mutator));
        }));
        assert!(duplicate_panic.is_err());

        let identity_panic = catch_unwind(AssertUnwindSafe(|| {
            observer.with_mutator(|mutator| value.0.same_allocation_in(&value.0, mutator));
        }));
        assert!(identity_panic.is_err());

        // SAFETY: this deliberately violates the typed-pointer construction
        // contract so the explicit operation can prove it checks canonical
        // representation before copying the address.
        let reinterpreted = unsafe { Gc::<u32>::from_raw(value.0.erase().as_ptr().cast()) };
        let representation_panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = owner.with_mutator(|mutator| reinterpreted.duplicate_in(mutator));
        }));
        assert!(representation_panic.is_err());
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
