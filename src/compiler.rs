use std::sync::Arc;

use crate::api::CompilationExecution;
use crate::core::{
    Atom, CoreValueFactory, Dict, EvaluationFailure, HostCallRecord, Key, PromisedValue, Value,
    keys,
};
use crate::diagnostic::{CompilationTrace, Severity};
use crate::runtime::RuntimeValueRoot;
use crate::source::{RelativeSourcePath, SourceArtifact};

pub(crate) type ModuleLoader =
    Arc<dyn Fn(ModuleLoadArgs) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> + Send + Sync>;
pub(crate) type BinaryFileLoader =
    Arc<dyn Fn(BinaryLoadArgs) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> + Send + Sync>;
pub(crate) type CompileDiagnosticEmitter = Arc<dyn Fn(Severity, Value) + Send + Sync>;

/// Validates a location-independent local source request. This is deliberately
/// lexical and platform-independent: source code uses `/`, while filesystem
/// interpretation remains assembler-owned.
pub(crate) fn validate_local_source_request(request: &str) -> Result<(), String> {
    RelativeSourcePath::new(request)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone)]
pub(crate) struct BinaryLoadArgs {
    pub(crate) request: RelativeSourcePath,
    pub(crate) importer_source: Option<Arc<SourceArtifact>>,
    pub(crate) importer_trace: Option<Arc<CompilationTrace>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ModuleLoadArgs {
    pub(crate) request: RelativeSourcePath,
    pub(crate) importer_source: Option<Arc<SourceArtifact>>,
    pub(crate) importer_trace: Option<Arc<CompilationTrace>>,
    pub(crate) extends: Arc<[String]>,
    pub(crate) module_path: Arc<[String]>,
    pub(crate) prior_defs: RuntimeValueRoot,
    pub(crate) final_defs: RuntimeValueRoot,
}

#[derive(Clone)]
pub(crate) struct CompileContext {
    // Invocation-scoped inputs and capabilities for a front end. Ordinary
    // values belong to the front end and are not constructed through here.
    values: CoreValueFactory,
    importer_source: Option<Arc<SourceArtifact>>,
    compilation_trace: Option<Arc<CompilationTrace>>,
    opaque_origin: Option<RuntimeValueRoot>,
    module_path: Arc<[String]>,
    prior_defs: RuntimeValueRoot, // definitions visible before compiling this source
    final_defs: RuntimeValueRoot, // promised final definitions for recursive access
    local_module_loader: Option<ModuleLoader>,
    local_binary_loader: Option<BinaryFileLoader>,
    diagnostic_emitter: Option<CompileDiagnosticEmitter>,
    compilation_execution: Option<Arc<CompilationExecution>>,
}

#[cfg(test)]
impl Default for CompileContext {
    fn default() -> Self {
        Self::new(test_value_factory())
    }
}

#[cfg(test)]
pub(crate) fn test_value_factory() -> CoreValueFactory {
    static FACTORY: std::sync::LazyLock<CoreValueFactory> = std::sync::LazyLock::new(|| {
        CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::compiler_test_values(),
        )
    });
    FACTORY.clone()
}

impl CompileContext {
    pub(crate) fn new(values: CoreValueFactory) -> Self {
        let prior_defs = RuntimeValueRoot::new(&values, Value::Dict(Dict::new_sync()));
        let final_defs = RuntimeValueRoot::new(
            &values,
            Value::Promised(PromisedValue::new(&values, "final definitions")),
        );
        Self {
            values: values.scoped(),
            importer_source: None,
            compilation_trace: None,
            opaque_origin: None,
            module_path: Arc::from([]),
            prior_defs,
            final_defs,
            local_module_loader: None,
            local_binary_loader: None,
            diagnostic_emitter: None,
            compilation_execution: None,
        }
    }

    pub(crate) fn from_module_path_with_values<I, S>(values: CoreValueFactory, parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::new(values).with_module_path(parts)
    }

    #[cfg(test)]
    pub(crate) fn from_module_path<I, S>(parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::from_module_path_with_values(test_value_factory(), parts)
    }

    pub(crate) fn values(&self) -> &CoreValueFactory {
        &self.values
    }

    pub(crate) fn with_importer_source(mut self, source: Arc<SourceArtifact>) -> Self {
        self.importer_source = Some(source);
        self
    }

    pub(crate) fn with_compilation_trace(mut self, trace: Arc<CompilationTrace>) -> Self {
        self.opaque_origin = Some(RuntimeValueRoot::new(
            &self.values,
            crate::diagnostic::opaque_compilation_origin(&self.values, &trace),
        ));
        self.compilation_trace = Some(trace);
        self
    }

    pub(crate) fn with_module_path<I, S>(mut self, parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.module_path = Arc::from(
            parts
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        self
    }

    #[cfg(test)]
    pub(crate) fn with_prior_defs(mut self, prior: Value) -> Self {
        self.prior_defs = RuntimeValueRoot::new(&self.values, prior);
        self
    }

    pub(crate) fn with_prior_defs_root(mut self, prior: RuntimeValueRoot) -> Self {
        assert_eq!(prior.runtime_id(), self.values.runtime_id());
        self.prior_defs = prior;
        self
    }

    pub(crate) fn with_final_defs_root(mut self, final_defs: RuntimeValueRoot) -> Self {
        assert_eq!(final_defs.runtime_id(), self.values.runtime_id());
        self.final_defs = final_defs;
        self
    }

    pub(crate) fn with_local_module_loader(mut self, loader: ModuleLoader) -> Self {
        self.local_module_loader = Some(loader);
        self
    }

    pub(crate) fn with_local_binary_loader(mut self, loader: BinaryFileLoader) -> Self {
        self.local_binary_loader = Some(loader);
        self
    }

    pub(crate) fn with_diagnostic_emitter(mut self, emitter: CompileDiagnosticEmitter) -> Self {
        self.diagnostic_emitter = Some(emitter);
        self
    }

    pub(crate) fn with_compilation_execution(
        mut self,
        execution: Arc<CompilationExecution>,
    ) -> Self {
        self.compilation_execution = Some(execution);
        self
    }

    #[cfg(test)]
    pub(crate) fn prior_defs(&self) -> Value {
        self.clone_root(&self.prior_defs)
    }

    pub(crate) fn final_defs(&self) -> Value {
        self.clone_root(&self.final_defs)
    }

    pub(crate) fn prior_defs_root(&self) -> &RuntimeValueRoot {
        &self.prior_defs
    }

    pub(crate) fn final_defs_root(&self) -> &RuntimeValueRoot {
        &self.final_defs
    }

    pub(crate) fn compilation_execution(&self) -> Option<&Arc<CompilationExecution>> {
        self.compilation_execution.as_ref()
    }

    /// Returns the assembler-owned source origin without exposing its fields.
    ///
    /// The same opaque value is cloned for every annotation emitted from this
    /// source context. The front end owns any surrounding span representation.
    pub(crate) fn opaque_origin(&self) -> Option<Value> {
        self.opaque_origin
            .as_ref()
            .map(|origin| self.clone_root(origin))
    }

    /// Returns the abstract global-path value for a path relative to the
    /// current module without revealing its absolute namespace.
    pub(crate) fn abstract_global_path(&self, target: &str) -> Value {
        // TODO: support expression-indexed paths, e.g. foo.bar.[42].baz
        let mut parts = self.module_path.iter().cloned().collect::<Vec<_>>();
        parts.extend(target.split('.').map(ToOwned::to_owned));
        Value::Atom(Atom::from_key(&Key::AbstractGlobalPath(Arc::from(
            parts.into_boxed_slice(),
        ))))
    }

    pub(crate) fn emit_diagnostic(&self, severity: Severity, message: Value) {
        if let Some(emitter) = &self.diagnostic_emitter {
            emitter(severity, message);
        }
    }

    pub(crate) fn unit_value(&self) -> Value {
        self.values.unit()
    }

    /// Requests a module import in the current or a relative child namespace.
    /// Source resolution and absolute namespace qualification remain hidden.
    pub(crate) fn import_module(
        &self,
        request: &str,
        relative_namespace: Option<&str>,
        prior_defs: Value,
        final_defs: Value,
    ) -> Value {
        let request = match RelativeSourcePath::new(request) {
            Ok(request) => request,
            Err(error) => {
                return invalid_import_request(
                    &self.values,
                    request,
                    error.to_string(),
                    self.compilation_trace.as_deref(),
                    self.importer_source.as_deref(),
                );
            }
        };
        let (module_path, extends) = self.qualify_module_path(relative_namespace);
        let args = ModuleLoadArgs {
            request,
            importer_source: self.importer_source.clone(),
            importer_trace: self.compilation_trace.clone(),
            extends,
            module_path,
            prior_defs: RuntimeValueRoot::new(&self.values, prior_defs),
            final_defs: RuntimeValueRoot::new(&self.values, final_defs),
        };
        let label: Arc<str> = Arc::from(format!("import {}", args.request.as_str()));
        let loader = self.local_module_loader.clone();

        Value::external_host_call(
            &self.values,
            label,
            HostCallRecord::external(
                "deferred module import",
                "src/compiler.rs",
                "module loader plus same-runtime prior/final definition roots",
            ),
            move || {
                let Some(loader) = &loader else {
                    return Err(import_failure(
                        format!(
                            "local import `{}` cannot be loaded without a module loader",
                            args.request.as_str()
                        ),
                        args.request.as_str(),
                        args.importer_trace.as_deref(),
                        args.importer_source.as_deref(),
                    ));
                };
                loader(args.clone())
            },
        )
    }

    pub(crate) fn import_binary(&self, request: &str) -> Value {
        let request = match RelativeSourcePath::new(request) {
            Ok(request) => request,
            Err(error) => {
                return invalid_import_request(
                    &self.values,
                    request,
                    error.to_string(),
                    self.compilation_trace.as_deref(),
                    self.importer_source.as_deref(),
                );
            }
        };
        let args = BinaryLoadArgs {
            request,
            importer_source: self.importer_source.clone(),
            importer_trace: self.compilation_trace.clone(),
        };
        let label: Arc<str> = Arc::from(format!("import binary {}", args.request.as_str()));
        let loader = self.local_binary_loader.clone();

        Value::external_host_call(
            &self.values,
            label,
            HostCallRecord::external(
                "deferred binary import",
                "src/compiler.rs",
                "binary loader and edge-free source provenance",
            ),
            move || {
                let Some(loader) = &loader else {
                    return Err(import_failure(
                        format!(
                            "binary import `{}` cannot be loaded without a binary loader",
                            args.request.as_str()
                        ),
                        args.request.as_str(),
                        args.importer_trace.as_deref(),
                        args.importer_source.as_deref(),
                    ));
                };
                loader(args.clone())
            },
        )
    }

    fn qualify_module_path(
        &self,
        relative_namespace: Option<&str>,
    ) -> (Arc<[String]>, Arc<[String]>) {
        let extends: Vec<String> = relative_namespace
            .map(|namespace| namespace.split('.').map(ToOwned::to_owned).collect())
            .unwrap_or_default();
        let mut parts = self.module_path.to_vec();
        parts.extend(extends.iter().cloned());
        (
            Arc::from(parts.into_boxed_slice()),
            Arc::from(extends.into_boxed_slice()),
        )
    }

    fn clone_root(&self, root: &RuntimeValueRoot) -> Value {
        self.values
            .with_runtime_value_access(|access| root.clone_core_with(&access))
    }
}

pub(crate) fn import_failure(
    message: impl AsRef<str>,
    request: &str,
    trace: Option<&CompilationTrace>,
    importer_source: Option<&SourceArtifact>,
) -> Arc<EvaluationFailure> {
    let request = Value::Dict(
        Dict::new_sync().insert((*keys::FILE).clone(), Value::binary_from_text(request)),
    );
    let mut details = Dict::new_sync().insert((*keys::REQUEST).clone(), request);
    if let Some(trace) = trace {
        details = details.insert((*keys::ORIGIN).clone(), trace.origin_value());
    } else if let Some(source) = importer_source {
        details = details.insert((*keys::SOURCE).clone(), source.identity().value());
    }
    let context =
        Value::Dict(Dict::new_sync().insert((*keys::IMPORT).clone(), Value::Dict(details)));
    Arc::new(EvaluationFailure::message(message).with_context(context))
}

fn invalid_import_request(
    values: &CoreValueFactory,
    request: &str,
    message: impl AsRef<str>,
    trace: Option<&CompilationTrace>,
    importer_source: Option<&SourceArtifact>,
) -> Value {
    let failure = import_failure(message, request, trace, importer_source);
    Value::failure(
        values,
        Arc::from(format!("invalid import request {request}")),
        failure,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_compiler_owner_inventory(
        binary: &BinaryLoadArgs,
        module: &ModuleLoadArgs,
        context: &CompileContext,
    ) {
        let BinaryLoadArgs {
            request: _,
            importer_source: _,
            importer_trace: _,
        } = binary;
        let ModuleLoadArgs {
            request: _,
            importer_source: _,
            importer_trace: _,
            extends: _,
            module_path: _,
            prior_defs,
            final_defs,
        } = module;
        let _: &RuntimeValueRoot = prior_defs;
        let _: &RuntimeValueRoot = final_defs;

        let CompileContext {
            values: _,
            importer_source: _,
            compilation_trace: _,
            opaque_origin,
            module_path: _,
            prior_defs,
            final_defs,
            local_module_loader: _,
            local_binary_loader: _,
            diagnostic_emitter: _,
            compilation_execution: _,
        } = context;
        let _: &Option<RuntimeValueRoot> = opaque_origin;
        let _: &RuntimeValueRoot = prior_defs;
        let _: &RuntimeValueRoot = final_defs;
    }

    #[test]
    fn compiler_durable_owner_inventory_is_compile_exhaustive() {
        let _: fn(&BinaryLoadArgs, &ModuleLoadArgs, &CompileContext) =
            assert_compiler_owner_inventory;
    }

    #[test]
    fn module_load_arguments_retain_definition_roots_until_handoff_retires() {
        let values = test_value_factory();
        let public_values = crate::api::Values::from_core_factory(values.clone());
        let domain = crate::api::EffectTokenDomain::new(&public_values);
        let prior_payload = Arc::new(());
        let prior_retained = Arc::downgrade(&prior_payload);
        let final_payload = Arc::new(());
        let final_retained = Arc::downgrade(&final_payload);
        let prior_value = domain.issue(prior_payload);
        let final_value = domain.issue(final_payload);
        let prior_defs =
            RuntimeValueRoot::new(&values, public_values.clone_core(&prior_value).unwrap());
        let final_defs =
            RuntimeValueRoot::new(&values, public_values.clone_core(&final_value).unwrap());
        drop(prior_value);
        drop(final_value);
        let args = ModuleLoadArgs {
            request: RelativeSourcePath::new("child.g").expect("request should be relative"),
            importer_source: None,
            importer_trace: None,
            extends: Arc::from([]),
            module_path: Arc::from(["child".to_owned()]),
            prior_defs,
            final_defs,
        };

        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(prior_retained.upgrade().is_some());
        assert!(final_retained.upgrade().is_some());
        drop(args);
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(prior_retained.upgrade().is_none());
        assert!(final_retained.upgrade().is_none());
    }

    #[test]
    fn local_source_requests_are_child_relative_and_platform_independent() {
        for request in ["child.g", "lib/child.g", "assets/payload.bin"] {
            assert_eq!(validate_local_source_request(request), Ok(()));
        }
        for request in [
            "",
            "/absolute.g",
            "C:/absolute.g",
            "C:\\absolute.g",
            "../parent.g",
            "lib/../parent.g",
            "./current.g",
            ".hidden.g",
            "lib/.hidden/child.g",
            "lib//child.g",
            "lib\\child.g",
        ] {
            assert!(
                validate_local_source_request(request).is_err(),
                "request `{request}` should be rejected"
            );
        }
    }

    #[test]
    fn invalid_local_request_never_reaches_the_loader() {
        let context = CompileContext::default().with_local_module_loader(Arc::new(|args| {
            panic!("invalid request reached loader: {}", args.request.as_str())
        }));
        let eval_context = crate::evaluation::EvalContext::isolated(context.values().clone());
        let error = eval_context
            .evaluate_whnf(&context.import_module(
                "../outside.g",
                None,
                Value::Dict(Dict::new_sync()),
                Value::Dict(Dict::new_sync()),
            ))
            .expect_err("parent-relative request should be a stuck error");
        assert!(error.to_string().contains("must not traverse to a parent"));
        let failure = error.into_permanent_failure();
        let request = failure
            .contexts()
            .iter()
            .find_map(|context| {
                let Value::Dict(context) = context else {
                    return None;
                };
                let Value::Dict(context) = context.get(&*keys::IMPORT)? else {
                    return None;
                };
                let Value::Dict(request) = context.get(&*keys::REQUEST)? else {
                    return None;
                };
                request.get(&*keys::FILE)
            })
            .expect("invalid request should retain its source spelling");
        assert_eq!(request, &Value::binary_from_text("../outside.g"));
    }

    #[test]
    fn binary_import_forwards_hidden_source_provenance() {
        let received = Arc::new(std::sync::Mutex::new(None));
        let captured = received.clone();
        let source = Arc::new(SourceArtifact::new(
            bytes::Bytes::from_static(b"source"),
            crate::source::SourceIdentity::file("samples/hello/hello_text.g"),
        ));
        let trace = Arc::new(CompilationTrace::root(
            crate::diagnostic::CompilationInvocationId::new(1),
            &source,
            Arc::from(["test".to_owned()]),
        ));
        let loader_values = CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        );
        let context = CompileContext::from_module_path_with_values(
            loader_values.clone(),
            std::iter::empty::<&str>(),
        )
        .with_importer_source(source)
        .with_compilation_trace(trace.clone())
        .with_local_binary_loader(Arc::new(move |args| {
            loader_values
                .collect_managed_for_test()
                .expect("an import loader callback must inherit no evaluator mutator");
            *captured
                .lock()
                .expect("loader mutex should not be poisoned") = Some(args);
            Ok(RuntimeValueRoot::new(
                &loader_values,
                Value::binary_from_text("loaded"),
            ))
        }));

        let eval_context = crate::evaluation::EvalContext::isolated(context.values().clone());
        eval_context
            .evaluate_whnf(&context.import_binary("message.txt"))
            .expect("binary import should load");

        let received = received
            .lock()
            .expect("loader mutex should not be poisoned");
        let args = received
            .as_ref()
            .expect("loader should receive one request");

        assert_eq!(
            args.importer_source
                .as_ref()
                .map(|source| source.identity().label()),
            Some("samples/hello/hello_text.g")
        );
        assert_eq!(args.importer_trace.as_deref(), Some(trace.as_ref()));
        assert_eq!(args.request.as_str(), "message.txt");
    }

    #[test]
    fn module_import_qualifies_only_the_relative_child_namespace() {
        let received = Arc::new(std::sync::Mutex::new(None));
        let captured = received.clone();
        let source = SourceArtifact::new(
            bytes::Bytes::from_static(b"source"),
            crate::source::SourceIdentity::file("root.g"),
        );
        let trace = Arc::new(CompilationTrace::root(
            crate::diagnostic::CompilationInvocationId::new(1),
            &source,
            Arc::from(["root".to_owned(), "module".to_owned()]),
        ));
        let context = CompileContext::from_module_path(["root", "module"])
            .with_compilation_trace(trace.clone())
            .with_local_module_loader(Arc::new(move |args| {
                *captured
                    .lock()
                    .expect("loader mutex should not be poisoned") = Some(args);
                Ok(RuntimeValueRoot::new(
                    &test_value_factory(),
                    Value::Dict(Dict::new_sync()),
                ))
            }));

        let eval_context = crate::evaluation::EvalContext::isolated(context.values().clone());
        eval_context
            .evaluate_whnf(&context.import_module(
                "child.g",
                Some("nested.child"),
                Value::Number(1.into()),
                Value::Number(2.into()),
            ))
            .expect("module import should load");

        let received = received
            .lock()
            .expect("loader mutex should not be poisoned");
        let args = received
            .as_ref()
            .expect("loader should receive one request");
        assert_eq!(
            args.module_path.as_ref(),
            &["root", "module", "nested", "child"]
        );
        assert_eq!(args.extends.as_ref(), &["nested", "child"]);
        assert_eq!(args.importer_trace.as_deref(), Some(trace.as_ref()));
    }

    #[test]
    fn compiler_suspension_parks_only_roots() {
        let values = CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        );
        let callback_values = values.clone();
        let context = CompileContext::from_module_path_with_values(values.clone(), ["root"])
            .with_local_module_loader(Arc::new(move |args| {
                assert_eq!(args.prior_defs.runtime_id(), callback_values.runtime_id());
                assert_eq!(args.final_defs.runtime_id(), callback_values.runtime_id());
                callback_values
                    .collect_managed_for_test()
                    .expect("a suspended compiler loader must inherit no managed access");
                Ok(args.prior_defs)
            }));
        let eval_context = crate::evaluation::EvalContext::isolated(values);

        let loaded = eval_context
            .evaluate_whnf(&context.import_module(
                "child.g",
                None,
                Value::Number(1.into()),
                Value::Number(2.into()),
            ))
            .expect("the rooted compiler suspension should resume");

        assert_eq!(loaded, Value::Number(1.into()));
    }

    #[test]
    fn abstract_global_path_qualifies_without_exposing_the_namespace() {
        let context = CompileContext::from_module_path(["root", "module"]);

        assert_eq!(
            context.abstract_global_path("nested.Name"),
            Value::Atom(Atom::from_key(&Key::abstract_global_path([
                "root", "module", "nested", "Name"
            ])))
        );
    }

    #[test]
    fn compile_context_defaults_prior_to_empty_dict() {
        let context = CompileContext::default();

        assert_eq!(context.prior_defs(), Value::Dict(Dict::new_sync()));
    }

    #[test]
    fn unit_value_uses_abstract_global_path_atom() {
        let context = CompileContext::default();
        let unit = context.unit_value();
        let forged = Value::Atom(Atom::from_key(&Key::List(Arc::from([
            Key::binary_from_text("builtin"),
            Key::binary_from_text("unit"),
        ]))));

        assert_eq!(
            unit,
            Value::Atom(Atom::from_key(&Key::abstract_global_path([
                "builtin", "unit"
            ])))
        );
        assert_ne!(unit, forged);
    }
}
