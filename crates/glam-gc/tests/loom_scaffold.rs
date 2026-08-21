//! Loom smoke model for the C1A heap-entry API.
//!
//! The prototype heap still has no collector synchronization to prove. This
//! model verifies that the crate and its shareable entry surface run under
//! Loom; the first substantive state-machine model belongs to C3.

use glam_gc::Heap;

#[test]
fn empty_heap_entry_runs_under_loom() {
    loom::model(|| {
        let heap = Heap::new();
        let other = heap.clone();

        let thread = loom::thread::spawn(move || other.with_mutator(|_| ()));
        heap.with_mutator(|_| ());
        thread.join().expect("modeled empty-heap worker panicked");
    });
}
