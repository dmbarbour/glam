//! Test-only expression fixtures and their interaction-net lowerer.
//! Every fixture is lowered before evaluation; this module does not provide a
//! second expression interpreter or evaluator-local environment.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TestExpr {
    Value(Value),
    List(Arc<[Arc<TestExpr>]>),
    Apply(Arc<TestExpr>, Arc<TestExpr>),
    Function {
        code: Arc<FunctionCode>,
        captures: Arc<[Arc<TestExpr>]>,
    },
    Local(usize),
    Access(Arc<TestExpr>, Arc<[TestKey]>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TestKey {
    Key(Key),
    PathIndex(Arc<TestExpr>),
}

pub(crate) fn test_context() -> EvalContext {
    EvalContext::standalone()
}

pub(super) fn eval_closed_expr(expr: &TestExpr) -> Result<Value, EvalError> {
    let context = test_context();
    let mut value = eval_value(&context, &lower_test_computation_value(expr.clone()))?;
    while matches!(&value, Value::Lazy(lazy)
        if matches!(lazy.source(), crate::core::LazySource::FunctionCall { .. }))
    {
        value = eval_value(&context, &value)?;
    }
    Ok(value)
}

pub(super) fn lower_test_computation_value(expr: TestExpr) -> Value {
    let code = lower_test_function_code(0, expr);
    assert_eq!(code.capture_count(), 0, "test computation must be closed");
    Value::Lazy(LazyValue::from_net_computation(NetValue::new(
        code.runtime().clone(),
    )))
}

pub(super) fn eval_key(value: &Value) -> Result<Key, EvalError> {
    let context = test_context();
    let value = force_value_shell(&context, value)?;
    value_to_key(&context, &value)
}

pub(super) fn closed_function_value(arity: usize, body: TestExpr) -> Value {
    let code = lower_test_function_code(arity, body);
    assert_eq!(code.capture_count(), 0, "test function must be closed");
    Value::Function(FunctionValue::new(
        NetValue::new(code.runtime().clone()),
        arity,
    ))
}

pub(super) fn lower_test_function_code(arity: usize, body: TestExpr) -> FunctionCode {
    let mut lowerer = FixtureNetLowerer {
        net: NetBuilder::new(),
        local_uses: Vec::new(),
    };
    let boundary = lowerer.net.copy(1);
    lowerer.compile_into(&body, boundary.outputs[0]);
    let capture_count = lowerer.local_uses.len().saturating_sub(arity);
    let bind_count = arity + capture_count;
    let exposed = if bind_count == 0 {
        boundary.input
    } else {
        let binds = lowerer.net.bind_spine(bind_count);
        lowerer.net.wire(binds.result, boundary.input);
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
        binds.input
    };
    let runtime = lowerer.net.finish(exposed).instantiate_shared();
    FunctionCode::new(runtime, arity, capture_count)
}

struct FixtureNetLowerer {
    net: NetBuilder<CoreSpecialization>,
    local_uses: Vec<Vec<Port>>,
}

impl FixtureNetLowerer {
    fn compile_into(&mut self, expr: &TestExpr, target: Port) {
        match expr {
            TestExpr::Value(value) => self.data_into(value.clone(), target),
            TestExpr::List(items) => {
                if items.is_empty() {
                    self.data_into(Value::List(List::empty()), target);
                } else {
                    let arguments = items.iter().map(Arc::as_ref).collect::<Vec<_>>();
                    self.operator_application_into(
                        list_operator(arguments.len(), Arc::from([])),
                        &arguments,
                        target,
                    );
                }
            }
            TestExpr::Apply(_, _) => {
                let mut head = expr;
                let mut arguments = Vec::new();
                while let TestExpr::Apply(function, argument) = head {
                    arguments.push(argument.as_ref());
                    head = function;
                }
                arguments.reverse();
                self.semantic_application_into(head, &arguments, target);
            }
            TestExpr::Function { code, captures } => {
                if captures.is_empty() {
                    self.data_into(
                        Value::Function(FunctionValue::new(
                            NetValue::new(code.runtime().clone()),
                            code.arity(),
                        )),
                        target,
                    );
                } else {
                    let captures = captures.iter().map(Arc::as_ref).collect::<Vec<_>>();
                    self.operator_application_into(
                        function_capture_operator(code.clone(), Arc::from([])),
                        &captures,
                        target,
                    );
                }
            }
            TestExpr::Local(index) => {
                if self.local_uses.len() <= *index {
                    self.local_uses.resize_with(index + 1, Vec::new);
                }
                self.local_uses[*index].push(target);
            }
            TestExpr::Access(base, path) => {
                let mut arguments = vec![base.as_ref()];
                let path = path
                    .iter()
                    .map(|part| match part {
                        TestKey::Key(key) => CoreDataKey::Key(key.clone()),
                        TestKey::PathIndex(expr) => {
                            arguments.push(expr);
                            CoreDataKey::PathIndex
                        }
                    })
                    .collect::<Vec<_>>();
                self.operator_application_into(
                    access_operator(Arc::from(path), Arc::from([])),
                    &arguments,
                    target,
                );
            }
        }
    }

    fn semantic_application_into(
        &mut self,
        function: &TestExpr,
        arguments: &[&TestExpr],
        target: Port,
    ) {
        let mut output = self
            .net
            .unary_operator(apply_arity_operator(arguments.len(), Arc::from([])));
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
        arguments: &[&TestExpr],
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

    fn compile_lazy_into(&mut self, expr: &TestExpr, target: Port) {
        if let TestExpr::Value(value) = expr {
            self.data_into(value.clone(), target);
            return;
        }
        let code = Arc::new(lower_test_function_code(0, expr.clone()));
        if code.capture_count() == 0 {
            self.data_into(
                Value::Lazy(LazyValue::from_net_computation(NetValue::new(
                    code.runtime().clone(),
                ))),
                target,
            );
        } else {
            let captures = (0..code.capture_count())
                .map(TestExpr::Local)
                .collect::<Vec<_>>();
            let captures = captures.iter().collect::<Vec<_>>();
            self.operator_application_into(
                computation_capture_operator(code, Arc::from([])),
                &captures,
                target,
            );
        }
    }

    fn data_into(&mut self, value: Value, target: Port) {
        let data = self.net.data(value);
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
