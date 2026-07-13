//! Port-and-wire interaction nets and lambda lowering.
//!
//! A [`crate::core::Lambda`] owns one immutable `InteractionNet` template.
//! Runtime reconstruction may clone its topology, but never lowers the lambda
//! body again. Every source-level `Copy` constructor receives a global identity;
//! copies made by interaction preserve that identity.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::{DeferredValue, Expr, IVar, Key, KeyExpr, Lambda, Value};

pub type NodeId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CopyUid(u64);

impl CopyUid {
    pub fn fresh() -> Self {
        static NEXT_UID: AtomicU64 = AtomicU64::new(1);

        let uid = NEXT_UID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |uid| {
                uid.checked_add(1)
            })
            .expect("interaction-net copy UID space exhausted");
        Self(uid)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    fn from_raw(uid: u64) -> Self {
        Self(uid)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataKey {
    Key(Key),
    Index,
    PathIndex,
}

/// Opaque expressions admitted through the single-port `Data` constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedData {
    Value(Value),
    Lambda(Arc<Lambda>),
    Capture(usize),
    List(usize),
    Access(Arc<[DataKey]>),
    Deferred(Arc<DeferredValue>),
    Future(IVar),
    Error(Arc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Ports: `[ap*, arg, result]`.
    Bind,
    /// Ports: `[input*, output_0, ..., output_n]`.
    Copy { uid: CopyUid, outputs: u32 },
    /// Ports: `[data*]`.
    Data(EmbeddedData),
}

impl Node {
    pub fn port_count(&self) -> u32 {
        match self {
            Self::Bind => 3,
            Self::Copy { outputs, .. } => outputs + 1,
            Self::Data(_) => 1,
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
pub struct InteractionNet {
    nodes: Arc<[Node]>,
    wires: Arc<[Wire]>,
    exposed: Port,
    active_pairs: Arc<[ActivePair]>,
}

impl InteractionNet {
    pub fn lower_lambda(body: Arc<Expr>) -> Self {
        Lowerer::lower_lambda(body)
    }

    pub fn nodes(&self) -> &[Node] {
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

    pub fn instantiate(&self) -> RuntimeNet {
        RuntimeNet::new(self)
    }
}

struct Lowerer {
    nodes: Vec<Node>,
    wires: Vec<Wire>,
    local_uses: Vec<Vec<Port>>,
}

impl Lowerer {
    fn lower_lambda(body: Arc<Expr>) -> InteractionNet {
        let mut lowerer = Self {
            nodes: Vec::new(),
            wires: Vec::new(),
            local_uses: Vec::new(),
        };
        let root = lowerer.push(Node::Bind);
        lowerer.compile_into(&body, Port::auxiliary(root, 2));
        lowerer.close_locals(root);
        let exposed = Port::principal(root);
        lowerer.validate(exposed);
        let active_pairs = lowerer.find_active_pairs();

        InteractionNet {
            nodes: Arc::from(lowerer.nodes),
            wires: Arc::from(lowerer.wires),
            exposed,
            active_pairs: Arc::from(active_pairs),
        }
    }

    fn push(&mut self, node: Node) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(node);
        id
    }

    fn wire(&mut self, left: Port, right: Port) {
        self.wires.push(Wire { left, right });
    }

    fn compile_into(&mut self, expr: &Expr, target: Port) {
        match expr {
            Expr::Value(value) => self.data_into(EmbeddedData::Value(value.clone()), target),
            Expr::List(items) => {
                let args = items.iter().map(Arc::as_ref).collect::<Vec<_>>();
                self.data_application_into(EmbeddedData::List(items.len()), &args, target);
            }
            Expr::Apply(function, argument) => {
                let bind = self.push(Node::Bind);
                self.wire(Port::auxiliary(bind, 2), target);
                self.compile_into(function, Port::principal(bind));
                self.compile_into(argument, Port::auxiliary(bind, 1));
            }
            Expr::Lambda(lambda) => self.data_into(EmbeddedData::Lambda(lambda.clone()), target),
            Expr::Local(index) => {
                if self.local_uses.len() <= *index {
                    self.local_uses.resize_with(index + 1, Vec::new);
                }
                self.local_uses[*index].push(target);
            }
            Expr::Access(base, path) => {
                let mut args = vec![base.as_ref()];
                let keys = path
                    .iter()
                    .map(|key| match key {
                        KeyExpr::Key(key) => DataKey::Key(key.clone()),
                        KeyExpr::Index(expr) => {
                            args.push(expr);
                            DataKey::Index
                        }
                        KeyExpr::PathIndex(expr) => {
                            args.push(expr);
                            DataKey::PathIndex
                        }
                    })
                    .collect::<Vec<_>>();
                self.data_application_into(EmbeddedData::Access(Arc::from(keys)), &args, target);
            }
            Expr::Deferred(value) => self.data_into(EmbeddedData::Deferred(value.clone()), target),
            Expr::Future(value) => self.data_into(EmbeddedData::Future(value.clone()), target),
            Expr::Error(message) => self.data_into(EmbeddedData::Error(message.clone()), target),
        }
    }

    fn data_into(&mut self, data: EmbeddedData, target: Port) {
        let node = self.push(Node::Data(data));
        self.wire(Port::principal(node), target);
    }

    fn data_application_into(&mut self, data: EmbeddedData, args: &[&Expr], target: Port) {
        if args.is_empty() {
            self.data_into(data, target);
            return;
        }

        let function = self.push(Node::Data(data));
        let mut output = Port::principal(function);
        for argument in args {
            let bind = self.push(Node::Bind);
            self.wire(output, Port::principal(bind));
            self.compile_into(argument, Port::auxiliary(bind, 1));
            output = Port::auxiliary(bind, 2);
        }
        self.wire(output, target);
    }

    fn close_locals(&mut self, root: NodeId) {
        let uses = std::mem::take(&mut self.local_uses);
        let max_index = uses.len().max(1);
        for index in 0..max_index {
            let targets = uses.get(index).map(Vec::as_slice).unwrap_or_default();
            let source = if index == 0 {
                Port::auxiliary(root, 1)
            } else {
                let capture = self.push(Node::Data(EmbeddedData::Capture(index - 1)));
                Port::principal(capture)
            };
            let copy = self.push(Node::Copy {
                uid: CopyUid::fresh(),
                outputs: u32::try_from(targets.len()).expect("too many local uses in lambda"),
            });
            self.wire(source, Port::principal(copy));
            for (output, target) in targets.iter().enumerate() {
                self.wire(
                    Port::auxiliary(
                        copy,
                        u32::try_from(output + 1).expect("too many local uses in lambda"),
                    ),
                    *target,
                );
            }
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

    fn find_active_pairs(&self) -> Vec<ActivePair> {
        self.wires
            .iter()
            .filter(|wire| wire.left.is_principal() && wire.right.is_principal())
            .map(|wire| ActivePair {
                left: wire.left.node,
                right: wire.right.node,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reduction {
    BindJoin,
    CopyJoin { uid: CopyUid },
    CopyDup { left: CopyUid, right: CopyUid },
    CopyData { uid: CopyUid },
    CopyBind { uid: CopyUid },
    Call,
    Stuck,
}

/// Mutable topology reconstructed from a shared net template.
pub struct RuntimeNet {
    nodes: Vec<Option<Node>>,
    links: Vec<Vec<Option<Port>>>,
    active: VecDeque<ActivePair>,
}

impl RuntimeNet {
    fn new(net: &InteractionNet) -> Self {
        let nodes = net.nodes.iter().cloned().map(Some).collect::<Vec<_>>();
        let mut runtime = Self {
            links: net
                .nodes
                .iter()
                .map(|node| vec![None; node.port_count() as usize])
                .collect(),
            nodes,
            active: VecDeque::new(),
        };
        for wire in net.wires.iter() {
            runtime.connect(wire.left, wire.right);
        }
        runtime
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

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id).and_then(Option::as_ref)
    }

    pub fn reduce_next(&mut self) -> Option<Reduction> {
        while let Some(pair) = self.active.pop_front() {
            let left_port = Port::principal(pair.left);
            let right_port = Port::principal(pair.right);
            if self.neighbor(left_port) != Some(right_port) {
                continue;
            }
            let left = self.node(pair.left)?.clone();
            let right = self.node(pair.right)?.clone();
            return Some(match (&left, &right) {
                (Node::Bind, Node::Bind) => {
                    self.join(pair.left, pair.right, 2);
                    Reduction::BindJoin
                }
                (
                    Node::Copy {
                        uid: left_uid,
                        outputs: left_outputs,
                    },
                    Node::Copy {
                        uid: right_uid,
                        outputs: right_outputs,
                    },
                ) if left_uid == right_uid => {
                    assert_eq!(
                        left_outputs, right_outputs,
                        "equal copy UIDs need equal arity"
                    );
                    self.join(pair.left, pair.right, *left_outputs);
                    Reduction::CopyJoin { uid: *left_uid }
                }
                (
                    Node::Copy {
                        uid: left_uid,
                        outputs: left_outputs,
                    },
                    Node::Copy {
                        uid: right_uid,
                        outputs: right_outputs,
                    },
                ) => {
                    self.duplicate_copies(
                        pair.left,
                        *left_uid,
                        *left_outputs,
                        pair.right,
                        *right_uid,
                        *right_outputs,
                    );
                    Reduction::CopyDup {
                        left: *left_uid,
                        right: *right_uid,
                    }
                }
                (Node::Copy { uid, outputs }, Node::Data(_)) => {
                    self.duplicate_data(pair.left, *outputs, pair.right);
                    Reduction::CopyData { uid: *uid }
                }
                (Node::Data(_), Node::Copy { uid, outputs }) => {
                    self.duplicate_data(pair.right, *outputs, pair.left);
                    Reduction::CopyData { uid: *uid }
                }
                (Node::Copy { uid, outputs }, Node::Bind) => {
                    self.duplicate_bind(pair.left, *uid, *outputs, pair.right);
                    Reduction::CopyBind { uid: *uid }
                }
                (Node::Bind, Node::Copy { uid, outputs }) => {
                    self.duplicate_bind(pair.right, *uid, *outputs, pair.left);
                    Reduction::CopyBind { uid: *uid }
                }
                (Node::Bind, Node::Data(_)) | (Node::Data(_), Node::Bind) => Reduction::Call,
                (Node::Data(_), Node::Data(_)) => Reduction::Stuck,
            });
        }
        None
    }

    fn join(&mut self, left: NodeId, right: NodeId, auxiliaries: u32) {
        self.disconnect(Port::principal(left));
        let left_neighbors = (1..=auxiliaries)
            .map(|index| self.disconnect(Port::auxiliary(left, index)))
            .collect::<Vec<_>>();
        let right_neighbors = (1..=auxiliaries)
            .map(|index| self.disconnect(Port::auxiliary(right, index)))
            .collect::<Vec<_>>();
        self.remove_node(left);
        self.remove_node(right);
        for (left, right) in left_neighbors.into_iter().zip(right_neighbors) {
            self.connect(
                left.expect("joined port must be wired"),
                right.expect("joined port must be wired"),
            );
        }
    }

    fn duplicate_data(&mut self, copy: NodeId, outputs: u32, data: NodeId) {
        self.disconnect(Port::principal(copy));
        let targets = (1..=outputs)
            .map(|index| {
                self.disconnect(Port::auxiliary(copy, index))
                    .expect("copy output must be wired")
            })
            .collect::<Vec<_>>();
        let Node::Data(payload) = self.nodes[data].take().expect("data node must exist") else {
            unreachable!();
        };
        self.links[data].clear();
        self.remove_node(copy);
        for target in targets {
            let clone = self.add_node(Node::Data(payload.clone()));
            self.connect(Port::principal(clone), target);
        }
    }

    fn duplicate_bind(&mut self, copy: NodeId, uid: CopyUid, outputs: u32, bind: NodeId) {
        self.disconnect(Port::principal(copy));
        let copy_targets = (1..=outputs)
            .map(|index| {
                self.disconnect(Port::auxiliary(copy, index))
                    .expect("copy output must be wired")
            })
            .collect::<Vec<_>>();
        let bind_targets = (1..=2)
            .map(|index| {
                self.disconnect(Port::auxiliary(bind, index))
                    .expect("bind auxiliary must be wired")
            })
            .collect::<Vec<_>>();
        self.remove_node(copy);
        self.remove_node(bind);

        let binds = copy_targets
            .into_iter()
            .map(|target| {
                let node = self.add_node(Node::Bind);
                self.connect(Port::principal(node), target);
                node
            })
            .collect::<Vec<_>>();
        for (auxiliary, target) in bind_targets.into_iter().enumerate() {
            let fan = self.add_node(Node::Copy { uid, outputs });
            self.connect(Port::principal(fan), target);
            for (output, bind) in binds.iter().enumerate() {
                self.connect(
                    Port::auxiliary(fan, output as u32 + 1),
                    Port::auxiliary(*bind, auxiliary as u32 + 1),
                );
            }
        }
    }

    fn duplicate_copies(
        &mut self,
        left: NodeId,
        left_uid: CopyUid,
        left_outputs: u32,
        right: NodeId,
        right_uid: CopyUid,
        right_outputs: u32,
    ) {
        self.disconnect(Port::principal(left));
        let left_targets = (1..=left_outputs)
            .map(|index| {
                self.disconnect(Port::auxiliary(left, index))
                    .expect("copy output must be wired")
            })
            .collect::<Vec<_>>();
        let right_targets = (1..=right_outputs)
            .map(|index| {
                self.disconnect(Port::auxiliary(right, index))
                    .expect("copy output must be wired")
            })
            .collect::<Vec<_>>();
        self.remove_node(left);
        self.remove_node(right);

        let right_copies = left_targets
            .into_iter()
            .map(|target| {
                let node = self.add_node(Node::Copy {
                    uid: right_uid,
                    outputs: right_outputs,
                });
                self.connect(Port::principal(node), target);
                node
            })
            .collect::<Vec<_>>();
        let left_copies = right_targets
            .into_iter()
            .map(|target| {
                let node = self.add_node(Node::Copy {
                    uid: left_uid,
                    outputs: left_outputs,
                });
                self.connect(Port::principal(node), target);
                node
            })
            .collect::<Vec<_>>();
        for (left_index, right_copy) in right_copies.iter().enumerate() {
            for (right_index, left_copy) in left_copies.iter().enumerate() {
                self.connect(
                    Port::auxiliary(*right_copy, right_index as u32 + 1),
                    Port::auxiliary(*left_copy, left_index as u32 + 1),
                );
            }
        }
    }

    fn add_node(&mut self, node: Node) -> NodeId {
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
    use crate::core::Dict;

    fn unit() -> Value {
        Value::Dict(Dict::new_sync())
    }

    #[test]
    fn identity_lowers_to_bind_copy_and_wires() {
        let net = InteractionNet::lower_lambda(Arc::new(Expr::Local(0)));
        assert!(matches!(net.nodes()[0], Node::Bind));
        let Node::Copy { outputs, .. } = net.nodes()[1] else {
            panic!("identity argument should lower through Copy 1");
        };
        assert_eq!(outputs, 1);
        assert_eq!(net.exposed(), Port::principal(0));
        assert_eq!(net.wires().len(), 2);
        assert!(net.active_pairs().is_empty());
    }

    #[test]
    fn unused_argument_lowers_to_copy_zero() {
        let net = InteractionNet::lower_lambda(Arc::new(Expr::Value(unit())));
        assert!(
            net.nodes()
                .iter()
                .any(|node| matches!(node, Node::Copy { outputs: 0, .. }))
        );
    }

    #[test]
    fn repeated_argument_lowers_to_one_copy_with_matching_arity() {
        let body = Expr::Apply(Arc::new(Expr::Local(0)), Arc::new(Expr::Local(0)));
        let net = InteractionNet::lower_lambda(Arc::new(body));
        let copies = net
            .nodes()
            .iter()
            .filter_map(|node| match node {
                Node::Copy { uid, outputs } => Some((*uid, *outputs)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].1, 2);
    }

    #[test]
    fn every_source_copy_gets_a_distinct_global_uid() {
        let first = InteractionNet::lower_lambda(Arc::new(Expr::Local(0)));
        let second = InteractionNet::lower_lambda(Arc::new(Expr::Local(0)));
        let uid = |net: &InteractionNet| {
            net.nodes()
                .iter()
                .find_map(|node| match node {
                    Node::Copy { uid, .. } => Some(*uid),
                    _ => None,
                })
                .unwrap()
        };
        assert_ne!(uid(&first), uid(&second));
    }

    fn copy_pair(left_uid: CopyUid, right_uid: CopyUid) -> RuntimeNet {
        let nodes = vec![
            Node::Copy {
                uid: left_uid,
                outputs: 1,
            },
            Node::Copy {
                uid: right_uid,
                outputs: 1,
            },
            Node::Data(EmbeddedData::Value(unit())),
            Node::Data(EmbeddedData::Value(unit())),
        ];
        let net = InteractionNet {
            nodes: Arc::from(nodes),
            wires: Arc::from([
                Wire {
                    left: Port::principal(0),
                    right: Port::principal(1),
                },
                Wire {
                    left: Port::auxiliary(0, 1),
                    right: Port::principal(2),
                },
                Wire {
                    left: Port::auxiliary(1, 1),
                    right: Port::principal(3),
                },
            ]),
            exposed: Port::principal(usize::MAX),
            active_pairs: Arc::from([ActivePair { left: 0, right: 1 }]),
        };
        net.instantiate()
    }

    #[test]
    fn same_uid_copy_pair_joins() {
        let uid = CopyUid::from_raw(7);
        let mut net = copy_pair(uid, uid);
        assert_eq!(net.reduce_next(), Some(Reduction::CopyJoin { uid }));
        assert!(net.node(0).is_none());
        assert!(net.node(1).is_none());
        assert_eq!(net.active_pairs(), vec![ActivePair { left: 2, right: 3 }]);
    }

    #[test]
    fn different_uid_copy_pair_duplicates() {
        let left = CopyUid::from_raw(7);
        let right = CopyUid::from_raw(8);
        let mut net = copy_pair(left, right);
        assert_eq!(net.reduce_next(), Some(Reduction::CopyDup { left, right }));
        assert!(net.node(0).is_none());
        assert!(net.node(1).is_none());
        assert!(matches!(
            net.node(4),
            Some(Node::Copy { uid, .. }) if *uid == right
        ));
        assert!(matches!(
            net.node(5),
            Some(Node::Copy { uid, .. }) if *uid == left
        ));
        assert_eq!(net.active_pairs().len(), 2);
    }
}
