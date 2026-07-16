//! Direct lowering from resolved g-syntax expressions to closed interaction nets.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::core::{FunctionCode, FunctionValue, NetValue, Value};
use crate::core_net::{CoreDataKey, CoreOperator, CoreSpecialization};
use crate::interaction_net::{NetBuilder, Port};

use super::resolved::{BindingId, ResolvedExpr, ResolvedPathPart};

/// Consumes one closed front-end semantic expression and lowers it directly to
/// a shared interaction-net computation. No syntax-shaped value survives this
/// boundary.
pub(super) fn lower_resolved_expr(expr: ResolvedExpr<Value>) -> Value {
    match expr {
        ResolvedExpr::Embedded(value) | ResolvedExpr::Provided(value) => value,
        expr => {
            let (code, captures) = ResolvedNetLowerer::lower_code(Vec::new(), expr);
            assert!(
                captures.is_empty(),
                "a value leaving g-syntax must be a closed interaction net"
            );
            Value::Lazy(crate::core::LazyValue::from_net_computation(NetValue::new(
                code.runtime().clone(),
            )))
        }
    }
}

pub(super) struct ResolvedNetLowerer {
    net: NetBuilder<CoreSpecialization>,
    local_uses: BTreeMap<BindingId, Vec<Port>>,
}

impl ResolvedNetLowerer {
    pub(super) fn lower_code(
        parameters: Vec<BindingId>,
        body: ResolvedExpr<Value>,
    ) -> (FunctionCode, Vec<BindingId>) {
        let mut captures = body.free_bindings();
        for parameter in &parameters {
            captures.remove(parameter);
        }
        let captures = captures.into_iter().collect::<Vec<_>>();
        let mut inputs = captures.clone();
        inputs.extend(parameters.iter().copied());
        let runtime = Self::lower_template(inputs, body).instantiate_shared();
        (
            FunctionCode::new(runtime, parameters.len(), captures.len()),
            captures,
        )
    }

    pub(super) fn lower_template(
        inputs: Vec<BindingId>,
        body: ResolvedExpr<Value>,
    ) -> crate::core_net::CoreInteractionNet {
        let mut lowerer = Self {
            net: NetBuilder::new(),
            local_uses: BTreeMap::new(),
        };
        let body_boundary = lowerer.net.copy(1);
        lowerer.compile_into(body, body_boundary.outputs[0]);

        if inputs.is_empty() {
            assert!(
                lowerer.local_uses.is_empty(),
                "closed net body contains an unbound local"
            );
            return lowerer.net.finish(body_boundary.input);
        }

        let binds = lowerer.net.bind_spine(inputs.len());
        lowerer.net.wire(binds.result, body_boundary.input);
        for (binding, source) in inputs.into_iter().zip(binds.arguments) {
            let targets = lowerer.local_uses.remove(&binding).unwrap_or_default();
            lowerer.distribute(source, &targets);
        }
        assert!(
            lowerer.local_uses.is_empty(),
            "lowered function body contains an unbound local"
        );
        lowerer.net.finish(binds.input)
    }

    fn compile_into(&mut self, expr: ResolvedExpr<Value>, target: Port) {
        match expr {
            ResolvedExpr::Embedded(value) | ResolvedExpr::Provided(value) => {
                self.data_into(value, target);
            }
            ResolvedExpr::Local(binding) => {
                self.local_uses.entry(binding).or_default().push(target)
            }
            ResolvedExpr::List(items) => {
                if items.is_empty() {
                    self.data_into(Value::List(crate::core::List::empty()), target);
                } else {
                    let arity = items.len();
                    self.lazy_operator_application_into(
                        crate::eval::list_operator(arity, Arc::from([])),
                        items,
                        target,
                    );
                }
            }
            ResolvedExpr::Access { base, path } => {
                let mut arguments = vec![*base];
                let path = path
                    .into_iter()
                    .map(|part| match part {
                        ResolvedPathPart::Key(key) => CoreDataKey::Key(key),
                        ResolvedPathPart::Index(expr) => {
                            arguments.push(*expr);
                            CoreDataKey::Index
                        }
                        ResolvedPathPart::PathIndex(expr) => {
                            arguments.push(*expr);
                            CoreDataKey::PathIndex
                        }
                    })
                    .collect::<Vec<_>>();
                self.operator_application_into(
                    crate::eval::access_operator(Arc::from(path), Arc::from([])),
                    arguments,
                    target,
                );
            }
            ResolvedExpr::Lambda { parameters, body } => {
                self.function_into(parameters, *body, target);
            }
            ResolvedExpr::Apply {
                function,
                arguments,
            } => self.semantic_application_into(*function, arguments, target),
            ResolvedExpr::ApplyLambda {
                parameters,
                body,
                arguments,
            } => {
                // Preserve the grouped redex as one lowering operation. The
                // function template is emitted once and the complete argument
                // spine is attached without intermediate host expressions.
                self.semantic_application_into(
                    ResolvedExpr::Lambda { parameters, body },
                    arguments,
                    target,
                );
            }
        }
    }

    fn function_into(
        &mut self,
        parameters: Vec<BindingId>,
        body: ResolvedExpr<Value>,
        target: Port,
    ) {
        assert!(!parameters.is_empty(), "a function must bind an argument");
        let (code, captures) = Self::lower_code(parameters, body);
        let code = Arc::new(code);
        if captures.is_empty() {
            self.data_into(
                Value::Function(FunctionValue::new(
                    NetValue::new(code.runtime().clone()),
                    code.arity(),
                )),
                target,
            );
        } else {
            let operator = crate::eval::function_capture_operator(code, Arc::from([]));
            self.binding_operator_application_into(operator, captures, target);
        }
    }

    fn semantic_application_into(
        &mut self,
        function: ResolvedExpr<Value>,
        arguments: Vec<ResolvedExpr<Value>>,
        target: Port,
    ) {
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
            self.compile_lazy_into(argument, argument_port);
            output = result;
        }
        self.net.wire(output, target);
    }

    fn operator_application_into(
        &mut self,
        operator: CoreOperator,
        arguments: Vec<ResolvedExpr<Value>>,
        target: Port,
    ) {
        let mut output = self.net.unary_operator(operator);
        for argument in arguments {
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
        arguments: Vec<ResolvedExpr<Value>>,
        target: Port,
    ) {
        let mut output = self.net.unary_operator(operator);
        for argument in arguments {
            let [application, argument_port, result] = self.net.bind();
            self.net.wire(output, application);
            self.compile_lazy_into(argument, argument_port);
            output = result;
        }
        self.net.wire(output, target);
    }

    fn binding_operator_application_into(
        &mut self,
        operator: CoreOperator,
        bindings: Vec<BindingId>,
        target: Port,
    ) {
        let mut output = self.net.unary_operator(operator);
        for binding in bindings {
            let [application, argument_port, result] = self.net.bind();
            self.net.wire(output, application);
            self.local_uses
                .entry(binding)
                .or_default()
                .push(argument_port);
            output = result;
        }
        self.net.wire(output, target);
    }

    fn compile_lazy_into(&mut self, expr: ResolvedExpr<Value>, target: Port) {
        match expr {
            ResolvedExpr::Embedded(value) | ResolvedExpr::Provided(value) => {
                self.data_into(value, target);
            }
            expr => {
                let (code, captures) = Self::lower_code(Vec::new(), expr);
                let code = Arc::new(code);
                if captures.is_empty() {
                    self.data_into(
                        Value::Lazy(crate::core::LazyValue::from_net_computation(NetValue::new(
                            code.runtime().clone(),
                        ))),
                        target,
                    );
                } else {
                    let operator = crate::eval::computation_capture_operator(code, Arc::from([]));
                    self.binding_operator_application_into(operator, captures, target);
                }
            }
        }
    }

    fn data_into(&mut self, data: Value, target: Port) {
        let data = self.net.data(data);
        self.net.wire(data, target);
    }

    fn distribute(&mut self, source: Port, targets: &[Port]) {
        let copy = self.net.copy(targets.len());
        self.net.wire(source, copy.input);
        for (output, target) in copy.outputs.into_iter().zip(targets) {
            self.net.wire(output, *target);
        }
    }
}
