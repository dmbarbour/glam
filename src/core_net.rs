//! Lowering from syntax-independent core expressions into generic nets.

use std::sync::Arc;

use crate::core::{DeferredValue, Expr, IVar, Key, KeyExpr, Lambda, Value};
use crate::interaction_net::{InteractionNet, NetBuilder, Node, NodeId, Port, SharedRuntimeNet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreDataKey {
    Key(Key),
    Index,
    PathIndex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreNetData {
    Value(Value),
    Lambda(Arc<Lambda>),
    Capture(usize),
    List(usize),
    Access(Arc<[CoreDataKey]>),
    Deferred(Arc<DeferredValue>),
    Future(IVar),
    Error(Arc<str>),
}

pub type CoreInteractionNet = InteractionNet<CoreNetData>;
pub type CoreRuntimeNet = SharedRuntimeNet<CoreNetData>;

pub fn lower_lambda(body: Arc<Expr>) -> CoreInteractionNet {
    Lowerer::lower_lambda(body)
}

struct Lowerer {
    net: NetBuilder<CoreNetData>,
    local_uses: Vec<Vec<Port>>,
}

impl Lowerer {
    fn lower_lambda(body: Arc<Expr>) -> CoreInteractionNet {
        let mut lowerer = Self {
            net: NetBuilder::new(),
            local_uses: Vec::new(),
        };
        let root = lowerer.net.push(Node::Bind);
        lowerer.compile_into(&body, Port::auxiliary(root, 2));
        lowerer.close_locals(root);
        lowerer.net.finish(Port::principal(root))
    }

    fn compile_into(&mut self, expr: &Expr, target: Port) {
        match expr {
            Expr::Value(value) => self.data_into(CoreNetData::Value(value.clone()), target),
            Expr::List(items) => {
                let args = items.iter().map(Arc::as_ref).collect::<Vec<_>>();
                self.data_application_into(CoreNetData::List(items.len()), &args, target);
            }
            Expr::Apply(function, argument) => {
                let bind = self.net.push(Node::Bind);
                self.net.wire(Port::auxiliary(bind, 2), target);
                self.compile_into(function, Port::principal(bind));
                self.compile_into(argument, Port::auxiliary(bind, 1));
            }
            Expr::Lambda(lambda) => self.data_into(CoreNetData::Lambda(lambda.clone()), target),
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
                        KeyExpr::Key(key) => CoreDataKey::Key(key.clone()),
                        KeyExpr::Index(expr) => {
                            args.push(expr);
                            CoreDataKey::Index
                        }
                        KeyExpr::PathIndex(expr) => {
                            args.push(expr);
                            CoreDataKey::PathIndex
                        }
                    })
                    .collect::<Vec<_>>();
                self.data_application_into(CoreNetData::Access(Arc::from(keys)), &args, target);
            }
            Expr::Deferred(value) => self.data_into(CoreNetData::Deferred(value.clone()), target),
            Expr::Future(value) => self.data_into(CoreNetData::Future(value.clone()), target),
            Expr::Error(message) => self.data_into(CoreNetData::Error(message.clone()), target),
        }
    }

    fn data_into(&mut self, data: CoreNetData, target: Port) {
        let node = self.net.push(Node::Data(data));
        self.net.wire(Port::principal(node), target);
    }

    fn data_application_into(&mut self, data: CoreNetData, args: &[&Expr], target: Port) {
        if args.is_empty() {
            self.data_into(data, target);
            return;
        }
        let function = self.net.push(Node::Data(data));
        let mut output = Port::principal(function);
        for argument in args {
            let bind = self.net.push(Node::Bind);
            self.net.wire(output, Port::principal(bind));
            self.compile_into(argument, Port::auxiliary(bind, 1));
            output = Port::auxiliary(bind, 2);
        }
        self.net.wire(output, target);
    }

    fn close_locals(&mut self, root: NodeId) {
        let uses = std::mem::take(&mut self.local_uses);
        let max_index = uses.len().max(1);
        for index in 0..max_index {
            let targets = uses.get(index).map(Vec::as_slice).unwrap_or_default();
            let source = if index == 0 {
                Port::auxiliary(root, 1)
            } else {
                let capture = self.net.push(Node::Data(CoreNetData::Capture(index - 1)));
                Port::principal(capture)
            };
            self.distribute(source, targets);
        }
    }

    fn distribute(&mut self, source: Port, targets: &[Port]) {
        match targets {
            [] => {
                let erase = self.net.push(Node::Erase);
                self.net.wire(source, Port::principal(erase));
            }
            [target] => self.net.wire(source, *target),
            _ => {
                let fan = self.net.push_fan();
                self.net.wire(source, Port::principal(fan));
                let middle = targets.len() / 2;
                self.distribute(Port::auxiliary(fan, 1), &targets[..middle]);
                self.distribute(Port::auxiliary(fan, 2), &targets[middle..]);
            }
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
    fn identity_uses_a_direct_wire_without_a_fan() {
        let net = lower_lambda(Arc::new(Expr::Local(0)));
        assert!(matches!(net.nodes()[0], Node::Bind));
        assert!(
            !net.nodes()
                .iter()
                .any(|node| matches!(node, Node::Fan { .. }))
        );
        assert!(!net.nodes().iter().any(|node| matches!(node, Node::Erase)));
        assert!(net.exposed().is_principal());
        assert_eq!(net.exposed().node().get(), 0);
        assert_eq!(net.wires().len(), 1);
    }

    #[test]
    fn unused_argument_lowers_to_eraser() {
        let net = lower_lambda(Arc::new(Expr::Value(unit())));
        assert!(net.nodes().iter().any(|node| matches!(node, Node::Erase)));
    }

    #[test]
    fn repeated_argument_lowers_to_one_binary_fan() {
        let body = Expr::Apply(Arc::new(Expr::Local(0)), Arc::new(Expr::Local(0)));
        let net = lower_lambda(Arc::new(body));
        assert_eq!(
            net.nodes()
                .iter()
                .filter(|node| matches!(node, Node::Fan { .. }))
                .count(),
            1
        );
    }
}
