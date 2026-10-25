//! The runtime's child machines, as the tasks on them need them
//! (`reflection-tasks-are-machine-tasks`).
//!
//! A child machine's queue is its task input, a port, and its error flag
//! lies in the flag record, both in the transactional state, so a status
//! read is observed and a task queued within a cut reaches the machine when
//! the cut commits. Its local state, and whether a task runs on it, live
//! here: only its own tasks read or write them, one at a time.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, PoisonError};

use super::RuntimeSharedResources;
use super::events::{RuntimeEventJournal, TASKS, task_address};
use crate::core::{EvaluationFailure, Key, ManagedResolverRoot, RuntimeValueAccess, Value};
use crate::evaluation::{
    DeferredHalt, EvaluationTaskHandle, MachineHost, MachineTaskOutcome, QueuedTask,
};
use crate::reflection::{Resolution, StoreCommitResult, StoreJournal};
use crate::runtime::RuntimeValueRoot;

/// What the runtime keeps of each child machine beside its transactional
/// state.
#[derive(Default)]
pub(super) struct MachineCells {
    /// Leaf lock beside the transaction lock, which it may take inside:
    /// critical sections make only whole updates, so poison is recovered.
    /// A task is cancelled once the lock is released, since cancelling may
    /// end the task, and its end comes back here.
    cells: Mutex<HashMap<Vec<Key>, MachineCell>>,
    /// The halt actions of runs whose tasks stopped where no halt could run,
    /// as under a mutator, which run at the next safe point.
    ///
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    deferred: Mutex<Vec<DeferredHalt>>,
    /// The anonymous roots whose tasks ended and whose subtrees wait to be
    /// reclaimed.
    ///
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    ended_roots: Mutex<BTreeSet<Vec<Key>>>,
}

#[derive(Default)]
struct MachineCell {
    /// The local state its last task left.
    state: Option<RuntimeValueRoot>,
    /// The task that runs on it, started and not yet finished.
    run: Option<Run>,
}

/// A task's run on a machine, named by its item's sequence in the queue.
struct Run {
    sequence: u64,
    /// The task, once it launched.
    task: Option<EvaluationTaskHandle>,
    /// Whether an interrupt stopped the run.
    stopped: bool,
    /// Whether the state the task leaves is dropped: a clear came while
    /// the stopped or failed task wound down.
    discard: bool,
}

impl RuntimeSharedResources {
    /// The task the child machine at `machine` runs next, as committed: its
    /// queue's head, unless the machine is flagged.
    fn queued_head(&self, machine: &[Key]) -> Option<QueuedTask> {
        let state = self
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        if state.events.flags.is_set(machine) {
            return None;
        }
        let (sequence, record) = state.events.ports.head(&task_address(machine.to_vec()))?;
        Some(QueuedTask { sequence, record })
    }

    /// Reclaims the subtree of the anonymous root at `root` where the root
    /// ended and no task runs or waits there: drops its machines' cells and
    /// port state, those of flagged machines aside, which stay whole for
    /// settlement to report (`reflection-tasks-are-machine-tasks`).
    fn reclaim_if_idle(&self, root: &[Key]) {
        let ended = || {
            self.machines
                .ended_roots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        };
        if !ended().contains(root) {
            return;
        }
        let mut cells = self.cells();
        if cells
            .iter()
            .any(|(path, cell)| path.starts_with(root) && cell.run.is_some())
        {
            return;
        }
        let mut state = self
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        let flags = state.events.flags.clone();
        if state
            .events
            .ports
            .holds_items_below(root, &TASKS, |machine| !flags.is_set(machine))
        {
            return;
        }
        state
            .events
            .ports
            .remove_below(root, |machine| flags.is_set(machine));
        drop(state);
        cells.retain(|path, _| !path.starts_with(root) || flags.is_set(path));
        drop(cells);
        ended().remove(root);
    }

    /// The machines below anonymous roots that hold a cell or port state.
    #[cfg(test)]
    pub(in crate::api) fn anonymous_machines_for_test(&self) -> BTreeSet<Vec<Key>> {
        let mut machines: BTreeSet<Vec<Key>> = self
            .cells()
            .keys()
            .filter(|path| anonymous_root_of(path).is_some())
            .cloned()
            .collect();
        let (_, _, events) = self.transaction_snapshot();
        machines.extend(
            RuntimeEventJournal::new(events)
                .present_below_for_test(&[Key::atom_from_text("refl")])
                .into_iter()
                .filter(|path| anonymous_root_of(path).is_some()),
        );
        machines
    }

    fn cells(&self) -> std::sync::MutexGuard<'_, HashMap<Vec<Key>, MachineCell>> {
        self.machines
            .cells
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// The anonymous root `machine` lies below, where it lies below one.
fn anonymous_root_of(machine: &[Key]) -> Option<&[Key]> {
    (machine.len() >= 2
        && machine[0] == Key::atom_from_text("refl")
        && crate::reflection::is_anonymous_key(&machine[1]))
    .then(|| &machine[..2])
}

/// The resolver of the reply `task`, a queued task's record, holds.
fn task_reply(
    access: &RuntimeValueAccess<'_>,
    task: &RuntimeValueRoot,
) -> Option<ManagedResolverRoot> {
    let Value::Dict(task) = task.clone_core_with(access) else {
        return None;
    };
    task.get(&Key::atom_from_text("reply"))
        .and_then(|reply| access.resolver_root(reply))
}

/// Stages, in `store`, the break of each reply `tasks` hold, records a
/// control operation took from their queues, with `reason`: a reply already
/// resolved keeps its resolution.
pub(crate) fn break_replies(
    access: &RuntimeValueAccess<'_>,
    store: &mut StoreJournal,
    tasks: Vec<crate::api::Value>,
    reason: &RuntimeValueRoot,
) {
    for task in tasks {
        if let Some(resolver) = task_reply(access, &task.into_runtime_root()) {
            let _ = store.resolve(&resolver, Resolution::Break(reason.clone()));
        }
    }
}

/// The diagnostic an interrupt flags a machine with: its reason, as a
/// failure's emission.
pub(crate) fn reason_diagnostic(
    access: &RuntimeValueAccess<'_>,
    reason: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    let failure = EvaluationFailure::emission_in(access, reason.clone_core_with(access));
    access.root_runtime_value(crate::diagnostic::failure_diagnostic_in(access, &failure))
}

/// The diagnostic a control operation breaks a reply with: what happened
/// to the task's machine.
pub(crate) fn machine_diagnostic(access: &RuntimeValueAccess<'_>, text: &str) -> RuntimeValueRoot {
    access.root_runtime_value(crate::diagnostic::failure_diagnostic_in(
        access,
        &EvaluationFailure::message(text),
    ))
}

/// Clears the child machine at `machine` and its descendants within a
/// transaction, as a turn's clear does its machine: their queues, port
/// state, shares and flags (`clear_machines`), and their environment.
/// Returns the tasks it took from their queues.
pub(crate) fn clear_child_machines(
    store: &mut StoreJournal,
    events: &mut RuntimeEventJournal,
    machine: &[Key],
) -> Vec<crate::api::Value> {
    store.remove_environment(crate::api::turn::environment::environment_node(machine));
    events.clear_machines(machine)
}

impl MachineHost for RuntimeSharedResources {
    fn local_state(&self, machine: &[Key]) -> Option<RuntimeValueRoot> {
        self.cells()
            .get(machine)
            .and_then(|cell| cell.state.clone())
    }

    fn start(&self, machine: &[Key]) -> Option<QueuedTask> {
        let mut cells = self.cells();
        let cell = cells.entry(machine.to_vec()).or_default();
        if cell.run.is_some() {
            return None;
        }
        let head = self.queued_head(machine)?;
        cell.run = Some(Run {
            sequence: head.sequence,
            task: None,
            stopped: false,
            discard: false,
        });
        Some(head)
    }

    fn attach(&self, machine: &[Key], sequence: u64, task: &EvaluationTaskHandle) {
        let stopped = {
            let mut cells = self.cells();
            let Some(run) = cells
                .get_mut(machine)
                .and_then(|cell| cell.run.as_mut())
                .filter(|run| run.sequence == sequence)
            else {
                return;
            };
            run.task = Some(task.clone());
            run.stopped
        };
        if stopped {
            task.cancel();
        }
    }

    fn finish(
        &self,
        machine: &[Key],
        sequence: u64,
        outcome: MachineTaskOutcome,
        state: Option<RuntimeValueRoot>,
    ) -> Option<QueuedTask> {
        let completed = matches!(outcome, MachineTaskOutcome::Complete(_));
        let failed = matches!(outcome, MachineTaskOutcome::Failed(_));
        let resolution = self
            .values
            .core()
            .construct_runtime_value_root(|access| match &outcome {
                MachineTaskOutcome::Complete(value) => value.clone_core_with(access),
                MachineTaskOutcome::Failed(failure) => {
                    crate::diagnostic::failure_diagnostic_in(access, failure.as_failure())
                }
                MachineTaskOutcome::Exited => crate::diagnostic::failure_diagnostic_in(
                    access,
                    &EvaluationFailure::message("the task exited before it finished"),
                ),
            });
        let flag = failed.then(|| resolution.clone());
        let resolution = if completed {
            Resolution::Fulfill(resolution)
        } else {
            Resolution::Break(resolution)
        };
        // The task leaves the queue, its reply resolved, in one commit; a
        // failure flags the machine with it. Where an interrupt or a clear
        // took the task first, the run claims nothing.
        loop {
            let (_, store, events) = self.transaction_snapshot();
            let mut store = StoreJournal::new(store);
            let mut events = RuntimeEventJournal::new(events);
            let address = task_address(machine.to_vec());
            if events.ports.next_sequence(&address) != sequence {
                break;
            }
            let Some(mut claimed) = events.ports.claim(&address, 1) else {
                break;
            };
            let task = claimed
                .pop()
                .expect("a claim of one holds one")
                .into_runtime_root();
            let resolver = self
                .values
                .core()
                .with_boundary_access(|access| task_reply(&access, &task));
            if let Some(resolver) = resolver {
                // A reply already resolved keeps its resolution.
                let _ = store.resolve(&resolver, resolution.clone());
            }
            if let Some(diagnostic) = &flag {
                events.flags.set(machine.to_vec(), diagnostic.clone());
            }
            match self.try_commit_transaction(&store, &events) {
                StoreCommitResult::Conflict => continue,
                StoreCommitResult::Committed | StoreCommitResult::MissingVolume(_) => break,
            }
        }
        let mut cells = self.cells();
        let cell = cells.entry(machine.to_vec()).or_default();
        let run = cell.run.take_if(|run| run.sequence == sequence)?;
        if let Some(state) = state
            && !run.discard
        {
            cell.state = Some(state);
        }
        let next = self.queued_head(machine);
        if let Some(next) = &next {
            cell.run = Some(Run {
                sequence: next.sequence,
                task: None,
                stopped: false,
                discard: false,
            });
        }
        drop(cells);
        // The last run in an ended root's subtree may leave it idle.
        if next.is_none()
            && let Some(root) = anonymous_root_of(machine)
        {
            self.reclaim_if_idle(root);
        }
        next
    }

    fn interrupt(&self, machine: &[Key], sequence: u64) {
        let task = {
            let mut cells = self.cells();
            let Some(run) = cells
                .get_mut(machine)
                .and_then(|cell| cell.run.as_mut())
                .filter(|run| run.sequence == sequence)
            else {
                return;
            };
            run.stopped = true;
            run.task.clone()
        };
        if let Some(task) = task {
            task.cancel();
        }
    }

    fn root_ended(&self, root: &[Key]) {
        self.machines
            .ended_roots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(root.to_vec());
        self.reclaim_if_idle(root);
    }

    fn defer(&self, halt: DeferredHalt) {
        self.machines
            .deferred
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(halt);
    }

    fn run_deferred(&self) {
        let deferred = std::mem::take(
            &mut *self
                .machines
                .deferred
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        // A torn runtime commits nothing more.
        if self.transactions.state.is_poisoned() {
            return;
        }
        for halt in deferred {
            halt();
        }
    }

    fn clear(&self, machine: &[Key]) {
        let mut cells = self.cells();
        cells.retain(|path, cell| !path.starts_with(machine) || cell.run.is_some());
        for (_, cell) in cells
            .iter_mut()
            .filter(|(path, _)| path.starts_with(machine))
        {
            cell.state = None;
            if let Some(run) = &mut cell.run {
                run.discard = true;
            }
        }
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
        assert_eq!(task(first.record), Some(1));
        assert!(resources.start(&machine).is_none(), "one task at a time");
        let second = resources
            .finish(
                &machine,
                first.sequence,
                MachineTaskOutcome::Complete(unit()),
                None,
            )
            .expect("finishing hands over the next task");
        assert_eq!(task(second.record), Some(2));
        assert!(resources.start(&machine).is_none(), "the second runs");
        assert!(
            resources
                .finish(
                    &machine,
                    second.sequence,
                    MachineTaskOutcome::Complete(unit()),
                    None
                )
                .is_none(),
            "the queue is empty"
        );
        assert!(resources.start(&machine).is_none(), "nothing is queued");
    }

    #[test]
    fn a_run_stopped_before_its_task_attaches_cancels_the_task() {
        // An interrupt may commit between a run's start and its task's
        // launch: the task, attached once launched, is cancelled then.
        use crate::evaluation::{EvaluationMachinePoll, EvaluationTaskMachine, EvaluationWaitPoll};

        struct CompleteTask;
        impl EvaluationTaskMachine for CompleteTask {
            fn poll(
                &mut self,
                context: &mut crate::evaluation::EvaluationPollContext,
                _step_budget: &mut crate::evaluation::EvaluationStepBudget,
            ) -> EvaluationMachinePoll {
                EvaluationMachinePoll::Complete(context.root_unit())
            }
        }

        // A dormant runtime runs nothing until pumped.
        let runtime = EvaluationRuntime::new(0).expect("the runtime should open");
        let assembler = crate::api::Assembler::builder()
            .evaluation_runtime(runtime.clone())
            .build()
            .expect("the assembler should build");
        let resources = runtime.state.shared_resources.clone();
        let machine = vec![Key::atom_from_text("refl"), Key::atom_from_text("a")];
        let (_, store, events) = resources.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(events);
        events.queue_task(machine.clone(), runtime.values().integer(1));
        assert!(matches!(
            resources.try_commit_transaction(&StoreJournal::new(store), &events),
            StoreCommitResult::Committed
        ));

        let run = resources.start(&machine).expect("the queue holds a task");
        resources.interrupt(&machine, run.sequence);
        let task = assembler
            .eval_context()
            .schedule_task(|_| Ok(Box::new(CompleteTask)))
            .expect("the task should schedule");
        resources.attach(&machine, run.sequence, &task);
        runtime.pump_until_stable();
        assert!(matches!(
            assembler.eval_context().poll_reflection_task(&task),
            EvaluationWaitPoll::Cancelled
        ));
    }

    #[test]
    fn a_clear_drops_the_state_a_winding_down_task_leaves() {
        // A clear stops nothing, but a stopped or failed task may still be
        // winding down when it commits: the state that task leaves is
        // dropped, where a run the clear did not meet keeps its own.
        let runtime = EvaluationRuntime::new(0).expect("the runtime should open");
        let resources = runtime.state.shared_resources.clone();
        let values = runtime.values();
        let cleared = vec![Key::atom_from_text("refl"), Key::atom_from_text("a")];
        let kept = vec![Key::atom_from_text("refl"), Key::atom_from_text("b")];
        let (_, store, events) = resources.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(events);
        for machine in [&cleared, &kept] {
            events.queue_task(machine.clone(), values.integer(1));
        }
        assert!(matches!(
            resources.try_commit_transaction(&StoreJournal::new(store), &events),
            StoreCommitResult::Committed
        ));
        let root = |value: i64| values.integer(value).into_runtime_root();
        for machine in [&cleared, &kept] {
            let run = resources.start(machine).expect("the queue holds a task");
            if machine == &cleared {
                resources.clear(machine);
            }
            resources.finish(
                machine,
                run.sequence,
                MachineTaskOutcome::Complete(root(0)),
                Some(root(7)),
            );
        }
        assert!(resources.local_state(&cleared).is_none());
        assert!(resources.local_state(&kept).is_some());
    }

    #[test]
    fn a_halt_under_a_mutator_waits_for_the_next_safe_point() {
        // A run may halt where its thread holds a mutator, under which no
        // halt may commit: the host keeps the halt action, and death-notice
        // processing, a safe point, runs it. A run halts once.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let runtime = EvaluationRuntime::new(0).expect("the runtime should open");
        let resources = runtime.state.shared_resources.clone();
        let halts = Arc::new(AtomicUsize::new(0));
        let machine = crate::evaluation::TaskMachine::new(
            Arc::from(vec![Key::atom_from_text("refl"), Key::atom_from_text("a")]),
            resources.clone(),
            0,
            Box::new({
                let halts = halts.clone();
                move |_| {
                    halts.fetch_add(1, Ordering::SeqCst);
                }
            }),
        );
        runtime
            .values()
            .core()
            .with_boundary_access(|_| machine.halt(crate::evaluation::RunHalt::Stopped));
        assert_eq!(halts.load(Ordering::SeqCst), 0, "no halt under a mutator");
        resources.process_death_notices();
        assert_eq!(halts.load(Ordering::SeqCst), 1);
        machine.halt(crate::evaluation::RunHalt::Stopped);
        resources.process_death_notices();
        assert_eq!(halts.load(Ordering::SeqCst), 1, "a run halts once");
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
