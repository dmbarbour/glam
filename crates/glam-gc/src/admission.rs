//! Outer-mutator admission and the collector's coordinator state.
//!
//! [`AdmissionGate`] is the coordinator lock, with a lock-free path for
//! ordinary outer entries and exits. [`MutatorCoordinator`] is the state it
//! guards: the admission phase and collection epochs. Every phase change is a
//! method of the locked guard, here, so outer mutators bypass the lock only in
//! `Ordinary` admission, and an election to collect sees every active mutator.

mod gate;

/// The primitives `gate` is built on; the Loom models substitute Loom's.
mod sync {
    pub(super) use std::sync::atomic::{AtomicUsize, Ordering};
    pub(super) use std::sync::{Condvar, Mutex, MutexGuard};
}

use std::num::NonZeroU64;

use crate::heap::CollectionReport;
pub(crate) use gate::AdmissionGate;
use gate::{GateGuard, GateState};

/// The locked coordinator.
pub(crate) type CoordinatorGuard<'gate> = GateGuard<'gate, MutatorCoordinator>;

#[derive(Debug, Default)]
pub(crate) struct MutatorCoordinator {
    phase: AdmissionPhase,
    active_collection: Option<CollectionEpoch>,
    completed_collection_epoch: u64,
    latest_collection_report: Option<CollectionReport>,
    #[cfg(test)]
    pub(crate) blocked_outer_mutators: usize,
    #[cfg(test)]
    pub(crate) blocked_collection_waiters: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AdmissionPhase {
    #[default]
    Ordinary,
    Exclusive,
    Finalizing,
    Poisoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct CollectionEpoch(NonZeroU64);

impl CollectionEpoch {
    fn after(completed: u64) -> Self {
        let next = completed
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .expect("collection epoch exhausted");
        Self(next)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) const fn non_zero(self) -> NonZeroU64 {
        self.0
    }
}

impl GateState for MutatorCoordinator {
    fn coordinates_mutators(&self) -> bool {
        self.phase != AdmissionPhase::Ordinary
    }
}

impl MutatorCoordinator {
    pub(crate) fn phase(&self) -> AdmissionPhase {
        self.phase
    }

    pub(crate) fn active_collection(&self) -> Option<CollectionEpoch> {
        self.active_collection
    }

    pub(crate) fn completed_collection_epoch(&self) -> u64 {
        self.completed_collection_epoch
    }

    pub(crate) fn latest_collection_report(&self) -> Option<CollectionReport> {
        self.latest_collection_report
    }

    /// The epoch a synchronous collection waits for, and whether it must
    /// request that epoch rather than join the active collection.
    pub(crate) fn request_synchronous_collection(&self) -> (CollectionEpoch, bool) {
        match self.phase {
            AdmissionPhase::Ordinary => (
                CollectionEpoch::after(self.completed_collection_epoch),
                true,
            ),
            AdmissionPhase::Exclusive | AdmissionPhase::Finalizing => (
                self.active_collection
                    .expect("active collection phase must have an epoch"),
                false,
            ),
            AdmissionPhase::Poisoned => {
                unreachable!("poisoned heaps reject collection before requesting an epoch")
            }
        }
    }
}

impl CoordinatorGuard<'_> {
    /// Elects the caller to collect when a collection is requested and no
    /// outer mutator is active in `Ordinary` admission.
    pub(crate) fn elect_idle_collection(
        &mut self,
        collection_requested: bool,
    ) -> Option<CollectionEpoch> {
        if self.phase != AdmissionPhase::Ordinary
            || !collection_requested
            || self.active_outer_mutators() != 0
        {
            return None;
        }
        let epoch = CollectionEpoch::after(self.completed_collection_epoch);
        self.active_collection = Some(epoch);
        self.phase = AdmissionPhase::Exclusive;
        self.notify();
        Some(epoch)
    }

    /// Turns exclusive authority directly into the collector's own finalizer
    /// mutator, leaving no gap in which neither holds.
    pub(crate) fn begin_finalizing(&mut self, epoch: CollectionEpoch) {
        assert_eq!(self.phase, AdmissionPhase::Exclusive);
        assert_eq!(self.active_outer_mutators(), 0);
        assert_eq!(self.active_collection, Some(epoch));
        self.phase = AdmissionPhase::Finalizing;
        self.admit_outer_mutator();
        self.notify();
    }

    /// Publishes a completed collection and restores ordinary admission.
    pub(crate) fn complete_collection(&mut self, epoch: CollectionEpoch, report: CollectionReport) {
        assert_eq!(self.phase, AdmissionPhase::Finalizing);
        assert_eq!(self.active_collection, Some(epoch));
        self.latest_collection_report = Some(report);
        self.completed_collection_epoch = epoch.get();
        self.active_collection = None;
        self.phase = AdmissionPhase::Ordinary;
        self.notify();
    }

    /// Restores ordinary admission after a failed attempt, if `epoch` still
    /// holds collection authority.
    pub(crate) fn abandon_collection(&mut self, epoch: CollectionEpoch) {
        if self.active_collection == Some(epoch) {
            self.active_collection = None;
            self.phase = AdmissionPhase::Ordinary;
        }
        self.notify();
    }

    /// Refuses all further admission.
    pub(crate) fn poison(&mut self, epoch: CollectionEpoch) {
        if self.active_collection == Some(epoch) {
            self.active_collection = None;
        }
        self.phase = AdmissionPhase::Poisoned;
        self.notify();
    }

    #[cfg(test)]
    pub(crate) fn begin_synthetic_exclusive(&mut self) {
        assert_eq!(self.phase, AdmissionPhase::Ordinary);
        assert_eq!(self.active_outer_mutators(), 0);
        self.phase = AdmissionPhase::Exclusive;
        self.notify();
    }

    #[cfg(test)]
    pub(crate) fn end_synthetic_exclusive(&mut self) {
        assert_eq!(self.phase, AdmissionPhase::Exclusive);
        assert_eq!(self.active_outer_mutators(), 0);
        self.phase = AdmissionPhase::Ordinary;
        self.notify();
    }
}
