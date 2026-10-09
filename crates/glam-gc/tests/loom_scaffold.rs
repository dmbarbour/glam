//! Loom models for the current collector coordination surface.
//!
//! The heap-entry test remains an API smoke model. The coordinator models
//! drive the collector's own admission gate, compiled here against Loom's
//! primitives, beneath an abstract copy of the coordinator's phases. The
//! atomic lease-bit transition remains independent of the raw arena-pointer
//! integration exercised by native forced schedules.

use glam_gc::Heap;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The primitives the admission gate is built on.
mod sync {
    pub(crate) use loom::sync::atomic::{AtomicUsize, Ordering};
    pub(crate) use loom::sync::{Condvar, Mutex, MutexGuard};
}

#[path = "../src/admission/gate.rs"]
#[allow(dead_code, reason = "the models use only part of the gate")]
mod gate;

use gate::{AdmissionGate, GateState};

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
    completed: u64,
}

impl GateState for CoordinatorState {
    fn coordinates_mutators(&self) -> bool {
        self.phase != AdmissionPhase::Ordinary
    }
}

/// One heap's admission: the gate, and the collection request beside it.
struct Coordinator {
    gate: AdmissionGate<CoordinatorState>,
    requested: AtomicBool,
    /// Whether an idle entry elects itself to collect, as under
    /// `CollectionPolicy::Automatic`.
    automatic: bool,
}

impl Coordinator {
    fn new(automatic: bool) -> Self {
        Self {
            gate: AdmissionGate::new(CoordinatorState::default()),
            requested: AtomicBool::new(false),
            automatic,
        }
    }

    fn collection_requested(&self) -> bool {
        self.automatic && self.requested.load(Ordering::Acquire)
    }
}

/// Admits an outer mutator, returning whether it was elected to collect
/// first instead.
fn admit_mutator(coordinator: &Coordinator) -> bool {
    if coordinator
        .gate
        .try_enter(|| coordinator.collection_requested())
    {
        return false;
    }
    let mut state = coordinator.gate.lock().unwrap();
    loop {
        match state.phase {
            AdmissionPhase::Ordinary => {
                if coordinator.collection_requested() && state.active_outer_mutators() == 0 {
                    state.phase = AdmissionPhase::Exclusive;
                    state.notify();
                    return true;
                }
                state.admit_outer_mutator();
                return false;
            }
            AdmissionPhase::Finalizing => {
                state.admit_outer_mutator();
                return false;
            }
            AdmissionPhase::Exclusive => state = state.wait().unwrap(),
        }
    }
}

fn release_mutator(coordinator: &Coordinator) {
    drop(coordinator.gate.exit());
}

fn request_collection(coordinator: &Coordinator) {
    coordinator.requested.store(true, Ordering::Release);
}

/// Waits for an idle heap and elects the caller, like `Heap::collect_full`.
fn collect_when_idle(coordinator: &Coordinator) {
    let mut state = coordinator.gate.lock().unwrap();
    while state.phase != AdmissionPhase::Ordinary || state.active_outer_mutators() != 0 {
        state = state.wait().unwrap();
    }
    state.phase = AdmissionPhase::Exclusive;
    state.notify();
}

/// Hands exclusive authority to the collector's own mutator, then completes
/// the collection; the collector still holds that mutator afterwards.
fn complete_collection_as_entry(coordinator: &Coordinator) {
    {
        let mut state = coordinator.gate.lock().unwrap();
        assert_eq!(state.phase, AdmissionPhase::Exclusive);
        assert_eq!(state.active_outer_mutators(), 0);
        state.phase = AdmissionPhase::Finalizing;
        state.admit_outer_mutator();
        state.notify();
    }
    loom::thread::yield_now();
    let mut state = coordinator.gate.lock().unwrap();
    assert_eq!(state.phase, AdmissionPhase::Finalizing);
    assert_ne!(state.active_outer_mutators(), 0);
    coordinator.requested.store(false, Ordering::Release);
    state.phase = AdmissionPhase::Ordinary;
    state.completed += 1;
    state.notify();
}

/// Runs a model whose two-sided coordinator traffic makes exhaustive search
/// slow. Three preemptions still cover every planted gate fault the models
/// were checked against.
fn bounded_model(model: impl Fn() + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(model);
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

fn claim_exact_bit(lease: &AtomicU64, allocation: &AtomicU64, bit: u64) -> bool {
    let observed = lease.load(Ordering::Acquire);
    if observed & bit != 0 {
        return false;
    }
    let claimed = lease
        .compare_exchange(
            observed,
            observed | bit,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok();
    if claimed {
        assert_eq!(
            allocation.load(Ordering::Relaxed),
            0,
            "winning the released lease must observe allocation retirement"
        );
    }
    claimed
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
fn finalized_word_release_preserves_neighbor_and_has_one_visible_winner() {
    loom::model(|| {
        let allocation = Arc::new(AtomicU64::new(1));
        let lease = Arc::new(AtomicU64::new(0b01));

        let finalizer = loom::thread::spawn({
            let allocation = Arc::clone(&allocation);
            let lease = Arc::clone(&lease);
            move || {
                allocation.store(0, Ordering::Relaxed);
                let prior = lease.fetch_and(!0b01, Ordering::Release);
                assert_ne!(prior & 0b01, 0);
            }
        });
        let first_claim = loom::thread::spawn({
            let allocation = Arc::clone(&allocation);
            let lease = Arc::clone(&lease);
            move || claim_exact_bit(&lease, &allocation, 0b01)
        });

        // This RMW models a concurrent claim in a neighboring allocation word
        // represented by the same lease word.
        assert_eq!(lease.fetch_or(0b10, Ordering::AcqRel) & 0b10, 0);
        finalizer.join().unwrap();
        let first_claim = first_claim.join().unwrap();
        let published = lease.load(Ordering::Acquire);
        assert_eq!(published & 0b10, 0b10);
        assert_eq!(published & 0b01 != 0, first_claim);

        // A claimant which overlaps the Release must observe the prior
        // allocation retirement when it wins. If it arrived too early, two
        // post-release allocators race for the word instead. Either way, the
        // lease CAS grants the released bit exactly once.
        let winners = if first_claim {
            assert!(!claim_exact_bit(&lease, &allocation, 0b01));
            1
        } else {
            let second = loom::thread::spawn({
                let allocation = Arc::clone(&allocation);
                let lease = Arc::clone(&lease);
                move || claim_exact_bit(&lease, &allocation, 0b01)
            });
            let third = claim_exact_bit(&lease, &allocation, 0b01);
            usize::from(second.join().unwrap()) + usize::from(third)
        };
        assert_eq!(winners, 1);
        assert_eq!(lease.load(Ordering::Acquire) & 0b11, 0b11);
    });
}

#[test]
fn mutator_release_publishes_prior_work_to_exclusive_admission() {
    loom::model(|| {
        let coordinator = Arc::new(Coordinator::new(false));
        let published = Arc::new(AtomicU64::new(0));

        admit_mutator(&coordinator);
        request_collection(&coordinator);
        published.store(73, Ordering::Relaxed);

        // A lock-free exit must still wake the collector waiting for zero.
        let collector = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let published = Arc::clone(&published);
            move || {
                collect_when_idle(&coordinator);
                let observed = published.load(Ordering::Relaxed);
                complete_collection_as_entry(&coordinator);
                release_mutator(&coordinator);
                observed
            }
        });

        release_mutator(&coordinator);
        assert_eq!(collector.join().unwrap(), 73);
    });
}

#[test]
fn waiting_collector_wakes_after_the_last_of_several_exits() {
    bounded_model(|| {
        let coordinator = Arc::new(Coordinator::new(false));
        admit_mutator(&coordinator);
        admit_mutator(&coordinator);

        // The first exit takes the lock and unlocks again while the
        // collector still waits; the second must then wake it.
        let collector = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            move || {
                collect_when_idle(&coordinator);
                complete_collection_as_entry(&coordinator);
                release_mutator(&coordinator);
            }
        });
        let other = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            move || release_mutator(&coordinator)
        });

        release_mutator(&coordinator);
        other.join().unwrap();
        collector.join().unwrap();
        assert_eq!(coordinator.gate.lock().unwrap().active_outer_mutators(), 0);
    });
}

#[test]
fn simultaneous_idle_entries_elect_exactly_one_collector() {
    bounded_model(|| {
        let coordinator = Arc::new(Coordinator::new(true));
        let collectors = Arc::new(AtomicU64::new(0));
        request_collection(&coordinator);

        let entrant = |coordinator: Arc<Coordinator>, collectors: Arc<AtomicU64>| {
            move || {
                if admit_mutator(&coordinator) {
                    collectors.fetch_add(1, Ordering::Relaxed);
                    complete_collection_as_entry(&coordinator);
                }
                release_mutator(&coordinator);
            }
        };
        let first = loom::thread::spawn(entrant(Arc::clone(&coordinator), Arc::clone(&collectors)));
        let second =
            loom::thread::spawn(entrant(Arc::clone(&coordinator), Arc::clone(&collectors)));

        first.join().unwrap();
        second.join().unwrap();
        assert_eq!(collectors.load(Ordering::Relaxed), 1);
        assert!(!coordinator.requested.load(Ordering::Acquire));
        let state = coordinator.gate.lock().unwrap();
        assert_eq!(state.phase, AdmissionPhase::Ordinary);
        assert_eq!(state.active_outer_mutators(), 0);
        assert_eq!(state.completed, 1);
    });
}

#[test]
fn election_excludes_a_racing_lock_free_entry() {
    bounded_model(|| {
        let coordinator = Arc::new(Coordinator::new(false));
        request_collection(&coordinator);

        let entrant = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            move || {
                admit_mutator(&coordinator);
                release_mutator(&coordinator);
            }
        });

        collect_when_idle(&coordinator);
        assert_eq!(
            coordinator.gate.lock().unwrap().active_outer_mutators(),
            0,
            "a mutator entered during exclusive collection"
        );
        complete_collection_as_entry(&coordinator);
        release_mutator(&coordinator);
        entrant.join().unwrap();
        assert_eq!(coordinator.gate.lock().unwrap().active_outer_mutators(), 0);
    });
}

#[test]
fn lock_free_entry_after_collection_observes_its_work() {
    loom::model(|| {
        let coordinator = Arc::new(Coordinator::new(false));
        coordinator.gate.lock().unwrap().phase = AdmissionPhase::Exclusive;
        let published = Arc::new(AtomicU64::new(0));

        let entrant = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let published = Arc::clone(&published);
            move || {
                admit_mutator(&coordinator);
                let observed = published.load(Ordering::Relaxed);
                release_mutator(&coordinator);
                observed
            }
        });

        published.store(29, Ordering::Relaxed);
        complete_collection_as_entry(&coordinator);
        release_mutator(&coordinator);
        assert_eq!(entrant.join().unwrap(), 29);
    });
}

#[test]
fn reciprocal_nested_admission_passes_uncommitted_requests() {
    loom::model(|| {
        let first = Arc::new(Coordinator::new(false));
        let second = Arc::new(Coordinator::new(false));
        admit_mutator(&first);
        admit_mutator(&second);
        request_collection(&first);
        request_collection(&second);

        let first_then_second = loom::thread::spawn({
            let first = Arc::clone(&first);
            let second = Arc::clone(&second);
            move || {
                assert!(!admit_mutator(&second));
                release_mutator(&second);
                release_mutator(&first);
            }
        });
        let second_then_first = loom::thread::spawn({
            let first = Arc::clone(&first);
            let second = Arc::clone(&second);
            move || {
                assert!(!admit_mutator(&first));
                release_mutator(&first);
                release_mutator(&second);
            }
        });

        first_then_second.join().unwrap();
        second_then_first.join().unwrap();
        assert_eq!(first.gate.lock().unwrap().active_outer_mutators(), 0);
        assert_eq!(second.gate.lock().unwrap().active_outer_mutators(), 0);
    });
}

#[test]
fn exclusive_to_finalizer_handoff_never_publishes_an_authority_gap() {
    loom::model(|| {
        let coordinator = Arc::new(Coordinator::new(false));
        coordinator.gate.lock().unwrap().phase = AdmissionPhase::Exclusive;
        let stage = Arc::new(AtomicU64::new(0));
        let collector = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let stage = Arc::clone(&stage);
            move || {
                {
                    let mut state = coordinator.gate.lock().unwrap();
                    assert_eq!(state.phase, AdmissionPhase::Exclusive);
                    assert_eq!(state.active_outer_mutators(), 0);
                    state.phase = AdmissionPhase::Finalizing;
                    state.admit_outer_mutator();
                    stage.store(1, Ordering::Release);
                    state.notify();
                }
                loom::thread::yield_now();
                let mut state = coordinator.gate.lock().unwrap();
                assert_eq!(state.phase, AdmissionPhase::Finalizing);
                assert_eq!(state.active_outer_mutators(), 1);
                state.phase = AdmissionPhase::Ordinary;
                state.completed = 1;
                stage.store(2, Ordering::Release);
                state.notify();
            }
        });
        let observer = loom::thread::spawn({
            let coordinator = Arc::clone(&coordinator);
            let stage = Arc::clone(&stage);
            move || {
                while stage.load(Ordering::Acquire) == 0 {
                    loom::thread::yield_now();
                }
                let state = coordinator.gate.lock().unwrap();
                if state.phase == AdmissionPhase::Finalizing {
                    assert_ne!(state.active_outer_mutators(), 0);
                } else {
                    assert_eq!(state.phase, AdmissionPhase::Ordinary);
                    assert_eq!(state.completed, 1);
                    assert_ne!(state.active_outer_mutators(), 0);
                }
            }
        });

        collector.join().unwrap();
        observer.join().unwrap();
        release_mutator(&coordinator);
    });
}
