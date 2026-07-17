use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};

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

#[derive(Clone)]
pub enum CursorDependency<S: NetSpecialization> {
    LocalCursor(NodeId),
    SourceCursor {
        source: SharedRuntimeNet<S>,
        cursor: NodeId,
    },
    SourcePair {
        source: SharedRuntimeNet<S>,
        pair: ActivePairKey,
    },
}

impl<S: NetSpecialization> fmt::Debug for CursorDependency<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalCursor(cursor) => {
                formatter.debug_tuple("LocalCursor").field(cursor).finish()
            }
            Self::SourceCursor { source, cursor } => formatter
                .debug_struct("SourceCursor")
                .field("source", source)
                .field("cursor", cursor)
                .finish(),
            Self::SourcePair { source, pair } => formatter
                .debug_struct("SourcePair")
                .field("source", source)
                .field("pair", pair)
                .finish(),
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedCursor {
    pub pair: ActivePairKey,
    pub cursor: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ActivePairState<S: NetSpecialization> {
    Ready,
    Claimed,
    BlockedCall { wait: S::WaitToken },
    BlockedCursor { cursor: NodeId },
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
    inner: Arc<Mutex<RuntimeNet<S>>>,
}

impl<S: NetSpecialization> SharedRuntimeNet<S> {
    pub fn new(runtime: RuntimeNet<S>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(runtime)),
        }
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn with<R>(&self, inspect: impl FnOnce(&RuntimeNet<S>) -> R) -> R {
        let runtime = self.inner.lock().expect("shared runtime net was poisoned");
        inspect(&runtime)
    }

    pub fn with_mut<R>(&self, update: impl FnOnce(&mut RuntimeNet<S>) -> R) -> R {
        let mut runtime = self.inner.lock().expect("shared runtime net was poisoned");
        update(&mut runtime)
    }
}

impl<S: NetSpecialization> SharedRuntimeNet<S> {
    /// Inspects and advances a previously claimed cursor without holding target
    /// and source runtime locks at the same time.
    pub fn advance_claimed_cursor(&self, cursor: NodeId) -> Option<CursorProgress> {
        let claim = self.with(|target| target.cursor_claim(cursor))?;
        let source = claim.source.clone();
        let frontier = source.with(|runtime| runtime.inspect_source_frontier(claim.remote));
        Some(self.with_mut(|target| target.finish_cursor_claim(claim, frontier)))
    }
}

impl<S: NetSpecialization> SharedRuntimeNet<S> {
    /// Resolves one exact claimed `Data >< Bind` pair using client callable
    /// policy. Claiming and finishing each take a short target lock; callable
    /// conversion itself runs without holding the runtime mutex.
    pub fn resolve_call(&self, call: Call) -> Result<bool, S::StuckReason> {
        let Some(data) = self.with_mut(|runtime| runtime.claim_call(call)) else {
            return Ok(false);
        };

        match S::callable(data) {
            Ok(Callable::Net(source)) => {
                self.with_mut(|runtime| runtime.resume_claimed_call_with_copy(call, source));
                Ok(true)
            }
            Ok(Callable::Operator(operator)) => {
                self.with_mut(|runtime| {
                    runtime.resume_claimed_call_with_operator(call, operator);
                });
                Ok(true)
            }
            Err(error) => {
                self.with_mut(|runtime| {
                    runtime.fail_claimed_call(call, error.clone());
                });
                Err(error)
            }
        }
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
    pair: Option<ActivePairKey>,
    copy: CopyId,
    remote: Port,
    source: SharedRuntimeNet<S>,
}

enum SourceFrontier<S: NetSpecialization> {
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
    cursor_dependencies: HashMap<NodeId, CursorDependency<S>>,

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
            cursor_dependencies: HashMap::new(),
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
            cursor_dependencies: HashMap::new(),
            active: BTreeMap::new(),
        }
    }

    pub fn active_pairs(&self) -> impl ExactSizeIterator<Item = ActivePairKey> + '_ {
        self.active.keys().copied()
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
                ActivePairState::BlockedCursor { cursor } => Some((
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

    pub fn blocked_cursor(&self, pair: ActivePairKey) -> Option<BlockedCursor> {
        match self.active.get(&pair) {
            Some(ActivePairState::BlockedCursor { cursor }) => Some(BlockedCursor {
                pair,
                cursor: *cursor,
            }),
            _ => None,
        }
    }

    pub fn blocked_calls(&self) -> impl Iterator<Item = BlockedCall<S::WaitToken>> + '_ {
        self.active.iter().filter_map(|(pair, state)| match state {
            ActivePairState::BlockedCall { wait } => Some(BlockedCall {
                pair: *pair,
                wait: wait.clone(),
            }),
            _ => None,
        })
    }

    pub fn blocked_call(&self, pair: ActivePairKey) -> Option<BlockedCall<S::WaitToken>> {
        match self.active.get(&pair) {
            Some(ActivePairState::BlockedCall { wait }) => Some(BlockedCall {
                pair,
                wait: wait.clone(),
            }),
            _ => None,
        }
    }

    pub fn cursor_dependency(&self, cursor: NodeId) -> Option<CursorDependency<S>> {
        self.cursor_dependencies.get(&cursor).cloned()
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
    fn claim_call(&mut self, call: Call) -> Option<S::Data> {
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
    fn fail_claimed_call(&mut self, call: Call, reason: S::StuckReason) {
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
    /// No evaluator currently produces this state; this transition is the
    /// runtime contract for the later blocking-callable spike.
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

    pub fn demand_interface(&mut self, interface: Port) -> Option<CursorProgress> {
        self.assert_interface(interface);
        let cursor = self.cursor_across(interface)?;
        self.begin_cursor_claim(cursor, None)
    }

    /// Claims a cursor reached through an exact layered-copy dependency.
    pub fn claim_dependent_cursor(&mut self, cursor: NodeId) -> Option<CursorProgress> {
        if !matches!(self.node(cursor), Some(RuntimeNode::RemoteCursor { .. })) {
            return None;
        }
        self.begin_cursor_claim(cursor, None)
    }

    pub fn retry_blocked_cursor(&mut self, cursor: NodeId) -> bool {
        let Some(pair) = self.active_pair_key(cursor) else {
            return false;
        };
        if !matches!(
            self.active.get(&pair),
            Some(ActivePairState::BlockedCursor { cursor: blocked }) if *blocked == cursor
        ) {
            return false;
        }
        self.active.insert(pair, ActivePairState::Ready);
        true
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
