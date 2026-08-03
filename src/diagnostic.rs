use std::fmt;
use std::sync::Arc;

use crate::core::{Builtin, CoreValueFactory, Dict, List, OpaqueValue, Value, keys};
use crate::eval;
use crate::number::Number;
use crate::source::{ContentDigest, SourceArtifact, SourceIdentity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
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
    digest: ContentDigest,
    namespace: Arc<[String]>,
    imported_from: Option<ImportOrigin>,
}

struct CompilationOrigin {
    value: Value,
}

pub(crate) fn opaque_compilation_origin(trace: &CompilationTrace) -> Value {
    Value::Opaque(OpaqueValue::new(Arc::new(CompilationOrigin {
        value: trace.origin_value(),
    })))
}

pub(crate) fn inspect_compilation_origin(origin: &OpaqueValue) -> Option<Value> {
    origin
        .downcast::<CompilationOrigin>()
        .map(|origin| origin.value.clone())
}

impl CompilationTrace {
    pub(crate) fn root(
        invocation: CompilationInvocationId,
        source: &SourceArtifact,
        namespace: Arc<[String]>,
    ) -> Self {
        Self {
            invocation,
            source: source.identity().clone(),
            digest: source.digest(),
            namespace,
            imported_from: None,
        }
    }

    pub(crate) fn imported(
        invocation: CompilationInvocationId,
        source: &SourceArtifact,
        namespace: Arc<[String]>,
        parent: Arc<Self>,
        request: Arc<str>,
        extends: Arc<[String]>,
    ) -> Self {
        Self {
            invocation,
            source: source.identity().clone(),
            digest: source.digest(),
            namespace,
            imported_from: Some(ImportOrigin {
                parent,
                request,
                extends,
            }),
        }
    }

    pub(crate) fn source_label(&self) -> &str {
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
                .insert((*keys::DIGEST).clone(), self.digest.value())
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
    pub(crate) fn value(self, values: &CoreValueFactory) -> Value {
        match self {
            Self::Info => values.info(),
            Self::Warning => values.warn(),
            Self::Error => values.error(),
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

pub(crate) fn assembler_metadata(
    values: &CoreValueFactory,
    severity: Severity,
    origin: Option<Value>,
) -> Dict {
    let mut message = Dict::new_sync().insert((*keys::SEVERITY).clone(), severity.value(values));
    if let Some(origin) = origin {
        message = message.insert((*keys::ORIGIN).clone(), origin);
    }
    Dict::new_sync().insert((*keys::MSG).clone(), Value::Dict(message))
}

/// Applies one set of object updates as a definitions mixin. Keeping this
/// operation separate lets observers add their own context without mutating
/// the original emission.
pub(crate) fn apply_updates(
    values: &CoreValueFactory,
    message: Value,
    updates: Value,
) -> Result<Value, crate::core::EvaluationHalt> {
    let context = crate::evaluation::EvalContext::isolated(values.clone());
    let extension_defs = Value::builtin_call(values, Builtin::ObjectOverrideDefs, vec![updates]);
    let value = eval::apply_values(
        &context,
        Value::Builtin(Builtin::ObjectWithDefs),
        vec![message, extension_defs],
    )?;
    eval::eval_value(&context, &value)
}

/// Turns a diagnostic emission into an object when needed, then applies an
/// independent compiler- or observer-owned mixin without changing the source
/// emission.
pub(crate) fn apply_emission_updates(
    values: &CoreValueFactory,
    message: Value,
    updates: Value,
) -> Result<Value, crate::core::EvaluationHalt> {
    let message = diagnostic_object(values, message)?;
    apply_updates(values, message, updates)
}

/// Prepends one semantic demand frame to the conventional diagnostic context.
///
/// Evaluation failures already use outermost-to-innermost ordering. Keeping
/// host-owned observation frames in that same list lets clients add context
/// without rewriting the diagnostic's source text.
pub(crate) fn prepend_context(
    message: Value,
    context: Value,
) -> Result<Value, crate::core::EvaluationHalt> {
    prepend_contexts(message, &[context])
}

/// Prepends semantic demand frames while preserving context supplied by the
/// original diagnostic emission. An empty prefix still normalizes
/// `msg.context` to a list.
pub(crate) fn prepend_contexts(
    message: Value,
    contexts: &[Value],
) -> Result<Value, crate::core::EvaluationHalt> {
    let Value::Dict(message) = message else {
        return Err(crate::core::EvaluationHalt::new(
            "diagnostic context requires an immediately structured diagnostic",
        ));
    };
    let interface = match message.get(&*keys::MSG) {
        Some(Value::Dict(interface)) => interface.clone(),
        _ => Dict::new_sync(),
    };
    let existing = match interface.get(&*keys::CONTEXT) {
        Some(Value::List(contexts)) => contexts.clone(),
        Some(context) => List::from_values(vec![context.clone()]),
        None => List::empty(),
    };
    let contexts = List::concat(List::from_values(contexts.to_vec()), existing);
    let interface = interface.insert((*keys::CONTEXT).clone(), Value::List(contexts));
    Ok(Value::Dict(
        message.insert((*keys::MSG).clone(), Value::Dict(interface)),
    ))
}

/// Runtime-aware context projection for evaluator failures. Unlike the
/// structural fast path, this may demand an error emission far enough to
/// expose its diagnostic object before adding the context list.
pub(crate) fn prepend_contexts_with(
    values: &CoreValueFactory,
    message: Value,
    contexts: &[Value],
) -> Result<Value, crate::core::EvaluationHalt> {
    let message = diagnostic_object(values, message)?;
    let context = crate::evaluation::EvalContext::isolated(values.clone());
    let existing = match &message {
        Value::Dict(message) => match message.get(&*keys::MSG) {
            Some(interface) => match eval::eval_value(&context, interface)? {
                Value::Dict(interface) => match interface.get(&*keys::CONTEXT) {
                    Some(Value::List(contexts)) => contexts.clone(),
                    Some(context) => List::from_values(vec![context.clone()]),
                    None => List::empty(),
                },
                _ => List::empty(),
            },
            None => List::empty(),
        },
        _ => unreachable!("diagnostic_object always returns a dictionary"),
    };
    let contexts = List::concat(List::from_values(contexts.to_vec()), existing);
    let updates = Value::Dict(Dict::new_sync().insert(
        (*keys::MSG).clone(),
        Value::Dict(Dict::new_sync().insert((*keys::CONTEXT).clone(), Value::List(contexts))),
    ));
    apply_updates(values, message, updates)
}

/// Applies assembler-owned metadata as a real object definitions mixin so the
/// resulting `spec` also records the extension for subsequent observers.
pub(crate) fn enrich(
    values: &CoreValueFactory,
    message: Value,
    severity: Severity,
    origin: Option<Value>,
) -> Result<Value, crate::core::EvaluationHalt> {
    let message = diagnostic_object(values, message)?;
    apply_updates(
        values,
        message,
        Value::Dict(assembler_metadata(values, severity, origin)),
    )
}

fn diagnostic_object(
    values: &CoreValueFactory,
    message: Value,
) -> Result<Value, crate::core::EvaluationHalt> {
    let context = crate::evaluation::EvalContext::isolated(values.clone());
    let message = eval::eval_value(&context, &message)?;
    let has_defined_spec = match &message {
        Value::Dict(message) => match message.get(&*keys::SPEC) {
            Some(spec) => {
                let spec = eval::eval_value(&context, spec)?;
                !matches!(spec, Value::Dict(spec) if spec.is_empty())
            }
            None => false,
        },
        _ => false,
    };
    let message = if has_defined_spec {
        message
    } else {
        let message = eval::apply_values(
            &context,
            Value::Builtin(Builtin::ObjectFromDict),
            vec![message],
        )?;
        eval::eval_value(&context, &message)?
    };
    Ok(message)
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

pub(crate) fn conventional_summary_with(
    values: &CoreValueFactory,
    message: &Value,
) -> (Option<usize>, Option<Arc<str>>) {
    let context = crate::evaluation::EvalContext::isolated(values.clone());
    let Ok(Value::Dict(message)) = eval::eval_value(&context, message) else {
        return (None, None);
    };
    let Some(interface) = message.get(&*keys::MSG) else {
        return (None, None);
    };
    let Ok(Value::Dict(interface)) = eval::eval_value(&context, interface) else {
        return (None, None);
    };
    let text = interface.get(&*keys::TEXT).and_then(|value| {
        let Value::Binary(bytes) = eval::eval_value(&context, value).ok()? else {
            return None;
        };
        Some(Arc::from(String::from_utf8_lossy(&bytes).as_ref()))
    });
    let line = interface
        .get(&*keys::LOCATION)
        .and_then(|value| eval::eval_value(&context, value).ok())
        .and_then(|value| match value {
            Value::Dict(location) => location.get(&*keys::LINE).cloned(),
            _ => None,
        })
        .and_then(|value| eval::eval_value(&context, &value).ok())
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
    use bytes::Bytes;

    fn file_source(path: &str) -> SourceArtifact {
        SourceArtifact::new(Bytes::from_static(b"source"), SourceIdentity::file(path))
    }

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
        let root_source = file_source("root.g");
        let root = Arc::new(CompilationTrace::root(
            CompilationInvocationId::new(1),
            &root_source,
            Arc::from(["pkg".to_owned()]),
        ));
        let child_source = file_source("lib/child.g");
        let child = Arc::new(CompilationTrace::imported(
            CompilationInvocationId::new(2),
            &child_source,
            Arc::from(["pkg".to_owned(), "child".to_owned()]),
            root,
            Arc::from("lib/child.g"),
            Arc::from(["child".to_owned()]),
        ));
        let leaf_source = file_source("lib/leaf.g");
        let leaf = CompilationTrace::imported(
            CompilationInvocationId::new(3),
            &leaf_source,
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
        assert_eq!(
            origin.get(&*keys::DIGEST),
            Some(&ContentDigest::of(b"source").value())
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
        let bytes = Bytes::from_static(b"language g0\nbroken =\n");
        let source =
            SourceArtifact::new(bytes.clone(), SourceIdentity::script("<script.g>", bytes));
        let trace = CompilationTrace::root(
            CompilationInvocationId::new(1),
            &source,
            Arc::from(["assembly".to_owned()]),
        );
        let Value::Dict(origin) = trace.origin_value() else {
            unreachable!()
        };
        let Some(Value::Dict(source)) = origin.get(&*keys::SOURCE) else {
            panic!("source should be tagged");
        };
        assert_eq!(
            source.get(&crate::core::Key::atom_from_text("script")),
            Some(&Value::Binary(Bytes::from_static(
                b"language g0\nbroken =\n"
            )))
        );
        assert!(source.get(&*keys::FILE).is_none());
    }
}
