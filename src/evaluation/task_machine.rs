//! The machine a reflection task runs on (`reflection-tasks-are-machine-tasks`).
//!
//! A task that `.child.task` queued runs on its child machine, whose local
//! state persists from task to task and whose queue runs one task at a time.
//! Any other task is anonymous: its own root for the children it tasks,
//! keyed by its task id, unnamed in listings and never joined.
//!
//! A task's item stays at the head of its machine's queue while it runs, and
//! the item's sequence in the queue names the run: an interrupt takes the
//! item, and the task, stopped, ends a run that is no longer the machine's,
//! so its end claims nothing and starts nothing. Only an interrupt stops a
//! task; a clear waits for the machines it clears to be idle or flagged.

use std::fmt;
use std::sync::Arc;

use super::EvaluationTaskHandle;
use crate::core::Key;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

/// A task's run on a child machine: the machine, by its path from the root,
/// the host that keeps its state and its queue, and the sequence of the
/// task's item in the queue.
pub(crate) struct TaskMachine {
    path: Arc<[Key]>,
    host: Arc<dyn MachineHost>,
    sequence: u64,
}

impl TaskMachine {
    pub(crate) fn new(path: Arc<[Key]>, host: Arc<dyn MachineHost>, sequence: u64) -> Self {
        Self {
            path,
            host,
            sequence,
        }
    }

    pub(crate) fn path(&self) -> &Arc<[Key]> {
        &self.path
    }

    pub(crate) fn host(&self) -> &Arc<dyn MachineHost> {
        &self.host
    }

    /// The sequence of the task's item in the machine's queue.
    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }

    /// The local state the machine's next task starts from.
    pub(crate) fn local_state(&self) -> Option<RuntimeValueRoot> {
        self.host.local_state(&self.path)
    }
}

impl fmt::Debug for TaskMachine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskMachine")
            .field("path", &self.path)
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

/// A task a machine's queue holds: its item's sequence, and its `{op,
/// reply}` record.
pub(crate) struct QueuedTask {
    pub(crate) sequence: u64,
    pub(crate) record: RuntimeValueRoot,
}

/// How a machine's task ended.
pub(crate) enum MachineTaskOutcome {
    /// Its result, which fulfills the task's reply.
    Complete(RuntimeValueRoot),
    /// Its failure, which breaks the reply and flags the machine.
    Failed(RuntimeFailureRoot),
}

/// The runtime's child machines, as the tasks on them need them: their
/// local state, and their queues of tasks, which the host keeps with its
/// transactional state. It never evaluates: a caller holds no mutator.
pub(crate) trait MachineHost: Send + Sync {
    /// The local state the machine's next task starts from, where it has
    /// one.
    fn local_state(&self, machine: &[Key]) -> Option<RuntimeValueRoot>;

    /// Where the machine is idle, unflagged and has a task queued, marks it
    /// running that task and returns it, which the caller starts.
    fn start(&self, machine: &[Key]) -> Option<QueuedTask>;

    /// Records `task` as the task of the machine's run `sequence`, which an
    /// interrupt stops. Where one stopped the run before the task launched,
    /// it cancels the task.
    fn attach(&self, machine: &[Key], sequence: u64, task: &EvaluationTaskHandle);

    /// Ends the machine's run `sequence` with `outcome`, keeping `state` as
    /// its local state where given: takes the task from the queue, resolves
    /// its reply, and flags the machine on a failure. A run an interrupt took
    /// the task from claims nothing. Returns the next task where
    /// the machine goes on, which the caller starts.
    fn finish(
        &self,
        machine: &[Key],
        sequence: u64,
        outcome: MachineTaskOutcome,
        state: Option<RuntimeValueRoot>,
    ) -> Option<QueuedTask>;

    /// Stops the machine's run `sequence`, whose task an interrupt took from
    /// the queue as it committed, cancelling its task: the state it leaves
    /// stays the machine's.
    fn interrupt(&self, machine: &[Key], sequence: u64);

    /// Empties the machine and its descendants, as a clear committed: their
    /// local state goes, and so does the state a task still winding down
    /// there, stopped or failed, leaves. It stops nothing: a clear waits for
    /// its machines to be idle or flagged.
    fn clear(&self, machine: &[Key]);
}
