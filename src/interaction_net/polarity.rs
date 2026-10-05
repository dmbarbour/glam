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

use std::collections::VecDeque;
use std::fmt;

#[cfg(test)]
use super::model::InteractionNet;
use super::model::{NetSpecialization, Node, NodeId, Port, Wire};

/// Why a template is not polarized or not connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PolarityViolation {
    /// `closing` contradicts signs that earlier rules already forced. Those
    /// rules, in order from one end of `closing` to the other, are
    /// `forced_by`.
    Conflict {
        closing: PolarityRule,
        forced_by: Vec<PolarityRule>,
    },
    /// A node cannot be reached from the exposed port.
    Disconnected(NodeId),
}

/// One sign equation.
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
    TunnelPassesThrough,
}

/// The node shapes the checker distinguishes. A tunnel is the builder-only
/// two-port splice for `.copy 1`; finishing a template removes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    Bind,
    Fan,
    Erase,
    Data,
    Operator,
    Tunnel,
}

impl Shape {
    pub(crate) fn of<S: NetSpecialization>(node: &Node<S>) -> Self {
        match node {
            Node::Bind => Self::Bind,
            Node::Fan { .. } => Self::Fan,
            Node::Erase => Self::Erase,
            Node::Data(_) => Self::Data,
            Node::Operator(_) => Self::Operator,
        }
    }
}

/// Which template checks apply. Production always applies both; a test may
/// opt out of one to model a deliberately malformed net.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TemplateChecks {
    pub(crate) signs: bool,
    pub(crate) connected: bool,
}

impl TemplateChecks {
    pub(crate) const ALL: Self = Self {
        signs: true,
        connected: true,
    };
}

impl PolarityViolation {
    /// Explains the violation, naming ports and nodes through the caller's
    /// descriptions so a user sees the ports they built.
    pub(crate) fn describe(
        &self,
        port: &dyn Fn(Port) -> String,
        node: &dyn Fn(NodeId) -> String,
    ) -> String {
        const SHOWN_STEPS: usize = 6;
        match self {
            Self::Conflict { closing, forced_by } => {
                let (finding, rule) = match closing {
                    PolarityRule::Wire(left, right) => (
                        format!(
                            "the wire between {} and {} joins two ports of the same sign",
                            port(*left),
                            port(*right)
                        ),
                        "A wire must join a port that provides a value to one that consumes it.",
                    ),
                    PolarityRule::Exposed(exposed) => (
                        format!("{} is exposed but consumes a value", port(*exposed)),
                        "A net must expose a port that provides a value.",
                    ),
                    PolarityRule::Node(at, rule) => (
                        format!("{} {} contradicts its wiring", port(*at), rule.explain()),
                        "Each node fixes the signs of its ports.",
                    ),
                };
                let mut message = format!("interaction net is not polarized: {finding}");
                if !forced_by.is_empty() {
                    let steps = forced_by
                        .iter()
                        .take(SHOWN_STEPS)
                        .map(|rule| match rule {
                            PolarityRule::Wire(left, right) => {
                                format!("{} is wired to {}", port(*left), port(*right))
                            }
                            PolarityRule::Exposed(exposed) => {
                                format!("{} is exposed", port(*exposed))
                            }
                            PolarityRule::Node(at, rule) => {
                                format!("{} {}", port(*at), rule.explain())
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", and ");
                    message.push_str(", because ");
                    message.push_str(&steps);
                    if forced_by.len() > SHOWN_STEPS {
                        message.push_str(", and …");
                    }
                }
                message.push_str(". ");
                message.push_str(rule);
                message
            }
            Self::Disconnected(id) => format!(
                "interaction net is not connected: {} cannot be reached from the exposed port",
                node(*id)
            ),
        }
    }
}

impl NodeRule {
    fn explain(self) -> &'static str {
        match self {
            Self::DataProvides => "provides its data",
            Self::OperatorConsumesInput => "consumes as an operator's input",
            Self::OperatorProvidesResult => "provides as an operator's result",
            Self::BindFirstAuxiliaryConsumes => "consumes as a bind's first auxiliary",
            Self::BindSecondAuxiliaryProvides => "provides as a bind's second auxiliary",
            Self::FanPrincipalOpposesBranches => "is a fan principal, opposite to its branches",
            Self::FanBranchesAgree => "is a fan branch, matching the other branch",
            Self::TunnelPassesThrough => "passes through a `.copy 1`",
        }
    }
}

impl fmt::Display for PolarityViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.describe(&|port| format!("{port:?}"), &|node| {
            format!("node {}", node.get())
        }))
    }
}

/// A template's topology as the checker sees it: one shape per node.
pub(crate) struct Topology<'a> {
    pub(crate) shapes: &'a [Shape],
    pub(crate) wires: &'a [Wire],
    pub(crate) exposed: Port,
}

/// Checks that a finished template is polarized and connected.
#[cfg(test)]
pub(crate) fn check_template<S: NetSpecialization>(
    net: &InteractionNet<S>,
) -> Result<(), PolarityViolation> {
    let shapes = net.nodes.iter().map(Shape::of).collect::<Vec<_>>();
    Topology {
        shapes: &shapes,
        wires: &net.wires,
        exposed: net.exposed,
    }
    .check(TemplateChecks::ALL)
}

/// One sign equation between two slots: a port, or the constant `+` anchor.
#[derive(Clone, Copy)]
struct Constraint {
    left: Port,
    /// `None` is the `+` anchor.
    right: Option<Port>,
    opposite: bool,
    rule: PolarityRule,
}

impl Topology<'_> {
    pub(crate) fn check(&self, checks: TemplateChecks) -> Result<(), PolarityViolation> {
        if checks.signs {
            self.check_signs()?;
        }
        if checks.connected {
            self.check_connected()?;
        }
        Ok(())
    }

    /// Every sign equation, in a fixed order: node rules, then wires, then
    /// the exposed port. Node rules touch only their own node's ports, so a
    /// conflict always closes at a wire or at the exposed port.
    fn constraints(&self) -> impl Iterator<Item = Constraint> + '_ {
        let nodes = (0..self.shapes.len()).flat_map(move |index| {
            let id = NodeId::from_index(index);
            let principal = Port::principal(id);
            let fixed = |port: Port, consumes: bool, rule: NodeRule| Constraint {
                left: port,
                right: None,
                opposite: consumes,
                rule: PolarityRule::Node(port, rule),
            };
            let related = |left: Port, right: Port, opposite: bool, rule: NodeRule| Constraint {
                left,
                right: Some(right),
                opposite,
                rule: PolarityRule::Node(left, rule),
            };
            let rules: [Option<Constraint>; 2] = match self.shapes[index] {
                Shape::Data => [Some(fixed(principal, false, NodeRule::DataProvides)), None],
                Shape::Operator => [
                    Some(fixed(principal, true, NodeRule::OperatorConsumesInput)),
                    Some(fixed(
                        Port::auxiliary(id, 1),
                        false,
                        NodeRule::OperatorProvidesResult,
                    )),
                ],
                // A bind's principal is free: its sign is the bind's role.
                Shape::Bind => [
                    Some(fixed(
                        Port::auxiliary(id, 1),
                        true,
                        NodeRule::BindFirstAuxiliaryConsumes,
                    )),
                    Some(fixed(
                        Port::auxiliary(id, 2),
                        false,
                        NodeRule::BindSecondAuxiliaryProvides,
                    )),
                ],
                Shape::Fan => [
                    Some(related(
                        principal,
                        Port::auxiliary(id, 1),
                        true,
                        NodeRule::FanPrincipalOpposesBranches,
                    )),
                    Some(related(
                        Port::auxiliary(id, 1),
                        Port::auxiliary(id, 2),
                        false,
                        NodeRule::FanBranchesAgree,
                    )),
                ],
                Shape::Tunnel => [
                    Some(related(
                        principal,
                        Port::auxiliary(id, 1),
                        true,
                        NodeRule::TunnelPassesThrough,
                    )),
                    None,
                ],
                // An eraser's sign is free: it discards a value, or stands for
                // an error value where one is consumed.
                Shape::Erase => [None, None],
            };
            rules.into_iter().flatten()
        });
        let wires = self.wires.iter().map(|wire| Constraint {
            left: wire.left,
            right: Some(wire.right),
            opposite: true,
            rule: PolarityRule::Wire(wire.left, wire.right),
        });
        let exposed = std::iter::once(Constraint {
            left: self.exposed,
            right: None,
            opposite: false,
            rule: PolarityRule::Exposed(self.exposed),
        });
        nodes.chain(wires).chain(exposed)
    }

    fn anchor(&self) -> usize {
        self.shapes.len() * 3
    }

    fn slot(&self, port: Option<Port>) -> usize {
        port.map_or(self.anchor(), |port| {
            port.node().index() * 3 + port.index() as usize
        })
    }

    fn check_signs(&self) -> Result<(), PolarityViolation> {
        let mut signs = Signs::new(self.anchor() + 1);
        for (index, constraint) in self.constraints().enumerate() {
            let left = self.slot(Some(constraint.left));
            let right = self.slot(constraint.right);
            if !signs.join(left, right, constraint.opposite) {
                return Err(PolarityViolation::Conflict {
                    closing: constraint.rule,
                    forced_by: self.explain(index, left, right),
                });
            }
        }
        Ok(())
    }

    /// Finds the earlier rules that forced the conflict closed by constraint
    /// `closing`: a path between its two slots. This runs only on failure,
    /// so the ordinary check allocates nothing beyond its union-find.
    fn explain(&self, closing: usize, from: usize, to: usize) -> Vec<PolarityRule> {
        let mut edges = vec![Vec::new(); self.anchor() + 1];
        let earlier = self.constraints().take(closing).collect::<Vec<_>>();
        for (index, constraint) in earlier.iter().enumerate() {
            let left = self.slot(Some(constraint.left));
            let right = self.slot(constraint.right);
            edges[left].push((right, index));
            edges[right].push((left, index));
        }
        let mut reached_by = vec![None; edges.len()];
        let mut visited = vec![false; edges.len()];
        visited[from] = true;
        let mut pending = VecDeque::from([from]);
        while let Some(slot) = pending.pop_front() {
            if slot == to {
                break;
            }
            for &(next, constraint) in &edges[slot] {
                if !visited[next] {
                    visited[next] = true;
                    reached_by[next] = Some((slot, constraint));
                    pending.push_back(next);
                }
            }
        }
        let mut path = Vec::new();
        let mut slot = to;
        while let Some((previous, constraint)) = reached_by[slot] {
            path.push(earlier[constraint].rule);
            slot = previous;
        }
        path.reverse();
        path
    }

    fn check_connected(&self) -> Result<(), PolarityViolation> {
        let mut neighbors = vec![Vec::new(); self.shapes.len()];
        for wire in self.wires {
            neighbors[wire.left.node().index()].push(wire.right.node());
            neighbors[wire.right.node().index()].push(wire.left.node());
        }
        let mut reached = vec![false; self.shapes.len()];
        let mut pending = vec![self.exposed.node()];
        reached[self.exposed.node().index()] = true;
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
}

/// Union-find over port signs. Each entry records whether its sign differs
/// from its parent's; the extra final entry is a constant `+` anchor.
struct Signs {
    parent: Vec<usize>,
    flipped: Vec<bool>,
}

impl Signs {
    fn new(slots: usize) -> Self {
        Self {
            parent: (0..slots).collect(),
            flipped: vec![false; slots],
        }
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

    /// Records that `left` and `right` have equal signs, or opposite ones.
    /// Returns false when that contradicts what is already known.
    fn join(&mut self, left: usize, right: usize, opposite: bool) -> bool {
        let (left_root, left_flipped) = self.find(left);
        let (right_root, right_flipped) = self.find(right);
        let flipped = left_flipped ^ right_flipped ^ opposite;
        if left_root == right_root {
            return !flipped;
        }
        self.parent[left_root] = right_root;
        self.flipped[left_root] = flipped;
        true
    }
}

#[cfg(test)]
mod tests;
