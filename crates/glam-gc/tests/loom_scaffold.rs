//! Loom smoke model for the C0 heap API.
//!
//! The empty heap has no collector synchronization to prove yet. This model
//! verifies that the crate and its shareable entry surface run under Loom; the
//! first substantive state-machine model belongs to the phase which adds that
//! state.

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
