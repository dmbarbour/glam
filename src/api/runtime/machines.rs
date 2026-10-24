//! The runtime's child machines, as the tasks on them need them
//! (`reflection-tasks-are-machine-tasks`).
//!
//! A child machine's queue is its task input, a port, and its error flag
//! lies in the flag record, both in the transactional state, so a status
//! read is observed and a task queued within a cut reaches the machine when
//! the cut commits. Its local state, and whether a task runs on it, live
//! here: only its own tasks read or write them, one at a time.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use super::RuntimeSharedResources;
use super::events::{RuntimeEventJournal, task_address};
use crate::core::{Key, Value};
use crate::evaluation::{MachineHost, MachineTaskOutcome};
use crate::reflection::{Resolution, StoreCommitResult, StoreJournal};
use crate::runtime::RuntimeValueRoot;

/// What the runtime keeps of each child machine beside its transactional
/// state.
#[derive(Default)]
pub(super) struct MachineCells {
    /// Leaf lock beside the transaction lock, which it may take inside:
    /// critical sections make only whole updates, so poison is recovered.
    cells: Mutex<HashMap<Vec<Key>, MachineCell>>,
}

#[derive(Default)]
struct MachineCell {
    /// The local state its last task left.
    state: Option<RuntimeValueRoot>,
    /// Whether a task runs on it, started and not yet finished.
    running: bool,
}

impl RuntimeSharedResources {
    /// The task the child machine at `machine` runs next, as committed: its
    /// queue's head, unless the machine is flagged.
    fn queued_head(&self, machine: &[Key]) -> Option<RuntimeValueRoot> {
        let state = self
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        if state.events.flags.is_set(machine) {
            return None;
        }
        state.events.ports.head(&task_address(machine.to_vec()))
    }
}

impl MachineHost for RuntimeSharedResources {
    fn local_state(&self, machine: &[Key]) -> Option<RuntimeValueRoot> {
        self.machines
            .cells
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(machine)
            .and_then(|cell| cell.state.clone())
    }

    fn start(&self, machine: &[Key]) -> Option<RuntimeValueRoot> {
        let mut cells = self
            .machines
            .cells
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let cell = cells.entry(machine.to_vec()).or_default();
        if cell.running {
            return None;
        }
        let head = self.queued_head(machine)?;
        cell.running = true;
        Some(head)
    }

    fn finish(
        &self,
        machine: &[Key],
        outcome: MachineTaskOutcome,
        state: Option<RuntimeValueRoot>,
    ) -> Option<RuntimeValueRoot> {
        let failed = matches!(outcome, MachineTaskOutcome::Failed(_));
        let resolution = self
            .values
            .core()
            .construct_runtime_value_root(|access| match &outcome {
                MachineTaskOutcome::Complete(value) => value.clone_core_with(access),
                MachineTaskOutcome::Failed(failure) => {
                    crate::diagnostic::failure_diagnostic_in(access, failure.as_failure())
                }
            });
        let resolution = if failed {
            Resolution::Break(resolution)
        } else {
            Resolution::Fulfill(resolution)
        };
        // The task leaves the queue, its reply resolved, in one commit; a
        // failure flags the machine with it.
        loop {
            let (_, store, events) = self.transaction_snapshot();
            let mut store = StoreJournal::new(store);
            let mut events = RuntimeEventJournal::new(events);
            let Some(mut claimed) = events.ports.claim(&task_address(machine.to_vec()), 1) else {
                break;
            };
            let task = claimed
                .pop()
                .expect("a claim of one holds one")
                .into_runtime_root();
            let resolver = self.values.core().with_boundary_access(|access| {
                let Value::Dict(task) = task.clone_core_with(&access) else {
                    return None;
                };
                task.get(&Key::atom_from_text("reply"))
                    .and_then(|reply| access.resolver_root(reply))
            });
            if let Some(resolver) = resolver {
                // A reply already resolved keeps its resolution.
                let _ = store.resolve(&resolver, resolution.clone());
            }
            if failed {
                events.flags.set(machine.to_vec());
            }
            match self.try_commit_transaction(&store, &events) {
                StoreCommitResult::Conflict => continue,
                StoreCommitResult::Committed | StoreCommitResult::MissingVolume(_) => break,
            }
        }
        let mut cells = self
            .machines
            .cells
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let cell = cells.entry(machine.to_vec()).or_default();
        if let Some(state) = state {
            cell.state = Some(state);
        }
        let next = self.queued_head(machine);
        cell.running = next.is_some();
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EvaluationRuntime;

    #[test]
    fn a_machine_runs_one_task_at_a_time_and_takes_its_tasks_in_order() {
        // `reflection-tasks-are-machine-tasks`: a running machine starts
        // nothing more, whatever wakes it; finishing a task hands over the
        // next, and an empty queue leaves the machine idle.
        let runtime = EvaluationRuntime::new(0).expect("the runtime should open");
        let resources = runtime.state.shared_resources.clone();
        let values = runtime.values();
        let machine = vec![Key::atom_from_text("refl"), Key::atom_from_text("a")];
        let (_, store, events) = resources.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(events);
        for item in [1, 2] {
            events.queue_task(machine.clone(), values.integer(item));
        }
        assert!(matches!(
            resources.try_commit_transaction(&StoreJournal::new(store), &events),
            StoreCommitResult::Committed
        ));
        let task = |root: RuntimeValueRoot| {
            values
                .core()
                .with_boundary_access(|access| match root.clone_core_with(&access) {
                    Value::Number(number) => number.to_i64_if_integer(),
                    _ => None,
                })
        };
        let unit = || {
            values
                .core()
                .construct_runtime_value_root(|access| access.unit())
        };

        let first = resources.start(&machine).expect("the queue holds a task");
        assert_eq!(task(first), Some(1));
        assert!(resources.start(&machine).is_none(), "one task at a time");
        let second = resources
            .finish(&machine, MachineTaskOutcome::Complete(unit()), None)
            .expect("finishing hands over the next task");
        assert_eq!(task(second), Some(2));
        assert!(resources.start(&machine).is_none(), "the second runs");
        assert!(
            resources
                .finish(&machine, MachineTaskOutcome::Complete(unit()), None)
                .is_none(),
            "the queue is empty"
        );
        assert!(resources.start(&machine).is_none(), "nothing is queued");
    }

    #[test]
    fn listings_leave_anonymous_roots_out() {
        // An anonymous root, and the machines below it, are unnamed in
        // listings, where a named machine's task input shows it present.
        let runtime = EvaluationRuntime::new(0).expect("the runtime should open");
        let resources = runtime.state.shared_resources.clone();
        let values = runtime.values();
        let refl = vec![Key::atom_from_text("refl")];
        let mut anonymous = crate::reflection::anonymous_root(
            crate::evaluation::EvaluationTaskId::from_nonzero(std::num::NonZeroU64::MIN),
        );
        anonymous.push(Key::atom_from_text("child"));
        let mut named = refl.clone();
        named.push(Key::atom_from_text("named"));
        let (_, store, events) = resources.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(events);
        events.queue_task(anonymous, values.integer(1));
        events.queue_task(named.clone(), values.integer(2));
        assert!(matches!(
            resources.try_commit_transaction(&StoreJournal::new(store), &events),
            StoreCommitResult::Committed
        ));
        let (_, _, events) = resources.transaction_snapshot();
        assert_eq!(
            RuntimeEventJournal::new(events).present_below(&refl),
            [named].into_iter().collect()
        );
    }
}
