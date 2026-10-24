//! Which child machines are flagged: a task that failed on them, or was
//! interrupted, left them in error until their parent recovers them
//! (`reflection-tasks-are-machine-tasks`).
//!
//! A reader of a machine's status observes whether it is flagged, so a cut
//! awaiting a status runs again when the flag changes: the journal keeps the
//! machines whose flags it read, and validation fails where one changed.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use rpds::RedBlackTreeSetSync;

use crate::core::Key;

type Flagged = RedBlackTreeSetSync<Vec<Key>>;

/// The flagged machines, by path, as committed.
#[derive(Clone)]
pub(in crate::api) struct FlagRecord {
    flagged: Flagged,
}

#[derive(Clone)]
#[allow(dead_code)]
enum FlagEdit {
    Set(Vec<Key>),
    Clear(Vec<Key>),
}

fn apply(flagged: &mut Flagged, edit: &FlagEdit) {
    match edit {
        FlagEdit::Set(machine) => flagged.insert_mut(machine.clone()),
        FlagEdit::Clear(machine) => {
            flagged.remove_mut(machine);
        }
    }
}

impl FlagRecord {
    pub(in crate::api) fn new() -> Self {
        Self {
            flagged: RedBlackTreeSetSync::new_sync(),
        }
    }

    /// Whether the machine at `machine` is flagged, as committed.
    pub(in crate::api) fn is_set(&self, machine: &[Key]) -> bool {
        self.flagged.contains(machine)
    }

    /// Whether each flag `journal` read stands as it read it.
    pub(in crate::api) fn validate(&self, journal: &FlagJournal) -> bool {
        journal
            .reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .all(|machine| self.is_set(machine) == journal.snapshot.is_set(machine))
    }

    /// Applies what `journal` changed, returning whether it changed
    /// anything.
    pub(in crate::api) fn commit(&mut self, journal: &FlagJournal) -> bool {
        for edit in &journal.edits {
            apply(&mut self.flagged, edit);
        }
        !journal.edits.is_empty()
    }
}

/// One transaction's flag reads and changes, which its view shows as made.
#[derive(Clone)]
pub(in crate::api) struct FlagJournal {
    snapshot: FlagRecord,
    view: Flagged,
    edits: Vec<FlagEdit>,
    /// The machines whose flags the transaction read, shared by every
    /// alternative forked from it.
    ///
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    reads: Arc<Mutex<BTreeSet<Vec<Key>>>>,
}

impl FlagJournal {
    pub(in crate::api) fn new(record: &FlagRecord) -> Self {
        Self {
            view: record.flagged.clone(),
            snapshot: record.clone(),
            edits: Vec::new(),
            reads: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    /// Whether the machine at `machine` is flagged, observing it.
    pub(in crate::api) fn read(&mut self, machine: &[Key]) -> bool {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(machine.to_vec());
        self.view.contains(machine)
    }

    pub(in crate::api) fn set(&mut self, machine: Vec<Key>) {
        self.edit(FlagEdit::Set(machine));
    }

    // Recovery clears a flag (`child-task-control`).
    #[allow(dead_code)]
    pub(in crate::api) fn clear(&mut self, machine: Vec<Key>) {
        self.edit(FlagEdit::Clear(machine));
    }

    fn edit(&mut self, edit: FlagEdit) {
        apply(&mut self.view, &edit);
        self.edits.push(edit);
    }
}
