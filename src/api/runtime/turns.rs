//! The runs that turns have in progress (`quiescence-is-analyzed`): a
//! host's interrupt reaches those at or below the machine it names, which
//! learn of it at their waits, and an analysis reads what each thread's
//! innermost run does, to tell a turn waiting idle from one in progress.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::ThreadId;
use std::time::Duration;

use super::RuntimeEventJournal;
use crate::core::Key;
use crate::counted_condvar::CountedCondvar;
use crate::reflection::StoreJournal;
use crate::runtime::RuntimeValueRoot;

/// The runs that turns have in progress, and the interrupts that may still
/// reach one.
#[derive(Default)]
pub(in crate::api) struct TurnRuns {
    /// Leaf lock: critical sections make only whole updates, so poison is
    /// recovered.
    state: Mutex<TurnRunState>,
    /// Notified as a run enters, leaves or changes what it does.
    changed: CountedCondvar,
}

#[derive(Default)]
struct TurnRunState {
    /// The serial that the next run or interrupt takes: an interrupt reaches
    /// the runs that entered before it.
    next: u64,
    /// The runs in progress, by serial.
    runs: BTreeMap<u64, RunRecord>,
    /// The interrupts posted that reach a run in progress.
    interrupts: Vec<TurnInterrupt>,
    /// Advanced as a run enters, leaves or changes what it does.
    version: u64,
}

struct RunRecord {
    machine: Vec<Key>,
    /// The thread the run is on, once it has started: a fork's run enters
    /// before its thread does.
    thread: Option<ThreadId>,
    activity: TurnActivity,
}

/// What a run does.
#[derive(Clone)]
pub(in crate::api) enum TurnActivity {
    Running,
    /// Waiting for what these journals observed to change: idle while they
    /// still validate.
    Waiting(Arc<(StoreJournal, RuntimeEventJournal)>),
    /// Waiting for evaluation to progress, or for anything to change since
    /// this observation epoch.
    AwaitingEvaluation(u64),
    /// Joining its forks, whose own runs say what is in progress.
    Joining,
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
                        .any(|(serial, run)| interrupt.reaches(*serial, &run.machine))
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

/// Returns a run to running once what it did is over.
pub(in crate::api) struct TurnActivityGuard<'a>(&'a TurnRun);

impl Drop for TurnActivityGuard<'_> {
    fn drop(&mut self) {
        self.0.runs.update(|state| {
            if let Some(run) = state.runs.get_mut(&self.0.serial) {
                run.activity = TurnActivity::Running;
            }
        });
    }
}

impl TurnRun {
    /// Starts the run on the calling thread: a fork's, as its thread starts.
    pub(in crate::api) fn start(&self) {
        let thread = std::thread::current().id();
        self.runs.update(|state| {
            if let Some(run) = state.runs.get_mut(&self.serial) {
                run.thread = Some(thread);
            }
        });
    }

    /// Records that the run does `activity` until the guard drops, when it
    /// runs again.
    pub(in crate::api) fn doing(&self, activity: TurnActivity) -> TurnActivityGuard<'_> {
        self.runs.update(|state| {
            if let Some(run) = state.runs.get_mut(&self.serial) {
                run.activity = activity;
            }
        });
        TurnActivityGuard(self)
    }
}

impl TurnRuns {
    fn state(&self) -> std::sync::MutexGuard<'_, TurnRunState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Applies `change`, a change in the runs, advancing the version and
    /// waking those waiting on it.
    fn update<T>(&self, change: impl FnOnce(&mut TurnRunState) -> T) -> T {
        let mut state = self.state();
        let result = change(&mut state);
        state.version += 1;
        drop(state);
        self.changed.notify_all();
        result
    }

    /// Enters a run on the machine at `machine`, on the calling thread, or,
    /// for a fork, on its thread once it starts.
    pub(in crate::api) fn enter(self: &Arc<Self>, machine: Vec<Key>, fork: bool) -> TurnRun {
        let thread = (!fork).then(|| std::thread::current().id());
        let serial = self.update(|state| {
            let serial = state.take_serial();
            state.runs.insert(
                serial,
                RunRecord {
                    machine: machine.clone(),
                    thread,
                    activity: TurnActivity::Running,
                },
            );
            serial
        });
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
            .any(|(serial, run)| interrupt.reaches(*serial, &run.machine));
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

    /// The version, and what the innermost run on each thread but
    /// `observer`'s does, with its machine's path: a fork's run that has
    /// not started yet counts as running.
    pub(in crate::api) fn activities(
        &self,
        observer: ThreadId,
    ) -> (u64, Vec<(Vec<Key>, TurnActivity)>) {
        let state = self.state();
        let mut innermost: HashMap<ThreadId, &RunRecord> = HashMap::new();
        let mut unstarted = Vec::new();
        for run in state.runs.values() {
            match run.thread {
                Some(thread) if thread == observer => {}
                Some(thread) => {
                    // Runs nested on a thread enter after those they are
                    // within, and the runs go in serial order.
                    innermost.insert(thread, run);
                }
                None => unstarted.push((run.machine.clone(), TurnActivity::Running)),
            }
        }
        let activities = innermost
            .into_values()
            .map(|run| (run.machine.clone(), run.activity.clone()))
            .chain(unstarted)
            .collect();
        (state.version, activities)
    }

    /// Waits until the version passes `version`, or `timeout` passes.
    pub(in crate::api) fn wait_for_change(&self, version: u64, timeout: Duration) {
        let state = self.state();
        let _ = self
            .changed
            .wait_timeout_while(state, timeout, |state| state.version == version)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

impl Drop for TurnRun {
    fn drop(&mut self) {
        let pruned = self.runs.update(|state| {
            state.runs.remove(&self.serial);
            state.prune()
        });
        drop(pruned);
    }
}

impl super::RuntimeSharedResources {
    /// Enters a turn's run on the machine at `machine`, on the calling
    /// thread, or, for a fork, on its thread once it starts.
    pub(in crate::api) fn enter_turn(&self, machine: Vec<Key>, fork: bool) -> TurnRun {
        self.turns.enter(machine, fork)
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

    /// The version of the turns' runs, and the machines where a turn is in
    /// progress, on any thread but `observer`'s: a run that runs, or that
    /// waits where what it observed has changed since, or evaluation waits
    /// where the observation epoch has passed. A run joining its forks
    /// defers to them.
    pub(in crate::api) fn turns_in_progress(&self, observer: ThreadId) -> (u64, Vec<Vec<Key>>) {
        let (version, activities) = self.turns.activities(observer);
        let epoch = self.observations.current().get();
        let busy = activities
            .into_iter()
            .filter(|(_, activity)| match activity {
                TurnActivity::Running => true,
                TurnActivity::Joining => false,
                TurnActivity::Waiting(journals) => {
                    let (journal, events) = &**journals;
                    !matches!(
                        self.validate_transaction(journal, events).0,
                        crate::reflection::StoreCommitResult::Committed
                    )
                }
                TurnActivity::AwaitingEvaluation(generation) => *generation != epoch,
            })
            .map(|(machine, _)| machine)
            .collect();
        (version, busy)
    }

    /// Waits until a turn's run enters, leaves or changes what it does
    /// since `version`, or `timeout` passes.
    pub(in crate::api) fn wait_for_turns(&self, version: u64, timeout: Duration) {
        self.turns.wait_for_change(version, timeout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EvaluationRuntime;
    use crate::api::runtime::ports::PortAddress;
    use crate::reflection::StoreCommitResult;

    #[test]
    fn an_analysis_reads_each_thread_s_innermost_run_but_the_observer_s() {
        // `quiescence-is-analyzed`: a run that runs is in progress; one
        // waiting where what it observed holds is idle, and in progress
        // once that changes; one awaiting evaluation is idle until the
        // epoch passes; one joining defers to its forks; a fork's run that
        // has not started runs; and the observer's own thread is left out.
        let runtime = EvaluationRuntime::new(0).expect("the runtime should open");
        let resources = runtime.state.shared_resources.clone();
        let values = runtime.values();
        let path = |keys: &[&str]| keys.iter().copied().map(Key::atom_from_text).collect();
        let observer = std::thread::spawn(|| std::thread::current().id())
            .join()
            .expect("the observer's thread ends");
        let busy = || {
            let mut busy = resources.turns_in_progress(observer).1;
            busy.sort();
            busy
        };
        let input = PortAddress {
            machine: path(&["a", "b"]),
            port: Key::atom_from_text("in"),
        };

        let _outer = resources.enter_turn(path(&["a"]), false);
        assert_eq!(busy(), [path(&["a"])], "a run runs");
        let inner = resources.enter_turn(path(&["a", "b"]), false);
        assert_eq!(busy(), [path(&["a", "b"])], "the innermost run says");

        let (_, store, events) = resources.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(events);
        assert_eq!(events.ports.available(&input), 0);
        let waiting = inner.doing(TurnActivity::Waiting(Arc::new((
            StoreJournal::new(store),
            events,
        ))));
        assert!(busy().is_empty(), "it waits where what it observed holds");
        let (_, store, events) = resources.transaction_snapshot();
        let mut write = RuntimeEventJournal::new(events);
        write.ports.append(input, values.integer(1));
        assert!(matches!(
            resources.try_commit_transaction(&StoreJournal::new(store), &write),
            StoreCommitResult::Committed
        ));
        assert_eq!(busy(), [path(&["a", "b"])], "what it observed changed");
        drop(waiting);

        let epoch = resources.observations.current().get();
        let awaiting = inner.doing(TurnActivity::AwaitingEvaluation(epoch));
        assert!(busy().is_empty(), "nothing changed since its epoch");
        drop(awaiting);

        let joining = inner.doing(TurnActivity::Joining);
        assert!(busy().is_empty(), "joining defers to its forks");
        let _fork = resources.enter_turn(path(&["a", "b", "f"]), true);
        assert_eq!(busy(), [path(&["a", "b", "f"])], "an unstarted fork runs");
        drop(joining);
        assert_eq!(
            resources.turns_in_progress(std::thread::current().id()).1,
            [path(&["a", "b", "f"])],
            "the observer's own runs are left out"
        );
    }
}
