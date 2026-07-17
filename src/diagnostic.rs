use std::fmt;
use std::sync::Arc;

use crate::core::{Builtin, Dict, List, Value, keys};
use crate::eval;
use crate::number::Number;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    File,
    Script,
}

impl SourceKind {
    fn value(self) -> Value {
        match self {
            Self::File => (*keys::FILE_VALUE).clone(),
            Self::Script => (*keys::SCRIPT_VALUE).clone(),
        }
    }
}

/// Assembler-owned identity for source content. The bootstrap uses a source
/// label as its identity; invocation identity remains separate so the same
/// file can be compiled repeatedly under different module environments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceIdentity {
    kind: SourceKind,
    label: Arc<str>,
}

impl SourceIdentity {
    pub(crate) fn file(label: impl Into<Arc<str>>) -> Self {
        Self {
            kind: SourceKind::File,
            label: label.into(),
        }
    }

    pub(crate) fn script(label: impl Into<Arc<str>>) -> Self {
        Self {
            kind: SourceKind::Script,
            label: label.into(),
        }
    }

    pub(crate) fn label(&self) -> &Arc<str> {
        &self.label
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompilationInvocationId(u64);

impl CompilationInvocationId {
    pub(crate) fn new(id: u64) -> Self {
        assert!(id != 0, "compilation invocation IDs start at one");
        Self(id)
    }

    fn value(self) -> Value {
        Value::Number(Number::from_u64(self.0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportOrigin {
    parent: Arc<CompilationTrace>,
    reference: Arc<str>,
}

/// Compact immutable provenance for one compilation invocation. Import traces
/// retain identifiers and labels only, never source bytes or environment
/// values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompilationTrace {
    invocation: CompilationInvocationId,
    source: SourceIdentity,
    module_path: Arc<[String]>,
    imported_from: Option<ImportOrigin>,
}

impl CompilationTrace {
    pub(crate) fn root(
        invocation: CompilationInvocationId,
        source: SourceIdentity,
        module_path: Arc<[String]>,
    ) -> Self {
        Self {
            invocation,
            source,
            module_path,
            imported_from: None,
        }
    }

    pub(crate) fn imported(
        invocation: CompilationInvocationId,
        source: SourceIdentity,
        module_path: Arc<[String]>,
        parent: Arc<Self>,
        reference: Arc<str>,
    ) -> Self {
        Self {
            invocation,
            source,
            module_path,
            imported_from: Some(ImportOrigin { parent, reference }),
        }
    }

    pub(crate) fn source_label(&self) -> &Arc<str> {
        self.source.label()
    }

    pub(crate) fn origin_value(&self) -> Value {
        let Value::Dict(origin) = self.frame_value(None) else {
            unreachable!()
        };
        Value::Dict(origin.insert((*keys::IMPORTS).clone(), self.import_chain_value()))
    }

    fn import_chain_value(&self) -> Value {
        let mut chain = Vec::new();
        let mut current = self;
        while let Some(import) = &current.imported_from {
            chain.push((import.parent.clone(), import.reference.clone()));
            current = &import.parent;
        }
        chain.reverse();
        Value::List(List::from_values(
            chain
                .into_iter()
                .map(|(parent, reference)| parent.frame_value(Some(&reference)))
                .collect(),
        ))
    }

    fn frame_value(&self, reference: Option<&str>) -> Value {
        let mut frame = Dict::new_sync()
            .insert((*keys::INVOCATION).clone(), self.invocation.value())
            .insert(
                (*keys::SOURCE).clone(),
                Value::binary_from_text(&self.source.label),
            )
            .insert((*keys::SOURCE_KIND).clone(), self.source.kind.value())
            .insert(
                (*keys::MODULE).clone(),
                module_path_value(&self.module_path),
            );
        if let Some(reference) = reference {
            frame = frame.insert(
                (*keys::REFERENCE).clone(),
                Value::binary_from_text(reference),
            );
        }
        Value::Dict(frame)
    }
}

fn module_path_value(module_path: &[String]) -> Value {
    Value::List(List::from_values(
        module_path
            .iter()
            .map(|part| Value::binary_from_text(part))
            .collect(),
    ))
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

pub(crate) fn assembler_metadata(severity: Severity, origin: Option<Value>) -> Dict {
    let mut message = Dict::new_sync().insert((*keys::SEVERITY).clone(), severity.value());
    if let Some(origin) = origin {
        message = message.insert((*keys::ORIGIN).clone(), origin);
    }
    Dict::new_sync().insert((*keys::MSG).clone(), Value::Dict(message))
}

/// Applies assembler-owned metadata as a real object definitions mixin so the
/// resulting `spec` also records the extension for subsequent observers.
pub(crate) fn enrich(
    message: Value,
    severity: Severity,
    origin: Option<Value>,
) -> Result<Value, String> {
    let metadata = Value::Dict(assembler_metadata(severity, origin));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn list_values(list: &List) -> Vec<Value> {
        let mut values = Vec::new();
        list.for_each_segment(
            &mut |bytes| panic!("provenance lists must not contain byte segments: {bytes:?}"),
            &mut |segment| {
                values.extend_from_slice(segment);
                Ok::<_, ()>(())
            },
        )
        .expect("closed provenance list should not fail");
        values
    }

    #[test]
    fn imported_trace_projects_a_root_to_parent_chain() {
        let root = Arc::new(CompilationTrace::root(
            CompilationInvocationId::new(1),
            SourceIdentity::file("root.g"),
            Arc::from(["pkg".to_owned()]),
        ));
        let child = Arc::new(CompilationTrace::imported(
            CompilationInvocationId::new(2),
            SourceIdentity::file("lib/child.g"),
            Arc::from(["pkg".to_owned(), "child".to_owned()]),
            root,
            Arc::from("lib/child.g"),
        ));
        let leaf = CompilationTrace::imported(
            CompilationInvocationId::new(3),
            SourceIdentity::file("lib/leaf.g"),
            Arc::from(["pkg".to_owned(), "child".to_owned()]),
            child,
            Arc::from("leaf.g"),
        );

        let Value::Dict(origin) = leaf.origin_value() else {
            unreachable!()
        };
        assert_eq!(
            origin.get(&*keys::INVOCATION),
            Some(&Value::Number(Number::from_u64(3)))
        );
        assert_eq!(
            origin.get(&*keys::SOURCE),
            Some(&Value::binary_from_text("lib/leaf.g"))
        );
        let Some(Value::List(imports)) = origin.get(&*keys::IMPORTS) else {
            panic!("origin should contain an import chain");
        };
        let imports = list_values(imports);
        assert_eq!(imports.len(), 2);
        let Value::Dict(root_frame) = &imports[0] else {
            unreachable!()
        };
        let Value::Dict(child_frame) = &imports[1] else {
            unreachable!()
        };
        assert_eq!(
            root_frame.get(&*keys::REFERENCE),
            Some(&Value::binary_from_text("lib/child.g"))
        );
        assert_eq!(
            child_frame.get(&*keys::REFERENCE),
            Some(&Value::binary_from_text("leaf.g"))
        );
        assert_eq!(
            child_frame.get(&*keys::INVOCATION),
            Some(&Value::Number(Number::from_u64(2)))
        );
    }
}
