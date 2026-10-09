//! A coordinator lock with a lock-free path for ordinary outer mutators.
//!
//! The gate guards coordinator state with a mutex and a condition variable,
//! and keeps the active outer-mutator count beside them in one atomic word.
//! The word's top bit, `COORDINATED`, is set while outer entries and exits
//! must take the lock: while the state asks for that, or a thread waits on
//! the condition variable. While it is clear, an outer entry or exit only
//! changes the count.
//!
//! - A lock holder reads the count by setting `COORDINATED` in the same
//!   atomic step. No lock-free entry can then join a count the holder has
//!   seen, and every later exit takes the lock, so a waiter for zero cannot
//!   miss its wake-up.
//! - The guard settles the bit as it unlocks: set if the state coordinates
//!   mutators or anyone waits, clear otherwise. Settling under the lock
//!   orders every change of the bit.
//! - Every operation on the word is a read-modify-write, so a release
//!   sequence runs through all of them. An Acquire read of the count sees
//!   the work of every exit it counts, and a lock-free entry sees everything
//!   published before the bit last cleared.
//!
//! The Loom models in `tests/loom_scaffold.rs` compile this file against
//! Loom's primitives.

use std::ops::{Deref, DerefMut};
use std::sync::{LockResult, PoisonError};

use super::sync::{AtomicUsize, Condvar, Mutex, MutexGuard, Ordering};

/// Set while outer entries and exits must take the lock.
const COORDINATED: usize = 1 << (usize::BITS - 1);
/// The active outer-mutator count.
const ACTIVE: usize = !COORDINATED;

/// Coordinator state guarded by an [`AdmissionGate`].
pub(crate) trait GateState {
    /// Whether outer entries and exits must take the lock for this state.
    /// The gate adds its own waiters.
    fn coordinates_mutators(&self) -> bool;
}

/// The coordinator lock, with the active outer-mutator count beside it.
pub(crate) struct AdmissionGate<S> {
    /// The active outer-mutator count, plus `COORDINATED`.
    word: AtomicUsize,
    locked: Mutex<Locked<S>>,
    changed: Condvar,
    #[cfg(test)]
    notifications: std::sync::atomic::AtomicUsize,
}

struct Locked<S> {
    state: S,
    /// Threads waiting on `changed`. A notification with no waiter is
    /// skipped: the condition variable would make a futex call anyway.
    waiters: usize,
}

impl<S: GateState> AdmissionGate<S> {
    pub(crate) fn new(state: S) -> Self {
        let word = if state.coordinates_mutators() {
            COORDINATED
        } else {
            0
        };
        Self {
            word: AtomicUsize::new(word),
            locked: Mutex::new(Locked { state, waiters: 0 }),
            changed: Condvar::new(),
            #[cfg(test)]
            notifications: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Admits one outer mutator without the lock, if the coordinator need
    /// not see it.
    ///
    /// An entry which would be the only active mutator first asks
    /// `lock_when_idle`, so the coordinator can elect it to collect instead.
    /// On `false` the caller must take the lock.
    pub(crate) fn try_enter(&self, lock_when_idle: impl Fn() -> bool) -> bool {
        let mut word = self.word.load(Ordering::Relaxed);
        loop {
            // A full count also goes to the lock, which reports exhaustion.
            if word & COORDINATED != 0 || word == ACTIVE || (word == 0 && lock_when_idle()) {
                return false;
            }
            match self.word.compare_exchange_weak(
                word,
                word + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(current) => word = current,
            }
        }
    }

    /// Retires one outer mutator.
    ///
    /// When the coordinator must see the exit, returns its locked state,
    /// having woken the waiters if no mutator remains active.
    pub(crate) fn exit(&self) -> Option<GateGuard<'_, S>> {
        let prior = self.word.fetch_sub(1, Ordering::Release);
        assert_ne!(prior & ACTIVE, 0, "active mutator count underflow");
        if prior & COORDINATED == 0 {
            return None;
        }
        // An exit cannot refuse to retire, so it recovers a poisoned lock.
        let guard = self.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.active_outer_mutators() == 0 {
            guard.notify();
        }
        Some(guard)
    }

    pub(crate) fn lock(&self) -> LockResult<GateGuard<'_, S>> {
        match self.locked.lock() {
            Ok(locked) => Ok(GateGuard {
                gate: self,
                locked: Some(locked),
            }),
            Err(poisoned) => Err(PoisonError::new(GateGuard {
                gate: self,
                locked: Some(poisoned.into_inner()),
            })),
        }
    }

    #[cfg(test)]
    pub(crate) fn notification_count(&self) -> usize {
        self.notifications
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The locked coordinator state.
pub(crate) struct GateGuard<'gate, S: GateState> {
    gate: &'gate AdmissionGate<S>,
    /// `None` only once [`GateGuard::wait`] has handed the lock to the
    /// condition variable.
    locked: Option<MutexGuard<'gate, Locked<S>>>,
}

impl<S: GateState> GateGuard<'_, S> {
    /// Reads the active outer-mutator count. In the same step it sets
    /// `COORDINATED`, which stays set until this guard unlocks.
    pub(crate) fn active_outer_mutators(&self) -> usize {
        self.gate.word.fetch_or(COORDINATED, Ordering::AcqRel) & ACTIVE
    }

    /// Admits one outer mutator under the lock.
    pub(crate) fn admit_outer_mutator(&mut self) {
        assert_ne!(
            self.active_outer_mutators(),
            ACTIVE,
            "active mutator count exhausted"
        );
        self.gate.word.fetch_add(1, Ordering::AcqRel);
    }

    /// Wakes every waiter.
    pub(crate) fn notify(&self) {
        #[cfg(test)]
        self.gate
            .notifications
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.locked().waiters != 0 {
            self.gate.changed.notify_all();
        }
    }

    /// Releases the lock until a notification, then retakes it.
    ///
    /// Exits take the lock while anyone waits, so a waiter whose predicate
    /// read the active count cannot miss the exit that empties it.
    pub(crate) fn wait(mut self) -> LockResult<Self> {
        let gate = self.gate;
        let mut locked = self.locked.take().expect("gate guard holds its lock");
        locked.waiters += 1;
        gate.word.fetch_or(COORDINATED, Ordering::AcqRel);
        drop(self);
        let (mut locked, poisoned) = match gate.changed.wait(locked) {
            Ok(locked) => (locked, false),
            Err(poisoned) => (poisoned.into_inner(), true),
        };
        locked.waiters -= 1;
        let guard = Self {
            gate,
            locked: Some(locked),
        };
        if poisoned {
            Err(PoisonError::new(guard))
        } else {
            Ok(guard)
        }
    }

    fn locked(&self) -> &Locked<S> {
        self.locked.as_ref().expect("gate guard holds its lock")
    }
}

impl<S: GateState> Deref for GateGuard<'_, S> {
    type Target = S;

    fn deref(&self) -> &S {
        &self.locked().state
    }
}

impl<S: GateState> DerefMut for GateGuard<'_, S> {
    fn deref_mut(&mut self) -> &mut S {
        &mut self
            .locked
            .as_mut()
            .expect("gate guard holds its lock")
            .state
    }
}

impl<S: GateState> Drop for GateGuard<'_, S> {
    fn drop(&mut self) {
        let Some(locked) = &self.locked else {
            return;
        };
        if locked.waiters != 0 || locked.state.coordinates_mutators() {
            self.gate.word.fetch_or(COORDINATED, Ordering::AcqRel);
        } else {
            self.gate.word.fetch_and(ACTIVE, Ordering::AcqRel);
        }
    }
}
