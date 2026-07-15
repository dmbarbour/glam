//! Lowering from syntax-independent core expressions into generic nets.

use std::sync::Arc;

use crate::core::{BuiltinCall, DeferredValue, Expr, IVar, Key, KeyExpr, Value};
use crate::interaction_net::{HostFn, InteractionNet, NetBuilder, Port, SharedRuntimeNet};

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
    Deferred(Arc<DeferredValue>),
    Future(IVar),
    Error(Arc<str>),
}

pub type CoreInteractionNet = InteractionNet<CoreNetData>;
pub type CoreRuntimeNet = SharedRuntimeNet<CoreNetData>;

#[derive(Debug, Clone)]
pub struct LiftedFunctionNet {
    pub runtime: CoreRuntimeNet,
    pub capture_count: usize,
}

/// Lambda-lifts every local outside `arity` into an explicit leading bind,
/// leaving one closed shared net. Capture binds precede the function's ordinary
/// argument binds and are supplied immediately by the enclosing lowerer.
pub(crate) fn lower_function(arity: usize, body: Arc<Expr>) -> LiftedFunctionNet {
    assert!(arity > 0, "a function must bind at least one argument");
    ClosedLowerer::lower_function(arity, body)
}

/// Lowers net-safe functions immediately and confines the remaining
/// data-boundary compatibility representation to this construction boundary.
pub(crate) fn lower_function_value(arity: usize, body: Expr) -> Value {
    assert!(arity > 0, "a function must bind at least one argument");
    if function_body_is_net_safe(&body, arity) {
        let lifted = lower_function(arity, Arc::new(body));
        debug_assert_eq!(lifted.capture_count, 0);
        return Value::Net(crate::core::NetValue::new(lifted.runtime));
    }

    let mut expression = body;
    for _ in 0..arity {
        expression = Expr::Lambda(Arc::new(crate::core::Lambda::new(Arc::new(expression))));
    }
    Value::expr(expression)
}

/// Transitional safety boundary for legacy host callbacks that can consume
/// only embedded data. Structural functions passed to those callbacks cannot
/// yet be reified as data, so expressions involving access, aggregates, or
/// nested functions retain evaluator compatibility lowering for now.
pub(crate) fn function_body_is_net_safe(expr: &Expr, arity: usize) -> bool {
    match expr {
        Expr::Value(value) => matches!(
            value,
            Value::Atom(_)
                | Value::Number(_)
                | Value::Binary(_)
                | Value::Builtin(_)
                | Value::Net(_)
        ),
        Expr::Deferred(_) | Expr::Future(_) | Expr::Error(_) => true,
        Expr::List(items) => items
            .iter()
            .all(|item| function_body_is_net_safe(item, arity)),
        Expr::Local(_) => true,
        Expr::Apply(function, argument) => {
            function_body_is_net_safe(function, arity) && function_body_is_net_safe(argument, arity)
        }
        Expr::Lambda(_) | Expr::Access(_, _) => false,
    }
}

struct ClosedLowerer {
    net: NetBuilder<CoreNetData>,
    local_uses: Vec<Vec<Port>>,
}

impl ClosedLowerer {
    fn lower_function(arity: usize, body: Arc<Expr>) -> LiftedFunctionNet {
        let (template, capture_count) = Self::lower_template(arity, body);
        LiftedFunctionNet {
            runtime: template.instantiate_shared(),
            capture_count,
        }
    }

    fn lower_template(arity: usize, body: Arc<Expr>) -> (CoreInteractionNet, usize) {
        let mut lowerer = Self {
            net: NetBuilder::new(),
            local_uses: Vec::new(),
        };
        let body_boundary = lowerer.net.copy(1);
        lowerer.compile_into(&body, body_boundary.outputs[0]);

        let capture_count = lowerer.local_uses.len().saturating_sub(arity);
        let bind_count = capture_count + arity;
        let binds = lowerer.net.bind_spine(bind_count);
        lowerer.net.wire(binds.result, body_boundary.input);

        let uses = std::mem::take(&mut lowerer.local_uses);
        for index in 0..bind_count {
            let targets = uses.get(index).map(Vec::as_slice).unwrap_or_default();
            let bind_index = if index < arity {
                capture_count + arity - index - 1
            } else {
                index - arity
            };
            lowerer.distribute(binds.arguments[bind_index], targets);
        }

        (lowerer.net.finish(binds.input), capture_count)
    }

    fn compile_into(&mut self, expr: &Expr, target: Port) {
        match expr {
            Expr::Value(Value::Builtin(builtin)) => self.host_fn_into(
                crate::eval::builtin_host_fn(BuiltinCall::new(*builtin)),
                target,
            ),
            Expr::Value(Value::PartialBuiltin(call)) => {
                self.host_fn_into(crate::eval::builtin_host_fn(call.clone()), target)
            }
            Expr::Value(value) => self.data_into(CoreNetData::Value(value.clone()), target),
            Expr::List(items) => {
                let args = items.iter().map(Arc::as_ref).collect::<Vec<_>>();
                if args.is_empty() {
                    self.data_into(
                        CoreNetData::Value(Value::List(crate::core::List::empty())),
                        target,
                    );
                } else {
                    self.host_application_into(
                        crate::eval::list_host_fn(args.len(), Arc::from([])),
                        &args,
                        target,
                    );
                }
            }
            Expr::Apply(function, argument) => {
                let [application, argument_port, result] = self.net.bind();
                self.net.wire(result, target);
                self.compile_into(function, application);
                self.compile_into(argument, argument_port);
            }
            Expr::Lambda(_) => {
                unreachable!("nested compatibility lambdas are excluded from net lowering")
            }
            Expr::Local(index) => self.use_local(*index, target),
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
                self.host_application_into(
                    crate::eval::access_host_fn(Arc::from(keys), Arc::from([])),
                    &args,
                    target,
                );
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

    fn host_fn_into(&mut self, host_fn: HostFn<CoreNetData>, target: Port) {
        let function = self.net.unary_host_fn(host_fn);
        self.net.wire(function, target);
    }

    fn host_application_into(
        &mut self,
        host_fn: HostFn<CoreNetData>,
        args: &[&Expr],
        target: Port,
    ) {
        let mut output = self.net.unary_host_fn(host_fn);
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
        let (net, _) = ClosedLowerer::lower_template(1, Arc::new(Expr::Local(0)));
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
        let (net, _) = ClosedLowerer::lower_template(1, Arc::new(Expr::Value(unit())));
        assert!(net.nodes().iter().any(|node| matches!(node, Node::Erase)));
    }

    #[test]
    fn repeated_argument_lowers_to_one_binary_fan() {
        let body = Expr::Apply(Arc::new(Expr::Local(0)), Arc::new(Expr::Local(0)));
        let (net, _) = ClosedLowerer::lower_template(1, Arc::new(body));
        assert_eq!(
            net.nodes()
                .iter()
                .filter(|node| matches!(node, Node::Fan { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn curried_function_lowers_to_one_bind_chain() {
        let (net, _) = ClosedLowerer::lower_template(2, Arc::new(Expr::Local(1)));

        assert_eq!(
            net.nodes()
                .iter()
                .filter(|node| matches!(node, Node::Bind))
                .count(),
            2
        );
    }

    #[test]
    fn list_application_lowers_to_host_functions_not_callable_data() {
        let (net, _) = ClosedLowerer::lower_template(
            1,
            Arc::new(Expr::List(Arc::from([Arc::new(Expr::Local(0))]))),
        );

        assert!(
            net.nodes()
                .iter()
                .any(|node| matches!(node, Node::HostFn(_)))
        );
        assert!(!net.nodes().iter().any(|node| matches!(node, Node::Data(_))));
    }

    #[test]
    fn access_application_lowers_to_host_functions_not_callable_data() {
        let (net, _) = ClosedLowerer::lower_template(
            1,
            Arc::new(Expr::Access(
                Arc::new(Expr::Local(0)),
                Arc::from([KeyExpr::Key(Key::atom_from_text("answer"))]),
            )),
        );

        assert!(
            net.nodes()
                .iter()
                .any(|node| matches!(node, Node::HostFn(_)))
        );
        assert!(!net.nodes().iter().any(|node| matches!(node, Node::Data(_))));
    }

    #[test]
    fn closed_lowering_lifts_free_locals_into_leading_binds() {
        let closed = lower_function(1, Arc::new(Expr::Local(1)));

        assert_eq!(closed.capture_count, 1);
        let exposed_neighbor = closed
            .runtime
            .with(|runtime| runtime.interface_neighbor(runtime.exposed()));
        assert!(exposed_neighbor.is_some_and(Port::is_principal));
    }

    #[test]
    fn function_arity_distinguishes_arguments_from_captures() {
        let closed = lower_function(2, Arc::new(Expr::Local(1)));

        assert_eq!(closed.capture_count, 0);
    }
}
