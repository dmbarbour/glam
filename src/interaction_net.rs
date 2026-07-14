//! Generic port-and-wire interaction-net topology and reduction.
//!
//! Embedded data is supplied by the client. Immutable templates and runtime
//! nets allocate fan sites locally. Lazy copies translate source sites into
//! fresh target sites while preserving the complete residual history.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

const PORT_BITS: u32 = 2;
const PORT_MASK: u64 = (1 << PORT_BITS) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u64);

impl NodeId {
    fn from_index(index: usize) -> Self {
        Self(u64::try_from(index).expect("interaction-net node index does not fit in u64"))
    }

    fn index(self) -> usize {
        usize::try_from(self.0).expect("interaction-net node ID does not fit in usize")
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FanSite(u64);

impl FanSite {
    pub fn get(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    const fn from_raw(site: u64) -> Self {
        Self(site)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DuplicationStep {
    pub through: FanIdentity,
    pub branch: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FanIdentity {
    pub site: FanSite,
    pub context: Arc<[DuplicationStep]>,
}

impl FanIdentity {
    fn root(site: FanSite) -> Self {
        Self {
            site,
            context: Arc::from([]),
        }
    }

    fn residual(&self, through: &Self, branch: u8) -> Self {
        let mut context = self.context.to_vec();
        context.push(DuplicationStep {
            through: through.clone(),
            branch,
        });
        Self {
            site: self.site,
            context: Arc::from(context),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Port(NonZeroU64);

impl Port {
    pub fn principal(node: NodeId) -> Self {
        Self::new(node, 0)
    }

    pub fn auxiliary(node: NodeId, index: u32) -> Self {
        assert!(
            (1..=2).contains(&index),
            "auxiliary port index must be 1 or 2"
        );
        Self::new(node, index)
    }

    fn new(node: NodeId, index: u32) -> Self {
        let index = u64::from(index);
        let max_node = (u64::MAX - index - 1) >> PORT_BITS;
        assert!(
            node.0 <= max_node,
            "interaction-net packed port space exhausted"
        );
        let tagged = (node.0 << PORT_BITS) + index + 1;
        Self(NonZeroU64::new(tagged).expect("packed port is always nonzero"))
    }

    pub fn node(self) -> NodeId {
        NodeId((self.0.get() - 1) >> PORT_BITS)
    }

    pub fn index(self) -> u32 {
        ((self.0.get() - 1) & PORT_MASK) as u32
    }

    pub fn is_principal(self) -> bool {
        self.index() == 0
    }
}

impl fmt::Debug for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Port")
            .field("node", &self.node())
            .field("index", &self.index())
            .finish()
    }
}

/// Immutable nodes in a reusable interaction-net template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node<D> {
    /// Function or application constructor. Ports: `[ap*, arg, result]`.
    Bind,
    /// Binary Lamping-style fan. Ports: `[input*, left, right]`.
    Fan { site: FanSite },
    /// Eraser for a value used zero times. Port: `[input*]`.
    Erase,
    /// Client-defined embedded data. Port: `[data*]`.
    Data(D),
}

impl<D> Node<D> {
    fn port_count(&self) -> u32 {
        match self {
            Self::Bind | Self::Fan { .. } => 3,
            Self::Erase | Self::Data(_) => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeNode<D> {
    Bind,
    Fan {
        identity: FanIdentity,
    },
    Erase,
    Data(D),
    /// Stable, evaluator-only anchor for a runtime net's exposed port.
    Interface,
    /// Evaluator-only one-way wire into a logical copy of another runtime net.
    RemoteCursor {
        copy: CopyId,
        remote: Port,
    },
}

impl<D> RuntimeNode<D> {
    fn port_count(&self) -> u32 {
        match self {
            Self::Bind | Self::Fan { .. } => 3,
            Self::Interface => 2,
            Self::Erase | Self::Data(_) | Self::RemoteCursor { .. } => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CopyId(u64);

impl CopyId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wire {
    pub left: Port,  // port including node ID and index
    pub right: Port, // each port is wired to exactly one other port (except the exposed port)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivePair {
    pub left: NodeId,
    pub right: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionNet<D> {
    nodes: Arc<[Node<D>]>,           // nodes identified by index
    wires: Arc<[Wire]>,              // all wires between ports
    exposed: Port,                   // closed net has one exposed port
    active_pairs: Arc<[ActivePair]>, // subset of wires connecting principal ports
}

impl<D> InteractionNet<D> {
    pub fn nodes(&self) -> &[Node<D>] {
        &self.nodes
    }

    pub fn wires(&self) -> &[Wire] {
        &self.wires
    }

    pub fn exposed(&self) -> Port {
        self.exposed
    }

    pub fn active_pairs(&self) -> &[ActivePair] {
        &self.active_pairs
    }
}

impl<D: Clone + 'static> InteractionNet<D> {
    pub fn instantiate(&self) -> RuntimeNet<D> {
        self.instantiate_with(Arc::new(D::clone))
    }

    pub fn instantiate_with(&self, map_data: Arc<dyn Fn(&D) -> D + Send + Sync>) -> RuntimeNet<D> {
        RuntimeNet::new(self, map_data)
    }

    pub fn instantiate_shared(&self) -> SharedRuntimeNet<D> {
        SharedRuntimeNet::new(self.instantiate())
    }

    pub fn instantiate_shared_with(
        &self,
        map_data: Arc<dyn Fn(&D) -> D + Send + Sync>,
    ) -> SharedRuntimeNet<D> {
        SharedRuntimeNet::new(self.instantiate_with(map_data))
    }
}

/// Checked construction of a reusable net template.
pub struct NetBuilder<D> {
    nodes: Vec<BuilderNode<D>>,
    wires: Vec<Wire>,
    next_fan_site: u64,
}

enum BuilderNode<D> {
    Runtime(Node<D>),
    /// Builder-only two-ended alias used to represent `.copy 1`. Finalization
    /// splices it out, so tunnels never enter an immutable template.
    Tunnel,
}

impl<D> BuilderNode<D> {
    fn port_count(&self) -> u32 {
        match self {
            Self::Runtime(node) => node.port_count(),
            Self::Tunnel => 2,
        }
    }

    fn is_tunnel(&self) -> bool {
        matches!(self, Self::Tunnel)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyPorts {
    pub input: Port,
    pub outputs: Vec<Port>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetBuildError {
    InvalidPort(Port),
    InvalidExposedPort(Port),
    SelfWire(Port),
    PortAlreadyWired(Port),
    ExposedPortWired(Port),
    PortUnwired(Port),
    TunnelCycle,
}

impl fmt::Display for NetBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPort(port) => write!(formatter, "invalid interaction-net port {port:?}"),
            Self::InvalidExposedPort(port) => {
                write!(formatter, "invalid exposed interaction-net port {port:?}")
            }
            Self::SelfWire(port) => {
                write!(
                    formatter,
                    "interaction-net port {port:?} is wired to itself"
                )
            }
            Self::PortAlreadyWired(port) => {
                write!(
                    formatter,
                    "interaction-net port {port:?} is wired more than once"
                )
            }
            Self::ExposedPortWired(port) => {
                write!(formatter, "exposed interaction-net port {port:?} is wired")
            }
            Self::PortUnwired(port) => {
                write!(formatter, "interaction-net port {port:?} is unwired")
            }
            Self::TunnelCycle => formatter
                .write_str("interaction-net copy tunnels form a component with no runtime node"),
        }
    }
}

impl std::error::Error for NetBuildError {}

impl<D> Default for NetBuilder<D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<D> NetBuilder<D> {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            wires: Vec::new(),
            next_fan_site: 0,
        }
    }

    pub fn push(&mut self, node: Node<D>) -> NodeId {
        let id = NodeId::from_index(self.nodes.len());
        self.nodes.push(BuilderNode::Runtime(node));
        id
    }

    pub fn bind(&mut self) -> [Port; 3] {
        let node = self.push(Node::Bind);
        [
            Port::principal(node),
            Port::auxiliary(node, 1),
            Port::auxiliary(node, 2),
        ]
    }

    pub fn data(&mut self, data: D) -> Port {
        let node = self.push(Node::Data(data));
        Port::principal(node)
    }

    pub fn push_fan(&mut self) -> NodeId {
        let site = FanSite(self.next_fan_site);
        self.next_fan_site = self
            .next_fan_site
            .checked_add(1)
            .expect("too many fan sites in one interaction-net template");
        self.push(Node::Fan { site })
    }

    /// Constructs an N-way logical copy. The first port is the input and the
    /// returned outputs are its branches. Zero outputs use an eraser, one uses
    /// a builder-only tunnel, and larger copies use a balanced binary fan tree.
    pub fn copy(&mut self, outputs: usize) -> CopyPorts {
        match outputs {
            0 => {
                let erase = self.push(Node::Erase);
                CopyPorts {
                    input: Port::principal(erase),
                    outputs: Vec::new(),
                }
            }
            1 => {
                let tunnel = NodeId::from_index(self.nodes.len());
                self.nodes.push(BuilderNode::Tunnel);
                CopyPorts {
                    input: Port::principal(tunnel),
                    outputs: vec![Port::auxiliary(tunnel, 1)],
                }
            }
            outputs => {
                let root = self.push_fan();
                let mut leaves = Vec::with_capacity(outputs);
                let left = outputs / 2;
                self.copy_branch(Port::auxiliary(root, 1), left, &mut leaves);
                self.copy_branch(Port::auxiliary(root, 2), outputs - left, &mut leaves);
                CopyPorts {
                    input: Port::principal(root),
                    outputs: leaves,
                }
            }
        }
    }

    fn copy_branch(&mut self, branch: Port, outputs: usize, leaves: &mut Vec<Port>) {
        if outputs == 1 {
            leaves.push(branch);
            return;
        }
        let fan = self.push_fan();
        self.wire(branch, Port::principal(fan));
        let left = outputs / 2;
        self.copy_branch(Port::auxiliary(fan, 1), left, leaves);
        self.copy_branch(Port::auxiliary(fan, 2), outputs - left, leaves);
    }

    pub fn try_wire(&mut self, left: Port, right: Port) -> Result<(), NetBuildError> {
        for port in [left, right] {
            if !self.valid_port(port) {
                return Err(NetBuildError::InvalidPort(port));
            }
            if self.port_is_wired(port) {
                return Err(NetBuildError::PortAlreadyWired(port));
            }
        }
        if left == right {
            return Err(NetBuildError::SelfWire(left));
        }
        self.wires.push(Wire { left, right });
        Ok(())
    }

    pub fn wire(&mut self, left: Port, right: Port) {
        self.try_wire(left, right)
            .expect("invalid interaction-net wire")
    }

    pub fn finish(self, exposed: Port) -> InteractionNet<D> {
        self.try_finish(exposed)
            .expect("invalid interaction-net template")
    }

    pub fn try_finish(self, exposed: Port) -> Result<InteractionNet<D>, NetBuildError> {
        self.validate(exposed)?;
        self.normalize(exposed)
    }

    fn normalize(self, exposed: Port) -> Result<InteractionNet<D>, NetBuildError> {
        let is_tunnel = self
            .nodes
            .iter()
            .map(BuilderNode::is_tunnel)
            .collect::<Vec<_>>();
        let tunnel_count = is_tunnel.iter().filter(|is_tunnel| **is_tunnel).count();
        let links = self
            .wires
            .iter()
            .flat_map(|wire| [(wire.left, wire.right), (wire.right, wire.left)])
            .collect::<HashMap<_, _>>();

        let mut runtime_nodes = Vec::with_capacity(self.nodes.len() - tunnel_count);
        let mut node_map = vec![None; self.nodes.len()];
        for (old_index, node) in self.nodes.into_iter().enumerate() {
            if let BuilderNode::Runtime(node) = node {
                let new = NodeId::from_index(runtime_nodes.len());
                node_map[old_index] = Some(new);
                runtime_nodes.push(node);
            }
        }

        let mut visited_tunnels = HashSet::new();
        let exposed_runtime = if is_tunnel[exposed.node().index()] {
            let terminal =
                follow_tunnels(exposed, exposed, &links, &is_tunnel, &mut visited_tunnels)?;
            remap_port(terminal, &node_map)
        } else {
            remap_port(exposed, &node_map)
        };

        let mut runtime_wires = Vec::new();
        for (old_index, mapped) in node_map.iter().enumerate() {
            let Some(mapped_node) = mapped else {
                continue;
            };
            let port_count = runtime_nodes[mapped_node.index()].port_count();
            for index in 0..port_count {
                let old = Port::new(NodeId::from_index(old_index), index);
                let local = Port::new(*mapped_node, index);
                if local == exposed_runtime {
                    continue;
                }
                let neighbor = *links
                    .get(&old)
                    .expect("validated non-exposed port must remain wired");
                let terminal =
                    follow_tunnels(neighbor, exposed, &links, &is_tunnel, &mut visited_tunnels)?;
                let remote = remap_port(terminal, &node_map);
                if local == remote {
                    return Err(NetBuildError::SelfWire(local));
                }
                if local < remote {
                    runtime_wires.push(Wire {
                        left: local,
                        right: remote,
                    });
                }
            }
        }
        if visited_tunnels.len() != tunnel_count {
            return Err(NetBuildError::TunnelCycle);
        }

        let active_pairs = runtime_wires
            .iter()
            .filter(|wire| wire.left.is_principal() && wire.right.is_principal())
            .map(|wire| ActivePair {
                left: wire.left.node(),
                right: wire.right.node(),
            })
            .collect::<Vec<_>>();
        Ok(InteractionNet {
            nodes: Arc::from(runtime_nodes),
            wires: Arc::from(runtime_wires),
            exposed: exposed_runtime,
            active_pairs: Arc::from(active_pairs),
        })
    }

    fn validate(&self, exposed: Port) -> Result<(), NetBuildError> {
        if !self.valid_port(exposed) {
            return Err(NetBuildError::InvalidExposedPort(exposed));
        }
        let mut wired = self
            .nodes
            .iter()
            .map(|node| vec![false; node.port_count() as usize])
            .collect::<Vec<_>>();
        for wire in &self.wires {
            for port in [wire.left, wire.right] {
                if port == exposed {
                    return Err(NetBuildError::ExposedPortWired(port));
                }
                let Some(slot) = wired
                    .get_mut(port.node().index())
                    .and_then(|ports| ports.get_mut(port.index() as usize))
                else {
                    return Err(NetBuildError::InvalidPort(port));
                };
                if *slot {
                    return Err(NetBuildError::PortAlreadyWired(port));
                }
                *slot = true;
            }
        }
        for (node_id, ports) in wired.iter().enumerate() {
            for (index, is_wired) in ports.iter().enumerate() {
                let node = NodeId::from_index(node_id);
                let port = if index == 0 {
                    Port::principal(node)
                } else {
                    Port::auxiliary(node, index as u32)
                };
                if !*is_wired && port != exposed {
                    return Err(NetBuildError::PortUnwired(port));
                }
            }
        }
        Ok(())
    }

    fn valid_port(&self, port: Port) -> bool {
        self.nodes
            .get(port.node().index())
            .is_some_and(|node| port.index() < node.port_count())
    }

    fn port_is_wired(&self, port: Port) -> bool {
        self.wires
            .iter()
            .any(|wire| wire.left == port || wire.right == port)
    }
}

fn follow_tunnels(
    mut port: Port,
    exposed: Port,
    links: &HashMap<Port, Port>,
    is_tunnel: &[bool],
    visited_tunnels: &mut HashSet<NodeId>,
) -> Result<Port, NetBuildError> {
    let mut path = HashSet::new();
    loop {
        let Some(is_tunnel) = is_tunnel.get(port.node().index()) else {
            return Err(NetBuildError::InvalidPort(port));
        };
        if !is_tunnel {
            return Ok(port);
        }
        if !path.insert(port) {
            return Err(NetBuildError::TunnelCycle);
        }
        visited_tunnels.insert(port.node());
        let other = if port.index() == 0 {
            Port::auxiliary(port.node(), 1)
        } else {
            Port::principal(port.node())
        };
        if other == exposed {
            return Err(NetBuildError::TunnelCycle);
        }
        port = *links.get(&other).ok_or(NetBuildError::PortUnwired(other))?;
    }
}

fn remap_port(port: Port, node_map: &[Option<NodeId>]) -> Port {
    let node = node_map[port.node().index()].expect("terminal port must belong to a runtime node");
    Port::new(node, port.index())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reduction {
    pub pair: ActivePair,
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
    Erase,
    Call {
        bind: NodeId,
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
    Materialized { node: NodeId },
    MaterializedPair { left: NodeId, right: NodeId },
    Joined,
    SourceSweep { reductions: usize },
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedCall {
    pub pair: ActivePair,
    pub bind: NodeId,
    pub data: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFrame<D> {
    pub callable: D,
    pub argument: Port,
    pub result: Port,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedCursor {
    pub pair: ActivePair,
    pub cursor: NodeId,
}

pub struct SharedRuntimeNet<D> {
    inner: Arc<Mutex<RuntimeNet<D>>>,
}

impl<D> SharedRuntimeNet<D> {
    pub fn new(runtime: RuntimeNet<D>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(runtime)),
        }
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn with<R>(&self, inspect: impl FnOnce(&RuntimeNet<D>) -> R) -> R {
        let runtime = self.inner.lock().expect("shared runtime net was poisoned");
        inspect(&runtime)
    }

    pub fn with_mut<R>(&self, update: impl FnOnce(&mut RuntimeNet<D>) -> R) -> R {
        let mut runtime = self.inner.lock().expect("shared runtime net was poisoned");
        update(&mut runtime)
    }
}

impl<D> Clone for SharedRuntimeNet<D> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<D> fmt::Debug for SharedRuntimeNet<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SharedRuntimeNet")
            .field(&Arc::as_ptr(&self.inner))
            .finish()
    }
}

impl<D> PartialEq for SharedRuntimeNet<D> {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other)
    }
}

impl<D> Eq for SharedRuntimeNet<D> {}

struct CopyState<D> {
    source: SharedRuntimeNet<D>,
    map_data: Arc<dyn Fn(&D) -> D + Send + Sync>,
    mapped_nodes: HashMap<NodeId, NodeId>,
    frontiers: HashMap<Port, NodeId>,
    fan_sites: HashMap<FanSite, FanSite>,
}

struct RuntimeEntry<D> {
    node: RuntimeNode<D>,
    links: [Option<Port>; 3],
}

impl<D> RuntimeEntry<D> {
    fn new(node: RuntimeNode<D>) -> Self {
        Self {
            node,
            links: [None; 3],
        }
    }
}

pub struct RuntimeNet<D> {
    next_node_id: u64,
    next_fan_site: u64,
    exposed: Option<Port>,
    nodes: HashMap<NodeId, RuntimeEntry<D>>,
    next_copy_id: u64,
    has_imported_copy: bool,
    copies: HashMap<CopyId, CopyState<D>>,
    ready: VecDeque<ActivePair>,
    calls: VecDeque<BlockedCall>,
    blocked_cursors: VecDeque<BlockedCursor>,
    stuck: Vec<ActivePair>,
}

impl<D: Clone + 'static> RuntimeNet<D> {
    fn new(net: &InteractionNet<D>, map_data: Arc<dyn Fn(&D) -> D + Send + Sync>) -> Self {
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
                    Node::Data(data) => RuntimeNode::Data(map_data(data)),
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
            has_imported_copy: false,
            copies: HashMap::new(),
            ready: VecDeque::new(),
            calls: VecDeque::new(),
            blocked_cursors: VecDeque::new(),
            stuck: Vec::new(),
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
            has_imported_copy: false,
            copies: HashMap::new(),
            ready: VecDeque::new(),
            calls: VecDeque::new(),
            blocked_cursors: VecDeque::new(),
            stuck: Vec::new(),
        }
    }

    pub fn active_pairs(&self) -> Vec<ActivePair> {
        self.ready
            .iter()
            .copied()
            .chain(self.calls.iter().map(|call| call.pair))
            .chain(self.blocked_cursors.iter().map(|cursor| cursor.pair))
            .chain(self.stuck.iter().copied())
            .collect()
    }

    /// Stable evaluator-owned anchor wired to the net's exposed template port.
    pub fn exposed(&self) -> Port {
        self.exposed
            .expect("runtime net was constructed without an exposed port")
    }

    pub fn ready_pairs(&self) -> &VecDeque<ActivePair> {
        &self.ready
    }

    pub fn blocked_calls(&self) -> &VecDeque<BlockedCall> {
        &self.calls
    }

    pub fn blocked_cursors(&self) -> &VecDeque<BlockedCursor> {
        &self.blocked_cursors
    }

    pub fn blocked_cursor_sources(&self) -> Vec<SharedRuntimeNet<D>> {
        self.blocked_cursors
            .iter()
            .filter_map(|blocked| {
                let RuntimeNode::RemoteCursor { copy, .. } = self.node(blocked.cursor)? else {
                    return None;
                };
                self.copies.get(copy).map(|state| state.source.clone())
            })
            .collect()
    }

    pub fn stuck_pairs(&self) -> &[ActivePair] {
        &self.stuck
    }

    /// Whether this runtime has ever imported a logical copy. This remains
    /// true after its copy frontiers converge, so evaluator-owned suspended
    /// wires can be distinguished from an unsupplied canonical root.
    pub fn has_imported_copy(&self) -> bool {
        self.has_imported_copy
    }

    pub fn node(&self, id: NodeId) -> Option<&RuntimeNode<D>> {
        self.nodes.get(&id).map(|entry| &entry.node)
    }

    pub fn call_data(&self, call: BlockedCall) -> &D {
        assert!(self.calls.contains(&call), "call must still be blocked");
        match self.node(call.data) {
            Some(RuntimeNode::Data(data)) => data,
            _ => panic!("blocked call data node must exist"),
        }
    }

    pub fn call_argument_data(&self, call: BlockedCall) -> Option<&D> {
        assert!(self.calls.contains(&call), "call must still be blocked");
        let argument = self.neighbor(Port::auxiliary(call.bind, 1))?;
        if !argument.is_principal() {
            return None;
        }
        match self.node(argument.node())? {
            RuntimeNode::Data(data) => Some(data),
            _ => None,
        }
    }

    pub fn demand_call_argument(&mut self, call: BlockedCall) -> Option<CursorProgress> {
        assert!(self.calls.contains(&call), "call must still be blocked");
        self.demand_cursor_across(Port::auxiliary(call.bind, 1))
    }

    pub fn call_argument_cursor_source(&self, call: BlockedCall) -> Option<SharedRuntimeNet<D>> {
        assert!(self.calls.contains(&call), "call must still be blocked");
        self.cursor_source_across(Port::auxiliary(call.bind, 1))
    }

    pub fn interface_data(&self, interface: Port) -> Option<&D> {
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

    /// Returns the port wired to `port`, for evaluator diagnostics and demand
    /// propagation across evaluator-owned interfaces.
    pub fn port_neighbor(&self, port: Port) -> Option<Port> {
        self.neighbor(port)
    }

    pub fn demand_interface(&mut self, interface: Port) -> Option<CursorProgress> {
        self.assert_interface(interface);
        self.demand_cursor_across(interface)
    }

    pub fn interface_cursor_source(&self, interface: Port) -> Option<SharedRuntimeNet<D>> {
        self.assert_interface(interface);
        self.cursor_source_across(interface)
    }

    pub fn wake_blocked_cursors(&mut self) {
        while let Some(blocked) = self.blocked_cursors.pop_front() {
            if self.principals_connect(blocked.pair) {
                self.ready.push_back(blocked.pair);
            }
        }
    }

    pub fn reduce_next(&mut self) -> Option<Reduction> {
        let pair = self.ready.pop_front()?;
        let left_port = Port::principal(pair.left);
        let right_port = Port::principal(pair.right);
        assert_eq!(
            self.neighbor(left_port),
            Some(right_port),
            "ready pair must still connect both principal ports"
        );
        let left = self
            .node(pair.left)
            .expect("ready pair left node must exist")
            .clone();
        let right = self
            .node(pair.right)
            .expect("ready pair right node must exist")
            .clone();
        let cursor = match (&left, &right) {
            (RuntimeNode::RemoteCursor { .. }, _) => Some(pair.left),
            (_, RuntimeNode::RemoteCursor { .. }) => Some(pair.right),
            _ => None,
        };
        if let Some(cursor) = cursor {
            let eraser = match (&left, &right) {
                (RuntimeNode::Erase, RuntimeNode::RemoteCursor { .. }) => Some(pair.left),
                (RuntimeNode::RemoteCursor { .. }, RuntimeNode::Erase) => Some(pair.right),
                _ => None,
            };
            if let Some(eraser) = eraser {
                self.erase_remote_cursor(eraser, cursor);
                return Some(Reduction {
                    pair,
                    kind: ReductionKind::Erase,
                });
            }
            let progress = self.advance_remote_cursor(cursor);
            if progress == CursorProgress::Blocked {
                self.blocked_cursors
                    .push_back(BlockedCursor { pair, cursor });
            } else if matches!(progress, CursorProgress::SourceSweep { .. }) {
                self.ready.push_back(pair);
            }
            return Some(Reduction {
                pair,
                kind: ReductionKind::RemoteCursor { cursor, progress },
            });
        }
        let kind = match (&left, &right) {
            (RuntimeNode::Bind, RuntimeNode::Bind) => {
                self.join(pair.left, pair.right, 2);
                ReductionKind::BindJoin
            }
            (RuntimeNode::Fan { identity: left }, RuntimeNode::Fan { identity: right }) => {
                if left == right {
                    self.join(pair.left, pair.right, 2);
                    ReductionKind::FanJoin {
                        identity: left.clone(),
                    }
                } else {
                    self.commute_fans(pair.left, left, pair.right, right);
                    ReductionKind::FanCommute {
                        left: left.clone(),
                        right: right.clone(),
                    }
                }
            }
            (RuntimeNode::Fan { identity }, RuntimeNode::Data(_)) => {
                self.duplicate_data(pair.left, pair.right);
                ReductionKind::FanData {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Data(_), RuntimeNode::Fan { identity }) => {
                self.duplicate_data(pair.right, pair.left);
                ReductionKind::FanData {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Fan { identity }, RuntimeNode::Bind) => {
                self.duplicate_bind(pair.left, identity, pair.right);
                ReductionKind::FanBind {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Bind, RuntimeNode::Fan { identity }) => {
                self.duplicate_bind(pair.right, identity, pair.left);
                ReductionKind::FanBind {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::Erase, _) => {
                self.erase(pair.left, pair.right);
                ReductionKind::Erase
            }
            (_, RuntimeNode::Erase) => {
                self.erase(pair.right, pair.left);
                ReductionKind::Erase
            }
            (RuntimeNode::Bind, RuntimeNode::Data(_)) => {
                self.calls.push_back(BlockedCall {
                    pair,
                    bind: pair.left,
                    data: pair.right,
                });
                ReductionKind::Call {
                    bind: pair.left,
                    data: pair.right,
                }
            }
            (RuntimeNode::Data(_), RuntimeNode::Bind) => {
                self.calls.push_back(BlockedCall {
                    pair,
                    bind: pair.right,
                    data: pair.left,
                });
                ReductionKind::Call {
                    bind: pair.right,
                    data: pair.left,
                }
            }
            (RuntimeNode::Data(_), RuntimeNode::Data(_)) => {
                self.stuck.push(pair);
                ReductionKind::Stuck
            }
            (RuntimeNode::Interface, _)
            | (_, RuntimeNode::Interface)
            | (RuntimeNode::RemoteCursor { .. }, _)
            | (_, RuntimeNode::RemoteCursor { .. }) => {
                unreachable!("evaluator-only nodes do not use ordinary interaction rules")
            }
        };
        Some(Reduction { pair, kind })
    }

    /// Starts one logical copy and returns its initially unwired remote cursor.
    pub fn begin_copy(&mut self, source: SharedRuntimeNet<D>) -> NodeId {
        self.begin_copy_with(source, Arc::new(D::clone))
    }

    pub fn begin_copy_with(
        &mut self,
        source: SharedRuntimeNet<D>,
        map_data: Arc<dyn Fn(&D) -> D + Send + Sync>,
    ) -> NodeId {
        self.has_imported_copy = true;
        let remote = source.with(RuntimeNet::exposed);
        let copy = CopyId(self.next_copy_id);
        self.next_copy_id = self
            .next_copy_id
            .checked_add(1)
            .expect("interaction-net copy ID space exhausted");
        assert!(
            self.copies
                .insert(
                    copy,
                    CopyState {
                        source,
                        map_data,
                        mapped_nodes: HashMap::new(),
                        frontiers: HashMap::new(),
                        fan_sites: HashMap::new(),
                    },
                )
                .is_none()
        );
        let cursor = self.add_node(RuntimeNode::RemoteCursor { copy, remote });
        self.copies
            .get_mut(&copy)
            .unwrap()
            .frontiers
            .insert(remote, cursor);
        cursor
    }

    /// Replaces a blocked bind-data call with a cursor into a shared net.
    pub fn resume_call_with_copy(
        &mut self,
        call: BlockedCall,
        source: SharedRuntimeNet<D>,
    ) -> NodeId {
        let Some(index) = self.calls.iter().position(|blocked| *blocked == call) else {
            panic!("resumed interaction-net call must still be blocked");
        };
        self.calls.remove(index);
        assert_eq!(
            self.disconnect(Port::principal(call.bind)),
            Some(Port::principal(call.data))
        );
        assert!(matches!(self.remove_node(call.data), RuntimeNode::Data(_)));
        let cursor = self.begin_copy(source);
        self.connect(Port::principal(call.bind), Port::principal(cursor));
        cursor
    }

    pub fn resume_call_with_copy_map(
        &mut self,
        call: BlockedCall,
        source: SharedRuntimeNet<D>,
        map_data: Arc<dyn Fn(&D) -> D + Send + Sync>,
    ) -> NodeId {
        let Some(index) = self.calls.iter().position(|blocked| *blocked == call) else {
            panic!("resumed interaction-net call must still be blocked");
        };
        self.calls.remove(index);
        assert_eq!(
            self.disconnect(Port::principal(call.bind)),
            Some(Port::principal(call.data))
        );
        assert!(matches!(self.remove_node(call.data), RuntimeNode::Data(_)));
        let cursor = self.begin_copy_with(source, map_data);
        self.connect(Port::principal(call.bind), Port::principal(cursor));
        cursor
    }

    /// Removes a blocked bind-data pair and preserves its argument and result
    /// wires behind stable evaluator interfaces.
    pub fn take_call(&mut self, call: BlockedCall) -> CallFrame<D> {
        let Some(index) = self.calls.iter().position(|blocked| *blocked == call) else {
            panic!("taken interaction-net call must still be blocked");
        };
        self.calls.remove(index);
        assert_eq!(
            self.disconnect(Port::principal(call.bind)),
            Some(Port::principal(call.data))
        );
        let RuntimeNode::Data(callable) = self.remove_node(call.data) else {
            unreachable!();
        };
        let mut auxiliaries = self.take_auxiliaries(call.bind, 2).into_iter();
        let argument = auxiliaries.next().unwrap();
        let result = auxiliaries.next().unwrap();
        self.remove_node(call.bind);
        CallFrame {
            callable,
            argument: self.add_interface(argument),
            result: self.add_interface(result),
        }
    }

    /// Consumes an interface whose neighbor is embedded data.
    pub fn take_interface_data(&mut self, interface: Port) -> Option<D> {
        self.assert_interface(interface);
        let neighbor = self.neighbor(interface)?;
        if !neighbor.is_principal()
            || !matches!(self.node(neighbor.node()), Some(RuntimeNode::Data(_)))
        {
            return None;
        }
        self.disconnect(interface);
        self.remove_node(interface.node());
        let RuntimeNode::Data(data) = self.remove_node(neighbor.node()) else {
            unreachable!();
        };
        Some(data)
    }

    /// Replaces an evaluator interface with one embedded data node.
    pub fn complete_interface_with_data(&mut self, interface: Port, data: D) -> NodeId {
        self.assert_interface(interface);
        let target = self
            .disconnect(interface)
            .expect("completed interaction-net interface must remain wired");
        self.remove_node(interface.node());
        let node = self.add_node(RuntimeNode::Data(data));
        self.connect(Port::principal(node), target);
        node
    }

    fn advance_remote_cursor(&mut self, cursor: NodeId) -> CursorProgress {
        let RuntimeNode::RemoteCursor { copy, remote } = self
            .node(cursor)
            .expect("advanced remote cursor must exist")
            .clone()
        else {
            panic!("advanced runtime node must be a remote cursor");
        };
        let source_handle = self
            .copies
            .get(&copy)
            .expect("remote cursor must reference a live copy")
            .source
            .clone();
        let mut source = source_handle
            .inner
            .lock()
            .expect("shared runtime net was poisoned");
        let neighbor = source
            .neighbor(remote)
            .expect("remote cursor anchor must remain wired in its source");

        if self
            .copies
            .get(&copy)
            .unwrap()
            .mapped_nodes
            .contains_key(&neighbor.node())
        {
            drop(source);
            return self.join_remote_frontiers(copy, cursor, remote, neighbor);
        }

        if neighbor.is_principal() {
            let source_node = source
                .node(neighbor.node())
                .expect("remote cursor neighbor must exist")
                .clone();
            drop(source);
            return self.materialize_remote_node(
                copy,
                cursor,
                remote,
                neighbor.node(),
                source_node,
            );
        }

        let source_node = neighbor.node();
        let principal = Port::principal(source_node);
        if let Some(partner) = source
            .neighbor(principal)
            .filter(|port| port.is_principal())
        {
            let pair_is_blocked = source.calls.iter().any(|call| {
                (call.pair.left == source_node && call.pair.right == partner.node())
                    || (call.pair.right == source_node && call.pair.left == partner.node())
            }) || source.stuck.iter().any(|pair| {
                (pair.left == source_node && pair.right == partner.node())
                    || (pair.right == source_node && pair.left == partner.node())
            });
            if pair_is_blocked {
                let left = source
                    .node(source_node)
                    .expect("blocked remote pair left node must exist")
                    .clone();
                let right = source
                    .node(partner.node())
                    .expect("blocked remote pair right node must exist")
                    .clone();
                drop(source);
                return self
                    .materialize_remote_pair(copy, cursor, remote, neighbor, left, partner, right);
            }
        }

        let ready = source.ready.len();
        if ready == 0 {
            return CursorProgress::Blocked;
        }
        let mut reductions = 0;
        for _ in 0..ready {
            if source.reduce_next().is_some() {
                reductions += 1;
            }
        }
        CursorProgress::SourceSweep { reductions }
    }

    fn demand_cursor_across(&mut self, local: Port) -> Option<CursorProgress> {
        let neighbor = self.neighbor(local)?;
        if !neighbor.is_principal()
            || !matches!(
                self.node(neighbor.node()),
                Some(RuntimeNode::RemoteCursor { .. })
            )
        {
            return None;
        }
        Some(self.advance_remote_cursor(neighbor.node()))
    }

    fn cursor_source_across(&self, local: Port) -> Option<SharedRuntimeNet<D>> {
        let neighbor = self.neighbor(local)?;
        let RuntimeNode::RemoteCursor { copy, .. } = self.node(neighbor.node())? else {
            return None;
        };
        self.copies.get(copy).map(|state| state.source.clone())
    }

    fn erase_remote_cursor(&mut self, eraser: NodeId, cursor: NodeId) {
        let RuntimeNode::RemoteCursor { copy, remote } = self
            .node(cursor)
            .expect("erased remote cursor must exist")
            .clone()
        else {
            unreachable!();
        };
        self.disconnect(Port::principal(eraser));
        self.remove_node(eraser);
        self.remove_node(cursor);
        let state = self
            .copies
            .get_mut(&copy)
            .expect("erased remote cursor must reference a live copy");
        assert_eq!(state.frontiers.remove(&remote), Some(cursor));
        if state.frontiers.is_empty() {
            self.copies.remove(&copy);
        }
    }

    fn materialize_remote_node(
        &mut self,
        copy: CopyId,
        cursor: NodeId,
        remote: Port,
        source_node: NodeId,
        node: RuntimeNode<D>,
    ) -> CursorProgress {
        let mut state = self
            .copies
            .remove(&copy)
            .expect("materialized cursor must reference a live copy");
        let node = match node {
            RuntimeNode::Bind => RuntimeNode::Bind,
            RuntimeNode::Fan { identity } => RuntimeNode::Fan {
                identity: self.translate_fan_identity(&mut state, &identity),
            },
            RuntimeNode::Erase => RuntimeNode::Erase,
            RuntimeNode::Data(data) => RuntimeNode::Data((state.map_data)(&data)),
            RuntimeNode::Interface | RuntimeNode::RemoteCursor { .. } => {
                self.copies.insert(copy, state);
                return CursorProgress::Blocked;
            }
        };
        let auxiliaries = match &node {
            RuntimeNode::Bind | RuntimeNode::Fan { .. } => 2,
            RuntimeNode::Erase | RuntimeNode::Data(_) => 0,
            RuntimeNode::Interface | RuntimeNode::RemoteCursor { .. } => unreachable!(),
        };

        let local = self
            .disconnect(Port::principal(cursor))
            .expect("active remote cursor must face the local net");
        self.remove_node(cursor);
        assert_eq!(state.frontiers.remove(&remote), Some(cursor));

        let target = self.add_node(node);
        assert!(state.mapped_nodes.insert(source_node, target).is_none());
        self.connect(Port::principal(target), local);
        for index in 1..=auxiliaries {
            let source_anchor = Port::auxiliary(source_node, index);
            let next = self.add_node(RuntimeNode::RemoteCursor {
                copy,
                remote: source_anchor,
            });
            assert!(state.frontiers.insert(source_anchor, next).is_none());
            self.connect(Port::auxiliary(target, index), Port::principal(next));
        }
        self.copies.insert(copy, state);
        CursorProgress::Materialized { node: target }
    }

    fn materialize_remote_pair(
        &mut self,
        copy: CopyId,
        cursor: NodeId,
        remote: Port,
        entered: Port,
        entered_node: RuntimeNode<D>,
        partner: Port,
        partner_node: RuntimeNode<D>,
    ) -> CursorProgress {
        let mut state = self
            .copies
            .remove(&copy)
            .expect("materialized cursor pair must reference a live copy");
        let Some((entered_node, entered_auxiliaries)) =
            self.copy_remote_node(&mut state, entered_node)
        else {
            self.copies.insert(copy, state);
            return CursorProgress::Blocked;
        };
        let Some((partner_node, partner_auxiliaries)) =
            self.copy_remote_node(&mut state, partner_node)
        else {
            self.copies.insert(copy, state);
            return CursorProgress::Blocked;
        };

        let local = self
            .disconnect(Port::principal(cursor))
            .expect("active remote cursor must face the local net");
        self.remove_node(cursor);
        assert_eq!(state.frontiers.remove(&remote), Some(cursor));

        let entered_target = self.add_node(entered_node);
        let partner_target = self.add_node(partner_node);
        assert!(
            state
                .mapped_nodes
                .insert(entered.node(), entered_target)
                .is_none()
        );
        assert!(
            state
                .mapped_nodes
                .insert(partner.node(), partner_target)
                .is_none()
        );
        self.connect(
            Port::principal(entered_target),
            Port::principal(partner_target),
        );

        for index in 1..=entered_auxiliaries {
            let source_anchor = Port::auxiliary(entered.node(), index);
            if index == entered.index() {
                self.connect(Port::auxiliary(entered_target, index), local);
            } else {
                self.add_remote_frontier(
                    copy,
                    &mut state,
                    source_anchor,
                    Port::auxiliary(entered_target, index),
                );
            }
        }
        for index in 1..=partner_auxiliaries {
            self.add_remote_frontier(
                copy,
                &mut state,
                Port::auxiliary(partner.node(), index),
                Port::auxiliary(partner_target, index),
            );
        }
        self.copies.insert(copy, state);
        CursorProgress::MaterializedPair {
            left: entered_target,
            right: partner_target,
        }
    }

    fn copy_remote_node(
        &mut self,
        state: &mut CopyState<D>,
        node: RuntimeNode<D>,
    ) -> Option<(RuntimeNode<D>, u32)> {
        let node = match node {
            RuntimeNode::Bind => RuntimeNode::Bind,
            RuntimeNode::Fan { identity } => RuntimeNode::Fan {
                identity: self.translate_fan_identity(state, &identity),
            },
            RuntimeNode::Erase => RuntimeNode::Erase,
            RuntimeNode::Data(data) => RuntimeNode::Data((state.map_data)(&data)),
            RuntimeNode::Interface | RuntimeNode::RemoteCursor { .. } => return None,
        };
        let auxiliaries = match &node {
            RuntimeNode::Bind | RuntimeNode::Fan { .. } => 2,
            RuntimeNode::Erase | RuntimeNode::Data(_) => 0,
            RuntimeNode::Interface | RuntimeNode::RemoteCursor { .. } => unreachable!(),
        };
        Some((node, auxiliaries))
    }

    fn add_remote_frontier(
        &mut self,
        copy: CopyId,
        state: &mut CopyState<D>,
        source_anchor: Port,
        target: Port,
    ) {
        let cursor = self.add_node(RuntimeNode::RemoteCursor {
            copy,
            remote: source_anchor,
        });
        assert!(state.frontiers.insert(source_anchor, cursor).is_none());
        self.connect(target, Port::principal(cursor));
    }

    fn join_remote_frontiers(
        &mut self,
        copy: CopyId,
        cursor: NodeId,
        remote: Port,
        neighbor: Port,
    ) -> CursorProgress {
        let (peer, copy_finished) = {
            let state = self
                .copies
                .get_mut(&copy)
                .expect("joined cursor must reference a live copy");
            let Some(peer) = state.frontiers.remove(&neighbor) else {
                return CursorProgress::Blocked;
            };
            assert_eq!(state.frontiers.remove(&remote), Some(cursor));
            (peer, state.frontiers.is_empty())
        };
        assert_ne!(
            cursor, peer,
            "a remote wire cannot join one cursor to itself"
        );

        let left = self
            .disconnect(Port::principal(cursor))
            .expect("remote cursor must face the local net");
        let right = self
            .disconnect(Port::principal(peer))
            .expect("peer remote cursor must face the local net");
        self.remove_node(cursor);
        self.unschedule_node(peer);
        self.remove_node(peer);
        self.connect(left, right);
        if copy_finished {
            self.copies.remove(&copy);
        }
        CursorProgress::Joined
    }

    fn translate_fan_identity(
        &mut self,
        state: &mut CopyState<D>,
        identity: &FanIdentity,
    ) -> FanIdentity {
        let site = *state.fan_sites.entry(identity.site).or_insert_with(|| {
            let site = FanSite(self.next_fan_site);
            self.next_fan_site = self
                .next_fan_site
                .checked_add(1)
                .expect("interaction-net fan site space exhausted");
            site
        });
        let context = identity
            .context
            .iter()
            .map(|step| DuplicationStep {
                through: self.translate_fan_identity(state, &step.through),
                branch: step.branch,
            })
            .collect::<Vec<_>>();
        FanIdentity {
            site,
            context: Arc::from(context),
        }
    }

    fn join(&mut self, left: NodeId, right: NodeId, auxiliaries: u32) {
        self.disconnect(Port::principal(left));
        let left_neighbors = self.take_auxiliaries(left, auxiliaries);
        let right_neighbors = self.take_auxiliaries(right, auxiliaries);
        self.remove_node(left);
        self.remove_node(right);
        for (left, right) in left_neighbors.into_iter().zip(right_neighbors) {
            self.connect(left, right);
        }
    }

    fn duplicate_data(&mut self, fan: NodeId, data: NodeId) {
        self.disconnect(Port::principal(fan));
        let targets = self.take_auxiliaries(fan, 2);
        let RuntimeNode::Data(payload) = self.remove_node(data) else {
            unreachable!();
        };
        self.remove_node(fan);
        for target in targets {
            let clone = self.add_node(RuntimeNode::Data(payload.clone()));
            self.connect(Port::principal(clone), target);
        }
    }

    fn duplicate_bind(&mut self, fan: NodeId, identity: &FanIdentity, bind: NodeId) {
        self.disconnect(Port::principal(fan));
        let fan_targets = self.take_auxiliaries(fan, 2);
        let bind_targets = self.take_auxiliaries(bind, 2);
        self.remove_node(fan);
        self.remove_node(bind);

        let binds = fan_targets
            .into_iter()
            .map(|target| {
                let node = self.add_node(RuntimeNode::Bind);
                self.connect(Port::principal(node), target);
                node
            })
            .collect::<Vec<_>>();
        for (auxiliary, target) in bind_targets.into_iter().enumerate() {
            let residual = self.add_node(RuntimeNode::Fan {
                identity: identity.clone(),
            });
            self.connect(Port::principal(residual), target);
            for (branch, bind) in binds.iter().enumerate() {
                self.connect(
                    Port::auxiliary(residual, branch as u32 + 1),
                    Port::auxiliary(*bind, auxiliary as u32 + 1),
                );
            }
        }
    }

    fn commute_fans(
        &mut self,
        left: NodeId,
        left_identity: &FanIdentity,
        right: NodeId,
        right_identity: &FanIdentity,
    ) {
        self.disconnect(Port::principal(left));
        let left_targets = self.take_auxiliaries(left, 2);
        let right_targets = self.take_auxiliaries(right, 2);
        self.remove_node(left);
        self.remove_node(right);

        let right_fans = left_targets
            .into_iter()
            .enumerate()
            .map(|(branch, target)| {
                let node = self.add_node(RuntimeNode::Fan {
                    identity: right_identity.residual(left_identity, branch as u8),
                });
                self.connect(Port::principal(node), target);
                node
            })
            .collect::<Vec<_>>();
        let left_fans = right_targets
            .into_iter()
            .enumerate()
            .map(|(branch, target)| {
                let node = self.add_node(RuntimeNode::Fan {
                    identity: left_identity.residual(right_identity, branch as u8),
                });
                self.connect(Port::principal(node), target);
                node
            })
            .collect::<Vec<_>>();
        for (left_branch, right_fan) in right_fans.iter().enumerate() {
            for (right_branch, left_fan) in left_fans.iter().enumerate() {
                self.connect(
                    Port::auxiliary(*right_fan, right_branch as u32 + 1),
                    Port::auxiliary(*left_fan, left_branch as u32 + 1),
                );
            }
        }
    }

    fn erase(&mut self, eraser: NodeId, other: NodeId) {
        self.disconnect(Port::principal(eraser));
        let auxiliaries = match self.node(other).expect("erased node must exist") {
            RuntimeNode::Bind | RuntimeNode::Fan { .. } => 2,
            RuntimeNode::Erase | RuntimeNode::Data(_) => 0,
            RuntimeNode::Interface | RuntimeNode::RemoteCursor { .. } => {
                unreachable!("evaluator-only nodes are not erased as ordinary agents")
            }
        };
        let targets = self.take_auxiliaries(other, auxiliaries);
        self.remove_node(eraser);
        self.remove_node(other);
        for target in targets {
            let erase = self.add_node(RuntimeNode::Erase);
            self.connect(Port::principal(erase), target);
        }
    }

    fn take_auxiliaries(&mut self, node: NodeId, count: u32) -> Vec<Port> {
        (1..=count)
            .map(|index| {
                self.disconnect(Port::auxiliary(node, index))
                    .expect("interaction auxiliary port must be wired")
            })
            .collect()
    }

    fn add_interface(&mut self, target: Port) -> Port {
        let interface = self.add_node(RuntimeNode::Interface);
        let port = Port::auxiliary(interface, 1);
        self.connect(port, target);
        port
    }

    fn assert_interface(&self, interface: Port) {
        assert_eq!(interface.index(), 1, "interface must use its boundary port");
        assert!(matches!(
            self.node(interface.node()),
            Some(RuntimeNode::Interface)
        ));
    }

    fn add_node(&mut self, node: RuntimeNode<D>) -> NodeId {
        let id = NodeId(self.next_node_id);
        self.next_node_id = self
            .next_node_id
            .checked_add(1)
            .expect("interaction-net node ID space exhausted");
        assert!(self.nodes.insert(id, RuntimeEntry::new(node)).is_none());
        id
    }

    fn remove_node(&mut self, node: NodeId) -> RuntimeNode<D> {
        let entry = self.nodes.remove(&node).expect("removed node must exist");
        assert!(entry.links.iter().all(Option::is_none));
        entry.node
    }

    fn unschedule_node(&mut self, node: NodeId) {
        self.ready
            .retain(|pair| pair.left != node && pair.right != node);
        self.calls
            .retain(|call| call.pair.left != node && call.pair.right != node);
        self.blocked_cursors
            .retain(|cursor| cursor.pair.left != node && cursor.pair.right != node);
        self.stuck
            .retain(|pair| pair.left != node && pair.right != node);
    }

    fn neighbor(&self, port: Port) -> Option<Port> {
        let entry = self.nodes.get(&port.node())?;
        if port.index() >= entry.node.port_count() {
            return None;
        }
        entry.links[port.index() as usize]
    }

    fn disconnect(&mut self, port: Port) -> Option<Port> {
        let neighbor = self.neighbor(port)?;
        self.nodes
            .get_mut(&port.node())
            .expect("disconnected node must exist")
            .links[port.index() as usize] = None;
        self.nodes
            .get_mut(&neighbor.node())
            .expect("neighbor node must exist")
            .links[neighbor.index() as usize] = None;
        Some(neighbor)
    }

    fn connect(&mut self, left: Port, right: Port) {
        assert_ne!(left, right, "an interaction-net port cannot wire to itself");
        assert!(self.valid_port(left) && self.valid_port(right));
        assert!(self.neighbor(left).is_none() && self.neighbor(right).is_none());
        self.nodes.get_mut(&left.node()).unwrap().links[left.index() as usize] = Some(right);
        self.nodes.get_mut(&right.node()).unwrap().links[right.index() as usize] = Some(left);
        if left.is_principal() && right.is_principal() {
            self.ready.push_back(ActivePair {
                left: left.node(),
                right: right.node(),
            });
        }
    }

    fn valid_port(&self, port: Port) -> bool {
        self.nodes
            .get(&port.node())
            .is_some_and(|entry| port.index() < entry.node.port_count())
    }

    fn principals_connect(&self, pair: ActivePair) -> bool {
        self.neighbor(Port::principal(pair.left)) == Some(Port::principal(pair.right))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_reports_wiring_errors_without_panicking() {
        let mut net = NetBuilder::<()>::new();
        let [exposed, argument, result] = net.bind();
        let unwired = net.data(());
        net.try_wire(argument, result).unwrap();

        assert_eq!(
            net.try_wire(argument, exposed),
            Err(NetBuildError::PortAlreadyWired(argument))
        );
        assert_eq!(
            net.try_finish(exposed),
            Err(NetBuildError::PortUnwired(unwired))
        );
    }

    #[test]
    fn builder_rejects_a_wired_exposed_port() {
        let mut net = NetBuilder::new();
        let left = net.data(());
        let right = net.data(());
        net.try_wire(left, right).unwrap();

        assert_eq!(
            net.try_finish(left),
            Err(NetBuildError::ExposedPortWired(left))
        );
    }

    #[test]
    fn builder_rejects_ports_from_another_builder() {
        let mut net = NetBuilder::new();
        let exposed = net.data(());
        let mut other = NetBuilder::new();
        other.data(());
        let foreign = other.data(());

        assert_eq!(
            net.try_wire(exposed, foreign),
            Err(NetBuildError::InvalidPort(foreign))
        );
    }

    #[test]
    fn zero_way_copy_is_an_eraser() {
        let mut builder = NetBuilder::<()>::new();
        let copy = builder.copy(0);
        let net = builder.try_finish(copy.input).unwrap();

        assert!(copy.outputs.is_empty());
        assert_eq!(net.nodes(), &[Node::Erase]);
        assert!(net.wires().is_empty());
    }

    #[test]
    fn one_way_copy_is_normalized_out_of_the_template() {
        let mut builder = NetBuilder::new();
        let copy = builder.copy(1);
        let data = builder.data("value");
        builder.wire(copy.outputs[0], data);
        let net = builder.try_finish(copy.input).unwrap();

        assert_eq!(net.nodes(), &[Node::Data("value")]);
        assert_eq!(net.exposed(), Port::principal(NodeId::from_index(0)));
        assert!(net.wires().is_empty());
    }

    #[test]
    fn many_way_copy_builds_a_balanced_binary_fan_tree() {
        let mut builder = NetBuilder::new();
        let copy = builder.copy(5);
        for output in copy.outputs.iter().copied() {
            let data = builder.data(());
            builder.wire(output, data);
        }
        let net = builder.try_finish(copy.input).unwrap();

        assert_eq!(copy.outputs.len(), 5);
        assert_eq!(
            net.nodes()
                .iter()
                .filter(|node| matches!(node, Node::Fan { .. }))
                .count(),
            4
        );
        assert_eq!(
            net.nodes()
                .iter()
                .filter(|node| matches!(node, Node::Data(())))
                .count(),
            5
        );
    }

    fn identity(site: u64) -> FanIdentity {
        FanIdentity::root(FanSite::from_raw(site))
    }

    fn duplicated_argument_template() -> InteractionNet<()> {
        let mut net = NetBuilder::new();
        let bind = net.push(Node::Bind);
        let fan = net.push_fan();
        let left = net.push(Node::Data(()));
        let right = net.push(Node::Data(()));
        let result = net.push(Node::Data(()));
        net.wire(Port::auxiliary(bind, 1), Port::principal(fan));
        net.wire(Port::auxiliary(fan, 1), Port::principal(left));
        net.wire(Port::auxiliary(fan, 2), Port::principal(right));
        net.wire(Port::auxiliary(bind, 2), Port::principal(result));
        net.finish(Port::principal(bind))
    }

    #[test]
    fn runtime_remembers_a_stable_anchor_for_the_exposed_port() {
        let net = duplicated_argument_template();
        let runtime = net.instantiate();
        assert!(matches!(
            runtime.node(runtime.exposed().node()),
            Some(RuntimeNode::Interface)
        ));
        assert_eq!(runtime.neighbor(runtime.exposed()), Some(net.exposed()));
    }

    fn fan_pair(left: FanIdentity, right: FanIdentity) -> RuntimeNet<()> {
        let mut runtime = RuntimeNet::empty();
        let left = runtime.add_node(RuntimeNode::Fan { identity: left });
        let right = runtime.add_node(RuntimeNode::Fan { identity: right });
        let left_1 = runtime.add_node(RuntimeNode::Data(()));
        let left_2 = runtime.add_node(RuntimeNode::Data(()));
        let right_1 = runtime.add_node(RuntimeNode::Data(()));
        let right_2 = runtime.add_node(RuntimeNode::Data(()));
        runtime.connect(Port::principal(left), Port::principal(right));
        runtime.connect(Port::auxiliary(left, 1), Port::principal(left_1));
        runtime.connect(Port::auxiliary(left, 2), Port::principal(left_2));
        runtime.connect(Port::auxiliary(right, 1), Port::principal(right_1));
        runtime.connect(Port::auxiliary(right, 2), Port::principal(right_2));
        runtime
    }

    #[test]
    fn identical_fan_histories_join() {
        let fan = identity(3);
        let mut net = fan_pair(fan.clone(), fan.clone());
        let pair = ActivePair {
            left: NodeId(0),
            right: NodeId(1),
        };
        assert_eq!(
            net.reduce_next(),
            Some(Reduction {
                pair,
                kind: ReductionKind::FanJoin {
                    identity: fan.clone()
                }
            })
        );
        assert!(net.node(NodeId(0)).is_none());
        assert!(net.node(NodeId(1)).is_none());
        assert_eq!(net.active_pairs().len(), 2);
    }

    #[test]
    fn different_runtime_local_fan_sites_do_not_pair() {
        let left = identity(3);
        let right = identity(4);
        let mut net = fan_pair(left.clone(), right.clone());
        let pair = ActivePair {
            left: NodeId(0),
            right: NodeId(1),
        };
        assert_eq!(
            net.reduce_next(),
            Some(Reduction {
                pair,
                kind: ReductionKind::FanCommute {
                    left: left.clone(),
                    right: right.clone()
                }
            })
        );
        assert_eq!(net.active_pairs().len(), 4);
    }

    #[test]
    fn fan_commutation_records_dynamic_duplication_history() {
        let left = identity(3);
        let right = identity(4);
        let mut net = fan_pair(left.clone(), right.clone());
        assert!(matches!(
            net.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::FanCommute { .. },
                ..
            })
        ));
        let residuals = net
            .nodes
            .values()
            .filter_map(|entry| match &entry.node {
                RuntimeNode::Fan { identity } => Some(identity),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(residuals.len(), 4);
        assert!(residuals.iter().all(|fan| fan.context.len() == 1));
    }

    #[test]
    fn ports_and_optional_ports_are_one_word() {
        assert_eq!(std::mem::size_of::<Port>(), std::mem::size_of::<u64>());
        assert_eq!(
            std::mem::size_of::<Option<Port>>(),
            std::mem::size_of::<u64>()
        );
    }

    #[test]
    fn blocked_and_stuck_pairs_leave_the_ready_queue() {
        let mut calls = RuntimeNet::empty();
        let bind = calls.add_node(RuntimeNode::Bind);
        let data = calls.add_node(RuntimeNode::Data(()));
        calls.connect(Port::principal(bind), Port::principal(data));
        let call_pair = ActivePair {
            left: bind,
            right: data,
        };
        assert_eq!(
            calls.reduce_next(),
            Some(Reduction {
                pair: call_pair,
                kind: ReductionKind::Call { bind, data },
            })
        );
        assert!(calls.ready_pairs().is_empty());
        assert_eq!(
            calls.blocked_calls(),
            &VecDeque::from([BlockedCall {
                pair: call_pair,
                bind,
                data,
            }])
        );
        assert_eq!(calls.reduce_next(), None);

        let mut stuck = RuntimeNet::empty();
        let left = stuck.add_node(RuntimeNode::Data(()));
        let right = stuck.add_node(RuntimeNode::Data(()));
        stuck.connect(Port::principal(left), Port::principal(right));
        let stuck_pair = ActivePair { left, right };
        assert_eq!(
            stuck.reduce_next(),
            Some(Reduction {
                pair: stuck_pair,
                kind: ReductionKind::Stuck,
            })
        );
        assert!(stuck.ready_pairs().is_empty());
        assert_eq!(stuck.stuck_pairs(), &[stuck_pair]);
        assert_eq!(stuck.reduce_next(), None);
    }

    #[test]
    fn scheduler_collections_partition_principal_connections() {
        let mut net = RuntimeNet::empty();
        let bind = net.add_node(RuntimeNode::Bind);
        let call_data = net.add_node(RuntimeNode::Data(()));
        let stuck_left = net.add_node(RuntimeNode::Data(()));
        let stuck_right = net.add_node(RuntimeNode::Data(()));
        let ready_fan = net.add_node(RuntimeNode::Fan {
            identity: identity(0),
        });
        let ready_data = net.add_node(RuntimeNode::Data(()));
        net.connect(Port::principal(bind), Port::principal(call_data));
        net.connect(Port::principal(stuck_left), Port::principal(stuck_right));
        net.connect(Port::principal(ready_fan), Port::principal(ready_data));

        assert!(matches!(
            net.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::Call { .. },
                ..
            })
        ));
        assert!(matches!(
            net.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::Stuck,
                ..
            })
        ));

        let mut graph_pairs = net
            .nodes
            .keys()
            .filter_map(|node| {
                let neighbor = net.neighbor(Port::principal(*node))?;
                (neighbor.is_principal() && node.get() < neighbor.node().get())
                    .then_some((node.get(), neighbor.node().get()))
            })
            .collect::<Vec<_>>();
        graph_pairs.sort_unstable();

        let mut scheduled_pairs = net
            .active_pairs()
            .into_iter()
            .map(|pair| {
                let left = pair.left.get();
                let right = pair.right.get();
                (left.min(right), left.max(right))
            })
            .collect::<Vec<_>>();
        scheduled_pairs.sort_unstable();

        assert_eq!(scheduled_pairs, graph_pairs);
    }

    fn source_requiring_one_sweep() -> InteractionNet<&'static str> {
        let mut net = NetBuilder::new();
        let left = net.push(Node::Bind);
        let right = net.push(Node::Bind);
        let left_result = net.push(Node::Data("left-result"));
        let exposed_result = net.push(Node::Data("exposed-result"));
        let right_result = net.push(Node::Data("right-result"));
        net.wire(Port::principal(left), Port::principal(right));
        net.wire(Port::auxiliary(left, 2), Port::principal(left_result));
        net.wire(Port::auxiliary(right, 1), Port::principal(exposed_result));
        net.wire(Port::auxiliary(right, 2), Port::principal(right_result));
        net.finish(Port::auxiliary(left, 1))
    }

    fn target_waiting_on(source: SharedRuntimeNet<&'static str>) -> RuntimeNet<&'static str> {
        let mut target = RuntimeNet::empty();
        let local = target.add_node(RuntimeNode::Data("local"));
        let cursor = target.begin_copy(source);
        target.connect(Port::principal(local), Port::principal(cursor));
        target
    }

    #[test]
    fn remote_cursor_sweeps_shared_source_once_then_materializes() {
        let source = source_requiring_one_sweep().instantiate_shared();
        let mut first = target_waiting_on(source.clone());

        assert!(matches!(
            first.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::RemoteCursor {
                    progress: CursorProgress::SourceSweep { reductions: 1 },
                    ..
                },
                ..
            })
        ));
        assert!(matches!(
            first.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::RemoteCursor {
                    progress: CursorProgress::Materialized { .. },
                    ..
                },
                ..
            })
        ));
        // The sweep advances only the pairs that were ready when it began.
        // Newly exposed, unrelated pairs remain lazy in the shared source.
        assert_eq!(source.with(|runtime| runtime.ready_pairs().len()), 1);

        let mut second = target_waiting_on(source);
        assert!(matches!(
            second.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::RemoteCursor {
                    progress: CursorProgress::Materialized { .. },
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn materializing_a_root_creates_lazy_auxiliary_cursors() {
        let source = duplicated_argument_template().instantiate_shared();
        let source_nodes = source.with(|runtime| runtime.nodes.len());
        let mut target = RuntimeNet::empty();
        let local = target.add_node(RuntimeNode::Data(()));
        let cursor = target.begin_copy(source.clone());
        target.connect(Port::principal(local), Port::principal(cursor));

        assert!(matches!(
            target.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::RemoteCursor {
                    progress: CursorProgress::Materialized { .. },
                    ..
                },
                ..
            })
        ));
        let cursors = target
            .nodes
            .values()
            .filter(|entry| matches!(entry.node, RuntimeNode::RemoteCursor { .. }))
            .count();
        assert_eq!(cursors, 2);
        assert_eq!(source.with(|runtime| runtime.nodes.len()), source_nodes);
    }

    #[test]
    fn resuming_a_call_materializes_only_the_root_bind() {
        let source = duplicated_argument_template().instantiate_shared();
        let mut caller = RuntimeNet::empty();
        let bind = caller.add_node(RuntimeNode::Bind);
        let function = caller.add_node(RuntimeNode::Data(()));
        let argument = caller.add_node(RuntimeNode::Data(()));
        let result = caller.add_node(RuntimeNode::Data(()));
        caller.connect(Port::principal(bind), Port::principal(function));
        caller.connect(Port::auxiliary(bind, 1), Port::principal(argument));
        caller.connect(Port::auxiliary(bind, 2), Port::principal(result));

        let Some(Reduction {
            kind: ReductionKind::Call { .. },
            ..
        }) = caller.reduce_next()
        else {
            panic!("bind-data must block as a call");
        };
        let call = caller.blocked_calls()[0];
        caller.resume_call_with_copy(call, source);
        assert!(matches!(
            caller.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::RemoteCursor {
                    progress: CursorProgress::Materialized { .. },
                    ..
                },
                ..
            })
        ));
        assert!(matches!(
            caller.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::BindJoin,
                ..
            })
        ));
        assert_eq!(
            caller
                .nodes
                .values()
                .filter(|entry| matches!(entry.node, RuntimeNode::RemoteCursor { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn converging_frontiers_join_without_leaving_a_stale_cursor_pair() {
        let mut template = NetBuilder::new();
        let root = template.push(Node::Bind);
        template.wire(Port::auxiliary(root, 1), Port::auxiliary(root, 2));
        let source = template.finish(Port::principal(root)).instantiate_shared();

        let mut caller = RuntimeNet::empty();
        let bind = caller.add_node(RuntimeNode::Bind);
        let function = caller.add_node(RuntimeNode::Data(()));
        let left = caller.add_node(RuntimeNode::Data(()));
        let right = caller.add_node(RuntimeNode::Data(()));
        caller.connect(Port::principal(bind), Port::principal(function));
        caller.connect(Port::auxiliary(bind, 1), Port::principal(left));
        caller.connect(Port::auxiliary(bind, 2), Port::principal(right));

        caller.reduce_next();
        let call = caller.blocked_calls()[0];
        caller.resume_call_with_copy(call, source);
        caller.reduce_next();
        caller.reduce_next();
        assert!(matches!(
            caller.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::RemoteCursor {
                    progress: CursorProgress::Joined,
                    ..
                },
                ..
            })
        ));
        assert!(caller.copies.is_empty());
        assert!(matches!(
            caller.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::Stuck,
                ..
            })
        ));
        assert!(caller.reduce_next().is_none());
    }

    #[test]
    fn separate_logical_copies_rebase_fans_to_distinct_local_sites() {
        let mut template = NetBuilder::new();
        let fan = template.push_fan();
        let left = template.push(Node::Data(()));
        let right = template.push(Node::Data(()));
        template.wire(Port::auxiliary(fan, 1), Port::principal(left));
        template.wire(Port::auxiliary(fan, 2), Port::principal(right));
        let source = template.finish(Port::principal(fan)).instantiate_shared();

        let mut target = RuntimeNet::empty();
        for _ in 0..2 {
            let local = target.add_node(RuntimeNode::Data(()));
            let cursor = target.begin_copy(source.clone());
            target.connect(Port::principal(local), Port::principal(cursor));
        }
        assert!(matches!(
            target.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::RemoteCursor {
                    progress: CursorProgress::Materialized { .. },
                    ..
                },
                ..
            })
        ));
        assert!(matches!(
            target.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::RemoteCursor {
                    progress: CursorProgress::Materialized { .. },
                    ..
                },
                ..
            })
        ));
        let mut sites = target
            .nodes
            .values()
            .filter_map(|entry| match &entry.node {
                RuntimeNode::Fan { identity } => Some(identity.site.get()),
                _ => None,
            })
            .collect::<Vec<_>>();
        sites.sort_unstable();
        assert_eq!(sites, vec![0, 1]);
    }

    #[test]
    fn erasing_a_remote_cursor_does_not_materialize_its_source() {
        let source = duplicated_argument_template().instantiate_shared();
        let source_nodes = source.with(|runtime| runtime.nodes.len());
        let mut target = RuntimeNet::empty();
        let eraser = target.add_node(RuntimeNode::Erase);
        let cursor = target.begin_copy(source.clone());
        target.connect(Port::principal(eraser), Port::principal(cursor));

        assert!(matches!(
            target.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::Erase,
                ..
            })
        ));
        assert_eq!(source.with(|runtime| runtime.nodes.len()), source_nodes);
        assert!(target.copies.is_empty());
    }

    #[test]
    fn removed_node_ids_are_not_reused() {
        let mut net = RuntimeNet::empty();
        let first = net.add_node(RuntimeNode::Data(()));
        let second = net.add_node(RuntimeNode::Data(()));
        assert!(matches!(net.remove_node(first), RuntimeNode::Data(())));
        let third = net.add_node(RuntimeNode::Data(()));
        assert_eq!(first.get(), 0);
        assert_eq!(second.get(), 1);
        assert_eq!(third.get(), 2);
    }
}
