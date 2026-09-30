//! Test-only expression fixtures and their interaction-net lowerer.
//! Every fixture is lowered before evaluation; this module does not provide a
//! second expression interpreter or evaluator-local environment.

use super::*;
use crate::core::RuntimeValueAccess;

pub(crate) struct ResumableTestValueDemand {
    handle: crate::evaluation::ClientDemandHandle,
}

impl ResumableTestValueDemand {
    pub(crate) fn new(context: &EvalContext, value: &Value) -> Self {
        let root = context
            .values()
            .construct_runtime_value_root(|access| access.duplicate_value(value));
        let handle = context
            .demand_whnf(root)
            .expect("test value demand should be admitted");
        Self { handle }
    }

    pub(crate) fn advance(&mut self, context: &EvalContext) -> Result<Value, EvaluationHalt> {
        let Some(result) = context.advance_client_demand_for_test(&mut self.handle)? else {
            panic!("test value demand is currently claimed by another thread")
        };
        let crate::evaluation::ClientDemandResult::Complete(value) = result else {
            unreachable!("terminal client failure is returned as an evaluation halt")
        };
        let poll = crate::evaluation::EvaluationPollContext::for_context(context);
        Ok(poll.evaluate(context, |evaluator| {
            evaluator.project_root(&value, |_, value| value)
        }))
    }
}

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

pub(super) enum TestKey {
    Key(Key),
    PathIndex(Arc<TestExpr>),
}

pub(crate) fn test_context() -> OwnedEvalContext {
    EvalContext::isolated(crate::core::test_value_factory())
}

pub(crate) fn annotation_test_context() -> OwnedEvalContext {
    // Annotation launchers are runtime/coordinator defaults. Their fixtures
    // need a private scheduler even though cheap semantic values come from
    // the shared test factory.
    EvalContext::private_closed(crate::core::test_value_factory())
}

pub(super) fn eval_closed_expr(expr: &TestExpr) -> Result<Value, EvaluationHalt> {
    let context = test_context();
    eval_closed_expr_in(&context, expr)
}

pub(super) fn eval_closed_expr_in(
    context: &EvalContext,
    expr: &TestExpr,
) -> Result<Value, EvaluationHalt> {
    let code = lower_test_function_code_in(context.values(), 0, expr);
    assert_eq!(code.capture_count(), 0, "test computation must be closed");
    let computation = Value::Lazy(LazyValue::from_net_computation(
        context.values(),
        NetValue::new(code.runtime().duplicate_for_test(context.values())),
    ));
    let mut value = context.evaluate_compatibility_whnf(&computation)?;
    while matches!(&value, Value::Lazy(lazy)
    if lazy.source_snapshot(context.values()).is_some_and(|source| {
        matches!(source, crate::core::LazySource::FunctionCall { .. })
    })) {
        value = context.evaluate_compatibility_whnf(&value)?;
    }
    Ok(value)
}

pub(super) fn lower_test_computation_value(expr: TestExpr) -> Value {
    let values = crate::core::test_value_factory();
    let code = lower_test_function_code(0, expr);
    assert_eq!(code.capture_count(), 0, "test computation must be closed");
    Value::Lazy(LazyValue::from_net_computation(
        &values,
        NetValue::new(code.runtime().duplicate_for_test(&values)),
    ))
}

pub(super) fn eval_key(value: &Value) -> Result<Key, EvaluationHalt> {
    let context = test_context();
    let singleton = apply_values(
        &context,
        Value::Builtin(Builtin::DictSingleton),
        vec![
            value.duplicate_for_test(context.values()),
            Value::Number(1.into()),
        ],
    )?;
    let Value::Dict(singleton) = context.evaluate_compatibility_whnf(&singleton)? else {
        unreachable!("dictionary singleton must produce a dictionary")
    };
    Ok(singleton
        .iter()
        .next()
        .expect("a defined singleton value retains its key")
        .0
        .clone())
}

pub(super) fn closed_function_value(arity: usize, body: TestExpr) -> Value {
    closed_function_value_in(&crate::core::test_value_factory(), arity, body)
}

pub(super) fn closed_function_value_in(
    values: &CoreValueFactory,
    arity: usize,
    body: TestExpr,
) -> Value {
    values
        .with_runtime_value_access(|access| closed_function_value_with_access(&access, arity, body))
}

pub(super) fn closed_function_value_with_access(
    access: &RuntimeValueAccess<'_>,
    arity: usize,
    body: TestExpr,
) -> Value {
    let code = lower_test_function_code_with_access(access, arity, &body);
    assert_eq!(code.capture_count(), 0, "test function must be closed");
    Value::Function(FunctionValue::new(
        NetValue::new(code.runtime().duplicate_in(access)),
        arity,
    ))
}

pub(super) fn lower_test_function_code(
    arity: usize,
    body: impl std::borrow::Borrow<TestExpr>,
) -> FunctionCode {
    lower_test_function_code_in(&crate::core::test_value_factory(), arity, body)
}

pub(super) fn lower_test_function_code_in(
    values: &CoreValueFactory,
    arity: usize,
    body: impl std::borrow::Borrow<TestExpr>,
) -> FunctionCode {
    values.with_runtime_value_access(|access| {
        lower_test_function_code_with_access(&access, arity, std::borrow::Borrow::borrow(&body))
    })
}

fn lower_test_function_code_with_access(
    access: &RuntimeValueAccess<'_>,
    arity: usize,
    body: &TestExpr,
) -> FunctionCode {
    let mut lowerer = FixtureNetLowerer {
        net: NetBuilder::new(),
        local_uses: Vec::new(),
        access,
    };
    let boundary = lowerer.net.copy(1);
    lowerer.compile_into(body, boundary.outputs[0]);
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
    let template = lowerer.net.finish(exposed);
    let runtime = access
        .construct_managed_core_net(template.instantiate_with(access))
        .expect("managed core-net representation must fit one collector run");
    FunctionCode::new(runtime, arity, capture_count)
}

struct FixtureNetLowerer<'access, 'scope> {
    net: NetBuilder<CoreSpecialization>,
    local_uses: Vec<Vec<Port>>,
    access: &'access RuntimeValueAccess<'scope>,
}

impl FixtureNetLowerer<'_, '_> {
    fn compile_into(&mut self, expr: &TestExpr, target: Port) {
        match expr {
            TestExpr::Value(value) => self.data_into(self.access.duplicate_value(value), target),
            TestExpr::List(items) => {
                if items.is_empty() {
                    self.data_into(Value::List(List::empty()), target);
                } else {
                    let arguments = items.iter().map(Arc::as_ref).collect::<Vec<_>>();
                    self.operator_application_into(
                        list_operator(self.access, arguments.len(), Arc::from([])),
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
                            NetValue::new(code.duplicate_runtime_in(self.access)),
                            code.arity(),
                        )),
                        target,
                    );
                } else {
                    let captures = captures.iter().map(Arc::as_ref).collect::<Vec<_>>();
                    self.operator_application_into(
                        function_capture_operator(self.access, code.clone(), Arc::from([])),
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
                    access_operator(self.access, Arc::from(path), Arc::from([])),
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
        let mut output = self.net.unary_operator(apply_arity_operator(
            self.access,
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
            self.data_into(self.access.duplicate_value(value), target);
            return;
        }
        let code = Arc::new(lower_test_function_code_with_access(self.access, 0, expr));
        if code.capture_count() == 0 {
            self.data_into(
                Value::Lazy(LazyValue::from_net_computation_in(
                    self.access,
                    NetValue::new(code.duplicate_runtime_in(self.access)),
                )),
                target,
            );
        } else {
            let captures = (0..code.capture_count())
                .map(TestExpr::Local)
                .collect::<Vec<_>>();
            let captures = captures.iter().collect::<Vec<_>>();
            self.operator_application_into(
                computation_capture_operator(self.access, code, Arc::from([])),
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
