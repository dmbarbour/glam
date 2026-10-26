//! A host's options on the runtime's machines (`quiescence-is-analyzed`):
//! the operations it takes after an analysis, or at any other time, on the
//! machines that the analysis's entries name by path.

use std::sync::Arc;

use super::{
    EvaluationRuntime, RuntimeEventJournal, break_replies, clear_child_machines,
    machine_diagnostic, reason_diagnostic,
};
use crate::api::{Error, MachinePath, Value};
use crate::core::{EvaluationFailure, Key};
use crate::evaluation::MachineHost;
use crate::reflection::{StoreCommitResult, StoreJournal};

impl EvaluationRuntime {
    /// Interrupts the work on the machine at `machine` and its descendants,
    /// transitively, as `.child.interrupt` does a child's: stops each run
    /// there, breaking its task's reply with `reason`, and the annotation
    /// task of each anonymous root there. Each machine whose work it halts
    /// is flagged with `reason`: the machine of each run it stops, each
    /// child that run waits within by call, and the root of each annotation
    /// task, as the task's failure would flag it. Idle machines are not.
    /// Returns the machines it flagged, by path.
    ///
    /// It reaches turns too, those with a run in progress there when it is
    /// posted, and what they run within those runs: each fails its next
    /// wait, a cut waiting for a change, a blocking read, or an effect it
    /// runs, between quanta, with an [`crate::ErrorKind::Interrupted`] error
    /// whose diagnostic is `reason`. A turn's machine is in error only where
    /// the turn ends with that error, so none is among those returned.
    ///
    /// A run is stopped as it waits; one running a quantum stops at the
    /// quantum's end, flagging only its own machine, since nothing of the
    /// quantum's calls commits.
    pub fn interrupt(
        &self,
        machine: &MachinePath,
        reason: Value,
    ) -> Result<Vec<MachinePath>, Error> {
        let resources = &self.state.shared_resources;
        let reason = resources.root_value(reason)?;
        let (diagnostic, failure) = resources.values.core().with_boundary_access(|access| {
            let failure = EvaluationFailure::emission_in(&access, reason.clone_core_with(&access));
            (
                reason_diagnostic(&access, &reason),
                access.root_runtime_failure(Arc::new(failure)),
            )
        });
        let path = machine.keys();
        let roots: Vec<_> = self
            .state
            .work
            .annotation_tasks()
            .into_iter()
            .filter(|(task, _)| crate::reflection::anonymous_root(*task).starts_with(path))
            .collect();
        let (stopped, mut flagged) = loop {
            let (_, store, events) = resources.transaction_snapshot();
            let mut store = StoreJournal::new(store);
            let mut events = RuntimeEventJournal::new(events);
            let stopped = events.interrupt_tasks(path, &diagnostic, |machine, sequence| {
                resources.run_calls(machine, sequence)
            });
            let mut flagged: Vec<Vec<Key>> = Vec::new();
            for run in &stopped {
                flagged.push(run.machine.clone());
                flagged.extend(run.callees.iter().map(|callee| callee.to_vec()));
            }
            for (_, work) in &roots {
                for callee in self.state.work.waiting_calls(*work) {
                    events.flags.set(callee.to_vec(), diagnostic.clone());
                    flagged.push(callee.to_vec());
                }
            }
            let tasks = stopped.iter().map(|run| run.task.clone()).collect();
            resources
                .values
                .core()
                .with_boundary_access(|access| break_replies(&access, &mut store, tasks, &reason));
            match resources.try_commit_transaction(&store, &events) {
                StoreCommitResult::Conflict => continue,
                StoreCommitResult::Committed => break (stopped, flagged),
                StoreCommitResult::MissingVolume(volume) => {
                    return Err(Error::new(format!(
                        "reflection volume {} is unavailable",
                        volume.get()
                    )));
                }
            }
        };
        for run in &stopped {
            resources.interrupt(&run.machine, run.sequence);
        }
        for (task, work) in roots {
            if self.state.work.interrupt_root(work, failure.clone()) {
                flagged.push(crate::reflection::anonymous_root(task));
            }
        }
        resources.interrupt_turns(path, reason);
        flagged.sort();
        flagged.dedup();
        Ok(flagged.into_iter().map(MachinePath::from).collect())
    }

    /// Clears the machine at `machine` and its subtree, as `.child.clear`
    /// does a child's, whether in error or not, under an ended root or a
    /// live one: their flags, local states, ports, shares and environments,
    /// what they accepted, abandoned, their queues, whose tasks never ran,
    /// their replies broken, and the failure of each anonymous root there.
    ///
    /// It stops no work: it fails, changing nothing, where a task runs or
    /// waits in the subtree, which a host interrupts first. A call into the
    /// subtree from a run outside it leaves it clearable: the caller's cut
    /// observed what it called, so it conflicts with the clear and runs
    /// again over what the clear leaves.
    pub fn clear(&self, machine: &MachinePath) -> Result<(), Error> {
        let resources = &self.state.shared_resources;
        let path = machine.keys();
        let busy = || {
            Error::new(format!(
                "a task runs or waits at or below machine {path:?}; interrupt it first"
            ))
        };
        if self
            .state
            .work
            .annotation_tasks()
            .iter()
            .any(|(task, _)| crate::reflection::anonymous_root(*task).starts_with(path))
        {
            return Err(busy());
        }
        loop {
            let (_, store, events) = resources.transaction_snapshot();
            let mut store = StoreJournal::new(store);
            let mut events = RuntimeEventJournal::new(events);
            if events.machines_busy(path) {
                return Err(busy());
            }
            let cleared = clear_child_machines(&mut store, &mut events, path);
            resources.values.core().with_boundary_access(|access| {
                let reason = machine_diagnostic(
                    &access,
                    "the task's machine was cleared before the task ran",
                );
                break_replies(&access, &mut store, cleared, &reason);
            });
            match resources.try_commit_transaction(&store, &events) {
                StoreCommitResult::Conflict => continue,
                StoreCommitResult::Committed => break,
                StoreCommitResult::MissingVolume(volume) => {
                    return Err(Error::new(format!(
                        "reflection volume {} is unavailable",
                        volume.get()
                    )));
                }
            }
        }
        resources.clear(path);
        self.state
            .work
            .clear_failed_roots(|task| crate::reflection::anonymous_root(task).starts_with(path));
        // A root that ended, its subtree cleared or its last debtor gone, is
        // forgotten.
        resources.reclaim_ended_roots();
        Ok(())
    }
}
