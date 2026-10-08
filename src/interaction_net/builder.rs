use crate::trusted_hash::{TrustedHashMap, TrustedHashSet};
use std::fmt;
use std::sync::Arc;

use super::model::*;
use super::polarity::{PolarityViolation, Shape, TemplateChecks, Topology, solved_sign};

pub struct NetBuilder<S: NetSpecialization> {
    nodes: Vec<BuilderNode<S>>,
    wires: Vec<Wire>,
    /// Both ends of every wire, so checking a port is constant time rather
    /// than a scan of the wires.
    wired_ports: TrustedHashSet<Port>,
    next_fan_site: u64,
    /// Every finished template is checked for polarity and connectivity. A
    /// test may opt out to model a deliberately malformed net.
    #[cfg(test)]
    checks: TemplateChecks,
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
/// `arguments` contains one argument port per bind in application order, and
/// `result` is the final bind's result port.
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
    /// The wiring is not polarized, or a node is unreachable from the exposed
    /// port.
    Polarity(PolarityViolation),
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
            Self::Polarity(violation) => violation.fmt(formatter),
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
            wired_ports: TrustedHashSet::default(),
            next_fan_site: 0,
            #[cfg(test)]
            checks: TemplateChecks::ALL,
        }
    }

    /// Exempts this template from the test-build template checks, for a test
    /// that builds an unpolarized net on purpose.
    #[cfg(test)]
    pub fn unpolarized_for_test(mut self) -> Self {
        self.checks.signs = false;
        self.checks.connected = false;
        self
    }

    /// Exempts this template from the test-build connectivity check only, for
    /// a test that models disconnected work, which reduction can leave behind.
    #[cfg(test)]
    pub fn disconnected_for_test(mut self) -> Self {
        self.checks.connected = false;
        self
    }

    pub fn push(&mut self, node: Node<S>) -> NodeId {
        let id = NodeId::from_index(self.nodes.len());
        self.nodes.push(BuilderNode::Runtime(node));
        id
    }

    /// Returns a bind's `[principal, auxiliary 1, auxiliary 2]` ports. In the
    /// application role these are `[application, argument, result]`.
    pub fn bind(&mut self) -> [Port; 3] {
        let node = self.push(Node::Bind);
        [
            Port::principal(node),
            Port::auxiliary(node, 1),
            Port::auxiliary(node, 2),
        ]
    }

    /// Returns a function-role bind as `[function, argument, result]`.
    ///
    /// Binds join crossed, so a function lists its auxiliaries as
    /// `[result, argument]`, opposite to an application. This accessor names
    /// the ports by role.
    pub fn function_bind(&mut self) -> [Port; 3] {
        let [function, result, argument] = self.bind();
        [function, argument, result]
    }

    /// Builds the application spine `f a1 .. an`: `input` meets the
    /// function, and each bind supplies one argument.
    pub fn application_spine(&mut self, arity: usize) -> BindSpine {
        self.spine(arity, Self::bind)
    }

    /// Builds the function spine `\x1 .. xn -> body`: `input` is the
    /// function, `arguments` are its variables, and `result` is its body.
    pub fn function_spine(&mut self, arity: usize) -> BindSpine {
        self.spine(arity, Self::function_bind)
    }

    fn spine(&mut self, arity: usize, bind: fn(&mut Self) -> [Port; 3]) -> BindSpine {
        assert!(arity > 0, "a bind spine must contain at least one bind");
        let binds = (0..arity).map(|_| bind(self)).collect::<Vec<_>>();
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
        let [function, argument, result] = self.function_bind();
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
        self.wired_ports.insert(left);
        self.wired_ports.insert(right);
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
        let signs = self.check_polarity(exposed)?;
        self.normalize(exposed, signs.as_deref())
    }

    /// The number of nodes constructed so far, including builder tunnels.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Checks the builder's own topology before tunnels are spliced out, so
    /// a violation names the ports the caller constructed.
    fn check_polarity(&self, exposed: Port) -> Result<Option<Vec<Sign>>, NetBuildError> {
        #[cfg(test)]
        let checks = self.checks;
        #[cfg(not(test))]
        let checks = TemplateChecks::ALL;
        let shapes = self
            .nodes
            .iter()
            .map(|node| match node {
                BuilderNode::Runtime(node) => Shape::of(node),
                BuilderNode::Tunnel => Shape::Tunnel,
            })
            .collect::<Vec<_>>();
        Topology {
            shapes: &shapes,
            wires: &self.wires,
            exposed,
        }
        .check(checks)
        .map_err(NetBuildError::Polarity)
    }

    /// Splices out tunnels and renumbers nodes. With solved `signs`, each
    /// wire is stored provider-first, so instantiation can type every link.
    fn normalize(
        self,
        exposed: Port,
        signs: Option<&[Sign]>,
    ) -> Result<InteractionNet<S>, NetBuildError> {
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
            .collect::<TrustedHashMap<_, _>>();

        let mut runtime_nodes = Vec::with_capacity(self.nodes.len() - tunnel_count);
        let mut node_map = vec![None; self.nodes.len()];
        for (old_index, node) in self.nodes.into_iter().enumerate() {
            if let BuilderNode::Runtime(node) = node {
                let new = NodeId::from_index(runtime_nodes.len());
                node_map[old_index] = Some(new);
                runtime_nodes.push(node);
            }
        }

        let mut visited_tunnels = TrustedHashSet::default();
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
                // Each wire is visited from both ends; keep it once, from its
                // providing end. Without signs, the orientation is arbitrary.
                let keep = match signs {
                    Some(signs) => solved_sign(signs, old) == Sign::Provides,
                    None => local < remote,
                };
                if keep {
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

        Ok(InteractionNet {
            nodes: Arc::from(runtime_nodes),
            wires: Arc::from(runtime_wires),
            exposed: exposed_runtime,
            #[cfg(test)]
            polarized: signs.is_some(),
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
        self.wired_ports.contains(&port)
    }
}

fn follow_tunnels(
    mut port: Port,
    exposed: Port,
    links: &TrustedHashMap<Port, Port>,
    is_tunnel: &[bool],
    visited_tunnels: &mut TrustedHashSet<NodeId>,
) -> Result<Port, NetBuildError> {
    let mut path = TrustedHashSet::default();
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
