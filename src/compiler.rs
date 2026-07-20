use std::sync::Arc;

use crate::core::Builtin;
use crate::core::{Atom, Dict, Key, LazyValue, Value, keys};
use crate::diagnostic::{CompilationTrace, Severity};
use crate::number::Number;
use crate::source::{RelativeSourcePath, SourceArtifact};

pub(crate) type ModuleLoader = Arc<dyn Fn(ModuleLoadArgs) -> Result<Value, String> + Send + Sync>;
pub(crate) type BinaryFileLoader =
    Arc<dyn Fn(BinaryLoadArgs) -> Result<Value, String> + Send + Sync>;
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
}

#[derive(Debug, Clone)]
pub(crate) struct ModuleLoadArgs {
    pub(crate) request: RelativeSourcePath,
    pub(crate) importer_source: Option<Arc<SourceArtifact>>,
    pub(crate) importer_trace: Option<Arc<CompilationTrace>>,
    pub(crate) extends: Arc<[String]>,
    pub(crate) module_path: Arc<[String]>,
    pub(crate) prior_defs: Value,
    pub(crate) final_defs: Value,
}

#[derive(Clone)]
pub struct CompileContext {
    // The bootstrap still exposes core Values, but never a semantic expression
    // language. Front ends own their IR and lower it before returning Values.
    importer_source: Option<Arc<SourceArtifact>>,
    compilation_trace: Option<Arc<CompilationTrace>>,
    module_path: Arc<[String]>,
    prior_defs: Value, // prior dictionary, can be observed at compile-time
    final_defs: Value, // future dictionary, cannot observe at compile-time
    local_module_loader: Option<ModuleLoader>,
    local_binary_loader: Option<BinaryFileLoader>,
    diagnostic_emitter: Option<CompileDiagnosticEmitter>,
}

impl Default for CompileContext {
    fn default() -> Self {
        Self {
            importer_source: None,
            compilation_trace: None,
            module_path: Arc::from([]),
            prior_defs: Value::Dict(Dict::new_sync()), // empty prior dictionary
            final_defs: Value::Lazy(LazyValue::promised("final definitions")),
            local_module_loader: None,
            local_binary_loader: None,
            diagnostic_emitter: None,
        }
    }
}

impl CompileContext {
    pub(crate) fn from_module_path<I, S>(parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::default().with_module_path(parts)
    }

    pub(crate) fn with_importer_source(mut self, source: Arc<SourceArtifact>) -> Self {
        self.importer_source = Some(source);
        self
    }

    pub(crate) fn with_compilation_trace(mut self, trace: Arc<CompilationTrace>) -> Self {
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

    pub fn with_prior_defs(mut self, prior: Value) -> Self {
        self.prior_defs = prior;
        self
    }

    pub fn with_final_defs(mut self, final_defs: Value) -> Self {
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

    pub fn prior_defs(&self) -> &Value {
        &self.prior_defs
    }

    pub fn final_defs(&self) -> &Value {
        &self.final_defs
    }

    /// Returns the abstract global-path value for a path relative to the
    /// current module without revealing its absolute namespace.
    pub fn abstract_global_path(&self, target: &str) -> Value {
        // TODO: support expression-indexed paths, e.g. foo.bar.[42].baz
        let mut parts = self.module_path.iter().cloned().collect::<Vec<_>>();
        parts.extend(target.split('.').map(ToOwned::to_owned));
        self.value_atom(Atom::from_key(&Key::AbstractGlobalPath(Arc::from(
            parts.into_boxed_slice(),
        ))))
    }

    pub(crate) fn emit_diagnostic(&self, severity: Severity, message: Value) {
        if let Some(emitter) = &self.diagnostic_emitter {
            emitter(severity, message);
        }
    }

    // TODO: eliminate direct use of Builtin in this API. The front-end
    // knows about builtins, but will access them as atoms, not as the Builtin enum.

    pub fn value_number(&self, number: Number) -> Value {
        Value::Number(number)
    }

    pub fn value_binary(&self, text: &str) -> Value {
        Value::binary_from_text(text)
    }

    pub fn value_atom(&self, atom: Atom) -> Value {
        Value::Atom(atom)
    }

    pub fn value_dict(&self, dict: Dict) -> Value {
        Value::Dict(dict)
    }

    pub fn value_builtin(&self, builtin: Builtin) -> Value {
        Value::Builtin(builtin)
    }

    pub fn empty_dict_value(&self) -> Value {
        self.value_dict(Dict::new_sync())
    }

    pub fn unit_value(&self) -> Value {
        (*keys::UNIT_VALUE).clone()
    }

    /// Requests a module import in the current or a relative child namespace.
    /// Source resolution and absolute namespace qualification remain hidden.
    pub fn import_module(
        &self,
        request: &str,
        relative_namespace: Option<&str>,
        prior_defs: Value,
        final_defs: Value,
    ) -> Value {
        let request = match RelativeSourcePath::new(request) {
            Ok(request) => request,
            Err(error) => return Value::error(error.to_string()),
        };
        let (module_path, extends) = self.qualify_module_path(relative_namespace);
        let args = ModuleLoadArgs {
            request,
            importer_source: self.importer_source.clone(),
            importer_trace: self.compilation_trace.clone(),
            extends,
            module_path,
            prior_defs,
            final_defs,
        };
        let label: Arc<str> = Arc::from(format!("import {}", args.request.as_str()));
        let loader = self.local_module_loader.clone();

        Value::deferred(label, move |_| {
            let Some(loader) = &loader else {
                return Err(format!(
                    "local import `{}` cannot be loaded without a module loader",
                    args.request.as_str()
                ));
            };
            loader(args.clone())
        })
    }

    pub fn import_binary(&self, request: &str) -> Value {
        let request = match RelativeSourcePath::new(request) {
            Ok(request) => request,
            Err(error) => return Value::error(error.to_string()),
        };
        let args = BinaryLoadArgs {
            request,
            importer_source: self.importer_source.clone(),
        };
        let label: Arc<str> = Arc::from(format!("import binary {}", args.request.as_str()));
        let loader = self.local_binary_loader.clone();

        Value::deferred(label, move |_| {
            let Some(loader) = &loader else {
                return Err(format!(
                    "binary import `{}` cannot be loaded without a binary loader",
                    args.request.as_str()
                ));
            };
            loader(args.clone())
        })
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let error = crate::eval::eval_value(
            &crate::evaluation::EvalContext::standalone(),
            &context.import_module(
                "../outside.g",
                None,
                Value::Dict(Dict::new_sync()),
                Value::Dict(Dict::new_sync()),
            ),
        )
        .expect_err("parent-relative request should be a stuck error");
        assert!(error.to_string().contains("must not traverse to a parent"));
    }

    #[test]
    fn binary_import_forwards_hidden_source_provenance() {
        let received = Arc::new(std::sync::Mutex::new(None));
        let captured = received.clone();
        let source = Arc::new(SourceArtifact::new(
            bytes::Bytes::from_static(b"source"),
            crate::source::SourceIdentity::file("samples/assembly/hello_text.g"),
        ));
        let context = CompileContext::default()
            .with_importer_source(source)
            .with_local_binary_loader(Arc::new(move |args| {
                *captured
                    .lock()
                    .expect("loader mutex should not be poisoned") = Some(args);
                Ok(Value::binary_from_text("loaded"))
            }));

        crate::eval::eval_value(
            &crate::evaluation::EvalContext::standalone(),
            &context.import_binary("message.txt"),
        )
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
            Some("samples/assembly/hello_text.g")
        );
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
                Ok(Value::Dict(Dict::new_sync()))
            }));

        crate::eval::eval_value(
            &crate::evaluation::EvalContext::standalone(),
            &context.import_module(
                "child.g",
                Some("nested.child"),
                Value::Number(1.into()),
                Value::Number(2.into()),
            ),
        )
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

        assert_eq!(context.prior_defs(), &Value::Dict(Dict::new_sync()));
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
            context.value_atom(Atom::from_key(&Key::abstract_global_path([
                "builtin", "unit"
            ])))
        );
        assert_ne!(unit, forged);
    }
}
