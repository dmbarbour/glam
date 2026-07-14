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

#[derive(Debug, Clone)]
pub struct ClosedLambdaNet {
    pub runtime: CoreRuntimeNet,
    pub capture_count: usize,
}

pub fn lower_lambda(body: Arc<Expr>) -> CoreInteractionNet {
    Lowerer::lower_lambda(body)
}

/// Lambda-lifts every free local into an explicit leading bind, leaving one
/// closed shared net. The original lambda argument is the final bind. Nested
/// lambdas remain on the compatibility lowering until demand can cross a
/// second logical-copy argument frontier.
pub(crate) fn lower_closed_lambda(body: Arc<Expr>) -> ClosedLambdaNet {
    ClosedLowerer::lower_lambda(body)
}

/// Returns the body and arity of the maximal leading curried lambda spine.
/// The outer `Lambda` owning the supplied body accounts for the first bind.
fn lambda_spine(mut body: Arc<Expr>) -> (usize, Arc<Expr>) {
    let mut arity = 1;
    while let Expr::Lambda(lambda) = body.as_ref() {
        arity += 1;
        body = lambda.body().clone();
    }
    (arity, body)
}

struct Lowerer {
    net: NetBuilder<CoreNetData>,
    local_uses: Vec<Vec<Port>>,
}

struct ClosedLowerer {
    net: NetBuilder<CoreNetData>,
    local_uses: Vec<Vec<Port>>,
}

impl ClosedLowerer {
    fn lower_lambda(body: Arc<Expr>) -> ClosedLambdaNet {
        let (arity, body) = lambda_spine(body);
        let mut lowerer = Self {
            net: NetBuilder::new(),
            local_uses: Vec::new(),
        };
        let body_boundary = lowerer.net.copy(1);
        lowerer.compile_into(&body, body_boundary.outputs[0]);

        let capture_count = lowerer.local_uses.len().saturating_sub(arity);
        let bind_count = capture_count + arity;
        let mut binds = Vec::with_capacity(bind_count);
        for _ in 0..bind_count {
            binds.push(lowerer.net.bind());
        }
        for pair in binds.windows(2) {
            lowerer.net.wire(pair[0][2], pair[1][0]);
        }
        lowerer.net.wire(
            binds.last().expect("lambda always has one bind")[2],
            body_boundary.input,
        );

        let uses = std::mem::take(&mut lowerer.local_uses);
        for index in 0..bind_count {
            let targets = uses.get(index).map(Vec::as_slice).unwrap_or_default();
            let bind_index = if index < arity {
                capture_count + arity - index - 1
            } else {
                index - arity
            };
            lowerer.distribute(binds[bind_index][1], targets);
        }

        let template = lowerer.net.finish(binds[0][0]);
        ClosedLambdaNet {
            runtime: template.instantiate_shared(),
            capture_count,
        }
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
            Expr::Lambda(_) => {
                unreachable!("nested lambdas remain on the compatibility evaluator")
            }
            Expr::Local(index) => self.use_local(*index, target),
            Expr::Access(_, _) => {
                unreachable!("dictionary access remains on the compatibility evaluator")
            }
            Expr::Deferred(value) => self.data_into(CoreNetData::Deferred(value.clone()), target),
            Expr::Future(value) => self.data_into(CoreNetData::Future(value.clone()), target),
            Expr::Error(message) => self.data_into(CoreNetData::Error(message.clone()), target),
        }
    }

    fn use_local(&mut self, index: usize, target: Port) {
        if self.local_uses.len() <= index {
            self.local_uses.resize_with(index + 1, Vec::new);
        }
        self.local_uses[index].push(target);
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

    fn distribute(&mut self, source: Port, targets: &[Port]) {
        let copy = self.net.copy(targets.len());
        self.net.wire(source, copy.input);
        for (output, target) in copy.outputs.into_iter().zip(targets) {
            self.net.wire(output, *target);
        }
    }
}

impl Lowerer {
    fn lower_lambda(body: Arc<Expr>) -> CoreInteractionNet {
        let (arity, body) = lambda_spine(body);
        let mut lowerer = Self {
            net: NetBuilder::new(),
            local_uses: Vec::new(),
        };
        let mut binds = Vec::with_capacity(arity);
        for _ in 0..arity {
            binds.push(lowerer.net.bind());
        }
        for pair in binds.windows(2) {
            lowerer.net.wire(pair[0][2], pair[1][0]);
        }
        lowerer.compile_into(&body, binds.last().unwrap()[2]);
        let arguments = binds.iter().map(|bind| bind[1]).collect::<Vec<_>>();
        lowerer.close_locals(&arguments);
        lowerer.net.finish(binds[0][0])
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

    fn close_locals(&mut self, arguments: &[Port]) {
        let uses = std::mem::take(&mut self.local_uses);
        let arity = arguments.len();
        let max_index = uses.len().max(arity);
        for index in 0..max_index {
            let targets = uses.get(index).map(Vec::as_slice).unwrap_or_default();
            let source = if index < arity {
                arguments[arity - index - 1]
            } else {
                self.net.data(CoreNetData::Capture(index - arity))
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

    #[test]
    fn curried_lambda_spine_lowers_to_one_bind_chain() {
        let inner = Arc::new(Lambda::new(Arc::new(Expr::Local(1))));
        let net = lower_lambda(Arc::new(Expr::Lambda(inner)));

        assert_eq!(
            net.nodes()
                .iter()
                .filter(|node| matches!(node, Node::Bind))
                .count(),
            2
        );
        assert!(
            !net.nodes()
                .iter()
                .any(|node| matches!(node, Node::Data(CoreNetData::Lambda(_))))
        );
    }

    #[test]
    fn closed_lowering_lifts_free_locals_into_leading_binds() {
        let closed = lower_closed_lambda(Arc::new(Expr::Local(1)));

        assert_eq!(closed.capture_count, 1);
        let exposed_neighbor = closed
            .runtime
            .with(|runtime| runtime.interface_neighbor(runtime.exposed()));
        assert!(exposed_neighbor.is_some_and(Port::is_principal));
    }

    #[test]
    fn closed_lowering_counts_lambda_spine_locals_as_arguments() {
        let inner = Arc::new(Lambda::new(Arc::new(Expr::Local(1))));
        let closed = lower_closed_lambda(Arc::new(Expr::Lambda(inner)));

        assert_eq!(closed.capture_count, 0);
    }
}
