use std::sync::Arc;

use crate::core::Builtin;
use crate::core::{Atom, Dict, Key, LazyValue, Value};
use crate::diagnostic::Severity;
use crate::number::Number;

pub type ModuleLoader = Arc<dyn Fn(ModuleLoadArgs) -> Result<Value, String> + Send + Sync>;
pub type BinaryFileLoader = Arc<dyn Fn(BinaryLoadArgs) -> Result<Value, String> + Send + Sync>;
pub(crate) type CompileDiagnosticEmitter = Arc<dyn Fn(CompileDiagnostic) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompileDiagnostic {
    pub severity: Severity,
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryLoadArgs {
    pub reference: Arc<str>,
    pub importer_source_path: Option<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleLoadArgs {
    pub reference: Arc<str>,
    pub importer_source_path: Option<Arc<str>>,
    pub module_path: Arc<[String]>,
    pub prior_defs: Value,
    pub final_defs: Value,
}

#[derive(Clone)]
pub struct CompileContext {
    // The bootstrap still exposes core Values, but never a semantic expression
    // language. Front ends own their IR and lower it before returning Values.
    source_path: Option<Arc<str>>,
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
            source_path: None,
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
    pub(crate) fn from_source_path(path: impl Into<Arc<str>>) -> Self {
        Self::default().with_source_path(path)
    }

    pub fn from_module_path<I, S>(parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::default().with_module_path(parts)
    }

    pub(crate) fn with_source_path(mut self, path: impl Into<Arc<str>>) -> Self {
        self.source_path = Some(path.into());
        self
    }

    pub fn with_module_path<I, S>(mut self, parts: I) -> Self
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

    pub fn with_local_module_loader(mut self, loader: ModuleLoader) -> Self {
        self.local_module_loader = Some(loader);
        self
    }

    pub fn with_local_binary_loader(mut self, loader: BinaryFileLoader) -> Self {
        self.local_binary_loader = Some(loader);
        self
    }

    pub(crate) fn with_diagnostic_emitter(mut self, emitter: CompileDiagnosticEmitter) -> Self {
        self.diagnostic_emitter = Some(emitter);
        self
    }

    pub fn module_path(&self) -> &[String] {
        &self.module_path
    }

    pub fn prior_defs(&self) -> &Value {
        &self.prior_defs
    }

    pub fn final_defs(&self) -> &Value {
        &self.final_defs
    }

    pub fn abstract_global_path(&self, target: &str) -> Arc<[String]> {
        // TODO: support expression-indexed paths, e.g. foo.bar.[42].baz
        let mut parts = self.module_path.iter().cloned().collect::<Vec<_>>();
        parts.extend(target.split('.').map(ToOwned::to_owned));
        Arc::from(parts.into_boxed_slice())
    }

    pub(crate) fn emit_diagnostic(&self, diagnostic: CompileDiagnostic) {
        if let Some(emitter) = &self.diagnostic_emitter {
            emitter(diagnostic);
        }
    }

    // TODO: replace the typed bootstrap diagnostic with an open Value payload.
    // The assembler-owned emitter will continue to attach source provenance.

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

    pub fn abstract_global_path_value(&self, path: &[String]) -> Value {
        self.value_atom(Atom::from_key(&Key::AbstractGlobalPath(Arc::from(
            path.to_vec(),
        ))))
    }

    pub fn unit_value(&self) -> Value {
        self.value_atom(Atom::from_key(&Key::abstract_global_path([
            "builtin", "unit",
        ])))
    }

    pub fn local_module_load_args(
        &self,
        reference: &str,
        module_path: Arc<[String]>,
        prior_defs: Value,
        final_defs: Value,
    ) -> ModuleLoadArgs {
        ModuleLoadArgs {
            reference: Arc::from(reference),
            importer_source_path: self.source_path.clone(),
            module_path,
            prior_defs,
            final_defs,
        }
    }

    pub fn value_load_local_module(&self, args: ModuleLoadArgs) -> Value {
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

    pub fn local_binary_load_args(&self, reference: &str) -> BinaryLoadArgs {
        BinaryLoadArgs {
            reference: Arc::from(reference),
            importer_source_path: self.source_path.clone(),
        }
    }

    pub fn value_load_local_binary(&self, args: BinaryLoadArgs) -> Value {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_path_is_only_forwarded_to_load_requests() {
        let context = CompileContext::from_source_path("samples/assembly/hello_text.g");
        let args = context.local_binary_load_args("message.txt");

        assert_eq!(
            args.importer_source_path.as_deref(),
            Some("samples/assembly/hello_text.g")
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
            context.abstract_global_path_value(&["builtin".to_owned(), "unit".to_owned()])
        );
        assert_ne!(unit, forged);
    }
}
