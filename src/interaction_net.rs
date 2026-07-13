//! Immutable interaction-net code used by the evaluator.
//!
//! A lambda owns a single, lazily lowered net. Applying a closure only supplies
//! an environment to that shared code; it neither lowers nor copies the body.

use std::sync::Arc;

use crate::core::{DeferredValue, Expr, IVar, Key, Lambda, Value};

pub type NodeId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyNode {
    Key(Key),
    Index(NodeId),
    PathIndex(NodeId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Value(Value),
    List(Arc<[NodeId]>),
    Apply(NodeId, NodeId),
    Lambda(Arc<Lambda>),
    Local(usize),
    Access(NodeId, Arc<[KeyNode]>),
    Deferred(Arc<DeferredValue>),
    Future(IVar),
    Error(Arc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionNet {
    nodes: Arc<[Node]>,
    root: NodeId,
}

impl InteractionNet {
    pub fn lower(expr: &Expr) -> Self {
        let mut lowerer = Lowerer { nodes: Vec::new() };
        let root = lowerer.lower_expr(expr);
        Self {
            nodes: Arc::from(lowerer.nodes),
            root,
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

struct Lowerer {
    nodes: Vec<Node>,
}

impl Lowerer {
    fn push(&mut self, node: Node) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(node);
        id
    }

    fn lower_expr(&mut self, expr: &Expr) -> NodeId {
        let node = match expr {
            Expr::Value(value) => Node::Value(value.clone()),
            Expr::List(items) => Node::List(Arc::from(
                items
                    .iter()
                    .map(|item| self.lower_expr(item))
                    .collect::<Vec<_>>(),
            )),
            Expr::Apply(function, argument) => {
                let function = self.lower_expr(function);
                let argument = self.lower_expr(argument);
                Node::Apply(function, argument)
            }
            Expr::Lambda(lambda) => Node::Lambda(lambda.clone()),
            Expr::Local(index) => Node::Local(*index),
            Expr::Access(base, path) => {
                let base = self.lower_expr(base);
                let path = path
                    .iter()
                    .map(|part| match part {
                        crate::core::KeyExpr::Key(key) => KeyNode::Key(key.clone()),
                        crate::core::KeyExpr::Index(expr) => KeyNode::Index(self.lower_expr(expr)),
                        crate::core::KeyExpr::PathIndex(expr) => {
                            KeyNode::PathIndex(self.lower_expr(expr))
                        }
                    })
                    .collect::<Vec<_>>();
                Node::Access(base, Arc::from(path))
            }
            Expr::Deferred(value) => Node::Deferred(value.clone()),
            Expr::Future(value) => Node::Future(value.clone()),
            Expr::Error(message) => Node::Error(message.clone()),
            Expr::Net(net, node) => return self.import_net(net, *node),
        };
        self.push(node)
    }

    fn import_net(&mut self, net: &InteractionNet, node: NodeId) -> NodeId {
        // Net references only occur in evaluator-created thunks. They are not
        // expected in source lambda bodies; retaining the reference as data is
        // both cheaper and preserves sharing.
        self.push(Node::Value(Value::expr(Expr::Net(
            Arc::new(net.clone()),
            node,
        ))))
    }
}
