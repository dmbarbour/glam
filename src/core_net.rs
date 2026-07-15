//! Lowering from syntax-independent core expressions into generic nets.

use std::sync::Arc;

use crate::core::{
    BuiltinCall, DeferredValue, Expr, FunctionCode, FunctionValue, IVar, Key, KeyExpr, NetValue,
    Value,
};
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
    Deferred(Arc<DeferredValue>),
    Future(IVar),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreOperator {
    ApplyArity {
        arity: usize,
        supplied: Arc<[Value]>,
    },
    FunctionCaptures {
        code: Arc<FunctionCode>,
        supplied: Arc<[Value]>,
    },
    ComputationCaptures {
        code: Arc<FunctionCode>,
        supplied: Arc<[Value]>,
    },
    Dict {
        keys: Arc<[Key]>,
        supplied: Arc<[Value]>,
    },
    Builtin(BuiltinCall),
    Applicable(Value),
    List {
        arity: usize,
        supplied: Arc<[Value]>,
    },
    Access {
        path: Arc<[CoreDataKey]>,
        supplied: Arc<[Value]>,
    },
    Error(Arc<str>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreSpecialization;

pub type CoreInteractionNet = InteractionNet<CoreSpecialization>;
pub type CoreRuntimeNet = SharedRuntimeNet<CoreSpecialization>;

#[derive(Debug, Clone)]
pub struct LiftedFunctionNet {
    pub runtime: CoreRuntimeNet,
    pub capture_count: usize,
}

/// Lambda-lifts every local outside `arity` into an explicit leading bind,
/// leaving one closed shared net. Capture binds precede the function's ordinary
/// argument binds and are supplied immediately by the enclosing lowerer.
pub(crate) fn lower_function(arity: usize, body: Arc<Expr>) -> LiftedFunctionNet {
    ClosedLowerer::lower_function(arity, body)
}

pub(crate) fn lower_function_code(arity: usize, body: Arc<Expr>) -> FunctionCode {
    let lifted = lower_function(arity, body);
    FunctionCode::new(lifted.runtime, arity, lifted.capture_count)
}

pub(crate) fn lower_closed_function_value(arity: usize, body: Expr) -> Value {
    let code = lower_function_code(arity, Arc::new(body));
    assert_eq!(
        code.capture_count(),
        0,
        "closed function helper cannot lift captures"
    );
    Value::Function(FunctionValue::new(
        NetValue::new(code.runtime().clone()),
        arity,
    ))
}

struct ClosedLowerer {
    net: NetBuilder<CoreSpecialization>,
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
        if bind_count == 0 {
            return (lowerer.net.finish(body_boundary.input), 0);
        }
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
            Expr::Value(Value::Dict(dict)) if !dict.is_empty() => {
                let entries = dict.iter().collect::<Vec<_>>();
                let keys = entries
                    .iter()
                    .map(|(key, _)| (*key).clone())
                    .collect::<Vec<_>>();
                let values = entries
                    .into_iter()
                    .map(|(_, value)| value)
                    .collect::<Vec<_>>();
                self.lazy_operator_application_into(
                    crate::eval::dict_operator(Arc::from(keys), Arc::from([])),
                    &values,
                    target,
                );
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
                    self.operator_application_into(
                        crate::eval::list_operator(args.len(), Arc::from([])),
                        &args,
                        target,
                    );
                }
            }
            Expr::Apply(_, _) => {
                let (function, arguments) = application_spine(expr);
                self.semantic_application_into(function, &arguments, target);
            }
            Expr::Function { code, captures } => {
                debug_assert_eq!(code.capture_count(), captures.len());
                if captures.is_empty() {
                    self.data_into(
                        CoreNetData::Value(Value::Function(FunctionValue::new(
                            NetValue::new(code.runtime().clone()),
                            code.arity(),
                        ))),
                        target,
                    );
                } else {
                    let captures = captures.iter().map(Arc::as_ref).collect::<Vec<_>>();
                    self.operator_application_into(
                        crate::eval::function_capture_operator(code.clone(), Arc::from([])),
                        &captures,
                        target,
                    );
                }
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
                self.operator_application_into(
                    crate::eval::access_operator(Arc::from(keys), Arc::from([])),
                    &args,
                    target,
                );
            }
            Expr::Deferred(value) => self.data_into(CoreNetData::Deferred(value.clone()), target),
            Expr::Future(value) => self.data_into(CoreNetData::Future(value.clone()), target),
            Expr::Error(message) => {
                let [input, output] = self.net.operator(CoreOperator::Error(message.clone()));
                let trigger = self.net.data(CoreNetData::Value(Value::Dict(
                    crate::core::Dict::new_sync(),
                )));
                self.net.wire(input, trigger);
                self.net.wire(output, target);
            }
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

    fn operator_application_into(&mut self, operator: CoreOperator, args: &[&Expr], target: Port) {
        let mut output = self.net.unary_operator(operator);
        for argument in args {
            let [application, argument_port, result] = self.net.bind();
            self.net.wire(output, application);
            self.compile_into(argument, argument_port);
            output = result;
        }
        self.net.wire(output, target);
    }

    fn lazy_operator_application_into(
        &mut self,
        operator: CoreOperator,
        args: &[&Value],
        target: Port,
    ) {
        let mut output = self.net.unary_operator(operator);
        for argument in args {
            let [application, argument_port, result] = self.net.bind();
            self.net.wire(output, application);
            self.compile_lazy_value_into(argument, argument_port);
            output = result;
        }
        self.net.wire(output, target);
    }

    fn semantic_application_into(&mut self, function: &Expr, arguments: &[&Expr], target: Port) {
        let mut output = self.net.unary_operator(crate::eval::apply_arity_operator(
            arguments.len(),
            Arc::from([]),
        ));
        let [application, function_port, result] = self.net.bind();
        self.net.wire(output, application);
        self.compile_into(function, function_port);
        output = result;
        for argument in arguments {
            let [application, argument_port, result] = self.net.bind();
            self.net.wire(output, application);
            self.compile_lazy_expr_into(argument, argument_port);
            output = result;
        }
        self.net.wire(output, target);
    }

    fn compile_lazy_value_into(&mut self, value: &Value, target: Port) {
        if !matches!(value, Value::Expr(_)) {
            self.data_into(CoreNetData::Value(value.clone()), target);
            return;
        }
        let expr = value_to_expr(value.clone());
        if matches!(expr, Expr::Value(_)) {
            self.compile_into(&expr, target);
            return;
        }
        self.compile_lazy_expr_into(&expr, target);
    }

    fn compile_lazy_expr_into(&mut self, expr: &Expr, target: Port) {
        if let Expr::Value(value) = expr {
            self.data_into(CoreNetData::Value(value.clone()), target);
            return;
        }
        let code = Arc::new(lower_function_code(0, Arc::new(expr.clone())));
        if code.capture_count() == 0 {
            self.data_into(
                CoreNetData::Value(Value::Expr(crate::core::Thunk::from_net(NetValue::new(
                    code.runtime().clone(),
                )))),
                target,
            );
            return;
        }
        let captures = (0..code.capture_count())
            .map(Expr::Local)
            .collect::<Vec<_>>();
        let captures = captures.iter().collect::<Vec<_>>();
        self.operator_application_into(
            crate::eval::computation_capture_operator(code, Arc::from([])),
            &captures,
            target,
        );
    }

    fn distribute(&mut self, source: Port, targets: &[Port]) {
        let copy = self.net.copy(targets.len());
        self.net.wire(source, copy.input);
        for (output, target) in copy.outputs.into_iter().zip(targets) {
            self.net.wire(output, *target);
        }
    }
}

fn value_to_expr(value: Value) -> Expr {
    match value {
        Value::Expr(thunk)
            if thunk.env().is_some_and(|env| env.is_empty())
                && expr_contains_local(thunk.expr().unwrap()) =>
        {
            thunk.expr().unwrap().as_ref().clone()
        }
        value => Expr::Value(value),
    }
}

fn expr_contains_local(expr: &Expr) -> bool {
    match expr {
        Expr::Value(_) | Expr::Deferred(_) | Expr::Future(_) | Expr::Error(_) => false,
        Expr::List(items) => items.iter().any(|item| expr_contains_local(item)),
        Expr::Apply(function, argument) => {
            expr_contains_local(function) || expr_contains_local(argument)
        }
        Expr::Function { captures, .. } => {
            captures.iter().any(|capture| expr_contains_local(capture))
        }
        Expr::Local(_) => true,
        Expr::Access(base, path) => {
            expr_contains_local(base)
                || path.iter().any(|key| match key {
                    KeyExpr::Key(_) => false,
                    KeyExpr::Index(expr) | KeyExpr::PathIndex(expr) => expr_contains_local(expr),
                })
        }
    }
}

fn application_spine(mut expr: &Expr) -> (&Expr, Vec<&Expr>) {
    let mut arguments = Vec::new();
    while let Expr::Apply(function, argument) = expr {
        arguments.push(argument.as_ref());
        expr = function;
    }
    arguments.reverse();
    (expr, arguments)
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
    fn list_application_lowers_to_operators_not_callable_data() {
        let (net, _) = ClosedLowerer::lower_template(
            1,
            Arc::new(Expr::List(Arc::from([Arc::new(Expr::Local(0))]))),
        );

        assert!(
            net.nodes()
                .iter()
                .any(|node| matches!(node, Node::Operator(_)))
        );
        assert!(!net.nodes().iter().any(|node| matches!(node, Node::Data(_))));
    }

    #[test]
    fn access_application_lowers_to_operators_not_callable_data() {
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
                .any(|node| matches!(node, Node::Operator(_)))
        );
        assert!(!net.nodes().iter().any(|node| matches!(node, Node::Data(_))));
    }

    #[test]
    fn error_lowering_builds_an_operator_that_can_only_get_stuck() {
        let (net, _) = ClosedLowerer::lower_template(
            0,
            Arc::new(Expr::Error(Arc::from("deliberate failure"))),
        );

        assert!(net.nodes().iter().any(|node| matches!(
            node,
            Node::Operator(CoreOperator::Error(message)) if message.as_ref() == "deliberate failure"
        )));
        assert_eq!(net.active_pairs().len(), 1);
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
