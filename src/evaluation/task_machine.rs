//! The machine a reflection task runs on (`reflection-tasks-are-machine-tasks`).
//!
//! A task that `.child.task` queued runs on its child machine, whose local
//! state persists from task to task and whose queue runs one task at a time.
//! Any other task is anonymous: its own root for the children it tasks,
//! keyed by its task id, unnamed in listings and never joined.

use std::fmt;
use std::sync::Arc;

use crate::core::Key;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

/// A child machine, by its path from the root, and the host that keeps its
/// state and its queue.
pub(crate) struct TaskMachine {
    path: Arc<[Key]>,
    host: Arc<dyn MachineHost>,
}

impl TaskMachine {
    pub(crate) fn new(path: Arc<[Key]>, host: Arc<dyn MachineHost>) -> Self {
        Self { path, host }
    }

    pub(crate) fn path(&self) -> &Arc<[Key]> {
        &self.path
    }

    pub(crate) fn host(&self) -> &Arc<dyn MachineHost> {
        &self.host
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
            .finish_non_exhaustive()
    }
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
    /// running and returns that task's operation, which the caller starts.
    fn start(&self, machine: &[Key]) -> Option<RuntimeValueRoot>;

    /// Ends the machine's running task with `outcome`, keeping `state` as its
    /// local state where given: takes the task from the queue, resolves its
    /// reply, and flags the machine on a failure. Returns the next task's
    /// operation where the machine goes on, which the caller starts.
    fn finish(
        &self,
        machine: &[Key],
        outcome: MachineTaskOutcome,
        state: Option<RuntimeValueRoot>,
    ) -> Option<RuntimeValueRoot>;
}
