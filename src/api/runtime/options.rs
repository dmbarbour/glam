//! A host's options on the runtime's machines (`quiescence-is-analyzed`):
//! the operations it takes after an analysis, or at any other time, on the
//! machines that the analysis's entries name by path.

use std::sync::Arc;

use super::{EvaluationRuntime, RuntimeEventJournal, break_replies, reason_diagnostic};
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
    /// task, as the task's failure would flag it. Idle machines are not, and
    /// a Rust-scheduled run is its host's to stop. Returns the machines it
    /// flagged, by path.
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
        flagged.sort();
        flagged.dedup();
        Ok(flagged.into_iter().map(MachinePath::from).collect())
    }
}
