//! Journaled shared state for reflection tasks.
//!
//! Conflict-analysis strategies summarize reads only. The store retains exact
//! edit paths so strategy selection cannot change edit or rebase semantics.

use std::collections::{BTreeMap, BTreeSet, hash_map::RandomState};
use std::hash::BuildHasher;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, LazyLock, Weak};

use rpds::RedBlackTreeMapSync;

use crate::api::Value as PublicValue;
use crate::core::{Atom, Builtin, Dict, Key, LazyValue, List, Value, keys};
use crate::core_net::CoreDataKey;
use crate::number::Number;

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

/// One shared-state partition within a reasoning session.
///
/// IDs are allocated monotonically by the store and are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VolumeId(NonZeroU64);

impl VolumeId {
    pub fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) fn from_u64(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct EvaluationQueryId(NonZeroU64);

impl EvaluationQueryId {
    fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug)]
struct QueryDomain {
    next_id: AtomicU64,
    retired: Sender<EvaluationQueryId>,
}

impl QueryDomain {
    fn new() -> (Arc<Self>, Receiver<EvaluationQueryId>) {
        let (retired, retirements) = mpsc::channel();
        (
            Arc::new(Self {
                next_id: AtomicU64::new(1),
                retired,
            }),
            retirements,
        )
    }

    fn allocate(self: &Arc<Self>) -> Result<Arc<EvaluationQueryHandle>, Arc<str>> {
        let id = self
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| Arc::from("evaluation query IDs exhausted"))?;
        let id = NonZeroU64::new(id).expect("evaluation query IDs start at one");
        Ok(Arc::new(EvaluationQueryHandle {
            id: EvaluationQueryId(id),
            domain: Arc::downgrade(self),
        }))
    }

    fn retire(&self, id: EvaluationQueryId) {
        // Failure only means the owning store has already been dropped.
        let _ = self.retired.send(id);
    }
}

/// Opaque lifetime lease for one asynchronous reflection query.
///
/// The final clone queues removal of the query's private-volume state. Cleanup
/// is performed later while the store is already at a safe mutation point.
#[doc(hidden)]
pub struct EvaluationQueryHandle {
    id: EvaluationQueryId,
    domain: Weak<QueryDomain>,
}

impl std::fmt::Debug for EvaluationQueryHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("EvaluationQueryHandle")
            .field(&self.id.get())
            .finish()
    }
}

impl Drop for EvaluationQueryHandle {
    fn drop(&mut self) {
        if let Some(domain) = self.domain.upgrade() {
            domain.retire(self.id);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluationQueryPoll {
    State { value: PublicValue, observed: bool },
    ForeignSession,
}

/// A conflict-analysis address. Hierarchical path relationships never cross
/// volume boundaries.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StoreAddress {
    volume: VolumeId,
    path: ConflictPath,
}

impl StoreAddress {
    fn new(volume: VolumeId, path: ConflictPath) -> Self {
        Self { volume, path }
    }

    fn root(volume: VolumeId) -> Self {
        Self::new(volume, ConflictPath::root())
    }

    fn is_prefix_of(&self, other: &Self) -> bool {
        self.volume == other.volume && self.path.is_prefix_of(&other.path)
    }

    fn overlaps(&self, other: &Self) -> bool {
        self.volume == other.volume && self.path.overlaps(&other.path)
    }

    fn prefixes(&self) -> impl Iterator<Item = Self> + '_ {
        self.path
            .prefixes()
            .map(|path| Self::new(self.volume, path))
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
    fn observe(&mut self, address: &StoreAddress);
    fn may_conflict(&self, changed: &StoreAddress) -> bool;
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
    addresses: BTreeSet<StoreAddress>,
}

impl ConflictObservationIndex for ExactObservationIndex {
    fn clone_box(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(self.clone())
    }

    fn observe(&mut self, address: &StoreAddress) {
        if self.addresses.iter().any(|seen| seen.is_prefix_of(address)) {
            return;
        }
        self.addresses.retain(|seen| !address.is_prefix_of(seen));
        self.addresses.insert(address.clone());
    }

    fn may_conflict(&self, changed: &StoreAddress) -> bool {
        self.addresses.iter().any(|read| read.overlaps(changed))
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
    fn fingerprint(&self, address: &StoreAddress) -> u64 {
        self.hash_builder.hash_one(address)
    }
}

impl ConflictObservationIndex for FingerprintObservationIndex {
    fn clone_box(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(self.clone())
    }

    fn observe(&mut self, address: &StoreAddress) {
        self.complete_reads.insert(self.fingerprint(address));
        for prefix in address.prefixes() {
            self.read_prefixes.insert(self.fingerprint(&prefix));
        }
    }

    fn may_conflict(&self, changed: &StoreAddress) -> bool {
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

    fn observe(&mut self, _address: &StoreAddress) {
        self.0 = true;
    }

    fn may_conflict(&self, _changed: &StoreAddress) -> bool {
        self.0
    }
}

/// Immutable heap state captured at the beginning of a transaction.
#[derive(Clone)]
pub struct StoreSnapshot {
    // Revisions are store-local; identity prevents a coincidental revision
    // match from enabling the cached-view commit path for another store.
    identity: Arc<()>,
    revision: u64,
    heap_volume: VolumeId,
    query_volume: VolumeId,
    query_domain: Arc<QueryDomain>,
    roots: RedBlackTreeMapSync<VolumeId, PublicValue>,
    strategy: Arc<dyn ConflictAnalysisStrategy>,
}

impl StoreSnapshot {
    #[doc(hidden)]
    pub fn root(&self) -> &PublicValue {
        self.roots
            .get(&self.heap_volume)
            .expect("the user heap volume must always exist")
    }

    pub(crate) fn volume(&self, volume: VolumeId) -> Option<&PublicValue> {
        self.roots.get(&volume)
    }

    pub(crate) fn poll_query(&self, handle: &Arc<EvaluationQueryHandle>) -> EvaluationQueryPoll {
        if !query_belongs_to(&self.query_domain, handle) {
            return EvaluationQueryPoll::ForeignSession;
        }
        let Some(root) = self.volume(self.query_volume) else {
            return EvaluationQueryPoll::State {
                value: PublicValue::empty_record(),
                observed: true,
            };
        };
        EvaluationQueryPoll::State {
            value: PublicValue::from_core(lazy_core_value_path(
                root.as_core().clone(),
                &query_path(handle.id),
            )),
            observed: true,
        }
    }
}

#[derive(Clone)]
enum StoreEdit {
    Set {
        address: StoreAddress,
        value: PublicValue,
    },
    Rewrite {
        address: StoreAddress,
        updater: PublicValue,
    },
}

impl StoreEdit {
    fn address(&self) -> &StoreAddress {
        match self {
            Self::Set { address, .. } | Self::Rewrite { address, .. } => address,
        }
    }
}

/// Reads and ordered edits accumulated by one optimistic transaction.
#[derive(Clone)]
pub struct StoreJournal {
    snapshot: StoreSnapshot,
    views: RedBlackTreeMapSync<VolumeId, PublicValue>,
    observations: Box<dyn ConflictObservationIndex>,
    edits: Vec<StoreEdit>,
}

impl StoreJournal {
    #[doc(hidden)]
    pub fn new(snapshot: StoreSnapshot) -> Self {
        let observations = snapshot.strategy.begin();
        let views = snapshot.roots.clone();
        Self {
            snapshot,
            views,
            observations,
            edits: Vec::new(),
        }
    }

    /// Records the portion of the snapshot needed by this read. Local rewrites
    /// may widen that dependency; an earlier covering set makes it internal.
    /// Earlier observations remain intact.
    pub(crate) fn observe_read(&mut self, path: &[Key]) -> bool {
        self.observe_volume_read(self.snapshot.heap_volume, path)
    }

    pub(crate) fn observe_volume_read(&mut self, volume: VolumeId, path: &[Key]) -> bool {
        let address = StoreAddress::new(volume, ConflictPath::from_keys(path.to_vec()));
        if self.snapshot.volume(volume).is_none() {
            self.observations.observe(&address);
            return true;
        }
        let mut dependency = ConflictPath::from_keys(path.to_vec());
        for edit in self.edits.iter().rev() {
            match edit {
                StoreEdit::Set { address, .. }
                    if address.volume == volume && address.path.is_prefix_of(&dependency) =>
                {
                    return false;
                }
                StoreEdit::Rewrite { address, .. }
                    if address.volume == volume && address.path.overlaps(&dependency) =>
                {
                    if address.path.is_prefix_of(&dependency) {
                        dependency = address.path.clone();
                    }
                }
                StoreEdit::Set { .. } | StoreEdit::Rewrite { .. } => {}
            }
        }
        self.observations
            .observe(&StoreAddress::new(volume, dependency));
        true
    }

    pub(crate) fn view(&self) -> PublicValue {
        self.volume_view(self.snapshot.heap_volume)
            .expect("the user heap volume must always exist")
    }

    pub(crate) fn volume_view(&self, volume: VolumeId) -> Option<PublicValue> {
        self.views.get(&volume).cloned()
    }

    pub(crate) fn reserve_query(&mut self) -> Result<Arc<EvaluationQueryHandle>, Arc<str>> {
        self.reserve_query_state(pending_query_value())
    }

    pub(crate) fn reserve_query_with(
        &mut self,
        result: PublicValue,
    ) -> Result<Arc<EvaluationQueryHandle>, Arc<str>> {
        self.reserve_query_state(complete_query_value(result))
    }

    fn reserve_query_state(
        &mut self,
        state: PublicValue,
    ) -> Result<Arc<EvaluationQueryHandle>, Arc<str>> {
        let handle = self.snapshot.query_domain.allocate()?;
        self.write_volume(self.snapshot.query_volume, query_path(handle.id), state);
        Ok(handle)
    }

    #[cfg(test)]
    pub(crate) fn poll_query(
        &mut self,
        handle: &Arc<EvaluationQueryHandle>,
    ) -> EvaluationQueryPoll {
        let observed = self.observe_query(handle);
        self.peek_query_with_observation(handle, observed)
    }

    pub(crate) fn peek_query(&self, handle: &Arc<EvaluationQueryHandle>) -> EvaluationQueryPoll {
        self.peek_query_with_observation(handle, false)
    }

    pub(crate) fn observe_query(&mut self, handle: &Arc<EvaluationQueryHandle>) -> bool {
        if !query_belongs_to(&self.snapshot.query_domain, handle) {
            return false;
        }
        let path = query_path(handle.id);
        self.observe_volume_read(self.snapshot.query_volume, &path)
    }

    fn peek_query_with_observation(
        &self,
        handle: &Arc<EvaluationQueryHandle>,
        observed: bool,
    ) -> EvaluationQueryPoll {
        if !query_belongs_to(&self.snapshot.query_domain, handle) {
            return EvaluationQueryPoll::ForeignSession;
        }
        let Some(root) = self.volume_view(self.snapshot.query_volume) else {
            return EvaluationQueryPoll::State {
                value: PublicValue::empty_record(),
                observed,
            };
        };
        EvaluationQueryPoll::State {
            value: PublicValue::from_core(lazy_core_value_path(
                root.into_core(),
                &query_path(handle.id),
            )),
            observed,
        }
    }

    pub(crate) fn write(&mut self, path: Vec<Key>, value: PublicValue) {
        self.write_volume(self.snapshot.heap_volume, path, value);
    }

    pub(crate) fn write_volume(&mut self, volume: VolumeId, path: Vec<Key>, value: PublicValue) {
        let edit = StoreEdit::Set {
            address: StoreAddress::new(volume, ConflictPath::from_keys(path)),
            value,
        };
        if let Some(view) = self.views.get(&volume).cloned() {
            self.views.insert_mut(volume, apply_edit(view, &edit));
        }
        self.edits.push(edit);
    }

    pub(crate) fn rewrite(&mut self, path: Vec<Key>, updater: PublicValue) {
        self.rewrite_volume(self.snapshot.heap_volume, path, updater);
    }

    pub(crate) fn rewrite_volume(
        &mut self,
        volume: VolumeId,
        path: Vec<Key>,
        updater: PublicValue,
    ) {
        let edit = StoreEdit::Rewrite {
            address: StoreAddress::new(volume, ConflictPath::from_keys(path)),
            updater,
        };
        if let Some(view) = self.views.get(&volume).cloned() {
            self.views.insert_mut(volume, apply_edit(view, &edit));
        }
        self.edits.push(edit);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreCommitResult {
    Committed,
    Conflict,
    MissingVolume(VolumeId),
}

/// Shared reflection heap state. Hosts place this inside their existing lock
/// so heap and specialization commits remain atomic.
pub struct ReflectionStore {
    identity: Arc<()>,
    heap_volume: VolumeId,
    query_volume: VolumeId,
    query_domain: Arc<QueryDomain>,
    query_retirements: Receiver<EvaluationQueryId>,
    next_volume: u64,
    roots: RedBlackTreeMapSync<VolumeId, PublicValue>,
    revision: u64,
    latest_changes: BTreeMap<StoreAddress, u64>,
    strategy: Arc<dyn ConflictAnalysisStrategy>,
}

impl ReflectionStore {
    pub fn new(strategy: Arc<dyn ConflictAnalysisStrategy>) -> Self {
        let heap_volume = VolumeId::from_u64(1).expect("one is a nonzero volume ID");
        let query_volume = VolumeId::from_u64(2).expect("two is a nonzero volume ID");
        let (query_domain, query_retirements) = QueryDomain::new();
        Self {
            identity: Arc::new(()),
            heap_volume,
            query_volume,
            query_domain,
            query_retirements,
            next_volume: 3,
            roots: RedBlackTreeMapSync::new_sync()
                .insert(heap_volume, PublicValue::empty_record())
                .insert(query_volume, PublicValue::empty_record()),
            revision: 0,
            latest_changes: BTreeMap::new(),
            strategy,
        }
    }

    #[doc(hidden)]
    pub fn snapshot(&self) -> StoreSnapshot {
        StoreSnapshot {
            identity: self.identity.clone(),
            revision: self.revision,
            heap_volume: self.heap_volume,
            query_volume: self.query_volume,
            query_domain: self.query_domain.clone(),
            roots: self.roots.clone(),
            strategy: self.strategy.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &PublicValue {
        self.roots
            .get(&self.heap_volume)
            .expect("the user heap volume must always exist")
    }

    #[cfg(test)]
    fn volume_root(&self, volume: VolumeId) -> Option<&PublicValue> {
        self.roots.get(&volume)
    }

    #[doc(hidden)]
    pub fn strategy(&self) -> Arc<dyn ConflictAnalysisStrategy> {
        self.strategy.clone()
    }

    pub(crate) fn set_strategy(&mut self, strategy: Arc<dyn ConflictAnalysisStrategy>) {
        self.strategy = strategy;
    }

    #[doc(hidden)]
    pub fn replace_root(&mut self, root: PublicValue) {
        self.roots.insert_mut(self.heap_volume, root);
        self.revision = self.revision.wrapping_add(1);
        self.latest_changes
            .insert(StoreAddress::root(self.heap_volume), self.revision);
    }

    pub(crate) fn create_volume(&mut self, initial: PublicValue) -> Result<VolumeId, Arc<str>> {
        let volume = VolumeId::from_u64(self.next_volume)
            .ok_or_else(|| Arc::from("reflection volume IDs exhausted"))?;
        self.next_volume = self
            .next_volume
            .checked_add(1)
            .ok_or_else(|| Arc::from("reflection volume IDs exhausted"))?;
        self.roots.insert_mut(volume, initial);
        self.revision = self.revision.wrapping_add(1);
        self.latest_changes
            .insert(StoreAddress::root(volume), self.revision);
        Ok(volume)
    }

    pub(crate) fn revoke_volume(&mut self, volume: VolumeId) -> Option<PublicValue> {
        if volume == self.heap_volume || volume == self.query_volume {
            return None;
        }
        let root = self.roots.get(&volume).cloned()?;
        self.roots.remove_mut(&volume);
        self.revision = self.revision.wrapping_add(1);
        self.latest_changes
            .insert(StoreAddress::root(volume), self.revision);
        Some(root)
    }

    #[doc(hidden)]
    pub fn update_query(
        &mut self,
        handle: &Arc<EvaluationQueryHandle>,
        result: PublicValue,
    ) -> bool {
        if !query_belongs_to(&self.query_domain, handle) {
            return false;
        }
        if self.roots.get(&self.query_volume).is_none() {
            return false;
        }
        let mut journal = StoreJournal::new(self.snapshot());
        journal.write_volume(
            self.query_volume,
            query_path(handle.id),
            complete_query_value(result),
        );
        matches!(self.try_commit(&journal), StoreCommitResult::Committed)
    }

    /// Validates and commits a journal. Exact edit paths and rebase policy
    /// remain independent of the selected read-analysis strategy.
    #[doc(hidden)]
    pub fn try_commit(&mut self, journal: &StoreJournal) -> StoreCommitResult {
        if let Some(volume) = journal
            .edits
            .iter()
            .map(|edit| edit.address().volume)
            .find(|volume| self.roots.get(volume).is_none())
        {
            return StoreCommitResult::MissingVolume(volume);
        }
        if self.conflicts(journal) {
            return StoreCommitResult::Conflict;
        }
        if journal.edits.is_empty() {
            self.retire_queries();
            return StoreCommitResult::Committed;
        }

        self.roots = if Arc::ptr_eq(&self.identity, &journal.snapshot.identity)
            && self.revision == journal.snapshot.revision
        {
            journal.views.clone()
        } else {
            apply_edits(self.roots.clone(), &journal.edits)
        };
        self.revision = self.revision.wrapping_add(1);
        for path in normalized_edit_paths(&journal.edits) {
            self.latest_changes.insert(path, self.revision);
        }
        self.retire_queries();
        StoreCommitResult::Committed
    }

    fn retire_queries(&mut self) {
        let retired = self.query_retirements.try_iter().collect::<Vec<_>>();
        if retired.is_empty() {
            return;
        }
        let Some(mut root) = self.roots.get(&self.query_volume).cloned() else {
            return;
        };
        self.revision = self.revision.wrapping_add(1);
        for id in retired {
            let path = ConflictPath::from_keys(query_path(id));
            root = apply_value_at_path(root, &path, Value::Dict(Dict::new_sync()));
            self.latest_changes
                .insert(StoreAddress::new(self.query_volume, path), self.revision);
        }
        self.roots.insert_mut(self.query_volume, root);
    }

    fn conflicts(&self, journal: &StoreJournal) -> bool {
        self.latest_changes.iter().any(|(changed, revision)| {
            *revision > journal.snapshot.revision && journal.observations.may_conflict(changed)
        })
    }
}

fn normalized_edit_paths(edits: &[StoreEdit]) -> Vec<StoreAddress> {
    let mut addresses = BTreeSet::new();
    for edit in edits {
        let edit_address = edit.address();
        if addresses
            .iter()
            .any(|address: &StoreAddress| address.is_prefix_of(edit_address))
        {
            continue;
        }
        addresses.retain(|address| !edit_address.is_prefix_of(address));
        addresses.insert(edit_address.clone());
    }
    addresses.into_iter().collect()
}

fn apply_edits(
    mut roots: RedBlackTreeMapSync<VolumeId, PublicValue>,
    edits: &[StoreEdit],
) -> RedBlackTreeMapSync<VolumeId, PublicValue> {
    for edit in edits {
        let volume = edit.address().volume;
        let root = roots
            .get(&volume)
            .cloned()
            .expect("commit validates every edited volume before replay");
        roots.insert_mut(volume, apply_edit(root, edit));
    }
    roots
}

static QUERY_PENDING: LazyLock<Key> = LazyLock::new(|| {
    Key::abstract_global_path(["reflection_runtime", "v0", "query_state", "pending"])
});
static QUERY_COMPLETE: LazyLock<Key> = LazyLock::new(|| {
    Key::abstract_global_path(["reflection_runtime", "v0", "query_state", "complete"])
});
static QUERY_RESULT: LazyLock<Key> = LazyLock::new(|| {
    Key::abstract_global_path(["reflection_runtime", "v0", "query_state", "result"])
});
static QUERY_PRESENT: LazyLock<Key> = LazyLock::new(|| {
    Key::abstract_global_path(["reflection_runtime", "v0", "query_state", "present"])
});

pub(crate) enum EvaluationQueryState {
    Pending,
    Complete(PublicValue),
}

fn query_belongs_to(domain: &Arc<QueryDomain>, handle: &Arc<EvaluationQueryHandle>) -> bool {
    handle
        .domain
        .upgrade()
        .is_some_and(|owner| Arc::ptr_eq(domain, &owner))
}

fn query_path(id: EvaluationQueryId) -> Vec<Key> {
    vec![Key::Number(Number::from_u64(id.get()))]
}

fn pending_query_value() -> PublicValue {
    PublicValue::from_core(Value::Dict(
        Dict::new_sync().insert(QUERY_PENDING.clone(), (*keys::UNIT_VALUE).clone()),
    ))
}

fn complete_query_value(result: PublicValue) -> PublicValue {
    let payload = Value::Dict(
        Dict::new_sync()
            .insert(QUERY_PRESENT.clone(), (*keys::UNIT_VALUE).clone())
            .insert(QUERY_RESULT.clone(), result.into_core()),
    );
    PublicValue::from_core(Value::Dict(
        Dict::new_sync().insert(QUERY_COMPLETE.clone(), payload),
    ))
}

pub(crate) fn decode_query_state(value: &Value) -> Option<EvaluationQueryState> {
    let Value::Dict(state) = value else {
        return None;
    };
    if state.iter().count() != 1 {
        return None;
    }
    if state.get(&QUERY_PENDING).is_some() {
        return Some(EvaluationQueryState::Pending);
    }
    let Value::Dict(complete) = state.get(&QUERY_COMPLETE)? else {
        return None;
    };
    complete.get(&QUERY_PRESENT)?;
    Some(EvaluationQueryState::Complete(PublicValue::from_core(
        complete
            .get(&QUERY_RESULT)
            .cloned()
            .unwrap_or_else(|| Value::Dict(Dict::new_sync())),
    )))
}

fn apply_edit(root: PublicValue, edit: &StoreEdit) -> PublicValue {
    match edit {
        StoreEdit::Set { address, value } => {
            apply_value_at_path(root, &address.path, value.as_core().clone())
        }
        StoreEdit::Rewrite { address, updater } => {
            let prior = lazy_core_value_path(root.as_core().clone(), address.path.keys());
            let updated = Value::Lazy(LazyValue::from_application(
                updater.as_core().clone(),
                Arc::from([prior]),
            ));
            apply_value_at_path(root, &address.path, updated)
        }
    }
}

fn apply_value_at_path(root: PublicValue, path: &ConflictPath, value: Value) -> PublicValue {
    if path.depth() == 0 {
        return PublicValue::from_core(value);
    }
    let path = Value::List(List::from_values(
        path.keys().iter().cloned().map(key_value).collect(),
    ));
    PublicValue::from_core(Value::builtin_call(
        Builtin::DictUpdate,
        vec![path, value, root.into_core()],
    ))
}

fn lazy_core_value_path(value: Value, path: &[Key]) -> Value {
    if path.is_empty() {
        return value;
    }
    Value::Lazy(LazyValue::from_access(
        Arc::from(
            path.iter()
                .cloned()
                .map(CoreDataKey::Key)
                .collect::<Vec<_>>(),
        ),
        Arc::from([value]),
    ))
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
    use crate::api::Assembler;

    fn path(parts: &[&str]) -> Vec<Key> {
        parts.iter().map(Key::atom_from_text).collect()
    }

    fn address(volume: VolumeId, parts: &[&str]) -> StoreAddress {
        StoreAddress::new(volume, ConflictPath::from_keys(path(parts)))
    }

    fn store() -> ReflectionStore {
        ReflectionStore::new(Arc::new(ExactConflictAnalysis))
    }

    fn assert_list_values(assembler: &Assembler, actual: &PublicValue, expected: &PublicValue) {
        let actual = assembler.evaluate(actual).unwrap();
        let Value::List(actual) = actual.as_core() else {
            panic!("actual value should be a list")
        };
        let Value::List(expected) = expected.as_core() else {
            panic!("expected value should be a list")
        };
        assert_eq!(
            crate::eval::list_to_value_items(&assembler.eval_context(), actual).unwrap(),
            crate::eval::list_to_value_items(&assembler.eval_context(), expected).unwrap(),
        );
    }

    fn evaluate_query_state(
        assembler: &Assembler,
        value: PublicValue,
    ) -> Option<EvaluationQueryState> {
        let value = assembler.evaluate(&value).unwrap();
        decode_query_state(value.as_core())
    }

    #[test]
    fn query_state_is_transactional_and_retired_after_the_last_handle() {
        let assembler = Assembler::default();
        let mut store = store();
        let mut reservation = StoreJournal::new(store.snapshot());
        let handle = reservation.reserve_query().unwrap();
        assert!(matches!(
            reservation.poll_query(&handle),
            EvaluationQueryPoll::State {
                observed: false,
                ..
            }
        ));
        assert_eq!(store.try_commit(&reservation), StoreCommitResult::Committed);

        let EvaluationQueryPoll::State { value, observed } = store.snapshot().poll_query(&handle)
        else {
            panic!("committed query should belong to its store")
        };
        assert!(observed);
        assert!(matches!(
            evaluate_query_state(&assembler, value),
            Some(EvaluationQueryState::Pending)
        ));

        assert!(store.update_query(&handle, PublicValue::text("snapshot")));
        let EvaluationQueryPoll::State { value, .. } = store.snapshot().poll_query(&handle) else {
            panic!("completed query should remain available")
        };
        assert!(matches!(
            evaluate_query_state(&assembler, value),
            Some(EvaluationQueryState::Complete(value))
                if value.as_binary() == Some(b"snapshot".as_slice())
        ));

        assert!(store.update_query(&handle, PublicValue::text("updated")));
        let EvaluationQueryPoll::State { value, .. } = store.snapshot().poll_query(&handle) else {
            panic!("updated query should remain available")
        };
        assert!(matches!(
            evaluate_query_state(&assembler, value),
            Some(EvaluationQueryState::Complete(value))
                if value.as_binary() == Some(b"updated".as_slice())
        ));

        let id = handle.id;
        drop(handle);
        let maintenance = StoreJournal::new(store.snapshot());
        assert_eq!(store.try_commit(&maintenance), StoreCommitResult::Committed);
        let root = store.roots.get(&store.query_volume).unwrap();
        let retired = PublicValue::from_core(lazy_core_value_path(
            root.as_core().clone(),
            &query_path(id),
        ));
        assert!(assembler.evaluate(&retired).unwrap().is_undefined());
    }

    #[test]
    fn exact_strategy_detects_both_overlap_directions() {
        let strategy = ExactConflictAnalysis;
        let mut observations = strategy.begin();
        let volume = VolumeId::from_u64(1).unwrap();
        observations.observe(&address(volume, &["a", "b"]));
        assert!(observations.may_conflict(&address(volume, &["a"])));
        assert!(observations.may_conflict(&address(volume, &["a", "b", "c"])));
        assert!(!observations.may_conflict(&address(volume, &["z"])));
    }

    #[test]
    fn fingerprint_strategy_is_conservative_for_path_overlap() {
        let strategy = FingerprintConflictAnalysis;
        let mut observations = strategy.begin();
        let volume = VolumeId::from_u64(1).unwrap();
        observations.observe(&address(volume, &["a", "b"]));
        assert!(observations.may_conflict(&address(volume, &["a"])));
        assert!(observations.may_conflict(&address(volume, &["a", "b", "c"])));
    }

    #[test]
    fn exact_strategy_treats_distinct_volumes_as_disjoint() {
        let strategy = ExactConflictAnalysis;
        let mut observations = strategy.begin();
        let first = VolumeId::from_u64(1).unwrap();
        let second = VolumeId::from_u64(2).unwrap();
        observations.observe(&address(first, &["same"]));

        assert!(!observations.may_conflict(&address(second, &["same"])));
    }

    #[test]
    fn fingerprint_strategy_treats_distinct_volumes_as_disjoint() {
        let strategy = FingerprintConflictAnalysis;
        let mut observations = strategy.begin();
        let first = VolumeId::from_u64(1).unwrap();
        let second = VolumeId::from_u64(2).unwrap();
        observations.observe(&address(first, &["same"]));

        assert!(!observations.may_conflict(&address(second, &["same"])));
    }

    #[test]
    fn journal_caches_its_view_and_uncontended_commit_installs_it() {
        let mut store = store();
        let mut journal = StoreJournal::new(store.snapshot());
        journal.write(path(&["value"]), PublicValue::integer(1));

        let cached_view = journal.view();
        assert_eq!(journal.view(), cached_view);
        assert_eq!(store.try_commit(&journal), StoreCommitResult::Committed);
        assert_eq!(store.root(), &cached_view);
    }

    #[test]
    fn concurrent_commit_rebases_instead_of_installing_cached_view() {
        let mut store = store();
        let snapshot = store.snapshot();
        let mut first = StoreJournal::new(snapshot.clone());
        first.write(path(&["first"]), PublicValue::integer(1));
        let mut second = StoreJournal::new(snapshot);
        second.write(path(&["second"]), PublicValue::integer(2));
        let stale_cached_view = second.view();

        assert_eq!(store.try_commit(&first), StoreCommitResult::Committed);
        assert_eq!(store.try_commit(&second), StoreCommitResult::Committed);
        assert_ne!(store.root(), &stale_cached_view);
    }

    #[test]
    fn one_journal_updates_multiple_volumes_atomically() {
        let mut store = store();
        let first = store.create_volume(PublicValue::empty_record()).unwrap();
        let second = store.create_volume(PublicValue::empty_record()).unwrap();
        let mut journal = StoreJournal::new(store.snapshot());
        journal.write_volume(first, path(&["value"]), PublicValue::integer(1));
        journal.write_volume(second, path(&["value"]), PublicValue::integer(2));

        assert_eq!(store.try_commit(&journal), StoreCommitResult::Committed);
        assert_eq!(
            store.volume_root(first),
            journal.volume_view(first).as_ref()
        );
        assert_eq!(
            store.volume_root(second),
            journal.volume_view(second).as_ref()
        );
    }

    #[test]
    fn revoked_volume_rejects_staged_blind_edits_without_partial_commit() {
        let mut store = store();
        let revoked = store.create_volume(PublicValue::empty_record()).unwrap();
        let surviving = store.create_volume(PublicValue::empty_record()).unwrap();
        let original_surviving = store.volume_root(surviving).cloned().unwrap();
        let mut journal = StoreJournal::new(store.snapshot());
        journal.write_volume(revoked, Vec::new(), PublicValue::integer(1));
        journal.write_volume(surviving, Vec::new(), PublicValue::integer(2));
        assert!(store.revoke_volume(revoked).is_some());

        assert_eq!(
            store.try_commit(&journal),
            StoreCommitResult::MissingVolume(revoked)
        );
        assert_eq!(store.volume_root(surviving), Some(&original_surviving));
        assert!(store.volume_root(revoked).is_none());
    }

    #[test]
    fn revoked_volume_conflicts_with_an_earlier_read() {
        let mut store = store();
        let volume = store.create_volume(PublicValue::empty_record()).unwrap();
        let mut journal = StoreJournal::new(store.snapshot());
        assert!(journal.observe_volume_read(volume, &[]));
        assert!(store.revoke_volume(volume).is_some());

        assert_eq!(store.try_commit(&journal), StoreCommitResult::Conflict);
    }

    #[test]
    fn writes_never_materialize_a_missing_volume() {
        let mut store = store();
        let volume = store.create_volume(PublicValue::empty_record()).unwrap();
        assert!(store.revoke_volume(volume).is_some());
        let mut journal = StoreJournal::new(store.snapshot());
        journal.write_volume(volume, Vec::new(), PublicValue::integer(1));

        assert!(journal.volume_view(volume).is_none());
        assert_eq!(
            store.try_commit(&journal),
            StoreCommitResult::MissingVolume(volume)
        );
        assert!(store.volume_root(volume).is_none());
    }

    #[test]
    fn revoked_volume_ids_are_not_reused() {
        let mut store = store();
        let first = store.create_volume(PublicValue::empty_record()).unwrap();
        assert!(store.revoke_volume(first).is_some());
        let second = store.create_volume(PublicValue::empty_record()).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn covering_set_keeps_a_later_rewrite_read_internal() {
        let mut store = store();
        let snapshot = store.snapshot();
        let mut local = StoreJournal::new(snapshot.clone());
        local.write(path(&["x"]), PublicValue::empty_record());
        local.rewrite(path(&["x", "y"]), PublicValue::builtin(Builtin::Seq));
        assert!(!local.observe_read(&path(&["x", "y", "z"])));

        let mut concurrent = StoreJournal::new(snapshot);
        concurrent.write(path(&["x", "other"]), PublicValue::integer(1));
        assert_eq!(store.try_commit(&concurrent), StoreCommitResult::Committed);
        assert_eq!(store.try_commit(&local), StoreCommitResult::Committed);
    }

    #[test]
    fn rewrite_widens_a_descendant_read_dependency() {
        let mut store = store();
        let snapshot = store.snapshot();
        let mut local = StoreJournal::new(snapshot.clone());
        local.rewrite(path(&["x", "y"]), PublicValue::builtin(Builtin::Seq));
        assert!(local.observe_read(&path(&["x", "y", "z"])));

        let mut concurrent = StoreJournal::new(snapshot);
        concurrent.write(path(&["x", "y", "sibling"]), PublicValue::integer(1));
        assert_eq!(store.try_commit(&concurrent), StoreCommitResult::Committed);
        assert_eq!(store.try_commit(&local), StoreCommitResult::Conflict);
    }

    #[test]
    fn rebased_rewrites_apply_in_commit_order() {
        let assembler = Assembler::default();
        let module = assembler
            .module(["reflection_store_test"])
            .script(
                "g",
                "language g0\nappend_a = \\items -> items ++ [\"A\"]\nappend_b = \\items -> items ++ [\"B\"]\n",
            )
            .build()
            .expect("rewrite fixture should compile");
        let append_a = assembler
            .evaluate(&assembler.get(module.value(), "append_a").unwrap())
            .unwrap();
        let append_b = assembler
            .evaluate(&assembler.get(module.value(), "append_b").unwrap())
            .unwrap();

        let apply_in_order = |first: PublicValue, second: PublicValue| {
            let mut store = store();
            store.replace_root(PublicValue::list([PublicValue::text("base")]));
            let snapshot = store.snapshot();
            let mut first_edit = StoreJournal::new(snapshot.clone());
            first_edit.rewrite(Vec::new(), first);
            let mut second_edit = StoreJournal::new(snapshot);
            second_edit.rewrite(Vec::new(), second);
            assert_eq!(store.try_commit(&first_edit), StoreCommitResult::Committed);
            assert_eq!(store.try_commit(&second_edit), StoreCommitResult::Committed);
            assembler.evaluate(store.root()).unwrap()
        };

        assert_list_values(
            &assembler,
            &apply_in_order(append_a.clone(), append_b.clone()),
            &PublicValue::list([
                PublicValue::text("base"),
                PublicValue::text("A"),
                PublicValue::text("B"),
            ]),
        );
        assert_list_values(
            &assembler,
            &apply_in_order(append_b, append_a),
            &PublicValue::list([
                PublicValue::text("base"),
                PublicValue::text("B"),
                PublicValue::text("A"),
            ]),
        );
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

        assert_eq!(store.try_commit(&left), StoreCommitResult::Committed);
        assert_eq!(store.try_commit(&right), StoreCommitResult::Committed);
        assert_eq!(store.try_commit(&later_left), StoreCommitResult::Committed);
    }

    #[test]
    fn disjoint_nested_siblings_rebase() {
        let mut store = store();
        let mut establish_parent = StoreJournal::new(store.snapshot());
        establish_parent.write(path(&["tree"]), PublicValue::empty_record());
        assert_eq!(
            store.try_commit(&establish_parent),
            StoreCommitResult::Committed
        );

        let snapshot = store.snapshot();
        let mut left = StoreJournal::new(snapshot.clone());
        left.write(path(&["tree", "left"]), PublicValue::integer(1));
        let mut right = StoreJournal::new(snapshot);
        right.write(path(&["tree", "right"]), PublicValue::integer(2));

        assert_eq!(store.try_commit(&left), StoreCommitResult::Committed);
        assert_eq!(store.try_commit(&right), StoreCommitResult::Committed);
    }

    #[test]
    fn root_observation_conflicts_with_every_write() {
        let mut store = store();
        let snapshot = store.snapshot();
        let mut reader = StoreJournal::new(snapshot.clone());
        reader.observe_read(&[]);
        let mut writer = StoreJournal::new(snapshot);
        writer.write(path(&["anywhere"]), PublicValue::integer(1));

        assert_eq!(store.try_commit(&writer), StoreCommitResult::Committed);
        assert_eq!(store.try_commit(&reader), StoreCommitResult::Conflict);
    }

    #[test]
    fn reads_conflict_while_nested_blind_writes_serialize() {
        let mut store = store();
        let snapshot = store.snapshot();
        let mut reader = StoreJournal::new(snapshot.clone());
        reader.observe_read(&path(&["missing", "child"]));
        let mut nested_writer = StoreJournal::new(snapshot.clone());
        nested_writer.write(path(&["tree", "child"]), PublicValue::integer(1));
        let mut parent_writer = StoreJournal::new(snapshot.clone());
        parent_writer.write(path(&["tree"]), PublicValue::empty_record());
        let mut missing_writer = StoreJournal::new(snapshot);
        missing_writer.write(path(&["missing"]), PublicValue::empty_record());

        assert_eq!(
            store.try_commit(&nested_writer),
            StoreCommitResult::Committed
        );
        assert_eq!(
            store.try_commit(&parent_writer),
            StoreCommitResult::Committed
        );
        assert_eq!(
            store.try_commit(&missing_writer),
            StoreCommitResult::Committed
        );
        assert_eq!(store.try_commit(&reader), StoreCommitResult::Conflict);
    }

    #[test]
    fn overlapping_blind_writes_serialize_in_commit_order() {
        let mut store = store();
        let snapshot = store.snapshot();
        let mut child = StoreJournal::new(snapshot.clone());
        child.write(path(&["tree", "child"]), PublicValue::integer(1));
        let mut parent = StoreJournal::new(snapshot);
        parent.write(path(&["tree"]), PublicValue::integer(2));

        assert_eq!(store.try_commit(&child), StoreCommitResult::Committed);
        assert_eq!(store.try_commit(&parent), StoreCommitResult::Committed);
    }

    #[test]
    fn reads_after_covering_writes_do_not_observe_the_snapshot() {
        let mut store = store();
        let snapshot = store.snapshot();
        let mut local = StoreJournal::new(snapshot.clone());
        local.write(path(&["value"]), PublicValue::integer(1));
        assert!(!local.observe_read(&path(&["value"])));

        let mut concurrent = StoreJournal::new(snapshot);
        concurrent.write(path(&["value"]), PublicValue::integer(2));
        assert_eq!(store.try_commit(&concurrent), StoreCommitResult::Committed);
        assert_eq!(store.try_commit(&local), StoreCommitResult::Committed);
    }

    #[test]
    fn writes_do_not_erase_earlier_read_dependencies() {
        let mut store = store();
        let snapshot = store.snapshot();
        let mut local = StoreJournal::new(snapshot.clone());
        assert!(local.observe_read(&path(&["value"])));
        local.write(path(&["value"]), PublicValue::integer(1));

        let mut concurrent = StoreJournal::new(snapshot);
        concurrent.write(path(&["value"]), PublicValue::integer(2));
        assert_eq!(store.try_commit(&concurrent), StoreCommitResult::Committed);
        assert_eq!(store.try_commit(&local), StoreCommitResult::Conflict);
    }

    #[test]
    fn coarse_strategy_conflicts_after_any_observation() {
        let strategy = CoarseConflictAnalysis;
        let mut observations = strategy.begin();
        let volume = VolumeId::from_u64(1).unwrap();
        assert!(!observations.may_conflict(&address(volume, &[])));
        observations.observe(&address(volume, &["a"]));
        assert!(observations.may_conflict(&address(volume, &["z"])));
    }
}
