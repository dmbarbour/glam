//! Port polarity for interaction-net templates.
//!
//! Every port carries a sign: `+` provides a value and `−` consumes one. Each
//! wire joins a `+` port to a `−` port, and a template's exposed port is `+`
//! by fiat, like `Data`. Signs are a construction contract, not runtime state:
//! nodes store none and rewrite rules ignore them. See
//! `docs/agent_context/interaction_nets.md` for the table this checker
//! implements.
//!
//! The checker solves sign equations with a union-find over ports that records
//! each port's parity relative to its root, so it runs in near-linear time and
//! allocates only those arrays. A conflict is an odd cycle of constraints. It
//! also requires every node to be reachable from the exposed port: a
//! constructed component that nothing can observe is miswired. Reduction may
//! later leave such garbage, so this applies to templates only.

use std::fmt;

use super::model::{InteractionNet, NetSpecialization, Node, NodeId, Port};

/// Why a template is not polarized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PolarityViolation {
    /// A rule contradicts signs that earlier rules already forced.
    Conflict(PolarityRule),
    /// A node cannot be reached from the exposed port.
    Disconnected(NodeId),
}

/// One sign equation, reported as the rule that closed an odd cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolarityRule {
    /// Each wire joins opposite signs.
    Wire(Port, Port),
    /// The exposed port provides the template's value.
    Exposed(Port),
    /// A node's own port signs, from the polarity table.
    Node(Port, NodeRule),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeRule {
    DataProvides,
    OperatorConsumesInput,
    OperatorProvidesResult,
    BindFirstAuxiliaryConsumes,
    BindSecondAuxiliaryProvides,
    FanPrincipalOpposesBranches,
    FanBranchesAgree,
}

impl fmt::Display for PolarityViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict(rule) => write!(formatter, "net polarity conflict at {rule}"),
            Self::Disconnected(node) => write!(
                formatter,
                "net node {} is disconnected from the exposed port",
                node.get()
            ),
        }
    }
}

impl fmt::Display for PolarityRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wire(left, right) => write!(
                formatter,
                "the wire {left:?} -- {right:?}, which must join `+` to `−`"
            ),
            Self::Exposed(port) => {
                write!(formatter, "the exposed port {port:?}, which must be `+`")
            }
            Self::Node(port, rule) => write!(formatter, "{port:?}: {rule}"),
        }
    }
}

impl fmt::Display for NodeRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DataProvides => "data provides `+`",
            Self::OperatorConsumesInput => "an operator's input consumes `−`",
            Self::OperatorProvidesResult => "an operator's result provides `+`",
            Self::BindFirstAuxiliaryConsumes => "a bind's first auxiliary consumes `−`",
            Self::BindSecondAuxiliaryProvides => "a bind's second auxiliary provides `+`",
            Self::FanPrincipalOpposesBranches => "a fan's principal opposes its branches",
            Self::FanBranchesAgree => "a fan's branches share one sign",
        })
    }
}

/// Checks that a constructed template is polarized and that every node is
/// connected to its exposed port.
pub(crate) fn check_template<S: NetSpecialization>(
    net: &InteractionNet<S>,
) -> Result<(), PolarityViolation> {
    check_signs(net)?;
    check_connected(net)
}

/// Checks only that `net` is polarized. Disconnected components are allowed,
/// as reduction may leave them.
pub(crate) fn check_signs<S: NetSpecialization>(
    net: &InteractionNet<S>,
) -> Result<(), PolarityViolation> {
    let mut signs = Signs::new(net.nodes.len());
    for (index, node) in net.nodes.iter().enumerate() {
        let id = NodeId::from_index(index);
        let principal = Port::principal(id);
        match node {
            Node::Data(_) => signs.fix(principal, Sign::Provides, NodeRule::DataProvides)?,
            Node::Operator(_) => {
                signs.fix(principal, Sign::Consumes, NodeRule::OperatorConsumesInput)?;
                signs.fix(
                    Port::auxiliary(id, 1),
                    Sign::Provides,
                    NodeRule::OperatorProvidesResult,
                )?;
            }
            // A bind's principal is free: its sign is the bind's role.
            Node::Bind => {
                signs.fix(
                    Port::auxiliary(id, 1),
                    Sign::Consumes,
                    NodeRule::BindFirstAuxiliaryConsumes,
                )?;
                signs.fix(
                    Port::auxiliary(id, 2),
                    Sign::Provides,
                    NodeRule::BindSecondAuxiliaryProvides,
                )?;
            }
            Node::Fan { .. } => {
                let left = Port::auxiliary(id, 1);
                signs.relate(
                    principal,
                    left,
                    true,
                    PolarityRule::Node(principal, NodeRule::FanPrincipalOpposesBranches),
                )?;
                signs.relate(
                    left,
                    Port::auxiliary(id, 2),
                    false,
                    PolarityRule::Node(left, NodeRule::FanBranchesAgree),
                )?;
            }
            // An eraser's sign is free: it discards a value, or stands for an
            // error value where one is consumed.
            Node::Erase => {}
        }
    }
    for wire in net.wires.iter() {
        signs.relate(
            wire.left,
            wire.right,
            true,
            PolarityRule::Wire(wire.left, wire.right),
        )?;
    }
    signs.relate_to_anchor(
        net.exposed,
        Sign::Provides,
        PolarityRule::Exposed(net.exposed),
    )
}

/// Checks that every node can be reached from the exposed port.
pub(crate) fn check_connected<S: NetSpecialization>(
    net: &InteractionNet<S>,
) -> Result<(), PolarityViolation> {
    let mut neighbors = vec![Vec::new(); net.nodes.len()];
    for wire in net.wires.iter() {
        neighbors[wire.left.node().index()].push(wire.right.node());
        neighbors[wire.right.node().index()].push(wire.left.node());
    }
    let mut reached = vec![false; net.nodes.len()];
    let mut pending = vec![net.exposed.node()];
    reached[net.exposed.node().index()] = true;
    while let Some(node) = pending.pop() {
        for &neighbor in &neighbors[node.index()] {
            if !reached[neighbor.index()] {
                reached[neighbor.index()] = true;
                pending.push(neighbor);
            }
        }
    }
    match reached.iter().position(|reached| !reached) {
        Some(index) => Err(PolarityViolation::Disconnected(NodeId::from_index(index))),
        None => Ok(()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sign {
    Provides,
    Consumes,
}

/// Union-find over port signs. Each entry records whether its sign differs
/// from its parent's; the extra final entry is a constant `+` anchor.
struct Signs {
    parent: Vec<usize>,
    flipped: Vec<bool>,
}

impl Signs {
    fn new(nodes: usize) -> Self {
        let count = nodes * 3 + 1;
        Self {
            parent: (0..count).collect(),
            flipped: vec![false; count],
        }
    }

    fn anchor(&self) -> usize {
        self.parent.len() - 1
    }

    fn slot(port: Port) -> usize {
        port.node().index() * 3 + port.index() as usize
    }

    /// Returns the root of `slot` and whether `slot`'s sign differs from it,
    /// compressing the path behind it. Iterative, so long chains cannot
    /// exhaust the stack.
    fn find(&mut self, slot: usize) -> (usize, bool) {
        let mut root = slot;
        let mut flipped = false;
        while self.parent[root] != root {
            flipped ^= self.flipped[root];
            root = self.parent[root];
        }
        let mut current = slot;
        let mut current_flipped = flipped;
        while self.parent[current] != current {
            let next = self.parent[current];
            let next_flipped = current_flipped ^ self.flipped[current];
            self.parent[current] = root;
            self.flipped[current] = current_flipped;
            current = next;
            current_flipped = next_flipped;
        }
        (root, flipped)
    }

    fn join(
        &mut self,
        left: usize,
        right: usize,
        opposite: bool,
        rule: PolarityRule,
    ) -> Result<(), PolarityViolation> {
        let (left_root, left_flipped) = self.find(left);
        let (right_root, right_flipped) = self.find(right);
        let flipped = left_flipped ^ right_flipped ^ opposite;
        if left_root == right_root {
            return if flipped {
                Err(PolarityViolation::Conflict(rule))
            } else {
                Ok(())
            };
        }
        self.parent[left_root] = right_root;
        self.flipped[left_root] = flipped;
        Ok(())
    }

    fn relate(
        &mut self,
        left: Port,
        right: Port,
        opposite: bool,
        rule: PolarityRule,
    ) -> Result<(), PolarityViolation> {
        self.join(Self::slot(left), Self::slot(right), opposite, rule)
    }

    fn relate_to_anchor(
        &mut self,
        port: Port,
        sign: Sign,
        rule: PolarityRule,
    ) -> Result<(), PolarityViolation> {
        let anchor = self.anchor();
        self.join(Self::slot(port), anchor, sign == Sign::Consumes, rule)
    }

    fn fix(&mut self, port: Port, sign: Sign, rule: NodeRule) -> Result<(), PolarityViolation> {
        self.relate_to_anchor(port, sign, PolarityRule::Node(port, rule))
    }
}

/// Which template checks a test build applies to a finished template.
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) struct TemplateChecks {
    pub(crate) signs: bool,
    pub(crate) connected: bool,
}

#[cfg(test)]
impl TemplateChecks {
    pub(crate) const ALL: Self = Self {
        signs: true,
        connected: true,
    };
}

/// Test-build hook: every template built by a test must pass the template
/// checks unless its builder opted out of one.
#[cfg(test)]
pub(crate) fn assert_template_for_test<S: NetSpecialization>(
    net: &InteractionNet<S>,
    checks: TemplateChecks,
) {
    let result = if checks.signs {
        check_signs(net)
    } else {
        Ok(())
    }
    .and_then(|()| {
        if checks.connected {
            check_connected(net)
        } else {
            Ok(())
        }
    });
    if let Err(violation) = result {
        let nodes = net
            .nodes
            .iter()
            .map(|node| match node {
                Node::Bind => "Bind",
                Node::Fan { .. } => "Fan",
                Node::Erase => "Erase",
                Node::Data(_) => "Data",
                Node::Operator(_) => "Operator",
            })
            .collect::<Vec<_>>();
        panic!(
            "{violation}; nodes: {nodes:?}; wires: {:?}; exposed: {:?}",
            net.wires, net.exposed
        );
    }
}

#[cfg(test)]
mod tests;
