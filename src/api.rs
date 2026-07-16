//! Stable embedding-oriented facade for assembling modules and observing values.
//!
//! This module owns host capabilities and orchestration. Front-end syntax,
//! core values, evaluator topology, and interaction-net scheduling remain
//! implementation details behind the facade.

use std::collections::VecDeque;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::compiler::{
    BinaryFileLoader, BinaryLoadArgs, CompileContext, ModuleLoadArgs, ModuleLoader,
};
use crate::core::Value as CoreValue;
use crate::core::{Builtin, Dict, Key, List};
use crate::diagnostic::Severity;
use crate::eval;
use crate::g_syntax::{Diagnostic as SyntaxDiagnostic, SourceFile, lower_to_core_with_context};

pub const DEFAULT_DIAGNOSTIC_CAPACITY: usize = 1_000;

/// An assembly-time value whose concrete evaluator representation is private.
#[derive(Clone, PartialEq, Eq)]
pub struct Value(CoreValue);

impl Value {
    pub fn binary(bytes: impl Into<Bytes>) -> Self {
        Self(CoreValue::Binary(bytes.into()))
    }

    pub fn text(text: impl AsRef<str>) -> Self {
        Self(CoreValue::binary_from_text(text.as_ref()))
    }

    pub fn kind(&self) -> ValueKind {
        match &self.0 {
            CoreValue::Atom(_) => ValueKind::Atom,
            CoreValue::Number(_) => ValueKind::Number,
            CoreValue::Binary(_) => ValueKind::Binary,
            CoreValue::List(_) => ValueKind::List,
            CoreValue::Dict(_) => ValueKind::Dict,
            CoreValue::Builtin(_) | CoreValue::PartialBuiltin(_) | CoreValue::Function(_) => {
                ValueKind::Function
            }
            CoreValue::Net(_) => ValueKind::Net,
            CoreValue::Lazy(_) => ValueKind::Lazy,
        }
    }

    pub fn as_binary(&self) -> Option<&[u8]> {
        match &self.0 {
            CoreValue::Binary(bytes) => Some(bytes.as_ref()),
            _ => None,
        }
    }

    pub(crate) fn from_core(value: CoreValue) -> Self {
        Self(value)
    }

    pub(crate) fn as_core(&self) -> &CoreValue {
        &self.0
    }

    pub(crate) fn into_core(self) -> CoreValue {
        self.0
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Value")
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValueKind {
    Atom,
    Number,
    Binary,
    List,
    Dict,
    Function,
    Net,
    Lazy,
}

/// One source or runtime diagnostic retained by an [`Assembler`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    source: Option<Arc<str>>,
    severity: Severity,
    line: Option<usize>,
    message: Arc<str>,
}

impl Diagnostic {
    pub fn new(severity: Severity, message: impl Into<Arc<str>>) -> Self {
        Self {
            source: None,
            severity,
            line: None,
            message: message.into(),
        }
    }

    pub fn with_source_location(mut self, source: impl Into<Arc<str>>, line: usize) -> Self {
        self.source = Some(source.into());
        self.line = Some(line);
        self
    }

    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    pub fn severity(&self) -> Severity {
        self.severity
    }

    pub fn line(&self) -> Option<usize> {
        self.line
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn from_syntax(source: &str, diagnostic: &SyntaxDiagnostic) -> Self {
        Self::new(diagnostic.severity, Arc::from(diagnostic.message.as_str()))
            .with_source_location(source, diagnostic.line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticSnapshot {
    entries: Vec<Diagnostic>,
    dropped: u64,
}

impl DiagnosticSnapshot {
    pub fn entries(&self) -> &[Diagnostic] {
        &self.entries
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[derive(Debug)]
struct DiagnosticHistory {
    capacity: usize,
    entries: VecDeque<Diagnostic>,
    dropped: u64,
}

/// Destination for diagnostics emitted by assembly and future reflection work.
/// Implementations may be called concurrently.
pub trait DiagnosticSink: Send + Sync {
    fn emit(&self, diagnostic: Diagnostic);

    /// Atomically reads and consumes any retained diagnostics.
    fn read(&self) -> Option<DiagnosticSnapshot> {
        None
    }
}

impl<T: DiagnosticSink + ?Sized> DiagnosticSink for Arc<T> {
    fn emit(&self, diagnostic: Diagnostic) {
        (**self).emit(diagnostic);
    }

    fn read(&self) -> Option<DiagnosticSnapshot> {
        (**self).read()
    }
}

/// Bounded, oldest-first diagnostic history used by [`Assembler::default`].
#[derive(Debug)]
pub struct DiagnosticBuffer {
    history: Mutex<DiagnosticHistory>,
}

impl DiagnosticBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            history: Mutex::new(DiagnosticHistory::new(capacity)),
        }
    }

    /// Atomically reads and removes all retained diagnostics and resets the
    /// dropped-entry count.
    pub fn read(&self) -> DiagnosticSnapshot {
        self.history
            .lock()
            .expect("diagnostic history mutex should not be poisoned")
            .read()
    }
}

impl DiagnosticSink for DiagnosticBuffer {
    fn emit(&self, diagnostic: Diagnostic) {
        self.history
            .lock()
            .expect("diagnostic history mutex should not be poisoned")
            .push(diagnostic);
    }

    fn read(&self) -> Option<DiagnosticSnapshot> {
        Some(DiagnosticBuffer::read(self))
    }
}

struct DiagnosticCallback<F>(F);

impl<F> DiagnosticSink for DiagnosticCallback<F>
where
    F: Fn(Diagnostic) + Send + Sync,
{
    fn emit(&self, diagnostic: Diagnostic) {
        (self.0)(diagnostic);
    }
}

impl DiagnosticHistory {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::with_capacity(capacity),
            dropped: 0,
        }
    }

    fn push(&mut self, diagnostic: Diagnostic) {
        if self.capacity == 0 {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.entries.push_back(diagnostic);
    }

    fn read(&mut self) -> DiagnosticSnapshot {
        DiagnosticSnapshot {
            entries: self.entries.drain(..).collect(),
            dropped: std::mem::take(&mut self.dropped),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError {
    message: Arc<str>,
}

impl HostError {
    pub fn new(message: impl Into<Arc<str>>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HostError {}

/// External capabilities used by module and binary loading.
pub trait Host: Send + Sync {
    fn read(&self, path: &Path) -> Result<Bytes, HostError>;

    fn path_exists(&self, path: &Path) -> bool;

    fn environment_variable(&self, name: &str) -> Option<OsString>;
}

impl<T: Host + ?Sized> Host for Arc<T> {
    fn read(&self, path: &Path) -> Result<Bytes, HostError> {
        (**self).read(path)
    }

    fn path_exists(&self, path: &Path) -> bool {
        (**self).path_exists(path)
    }

    fn environment_variable(&self, name: &str) -> Option<OsString> {
        (**self).environment_variable(name)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemHost;

impl Host for SystemHost {
    fn read(&self, path: &Path) -> Result<Bytes, HostError> {
        std::fs::read(path).map(Bytes::from).map_err(|error| {
            HostError::new(format!("could not read `{}`: {error}", path.display()))
        })
    }

    fn path_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn environment_variable(&self, name: &str) -> Option<OsString> {
        env::var_os(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleInput {
    File(PathBuf),
    Script { extension: String, body: String },
}

impl ModuleInput {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }

    pub fn script(extension: impl Into<String>, body: impl Into<String>) -> Self {
        Self::Script {
            extension: extension.into(),
            body: body.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BuiltModule {
    value: Value,
    diagnostics: Vec<Diagnostic>,
}

impl BuiltModule {
    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: Arc<str>,
    diagnostics: Vec<Diagnostic>,
}

impl Error {
    fn new(message: impl Into<Arc<str>>) -> Self {
        Self {
            message: message.into(),
            diagnostics: Vec::new(),
        }
    }

    fn with_diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Assembler {
    host: Arc<dyn Host>,
    diagnostic_sink: Arc<dyn DiagnosticSink>,
}

impl Default for Assembler {
    fn default() -> Self {
        Self {
            host: Arc::new(SystemHost),
            diagnostic_sink: Arc::new(DiagnosticBuffer::new(DEFAULT_DIAGNOSTIC_CAPACITY)),
        }
    }
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_host(mut self, host: impl Host + 'static) -> Self {
        self.host = Arc::new(host);
        self
    }

    pub fn with_diagnostic_sink(mut self, sink: impl DiagnosticSink + 'static) -> Self {
        self.diagnostic_sink = Arc::new(sink);
        self
    }

    pub fn with_diagnostic_buffer(self, capacity: usize) -> Self {
        self.with_diagnostic_sink(DiagnosticBuffer::new(capacity))
    }

    pub fn with_diagnostic_callback<F>(self, callback: F) -> Self
    where
        F: Fn(Diagnostic) + Send + Sync + 'static,
    {
        self.with_diagnostic_sink(DiagnosticCallback(callback))
    }

    pub fn module(&self) -> ModuleBuilder<'_> {
        ModuleBuilder {
            assembler: self,
            inputs: Vec::new(),
            arguments: Vec::new(),
            environment: None,
        }
    }

    /// Atomically reads and consumes retained diagnostics when supported by
    /// the configured sink.
    pub fn read_diagnostics(&self) -> Option<DiagnosticSnapshot> {
        self.diagnostic_sink.read()
    }

    pub(crate) fn record_diagnostic(&self, diagnostic: Diagnostic) {
        self.diagnostic_sink.emit(diagnostic);
    }

    pub fn force(&self, value: &Value) -> Result<Value, Error> {
        eval::eval_value(value.as_core())
            .map(Value::from_core)
            .map_err(|error| Error::new(error.to_string()))
    }

    // TODO: add multi-argument application here once evaluator application is
    // exposed through one syntax-independent crate-internal operation.

    // TODO: add reflection snapshots and event subscriptions here. Reflection
    // producers should feed the same bounded history rather than print.

    // TODO: add the checked effect-style bind/copy/data/wire builder here
    // without exposing the generic core specialization or runtime scheduler.

    pub fn get(&self, root: &Value, path: &str) -> Result<Value, Error> {
        self.core_value_at_path(root.as_core(), path)
            .map(Value::from_core)
    }

    pub fn to_binary(&self, value: &Value) -> Result<Bytes, Error> {
        self.core_value_bytes(value.as_core(), "value")
    }

    pub fn binary_at(&self, root: &Value, path: &str) -> Result<Bytes, Error> {
        let value = self.core_value_at_path(root.as_core(), path)?;
        self.core_value_bytes(&value, path)
    }

    fn build_module(
        &self,
        inputs: Vec<ModuleInput>,
        arguments: Vec<String>,
        environment: Option<Value>,
    ) -> Result<BuiltModule, Error> {
        let session = Arc::new(Mutex::new(Vec::new()));
        let result = self.build_module_inner(inputs, arguments, environment, session.clone());
        let diagnostics = session
            .lock()
            .expect("build diagnostic mutex should not be poisoned")
            .clone();

        match result {
            Ok(value) => Ok(BuiltModule {
                value: Value::from_core(value),
                diagnostics,
            }),
            Err(error) => Err(error.with_diagnostics(diagnostics)),
        }
    }

    fn build_module_inner(
        &self,
        inputs: Vec<ModuleInput>,
        arguments: Vec<String>,
        environment: Option<Value>,
        session: Arc<Mutex<Vec<Diagnostic>>>,
    ) -> Result<CoreValue, Error> {
        let module_loader = self.module_loader(session.clone());
        let binary_loader = self.binary_loader();
        let environment = match environment {
            Some(environment) => environment.into_core(),
            None => self.load_configuration(
                module_loader.clone(),
                binary_loader.clone(),
                session.clone(),
            )?,
        };
        let assembly_context = CompileContext::from_module_path(["assembly"])
            .with_local_module_loader(module_loader.clone())
            .with_local_binary_loader(binary_loader.clone());
        let final_defs = assembly_context.final_defs.clone();
        let mut definitions =
            self.initial_assembly_definitions(&assembly_context, &arguments, environment);
        let mut had_errors = false;

        for input in inputs.iter().rev() {
            let (source, context, label) = self.prepare_input(
                input,
                definitions.clone(),
                final_defs.clone(),
                module_loader.clone(),
                binary_loader.clone(),
            )?;
            let parsed = source.parse_with_context(&context);
            let lowered = lower_to_core_with_context(parsed, &context);
            had_errors |= self.record_syntax_diagnostics(&label, &lowered.diagnostics, &session);
            definitions = lowered.definitions;
        }

        if had_errors {
            return Err(Error::new("assembly failed to compile"));
        }

        let module_value = self.seal_module(&assembly_context, &definitions);
        eval::eval_value(&module_value).map_err(|error| Error::new(error.to_string()))
    }

    fn prepare_input(
        &self,
        input: &ModuleInput,
        prior_defs: CoreValue,
        final_defs: CoreValue,
        module_loader: ModuleLoader,
        binary_loader: BinaryFileLoader,
    ) -> Result<(SourceFile, CompileContext, String), Error> {
        match input {
            ModuleInput::File(path) => {
                let source = self.read_source(path)?;
                let label = source.path.clone();
                let context = CompileContext::from_source_path(&label)
                    .with_module_path(["assembly"])
                    .with_prior_defs(prior_defs)
                    .with_final_defs(final_defs)
                    .with_local_module_loader(module_loader)
                    .with_local_binary_loader(binary_loader)
                    .with_source_binary(source.text.as_bytes());
                Ok((source, context, label))
            }
            ModuleInput::Script { extension, body } => {
                let label = format!("<script.{extension}>");
                let source = SourceFile::new(&label, body);
                let context = CompileContext::from_module_path(["assembly"])
                    .with_prior_defs(prior_defs)
                    .with_final_defs(final_defs)
                    .with_local_module_loader(module_loader)
                    .with_local_binary_loader(binary_loader)
                    .with_source_binary(source.text.as_bytes());
                Ok((source, context, label))
            }
        }
    }

    fn initial_assembly_definitions(
        &self,
        context: &CompileContext,
        arguments: &[String],
        environment: CoreValue,
    ) -> CoreValue {
        let arguments = CoreValue::List(List::from_values(
            arguments
                .iter()
                .map(|argument| context.value_binary(argument))
                .collect(),
        ));
        let assembly =
            CoreValue::Dict(Dict::new_sync().insert(Key::atom_from_text("args"), arguments));
        CoreValue::Dict(
            Dict::new_sync()
                .insert(Key::atom_from_text("asm"), assembly)
                .insert(Key::atom_from_text("env"), environment),
        )
    }

    fn load_configuration(
        &self,
        module_loader: ModuleLoader,
        binary_loader: BinaryFileLoader,
        session: Arc<Mutex<Vec<Diagnostic>>>,
    ) -> Result<CoreValue, Error> {
        let context = CompileContext::from_module_path(["configuration"])
            .with_local_module_loader(module_loader.clone())
            .with_local_binary_loader(binary_loader.clone());
        let final_defs = context.final_defs.clone();
        let mut definitions = self.initial_configuration_definitions(&context);
        let mut had_errors = false;

        for path in self.configuration_paths().into_iter().rev() {
            let source = self.read_source(&path)?;
            let label = source.path.clone();
            let source_context = CompileContext::from_source_path(&label)
                .with_module_path(["configuration"])
                .with_prior_defs(definitions.clone())
                .with_final_defs(final_defs.clone())
                .with_local_module_loader(module_loader.clone())
                .with_local_binary_loader(binary_loader.clone())
                .with_source_binary(source.text.as_bytes());
            let parsed = source.parse_with_context(&source_context);
            let lowered = lower_to_core_with_context(parsed, &source_context);
            had_errors |= self.record_syntax_diagnostics(&label, &lowered.diagnostics, &session);
            definitions = lowered.definitions;
        }

        if had_errors {
            return Err(Error::new("configuration failed to compile"));
        }

        let module_value = self.seal_module(&context, &definitions);
        let root = eval::eval_value(&module_value)
            .map_err(|error| Error::new(format!("configuration evaluation failed: {error}")))?;

        match self.core_value_at_path(&root, "conf.env") {
            Ok(environment) if !is_undefined_value(&environment) => eval::eval_value(&environment)
                .map_err(|error| {
                    Error::new(format!(
                        "configuration `conf.env` failed to evaluate: {error}"
                    ))
                }),
            Ok(_) | Err(_) => Ok(self.empty_environment_object(&context)),
        }
    }

    fn initial_configuration_definitions(&self, context: &CompileContext) -> CoreValue {
        CoreValue::Dict(Dict::new_sync().insert(
            Key::atom_from_text("env"),
            self.empty_environment_object(context),
        ))
    }

    fn empty_environment_object(&self, context: &CompileContext) -> CoreValue {
        let spec = Dict::new_sync()
            .insert(
                Key::atom_from_text("name"),
                context.abstract_global_path_value(context.abstract_global_path("env").as_ref()),
            )
            .insert(Key::atom_from_text("deps"), CoreValue::List(List::empty()))
            .insert(
                Key::atom_from_text("defs"),
                CoreValue::Builtin(Builtin::ObjectDefaultDefs),
            );

        CoreValue::builtin_call(Builtin::ObjectInstance, vec![CoreValue::Dict(spec)])
    }

    fn configuration_paths(&self) -> Vec<PathBuf> {
        if let Some(paths) = self
            .configuration_paths_from_environment("GLAS_CONF")
            .or_else(|| self.configuration_paths_from_environment("GLAM_CONF"))
        {
            return paths;
        }

        if let Some(path) = self
            .default_user_configuration_path()
            .filter(|path| self.host.path_exists(path))
        {
            return vec![path];
        }

        let workspace_default = PathBuf::from("samples/config/dev.g");
        if self.host.path_exists(&workspace_default) {
            return vec![workspace_default];
        }

        Vec::new()
    }

    fn configuration_paths_from_environment(&self, name: &str) -> Option<Vec<PathBuf>> {
        self.host.environment_variable(name).map(|value| {
            env::split_paths(&value)
                .filter(|path| !path.as_os_str().is_empty())
                .collect()
        })
    }

    fn default_user_configuration_path(&self) -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            self.host
                .environment_variable("APPDATA")
                .map(PathBuf::from)
                .map(|path| path.join("glas").join("conf.g"))
        }

        #[cfg(target_os = "macos")]
        {
            self.home_dir().map(|path| {
                path.join("Library")
                    .join("Application Support")
                    .join("glas")
                    .join("conf.g")
            })
        }

        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.host
                .environment_variable("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| self.home_dir().map(|home| home.join(".config")))
                .map(|path| path.join("glas").join("conf.g"))
        }

        #[cfg(not(any(unix, target_os = "windows")))]
        {
            None
        }
    }

    fn home_dir(&self) -> Option<PathBuf> {
        self.host
            .environment_variable("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
    }

    fn module_loader(&self, session: Arc<Mutex<Vec<Diagnostic>>>) -> ModuleLoader {
        let assembler = self.clone();
        Arc::new(move |args| assembler.load_local_module(args, session.clone()))
    }

    fn binary_loader(&self) -> BinaryFileLoader {
        let assembler = self.clone();
        Arc::new(move |args| assembler.load_local_binary(args))
    }

    fn load_local_module(
        &self,
        args: ModuleLoadArgs,
        session: Arc<Mutex<Vec<Diagnostic>>>,
    ) -> Result<CoreValue, String> {
        let path = resolve_local_import_path(
            args.importer_source_path.as_deref(),
            &args.reference,
            "local import",
        )?;
        let label = path.display().to_string();
        let source = self.read_source(&path).map_err(|error| error.to_string())?;
        let module_loader = self.module_loader(session.clone());
        let binary_loader = self.binary_loader();
        let context = CompileContext::from_source_path(&label)
            .with_module_path(args.module_path.iter().cloned())
            .with_prior_defs(args.prior_defs)
            .with_final_defs(args.final_defs)
            .with_local_module_loader(module_loader)
            .with_local_binary_loader(binary_loader)
            .with_source_binary(source.text.as_bytes());
        let parsed = source.parse_with_context(&context);
        let lowered = lower_to_core_with_context(parsed, &context);
        let had_errors = self.record_syntax_diagnostics(&label, &lowered.diagnostics, &session);

        if had_errors {
            Err(format!("local import `{label}` failed to compile"))
        } else {
            Ok(lowered.definitions)
        }
    }

    fn load_local_binary(&self, args: BinaryLoadArgs) -> Result<CoreValue, String> {
        let path = resolve_local_import_path(
            args.importer_source_path.as_deref(),
            &args.reference,
            "binary import",
        )?;
        self.host
            .read(&path)
            .map(CoreValue::Binary)
            .map_err(|error| error.to_string())
    }

    fn read_source(&self, path: &Path) -> Result<SourceFile, Error> {
        let bytes = self
            .host
            .read(path)
            .map_err(|error| Error::new(error.to_string()))?;
        let text = String::from_utf8(bytes.to_vec()).map_err(|error| {
            Error::new(format!(
                "could not read `{}` as UTF-8 source: {error}",
                path.display()
            ))
        })?;
        Ok(SourceFile::new(path.display().to_string(), text))
    }

    fn seal_module(&self, context: &CompileContext, definitions: &CoreValue) -> CoreValue {
        let CoreValue::Lazy(final_defs) = &context.final_defs else {
            panic!("CompileContext.final_defs must be a pending lazy value");
        };
        final_defs
            .set(definitions.clone())
            .expect("CompileContext.final_defs future must be unassigned");
        definitions.clone()
    }

    fn core_value_at_path(&self, root: &CoreValue, path: &str) -> Result<CoreValue, Error> {
        let mut current = root.clone();

        for part in path.split('.') {
            let current_value =
                eval::eval_value(&current).map_err(|error| Error::new(error.to_string()))?;
            let CoreValue::Dict(dict) = current_value else {
                return Err(Error::new(format!("module did not define `{path}`")));
            };
            current = dict
                .get(&Key::atom_from_text(part))
                .cloned()
                .ok_or_else(|| Error::new(format!("module did not define `{path}`")))?;
        }

        Ok(current)
    }

    fn core_value_bytes(&self, value: &CoreValue, label: &str) -> Result<Bytes, Error> {
        match value {
            CoreValue::Binary(bytes) => Ok(bytes.clone()),
            CoreValue::List(list) => eval::list_output_bytes(list)
                .map(Bytes::from)
                .map_err(|error| Error::new(format!("`{label}` {error}"))),
            CoreValue::Lazy(_) => {
                let value =
                    eval::eval_value(value).map_err(|error| Error::new(error.to_string()))?;
                self.core_value_bytes(&value, label)
            }
            CoreValue::Atom(_)
            | CoreValue::Dict(_)
            | CoreValue::Number(_)
            | CoreValue::Function(_)
            | CoreValue::Net(_)
            | CoreValue::Builtin(_)
            | CoreValue::PartialBuiltin(_) => {
                Err(Error::new(format!("`{label}` is not binary text data")))
            }
        }
    }

    fn record_syntax_diagnostics(
        &self,
        source: &str,
        diagnostics: &[SyntaxDiagnostic],
        session: &Arc<Mutex<Vec<Diagnostic>>>,
    ) -> bool {
        let mut had_errors = false;
        for diagnostic in diagnostics {
            had_errors |= diagnostic.severity == Severity::Error;
            let diagnostic = Diagnostic::from_syntax(source, diagnostic);
            session
                .lock()
                .expect("build diagnostic mutex should not be poisoned")
                .push(diagnostic.clone());
            self.record_diagnostic(diagnostic);
        }
        had_errors
    }
}

pub struct ModuleBuilder<'a> {
    assembler: &'a Assembler,
    inputs: Vec<ModuleInput>,
    arguments: Vec<String>,
    environment: Option<Value>,
}

impl ModuleBuilder<'_> {
    pub fn input(mut self, input: ModuleInput) -> Self {
        self.inputs.push(input);
        self
    }

    pub fn inputs(mut self, inputs: impl IntoIterator<Item = ModuleInput>) -> Self {
        self.inputs.extend(inputs);
        self
    }

    pub fn file(self, path: impl Into<PathBuf>) -> Self {
        self.input(ModuleInput::file(path))
    }

    pub fn script(self, extension: impl Into<String>, body: impl Into<String>) -> Self {
        self.input(ModuleInput::script(extension, body))
    }

    pub fn argument(mut self, argument: impl Into<String>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    pub fn arguments<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.arguments.extend(arguments.into_iter().map(Into::into));
        self
    }

    /// Supplies the assembly-level `env` value and skips configuration loading.
    pub fn env(mut self, environment: Value) -> Self {
        self.environment = Some(environment);
        self
    }

    pub fn build(self) -> Result<BuiltModule, Error> {
        self.assembler
            .build_module(self.inputs, self.arguments, self.environment)
    }
}

fn is_undefined_value(value: &CoreValue) -> bool {
    matches!(value, CoreValue::Dict(dict) if dict.is_empty())
}

fn resolve_local_import_path(
    importer_source_path: Option<&str>,
    reference: &str,
    kind: &str,
) -> Result<PathBuf, String> {
    let importer = importer_source_path.ok_or_else(|| {
        format!("{kind} `{reference}` cannot be loaded from a source without a file path")
    })?;
    let base = Path::new(importer)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    Ok(base.join(reference))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_history_is_bounded_and_counts_dropped_entries() {
        let assembler = Assembler::default().with_diagnostic_buffer(2);
        let session = Arc::new(Mutex::new(Vec::new()));
        for line in 1..=3 {
            assembler.record_syntax_diagnostics(
                "test.g",
                &[SyntaxDiagnostic {
                    severity: Severity::Warning,
                    line,
                    message: format!("warning {line}"),
                }],
                &session,
            );
        }

        let snapshot = assembler
            .read_diagnostics()
            .expect("diagnostic buffer should support reads");
        assert_eq!(snapshot.dropped(), 1);
        assert_eq!(
            snapshot
                .entries()
                .iter()
                .filter_map(Diagnostic::line)
                .collect::<Vec<_>>(),
            [2, 3]
        );
        assert_eq!(
            assembler
                .read_diagnostics()
                .expect("diagnostic buffer should support reads"),
            DiagnosticSnapshot {
                entries: Vec::new(),
                dropped: 0,
            }
        );
    }

    #[test]
    fn diagnostic_callback_replaces_the_default_buffer() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let callback_values = received.clone();
        let assembler = Assembler::default().with_diagnostic_callback(move |diagnostic| {
            callback_values
                .lock()
                .expect("callback collection mutex should not be poisoned")
                .push(diagnostic);
        });

        assembler.record_diagnostic(Diagnostic::new(Severity::Info, "hello"));

        assert!(assembler.read_diagnostics().is_none());
        assert_eq!(
            received
                .lock()
                .expect("callback collection mutex should not be poisoned")[0]
                .message(),
            "hello"
        );
    }
}
