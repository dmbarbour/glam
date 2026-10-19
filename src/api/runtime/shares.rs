//! Which machines have a share of their environment
//! (`machines-inherit-an-environment`).
//!
//! The shares lie in the reflection store's environment volume, whose
//! structure is built lazily, so this record beside them says which
//! machines have one without evaluating anything: for composing an
//! environment from the shares there are, and for counting a machine with a
//! share as present. It changes with the share, in one transaction, and a
//! reader of an environment observes each share's place in the store, set or
//! not, so the store's validation covers the record too.

use std::collections::BTreeSet;

use rpds::RedBlackTreeSetSync;

use crate::core::Key;

type Bound = RedBlackTreeSetSync<Vec<Key>>;

/// The machines, by path, that have a share, as committed.
#[derive(Clone)]
pub(in crate::api) struct ShareRecord {
    bound: Bound,
}

#[derive(Clone)]
enum ShareEdit {
    Set(Vec<Key>),
    Clear(Vec<Key>),
    /// Clears the share of a machine and its descendants'.
    ClearBelow(Vec<Key>),
}

fn apply(bound: &mut Bound, edit: &ShareEdit) {
    match edit {
        ShareEdit::Set(machine) => bound.insert_mut(machine.clone()),
        ShareEdit::Clear(machine) => {
            bound.remove_mut(machine);
        }
        ShareEdit::ClearBelow(machine) => {
            for below in at_or_below(bound, machine) {
                bound.remove_mut(&below);
            }
        }
    }
}

/// The machines with a share at or below `machine`.
fn at_or_below(bound: &Bound, machine: &[Key]) -> BTreeSet<Vec<Key>> {
    bound
        .iter()
        .filter(|path| path.starts_with(machine))
        .cloned()
        .collect()
}

impl ShareRecord {
    pub(in crate::api) fn new() -> Self {
        Self {
            bound: RedBlackTreeSetSync::new_sync(),
        }
    }

    /// Applies what `journal` changed, returning whether it changed
    /// anything.
    pub(in crate::api) fn commit(&mut self, journal: &ShareJournal) -> bool {
        for edit in &journal.edits {
            apply(&mut self.bound, edit);
        }
        !journal.edits.is_empty()
    }
}

/// One transaction's changes to the record, which its view shows as made.
#[derive(Clone)]
pub(in crate::api) struct ShareJournal {
    view: Bound,
    edits: Vec<ShareEdit>,
    /// Where a fork's branch began among the edits, for its merge.
    branch_base: usize,
}

impl ShareJournal {
    pub(in crate::api) fn new(record: &ShareRecord) -> Self {
        Self {
            view: record.bound.clone(),
            edits: Vec::new(),
            branch_base: 0,
        }
    }

    /// Whether the machine at `machine` has a share.
    pub(in crate::api) fn is_set(&self, machine: &[Key]) -> bool {
        self.view.contains(machine)
    }

    /// The machines below `machine`, not it, that have a share.
    pub(in crate::api) fn set_below(&self, machine: &[Key]) -> BTreeSet<Vec<Key>> {
        let mut below = at_or_below(&self.view, machine);
        below.remove(machine);
        below
    }

    pub(in crate::api) fn set(&mut self, machine: Vec<Key>) {
        self.edit(ShareEdit::Set(machine));
    }

    pub(in crate::api) fn clear(&mut self, machine: Vec<Key>) {
        self.edit(ShareEdit::Clear(machine));
    }

    pub(in crate::api) fn clear_below(&mut self, machine: Vec<Key>) {
        self.edit(ShareEdit::ClearBelow(machine));
    }

    fn edit(&mut self, edit: ShareEdit) {
        apply(&mut self.view, &edit);
        self.edits.push(edit);
    }

    /// A branch of this journal, for a fork within its transaction.
    pub(in crate::api) fn branch(&self) -> Self {
        Self {
            branch_base: self.edits.len(),
            ..self.clone()
        }
    }

    /// Takes in what `branch` changed since it began.
    pub(in crate::api) fn merge(&mut self, branch: &ShareJournal) {
        for edit in &branch.edits[branch.branch_base..] {
            self.edit(edit.clone());
        }
    }
}
