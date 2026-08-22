//! Loom models for the current collector coordination surface.
//!
//! The heap-entry test remains an API smoke model until C3 introduces the
//! stop-the-world state machine. C2C.5 additionally models the atomic lease-bit
//! transition independently of the raw arena-pointer integration exercised by
//! the native forced schedules.

use glam_gc::Heap;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicU64, Ordering};

fn claim_bit(lease: &AtomicU64, valid_bits: u32) -> Option<u32> {
    let mut observed = lease.load(Ordering::Acquire);
    loop {
        let candidates = !observed;
        if candidates == 0 {
            return None;
        }
        let bit_index = candidates.trailing_zeros();
        if bit_index >= valid_bits {
            return None;
        }
        let bit = 1_u64 << bit_index;
        match lease.compare_exchange_weak(
            observed,
            observed | bit,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Some(bit_index),
            Err(actual) => observed = actual,
        }
    }
}

#[test]
fn empty_heap_entry_runs_under_loom() {
    loom::model(|| {
        let heap = Heap::new();
        let other = heap.clone();

        let thread = loom::thread::spawn(move || other.with_mutator(|_| ()));
        heap.with_mutator(|_| ());
        thread.join().expect("modeled empty-heap worker panicked");
        assert_eq!(Heap::release_current_thread_caches(), 1);
    });
}

#[test]
fn lease_claim_transition_is_unique_under_loom() {
    loom::model(|| {
        let lease = Arc::new(AtomicU64::new(0));
        let other = Arc::clone(&lease);
        let thread = loom::thread::spawn(move || claim_bit(&other, 2));
        let local = claim_bit(&lease, 2);
        let remote = thread.join().expect("modeled lease claimer panicked");

        assert_eq!(lease.load(Ordering::Acquire), 0b11);
        assert_ne!(local, remote);
        assert!(local.is_some());
        assert!(remote.is_some());
    });
}
