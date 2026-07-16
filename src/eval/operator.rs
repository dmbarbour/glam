use super::*;

pub(crate) fn apply_arity_operator(arity: usize, supplied: Arc<[Value]>) -> CoreOperator {
    assert!(supplied.len() < arity + 1);
    CoreOperator::ApplyArity { arity, supplied }
}

/// Builds the semantic value for builtin application without executing a
/// saturated call. Net construction may place that call in a lazy aggregate;
/// evaluating it here would make enclosing construction accidentally strict.
pub(super) fn apply_builtin_values_lazily(
    builtin: Builtin,
    mut supplied: Vec<Value>,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    let remaining = builtin
        .arity()
        .checked_sub(supplied.len())
        .expect("partial builtin cannot contain too many arguments");
    if arguments.len() < remaining {
        supplied.extend(arguments);
        return Ok(Value::PartialBuiltin(BuiltinCall {
            builtin,
            arguments: Arc::from(supplied),
        }));
    }

    let (saturating, rest) = arguments.split_at(remaining);
    supplied.extend_from_slice(saturating);
    let result = Value::Lazy(LazyValue::from_builtin(BuiltinCall {
        builtin,
        arguments: Arc::from(supplied),
    }));
    if rest.is_empty() {
        Ok(result)
    } else {
        apply_values(result, rest.to_vec(), &[])
    }
}

pub(crate) fn function_capture_operator(
    code: Arc<FunctionCode>,
    supplied: Arc<[Value]>,
) -> CoreOperator {
    assert!(code.capture_count() > 0);
    assert!(supplied.len() < code.capture_count());
    CoreOperator::FunctionCaptures { code, supplied }
}

pub(crate) fn computation_capture_operator(
    code: Arc<FunctionCode>,
    supplied: Arc<[Value]>,
) -> CoreOperator {
    assert_eq!(code.arity(), 0);
    assert!(code.capture_count() > 0);
    assert!(supplied.len() < code.capture_count());
    CoreOperator::ComputationCaptures { code, supplied }
}

pub(crate) fn dict_operator(keys: Arc<[Key]>, supplied: Arc<[Value]>) -> CoreOperator {
    assert!(!keys.is_empty());
    assert!(supplied.len() < keys.len());
    CoreOperator::Dict { keys, supplied }
}

pub(crate) fn builtin_operator(call: BuiltinCall) -> CoreOperator {
    CoreOperator::Builtin(call)
}

pub(super) fn applicable_operator(function: Value) -> CoreOperator {
    CoreOperator::Applicable(function)
}

pub(crate) fn list_operator(arity: usize, supplied: Arc<[Value]>) -> CoreOperator {
    assert!(supplied.len() < arity);
    CoreOperator::List { arity, supplied }
}

pub(crate) fn access_operator(path: Arc<[CoreDataKey]>, supplied: Arc<[Value]>) -> CoreOperator {
    let arity = 1 + path
        .iter()
        .filter(|key| !matches!(key, CoreDataKey::Key(_)))
        .count();
    assert!(supplied.len() < arity);
    CoreOperator::Access { path, supplied }
}

pub(super) fn apply_core_operator(
    operator: &CoreOperator,
    data: &Value,
) -> Result<OperatorYield<CoreSpecialization>, EvalError> {
    let operand = data.clone();
    match operator {
        CoreOperator::ApplyArity { arity, supplied } => {
            let mut operands = supplied.iter().cloned().collect::<Vec<_>>();
            operands.push(operand);
            if operands.len() < *arity + 1 {
                return Ok(OperatorYield::Operator(apply_arity_operator(
                    *arity,
                    Arc::from(operands),
                )));
            }
            let function = operands.remove(0);
            if *arity == 0 {
                return Ok(OperatorYield::Data(function));
            }
            let result = match function {
                Value::Builtin(builtin) => {
                    apply_builtin_values_lazily(builtin, Vec::new(), operands)
                }
                Value::PartialBuiltin(call) => apply_builtin_values_lazily(
                    call.builtin,
                    call.arguments.iter().cloned().collect(),
                    operands,
                ),
                function => apply_values(function, operands, &[]),
            }?;
            Ok(OperatorYield::Data(result))
        }
        CoreOperator::FunctionCaptures { code, supplied } => {
            let mut captures = supplied.iter().cloned().collect::<Vec<_>>();
            captures.push(operand);
            if captures.len() < code.capture_count() {
                return Ok(OperatorYield::Operator(function_capture_operator(
                    code.clone(),
                    Arc::from(captures),
                )));
            }
            Ok(OperatorYield::Data(instantiate_function(code, captures)?))
        }
        CoreOperator::ComputationCaptures { code, supplied } => {
            let mut captures = supplied.iter().cloned().collect::<Vec<_>>();
            captures.push(operand);
            if captures.len() < code.capture_count() {
                return Ok(OperatorYield::Operator(computation_capture_operator(
                    code.clone(),
                    Arc::from(captures),
                )));
            }
            let stage =
                attach_net_many(Value::Net(NetValue::new(code.runtime().clone())), captures);
            Ok(OperatorYield::Data(Value::Lazy(
                LazyValue::from_net_computation(stage),
            )))
        }
        CoreOperator::Dict { keys, supplied } => {
            let mut values = supplied.iter().cloned().collect::<Vec<_>>();
            values.push(operand);
            if values.len() < keys.len() {
                return Ok(OperatorYield::Operator(dict_operator(
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
            let mut arguments = call.arguments.iter().cloned().collect::<Vec<_>>();
            arguments.push(operand);
            if arguments.len() < call.builtin.arity() {
                return Ok(OperatorYield::Operator(builtin_operator(BuiltinCall {
                    builtin: call.builtin,
                    arguments: Arc::from(arguments),
                })));
            }
            if arguments.len() > call.builtin.arity() {
                return Err(EvalError::new(
                    "builtin operator received too many arguments",
                ));
            }
            Ok(OperatorYield::Data(Value::Lazy(LazyValue::from_builtin(
                BuiltinCall {
                    builtin: call.builtin,
                    arguments: Arc::from(arguments),
                },
            ))))
        }
        CoreOperator::Applicable(function) => Ok(OperatorYield::Data(apply_value(
            function.clone(),
            operand,
            &[],
        )?)),
        CoreOperator::List { arity, supplied } => {
            let mut arguments = supplied.iter().cloned().collect::<Vec<_>>();
            arguments.push(operand);
            if arguments.len() < *arity {
                return Ok(OperatorYield::Operator(list_operator(
                    *arity,
                    Arc::from(arguments),
                )));
            }
            let list = arguments.into_iter().fold(List::empty(), |list, value| {
                List::concat(list, list_literal_segment(value))
            });
            Ok(OperatorYield::Data(Value::List(list)))
        }
        CoreOperator::Access { path, supplied } => {
            let arity = 1 + path
                .iter()
                .filter(|key| !matches!(key, CoreDataKey::Key(_)))
                .count();
            let mut arguments = supplied.iter().cloned().collect::<Vec<_>>();
            arguments.push(operand);
            if arguments.len() < arity {
                return Ok(OperatorYield::Operator(access_operator(
                    path.clone(),
                    Arc::from(arguments),
                )));
            }
            Ok(OperatorYield::Data(Value::Lazy(LazyValue::from_access(
                path.clone(),
                Arc::from(arguments),
            ))))
        }
    }
}
