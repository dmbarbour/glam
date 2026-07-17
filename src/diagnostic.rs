use std::fmt;
use std::sync::Arc;

use bytes::Bytes;

use crate::core::{Builtin, Dict, List, Value, keys};
use crate::eval;
use crate::number::Number;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// Assembler-owned identity for source content. Reloadable files retain their
/// path; inline scripts retain their bytes because no external source handle
/// exists. Invocation identity remains separate in either case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourceIdentity {
    File { path: Arc<str> },
    Script { label: Arc<str>, text: Bytes },
}

impl SourceIdentity {
    pub(crate) fn file(path: impl Into<Arc<str>>) -> Self {
        Self::File { path: path.into() }
    }

    pub(crate) fn script(label: impl Into<Arc<str>>, text: Bytes) -> Self {
        Self::Script {
            label: label.into(),
            text,
        }
    }

    pub(crate) fn label(&self) -> &Arc<str> {
        match self {
            Self::File { path } => path,
            Self::Script { label, .. } => label,
        }
    }

    fn value(&self) -> Value {
        let (tag, payload) = match self {
            Self::File { path } => (&*keys::FILE, Value::binary_from_text(path)),
            Self::Script { text, .. } => (&*keys::SCRIPT, Value::Binary(text.clone())),
        };
        Value::Dict(Dict::new_sync().insert(tag.clone(), payload))
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
    request: Arc<str>,
    extends: Arc<[String]>,
}

/// Immutable provenance for one compilation invocation. Import traces retain
/// source identities and namespace labels, but never module or environment
/// values. Inline source bytes are shared through `Bytes` clones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompilationTrace {
    invocation: CompilationInvocationId,
    source: SourceIdentity,
    namespace: Arc<[String]>,
    imported_from: Option<ImportOrigin>,
}

impl CompilationTrace {
    pub(crate) fn root(
        invocation: CompilationInvocationId,
        source: SourceIdentity,
        namespace: Arc<[String]>,
    ) -> Self {
        Self {
            invocation,
            source,
            namespace,
            imported_from: None,
        }
    }

    pub(crate) fn imported(
        invocation: CompilationInvocationId,
        source: SourceIdentity,
        namespace: Arc<[String]>,
        parent: Arc<Self>,
        request: Arc<str>,
        extends: Arc<[String]>,
    ) -> Self {
        Self {
            invocation,
            source,
            namespace,
            imported_from: Some(ImportOrigin {
                parent,
                request,
                extends,
            }),
        }
    }

    pub(crate) fn source_label(&self) -> &Arc<str> {
        self.source.label()
    }

    pub(crate) fn origin_value(&self) -> Value {
        let Value::Dict(origin) = self.frame_value() else {
            unreachable!()
        };
        Value::Dict(origin.insert((*keys::IMPORT_CHAIN).clone(), self.import_chain_value()))
    }

    fn import_chain_value(&self) -> Value {
        let mut chain = Vec::new();
        let mut current = self;
        while let Some(import) = &current.imported_from {
            chain.push(import.clone());
            current = &import.parent;
        }
        chain.reverse();
        Value::List(List::from_values(
            chain
                .into_iter()
                .map(|import| import.edge_value())
                .collect(),
        ))
    }

    fn frame_value(&self) -> Value {
        Value::Dict(
            Dict::new_sync()
                .insert((*keys::INVOCATION).clone(), self.invocation.value())
                .insert((*keys::SOURCE).clone(), self.source.value())
                .insert((*keys::NAMESPACE).clone(), namespace_value(&self.namespace)),
        )
    }
}

impl ImportOrigin {
    fn edge_value(&self) -> Value {
        let request = Value::Dict(Dict::new_sync().insert(
            (*keys::FILE).clone(),
            Value::binary_from_text(&self.request),
        ));
        Value::Dict(
            Dict::new_sync()
                .insert((*keys::IMPORTER).clone(), self.parent.frame_value())
                .insert((*keys::REQUEST).clone(), request)
                .insert((*keys::EXTENDS).clone(), namespace_value(&self.extends)),
        )
    }
}

fn namespace_value(namespace: &[String]) -> Value {
    Value::List(List::from_values(
        namespace
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
            Arc::from(["child".to_owned()]),
        ));
        let leaf = CompilationTrace::imported(
            CompilationInvocationId::new(3),
            SourceIdentity::file("lib/leaf.g"),
            Arc::from(["pkg".to_owned(), "child".to_owned()]),
            child,
            Arc::from("leaf.g"),
            Arc::from([]),
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
            Some(&Value::Dict(Dict::new_sync().insert(
                (*keys::FILE).clone(),
                Value::binary_from_text("lib/leaf.g")
            )))
        );
        let Some(Value::List(namespace)) = origin.get(&*keys::NAMESPACE) else {
            panic!("origin should contain its global namespace");
        };
        assert_eq!(
            list_values(namespace),
            [
                Value::binary_from_text("pkg"),
                Value::binary_from_text("child")
            ]
        );
        let Some(Value::List(imports)) = origin.get(&*keys::IMPORT_CHAIN) else {
            panic!("origin should contain an import chain");
        };
        let imports = list_values(imports);
        assert_eq!(imports.len(), 2);
        let Value::Dict(root_edge) = &imports[0] else {
            unreachable!()
        };
        let Value::Dict(child_edge) = &imports[1] else {
            unreachable!()
        };
        let Some(Value::Dict(root_request)) = root_edge.get(&*keys::REQUEST) else {
            panic!("import edge should contain a tagged request");
        };
        assert_eq!(
            root_request.get(&*keys::FILE),
            Some(&Value::binary_from_text("lib/child.g"))
        );
        let Some(Value::List(extends)) = root_edge.get(&*keys::EXTENDS) else {
            panic!("import edge should say which relative namespace it extends");
        };
        assert_eq!(list_values(extends), [Value::binary_from_text("child")]);
        let Some(Value::Dict(child_request)) = child_edge.get(&*keys::REQUEST) else {
            panic!("import edge should contain a tagged request");
        };
        assert_eq!(
            child_request.get(&*keys::FILE),
            Some(&Value::binary_from_text("leaf.g"))
        );
        let Some(Value::Dict(child_importer)) = child_edge.get(&*keys::IMPORTER) else {
            panic!("import edge should identify its importer");
        };
        assert_eq!(
            child_importer.get(&*keys::INVOCATION),
            Some(&Value::Number(Number::from_u64(2)))
        );
    }

    #[test]
    fn inline_script_source_is_tagged_with_its_text() {
        let trace = CompilationTrace::root(
            CompilationInvocationId::new(1),
            SourceIdentity::script("<script.g>", Bytes::from_static(b"language g0\nbroken =\n")),
            Arc::from(["assembly".to_owned()]),
        );
        let Value::Dict(origin) = trace.origin_value() else {
            unreachable!()
        };
        let Some(Value::Dict(source)) = origin.get(&*keys::SOURCE) else {
            panic!("source should be tagged");
        };
        assert_eq!(
            source.get(&*keys::SCRIPT),
            Some(&Value::Binary(Bytes::from_static(
                b"language g0\nbroken =\n"
            )))
        );
        assert!(source.get(&*keys::FILE).is_none());
    }
}
