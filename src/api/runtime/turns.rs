//! Interrupts of turns (`quiescence-is-analyzed`): a host's interrupt
//! reaches the runs that turns have in progress at or below the machine it
//! names, and a turn learns of it at its waits.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::core::Key;
use crate::runtime::RuntimeValueRoot;

/// The runs that turns have in progress, and the interrupts that may still
/// reach one.
#[derive(Default)]
pub(in crate::api) struct TurnRuns {
    /// Leaf lock: critical sections make only whole updates, so poison is
    /// recovered.
    state: Mutex<TurnRunState>,
}

#[derive(Default)]
struct TurnRunState {
    /// The serial that the next run or interrupt takes: an interrupt reaches
    /// the runs that entered before it.
    next: u64,
    /// The runs in progress, by serial, with their machines' paths.
    runs: BTreeMap<u64, Vec<Key>>,
    /// The interrupts posted that reach a run in progress.
    interrupts: Vec<TurnInterrupt>,
}

struct TurnInterrupt {
    serial: u64,
    machine: Vec<Key>,
    reason: RuntimeValueRoot,
}

impl TurnInterrupt {
    /// Whether this interrupt reaches the run that entered at `serial` on
    /// the machine at `machine`: one in progress, when it was posted, at or
    /// below the machine it names.
    fn reaches(&self, serial: u64, machine: &[Key]) -> bool {
        serial < self.serial && machine.starts_with(&self.machine)
    }
}

impl TurnRunState {
    fn take_serial(&mut self) -> u64 {
        let serial = self.next;
        self.next += 1;
        serial
    }

    /// Drops the interrupts that reach no run in progress, returning them,
    /// to drop outside the lock.
    fn prune(&mut self) -> Vec<TurnInterrupt> {
        let (kept, pruned) =
            std::mem::take(&mut self.interrupts)
                .into_iter()
                .partition(|interrupt| {
                    self.runs
                        .iter()
                        .any(|(serial, machine)| interrupt.reaches(*serial, machine))
                });
        self.interrupts = kept;
        pruned
    }
}

/// A turn's run on a machine, in progress until it is dropped.
pub(in crate::api) struct TurnRun {
    serial: u64,
    machine: Vec<Key>,
    runs: Arc<TurnRuns>,
}

impl TurnRuns {
    fn state(&self) -> std::sync::MutexGuard<'_, TurnRunState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Enters a run on the machine at `machine`.
    pub(in crate::api) fn enter(self: &Arc<Self>, machine: Vec<Key>) -> TurnRun {
        let mut state = self.state();
        let serial = state.take_serial();
        state.runs.insert(serial, machine.clone());
        TurnRun {
            serial,
            machine,
            runs: self.clone(),
        }
    }

    /// Posts an interrupt, with `reason`, of the runs in progress on the
    /// machine at `machine` and its descendants. Returns whether it reaches
    /// one. The caller publishes a change, which wakes the turns waiting,
    /// to learn of it.
    pub(in crate::api) fn interrupt(&self, machine: &[Key], reason: RuntimeValueRoot) -> bool {
        let mut state = self.state();
        let serial = state.take_serial();
        let interrupt = TurnInterrupt {
            serial,
            machine: machine.to_vec(),
            reason,
        };
        let reaches = state
            .runs
            .iter()
            .any(|(serial, machine)| interrupt.reaches(*serial, machine));
        if reaches {
            state.interrupts.push(interrupt);
        }
        reaches
    }

    /// The reason of the first interrupt posted that reaches one of `runs`,
    /// a turn's runs, from the outermost in, if any.
    pub(in crate::api) fn interrupted(&self, runs: &[Arc<TurnRun>]) -> Option<RuntimeValueRoot> {
        self.state()
            .interrupts
            .iter()
            .find(|interrupt| {
                runs.iter()
                    .any(|run| interrupt.reaches(run.serial, &run.machine))
            })
            .map(|interrupt| interrupt.reason.clone())
    }
}

impl Drop for TurnRun {
    fn drop(&mut self) {
        let pruned = {
            let mut state = self.runs.state();
            state.runs.remove(&self.serial);
            state.prune()
        };
        drop(pruned);
    }
}

impl super::RuntimeSharedResources {
    /// Enters a turn's run on the machine at `machine`.
    pub(in crate::api) fn enter_turn(&self, machine: Vec<Key>) -> TurnRun {
        self.turns.enter(machine)
    }

    /// Interrupts the runs that turns have in progress at or below the
    /// machine at `machine`, with `reason`, publishing a change where it
    /// reaches one, which wakes the turns waiting, to learn of it.
    pub(in crate::api) fn interrupt_turns(&self, machine: &[Key], reason: RuntimeValueRoot) {
        let mutation = self.mutation_guard();
        if self.turns.interrupt(machine, reason) {
            super::publish_runtime_observation(self, mutation);
        }
    }

    /// The reason of an interrupt that reaches one of `runs`, a turn's
    /// runs, if any.
    pub(in crate::api) fn turn_interrupted(
        &self,
        runs: &[Arc<TurnRun>],
    ) -> Option<RuntimeValueRoot> {
        self.turns.interrupted(runs)
    }
}
