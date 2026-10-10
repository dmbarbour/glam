use std::collections::BTreeMap;

use crate::trusted_hash::TrustedHashMap;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};

use crate::counted_condvar::CountedCondvar;

use super::model::*;

mod cursor;
mod graph;
mod rewrite;
mod whole_copy;

pub(crate) use cursor::PreparedCopySource;
pub(crate) use whole_copy::WholeCopy;

#[cfg(test)]
mod tests;

impl<S: NetSpecialization> InteractionNet<S> {
    #[cfg(test)]
    pub fn instantiate(&self) -> RuntimeNet<S>
    where
        S::Data: Clone,
        S::Operator: Clone,
        S::RuntimeSource: Clone + PartialEq,
    {
        RuntimeNet::new(self, &DIRECT_RUNTIME_NET_MUTATION_GATEWAY)
    }

    pub(crate) fn instantiate_with(
        &self,
        duplicator: &impl RuntimeNetPayloadDuplicator<S>,
    ) -> RuntimeNet<S> {
        RuntimeNet::new(self, duplicator)
    }

    #[cfg(test)]
    pub fn instantiate_shared(&self) -> SharedRuntimeNet<S>
    where
        S: NetSpecialization<RuntimeSource = SharedRuntimeNet<S>>,
        S::Data: Clone,
        S::Operator: Clone,
    {
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
    CallableCheckpoint {
        bind: NodeId,
        checkpoint: NodeId,
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
    /// A raw `RuntimeNet::reduce_pair` has reserved the transition but has not
    /// inspected its source frontier. Shared runtime steps consume this state
    /// behind a private guard and never publish it to the core evaluator.
    Claimed,
    Materialized {
        node: NodeId,
    },
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
pub struct NetContention {
    disturbance: RuntimeNetDisturbance,
    revisions: RuntimeNetRevisions,
}

impl NetContention {
    #[cfg(test)]
    pub fn revisions(&self) -> RuntimeNetRevisions {
        self.revisions
    }

    /// Waits for progress beyond the revisions observed at contention.
    /// `false` means the semantic net closed instead.
    pub fn wait_for_disturbance(&self) -> bool {
        self.disturbance
            .wait_for_change(self.revisions.disturbance_epoch())
    }
}

impl fmt::Debug for NetContention {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetContention")
            .field("revisions", &self.revisions)
            .finish()
    }
}

#[derive(Debug)]
pub enum CursorStep<S: NetSpecialization> {
    Progressed(CursorProgress),
    Dependency(CursorDependency<S>),
    Stable,
    Contended(NetContention),
    Disturbed,
    Gone,
    /// The cursor was claimable, but its caller refused the claim.
    NotAdmitted,
}

/// What a step is about to claim, so its caller can decide whether to admit
/// the claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimKind {
    /// A rule application: a rewrite, a cursor step, or a new semantic call.
    Reduction,
    /// Resuming a callable checkpoint's suspended evaluation. This continues
    /// an earlier call rather than applying a new rule.
    CheckpointResumption,
}

#[derive(Debug)]
pub enum ActivePairStep<S: NetSpecialization> {
    Reduction(Reduction),
    Cursor(NodeId),
    BlockedCallableCheckpoint(BlockedCallableCheckpoint<S::WaitToken>),
    Stuck(StuckPair<S::StuckReason>),
    Contended(NetContention),
    Disturbed,
    Gone,
    /// The pair was ready, but its caller refused the claim.
    NotAdmitted,
}

/// One versioned observation of the work currently demanded from a source
/// frontier. The complete auxiliary/principal spine is deliberately not
/// retained; a disturbed observation is reconstructed from the authoritative
/// parent cursor and evaluator request root.
pub struct FrontierObservation<S: NetSpecialization> {
    source: S::RuntimeSource,
    observed_topology: u64,
    endpoint: DemandEndpoint,
}

impl<S: NetSpecialization> FrontierObservation<S> {
    fn duplicate_with(&self, gateway: &impl RuntimeNetMutationGateway<S>) -> Self {
        Self {
            source: gateway.duplicate_runtime_source(&self.source),
            observed_topology: self.observed_topology,
            endpoint: self.endpoint,
        }
    }

    fn same_with(&self, other: &Self, gateway: &impl RuntimeNetMutationGateway<S>) -> bool {
        self.observed_topology == other.observed_topology
            && self.endpoint == other.endpoint
            && gateway.same_runtime_source(&self.source, &other.source)
    }

    pub(crate) fn from_snapshot(
        source: S::RuntimeSource,
        observed_topology: u64,
        endpoint: DemandEndpoint,
    ) -> Self {
        Self {
            source,
            observed_topology,
            endpoint,
        }
    }

    pub fn source(&self) -> &S::RuntimeSource {
        &self.source
    }

    pub fn endpoint(&self) -> DemandEndpoint {
        self.endpoint
    }

    pub(crate) fn observed_topology_revision(&self) -> u64 {
        self.observed_topology
    }
}

#[cfg(test)]
impl<S> FrontierObservation<S>
where
    S: NetSpecialization<RuntimeSource = SharedRuntimeNet<S>>,
    S::Data: Clone,
    S::Operator: Clone,
{
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
            .field("source", &"..")
            .field("observed_topology", &self.observed_topology)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

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

impl<S: NetSpecialization> CursorDependency<S> {
    fn duplicate_with(&self, gateway: &impl RuntimeNetMutationGateway<S>) -> Self {
        match self {
            Self::LocalCursor(cursor) => Self::LocalCursor(*cursor),
            Self::SourceCursor(observation) => {
                Self::SourceCursor(observation.duplicate_with(gateway))
            }
            Self::SourceFrontier(observation) => {
                Self::SourceFrontier(observation.duplicate_with(gateway))
            }
        }
    }

    fn same_with(&self, other: &Self, gateway: &impl RuntimeNetMutationGateway<S>) -> bool {
        match (self, other) {
            (Self::LocalCursor(left), Self::LocalCursor(right)) => left == right,
            (Self::SourceCursor(left), Self::SourceCursor(right))
            | (Self::SourceFrontier(left), Self::SourceFrontier(right)) => {
                left.same_with(right, gateway)
            }
            _ => false,
        }
    }

    fn source_runtime(&self) -> Option<&S::RuntimeSource> {
        match self {
            Self::LocalCursor(_) => None,
            Self::SourceCursor(observation) | Self::SourceFrontier(observation) => {
                Some(&observation.source)
            }
        }
    }
}

#[derive(Debug)]
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

#[derive(Debug)]
pub(super) enum CursorBlockage<S: NetSpecialization> {
    Dependency(CursorDependency<S>),
    Stable,
}

impl<S: NetSpecialization> PairlessCursorState<S> {
    fn is_claimed(&self) -> bool {
        matches!(self, Self::Claimed)
    }
}

#[derive(Debug)]
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
pub struct CallableCheckpointCall {
    pub pair: ActivePairKey,
    pub bind: NodeId,
    pub checkpoint: NodeId,
    pub generation: u64,
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
pub struct BlockedCallableCheckpoint<W> {
    pub call: CallableCheckpointCall,
    pub wait: W,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointBlockResult {
    Blocked,
    Disturbed,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedCursor {
    pub pair: ActivePairKey,
    pub cursor: NodeId,
}

#[derive(Debug)]
pub(super) enum ActivePairState<S: NetSpecialization> {
    Ready,
    Claimed,
    BlockedCallableCheckpoint {
        generation: u64,
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

#[cfg(test)]
pub struct SharedRuntimeNet<S: NetSpecialization> {
    inner: Arc<RuntimeNetCell<S>>,
}

/// Synchronization-owning mutable state for one runtime net.
///
/// This cell deliberately has no ownership policy. `SharedRuntimeNet` keeps
/// using `Arc` for generic clients, while the core specialization may place
/// the same cell behind managed ownership without changing its topology or
/// mutation protocol.
pub(crate) struct RuntimeNetCell<S: NetSpecialization> {
    runtime: Mutex<SharedRuntimeNetState<S>>,
    topology_revision: AtomicU64,
    disturbance: RuntimeNetDisturbance,
}

/// Edge-free notification state for runtime-net progress.
///
/// A waiter may retain this companion without retaining the semantic net.
/// Closing the owning cell advances the epoch and wakes every waiter, so a
/// detached companion cannot wait forever on a net which no longer exists.
/// Publishers may acquire the signal mutex while holding the runtime-net
/// mutex; signal waiters never acquire or re-enter the runtime-net mutex, so
/// the lock order has no reverse edge.
#[derive(Clone)]
pub(crate) struct RuntimeNetDisturbance {
    inner: Arc<RuntimeNetDisturbanceInner>,
}

struct RuntimeNetDisturbanceInner {
    changed: CountedCondvar,
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    wait: Mutex<()>,
    epoch: AtomicU64,
    closed: AtomicBool,
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

#[cfg(test)]
pub struct NormalizationBatchLease<S: NetSpecialization> {
    inner: std::sync::Weak<RuntimeNetCell<S>>,
    id: u64,
    closed: bool,
}

/// One access-bounded normalization claim over an ownership-neutral cell.
///
/// Unlike the generic external lease, this guard borrows the cell and cannot
/// become durable machine state. Core evaluation uses this form so no weak or
/// strong owner representation crosses its managed-access boundary.
pub(crate) struct NormalizationBatchGuard<'cell, S: NetSpecialization> {
    cell: &'cell RuntimeNetCell<S>,
    id: u64,
    closed: bool,
    _thread_bound: PhantomData<Rc<()>>,
}

/// Representation-local structural gateway around one runtime-net mutation.
///
/// The generic runtime supplies a no-op implementation. Managed
/// specializations use this seam to keep collector edge accounting inside the
/// same net mutex which authorizes the topology or payload edit.
pub(crate) trait RuntimeNetPayloadDuplicator<S: NetSpecialization> {
    fn duplicate_data(&self, data: &S::Data) -> S::Data;

    fn duplicate_operator(&self, operator: &S::Operator) -> S::Operator;
}

pub(crate) trait RuntimeNetMutationGateway<S: NetSpecialization>:
    RuntimeNetPayloadDuplicator<S>
{
    /// Duplicates a runtime-source edge while the specialization's mutation
    /// authority is active.
    ///
    /// Cursor claims carry this source outside the target-net lock for one
    /// source-frontier inspection. Managed specializations qualify that
    /// temporary edge through their value-access region; the generic direct
    /// specialization retains ordinary owner cloning.
    fn duplicate_runtime_source(&self, source: &S::RuntimeSource) -> S::RuntimeSource;

    fn same_runtime_source(&self, left: &S::RuntimeSource, right: &S::RuntimeSource) -> bool;

    /// Performs an edge-free topology or coordination transition.
    ///
    /// Callers must use `transition_edges` when the update installs or removes
    /// any specialization payload reported by `RuntimeNet::visit_logical_payloads`.
    fn transition<Result>(
        &self,
        runtime: &mut RuntimeNet<S>,
        update: impl FnOnce(&mut RuntimeNet<S>) -> Result,
    ) -> Result {
        self.transition_edges(runtime, RuntimeNetEdgeTransition::default(), update)
    }

    /// Performs one topology transition with exact pre- and post-write owner
    /// addresses for its semantic edge delta.
    fn transition_edges<Result>(
        &self,
        runtime: &mut RuntimeNet<S>,
        edges: RuntimeNetEdgeTransition,
        update: impl FnOnce(&mut RuntimeNet<S>) -> Result,
    ) -> Result;
}

/// Allocation-free addresses of semantic payload owners in one runtime net.
///
/// The set deliberately describes representation locations rather than
/// cloning payloads. A collector policy which needs a side resolves these
/// addresses while the runtime-net mutex still holds the corresponding
/// pre- or post-write state. Two node slots cover every rewrite: an
/// operator completion removes two payload nodes, while duplication installs
/// at most two payload nodes. A whole copy installs a run of fresh nodes,
/// the first and how many.
#[derive(Clone, Copy, Default)]
pub(crate) struct RuntimeNetEdgeSet {
    nodes: [Option<NodeId>; 2],
    fresh: Option<(NodeId, usize)>,
    copy: Option<CopyId>,
    active: Option<ActivePairKey>,
    obligation: Option<NodeId>,
}

impl RuntimeNetEdgeSet {
    fn node(node: NodeId) -> Self {
        Self {
            nodes: [Some(node), None],
            ..Self::default()
        }
    }

    fn nodes(left: NodeId, right: NodeId) -> Self {
        Self {
            nodes: [Some(left), Some(right)],
            ..Self::default()
        }
    }

    fn copy(copy: CopyId) -> Self {
        Self {
            copy: Some(copy),
            ..Self::default()
        }
    }

    fn fresh(first: NodeId, count: usize) -> Self {
        Self {
            fresh: Some((first, count)),
            ..Self::default()
        }
    }

    fn active(pair: ActivePairKey) -> Self {
        Self {
            active: Some(pair),
            ..Self::default()
        }
    }

    fn with_active(mut self, pair: ActivePairKey) -> Self {
        self.active = Some(pair);
        self
    }

    fn with_copy(mut self, copy: CopyId) -> Self {
        self.copy = Some(copy);
        self
    }

    fn with_obligation(mut self, cursor: NodeId) -> Self {
        self.obligation = Some(cursor);
        self
    }

    fn visit<S: NetSpecialization>(
        self,
        runtime: &RuntimeNet<S>,
        visit: &mut impl FnMut(RuntimeNetPayload<'_, S>),
    ) {
        let fresh = self.fresh.into_iter().flat_map(|(first, count)| {
            (0..count as u64).map(move |offset| NodeId::from_zero_based(first.get() + offset))
        });
        for node in self.nodes.into_iter().flatten().chain(fresh) {
            match runtime.node(node) {
                Some(RuntimeNode::Data(data)) => visit(RuntimeNetPayload::Data(data)),
                Some(RuntimeNode::Operator(operator)) => {
                    visit(RuntimeNetPayload::Operator(operator));
                }
                Some(RuntimeNode::CallableCheckpoint(checkpoint)) => {
                    if let Some(payload) = &checkpoint.payload {
                        visit(RuntimeNetPayload::CallableCheckpoint(payload));
                    }
                }
                Some(
                    RuntimeNode::Bind
                    | RuntimeNode::Fan { .. }
                    | RuntimeNode::Erase
                    | RuntimeNode::Interface
                    | RuntimeNode::RemoteCursor { .. },
                )
                | None => {}
            }
        }

        if let Some(copy) = self.copy
            && let Some(state) = runtime.copies.get(&copy)
        {
            visit(RuntimeNetPayload::Source(&state.source));
        }
        if let Some(pair) = self.active
            && let Some(state) = runtime.active.get(&pair)
        {
            visit_active_pair_payload(state, visit);
        }
        if let Some(cursor) = self.obligation
            && let Some(obligation) = runtime.cursor_obligations.get(&cursor)
            && let PairlessCursorState::Blocked(dependency) = &obligation.state
            && let Some(source) = dependency.source_runtime()
        {
            visit(RuntimeNetPayload::Source(source));
        }
    }
}

fn visit_active_pair_payload<'payload, S: NetSpecialization>(
    state: &'payload ActivePairState<S>,
    visit: &mut impl FnMut(RuntimeNetPayload<'payload, S>),
) {
    match state {
        ActivePairState::BlockedCursor {
            blockage: CursorBlockage::Dependency(dependency),
            ..
        } => {
            if let Some(source) = dependency.source_runtime() {
                visit(RuntimeNetPayload::Source(source));
            }
        }
        ActivePairState::Stuck(StuckReason::Specialization(reason)) => {
            visit(RuntimeNetPayload::StuckReason(reason));
        }
        ActivePairState::Ready
        | ActivePairState::Claimed
        | ActivePairState::BlockedCallableCheckpoint { .. }
        | ActivePairState::BlockedCursor {
            blockage: CursorBlockage::Stable,
            ..
        }
        | ActivePairState::Stuck(StuckReason::NoRule) => {}
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct RuntimeNetEdgeTransition {
    leaving: RuntimeNetEdgeSet,
    adding: RuntimeNetEdgeSet,
}

impl RuntimeNetEdgeTransition {
    fn new(leaving: RuntimeNetEdgeSet, adding: RuntimeNetEdgeSet) -> Self {
        Self { leaving, adding }
    }

    pub(crate) fn visit_leaving<S: NetSpecialization>(
        self,
        runtime: &RuntimeNet<S>,
        visit: &mut impl FnMut(RuntimeNetPayload<'_, S>),
    ) {
        self.leaving.visit(runtime, visit);
    }

    pub(crate) fn visit_adding<S: NetSpecialization>(
        self,
        runtime: &RuntimeNet<S>,
        visit: &mut impl FnMut(RuntimeNetPayload<'_, S>),
    ) {
        self.adding.visit(runtime, visit);
    }
}

/// Applies generic-runtime mutations unchanged, for the non-core test
/// specialization.
#[cfg(test)]
#[derive(Clone, Copy)]
struct DirectRuntimeNetMutationGateway;

#[cfg(test)]
impl<S> RuntimeNetPayloadDuplicator<S> for DirectRuntimeNetMutationGateway
where
    S: NetSpecialization,
    S::Data: Clone,
    S::Operator: Clone,
{
    #[inline(always)]
    fn duplicate_data(&self, data: &S::Data) -> S::Data {
        data.clone()
    }

    #[inline(always)]
    fn duplicate_operator(&self, operator: &S::Operator) -> S::Operator {
        operator.clone()
    }
}

#[cfg(test)]
impl<S> RuntimeNetMutationGateway<S> for DirectRuntimeNetMutationGateway
where
    S: NetSpecialization,
    S::Data: Clone,
    S::Operator: Clone,
    S::RuntimeSource: Clone + PartialEq,
{
    #[inline(always)]
    fn duplicate_runtime_source(&self, source: &S::RuntimeSource) -> S::RuntimeSource {
        source.clone()
    }

    #[inline(always)]
    fn same_runtime_source(&self, left: &S::RuntimeSource, right: &S::RuntimeSource) -> bool {
        left == right
    }

    #[inline(always)]
    fn transition_edges<Result>(
        &self,
        runtime: &mut RuntimeNet<S>,
        _edges: RuntimeNetEdgeTransition,
        update: impl FnOnce(&mut RuntimeNet<S>) -> Result,
    ) -> Result {
        let result = update(runtime);
        #[cfg(test)]
        runtime.check_invariants_after_transition();
        result
    }
}

#[cfg(test)]
const DIRECT_RUNTIME_NET_MUTATION_GATEWAY: DirectRuntimeNetMutationGateway =
    DirectRuntimeNetMutationGateway;

#[cfg(test)]
impl<S: NetSpecialization> fmt::Debug for NormalizationBatchLease<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NormalizationBatchLease")
            .field("id", &self.id)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
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
        inner.close_normalization_batch(self.id);
    }
}

#[cfg(test)]
impl<S: NetSpecialization> Drop for NormalizationBatchLease<S> {
    fn drop(&mut self) {
        self.close_inner();
    }
}

impl<S: NetSpecialization> NormalizationBatchGuard<'_, S> {
    pub(crate) fn close(mut self) {
        self.close_inner();
    }

    fn close_inner(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.cell.close_normalization_batch(self.id);
    }
}

impl<S: NetSpecialization> Drop for NormalizationBatchGuard<'_, S> {
    fn drop(&mut self) {
        self.close_inner();
    }
}

impl RuntimeNetDisturbance {
    fn new() -> Self {
        Self {
            inner: Arc::new(RuntimeNetDisturbanceInner {
                changed: CountedCondvar::new(),
                wait: Mutex::new(()),
                epoch: AtomicU64::new(0),
                closed: AtomicBool::new(false),
            }),
        }
    }

    fn epoch(&self) -> u64 {
        self.inner.epoch.load(Ordering::Relaxed)
    }

    fn publish(&self) {
        let _wait = self
            .inner
            .wait
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.inner.epoch.fetch_add(1, Ordering::Relaxed);
        self.inner.changed.notify_all();
    }

    fn close(&self) {
        let _wait = self
            .inner
            .wait
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.inner.closed.store(true, Ordering::Relaxed);
        self.inner.epoch.fetch_add(1, Ordering::Relaxed);
        self.inner.changed.notify_all();
    }

    /// Waits until the observed epoch changes. `false` reports that the
    /// semantic cell closed instead of publishing further progress.
    fn wait_for_change(&self, observed_epoch: u64) -> bool {
        self.wait_for_change_after(observed_epoch, || {})
    }

    fn wait_for_change_after(&self, observed_epoch: u64, before_block: impl FnOnce()) -> bool {
        let mut wait = self
            .inner
            .wait
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut before_block = Some(before_block);
        while self.epoch() == observed_epoch && !self.inner.closed.load(Ordering::Relaxed) {
            if let Some(before_block) = before_block.take() {
                before_block();
            }
            wait = self
                .inner
                .changed
                .wait(wait)
                .unwrap_or_else(PoisonError::into_inner);
        }
        !self.inner.closed.load(Ordering::Relaxed)
    }
}

impl<S: NetSpecialization> RuntimeNetCell<S> {
    pub(crate) fn new(runtime: RuntimeNet<S>) -> Self {
        Self {
            runtime: Mutex::new(SharedRuntimeNetState {
                runtime,
                batches: NormalizationBatchState::default(),
            }),
            topology_revision: AtomicU64::new(0),
            disturbance: RuntimeNetDisturbance::new(),
        }
    }

    fn revisions(&self) -> RuntimeNetRevisions {
        RuntimeNetRevisions {
            topology_revision: self.topology_revision.load(Ordering::Relaxed),
            disturbance_epoch: self.disturbance.epoch(),
        }
    }

    fn publish_mutation(&self, batches: &mut NormalizationBatchState) {
        self.topology_revision.fetch_add(1, Ordering::Relaxed);
        if let Some(active) = batches.active.as_mut() {
            active.dirty = true;
        } else {
            self.disturbance.publish();
        }
    }

    fn contention(&self, revisions: RuntimeNetRevisions) -> NetContention {
        NetContention {
            disturbance: self.disturbance(),
            revisions,
        }
    }

    fn claim_normalization_batch(&self) -> Result<u64, NetContention> {
        let mut state = self
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        let revisions = self.revisions();
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
        Ok(id)
    }

    /// Closes one batch from its guard, including during an unwind.
    ///
    /// Batch bookkeeping is independent of the topology a panic may have
    /// torn, so a poisoned net still closes its batch and wakes contenders.
    /// Reading through poison leaves the poison itself set.
    fn close_normalization_batch(&self, id: u64) {
        let mut state = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        let publish = if state
            .batches
            .active
            .as_ref()
            .is_some_and(|active| active.id == id)
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
            self.disturbance.publish();
        }
    }

    pub(crate) fn try_begin_normalization_batch(
        &self,
    ) -> Result<NormalizationBatchGuard<'_, S>, NetContention> {
        let id = self.claim_normalization_batch()?;
        Ok(NormalizationBatchGuard {
            cell: self,
            id,
            closed: false,
            _thread_bound: PhantomData,
        })
    }

    pub(crate) fn with<R>(&self, inspect: impl FnOnce(&RuntimeNet<S>) -> R) -> R {
        let state = self
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        inspect(&state.runtime)
    }

    pub(crate) fn with_revisions<R>(
        &self,
        inspect: impl FnOnce(&RuntimeNet<S>) -> R,
    ) -> (R, RuntimeNetRevisions) {
        let state = self
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        let revisions = self.revisions();
        (inspect(&state.runtime), revisions)
    }

    /// Visits one stable logical payload snapshot during exclusive tracing.
    ///
    /// A managed cell's mutation gateways require a mutator, so collection's
    /// exclusive heap phase guarantees this lock is immediately available.
    /// Failing rather than blocking keeps an integration violation
    /// recoverable by the collector's trace-panic protocol. A poisoned net
    /// still holds its edges, and tracing may not omit them, so poison is
    /// read through.
    pub(crate) fn try_visit_logical_payloads(
        &self,
        visit: &mut impl FnMut(RuntimeNetPayload<'_, S>),
    ) {
        let state = match self.runtime.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                panic!("managed runtime net must be quiescent during tracing")
            }
        };
        state.runtime.visit_logical_payloads(visit);
    }

    #[cfg(test)]
    pub(crate) fn with_mut<R>(&self, update: impl FnOnce(&mut RuntimeNet<S>) -> R) -> R
    where
        S::Data: Clone,
        S::Operator: Clone,
        S::RuntimeSource: Clone + PartialEq,
    {
        self.with_mut_via(&DIRECT_RUNTIME_NET_MUTATION_GATEWAY, update)
    }

    #[cfg(test)]
    pub(crate) fn with_mut_via<Gateway, R>(
        &self,
        gateway: &Gateway,
        update: impl FnOnce(&mut RuntimeNet<S>) -> R,
    ) -> R
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        let mut state = self
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        let result = gateway.transition(&mut state.runtime, update);
        self.publish_mutation(&mut state.batches);
        result
    }

    pub(crate) fn with_edge_mut_via<Gateway, R>(
        &self,
        gateway: &Gateway,
        edges: impl FnOnce(&RuntimeNet<S>) -> RuntimeNetEdgeTransition,
        update: impl FnOnce(&mut RuntimeNet<S>) -> R,
    ) -> R
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        let mut state = self
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        let edges = edges(&state.runtime);
        let result = gateway.transition_edges(&mut state.runtime, edges, update);
        self.publish_mutation(&mut state.batches);
        result
    }

    pub(crate) fn with_input_edge_mut_via<Gateway, Input, R>(
        &self,
        gateway: &Gateway,
        input: Input,
        edges: impl FnOnce(&RuntimeNet<S>, &Input) -> RuntimeNetEdgeTransition,
        update: impl FnOnce(&mut RuntimeNet<S>, Input) -> R,
    ) -> R
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        let mut state = self
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        let edges = edges(&state.runtime, &input);
        let result =
            gateway.transition_edges(&mut state.runtime, edges, |runtime| update(runtime, input));
        self.publish_mutation(&mut state.batches);
        result
    }

    #[cfg(test)]
    pub(crate) fn with_conditional_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<S>) -> RuntimeNetMutation<R>,
    ) -> R
    where
        S::Data: Clone,
        S::Operator: Clone,
        S::RuntimeSource: Clone + PartialEq,
    {
        self.with_conditional_mut_via(&DIRECT_RUNTIME_NET_MUTATION_GATEWAY, update)
    }

    pub(crate) fn with_conditional_mut_via<Gateway, R>(
        &self,
        gateway: &Gateway,
        update: impl FnOnce(&mut RuntimeNet<S>) -> RuntimeNetMutation<R>,
    ) -> R
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        let mut state = self
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        self.apply_conditional_mut_via(&mut state, gateway, update)
    }

    /// Applies a claim release or restore unless a panic poisoned the net.
    ///
    /// A poisoned net may be torn mid-rewrite, so its evaluation is already
    /// interrupted and restoring a claim on it is moot. Running the
    /// transition anyway could panic again, which aborts the process when
    /// the cleanup runs in a destructor during an unwind. Returns `None` for
    /// a poisoned net.
    pub(crate) fn with_cleanup_mut_via<Gateway, R>(
        &self,
        gateway: &Gateway,
        update: impl FnOnce(&mut RuntimeNet<S>) -> RuntimeNetMutation<R>,
    ) -> Option<R>
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        let mut state = self.runtime.lock().ok()?;
        Some(self.apply_conditional_mut_via(&mut state, gateway, update))
    }

    fn apply_conditional_mut_via<Gateway, R>(
        &self,
        state: &mut SharedRuntimeNetState<S>,
        gateway: &Gateway,
        update: impl FnOnce(&mut RuntimeNet<S>) -> RuntimeNetMutation<R>,
    ) -> R
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        match gateway.transition(&mut state.runtime, update) {
            RuntimeNetMutation::Unchanged(result) => result,
            RuntimeNetMutation::Changed(result) => {
                self.publish_mutation(&mut state.batches);
                result
            }
        }
    }

    pub(crate) fn with_conditional_edge_mut_via<Gateway, R>(
        &self,
        gateway: &Gateway,
        edges: impl FnOnce(&RuntimeNet<S>) -> RuntimeNetEdgeTransition,
        update: impl FnOnce(&mut RuntimeNet<S>) -> RuntimeNetMutation<R>,
    ) -> R
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        let mut state = self
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        self.apply_conditional_edge_mut_via(&mut state, gateway, edges, update)
    }

    /// Edge-transition form of [`Self::with_cleanup_mut_via`].
    pub(crate) fn with_cleanup_edge_mut_via<Gateway, R>(
        &self,
        gateway: &Gateway,
        edges: impl FnOnce(&RuntimeNet<S>) -> RuntimeNetEdgeTransition,
        update: impl FnOnce(&mut RuntimeNet<S>) -> RuntimeNetMutation<R>,
    ) -> Option<R>
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        let mut state = self.runtime.lock().ok()?;
        Some(self.apply_conditional_edge_mut_via(&mut state, gateway, edges, update))
    }

    fn apply_conditional_edge_mut_via<Gateway, R>(
        &self,
        state: &mut SharedRuntimeNetState<S>,
        gateway: &Gateway,
        edges: impl FnOnce(&RuntimeNet<S>) -> RuntimeNetEdgeTransition,
        update: impl FnOnce(&mut RuntimeNet<S>) -> RuntimeNetMutation<R>,
    ) -> R
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        let edges = edges(&state.runtime);
        match gateway.transition_edges(&mut state.runtime, edges, update) {
            RuntimeNetMutation::Unchanged(result) => result,
            RuntimeNetMutation::Changed(result) => {
                self.publish_mutation(&mut state.batches);
                result
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn with_optional_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<S>) -> Option<R>,
    ) -> Option<R>
    where
        S::Data: Clone,
        S::Operator: Clone,
        S::RuntimeSource: Clone + PartialEq,
    {
        self.with_optional_mut_via(&DIRECT_RUNTIME_NET_MUTATION_GATEWAY, update)
    }

    #[cfg(test)]
    pub(crate) fn with_optional_mut_via<Gateway, R>(
        &self,
        gateway: &Gateway,
        update: impl FnOnce(&mut RuntimeNet<S>) -> Option<R>,
    ) -> Option<R>
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        self.with_conditional_mut_via(gateway, |runtime| match update(runtime) {
            Some(result) => RuntimeNetMutation::Changed(Some(result)),
            None => RuntimeNetMutation::Unchanged(None),
        })
    }

    #[cfg(test)]
    pub(crate) fn poll_interface_demand(&self, interface: Port) -> InterfaceDemand
    where
        S::Data: Clone,
        S::Operator: Clone,
        S::RuntimeSource: Clone + PartialEq,
    {
        self.with_conditional_mut(|runtime| {
            runtime.poll_interface_demand(interface, &mut InterfaceRoute::default())
        })
    }

    #[cfg(test)]
    pub(crate) fn resolve_cursor_dependency(
        &self,
        cursor: NodeId,
        expected: &CursorDependency<S>,
        disposition: CursorDependencyDisposition,
    ) -> CursorDependencyResolution
    where
        S::Data: Clone,
        S::Operator: Clone,
        S::RuntimeSource: Clone + PartialEq,
    {
        self.with_conditional_edge_mut_via(
            &DIRECT_RUNTIME_NET_MUTATION_GATEWAY,
            |runtime| {
                runtime.resolve_cursor_dependency_edge_transition_with_gateway(
                    cursor,
                    expected,
                    &DIRECT_RUNTIME_NET_MUTATION_GATEWAY,
                )
            },
            |runtime| {
                let resolution = runtime.resolve_cursor_dependency_with_gateway(
                    cursor,
                    expected,
                    disposition,
                    &DIRECT_RUNTIME_NET_MUTATION_GATEWAY,
                );
                if resolution == CursorDependencyResolution::Resolved {
                    RuntimeNetMutation::Changed(resolution)
                } else {
                    RuntimeNetMutation::Unchanged(resolution)
                }
            },
        )
    }

    fn disturbance(&self) -> RuntimeNetDisturbance {
        self.disturbance.clone()
    }

    #[cfg(test)]
    pub(crate) fn active_normalization_batch(&self) -> Option<(u64, bool)> {
        let state = self
            .runtime
            .lock()
            .expect("shared runtime net was poisoned");
        state
            .batches
            .active
            .as_ref()
            .map(|active| (active.id, active.contended))
    }

    #[cfg(test)]
    pub(crate) fn step_active_pair_with(
        &self,
        pair: ActivePairKey,
        expected_topology_revision: Option<u64>,
        inspect_source: impl FnOnce(&S::RuntimeSource, Port) -> SourceFrontier<S>,
    ) -> ActivePairStep<S>
    where
        S::Data: Clone,
        S::Operator: Clone,
        S::RuntimeSource: Clone + PartialEq,
    {
        self.step_active_pair_with_gateway(
            pair,
            expected_topology_revision,
            &DIRECT_RUNTIME_NET_MUTATION_GATEWAY,
            |_| true,
            inspect_source,
        )
    }

    /// Takes one non-blocking step at `pair`. `admit` runs only when the pair
    /// is ready to claim, told what kind of claim it is; returning `false`
    /// leaves the pair unclaimed and reports `NotAdmitted`. Every other
    /// outcome only observes.
    pub(crate) fn step_active_pair_with_gateway<Gateway>(
        &self,
        pair: ActivePairKey,
        expected_topology_revision: Option<u64>,
        gateway: &Gateway,
        admit: impl FnOnce(ClaimKind) -> bool,
        inspect_source: impl FnOnce(&S::RuntimeSource, Port) -> SourceFrontier<S>,
    ) -> ActivePairStep<S>
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        let (mut outcome, cursor_claim) = {
            let mut state = self
                .runtime
                .lock()
                .expect("shared runtime net was poisoned");
            let revisions = self.revisions();
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
            let mut cursor_claim = None;
            let (outcome, changed) = match state.runtime.active.get(&pair) {
                Some(ActivePairState::Ready)
                    if !admit(if state.runtime.callable_checkpoint(pair).is_some() {
                        ClaimKind::CheckpointResumption
                    } else {
                        ClaimKind::Reduction
                    }) =>
                {
                    (ActivePairStep::NotAdmitted, false)
                }
                Some(ActivePairState::Ready) => {
                    let edges = state.runtime.reduce_pair_edge_transition(pair);
                    let reduction = gateway
                        .transition_edges(&mut state.runtime, edges, |runtime| {
                            runtime.reduce_pair_with_gateway(pair, gateway)
                        })
                        .expect("ready pair must produce one reduction");
                    if let ReductionKind::RemoteCursor {
                        cursor,
                        progress: CursorProgress::Claimed,
                    } = &reduction.kind
                    {
                        cursor_claim = Some(
                            state
                                .runtime
                                .cursor_claim(*cursor, gateway)
                                .expect("cursor reduction must retain its claimed transition"),
                        );
                    }
                    (ActivePairStep::Reduction(reduction), true)
                }
                Some(ActivePairState::Claimed) => {
                    (ActivePairStep::Contended(self.contention(revisions)), false)
                }
                Some(ActivePairState::BlockedCursor { cursor, .. }) => {
                    (ActivePairStep::Cursor(*cursor), false)
                }
                Some(ActivePairState::BlockedCallableCheckpoint { generation, wait }) => {
                    let call = state
                        .runtime
                        .callable_checkpoint(pair)
                        .expect("blocked checkpoint state must retain its structural pair");
                    debug_assert_eq!(call.generation, *generation);
                    (
                        ActivePairStep::BlockedCallableCheckpoint(BlockedCallableCheckpoint {
                            call,
                            wait: wait.clone(),
                        }),
                        false,
                    )
                }
                Some(ActivePairState::Stuck(reason)) => (
                    ActivePairStep::Stuck(StuckPair {
                        pair,
                        reason: reason.clone(),
                    }),
                    false,
                ),
                None => (ActivePairStep::Gone, false),
            };
            if changed {
                self.publish_mutation(&mut state.batches);
            }
            (
                outcome,
                cursor_claim.map(|claim| CursorClaimGuard::new(self, claim, gateway)),
            )
        };

        if let Some(claim) = cursor_claim {
            let progress = claim.advance_with(inspect_source);
            let ActivePairStep::Reduction(Reduction {
                kind:
                    ReductionKind::RemoteCursor {
                        progress: published,
                        ..
                    },
                ..
            }) = &mut outcome
            else {
                unreachable!("a cursor claim must accompany its cursor reduction")
            };
            *published = progress;
        }
        outcome
    }

    #[cfg(test)]
    pub(crate) fn step_cursor_with(
        &self,
        cursor: NodeId,
        expected_topology_revision: Option<u64>,
        inspect_source: impl FnOnce(&S::RuntimeSource, Port) -> SourceFrontier<S>,
    ) -> CursorStep<S>
    where
        S::Data: Clone,
        S::Operator: Clone,
        S::RuntimeSource: Clone + PartialEq,
    {
        self.step_cursor_with_gateway(
            cursor,
            expected_topology_revision,
            &DIRECT_RUNTIME_NET_MUTATION_GATEWAY,
            |_| true,
            inspect_source,
        )
    }

    /// Takes one non-blocking step at `cursor`. `admit` runs only when the
    /// cursor is ready to claim, always as a `Reduction`; returning `false`
    /// leaves it unclaimed and reports `NotAdmitted`. Every other outcome only
    /// observes.
    pub(crate) fn step_cursor_with_gateway<Gateway>(
        &self,
        cursor: NodeId,
        expected_topology_revision: Option<u64>,
        gateway: &Gateway,
        admit: impl FnOnce(ClaimKind) -> bool,
        inspect_source: impl FnOnce(&S::RuntimeSource, Port) -> SourceFrontier<S>,
    ) -> CursorStep<S>
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        let claim = {
            let mut state = self
                .runtime
                .lock()
                .expect("shared runtime net was poisoned");
            let revisions = self.revisions();
            if expected_topology_revision
                .is_some_and(|expected| expected != revisions.topology_revision())
            {
                return CursorStep::Disturbed;
            }
            match state.runtime.inspect_cursor_step(cursor, gateway) {
                CursorStepInspection::Claimable(_) if !admit(ClaimKind::Reduction) => {
                    return CursorStep::NotAdmitted;
                }
                CursorStepInspection::Claimable(expected_pair) => {
                    let progress = gateway
                        .transition(&mut state.runtime, |runtime| {
                            runtime.begin_cursor_claim(cursor, expected_pair)
                        })
                        .expect("claimable cursor must accept its owning transition");
                    assert_eq!(progress, CursorProgress::Claimed);
                    let claim = state
                        .runtime
                        .cursor_claim(cursor, gateway)
                        .expect("claimed cursor step must retain its transition");
                    self.publish_mutation(&mut state.batches);
                    CursorClaimGuard::new(self, claim, gateway)
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
        let progress = claim.advance_with(inspect_source);
        if progress != CursorProgress::Blocked {
            return CursorStep::Progressed(progress);
        }
        let (inspection, revisions) =
            self.with_revisions(|runtime| runtime.inspect_cursor_step(cursor, gateway));
        match inspection {
            CursorStepInspection::Claimable(_) => CursorStep::Progressed(progress),
            CursorStepInspection::Dependency(dependency) => CursorStep::Dependency(dependency),
            CursorStepInspection::Stable => CursorStep::Stable,
            CursorStepInspection::Claimed => CursorStep::Contended(self.contention(revisions)),
            CursorStepInspection::Gone => CursorStep::Gone,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_advance_claimed_cursor_with_gateway<Gateway>(
        &self,
        cursor: NodeId,
        gateway: &Gateway,
        inspect_source: impl FnOnce(&S::RuntimeSource, Port) -> SourceFrontier<S>,
    ) -> Option<CursorProgress>
    where
        Gateway: RuntimeNetMutationGateway<S>,
    {
        let claim = self.with(|runtime| runtime.cursor_claim(cursor, gateway))?;
        Some(CursorClaimGuard::new(self, claim, gateway).advance_with(inspect_source))
    }
}

impl<S: NetSpecialization> Drop for RuntimeNetCell<S> {
    fn drop(&mut self) {
        self.disturbance.close();
    }
}

/// Result of an update which may discover that no authoritative state needs
/// to change. Only `Changed` publishes a topology revision and disturbance.
pub(crate) enum RuntimeNetMutation<R> {
    Unchanged(R),
    Changed(R),
}

#[cfg(test)]
impl<S: NetSpecialization> SharedRuntimeNet<S> {
    pub fn new(runtime: RuntimeNet<S>) -> Self {
        Self {
            inner: Arc::new(RuntimeNetCell::new(runtime)),
        }
    }
}

#[cfg(test)]
impl<S: NetSpecialization> RuntimeNet<S> {
    pub(crate) fn test_stable_auxiliary() -> (Self, Port) {
        let mut runtime = RuntimeNet::empty();
        let bind = runtime.add_node(RuntimeNode::Bind);
        let interface = runtime.add_interface(Port::auxiliary(bind, 1));
        runtime.exposed = Some(interface);
        (runtime, interface)
    }

    pub(crate) fn test_copy_layer_from(source: PreparedCopySource<S>) -> (Self, Port) {
        let mut target = RuntimeNet::empty();
        let cursor = target.begin_copy(source);
        let interface = target.add_interface(Port::principal(cursor));
        target.exposed = Some(interface);
        (target, interface)
    }

    pub(crate) fn test_pair_owned_copy_layer_from(
        source: PreparedCopySource<S>,
    ) -> (Self, Port, NodeId) {
        let mut target = RuntimeNet::empty();
        let bind = target.add_node(RuntimeNode::Bind);
        let cursor = target.begin_copy(source);
        target.connect(Port::principal(bind), Port::principal(cursor));
        let interface = target.add_interface(Port::auxiliary(bind, 1));
        target.exposed = Some(interface);
        (target, interface, cursor)
    }

    pub(crate) fn test_productive_pair_owned_copy_layer_from(
        source: PreparedCopySource<S>,
    ) -> (Self, Port) {
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
        (target, interface)
    }

    pub(crate) fn test_stable_root_with_claimed_cursor_from(
        source: PreparedCopySource<S>,
    ) -> (Self, Port, NodeId) {
        let mut target = RuntimeNet::empty();
        let root = target.add_node(RuntimeNode::Erase);
        let interface = target.add_interface(Port::principal(root));
        let cursor = target.begin_copy(source);
        assert!(target.ensure_pairless_cursor_obligation(cursor));
        assert!(target.claim_pairless_cursor_obligation(cursor));
        target.exposed = Some(interface);
        (target, interface, cursor)
    }
}

#[cfg(test)]
impl<S> SharedRuntimeNet<S>
where
    S: NetSpecialization<RuntimeSource = SharedRuntimeNet<S>>,
    S::Data: Clone,
    S::Operator: Clone,
{
    #[cfg(test)]
    fn test_cursor_claim_guard(
        &self,
        cursor: NodeId,
    ) -> Option<CursorClaimGuard<'_, S, DirectRuntimeNetMutationGateway>> {
        let claim = self
            .with(|runtime| runtime.cursor_claim(cursor, &DIRECT_RUNTIME_NET_MUTATION_GATEWAY))?;
        Some(CursorClaimGuard::new(
            self.cell(),
            claim,
            &DIRECT_RUNTIME_NET_MUTATION_GATEWAY,
        ))
    }

    #[cfg(test)]
    pub(crate) fn test_advance_claimed_cursor(&self, cursor: NodeId) -> Option<CursorProgress> {
        self.test_cursor_claim_guard(cursor).map(|claim| {
            claim.advance_with(|source, anchor| source.inspect_source_frontier(anchor))
        })
    }

    pub fn with<R>(&self, inspect: impl FnOnce(&RuntimeNet<S>) -> R) -> R {
        self.inner.with(inspect)
    }

    pub fn with_revisions<R>(
        &self,
        inspect: impl FnOnce(&RuntimeNet<S>) -> R,
    ) -> (R, RuntimeNetRevisions) {
        self.inner.with_revisions(inspect)
    }

    pub fn with_mut<R>(&self, update: impl FnOnce(&mut RuntimeNet<S>) -> R) -> R {
        self.inner.with_mut(update)
    }

    pub(crate) fn with_conditional_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<S>) -> RuntimeNetMutation<R>,
    ) -> R {
        self.inner.with_conditional_mut(update)
    }

    #[cfg(test)]
    pub(crate) fn with_optional_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<S>) -> Option<R>,
    ) -> Option<R> {
        self.inner.with_optional_mut(update)
    }

    pub fn poll_interface_demand(&self, interface: Port) -> InterfaceDemand {
        self.inner.poll_interface_demand(interface)
    }

    pub fn resolve_cursor_dependency(
        &self,
        cursor: NodeId,
        expected: &CursorDependency<S>,
        disposition: CursorDependencyDisposition,
    ) -> CursorDependencyResolution {
        self.inner
            .resolve_cursor_dependency(cursor, expected, disposition)
    }

    #[cfg(test)]
    fn revisions(&self) -> (u64, u64) {
        let revisions = self.inner.with_revisions(|_| ()).1;
        (revisions.topology_revision(), revisions.disturbance_epoch())
    }

    pub fn wait_for_disturbance(&self, observed_epoch: u64) {
        let disturbance = self.inner.disturbance();
        let _ = disturbance.wait_for_change(observed_epoch);
    }

    pub fn try_begin_normalization_batch(
        &self,
    ) -> Result<NormalizationBatchLease<S>, NetContention> {
        let id = self.inner.claim_normalization_batch()?;
        Ok(NormalizationBatchLease {
            inner: Arc::downgrade(&self.inner),
            id,
            closed: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn active_normalization_batch(&self) -> Option<(u64, bool)> {
        self.inner.active_normalization_batch()
    }

    pub fn step_active_pair(&self, pair: ActivePairKey) -> ActivePairStep<S> {
        self.step_active_pair_if_current(pair, None)
    }

    fn step_active_pair_if_current(
        &self,
        pair: ActivePairKey,
        expected_topology_revision: Option<u64>,
    ) -> ActivePairStep<S> {
        self.inner
            .step_active_pair_with(pair, expected_topology_revision, |source, anchor| {
                source.inspect_source_frontier(anchor)
            })
    }

    pub fn step_cursor(&self, cursor: NodeId) -> CursorStep<S> {
        self.step_cursor_if_current(cursor, None)
    }

    fn step_cursor_if_current(
        &self,
        cursor: NodeId,
        expected_topology_revision: Option<u64>,
    ) -> CursorStep<S> {
        self.inner
            .step_cursor_with(cursor, expected_topology_revision, |source, anchor| {
                source.inspect_source_frontier(anchor)
            })
    }
}

#[cfg(test)]
impl<S: NetSpecialization> SharedRuntimeNet<S> {
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub(crate) fn cell(&self) -> &RuntimeNetCell<S> {
        &self.inner
    }
}

#[cfg(test)]
impl<S: NetSpecialization> Clone for SharedRuntimeNet<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

#[cfg(test)]
impl<S: NetSpecialization> fmt::Debug for SharedRuntimeNet<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SharedRuntimeNet")
            .field(&Arc::as_ptr(&self.inner))
            .finish()
    }
}

#[cfg(test)]
impl<S: NetSpecialization> PartialEq for SharedRuntimeNet<S> {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other)
    }
}

#[cfg(test)]
impl<S: NetSpecialization> Eq for SharedRuntimeNet<S> {}

struct CopyState<S: NetSpecialization> {
    source: S::RuntimeSource,
    frontiers: TrustedHashMap<Port, NodeId>,
    fan_sites: TrustedHashMap<FanSite, FanSite>,
}

struct CursorClaim<S: NetSpecialization> {
    cursor: NodeId,
    owner: CursorClaimOwner,
    copy: CopyId,
    remote: Port,
    source: S::RuntimeSource,
}

enum CursorDisposition<S: NetSpecialization> {
    Advance(SourceFrontier<S>),
    #[cfg(test)]
    Release,
}

/// One stack-bound cursor transition. It carries no target or source mutex
/// guard, so source-frontier inspection and target publication remain
/// disjoint. Dropping an unfinished guard restores ready owner state.
#[must_use = "a cursor claim must be advanced or released"]
struct CursorClaimGuard<'claim, S: NetSpecialization, Gateway: RuntimeNetMutationGateway<S>> {
    target: &'claim RuntimeNetCell<S>,
    claim: Option<CursorClaim<S>>,
    gateway: &'claim Gateway,
    _thread_bound: PhantomData<Rc<()>>,
}

impl<'claim, S, Gateway> CursorClaimGuard<'claim, S, Gateway>
where
    S: NetSpecialization,
    Gateway: RuntimeNetMutationGateway<S>,
{
    fn new(
        target: &'claim RuntimeNetCell<S>,
        claim: CursorClaim<S>,
        gateway: &'claim Gateway,
    ) -> Self {
        Self {
            target,
            claim: Some(claim),
            gateway,
            _thread_bound: PhantomData,
        }
    }

    fn advance_with(
        self,
        inspect_source: impl FnOnce(&S::RuntimeSource, Port) -> SourceFrontier<S>,
    ) -> CursorProgress {
        let claim = self
            .claim
            .as_ref()
            .expect("an unfinished cursor guard must retain its claim");
        let frontier = inspect_source(&claim.source, claim.remote);
        self.finish(CursorDisposition::Advance(frontier))
            .expect("advancing a cursor claim must produce progress")
    }

    fn finish(mut self, disposition: CursorDisposition<S>) -> Option<CursorProgress> {
        match disposition {
            CursorDisposition::Advance(frontier) => {
                let claim = self
                    .claim
                    .take()
                    .expect("an unfinished cursor guard must retain its claim");
                Some(self.target.with_input_edge_mut_via(
                    self.gateway,
                    (claim, frontier),
                    |target, (claim, frontier)| {
                        target.finish_cursor_claim_edge_transition(claim, frontier)
                    },
                    |target, (claim, frontier)| target.finish_cursor_claim(claim, frontier),
                ))
            }
            #[cfg(test)]
            CursorDisposition::Release => {
                let restored = self.restore_fallback();
                self.claim = None;
                debug_assert_ne!(
                    restored,
                    Some(false),
                    "released cursor claim must remain current"
                );
                None
            }
        }
    }

    /// Restores the claim, or returns `None` when a panic poisoned the net.
    fn restore_fallback(&self) -> Option<bool> {
        let Some(claim) = self.claim.as_ref() else {
            return Some(true);
        };
        self.target.with_cleanup_mut_via(self.gateway, |target| {
            if target.release_cursor_claim(claim) {
                RuntimeNetMutation::Changed(true)
            } else {
                RuntimeNetMutation::Unchanged(false)
            }
        })
    }
}

impl<S, Gateway> Drop for CursorClaimGuard<'_, S, Gateway>
where
    S: NetSpecialization,
    Gateway: RuntimeNetMutationGateway<S>,
{
    fn drop(&mut self) {
        if self.claim.is_some() {
            let _ = self.restore_fallback();
        }
    }
}

pub(crate) struct SourceFrontier<S: NetSpecialization> {
    anchor: Port,
    shape: SourceFrontierShape<S>,
    observation: Option<FrontierObservation<S>>,
}

enum SourceFrontierShape<S: NetSpecialization> {
    Principal {
        port: Port,
        node: SourcePrincipalNode<S>,
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

/// A source principal classified without copying a linear evaluator payload.
enum SourcePrincipalNode<S: NetSpecialization> {
    Copyable(RuntimeNode<S>),
    CallableCheckpoint,
}

struct RuntimeEntry<S: NetSpecialization> {
    node: RuntimeNode<S>,
    /// Each wired port's typed reference to its peer.
    links: [Option<Link>; 3],
}

/// One semantic payload held directly by an instantiated runtime net.
///
/// This is a read-only representation walk, not an evaluator operation. In
/// particular, visiting a remote source reports its existing shared identity;
/// it never follows or materializes the cursor.
pub(crate) enum RuntimeNetPayload<'payload, S: NetSpecialization> {
    Data(&'payload S::Data),
    Operator(&'payload S::Operator),
    CallableCheckpoint(&'payload S::CallableCheckpoint),
    Source(&'payload S::RuntimeSource),
    StuckReason(&'payload S::StuckReason),
}

impl<S: NetSpecialization> RuntimeEntry<S> {
    fn new(node: RuntimeNode<S>) -> Self {
        Self {
            node,
            links: [None; 3],
        }
    }
}

/// How many nodes a fresh interface walk takes before it records its route:
/// a walk this short costs less to repeat than to record.
const UNRECORDED_ROUTE_PREFIX: usize = 16;

/// An evaluation's demand stack into one net, from the net's interface: the
/// end of the route its last demand walk took, each node waiting through its
/// principal port on the next, up to the near node of the active pair the
/// walk found. A rewrite at the tip pushes the nodes it leaves waiting; a
/// result reaching the tip pops it. The evaluation demanding the interface
/// owns it, and its next walk resumes it (see
/// [`RuntimeNet::walk_interface_route`]); an empty one walks from the
/// interface. It records nothing for a fresh walk's first
/// [`UNRECORDED_ROUTE_PREFIX`] nodes, so a walk that stays short never
/// allocates.
#[derive(Debug, Default)]
pub struct InterfaceRoute(Vec<NodeId>);

pub struct RuntimeNet<S: NetSpecialization> {
    next_node_id: u64,
    next_fan_site: u64,
    exposed: Option<Port>,
    nodes: TrustedHashMap<NodeId, RuntimeEntry<S>>,
    next_copy_id: u64,
    copies: TrustedHashMap<CopyId, CopyState<S>>,
    // Pairless cursor demand is owned here until the cursor participates in
    // an active pair, at which point `connect` transfers the state into the
    // pair's authoritative record.
    cursor_obligations: TrustedHashMap<NodeId, PairlessCursorObligation<S>>,

    // Every live principal-principal wire has exactly one authoritative state.
    // External work changes Ready to Claimed while the runtime lock is held,
    // then completes as a rewrite, a blocked call or cursor, or a permanent
    // stuck reason.
    pub(super) active: BTreeMap<ActivePairKey, ActivePairState<S>>,

    /// Whether debug checks of the polarity type apply. A test net built by
    /// hand, or from a deliberately unpolarized template, opts out.
    #[cfg(test)]
    polarity_checked: bool,
}

impl<S: NetSpecialization> RuntimeNet<S> {
    fn new(net: &InteractionNet<S>, duplicator: &impl RuntimeNetPayloadDuplicator<S>) -> Self {
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
                    Node::Data(data) => RuntimeNode::Data(duplicator.duplicate_data(data)),
                    Node::Operator(operator) => {
                        RuntimeNode::Operator(duplicator.duplicate_operator(operator))
                    }
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
            copies: TrustedHashMap::default(),
            cursor_obligations: TrustedHashMap::default(),
            active: BTreeMap::new(),
            #[cfg(test)]
            polarity_checked: net.polarized,
        };
        // Template wires are stored provider-first.
        for wire in net.wires.iter() {
            runtime.connect_provider(wire.left, wire.right);
        }
        let exposed = runtime.add_interface(net.exposed);
        runtime.exposed = Some(exposed);
        for index in 0..net.nodes.len() {
            runtime.debug_check_node_polarity(NodeId::from_index(index));
        }
        runtime
    }

    /// Enumerates direct semantic payloads without reducing, claiming,
    /// materializing, or otherwise mutating the net.
    ///
    /// The caller owns whatever synchronization makes this shared borrow
    /// stable. The callback is synchronous and must not re-enter that owner.
    /// Structural nodes, ports, fan identities, wait tokens, and no-rule stuck
    /// states contain no specialization payload and are intentionally omitted.
    pub(crate) fn visit_logical_payloads(&self, visit: &mut impl FnMut(RuntimeNetPayload<'_, S>)) {
        for entry in self.nodes.values() {
            match &entry.node {
                RuntimeNode::Data(data) => {
                    visit(RuntimeNetPayload::Data(data));
                }
                RuntimeNode::Operator(operator) => {
                    visit(RuntimeNetPayload::Operator(operator));
                }
                RuntimeNode::CallableCheckpoint(checkpoint) => {
                    if let Some(payload) = &checkpoint.payload {
                        visit(RuntimeNetPayload::CallableCheckpoint(payload));
                    }
                }
                RuntimeNode::Bind
                | RuntimeNode::Fan { .. }
                | RuntimeNode::Erase
                | RuntimeNode::Interface
                | RuntimeNode::RemoteCursor { .. } => {}
            }
        }

        for copy in self.copies.values() {
            visit(RuntimeNetPayload::Source(&copy.source));
        }

        for obligation in self.cursor_obligations.values() {
            if let PairlessCursorState::Blocked(dependency) = &obligation.state
                && let Some(source) = dependency.source_runtime()
            {
                visit(RuntimeNetPayload::Source(source));
            }
        }

        for state in self.active.values() {
            match state {
                ActivePairState::BlockedCursor {
                    blockage: CursorBlockage::Dependency(dependency),
                    ..
                } => {
                    if let Some(source) = dependency.source_runtime() {
                        visit(RuntimeNetPayload::Source(source));
                    }
                }
                ActivePairState::Stuck(StuckReason::Specialization(reason)) => {
                    visit(RuntimeNetPayload::StuckReason(reason));
                }
                ActivePairState::Ready
                | ActivePairState::Claimed
                | ActivePairState::BlockedCallableCheckpoint { .. }
                | ActivePairState::BlockedCursor {
                    blockage: CursorBlockage::Stable,
                    ..
                }
                | ActivePairState::Stuck(StuckReason::NoRule) => {}
            }
        }
    }

    fn next_node(&self, offset: u64) -> NodeId {
        NodeId::from_zero_based(
            self.next_node_id
                .checked_add(offset)
                .expect("interaction-net node ID space exhausted"),
        )
    }

    pub(crate) fn reduce_pair_edge_transition(
        &self,
        pair: ActivePairKey,
    ) -> RuntimeNetEdgeTransition {
        if !self
            .active
            .get(&pair)
            .is_some_and(ActivePairState::is_ready)
        {
            return RuntimeNetEdgeTransition::default();
        }
        let Some((left, right)) = self.pair_nodes(pair) else {
            return RuntimeNetEdgeTransition::default();
        };
        match (self.node(left), self.node(right)) {
            (Some(RuntimeNode::Fan { .. }), Some(RuntimeNode::Data(_))) => {
                RuntimeNetEdgeTransition::new(
                    RuntimeNetEdgeSet::node(right),
                    RuntimeNetEdgeSet::nodes(self.next_node(0), self.next_node(1)),
                )
            }
            (Some(RuntimeNode::Data(_)), Some(RuntimeNode::Fan { .. })) => {
                RuntimeNetEdgeTransition::new(
                    RuntimeNetEdgeSet::node(left),
                    RuntimeNetEdgeSet::nodes(self.next_node(0), self.next_node(1)),
                )
            }
            (Some(RuntimeNode::Fan { .. }), Some(RuntimeNode::Operator(_))) => {
                RuntimeNetEdgeTransition::new(
                    RuntimeNetEdgeSet::node(right),
                    RuntimeNetEdgeSet::nodes(self.next_node(0), self.next_node(1)),
                )
            }
            (Some(RuntimeNode::Operator(_)), Some(RuntimeNode::Fan { .. })) => {
                RuntimeNetEdgeTransition::new(
                    RuntimeNetEdgeSet::node(left),
                    RuntimeNetEdgeSet::nodes(self.next_node(0), self.next_node(1)),
                )
            }
            (Some(RuntimeNode::Erase), Some(RuntimeNode::Data(_) | RuntimeNode::Operator(_))) => {
                RuntimeNetEdgeTransition::new(
                    RuntimeNetEdgeSet::node(right),
                    RuntimeNetEdgeSet::default(),
                )
            }
            (Some(RuntimeNode::Data(_) | RuntimeNode::Operator(_)), Some(RuntimeNode::Erase)) => {
                RuntimeNetEdgeTransition::new(
                    RuntimeNetEdgeSet::node(left),
                    RuntimeNetEdgeSet::default(),
                )
            }
            _ => RuntimeNetEdgeTransition::default(),
        }
    }

    /// `whole` is how many nodes a whole copy installs, or `None` for a copy
    /// through a remote cursor (see [`PreparedCopySource::whole_len`]).
    pub(crate) fn resume_call_with_copy_edge_transition(
        &self,
        call: Call,
        whole: Option<usize>,
    ) -> RuntimeNetEdgeTransition {
        RuntimeNetEdgeTransition::new(RuntimeNetEdgeSet::node(call.data), self.copy_edges(whole))
    }

    fn copy_edges(&self, whole: Option<usize>) -> RuntimeNetEdgeSet {
        match whole {
            Some(count) => RuntimeNetEdgeSet::fresh(self.next_node(0), count),
            None => RuntimeNetEdgeSet::copy(CopyId(self.next_copy_id)),
        }
    }

    pub(crate) fn resume_call_with_operator_edge_transition(
        &self,
        call: Call,
    ) -> RuntimeNetEdgeTransition {
        RuntimeNetEdgeTransition::new(
            RuntimeNetEdgeSet::node(call.data),
            RuntimeNetEdgeSet::node(self.next_node(0)),
        )
    }

    pub(crate) fn fail_call_edge_transition(&self, call: Call) -> RuntimeNetEdgeTransition {
        RuntimeNetEdgeTransition::new(
            RuntimeNetEdgeSet::default(),
            RuntimeNetEdgeSet::active(call.pair),
        )
    }

    pub(crate) fn fail_published_checkpoint_edge_transition(
        &self,
        call: CallableCheckpointCall,
    ) -> RuntimeNetEdgeTransition {
        RuntimeNetEdgeTransition::new(
            RuntimeNetEdgeSet::node(call.checkpoint),
            RuntimeNetEdgeSet::active(call.pair),
        )
    }

    pub(crate) fn install_call_checkpoint_edge_transition(
        &self,
        call: Call,
    ) -> RuntimeNetEdgeTransition {
        RuntimeNetEdgeTransition::new(
            RuntimeNetEdgeSet::node(call.data),
            RuntimeNetEdgeSet::node(call.data),
        )
    }

    pub(crate) fn take_checkpoint_edge_transition(
        &self,
        call: CallableCheckpointCall,
    ) -> RuntimeNetEdgeTransition {
        RuntimeNetEdgeTransition::new(
            RuntimeNetEdgeSet::node(call.checkpoint),
            RuntimeNetEdgeSet::default(),
        )
    }

    pub(crate) fn publish_checkpoint_edge_transition(
        &self,
        call: CallableCheckpointCall,
    ) -> RuntimeNetEdgeTransition {
        RuntimeNetEdgeTransition::new(
            RuntimeNetEdgeSet::default(),
            RuntimeNetEdgeSet::node(call.checkpoint),
        )
    }

    pub(crate) fn resume_checkpoint_with_copy_edge_transition(
        &self,
        _call: CallableCheckpointCall,
        whole: Option<usize>,
    ) -> RuntimeNetEdgeTransition {
        RuntimeNetEdgeTransition::new(RuntimeNetEdgeSet::default(), self.copy_edges(whole))
    }

    pub(crate) fn resume_checkpoint_with_operator_edge_transition(
        &self,
        _call: CallableCheckpointCall,
    ) -> RuntimeNetEdgeTransition {
        RuntimeNetEdgeTransition::new(
            RuntimeNetEdgeSet::default(),
            RuntimeNetEdgeSet::node(self.next_node(0)),
        )
    }

    pub(crate) fn complete_operator_edge_transition(
        &self,
        call: OperatorCall,
        result: &OperatorYield<S>,
    ) -> RuntimeNetEdgeTransition {
        let adding = match result {
            OperatorYield::Data(_) => RuntimeNetEdgeSet::node(self.next_node(0)),
            // The structural Bind is allocated first.
            OperatorYield::Operator(_) => RuntimeNetEdgeSet::node(self.next_node(1)),
        };
        RuntimeNetEdgeTransition::new(RuntimeNetEdgeSet::nodes(call.operator, call.data), adding)
    }

    pub(crate) fn fail_operator_edge_transition(
        &self,
        call: OperatorCall,
    ) -> RuntimeNetEdgeTransition {
        RuntimeNetEdgeTransition::new(
            RuntimeNetEdgeSet::default(),
            RuntimeNetEdgeSet::active(call.pair),
        )
    }

    pub(crate) fn resolve_cursor_dependency_edge_transition_with_gateway(
        &self,
        cursor: NodeId,
        expected: &CursorDependency<S>,
        gateway: &impl RuntimeNetMutationGateway<S>,
    ) -> RuntimeNetEdgeTransition {
        let leaving = match self.cursor_claim_owner(cursor) {
            Some(CursorClaimOwner::ActivePair(pair))
                if matches!(
                    self.active.get(&pair),
                    Some(ActivePairState::BlockedCursor {
                        cursor: blocked,
                        blockage: CursorBlockage::Dependency(actual),
                    }) if *blocked == cursor && actual.same_with(expected, gateway)
                ) =>
            {
                RuntimeNetEdgeSet::active(pair)
            }
            Some(CursorClaimOwner::Obligation)
                if matches!(
                    self.cursor_obligations.get(&cursor),
                    Some(PairlessCursorObligation {
                        state: PairlessCursorState::Blocked(actual),
                        ..
                    }) if actual.same_with(expected, gateway)
                ) =>
            {
                RuntimeNetEdgeSet::default().with_obligation(cursor)
            }
            _ => RuntimeNetEdgeSet::default(),
        };
        RuntimeNetEdgeTransition::new(leaving, RuntimeNetEdgeSet::default())
    }

    #[cfg(test)]
    fn empty() -> Self {
        Self {
            next_node_id: 0,
            next_fan_site: 0,
            exposed: None,
            nodes: TrustedHashMap::default(),
            next_copy_id: 0,
            copies: TrustedHashMap::default(),
            cursor_obligations: TrustedHashMap::default(),
            active: BTreeMap::new(),
            #[cfg(test)]
            polarity_checked: true,
        }
    }

    #[cfg(test)]
    pub fn active_pairs(&self) -> impl ExactSizeIterator<Item = ActivePairKey> + '_ {
        self.active.keys().copied()
    }

    #[cfg(test)]
    pub fn callable_checkpoint_count(&self) -> usize {
        self.nodes
            .values()
            .filter(|entry| matches!(entry.node, RuntimeNode::CallableCheckpoint(_)))
            .count()
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

    fn cursor_claim_is_stable(&self, cursor: NodeId) -> bool {
        match self.cursor_claim_owner(cursor) {
            Some(CursorClaimOwner::ActivePair(pair)) => matches!(
                self.active.get(&pair),
                Some(ActivePairState::BlockedCursor {
                    cursor: blocked,
                    blockage: CursorBlockage::Stable,
                }) if *blocked == cursor
            ),
            Some(CursorClaimOwner::Obligation) => matches!(
                self.cursor_obligations.get(&cursor),
                Some(PairlessCursorObligation {
                    state: PairlessCursorState::Stable,
                    ..
                })
            ),
            None => false,
        }
    }

    fn inspect_cursor_step(
        &self,
        cursor: NodeId,
        gateway: &impl RuntimeNetMutationGateway<S>,
    ) -> CursorStepInspection<S> {
        match self.cursor_claim_owner(cursor) {
            Some(CursorClaimOwner::ActivePair(pair)) => match self.active.get(&pair) {
                Some(ActivePairState::Ready) => CursorStepInspection::Claimable(Some(pair)),
                Some(ActivePairState::Claimed) => CursorStepInspection::Claimed,
                Some(ActivePairState::BlockedCursor {
                    cursor: blocked,
                    blockage: CursorBlockage::Dependency(dependency),
                }) if *blocked == cursor => {
                    CursorStepInspection::Dependency(dependency.duplicate_with(gateway))
                }
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
                        CursorStepInspection::Dependency(dependency.duplicate_with(gateway))
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

    pub(crate) fn claim_pairless_cursor_obligation(&mut self, cursor: NodeId) -> bool {
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
    pub fn blocked_callable_checkpoint(
        &self,
        pair: ActivePairKey,
    ) -> Option<BlockedCallableCheckpoint<S::WaitToken>> {
        let call = self.callable_checkpoint(pair)?;
        match self.active.get(&pair) {
            Some(ActivePairState::BlockedCallableCheckpoint { generation, wait })
                if *generation == call.generation =>
            {
                Some(BlockedCallableCheckpoint {
                    call,
                    wait: wait.clone(),
                })
            }
            _ => None,
        }
    }

    /// Recovers the structural call represented by a principal `Bind >< Data`
    /// pair, whatever the pair's state.
    #[cfg(test)]
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

    pub fn callable_checkpoint(&self, pair: ActivePairKey) -> Option<CallableCheckpointCall> {
        let (left, right) = self.active_pair_nodes(pair)?;
        match (self.node(left), self.node(right)) {
            (Some(RuntimeNode::Bind), Some(RuntimeNode::CallableCheckpoint(checkpoint))) => {
                Some(CallableCheckpointCall {
                    pair,
                    bind: left,
                    checkpoint: right,
                    generation: checkpoint.generation,
                })
            }
            (Some(RuntimeNode::CallableCheckpoint(checkpoint)), Some(RuntimeNode::Bind)) => {
                Some(CallableCheckpointCall {
                    pair,
                    bind: right,
                    checkpoint: left,
                    generation: checkpoint.generation,
                })
            }
            _ => None,
        }
    }

    pub(crate) fn install_claimed_call_checkpoint(
        &mut self,
        call: Call,
        payload: S::CallableCheckpoint,
    ) -> Result<CallableCheckpointCall, S::CallableCheckpoint> {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
            || !matches!(self.node(call.bind), Some(RuntimeNode::Bind))
            || !matches!(self.node(call.data), Some(RuntimeNode::Data(_)))
        {
            return Err(payload);
        }
        let generation = 0;
        let entry = self
            .nodes
            .get_mut(&call.data)
            .expect("claimed call data node must exist");
        entry.node = RuntimeNode::CallableCheckpoint(RuntimeCallableCheckpoint {
            generation,
            payload: Some(payload),
        });
        self.active.insert(call.pair, ActivePairState::Ready);
        Ok(CallableCheckpointCall {
            pair: call.pair,
            bind: call.bind,
            checkpoint: call.data,
            generation,
        })
    }

    pub(crate) fn take_claimed_callable_checkpoint(
        &mut self,
        call: CallableCheckpointCall,
    ) -> Option<S::CallableCheckpoint> {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
            || !matches!(self.node(call.bind), Some(RuntimeNode::Bind))
        {
            return None;
        }
        let RuntimeNode::CallableCheckpoint(checkpoint) = &mut self
            .nodes
            .get_mut(&call.checkpoint)
            .expect("claimed callable checkpoint node must exist")
            .node
        else {
            return None;
        };
        if checkpoint.generation != call.generation {
            return None;
        }
        checkpoint.payload.take()
    }

    pub(crate) fn restore_claimed_callable_checkpoint(
        &mut self,
        call: CallableCheckpointCall,
        payload: S::CallableCheckpoint,
    ) -> Result<(), S::CallableCheckpoint> {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
            || !matches!(self.node(call.bind), Some(RuntimeNode::Bind))
        {
            return Err(payload);
        }
        let Some(RuntimeNode::CallableCheckpoint(checkpoint)) = self
            .nodes
            .get_mut(&call.checkpoint)
            .map(|entry| &mut entry.node)
        else {
            return Err(payload);
        };
        if checkpoint.generation != call.generation || checkpoint.payload.is_some() {
            return Err(payload);
        }
        checkpoint.payload = Some(payload);
        self.active.insert(call.pair, ActivePairState::Ready);
        Ok(())
    }

    pub(crate) fn replace_claimed_callable_checkpoint(
        &mut self,
        call: CallableCheckpointCall,
        payload: S::CallableCheckpoint,
    ) -> Result<CallableCheckpointCall, S::CallableCheckpoint> {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
            || !matches!(self.node(call.bind), Some(RuntimeNode::Bind))
        {
            return Err(payload);
        }
        let Some(RuntimeNode::CallableCheckpoint(checkpoint)) = self
            .nodes
            .get_mut(&call.checkpoint)
            .map(|entry| &mut entry.node)
        else {
            return Err(payload);
        };
        if checkpoint.generation != call.generation || checkpoint.payload.is_some() {
            return Err(payload);
        }
        let generation = checkpoint
            .generation
            .checked_add(1)
            .expect("interaction-net callable checkpoint generation space exhausted");
        checkpoint.generation = generation;
        checkpoint.payload = Some(payload);
        self.active.insert(call.pair, ActivePairState::Ready);
        Ok(CallableCheckpointCall { generation, ..call })
    }

    pub(crate) fn block_callable_checkpoint(
        &mut self,
        call: CallableCheckpointCall,
        wait: S::WaitToken,
    ) -> CheckpointBlockResult {
        if !matches!(self.active.get(&call.pair), Some(ActivePairState::Ready))
            || self.callable_checkpoint(call.pair) != Some(call)
        {
            return CheckpointBlockResult::Disturbed;
        }
        self.active.insert(
            call.pair,
            ActivePairState::BlockedCallableCheckpoint {
                generation: call.generation,
                wait,
            },
        );
        CheckpointBlockResult::Blocked
    }

    pub(crate) fn retry_blocked_callable_checkpoint(
        &mut self,
        blocked: &BlockedCallableCheckpoint<S::WaitToken>,
    ) -> bool {
        if self.callable_checkpoint(blocked.call.pair) != Some(blocked.call)
            || !matches!(
                self.active.get(&blocked.call.pair),
                Some(ActivePairState::BlockedCallableCheckpoint { generation, wait })
                    if *generation == blocked.call.generation && wait == &blocked.wait
            )
        {
            return false;
        }
        self.active
            .insert(blocked.call.pair, ActivePairState::Ready);
        true
    }

    pub(crate) fn fail_blocked_callable_checkpoint(
        &mut self,
        blocked: &BlockedCallableCheckpoint<S::WaitToken>,
        reason: S::StuckReason,
    ) -> Result<S::CallableCheckpoint, S::StuckReason> {
        if !matches!(self.node(blocked.call.bind), Some(RuntimeNode::Bind))
            || !matches!(
                self.active.get(&blocked.call.pair),
                Some(ActivePairState::BlockedCallableCheckpoint { generation, wait })
                    if *generation == blocked.call.generation && wait == &blocked.wait
            )
        {
            return Err(reason);
        }
        let Some(RuntimeNode::CallableCheckpoint(checkpoint)) = self
            .nodes
            .get_mut(&blocked.call.checkpoint)
            .map(|entry| &mut entry.node)
        else {
            return Err(reason);
        };
        if checkpoint.generation != blocked.call.generation {
            return Err(reason);
        }
        let Some(payload) = checkpoint.payload.take() else {
            return Err(reason);
        };
        self.active.insert(
            blocked.call.pair,
            ActivePairState::Stuck(StuckReason::Specialization(reason)),
        );
        Ok(payload)
    }

    pub(crate) fn fail_claimed_callable_checkpoint(
        &mut self,
        call: CallableCheckpointCall,
        reason: S::StuckReason,
    ) -> Result<(), S::StuckReason> {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
            || !matches!(self.node(call.bind), Some(RuntimeNode::Bind))
            || !matches!(
                self.node(call.checkpoint),
                Some(RuntimeNode::CallableCheckpoint(checkpoint))
                    if checkpoint.generation == call.generation && checkpoint.payload.is_none()
            )
        {
            return Err(reason);
        }
        self.active.insert(
            call.pair,
            ActivePairState::Stuck(StuckReason::Specialization(reason)),
        );
        Ok(())
    }

    pub(crate) fn fail_published_callable_checkpoint(
        &mut self,
        call: CallableCheckpointCall,
        reason: S::StuckReason,
    ) -> Result<(), S::StuckReason> {
        if !matches!(self.active.get(&call.pair), Some(ActivePairState::Ready))
            || !matches!(self.node(call.bind), Some(RuntimeNode::Bind))
        {
            return Err(reason);
        }
        let Some(RuntimeNode::CallableCheckpoint(checkpoint)) = self
            .nodes
            .get_mut(&call.checkpoint)
            .map(|entry| &mut entry.node)
        else {
            return Err(reason);
        };
        if checkpoint.generation != call.generation || checkpoint.payload.is_none() {
            return Err(reason);
        }
        drop(checkpoint.payload.take());
        self.active.insert(
            call.pair,
            ActivePairState::Stuck(StuckReason::Specialization(reason)),
        );
        Ok(())
    }

    #[cfg(test)]
    pub fn cursor_dependency(&self, cursor: NodeId) -> Option<CursorDependency<S>>
    where
        S::Data: Clone,
        S::Operator: Clone,
        S::RuntimeSource: Clone + PartialEq,
    {
        self.cursor_dependency_with(cursor, &DIRECT_RUNTIME_NET_MUTATION_GATEWAY)
    }

    #[cfg(test)]
    fn cursor_dependency_with(
        &self,
        cursor: NodeId,
        gateway: &impl RuntimeNetMutationGateway<S>,
    ) -> Option<CursorDependency<S>> {
        match self.cursor_claim_owner(cursor) {
            Some(CursorClaimOwner::ActivePair(pair)) => match self.active.get(&pair) {
                Some(ActivePairState::BlockedCursor {
                    cursor: blocked,
                    blockage: CursorBlockage::Dependency(dependency),
                }) if *blocked == cursor => Some(dependency.duplicate_with(gateway)),
                _ => None,
            },
            Some(CursorClaimOwner::Obligation) => {
                match &self.cursor_obligations.get(&cursor)?.state {
                    PairlessCursorState::Blocked(dependency) => {
                        Some(dependency.duplicate_with(gateway))
                    }
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
    pub fn claim_call(
        &self,
        call: Call,
        duplicator: &impl RuntimeNetPayloadDuplicator<S>,
    ) -> Option<S::Data> {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
        {
            return None;
        }
        let callable = match self.node(call.data) {
            Some(RuntimeNode::Data(data)) => duplicator.duplicate_data(data),
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

    /// Clones a claimed operator transition so specialization code can run
    /// without holding the shared runtime-net mutex.
    pub fn claim_operator_call(
        &self,
        call: OperatorCall,
        duplicator: &impl RuntimeNetPayloadDuplicator<S>,
    ) -> Option<(S::Operator, S::Data)> {
        if !self
            .active
            .get(&call.pair)
            .is_some_and(ActivePairState::is_claimed)
        {
            return None;
        }
        let operator = match self.node(call.operator) {
            Some(RuntimeNode::Operator(operator)) => duplicator.duplicate_operator(operator),
            _ => panic!("pending operator call agent must exist"),
        };
        let data = match self.node(call.data) {
            Some(RuntimeNode::Data(data)) => duplicator.duplicate_data(data),
            _ => panic!("pending operator call data must exist"),
        };
        Some((operator, data))
    }

    /// Clones a pending operator transition after asserting that it remains
    /// claimed. This compatibility helper does not acquire ownership.
    #[cfg(test)]
    pub fn operator_call_parts(&self, call: OperatorCall) -> (S::Operator, S::Data)
    where
        S::Data: Clone,
        S::Operator: Clone,
    {
        self.claim_operator_call(call, &DIRECT_RUNTIME_NET_MUTATION_GATEWAY)
            .expect("pending operator call must remain claimed")
    }

    /// Recovers the structural operator call represented by a principal
    /// `Operator >< Data` pair, whatever the pair's state.
    #[cfg(test)]
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

    pub fn complete_operator_call(
        &mut self,
        call: OperatorCall,
        result: OperatorYield<S>,
    ) -> NodeId {
        let target = self.take_operator_call(call);
        match result {
            OperatorYield::Data(data) => {
                let node = self.add_node(RuntimeNode::Data(data));
                self.bind_reference(Port::principal(node), target);
                self.debug_check_node_polarity(node);
                node
            }
            OperatorYield::Operator(operator) => {
                // The returned operator becomes a function awaiting its next
                // argument. A function bind lists `[result, argument]`.
                let bind = self.add_node(RuntimeNode::Bind);
                let operator = self.add_node(RuntimeNode::Operator(operator));
                self.bind_reference(Port::principal(bind), target);
                self.connect_provider(Port::auxiliary(bind, 2), Port::principal(operator));
                self.connect_provider(Port::auxiliary(operator, 1), Port::auxiliary(bind, 1));
                self.debug_check_node_polarity(bind);
                self.debug_check_node_polarity(operator);
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

    /// Classifies the work `interface` demands, resuming `route`, the
    /// demanding evaluation's stack from its last poll of this interface.
    pub(crate) fn poll_interface_demand(
        &mut self,
        interface: Port,
        route: &mut InterfaceRoute,
    ) -> RuntimeNetMutation<InterfaceDemand> {
        self.assert_interface(interface);
        let Some(neighbor) = self.neighbor(interface) else {
            route.0.clear();
            return RuntimeNetMutation::Unchanged(InterfaceDemand::NormalForm);
        };

        if neighbor.is_principal() {
            route.0.clear();
            let node = neighbor.node();
            let demand = match self.node(node) {
                Some(RuntimeNode::Data(_)) => InterfaceDemand::Data,
                Some(RuntimeNode::Bind) => InterfaceDemand::Bind,
                Some(RuntimeNode::RemoteCursor { .. }) => {
                    let inserted = self.cursor_claim_owner(node).is_none()
                        && self.ensure_pairless_cursor_obligation(node);
                    let demand = if self.cursor_claim_is_stable(node) {
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
                    | RuntimeNode::CallableCheckpoint(_)
                    | RuntimeNode::Interface,
                )
                | None => InterfaceDemand::NormalForm,
            };
            return RuntimeNetMutation::Unchanged(demand);
        }

        let pair = self.walk_interface_route(neighbor, &mut route.0);
        #[cfg(debug_assertions)]
        assert_eq!(
            pair,
            self.walk_interface_route(neighbor, &mut Vec::new()),
            "a resumed interface walk must find the pair a full walk finds"
        );
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

    /// Walks from an interface's auxiliary `neighbor`, along each node's
    /// principal port to the next node's auxiliary port, to the first
    /// principal-principal wire: the active pair the interface demands.
    /// Returns it, leaving the end of the route walked in `route`, or `None`
    /// at a dead end or a cycle, leaving `route` empty.
    ///
    /// `route` holds the end of the route the last walk took, which this
    /// one resumes. Since then, rewrites may have consumed nodes and wired in
    /// new ones, but the consumed route nodes are a suffix of the route: a
    /// route node's principal port faces the next route node's auxiliary
    /// port, so it joins an active pair only once that next node is
    /// consumed, and a wire between two surviving nodes never changes. So
    /// while a recorded node survives, so does every node before it. Node
    /// IDs are never reused, so the walk drops missing nodes from the tip
    /// and goes on from the last surviving one; with none left it starts
    /// again at the interface. A poll after each rewrite at the far end of
    /// a long chain then costs a few steps, not the chain's length.
    fn walk_interface_route(
        &self,
        neighbor: Port,
        route: &mut Vec<NodeId>,
    ) -> Option<ActivePairKey> {
        while route.last().is_some_and(|node| self.node(*node).is_none()) {
            route.pop();
        }
        let (mut node, mut unrecorded) = match route.pop() {
            Some(tip) => (tip, 0),
            None if neighbor.is_principal() => return None,
            None => (neighbor.node(), UNRECORDED_ROUTE_PREFIX),
        };
        let mut cycle = crate::walk_cycle::WalkCycle::new();
        loop {
            if !cycle.advance(node) {
                route.clear();
                break None;
            }
            if unrecorded == 0 {
                route.push(node);
            } else {
                unrecorded -= 1;
            }
            let Some(principal_neighbor) = self.neighbor(Port::principal(node)) else {
                route.clear();
                break None;
            };
            if principal_neighbor.is_principal() {
                break Some(ActivePairKey::new(node, principal_neighbor.node()));
            }
            node = principal_neighbor.node();
        }
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

    pub(crate) fn resolve_cursor_dependency_with_gateway(
        &mut self,
        cursor: NodeId,
        expected: &CursorDependency<S>,
        disposition: CursorDependencyDisposition,
        gateway: &impl RuntimeNetMutationGateway<S>,
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
                }) if *blocked == cursor && actual.same_with(expected, gateway)
            ),
            CursorClaimOwner::Obligation => matches!(
                self.cursor_obligations.get(&cursor),
                Some(PairlessCursorObligation {
                    state: PairlessCursorState::Blocked(actual),
                    ..
                }) if actual.same_with(expected, gateway)
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

    /// Reduces one arbitrary ready pair, as a low-level test utility.
    /// Cursor-WHNF evaluation deliberately uses exact demand endpoints instead.
    #[cfg(test)]
    pub fn reduce_next(&mut self) -> Option<Reduction>
    where
        S::Data: Clone,
        S::Operator: Clone,
    {
        let pair = self.next_ready_pair()?;
        self.reduce_pair(pair)
    }

    #[cfg(test)]
    pub(crate) fn next_ready_pair(&self) -> Option<ActivePairKey> {
        self.active
            .iter()
            .find_map(|(pair, state)| matches!(state, ActivePairState::Ready).then_some(*pair))
    }

    /// Reduces one exact ready pair through the direct gateway, as a
    /// low-level test utility. Evaluation reduces through its specialization's
    /// gateway with `reduce_pair_with_gateway`.
    #[cfg(test)]
    pub fn reduce_pair(&mut self, pair: ActivePairKey) -> Option<Reduction>
    where
        S::Data: Clone,
        S::Operator: Clone,
    {
        self.reduce_pair_with_gateway(pair, &DIRECT_RUNTIME_NET_MUTATION_GATEWAY)
    }

    pub(crate) fn reduce_pair_with_gateway(
        &mut self,
        pair: ActivePairKey,
        gateway: &impl RuntimeNetPayloadDuplicator<S>,
    ) -> Option<Reduction> {
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
        let left = self.node(left_id).expect("ready pair left node must exist");
        let right = self
            .node(right_id)
            .expect("ready pair right node must exist");
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
        let checkpoint = match (left, right) {
            (RuntimeNode::Bind, RuntimeNode::CallableCheckpoint(_)) => Some((left_id, right_id)),
            (RuntimeNode::CallableCheckpoint(_), RuntimeNode::Bind) => Some((right_id, left_id)),
            _ => None,
        };
        if let Some((bind, checkpoint)) = checkpoint {
            return Some(Reduction {
                pair,
                kind: ReductionKind::CallableCheckpoint { bind, checkpoint },
            });
        }
        if matches!(left, RuntimeNode::CallableCheckpoint(_))
            || matches!(right, RuntimeNode::CallableCheckpoint(_))
        {
            *self.active.get_mut(&pair).unwrap() = ActivePairState::Stuck(StuckReason::NoRule);
            return Some(Reduction {
                pair,
                kind: ReductionKind::Stuck,
            });
        }
        let left = left
            .duplicate_copyable(gateway)
            .expect("non-checkpoint node must remain generically copyable");
        let right = right
            .duplicate_copyable(gateway)
            .expect("non-checkpoint node must remain generically copyable");
        let kind = match (&left, &right) {
            (RuntimeNode::Bind, RuntimeNode::Bind) => {
                self.join(left_id, right_id, 2, rewrite::AuxiliaryPairing::Crossed);
                ReductionKind::BindJoin
            }
            (RuntimeNode::Fan { identity: left }, RuntimeNode::Fan { identity: right }) => {
                if left == right {
                    self.join(left_id, right_id, 2, rewrite::AuxiliaryPairing::Positional);
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
                self.duplicate_data(gateway, left_id, right_id);
                ReductionKind::FanData {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Data(_), RuntimeNode::Fan { identity }) => {
                self.duplicate_data(gateway, right_id, left_id);
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
                self.duplicate_operator(gateway, left_id, identity, right_id);
                ReductionKind::FanOperator {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Operator(_), RuntimeNode::Fan { identity }) => {
                self.duplicate_operator(gateway, right_id, identity, left_id);
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
            (RuntimeNode::CallableCheckpoint(_), _) | (_, RuntimeNode::CallableCheckpoint(_)) => {
                unreachable!("linear checkpoints were excluded before ordinary dispatch")
            }
        };
        if !matches!(
            kind,
            ReductionKind::Call { .. }
                | ReductionKind::CallableCheckpoint { .. }
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
