use super::super::super::*;

pub(super) fn eval_anno_builtin(
    context: &EvaluatorStepContext<'_>,
    annotation: &Value,
    target: &Value,
) -> Result<Value, EvaluationHalt> {
    match recognize_annotation(context, annotation)? {
        RecognizedAnnotation::AssertDefined { name, defined } => {
            if defined {
                Ok(target.clone())
            } else {
                Ok(annotation_error_value(
                    context,
                    format!("cannot override `{name}` because it is not defined"),
                ))
            }
        }
        RecognizedAnnotation::AssertUndefined { name, defined } => {
            if defined {
                Ok(annotation_error_value(
                    context,
                    format!("cannot introduce `{name}` because it is already defined"),
                ))
            } else {
                Ok(target.clone())
            }
        }
        RecognizedAnnotation::AssertUnit {
            value,
            diagnostic_context,
        } => super::super::assertion::assert_unit_in(
            context,
            diagnostic_context.as_ref(),
            &value,
            target,
        ),
        RecognizedAnnotation::MetadataInitialize => {
            let carrier = Value::initial_metadata_carrier(context.context().values());
            super::super::assertion::assert_unit_in(context, None, target, &carrier)
        }
        RecognizedAnnotation::MetadataPure { function } => {
            eval_metadata_pure_annotation(context, function, target)
        }
        RecognizedAnnotation::MetadataReflection { function } => {
            eval_metadata_reflection_annotation(context, function, target)
        }
        RecognizedAnnotation::Deque => eval_deque_annotation(context, target),
        RecognizedAnnotation::Binary => eval_binary_annotation(context, target),
        RecognizedAnnotation::Array => eval_array_annotation(context, target),
        RecognizedAnnotation::Reflection { effect } => Ok(defer_reflection_annotation(
            context.context(),
            effect,
            target,
        )),
        RecognizedAnnotation::Seq { value } => {
            super::super::strategy::seq(context.context(), &value, target)
        }
        RecognizedAnnotation::Spark { value } => Ok(super::super::strategy::spark(
            context.context(),
            value,
            target,
        )),
        RecognizedAnnotation::Error => {
            let message = eval_value_in(context, target)
                .map_err(|error| error.with_context(evaluation_context_frame("error_message")))?;
            Err(EvaluationHalt::from_value(message))
        }
        RecognizedAnnotation::Context {
            context: diagnostic_context,
        } => eval_value_in(context, target).map_err(|error| error.with_context(diagnostic_context)),
        RecognizedAnnotation::Invalid(message) => Ok(annotation_error_value(context, message)),
        RecognizedAnnotation::Unknown(rendered) => {
            warn_unknown_annotation(&rendered);
            Ok(target.clone())
        }
    }
}

enum RecognizedAnnotation {
    AssertDefined {
        name: String,
        defined: bool,
    },
    AssertUndefined {
        name: String,
        defined: bool,
    },
    AssertUnit {
        value: Value,
        diagnostic_context: Option<Value>,
    },
    MetadataInitialize,
    MetadataPure {
        function: Value,
    },
    MetadataReflection {
        function: Value,
    },
    Deque,
    Binary,
    Array,
    Reflection {
        effect: Value,
    },
    Seq {
        value: Value,
    },
    Spark {
        value: Value,
    },
    Error,
    Context {
        context: Value,
    },
    Invalid(String),
    Unknown(String),
}

fn recognize_annotation(
    context: &EvaluatorStepContext<'_>,
    annotation: &Value,
) -> Result<RecognizedAnnotation, EvaluationHalt> {
    let annotation = eval_value_in(context, annotation)
        .map_err(|error| error.with_context(evaluation_context_frame("annotation")))?;
    if let Value::Atom(atom) = &annotation {
        return Ok(recognize_simple_annotation(atom)
            .unwrap_or_else(|| RecognizedAnnotation::Unknown(format!("{annotation:?}"))));
    }

    let Value::Dict(annotation) = annotation else {
        return Ok(RecognizedAnnotation::Unknown(format!("{annotation:?}")));
    };

    let Some((tag, payload)) = annotation.iter().next() else {
        return Ok(RecognizedAnnotation::Unknown(format!("{annotation:?}")));
    };
    if annotation.iter().nth(1).is_some() {
        return Ok(RecognizedAnnotation::Unknown(format!("{annotation:?}")));
    }

    match tag {
        Key::Atom(atom) if atom_name(atom) == Some("refl") => {
            Ok(RecognizedAnnotation::Reflection {
                effect: payload.clone(),
            })
        }
        Key::Atom(atom) if atom_name(atom) == Some("seq") => Ok(RecognizedAnnotation::Seq {
            value: payload.clone(),
        }),
        Key::Atom(atom) if atom_name(atom) == Some("spark") => Ok(RecognizedAnnotation::Spark {
            value: payload.clone(),
        }),
        Key::Atom(atom) if atom_name(atom) == Some("context") => {
            Ok(RecognizedAnnotation::Context {
                context: payload.clone(),
            })
        }
        Key::Atom(atom) if atom_name(atom) == Some("meta_pure") => {
            Ok(RecognizedAnnotation::MetadataPure {
                function: payload.clone(),
            })
        }
        Key::Atom(atom) if atom_name(atom) == Some("meta_refl") => {
            Ok(RecognizedAnnotation::MetadataReflection {
                function: payload.clone(),
            })
        }
        Key::Atom(atom) if atom_name(atom) == Some("assert_defined") => Ok(
            match parse_assertion_annotation(context, payload, "assert_defined")? {
                ParsedAssertion::Valid { name, defined } => {
                    RecognizedAnnotation::AssertDefined { name, defined }
                }
                ParsedAssertion::Invalid(message) => RecognizedAnnotation::Invalid(message),
            },
        ),
        Key::Atom(atom) if atom_name(atom) == Some("assert_undefined") => Ok(
            match parse_assertion_annotation(context, payload, "assert_undefined")? {
                ParsedAssertion::Valid { name, defined } => {
                    RecognizedAnnotation::AssertUndefined { name, defined }
                }
                ParsedAssertion::Invalid(message) => RecognizedAnnotation::Invalid(message),
            },
        ),
        Key::Atom(atom) if atom_name(atom) == Some("assert_unit") => Ok(
            match parse_value_annotation(context, payload, "assert_unit")? {
                ParsedValueAnnotation::Valid {
                    value,
                    diagnostic_context,
                } => RecognizedAnnotation::AssertUnit {
                    value,
                    diagnostic_context,
                },
                ParsedValueAnnotation::Invalid(message) => RecognizedAnnotation::Invalid(message),
            },
        ),
        Key::Atom(atom) if payload_is_unit(payload) => Ok(recognize_simple_annotation(atom)
            .unwrap_or_else(|| RecognizedAnnotation::Unknown(format!("{annotation:?}")))),
        _ => Ok(RecognizedAnnotation::Unknown(format!("{annotation:?}"))),
    }
}

fn recognize_simple_annotation(atom: &crate::core::Atom) -> Option<RecognizedAnnotation> {
    match atom_name(atom)? {
        "deque" => Some(RecognizedAnnotation::Deque),
        "binary" => Some(RecognizedAnnotation::Binary),
        "array" => Some(RecognizedAnnotation::Array),
        "error" => Some(RecognizedAnnotation::Error),
        "meta_init" => Some(RecognizedAnnotation::MetadataInitialize),
        _ => None,
    }
}

fn payload_is_unit(payload: &Value) -> bool {
    matches!(payload, Value::Dict(dict) if dict.is_empty())
}

enum ParsedAssertion {
    Valid { name: String, defined: bool },
    Invalid(String),
}

enum ParsedValueAnnotation {
    Valid {
        value: Value,
        diagnostic_context: Option<Value>,
    },
    Invalid(String),
}

fn parse_assertion_annotation(
    context: &EvaluatorStepContext<'_>,
    payload: &Value,
    tag_name: &str,
) -> Result<ParsedAssertion, EvaluationHalt> {
    let payload = eval_value_in(context, payload)?;
    let Value::Dict(payload) = payload else {
        return Ok(ParsedAssertion::Invalid(format!(
            "invalid `{tag_name}` annotation payload"
        )));
    };

    let Some(name_value) = payload.get(&*keys::NAME) else {
        return Ok(ParsedAssertion::Invalid(format!(
            "invalid `{tag_name}` annotation payload"
        )));
    };
    let Some(value) = payload.get(&*keys::VALUE) else {
        return Ok(ParsedAssertion::Invalid(format!(
            "invalid `{tag_name}` annotation payload"
        )));
    };

    let name = annotation_name(context, name_value)?;
    let defined = !is_undefined_value(&eval_value_in(context, value)?);
    Ok(ParsedAssertion::Valid { name, defined })
}

fn parse_value_annotation(
    context: &EvaluatorStepContext<'_>,
    payload: &Value,
    tag_name: &str,
) -> Result<ParsedValueAnnotation, EvaluationHalt> {
    let payload = eval_value_in(context, payload)?;
    let Value::Dict(payload) = payload else {
        return Ok(ParsedValueAnnotation::Invalid(format!(
            "invalid `{tag_name}` annotation payload"
        )));
    };

    let Some(value) = payload.get(&*keys::VALUE) else {
        return Ok(ParsedValueAnnotation::Invalid(format!(
            "invalid `{tag_name}` annotation payload"
        )));
    };

    Ok(ParsedValueAnnotation::Valid {
        value: value.clone(),
        diagnostic_context: payload.get(&*keys::CONTEXT).cloned(),
    })
}

fn annotation_name(
    context: &EvaluatorStepContext<'_>,
    value: &Value,
) -> Result<String, EvaluationHalt> {
    let value = eval_value_in(context, value)?;
    Ok(match value {
        Value::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Value::Atom(atom) => atom_name(&atom)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{atom:?}")),
        Value::Number(number) => number.to_string(),
        other => format!("{other:?}"),
    })
}

pub(in crate::eval) fn atom_name(atom: &crate::core::Atom) -> Option<&str> {
    match atom.key() {
        Key::Binary(bytes) => std::str::from_utf8(bytes).ok(),
        _ => None,
    }
}

pub(in crate::eval) fn is_undefined_value(value: &Value) -> bool {
    matches!(value, Value::Dict(dict) if dict.is_empty())
}

pub(in crate::eval) fn annotation_error_value(
    context: &EvaluatorStepContext<'_>,
    message: impl Into<String>,
) -> Value {
    Value::error(context.context().values(), message.into())
}

fn eval_metadata_pure_annotation(
    context: &EvaluatorStepContext<'_>,
    function: Value,
    target: &Value,
) -> Result<Value, EvaluationHalt> {
    let metadata = metadata_update_inputs(context, target, "meta_pure")?;
    let output_count = metadata.len();
    let updates = Value::Lazy(LazyValue::from_application(
        context.context().values(),
        function,
        Arc::from([Value::List(List::from_values(metadata))]),
    ));
    Ok(metadata_update_outputs(context, output_count, updates))
}

fn eval_metadata_reflection_annotation(
    context: &EvaluatorStepContext<'_>,
    function: Value,
    target: &Value,
) -> Result<Value, EvaluationHalt> {
    let metadata = metadata_update_inputs(context, target, "meta_refl")?;
    let output_count = metadata.len();
    let effect = Value::Lazy(LazyValue::from_application(
        context.context().values(),
        function,
        Arc::from([Value::List(List::from_values(metadata))]),
    ));
    Ok(metadata_update_outputs(
        context,
        output_count,
        defer_metadata_reflection(context.context(), effect),
    ))
}

fn metadata_update_inputs(
    context: &EvaluatorStepContext<'_>,
    target: &Value,
    annotation_name: &str,
) -> Result<Vec<Value>, EvaluationHalt> {
    let Value::List(carriers) = eval_value_in(context, target)? else {
        return Err(EvaluationHalt::new(format!(
            "`{annotation_name}` annotation requires a list of sealed metadata carriers"
        )));
    };
    let carriers = list_to_value_items_in(context, &carriers)?;
    let mut metadata = Vec::with_capacity(carriers.len());
    for (index, carrier) in carriers.into_iter().enumerate() {
        let carrier = eval_value_in(context, &carrier)?;
        let Some(value) = carrier.associated_metadata() else {
            return Err(EvaluationHalt::new(format!(
                "`{annotation_name}` annotation item {index} must be a sealed metadata carrier, received {}",
                carrier.diagnostic_kind_name()
            )));
        };
        metadata.push(value);
    }
    Ok(metadata)
}

fn metadata_update_outputs(
    context: &EvaluatorStepContext<'_>,
    output_count: usize,
    updates: Value,
) -> Value {
    let projection_context = Value::Dict(crate::core::Dict::new_sync().insert(
        (*keys::CONTEXT).clone(),
        evaluation_context_frame("wrap_metadata"),
    ));
    let carriers = (0..output_count)
        .map(|index| {
            let projection = Value::Lazy(LazyValue::from_builtin(
                context.context().values(),
                BuiltinCall {
                    builtin: Builtin::ListAt,
                    arguments: Arc::from([
                        Value::Number(Number::from_usize(index)),
                        updates.clone(),
                    ]),
                },
            ));
            Value::metadata_carrier(Value::builtin_call(
                context.context().values(),
                Builtin::Anno,
                vec![projection_context.clone(), projection],
            ))
        })
        .collect();
    Value::List(List::from_values(carriers))
}

fn eval_deque_annotation(
    context: &EvaluatorStepContext<'_>,
    target: &Value,
) -> Result<Value, EvaluationHalt> {
    match eval_value_in(context, target)? {
        Value::List(list) => {
            Ok(Value::List(list.try_balanced(&mut |thunk| {
                force_list_thunk_in(context, thunk)
            })?))
        }
        other => Ok(annotation_error_value(
            context,
            format!("`deque` annotation requires a list target, got {other:?}"),
        )),
    }
}

fn eval_binary_annotation(
    context: &EvaluatorStepContext<'_>,
    target: &Value,
) -> Result<Value, EvaluationHalt> {
    match eval_value_in(context, target)? {
        Value::Binary(bytes) => Ok(Value::Binary(bytes)),
        Value::List(list) => Ok(Value::Binary(Bytes::from(list_to_binary_bytes_in(
            context,
            &list,
            "`binary` annotation",
        )?))),
        other => Ok(annotation_error_value(
            context,
            format!("`binary` annotation requires a list or binary target, got {other:?}"),
        )),
    }
}

fn eval_array_annotation(
    context: &EvaluatorStepContext<'_>,
    target: &Value,
) -> Result<Value, EvaluationHalt> {
    match eval_value_in(context, target)? {
        Value::Binary(bytes) => Ok(Value::List(List::from_values(
            bytes
                .iter()
                .map(|byte| Value::Number(Number::from_u8(*byte)))
                .collect(),
        ))),
        Value::List(list) => Ok(Value::List(List::from_values(list_to_value_items_in(
            context, &list,
        )?))),
        other => Ok(annotation_error_value(
            context,
            format!("`array` annotation requires a list or binary target, got {other:?}"),
        )),
    }
}

/// Durable handoff for a reflection gate. The target is not evaluated here;
/// its later reflection task runs outside any scoped value-access region.
fn defer_reflection_annotation(context: &EvalContext, effect: Value, target: &Value) -> Value {
    Value::reflection_gate(context.values(), effect, target.clone())
}

/// Durable handoff for a metadata-reflection update. Input carrier validation
/// is pure and scoped; only the eventual reflection task crosses this seam.
fn defer_metadata_reflection(context: &EvalContext, effect: Value) -> Value {
    Value::reflection_task_result(context.values(), effect)
}

/// Compatibility warning sink for unknown annotations. This is deliberately
/// outside annotation recognition and semantic value inspection.
fn warn_unknown_annotation(rendered: &str) {
    eprintln!("warning: unrecognized annotation encountered: {rendered}");
}
