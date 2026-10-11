use super::*;
use crate::core::{RuntimeValueAccess, WrappedOperator};
use crate::evaluation::EvaluationValueAccess;
use crate::interaction_net::{CompiledNet, CompiledOperand};

pub(crate) fn apply_arity_operator(
    _access: &RuntimeValueAccess<'_>,
    arity: usize,
    supplied: Arc<[Value]>,
) -> CoreOperator {
    assert!(supplied.len() < arity + 1);
    CoreOperator::ApplyArity { arity, supplied }
}

pub(crate) fn function_capture_operator(
    _access: &RuntimeValueAccess<'_>,
    code: Arc<FunctionCode>,
    supplied: Arc<[Value]>,
) -> CoreOperator {
    assert!(code.capture_count() > 0);
    assert!(supplied.len() < code.capture_count());
    CoreOperator::FunctionCaptures { code, supplied }
}

pub(crate) fn computation_capture_operator(
    _access: &RuntimeValueAccess<'_>,
    code: Arc<FunctionCode>,
    supplied: Arc<[Value]>,
) -> CoreOperator {
    assert_eq!(code.arity(), 0);
    assert!(code.capture_count() > 0);
    assert!(supplied.len() < code.capture_count());
    CoreOperator::ComputationCaptures { code, supplied }
}

pub(crate) fn dict_operator(
    _access: &RuntimeValueAccess<'_>,
    keys: Arc<[Key]>,
    supplied: Arc<[Value]>,
) -> CoreOperator {
    assert!(!keys.is_empty());
    assert!(supplied.len() < keys.len());
    CoreOperator::Dict { keys, supplied }
}

pub(crate) fn builtin_operator(
    _access: &RuntimeValueAccess<'_>,
    call: BuiltinCall,
) -> CoreOperator {
    CoreOperator::Builtin(call)
}

pub(super) fn applicable_operator(
    _access: &RuntimeValueAccess<'_>,
    function: Value,
) -> CoreOperator {
    CoreOperator::Applicable(function)
}

/// The most items one list operator collects. Longer literals compose
/// shorter lists with `++`: an operator collecting n items would take n
/// partial applications, each copying the items so far.
pub(crate) const LIST_OPERATOR_MAX_ARITY: usize = 8;

pub(crate) fn list_operator(
    _access: &RuntimeValueAccess<'_>,
    arity: usize,
    supplied: Arc<[Value]>,
) -> CoreOperator {
    assert!(supplied.len() < arity);
    assert!(
        arity <= LIST_OPERATOR_MAX_ARITY,
        "a list operator collects at most {LIST_OPERATOR_MAX_ARITY} items"
    );
    CoreOperator::List { arity, supplied }
}

pub(crate) fn access_operator(
    _access: &RuntimeValueAccess<'_>,
    path: Arc<[CoreDataKey]>,
    supplied: Arc<[Value]>,
) -> CoreOperator {
    assert!(supplied.len() < crate::core_net::access_arity(&path));
    CoreOperator::Access { path, supplied }
}

/// The operator a wrapped function applies, with no operands yet.
pub(super) fn wrapped_core_operator(
    access: &RuntimeValueAccess<'_>,
    operator: &WrappedOperator,
) -> CoreOperator {
    match operator {
        WrappedOperator::Access(path) => access_operator(access, path.clone(), Arc::from([])),
    }
}

/// Applies a function that only wraps `operator` to all of its arguments,
/// giving what its net would: the operator's result.
pub(super) fn apply_wrapped_operator(
    access: &RuntimeValueAccess<'_>,
    operator: &WrappedOperator,
    arguments: Vec<Value>,
) -> Value {
    assert_eq!(
        arguments.len(),
        operator.arity(),
        "a wrapped operator applies to all of its function's arguments"
    );
    match operator {
        WrappedOperator::Access(path) => access_value(access, path.clone(), arguments),
    }
}

/// Applies a compiled net to its inputs, giving what the net would: each
/// step's operator applied to its operands in order (`perf-compiled-nets`).
pub(super) fn apply_compiled(
    access: &EvaluationValueAccess<'_>,
    compiled: &CompiledNet<CoreSpecialization>,
    inputs: &[Value],
) -> Value {
    assert_eq!(
        inputs.len(),
        compiled.inputs(),
        "a compiled net applies to all its inputs"
    );
    #[cfg(feature = "glam-prof")]
    access
        .values()
        .values()
        .interaction_net_profile()
        .record_reduction(crate::interaction_net::profiling::ReductionEvent::CompiledApplication);
    let mut results = Vec::with_capacity(compiled.steps().len());
    for step in compiled.steps() {
        let mut operator = None;
        let mut result = None;
        for operand in &step.operands {
            debug_assert!(
                result.is_none(),
                "a compiled step yields at its last operand"
            );
            let current = operator.as_ref().unwrap_or(&step.operator);
            let operand = compiled_operand(access, operand, inputs, &results);
            match apply_operator(access, current, operand)
                .expect("a compiled step's operator takes each of its operands")
            {
                OperatorYield::Operator(next) => operator = Some(next),
                OperatorYield::Data(data) => result = Some(data),
            }
        }
        results.push(result.expect("a compiled step's operands saturate its operator"));
    }
    access.values().duplicate_value(compiled_operand(
        access,
        compiled.result(),
        inputs,
        &results,
    ))
}

fn compiled_operand<'value>(
    _access: &EvaluationValueAccess<'_>,
    operand: &'value CompiledOperand<CoreSpecialization>,
    inputs: &'value [Value],
    results: &'value [Value],
) -> &'value Value {
    match operand {
        CompiledOperand::Input(input) => &inputs[*input],
        CompiledOperand::Constant(value) => value,
        CompiledOperand::Step(step) => &results[*step],
    }
}

/// The value an access operator yields once it has all its operands.
fn access_value(
    access: &RuntimeValueAccess<'_>,
    path: Arc<[CoreDataKey]>,
    operands: Vec<Value>,
) -> Value {
    Value::Lazy(LazyValue::from_access_in(access, path, Arc::from(operands)))
}

pub(crate) fn request_operator(
    _access: &RuntimeValueAccess<'_>,
    tag: Key,
    arity: usize,
    supplied: Arc<[Value]>,
    wrap_effect: bool,
) -> CoreOperator {
    assert!(arity > 0, "nullary requests are already data values");
    assert!(supplied.len() < arity);
    CoreOperator::Request {
        tag,
        arity,
        supplied,
        wrap_effect,
    }
}

pub(super) fn apply_core_operator(
    access: &EvaluationValueAccess<'_>,
    operator: &CoreOperator,
    data: &Value,
) -> Result<OperatorYield<CoreSpecialization>, EvaluationHalt> {
    if let CoreOperator::ComputationCaptures { code, supplied } = operator
        && let Some(value) = eagerly_completed_thunk(access, code, supplied, data)
    {
        return Ok(OperatorYield::Data(value));
    }
    apply_operator(access, operator, data)
}

/// A thunk's value, if `operand` is its last capture and its code is
/// compiled and builds no code of its own. Running the code then only
/// builds values, as building its net would, so the thunk may as well be
/// its result. Compiled code that builds code runs only when its thunk is
/// forced, which also keeps running compiled code from nesting.
fn eagerly_completed_thunk(
    access: &EvaluationValueAccess<'_>,
    code: &FunctionCode,
    supplied: &[Value],
    operand: &Value,
) -> Option<Value> {
    if supplied.len() + 1 != code.capture_count() {
        return None;
    }
    let compiled = code
        .runtime()
        .access(access.values())
        .compiled(code.capture_count())?;
    let builds_closures = compiled
        .steps()
        .iter()
        .any(|step| matches!(step.operator, CoreOperator::FunctionCaptures { .. }));
    if builds_closures {
        return None;
    }
    let captures = supplied
        .iter()
        .chain([operand])
        .map(|value| access.values().duplicate_value(value))
        .collect::<Vec<_>>();
    Some(apply_compiled(access, compiled, &captures))
}

/// Applies `operator` to `data`. A completed thunk over compiled code runs
/// it when forced.
fn apply_operator(
    access: &EvaluationValueAccess<'_>,
    operator: &CoreOperator,
    data: &Value,
) -> Result<OperatorYield<CoreSpecialization>, EvaluationHalt> {
    let operand = access.values().duplicate_value(data);
    match operator {
        CoreOperator::ApplyArity { arity, supplied } => {
            let mut operands = supplied
                .iter()
                .map(|value| access.values().duplicate_value(value))
                .collect::<Vec<_>>();
            operands.push(operand);
            if operands.len() < *arity + 1 {
                return Ok(OperatorYield::Operator(apply_arity_operator(
                    access.values(),
                    *arity,
                    Arc::from(operands),
                )));
            }
            let function = operands.remove(0);
            if *arity == 0 {
                return Ok(OperatorYield::Data(function));
            }
            // A saturated apply operator may still represent over-application:
            // evaluating `function` can first yield another function before the
            // remaining operands are consumed. Keep that progress in the lazy
            // application's WHNF machine. Executing the application while the
            // operator pair is claimed would lose the intermediate state when
            // the nested evaluation yields, causing the restored pair to replay
            // the application from its beginning.
            Ok(OperatorYield::Data(Value::Lazy(
                LazyValue::from_application_in(access.values(), function, Arc::from(operands)),
            )))
        }
        CoreOperator::FunctionCaptures { code, supplied } => {
            let mut captures = supplied
                .iter()
                .map(|value| access.values().duplicate_value(value))
                .collect::<Vec<_>>();
            captures.push(operand);
            if captures.len() < code.capture_count() {
                return Ok(OperatorYield::Operator(function_capture_operator(
                    access.values(),
                    code.clone(),
                    Arc::from(captures),
                )));
            }
            if captures.len() != code.capture_count() {
                return Err(EvaluationHalt::new("function capture arity mismatch"));
            }
            let stage = NetValue::new(code.duplicate_runtime_in(access.values()));
            let stage = attach_net_many_in(access.values(), stage, captures);
            Ok(OperatorYield::Data(Value::Function(FunctionValue::new(
                stage,
                code.arity(),
            ))))
        }
        CoreOperator::ComputationCaptures { code, supplied } => {
            let mut captures = supplied
                .iter()
                .map(|value| access.values().duplicate_value(value))
                .collect::<Vec<_>>();
            captures.push(operand);
            if captures.len() < code.capture_count() {
                return Ok(OperatorYield::Operator(computation_capture_operator(
                    access.values(),
                    code.clone(),
                    Arc::from(captures),
                )));
            }
            // A thunk over compiled code runs it when forced.
            let compiled = code
                .runtime()
                .access(access.values())
                .compiled(captures.len());
            if compiled.is_some() {
                let code = NetValue::new(code.duplicate_runtime_in(access.values()));
                return Ok(OperatorYield::Data(Value::Lazy(LazyValue::from_thunk_in(
                    access.values(),
                    code,
                    Arc::from(captures),
                ))));
            }
            let stage = NetValue::new(code.duplicate_runtime_in(access.values()));
            let stage = attach_net_many_in(access.values(), stage, captures);
            Ok(OperatorYield::Data(Value::Lazy(
                LazyValue::from_net_computation_in(access.values(), stage),
            )))
        }
        CoreOperator::Dict { keys, supplied } => {
            let mut values = supplied
                .iter()
                .map(|value| access.values().duplicate_value(value))
                .collect::<Vec<_>>();
            values.push(operand);
            if values.len() < keys.len() {
                return Ok(OperatorYield::Operator(dict_operator(
                    access.values(),
                    keys.clone(),
                    Arc::from(values),
                )));
            }
            let dict = keys
                .iter()
                .cloned()
                .zip(values)
                .fold(crate::core::Dict::new_sync(), |dict, (key, value)| {
                    dict.insert(key, value)
                });
            Ok(OperatorYield::Data(Value::Dict(dict)))
        }
        CoreOperator::Builtin(call) => {
            let mut arguments = call
                .arguments
                .iter()
                .map(|value| access.values().duplicate_value(value))
                .collect::<Vec<_>>();
            arguments.push(operand);
            if arguments.len() < call.builtin.arity() {
                return Ok(OperatorYield::Operator(builtin_operator(
                    access.values(),
                    BuiltinCall {
                        builtin: call.builtin,
                        arguments: Arc::from(arguments),
                    },
                )));
            }
            if arguments.len() > call.builtin.arity() {
                return Err(EvaluationHalt::new(
                    "builtin operator received too many arguments",
                ));
            }
            Ok(OperatorYield::Data(Value::Lazy(
                LazyValue::from_builtin_in(
                    access.values(),
                    BuiltinCall {
                        builtin: call.builtin,
                        arguments: Arc::from(arguments),
                    },
                ),
            )))
        }
        CoreOperator::Applicable(function) => Ok(OperatorYield::Data(Value::Lazy(
            LazyValue::from_application_in(
                access.values(),
                access.values().duplicate_value(function),
                Arc::from([operand]),
            ),
        ))),
        CoreOperator::List { arity, supplied } => {
            let mut arguments = supplied
                .iter()
                .map(|value| access.values().duplicate_value(value))
                .collect::<Vec<_>>();
            arguments.push(operand);
            if arguments.len() < *arity {
                return Ok(OperatorYield::Operator(list_operator(
                    access.values(),
                    *arity,
                    Arc::from(arguments),
                )));
            }
            Ok(OperatorYield::Data(Value::List(List::from_values(
                arguments,
            ))))
        }
        CoreOperator::Access { path, supplied } => {
            let arity = crate::core_net::access_arity(path);
            let mut arguments = supplied
                .iter()
                .map(|value| access.values().duplicate_value(value))
                .collect::<Vec<_>>();
            arguments.push(operand);
            if arguments.len() < arity {
                return Ok(OperatorYield::Operator(access_operator(
                    access.values(),
                    path.clone(),
                    Arc::from(arguments),
                )));
            }
            Ok(OperatorYield::Data(access_value(
                access.values(),
                path.clone(),
                arguments,
            )))
        }
        CoreOperator::Request {
            tag,
            arity,
            supplied,
            wrap_effect,
        } => {
            let mut arguments = supplied
                .iter()
                .map(|value| access.values().duplicate_value(value))
                .collect::<Vec<_>>();
            arguments.push(operand);
            if arguments.len() < *arity {
                return Ok(OperatorYield::Operator(request_operator(
                    access.values(),
                    tag.clone(),
                    *arity,
                    Arc::from(arguments),
                    *wrap_effect,
                )));
            }
            let request = Value::Dict(
                crate::core::Dict::new_sync()
                    .insert(tag.clone(), Value::List(List::from_values(arguments))),
            );
            Ok(OperatorYield::Data(if *wrap_effect {
                constant_effect_in(access.values(), request)
            } else {
                request
            }))
        }
    }
}

fn constant_effect_template(
    _access: &RuntimeValueAccess<'_>,
    request: Value,
) -> crate::core_net::CoreInteractionNet {
    let mut net = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
    let [input, argument, result] = net.function_bind();
    let erase = net.copy(0).input;
    net.wire(argument, erase);
    let data = net.data(request);
    net.wire(result, data);
    net.finish(input)
}

pub(crate) fn constant_effect_in(
    access: &crate::core::RuntimeValueAccess<'_>,
    request: Value,
) -> Value {
    let template = constant_effect_template(access, request);
    let function = Value::Function(FunctionValue::new(
        NetValue::new(
            access
                .construct_managed_core_net(template.instantiate_with(access))
                .expect("managed core-net representation must fit one collector run"),
        ),
        1,
    ));
    Value::Dict(crate::core::Dict::new_sync().insert((*keys::EFF).clone(), function))
}

#[cfg(test)]
pub(crate) fn constant_effect(values: &CoreValueFactory, request: Value) -> Value {
    values.with_runtime_value_access(|access| constant_effect_in(&access, request))
}
