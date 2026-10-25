//! The local state of child machines, as committed
//! (`reflection-tasks-are-machine-tasks`): a task's run takes its machine's
//! state as it starts and writes the state it leaves in the commit that ends
//! it, and a call reads and writes it within its caller's transaction, so
//! the two commit or abort together.
//!
//! A reader of a machine's state observes it: the journal keeps the version
//! of each state it read, and validation fails where one changed, so a call
//! conflicts with a run that ended on its child meanwhile.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use rpds::RedBlackTreeMapSync;

use crate::core::Key;
use crate::runtime::RuntimeValueRoot;

/// A machine's committed state, and the version that names it.
#[derive(Clone)]
struct Versioned {
    state: RuntimeValueRoot,
    version: u64,
}

type States = RedBlackTreeMapSync<Vec<Key>, Versioned>;

/// The child machines' states, by path, as committed.
#[derive(Clone)]
pub(in crate::api) struct StateRecord {
    states: States,
    /// The version the next state written takes. An absent state has
    /// version zero.
    next: u64,
}

#[derive(Clone)]
enum StateEdit {
    Set(Vec<Key>, RuntimeValueRoot),
    /// Drops the states of the machine at the path and its descendants.
    ClearBelow(Vec<Key>),
}

impl StateRecord {
    pub(in crate::api) fn new() -> Self {
        Self {
            states: RedBlackTreeMapSync::new_sync(),
            next: 1,
        }
    }

    /// The state of the machine at `machine`, as committed.
    pub(in crate::api) fn get(&self, machine: &[Key]) -> Option<RuntimeValueRoot> {
        self.states.get(machine).map(|entry| entry.state.clone())
    }

    fn version(&self, machine: &[Key]) -> u64 {
        self.states.get(machine).map_or(0, |entry| entry.version)
    }

    /// Whether each state `journal` read stands as it read it.
    pub(in crate::api) fn validate(&self, journal: &StateJournal) -> bool {
        journal
            .reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .all(|(machine, version)| self.version(machine) == *version)
    }

    /// Applies what `journal` changed, returning whether it changed
    /// anything.
    pub(in crate::api) fn commit(&mut self, journal: &StateJournal) -> bool {
        for edit in &journal.edits {
            match edit {
                StateEdit::Set(machine, state) => {
                    let version = self.next;
                    self.next += 1;
                    self.states.insert_mut(
                        machine.clone(),
                        Versioned {
                            state: state.clone(),
                            version,
                        },
                    );
                }
                StateEdit::ClearBelow(machine) => self.remove_below(machine, |_| false),
            }
        }
        !journal.edits.is_empty()
    }

    /// Drops the states of the machine at `root` and its descendants, those
    /// of machines that `keep` aside.
    pub(in crate::api) fn remove_below(&mut self, root: &[Key], keep: impl Fn(&[Key]) -> bool) {
        let dropped: Vec<Vec<Key>> = self
            .states
            .keys()
            .filter(|machine| machine.starts_with(root) && !keep(machine))
            .cloned()
            .collect();
        for machine in dropped {
            self.states.remove_mut(&machine);
        }
    }

    /// The machines with a state.
    #[cfg(test)]
    pub(in crate::api) fn machines_for_test(&self) -> Vec<Vec<Key>> {
        self.states.keys().cloned().collect()
    }
}

/// One transaction's state reads and writes, which its view shows as made.
#[derive(Clone)]
pub(in crate::api) struct StateJournal {
    snapshot: StateRecord,
    view: BTreeMap<Vec<Key>, Option<RuntimeValueRoot>>,
    edits: Vec<StateEdit>,
    /// The versions of the states the transaction read, shared by every
    /// alternative forked from it.
    ///
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    reads: Arc<Mutex<BTreeMap<Vec<Key>, u64>>>,
}

impl StateJournal {
    pub(in crate::api) fn new(record: &StateRecord) -> Self {
        Self {
            snapshot: record.clone(),
            view: BTreeMap::new(),
            edits: Vec::new(),
            reads: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// The state of the machine at `machine`, with this journal's writes,
    /// observing it.
    pub(in crate::api) fn read(&mut self, machine: &[Key]) -> Option<RuntimeValueRoot> {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(machine.to_vec())
            .or_insert_with(|| self.snapshot.version(machine));
        match self.view.get(machine) {
            Some(state) => state.clone(),
            None if self.cleared(machine) => None,
            None => self.snapshot.get(machine),
        }
    }

    /// Writes `state` as the state of the machine at `machine`.
    pub(in crate::api) fn set(&mut self, machine: Vec<Key>, state: RuntimeValueRoot) {
        self.view.insert(machine.clone(), Some(state.clone()));
        self.edits.push(StateEdit::Set(machine, state));
    }

    /// Drops the states of the machine at `machine` and its descendants.
    pub(in crate::api) fn clear_below(&mut self, machine: &[Key]) {
        self.view.retain(|path, _| !path.starts_with(machine));
        self.edits.push(StateEdit::ClearBelow(machine.to_vec()));
    }

    /// Whether a clear in this journal dropped the state at `machine`
    /// after its last write here.
    fn cleared(&self, machine: &[Key]) -> bool {
        self.edits.iter().rev().find_map(|edit| match edit {
            StateEdit::Set(path, _) if path == machine => Some(false),
            StateEdit::ClearBelow(below) if machine.starts_with(below) => Some(true),
            _ => None,
        }) == Some(true)
    }
}
