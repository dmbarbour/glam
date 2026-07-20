//! Journaled shared state for reflection tasks.
//!
//! Conflict-analysis strategies summarize reads only. The store retains exact
//! write paths so strategy selection cannot change write or rebase semantics.

use std::collections::{BTreeMap, BTreeSet, hash_map::RandomState};
use std::hash::BuildHasher;
use std::sync::Arc;

use crate::api::Value as PublicValue;
use crate::core::{Atom, Builtin, Dict, Key, List, Value};

/// A hierarchical address in the shared reflection heap.
///
/// Core keys remain an implementation detail, but paths can be retained in
/// custom indexes and compared for exact or ancestor relationships.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConflictPath(Arc<[Key]>);

impl ConflictPath {
    pub fn root() -> Self {
        Self(Arc::from([]))
    }

    pub fn depth(&self) -> usize {
        self.0.len()
    }

    pub fn is_prefix_of(&self, other: &Self) -> bool {
        other.0.starts_with(&self.0)
    }

    pub fn overlaps(&self, other: &Self) -> bool {
        self.is_prefix_of(other) || other.is_prefix_of(self)
    }

    fn from_keys(keys: Vec<Key>) -> Self {
        Self(Arc::from(keys))
    }

    fn prefixes(&self) -> impl Iterator<Item = Self> + '_ {
        (0..=self.0.len()).map(|length| Self(Arc::from(&self.0[..length])))
    }

    fn keys(&self) -> &[Key] {
        &self.0
    }
}

impl std::fmt::Debug for ConflictPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ConflictPath")
            .field(&self.0)
            .finish()
    }
}

/// Creates the read index used by one optimistic transaction.
pub trait ConflictAnalysisStrategy: Send + Sync {
    fn begin(&self) -> Box<dyn ConflictObservationIndex>;

    /// A stable descriptive name for diagnostics and configuration displays.
    fn name(&self) -> &'static str;
}

/// A cloneable summary of paths observed by one transaction branch.
pub trait ConflictObservationIndex: Send + Sync {
    fn clone_box(&self) -> Box<dyn ConflictObservationIndex>;
    fn observe(&mut self, path: &ConflictPath);
    fn may_conflict(&self, changed: &ConflictPath) -> bool;
}

impl Clone for Box<dyn ConflictObservationIndex> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Exact path-overlap analysis. This is the reference implementation.
#[derive(Debug, Default)]
pub struct ExactConflictAnalysis;

impl ConflictAnalysisStrategy for ExactConflictAnalysis {
    fn begin(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(ExactObservationIndex::default())
    }

    fn name(&self) -> &'static str {
        "exact"
    }
}

#[derive(Clone, Default)]
struct ExactObservationIndex {
    paths: BTreeSet<ConflictPath>,
}

impl ConflictObservationIndex for ExactObservationIndex {
    fn clone_box(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(self.clone())
    }

    fn observe(&mut self, path: &ConflictPath) {
        if self.paths.iter().any(|seen| seen.is_prefix_of(path)) {
            return;
        }
        self.paths.retain(|seen| !path.is_prefix_of(seen));
        self.paths.insert(path.clone());
    }

    fn may_conflict(&self, changed: &ConflictPath) -> bool {
        self.paths.iter().any(|read| read.overlaps(changed))
    }
}

/// Conservative fingerprint analysis. Hash collisions cause retries, never
/// missed conflicts.
#[derive(Debug, Default)]
pub struct FingerprintConflictAnalysis;

impl ConflictAnalysisStrategy for FingerprintConflictAnalysis {
    fn begin(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(FingerprintObservationIndex {
            hash_builder: RandomState::new(),
            complete_reads: BTreeSet::new(),
            read_prefixes: BTreeSet::new(),
        })
    }

    fn name(&self) -> &'static str {
        "fingerprint"
    }
}

#[derive(Clone)]
struct FingerprintObservationIndex {
    hash_builder: RandomState,
    complete_reads: BTreeSet<u64>,
    read_prefixes: BTreeSet<u64>,
}

impl FingerprintObservationIndex {
    fn fingerprint(&self, path: &ConflictPath) -> u64 {
        self.hash_builder.hash_one(path)
    }
}

impl ConflictObservationIndex for FingerprintObservationIndex {
    fn clone_box(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(self.clone())
    }

    fn observe(&mut self, path: &ConflictPath) {
        self.complete_reads.insert(self.fingerprint(path));
        for prefix in path.prefixes() {
            self.read_prefixes.insert(self.fingerprint(&prefix));
        }
    }

    fn may_conflict(&self, changed: &ConflictPath) -> bool {
        self.read_prefixes.contains(&self.fingerprint(changed))
            || changed
                .prefixes()
                .any(|prefix| self.complete_reads.contains(&self.fingerprint(&prefix)))
    }
}

/// Coarse analysis matching the former host-generation behavior: once a
/// transaction reads the heap, every committed heap write conflicts.
#[derive(Debug, Default)]
pub struct CoarseConflictAnalysis;

impl ConflictAnalysisStrategy for CoarseConflictAnalysis {
    fn begin(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(CoarseObservationIndex(false))
    }

    fn name(&self) -> &'static str {
        "coarse"
    }
}

#[derive(Clone)]
struct CoarseObservationIndex(bool);

impl ConflictObservationIndex for CoarseObservationIndex {
    fn clone_box(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(self.clone())
    }

    fn observe(&mut self, _path: &ConflictPath) {
        self.0 = true;
    }

    fn may_conflict(&self, _changed: &ConflictPath) -> bool {
        self.0
    }
}

/// Immutable heap state captured at the beginning of a transaction.
#[derive(Clone)]
pub struct StoreSnapshot {
    revision: u64,
    root: PublicValue,
    strategy: Arc<dyn ConflictAnalysisStrategy>,
}

impl StoreSnapshot {
    #[doc(hidden)]
    pub fn root(&self) -> &PublicValue {
        &self.root
    }
}

#[derive(Clone)]
struct StoreWrite {
    path: ConflictPath,
    value: PublicValue,
}

/// Reads and writes accumulated by one optimistic transaction.
#[derive(Clone)]
pub struct StoreJournal {
    snapshot: StoreSnapshot,
    observations: Box<dyn ConflictObservationIndex>,
    writes: Vec<StoreWrite>,
}

impl StoreJournal {
    #[doc(hidden)]
    pub fn new(snapshot: StoreSnapshot) -> Self {
        let observations = snapshot.strategy.begin();
        Self {
            snapshot,
            observations,
            writes: Vec::new(),
        }
    }

    pub(crate) fn observe(&mut self, path: &[Key]) {
        self.observations
            .observe(&ConflictPath::from_keys(path.to_vec()));
    }

    pub(crate) fn view(&self) -> PublicValue {
        apply_writes(self.snapshot.root.clone(), &self.writes)
    }

    pub(crate) fn write(&mut self, path: Vec<Key>, value: PublicValue) {
        self.writes.push(StoreWrite {
            path: ConflictPath::from_keys(path),
            value,
        });
    }
}

/// Shared reflection heap state. Hosts place this inside their existing lock
/// so heap and specialization commits remain atomic.
pub struct ReflectionStore {
    root: PublicValue,
    revision: u64,
    latest_changes: BTreeMap<ConflictPath, u64>,
    strategy: Arc<dyn ConflictAnalysisStrategy>,
}

impl ReflectionStore {
    pub fn new(strategy: Arc<dyn ConflictAnalysisStrategy>) -> Self {
        Self {
            root: PublicValue::empty_record(),
            revision: 0,
            latest_changes: BTreeMap::new(),
            strategy,
        }
    }

    #[doc(hidden)]
    pub fn snapshot(&self) -> StoreSnapshot {
        StoreSnapshot {
            revision: self.revision,
            root: self.root.clone(),
            strategy: self.strategy.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &PublicValue {
        &self.root
    }

    #[doc(hidden)]
    pub fn strategy(&self) -> Arc<dyn ConflictAnalysisStrategy> {
        self.strategy.clone()
    }

    #[doc(hidden)]
    pub fn replace_root(&mut self, root: PublicValue) {
        self.root = root;
        self.revision = self.revision.wrapping_add(1);
        self.latest_changes.clear();
        self.latest_changes
            .insert(ConflictPath::root(), self.revision);
    }

    /// Validates and commits a journal. Exact write paths and rebase policy
    /// remain independent of the selected read-analysis strategy.
    #[doc(hidden)]
    pub fn try_commit(&mut self, journal: &StoreJournal) -> bool {
        if self.conflicts(journal) {
            return false;
        }
        if journal.writes.is_empty() {
            return true;
        }

        self.root = apply_writes(self.root.clone(), &journal.writes);
        self.revision = self.revision.wrapping_add(1);
        for path in normalized_write_paths(&journal.writes) {
            self.latest_changes.insert(path, self.revision);
        }
        true
    }

    fn conflicts(&self, journal: &StoreJournal) -> bool {
        self.latest_changes.iter().any(|(changed, revision)| {
            if *revision <= journal.snapshot.revision {
                return false;
            }
            if journal.observations.may_conflict(changed) {
                return true;
            }
            journal
                .writes
                .iter()
                .any(|write| write.path != *changed && write.path.overlaps(changed))
        })
    }
}

fn normalized_write_paths(writes: &[StoreWrite]) -> Vec<ConflictPath> {
    let mut paths = BTreeSet::new();
    for write in writes {
        if paths
            .iter()
            .any(|path: &ConflictPath| path.is_prefix_of(&write.path))
        {
            continue;
        }
        paths.retain(|path| !write.path.is_prefix_of(path));
        paths.insert(write.path.clone());
    }
    paths.into_iter().collect()
}

fn apply_writes(mut root: PublicValue, writes: &[StoreWrite]) -> PublicValue {
    for write in writes {
        if write.path.depth() == 0 {
            root = write.value.clone();
            continue;
        }
        let path = Value::List(List::from_values(
            write.path.keys().iter().cloned().map(key_value).collect(),
        ));
        root = PublicValue::from_core(Value::builtin_call(
            Builtin::DictUpdate,
            vec![path, write.value.as_core().clone(), root.as_core().clone()],
        ));
    }
    root
}

fn key_value(key: Key) -> Value {
    match key {
        Key::Atom(atom) => Value::Atom(atom),
        Key::Number(number) => Value::Number(number),
        Key::Binary(bytes) => Value::Binary(bytes),
        Key::AbstractGlobalPath(parts) => {
            Value::Atom(Atom::from_key(&Key::AbstractGlobalPath(parts)))
        }
        Key::List(items) => Value::List(List::from_values(
            items.iter().cloned().map(key_value).collect(),
        )),
        Key::Dict(entries) => Value::Dict(
            entries
                .iter()
                .cloned()
                .fold(Dict::new_sync(), |dict, (key, value)| {
                    dict.insert(key, key_value(value))
                }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(parts: &[&str]) -> Vec<Key> {
        parts.iter().map(Key::atom_from_text).collect()
    }

    fn store() -> ReflectionStore {
        ReflectionStore::new(Arc::new(ExactConflictAnalysis))
    }

    #[test]
    fn exact_strategy_detects_both_overlap_directions() {
        let strategy = ExactConflictAnalysis;
        let mut observations = strategy.begin();
        observations.observe(&ConflictPath::from_keys(path(&["a", "b"])));
        assert!(observations.may_conflict(&ConflictPath::from_keys(path(&["a"]))));
        assert!(observations.may_conflict(&ConflictPath::from_keys(path(&["a", "b", "c"]))));
        assert!(!observations.may_conflict(&ConflictPath::from_keys(path(&["z"]))));
    }

    #[test]
    fn fingerprint_strategy_is_conservative_for_path_overlap() {
        let strategy = FingerprintConflictAnalysis;
        let mut observations = strategy.begin();
        observations.observe(&ConflictPath::from_keys(path(&["a", "b"])));
        assert!(observations.may_conflict(&ConflictPath::from_keys(path(&["a"]))));
        assert!(observations.may_conflict(&ConflictPath::from_keys(path(&["a", "b", "c"]))));
    }

    #[test]
    fn disjoint_writes_rebase_and_exact_blind_writes_replace() {
        let mut store = store();
        let snapshot = store.snapshot();
        let mut left = StoreJournal::new(snapshot.clone());
        left.write(path(&["left"]), PublicValue::integer(1));
        let mut right = StoreJournal::new(snapshot.clone());
        right.write(path(&["right"]), PublicValue::integer(2));
        let mut later_left = StoreJournal::new(snapshot);
        later_left.write(path(&["left"]), PublicValue::integer(3));

        assert!(store.try_commit(&left));
        assert!(store.try_commit(&right));
        assert!(store.try_commit(&later_left));
    }

    #[test]
    fn disjoint_nested_siblings_rebase() {
        let mut store = store();
        let mut establish_parent = StoreJournal::new(store.snapshot());
        establish_parent.write(path(&["tree"]), PublicValue::empty_record());
        assert!(store.try_commit(&establish_parent));

        let snapshot = store.snapshot();
        let mut left = StoreJournal::new(snapshot.clone());
        left.write(path(&["tree", "left"]), PublicValue::integer(1));
        let mut right = StoreJournal::new(snapshot);
        right.write(path(&["tree", "right"]), PublicValue::integer(2));

        assert!(store.try_commit(&left));
        assert!(store.try_commit(&right));
    }

    #[test]
    fn root_observation_conflicts_with_every_write() {
        let mut store = store();
        let snapshot = store.snapshot();
        let mut reader = StoreJournal::new(snapshot.clone());
        reader.observe(&[]);
        let mut writer = StoreJournal::new(snapshot);
        writer.write(path(&["anywhere"]), PublicValue::integer(1));

        assert!(store.try_commit(&writer));
        assert!(!store.try_commit(&reader));
    }

    #[test]
    fn reads_and_strictly_nested_writes_conflict() {
        let mut store = store();
        let snapshot = store.snapshot();
        let mut reader = StoreJournal::new(snapshot.clone());
        reader.observe(&path(&["missing", "child"]));
        let mut nested_writer = StoreJournal::new(snapshot.clone());
        nested_writer.write(path(&["tree", "child"]), PublicValue::integer(1));
        let mut parent_writer = StoreJournal::new(snapshot.clone());
        parent_writer.write(path(&["tree"]), PublicValue::empty_record());
        let mut missing_writer = StoreJournal::new(snapshot);
        missing_writer.write(path(&["missing"]), PublicValue::empty_record());

        assert!(store.try_commit(&nested_writer));
        assert!(!store.try_commit(&parent_writer));
        assert!(store.try_commit(&missing_writer));
        assert!(!store.try_commit(&reader));
    }

    #[test]
    fn coarse_strategy_conflicts_after_any_observation() {
        let strategy = CoarseConflictAnalysis;
        let mut observations = strategy.begin();
        assert!(!observations.may_conflict(&ConflictPath::root()));
        observations.observe(&ConflictPath::from_keys(path(&["a"])));
        assert!(observations.may_conflict(&ConflictPath::from_keys(path(&["z"]))));
    }
}
