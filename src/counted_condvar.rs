//! Condition variables whose notifications cost nothing without a waiter.
//!
//! The standard library's futex condvar makes a `FUTEX_WAKE` syscall on every
//! notification, waiter or not. Glam's publication paths notify far more
//! often than anyone waits: once per scheduler mutation, runtime transition,
//! or net disturbance, while a single-threaded evaluation never waits at all.
//! `CountedCondvar` counts its waiters and skips a notification when there
//! are none.
//!
//! **Protocol.** As with any condvar, a notifier changes the awaited state
//! while holding the waiter's mutex, then notifies, during or after that
//! critical section. A waiter registers under the mutex before it sleeps.
//! A notifier that finds no waiter therefore knows that any later waiter
//! will see the new state before it sleeps. Every glam condvar is a
//! `CountedCondvar`; a test rejects the standard one elsewhere.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, LockResult, MutexGuard, WaitTimeoutResult};
use std::time::Duration;

#[derive(Debug, Default)]
pub(crate) struct CountedCondvar {
    condvar: Condvar,
    /// Threads inside a wait, registered under the waited mutex.
    waiters: AtomicUsize,
}

impl CountedCondvar {
    pub(crate) const fn new() -> Self {
        Self {
            condvar: Condvar::new(),
            waiters: AtomicUsize::new(0),
        }
    }

    /// Runs one wait with this thread counted as a waiter. The count rises
    /// while the caller still holds the mutex and falls after the wait has
    /// reacquired it, so it covers every moment the thread may sleep.
    fn counted<R>(&self, wait: impl FnOnce(&Condvar) -> R) -> R {
        self.waiters.fetch_add(1, Ordering::SeqCst);
        let woken = wait(&self.condvar);
        self.waiters.fetch_sub(1, Ordering::SeqCst);
        woken
    }

    pub(crate) fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> LockResult<MutexGuard<'a, T>> {
        self.counted(|condvar| condvar.wait(guard))
    }

    #[cfg(test)]
    pub(crate) fn wait_while<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        condition: impl FnMut(&mut T) -> bool,
    ) -> LockResult<MutexGuard<'a, T>> {
        self.counted(|condvar| condvar.wait_while(guard, condition))
    }

    #[cfg(test)]
    pub(crate) fn wait_timeout<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        timeout: Duration,
    ) -> LockResult<(MutexGuard<'a, T>, WaitTimeoutResult)> {
        self.counted(|condvar| condvar.wait_timeout(guard, timeout))
    }

    pub(crate) fn wait_timeout_while<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        timeout: Duration,
        condition: impl FnMut(&mut T) -> bool,
    ) -> LockResult<(MutexGuard<'a, T>, WaitTimeoutResult)> {
        self.counted(|condvar| condvar.wait_timeout_while(guard, timeout, condition))
    }

    pub(crate) fn notify_all(&self) {
        if self.waiters.load(Ordering::SeqCst) != 0 {
            self.condvar.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn a_waiter_registered_before_a_publication_is_woken() {
        let state = Arc::new((Mutex::new(false), CountedCondvar::new()));
        let waiter = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                let (ready, changed) = &*state;
                let guard = changed
                    .wait_while(ready.lock().unwrap(), |ready| !*ready)
                    .unwrap();
                assert!(*guard);
            })
        };
        while state.1.waiters.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        *state.0.lock().unwrap() = true;
        state.1.notify_all();
        waiter.join().unwrap();
        assert_eq!(state.1.waiters.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_timed_out_wait_unregisters() {
        let ready = Mutex::new(false);
        let changed = CountedCondvar::new();
        let (_guard, timeout) = changed
            .wait_timeout(ready.lock().unwrap(), Duration::from_millis(1))
            .unwrap();
        assert!(timeout.timed_out());
        assert_eq!(changed.waiters.load(Ordering::SeqCst), 0);
    }

    /// Every condvar in this crate counts its waiters; a standard one would
    /// make a syscall on every idle notification again.
    #[test]
    fn standard_condvars_are_used_only_here() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let this_file = root.join(file!());
        let mut pending = vec![root.join("src")];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).expect("source directories are readable") {
                let path = entry.expect("source entries are readable").path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs")
                    && path != this_file
                {
                    let source = std::fs::read_to_string(&path).expect("sources are readable");
                    // Any `Condvar` not spelled `CountedCondvar` is the
                    // standard one, imported or named in full.
                    let uses_std = source
                        .match_indices("Condvar")
                        .any(|(at, _)| !source[..at].ends_with("Counted"));
                    assert!(
                        !uses_std,
                        "{} uses the standard condvar; use crate::counted_condvar::CountedCondvar",
                        path.display()
                    );
                }
            }
        }
    }
}
