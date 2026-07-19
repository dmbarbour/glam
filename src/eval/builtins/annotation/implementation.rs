use super::super::super::*;

pub(super) fn eval_anno_builtin(
    context: &EvalContext,
    annotation: &Value,
    target: &Value,
) -> Result<Value, EvalError> {
    match recognize_annotation(context, annotation)? {
        RecognizedAnnotation::AssertDefined { name, defined } => {
            if defined {
                Ok(target.clone())
            } else {
                Ok(annotation_error_value(format!(
                    "cannot override `{name}` because it is not defined"
                )))
            }
        }
        RecognizedAnnotation::AssertUndefined { name, defined } => {
            if defined {
                Ok(annotation_error_value(format!(
                    "cannot introduce `{name}` because it is already defined"
                )))
            } else {
                Ok(target.clone())
            }
        }
        RecognizedAnnotation::AssertUnit { value } => {
            let value = force_value_shell(context, &value)?;
            if is_unit_value(&value) {
                Ok(target.clone())
            } else {
                Ok(annotation_error_value(format!(
                    "`=>>` requires discarded effect results to be unit, got {value:?}"
                )))
            }
        }
        RecognizedAnnotation::Deque => eval_deque_annotation(context, target),
        RecognizedAnnotation::Binary => eval_binary_annotation(context, target),
        RecognizedAnnotation::Array => eval_array_annotation(context, target),
        RecognizedAnnotation::Reflection { effect } => {
            Ok(Value::reflection_gate(effect, target.clone()))
        }
        RecognizedAnnotation::Seq { value } => super::super::strategy::seq(context, &value, target),
        RecognizedAnnotation::Spark { value } => {
            Ok(super::super::strategy::spark(context, value, target))
        }
        RecognizedAnnotation::Invalid(message) => Ok(annotation_error_value(message)),
        RecognizedAnnotation::Unknown(rendered) => {
            eprintln!("warning: unrecognized annotation encountered: {rendered}");
            Ok(target.clone())
        }
    }
}

enum RecognizedAnnotation {
    AssertDefined { name: String, defined: bool },
    AssertUndefined { name: String, defined: bool },
    AssertUnit { value: Value },
    Deque,
    Binary,
    Array,
    Reflection { effect: Value },
    Seq { value: Value },
    Spark { value: Value },
    Invalid(String),
    Unknown(String),
}

fn recognize_annotation(
    context: &EvalContext,
    annotation: &Value,
) -> Result<RecognizedAnnotation, EvalError> {
    let annotation = force_value_shell(context, annotation)?;
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
                ParsedValueAnnotation::Valid { value } => {
                    RecognizedAnnotation::AssertUnit { value }
                }
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
    Valid { value: Value },
    Invalid(String),
}

fn parse_assertion_annotation(
    context: &EvalContext,
    payload: &Value,
    tag_name: &str,
) -> Result<ParsedAssertion, EvalError> {
    let payload = force_value_shell(context, payload)?;
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
    let defined = !is_undefined_value(&force_value_shell(context, value)?);
    Ok(ParsedAssertion::Valid { name, defined })
}

fn parse_value_annotation(
    context: &EvalContext,
    payload: &Value,
    tag_name: &str,
) -> Result<ParsedValueAnnotation, EvalError> {
    let payload = force_value_shell(context, payload)?;
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
    })
}

fn annotation_name(context: &EvalContext, value: &Value) -> Result<String, EvalError> {
    let value = force_value_shell(context, value)?;
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

fn is_unit_value(value: &Value) -> bool {
    matches!(
        value,
        Value::Atom(atom) if atom.key() == &Key::abstract_global_path(["builtin", "unit"])
    )
}

pub(in crate::eval) fn annotation_error_value(message: impl Into<String>) -> Value {
    Value::error(message.into())
}

fn eval_deque_annotation(context: &EvalContext, target: &Value) -> Result<Value, EvalError> {
    match force_value_shell(context, target)? {
        Value::List(list) => {
            Ok(Value::List(list.try_balanced(&mut |thunk| {
                force_list_thunk(context, thunk)
            })?))
        }
        other => Ok(annotation_error_value(format!(
            "`deque` annotation requires a list target, got {other:?}"
        ))),
    }
}

fn eval_binary_annotation(context: &EvalContext, target: &Value) -> Result<Value, EvalError> {
    match force_value_shell(context, target)? {
        Value::Binary(bytes) => Ok(Value::Binary(bytes)),
        Value::List(list) => match list_to_binary_bytes(context, &list, "`binary` annotation") {
            Ok(bytes) => Ok(Value::Binary(Bytes::from(bytes))),
            Err(message) => Ok(annotation_error_value(message)),
        },
        other => Ok(annotation_error_value(format!(
            "`binary` annotation requires a list or binary target, got {other:?}"
        ))),
    }
}

fn eval_array_annotation(context: &EvalContext, target: &Value) -> Result<Value, EvalError> {
    match force_value_shell(context, target)? {
        Value::Binary(bytes) => Ok(Value::List(List::from_values(
            bytes
                .iter()
                .map(|byte| Value::Number(Number::from_u8(*byte)))
                .collect(),
        ))),
        Value::List(list) => Ok(Value::List(List::from_values(list_to_value_items(
            context, &list,
        )?))),
        other => Ok(annotation_error_value(format!(
            "`array` annotation requires a list or binary target, got {other:?}"
        ))),
    }
}
