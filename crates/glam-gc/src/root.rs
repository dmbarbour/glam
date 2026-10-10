use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Weak};

use crate::{Gc, Mutator, Trace, heap::HeapInner, trace::ErasedGc};

/// A shareable liveness claim for one managed allocation in a live heap.
///
/// A root keeps its root cell alive but refers only weakly to the heap. It does
/// not extend the containing value domain's lifetime and cannot enter that
/// heap on its own. Access always requires a live matching [`Mutator`].
#[must_use = "dropping the last root permits its managed value to be collected"]
pub struct Root<T: Trace> {
    cell: Arc<RootCell>,
    marker: PhantomData<fn() -> Gc<T>>,
}

const _: () = assert!(std::mem::size_of::<Root<u64>>() == std::mem::size_of::<Arc<()>>());

pub(crate) struct RootCell {
    heap: Weak<HeapInner>,
    value: ErasedGc,
}

impl<T: Trace> Root<T> {
    pub(crate) fn candidate(heap: &Arc<HeapInner>, value: ErasedGc) -> (Self, Weak<RootCell>) {
        let cell = Arc::new(RootCell {
            heap: Arc::downgrade(heap),
            value,
        });
        let registration = Arc::downgrade(&cell);
        (
            Self {
                cell,
                marker: PhantomData,
            },
            registration,
        )
    }

    /// Projects this registered root as its typed, non-rooting managed edge.
    ///
    /// `mutator` must belong to this root's live heap. Its admitted region
    /// prevents collection while the returned edge is intended to be used;
    /// the edge itself does not retain that region, the heap, or this root.
    /// Consequently, retaining the returned `Gc<T>` after all independent
    /// liveness proofs end leaves the same potentially stale pointer as any
    /// other unrooted managed edge.
    ///
    /// Panics if `mutator` belongs to another heap. A root whose heap has been
    /// dropped cannot acquire such a mutator and therefore cannot be
    /// projected.
    pub fn as_gc(&self, mutator: &Mutator<'_>) -> Gc<T> {
        assert!(
            self.belongs_to(mutator.heap()),
            "root does not belong to this heap"
        );

        // SAFETY: the private constructor runs the all-build allocation,
        // canonical-metadata, and heap-provenance validation before sealing
        // this erased pointer behind `Root<T>`. The matching admitted mutator
        // excludes collection while the projection is intended to be used.
        unsafe { Gc::from_raw(self.cell.value.as_ptr().cast::<T>()) }
    }

    /// Borrows the rooted value under its live heap's mutator authority.
    ///
    /// Panics if `mutator` belongs to another heap. A root whose heap has been
    /// dropped therefore remains cloneable and droppable but cannot be read.
    #[must_use]
    pub fn get<'access>(&self, mutator: &'access Mutator<'_>) -> &'access T {
        let value = self.as_gc(mutator);
        // SAFETY: the root invariant above proves liveness and representation,
        // and the release-visible heap identity check proves ownership.
        unsafe { value.get_unchecked(mutator) }
    }

    /// Returns whether two handles share the same registered root cell.
    ///
    /// This compares root-handle identity only. It neither compares the
    /// managed values nor enters or retains their heap.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cell, &other.cell)
    }

    pub(crate) fn belongs_to(&self, heap: &Arc<HeapInner>) -> bool {
        // `Weak` retains its allocation's control block, so a dead heap's
        // address cannot be recycled while this root can still be compared.
        std::ptr::eq(self.cell.heap.as_ptr(), Arc::as_ptr(heap))
    }
}

impl RootCell {
    pub(crate) fn value(&self) -> ErasedGc {
        self.value
    }
}

impl<T: Trace> Clone for Root<T> {
    fn clone(&self) -> Self {
        Self {
            cell: Arc::clone(&self.cell),
            marker: PhantomData,
        }
    }
}

impl<T: Trace> fmt::Debug for Root<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Root")
            .field(&self.cell.value)
            .finish()
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
        let (value, root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let value = allocator.alloc(42_u64);
            (value.duplicate_in(mutator), mutator.root(value))
        });
        let alias = root.clone();

        heap.with_mutator(|mutator| {
            assert!(root.as_gc(mutator).same_allocation_in(&value, mutator));
            assert!(alias.as_gc(mutator).same_allocation_in(&value, mutator));
            assert_eq!(*root.get(mutator), 42);
            assert_eq!(*alias.get(mutator), 42);
        });
    }

    #[test]
    fn root_projection_preserves_identity_without_registering_another_root() {
        let heap = Heap::new();
        let (first, first_root, second, second_root) = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let first = allocator.alloc(11);
            let second = allocator.alloc(11);
            (
                first.duplicate_in(mutator),
                mutator.root(first),
                second.duplicate_in(mutator),
                mutator.root(second),
            )
        });
        assert_eq!(heap.root_registrations_for_verification(), 2);
        let first_alias = first_root.clone();
        assert_eq!(heap.root_registrations_for_verification(), 2);

        heap.with_mutator(|mutator| {
            let projected = first_root.as_gc(mutator);
            assert!(projected.same_allocation_in(&first, mutator));
            assert!(
                first_alias
                    .as_gc(mutator)
                    .same_allocation_in(&projected, mutator)
            );
            assert!(
                second_root
                    .as_gc(mutator)
                    .same_allocation_in(&second, mutator)
            );
            assert!(!projected.same_allocation_in(&second_root.as_gc(mutator), mutator));
        });
        assert_eq!(heap.root_registrations_for_verification(), 2);

        let report = heap.collect_full().unwrap();
        assert_eq!(report.root_entries(), 2);
        assert_eq!(report.marked_slots(), 2);
    }

    #[test]
    fn checked_root_can_cross_threads_with_its_live_heap() {
        let heap = Heap::new();
        let root = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let value = allocator.alloc(73_u64);
            mutator.root(value)
        });
        let worker_heap = heap.clone();

        let observed =
            std::thread::spawn(move || worker_heap.with_mutator(|mutator| *root.get(mutator)))
                .join()
                .expect("root worker panicked");

        assert_eq!(observed, 73);
    }

    #[test]
    fn root_validation_observes_a_word_while_its_owner_advances_it() {
        let heap = Heap::new();
        let value = heap.with_mutator(|mutator| mutator.allocator::<u64>().unwrap().alloc(1_u64));
        let start = Arc::new(std::sync::Barrier::new(2));
        let finish = Arc::new(std::sync::Barrier::new(2));
        let worker = std::thread::spawn({
            let heap = heap.clone();
            let start = Arc::clone(&start);
            let finish = Arc::clone(&finish);
            move || {
                heap.with_mutator(|mutator| {
                    start.wait();
                    for _ in 0..1_024 {
                        drop(mutator.root(value.duplicate_in(mutator)));
                    }
                    finish.wait();
                });
            }
        });

        heap.with_mutator(|mutator| {
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
        let value = owner.with_mutator(|mutator| mutator.allocator::<u64>().unwrap().alloc(42_u64));

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = observer.with_mutator(|mutator| mutator.root(value));
        }));

        assert!(panic.is_err());
    }

    #[test]
    fn representation_mismatch_is_rejected_before_root_construction() {
        let heap = Heap::new();
        let value = heap.with_mutator(|mutator| mutator.allocator::<u64>().unwrap().alloc(42_u64));
        // SAFETY: this deliberately violates the typed-pointer construction
        // contract so the safe root boundary can prove it rejects the
        // representation mismatch before dereference.
        let reinterpreted = unsafe { Gc::<u32>::from_raw(value.erase().as_ptr().cast()) };

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = heap.with_mutator(|mutator| mutator.root(reinterpreted));
        }));

        assert!(panic.is_err());
    }

    #[test]
    fn root_access_rejects_a_different_heap_in_all_builds() {
        let owner = Heap::new();
        let observer = Heap::new();
        let root = owner.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            let value = allocator.alloc(42_u64);
            mutator.root(value)
        });

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = observer.with_mutator(|mutator| root.as_gc(mutator));
        }));

        assert!(panic.is_err());
    }

    #[test]
    fn heap_ownership_predicate_accepts_only_the_recorded_live_heap() {
        let owner = Heap::new();
        let owner_alias = owner.clone();
        let observer = Heap::new();
        let root = owner.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            mutator.root(allocator.alloc(42_u64))
        });

        assert!(owner.owns(&root));
        assert!(owner_alias.owns(&root));
        assert!(!observer.owns(&root));

        drop(owner);
        drop(owner_alias);

        assert!(root.cell.heap.upgrade().is_none());
        assert!(!observer.owns(&root));
    }

    #[test]
    fn heap_ownership_predicate_tolerates_concurrent_root_clone_and_drop() {
        const THREADS: usize = 8;
        const ITERATIONS: usize = 1_024;

        let heap = Heap::new();
        let root = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<u64>().unwrap();
            mutator.root(allocator.alloc(73_u64))
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
        let root = heap.with_mutator(|mutator| {
            let allocator = mutator.allocator::<DropProbe>().unwrap();
            let value = allocator.alloc(DropProbe(Arc::clone(&drops)));
            mutator.root(value)
        });

        drop(heap);

        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(root.cell.heap.upgrade().is_none());
        drop(root.clone());
        drop(root);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}
