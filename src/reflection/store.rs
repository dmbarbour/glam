//! Journaled shared state for reflection tasks.
//!
//! Conflict-analysis policy lives in the `conflict` child; this owner retains
//! exact edits, query lifetime, snapshots, journals, and commits.

mod conflict;

#[cfg(test)]
mod tests;

pub use conflict::{
    CoarseConflictAnalysis, ConflictAddress, ConflictAnalysisStrategy, ConflictObservationIndex,
    ConflictPath, ExactConflictAnalysis, FingerprintConflictAnalysis,
};

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, LazyLock, Weak};

use rpds::RedBlackTreeMapSync;

use crate::api::{Value as PublicValue, Values};
use crate::core::{Builtin, CoreValueFactory, Dict, Key, LazyValue, List, Value};
use crate::core_net::CoreDataKey;
use crate::number::Number;

/// One shared-state partition within an evaluation runtime's reflection store.
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

/// Runtime-local identity of one admitted-input endpoint.
///
/// IDs are allocated monotonically by the evaluation runtime and are never
/// reused during that runtime's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeInputEndpointId(NonZeroU64);

impl RuntimeInputEndpointId {
    pub fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) fn from_u64(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }
}

/// Per-endpoint identity of one admitted input.
///
/// Sequence values are ordered only within their endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeInputSequence(u64);

impl RuntimeInputSequence {
    pub fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn from_u64(sequence: u64) -> Self {
        Self(sequence)
    }

    pub(crate) fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
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

#[derive(Debug, Clone)]
pub(crate) enum EvaluationQueryPoll {
    State {
        value: PublicValue,
        #[cfg_attr(not(test), allow(dead_code))]
        observed: bool,
    },
    ForeignQueryDomain,
}

/// Immutable heap state captured at the beginning of a transaction.
#[derive(Clone)]
pub struct StoreSnapshot {
    // Revisions are store-local; identity prevents a coincidental revision
    // match from enabling the cached-view commit path for another store.
    identity: Arc<()>,
    revision: u64,
    heap_volume: VolumeId,
    runtime_volume: VolumeId,
    query_domain: Arc<QueryDomain>,
    roots: RedBlackTreeMapSync<VolumeId, PublicValue>,
    strategy: Arc<dyn ConflictAnalysisStrategy>,
    values: CoreValueFactory,
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
            return EvaluationQueryPoll::ForeignQueryDomain;
        }
        let values = Values::from_core_factory(self.values.clone());
        let Some(root) = self.volume(self.runtime_volume) else {
            return EvaluationQueryPoll::State {
                value: values.empty_dict(),
                observed: true,
            };
        };
        EvaluationQueryPoll::State {
            value: values.wrap(lazy_core_value_path(
                &self.values,
                values
                    .clone_core(root)
                    .expect("query root belongs to its store runtime"),
                &query_path(handle.id),
            )),
            observed: true,
        }
    }
}

#[derive(Clone)]
enum StoreEdit {
    Set {
        address: ConflictAddress,
        value: PublicValue,
    },
    Rewrite {
        address: ConflictAddress,
        updater: PublicValue,
    },
}

impl StoreEdit {
    fn address(&self) -> &ConflictAddress {
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
        let address = ConflictAddress::reflection(volume, ConflictPath::from_keys(path.to_vec()));
        if self.snapshot.volume(volume).is_none() {
            self.observations.observe(&address);
            return true;
        }
        let mut dependency = ConflictPath::from_keys(path.to_vec());
        for edit in self.edits.iter().rev() {
            let (edit_volume, edit_path) = edit.address().reflection_parts();
            match edit {
                StoreEdit::Set { .. }
                    if edit_volume == volume && edit_path.is_prefix_of(&dependency) =>
                {
                    return false;
                }
                StoreEdit::Rewrite { .. }
                    if edit_volume == volume && edit_path.overlaps(&dependency) =>
                {
                    if edit_path.is_prefix_of(&dependency) {
                        dependency = edit_path.clone();
                    }
                }
                StoreEdit::Set { .. } | StoreEdit::Rewrite { .. } => {}
            }
        }
        self.observations
            .observe(&ConflictAddress::reflection(volume, dependency));
        true
    }

    pub(crate) fn view(&self) -> PublicValue {
        self.volume_view(self.snapshot.heap_volume)
            .expect("the user heap volume must always exist")
    }

    pub(crate) fn volume_view(&self, volume: VolumeId) -> Option<PublicValue> {
        self.views.get(&volume).cloned()
    }

    #[cfg(test)]
    pub(crate) fn reserve_query(&mut self) -> Result<Arc<EvaluationQueryHandle>, Arc<str>> {
        self.reserve_query_state(pending_query_value(&self.snapshot.values))
    }

    pub(crate) fn reserve_query_with(
        &mut self,
        result: PublicValue,
    ) -> Result<Arc<EvaluationQueryHandle>, Arc<str>> {
        self.reserve_query_state(complete_query_value(&self.snapshot.values, result))
    }

    fn reserve_query_state(
        &mut self,
        state: PublicValue,
    ) -> Result<Arc<EvaluationQueryHandle>, Arc<str>> {
        let handle = self.snapshot.query_domain.allocate()?;
        self.write_volume(self.snapshot.runtime_volume, query_path(handle.id), state);
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
        self.observe_volume_read(self.snapshot.runtime_volume, &path)
    }

    fn peek_query_with_observation(
        &self,
        handle: &Arc<EvaluationQueryHandle>,
        observed: bool,
    ) -> EvaluationQueryPoll {
        if !query_belongs_to(&self.snapshot.query_domain, handle) {
            return EvaluationQueryPoll::ForeignQueryDomain;
        }
        let values = Values::from_core_factory(self.snapshot.values.clone());
        let Some(root) = self.volume_view(self.snapshot.runtime_volume) else {
            return EvaluationQueryPoll::State {
                value: values.empty_dict(),
                observed,
            };
        };
        EvaluationQueryPoll::State {
            value: values.wrap(lazy_core_value_path(
                &self.snapshot.values,
                values
                    .clone_core(&root)
                    .expect("query view belongs to its store runtime"),
                &query_path(handle.id),
            )),
            observed,
        }
    }

    pub(crate) fn write(&mut self, path: Vec<Key>, value: PublicValue) {
        self.write_volume(self.snapshot.heap_volume, path, value);
    }

    pub(crate) fn write_volume(&mut self, volume: VolumeId, path: Vec<Key>, value: PublicValue) {
        debug_assert_eq!(value.runtime_id(), self.snapshot.values.runtime_id());
        let edit = StoreEdit::Set {
            address: ConflictAddress::reflection(volume, ConflictPath::from_keys(path)),
            value,
        };
        if let Some(view) = self.views.get(&volume).cloned() {
            self.views
                .insert_mut(volume, apply_edit(&self.snapshot.values, view, &edit));
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
        debug_assert_eq!(updater.runtime_id(), self.snapshot.values.runtime_id());
        let edit = StoreEdit::Rewrite {
            address: ConflictAddress::reflection(volume, ConflictPath::from_keys(path)),
            updater,
        };
        if let Some(view) = self.views.get(&volume).cloned() {
            self.views
                .insert_mut(volume, apply_edit(&self.snapshot.values, view, &edit));
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
    runtime_volume: VolumeId,
    query_domain: Arc<QueryDomain>,
    query_retirements: Receiver<EvaluationQueryId>,
    next_volume: u64,
    roots: RedBlackTreeMapSync<VolumeId, PublicValue>,
    revision: u64,
    latest_changes: BTreeMap<ConflictAddress, u64>,
    strategy: Arc<dyn ConflictAnalysisStrategy>,
    values: CoreValueFactory,
}

impl ReflectionStore {
    pub(crate) fn new(
        values: CoreValueFactory,
        strategy: Arc<dyn ConflictAnalysisStrategy>,
    ) -> Self {
        let heap_volume = VolumeId::from_u64(1).expect("one is a nonzero volume ID");
        let runtime_volume = VolumeId::from_u64(2).expect("two is a nonzero volume ID");
        let (query_domain, query_retirements) = QueryDomain::new();
        let public_values = Values::from_core_factory(values.clone());
        Self {
            identity: Arc::new(()),
            heap_volume,
            runtime_volume,
            query_domain,
            query_retirements,
            next_volume: 3,
            roots: RedBlackTreeMapSync::new_sync()
                .insert(heap_volume, public_values.empty_dict())
                .insert(runtime_volume, public_values.empty_dict()),
            revision: 0,
            latest_changes: BTreeMap::new(),
            strategy,
            values,
        }
    }

    #[doc(hidden)]
    pub fn snapshot(&self) -> StoreSnapshot {
        StoreSnapshot {
            identity: self.identity.clone(),
            revision: self.revision,
            heap_volume: self.heap_volume,
            runtime_volume: self.runtime_volume,
            query_domain: self.query_domain.clone(),
            roots: self.roots.clone(),
            strategy: self.strategy.clone(),
            values: self.values.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &PublicValue {
        self.roots
            .get(&self.heap_volume)
            .expect("the user heap volume must always exist")
    }

    #[cfg(test)]
    pub(crate) fn values(&self) -> &CoreValueFactory {
        &self.values
    }

    #[cfg(test)]
    fn volume_root(&self, volume: VolumeId) -> Option<&PublicValue> {
        self.roots.get(&volume)
    }

    #[doc(hidden)]
    pub fn strategy(&self) -> Arc<dyn ConflictAnalysisStrategy> {
        self.strategy.clone()
    }

    #[doc(hidden)]
    pub fn replace_root(&mut self, root: PublicValue) {
        debug_assert_eq!(root.runtime_id(), self.values.runtime_id());
        self.roots.insert_mut(self.heap_volume, root);
        self.revision = self.revision.wrapping_add(1);
        self.latest_changes.insert(
            ConflictAddress::reflection_root(self.heap_volume),
            self.revision,
        );
    }

    pub(crate) fn create_volume(&mut self, initial: PublicValue) -> Result<VolumeId, Arc<str>> {
        debug_assert_eq!(initial.runtime_id(), self.values.runtime_id());
        let volume = VolumeId::from_u64(self.next_volume)
            .ok_or_else(|| Arc::from("reflection volume IDs exhausted"))?;
        self.next_volume = self
            .next_volume
            .checked_add(1)
            .ok_or_else(|| Arc::from("reflection volume IDs exhausted"))?;
        self.roots.insert_mut(volume, initial);
        self.revision = self.revision.wrapping_add(1);
        self.latest_changes
            .insert(ConflictAddress::reflection_root(volume), self.revision);
        Ok(volume)
    }

    pub(crate) fn revoke_volume(&mut self, volume: VolumeId) -> Option<PublicValue> {
        if volume == self.heap_volume || volume == self.runtime_volume {
            return None;
        }
        let root = self.roots.get(&volume).cloned()?;
        self.roots.remove_mut(&volume);
        self.revision = self.revision.wrapping_add(1);
        self.latest_changes
            .insert(ConflictAddress::reflection_root(volume), self.revision);
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
        if self.roots.get(&self.runtime_volume).is_none() {
            return false;
        }
        let mut journal = StoreJournal::new(self.snapshot());
        journal.write_volume(
            self.runtime_volume,
            query_path(handle.id),
            complete_query_value(&self.values, result),
        );
        matches!(self.try_commit(&journal), StoreCommitResult::Committed)
    }

    /// Validates and commits a journal. Exact edit paths and rebase policy
    /// remain independent of the selected read-analysis strategy.
    #[doc(hidden)]
    pub fn try_commit(&mut self, journal: &StoreJournal) -> StoreCommitResult {
        self.try_commit_with_change(journal).0
    }

    /// Validates and commits a journal while preserving whether it changed
    /// observable store state. Runtime publication uses this distinction to
    /// avoid disturbing broad observers for a successful no-op validation.
    pub(crate) fn try_commit_with_change(
        &mut self,
        journal: &StoreJournal,
    ) -> (StoreCommitResult, bool) {
        let validation = self.validate(journal);
        if !matches!(validation, StoreCommitResult::Committed) {
            return (validation, false);
        }
        let changed = self.commit_validated(journal);
        (StoreCommitResult::Committed, changed)
    }

    /// Validates a journal without changing the store. The caller must retain
    /// exclusive access to the store until [`commit_validated`](Self::commit_validated)
    /// is called.
    pub(crate) fn validate(&self, journal: &StoreJournal) -> StoreCommitResult {
        if let Some(volume) = journal
            .edits
            .iter()
            .map(|edit| edit.address().reflection_parts().0)
            .find(|volume| self.roots.get(volume).is_none())
        {
            return StoreCommitResult::MissingVolume(volume);
        }
        if self.conflicts(journal) {
            return StoreCommitResult::Conflict;
        }
        StoreCommitResult::Committed
    }

    /// Applies a journal already accepted by [`validate`](Self::validate).
    /// Returns whether the store changed, including private query retirement.
    pub(crate) fn commit_validated(&mut self, journal: &StoreJournal) -> bool {
        if journal.edits.is_empty() {
            return self.retire_queries();
        }

        self.roots = if Arc::ptr_eq(&self.identity, &journal.snapshot.identity)
            && self.revision == journal.snapshot.revision
        {
            journal.views.clone()
        } else {
            apply_edits(&self.values, self.roots.clone(), &journal.edits)
        };
        self.revision = self.revision.wrapping_add(1);
        for path in normalized_edit_paths(&journal.edits) {
            self.latest_changes.insert(path, self.revision);
        }
        self.retire_queries();
        true
    }

    fn retire_queries(&mut self) -> bool {
        let retired = self.query_retirements.try_iter().collect::<Vec<_>>();
        if retired.is_empty() {
            return false;
        }
        let Some(mut root) = self.roots.get(&self.runtime_volume).cloned() else {
            return false;
        };
        self.revision = self.revision.wrapping_add(1);
        for id in retired {
            let path = ConflictPath::from_keys(query_path(id));
            root = apply_value_at_path(&self.values, root, &path, Value::Dict(Dict::new_sync()));
            self.latest_changes.insert(
                ConflictAddress::reflection(self.runtime_volume, path),
                self.revision,
            );
        }
        self.roots.insert_mut(self.runtime_volume, root);
        true
    }

    fn conflicts(&self, journal: &StoreJournal) -> bool {
        self.latest_changes.iter().any(|(changed, revision)| {
            *revision > journal.snapshot.revision && journal.observations.may_conflict(changed)
        })
    }
}

fn normalized_edit_paths(edits: &[StoreEdit]) -> Vec<ConflictAddress> {
    let mut addresses = BTreeSet::new();
    for edit in edits {
        let edit_address = edit.address();
        if addresses
            .iter()
            .any(|address: &ConflictAddress| address.is_prefix_of(edit_address))
        {
            continue;
        }
        addresses.retain(|address| !edit_address.is_prefix_of(address));
        addresses.insert(edit_address.clone());
    }
    addresses.into_iter().collect()
}

fn apply_edits(
    values: &CoreValueFactory,
    mut roots: RedBlackTreeMapSync<VolumeId, PublicValue>,
    edits: &[StoreEdit],
) -> RedBlackTreeMapSync<VolumeId, PublicValue> {
    for edit in edits {
        let volume = edit.address().reflection_parts().0;
        let root = roots
            .get(&volume)
            .cloned()
            .expect("commit validates every edited volume before replay");
        roots.insert_mut(volume, apply_edit(values, root, edit));
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
static QUERY_NAMESPACE: LazyLock<Key> = LazyLock::new(|| Key::atom_from_text("queries"));

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
    vec![
        QUERY_NAMESPACE.clone(),
        Key::Number(Number::from_u64(id.get())),
    ]
}

#[cfg(test)]
fn pending_query_value(values: &CoreValueFactory) -> PublicValue {
    Values::from_core_factory(values.clone()).wrap(Value::Dict(
        Dict::new_sync().insert(QUERY_PENDING.clone(), values.unit()),
    ))
}

fn complete_query_value(values: &CoreValueFactory, result: PublicValue) -> PublicValue {
    let public_values = Values::from_core_factory(values.clone());
    let payload = Value::Dict(
        Dict::new_sync()
            .insert(QUERY_PRESENT.clone(), values.unit())
            .insert(
                QUERY_RESULT.clone(),
                public_values
                    .clone_core(&result)
                    .expect("query result belongs to its store runtime"),
            ),
    );
    public_values.wrap(Value::Dict(
        Dict::new_sync().insert(QUERY_COMPLETE.clone(), payload),
    ))
}

pub(crate) fn decode_query_state(values: &Values, value: &Value) -> Option<EvaluationQueryState> {
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
    Some(EvaluationQueryState::Complete(
        values.wrap(
            complete
                .get(&QUERY_RESULT)
                .cloned()
                .unwrap_or_else(|| Value::Dict(Dict::new_sync())),
        ),
    ))
}

fn apply_edit(values: &CoreValueFactory, root: PublicValue, edit: &StoreEdit) -> PublicValue {
    let public_values = Values::from_core_factory(values.clone());
    match edit {
        StoreEdit::Set { address, value } => {
            let (_, path) = address.reflection_parts();
            apply_value_at_path(
                values,
                root,
                path,
                public_values
                    .clone_core(value)
                    .expect("store edit belongs to its store runtime"),
            )
        }
        StoreEdit::Rewrite { address, updater } => {
            let (_, path) = address.reflection_parts();
            let prior = lazy_core_value_path(
                values,
                public_values
                    .clone_core(&root)
                    .expect("store root belongs to its store runtime"),
                path.keys(),
            );
            let updated = Value::Lazy(LazyValue::from_application(
                values,
                public_values
                    .clone_core(updater)
                    .expect("store updater belongs to its store runtime"),
                Arc::from([prior]),
            ));
            apply_value_at_path(values, root, path, updated)
        }
    }
}

fn apply_value_at_path(
    values: &CoreValueFactory,
    root: PublicValue,
    path: &ConflictPath,
    value: Value,
) -> PublicValue {
    let public_values = Values::from_core_factory(values.clone());
    if path.depth() == 0 {
        return public_values.wrap(value);
    }
    let path = Value::List(List::from_values(
        path.keys()
            .iter()
            .map(|key| key.to_value_with(values))
            .collect(),
    ));
    public_values.wrap(Value::builtin_call(
        values,
        Builtin::DictUpdate,
        vec![
            path,
            value,
            public_values
                .clone_core(&root)
                .expect("store root belongs to its store runtime"),
        ],
    ))
}

fn lazy_core_value_path(values: &CoreValueFactory, value: Value, path: &[Key]) -> Value {
    if path.is_empty() {
        return value;
    }
    Value::Lazy(LazyValue::from_access(
        values,
        Arc::from(
            path.iter()
                .cloned()
                .map(CoreDataKey::Key)
                .collect::<Vec<_>>(),
        ),
        Arc::from([value]),
    ))
}
