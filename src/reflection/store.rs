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
use std::sync::{Arc, LazyLock, Mutex, Weak};

use rpds::RedBlackTreeMapSync;

use super::promises::{PromiseId, PromiseStatus, Promises, Resolution, Settled};
use crate::api::{Value as PublicValue, Values};
pub use crate::core::VolumeId;
use crate::core::{
    Builtin, CoreValueFactory, Dict, Key, LazyValue, List, ManagedResolverRoot, ManagedVolumeDraft,
    ManagedVolumeRoot, ManagedVolumeVersion, RuntimeValueAccess, Value, VolumeCellView,
    VolumeContentsView,
};
use crate::core_net::CoreDataKey;
use crate::number::Number;

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
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
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
        #[cfg_attr(
            not(test),
            expect(
                dead_code,
                reason = "production callers ignore whether polling observed the query; tests check it"
            )
        )]
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
    environment_volume: VolumeId,
    /// The store's next volume ID, which a transaction reserves from.
    next_volume: Arc<AtomicU64>,
    promises: Arc<Promises>,
    /// The cells of the volumes the store kept, which the snapshot keeps
    /// too.
    kept: RedBlackTreeMapSync<VolumeId, ManagedVolumeRoot>,
    query_domain: Arc<QueryDomain>,
    /// Each volume's committed version, which keeps its value only while
    /// the volume's cell is reachable (`api-volume-cells`).
    versions: RedBlackTreeMapSync<VolumeId, ManagedVolumeVersion>,
    strategy: Arc<dyn ConflictAnalysisStrategy>,
    values: CoreValueFactory,
}

impl StoreSnapshot {
    /// The shared heap's handle, as `.heap.vol` gives it.
    pub(crate) fn heap_handle(&self, access: &RuntimeValueAccess<'_>) -> Value {
        self.kept
            .get(&self.heap_volume)
            .expect("the store keeps the user heap volume")
            .handle_in(access)
    }

    /// The store's promises, which its snapshots share
    /// (`requests-travel-on-ports`).
    pub(crate) fn promises(&self) -> &Arc<Promises> {
        &self.promises
    }

    /// The value at `path` in `volume`, as a lazy access, or `None` where
    /// the volume does not exist.
    fn volume_value(&self, volume: VolumeId, path: &[Key]) -> Option<PublicValue> {
        version_value_at_path(&self.values, self.versions.get(&volume)?, path)
    }

    /// Whether `volume` exists and reads in this snapshot.
    #[cfg(test)]
    pub(crate) fn holds_volume_for_test(&self, volume: VolumeId) -> bool {
        self.volume_value(volume, &[]).is_some()
    }

    /// The shared heap's committed value.
    #[doc(hidden)]
    pub fn root(&self) -> PublicValue {
        self.committed(self.heap_volume)
            .expect("the user heap volume must always exist")
    }

    /// The value at `path` in the user heap, as a lazy access, so nothing is
    /// evaluated: a missing key reads `{}`.
    pub(crate) fn heap_value(&self, path: &[Key]) -> PublicValue {
        self.volume_value(self.heap_volume, path)
            .expect("the user heap volume must always exist")
    }

    /// The value at `path` in the machines' environment volume, as a lazy
    /// access: a missing key reads `{}`.
    pub(crate) fn environment_value(&self, path: &[Key]) -> PublicValue {
        self.volume_value(self.environment_volume, path)
            .expect("the environment volume must always exist")
    }

    /// The committed value of `volume`, or `None` where it does not exist.
    fn committed(&self, volume: VolumeId) -> Option<PublicValue> {
        version_value_at_path(&self.values, self.versions.get(&volume)?, &[])
    }

    /// As [`Self::committed`], for tests.
    #[cfg(test)]
    pub(crate) fn committed_for_test(&self, volume: VolumeId) -> Option<PublicValue> {
        self.committed(volume)
    }

    /// The cell of a host's protected volume, which its capability names by
    /// ID, viewed, where the store kept it: until the host revokes it.
    pub(crate) fn kept_volume_in<'access>(
        &self,
        access: &'access RuntimeValueAccess<'_>,
        volume: VolumeId,
    ) -> Option<VolumeCellView<'access>> {
        Some(self.kept.get(&volume)?.view_in(access))
    }

    /// The committed contents of `volume`, viewed, or `None` where the
    /// volume does not exist in this snapshot.
    pub(crate) fn read_volume_in<'access>(
        &self,
        access: &'access RuntimeValueAccess<'_>,
        volume: VolumeCellView<'_>,
    ) -> Option<VolumeContentsView<'access>> {
        self.versions.get(&volume.id())?.contents_of(access, volume)
    }

    /// Whether `volume` exists in this snapshot.
    fn has_volume(&self, volume: VolumeId) -> bool {
        self.versions.contains_key(&volume)
    }

    pub(crate) fn poll_query(&self, handle: &Arc<EvaluationQueryHandle>) -> EvaluationQueryPoll {
        if !query_belongs_to(&self.query_domain, handle) {
            return EvaluationQueryPoll::ForeignQueryDomain;
        }
        let values = Values::from_core_factory(self.values.clone());
        let value = self
            .volume_value(self.runtime_volume, &query_path(handle.id))
            .unwrap_or_else(|| values.empty_dict());
        EvaluationQueryPoll::State {
            value,
            observed: true,
        }
    }
}

/// An edit at an address. One without data writes a virtual resource: it
/// joins conflict analysis as its data counterpart does, but changes no
/// volume.
#[derive(Clone)]
enum StoreEdit {
    Set {
        address: ConflictAddress,
        value: Option<PublicValue>,
    },
    Rewrite {
        address: ConflictAddress,
        updater: Option<PublicValue>,
    },
    /// Allocates a volume whose ID the transaction reserved, with its
    /// initial root, at the volume's root address. The edit holds the
    /// volume's cell until the commit keys its first version on it.
    Create {
        address: ConflictAddress,
        initial: PublicValue,
        volume: ManagedVolumeRoot,
    },
    /// Resolves a promise, at its place in the runtime's volume, which it
    /// writes without changing data (`promise-resolution-commits-with-its-cut`).
    /// The edit holds the resolver's breaker until the transaction ends, so a
    /// staged resolution never races the breaker's death; a lost resolver's
    /// break has none.
    Resolve {
        address: ConflictAddress,
        promise: PromiseId,
        resolver: Option<ManagedResolverRoot>,
        resolution: Resolution,
    },
}

impl StoreEdit {
    fn address(&self) -> &ConflictAddress {
        match self {
            Self::Set { address, .. }
            | Self::Rewrite { address, .. }
            | Self::Create { address, .. }
            | Self::Resolve { address, .. } => address,
        }
    }
}

/// The shared heap's volume, the same in every store.
pub(super) fn heap_volume() -> VolumeId {
    VolumeId::from_u64(1).expect("one is a nonzero volume ID")
}

/// Reserves the next volume ID from `next`, shared by a store and its
/// snapshots, so a transaction reserves one without the store's lock.
fn reserve_volume(next: &AtomicU64) -> Result<VolumeId, Arc<str>> {
    next.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .ok()
        .and_then(VolumeId::from_u64)
        .ok_or_else(|| Arc::from("reflection volume IDs exhausted"))
}

/// Reads and ordered edits accumulated by one optimistic transaction.
///
/// Clones are alternative views of the same transaction attempt: they share
/// the monotone observation set while retaining independent speculative views
/// and edit overlays. Dropping a failed alternative therefore rolls back its
/// writes without forgetting the reads which influenced the enclosing cut.
#[derive(Clone)]
pub struct StoreJournal {
    snapshot: StoreSnapshot,
    /// Drafts of the volumes this journal has written, with its writes: the
    /// others read from the snapshot. Its commit keys each volume's new
    /// version on its draft.
    views: RedBlackTreeMapSync<VolumeId, ManagedVolumeDraft>,
    /// The cells of the volumes this journal reads or writes, which it keeps
    /// until its transaction ends.
    cells: RedBlackTreeMapSync<VolumeId, ManagedVolumeRoot>,
    observations: Arc<Mutex<Box<dyn ConflictObservationIndex>>>,
    edits: Vec<StoreEdit>,
    branch: Option<Branch>,
}

/// A branch of a journal, for concurrent work within one transaction that
/// merges back at its join ([`StoreJournal::branch`]).
#[derive(Clone)]
struct Branch {
    /// How many edits the journal held at the fork: the branch's own edits
    /// follow them.
    base: usize,
    /// What the branch read of the transaction's state at the fork, which
    /// the journal's later edits must not overlap at the merge. Clones of
    /// the branch, its alternatives, share it, as they share observations.
    reads: Arc<Mutex<Vec<ConflictAddress>>>,
}

impl StoreJournal {
    /// The snapshot the journal began from.
    pub(crate) fn snapshot(&self) -> &StoreSnapshot {
        &self.snapshot
    }

    #[doc(hidden)]
    pub fn new(snapshot: StoreSnapshot) -> Self {
        let observations = Arc::new(Mutex::new(snapshot.strategy.begin()));
        Self {
            snapshot,
            views: RedBlackTreeMapSync::new_sync(),
            cells: RedBlackTreeMapSync::new_sync(),
            observations,
            edits: Vec::new(),
            branch: None,
        }
    }

    /// A branch of this journal, for work concurrent with the rest of its
    /// transaction. It shares the transaction's observations, so its reads
    /// join the transaction's validation, and it records them apart too,
    /// for its merge.
    pub(crate) fn branch(&self) -> Self {
        let mut branch = self.clone();
        branch.branch = Some(Branch {
            base: self.edits.len(),
            reads: Arc::new(Mutex::new(Vec::new())),
        });
        branch
    }

    /// Merges `branch`, a branch of this journal, as if its work ran now:
    /// its edits replay onto this journal. It fails, merging nothing, when
    /// the branch read what this journal wrote after the fork, so that no
    /// serial order explains the branch's reads.
    pub(crate) fn merge_branch(&mut self, branch: &StoreJournal) -> bool {
        let Branch { base, reads } = branch
            .branch
            .as_ref()
            .expect("only a branch merges into its journal");
        let base = *base;
        let reads = reads.lock().unwrap().clone();
        let Some(later) = self.edits.get(base..) else {
            return false;
        };
        if reads
            .iter()
            .any(|read| later.iter().any(|edit| edit.address().overlaps(read)))
        {
            return false;
        }
        // What the branch read, this journal's own edits before the fork may
        // cover or widen, as for a read of its own.
        if let Some(Branch {
            base: own_base,
            reads: own_reads,
        }) = &self.branch
        {
            let own = self.edits.get(*own_base..base).unwrap_or_default();
            let inherited = reads.iter().filter_map(|read| {
                let (volume, path) = read.reflection_parts();
                dependency(own, volume, path.clone())
                    .map(|path| ConflictAddress::reflection(volume, path))
            });
            own_reads.lock().unwrap().extend(inherited);
        }
        for (volume, cell) in branch.cells.iter() {
            self.cells.insert_mut(*volume, cell.clone());
        }
        let values = self.snapshot.values.clone();
        values.with_boundary_access(|access| {
            for edit in branch.edits[base..].iter().cloned() {
                self.replay(&access, edit);
            }
        });
        true
    }

    fn replay(&mut self, access: &RuntimeValueAccess<'_>, edit: StoreEdit) {
        match &edit {
            StoreEdit::Set { value: None, .. }
            | StoreEdit::Rewrite { updater: None, .. }
            | StoreEdit::Resolve { .. } => {
                self.edits.push(edit);
            }
            StoreEdit::Set { .. } | StoreEdit::Rewrite { .. } => {
                let volume = edit.address().reflection_parts().0;
                self.push_edit(access, volume, edit);
            }
            StoreEdit::Create {
                initial,
                volume: cell,
                ..
            } => {
                let volume = edit.address().reflection_parts().0;
                self.views.insert_mut(
                    volume,
                    access.volume_draft(initial.runtime_root().clone_core_with(access)),
                );
                self.cells.insert_mut(volume, cell.clone());
                self.edits.push(edit);
            }
        }
    }

    /// Allocates a volume with `initial` as its root, returning a handle to
    /// it: its ID is reserved at once, and the volume exists from the commit
    /// on, so an aborted transaction burns the ID. This journal reads and
    /// writes it as any other volume.
    pub(crate) fn create_volume(&mut self, initial: PublicValue) -> Result<PublicValue, Arc<str>> {
        self.with_access(|journal, access| {
            let volume = journal.create_volume_in(access, initial)?;
            Ok(PublicValue::from_runtime_root(
                access.root_runtime_value(volume.handle_in(access)),
            ))
        })
    }

    /// As [`Self::create_volume`], under `access`, returning the volume's
    /// cell, viewed.
    pub(crate) fn create_volume_in<'access>(
        &mut self,
        access: &'access RuntimeValueAccess<'_>,
        initial: PublicValue,
    ) -> Result<VolumeCellView<'access>, Arc<str>> {
        debug_assert_eq!(initial.runtime_id(), self.snapshot.values.runtime_id());
        let volume = reserve_volume(&self.snapshot.next_volume)?;
        let cell = access.new_volume(volume);
        let view = cell.view_in(access);
        self.replay(
            access,
            StoreEdit::Create {
                address: ConflictAddress::reflection_root(volume),
                initial,
                volume: cell,
            },
        );
        Ok(view)
    }

    /// Runs `operation` on this journal under a fresh access region.
    pub(crate) fn with_access<R>(
        &mut self,
        operation: impl FnOnce(&mut Self, &RuntimeValueAccess<'_>) -> R,
    ) -> R {
        let values = self.snapshot.values.clone();
        values.with_boundary_access(|access| operation(self, &access))
    }

    /// Reads `volume`, returning its contents with this journal's writes,
    /// viewed, or `None` where the volume does not exist in the journal's
    /// snapshot. The journal keeps the volume until its transaction ends
    /// (`api-volume-cells`).
    pub(crate) fn read_volume_in<'access>(
        &mut self,
        access: &'access RuntimeValueAccess<'_>,
        volume: VolumeCellView<'_>,
    ) -> Option<VolumeContentsView<'access>> {
        self.keep_volume_in(access, volume);
        match self.views.get(&volume.id()) {
            Some(draft) => Some(draft.view_in(access)),
            None => self
                .snapshot
                .versions
                .get(&volume.id())?
                .contents_of(access, volume),
        }
    }

    /// Records a read at `path` in `volume`, as [`Self::observe_read`] does
    /// in the user heap.
    pub(crate) fn observe_volume_read(&mut self, volume: VolumeCellView<'_>, path: &[Key]) -> bool {
        self.observe_read_by_id(volume.id(), path)
    }

    /// Writes `value` at `path` in `volume`, keeping the volume until the
    /// transaction ends.
    pub(crate) fn write_volume_in(
        &mut self,
        access: &RuntimeValueAccess<'_>,
        volume: VolumeCellView<'_>,
        path: Vec<Key>,
        value: PublicValue,
    ) {
        self.keep_volume_in(access, volume);
        self.write_by_id_in(access, volume.id(), path, value);
    }

    /// Rewrites the value at `path` in `volume` with `updater`, as
    /// [`Self::write_volume_in`] writes.
    pub(crate) fn rewrite_volume_in(
        &mut self,
        access: &RuntimeValueAccess<'_>,
        volume: VolumeCellView<'_>,
        path: Vec<Key>,
        updater: PublicValue,
    ) {
        self.keep_volume_in(access, volume);
        self.rewrite_by_id_in(access, volume.id(), path, updater);
    }

    /// Records a read at `path` in a host's protected volume, which its
    /// capability names by ID.
    pub(crate) fn observe_kept_volume_read(&mut self, volume: VolumeId, path: &[Key]) -> bool {
        self.observe_read_by_id(volume, path)
    }

    /// Writes `value` at `path` in a host's protected volume, which its
    /// capability names by ID: blindly, so a write to one the host revoked
    /// fails at the commit.
    pub(crate) fn write_kept_volume_in(
        &mut self,
        access: &RuntimeValueAccess<'_>,
        volume: VolumeId,
        path: Vec<Key>,
        value: PublicValue,
    ) {
        self.write_by_id_in(access, volume, path, value);
    }

    /// As [`Self::write_kept_volume_in`], for a rewrite.
    pub(crate) fn rewrite_kept_volume_in(
        &mut self,
        access: &RuntimeValueAccess<'_>,
        volume: VolumeId,
        path: Vec<Key>,
        updater: PublicValue,
    ) {
        self.rewrite_by_id_in(access, volume, path, updater);
    }

    /// The value at `path` in `volume`, with this journal's writes, as a
    /// lazy access, or `None` where the volume does not exist.
    fn volume_value(&self, volume: VolumeId, path: &[Key]) -> Option<PublicValue> {
        self.snapshot
            .values
            .try_construct_runtime_value_root(|access| {
                self.value_at_in(access, volume, path).ok_or(())
            })
            .ok()
            .map(PublicValue::from_runtime_root)
    }

    /// As [`Self::volume_value`], under `access`, left to the caller to
    /// root.
    fn value_at_in(
        &self,
        access: &RuntimeValueAccess<'_>,
        volume: VolumeId,
        path: &[Key],
    ) -> Option<Value> {
        let value = match self.views.get(&volume) {
            Some(draft) => access.duplicate_value(draft.view_in(access).value_in(access)),
            None => self.snapshot.versions.get(&volume)?.value_in(access)?,
        };
        Some(lazy_core_value_path(access, value, path))
    }

    /// Keeps `volume` until this journal's transaction ends.
    fn keep_volume_in(&mut self, access: &RuntimeValueAccess<'_>, volume: VolumeCellView<'_>) {
        if !self.cells.contains_key(&volume.id()) {
            self.cells
                .insert_mut(volume.id(), access.keep_volume(volume));
        }
    }

    /// The cell of `volume`, viewed: the one this journal keeps, else its
    /// snapshot version's.
    fn volume_cell_in<'access>(
        &self,
        access: &'access RuntimeValueAccess<'_>,
        volume: VolumeId,
    ) -> Option<VolumeCellView<'access>> {
        match self.cells.get(&volume) {
            Some(cell) => Some(cell.view_in(access)),
            None => self.snapshot.versions.get(&volume)?.cell_in(access),
        }
    }

    /// Records the portion of the snapshot needed by this read. Local rewrites
    /// may widen that dependency; an earlier covering set makes it internal.
    /// Earlier observations remain intact.
    pub(crate) fn observe_read(&mut self, path: &[Key]) -> bool {
        self.observe_read_by_id(self.snapshot.heap_volume, path)
    }

    /// Records a read at `path` in the machines' environment volume, as
    /// [`Self::observe_read`] does in the user heap.
    pub(crate) fn observe_environment_read(&mut self, path: &[Key]) -> bool {
        self.observe_read_by_id(self.snapshot.environment_volume, path)
    }

    /// The value at `path` in the machines' environment volume, with this
    /// journal's writes, as a lazy access: a missing key reads `{}`.
    pub(crate) fn environment_value(&self, path: &[Key]) -> PublicValue {
        self.volume_value(self.snapshot.environment_volume, path)
            .expect("the environment volume must always exist")
    }

    /// Writes `value` at `path` in the machines' environment volume.
    pub(crate) fn write_environment(&mut self, path: Vec<Key>, value: PublicValue) {
        self.write_by_id(self.snapshot.environment_volume, path, value);
    }

    fn observe_read_by_id(&mut self, volume: VolumeId, path: &[Key]) -> bool {
        let path = ConflictPath::from_keys(path.to_vec());
        // A branch's read depends on the transaction's state at the fork,
        // which the branch's own edits alone may cover.
        if let Some(Branch { base, reads }) = &self.branch
            && let Some(read) = dependency(&self.edits[*base..], volume, path.clone())
        {
            reads
                .lock()
                .unwrap()
                .push(ConflictAddress::reflection(volume, read));
        }
        if !self.snapshot.has_volume(volume) {
            self.observations
                .lock()
                .unwrap()
                .observe(&ConflictAddress::reflection(volume, path));
            return true;
        }
        let Some(dependency) = dependency(&self.edits, volume, path) else {
            return false;
        };
        self.observations
            .lock()
            .unwrap()
            .observe(&ConflictAddress::reflection(volume, dependency));
        true
    }

    /// Records a read of `promise`'s status, which its resolution writes.
    /// Returns whether the read observed the snapshot.
    pub(crate) fn observe_promise_read(&mut self, promise: PromiseId) -> bool {
        self.observe_read_by_id(self.snapshot.runtime_volume, &promise_path(promise))
    }

    /// Stages `promise`'s resolution (`promise-resolution-commits-with-its-cut`):
    /// once the transaction commits, the promise is settled with the commit,
    /// which conflicts with the transactions that read its status and wakes
    /// those waiting on it. A promise already resolved, or resolved within
    /// this transaction already, is an error, which stages nothing. Where
    /// another resolution commits first, this transaction's commit
    /// conflicts.
    pub(crate) fn resolve(
        &mut self,
        resolver: &ManagedResolverRoot,
        resolution: Resolution,
    ) -> Result<(), Arc<str>> {
        if resolver.status() != PromiseStatus::Pending {
            return Err("the promise was already resolved".into());
        }
        let promise = resolver.id();
        if self.staged_status(promise).is_some() {
            return Err("the transaction resolves the promise already".into());
        }
        self.stage_resolution(promise, Some(resolver.clone()), resolution);
        Ok(())
    }

    /// Stages the break of `promise`, whose resolver is lost, with
    /// `resolution`, where it is pending. Returns whether it staged one.
    pub(crate) fn break_lost(&mut self, promise: PromiseId, resolution: Resolution) -> bool {
        if !self.snapshot.promises.is_pending(promise) || self.staged_status(promise).is_some() {
            return false;
        }
        self.stage_resolution(promise, None, resolution);
        true
    }

    fn stage_resolution(
        &mut self,
        promise: PromiseId,
        resolver: Option<ManagedResolverRoot>,
        resolution: Resolution,
    ) {
        self.edits.push(StoreEdit::Resolve {
            address: ConflictAddress::reflection(
                self.snapshot.runtime_volume,
                ConflictPath::from_keys(promise_path(promise)),
            ),
            promise,
            resolver,
            resolution,
        });
    }

    /// The status this transaction gives `promise`, where it resolves it.
    pub(crate) fn staged_status(&self, promise: PromiseId) -> Option<PromiseStatus> {
        self.edits.iter().find_map(|edit| match edit {
            StoreEdit::Resolve {
                promise: staged,
                resolution,
                ..
            } if *staged == promise => Some(resolution.status()),
            _ => None,
        })
    }

    /// Whether this transaction resolves any promise.
    pub(crate) fn resolves_any(&self) -> bool {
        self.edits
            .iter()
            .any(|edit| matches!(edit, StoreEdit::Resolve { .. }))
    }

    /// Whether this transaction resolves `promise`: one whose value it
    /// cannot read, since only its commit settles it.
    pub(crate) fn resolves_cell(&self, promise: PromiseId) -> bool {
        self.edits.iter().any(
            |edit| matches!(edit, StoreEdit::Resolve { promise: staged, .. } if *staged == promise),
        )
    }

    /// Whether `branch`, a branch of this journal, resolves a promise that
    /// this journal resolved too since the fork: a second resolution, an
    /// error rather than a conflict, since running again would race again.
    pub(crate) fn resolves_as_well(&self, branch: &StoreJournal) -> bool {
        let base = branch
            .branch
            .as_ref()
            .expect("only a branch merges into its journal")
            .base;
        let resolved = |edits: &[StoreEdit]| {
            edits
                .iter()
                .filter_map(|edit| match edit {
                    StoreEdit::Resolve { promise, .. } => Some(*promise),
                    _ => None,
                })
                .collect::<BTreeSet<_>>()
        };
        let ours = resolved(self.edits.get(base..).unwrap_or_default());
        resolved(branch.edits.get(base..).unwrap_or_default())
            .iter()
            .any(|promise| ours.contains(promise))
    }

    /// Records a read of the virtual resource at `path`: a proxy for host
    /// state that the store does not hold. Virtual resources follow the
    /// heap's rules: a set covers a later read, which then records nothing,
    /// and a rewrite widens it. Returns whether the read observed the
    /// snapshot.
    pub(crate) fn observe_virtual_read(&mut self, path: &[Key]) -> bool {
        let keys = virtual_keys(path);
        self.observe_read_by_id(self.snapshot.runtime_volume, &keys)
    }

    /// Records a set of the virtual resource at `path`, which changes no
    /// data: once committed, it conflicts with the transactions that read
    /// the resource and wakes those waiting on it.
    pub(crate) fn write_virtual(&mut self, path: &[Key]) {
        let address = self.virtual_address(path);
        self.edits.push(StoreEdit::Set {
            address,
            value: None,
        });
    }

    /// As [`Self::write_virtual`], for a rewrite.
    pub(crate) fn rewrite_virtual(&mut self, path: &[Key]) {
        let address = self.virtual_address(path);
        self.edits.push(StoreEdit::Rewrite {
            address,
            updater: None,
        });
    }

    /// Virtual resources' addresses lie in the runtime's private volume,
    /// apart from its queries.
    fn virtual_address(&self, path: &[Key]) -> ConflictAddress {
        ConflictAddress::reflection(
            self.snapshot.runtime_volume,
            ConflictPath::from_keys(virtual_keys(path)),
        )
    }

    #[cfg(test)]
    pub(crate) fn view(&self) -> PublicValue {
        self.volume_view(self.snapshot.heap_volume)
            .expect("the user heap volume must always exist")
    }

    /// As [`StoreSnapshot::heap_value`], in this journal's view, which
    /// holds its own writes. It records no read.
    pub(crate) fn heap_value(&self, path: &[Key]) -> PublicValue {
        self.volume_value(self.snapshot.heap_volume, path)
            .expect("the user heap volume must always exist")
    }

    /// The value of `volume`, with this journal's writes, or `None` where
    /// the volume does not exist.
    fn volume_view(&self, volume: VolumeId) -> Option<PublicValue> {
        self.volume_value(volume, &[])
    }

    /// As [`StoreSnapshot::heap_value`]'s root, in this journal's view, for
    /// a caller that holds `access`.
    pub(crate) fn view_in(&self, access: &RuntimeValueAccess<'_>) -> PublicValue {
        let value = self
            .value_at_in(access, self.snapshot.heap_volume, &[])
            .expect("the user heap volume must always exist");
        PublicValue::from_runtime_root(access.root_runtime_value(value))
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
        self.write_by_id(self.snapshot.runtime_volume, query_path(handle.id), state);
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
        self.observe_read_by_id(self.snapshot.runtime_volume, &path)
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
            value: PublicValue::from_runtime_root(
                self.snapshot.values.construct_runtime_value_root(|access| {
                    lazy_core_value_path(
                        access,
                        root.runtime_root().clone_core_with(access),
                        &query_path(handle.id),
                    )
                }),
            ),
            observed,
        }
    }

    pub(crate) fn write(&mut self, path: Vec<Key>, value: PublicValue) {
        self.write_by_id(self.snapshot.heap_volume, path, value);
    }

    /// As [`Self::write`], for a caller that holds `access`.
    pub(crate) fn write_in(
        &mut self,
        access: &RuntimeValueAccess<'_>,
        path: Vec<Key>,
        value: PublicValue,
    ) {
        self.write_by_id_in(access, self.snapshot.heap_volume, path, value);
    }

    fn write_by_id(&mut self, volume: VolumeId, path: Vec<Key>, value: PublicValue) {
        let edit = self.set_edit(volume, path, value);
        self.with_access(|journal, access| journal.push_edit(access, volume, edit));
    }

    /// As [`Self::write_by_id`], for a caller that holds `access`.
    fn write_by_id_in(
        &mut self,
        access: &RuntimeValueAccess<'_>,
        volume: VolumeId,
        path: Vec<Key>,
        value: PublicValue,
    ) {
        let edit = self.set_edit(volume, path, value);
        self.push_edit(access, volume, edit);
    }

    pub(crate) fn rewrite(&mut self, path: Vec<Key>, updater: PublicValue) {
        self.rewrite_by_id(self.snapshot.heap_volume, path, updater);
    }

    /// As [`Self::rewrite`], for a caller that holds `access`.
    pub(crate) fn rewrite_in(
        &mut self,
        access: &RuntimeValueAccess<'_>,
        path: Vec<Key>,
        updater: PublicValue,
    ) {
        self.rewrite_by_id_in(access, self.snapshot.heap_volume, path, updater);
    }

    fn rewrite_by_id(&mut self, volume: VolumeId, path: Vec<Key>, updater: PublicValue) {
        let edit = self.rewrite_edit(volume, path, updater);
        self.with_access(|journal, access| journal.push_edit(access, volume, edit));
    }

    /// As [`Self::rewrite_by_id`], for a caller that holds `access`.
    fn rewrite_by_id_in(
        &mut self,
        access: &RuntimeValueAccess<'_>,
        volume: VolumeId,
        path: Vec<Key>,
        updater: PublicValue,
    ) {
        let edit = self.rewrite_edit(volume, path, updater);
        self.push_edit(access, volume, edit);
    }

    fn set_edit(&self, volume: VolumeId, path: Vec<Key>, value: PublicValue) -> StoreEdit {
        debug_assert_eq!(value.runtime_id(), self.snapshot.values.runtime_id());
        StoreEdit::Set {
            address: ConflictAddress::reflection(volume, ConflictPath::from_keys(path)),
            value: Some(value),
        }
    }

    fn rewrite_edit(&self, volume: VolumeId, path: Vec<Key>, updater: PublicValue) -> StoreEdit {
        debug_assert_eq!(updater.runtime_id(), self.snapshot.values.runtime_id());
        StoreEdit::Rewrite {
            address: ConflictAddress::reflection(volume, ConflictPath::from_keys(path)),
            updater: Some(updater),
        }
    }

    /// Records `edit`, applying it to the volume's view where the volume
    /// exists, and keeping the volume's cell until the commit.
    fn push_edit(&mut self, access: &RuntimeValueAccess<'_>, volume: VolumeId, edit: StoreEdit) {
        if let Some(current) = self.value_at_in(access, volume, &[]) {
            if let Some(cell) = self.volume_cell_in(access, volume) {
                self.keep_volume_in(access, cell);
            }
            let draft = access.volume_draft(apply_edit_in(access, current, &edit));
            self.views.insert_mut(volume, draft);
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
    /// Each machine's share of its environment, which only the machine
    /// writes (`machines-inherit-an-environment`).
    environment_volume: VolumeId,
    query_domain: Arc<QueryDomain>,
    query_retirements: Receiver<EvaluationQueryId>,
    next_volume: Arc<AtomicU64>,
    promises: Arc<Promises>,
    /// The promises whose breakers died, as each queued its promise's ID
    /// (`promise-cells-are-weakly-resolved`). A volume reports nothing: its
    /// committed versions clear when it dies (`api-volume-cells`).
    lost_resolvers: Receiver<PromiseId>,
    /// The cells of the volumes the store keeps: its own, which live as
    /// long as it does, and those a host created, until revoked. Glam's
    /// volumes live while their handles do.
    kept: RedBlackTreeMapSync<VolumeId, ManagedVolumeRoot>,
    /// The volumes revoked through their owners, whose late edits are errors;
    /// a collected volume's are dropped, since nothing can read it.
    revoked: BTreeSet<VolumeId>,
    /// What the last commit settled, for its committer to publish.
    settled: Vec<Settled>,
    /// Each volume's committed version (`api-volume-cells`).
    versions: RedBlackTreeMapSync<VolumeId, ManagedVolumeVersion>,
    /// The heap's collection epoch when the store last pruned the versions
    /// of collected volumes.
    pruned_at: u64,
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
        let heap_volume = heap_volume();
        let runtime_volume = VolumeId::from_u64(2).expect("two is a nonzero volume ID");
        let environment_volume = VolumeId::from_u64(3).expect("three is a nonzero volume ID");
        let (query_domain, query_retirements) = QueryDomain::new();
        let (lost, lost_resolvers) = mpsc::channel();
        let mut kept = RedBlackTreeMapSync::new_sync();
        let mut versions = RedBlackTreeMapSync::new_sync();
        values.with_boundary_access(|access| {
            for volume in [heap_volume, runtime_volume, environment_volume] {
                let cell = access.new_volume(volume);
                versions.insert_mut(
                    volume,
                    access.volume_version(&cell, Value::Dict(Dict::new_sync())),
                );
                kept.insert_mut(volume, cell);
            }
        });
        Self {
            identity: Arc::new(()),
            heap_volume,
            runtime_volume,
            environment_volume,
            query_domain,
            query_retirements,
            next_volume: Arc::new(AtomicU64::new(4)),
            promises: Arc::new(Promises::new(lost)),
            lost_resolvers,
            kept,
            revoked: BTreeSet::new(),
            settled: Vec::new(),
            versions,
            pruned_at: 0,
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
            environment_volume: self.environment_volume,
            next_volume: self.next_volume.clone(),
            promises: self.promises.clone(),
            kept: self.kept.clone(),
            query_domain: self.query_domain.clone(),
            versions: self.versions.clone(),
            strategy: self.strategy.clone(),
            values: self.values.clone(),
        }
    }

    /// The shared heap's committed value.
    #[cfg(test)]
    pub(crate) fn root(&self) -> PublicValue {
        self.committed(self.heap_volume)
            .expect("the user heap volume must always exist")
    }

    /// The committed value of `volume`, or `None` where it does not exist.
    fn committed(&self, volume: VolumeId) -> Option<PublicValue> {
        version_value_at_path(&self.values, self.versions.get(&volume)?, &[])
    }

    /// How many volumes the store holds versions of, collected or not.
    #[cfg(test)]
    pub(crate) fn volume_count(&self) -> usize {
        self.versions.size()
    }

    #[cfg(test)]
    pub(crate) fn values(&self) -> &CoreValueFactory {
        &self.values
    }

    #[cfg(test)]
    fn volume_root(&self, volume: VolumeId) -> Option<PublicValue> {
        self.committed(volume)
    }

    #[doc(hidden)]
    pub fn strategy(&self) -> Arc<dyn ConflictAnalysisStrategy> {
        self.strategy.clone()
    }

    #[doc(hidden)]
    pub fn replace_root(&mut self, root: PublicValue) {
        debug_assert_eq!(root.runtime_id(), self.values.runtime_id());
        let version = self.values.with_boundary_access(|access| {
            access.volume_version(
                self.kept
                    .get(&self.heap_volume)
                    .expect("the store keeps the user heap volume"),
                root.runtime_root().clone_core_with(&access),
            )
        });
        self.versions.insert_mut(self.heap_volume, version);
        self.revision = self.revision.wrapping_add(1);
        self.latest_changes.insert(
            ConflictAddress::reflection_root(self.heap_volume),
            self.revision,
        );
    }

    /// Allocates a volume for a host, which names it by its ID: the store
    /// keeps it until the host revokes it.
    pub(crate) fn create_volume(&mut self, initial: PublicValue) -> Result<VolumeId, Arc<str>> {
        debug_assert_eq!(initial.runtime_id(), self.values.runtime_id());
        let volume = reserve_volume(&self.next_volume)?;
        let values = self.values.clone();
        values.with_boundary_access(|access| {
            self.prune_collected(&access);
            let cell = access.new_volume(volume);
            self.versions.insert_mut(
                volume,
                access.volume_version(&cell, initial.runtime_root().clone_core_with(&access)),
            );
            self.kept.insert_mut(volume, cell);
        });
        self.revision = self.revision.wrapping_add(1);
        self.latest_changes
            .insert(ConflictAddress::reflection_root(volume), self.revision);
        Ok(volume)
    }

    pub(crate) fn revoke_volume(&mut self, volume: VolumeId) -> Option<PublicValue> {
        if volume == self.heap_volume
            || volume == self.runtime_volume
            || volume == self.environment_volume
        {
            return None;
        }
        let root = self.committed(volume)?;
        self.versions.remove_mut(&volume);
        self.kept.remove_mut(&volume);
        self.revision = self.revision.wrapping_add(1);
        self.latest_changes
            .insert(ConflictAddress::reflection_root(volume), self.revision);
        self.revoked.insert(volume);
        Some(root)
    }

    /// Prunes the versions of the volumes that a collection found
    /// unreachable, if one has completed since the store last pruned: they
    /// hold nothing, and nothing can name them (`api-volume-cells`). The
    /// store prunes as it creates a volume, the only change that grows its
    /// map. It records no change, since nothing can read a collected volume.
    fn prune_collected(&mut self, access: &RuntimeValueAccess<'_>) {
        let epoch = self.values.completed_collection_epoch();
        if epoch == self.pruned_at {
            return;
        }
        self.pruned_at = epoch;
        let collected = self
            .versions
            .iter()
            .filter(|(_, version)| version.is_cleared_in(access))
            .map(|(volume, _)| *volume)
            .collect::<Vec<_>>();
        for volume in collected {
            self.versions.remove_mut(&volume);
        }
    }

    /// The store's promises (`requests-travel-on-ports`).
    pub(crate) fn promises(&self) -> &Arc<Promises> {
        &self.promises
    }

    /// What the commits since the last call settled, for the committer to
    /// publish once it has released the store
    /// (`promise-resolution-commits-with-its-cut`).
    pub(crate) fn take_settled(&mut self) -> Vec<Settled> {
        std::mem::take(&mut self.settled)
    }

    /// The promises whose breakers died since the last call.
    pub(crate) fn take_lost_resolvers(&self) -> Vec<PromiseId> {
        self.lost_resolvers.try_iter().collect()
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
        if !self.versions.contains_key(&self.runtime_volume) {
            return false;
        }
        let mut journal = StoreJournal::new(self.snapshot());
        journal.write_by_id(
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
        let created = journal
            .edits
            .iter()
            .filter(|edit| matches!(edit, StoreEdit::Create { .. }))
            .map(|edit| edit.address().reflection_parts().0)
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(volume) = journal
            .edits
            .iter()
            .map(|edit| edit.address().reflection_parts().0)
            .find(|volume| {
                !self.versions.contains_key(volume)
                    && !created.contains(volume)
                    && self.revoked.contains(volume)
            })
        {
            return StoreCommitResult::MissingVolume(volume);
        }
        if self.conflicts(journal) {
            return StoreCommitResult::Conflict;
        }
        // A resolution that another committed first conflicts, and the
        // transaction, running again, finds the promise resolved.
        if journal.edits.iter().any(|edit| {
            matches!(edit, StoreEdit::Resolve { promise, .. }
                if !self.promises.is_pending(*promise))
        }) {
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

        let current = Arc::ptr_eq(&self.identity, &journal.snapshot.identity)
            && self.revision == journal.snapshot.revision;
        let values = self.values.clone();
        values.with_boundary_access(|access| {
            if journal
                .edits
                .iter()
                .any(|edit| matches!(edit, StoreEdit::Create { .. }))
            {
                self.prune_collected(&access);
            }
            // A journal that began from the current revision already holds
            // a draft of each written volume, on which the new version is
            // keyed; another applies its edits to the volumes as they stand
            // now. A volume without a cell here was collected: nothing can
            // read its edits.
            if current {
                for volume in written_volumes(&journal.edits) {
                    if let (Some(cell), Some(draft)) =
                        (journal.cells.get(&volume), journal.views.get(&volume))
                    {
                        self.versions
                            .insert_mut(volume, access.volume_version_of(cell, draft));
                    }
                }
            } else {
                for (volume, value) in rebase_edits(&access, &self.versions, &journal.edits) {
                    if let Some(cell) = journal.cells.get(&volume) {
                        self.versions
                            .insert_mut(volume, access.volume_version(cell, value));
                    }
                }
            }
        });
        self.revision = self.revision.wrapping_add(1);
        for path in normalized_edit_paths(&journal.edits) {
            self.latest_changes.insert(path, self.revision);
        }
        for edit in &journal.edits {
            if let StoreEdit::Resolve {
                promise,
                resolver,
                resolution,
                ..
            } = edit
                && let Some(cell) = self.promises.settle(*promise)
            {
                if let Some(resolver) = resolver {
                    resolver.settle(resolution.fulfilled());
                }
                self.settled.push(Settled::new(cell, resolution.clone()));
            }
        }
        self.retire_queries();
        true
    }

    fn retire_queries(&mut self) -> bool {
        let retired = self.query_retirements.try_iter().collect::<Vec<_>>();
        if retired.is_empty() {
            return false;
        }
        let Some(version) = self.versions.get(&self.runtime_volume).cloned() else {
            return false;
        };
        self.revision = self.revision.wrapping_add(1);
        let paths = retired
            .into_iter()
            .map(|id| ConflictPath::from_keys(query_path(id)))
            .collect::<Vec<_>>();
        let version = self.values.with_boundary_access(|access| {
            let mut root = version
                .value_in(&access)
                .expect("the store keeps its runtime volume");
            for path in &paths {
                root = apply_value_at_path_in(&access, root, path, Value::Dict(Dict::new_sync()));
            }
            access.volume_version(
                self.kept
                    .get(&self.runtime_volume)
                    .expect("the store keeps its runtime volume"),
                root,
            )
        });
        for path in paths {
            self.latest_changes.insert(
                ConflictAddress::reflection(self.runtime_volume, path),
                self.revision,
            );
        }
        self.versions.insert_mut(self.runtime_volume, version);
        true
    }

    fn conflicts(&self, journal: &StoreJournal) -> bool {
        self.latest_changes.iter().any(|(changed, revision)| {
            *revision > journal.snapshot.revision
                && journal.observations.lock().unwrap().may_conflict(changed)
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

/// The volumes whose data `edits` change, which creating one does.
fn written_volumes(edits: &[StoreEdit]) -> BTreeSet<VolumeId> {
    edits
        .iter()
        .filter(|edit| {
            matches!(
                edit,
                StoreEdit::Set { value: Some(_), .. }
                    | StoreEdit::Rewrite {
                        updater: Some(_),
                        ..
                    }
                    | StoreEdit::Create { .. }
            )
        })
        .map(|edit| edit.address().reflection_parts().0)
        .collect()
}

/// The new value of each volume that `edits` write, applied in order to the
/// volume as `versions` hold it.
fn rebase_edits(
    access: &RuntimeValueAccess<'_>,
    versions: &RedBlackTreeMapSync<VolumeId, ManagedVolumeVersion>,
    edits: &[StoreEdit],
) -> BTreeMap<VolumeId, Value> {
    let mut written = BTreeMap::new();
    for edit in edits {
        let volume = edit.address().reflection_parts().0;
        if !written_volumes(std::slice::from_ref(edit)).contains(&volume) {
            continue;
        }
        // Validation refuses edits to revoked volumes, so a volume missing
        // here was collected, and nothing can read its edits.
        let current = match written.remove(&volume) {
            Some(current) => current,
            None => {
                let Some(value) = versions
                    .get(&volume)
                    .and_then(|version| version.value_in(access))
                else {
                    if let StoreEdit::Create { initial, .. } = edit {
                        written.insert(volume, initial.runtime_root().clone_core_with(access));
                    }
                    continue;
                };
                value
            }
        };
        written.insert(volume, apply_edit_in(access, current, edit));
    }
    written
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
static VIRTUAL_NAMESPACE: LazyLock<Key> = LazyLock::new(|| Key::atom_from_text("virtual"));
static PROMISE_NAMESPACE: LazyLock<Key> = LazyLock::new(|| Key::atom_from_text("promises"));

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

/// What a read of `path` in `volume` depends on, given the `edits` before
/// it: nothing when a set covers it, and a rewrite's whole path when one
/// above it applies.
fn dependency(edits: &[StoreEdit], volume: VolumeId, path: ConflictPath) -> Option<ConflictPath> {
    let mut dependency = path;
    for edit in edits.iter().rev() {
        let (edit_volume, edit_path) = edit.address().reflection_parts();
        match edit {
            StoreEdit::Set { .. } | StoreEdit::Resolve { .. }
                if edit_volume == volume && edit_path.is_prefix_of(&dependency) =>
            {
                return None;
            }
            // Nothing outside the transaction knows a volume it created.
            StoreEdit::Create { .. } if edit_volume == volume => return None,
            StoreEdit::Rewrite { .. }
                if edit_volume == volume && edit_path.is_prefix_of(&dependency) =>
            {
                dependency = edit_path.clone();
            }
            StoreEdit::Set { .. }
            | StoreEdit::Rewrite { .. }
            | StoreEdit::Create { .. }
            | StoreEdit::Resolve { .. } => {}
        }
    }
    Some(dependency)
}

fn virtual_keys(path: &[Key]) -> Vec<Key> {
    std::iter::once(VIRTUAL_NAMESPACE.clone())
        .chain(path.iter().cloned())
        .collect()
}

/// A promise's place in the runtime's volume, apart from virtual
/// resources, which users name.
fn promise_path(promise: PromiseId) -> Vec<Key> {
    vec![
        PROMISE_NAMESPACE.clone(),
        Key::Number(Number::from_u64(promise.get())),
    ]
}

fn query_path(id: EvaluationQueryId) -> Vec<Key> {
    vec![
        QUERY_NAMESPACE.clone(),
        Key::Number(Number::from_u64(id.get())),
    ]
}

#[cfg(test)]
fn pending_query_value(values: &CoreValueFactory) -> PublicValue {
    PublicValue::from_runtime_root(values.construct_runtime_value_root(|access| {
        Value::Dict(Dict::new_sync().insert(QUERY_PENDING.clone(), access.unit()))
    }))
}

fn complete_query_value(values: &CoreValueFactory, result: PublicValue) -> PublicValue {
    PublicValue::from_runtime_root(values.construct_runtime_value_root(|access| {
        let payload = Value::Dict(
            Dict::new_sync()
                .insert(QUERY_PRESENT.clone(), access.unit())
                .insert(
                    QUERY_RESULT.clone(),
                    result.into_runtime_root().clone_core_with(access),
                ),
        );
        Value::Dict(Dict::new_sync().insert(QUERY_COMPLETE.clone(), payload))
    }))
}

pub(crate) fn decode_query_state(
    access: &RuntimeValueAccess<'_>,
    value: &Value,
) -> Option<EvaluationQueryState> {
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
        PublicValue::from_runtime_root(
            access.root_runtime_value(
                complete
                    .get(&QUERY_RESULT)
                    .map(|value| access.duplicate_value(value))
                    .unwrap_or_else(|| Value::Dict(Dict::new_sync())),
            ),
        ),
    ))
}

/// `current` with `edit` applied.
fn apply_edit_in(access: &RuntimeValueAccess<'_>, current: Value, edit: &StoreEdit) -> Value {
    match edit {
        StoreEdit::Set { value: None, .. }
        | StoreEdit::Rewrite { updater: None, .. }
        | StoreEdit::Resolve { .. } => current,
        StoreEdit::Create { initial, .. } => initial.runtime_root().clone_core_with(access),
        StoreEdit::Set {
            address,
            value: Some(value),
        } => {
            let (_, path) = address.reflection_parts();
            apply_value_at_path_in(
                access,
                current,
                path,
                value.runtime_root().clone_core_with(access),
            )
        }
        StoreEdit::Rewrite {
            address,
            updater: Some(updater),
        } => {
            let (_, path) = address.reflection_parts();
            let prior = lazy_core_value_path(access, access.duplicate_value(&current), path.keys());
            let updated = Value::Lazy(LazyValue::from_application_in(
                access,
                updater.runtime_root().clone_core_with(access),
                Arc::from([prior]),
            ));
            apply_value_at_path_in(access, current, path, updated)
        }
    }
}

fn apply_value_at_path_in(
    access: &RuntimeValueAccess<'_>,
    root: Value,
    path: &ConflictPath,
    value: Value,
) -> Value {
    if path.depth() == 0 {
        return value;
    }
    let path = Value::List(List::from_values(
        path.keys()
            .iter()
            .map(|key| access.value_from_key(key))
            .collect(),
    ));
    Value::builtin_call_in(access, Builtin::DictUpdate, vec![path, value, root])
}

/// The value at `path` in a volume's `contents`, as a lazy access left to
/// the caller to root.
pub(crate) fn contents_at_path(
    access: &RuntimeValueAccess<'_>,
    contents: VolumeContentsView<'_>,
    path: &[Key],
) -> Value {
    lazy_core_value_path(
        access,
        access.duplicate_value(contents.value_in(access)),
        path,
    )
}

/// The value at `path` in `version`, as a lazy access, or `None` once a
/// collection found the volume unreachable.
fn version_value_at_path(
    values: &CoreValueFactory,
    version: &ManagedVolumeVersion,
    path: &[Key],
) -> Option<PublicValue> {
    values
        .try_construct_runtime_value_root(|access| {
            let value = version.value_in(access).ok_or(())?;
            Ok::<_, ()>(lazy_core_value_path(access, value, path))
        })
        .ok()
        .map(PublicValue::from_runtime_root)
}

fn lazy_core_value_path(
    access: &crate::core::RuntimeValueAccess<'_>,
    value: Value,
    path: &[Key],
) -> Value {
    if path.is_empty() {
        return value;
    }
    Value::Lazy(LazyValue::from_access_in(
        access,
        Arc::from(
            path.iter()
                .cloned()
                .map(CoreDataKey::Key)
                .collect::<Vec<_>>(),
        ),
        Arc::from([value]),
    ))
}
