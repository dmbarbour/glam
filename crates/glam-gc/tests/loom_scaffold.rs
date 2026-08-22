//! Loom models for the current collector coordination surface.
//!
//! The heap-entry test remains an API smoke model. Abstract coordinator models
//! cover the ordering edges of C3's stop-the-world state machine, while C2C.5's
//! atomic lease-bit transition remains independent of the raw arena-pointer
//! integration exercised by native forced schedules.

use glam_gc::Heap;
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use loom::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AdmissionPhase {
    #[default]
    Ordinary,
    ExclusivePending,
    Exclusive,
    Finalizing,
}

#[derive(Debug, Default)]
struct CoordinatorState {
    phase: AdmissionPhase,
    active: usize,
}

type Coordinator = (Mutex<CoordinatorState>, Condvar);

fn admit_mutator(coordinator: &Coordinator) {
    admit_mutator_as(coordinator, false);
}

fn admit_mutator_as(coordinator: &Coordinator, dependent: bool) {
    let (state, changed) = coordinator;
    let mut state = state.lock().unwrap();
    while state.phase != AdmissionPhase::Ordinary
        && !(dependent && state.phase == AdmissionPhase::ExclusivePending)
    {
        state = changed.wait(state).unwrap();
    }
    state.active += 1;
}

fn release_mutator(coordinator: &Coordinator) {
    let (state, changed) = coordinator;
    let mut state = state.lock().unwrap();
    state.active -= 1;
    if state.active == 0 {
        changed.notify_all();
    }
}

fn acquire_exclusive(coordinator: &Coordinator, pending: Option<&AtomicBool>) {
    let (state, changed) = coordinator;
    let mut state = state.lock().unwrap();
    while state.phase != AdmissionPhase::Ordinary {
        state = changed.wait(state).unwrap();
    }
    state.phase = AdmissionPhase::ExclusivePending;
    if let Some(pending) = pending {
        pending.store(true, Ordering::Release);
    }
    while state.active != 0 {
        state = changed.wait(state).unwrap();
    }
    state.phase = AdmissionPhase::Exclusive;
}

fn release_exclusive(coordinator: &Coordinator) {
    let (state, changed) = coordinator;
    let mut state = state.lock().unwrap();
    assert_eq!(state.phase, AdmissionPhase::Exclusive);
    assert_eq!(state.active, 0);
    state.phase = AdmissionPhase::Ordinary;
    changed.notify_all();
}

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

#[test]
fn mutator_release_publishes_prior_work_to_exclusive_admission() {
    loom::model(|| {
        let coordinator = Arc::new(Coordinator::default());
        let published = Arc::new(AtomicU64::new(0));

        admit_mutator(&coordinator);
        published.store(73, Ordering::Relaxed);

        let observer = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let published = Arc::clone(&published);
            move || {
                acquire_exclusive(&coordinator, None);
                let observed = published.load(Ordering::Relaxed);
                release_exclusive(&coordinator);
                observed
            }
        });

        release_mutator(&coordinator);
        assert_eq!(observer.join().unwrap(), 73);
    });
}

#[test]
fn pending_exclusive_precedes_fresh_mutator_admission() {
    loom::model(|| {
        let coordinator = Arc::new(Coordinator::default());
        let pending = Arc::new(AtomicBool::new(false));
        let exclusive_completed = Arc::new(AtomicBool::new(false));

        admit_mutator(&coordinator);
        let collector = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let pending = Arc::clone(&pending);
            let exclusive_completed = Arc::clone(&exclusive_completed);
            move || {
                acquire_exclusive(&coordinator, Some(&pending));
                exclusive_completed.store(true, Ordering::Relaxed);
                release_exclusive(&coordinator);
            }
        });
        let entrant = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let pending = Arc::clone(&pending);
            let exclusive_completed = Arc::clone(&exclusive_completed);
            move || {
                while !pending.load(Ordering::Acquire) {
                    loom::thread::yield_now();
                }
                admit_mutator(&coordinator);
                assert!(exclusive_completed.load(Ordering::Relaxed));
                release_mutator(&coordinator);
            }
        });

        release_mutator(&coordinator);
        collector.join().unwrap();
        entrant.join().unwrap();
    });
}

#[test]
fn reciprocal_dependent_admission_passes_pending_collections() {
    loom::model(|| {
        let first = Arc::new(Coordinator::default());
        let second = Arc::new(Coordinator::default());
        admit_mutator(&first);
        admit_mutator(&second);
        first.0.lock().unwrap().phase = AdmissionPhase::ExclusivePending;
        second.0.lock().unwrap().phase = AdmissionPhase::ExclusivePending;

        let first_then_second = loom::thread::spawn({
            let first = Arc::clone(&first);
            let second = Arc::clone(&second);
            move || {
                admit_mutator_as(&second, true);
                release_mutator(&second);
                release_mutator(&first);
            }
        });
        let second_then_first = loom::thread::spawn({
            let first = Arc::clone(&first);
            let second = Arc::clone(&second);
            move || {
                admit_mutator_as(&first, true);
                release_mutator(&first);
                release_mutator(&second);
            }
        });

        first_then_second.join().unwrap();
        second_then_first.join().unwrap();
        assert_eq!(first.0.lock().unwrap().active, 0);
        assert_eq!(second.0.lock().unwrap().active, 0);
    });
}

#[test]
fn exclusive_to_finalizer_handoff_never_publishes_an_authority_gap() {
    loom::model(|| {
        let coordinator = Arc::new(Coordinator::default());
        {
            let mut state = coordinator.0.lock().unwrap();
            state.phase = AdmissionPhase::Exclusive;
        }
        let finalizing = Arc::new(AtomicBool::new(false));
        let collector = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let finalizing = Arc::clone(&finalizing);
            move || {
                {
                    let mut state = coordinator.0.lock().unwrap();
                    assert_eq!(state.phase, AdmissionPhase::Exclusive);
                    assert_eq!(state.active, 0);
                    state.phase = AdmissionPhase::Finalizing;
                    state.active = 1;
                    finalizing.store(true, Ordering::Release);
                    coordinator.1.notify_all();
                }
                loom::thread::yield_now();
                let mut state = coordinator.0.lock().unwrap();
                assert_eq!(state.phase, AdmissionPhase::Finalizing);
                assert_eq!(state.active, 1);
                state.active = 0;
                state.phase = AdmissionPhase::Ordinary;
                coordinator.1.notify_all();
            }
        });
        let observer = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let finalizing = Arc::clone(&finalizing);
            move || {
                while !finalizing.load(Ordering::Acquire) {
                    loom::thread::yield_now();
                }
                let state = coordinator.0.lock().unwrap();
                if state.phase == AdmissionPhase::Finalizing {
                    assert_ne!(state.active, 0);
                } else {
                    assert_eq!(state.phase, AdmissionPhase::Ordinary);
                }
            }
        });

        collector.join().unwrap();
        observer.join().unwrap();
    });
}
