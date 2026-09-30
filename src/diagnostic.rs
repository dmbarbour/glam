use std::fmt;
use std::sync::Arc;

use crate::core::{
    Builtin, CoreValueFactory, Dict, EvaluationFailure, EvaluationHalt, Key, List,
    OpaquePayloadFamily, OpaquePayloadRecord, OpaqueValue, RuntimeValueAccess, Value, keys,
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

/// Transitional runtime-aware projection of one evaluator failure.
///
/// Unlike `eval::failure_diagnostic_value_in`, this path may evaluate the
/// emission while normalizing it into a diagnostic object. It therefore must
/// not receive or retain an active value-access region. D.2g replaces its raw
/// compatibility transport at the public diagnostic boundary.
pub(crate) fn failure_diagnostic_root_with(
    values: &CoreValueFactory,
    failure: &EvaluationFailure,
) -> RuntimeValueRoot {
    let (emission, contexts) = values.with_runtime_value_access(|access| {
        let emission = match failure.emission_value_in(&access) {
            Some(Value::Binary(text)) => {
                text_message_in(&access, None, String::from_utf8_lossy(text))
            }
            Some(emission) => access.duplicate_value(emission),
            None => text_message_in(&access, None, failure.to_string()),
        };
        let contexts = failure
            .contexts_in(&access)
            .iter()
            .map(|context| access.duplicate_value(context))
            .collect();
        (
            access.root_runtime_value(emission),
            access.root_runtime_value(Value::List(List::from_values(contexts))),
        )
    });
    prepend_contexts_root(values, emission.clone(), contexts).unwrap_or_else(|_| {
        values.construct_runtime_value_root(|access| {
            let emission = emission.clone_core_with(access);
            fallback_failure_diagnostic(
                access,
                failure,
                Some(emission),
                Value::List(List::from_values(
                    failure
                        .contexts_in(access)
                        .iter()
                        .map(|context| access.duplicate_value(context))
                        .collect(),
                )),
            )
        })
    })
}

pub(crate) fn halt_diagnostic_root_with(
    values: &CoreValueFactory,
    halt: &EvaluationHalt,
) -> Option<RuntimeValueRoot> {
    halt.permanent_failure()
        .map(|failure| failure_diagnostic_root_with(values, failure))
}

fn fallback_failure_diagnostic(
    _access: &RuntimeValueAccess<'_>,
    failure: &EvaluationFailure,
    emission: Option<Value>,
    contexts: Value,
) -> Value {
    let mut message = Dict::new_sync()
        .insert(
            (*keys::TEXT).clone(),
            Value::binary_from_text(&failure.to_string()),
        )
        .insert((*keys::CONTEXT).clone(), contexts);
    if let Some(emission) = emission {
        message = message.insert((*keys::VALUE).clone(), emission);
    }
    Value::Dict(Dict::new_sync().insert((*keys::MSG).clone(), Value::Dict(message)))
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

/// Applies one set of object updates as a definitions mixin. Keeping this
/// operation separate lets observers add their own context without mutating
/// the original emission.
/// Root-preserving form of [`apply_emission_updates`] for orchestration which
/// must release managed access before diagnostic normalization can demand
/// values or run object builtins.
pub(crate) fn apply_emission_updates_root(
    values: &CoreValueFactory,
    message: RuntimeValueRoot,
    updates: RuntimeValueRoot,
) -> Result<RuntimeValueRoot, crate::core::EvaluationHalt> {
    let context = crate::evaluation::EvalContext::isolated(values.clone());
    let message = diagnostic_object_root(&context, message)?;
    apply_updates_root(&context, message, updates)
}

pub(crate) fn prepend_contexts_in(
    access: &RuntimeValueAccess<'_>,
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
        Some(context) => List::from_values(vec![access.duplicate_value(context)]),
        None => List::empty(),
    };
    let prefix = contexts
        .iter()
        .map(|context| access.duplicate_value(context))
        .collect();
    let contexts = List::concat(List::from_values(prefix), existing);
    let interface = interface.insert((*keys::CONTEXT).clone(), Value::List(contexts));
    Ok(Value::Dict(
        message.insert((*keys::MSG).clone(), Value::Dict(interface)),
    ))
}

#[cfg(test)]
pub(crate) fn prepend_contexts(
    message: Value,
    contexts: &[Value],
) -> Result<Value, crate::core::EvaluationHalt> {
    let values = crate::compiler::test_value_factory();
    values.with_runtime_value_access(|access| prepend_contexts_in(&access, message, contexts))
}

fn apply_updates_root(
    context: &crate::evaluation::EvalContext,
    message: RuntimeValueRoot,
    updates: RuntimeValueRoot,
) -> Result<RuntimeValueRoot, crate::core::EvaluationHalt> {
    let extension_defs = context.compose_builtin(Builtin::ObjectOverrideDefs, |access| {
        vec![updates.clone_core_with(access)]
    });
    context.evaluate_builtin_whnf(Builtin::ObjectWithDefs, |access| {
        vec![
            message.clone_core_with(access),
            extension_defs.clone_core_with(access),
        ]
    })
}

pub(crate) fn prepend_contexts_root(
    values: &CoreValueFactory,
    message: RuntimeValueRoot,
    contexts: RuntimeValueRoot,
) -> Result<RuntimeValueRoot, crate::core::EvaluationHalt> {
    let context = crate::evaluation::EvalContext::isolated(values.clone());
    let message = diagnostic_object_root(&context, message)?;
    let interface = values.with_runtime_value_access(|access| {
        message.with_core(&access, |message| match message {
            Value::Dict(message) => message
                .get(&*keys::MSG)
                .map(|interface| access.root_runtime_value(access.duplicate_value(interface))),
            _ => None,
        })
    });
    let interface = match interface.flatten() {
        Some(interface) => Some(context.evaluate_root_whnf(interface)?),
        None => None,
    };
    let updates = values.construct_runtime_value_root(|access| {
        let prefix = contexts
            .with_core(access, |contexts| match contexts {
                Value::List(contexts) => contexts.clone(),
                _ => List::empty(),
            })
            .unwrap_or_else(List::empty);
        let existing = interface
            .as_ref()
            .and_then(|interface| {
                interface.with_core(access, |interface| match interface {
                    Value::Dict(interface) => interface
                        .get(&*keys::CONTEXT)
                        .map(|context| access.duplicate_value(context)),
                    _ => None,
                })
            })
            .flatten();
        let existing = match existing {
            Some(Value::List(contexts)) => contexts,
            Some(context) => List::from_values(vec![context]),
            None => List::empty(),
        };
        let contexts = List::concat(prefix, existing);
        Value::Dict(Dict::new_sync().insert(
            (*keys::MSG).clone(),
            Value::Dict(Dict::new_sync().insert((*keys::CONTEXT).clone(), Value::List(contexts))),
        ))
    });
    apply_updates_root(&context, message, updates)
}

pub(crate) fn enrich_root(
    values: &CoreValueFactory,
    message: RuntimeValueRoot,
    severity: Severity,
    origin: Option<RuntimeValueRoot>,
) -> Result<RuntimeValueRoot, crate::core::EvaluationHalt> {
    let updates = values.construct_runtime_value_root(|access| {
        let origin = origin.as_ref().map(|origin| origin.clone_core_with(access));
        Value::Dict(assembler_metadata(access, values, severity, origin))
    });
    apply_emission_updates_root(values, message, updates)
}

/// Prepends semantic demand frames while preserving context supplied by the
/// original diagnostic emission. An empty prefix still normalizes
/// `msg.context` to a list.
fn diagnostic_object_root(
    context: &crate::evaluation::EvalContext,
    message: crate::runtime::RuntimeValueRoot,
) -> Result<crate::runtime::RuntimeValueRoot, crate::core::EvaluationHalt> {
    let message = context.evaluate_root_whnf(message)?;
    let spec = context.values().with_runtime_value_access(|access| {
        message
            .with_core(&access, |message| match message {
                Value::Dict(message) => message
                    .get(&*keys::SPEC)
                    .map(|spec| access.root_runtime_value(access.duplicate_value(spec))),
                _ => None,
            })
            .flatten()
    });
    let has_defined_spec = if let Some(spec) = spec {
        let spec = context.evaluate_root_whnf(spec)?;
        context.values().with_runtime_value_access(|access| {
            spec.with_core(
                &access,
                |spec| !matches!(spec, Value::Dict(spec) if spec.is_empty()),
            )
            .unwrap_or(false)
        })
    } else {
        false
    };
    if has_defined_spec {
        Ok(message)
    } else {
        context.evaluate_builtin_whnf(Builtin::ObjectFromDict, |access| {
            vec![message.clone_core_with(access)]
        })
    }
}

pub(crate) fn conventional_summary(
    _access: &RuntimeValueAccess<'_>,
    message: &Value,
) -> (Option<usize>, Option<Arc<str>>) {
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
        values.with_runtime_value_access(|access| trace.origin_value(&access))
    }

    fn digest_value(digest: ContentDigest) -> Value {
        let values = crate::compiler::test_value_factory();
        values.with_runtime_value_access(|access| digest.value(&access))
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
