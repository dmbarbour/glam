use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::root_pool::RootSlot;
use crate::{Gc, GcRef, MutatorAccess, Trace, heap::HeapInner, trace::ErasedGc};

/// A shareable liveness claim for one managed allocation in a live heap.
///
/// A root is a reference to a slot in its heap's root pool. It does not keep
/// the heap alive, extend the containing value domain's lifetime, or enter
/// that heap on its own; a root may outlive its heap, and then can only be
/// cloned and dropped. Access always requires a live matching
/// [`MutatorAccess`].
#[must_use = "dropping the last root permits its managed value to be collected"]
pub struct Root<T: Trace> {
    slot: NonNull<RootSlot>,
    marker: PhantomData<fn() -> Gc<T>>,
}

const _: () = assert!(std::mem::size_of::<Root<u64>>() == std::mem::size_of::<usize>());

// SAFETY: a root shares its slot only through the slot's atomic count, and the
// slot's word does not change while any root references it. Reaching the
// rooted value requires a non-`Send` mutator for its live heap.
unsafe impl<T: Trace> Send for Root<T> {}

// SAFETY: as above; `&Root` grants no more than cloning and comparing.
unsafe impl<T: Trace> Sync for Root<T> {}

impl<T: Trace> Root<T> {
    /// Roots `value` in a free slot.
    ///
    /// # Safety
    ///
    /// As [`RootSlot::occupy`], and `value` must be a live, allocated `T`.
    pub(crate) unsafe fn occupy(slot: NonNull<RootSlot>, value: ErasedGc) -> Self {
        // SAFETY: the caller's contract.
        unsafe { RootSlot::occupy(slot, value) };
        Self {
            slot,
            marker: PhantomData,
        }
    }

    /// Views the rooted value while `mutator` stays borrowed.
    ///
    /// `mutator` must belong to this root's live heap. The view does not
    /// retain the heap or this root; it borrows the mutator, whose admission
    /// keeps the value live even if the root is dropped meanwhile.
    /// [`GcRef::edge`](crate::GcRef::edge) detaches a non-rooting edge from
    /// it.
    ///
    /// Panics if `mutator` belongs to another heap. A root whose heap has been
    /// dropped cannot acquire such a mutator and therefore cannot be
    /// projected.
    pub fn as_gc<'m>(&self, mutator: &'m MutatorAccess<'_>) -> GcRef<'m, T> {
        assert!(
            self.belongs_to(mutator.heap()),
            "root does not belong to this heap"
        );

        // SAFETY: the root's construction proved its value a live, allocated
        // `T` in this heap, from a view or by validation; the registered slot
        // keeps it live until the view borrows the matching admitted mutator.
        unsafe { GcRef::new(self.value().as_ptr().cast::<T>(), mutator) }
    }

    /// Borrows the rooted value under its live heap's mutator authority.
    ///
    /// Panics if `mutator` belongs to another heap. A root whose heap has been
    /// dropped therefore remains cloneable and droppable but cannot be read.
    #[must_use]
    pub fn get<'access>(&self, mutator: &'access MutatorAccess<'_>) -> &'access T {
        self.as_gc(mutator).get()
    }

    /// Returns whether two handles share the same root slot.
    ///
    /// This compares root-handle identity only. It neither compares the
    /// managed values nor enters or retains their heap.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.slot == other.slot
    }

    pub(crate) fn belongs_to(&self, heap: &HeapInner) -> bool {
        // SAFETY: this root references its slot. Heap identities are never
        // reused, so a dead heap's roots match no live heap.
        unsafe { RootSlot::heap(self.slot) == heap.id() }
    }

    #[cfg(test)]
    pub(crate) fn slot_address_for_test(&self) -> usize {
        self.slot.as_ptr().addr()
    }

    pub(crate) fn value(&self) -> ErasedGc {
        // SAFETY: this root references its slot.
        unsafe { RootSlot::value(self.slot) }
    }
}

impl<T: Trace> Clone for Root<T> {
    fn clone(&self) -> Self {
        // SAFETY: this root references its slot.
        unsafe { RootSlot::retain(self.slot) };
        Self {
            slot: self.slot,
            marker: PhantomData,
        }
    }
}

impl<T: Trace> Drop for Root<T> {
    fn drop(&mut self) {
        // SAFETY: this root references its slot and gives up that reference.
        unsafe { RootSlot::release(self.slot) };
    }
}

impl<T: Trace> fmt::Debug for Root<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Root").field(&self.value()).finish()
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use crate::{Gc, Heap, Trace, Visitor};

    use super::Root;

    #[derive(Debug)]
    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: `DropProbe` contains no managed edges.
    unsafe impl Trace for DropProbe {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    #[test]
    fn root_is_one_word_and_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_eq!(
            std::mem::size_of::<Root<u64>>(),
            std::mem::size_of::<Arc<()>>()
        );
        assert_send_sync::<Root<u64>>();
    }

    #[test]
    fn checked_root_can_be_cloned_and_read_in_later_regions() {
        let heap = Heap::new();
        let (value, root) = heap.mutate(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let value = allocator.alloc(42_u64);
            (value.duplicate_in(mutator), mutator.root_checked(value))
        });
        let alias = root.clone();

        heap.mutate(|mutator| {
            assert!(
                root.as_gc(mutator)
                    .edge()
                    .same_allocation_in(&value, mutator)
            );
            assert!(
                alias
                    .as_gc(mutator)
                    .edge()
                    .same_allocation_in(&value, mutator)
            );
            assert_eq!(*root.get(mutator), 42);
            assert_eq!(*alias.get(mutator), 42);
        });
    }

    #[test]
    fn root_projection_preserves_identity_without_registering_another_root() {
        let heap = Heap::new();
        let (first, first_root, second, second_root) = heap.mutate(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let first = allocator.alloc(11);
            let second = allocator.alloc(11);
            (
                first.duplicate_in(mutator),
                mutator.root_checked(first),
                second.duplicate_in(mutator),
                mutator.root_checked(second),
            )
        });
        assert_eq!(heap.root_registrations_for_verification(), 2);
        let first_alias = first_root.clone();
        assert_eq!(heap.root_registrations_for_verification(), 2);

        heap.mutate(|mutator| {
            let projected = first_root.as_gc(mutator).edge();
            assert!(projected.same_allocation_in(&first, mutator));
            assert!(
                first_alias
                    .as_gc(mutator)
                    .edge()
                    .same_allocation_in(&projected, mutator)
            );
            assert!(
                second_root
                    .as_gc(mutator)
                    .edge()
                    .same_allocation_in(&second, mutator)
            );
            assert!(!projected.same_allocation_in(&second_root.as_gc(mutator).edge(), mutator));
        });
        assert_eq!(heap.root_registrations_for_verification(), 2);

        let report = heap.collect_full().unwrap();
        assert_eq!(report.root_entries(), 2);
        assert_eq!(report.marked_slots(), 2);
    }

    #[test]
    fn viewed_roots_keep_their_values_through_a_collection() {
        let heap = Heap::new();
        let (fresh, projected) = heap.mutate(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let fresh = mutator.root(allocator.alloc_view(5));
            let checked = mutator.root_checked(allocator.alloc(6));
            let projected = mutator.root(checked.as_gc(mutator));
            let _unrooted = allocator.alloc_view(7);
            (fresh, projected)
        });
        assert_eq!(heap.root_registrations_for_verification(), 3);

        let report = heap.collect_full().unwrap();
        assert_eq!(report.root_entries(), 2);
        assert_eq!(report.marked_slots(), 2);
        heap.mutate(|mutator| {
            assert_eq!(*fresh.get(mutator), 5);
            assert_eq!(*projected.get(mutator), 6);
        });
    }

    #[test]
    fn checked_root_can_cross_threads_with_its_live_heap() {
        let heap = Heap::new();
        let root = heap.mutate(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let value = allocator.alloc(73_u64);
            mutator.root_checked(value)
        });
        let worker_heap = heap.clone();

        let observed = std::thread::spawn(move || worker_heap.mutate(|mutator| *root.get(mutator)))
            .join()
            .expect("root worker panicked");

        assert_eq!(observed, 73);
    }

    #[test]
    fn root_validation_observes_a_word_while_its_owner_advances_it() {
        let heap = Heap::new();
        let value = heap.mutate(|mutator| mutator.allocator::<u64>().unwrap().alloc(1_u64));
        let start = Arc::new(std::sync::Barrier::new(2));
        let finish = Arc::new(std::sync::Barrier::new(2));
        let worker = std::thread::spawn({
            let heap = heap.clone();
            let start = Arc::clone(&start);
            let finish = Arc::clone(&finish);
            move || {
                heap.mutate(|mutator| {
                    start.wait();
                    for _ in 0..1_024 {
                        drop(mutator.root_checked(value.duplicate_in(mutator)));
                    }
                    finish.wait();
                });
            }
        });

        heap.mutate(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            start.wait();
            for next in 2..=64 {
                let _ = allocator.alloc(next);
            }
            finish.wait();
        });
        worker.join().expect("root-validation worker panicked");
    }

    #[test]
    fn foreign_heap_root_construction_is_rejected_in_all_builds() {
        let owner = Heap::new();
        let observer = Heap::new();
        let value = owner.mutate(|mutator| mutator.allocator::<u64>().unwrap().alloc(42_u64));

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = observer.mutate(|mutator| mutator.root_checked(value));
        }));

        assert!(panic.is_err());
    }

    #[test]
    fn representation_mismatch_is_rejected_before_root_construction() {
        let heap = Heap::new();
        let value = heap.mutate(|mutator| mutator.allocator::<u64>().unwrap().alloc(42_u64));
        // SAFETY: this deliberately violates the typed-pointer construction
        // contract so the safe root boundary can prove it rejects the
        // representation mismatch before dereference.
        let reinterpreted = unsafe { Gc::<u32>::from_raw(value.erase().as_ptr().cast()) };

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = heap.mutate(|mutator| mutator.root_checked(reinterpreted));
        }));

        assert!(panic.is_err());
    }

    #[test]
    fn root_access_rejects_a_different_heap_in_all_builds() {
        let owner = Heap::new();
        let observer = Heap::new();
        let root = owner.mutate(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let value = allocator.alloc(42_u64);
            mutator.root_checked(value)
        });

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = observer.mutate(|mutator| root.as_gc(mutator).edge());
        }));

        assert!(panic.is_err());
    }

    #[test]
    fn heap_ownership_predicate_accepts_only_the_recorded_live_heap() {
        let owner = Heap::new();
        let owner_alias = owner.clone();
        let observer = Heap::new();
        let root = owner.mutate(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            mutator.root_checked(allocator.alloc(42_u64))
        });

        assert!(owner.owns(&root));
        assert!(owner_alias.owns(&root));
        assert!(!observer.owns(&root));

        let weak = Arc::downgrade(owner.inner_for_test());
        drop(owner);
        drop(owner_alias);

        assert!(weak.upgrade().is_none());
        assert!(!observer.owns(&root));
    }

    #[test]
    fn heap_ownership_predicate_tolerates_concurrent_root_clone_and_drop() {
        const THREADS: usize = 8;
        const ITERATIONS: usize = 1_024;

        let heap = Heap::new();
        let root = heap.mutate(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            mutator.root_checked(allocator.alloc(73_u64))
        });
        let start = Arc::new(Barrier::new(THREADS));
        let workers = (0..THREADS)
            .map(|_| {
                let heap = heap.clone();
                let root = root.clone();
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    for _ in 0..ITERATIONS {
                        let alias = root.clone();
                        assert!(heap.owns(&alias));
                        drop(alias);
                    }
                    assert!(heap.owns(&root));
                })
            })
            .collect::<Vec<_>>();

        for worker in workers {
            worker.join().expect("root ownership worker panicked");
        }
        assert!(heap.owns(&root));
    }

    #[test]
    fn escaped_root_does_not_retain_its_heap_or_payload() {
        let drops = Arc::new(AtomicUsize::new(0));
        let heap = Heap::new();
        let root = heap.mutate(|mutator| {
            let allocator = mutator.allocator::<DropProbe>().unwrap();
            let value = allocator.alloc(DropProbe(Arc::clone(&drops)));
            mutator.root_checked(value)
        });

        let weak = Arc::downgrade(heap.inner_for_test());
        let id = heap.inner_for_test().id();
        drop(heap);

        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(weak.upgrade().is_none());
        // The root's chunk outlives its heap until no root references it.
        assert_eq!(crate::root_pool::orphaned_chunks_of(id), 1);
        drop(root.clone());
        crate::root_pool::sweep_orphaned_chunks_for_test();
        assert_eq!(crate::root_pool::orphaned_chunks_of(id), 1);
        drop(root);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        crate::root_pool::sweep_orphaned_chunks_for_test();
        assert_eq!(crate::root_pool::orphaned_chunks_of(id), 0);
    }
}
