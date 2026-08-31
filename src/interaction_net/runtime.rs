use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::model::*;

mod cursor;
mod graph;
mod rewrite;

pub(crate) use cursor::PreparedCopySource;

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

/// One locked observation of the work required by an evaluator-owned
/// interface demand.
///
/// This classifies only the root frontier. Cursor and active-pair state is
/// interpreted by `step_cursor` and `step_active_pair`, except that a cursor
/// already known to be stable is terminal for this request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceDemand {
    Data,
    Bind,
    NormalForm,
    StableCursor(NodeId),
    Cursor(NodeId),
    ActivePair(ActivePairKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorDependencyDisposition {
    Progressed,
    Stable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorDependencyResolution {
    Resolved,
    Disturbed,
    Gone,
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
/// retained; a disturbed observation is reconstructed from the authoritative
/// parent cursor and evaluator request root.
#[derive(Clone, PartialEq, Eq)]
pub struct FrontierObservation<S: NetSpecialization> {
    source: SharedRuntimeNet<S>,
    observed_topology: u64,
    endpoint: DemandEndpoint,
}

impl<S: NetSpecialization> FrontierObservation<S> {
    pub fn source(&self) -> &SharedRuntimeNet<S> {
        &self.source
    }

    pub fn endpoint(&self) -> DemandEndpoint {
        self.endpoint
    }

    /// Takes one non-blocking step at the observed pair. Unlike `reduce_pair`,
    /// this reports claimed, blocked, stuck, gone, and disturbed states
    /// explicitly for an iterative normalization driver.
    pub fn step_active_pair(&self, pair: ActivePairKey) -> ActivePairStep<S> {
        assert_eq!(self.endpoint, DemandEndpoint::ActivePair(pair));
        self.source
            .step_active_pair_if_current(pair, Some(self.observed_topology))
    }

    /// Takes one non-blocking step at the observed cursor endpoint.
    pub fn step_cursor(&self, cursor: NodeId) -> CursorStep<S> {
        assert_eq!(self.endpoint, DemandEndpoint::Cursor(cursor));
        self.source
            .step_cursor_if_current(cursor, Some(self.observed_topology))
    }
}

impl<S: NetSpecialization> fmt::Debug for FrontierObservation<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrontierObservation")
            .field("source", &self.source)
            .field("observed_topology", &self.observed_topology)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum CursorDependency<S: NetSpecialization> {
    LocalCursor(NodeId),
    /// Versioned observation of a source cursor. Work is claimed through that
    /// cursor's owning-net obligation rather than directly from its observer.
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

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorObligationStatus {
    Ready,
    Claimed,
    Blocked,
    Stable,
}

#[cfg(test)]
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

#[cfg(test)]
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
        let source = source.prepare_copy_source();
        let mut target = RuntimeNet::empty();
        let cursor = target.begin_copy(source);
        let interface = target.add_interface(Port::principal(cursor));
        target.exposed = Some(interface);
        (Self::new(target), interface)
    }

    #[cfg(test)]
    pub(crate) fn test_stable_auxiliary() -> (Self, Port) {
        let mut runtime = RuntimeNet::empty();
        let bind = runtime.add_node(RuntimeNode::Bind);
        let interface = runtime.add_interface(Port::auxiliary(bind, 1));
        runtime.exposed = Some(interface);
        (Self::new(runtime), interface)
    }

    #[cfg(test)]
    pub(crate) fn test_pair_owned_copy_layer(source: Self) -> (Self, Port, NodeId) {
        let source = source.prepare_copy_source();
        let mut target = RuntimeNet::empty();
        let bind = target.add_node(RuntimeNode::Bind);
        let cursor = target.begin_copy(source);
        target.connect(Port::principal(bind), Port::principal(cursor));
        let interface = target.add_interface(Port::auxiliary(bind, 1));
        target.exposed = Some(interface);
        (Self::new(target), interface, cursor)
    }

    /// Builds a transparent data-producing layer whose cursor transition is
    /// owned by an active pair rather than a pairless obligation.
    #[cfg(test)]
    pub(crate) fn test_productive_pair_owned_copy_layer(source: Self) -> (Self, Port) {
        let source = source.prepare_copy_source();
        let mut target = RuntimeNet::empty();
        let site = FanSite(target.next_fan_site);
        target.next_fan_site = target
            .next_fan_site
            .checked_add(1)
            .expect("interaction-net fan site space exhausted");
        let fan = target.add_node(RuntimeNode::Fan {
            identity: FanIdentity::root(site),
        });
        let cursor = target.begin_copy(source);
        target.connect(Port::principal(fan), Port::principal(cursor));
        let discard = target.add_node(RuntimeNode::Erase);
        target.connect(Port::auxiliary(fan, 2), Port::principal(discard));
        let interface = target.add_interface(Port::auxiliary(fan, 1));
        target.exposed = Some(interface);
        (Self::new(target), interface)
    }

    #[cfg(test)]
    pub(crate) fn test_stable_root_with_claimed_cursor(source: Self) -> (Self, Port, NodeId) {
        let source = source.prepare_copy_source();
        let mut target = RuntimeNet::empty();
        let root = target.add_node(RuntimeNode::Erase);
        let interface = target.add_interface(Port::principal(root));
        let cursor = target.begin_copy(source);
        assert!(target.ensure_pairless_cursor_obligation(cursor));
        assert!(target.claim_pairless_cursor_obligation(cursor));
        target.exposed = Some(interface);
        (Self::new(target), interface, cursor)
    }

    #[cfg(test)]
    pub(crate) fn test_claim_pairless_cursor_obligation(&self, cursor: NodeId) -> bool {
        self.with_mut(|runtime| runtime.claim_pairless_cursor_obligation(cursor))
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

    #[cfg(test)]
    pub(crate) fn with_optional_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<S>) -> Option<R>,
    ) -> Option<R> {
        self.with_conditional_mut(|runtime| match update(runtime) {
            Some(result) => RuntimeNetMutation::Changed(Some(result)),
            None => RuntimeNetMutation::Unchanged(None),
        })
    }

    pub fn poll_interface_demand(&self, interface: Port) -> InterfaceDemand {
        self.with_conditional_mut(|runtime| runtime.poll_interface_demand(interface))
    }

    pub fn resolve_cursor_dependency(
        &self,
        cursor: NodeId,
        expected: &CursorDependency<S>,
        disposition: CursorDependencyDisposition,
    ) -> CursorDependencyResolution {
        self.with_conditional_mut(|runtime| {
            let resolution = runtime.resolve_cursor_dependency(cursor, expected, disposition);
            if resolution == CursorDependencyResolution::Resolved {
                RuntimeNetMutation::Changed(resolution)
            } else {
                RuntimeNetMutation::Unchanged(resolution)
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
            return match state.runtime.active.get(&pair) {
                Some(ActivePairState::Stuck(reason)) => ActivePairStep::Stuck(StuckPair {
                    pair,
                    reason: reason.clone(),
                }),
                _ => ActivePairStep::Disturbed,
            };
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
    anchor: Port,
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

    #[cfg(test)]
    pub fn active_pairs(&self) -> impl ExactSizeIterator<Item = ActivePairKey> + '_ {
        self.active.keys().copied()
    }

    #[cfg(test)]
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
    fn ensure_pairless_cursor_obligation(&mut self, cursor: NodeId) -> bool {
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

    fn claim_pairless_cursor_obligation(&mut self, cursor: NodeId) -> bool {
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

    fn block_pairless_cursor_obligation(
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

    fn stabilize_pairless_cursor_obligation(&mut self, cursor: NodeId) -> bool {
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

    #[cfg(test)]
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

    #[cfg(test)]
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

    #[cfg(test)]
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

    #[cfg(test)]
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
    pub fn claim_call(&self, call: Call) -> Option<S::Data> {
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

    /// Releases a freshly claimed call back to the ready worklist.
    pub fn release_claimed_call(&mut self, call: Call) -> bool {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
        {
            return false;
        }
        self.active.insert(call.pair, ActivePairState::Ready);
        true
    }

    /// Restores an exact retried call to the wait it held before reclamation.
    pub fn restore_blocked_call(&mut self, call: Call, wait: S::WaitToken) -> bool {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
        {
            return false;
        }
        self.active
            .insert(call.pair, ActivePairState::BlockedCall { wait });
        true
    }

    /// Clones a claimed operator transition so specialization code can run
    /// without holding the shared runtime-net mutex.
    pub fn claim_operator_call(&self, call: OperatorCall) -> Option<(S::Operator, S::Data)> {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
        {
            return None;
        }
        let operator = match self.node(call.operator) {
            Some(RuntimeNode::Operator(operator)) => operator.clone(),
            _ => panic!("pending operator call agent must exist"),
        };
        let data = match self.node(call.data) {
            Some(RuntimeNode::Data(data)) => data.clone(),
            _ => panic!("pending operator call data must exist"),
        };
        Some((operator, data))
    }

    /// Clones a pending operator transition after asserting that it remains
    /// claimed. This compatibility helper does not acquire ownership.
    #[cfg(test)]
    pub fn operator_call_parts(&self, call: OperatorCall) -> (S::Operator, S::Data) {
        self.claim_operator_call(call)
            .expect("pending operator call must remain claimed")
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

    /// Releases a freshly claimed operator call back to the ready worklist.
    pub fn release_claimed_operator_call(&mut self, call: OperatorCall) -> bool {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
        {
            return false;
        }
        self.active.insert(call.pair, ActivePairState::Ready);
        true
    }

    /// Restores an exact retried operator call to its prior blocked wait.
    pub fn restore_blocked_operator_call(
        &mut self,
        call: OperatorCall,
        wait: S::WaitToken,
    ) -> bool {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
        {
            return false;
        }
        self.active
            .insert(call.pair, ActivePairState::BlockedOperatorCall { wait });
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

    #[cfg(test)]
    pub fn interface_neighbor(&self, interface: Port) -> Option<Port> {
        self.assert_interface(interface);
        self.neighbor(interface)
    }

    fn poll_interface_demand(&mut self, interface: Port) -> RuntimeNetMutation<InterfaceDemand> {
        self.assert_interface(interface);
        let Some(neighbor) = self.neighbor(interface) else {
            return RuntimeNetMutation::Unchanged(InterfaceDemand::NormalForm);
        };

        if neighbor.is_principal() {
            let node = neighbor.node();
            let demand = match self.node(node) {
                Some(RuntimeNode::Data(_)) => InterfaceDemand::Data,
                Some(RuntimeNode::Bind) => InterfaceDemand::Bind,
                Some(RuntimeNode::RemoteCursor { .. }) => {
                    let inserted = self.cursor_claim_owner(node).is_none()
                        && self.ensure_pairless_cursor_obligation(node);
                    let demand =
                        if matches!(self.inspect_cursor_step(node), CursorStepInspection::Stable) {
                            InterfaceDemand::StableCursor(node)
                        } else {
                            InterfaceDemand::Cursor(node)
                        };
                    return if inserted {
                        RuntimeNetMutation::Changed(demand)
                    } else {
                        RuntimeNetMutation::Unchanged(demand)
                    };
                }
                Some(
                    RuntimeNode::Fan { .. }
                    | RuntimeNode::Erase
                    | RuntimeNode::Operator(_)
                    | RuntimeNode::Interface,
                )
                | None => InterfaceDemand::NormalForm,
            };
            return RuntimeNetMutation::Unchanged(demand);
        }

        let mut port = neighbor;
        let mut visited = HashSet::new();
        let pair = loop {
            if port.is_principal() || !visited.insert(port.node()) {
                break None;
            }
            let Some(principal_neighbor) = self.neighbor(Port::principal(port.node())) else {
                break None;
            };
            if principal_neighbor.is_principal() {
                break Some(ActivePairKey::new(port.node(), principal_neighbor.node()));
            }
            port = principal_neighbor;
        };
        let Some(pair) = pair else {
            return RuntimeNetMutation::Unchanged(InterfaceDemand::NormalForm);
        };
        let demand = match self.active.get(&pair) {
            Some(ActivePairState::BlockedCursor {
                cursor,
                blockage: CursorBlockage::Stable,
            }) => InterfaceDemand::StableCursor(*cursor),
            _ => InterfaceDemand::ActivePair(pair),
        };
        RuntimeNetMutation::Unchanged(demand)
    }

    /// Returns the port wired to `port`, for evaluator diagnostics and demand
    /// propagation across evaluator-owned interfaces.
    #[cfg(test)]
    pub fn port_neighbor(&self, port: Port) -> Option<Port> {
        self.neighbor(port)
    }

    #[cfg(test)]
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

    fn resolve_cursor_dependency(
        &mut self,
        cursor: NodeId,
        expected: &CursorDependency<S>,
        disposition: CursorDependencyDisposition,
    ) -> CursorDependencyResolution {
        let Some(owner) = self.cursor_claim_owner(cursor) else {
            return CursorDependencyResolution::Gone;
        };
        let matches_expected = match owner {
            CursorClaimOwner::ActivePair(pair) => matches!(
                self.active.get(&pair),
                Some(ActivePairState::BlockedCursor {
                    cursor: blocked,
                    blockage: CursorBlockage::Dependency(actual),
                }) if *blocked == cursor && actual == expected
            ),
            CursorClaimOwner::Obligation => matches!(
                self.cursor_obligations.get(&cursor),
                Some(PairlessCursorObligation {
                    state: PairlessCursorState::Blocked(actual),
                    ..
                }) if actual == expected
            ),
        };
        if !matches_expected {
            return CursorDependencyResolution::Disturbed;
        }

        match owner {
            CursorClaimOwner::ActivePair(pair) => {
                self.active.insert(
                    pair,
                    match disposition {
                        CursorDependencyDisposition::Progressed => ActivePairState::Ready,
                        CursorDependencyDisposition::Stable => ActivePairState::BlockedCursor {
                            cursor,
                            blockage: CursorBlockage::Stable,
                        },
                    },
                );
            }
            CursorClaimOwner::Obligation => {
                self.cursor_obligations
                    .get_mut(&cursor)
                    .expect("cursor obligation owner must remain installed")
                    .state = match disposition {
                    CursorDependencyDisposition::Progressed => PairlessCursorState::Ready,
                    CursorDependencyDisposition::Stable => PairlessCursorState::Stable,
                };
            }
        }
        CursorDependencyResolution::Resolved
    }

    /// Reduces one arbitrary ready pair. Cursor-WHNF evaluation deliberately
    /// uses exact demand endpoints instead; this remains the generic runtime's
    /// ordinary reducer and a low-level test utility.
    #[allow(dead_code)]
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
