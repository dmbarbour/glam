use std::fmt;
use std::sync::Arc;

use crate::core::{
    Builtin, CoreValueFactory, Dict, EvaluationFailure, Key, List, OpaquePayloadFamily,
    OpaquePayloadRecord, OpaqueValue, RuntimeValueAccess, Value, keys,
};
use crate::number::Number;
use crate::runtime::RuntimeValueRoot;
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

    fn value(self, _access: &RuntimeValueAccess<'_>) -> Value {
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
    trace: CompilationTrace,
}

#[cfg(test)]
pub(crate) fn assert_compilation_origin_family_shape() {
    fn inspect_origin(origin: &CompilationOrigin) {
        let CompilationOrigin { trace } = origin;
        let CompilationTrace {
            invocation,
            source,
            digest,
            namespace,
            imported_from,
        } = trace;
        let _: &CompilationInvocationId = invocation;
        let _: &SourceIdentity = source;
        let _: &ContentDigest = digest;
        let _: &Arc<[String]> = namespace;
        let _: &Option<ImportOrigin> = imported_from;
        if let Some(imported_from) = imported_from {
            let ImportOrigin {
                parent,
                request,
                extends,
            } = imported_from;
            let _: &Arc<CompilationTrace> = parent;
            let _: &Arc<str> = request;
            let _: &Arc<[String]> = extends;
        }
    }

    let _: fn(&CompilationOrigin) = inspect_origin;
    assert_eq!(
        <CompilationOrigin as OpaquePayloadFamily>::PAYLOAD_RECORD
            .fields()
            .2,
        "edge-free token"
    );
}

// SAFETY: the compilation trace contains source identities, digests, static
// namespace labels, and parent trace provenance only. It contains no Glam
// value, runtime root, managed pointer, or active runtime capability.
unsafe impl OpaquePayloadFamily for CompilationOrigin {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::edge_free("compilation origin provenance", "src/diagnostic.rs");
}

pub(crate) fn opaque_compilation_origin(
    access: &RuntimeValueAccess<'_>,
    values: &CoreValueFactory,
    trace: &CompilationTrace,
) -> Value {
    debug_assert!(access.belongs_to(values));
    Value::Opaque(OpaqueValue::new(
        values,
        Arc::new(CompilationOrigin {
            trace: trace.clone(),
        }),
    ))
}

pub(crate) fn inspect_compilation_origin(
    access: &RuntimeValueAccess<'_>,
    values: &CoreValueFactory,
    origin: &OpaqueValue,
) -> Option<Value> {
    origin
        .downcast::<CompilationOrigin>(values)
        .map(|origin| origin.trace.origin_value(access))
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

    pub(crate) fn origin_value(&self, access: &RuntimeValueAccess<'_>) -> Value {
        let Value::Dict(origin) = self.frame_value(access) else {
            unreachable!()
        };
        Value::Dict(origin.insert(
            (*keys::IMPORT_CHAIN).clone(),
            self.import_chain_value(access),
        ))
    }

    fn import_chain_value(&self, access: &RuntimeValueAccess<'_>) -> Value {
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
                .map(|import| import.edge_value(access))
                .collect(),
        ))
    }

    fn frame_value(&self, access: &RuntimeValueAccess<'_>) -> Value {
        Value::Dict(
            Dict::new_sync()
                .insert((*keys::INVOCATION).clone(), self.invocation.value(access))
                .insert((*keys::SOURCE).clone(), self.source.value(access))
                .insert((*keys::DIGEST).clone(), self.digest.value(access))
                .insert(
                    (*keys::NAMESPACE).clone(),
                    namespace_value(access, &self.namespace),
                ),
        )
    }
}

impl ImportOrigin {
    fn edge_value(&self, access: &RuntimeValueAccess<'_>) -> Value {
        let request = Value::Dict(Dict::new_sync().insert(
            (*keys::FILE).clone(),
            Value::binary_from_text(&self.request),
        ));
        Value::Dict(
            Dict::new_sync()
                .insert((*keys::IMPORTER).clone(), self.parent.frame_value(access))
                .insert((*keys::REQUEST).clone(), request)
                .insert(
                    (*keys::EXTENDS).clone(),
                    namespace_value(access, &self.extends),
                ),
        )
    }
}

fn namespace_value(_access: &RuntimeValueAccess<'_>, namespace: &[String]) -> Value {
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
    pub(crate) fn value(self, access: &RuntimeValueAccess<'_>, values: &CoreValueFactory) -> Value {
        debug_assert!(access.belongs_to(values));
        match self {
            Self::Info => access.info(),
            Self::Warning => access.warn(),
            Self::Error => access.error(),
        }
    }
}

/// Builds the conventional bootstrap message body. Severity and assembler
/// provenance are emission-effect metadata and are mixed in later.
pub(crate) fn text_message_in(
    _access: &RuntimeValueAccess<'_>,
    line: Option<usize>,
    message: impl AsRef<str>,
) -> Value {
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

#[cfg(test)]
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

/// Transitional context-frame constructor for orchestration, reflection, and
/// compiler callers which have not yet adopted their regional value-access
/// boundary. Evaluator code uses `eval::evaluation_context_frame_in` instead.
pub(crate) fn evaluation_context_frame_in(
    access: &RuntimeValueAccess<'_>,
    operation: &str,
) -> Value {
    evaluation_context_frame_with_args_in(access, operation, Dict::new_sync())
}

pub(crate) fn evaluation_context_frame_with_args_in(
    _access: &RuntimeValueAccess<'_>,
    operation: &str,
    args: Dict,
) -> Value {
    let operation = Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
        operation,
    )));
    let mut detail = Dict::new_sync().insert((*keys::OP).clone(), operation);
    if !args.is_empty() {
        detail = detail.insert((*keys::ARGS).clone(), Value::Dict(args));
    }
    Value::Dict(Dict::new_sync().insert((*keys::EVAL).clone(), Value::Dict(detail)))
}

#[cfg(test)]
pub(crate) fn evaluation_context_frame(operation: &str) -> Value {
    evaluation_context_frame_with_args(operation, Dict::new_sync())
}

#[cfg(test)]
pub(crate) fn evaluation_context_frame_with_args(operation: &str, args: Dict) -> Value {
    let operation = Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
        operation,
    )));
    let mut detail = Dict::new_sync().insert((*keys::OP).clone(), operation);
    if !args.is_empty() {
        detail = detail.insert((*keys::ARGS).clone(), Value::Dict(args));
    }
    Value::Dict(Dict::new_sync().insert((*keys::EVAL).clone(), Value::Dict(detail)))
}

/// The diagnostic for `failure`, composed without evaluating it
/// (`senders-never-evaluate-diagnostics`): its emission, or its text,
/// normalized to an object, with its context frames prepended to
/// `msg.context`. A renderer evaluates it.
pub(crate) fn failure_diagnostic_in(
    access: &RuntimeValueAccess<'_>,
    failure: &EvaluationFailure,
) -> Value {
    let emission = match failure.emission_value_in(access) {
        Some(Value::Binary(text)) => text_message_in(access, None, String::from_utf8_lossy(text)),
        Some(emission) => access.duplicate_value(emission),
        None => text_message_in(access, None, failure.to_string()),
    };
    let frames = failure
        .contexts_in(access)
        .iter()
        .map(|context| access.duplicate_value(context))
        .collect();
    diagnostic_with_frames_in(access, emission, frames)
}

/// `message` as a diagnostic object with `frames` prepended to its
/// `msg.context`, composed without evaluating either:
/// `diagnostic_object message with { msg.context ::= \ prior -> frames ++
/// prior }`. A failure to evaluate the message carries an
/// `eval:{op:'diagnostic_message}` frame.
pub(crate) fn diagnostic_with_frames_in(
    access: &RuntimeValueAccess<'_>,
    message: Value,
    frames: Vec<Value>,
) -> Value {
    let object = Value::builtin_call_in(access, Builtin::DiagnosticObject, vec![message]);
    let context_defs = Value::PartialBuiltin(crate::core::BuiltinCall {
        builtin: Builtin::DiagnosticContextDefs,
        arguments: Arc::from([Value::List(List::from_values(frames))]),
    });
    let diagnostic =
        Value::builtin_call_in(access, Builtin::ObjectWithDefs, vec![object, context_defs]);
    let annotation = Value::Dict(Dict::new_sync().insert(
        (*keys::CONTEXT).clone(),
        evaluation_context_frame_in(access, "diagnostic_message"),
    ));
    Value::builtin_call_in(access, Builtin::Anno, vec![annotation, diagnostic])
}

/// `message` as a diagnostic object extended by the object `updates`,
/// composed without evaluating either:
/// `diagnostic_object message with updates`.
pub(crate) fn apply_updates_in(
    access: &RuntimeValueAccess<'_>,
    message: Value,
    updates: Value,
) -> Value {
    let object = Value::builtin_call_in(access, Builtin::DiagnosticObject, vec![message]);
    let defs = Value::PartialBuiltin(crate::core::BuiltinCall {
        builtin: Builtin::ObjectOverrideDefs,
        arguments: Arc::from([updates]),
    });
    Value::builtin_call_in(access, Builtin::ObjectWithDefs, vec![object, defs])
}

/// The diagnostic for `failure`, composed without evaluating it, as a
/// boundary of its own.
pub(crate) fn failure_diagnostic_root_with(
    values: &CoreValueFactory,
    failure: &EvaluationFailure,
) -> RuntimeValueRoot {
    values.with_boundary_access(|access| {
        access.root_runtime_value(failure_diagnostic_in(&access, failure))
    })
}

pub(crate) fn assembler_metadata(
    access: &RuntimeValueAccess<'_>,
    values: &CoreValueFactory,
    severity: Severity,
    origin: Option<Value>,
) -> Dict {
    let mut message =
        Dict::new_sync().insert((*keys::SEVERITY).clone(), severity.value(access, values));
    if let Some(origin) = origin {
        message = message.insert((*keys::ORIGIN).clone(), origin);
    }
    Dict::new_sync().insert((*keys::MSG).clone(), Value::Dict(message))
}

/// The message `value` presents without evaluating anything: itself, a
/// lazy's cached result, or, for an unevaluated diagnostic composition
/// (`senders-never-evaluate-diagnostics`), the message it composes. The
/// mixins senders compose add context, metadata and viewer fields; the
/// message keeps its text and location.
fn immediate_message_in(access: &RuntimeValueAccess<'_>, value: &Value) -> Option<Value> {
    // Compositions nest one level per sender that enriched the diagnostic.
    const DEPTH_LIMIT: usize = 32;
    let mut current = access.duplicate_value(value);
    for _ in 0..DEPTH_LIMIT {
        let Value::Lazy(lazy) = &current else {
            return Some(current);
        };
        let lazy = lazy.access(access);
        if let Some(result) = lazy.cached() {
            current = result.ok()?.into_value_in(access);
            continue;
        }
        let Some(crate::core::LazySource::Builtin(call)) = lazy.source_snapshot() else {
            return None;
        };
        current = match (call.builtin, call.arguments.as_ref()) {
            (Builtin::Anno, [_, message])
            | (Builtin::ObjectWithDefs, [message, _])
            | (Builtin::DiagnosticObject, [message]) => access.duplicate_value(message),
            _ => return None,
        };
    }
    None
}

pub(crate) fn conventional_summary(
    access: &RuntimeValueAccess<'_>,
    message: &Value,
) -> (Option<usize>, Option<Arc<str>>) {
    let Some(message) = immediate_message_in(access, message) else {
        return (None, None);
    };
    let Value::Dict(message) = &message else {
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
    use bytes::Bytes;

    fn file_source(path: &str) -> SourceArtifact {
        SourceArtifact::new(Bytes::from_static(b"source"), SourceIdentity::file(path))
    }

    fn list_values(values: &crate::core::CoreValueFactory, list: &List) -> Vec<Value> {
        let mut items = Vec::new();
        list.for_each_segment(
            &mut |bytes| panic!("provenance lists must not contain byte segments: {bytes:?}"),
            &mut |segment| {
                items.extend(segment.iter().map(|value| value.duplicate_for_test(values)));
                Ok::<_, ()>(())
            },
        )
        .expect("closed provenance list should not fail");
        items
    }

    fn trace_origin_value(trace: &CompilationTrace) -> Value {
        let values = crate::compiler::test_value_factory();
        values.with_boundary_access(|access| trace.origin_value(&access))
    }

    fn digest_value(digest: ContentDigest) -> Value {
        let values = crate::compiler::test_value_factory();
        values.with_boundary_access(|access| digest.value(&access))
    }

    fn assert_optional_value(
        values: &crate::core::CoreValueFactory,
        actual: Option<&Value>,
        expected: &Value,
    ) {
        let Some(actual) = actual else {
            panic!("expected a diagnostic field")
        };
        values.assert_same_representation_for_test(actual, expected);
    }

    #[test]
    fn opaque_edge_free_families_have_no_runtime_or_managed_edge() {
        assert_compilation_origin_family_shape();
        crate::eval::assert_construction_port_family_shape();
    }

    #[test]
    fn imported_trace_projects_a_root_to_parent_chain() {
        let values = crate::compiler::test_value_factory();
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

        let Value::Dict(origin) = trace_origin_value(&leaf) else {
            unreachable!()
        };
        assert_optional_value(
            &values,
            origin.get(&*keys::INVOCATION),
            &Value::Number(Number::from_u64(3)),
        );
        assert_optional_value(
            &values,
            origin.get(&*keys::SOURCE),
            &Value::Dict(
                Dict::new_sync()
                    .insert((*keys::FILE).clone(), Value::binary_from_text("lib/leaf.g")),
            ),
        );
        assert_optional_value(
            &values,
            origin.get(&*keys::DIGEST),
            &digest_value(ContentDigest::of(b"source")),
        );
        let Some(Value::List(namespace)) = origin.get(&*keys::NAMESPACE) else {
            panic!("origin should contain its global namespace");
        };
        values.assert_same_representation_for_test(
            &list_values(&values, namespace),
            &vec![
                Value::binary_from_text("pkg"),
                Value::binary_from_text("child"),
            ],
        );
        let Some(Value::List(imports)) = origin.get(&*keys::IMPORT_CHAIN) else {
            panic!("origin should contain an import chain");
        };
        let imports = list_values(&values, imports);
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
        assert_optional_value(
            &values,
            root_request.get(&*keys::FILE),
            &Value::binary_from_text("lib/child.g"),
        );
        let Some(Value::List(extends)) = root_edge.get(&*keys::EXTENDS) else {
            panic!("import edge should say which relative namespace it extends");
        };
        values.assert_same_representation_for_test(
            &list_values(&values, extends),
            &vec![Value::binary_from_text("child")],
        );
        let Some(Value::Dict(child_request)) = child_edge.get(&*keys::REQUEST) else {
            panic!("import edge should contain a tagged request");
        };
        assert_optional_value(
            &values,
            child_request.get(&*keys::FILE),
            &Value::binary_from_text("leaf.g"),
        );
        let Some(Value::Dict(child_importer)) = child_edge.get(&*keys::IMPORTER) else {
            panic!("import edge should identify its importer");
        };
        assert_optional_value(
            &values,
            child_importer.get(&*keys::INVOCATION),
            &Value::Number(Number::from_u64(2)),
        );
    }

    #[test]
    fn inline_script_source_is_tagged_with_its_text() {
        let values = crate::compiler::test_value_factory();
        let bytes = Bytes::from_static(b"language g0\nbroken =\n");
        let source =
            SourceArtifact::new(bytes.clone(), SourceIdentity::script("<script.g>", bytes));
        let trace = CompilationTrace::root(
            CompilationInvocationId::new(1),
            &source,
            Arc::from(["assembly".to_owned()]),
        );
        let Value::Dict(origin) = trace_origin_value(&trace) else {
            unreachable!()
        };
        let Some(Value::Dict(source)) = origin.get(&*keys::SOURCE) else {
            panic!("source should be tagged");
        };
        assert_optional_value(
            &values,
            source.get(&crate::core::Key::atom_from_text("script")),
            &Value::Binary(Bytes::from_static(b"language g0\nbroken =\n")),
        );
        assert!(source.get(&*keys::FILE).is_none());
    }
}
