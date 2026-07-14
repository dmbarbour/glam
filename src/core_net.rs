//! Lowering from syntax-independent core expressions into generic nets.

use std::sync::Arc;

use crate::core::{BuiltinCall, DeferredValue, Expr, IVar, Key, KeyExpr, Lambda, Value};
use crate::interaction_net::{InteractionNet, NetBuilder, Port, SharedRuntimeNet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreDataKey {
    Key(Key),
    Index,
    PathIndex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreNetData {
    Value(Value),
    Builtin(BuiltinCall),
    Lambda(Arc<Lambda>),
    Capture(usize),
    List {
        arity: usize,
        arguments: Arc<[Value]>,
    },
    Access {
        path: Arc<[CoreDataKey]>,
        arguments: Arc<[Value]>,
    },
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
        let [application, argument, result] = lowerer.net.bind();
        lowerer.compile_into(&body, result);
        lowerer.close_locals(argument);
        lowerer.net.finish(application)
    }

    fn compile_into(&mut self, expr: &Expr, target: Port) {
        match expr {
            Expr::Value(Value::Builtin(builtin)) => {
                self.data_into(CoreNetData::Builtin(BuiltinCall::new(*builtin)), target)
            }
            Expr::Value(Value::PartialBuiltin(call)) => {
                self.data_into(CoreNetData::Builtin(call.clone()), target)
            }
            Expr::Value(value) => self.data_into(CoreNetData::Value(value.clone()), target),
            Expr::List(items) => {
                let args = items.iter().map(Arc::as_ref).collect::<Vec<_>>();
                self.data_application_into(
                    CoreNetData::List {
                        arity: items.len(),
                        arguments: Arc::from([]),
                    },
                    &args,
                    target,
                );
            }
            Expr::Apply(function, argument) => {
                let [application, argument_port, result] = self.net.bind();
                self.net.wire(result, target);
                self.compile_into(function, application);
                self.compile_into(argument, argument_port);
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
                self.data_application_into(
                    CoreNetData::Access {
                        path: Arc::from(keys),
                        arguments: Arc::from([]),
                    },
                    &args,
                    target,
                );
            }
            Expr::Deferred(value) => self.data_into(CoreNetData::Deferred(value.clone()), target),
            Expr::Future(value) => self.data_into(CoreNetData::Future(value.clone()), target),
            Expr::Error(message) => self.data_into(CoreNetData::Error(message.clone()), target),
        }
    }

    fn data_into(&mut self, data: CoreNetData, target: Port) {
        let data = self.net.data(data);
        self.net.wire(data, target);
    }

    fn data_application_into(&mut self, data: CoreNetData, args: &[&Expr], target: Port) {
        if args.is_empty() {
            self.data_into(data, target);
            return;
        }
        let mut output = self.net.data(data);
        for argument in args {
            let [application, argument_port, result] = self.net.bind();
            self.net.wire(output, application);
            self.compile_into(argument, argument_port);
            output = result;
        }
        self.net.wire(output, target);
    }

    fn close_locals(&mut self, argument: Port) {
        let uses = std::mem::take(&mut self.local_uses);
        let max_index = uses.len().max(1);
        for index in 0..max_index {
            let targets = uses.get(index).map(Vec::as_slice).unwrap_or_default();
            let source = if index == 0 {
                argument
            } else {
                self.net.data(CoreNetData::Capture(index - 1))
            };
            self.distribute(source, targets);
        }
    }

    fn distribute(&mut self, source: Port, targets: &[Port]) {
        let copy = self.net.copy(targets.len());
        self.net.wire(source, copy.input);
        for (output, target) in copy.outputs.into_iter().zip(targets) {
            self.net.wire(output, *target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Dict;
    use crate::interaction_net::Node;

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
