//! Runtime-local garbage collection support for Glam.
//!
//! Phase C1A adds a deliberately leaking prototype allocation path so the
//! managed-pointer and mutator access contracts can be tested before the real
//! allocator, tracing, or collection exist.

#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

mod heap;
#[expect(unsafe_code, reason = "reviewed C1C mutation boundary")]
mod mutation;
#[expect(unsafe_code, reason = "reviewed C1A allocation boundary")]
mod mutator;
#[expect(unsafe_code, reason = "reviewed C1A pointer boundary")]
mod pointer;
#[expect(unsafe_code, reason = "reviewed C1B trace boundary")]
mod trace;

#[cfg(feature = "deterministic-test-hooks")]
mod deterministic;

pub use heap::Heap;
pub use mutator::Mutator;
pub use pointer::Gc;
pub use trace::{Trace, Visitor};

#[cfg(test)]
#[expect(unsafe_code, reason = "reviewed C1A boundary verification")]
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
        let value = heap.with_mutator(|mutator| mutator.alloc(42_u64));
        let worker_heap = heap.clone();

        let observed = std::thread::spawn(move || {
            worker_heap.with_mutator(|mutator| {
                // SAFETY: `value` was allocated by `worker_heap`, prototype
                // allocations remain live forever, and its type is `u64`.
                unsafe { *value.get_unchecked(mutator) }
            })
        })
        .join()
        .expect("managed-pointer worker panicked");

        assert_eq!(observed, 42);
    }

    #[test]
    fn nested_heap_entries_keep_their_authority_separate() {
        let first_heap = Heap::new();
        let second_heap = Heap::new();

        first_heap.with_mutator(|first_mutator| {
            let first = first_mutator.alloc(11_u64);
            second_heap.with_mutator(|second_mutator| {
                let second = second_mutator.alloc(22_u64);

                // SAFETY: each pointer is paired with the mutator for the heap
                // which allocated it, and both prototype allocations are live.
                let first = unsafe { *first.get_unchecked(first_mutator) };
                // SAFETY: as above, for the second heap and allocation.
                let second = unsafe { *second.get_unchecked(second_mutator) };
                assert_eq!((first, second), (11, 22));
            });
        });
    }

    #[cfg(debug_assertions)]
    #[test]
    fn wrong_heap_access_fails_before_dereference() {
        let owner = Heap::new();
        let other = Heap::new();
        let value = owner.with_mutator(|mutator| mutator.alloc(42_u64));

        let panic = std::panic::catch_unwind(|| {
            other.with_mutator(|mutator| {
                // SAFETY: this deliberately violates the heap precondition to
                // verify that C1A's debug gateway rejects it before dereference.
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
