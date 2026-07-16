use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use super::model::*;

pub struct NetBuilder<S: NetSpecialization> {
    nodes: Vec<BuilderNode<S>>,
    wires: Vec<Wire>,
    next_fan_site: u64,
}

enum BuilderNode<S: NetSpecialization> {
    Runtime(Node<S>),
    /// Builder-only two-ended alias used to represent `.copy 1`. Finalization
    /// splices it out, so tunnels never enter an immutable template.
    Tunnel,
}

impl<S: NetSpecialization> BuilderNode<S> {
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

pub struct CopyPorts {
    pub input: Port,
    pub outputs: Vec<Port>,
}

/// A curried chain of bind nodes. `input` is the first principal port,
/// `arguments` contains one first auxiliary per bind in application order,
/// and `result` is the final bind's second auxiliary.
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

impl<S: NetSpecialization> Default for NetBuilder<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: NetSpecialization> NetBuilder<S> {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            wires: Vec::new(),
            next_fan_site: 0,
        }
    }

    pub fn push(&mut self, node: Node<S>) -> NodeId {
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

    pub fn data(&mut self, data: S::Data) -> Port {
        let node = self.push(Node::Data(data));
        Port::principal(node)
    }

    pub fn operator(&mut self, operator: S::Operator) -> [Port; 2] {
        let node = self.push(Node::Operator(operator));
        [Port::principal(node), Port::auxiliary(node, 1)]
    }

    /// Constructs a unary function from an ordinary bind and an operator.
    /// The returned ports are the exposed function port and its internal result
    /// port, which is already wired to the operator continuation.
    pub fn unary_operator(&mut self, operator: S::Operator) -> Port {
        let [function, argument, result] = self.bind();
        let [input, output] = self.operator(operator);
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

    pub fn finish(self, exposed: Port) -> InteractionNet<S> {
        self.try_finish(exposed)
            .expect("invalid interaction-net template")
    }

    pub fn try_finish(self, exposed: Port) -> Result<InteractionNet<S>, NetBuildError> {
        self.validate(exposed)?;
        self.normalize(exposed)
    }

    fn normalize(self, exposed: Port) -> Result<InteractionNet<S>, NetBuildError> {
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
