//! Runtime-wide semantic observation epochs.
//!
//! Semantic observations advance independently of scheduler queue churn. The
//! coordinator publishes changes under runtime mutation admission, while this
//! state only owns the current epoch and the condition variable used by
//! external observers.

use std::num::NonZeroU64;
#[cfg(test)]
use std::sync::MutexGuard;
use std::sync::{Arc, Condvar, Mutex};

/// Runtime-wide semantic-state revision observed by retryable evaluation.
///
/// Epochs begin at one so `Option<RuntimeObservationEpoch>` can use the zero
/// niche for absence without increasing block representations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RuntimeObservationEpoch(NonZeroU64);

impl RuntimeObservationEpoch {
    pub(crate) fn from_raw(epoch: u64) -> Self {
        Self(NonZeroU64::new(epoch).expect("runtime observation epochs must be nonzero"))
    }

    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }

    #[cfg(test)]
    pub(super) fn advance_for_test(&mut self) {
        self.0 = NonZeroU64::new(
            self.get()
                .checked_add(1)
                .expect("test observation epoch should advance"),
        )
        .expect("advanced test observation epoch must remain nonzero");
    }
}

pub(crate) struct RuntimeObservationState {
    epoch: Mutex<RuntimeObservationEpoch>,
    changed: Condvar,
}

impl RuntimeObservationState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: Mutex::new(RuntimeObservationEpoch::from_raw(1)),
            changed: Condvar::new(),
        })
    }

    pub(crate) fn current(&self) -> RuntimeObservationEpoch {
        *self
            .epoch
            .lock()
            .expect("runtime observation mutex should not be poisoned")
    }

    pub(crate) fn advance(&self) -> RuntimeObservationEpoch {
        let mut epoch = self
            .epoch
            .lock()
            .expect("runtime observation mutex should not be poisoned");
        *epoch = RuntimeObservationEpoch::from_raw(
            epoch
                .get()
                .checked_add(1)
                .expect("runtime observation epochs exhausted"),
        );
        *epoch
    }

    pub(crate) fn notify_all(&self) {
        self.changed.notify_all();
    }

    pub(crate) fn wait_for_change(&self, observed: RuntimeObservationEpoch) {
        let mut epoch = self
            .epoch
            .lock()
            .expect("runtime observation mutex should not be poisoned");
        while *epoch == observed {
            epoch = self
                .changed
                .wait(epoch)
                .expect("runtime observation mutex should not be poisoned");
        }
    }

    #[cfg(test)]
    pub(super) fn lock_epoch_for_test(&self) -> MutexGuard<'_, RuntimeObservationEpoch> {
        self.epoch
            .lock()
            .expect("runtime observation mutex should not be poisoned")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    #[test]
    fn observation_epochs_are_nonzero_and_option_niche_optimized() {
        assert_eq!(
            std::mem::size_of::<Option<RuntimeObservationEpoch>>(),
            std::mem::size_of::<u64>()
        );

        let observations = RuntimeObservationState::new();
        assert_eq!(observations.current().get(), 1);
        assert_eq!(observations.advance().get(), 2);
    }

    #[test]
    fn observation_wait_returns_after_an_advance_is_published() {
        let observations = RuntimeObservationState::new();
        let observed = observations.current();
        let waiting = observations.clone();
        let (started_tx, started_rx) = mpsc::channel();

        let waiter = thread::spawn(move || {
            started_tx
                .send(())
                .expect("observation waiter should announce startup");
            waiting.wait_for_change(observed);
            waiting.current()
        });

        started_rx
            .recv()
            .expect("observation waiter should announce startup");
        let advanced = observations.advance();
        observations.notify_all();

        assert_eq!(
            waiter.join().expect("observation waiter should finish"),
            advanced
        );
    }
}
