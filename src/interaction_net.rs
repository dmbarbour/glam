//! Generic port-and-wire interaction-net topology and reduction.
//!
//! Embedded data is supplied by the client. Immutable templates use local fan
//! sites; instantiation allocates one process-global namespace for the whole
//! graph. Runtime fan identities carry duplication history behind an explicit
//! oracle boundary, the direct-history stepping stone toward Lamping's local
//! bracket/croissant oracle.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub type NodeId = usize;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Port {
    pub node: NodeId,
    pub index: u32,
}

impl Port {
    pub const fn principal(node: NodeId) -> Self {
        Self { node, index: 0 }
    }

    pub const fn auxiliary(node: NodeId, index: u32) -> Self {
        Self { node, index }
    }

    pub const fn is_principal(self) -> bool {
        self.index == 0
    }
}

/// Immutable nodes in a reusable interaction-net template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node<D> {
    /// Ports: `[ap*, arg, result]`.
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
    pub left: Port,
    pub right: Port,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivePair {
    pub left: NodeId,
    pub right: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionNet<D> {
    nodes: Arc<[Node<D>]>,
    wires: Arc<[Wire]>,
    exposed: Port,
    active_pairs: Arc<[ActivePair]>,
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
        let id = self.nodes.len();
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
                left: wire.left.node,
                right: wire.right.node,
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
                    .get_mut(port.node)
                    .and_then(|ports| ports.get_mut(port.index as usize))
                else {
                    panic!("wire references an invalid interaction-net port");
                };
                assert!(!*slot, "interaction-net port was wired more than once");
                *slot = true;
            }
        }
        for (node_id, ports) in wired.iter().enumerate() {
            for (index, is_wired) in ports.iter().enumerate() {
                let port = Port {
                    node: node_id,
                    index: index as u32,
                };
                assert!(
                    *is_wired || port == exposed,
                    "interaction-net port is unwired"
                );
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanRelationship {
    Paired,
    Independent,
}

/// The oracle is deliberately explicit: fan pairing is not raw label equality.
pub trait FanOracle {
    fn relationship(&self, left: &FanIdentity, right: &FanIdentity) -> FanRelationship;
}

/// A direct representation of duplication history. This avoids global fan
/// relabelling now and provides the semantic reference for a future local
/// bracket/croissant implementation of the same oracle.
#[derive(Debug, Default, Clone, Copy)]
pub struct HistoryOracle;

impl FanOracle for HistoryOracle {
    fn relationship(&self, left: &FanIdentity, right: &FanIdentity) -> FanRelationship {
        if left == right {
            FanRelationship::Paired
        } else {
            FanRelationship::Independent
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reduction {
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
    Call,
    Stuck,
}

pub struct RuntimeNet<D> {
    instance: InstanceId,
    nodes: Vec<Option<RuntimeNode<D>>>,
    links: Vec<Vec<Option<Port>>>,
    active: VecDeque<ActivePair>,
}

impl<D: Clone> RuntimeNet<D> {
    fn new(net: &InteractionNet<D>, instance: InstanceId) -> Self {
        let nodes = net
            .nodes
            .iter()
            .map(|node| {
                Some(match node {
                    Node::Bind => RuntimeNode::Bind,
                    Node::Fan { site } => RuntimeNode::Fan {
                        identity: FanIdentity::root(instance, *site),
                    },
                    Node::Erase => RuntimeNode::Erase,
                    Node::Data(data) => RuntimeNode::Data(data.clone()),
                })
            })
            .collect::<Vec<_>>();
        let mut runtime = Self {
            instance,
            links: nodes
                .iter()
                .map(|node| vec![None; node.as_ref().unwrap().port_count() as usize])
                .collect(),
            nodes,
            active: VecDeque::new(),
        };
        for wire in net.wires.iter() {
            runtime.connect(wire.left, wire.right);
        }
        runtime
    }

    pub fn instance(&self) -> InstanceId {
        self.instance
    }

    pub fn active_pairs(&self) -> Vec<ActivePair> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(left, node)| {
                node.as_ref()?;
                let right = self.neighbor(Port::principal(left))?;
                (right.is_principal() && left < right.node).then_some(ActivePair {
                    left,
                    right: right.node,
                })
            })
            .collect()
    }

    pub fn node(&self, id: NodeId) -> Option<&RuntimeNode<D>> {
        self.nodes.get(id).and_then(Option::as_ref)
    }

    pub fn reduce_next(&mut self) -> Option<Reduction> {
        self.reduce_next_with(&HistoryOracle)
    }

    pub fn reduce_next_with(&mut self, oracle: &impl FanOracle) -> Option<Reduction> {
        while let Some(pair) = self.active.pop_front() {
            let left_port = Port::principal(pair.left);
            let right_port = Port::principal(pair.right);
            if self.neighbor(left_port) != Some(right_port) {
                continue;
            }
            let left = self.node(pair.left)?.clone();
            let right = self.node(pair.right)?.clone();
            return Some(match (&left, &right) {
                (RuntimeNode::Bind, RuntimeNode::Bind) => {
                    self.join(pair.left, pair.right, 2);
                    Reduction::BindJoin
                }
                (RuntimeNode::Fan { identity: left }, RuntimeNode::Fan { identity: right }) => {
                    match oracle.relationship(left, right) {
                        FanRelationship::Paired => {
                            self.join(pair.left, pair.right, 2);
                            Reduction::FanJoin {
                                identity: left.clone(),
                            }
                        }
                        FanRelationship::Independent => {
                            self.commute_fans(pair.left, left, pair.right, right);
                            Reduction::FanCommute {
                                left: left.clone(),
                                right: right.clone(),
                            }
                        }
                    }
                }
                (RuntimeNode::Fan { identity }, RuntimeNode::Data(_)) => {
                    self.duplicate_data(pair.left, pair.right);
                    Reduction::FanData {
                        identity: identity.clone(),
                    }
                }
                (RuntimeNode::Data(_), RuntimeNode::Fan { identity }) => {
                    self.duplicate_data(pair.right, pair.left);
                    Reduction::FanData {
                        identity: identity.clone(),
                    }
                }
                (RuntimeNode::Fan { identity }, RuntimeNode::Bind) => {
                    self.duplicate_bind(pair.left, identity, pair.right);
                    Reduction::FanBind {
                        identity: identity.clone(),
                    }
                }
                (RuntimeNode::Bind, RuntimeNode::Fan { identity }) => {
                    self.duplicate_bind(pair.right, identity, pair.left);
                    Reduction::FanBind {
                        identity: identity.clone(),
                    }
                }
                (RuntimeNode::Erase, _) => {
                    self.erase(pair.left, pair.right);
                    Reduction::Erase
                }
                (_, RuntimeNode::Erase) => {
                    self.erase(pair.right, pair.left);
                    Reduction::Erase
                }
                (RuntimeNode::Bind, RuntimeNode::Data(_))
                | (RuntimeNode::Data(_), RuntimeNode::Bind) => Reduction::Call,
                (RuntimeNode::Data(_), RuntimeNode::Data(_)) => Reduction::Stuck,
            });
        }
        None
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
        let RuntimeNode::Data(payload) = self.nodes[data].take().expect("data node must exist")
        else {
            unreachable!();
        };
        self.links[data].clear();
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
        let id = self.nodes.len();
        self.links.push(vec![None; node.port_count() as usize]);
        self.nodes.push(Some(node));
        id
    }

    fn remove_node(&mut self, node: NodeId) {
        self.nodes[node] = None;
        self.links[node].clear();
    }

    fn neighbor(&self, port: Port) -> Option<Port> {
        self.links
            .get(port.node)?
            .get(port.index as usize)
            .copied()
            .flatten()
    }

    fn disconnect(&mut self, port: Port) -> Option<Port> {
        let neighbor = self.links[port.node][port.index as usize].take()?;
        self.links[neighbor.node][neighbor.index as usize] = None;
        Some(neighbor)
    }

    fn connect(&mut self, left: Port, right: Port) {
        assert!(self.neighbor(left).is_none() && self.neighbor(right).is_none());
        self.links[left.node][left.index as usize] = Some(right);
        self.links[right.node][right.index as usize] = Some(left);
        if left.is_principal() && right.is_principal() {
            self.active.push_back(ActivePair {
                left: left.node,
                right: right.node,
            });
        }
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
                .iter()
                .find_map(|node| match node.as_ref()? {
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
        let nodes = vec![
            Some(RuntimeNode::Fan { identity: left }),
            Some(RuntimeNode::Fan { identity: right }),
            Some(RuntimeNode::Data(())),
            Some(RuntimeNode::Data(())),
            Some(RuntimeNode::Data(())),
            Some(RuntimeNode::Data(())),
        ];
        let mut runtime = RuntimeNet {
            instance,
            links: nodes
                .iter()
                .map(|node| vec![None; node.as_ref().unwrap().port_count() as usize])
                .collect(),
            nodes,
            active: VecDeque::new(),
        };
        runtime.connect(Port::principal(0), Port::principal(1));
        runtime.connect(Port::auxiliary(0, 1), Port::principal(2));
        runtime.connect(Port::auxiliary(0, 2), Port::principal(3));
        runtime.connect(Port::auxiliary(1, 1), Port::principal(4));
        runtime.connect(Port::auxiliary(1, 2), Port::principal(5));
        runtime
    }

    #[test]
    fn oracle_pairs_identical_fan_histories() {
        let fan = identity(7, 3);
        let mut net = fan_pair(fan.clone(), fan.clone());
        assert_eq!(
            net.reduce_next(),
            Some(Reduction::FanJoin {
                identity: fan.clone()
            })
        );
        assert!(net.node(0).is_none());
        assert!(net.node(1).is_none());
        assert_eq!(net.active_pairs().len(), 2);
    }

    #[test]
    fn equal_template_sites_from_different_instances_do_not_pair() {
        let left = identity(7, 3);
        let right = identity(8, 3);
        let mut net = fan_pair(left.clone(), right.clone());
        assert_eq!(
            net.reduce_next(),
            Some(Reduction::FanCommute {
                left: left.clone(),
                right: right.clone()
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
            Some(Reduction::FanCommute { .. })
        ));
        let residuals = net
            .nodes
            .iter()
            .filter_map(|node| match node.as_ref()? {
                RuntimeNode::Fan { identity } => Some(identity),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(residuals.len(), 4);
        assert!(residuals.iter().all(|fan| fan.context.len() == 1));
    }
}
