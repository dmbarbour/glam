//! Runtime-local garbage collection support for Glam.
//!
//! - **Marking.** A checked, recoverable graph traversal publishes each scalar
//!   mark summary atomically with its collection epoch.
//! - **Sweeping no-drop runs.** A wholly dead run whose payloads need no
//!   destructor returns to untyped arena storage, which later typed-run
//!   publication prefers. A retained partial run clears its dead allocations
//!   and publishes exact lease masks, eligible class frontiers, and one final
//!   stale-cursor epoch.
//! - **Finalization.** Dead identities that need `Drop` move into a durable,
//!   non-rootable finalization batch. It drains outside collector locks under
//!   the installed finalizer mutator, and completed regions return allocator
//!   capacity. Each attempted finalization retires terminally whether `Drop`
//!   returns or unwinds; untouched pending work waits for a later collection,
//!   and the original panic propagates without persistent exceptional state.
//!   Activity, pressure, and reports follow those durable commits.
//! - **Teardown.** Last-owner teardown runs without mutator authority and
//!   visits detached pending runs before ordinary attached class runs.
//! - **Workers and metrics.** The lifetime and admission rules compose across
//!   shared workers. Cold-path collection reports and an explicit tuning
//!   snapshot never participate in collector state.
//!
//! Run and arena geometry, worker-cache width, collection-pressure thresholds,
//! and metric timing details remain implementation policy rather than runtime
//! configuration or managed-program semantics. The bootstrap collector uses
//! one fixed run size; it does not offer variable-size runs.

#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

mod admission;
#[expect(unsafe_code, reason = "reviewed arena and run topology")]
mod arena;
#[expect(unsafe_code, reason = "reviewed canonical metadata dispatch")]
mod class;
#[expect(unsafe_code, reason = "reviewed trace, sweep, and drop dispatch")]
mod heap;
#[expect(unsafe_code, reason = "reviewed mutation boundary")]
mod mutation;
#[expect(unsafe_code, reason = "reviewed allocation boundary")]
mod mutator;
#[expect(unsafe_code, reason = "reviewed pointer boundary")]
mod pointer;
#[expect(unsafe_code, reason = "reviewed root access boundary")]
mod root;
mod run;
#[expect(unsafe_code, reason = "reviewed worker-local allocation")]
mod thread_cache;
#[expect(unsafe_code, reason = "reviewed trace boundary")]
mod trace;
mod trusted_hash;

#[cfg(feature = "deterministic-test-hooks")]
mod deterministic;

#[cfg(feature = "deterministic-test-hooks")]
#[doc(hidden)]
pub use deterministic::{
    EdgeTransitionObservation, EdgeTransitionProbe, EdgeTransitionRecord, FinalizingPhaseProbe,
    GeometryMeasurement, SynchronousCollectionWaitProbe, geometry_measurement,
    is_rootable_for_measurement, trace_work_item_bytes,
};

pub use class::UnsupportedLayout;
pub use heap::{
    CollectionError, CollectionPolicy, CollectionReport, Heap, HeapActivity,
    HeapMaintenanceSnapshot, HeapMetrics, HeapStatistics,
};
pub use mutator::{Allocator, Mutator};
pub use pointer::Gc;
pub use root::Root;
pub use trace::{Trace, Visitor};

#[cfg(test)]
#[expect(unsafe_code, reason = "reviewed boundary verification")]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{Gc, Heap};

    #[test]
    fn empty_heap_can_be_entered_and_dropped() {
        let heap = Heap::new();

        let result = heap.with_mutator(|_| 42);

        assert_eq!(result, 42);
        drop(heap);
    }

    #[test]
    fn empty_heap_can_be_shared_entered_and_dropped_across_threads() {
        const THREADS: usize = 8;

        let heap = Heap::new();
        let entries = Arc::new(AtomicUsize::new(0));
        let threads = (0..THREADS)
            .map(|_| {
                let heap = heap.clone();
                let entries = Arc::clone(&entries);
                std::thread::spawn(move || {
                    heap.with_mutator(|_| {
                        entries.fetch_add(1, Ordering::Relaxed);
                    });
                })
            })
            .collect::<Vec<_>>();

        drop(heap);
        for thread in threads {
            thread.join().expect("empty-heap worker panicked");
        }

        assert_eq!(entries.load(Ordering::Relaxed), THREADS);
    }

    #[test]
    fn independent_empty_heaps_can_be_entered_on_several_threads() {
        let threads = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    Heap::new().with_mutator(|_| ());
                })
            })
            .collect::<Vec<_>>();

        for thread in threads {
            thread.join().expect("empty-heap worker panicked");
        }
    }

    #[test]
    fn managed_pointer_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<Gc<u64>>();
    }

    #[test]
    fn managed_pointer_can_cross_threads_when_its_value_can() {
        let heap = Heap::new();
        let (value, root) = heap.with_mutator(|mutator| {
            let value = mutator.allocator::<u64>().unwrap().alloc(42_u64);
            let root = mutator.root(value.duplicate_in(mutator));
            (value, root)
        });
        let worker_heap = heap.clone();

        let observed = std::thread::spawn(move || {
            worker_heap.with_mutator(|mutator| {
                // SAFETY: `root` remains live in the joining thread, so every
                // intervening collection retains `value`; the matching
                // mutator excludes collection during this access.
                unsafe { *value.get_unchecked(mutator) }
            })
        })
        .join()
        .expect("managed-pointer worker panicked");

        drop(root);
        assert_eq!(observed, 42);
    }

    #[test]
    fn a_thread_holds_mutators_for_one_heap_at_a_time() {
        let first_heap = Heap::new();
        let second_heap = Heap::new();

        first_heap.with_mutator(|first_mutator| {
            let first = first_mutator.allocator::<u64>().unwrap().alloc(11_u64);
            let nested = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                second_heap.with_mutator(|_| {});
            }))
            .expect_err("entering a second heap inside the first should panic");
            assert!(panic_message(nested).contains("only one heap at a time"));

            // SAFETY: `first` was allocated by this heap's mutator and is live.
            assert_eq!(unsafe { *first.get_unchecked(first_mutator) }, 11);
        });

        // Entered one after the other, each heap keeps its own authority.
        second_heap.with_mutator(|second_mutator| {
            let second = second_mutator.allocator::<u64>().unwrap().alloc(22_u64);
            // SAFETY: `second` was allocated by this heap's mutator and is live.
            assert_eq!(unsafe { *second.get_unchecked(second_mutator) }, 22);
        });
        first_heap.with_mutator(|first_mutator| {
            let first = first_mutator.allocator::<u64>().unwrap().alloc(33_u64);
            // SAFETY: as above, for the first heap.
            assert_eq!(unsafe { *first.get_unchecked(first_mutator) }, 33);
        });
    }

    #[cfg(debug_assertions)]
    #[test]
    fn wrong_heap_access_fails_before_dereference() {
        let owner = Heap::new();
        let other = Heap::new();
        let value = owner.with_mutator(|mutator| mutator.allocator::<u64>().unwrap().alloc(42_u64));

        let panic = std::panic::catch_unwind(|| {
            other.with_mutator(|mutator| {
                // SAFETY: this deliberately violates the heap precondition to
                // verify that indexed arena ownership rejects it before
                // dereference.
                let _ = unsafe { value.get_unchecked(mutator) };
            });
        })
        .expect_err("wrong-heap access should panic in debug builds");

        assert!(panic_message(panic).contains("does not belong to this heap"));
    }

    fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
        if let Some(message) = panic.downcast_ref::<String>() {
            message.clone()
        } else if let Some(message) = panic.downcast_ref::<&str>() {
            (*message).to_owned()
        } else {
            "non-string panic".to_owned()
        }
    }
}
