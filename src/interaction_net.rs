//! Generic port-and-wire interaction-net topology and reduction.
//!
//! Embedded data is supplied by the client. Immutable templates and runtime
//! nets allocate fan sites locally. Lazy copies translate source sites into
//! fresh target sites while preserving the complete residual history.

use std::collections::{BTreeMap, HashMap, HashSet};
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

/// A host-provided unary data transition.
///
/// The principal port consumes [`Node::Data`]. Its sole auxiliary port is the
/// result continuation. Returning another `HostFn` installs it behind a fresh
/// [`Node::Bind`], preserving ordinary unary function topology.
pub struct HostFn<D> {
    name: Arc<str>,
    implementation: Arc<dyn Fn(&D) -> HostFnResult<D> + Send + Sync>,
}

impl<D> HostFn<D> {
    pub fn new(
        name: impl Into<Arc<str>>,
        implementation: impl Fn(&D) -> HostFnResult<D> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            implementation: Arc::new(implementation),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn apply(&self, data: &D) -> HostFnResult<D> {
        (self.implementation)(data)
    }
}

impl<D> Clone for HostFn<D> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            implementation: self.implementation.clone(),
        }
    }
}

impl<D> fmt::Debug for HostFn<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostFn")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl<D> PartialEq for HostFn<D> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.implementation, &other.implementation)
    }
}

impl<D> Eq for HostFn<D> {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostFnYield<D> {
    Data(D),
    HostFn(HostFn<D>),
}

pub type HostFnResult<D> = Result<HostFnYield<D>, Arc<str>>;

/// The topology that callable data exposes when it meets a [`Node::Bind`].
///
/// A shared net is loaded through a lazy logical copy. A host function is
/// installed behind a fresh bind so the ordinary bind-join rule performs the
/// application.
pub enum Callable<D> {
    Net(SharedRuntimeNet<D>),
    HostFn(HostFn<D>),
}

/// Client policy for interpreting embedded data as a callable agent.
///
/// Implementations may force client-owned lazy values, so this method is
/// always invoked without holding the target runtime-net mutex. Returning an
/// error leaves the original `Data >< Bind` pair permanently stuck.
pub trait CallableData: Clone + 'static {
    type Error: fmt::Display;

    fn into_callable(self) -> Result<Callable<Self>, Self::Error>;
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
    /// Host-provided unary data transition. Ports: `[input*, result]`.
    HostFn(HostFn<D>),
}

impl<D> Node<D> {
    fn port_count(&self) -> u32 {
        match self {
            Self::Bind | Self::Fan { .. } => 3,
            Self::HostFn(_) => 2,
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
    HostFn(HostFn<D>),
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
            Self::HostFn(_) | Self::Interface => 2,
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

/// Stable key for a principal-principal wire.
///
/// Runtime node IDs are never reused, and a principal port has at most one
/// neighbor, so the lower-numbered endpoint uniquely identifies the pair. The
/// other endpoint is always recovered from the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActivePairKey(NodeId);

impl ActivePairKey {
    fn new(left: NodeId, right: NodeId) -> Self {
        Self(left.min(right))
    }

    pub fn node(self) -> NodeId {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionNet<D> {
    nodes: Arc<[Node<D>]>,              // nodes identified by index
    wires: Arc<[Wire]>,                 // all wires between ports
    exposed: Port,                      // closed net has one exposed port
    active_pairs: Arc<[ActivePairKey]>, // subset of wires connecting principal ports
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

    pub fn active_pairs(&self) -> &[ActivePairKey] {
        &self.active_pairs
    }
}

impl<D: Clone + 'static> InteractionNet<D> {
    pub fn instantiate(&self) -> RuntimeNet<D> {
        RuntimeNet::new(self)
    }

    pub fn instantiate_shared(&self) -> SharedRuntimeNet<D> {
        SharedRuntimeNet::new(self.instantiate())
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

/// A curried chain of bind nodes. `input` is the first principal port,
/// `arguments` contains one first auxiliary per bind in application order,
/// and `result` is the final bind's second auxiliary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindSpine {
    pub input: Port,
    pub arguments: Vec<Port>,
    pub result: Port,
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

    pub fn bind_spine(&mut self, arity: usize) -> BindSpine {
        assert!(arity > 0, "a bind spine must contain at least one bind");
        let binds = (0..arity).map(|_| self.bind()).collect::<Vec<_>>();
        for pair in binds.windows(2) {
            self.wire(pair[0][2], pair[1][0]);
        }
        BindSpine {
            input: binds[0][0],
            arguments: binds.iter().map(|bind| bind[1]).collect(),
            result: binds.last().unwrap()[2],
        }
    }

    pub fn data(&mut self, data: D) -> Port {
        let node = self.push(Node::Data(data));
        Port::principal(node)
    }

    pub fn host_fn(&mut self, host_fn: HostFn<D>) -> [Port; 2] {
        let node = self.push(Node::HostFn(host_fn));
        [Port::principal(node), Port::auxiliary(node, 1)]
    }

    /// Constructs a unary function from an ordinary bind and a host function.
    /// The returned ports are the exposed function port and its internal result
    /// port, which is already wired to the host function continuation.
    pub fn unary_host_fn(&mut self, host_fn: HostFn<D>) -> Port {
        let [function, argument, result] = self.bind();
        let [input, output] = self.host_fn(host_fn);
        self.wire(argument, input);
        self.wire(result, output);
        function
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
            .map(|wire| ActivePairKey::new(wire.left.node(), wire.right.node()))
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
    FanHostFn {
        identity: FanIdentity,
    },
    Erase,
    Call {
        bind: NodeId,
        data: NodeId,
    },
    HostCall {
        host_fn: NodeId,
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
pub enum CursorDependency<D> {
    LocalCursor(NodeId),
    SourceCursor {
        source: SharedRuntimeNet<D>,
        cursor: NodeId,
    },
    SourcePair {
        source: SharedRuntimeNet<D>,
        pair: ActivePairKey,
    },
}

impl<D> fmt::Debug for CursorDependency<D> {
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
pub struct HostCall {
    pub pair: ActivePairKey,
    pub host_fn: NodeId,
    pub data: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StuckReason {
    NoRule,
    HostError(Arc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckPair {
    pub pair: ActivePairKey,
    pub reason: StuckReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedCursor {
    pub pair: ActivePairKey,
    pub cursor: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActivePairState {
    Ready,
    Claimed,
    BlockedCursor { cursor: NodeId },
    Stuck(StuckReason),
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

impl<D: Clone + 'static> SharedRuntimeNet<D> {
    /// Inspects and advances a previously claimed cursor without holding target
    /// and source runtime locks at the same time.
    pub fn advance_claimed_cursor(&self, cursor: NodeId) -> Option<CursorProgress> {
        let claim = self.with(|target| target.cursor_claim(cursor))?;
        let source = claim.source.clone();
        let frontier = source.with(|runtime| runtime.inspect_source_frontier(claim.remote));
        Some(self.with_mut(|target| target.finish_cursor_claim(claim, frontier)))
    }
}

impl<D: CallableData> SharedRuntimeNet<D> {
    /// Resolves one exact claimed `Data >< Bind` pair using client callable
    /// policy. Claiming and finishing each take a short target lock; callable
    /// conversion itself runs without holding the runtime mutex.
    pub fn resolve_call(&self, call: Call) -> Result<bool, D::Error> {
        let Some(data) = self.with_mut(|runtime| runtime.claim_call(call)) else {
            return Ok(false);
        };

        match data.into_callable() {
            Ok(Callable::Net(source)) => {
                self.with_mut(|runtime| runtime.resume_claimed_call_with_copy(call, source));
                Ok(true)
            }
            Ok(Callable::HostFn(host_fn)) => {
                self.with_mut(|runtime| {
                    runtime.resume_claimed_call_with_host_fn(call, host_fn);
                });
                Ok(true)
            }
            Err(error) => {
                self.with_mut(|runtime| {
                    runtime.fail_claimed_call(call, Arc::<str>::from(error.to_string()));
                });
                Err(error)
            }
        }
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
    frontiers: HashMap<Port, NodeId>,
    fan_sites: HashMap<FanSite, FanSite>,
}

#[derive(Clone)]
struct CursorClaim<D> {
    cursor: NodeId,
    pair: Option<ActivePairKey>,
    copy: CopyId,
    remote: Port,
    source: SharedRuntimeNet<D>,
}

enum SourceFrontier<D> {
    Principal {
        port: Port,
        node: RuntimeNode<D>,
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
    copies: HashMap<CopyId, CopyState<D>>,
    cursor_dependencies: HashMap<NodeId, CursorDependency<D>>,

    // Every live principal-principal wire has exactly one authoritative state.
    // External work changes Ready to Claimed while the runtime lock is held,
    // then completes as a rewrite, a blocked cursor, or a permanent error.
    active: BTreeMap<ActivePairKey, ActivePairState>,
}

impl<D: Clone + 'static> RuntimeNet<D> {
    fn new(net: &InteractionNet<D>) -> Self {
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
                    Node::HostFn(host_fn) => RuntimeNode::HostFn(host_fn.clone()),
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

    pub fn cursor_dependency(&self, cursor: NodeId) -> Option<CursorDependency<D>> {
        self.cursor_dependencies.get(&cursor).cloned()
    }

    pub fn interface_cursor(&self, interface: Port) -> Option<NodeId> {
        self.assert_interface(interface);
        self.cursor_across(interface)
    }

    pub fn stuck_pairs(&self) -> impl Iterator<Item = StuckPair> + '_ {
        self.active.iter().filter_map(|(pair, state)| match state {
            ActivePairState::Stuck(reason) => Some(StuckPair {
                pair: *pair,
                reason: reason.clone(),
            }),
            _ => None,
        })
    }

    pub fn stuck_reason(&self, pair: ActivePairKey) -> Option<&StuckReason> {
        match self.active.get(&pair) {
            Some(ActivePairState::Stuck(reason)) => Some(reason),
            _ => None,
        }
    }

    pub fn node(&self, id: NodeId) -> Option<&RuntimeNode<D>> {
        self.nodes.get(&id).map(|entry| &entry.node)
    }

    /// Reads callable data from an active pair already claimed by reduction.
    fn claim_call(&mut self, call: Call) -> Option<D> {
        if self.active.get(&call.pair) != Some(&ActivePairState::Claimed) {
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
    fn fail_claimed_call(&mut self, call: Call, error: Arc<str>) {
        assert_eq!(
            self.active.insert(
                call.pair,
                ActivePairState::Stuck(StuckReason::HostError(error))
            ),
            Some(ActivePairState::Claimed),
            "failed call must still be claimed"
        );
    }

    /// Clones a pending host transition so the host callback can run without
    /// holding the shared runtime-net mutex.
    pub fn host_call_parts(&self, call: HostCall) -> (HostFn<D>, D) {
        assert_eq!(self.active.get(&call.pair), Some(&ActivePairState::Claimed));
        let host_fn = match self.node(call.host_fn) {
            Some(RuntimeNode::HostFn(host_fn)) => host_fn.clone(),
            _ => panic!("pending host call function must exist"),
        };
        let data = match self.node(call.data) {
            Some(RuntimeNode::Data(data)) => data.clone(),
            _ => panic!("pending host call data must exist"),
        };
        (host_fn, data)
    }

    pub fn complete_host_call(&mut self, call: HostCall, result: HostFnYield<D>) -> NodeId {
        let target = self.take_host_call(call);
        match result {
            HostFnYield::Data(data) => {
                let node = self.add_node(RuntimeNode::Data(data));
                self.connect(Port::principal(node), target);
                node
            }
            HostFnYield::HostFn(host_fn) => {
                let bind = self.add_node(RuntimeNode::Bind);
                let host = self.add_node(RuntimeNode::HostFn(host_fn));
                self.connect(Port::principal(bind), target);
                self.connect(Port::auxiliary(bind, 1), Port::principal(host));
                self.connect(Port::auxiliary(bind, 2), Port::auxiliary(host, 1));
                bind
            }
        }
    }

    pub fn fail_host_call(&mut self, call: HostCall, error: Arc<str>) {
        assert_eq!(
            self.active.insert(
                call.pair,
                ActivePairState::Stuck(StuckReason::HostError(error))
            ),
            Some(ActivePairState::Claimed),
            "failed host call must still be claimed"
        );
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
        if self.active.get(&pair) != Some(&ActivePairState::Ready) {
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
            (RuntimeNode::Fan { identity }, RuntimeNode::HostFn(_)) => {
                self.duplicate_host_fn(left_id, identity, right_id);
                ReductionKind::FanHostFn {
                    identity: identity.clone(),
                }
            }
            (RuntimeNode::HostFn(_), RuntimeNode::Fan { identity }) => {
                self.duplicate_host_fn(right_id, identity, left_id);
                ReductionKind::FanHostFn {
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
            (RuntimeNode::HostFn(_), RuntimeNode::Data(_)) => ReductionKind::HostCall {
                host_fn: left_id,
                data: right_id,
            },
            (RuntimeNode::Data(_), RuntimeNode::HostFn(_)) => ReductionKind::HostCall {
                host_fn: right_id,
                data: left_id,
            },
            (RuntimeNode::Data(_), RuntimeNode::Data(_)) => {
                *self.active.get_mut(&pair).unwrap() = ActivePairState::Stuck(StuckReason::NoRule);
                ReductionKind::Stuck
            }
            (RuntimeNode::HostFn(_), _) | (_, RuntimeNode::HostFn(_)) => {
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
                | ReductionKind::HostCall { .. }
                | ReductionKind::RemoteCursor { .. }
                | ReductionKind::Stuck
        ) {
            assert_eq!(self.active.remove(&pair), Some(ActivePairState::Claimed));
        }
        Some(Reduction { pair, kind })
    }

    /// Starts one logical copy and returns its initially unwired remote cursor.
    pub fn begin_copy(&mut self, source: SharedRuntimeNet<D>) -> NodeId {
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

    /// Completes claimed applicable lowering by loading the
    /// resulting closed net at the original application's principal port.
    fn resume_claimed_call_with_copy(&mut self, call: Call, source: SharedRuntimeNet<D>) -> NodeId {
        self.attach_call_to_copy(call, source)
    }

    fn attach_call_to_copy(&mut self, call: Call, source: SharedRuntimeNet<D>) -> NodeId {
        assert_eq!(
            self.active.remove(&call.pair),
            Some(ActivePairState::Claimed),
            "resumed interaction-net call must still be claimed"
        );
        assert_eq!(
            self.disconnect(Port::principal(call.bind)),
            Some(Port::principal(call.data))
        );
        assert!(matches!(self.remove_node(call.data), RuntimeNode::Data(_)));
        let cursor = self.begin_copy(source);
        self.connect(Port::principal(call.bind), Port::principal(cursor));
        cursor
    }

    /// Completes applicable lowering by replacing callable data with
    /// an explicit unary function net. The newly introduced Bind then joins
    /// the original application Bind through the ordinary interaction rule.
    fn resume_claimed_call_with_host_fn(&mut self, call: Call, host_fn: HostFn<D>) -> NodeId {
        assert_eq!(
            self.active.remove(&call.pair),
            Some(ActivePairState::Claimed),
            "lowered host call must still be claimed"
        );
        assert_eq!(
            self.disconnect(Port::principal(call.bind)),
            Some(Port::principal(call.data))
        );
        assert!(matches!(self.remove_node(call.data), RuntimeNode::Data(_)));

        let function = self.add_node(RuntimeNode::Bind);
        let host = self.add_node(RuntimeNode::HostFn(host_fn));
        self.connect(Port::principal(call.bind), Port::principal(function));
        self.connect(Port::auxiliary(function, 1), Port::principal(host));
        self.connect(Port::auxiliary(function, 2), Port::auxiliary(host, 1));
        function
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

    fn take_host_call(&mut self, call: HostCall) -> Port {
        self.remove_pending_host_call(call);
        assert_eq!(
            self.disconnect(Port::principal(call.host_fn)),
            Some(Port::principal(call.data))
        );
        let target = self
            .disconnect(Port::auxiliary(call.host_fn, 1))
            .expect("host function result must remain wired");
        assert!(matches!(
            self.remove_node(call.host_fn),
            RuntimeNode::HostFn(_)
        ));
        assert!(matches!(self.remove_node(call.data), RuntimeNode::Data(_)));
        target
    }

    fn remove_pending_host_call(&mut self, call: HostCall) {
        assert_eq!(
            self.active.remove(&call.pair),
            Some(ActivePairState::Claimed),
            "completed host call must still be pending"
        );
    }

    fn begin_cursor_claim(
        &mut self,
        cursor: NodeId,
        expected_pair: Option<ActivePairKey>,
    ) -> Option<CursorProgress> {
        let pair = expected_pair.or_else(|| self.active_pair_key(cursor));
        if let Some(expected) = expected_pair {
            assert_eq!(pair, Some(expected));
        }
        if let Some(pair) = pair {
            match self.active.get_mut(&pair) {
                Some(state @ ActivePairState::Ready)
                | Some(state @ ActivePairState::BlockedCursor { .. }) => {
                    *state = ActivePairState::Claimed;
                }
                Some(ActivePairState::Claimed) if expected_pair == Some(pair) => {}
                _ => return None,
            }
        }
        self.cursor_dependencies.remove(&cursor);
        Some(CursorProgress::Claimed)
    }

    fn cursor_claim(&self, cursor: NodeId) -> Option<CursorClaim<D>> {
        let pair = self.active_pair_key(cursor);
        if pair.is_some_and(|pair| self.active.get(&pair) != Some(&ActivePairState::Claimed)) {
            return None;
        }
        let RuntimeNode::RemoteCursor { copy, remote } = self.node(cursor)?.clone() else {
            return None;
        };
        let source = self.copies.get(&copy)?.source.clone();
        Some(CursorClaim {
            cursor,
            pair,
            copy,
            remote,
            source,
        })
    }

    fn inspect_source_frontier(&self, remote: Port) -> SourceFrontier<D> {
        let port = self
            .neighbor(remote)
            .expect("remote cursor anchor must remain wired in its source");
        if port.is_principal() {
            let node = self
                .node(port.node())
                .expect("remote cursor neighbor must exist")
                .clone();
            return SourceFrontier::Principal { port, node };
        }

        let principal_neighbor = self.neighbor(Port::principal(port.node()));
        if let Some(partner) = principal_neighbor.filter(|neighbor| neighbor.is_principal()) {
            return SourceFrontier::ActiveAuxiliary {
                entered: port,
                partner,
            };
        }
        let mut principal_anchors = Vec::new();
        let mut terminal_pair = None;
        let mut node = port.node();
        let mut visited = HashSet::new();
        while visited.insert(node) {
            let Some(neighbor) = self.neighbor(Port::principal(node)) else {
                break;
            };
            if neighbor.is_principal() {
                terminal_pair = Some(ActivePairKey::new(node, neighbor.node()));
                break;
            }
            principal_anchors.push(neighbor);
            node = neighbor.node();
        }
        SourceFrontier::StableAuxiliary {
            port,
            principal_anchors,
            terminal_pair,
        }
    }

    fn finish_cursor_claim(
        &mut self,
        claim: CursorClaim<D>,
        frontier: SourceFrontier<D>,
    ) -> CursorProgress {
        if let Some(pair) = claim.pair {
            assert_eq!(self.active.get(&pair), Some(&ActivePairState::Claimed));
        }
        assert!(matches!(
            self.node(claim.cursor),
            Some(RuntimeNode::RemoteCursor { copy, remote })
                if *copy == claim.copy && *remote == claim.remote
        ));
        let frontier_port = match &frontier {
            SourceFrontier::Principal { port, .. }
            | SourceFrontier::StableAuxiliary { port, .. } => *port,
            SourceFrontier::ActiveAuxiliary { entered, .. } => *entered,
        };

        let converging_cursor = self
            .copies
            .get(&claim.copy)
            .expect("claimed cursor must reference a live copy")
            .frontiers
            .get(&frontier_port)
            .copied();
        let progress = if let Some(peer) = converging_cursor {
            assert_ne!(peer, claim.cursor, "a frontier cannot converge with itself");
            assert!(matches!(
                self.node(peer),
                Some(RuntimeNode::RemoteCursor { copy, remote })
                    if *copy == claim.copy && *remote == frontier_port
            ));
            self.join_remote_frontiers(claim.copy, claim.cursor, claim.remote, frontier_port)
        } else {
            match frontier {
                SourceFrontier::Principal {
                    port,
                    node: RuntimeNode::RemoteCursor { .. },
                } => {
                    self.cursor_dependencies.insert(
                        claim.cursor,
                        CursorDependency::SourceCursor {
                            source: claim.source,
                            cursor: port.node(),
                        },
                    );
                    CursorProgress::Blocked
                }
                SourceFrontier::Principal { port, node } => self.materialize_remote_node(
                    claim.copy,
                    claim.cursor,
                    claim.remote,
                    port.node(),
                    node,
                ),
                SourceFrontier::StableAuxiliary {
                    principal_anchors,
                    terminal_pair,
                    ..
                } => {
                    let peer = self.copies.get(&claim.copy).and_then(|state| {
                        principal_anchors
                            .iter()
                            .find_map(|anchor| state.frontiers.get(anchor).copied())
                    });
                    if let Some(peer) = peer {
                        assert_ne!(peer, claim.cursor);
                        self.cursor_dependencies
                            .insert(claim.cursor, CursorDependency::LocalCursor(peer));
                    } else if let Some(pair) = terminal_pair {
                        self.cursor_dependencies.insert(
                            claim.cursor,
                            CursorDependency::SourcePair {
                                source: claim.source,
                                pair,
                            },
                        );
                    }
                    CursorProgress::Blocked
                }
                SourceFrontier::ActiveAuxiliary { entered, partner } => {
                    self.cursor_dependencies.insert(
                        claim.cursor,
                        CursorDependency::SourcePair {
                            source: claim.source,
                            pair: ActivePairKey::new(entered.node(), partner.node()),
                        },
                    );
                    CursorProgress::Blocked
                }
            }
        };

        if progress == CursorProgress::Blocked {
            if let Some(pair) = claim.pair {
                *self.active.get_mut(&pair).unwrap() = ActivePairState::BlockedCursor {
                    cursor: claim.cursor,
                };
            }
        } else if let Some(pair) = claim.pair {
            if self.active.get(&pair) == Some(&ActivePairState::Claimed) {
                self.active.remove(&pair);
            }
        }
        progress
    }

    fn cursor_across(&self, local: Port) -> Option<NodeId> {
        let neighbor = self.neighbor(local)?;
        if !neighbor.is_principal()
            || !matches!(
                self.node(neighbor.node()),
                Some(RuntimeNode::RemoteCursor { .. })
            )
        {
            return None;
        }
        Some(neighbor.node())
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
            RuntimeNode::Data(data) => RuntimeNode::Data(data.clone()),
            RuntimeNode::HostFn(host_fn) => RuntimeNode::HostFn(host_fn),
            RuntimeNode::Interface | RuntimeNode::RemoteCursor { .. } => {
                self.copies.insert(copy, state);
                return CursorProgress::Blocked;
            }
        };
        let auxiliaries = match &node {
            RuntimeNode::Bind | RuntimeNode::Fan { .. } => 2,
            RuntimeNode::HostFn(_) => 1,
            RuntimeNode::Erase | RuntimeNode::Data(_) => 0,
            RuntimeNode::Interface | RuntimeNode::RemoteCursor { .. } => unreachable!(),
        };

        let local = self
            .disconnect(Port::principal(cursor))
            .expect("active remote cursor must face the local net");
        self.remove_node(cursor);
        assert_eq!(state.frontiers.remove(&remote), Some(cursor));

        let target = self.add_node(node);
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

    fn join_remote_frontiers(
        &mut self,
        copy: CopyId,
        cursor: NodeId,
        remote: Port,
        neighbor: Port,
    ) -> CursorProgress {
        let peer = {
            let state = self
                .copies
                .get(&copy)
                .expect("joined cursor must reference a live copy");
            let Some(peer) = state.frontiers.get(&neighbor).copied() else {
                return CursorProgress::Blocked;
            };
            peer
        };
        // A converging frontier may be inspected concurrently from its other
        // end. Leave both frontier records intact until that active-pair claim
        // is released.
        if self
            .active_pair_key(peer)
            .is_some_and(|pair| self.active.get(&pair) == Some(&ActivePairState::Claimed))
        {
            return CursorProgress::Blocked;
        }
        let copy_finished = {
            let state = self
                .copies
                .get_mut(&copy)
                .expect("joined cursor must reference a live copy");
            assert_eq!(state.frontiers.remove(&neighbor), Some(peer));
            assert_eq!(state.frontiers.remove(&remote), Some(cursor));
            state.frontiers.is_empty()
        };
        assert_ne!(
            cursor, peer,
            "a remote wire cannot join one cursor to itself"
        );

        let left = self
            .disconnect(Port::principal(cursor))
            .expect("remote cursor must face the local net");
        self.unschedule_node(peer);
        let right = self
            .disconnect(Port::principal(peer))
            .expect("peer remote cursor must face the local net");
        self.remove_node(cursor);
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

    fn duplicate_host_fn(&mut self, fan: NodeId, identity: &FanIdentity, host_fn: NodeId) {
        self.disconnect(Port::principal(fan));
        let fan_targets = self.take_auxiliaries(fan, 2);
        let [result] = <[Port; 1]>::try_from(self.take_auxiliaries(host_fn, 1)).unwrap();
        let RuntimeNode::HostFn(host_fn) = self.remove_node(host_fn) else {
            unreachable!();
        };
        self.remove_node(fan);

        let hosts = fan_targets
            .into_iter()
            .map(|target| {
                let node = self.add_node(RuntimeNode::HostFn(host_fn.clone()));
                self.connect(Port::principal(node), target);
                node
            })
            .collect::<Vec<_>>();
        let residual = self.add_node(RuntimeNode::Fan {
            identity: identity.clone(),
        });
        self.connect(Port::principal(residual), result);
        for (branch, host) in hosts.into_iter().enumerate() {
            self.connect(
                Port::auxiliary(residual, branch as u32 + 1),
                Port::auxiliary(host, 1),
            );
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
            RuntimeNode::HostFn(_) => 1,
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
        self.cursor_dependencies.remove(&node);
        let entry = self.nodes.remove(&node).expect("removed node must exist");
        assert!(entry.links.iter().all(Option::is_none));
        entry.node
    }

    fn unschedule_node(&mut self, node: NodeId) {
        let Some(pair) = self.active_pair_key(node) else {
            return;
        };
        self.active.remove(&pair);
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
            let pair = ActivePairKey::new(left.node(), right.node());
            match self.active.entry(pair) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(ActivePairState::Ready);
                }
                std::collections::btree_map::Entry::Occupied(mut entry)
                    if entry.get() == &ActivePairState::Claimed =>
                {
                    *entry.get_mut() = ActivePairState::Ready;
                }
                std::collections::btree_map::Entry::Occupied(_) => {
                    panic!("active pair must be new")
                }
            }
        }
    }

    fn valid_port(&self, port: Port) -> bool {
        self.nodes
            .get(&port.node())
            .is_some_and(|entry| port.index() < entry.node.port_count())
    }

    fn active_pair_key(&self, node: NodeId) -> Option<ActivePairKey> {
        let neighbor = self.neighbor(Port::principal(node))?;
        neighbor
            .is_principal()
            .then(|| ActivePairKey::new(node, neighbor.node()))
    }

    fn pair_nodes(&self, pair: ActivePairKey) -> Option<(NodeId, NodeId)> {
        let left = pair.node();
        let right = self.neighbor(Port::principal(left))?;
        if !right.is_principal() || left >= right.node() {
            return None;
        }
        Some((left, right.node()))
    }

    #[cfg(test)]
    fn principals_connect(&self, pair: ActivePairKey) -> bool {
        self.pair_nodes(pair).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish_claimed_cursor<D: Clone + 'static>(
        target: &mut RuntimeNet<D>,
        cursor: NodeId,
    ) -> CursorProgress {
        let claim = target
            .cursor_claim(cursor)
            .expect("cursor reduction should leave an inspectable claim");
        let frontier = claim
            .source
            .with(|source| source.inspect_source_frontier(claim.remote));
        target.finish_cursor_claim(claim, frontier)
    }

    fn reduce_next_cursor<D: Clone + 'static>(
        target: &mut RuntimeNet<D>,
    ) -> (NodeId, CursorProgress) {
        let Some(Reduction {
            kind:
                ReductionKind::RemoteCursor {
                    cursor,
                    progress: CursorProgress::Claimed,
                },
            ..
        }) = target.reduce_next()
        else {
            panic!("next reduction should claim a remote cursor");
        };
        let progress = finish_claimed_cursor(target, cursor);
        (cursor, progress)
    }

    fn reduce_pair_cursor<D: Clone + 'static>(
        target: &mut RuntimeNet<D>,
        pair: ActivePairKey,
    ) -> (NodeId, CursorProgress) {
        let Some(Reduction {
            kind:
                ReductionKind::RemoteCursor {
                    cursor,
                    progress: CursorProgress::Claimed,
                },
            ..
        }) = target.reduce_pair(pair)
        else {
            panic!("exact reduction should claim a remote cursor");
        };
        let progress = finish_claimed_cursor(target, cursor);
        (cursor, progress)
    }

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
    fn bind_spine_builds_one_curried_chain() {
        let mut builder = NetBuilder::new();
        let spine = builder.bind_spine(3);
        let function = builder.data(());
        builder.wire(spine.input, function);
        for argument in spine.arguments {
            let data = builder.data(());
            builder.wire(argument, data);
        }
        let net = builder.finish(spine.result);

        assert_eq!(
            net.nodes()
                .iter()
                .filter(|node| matches!(node, Node::Bind))
                .count(),
            3
        );
        assert_eq!(net.active_pairs().len(), 1);
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
        let pair = ActivePairKey::new(NodeId(0), NodeId(1));
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
        let pair = ActivePairKey::new(NodeId(0), NodeId(1));
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
    fn active_pair_key_is_the_lower_node_id_and_recovers_its_partner() {
        let mut net = RuntimeNet::empty();
        let lower = net.add_node(RuntimeNode::Data(()));
        let higher = net.add_node(RuntimeNode::Data(()));
        net.connect(Port::principal(higher), Port::principal(lower));

        let key = *net.ready_pairs().first().unwrap();
        assert_eq!(key.node(), lower);
        assert_eq!(net.pair_nodes(key), Some((lower, higher)));
    }

    #[test]
    fn claimed_and_stuck_pairs_remain_in_the_active_tree() {
        let mut calls = RuntimeNet::empty();
        let bind = calls.add_node(RuntimeNode::Bind);
        let data = calls.add_node(RuntimeNode::Data(()));
        calls.connect(Port::principal(bind), Port::principal(data));
        let call_pair = ActivePairKey::new(bind, data);
        assert_eq!(
            calls.reduce_next(),
            Some(Reduction {
                pair: call_pair,
                kind: ReductionKind::Call { bind, data },
            })
        );
        assert!(calls.ready_pairs().is_empty());
        assert_eq!(
            calls.active.get(&call_pair),
            Some(&ActivePairState::Claimed)
        );
        assert_eq!(calls.reduce_next(), None);

        let mut stuck = RuntimeNet::empty();
        let left = stuck.add_node(RuntimeNode::Data(()));
        let right = stuck.add_node(RuntimeNode::Data(()));
        stuck.connect(Port::principal(left), Port::principal(right));
        let stuck_pair = ActivePairKey::new(left, right);
        assert_eq!(
            stuck.reduce_next(),
            Some(Reduction {
                pair: stuck_pair,
                kind: ReductionKind::Stuck,
            })
        );
        assert!(stuck.ready_pairs().is_empty());
        assert_eq!(
            stuck.stuck_pairs().collect::<Vec<_>>(),
            vec![StuckPair {
                pair: stuck_pair,
                reason: StuckReason::NoRule,
            }]
        );
        assert_eq!(stuck.reduce_next(), None);
    }

    #[test]
    fn claimed_callable_data_lowers_to_an_explicit_host_function_bind() {
        let mut net = RuntimeNet::empty();
        let application = net.add_node(RuntimeNode::Bind);
        let callable = net.add_node(RuntimeNode::Data(0));
        let argument = net.add_node(RuntimeNode::Data(41));
        let interface = net.add_node(RuntimeNode::Interface);
        let result = Port::auxiliary(interface, 1);
        net.connect(Port::principal(application), Port::principal(callable));
        net.connect(Port::auxiliary(application, 1), Port::principal(argument));
        net.connect(Port::auxiliary(application, 2), result);

        let reduction = net.reduce_next().expect("bind-data must block as a call");
        let ReductionKind::Call { bind, data } = reduction.kind else {
            panic!("expected a claimed call");
        };
        let call = Call {
            pair: reduction.pair,
            bind,
            data,
        };
        assert_eq!(net.claim_call(call), Some(0));
        assert_eq!(net.active.get(&call.pair), Some(&ActivePairState::Claimed));

        net.resume_claimed_call_with_host_fn(
            call,
            HostFn::new("increment", |value| Ok(HostFnYield::Data(value + 1))),
        );
        assert_ne!(net.active.get(&call.pair), Some(&ActivePairState::Claimed));
        assert!(matches!(
            net.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::BindJoin,
                ..
            })
        ));
        let host_call = match net.reduce_next() {
            Some(Reduction {
                kind: ReductionKind::HostCall { host_fn, data },
                pair,
            }) => HostCall {
                pair,
                host_fn,
                data,
            },
            other => panic!("expected host call, got {other:?}"),
        };
        let (host_fn, data) = net.host_call_parts(host_call);
        net.complete_host_call(host_call, host_fn.apply(&data).unwrap());
        assert_eq!(net.interface_data(result), Some(&42));
    }

    fn host_call_net(host_fn: HostFn<i32>, input: i32) -> (RuntimeNet<i32>, HostCall, Port) {
        let mut net = RuntimeNet::empty();
        let host = net.add_node(RuntimeNode::HostFn(host_fn));
        let data = net.add_node(RuntimeNode::Data(input));
        let interface = net.add_node(RuntimeNode::Interface);
        let result = Port::auxiliary(interface, 1);
        net.connect(Port::principal(host), Port::principal(data));
        net.connect(Port::auxiliary(host, 1), result);
        let pair = ActivePairKey::new(host, data);
        assert!(matches!(
            net.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::HostCall { .. },
                ..
            })
        ));
        (
            net,
            HostCall {
                pair,
                host_fn: host,
                data,
            },
            result,
        )
    }

    #[test]
    fn host_fn_consumes_data_and_emits_data() {
        let (mut net, call, result) = host_call_net(
            HostFn::new("increment", |value| Ok(HostFnYield::Data(value + 1))),
            41,
        );
        let (host_fn, data) = net.host_call_parts(call);
        let outcome = host_fn.apply(&data).unwrap();

        net.complete_host_call(call, outcome);

        assert_eq!(net.interface_data(result), Some(&42));
        assert!(!net.active.contains_key(&call.pair));
    }

    #[test]
    fn returned_host_fn_is_wrapped_as_a_unary_function() {
        let next = HostFn::new("increment", |value| Ok(HostFnYield::Data(value + 1)));
        let (mut net, call, result) = host_call_net(
            HostFn::new("curry", move |_| Ok(HostFnYield::HostFn(next.clone()))),
            0,
        );
        let (host_fn, data) = net.host_call_parts(call);
        let outcome = host_fn.apply(&data).unwrap();

        let bind = net.complete_host_call(call, outcome);

        assert_eq!(net.interface_neighbor(result), Some(Port::principal(bind)));
        let host = net.port_neighbor(Port::auxiliary(bind, 1)).unwrap();
        assert!(matches!(
            net.node(host.node()),
            Some(RuntimeNode::HostFn(_))
        ));
        assert_eq!(
            net.port_neighbor(Port::auxiliary(bind, 2)),
            Some(Port::auxiliary(host.node(), 1))
        );
    }

    #[test]
    fn host_fn_error_preserves_the_stuck_active_pair() {
        let (mut failed, call, _) = host_call_net(
            HostFn::new("failed", |_| Err(Arc::from("invalid input"))),
            0,
        );
        let (host_fn, data) = failed.host_call_parts(call);
        let Err(error) = host_fn.apply(&data) else {
            panic!("host function should fail");
        };
        failed.fail_host_call(call, error);
        assert!(matches!(
            failed.active.get(&call.pair),
            Some(ActivePairState::Stuck(_))
        ));
        assert_eq!(
            failed.stuck_pairs().collect::<Vec<_>>(),
            vec![StuckPair {
                pair: call.pair,
                reason: StuckReason::HostError(Arc::from("invalid input")),
            }]
        );
        assert!(failed.principals_connect(call.pair));
    }

    #[test]
    fn active_tree_tracks_every_principal_connection_once() {
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
                let (left, right) = net.pair_nodes(pair).unwrap();
                (left.get(), right.get())
            })
            .collect::<Vec<_>>();
        scheduled_pairs.sort_unstable();

        assert_eq!(scheduled_pairs, graph_pairs);
    }

    fn source_requiring_one_reduction() -> InteractionNet<&'static str> {
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
    fn remote_cursor_exposes_source_progress_without_holding_nested_locks() {
        let source = source_requiring_one_reduction().instantiate_shared();
        let mut first = target_waiting_on(source.clone());

        let (_, progress) = reduce_next_cursor(&mut first);
        assert_eq!(progress, CursorProgress::Blocked);
        source.with_mut(|runtime| {
            assert!(matches!(
                runtime.reduce_next(),
                Some(Reduction {
                    kind: ReductionKind::BindJoin,
                    ..
                })
            ));
        });
        let cursor = first
            .blocked_cursors()
            .values()
            .next()
            .expect("cursor should remain blocked")
            .cursor;
        assert!(first.retry_blocked_cursor(cursor));
        assert!(matches!(
            reduce_next_cursor(&mut first).1,
            CursorProgress::Materialized { .. }
        ));
        // Driving demand advances only one source reduction. Newly exposed,
        // unrelated pairs remain lazy in the shared source.
        assert_eq!(source.with(|runtime| runtime.ready_pairs().len()), 1);

        let mut second = target_waiting_on(source);
        assert!(matches!(
            reduce_next_cursor(&mut second).1,
            CursorProgress::Materialized { .. }
        ));
    }

    #[test]
    fn active_source_call_is_a_dependency_and_is_never_copied() {
        let mut source: RuntimeNet<&'static str> = RuntimeNet::empty();
        let bind = source.add_node(RuntimeNode::Bind);
        let callable = source.add_node(RuntimeNode::Data("callable"));
        let result = source.add_node(RuntimeNode::Data("result"));
        source.connect(Port::principal(bind), Port::principal(callable));
        source.connect(Port::auxiliary(bind, 2), Port::principal(result));
        let exposed = source.add_interface(Port::auxiliary(bind, 1));
        source.exposed = Some(exposed);
        let pair = ActivePairKey::new(bind, callable);
        assert!(matches!(
            source.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::Call { .. },
                ..
            })
        ));
        let source = SharedRuntimeNet::new(source);

        let mut target = target_waiting_on(source.clone());
        let (cursor, progress) = reduce_next_cursor(&mut target);
        assert_eq!(progress, CursorProgress::Blocked);
        let dependency = target.cursor_dependency(cursor).unwrap();
        let CursorDependency::SourcePair {
            source: dependency_source,
            pair: dependency_pair,
        } = dependency
        else {
            panic!("active source call should remain an exact source dependency");
        };
        assert!(dependency_source.ptr_eq(&source));
        assert_eq!(dependency_pair, pair);
        source.with(|source| {
            assert_eq!(source.active.get(&pair), Some(&ActivePairState::Claimed));
        });
        assert!(
            !target
                .nodes
                .values()
                .any(|entry| matches!(entry.node, RuntimeNode::Bind))
        );
    }

    #[test]
    fn layered_cursor_reports_and_follows_an_exact_dependency() {
        let mut leaf = NetBuilder::new();
        let data = leaf.data("leaf");
        let leaf = leaf.finish(data).instantiate_shared();

        let mut middle = RuntimeNet::empty();
        let middle_cursor = middle.begin_copy(leaf);
        let exposed = middle.add_interface(Port::principal(middle_cursor));
        middle.exposed = Some(exposed);
        let middle = SharedRuntimeNet::new(middle);

        let mut outer = target_waiting_on(middle.clone());
        let (outer_cursor, progress) = reduce_next_cursor(&mut outer);
        assert_eq!(progress, CursorProgress::Blocked);
        let dependency = outer
            .cursor_dependency(outer_cursor)
            .expect("layered cursor should retain an exact dependency");
        let CursorDependency::SourceCursor { source, cursor } = dependency else {
            panic!("layered cursor should point to its exact source cursor");
        };
        assert!(source.ptr_eq(&middle));
        assert_eq!(cursor, middle_cursor);

        assert!(matches!(
            middle.with_mut(|runtime| runtime.claim_dependent_cursor(middle_cursor)),
            Some(CursorProgress::Claimed)
        ));
        assert!(matches!(
            middle.advance_claimed_cursor(middle_cursor),
            Some(CursorProgress::Materialized { .. })
        ));
        assert!(outer.retry_blocked_cursor(outer_cursor));
        assert!(matches!(
            reduce_next_cursor(&mut outer).1,
            CursorProgress::Materialized { .. }
        ));
    }

    #[test]
    fn auxiliary_cursor_drives_the_local_cursor_facing_the_principal() {
        let mut source: RuntimeNet<&'static str> = RuntimeNet::empty();
        let root = source.add_node(RuntimeNode::Bind);
        let host = source.add_node(RuntimeNode::HostFn(HostFn::new("identity", |data| {
            Ok(HostFnYield::Data(*data))
        })));
        let exposed = source.add_interface(Port::principal(root));
        source.connect(Port::auxiliary(root, 1), Port::principal(host));
        source.connect(Port::auxiliary(root, 2), Port::auxiliary(host, 1));
        source.exposed = Some(exposed);
        let source = SharedRuntimeNet::new(source);

        let mut target = RuntimeNet::empty();
        let root_cursor = target.begin_copy(source);
        let target_exposed = target.add_interface(Port::principal(root_cursor));
        assert_eq!(
            target.demand_interface(target_exposed),
            Some(CursorProgress::Claimed)
        );
        assert!(matches!(
            finish_claimed_cursor(&mut target, root_cursor),
            CursorProgress::Materialized { .. }
        ));

        let state = target.copies.values().next().unwrap();
        let argument_cursor = state.frontiers[&Port::auxiliary(root, 1)];
        let result_cursor = state.frontiers[&Port::auxiliary(root, 2)];
        assert_eq!(
            target.claim_dependent_cursor(result_cursor),
            Some(CursorProgress::Claimed)
        );
        assert_eq!(
            finish_claimed_cursor(&mut target, result_cursor),
            CursorProgress::Blocked
        );
        assert!(matches!(
            target.cursor_dependency(result_cursor),
            Some(CursorDependency::LocalCursor(cursor)) if cursor == argument_cursor
        ));
        assert_eq!(
            target.claim_dependent_cursor(argument_cursor),
            Some(CursorProgress::Claimed)
        );
        assert!(matches!(
            finish_claimed_cursor(&mut target, argument_cursor),
            CursorProgress::Materialized { .. }
        ));
        assert_eq!(
            target.claim_dependent_cursor(result_cursor),
            Some(CursorProgress::Claimed)
        );
        assert_eq!(
            finish_claimed_cursor(&mut target, result_cursor),
            CursorProgress::Joined
        );
    }

    #[test]
    fn auxiliary_cursor_traces_a_principal_chain_to_an_exact_source_pair() {
        let mut source: RuntimeNet<&'static str> = RuntimeNet::empty();
        let root = source.add_node(RuntimeNode::Bind);
        let middle = source.add_node(RuntimeNode::Bind);
        let upstream = source.add_node(RuntimeNode::Bind);
        let callable = source.add_node(RuntimeNode::Data("callable"));
        source.connect(Port::auxiliary(root, 2), Port::auxiliary(middle, 2));
        source.connect(Port::principal(middle), Port::auxiliary(upstream, 2));
        source.connect(Port::principal(upstream), Port::principal(callable));
        let exposed = source.add_interface(Port::principal(root));
        source.exposed = Some(exposed);
        let pair = ActivePairKey::new(upstream, callable);
        assert!(matches!(
            source.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::Call { .. },
                ..
            })
        ));
        let source = SharedRuntimeNet::new(source);

        let mut target = RuntimeNet::empty();
        let root_cursor = target.begin_copy(source.clone());
        let target_exposed = target.add_interface(Port::principal(root_cursor));
        assert_eq!(
            target.demand_interface(target_exposed),
            Some(CursorProgress::Claimed)
        );
        assert!(matches!(
            finish_claimed_cursor(&mut target, root_cursor),
            CursorProgress::Materialized { .. }
        ));

        let cursor = target.copies.values().next().unwrap().frontiers[&Port::auxiliary(root, 2)];
        assert_eq!(
            target.claim_dependent_cursor(cursor),
            Some(CursorProgress::Claimed)
        );
        assert_eq!(
            finish_claimed_cursor(&mut target, cursor),
            CursorProgress::Blocked
        );
        assert!(matches!(
            target.cursor_dependency(cursor),
            Some(CursorDependency::SourcePair {
                source: dependency_source,
                pair: dependency_pair,
            }) if dependency_source.ptr_eq(&source) && dependency_pair == pair
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
            reduce_next_cursor(&mut target).1,
            CursorProgress::Materialized { .. }
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
            pair,
            kind: ReductionKind::Call { bind, data },
        }) = caller.reduce_next()
        else {
            panic!("bind-data must block as a call");
        };
        let call = Call { pair, bind, data };
        caller.resume_claimed_call_with_copy(call, source);
        assert!(matches!(
            reduce_next_cursor(&mut caller).1,
            CursorProgress::Materialized { .. }
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

        let Some(Reduction {
            pair,
            kind: ReductionKind::Call { bind, data },
        }) = caller.reduce_next()
        else {
            panic!("bind-data must become a call");
        };
        let call = Call { pair, bind, data };
        caller.resume_claimed_call_with_copy(call, source);
        assert!(matches!(
            reduce_next_cursor(&mut caller).1,
            CursorProgress::Materialized { .. }
        ));
        caller.reduce_next();
        assert!(matches!(
            reduce_next_cursor(&mut caller).1,
            CursorProgress::Joined
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
    fn converging_frontier_waits_for_a_claimed_peer() {
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

        let Some(Reduction {
            pair,
            kind: ReductionKind::Call { bind, data },
        }) = caller.reduce_next()
        else {
            panic!("bind-data must become a call");
        };
        let call = Call { pair, bind, data };
        caller.resume_claimed_call_with_copy(call, source);
        assert!(matches!(
            reduce_next_cursor(&mut caller).1,
            CursorProgress::Materialized { .. }
        ));
        assert!(matches!(
            caller.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::BindJoin,
                ..
            })
        ));

        let mut claims = Vec::new();
        for _ in 0..2 {
            let Some(Reduction {
                kind:
                    ReductionKind::RemoteCursor {
                        cursor,
                        progress: CursorProgress::Claimed,
                    },
                ..
            }) = caller.reduce_next()
            else {
                panic!("each converging cursor should be independently claimable");
            };
            let claim = caller.cursor_claim(cursor).unwrap();
            let frontier = claim
                .source
                .with(|source| source.inspect_source_frontier(claim.remote));
            claims.push((claim, frontier));
        }

        let (first_claim, first_frontier) = claims.remove(0);
        assert_eq!(
            caller.finish_cursor_claim(first_claim, first_frontier),
            CursorProgress::Blocked
        );
        let (second_claim, second_frontier) = claims.remove(0);
        assert_eq!(
            caller.finish_cursor_claim(second_claim, second_frontier),
            CursorProgress::Joined
        );
        assert!(caller.copies.is_empty());
        assert!(caller.blocked_cursors().is_empty());
        assert!(
            caller
                .active
                .values()
                .all(|state| state != &ActivePairState::Claimed)
        );
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
        let mut cursor_pairs = Vec::new();
        for _ in 0..2 {
            let local = target.add_node(RuntimeNode::Data(()));
            let cursor = target.begin_copy(source.clone());
            target.connect(Port::principal(local), Port::principal(cursor));
            cursor_pairs.push(ActivePairKey::new(local, cursor));
        }
        for pair in cursor_pairs {
            assert!(matches!(
                reduce_pair_cursor(&mut target, pair).1,
                CursorProgress::Materialized { .. }
            ));
        }
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
    fn erasing_a_remote_cursor_materializes_then_uses_ordinary_erasure() {
        let source = duplicated_argument_template().instantiate_shared();
        let source_nodes = source.with(|runtime| runtime.nodes.len());
        let mut target = RuntimeNet::empty();
        let eraser = target.add_node(RuntimeNode::Erase);
        let cursor = target.begin_copy(source.clone());
        target.connect(Port::principal(eraser), Port::principal(cursor));

        assert!(matches!(
            reduce_next_cursor(&mut target).1,
            CursorProgress::Materialized { .. }
        ));
        assert!(matches!(
            target.reduce_next(),
            Some(Reduction {
                kind: ReductionKind::Erase,
                ..
            })
        ));
        assert_eq!(source.with(|runtime| runtime.nodes.len()), source_nodes);
        assert!(!target.copies.is_empty());
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
