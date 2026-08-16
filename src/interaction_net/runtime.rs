use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::model::*;

mod cursor;
mod graph;
mod rewrite;

#[cfg(test)]
mod tests;

impl<S: NetSpecialization> InteractionNet<S> {
    pub fn instantiate(&self) -> RuntimeNet<S> {
        RuntimeNet::new(self)
    }

    pub fn instantiate_shared(&self) -> SharedRuntimeNet<S> {
        SharedRuntimeNet::new(self.instantiate())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reduction {
    pub pair: ActivePairKey,
    pub kind: ReductionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReductionKind {
    BindJoin,
    FanJoin {
        identity: FanIdentity,
    },
    FanCommute {
        left: FanIdentity,
        right: FanIdentity,
    },
    FanData {
        identity: FanIdentity,
    },
    FanBind {
        identity: FanIdentity,
    },
    FanOperator {
        identity: FanIdentity,
    },
    Erase,
    Call {
        bind: NodeId,
        data: NodeId,
    },
    OperatorCall {
        operator: NodeId,
        data: NodeId,
    },
    RemoteCursor {
        cursor: NodeId,
        progress: CursorProgress,
    },
    Stuck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorProgress {
    Claimed,
    Materialized { node: NodeId },
    Joined,
    Blocked,
}

/// Work found at the end of one transient cursor-demand spine inspection.
///
/// The endpoint is a candidate for current progress, not the identity of the
/// continuing demand rooted at the inspected source anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemandEndpoint {
    Cursor(NodeId),
    ActivePair(ActivePairKey),
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontierObservationStatus {
    Current,
    Disturbed,
}

/// Revisions captured by one locked shared-net observation.
///
/// Topology invalidates structural observations. Disturbance coordinates
/// competing evaluators and may advance less frequently once batching is
/// enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeNetRevisions {
    topology_revision: u64,
    disturbance_epoch: u64,
}

impl RuntimeNetRevisions {
    pub fn topology_revision(self) -> u64 {
        self.topology_revision
    }

    pub fn disturbance_epoch(self) -> u64 {
        self.disturbance_epoch
    }
}

#[derive(Clone)]
pub struct NetContention<S: NetSpecialization> {
    runtime: SharedRuntimeNet<S>,
    revisions: RuntimeNetRevisions,
}

impl<S: NetSpecialization> NetContention<S> {
    pub fn runtime(&self) -> &SharedRuntimeNet<S> {
        &self.runtime
    }

    pub fn revisions(&self) -> RuntimeNetRevisions {
        self.revisions
    }
}

impl<S: NetSpecialization> fmt::Debug for NetContention<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetContention")
            .field("runtime", &self.runtime)
            .field("revisions", &self.revisions)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub enum CursorStep<S: NetSpecialization> {
    Progressed(CursorProgress),
    Dependency(CursorDependency<S>),
    Stable,
    Contended(NetContention<S>),
    Disturbed,
    Gone,
}

#[derive(Debug, Clone)]
pub enum ActivePairStep<S: NetSpecialization> {
    Reduction(Reduction),
    Cursor(NodeId),
    BlockedCall(BlockedCall<S::WaitToken>),
    BlockedOperatorCall(BlockedOperatorCall<S::WaitToken>),
    Stuck(StuckPair<S::StuckReason>),
    Contended(NetContention<S>),
    Disturbed,
    Gone,
}

/// One versioned observation of the work currently demanded from a source
/// frontier. The complete auxiliary/principal spine is deliberately not
/// retained; a disturbed observation is recomputed from `anchor`.
#[derive(Clone, PartialEq, Eq)]
pub struct FrontierObservation<S: NetSpecialization> {
    source: SharedRuntimeNet<S>,
    anchor: Port,
    observed_revisions: RuntimeNetRevisions,
    endpoint: DemandEndpoint,
}

impl<S: NetSpecialization> FrontierObservation<S> {
    pub fn source(&self) -> &SharedRuntimeNet<S> {
        &self.source
    }

    pub fn anchor(&self) -> Port {
        self.anchor
    }

    pub fn endpoint(&self) -> DemandEndpoint {
        self.endpoint
    }

    /// Validates under the source lock so a mutation cannot become visible
    /// before its version publication is observed.
    #[cfg(test)]
    pub fn status(&self) -> FrontierObservationStatus {
        let (_, current_revisions) = self.source.with_revisions(|_| ());
        if current_revisions.topology_revision() == self.observed_revisions.topology_revision() {
            FrontierObservationStatus::Current
        } else {
            FrontierObservationStatus::Disturbed
        }
    }

    /// Claims the observed active-pair endpoint if this observation is still
    /// current. An unavailable current endpoint is left untouched so waiting
    /// on this observation cannot disturb itself.
    #[cfg(test)]
    pub fn reduce_pair(
        &self,
        pair: ActivePairKey,
    ) -> Result<Option<Reduction>, FrontierObservationStatus> {
        assert_eq!(self.endpoint, DemandEndpoint::ActivePair(pair));
        self.update_source_if_current(|runtime| runtime.reduce_pair(pair))
    }

    /// Takes one non-blocking step at the observed pair. Unlike `reduce_pair`,
    /// this reports claimed, blocked, stuck, gone, and disturbed states
    /// explicitly for an iterative normalization driver.
    pub fn step_active_pair(&self, pair: ActivePairKey) -> ActivePairStep<S> {
        assert_eq!(self.endpoint, DemandEndpoint::ActivePair(pair));
        self.source
            .step_active_pair_if_current(pair, Some(self.observed_revisions.topology_revision()))
    }

    /// Takes one non-blocking step at the observed cursor endpoint.
    pub fn step_cursor(&self, cursor: NodeId) -> CursorStep<S> {
        assert_eq!(self.endpoint, DemandEndpoint::Cursor(cursor));
        self.source
            .step_cursor_if_current(cursor, Some(self.observed_revisions.topology_revision()))
    }

    /// Creates or claims the observed source cursor's owning-net obligation
    /// under the same source/version check used for active-pair endpoints.
    #[cfg(test)]
    pub fn claim_cursor_obligation(
        &self,
        cursor: NodeId,
    ) -> Result<Option<CursorProgress>, FrontierObservationStatus> {
        assert_eq!(self.endpoint, DemandEndpoint::Cursor(cursor));
        self.update_source_if_current(|runtime| runtime.claim_cursor_obligation(cursor))
    }

    #[cfg(test)]
    fn update_source_if_current<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<S>) -> Option<R>,
    ) -> Result<Option<R>, FrontierObservationStatus> {
        let mut state = self
            .source
            .inner
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        if self.source.inner.revisions().topology_revision()
            != self.observed_revisions.topology_revision()
        {
            return Err(FrontierObservationStatus::Disturbed);
        }
        let result = update(&mut state.runtime);
        if result.is_some() {
            self.source.inner.publish_mutation(&mut state.batches);
        }
        Ok(result)
    }
}

impl<S: NetSpecialization> fmt::Debug for FrontierObservation<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrontierObservation")
            .field("source", &self.source)
            .field("anchor", &self.anchor)
            .field("observed_revisions", &self.observed_revisions)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum CursorDependency<S: NetSpecialization> {
    LocalCursor(NodeId),
    /// Transitional classification for a source cursor. Phase 3 replaces
    /// direct cursor following with an owning-net normalization obligation.
    SourceCursor(FrontierObservation<S>),
    /// A versioned observation of an active-pair endpoint on the demanded
    /// source frontier. The pair is not retained as the dependency identity.
    SourceFrontier(FrontierObservation<S>),
}

impl<S: NetSpecialization> fmt::Debug for CursorDependency<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalCursor(cursor) => {
                formatter.debug_tuple("LocalCursor").field(cursor).finish()
            }
            Self::SourceCursor(observation) => formatter
                .debug_tuple("SourceCursor")
                .field(observation)
                .finish(),
            Self::SourceFrontier(observation) => formatter
                .debug_tuple("SourceFrontier")
                .field(observation)
                .finish(),
        }
    }
}

#[derive(Debug, Clone)]
enum PairlessCursorState<S: NetSpecialization> {
    Ready,
    Claimed,
    Blocked(CursorDependency<S>),
    Stable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorObligationStatus {
    Ready,
    Claimed,
    Blocked,
    Stable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CursorObligationSnapshot {
    pub cursor: NodeId,
    pub status: CursorObligationStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CursorBlockage<S: NetSpecialization> {
    Dependency(CursorDependency<S>),
    Stable,
}

impl<S: NetSpecialization> PairlessCursorState<S> {
    fn is_claimed(&self) -> bool {
        matches!(self, Self::Claimed)
    }
}

#[derive(Debug, Clone)]
struct PairlessCursorObligation<S: NetSpecialization> {
    cursor: NodeId,
    state: PairlessCursorState<S>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorClaimOwner {
    ActivePair(ActivePairKey),
    Obligation,
}

enum CursorStepInspection<S: NetSpecialization> {
    Claimable(Option<ActivePairKey>),
    Dependency(CursorDependency<S>),
    Stable,
    Claimed,
    Gone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Call {
    pub pair: ActivePairKey,
    pub bind: NodeId,
    pub data: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperatorCall {
    pub pair: ActivePairKey,
    pub operator: NodeId,
    pub data: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StuckReason<R> {
    NoRule,
    Specialization(R),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckPair<R> {
    pub pair: ActivePairKey,
    pub reason: StuckReason<R>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedCall<W> {
    pub pair: ActivePairKey,
    pub wait: W,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedOperatorCall<W> {
    pub pair: ActivePairKey,
    pub wait: W,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedCursor {
    pub pair: ActivePairKey,
    pub cursor: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ActivePairState<S: NetSpecialization> {
    Ready,
    Claimed,
    BlockedCall {
        wait: S::WaitToken,
    },
    BlockedOperatorCall {
        wait: S::WaitToken,
    },
    BlockedCursor {
        cursor: NodeId,
        blockage: CursorBlockage<S>,
    },
    Stuck(StuckReason<S::StuckReason>),
}

impl<S: NetSpecialization> ActivePairState<S> {
    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    fn is_claimed(&self) -> bool {
        matches!(self, Self::Claimed)
    }
}

pub struct SharedRuntimeNet<S: NetSpecialization> {
    inner: Arc<SharedRuntimeNetInner<S>>,
}

struct SharedRuntimeNetInner<S: NetSpecialization> {
    runtime: Mutex<SharedRuntimeNetState<S>>,
    changed: Condvar,
    topology_revision: AtomicU64,
    disturbance_epoch: AtomicU64,
}

struct SharedRuntimeNetState<S: NetSpecialization> {
    runtime: RuntimeNet<S>,
    batches: NormalizationBatchState,
}

#[derive(Default)]
struct NormalizationBatchState {
    next_id: u64,
    active: Option<ActiveNormalizationBatch>,
}

struct ActiveNormalizationBatch {
    id: u64,
    contended: bool,
    dirty: bool,
}

pub struct NormalizationBatchLease<S: NetSpecialization> {
    inner: std::sync::Weak<SharedRuntimeNetInner<S>>,
    id: u64,
    closed: bool,
}

impl<S: NetSpecialization> fmt::Debug for NormalizationBatchLease<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NormalizationBatchLease")
            .field("id", &self.id)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl<S: NetSpecialization> NormalizationBatchLease<S> {
    pub fn close(mut self) {
        self.close_inner();
    }

    fn close_inner(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut state = inner
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        let publish = if state
            .batches
            .active
            .as_ref()
            .is_some_and(|active| active.id == self.id)
        {
            let active = state
                .batches
                .active
                .take()
                .expect("matching normalization batch must remain installed");
            active.dirty || active.contended
        } else {
            false
        };
        if publish {
            inner.disturbance_epoch.fetch_add(1, Ordering::Relaxed);
        }
        drop(state);
        if publish {
            inner.changed.notify_all();
        }
    }
}

impl<S: NetSpecialization> Drop for NormalizationBatchLease<S> {
    fn drop(&mut self) {
        self.close_inner();
    }
}

impl<S: NetSpecialization> SharedRuntimeNetInner<S> {
    fn revisions(&self) -> RuntimeNetRevisions {
        RuntimeNetRevisions {
            topology_revision: self.topology_revision.load(Ordering::Relaxed),
            disturbance_epoch: self.disturbance_epoch.load(Ordering::Relaxed),
        }
    }

    fn publish_mutation(&self, batches: &mut NormalizationBatchState) {
        self.topology_revision.fetch_add(1, Ordering::Relaxed);
        if let Some(active) = batches.active.as_mut() {
            active.dirty = true;
        } else {
            self.disturbance_epoch.fetch_add(1, Ordering::Relaxed);
            self.changed.notify_all();
        }
    }
}

/// Result of an update which may discover that no authoritative state needs
/// to change. Only `Changed` publishes a topology revision and disturbance.
pub(crate) enum RuntimeNetMutation<R> {
    Unchanged(R),
    Changed(R),
}

impl<S: NetSpecialization> SharedRuntimeNet<S> {
    pub fn new(runtime: RuntimeNet<S>) -> Self {
        Self {
            inner: Arc::new(SharedRuntimeNetInner {
                runtime: Mutex::new(SharedRuntimeNetState {
                    runtime,
                    batches: NormalizationBatchState::default(),
                }),
                changed: Condvar::new(),
                topology_revision: AtomicU64::new(0),
                disturbance_epoch: AtomicU64::new(0),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_copy_layer(source: Self) -> (Self, Port) {
        let mut target = RuntimeNet::empty();
        let cursor = target.begin_copy(source);
        let interface = target.add_interface(Port::principal(cursor));
        target.exposed = Some(interface);
        (Self::new(target), interface)
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn with<R>(&self, inspect: impl FnOnce(&RuntimeNet<S>) -> R) -> R {
        let state = self
            .inner
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        inspect(&state.runtime)
    }

    pub fn with_revisions<R>(
        &self,
        inspect: impl FnOnce(&RuntimeNet<S>) -> R,
    ) -> (R, RuntimeNetRevisions) {
        let state = self
            .inner
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        let revisions = self.inner.revisions();
        (inspect(&state.runtime), revisions)
    }

    pub fn with_mut<R>(&self, update: impl FnOnce(&mut RuntimeNet<S>) -> R) -> R {
        let mut state = self
            .inner
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        let result = update(&mut state.runtime);
        self.inner.publish_mutation(&mut state.batches);
        result
    }

    pub(crate) fn with_conditional_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<S>) -> RuntimeNetMutation<R>,
    ) -> R {
        let mut state = self
            .inner
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        match update(&mut state.runtime) {
            RuntimeNetMutation::Unchanged(result) => result,
            RuntimeNetMutation::Changed(result) => {
                self.inner.publish_mutation(&mut state.batches);
                result
            }
        }
    }

    pub(crate) fn with_optional_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<S>) -> Option<R>,
    ) -> Option<R> {
        self.with_conditional_mut(|runtime| match update(runtime) {
            Some(result) => RuntimeNetMutation::Changed(Some(result)),
            None => RuntimeNetMutation::Unchanged(None),
        })
    }

    pub(crate) fn ensure_interface_cursor_obligation(&self, interface: Port) -> Option<NodeId> {
        self.with_conditional_mut(|runtime| {
            let Some(cursor) = runtime.interface_cursor(interface) else {
                return RuntimeNetMutation::Unchanged(None);
            };
            if runtime.ensure_pairless_cursor_obligation(cursor) {
                RuntimeNetMutation::Changed(Some(cursor))
            } else {
                RuntimeNetMutation::Unchanged(Some(cursor))
            }
        })
    }

    #[cfg(test)]
    fn revisions(&self) -> (u64, u64) {
        let _state = self
            .inner
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        let revisions = self.inner.revisions();
        (revisions.topology_revision(), revisions.disturbance_epoch())
    }

    pub fn wait_for_disturbance(&self, observed_epoch: u64) {
        let mut state = self
            .inner
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        while self.inner.disturbance_epoch.load(Ordering::Relaxed) == observed_epoch {
            state = self
                .inner
                .changed
                .wait(state)
                .expect("shared runtime net was poisoned");
        }
    }

    pub fn try_begin_normalization_batch(
        &self,
    ) -> Result<NormalizationBatchLease<S>, NetContention<S>> {
        let mut state = self
            .inner
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        let revisions = self.inner.revisions();
        if let Some(active) = state.batches.active.as_mut() {
            active.contended = true;
            return Err(self.contention(revisions));
        }
        let id = state.batches.next_id;
        state.batches.next_id = state
            .batches
            .next_id
            .checked_add(1)
            .expect("interaction-net normalization batch ID space exhausted");
        state.batches.active = Some(ActiveNormalizationBatch {
            id,
            contended: false,
            dirty: false,
        });
        Ok(NormalizationBatchLease {
            inner: Arc::downgrade(&self.inner),
            id,
            closed: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn active_normalization_batch(&self) -> Option<(u64, bool)> {
        let state = self
            .inner
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        state
            .batches
            .active
            .as_ref()
            .map(|active| (active.id, active.contended))
    }

    fn contention(&self, revisions: RuntimeNetRevisions) -> NetContention<S> {
        NetContention {
            runtime: self.clone(),
            revisions,
        }
    }

    pub fn step_active_pair(&self, pair: ActivePairKey) -> ActivePairStep<S> {
        self.step_active_pair_if_current(pair, None)
    }

    fn step_active_pair_if_current(
        &self,
        pair: ActivePairKey,
        expected_topology_revision: Option<u64>,
    ) -> ActivePairStep<S> {
        let mut state = self
            .inner
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        let revisions = self.inner.revisions();
        if expected_topology_revision
            .is_some_and(|expected| expected != revisions.topology_revision())
        {
            return ActivePairStep::Disturbed;
        }
        let pair_state = state.runtime.active.get(&pair).cloned();
        let (outcome, changed) = match pair_state {
            Some(ActivePairState::Ready) => (
                ActivePairStep::Reduction(
                    state
                        .runtime
                        .reduce_pair(pair)
                        .expect("ready pair must produce one reduction"),
                ),
                true,
            ),
            Some(ActivePairState::Claimed) => {
                (ActivePairStep::Contended(self.contention(revisions)), false)
            }
            Some(ActivePairState::BlockedCursor { cursor, .. }) => {
                (ActivePairStep::Cursor(cursor), false)
            }
            Some(ActivePairState::BlockedCall { wait }) => (
                ActivePairStep::BlockedCall(BlockedCall { pair, wait }),
                false,
            ),
            Some(ActivePairState::BlockedOperatorCall { wait }) => (
                ActivePairStep::BlockedOperatorCall(BlockedOperatorCall { pair, wait }),
                false,
            ),
            Some(ActivePairState::Stuck(reason)) => {
                (ActivePairStep::Stuck(StuckPair { pair, reason }), false)
            }
            None => (ActivePairStep::Gone, false),
        };
        if changed {
            self.inner.publish_mutation(&mut state.batches);
        }
        outcome
    }

    pub fn step_cursor(&self, cursor: NodeId) -> CursorStep<S> {
        self.step_cursor_if_current(cursor, None)
    }

    fn step_cursor_if_current(
        &self,
        cursor: NodeId,
        expected_topology_revision: Option<u64>,
    ) -> CursorStep<S> {
        let claimed = {
            let mut state = self
                .inner
                .runtime
                .lock()
                .expect("shared runtime net was poisoned");
            let revisions = self.inner.revisions();
            if expected_topology_revision
                .is_some_and(|expected| expected != revisions.topology_revision())
            {
                return CursorStep::Disturbed;
            }
            match state.runtime.inspect_cursor_step(cursor) {
                CursorStepInspection::Claimable(expected_pair) => {
                    let progress = state
                        .runtime
                        .begin_cursor_claim(cursor, expected_pair)
                        .expect("claimable cursor must accept its owning transition");
                    self.inner.publish_mutation(&mut state.batches);
                    progress
                }
                CursorStepInspection::Dependency(dependency) => {
                    return CursorStep::Dependency(dependency);
                }
                CursorStepInspection::Stable => return CursorStep::Stable,
                CursorStepInspection::Claimed => {
                    return CursorStep::Contended(self.contention(revisions));
                }
                CursorStepInspection::Gone => return CursorStep::Gone,
            }
        };
        assert_eq!(claimed, CursorProgress::Claimed);
        let progress = self
            .advance_claimed_cursor(cursor)
            .expect("cursor claimed by a step must remain advanceable");
        if progress != CursorProgress::Blocked {
            return CursorStep::Progressed(progress);
        }
        let (inspection, revisions) =
            self.with_revisions(|runtime| runtime.inspect_cursor_step(cursor));
        match inspection {
            CursorStepInspection::Claimable(_) => CursorStep::Progressed(progress),
            CursorStepInspection::Dependency(dependency) => CursorStep::Dependency(dependency),
            CursorStepInspection::Stable => CursorStep::Stable,
            CursorStepInspection::Claimed => CursorStep::Contended(self.contention(revisions)),
            CursorStepInspection::Gone => CursorStep::Gone,
        }
    }
}

impl<S: NetSpecialization> SharedRuntimeNet<S> {
    /// Inspects and advances a previously claimed cursor without holding target
    /// and source runtime locks at the same time.
    pub fn advance_claimed_cursor(&self, cursor: NodeId) -> Option<CursorProgress> {
        let claim = self.with(|target| target.cursor_claim(cursor))?;
        let source = claim.source.clone();
        let frontier = source.inspect_source_frontier(claim.remote);
        Some(self.with_mut(|target| target.finish_cursor_claim(claim, frontier)))
    }
}

impl<S: NetSpecialization> Clone for SharedRuntimeNet<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<S: NetSpecialization> fmt::Debug for SharedRuntimeNet<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SharedRuntimeNet")
            .field(&Arc::as_ptr(&self.inner))
            .finish()
    }
}

impl<S: NetSpecialization> PartialEq for SharedRuntimeNet<S> {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other)
    }
}

impl<S: NetSpecialization> Eq for SharedRuntimeNet<S> {}

struct CopyState<S: NetSpecialization> {
    source: SharedRuntimeNet<S>,
    frontiers: HashMap<Port, NodeId>,
    fan_sites: HashMap<FanSite, FanSite>,
}

#[derive(Clone)]
struct CursorClaim<S: NetSpecialization> {
    cursor: NodeId,
    owner: CursorClaimOwner,
    copy: CopyId,
    remote: Port,
    source: SharedRuntimeNet<S>,
}

struct SourceFrontier<S: NetSpecialization> {
    shape: SourceFrontierShape<S>,
    observation: Option<FrontierObservation<S>>,
}

enum SourceFrontierShape<S: NetSpecialization> {
    Principal {
        port: Port,
        node: RuntimeNode<S>,
    },
    StableAuxiliary {
        port: Port,
        principal_anchors: Vec<Port>,
        terminal_pair: Option<ActivePairKey>,
    },
    ActiveAuxiliary {
        entered: Port,
        partner: Port,
    },
}

struct RuntimeEntry<S: NetSpecialization> {
    node: RuntimeNode<S>,
    links: [Option<Port>; 3],
}

impl<S: NetSpecialization> RuntimeEntry<S> {
    fn new(node: RuntimeNode<S>) -> Self {
        Self {
            node,
            links: [None; 3],
        }
    }
}

pub struct RuntimeNet<S: NetSpecialization> {
    next_node_id: u64,
    next_fan_site: u64,
    exposed: Option<Port>,
    nodes: HashMap<NodeId, RuntimeEntry<S>>,
    next_copy_id: u64,
    copies: HashMap<CopyId, CopyState<S>>,
    // Pairless cursor demand is owned here until the cursor participates in
    // an active pair, at which point `connect` transfers the state into the
    // pair's authoritative record.
    cursor_obligations: HashMap<NodeId, PairlessCursorObligation<S>>,

    // Every live principal-principal wire has exactly one authoritative state.
    // External work changes Ready to Claimed while the runtime lock is held,
    // then completes as a rewrite, a blocked call or cursor, or a permanent
    // stuck reason.
    pub(super) active: BTreeMap<ActivePairKey, ActivePairState<S>>,
}

impl<S: NetSpecialization> RuntimeNet<S> {
    fn new(net: &InteractionNet<S>) -> Self {
        let nodes = net
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let id = NodeId::from_index(index);
                let node = match node {
                    Node::Bind => RuntimeNode::Bind,
                    Node::Fan { site } => RuntimeNode::Fan {
                        identity: FanIdentity::root(*site),
                    },
                    Node::Erase => RuntimeNode::Erase,
                    Node::Data(data) => RuntimeNode::Data(data.clone()),
                    Node::Operator(operator) => RuntimeNode::Operator(operator.clone()),
                };
                (id, RuntimeEntry::new(node))
            })
            .collect();
        let next_fan_site = net
            .nodes
            .iter()
            .filter_map(|node| match node {
                Node::Fan { site } => Some(site.get()),
                _ => None,
            })
            .max()
            .map_or(0, |site| {
                site.checked_add(1)
                    .expect("interaction-net fan site space exhausted")
            });
        let mut runtime = Self {
            next_node_id: u64::try_from(net.nodes.len())
                .expect("interaction-net node count does not fit in u64"),
            next_fan_site,
            exposed: None,
            nodes,
            next_copy_id: 0,
            copies: HashMap::new(),
            cursor_obligations: HashMap::new(),
            active: BTreeMap::new(),
        };
        for wire in net.wires.iter() {
            runtime.connect(wire.left, wire.right);
        }
        let exposed = runtime.add_interface(net.exposed);
        runtime.exposed = Some(exposed);
        runtime
    }

    #[cfg(test)]
    fn empty() -> Self {
        Self {
            next_node_id: 0,
            next_fan_site: 0,
            exposed: None,
            nodes: HashMap::new(),
            next_copy_id: 0,
            copies: HashMap::new(),
            cursor_obligations: HashMap::new(),
            active: BTreeMap::new(),
        }
    }

    pub fn active_pairs(&self) -> impl ExactSizeIterator<Item = ActivePairKey> + '_ {
        self.active.keys().copied()
    }

    pub fn has_in_flight_claims(&self) -> bool {
        self.cursor_obligations
            .values()
            .any(|obligation| obligation.state.is_claimed())
            || self
                .active
                .values()
                .any(|state| matches!(state, ActivePairState::Claimed))
    }

    #[cfg(test)]
    pub fn pair_is_claimed(&self, pair: ActivePairKey) -> bool {
        self.active
            .get(&pair)
            .is_some_and(ActivePairState::is_claimed)
    }

    fn cursor_claim_owner(&self, cursor: NodeId) -> Option<CursorClaimOwner> {
        let pair_owner = self
            .active_pair_key(cursor)
            .map(CursorClaimOwner::ActivePair);
        let obligation_owner = self.cursor_obligations.get(&cursor).map(|obligation| {
            assert_eq!(obligation.cursor, cursor);
            CursorClaimOwner::Obligation
        });
        assert!(
            pair_owner.is_none() || obligation_owner.is_none(),
            "a cursor transition cannot have both active-pair and obligation owners"
        );
        pair_owner.or(obligation_owner)
    }

    fn cursor_claim_is_in_flight(&self, cursor: NodeId) -> bool {
        match self.cursor_claim_owner(cursor) {
            Some(CursorClaimOwner::ActivePair(pair)) => self
                .active
                .get(&pair)
                .is_some_and(ActivePairState::is_claimed),
            Some(CursorClaimOwner::Obligation) => self
                .cursor_obligations
                .get(&cursor)
                .is_some_and(|obligation| obligation.state.is_claimed()),
            None => false,
        }
    }

    fn inspect_cursor_step(&self, cursor: NodeId) -> CursorStepInspection<S> {
        match self.cursor_claim_owner(cursor) {
            Some(CursorClaimOwner::ActivePair(pair)) => match self.active.get(&pair) {
                Some(ActivePairState::Ready) => CursorStepInspection::Claimable(Some(pair)),
                Some(ActivePairState::Claimed) => CursorStepInspection::Claimed,
                Some(ActivePairState::BlockedCursor {
                    cursor: blocked,
                    blockage: CursorBlockage::Dependency(dependency),
                }) if *blocked == cursor => CursorStepInspection::Dependency(dependency.clone()),
                Some(ActivePairState::BlockedCursor {
                    cursor: blocked,
                    blockage: CursorBlockage::Stable,
                }) if *blocked == cursor => CursorStepInspection::Stable,
                _ => CursorStepInspection::Gone,
            },
            Some(CursorClaimOwner::Obligation) => {
                match &self
                    .cursor_obligations
                    .get(&cursor)
                    .expect("cursor obligation owner must remain installed")
                    .state
                {
                    PairlessCursorState::Ready => CursorStepInspection::Claimable(None),
                    PairlessCursorState::Claimed => CursorStepInspection::Claimed,
                    PairlessCursorState::Blocked(dependency) => {
                        CursorStepInspection::Dependency(dependency.clone())
                    }
                    PairlessCursorState::Stable => CursorStepInspection::Stable,
                }
            }
            None if matches!(self.node(cursor), Some(RuntimeNode::RemoteCursor { .. })) => {
                CursorStepInspection::Claimable(None)
            }
            None => CursorStepInspection::Gone,
        }
    }

    /// Installs pairless cursor ownership without disturbing an existing
    /// obligation.
    pub fn ensure_pairless_cursor_obligation(&mut self, cursor: NodeId) -> bool {
        assert!(matches!(
            self.node(cursor),
            Some(RuntimeNode::RemoteCursor { .. })
        ));
        assert!(
            self.active_pair_key(cursor).is_none(),
            "an active-pair cursor cannot receive a pairless obligation"
        );
        if self.cursor_obligations.contains_key(&cursor) {
            return false;
        }
        assert!(
            self.cursor_obligations
                .insert(
                    cursor,
                    PairlessCursorObligation {
                        cursor,
                        state: PairlessCursorState::Ready,
                    },
                )
                .is_none()
        );
        true
    }

    pub fn claim_pairless_cursor_obligation(&mut self, cursor: NodeId) -> bool {
        let Some(obligation) = self.cursor_obligations.get_mut(&cursor) else {
            return false;
        };
        if !matches!(
            obligation.state,
            PairlessCursorState::Ready | PairlessCursorState::Blocked(_)
        ) {
            return false;
        }
        obligation.state = PairlessCursorState::Claimed;
        true
    }

    pub fn block_pairless_cursor_obligation(
        &mut self,
        cursor: NodeId,
        dependency: CursorDependency<S>,
    ) -> bool {
        let Some(obligation) = self.cursor_obligations.get_mut(&cursor) else {
            return false;
        };
        if !matches!(obligation.state, PairlessCursorState::Claimed) {
            return false;
        }
        obligation.state = PairlessCursorState::Blocked(dependency);
        true
    }

    pub fn stabilize_pairless_cursor_obligation(&mut self, cursor: NodeId) -> bool {
        let Some(obligation) = self.cursor_obligations.get_mut(&cursor) else {
            return false;
        };
        if !matches!(obligation.state, PairlessCursorState::Claimed) {
            return false;
        }
        obligation.state = PairlessCursorState::Stable;
        true
    }

    #[cfg(test)]
    fn assert_cursor_obligation_invariants(&self) {
        for (cursor, obligation) in &self.cursor_obligations {
            assert_eq!(*cursor, obligation.cursor);
            assert!(matches!(
                self.node(*cursor),
                Some(RuntimeNode::RemoteCursor { .. })
            ));
            assert_eq!(
                self.cursor_claim_owner(*cursor),
                Some(CursorClaimOwner::Obligation)
            );
        }
    }

    #[cfg(test)]
    pub fn contains_active_pair(&self, pair: ActivePairKey) -> bool {
        self.active.contains_key(&pair)
    }

    /// Recovers both endpoints of an active-pair key from the live graph.
    pub fn active_pair_nodes(&self, pair: ActivePairKey) -> Option<(NodeId, NodeId)> {
        self.pair_nodes(pair)
    }

    /// Stable evaluator-owned anchor wired to the net's exposed template port.
    pub fn exposed(&self) -> Port {
        self.exposed
            .expect("runtime net was constructed without an exposed port")
    }

    #[cfg(test)]
    fn ready_pairs(&self) -> Vec<ActivePairKey> {
        self.active
            .iter()
            .filter_map(|(pair, state)| matches!(state, ActivePairState::Ready).then_some(*pair))
            .collect()
    }

    pub fn blocked_cursors(&self) -> BTreeMap<ActivePairKey, BlockedCursor> {
        self.active
            .iter()
            .filter_map(|(pair, state)| match state {
                ActivePairState::BlockedCursor { cursor, .. } => Some((
                    *pair,
                    BlockedCursor {
                        pair: *pair,
                        cursor: *cursor,
                    },
                )),
                _ => None,
            })
            .collect()
    }

    #[cfg(test)]
    pub fn blocked_calls(&self) -> impl Iterator<Item = BlockedCall<S::WaitToken>> + '_ {
        self.active.iter().filter_map(|(pair, state)| match state {
            ActivePairState::BlockedCall { wait } => Some(BlockedCall {
                pair: *pair,
                wait: wait.clone(),
            }),
            _ => None,
        })
    }

    #[cfg(test)]
    pub fn blocked_call(&self, pair: ActivePairKey) -> Option<BlockedCall<S::WaitToken>> {
        match self.active.get(&pair) {
            Some(ActivePairState::BlockedCall { wait }) => Some(BlockedCall {
                pair,
                wait: wait.clone(),
            }),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn blocked_operator_call(
        &self,
        pair: ActivePairKey,
    ) -> Option<BlockedOperatorCall<S::WaitToken>> {
        match self.active.get(&pair) {
            Some(ActivePairState::BlockedOperatorCall { wait }) => Some(BlockedOperatorCall {
                pair,
                wait: wait.clone(),
            }),
            _ => None,
        }
    }

    /// Recovers the structural call represented by a principal `Bind >< Data`
    /// pair. Pair state is deliberately irrelevant so a blocked call can be
    /// reclaimed after its exact wait completes.
    pub fn call(&self, pair: ActivePairKey) -> Option<Call> {
        let (left, right) = self.active_pair_nodes(pair)?;
        match (self.node(left), self.node(right)) {
            (Some(RuntimeNode::Bind), Some(RuntimeNode::Data(_))) => Some(Call {
                pair,
                bind: left,
                data: right,
            }),
            (Some(RuntimeNode::Data(_)), Some(RuntimeNode::Bind)) => Some(Call {
                pair,
                bind: right,
                data: left,
            }),
            _ => None,
        }
    }

    pub fn cursor_dependency(&self, cursor: NodeId) -> Option<CursorDependency<S>> {
        match self.cursor_claim_owner(cursor) {
            Some(CursorClaimOwner::ActivePair(pair)) => match self.active.get(&pair) {
                Some(ActivePairState::BlockedCursor {
                    cursor: blocked,
                    blockage: CursorBlockage::Dependency(dependency),
                }) if *blocked == cursor => Some(dependency.clone()),
                _ => None,
            },
            Some(CursorClaimOwner::Obligation) => {
                match &self.cursor_obligations.get(&cursor)?.state {
                    PairlessCursorState::Blocked(dependency) => Some(dependency.clone()),
                    PairlessCursorState::Ready
                    | PairlessCursorState::Claimed
                    | PairlessCursorState::Stable => None,
                }
            }
            None => None,
        }
    }

    pub(crate) fn cursor_obligations(&self) -> impl Iterator<Item = CursorObligationSnapshot> + '_ {
        self.cursor_obligations
            .iter()
            .map(|(cursor, obligation)| CursorObligationSnapshot {
                cursor: *cursor,
                status: match &obligation.state {
                    PairlessCursorState::Ready => CursorObligationStatus::Ready,
                    PairlessCursorState::Claimed => CursorObligationStatus::Claimed,
                    PairlessCursorState::Blocked(_) => CursorObligationStatus::Blocked,
                    PairlessCursorState::Stable => CursorObligationStatus::Stable,
                },
            })
    }

    pub fn interface_cursor(&self, interface: Port) -> Option<NodeId> {
        self.assert_interface(interface);
        self.cursor_across(interface)
    }

    pub fn stuck_pairs(&self) -> impl Iterator<Item = StuckPair<S::StuckReason>> + '_ {
        self.active.iter().filter_map(|(pair, state)| match state {
            ActivePairState::Stuck(reason) => Some(StuckPair {
                pair: *pair,
                reason: reason.clone(),
            }),
            _ => None,
        })
    }

    pub fn stuck_reason(&self, pair: ActivePairKey) -> Option<&StuckReason<S::StuckReason>> {
        match self.active.get(&pair) {
            Some(ActivePairState::Stuck(reason)) => Some(reason),
            _ => None,
        }
    }

    pub fn node(&self, id: NodeId) -> Option<&RuntimeNode<S>> {
        self.nodes.get(&id).map(|entry| &entry.node)
    }

    /// Reads callable data from an active pair already claimed by reduction.
    pub fn claim_call(&mut self, call: Call) -> Option<S::Data> {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
        {
            return None;
        }
        let callable = match self.node(call.data) {
            Some(RuntimeNode::Data(data)) => data.clone(),
            _ => panic!("claimed call data node must exist"),
        };
        Some(callable)
    }

    /// Leaves a claimed call permanently stuck after applicable lowering
    /// fails.
    pub fn fail_claimed_call(&mut self, call: Call, reason: S::StuckReason) {
        let previous = self.active.insert(
            call.pair,
            ActivePairState::Stuck(StuckReason::Specialization(reason)),
        );
        assert!(
            matches!(previous, Some(ActivePairState::Claimed)),
            "failed call must still be claimed"
        );
    }

    /// Suspends an exact claimed call on specialization-owned external work.
    pub fn block_claimed_call(&mut self, call: Call, wait: S::WaitToken) {
        let previous = self
            .active
            .insert(call.pair, ActivePairState::BlockedCall { wait });
        assert!(
            matches!(previous, Some(ActivePairState::Claimed)),
            "blocked call must still be claimed"
        );
    }

    /// Claims a blocked call only when the wakeup identifies its current wait.
    pub fn retry_blocked_call(&mut self, call: Call, wait: &S::WaitToken) -> bool {
        if !matches!(
            self.active.get(&call.pair),
            Some(ActivePairState::BlockedCall { wait: current }) if current == wait
        ) {
            return false;
        }
        self.active.insert(call.pair, ActivePairState::Claimed);
        true
    }

    /// Clones a pending operator transition so specialization code can run without
    /// holding the shared runtime-net mutex.
    pub fn operator_call_parts(&self, call: OperatorCall) -> (S::Operator, S::Data) {
        assert!(
            self.active
                .get(&call.pair)
                .is_some_and(ActivePairState::is_claimed)
        );
        let operator = match self.node(call.operator) {
            Some(RuntimeNode::Operator(operator)) => operator.clone(),
            _ => panic!("pending operator call agent must exist"),
        };
        let data = match self.node(call.data) {
            Some(RuntimeNode::Data(data)) => data.clone(),
            _ => panic!("pending operator call data must exist"),
        };
        (operator, data)
    }

    /// Recovers the structural operator call represented by a principal
    /// `Operator >< Data` pair. Pair state is deliberately irrelevant so a
    /// blocked operation can be reclaimed after its exact wait completes.
    pub fn operator_call(&self, pair: ActivePairKey) -> Option<OperatorCall> {
        let (left, right) = self.active_pair_nodes(pair)?;
        match (self.node(left), self.node(right)) {
            (Some(RuntimeNode::Operator(_)), Some(RuntimeNode::Data(_))) => Some(OperatorCall {
                pair,
                operator: left,
                data: right,
            }),
            (Some(RuntimeNode::Data(_)), Some(RuntimeNode::Operator(_))) => Some(OperatorCall {
                pair,
                operator: right,
                data: left,
            }),
            _ => None,
        }
    }

    /// Suspends an exact claimed operator call on specialization-owned
    /// external work.
    pub fn block_claimed_operator_call(&mut self, call: OperatorCall, wait: S::WaitToken) {
        let previous = self
            .active
            .insert(call.pair, ActivePairState::BlockedOperatorCall { wait });
        assert!(
            matches!(previous, Some(ActivePairState::Claimed)),
            "blocked operator call must still be claimed"
        );
    }

    /// Claims a blocked operator call only when the wakeup identifies its
    /// current wait.
    pub fn retry_blocked_operator_call(&mut self, call: OperatorCall, wait: &S::WaitToken) -> bool {
        if !matches!(
            self.active.get(&call.pair),
            Some(ActivePairState::BlockedOperatorCall { wait: current }) if current == wait
        ) {
            return false;
        }
        self.active.insert(call.pair, ActivePairState::Claimed);
        true
    }

    pub fn complete_operator_call(
        &mut self,
        call: OperatorCall,
        result: OperatorYield<S>,
    ) -> NodeId {
        let target = self.take_operator_call(call);
        match result {
            OperatorYield::Data(data) => {
                let node = self.add_node(RuntimeNode::Data(data));
                self.connect(Port::principal(node), target);
                node
            }
            OperatorYield::Operator(operator) => {
                let bind = self.add_node(RuntimeNode::Bind);
                let operator = self.add_node(RuntimeNode::Operator(operator));
                self.connect(Port::principal(bind), target);
                self.connect(Port::auxiliary(bind, 1), Port::principal(operator));
                self.connect(Port::auxiliary(bind, 2), Port::auxiliary(operator, 1));
                bind
            }
        }
    }

    pub fn fail_operator_call(&mut self, call: OperatorCall, reason: S::StuckReason) {
        let previous = self.active.insert(
            call.pair,
            ActivePairState::Stuck(StuckReason::Specialization(reason)),
        );
        assert!(
            matches!(previous, Some(ActivePairState::Claimed)),
            "failed operator call must still be claimed"
        );
    }

    pub fn interface_data(&self, interface: Port) -> Option<&S::Data> {
        self.assert_interface(interface);
        let neighbor = self.neighbor(interface)?;
        if !neighbor.is_principal() {
            return None;
        }
        match self.node(neighbor.node())? {
            RuntimeNode::Data(data) => Some(data),
            _ => None,
        }
    }

    pub fn interface_neighbor(&self, interface: Port) -> Option<Port> {
        self.assert_interface(interface);
        self.neighbor(interface)
    }

    /// Finds the exact local active pair that can advance an interface whose
    /// current value is connected through auxiliary result ports.
    pub fn interface_dependency(&self, interface: Port) -> Option<ActivePairKey> {
        self.assert_interface(interface);
        let mut port = self.neighbor(interface)?;
        let mut visited = HashSet::new();
        while !port.is_principal() && visited.insert(port.node()) {
            let neighbor = self.neighbor(Port::principal(port.node()))?;
            if neighbor.is_principal() {
                return Some(ActivePairKey::new(port.node(), neighbor.node()));
            }
            port = neighbor;
        }
        None
    }

    /// Returns the port wired to `port`, for evaluator diagnostics and demand
    /// propagation across evaluator-owned interfaces.
    pub fn port_neighbor(&self, port: Port) -> Option<Port> {
        self.neighbor(port)
    }

    #[cfg(test)]
    pub fn demand_interface(&mut self, interface: Port) -> Option<CursorProgress> {
        self.assert_interface(interface);
        let cursor = self.cursor_across(interface)?;
        self.begin_cursor_claim(cursor, None)
    }

    /// Claims a cursor reached through an exact layered-copy dependency.
    #[cfg(test)]
    pub fn claim_dependent_cursor(&mut self, cursor: NodeId) -> Option<CursorProgress> {
        if !matches!(self.node(cursor), Some(RuntimeNode::RemoteCursor { .. })) {
            return None;
        }
        self.begin_cursor_claim(cursor, None)
    }

    /// Claims pairless cursor demand through its owning-net obligation.
    /// Nested source-frontier observations use this entry point so the work
    /// remains enumerable even when no active pair owns the cursor.
    #[cfg(test)]
    pub fn claim_cursor_obligation(&mut self, cursor: NodeId) -> Option<CursorProgress> {
        if !matches!(self.node(cursor), Some(RuntimeNode::RemoteCursor { .. }))
            || self.active_pair_key(cursor).is_some()
        {
            return None;
        }
        self.ensure_pairless_cursor_obligation(cursor);
        self.claim_pairless_cursor_obligation(cursor)
            .then_some(CursorProgress::Claimed)
    }

    pub fn retry_blocked_cursor(&mut self, cursor: NodeId) -> bool {
        match self.cursor_claim_owner(cursor) {
            Some(CursorClaimOwner::ActivePair(pair))
                if matches!(
                    self.active.get(&pair),
                    Some(ActivePairState::BlockedCursor {
                        cursor: blocked,
                        blockage: CursorBlockage::Dependency(_),
                    }) if *blocked == cursor
                ) =>
            {
                self.active.insert(pair, ActivePairState::Ready);
                true
            }
            Some(CursorClaimOwner::Obligation)
                if matches!(
                    self.cursor_obligations.get(&cursor),
                    Some(PairlessCursorObligation {
                        state: PairlessCursorState::Blocked(_),
                        ..
                    })
                ) =>
            {
                self.cursor_obligations.get_mut(&cursor).unwrap().state =
                    PairlessCursorState::Ready;
                true
            }
            _ => false,
        }
    }

    pub fn reduce_next(&mut self) -> Option<Reduction> {
        let pair = self
            .active
            .iter()
            .find_map(|(pair, state)| matches!(state, ActivePairState::Ready).then_some(*pair))?;
        self.reduce_pair(pair)
    }

    /// Reduces one exact ready pair. Cursor demand uses this to make progress
    /// in the source runtime without searching or sweeping unrelated work.
    pub fn reduce_pair(&mut self, pair: ActivePairKey) -> Option<Reduction> {
        if !self
            .active
            .get(&pair)
            .is_some_and(ActivePairState::is_ready)
        {
            return None;
        }
        *self.active.get_mut(&pair).unwrap() = ActivePairState::Claimed;
        let (left_id, right_id) = self
            .pair_nodes(pair)
            .expect("ready pair key must identify a principal-principal wire");
        let left = self
            .node(left_id)
            .expect("ready pair left node must exist")
            .clone();
        let right = self
            .node(right_id)
            .expect("ready pair right node must exist")
            .clone();
        let cursor = match (&left, &right) {
            (RuntimeNode::RemoteCursor { .. }, _) => Some(left_id),
            (_, RuntimeNode::RemoteCursor { .. }) => Some(right_id),
            _ => None,
        };
        if let Some(cursor) = cursor {
            let progress = self
                .begin_cursor_claim(cursor, Some(pair))
                .expect("ready cursor pair must be claimable");
            return Some(Reduction {
                pair,
                kind: ReductionKind::RemoteCursor { cursor, progress },
            });
        }
        let kind = match (&left, &right) {
            (RuntimeNode::Bind, RuntimeNode::Bind) => {
                self.join(left_id, right_id, 2);
                ReductionKind::BindJoin
            }
            (RuntimeNode::Fan { identity: left }, RuntimeNode::Fan { identity: right }) => {
                if left == right {
                    self.join(left_id, right_id, 2);
                    ReductionKind::FanJoin {
                        identity: left.clone(),
                    }
                } else {
                    self.commute_fans(left_id, left, right_id, right);
                    ReductionKind::FanCommute {
                        left: left.clone(),
                        right: right.clone(),
                    }
                }
            }
            (RuntimeNode::Fan { identity }, RuntimeNode::Data(_)) => {
                self.duplicate_data(left_id, right_id);
                ReductionKind::FanData {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Data(_), RuntimeNode::Fan { identity }) => {
                self.duplicate_data(right_id, left_id);
                ReductionKind::FanData {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Fan { identity }, RuntimeNode::Bind) => {
                self.duplicate_bind(left_id, identity, right_id);
                ReductionKind::FanBind {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Bind, RuntimeNode::Fan { identity }) => {
                self.duplicate_bind(right_id, identity, left_id);
                ReductionKind::FanBind {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Fan { identity }, RuntimeNode::Operator(_)) => {
                self.duplicate_operator(left_id, identity, right_id);
                ReductionKind::FanOperator {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Operator(_), RuntimeNode::Fan { identity }) => {
                self.duplicate_operator(right_id, identity, left_id);
                ReductionKind::FanOperator {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Erase, _) => {
                self.erase(left_id, right_id);
                ReductionKind::Erase
            }
            (_, RuntimeNode::Erase) => {
                self.erase(right_id, left_id);
                ReductionKind::Erase
            }
            (RuntimeNode::Bind, RuntimeNode::Data(_)) => ReductionKind::Call {
                bind: left_id,
                data: right_id,
            },
            (RuntimeNode::Data(_), RuntimeNode::Bind) => ReductionKind::Call {
                bind: right_id,
                data: left_id,
            },
            (RuntimeNode::Operator(_), RuntimeNode::Data(_)) => ReductionKind::OperatorCall {
                operator: left_id,
                data: right_id,
            },
            (RuntimeNode::Data(_), RuntimeNode::Operator(_)) => ReductionKind::OperatorCall {
                operator: right_id,
                data: left_id,
            },
            (RuntimeNode::Data(_), RuntimeNode::Data(_)) => {
                *self.active.get_mut(&pair).unwrap() = ActivePairState::Stuck(StuckReason::NoRule);
                ReductionKind::Stuck
            }
            (RuntimeNode::Operator(_), _) | (_, RuntimeNode::Operator(_)) => {
                *self.active.get_mut(&pair).unwrap() = ActivePairState::Stuck(StuckReason::NoRule);
                ReductionKind::Stuck
            }
            (RuntimeNode::Interface, _)
            | (_, RuntimeNode::Interface)
            | (RuntimeNode::RemoteCursor { .. }, _)
            | (_, RuntimeNode::RemoteCursor { .. }) => {
                unreachable!("evaluator-only nodes do not use ordinary interaction rules")
            }
        };
        if !matches!(
            kind,
            ReductionKind::Call { .. }
                | ReductionKind::OperatorCall { .. }
                | ReductionKind::RemoteCursor { .. }
                | ReductionKind::Stuck
        ) {
            assert!(
                self.active
                    .remove(&pair)
                    .is_some_and(|state| state.is_claimed())
            );
        }
        Some(Reduction { pair, kind })
    }
}
