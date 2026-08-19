//! Runtime-local garbage collection support for Glam.
//!
//! Phase C0 intentionally exposes only an empty shareable heap and scoped
//! entry. Allocation, managed pointers, tracing, and collection begin in later
//! phases after their safety contracts have been reviewed.

#![deny(unsafe_op_in_unsafe_fn)]

mod heap;
mod mutator;

#[cfg(feature = "deterministic-test-hooks")]
mod deterministic;

pub use heap::Heap;
pub use mutator::Mutator;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::Heap;

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
}
