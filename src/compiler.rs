use std::sync::Arc;

use crate::core::Builtin;
use crate::core::{Atom, Dict, Key, LazyValue, Value, keys};
use crate::diagnostic::Severity;
use crate::number::Number;

pub(crate) type ModuleLoader = Arc<dyn Fn(ModuleLoadArgs) -> Result<Value, String> + Send + Sync>;
pub(crate) type BinaryFileLoader =
    Arc<dyn Fn(BinaryLoadArgs) -> Result<Value, String> + Send + Sync>;
pub(crate) type CompileDiagnosticEmitter = Arc<dyn Fn(Severity, Value) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BinaryLoadArgs {
    pub(crate) reference: Arc<str>,
    pub(crate) importer_source_path: Option<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleLoadArgs {
    pub(crate) reference: Arc<str>,
    pub(crate) importer_source_path: Option<Arc<str>>,
    pub(crate) module_path: Arc<[String]>,
    pub(crate) prior_defs: Value,
    pub(crate) final_defs: Value,
}

#[derive(Clone)]
pub struct CompileContext {
    // The bootstrap still exposes core Values, but never a semantic expression
    // language. Front ends own their IR and lower it before returning Values.
    importer_source_path: Option<Arc<str>>,
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
            importer_source_path: None,
            module_path: Arc::from([]),
            prior_defs: Value::Dict(Dict::new_sync()), // empty prior dictionary
            final_defs: Value::Lazy(LazyValue::pending("final definitions")),
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

    pub(crate) fn with_importer_source_path(mut self, path: impl Into<Arc<str>>) -> Self {
        self.importer_source_path = Some(path.into());
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
        reference: &str,
        relative_namespace: Option<&str>,
        prior_defs: Value,
        final_defs: Value,
    ) -> Value {
        let args = ModuleLoadArgs {
            reference: Arc::from(reference),
            importer_source_path: self.importer_source_path.clone(),
            module_path: self.qualify_module_path(relative_namespace),
            prior_defs,
            final_defs,
        };
        let label: Arc<str> = Arc::from(format!("import {}", args.reference));
        let loader = self.local_module_loader.clone();

        Value::deferred(label, move || {
            let Some(loader) = &loader else {
                return Err(format!(
                    "local import `{}` cannot be loaded without a module loader",
                    args.reference
                ));
            };
            loader(args.clone())
        })
    }

    pub fn import_binary(&self, reference: &str) -> Value {
        let args = BinaryLoadArgs {
            reference: Arc::from(reference),
            importer_source_path: self.importer_source_path.clone(),
        };
        let label: Arc<str> = Arc::from(format!("import binary {}", args.reference));
        let loader = self.local_binary_loader.clone();

        Value::deferred(label, move || {
            let Some(loader) = &loader else {
                return Err(format!(
                    "binary import `{}` cannot be loaded without a binary loader",
                    args.reference
                ));
            };
            loader(args.clone())
        })
    }

    fn qualify_module_path(&self, relative_namespace: Option<&str>) -> Arc<[String]> {
        let mut parts = self.module_path.to_vec();
        if let Some(relative_namespace) = relative_namespace {
            parts.extend(relative_namespace.split('.').map(ToOwned::to_owned));
        }
        Arc::from(parts.into_boxed_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_import_forwards_hidden_source_provenance() {
        let received = Arc::new(std::sync::Mutex::new(None));
        let captured = received.clone();
        let context = CompileContext::default()
            .with_importer_source_path("samples/assembly/hello_text.g")
            .with_local_binary_loader(Arc::new(move |args| {
                *captured
                    .lock()
                    .expect("loader mutex should not be poisoned") = Some(args);
                Ok(Value::binary_from_text("loaded"))
            }));

        crate::eval::eval_value(&context.import_binary("message.txt"))
            .expect("binary import should load");

        let received = received
            .lock()
            .expect("loader mutex should not be poisoned");
        let args = received
            .as_ref()
            .expect("loader should receive one request");

        assert_eq!(
            args.importer_source_path.as_deref(),
            Some("samples/assembly/hello_text.g")
        );
        assert_eq!(args.reference.as_ref(), "message.txt");
    }

    #[test]
    fn module_import_qualifies_only_the_relative_child_namespace() {
        let received = Arc::new(std::sync::Mutex::new(None));
        let captured = received.clone();
        let context = CompileContext::from_module_path(["root", "module"])
            .with_local_module_loader(Arc::new(move |args| {
                *captured
                    .lock()
                    .expect("loader mutex should not be poisoned") = Some(args);
                Ok(Value::Dict(Dict::new_sync()))
            }));

        crate::eval::eval_value(&context.import_module(
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
