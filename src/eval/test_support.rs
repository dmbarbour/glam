//! Test-only expression fixtures, their direct interpreter, and net lowerer.
//! Fixture locals never cross into production evaluator state: deferred test
//! values close over any locals they need before entering ordinary evaluation.

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
#[allow(dead_code)]
pub(super) enum TestKey {
    Key(Key),
    Index(Arc<TestExpr>),
    PathIndex(Arc<TestExpr>),
}

pub(super) fn eval_closed_expr(expr: &TestExpr) -> Result<Value, EvalError> {
    eval_expr(expr, &[])
}

pub(super) fn eval_expr(expr: &TestExpr, local_env: &[Value]) -> Result<Value, EvalError> {
    match expr {
        TestExpr::Value(value) => eval_value(value),
        TestExpr::List(items) => {
            let mut list = List::empty();
            for item in items.iter() {
                let value = eval_expr(item, local_env)?;
                list = List::concat(list, list_literal_segment(value));
            }
            Ok(Value::List(list))
        }
        TestExpr::Apply(function, argument) => eval_apply(function, argument, local_env),
        TestExpr::Function { code, captures } => {
            let captures = captures
                .iter()
                .map(|capture| thunk_value(capture, local_env))
                .collect();
            instantiate_function(code, captures)
        }
        TestExpr::Local(index) => eval_local(*index, local_env),
        TestExpr::Access(base, path) => {
            let base = eval_expr(base, local_env)?;
            resolve_key_path(base, path, path, local_env)
        }
    }
}

pub(super) fn eval_key(value: &Value) -> Result<Key, EvalError> {
    let value = force_value_shell(value)?;
    value_to_key(&value)
}

fn format_name(path: &[TestKey]) -> String {
    path.iter()
        .map(format_name_key_expr)
        .collect::<Vec<_>>()
        .join(".")
}

fn format_name_key_expr(key: &TestKey) -> String {
    match key {
        TestKey::Key(key) => format_name_part(key),
        TestKey::Index(_) => "[index]".to_owned(),
        TestKey::PathIndex(_) => "(path-index)".to_owned(),
    }
}

fn eval_local(index: usize, local_env: &[Value]) -> Result<Value, EvalError> {
    let Some(value) = local_env.get(
        local_env
            .len()
            .checked_sub(index + 1)
            .ok_or_else(|| EvalError::new(format!("local `{index}` is out of scope")))?,
    ) else {
        return Err(EvalError::new(format!("local `{index}` is out of scope")));
    };

    eval_value(value)
}

fn resolve_key_path(
    current: Value,
    remaining: &[TestKey],
    full_path: &[TestKey],
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let Some((head, rest)) = remaining.split_first() else {
        return eval_value(&current);
    };

    let expanded = expand_key_expr(head, local_env)?;
    let next = resolve_expanded_keys(current, &expanded, full_path, remaining)?;
    resolve_key_path(next, rest, full_path, local_env)
}

fn resolve_expanded_keys(
    mut current: Value,
    expanded: &[Key],
    full_path: &[TestKey],
    remaining: &[TestKey],
) -> Result<Value, EvalError> {
    for key in expanded {
        let dict = force_dict_shell(&current, full_path, remaining)?;
        current = dict
            .get(key)
            .cloned()
            .unwrap_or_else(|| Value::Dict(crate::core::Dict::new_sync()));
    }
    Ok(current)
}

fn force_dict_shell(
    value: &Value,
    full_path: &[TestKey],
    remaining: &[TestKey],
) -> Result<crate::core::Dict, EvalError> {
    match force_value_shell(value)? {
        Value::Dict(dict) => Ok(dict),
        _ => {
            let traversed = &full_path[..full_path.len() - remaining.len()];
            let culprit = if traversed.is_empty() {
                full_path
            } else {
                traversed
            };
            Err(EvalError::new(format!(
                "name `{}` is not a dictionary",
                format_name(culprit)
            )))
        }
    }
}

fn expand_key_expr(key: &TestKey, local_env: &[Value]) -> Result<Vec<Key>, EvalError> {
    match key {
        TestKey::Key(key) => Ok(vec![key.clone()]),
        TestKey::Index(expr) => {
            let value = thunk_value(expr, local_env);
            let value = force_value_shell(&value)?;
            Ok(vec![value_to_key(&value)?])
        }
        TestKey::PathIndex(expr) => eval_key_path_list(&thunk_value(expr, local_env)),
    }
}

pub(super) fn eval_apply(
    function: &TestExpr,
    argument: &TestExpr,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let mut head = function;
    let mut arguments = vec![argument];
    while let TestExpr::Apply(next, argument) = head {
        arguments.push(argument);
        head = next;
    }
    arguments.reverse();

    let function = eval_expr(head, local_env)?;
    let arguments = arguments
        .into_iter()
        .map(|argument| thunk_value(argument, local_env))
        .collect::<Vec<_>>();
    let result = apply_values(function, arguments)?;
    match &result {
        // A source-level function application evaluates its call stage, just
        // as the former closure evaluator evaluated the body. Do not
        // recursively force an arbitrary expression returned by that body:
        // annotations and aggregate members deliberately return lazy values.
        Value::Lazy(thunk) if thunk.function_call().is_some() => eval_lazy(thunk),
        _ => Ok(result),
    }
}

#[cfg(test)]
pub(super) fn thunk_value(expr: &TestExpr, local_env: &[Value]) -> Value {
    match expr {
        TestExpr::Value(value) => value.clone(),
        _ => {
            let expr = expr.clone();
            let local_env = local_env.to_vec();
            Value::deferred("test expression", move || {
                eval_expr(&expr, &local_env).map_err(|error| error.to_string())
            })
        }
    }
}

#[cfg(test)]
pub(super) fn closed_function_value(arity: usize, body: TestExpr) -> Value {
    let code = lower_test_function_code(arity, body);
    assert_eq!(code.capture_count(), 0, "test function must be closed");
    Value::Function(FunctionValue::new(
        NetValue::new(code.runtime().clone()),
        arity,
    ))
}

#[cfg(test)]
pub(super) fn lower_test_function_code(arity: usize, body: TestExpr) -> FunctionCode {
    let mut lowerer = TestExprLowerer {
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

#[cfg(test)]
struct TestExprLowerer {
    net: NetBuilder<CoreSpecialization>,
    local_uses: Vec<Vec<Port>>,
}

#[cfg(test)]
impl TestExprLowerer {
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
                        TestKey::Index(expr) => {
                            arguments.push(expr);
                            CoreDataKey::Index
                        }
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
