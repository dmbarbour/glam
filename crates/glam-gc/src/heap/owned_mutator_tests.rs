//! Owned mutators: admission, suspension, and the transition latches.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use crate::thread_cache::cache_snapshot;
use crate::{Heap, Trace, Visitor};

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else {
        "non-string panic".to_owned()
    }
}

#[test]
fn an_owned_mutator_allocates_roots_and_views() {
    let heap = Heap::new();
    let root = heap.mutate(|mutator| {
        let value = mutator.allocator::<u64>().unwrap().alloc(42_u64);
        let root = mutator.root(value);
        let view = root.as_gc(mutator);
        assert_eq!(*view, 42);
        assert!(view.same_allocation(root.as_gc(mutator)));
        root
    });

    heap.mutate(|mutator| assert_eq!(*root.get(mutator), 42));
    let metrics = heap.metrics();
    assert_eq!(metrics.outer_mutator_entries(), 2);
    assert_eq!(metrics.recursive_mutator_entries(), 0);
}

#[test]
fn legacy_entries_inside_an_owned_mutator_are_recursive() {
    let heap = Heap::new();
    heap.mutate(|mutator| {
        let value = heap.with_mutator(|access| access.allocator::<u64>().unwrap().alloc(5_u64));
        let root = mutator.root(value);
        assert_eq!(*root.get(mutator), 5);
        // A held region inside the mutator adds nothing.
        heap.with_held_region(|| heap.with_mutator(|_| {}));
    });

    let metrics = heap.metrics();
    assert_eq!(metrics.outer_mutator_entries(), 1);
    assert_eq!(metrics.recursive_mutator_entries(), 2);
}

#[test]
fn a_thread_holds_at_most_one_mutator() {
    let heap = Heap::new();
    let other = Heap::new();

    heap.mutate(|_| {
        let nested = catch_unwind(AssertUnwindSafe(|| heap.mutate(|_| {})))
            .expect_err("a second mutator for the same heap should panic");
        assert!(panic_message(nested).contains("at most one mutator"));

        let foreign = catch_unwind(AssertUnwindSafe(|| other.mutate(|_| {})))
            .expect_err("a mutator for another heap should panic");
        assert!(panic_message(foreign).contains("only one heap at a time"));
    });

    heap.with_mutator(|_| {
        let inside_legacy = catch_unwind(AssertUnwindSafe(|| heap.mutate(|_| {})))
            .expect_err("a mutator inside a legacy access should panic");
        assert!(panic_message(inside_legacy).contains("at most one mutator"));
    });

    // The failed attempts left no entry behind.
    heap.mutate(|_| {});
    other.mutate(|_| {});
    assert_eq!(Heap::release_current_thread_caches(), 2);
}

#[test]
fn a_recursive_mutator_nests_inside_other_entries_without_suspending() {
    let heap = Heap::new();
    let other = Heap::new();

    // Outside every entry, it is an owned mutator.
    heap.mutate_recursive(|mutator| mutator.suspend(|| {}));

    heap.with_mutator(|_| {
        heap.mutate_recursive(|mutator| {
            let root = mutator.root(mutator.allocator::<u64>().unwrap().alloc(3_u64));
            assert_eq!(*root.get(mutator), 3);
            let panic = catch_unwind(AssertUnwindSafe(|| mutator.suspend(|| {})))
                .expect_err("a recursive mutator should not suspend");
            assert!(panic_message(panic).contains("recursive mutator cannot suspend"));
            assert_eq!(*root.get(mutator), 3);
        });
    });
    heap.mutate(|_| heap.mutate_recursive(|_| {}));

    // Inside a held region, a release point leaves the region alone while
    // the recursive mutator is active.
    heap.with_held_region(|| {
        heap.mutate_recursive(|_| assert!(!Heap::release_held_region()));
        assert!(Heap::release_held_region());
    });

    heap.mutate(|_| {
        let foreign = catch_unwind(AssertUnwindSafe(|| other.mutate_recursive(|_| {})))
            .expect_err("a recursive mutator for another heap should panic");
        assert!(panic_message(foreign).contains("only one heap at a time"));
    });

    let metrics = heap.metrics();
    assert_eq!(metrics.outer_mutator_entries(), 6);
    assert_eq!(metrics.recursive_mutator_entries(), 3);
}

#[test]
fn a_suspension_admits_a_collection_on_another_thread() {
    let heap = Heap::new();
    let (suspended, suspension) = mpsc::channel();
    let (collected, collection) = mpsc::channel();
    let collector = std::thread::spawn({
        let heap = heap.clone();
        move || {
            suspension.recv().unwrap();
            let report = heap.collect_full().unwrap();
            collected
                .send((report.root_entries(), report.marked_slots()))
                .unwrap();
        }
    });

    heap.mutate(|mutator| {
        let allocator = mutator.allocator::<u64>().unwrap();
        let root = mutator.root(allocator.alloc(7_u64));
        let _garbage = allocator.alloc(8_u64);
        let epoch = heap.inner.current_allocation_lease_epoch();

        // The collector starts only once this thread has suspended, and the
        // suspension ends only once it has finished: without a release of
        // admission, the two would deadlock.
        let (roots, marked) = mutator.suspend(|| {
            suspended.send(()).unwrap();
            collection.recv().unwrap()
        });
        assert_eq!((roots, marked), (1, 1));
        assert_ne!(heap.inner.current_allocation_lease_epoch(), epoch);

        // Resumption captured the new lease epoch before allocating again.
        let snapshot = cache_snapshot(&heap.inner).unwrap();
        assert_eq!(snapshot.recursive_depth, 1);
        assert_eq!(
            snapshot.captured_epoch,
            heap.inner.current_allocation_lease_epoch()
        );
        assert_eq!(*root.get(mutator), 7);
        let after = mutator.allocator::<u64>().unwrap().alloc(9_u64);
        assert_eq!(*mutator.root(after).get(mutator), 9);
    });
    collector.join().expect("collector thread panicked");
}

#[test]
fn a_collection_inside_a_suspension_keeps_the_mutator_record() {
    let heap = Heap::new();
    heap.mutate(|mutator| {
        let root = mutator.root(mutator.allocator::<u64>().unwrap().alloc(11_u64));
        let report = mutator.suspend(|| heap.collect_full().unwrap());
        assert_eq!(report.root_entries(), 1);
        assert_eq!(*root.get(mutator), 11);
        // Resumption found the same record, or it would have panicked.
        let _ = mutator.allocator::<u64>().unwrap().alloc(12_u64);
    });
}

#[test]
fn a_suspension_may_take_other_mutators() {
    let heap = Heap::new();
    let other = Heap::new();
    heap.mutate(|mutator| {
        let root = mutator.root(mutator.allocator::<u64>().unwrap().alloc(1_u64));
        let (same, foreign) = mutator.suspend(|| {
            let same = heap.mutate(|inner| {
                let value = inner.allocator::<u64>().unwrap().alloc(2_u64);
                inner.root(value)
            });
            let foreign = other.mutate(|inner| {
                let value = inner.allocator::<u64>().unwrap().alloc(3_u64);
                *inner.root(value).get(inner)
            });
            (same, foreign)
        });
        assert_eq!(*root.get(mutator), 1);
        assert_eq!(*same.get(mutator), 2);
        assert_eq!(foreign, 3);
    });
}

#[test]
fn a_mutator_cannot_suspend_inside_a_legacy_access() {
    let heap = Heap::new();
    heap.mutate(|mutator| {
        let root = mutator.root(mutator.allocator::<u64>().unwrap().alloc(4_u64));
        let panic = catch_unwind(AssertUnwindSafe(|| {
            heap.with_mutator(|_| mutator.suspend(|| {}));
        }))
        .expect_err("suspending under a legacy access should panic");
        assert!(panic_message(panic).contains("heap access is open"));

        // The refused suspension kept the mutator's admission.
        assert_eq!(*root.get(mutator), 4);
        mutator.suspend(|| {});
        assert_eq!(*root.get(mutator), 4);
    });
}

#[test]
fn release_points_panic_under_an_owned_mutator() {
    let heap = Heap::new();
    heap.mutate(|mutator| {
        let panic = catch_unwind(Heap::release_held_region)
            .expect_err("a release point under an owned mutator should panic");
        assert!(panic_message(panic).contains("under an owned mutator"));

        // Inside a suspension, held regions and their release points work.
        mutator.suspend(|| {
            heap.with_held_region(|| assert!(Heap::release_held_region()));
            assert!(!Heap::release_held_region());
        });
    });
}

#[test]
fn thread_caches_stay_while_a_mutator_is_suspended() {
    let heap = Heap::new();
    heap.mutate(|mutator| {
        mutator.suspend(|| {
            let panic = catch_unwind(Heap::release_current_thread_caches)
                .expect_err("releasing a suspended mutator's cache should panic");
            assert!(panic_message(panic).contains("mutator is suspended"));
        });
    });
    assert_eq!(Heap::release_current_thread_caches(), 1);
}

#[test]
fn an_unwinding_suspension_leaves_the_mutator_without_access() {
    let heap = Heap::new();
    heap.mutate(|mutator| {
        let panic = catch_unwind(AssertUnwindSafe(|| {
            mutator.suspend(|| panic!("suspended work failed"));
        }))
        .expect_err("the suspended work's panic should propagate");
        assert_eq!(panic_message(panic), "suspended work failed");

        let access = catch_unwind(AssertUnwindSafe(|| {
            let _ = mutator.allocator::<u64>();
        }))
        .expect_err("a mutator that did not re-admit should grant no access");
        assert!(panic_message(access).contains("grants no access"));
    });

    // Nothing of the failed mutator stays behind.
    assert!(cache_snapshot(&heap.inner).is_some_and(|snapshot| snapshot.recursive_depth == 0));
    heap.mutate(|mutator| {
        let _ = mutator.allocator::<u64>().unwrap().alloc(1_u64);
    });
    assert_eq!(Heap::release_current_thread_caches(), 1);
}

static POISON_DROPS: AtomicUsize = AtomicUsize::new(0);

struct CountedDrop(&'static AtomicUsize);

impl Drop for CountedDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: this fixture has no managed edge.
unsafe impl Trace for CountedDrop {
    fn trace(&self, _visitor: &mut Visitor<'_>) {}
}

#[test]
fn re_admission_into_a_poisoned_heap_leaves_the_mutator_without_access() {
    let heap = Heap::new();
    heap.mutate(|mutator| {
        let allocator = mutator.allocator::<u64>().unwrap();
        let _root = mutator.root(allocator.alloc(10_u64));
        let _dead = allocator.alloc(20_u64);
        let _dropping = mutator
            .allocator::<CountedDrop>()
            .unwrap()
            .alloc(CountedDrop(&POISON_DROPS));

        let panic = catch_unwind(AssertUnwindSafe(|| {
            mutator.suspend(|| {
                heap.inner.inject_panic_after_topology_mutation();
                let collection = catch_unwind(AssertUnwindSafe(|| heap.collect_full()));
                assert!(collection.is_err(), "the injected panic should poison");
            });
        }))
        .expect_err("re-admission into a poisoned heap should panic");
        assert_eq!(panic_message(panic), "managed heap is permanently poisoned");

        let access = catch_unwind(AssertUnwindSafe(|| {
            let _ = mutator.allocator::<u64>();
        }))
        .expect_err("a mutator that did not re-admit should grant no access");
        assert!(panic_message(access).contains("grants no access"));
    });
    assert!(cache_snapshot(&heap.inner).is_some_and(|snapshot| snapshot.recursive_depth == 0));
}

#[cfg(debug_assertions)]
#[test]
fn a_view_kept_across_a_suspension_by_unsafe_code_fails_on_use() {
    let heap = Heap::new();
    heap.mutate(|mutator| {
        let root = mutator.root(mutator.allocator::<u64>().unwrap().alloc(6_u64));
        // SAFETY: deliberately unsound lifetime extension, so the debug check
        // can reject the view after the suspension. The root keeps the value
        // live, so the rejected read could not dangle in this test.
        let kept: crate::GcRef<'static, u64> = unsafe { std::mem::transmute(root.as_gc(mutator)) };
        mutator.suspend(|| {});
        let panic = catch_unwind(AssertUnwindSafe(|| *kept))
            .expect_err("a view kept across a suspension should fail on use");
        assert!(panic_message(panic).contains("outlived its heap admission"));
    });
}
