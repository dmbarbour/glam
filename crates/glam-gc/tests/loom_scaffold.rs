//! Loom models for the current collector coordination surface.
//!
//! The heap-entry test remains an API smoke model. Abstract coordinator models
//! cover the ordering edges of C3's stop-the-world state machine, while C2C.5's
//! atomic lease-bit transition remains independent of the raw arena-pointer
//! integration exercised by native forced schedules.

use glam_gc::Heap;
use loom::sync::atomic::{AtomicU64, Ordering};
use loom::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AdmissionPhase {
    #[default]
    Ordinary,
    Exclusive,
    Finalizing,
}

#[derive(Debug, Default)]
struct CoordinatorState {
    phase: AdmissionPhase,
    active: usize,
    requested: bool,
    completed: u64,
}

type Coordinator = (Mutex<CoordinatorState>, Condvar);

fn admit_mutator(coordinator: &Coordinator) {
    let (state, changed) = coordinator;
    let mut state = state.lock().unwrap();
    while state.phase == AdmissionPhase::Exclusive {
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

fn request_collection(coordinator: &Coordinator) {
    let (state, changed) = coordinator;
    state.lock().unwrap().requested = true;
    changed.notify_all();
}

fn elect_idle_collection(coordinator: &Coordinator) -> bool {
    let (state, changed) = coordinator;
    let mut state = state.lock().unwrap();
    if state.phase == AdmissionPhase::Ordinary && state.active == 0 && state.requested {
        state.phase = AdmissionPhase::Exclusive;
        changed.notify_all();
        true
    } else {
        false
    }
}

fn complete_collection_as_entry(coordinator: &Coordinator) {
    let (state, changed) = coordinator;
    {
        let mut state = state.lock().unwrap();
        assert_eq!(state.phase, AdmissionPhase::Exclusive);
        assert_eq!(state.active, 0);
        state.phase = AdmissionPhase::Finalizing;
        state.active = 1;
        changed.notify_all();
    }
    loom::thread::yield_now();
    let mut state = state.lock().unwrap();
    assert_eq!(state.phase, AdmissionPhase::Finalizing);
    assert_ne!(state.active, 0);
    state.phase = AdmissionPhase::Ordinary;
    state.requested = false;
    state.completed += 1;
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
        request_collection(&coordinator);
        published.store(73, Ordering::Relaxed);

        let observer = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let published = Arc::clone(&published);
            move || {
                while !elect_idle_collection(&coordinator) {
                    loom::thread::yield_now();
                }
                let observed = published.load(Ordering::Relaxed);
                complete_collection_as_entry(&coordinator);
                release_mutator(&coordinator);
                observed
            }
        });

        release_mutator(&coordinator);
        assert_eq!(observer.join().unwrap(), 73);
    });
}

#[test]
fn simultaneous_idle_entries_elect_exactly_one_collector() {
    loom::model(|| {
        let coordinator = Arc::new(Coordinator::default());
        let collectors = Arc::new(AtomicU64::new(0));
        request_collection(&coordinator);

        let first = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let collectors = Arc::clone(&collectors);
            move || {
                if elect_idle_collection(&coordinator) {
                    collectors.fetch_add(1, Ordering::Relaxed);
                    complete_collection_as_entry(&coordinator);
                } else {
                    admit_mutator(&coordinator);
                }
                release_mutator(&coordinator);
            }
        });
        let second = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let collectors = Arc::clone(&collectors);
            move || {
                if elect_idle_collection(&coordinator) {
                    collectors.fetch_add(1, Ordering::Relaxed);
                    complete_collection_as_entry(&coordinator);
                } else {
                    admit_mutator(&coordinator);
                }
                release_mutator(&coordinator);
            }
        });

        first.join().unwrap();
        second.join().unwrap();
        assert_eq!(collectors.load(Ordering::Relaxed), 1);
        let state = coordinator.0.lock().unwrap();
        assert_eq!(state.phase, AdmissionPhase::Ordinary);
        assert_eq!(state.active, 0);
        assert_eq!(state.completed, 1);
        assert!(!state.requested);
    });
}

#[test]
fn reciprocal_nested_admission_passes_uncommitted_requests() {
    loom::model(|| {
        let first = Arc::new(Coordinator::default());
        let second = Arc::new(Coordinator::default());
        admit_mutator(&first);
        admit_mutator(&second);
        request_collection(&first);
        request_collection(&second);

        let first_then_second = loom::thread::spawn({
            let first = Arc::clone(&first);
            let second = Arc::clone(&second);
            move || {
                admit_mutator(&second);
                release_mutator(&second);
                release_mutator(&first);
            }
        });
        let second_then_first = loom::thread::spawn({
            let first = Arc::clone(&first);
            let second = Arc::clone(&second);
            move || {
                admit_mutator(&first);
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
        let stage = Arc::new(AtomicU64::new(0));
        let collector = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let stage = Arc::clone(&stage);
            move || {
                {
                    let mut state = coordinator.0.lock().unwrap();
                    assert_eq!(state.phase, AdmissionPhase::Exclusive);
                    assert_eq!(state.active, 0);
                    state.phase = AdmissionPhase::Finalizing;
                    state.active = 1;
                    stage.store(1, Ordering::Release);
                    coordinator.1.notify_all();
                }
                loom::thread::yield_now();
                let mut state = coordinator.0.lock().unwrap();
                assert_eq!(state.phase, AdmissionPhase::Finalizing);
                assert_eq!(state.active, 1);
                state.phase = AdmissionPhase::Ordinary;
                state.completed = 1;
                stage.store(2, Ordering::Release);
                coordinator.1.notify_all();
            }
        });
        let observer = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let stage = Arc::clone(&stage);
            move || {
                while stage.load(Ordering::Acquire) == 0 {
                    loom::thread::yield_now();
                }
                let state = coordinator.0.lock().unwrap();
                if state.phase == AdmissionPhase::Finalizing {
                    assert_ne!(state.active, 0);
                } else {
                    assert_eq!(state.phase, AdmissionPhase::Ordinary);
                    assert_eq!(state.completed, 1);
                    assert_ne!(state.active, 0);
                }
            }
        });

        collector.join().unwrap();
        observer.join().unwrap();
        release_mutator(&coordinator);
    });
}
