//! Generic port-and-wire interaction-net topology and reduction.
//!
//! Embedded data is supplied by the client. Immutable templates use local fan
//! sites; instantiation allocates one process-global namespace for the whole
//! graph. Runtime fan identities carry duplication history behind an explicit
//! oracle boundary, the direct-history stepping stone toward Lamping's local
//! bracket/croissant oracle.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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
pub struct FanSite(u32);

impl FanSite {
    pub fn get(self) -> u32 {
        self.0
    }

    #[cfg(test)]
    const fn from_raw(site: u32) -> Self {
        Self(site)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstanceId(u64);

impl InstanceId {
    fn fresh() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);

        let id = NEXT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("interaction-net instance ID space exhausted");
        Self(id)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    const fn from_raw(id: u64) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DuplicationStep {
    pub through: FanIdentity,
    pub branch: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FanIdentity {
    pub instance: InstanceId,
    pub site: FanSite,
    pub context: Arc<[DuplicationStep]>,
}

impl FanIdentity {
    fn root(instance: InstanceId, site: FanSite) -> Self {
        Self {
            instance,
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
            instance: self.instance,
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
    Fan { identity: FanIdentity },
    Erase,
    Data(D),
}

impl<D> RuntimeNode<D> {
    fn port_count(&self) -> u32 {
        match self {
            Self::Bind | Self::Fan { .. } => 3,
            Self::Erase | Self::Data(_) => 1,
        }
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

impl<D: Clone> InteractionNet<D> {
    pub fn instantiate(&self) -> RuntimeNet<D> {
        RuntimeNet::new(self, InstanceId::fresh())
    }
}

/// Checked construction of a reusable net template.
pub struct NetBuilder<D> {
    nodes: Vec<Node<D>>,
    wires: Vec<Wire>,
    next_fan_site: u32,
}

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
        self.nodes.push(node);
        id
    }

    pub fn push_fan(&mut self) -> NodeId {
        let site = FanSite(self.next_fan_site);
        self.next_fan_site = self
            .next_fan_site
            .checked_add(1)
            .expect("too many fan sites in one interaction-net template");
        self.push(Node::Fan { site })
    }

    pub fn wire(&mut self, left: Port, right: Port) {
        self.wires.push(Wire { left, right });
    }

    pub fn finish(self, exposed: Port) -> InteractionNet<D> {
        self.validate(exposed);
        let active_pairs = self
            .wires
            .iter()
            .filter(|wire| wire.left.is_principal() && wire.right.is_principal())
            .map(|wire| ActivePair {
                left: wire.left.node(),
                right: wire.right.node(),
            })
            .collect::<Vec<_>>();
        InteractionNet {
            nodes: Arc::from(self.nodes),
            wires: Arc::from(self.wires),
            exposed,
            active_pairs: Arc::from(active_pairs),
        }
    }

    fn validate(&self, exposed: Port) {
        let mut wired = self
            .nodes
            .iter()
            .map(|node| vec![false; node.port_count() as usize])
            .collect::<Vec<_>>();
        for wire in &self.wires {
            for port in [wire.left, wire.right] {
                assert_ne!(port, exposed, "the exposed port must remain unwired");
                let Some(slot) = wired
                    .get_mut(port.node().index())
                    .and_then(|ports| ports.get_mut(port.index() as usize))
                else {
                    panic!("wire references an invalid interaction-net port");
                };
                assert!(!*slot, "interaction-net port was wired more than once");
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
                assert!(
                    *is_wired || port == exposed,
                    "interaction-net port is unwired"
                );
            }
        }
    }
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
    Stuck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedCall {
    pub pair: ActivePair,
    pub bind: NodeId,
    pub data: NodeId,
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
    instance: InstanceId,
    next_node_id: u64,
    nodes: HashMap<NodeId, RuntimeEntry<D>>,
    ready: VecDeque<ActivePair>,
    calls: VecDeque<BlockedCall>,
    stuck: Vec<ActivePair>,
}

impl<D: Clone> RuntimeNet<D> {
    fn new(net: &InteractionNet<D>, instance: InstanceId) -> Self {
        let nodes = net
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let id = NodeId::from_index(index);
                let node = match node {
                    Node::Bind => RuntimeNode::Bind,
                    Node::Fan { site } => RuntimeNode::Fan {
                        identity: FanIdentity::root(instance, *site),
                    },
                    Node::Erase => RuntimeNode::Erase,
                    Node::Data(data) => RuntimeNode::Data(data.clone()),
                };
                (id, RuntimeEntry::new(node))
            })
            .collect();
        let mut runtime = Self {
            instance,
            next_node_id: u64::try_from(net.nodes.len())
                .expect("interaction-net node count does not fit in u64"),
            nodes,
            ready: VecDeque::new(),
            calls: VecDeque::new(),
            stuck: Vec::new(),
        };
        for wire in net.wires.iter() {
            runtime.connect(wire.left, wire.right);
        }
        runtime
    }

    #[cfg(test)]
    fn empty(instance: InstanceId) -> Self {
        Self {
            instance,
            next_node_id: 0,
            nodes: HashMap::new(),
            ready: VecDeque::new(),
            calls: VecDeque::new(),
            stuck: Vec::new(),
        }
    }

    pub fn instance(&self) -> InstanceId {
        self.instance
    }

    pub fn active_pairs(&self) -> Vec<ActivePair> {
        self.ready
            .iter()
            .copied()
            .chain(self.calls.iter().map(|call| call.pair))
            .chain(self.stuck.iter().copied())
            .collect()
    }

    pub fn ready_pairs(&self) -> &VecDeque<ActivePair> {
        &self.ready
    }

    pub fn blocked_calls(&self) -> &VecDeque<BlockedCall> {
        &self.calls
    }

    pub fn stuck_pairs(&self) -> &[ActivePair] {
        &self.stuck
    }

    pub fn node(&self, id: NodeId) -> Option<&RuntimeNode<D>> {
        self.nodes.get(&id).map(|entry| &entry.node)
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
        };
        Some(Reduction { pair, kind })
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(instance: u64, site: u32) -> FanIdentity {
        FanIdentity::root(InstanceId::from_raw(instance), FanSite::from_raw(site))
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
    fn instantiation_freshens_one_namespace_not_every_fan_site() {
        let net = duplicated_argument_template();
        let first = net.instantiate();
        let second = net.instantiate();
        assert_ne!(first.instance(), second.instance());
        let fan = |runtime: &RuntimeNet<()>| {
            runtime
                .nodes
                .values()
                .find_map(|entry| match &entry.node {
                    RuntimeNode::Fan { identity } => Some(identity.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let first_fan = fan(&first);
        let second_fan = fan(&second);
        assert_eq!(first_fan.site, second_fan.site);
        assert_ne!(first_fan.instance, second_fan.instance);
    }

    fn fan_pair(left: FanIdentity, right: FanIdentity) -> RuntimeNet<()> {
        let instance = left.instance;
        let mut runtime = RuntimeNet::empty(instance);
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
        let fan = identity(7, 3);
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
    fn equal_template_sites_from_different_instances_do_not_pair() {
        let left = identity(7, 3);
        let right = identity(8, 3);
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
        let left = identity(7, 3);
        let right = identity(7, 4);
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
        let mut calls = RuntimeNet::empty(InstanceId::from_raw(1));
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

        let mut stuck = RuntimeNet::empty(InstanceId::from_raw(2));
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
        let mut net = RuntimeNet::empty(InstanceId::from_raw(1));
        let bind = net.add_node(RuntimeNode::Bind);
        let call_data = net.add_node(RuntimeNode::Data(()));
        let stuck_left = net.add_node(RuntimeNode::Data(()));
        let stuck_right = net.add_node(RuntimeNode::Data(()));
        let ready_fan = net.add_node(RuntimeNode::Fan {
            identity: identity(1, 0),
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

    #[test]
    fn removed_node_ids_are_not_reused() {
        let mut net = RuntimeNet::empty(InstanceId::from_raw(1));
        let first = net.add_node(RuntimeNode::Data(()));
        let second = net.add_node(RuntimeNode::Data(()));
        assert!(matches!(net.remove_node(first), RuntimeNode::Data(())));
        let third = net.add_node(RuntimeNode::Data(()));
        assert_eq!(first.get(), 0);
        assert_eq!(second.get(), 1);
        assert_eq!(third.get(), 2);
    }
}
