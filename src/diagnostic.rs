use std::fmt;
use std::sync::Arc;

use crate::core::{Builtin, Dict, Value, keys};
use crate::eval;
use crate::number::Number;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Info => f.write_str("info"),
            Severity::Warning => f.write_str("warning"),
            Severity::Error => f.write_str("error"),
        }
    }
}

impl Severity {
    pub(crate) fn value(self) -> Value {
        match self {
            Self::Info => (*keys::INFO_VALUE).clone(),
            Self::Warning => (*keys::WARN_VALUE).clone(),
            Self::Error => (*keys::ERROR_VALUE).clone(),
        }
    }
}

/// Builds the conventional bootstrap message body. Severity and assembler
/// provenance are emission-effect metadata and are mixed in later.
pub(crate) fn text_message(line: Option<usize>, message: impl AsRef<str>) -> Value {
    let mut message_dict = Dict::new_sync().insert(
        (*keys::TEXT).clone(),
        Value::binary_from_text(message.as_ref()),
    );
    if let Some(line) = line {
        let location = Dict::new_sync().insert(
            (*keys::LINE).clone(),
            Value::Number(Number::from_usize(line)),
        );
        message_dict = message_dict.insert((*keys::LOCATION).clone(), Value::Dict(location));
    }
    Value::Dict(Dict::new_sync().insert((*keys::MSG).clone(), Value::Dict(message_dict)))
}

pub(crate) fn assembler_metadata(severity: Severity, source: Option<&str>) -> Dict {
    let mut message = Dict::new_sync().insert((*keys::SEVERITY).clone(), severity.value());
    if let Some(source) = source {
        let origin =
            Dict::new_sync().insert((*keys::SOURCE).clone(), Value::binary_from_text(source));
        message = message.insert((*keys::ORIGIN).clone(), Value::Dict(origin));
    }
    Dict::new_sync().insert((*keys::MSG).clone(), Value::Dict(message))
}

/// Applies assembler-owned metadata as a real object definitions mixin so the
/// resulting `spec` also records the extension for subsequent observers.
pub(crate) fn enrich(
    message: Value,
    severity: Severity,
    source: Option<&str>,
) -> Result<Value, String> {
    let metadata = Value::Dict(assembler_metadata(severity, source));
    let extension_defs = Value::builtin_call(Builtin::ObjectOverrideDefs, vec![metadata]);
    eval::apply_values(
        Value::Builtin(Builtin::ObjectWithDefs),
        vec![message, extension_defs],
        &[],
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn conventional_summary(message: &Value) -> (Option<usize>, Option<Arc<str>>) {
    let Value::Dict(message) = message else {
        return (None, None);
    };
    let Some(Value::Dict(interface)) = message.get(&*keys::MSG) else {
        return (None, None);
    };
    let text = interface.get(&*keys::TEXT).and_then(|value| match value {
        Value::Binary(bytes) => Some(Arc::from(String::from_utf8_lossy(bytes).as_ref())),
        _ => None,
    });
    let line = interface
        .get(&*keys::LOCATION)
        .and_then(|value| match value {
            Value::Dict(location) => location.get(&*keys::LINE),
            _ => None,
        })
        .and_then(|value| match value {
            Value::Number(number) => number.to_i64_if_integer(),
            _ => None,
        })
        .and_then(|line| usize::try_from(line).ok());
    (line, text)
}
