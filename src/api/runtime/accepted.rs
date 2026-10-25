//! The promises machines accepted, as committed
//! (`obligations-are-accepted-promises`): by promise, each with the root on
//! its resolver's breaker that acceptance holds, so the promise is never
//! lost, and its acceptors, each a machine's path with the diagnostic
//! saying what it owes. Resolving a promise discharges it for every
//! acceptor, in the commit that resolves it; abandoning removes one
//! acceptor; clearing a machine removes it from all.
//!
//! A read of whether a promise is accepted observes it: the journal keeps
//! the version it read, and validation fails where it changed.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use rpds::RedBlackTreeMapSync;

use crate::core::{Key, ManagedResolverRoot, PromiseId, PromiseStatus};
use crate::runtime::RuntimeValueRoot;

/// A machine's path, with the diagnostic saying what it owes.
pub(in crate::api) type Acceptor = (Vec<Key>, RuntimeValueRoot);

/// One accepted promise.
#[derive(Clone)]
struct Acceptance {
    /// Held so that the promise is never lost while it is owed.
    _resolver: ManagedResolverRoot,
    acceptors: RedBlackTreeMapSync<Vec<Key>, RuntimeValueRoot>,
    version: u64,
}

/// The accepted promises, by promise, as committed.
#[derive(Clone)]
pub(in crate::api) struct AcceptedRecord {
    entries: RedBlackTreeMapSync<PromiseId, Acceptance>,
    /// The version the next change takes. An unaccepted promise has
    /// version zero.
    next: u64,
}

#[derive(Clone)]
enum AcceptEdit {
    Accept(Vec<Key>, ManagedResolverRoot, RuntimeValueRoot),
    Abandon(Vec<Key>, PromiseId),
    /// Drops the acceptances of the machine at the path and its
    /// descendants.
    ClearBelow(Vec<Key>),
}

impl AcceptedRecord {
    pub(in crate::api) fn new() -> Self {
        Self {
            entries: RedBlackTreeMapSync::new_sync(),
            next: 1,
        }
    }

    fn version(&self, promise: PromiseId) -> u64 {
        self.entries.get(&promise).map_or(0, |entry| entry.version)
    }

    /// Whether each acceptance `journal` read stands as it read it.
    pub(in crate::api) fn validate(&self, journal: &AcceptedJournal) -> bool {
        journal
            .reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .all(|(promise, version)| self.version(*promise) == *version)
    }

    /// Applies what `journal` changed, returning whether it changed
    /// anything. An acceptance of a promise already resolved, by a commit
    /// since it was staged, owes nothing and is dropped.
    pub(in crate::api) fn commit(&mut self, journal: &AcceptedJournal) -> bool {
        let mut changed = false;
        for edit in &journal.edits {
            changed |= match edit {
                AcceptEdit::Accept(machine, resolver, diagnostic) => {
                    self.accept(machine, resolver, diagnostic)
                }
                AcceptEdit::Abandon(machine, promise) => self.abandon(machine, *promise),
                AcceptEdit::ClearBelow(root) => self.remove_below(root),
            };
        }
        changed
    }

    fn accept(
        &mut self,
        machine: &[Key],
        resolver: &ManagedResolverRoot,
        diagnostic: &RuntimeValueRoot,
    ) -> bool {
        if resolver.status() != PromiseStatus::Pending {
            return false;
        }
        let version = self.bump();
        let mut entry = self
            .entries
            .get(&resolver.id())
            .cloned()
            .unwrap_or_else(|| Acceptance {
                _resolver: resolver.clone(),
                acceptors: RedBlackTreeMapSync::new_sync(),
                version,
            });
        entry
            .acceptors
            .insert_mut(machine.to_vec(), diagnostic.clone());
        entry.version = version;
        self.entries.insert_mut(resolver.id(), entry);
        true
    }

    fn abandon(&mut self, machine: &[Key], promise: PromiseId) -> bool {
        let Some(mut entry) = self.entries.get(&promise).cloned() else {
            return false;
        };
        if !entry.acceptors.remove_mut(machine) {
            return false;
        }
        if entry.acceptors.is_empty() {
            self.entries.remove_mut(&promise);
        } else {
            entry.version = self.bump();
            self.entries.insert_mut(promise, entry);
        }
        true
    }

    /// Removes the machine at `root` and its descendants from every
    /// acceptance, dropping those left with no acceptor, as clearing the
    /// machine does.
    fn remove_below(&mut self, root: &[Key]) -> bool {
        let touched: Vec<PromiseId> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.acceptors.keys().any(|path| path.starts_with(root)))
            .map(|(promise, _)| *promise)
            .collect();
        for promise in &touched {
            let mut entry = self
                .entries
                .get(promise)
                .cloned()
                .expect("a touched acceptance stays until it is changed");
            let below: Vec<Vec<Key>> = entry
                .acceptors
                .keys()
                .filter(|path| path.starts_with(root))
                .cloned()
                .collect();
            for path in below {
                entry.acceptors.remove_mut(&path);
            }
            if entry.acceptors.is_empty() {
                self.entries.remove_mut(promise);
            } else {
                entry.version = self.bump();
                self.entries.insert_mut(*promise, entry);
            }
        }
        !touched.is_empty()
    }

    /// Discharges each of `resolved`, which a commit just resolved, for
    /// every acceptor. Returns whether any was accepted.
    pub(in crate::api) fn discharge(
        &mut self,
        resolved: impl IntoIterator<Item = PromiseId>,
    ) -> bool {
        let mut changed = false;
        for promise in resolved {
            changed |= self.entries.remove_mut(&promise);
        }
        changed
    }

    /// Whether the machine at `machine` accepted a promise still pending.
    pub(in crate::api) fn owes(&self, machine: &[Key]) -> bool {
        self.entries
            .values()
            .any(|entry| entry.acceptors.contains_key(machine))
    }

    /// Whether the machine at `root` or one of its descendants accepted a
    /// promise still pending.
    pub(in crate::api) fn owes_below(&self, root: &[Key]) -> bool {
        self.entries
            .values()
            .any(|entry| entry.acceptors.keys().any(|path| path.starts_with(root)))
    }

    /// Each accepted promise, by ID, with its acceptors and what they owe,
    /// by path.
    pub(in crate::api) fn pending(&self) -> Vec<(PromiseId, Vec<Acceptor>)> {
        self.entries
            .iter()
            .map(|(promise, entry)| {
                (
                    *promise,
                    entry
                        .acceptors
                        .iter()
                        .map(|(path, diagnostic)| (path.clone(), diagnostic.clone()))
                        .collect(),
                )
            })
            .collect()
    }

    fn bump(&mut self) -> u64 {
        let version = self.next;
        self.next += 1;
        version
    }
}

/// One transaction's acceptances, abandonments and reads.
#[derive(Clone)]
pub(in crate::api) struct AcceptedJournal {
    snapshot: AcceptedRecord,
    edits: Vec<AcceptEdit>,
    /// The versions of the acceptances the transaction read, shared by
    /// every alternative forked from it.
    ///
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    reads: Arc<Mutex<BTreeMap<PromiseId, u64>>>,
}

impl AcceptedJournal {
    pub(in crate::api) fn new(record: &AcceptedRecord) -> Self {
        Self {
            snapshot: record.clone(),
            edits: Vec::new(),
            reads: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Accepts `resolver`'s promise for the machine at `machine`, owing what
    /// `diagnostic` says.
    pub(in crate::api) fn accept(
        &mut self,
        machine: Vec<Key>,
        resolver: ManagedResolverRoot,
        diagnostic: RuntimeValueRoot,
    ) {
        self.edits
            .push(AcceptEdit::Accept(machine, resolver, diagnostic));
    }

    /// Removes the machine at `machine` from `promise`'s acceptors, without
    /// resolving it.
    pub(in crate::api) fn abandon(&mut self, machine: Vec<Key>, promise: PromiseId) {
        self.edits.push(AcceptEdit::Abandon(machine, promise));
    }

    /// Abandons every acceptance of the machine at `machine` and its
    /// descendants.
    pub(in crate::api) fn clear_below(&mut self, machine: &[Key]) {
        self.edits.push(AcceptEdit::ClearBelow(machine.to_vec()));
    }

    /// Whether any machine accepted `promise`, with this journal's edits,
    /// observing it.
    pub(in crate::api) fn accepted(&self, promise: PromiseId) -> bool {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(promise)
            .or_insert_with(|| self.snapshot.version(promise));
        let mut acceptors: Vec<Vec<Key>> = self
            .snapshot
            .entries
            .get(&promise)
            .map(|entry| entry.acceptors.keys().cloned().collect())
            .unwrap_or_default();
        for edit in &self.edits {
            match edit {
                AcceptEdit::Accept(machine, resolver, _) if resolver.id() == promise => {
                    if !acceptors.contains(machine) {
                        acceptors.push(machine.clone());
                    }
                }
                AcceptEdit::Abandon(machine, abandoned) if *abandoned == promise => {
                    acceptors.retain(|path| path != machine);
                }
                AcceptEdit::ClearBelow(root) => acceptors.retain(|path| !path.starts_with(root)),
                AcceptEdit::Accept(..) | AcceptEdit::Abandon(..) => {}
            }
        }
        !acceptors.is_empty()
    }
}
