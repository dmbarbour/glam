//! Stable embedding-oriented facade for assembling modules and observing values.
//!
//! This module owns host capabilities and orchestration. Front-end syntax,
//! core values, evaluator topology, and interaction-net scheduling remain
//! implementation details behind the facade.

use std::collections::VecDeque;
use std::fmt;
use std::marker::PhantomData;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use bytes::Bytes;

use crate::compiler::{
    BinaryFileLoader, BinaryLoadArgs, CompileContext, CompileDiagnosticEmitter, ModuleLoadArgs,
    ModuleLoader,
};
use crate::core::Value as CoreValue;
use crate::core::{Builtin, Dict, Key, List, NetValue};
use crate::core_net::CoreSpecialization;
use crate::diagnostic::{CompilationInvocationId, CompilationTrace, Severity, SourceIdentity};
use crate::eval;
use crate::evaluation::{
    EvalContext, EvaluationSession, EvaluationSessionRun, EvaluationUnfinishedState,
};
use crate::g_syntax::compile_source;
use crate::interaction_net::{NetBuildError, NetBuilder as CoreNetBuilder, Port as CorePort};
use crate::number::Number;
use crate::reflection::{
    CommitResult, HostSnapshot, ReflectionEffects, ReflectionServices, TaskCommit, TaskEnvironment,
    TaskHost, task_launcher,
};

pub const DEFAULT_DIAGNOSTIC_CAPACITY: usize = 1_000;
const BACKGROUND_TASK_STEP_BUDGET: usize = 256;
const GLAM_COMPATIBILITY_VERSION: &str = "0.1.0";
const IMPLEMENTATION_NAME: &str = "rust-bootstrap";

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

    pub fn atom_from_text(text: impl AsRef<str>) -> Self {
        let key = Key::binary_from_text(text.as_ref());
        Self(CoreValue::Atom(crate::core::Atom::from_key(&key)))
    }

    pub fn integer(value: i64) -> Self {
        Self(CoreValue::Number(Number::integer(value)))
    }

    /// Constructs a small exact rational, normalized to lowest terms.
    /// Returns `None` when `denominator` is zero.
    pub fn rational(numerator: i64, denominator: i64) -> Option<Self> {
        Number::from_ratio_i64(numerator, denominator).map(|number| Self(CoreValue::Number(number)))
    }

    /// Constructs the exact rational represented by a finite `f64`.
    /// NaN and either infinity return `None`.
    pub fn number_from_f64(value: f64) -> Option<Self> {
        Number::from_f64(value).map(|number| Self(CoreValue::Number(number)))
    }

    /// Parses an exact number without exposing the backing big-number types.
    /// Both `-3/2` and glam's `_3/2` spelling are accepted.
    pub fn number_from_text(text: impl AsRef<str>) -> Result<Self, Error> {
        Number::parse(text.as_ref())
            .map(|number| Self(CoreValue::Number(number)))
            .map_err(Error::new)
    }

    pub fn list(values: impl IntoIterator<Item = Value>) -> Self {
        Self(CoreValue::List(List::from_values(
            values.into_iter().map(Value::into_core).collect(),
        )))
    }

    pub fn record<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, Value)>,
        S: AsRef<str>,
    {
        let dict = entries
            .into_iter()
            .fold(Dict::new_sync(), |dict, (name, value)| {
                dict.insert(Key::atom_from_text(name), value.into_core())
            });
        Self(CoreValue::Dict(dict))
    }

    /// Constructs a dictionary from arbitrary keyable values.
    pub fn dictionary(entries: impl IntoIterator<Item = (Value, Value)>) -> Result<Self, Error> {
        let mut dict = Dict::new_sync();
        for (key, value) in entries {
            let key = Key::from_value(key.as_core())
                .ok_or_else(|| Error::new("dictionary key is not immediately keyable"))?;
            dict = dict.insert(key, value.into_core());
        }
        Ok(Self(CoreValue::Dict(dict)))
    }

    pub fn empty_record() -> Self {
        Self(CoreValue::Dict(Dict::new_sync()))
    }

    pub fn builtin(builtin: Builtin) -> Self {
        Self(CoreValue::Builtin(builtin))
    }

    pub fn builtin_call(builtin: Builtin, arguments: impl IntoIterator<Item = Value>) -> Self {
        Self(CoreValue::builtin_call(
            builtin,
            arguments.into_iter().map(Value::into_core).collect(),
        ))
    }

    pub fn abstract_global_path<I, S>(parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(CoreValue::Atom(crate::core::Atom::from_key(
            &Key::abstract_global_path(parts),
        )))
    }

    pub fn is_undefined(&self) -> bool {
        matches!(&self.0, CoreValue::Dict(dict) if dict.is_empty())
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

    pub fn as_i64(&self) -> Option<i64> {
        match &self.0 {
            CoreValue::Number(number) => number.to_i64_if_integer(),
            _ => None,
        }
    }

    pub fn as_rational_i64(&self) -> Option<(i64, i64)> {
        match &self.0 {
            CoreValue::Number(number) => number.to_ratio_i64(),
            _ => None,
        }
    }

    /// Converts a number lossily to a finite `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        match &self.0 {
            CoreValue::Number(number) => number.to_f64(),
            _ => None,
        }
    }

    /// Returns the canonical exact integer or `numerator/denominator` text.
    pub fn as_number_text(&self) -> Option<String> {
        match &self.0 {
            CoreValue::Number(number) => Some(number.to_string()),
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

/// An opaque port created during one [`Assembler::net`] construction.
///
/// The lifetime prevents ports from escaping their construction callback or
/// being mixed between builders. Copying a handle does not copy the net value;
/// wiring either copy twice is rejected by the checked builder.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NetPort<'net> {
    port: CorePort,
    brand: PhantomData<fn(&'net mut ()) -> &'net mut ()>,
}

impl fmt::Debug for NetPort<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NetPort(..)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetBind<'net> {
    pub application: NetPort<'net>,
    pub argument: NetPort<'net>,
    pub result: NetPort<'net>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetCopy<'net> {
    pub input: NetPort<'net>,
    pub outputs: Vec<NetPort<'net>>,
}

/// Checked, core-specialized construction of one closed interaction net.
///
/// This deliberately exposes only the operations needed by the future
/// `interaction_net` effect replay. Returning a port from the callback selects
/// the net's sole exposed port; every other port must be wired exactly once.
pub struct NetBuilder<'net> {
    builder: CoreNetBuilder<CoreSpecialization>,
    brand: PhantomData<fn(&'net mut ()) -> &'net mut ()>,
}

impl<'net> NetBuilder<'net> {
    pub fn bind(&mut self) -> NetBind<'net> {
        let [application, argument, result] = self.builder.bind();
        NetBind {
            application: self.port(application),
            argument: self.port(argument),
            result: self.port(result),
        }
    }

    pub fn copy(&mut self, outputs: usize) -> NetCopy<'net> {
        let copy = self.builder.copy(outputs);
        NetCopy {
            input: self.port(copy.input),
            outputs: copy
                .outputs
                .into_iter()
                .map(|port| self.port(port))
                .collect(),
        }
    }

    pub fn data(&mut self, value: Value) -> NetPort<'net> {
        let port = self.builder.data(value.into_core());
        self.port(port)
    }

    pub fn wire(&mut self, left: NetPort<'net>, right: NetPort<'net>) -> Result<(), Error> {
        self.builder
            .try_wire(left.port, right.port)
            .map_err(net_build_error)
    }

    fn new() -> Self {
        Self {
            builder: CoreNetBuilder::new(),
            brand: PhantomData,
        }
    }

    fn port(&self, port: CorePort) -> NetPort<'net> {
        NetPort {
            port,
            brand: PhantomData,
        }
    }
}

/// One raw diagnostic emission retained or dispatched by an [`Assembler`].
///
/// The emission stays unchanged in the envelope. Observers may explicitly
/// apply assembler provenance, then add viewer-specific context, without
/// affecting other observers of the same diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    emission: Value,
    origin: Option<Value>,
    // Transitional projections for simple embedding clients that do not yet
    // inspect the object message.
    source: Option<Arc<str>>,
    severity: Severity,
    line: Option<usize>,
    message: Arc<str>,
}

impl Diagnostic {
    pub fn new(severity: Severity, message: impl Into<Arc<str>>) -> Self {
        let message = message.into();
        Self::from_parts(
            None,
            severity,
            crate::diagnostic::text_message(None, &message),
            None,
        )
    }

    /// Wraps an arbitrary diagnostic value with separately supplied severity.
    /// Assembler and viewer metadata remain unapplied until enrichment.
    pub fn from_emission(severity: Severity, emission: Value) -> Self {
        Self::from_parts(None, severity, emission.into_core(), None)
    }

    pub fn with_source_location(self, source: impl Into<Arc<str>>, line: usize) -> Self {
        let source = source.into();
        let origin = CoreValue::Dict(Dict::new_sync().insert(
            (*crate::core::keys::SOURCE).clone(),
            SourceIdentity::file(source.clone()).value(),
        ));
        Self::from_parts(
            Some(source.clone()),
            self.severity,
            crate::diagnostic::text_message(Some(line), &self.message),
            Some(origin),
        )
    }

    /// Returns the front-end or runtime value exactly as it was emitted.
    pub fn emission(&self) -> &Value {
        &self.emission
    }

    /// Returns assembler provenance before it is mixed into the emission.
    pub fn origin(&self) -> Option<&Value> {
        self.origin.as_ref()
    }

    /// Applies authoritative assembler metadata to a fresh diagnostic object.
    pub fn enrich(&self) -> Result<Value, Error> {
        crate::diagnostic::enrich(
            self.emission.as_core().clone(),
            self.severity,
            self.origin.as_ref().map(|origin| origin.as_core().clone()),
        )
        .map(Value::from_core)
        .map_err(Error::new)
    }

    /// Applies assembler metadata followed by observer-specific object updates.
    /// The raw emission and other enriched views remain unchanged.
    pub fn enrich_with(&self, updates: Value) -> Result<Value, Error> {
        let enriched = self.enrich()?;
        crate::diagnostic::apply_updates(enriched.into_core(), updates.into_core())
            .map(Value::from_core)
            .map_err(Error::new)
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

    fn from_compile(trace: &CompilationTrace, severity: Severity, message: CoreValue) -> Self {
        Self::from_parts(
            Some(trace.source_label().clone()),
            severity,
            message,
            Some(trace.origin_value()),
        )
    }

    fn from_parts(
        source: Option<Arc<str>>,
        severity: Severity,
        message: CoreValue,
        origin: Option<CoreValue>,
    ) -> Self {
        let (line, text) = crate::diagnostic::conventional_summary(&message);
        Self {
            emission: Value::from_core(message),
            origin: origin.map(Value::from_core),
            source,
            severity,
            line,
            message: text.unwrap_or_else(|| Arc::from("<diagnostic has no immediate text view>")),
        }
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

struct AssemblerReflectionHost {
    reflection_environment: Value,
    diagnostic_sink: Arc<dyn DiagnosticSink>,
    state: Mutex<AssemblerReflectionState>,
    changed: Condvar,
}

struct AssemblerReflectionState {
    generation: u64,
    heap: Value,
}

impl AssemblerReflectionHost {
    fn new(reflection_environment: Value, diagnostic_sink: Arc<dyn DiagnosticSink>) -> Self {
        Self {
            reflection_environment,
            diagnostic_sink,
            state: Mutex::new(AssemblerReflectionState {
                generation: 1,
                heap: Value::empty_record(),
            }),
            changed: Condvar::new(),
        }
    }
}

impl TaskEnvironment for AssemblerReflectionHost {
    fn reflection_environment(&self) -> Value {
        self.reflection_environment.clone()
    }
}

impl ReflectionServices for AssemblerReflectionHost {
    fn emit_diagnostic(&self, diagnostic: Diagnostic) {
        self.diagnostic_sink.emit(diagnostic);
    }
}

impl TaskHost<ReflectionEffects> for AssemblerReflectionHost {
    fn snapshot(&self) -> HostSnapshot<ReflectionEffects> {
        let state = self
            .state
            .lock()
            .expect("assembler reflection host mutex should not be poisoned");
        HostSnapshot::new(state.generation, state.heap.clone(), ())
    }

    fn commit(&self, commit: TaskCommit<ReflectionEffects>) -> CommitResult {
        let diagnostics = {
            let mut state = self
                .state
                .lock()
                .expect("assembler reflection host mutex should not be poisoned");
            if state.generation != commit.generation() {
                return CommitResult::Conflict;
            }
            state.heap = commit.heap().clone();
            state.generation = state.generation.wrapping_add(1);
            self.changed.notify_all();
            commit.extra().diagnostics().to_vec()
        };
        for diagnostic in diagnostics {
            self.diagnostic_sink.emit(diagnostic);
        }
        commit.extra().commit_task_updates();
        CommitResult::Committed
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("assembler reflection host mutex should not be poisoned");
        while state.generation == observed_generation {
            state = self
                .changed
                .wait(state)
                .expect("assembler reflection host mutex should not be poisoned");
        }
        true
    }
}

fn evaluation_session(
    reflection_environment: Value,
    diagnostic_sink: Arc<dyn DiagnosticSink>,
) -> Arc<EvaluationSession> {
    let session = Arc::new(EvaluationSession::with_environment(
        reflection_environment.as_core().clone(),
    ));
    let host = Arc::new(AssemblerReflectionHost::new(
        reflection_environment,
        diagnostic_sink,
    ));
    session
        .install_reflection_launcher(task_launcher(ReflectionEffects, host))
        .expect("fresh evaluation session must accept its reflection launcher");
    session
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

/// External capabilities used to load module sources and binary inputs.
pub trait Host: Send + Sync {
    fn read(&self, path: &Path) -> Result<Bytes, HostError>;

    fn path_exists(&self, path: &Path) -> bool;
}

impl<T: Host + ?Sized> Host for Arc<T> {
    fn read(&self, path: &Path) -> Result<Bytes, HostError> {
        (**self).read(path)
    }

    fn path_exists(&self, path: &Path) -> bool {
        (**self).path_exists(path)
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleInput {
    File(PathBuf),
    Script { extension: String, body: Bytes },
}

impl ModuleInput {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }

    pub fn script(extension: impl Into<String>, body: impl Into<String>) -> Self {
        Self::Script {
            extension: extension.into(),
            body: Bytes::from(body.into()),
        }
    }
}

struct PreparedSource {
    bytes: Bytes,
    context: CompileContext,
    had_errors: Arc<AtomicBool>,
}

struct CompileSetup {
    module_path: Arc<[String]>,
    prior_defs: CoreValue,
    final_defs: CoreValue,
    module_loader: ModuleLoader,
    binary_loader: BinaryFileLoader,
    session: Arc<Mutex<Vec<Diagnostic>>>,
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

fn net_build_error(error: NetBuildError) -> Error {
    Error::new(format!("invalid interaction net: {error}"))
}

/// Result of running every currently scheduled reflection task to a terminal
/// state or to a stable quiescent pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningReport {
    status: ReasoningStatus,
    failures: Vec<ReasoningFailure>,
    unfinished: Vec<ReasoningTask>,
}

impl ReasoningReport {
    pub fn status(&self) -> ReasoningStatus {
        self.status
    }

    pub fn failures(&self) -> &[ReasoningFailure] {
        &self.failures
    }

    pub fn unfinished(&self) -> &[ReasoningTask] {
        &self.unfinished
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningStatus {
    Complete,
    Deadlocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningFailure {
    task_id: u64,
    message: Arc<str>,
}

impl ReasoningFailure {
    pub fn task_id(&self) -> u64 {
        self.task_id
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningTask {
    task_id: u64,
    state: ReasoningTaskState,
    waiting_on_task: Option<u64>,
    wait_id: Option<u64>,
    observed_generation: Option<u64>,
}

impl ReasoningTask {
    pub fn task_id(&self) -> u64 {
        self.task_id
    }

    pub fn state(&self) -> ReasoningTaskState {
        self.state
    }

    pub fn waiting_on_task(&self) -> Option<u64> {
        self.waiting_on_task
    }

    pub fn wait_id(&self) -> Option<u64> {
        self.wait_id
    }

    pub fn observed_generation(&self) -> Option<u64> {
        self.observed_generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningTaskState {
    Dormant,
    Reserved,
    Queued,
    Running,
    Blocked,
}

fn authoritative_reflection_environment(environment: Value) -> Result<Value, Error> {
    let CoreValue::Dict(root) = environment.into_core() else {
        return Err(Error::new("reflection environment must be a dictionary"));
    };
    let glam_key = Key::atom_from_text("glam");
    let mut glam = match root.get(&glam_key) {
        Some(CoreValue::Dict(glam)) => glam.clone(),
        _ => Dict::new_sync(),
    };
    glam = glam.insert(
        Key::atom_from_text("version"),
        CoreValue::binary_from_text(GLAM_COMPATIBILITY_VERSION),
    );
    let implementation_key = Key::atom_from_text("implementation");
    let mut implementation = match glam.get(&implementation_key) {
        Some(CoreValue::Dict(implementation)) => implementation.clone(),
        _ => Dict::new_sync(),
    };
    implementation = implementation
        .insert(
            Key::atom_from_text("name"),
            CoreValue::binary_from_text(IMPLEMENTATION_NAME),
        )
        .insert(
            Key::atom_from_text("version"),
            CoreValue::binary_from_text(env!("CARGO_PKG_VERSION")),
        );
    glam = glam.insert(implementation_key, CoreValue::Dict(implementation));
    Ok(Value(CoreValue::Dict(
        root.insert(glam_key, CoreValue::Dict(glam)),
    )))
}

#[derive(Clone)]
pub struct Assembler {
    host: Arc<dyn Host>,
    diagnostic_sink: Arc<dyn DiagnosticSink>,
    reflection_environment: Value,
    next_compilation_invocation: Arc<AtomicU64>,
    evaluation_session: Arc<EvaluationSession>,
}

impl Default for Assembler {
    fn default() -> Self {
        let diagnostic_sink: Arc<dyn DiagnosticSink> =
            Arc::new(DiagnosticBuffer::new(DEFAULT_DIAGNOSTIC_CAPACITY));
        let host: Arc<dyn Host> = Arc::new(SystemHost);
        let reflection_environment = authoritative_reflection_environment(Value::empty_record())
            .expect("the default reflection environment must be a dictionary");
        let evaluation_session =
            evaluation_session(reflection_environment.clone(), diagnostic_sink.clone());
        Self {
            host,
            diagnostic_sink,
            reflection_environment,
            next_compilation_invocation: Arc::new(AtomicU64::new(1)),
            evaluation_session,
        }
    }
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached closed Glam function used by the executable's
    /// default terminal logger. It expects an enriched diagnostic containing
    /// the conventional `msg` and `viewer` fields and returns bytes.
    pub fn default_diagnostic_formatter(&self) -> Value {
        Value::from_core(crate::g_syntax::default_diagnostic_formatter())
    }

    /// Returns the read-only environment shared by reflection tasks in this
    /// assembler's evaluation session.
    pub fn reflection_environment(&self) -> Value {
        self.reflection_environment.clone()
    }

    pub(crate) fn eval_context(&self) -> EvalContext {
        EvalContext::new(self.evaluation_session.clone())
    }

    fn pump_background_tasks(&self) {
        self.eval_context()
            .pump_background(BACKGROUND_TASK_STEP_BUDGET);
    }

    /// Runs scheduled reflection reasoning without imposing a step or time
    /// limit. A runnable infinite task therefore keeps this call running.
    pub fn drain_reasoning(&self) -> ReasoningReport {
        let run = self.eval_context().run_until_quiescent();
        let (status, report) = match run {
            EvaluationSessionRun::Complete(report) => (ReasoningStatus::Complete, report),
            EvaluationSessionRun::Quiescent(report) => (ReasoningStatus::Deadlocked, report),
        };
        ReasoningReport {
            status,
            failures: report
                .failures
                .into_iter()
                .map(|failure| ReasoningFailure {
                    task_id: failure.task.get(),
                    message: failure.error,
                })
                .collect(),
            unfinished: report
                .unfinished
                .into_iter()
                .map(|task| ReasoningTask {
                    task_id: task.task.get(),
                    state: match task.state {
                        EvaluationUnfinishedState::Dormant => ReasoningTaskState::Dormant,
                        EvaluationUnfinishedState::Reserved => ReasoningTaskState::Reserved,
                        EvaluationUnfinishedState::Queued => ReasoningTaskState::Queued,
                        EvaluationUnfinishedState::Running => ReasoningTaskState::Running,
                        EvaluationUnfinishedState::Blocked => ReasoningTaskState::Blocked,
                    },
                    waiting_on_task: task.dependency.map(|task| task.get()),
                    wait_id: task.wait,
                    observed_generation: task.observed_generation,
                })
                .collect(),
        }
    }

    pub fn with_host(mut self, host: impl Host + 'static) -> Self {
        self.host = Arc::new(host);
        self.evaluation_session = evaluation_session(
            self.reflection_environment.clone(),
            self.diagnostic_sink.clone(),
        );
        self
    }

    pub fn with_diagnostic_sink(mut self, sink: impl DiagnosticSink + 'static) -> Self {
        let diagnostic_sink: Arc<dyn DiagnosticSink> = Arc::new(sink);
        self.evaluation_session =
            evaluation_session(self.reflection_environment.clone(), diagnostic_sink.clone());
        self.diagnostic_sink = diagnostic_sink;
        self
    }

    /// Replaces the client-owned portion of the read-only reflection
    /// environment and starts a fresh evaluation session. The assembler
    /// always overwrites `glam.version` with its compatibility version and
    /// `glam.implementation` with the running implementation's identity.
    pub fn with_reflection_environment(mut self, environment: Value) -> Result<Self, Error> {
        self.reflection_environment = authoritative_reflection_environment(environment)?;
        self.evaluation_session = evaluation_session(
            self.reflection_environment.clone(),
            self.diagnostic_sink.clone(),
        );
        Ok(self)
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

    pub fn module<I, S>(&self, module_path: I) -> ModuleBuilder<'_>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        ModuleBuilder {
            assembler: self,
            module_path: Arc::from(
                module_path
                    .into_iter()
                    .map(Into::into)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            ),
            inputs: Vec::new(),
            initial_definitions: Value::empty_record(),
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

    fn next_compilation_invocation(&self) -> CompilationInvocationId {
        let id = self
            .next_compilation_invocation
            .fetch_add(1, Ordering::Relaxed);
        assert!(id != u64::MAX, "compilation invocation IDs exhausted");
        CompilationInvocationId::new(id)
    }

    /// Evaluates a value far enough to expose its outer semantic value.
    pub fn evaluate(&self, value: &Value) -> Result<Value, Error> {
        let result = eval::eval_value(&self.eval_context(), value.as_core())
            .map(Value::from_core)
            .map_err(|error| Error::new(error.to_string()));
        self.pump_background_tasks();
        result
    }

    /// Applies all supplied arguments while preserving evaluator laziness.
    /// Call [`Self::evaluate`] when the result itself must be observed.
    pub fn apply(
        &self,
        function: &Value,
        arguments: impl IntoIterator<Item = Value>,
    ) -> Result<Value, Error> {
        let result = eval::apply_values(
            &self.eval_context(),
            function.as_core().clone(),
            arguments.into_iter().map(Value::into_core).collect(),
        )
        .map(Value::from_core)
        .map_err(|error| Error::new(error.to_string()));
        self.pump_background_tasks();
        result
    }

    /// Builds one closed interaction-net value through a checked, effect-style
    /// API. The callback's returned port becomes the sole exposed port.
    pub fn net(
        &self,
        build: impl for<'net> FnOnce(&mut NetBuilder<'net>) -> Result<NetPort<'net>, Error>,
    ) -> Result<Value, Error> {
        let mut builder = NetBuilder::new();
        let exposed = build(&mut builder)?.port;
        let template = builder
            .builder
            .try_finish(exposed)
            .map_err(net_build_error)?;
        Ok(Value::from_core(CoreValue::Net(NetValue::new(
            template.instantiate_shared(),
        ))))
    }

    // TODO: add reflection snapshots and event subscriptions here. Reflection
    // producers should feed the same bounded history rather than print.

    pub fn get(&self, root: &Value, path: &str) -> Result<Value, Error> {
        let result = self
            .core_value_at_path(root.as_core(), path)
            .map(Value::from_core);
        self.pump_background_tasks();
        result
    }

    pub fn to_binary(&self, value: &Value) -> Result<Bytes, Error> {
        let result = self.core_value_bytes(value.as_core(), "value");
        self.pump_background_tasks();
        result
    }

    /// Extracts a byte range from compact binary data or a byte-valued list.
    /// Lazy list chunks are evaluated as required to locate the range.
    pub fn binary_slice(&self, value: &Value, range: Range<usize>) -> Result<Bytes, Error> {
        let result = self.core_value_binary_slice(value.as_core(), range, "value");
        self.pump_background_tasks();
        result
    }

    pub fn binary_at(&self, root: &Value, path: &str) -> Result<Bytes, Error> {
        let result = self
            .core_value_at_path(root.as_core(), path)
            .and_then(|value| self.core_value_bytes(&value, path));
        self.pump_background_tasks();
        result
    }

    fn build_module(
        &self,
        module_path: Arc<[String]>,
        inputs: Vec<ModuleInput>,
        initial_definitions: Value,
    ) -> Result<BuiltModule, Error> {
        let session = Arc::new(Mutex::new(Vec::new()));
        let result = self.build_module_inner(
            module_path,
            inputs,
            initial_definitions.into_core(),
            session.clone(),
        );
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
        module_path: Arc<[String]>,
        inputs: Vec<ModuleInput>,
        mut definitions: CoreValue,
        session: Arc<Mutex<Vec<Diagnostic>>>,
    ) -> Result<CoreValue, Error> {
        let module_loader = self.module_loader(session.clone());
        let binary_loader = self.binary_loader();
        let module_context = CompileContext::from_module_path(module_path.iter().cloned())
            .with_local_module_loader(module_loader.clone())
            .with_local_binary_loader(binary_loader.clone());
        let final_defs = module_context.final_defs().clone();
        let mut had_errors = false;

        for input in inputs.iter().rev() {
            let prepared = self.prepare_input(
                input,
                CompileSetup {
                    module_path: module_path.clone(),
                    prior_defs: definitions.clone(),
                    final_defs: final_defs.clone(),
                    module_loader: module_loader.clone(),
                    binary_loader: binary_loader.clone(),
                    session: session.clone(),
                },
            )?;
            definitions = compile_source(&prepared.bytes, &prepared.context);
            had_errors |= prepared.had_errors.load(Ordering::Relaxed);
        }

        if had_errors {
            return Err(Error::new("module failed to compile"));
        }

        let module_value = self.seal_module(&module_context, &definitions);
        eval::eval_value(&self.eval_context(), &module_value)
            .map_err(|error| Error::new(error.to_string()))
    }

    fn prepare_input(
        &self,
        input: &ModuleInput,
        setup: CompileSetup,
    ) -> Result<PreparedSource, Error> {
        let CompileSetup {
            module_path,
            prior_defs,
            final_defs,
            module_loader,
            binary_loader,
            session,
        } = setup;
        match input {
            ModuleInput::File(path) => {
                let bytes = self.read_source(path)?;
                let loader_label: Arc<str> = Arc::from(path.display().to_string());
                let source_label = absolute_source_label(path)?;
                let trace = Arc::new(CompilationTrace::root(
                    self.next_compilation_invocation(),
                    SourceIdentity::file(source_label),
                    module_path.clone(),
                ));
                let had_errors = Arc::new(AtomicBool::new(false));
                let context = CompileContext::from_module_path(module_path.iter().cloned())
                    .with_importer_source_path(loader_label)
                    .with_compilation_trace(trace.clone())
                    .with_prior_defs(prior_defs)
                    .with_final_defs(final_defs)
                    .with_local_module_loader(module_loader)
                    .with_local_binary_loader(binary_loader)
                    .with_diagnostic_emitter(self.compile_diagnostic_emitter(
                        trace,
                        session,
                        had_errors.clone(),
                    ));
                Ok(PreparedSource {
                    bytes,
                    context,
                    had_errors,
                })
            }
            ModuleInput::Script { extension, body } => {
                let label: Arc<str> = Arc::from(format!("<script.{extension}>"));
                let trace = Arc::new(CompilationTrace::root(
                    self.next_compilation_invocation(),
                    SourceIdentity::script(label, body.clone()),
                    module_path.clone(),
                ));
                let had_errors = Arc::new(AtomicBool::new(false));
                let context = CompileContext::from_module_path(module_path.iter().cloned())
                    .with_compilation_trace(trace.clone())
                    .with_prior_defs(prior_defs)
                    .with_final_defs(final_defs)
                    .with_local_module_loader(module_loader)
                    .with_local_binary_loader(binary_loader)
                    .with_diagnostic_emitter(self.compile_diagnostic_emitter(
                        trace,
                        session,
                        had_errors.clone(),
                    ));
                Ok(PreparedSource {
                    bytes: body.clone(),
                    context,
                    had_errors,
                })
            }
        }
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
            &args.request,
            "local import",
        )?;
        let loader_label: Arc<str> = Arc::from(path.display().to_string());
        let source_label = absolute_source_label(&path).map_err(|error| error.to_string())?;
        let source = self.read_source(&path).map_err(|error| error.to_string())?;
        let module_loader = self.module_loader(session.clone());
        let binary_loader = self.binary_loader();
        let had_errors = Arc::new(AtomicBool::new(false));
        let trace = match args.importer_trace {
            Some(parent) => Arc::new(CompilationTrace::imported(
                self.next_compilation_invocation(),
                SourceIdentity::file(source_label.clone()),
                args.module_path.clone(),
                parent,
                args.request.clone(),
                args.extends.clone(),
            )),
            None => Arc::new(CompilationTrace::root(
                self.next_compilation_invocation(),
                SourceIdentity::file(source_label),
                args.module_path.clone(),
            )),
        };
        let context = CompileContext::from_module_path(args.module_path.iter().cloned())
            .with_importer_source_path(loader_label.clone())
            .with_compilation_trace(trace.clone())
            .with_prior_defs(args.prior_defs)
            .with_final_defs(args.final_defs)
            .with_local_module_loader(module_loader)
            .with_local_binary_loader(binary_loader)
            .with_diagnostic_emitter(self.compile_diagnostic_emitter(
                trace,
                session,
                had_errors.clone(),
            ));
        let definitions = compile_source(&source, &context);

        if had_errors.load(Ordering::Relaxed) {
            Err(format!("local import `{loader_label}` failed to compile"))
        } else {
            Ok(definitions)
        }
    }

    fn load_local_binary(&self, args: BinaryLoadArgs) -> Result<CoreValue, String> {
        let path = resolve_local_import_path(
            args.importer_source_path.as_deref(),
            &args.request,
            "binary import",
        )?;
        self.host
            .read(&path)
            .map(CoreValue::Binary)
            .map_err(|error| error.to_string())
    }

    fn read_source(&self, path: &Path) -> Result<Bytes, Error> {
        self.host
            .read(path)
            .map_err(|error| Error::new(error.to_string()))
    }

    fn seal_module(&self, context: &CompileContext, definitions: &CoreValue) -> CoreValue {
        let CoreValue::Lazy(final_defs) = context.final_defs() else {
            panic!("CompileContext.final_defs must be a promised lazy value");
        };
        final_defs
            .set(definitions.clone())
            .expect("CompileContext.final_defs future must be unassigned");
        definitions.clone()
    }

    fn core_value_at_path(&self, root: &CoreValue, path: &str) -> Result<CoreValue, Error> {
        let mut current = root.clone();
        let context = self.eval_context();

        for part in path.split('.') {
            let current_value = eval::eval_value(&context, &current)
                .map_err(|error| Error::new(error.to_string()))?;
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
            CoreValue::List(list) => eval::list_output_bytes(&self.eval_context(), list)
                .map(Bytes::from)
                .map_err(|error| Error::new(format!("`{label}` {error}"))),
            CoreValue::Lazy(_) => {
                let value = eval::eval_value(&self.eval_context(), value)
                    .map_err(|error| Error::new(error.to_string()))?;
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

    fn core_value_binary_slice(
        &self,
        value: &CoreValue,
        range: Range<usize>,
        label: &str,
    ) -> Result<Bytes, Error> {
        if range.start > range.end {
            return Err(Error::new(format!(
                "invalid binary range {}..{}",
                range.start, range.end
            )));
        }

        match value {
            CoreValue::Binary(bytes) => {
                (range.end <= bytes.len()).then(|| bytes.slice(range.clone()))
            }
            CoreValue::List(list) => {
                { eval::list_output_bytes_range(&self.eval_context(), list, range.clone()) }
                    .map(|bytes| bytes.map(Bytes::from))
                    .map_err(|error| Error::new(format!("`{label}` {error}")))?
            }
            CoreValue::Lazy(_) | CoreValue::Net(_) => {
                let value = eval::eval_value(&self.eval_context(), value)
                    .map_err(|error| Error::new(error.to_string()))?;
                return self.core_value_binary_slice(&value, range, label);
            }
            CoreValue::Atom(_)
            | CoreValue::Dict(_)
            | CoreValue::Number(_)
            | CoreValue::Function(_)
            | CoreValue::Builtin(_)
            | CoreValue::PartialBuiltin(_) => {
                return Err(Error::new(format!("`{label}` is not binary list data")));
            }
        }
        .ok_or_else(|| {
            Error::new(format!(
                "binary range {}..{} is out of bounds for `{label}`",
                range.start, range.end
            ))
        })
    }

    fn compile_diagnostic_emitter(
        &self,
        trace: Arc<CompilationTrace>,
        session: Arc<Mutex<Vec<Diagnostic>>>,
        had_errors: Arc<AtomicBool>,
    ) -> CompileDiagnosticEmitter {
        let assembler = self.clone();
        Arc::new(move |severity, message| {
            if severity == Severity::Error {
                had_errors.store(true, Ordering::Relaxed);
            }
            let diagnostic = Diagnostic::from_compile(&trace, severity, message);
            session
                .lock()
                .expect("build diagnostic mutex should not be poisoned")
                .push(diagnostic.clone());
            assembler.record_diagnostic(diagnostic);
        })
    }
}

pub struct ModuleBuilder<'a> {
    assembler: &'a Assembler,
    module_path: Arc<[String]>,
    inputs: Vec<ModuleInput>,
    initial_definitions: Value,
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

    pub fn initial_definitions(mut self, definitions: Value) -> Self {
        self.initial_definitions = definitions;
        self
    }

    pub fn build(self) -> Result<BuiltModule, Error> {
        self.assembler
            .build_module(self.module_path, self.inputs, self.initial_definitions)
    }
}

fn resolve_local_import_path(
    importer_source_path: Option<&str>,
    request: &str,
    kind: &str,
) -> Result<PathBuf, String> {
    crate::compiler::validate_local_source_request(request)?;
    let importer = importer_source_path.ok_or_else(|| {
        format!("{kind} `{request}` cannot be loaded from a source without a file path")
    })?;
    let base = Path::new(importer)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    Ok(base.join(request))
}

fn absolute_source_label(path: &Path) -> Result<Arc<str>, Error> {
    std::path::absolute(path)
        .map(|path| Arc::from(path.display().to_string()))
        .map_err(|error| {
            Error::new(format!(
                "could not make source path `{}` absolute: {error}",
                path.display()
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_compilation_trace(source: &str) -> CompilationTrace {
        CompilationTrace::root(
            CompilationInvocationId::new(1),
            SourceIdentity::file(Arc::<str>::from(source)),
            Arc::from(["test".to_owned()]),
        )
    }

    #[test]
    fn assembler_clones_share_one_evaluation_session() {
        let assembler = Assembler::new();
        let clone = assembler.clone();

        assert!(
            assembler
                .eval_context()
                .shares_session_with(&clone.eval_context())
        );
        assert!(
            !assembler
                .eval_context()
                .shares_session_with(&Assembler::new().eval_context())
        );
    }

    #[test]
    fn reflection_annotations_launch_tasks_and_return_their_targets() {
        let assembler = Assembler::new();
        let module = assembler
            .module(["annotation_test"])
            .script(
                "g",
                "language g0\nimport 'std\neffect = .r ()\nresult = anno { refl:effect } \"ready\"\n",
            )
            .build()
            .expect("reflection annotation fixture should compile");
        let result = assembler
            .get(module.value(), "result")
            .expect("fixture should define result");

        assert_eq!(
            assembler
                .to_binary(&assembler.evaluate(&result).unwrap())
                .unwrap(),
            b"ready".as_slice()
        );
    }

    #[test]
    fn reflection_annotations_require_their_tasks_to_return_unit() {
        let assembler = Assembler::new();
        let module = assembler
            .module(["annotation_test"])
            .script(
                "g",
                "language g0\nimport 'std\neffect = .r \"not unit\"\nresult = anno { refl:effect } \"unreachable\"\n",
            )
            .build()
            .expect("reflection annotation fixture should compile");
        let result = assembler
            .get(module.value(), "result")
            .expect("fixture should define result");

        assert!(
            assembler
                .to_binary(&result)
                .unwrap_err()
                .to_string()
                .contains("reflection annotation requires its effect to return unit")
        );
    }

    #[test]
    fn reflection_annotation_logs_use_the_assembler_diagnostic_sink() {
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let received = diagnostics.clone();
        let assembler = Assembler::new().with_diagnostic_callback(move |diagnostic| {
            received
                .lock()
                .expect("diagnostic collection mutex should not be poisoned")
                .push(diagnostic);
        });
        let module = assembler
            .module(["annotation_test"])
            .script(
                "g",
                "language g0\nimport 'std\neffect = .log 'warn { msg:{ text:\"from annotation\" } }\nresult = anno { refl:effect } \"ready\"\n",
            )
            .build()
            .expect("reflection annotation fixture should compile");
        let result = assembler
            .get(module.value(), "result")
            .expect("fixture should define result");

        assert_eq!(
            assembler
                .to_binary(&result)
                .expect("logging annotation should complete"),
            b"ready".as_slice()
        );
        let diagnostics = diagnostics
            .lock()
            .expect("diagnostic collection mutex should not be poisoned");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity(), Severity::Warning);
        assert_eq!(diagnostics[0].message(), "from annotation");
    }

    #[test]
    fn diagnostic_history_is_bounded_and_counts_dropped_entries() {
        let assembler = Assembler::default().with_diagnostic_buffer(2);
        for line in 1..=3 {
            assembler.record_diagnostic(
                Diagnostic::new(Severity::Warning, format!("warning {line}"))
                    .with_source_location("test.g", line),
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
        let received = received
            .lock()
            .expect("callback collection mutex should not be poisoned");
        let CoreValue::Dict(emission) = received[0].emission().as_core() else {
            unreachable!()
        };
        assert!(emission.get(&*crate::core::keys::SPEC).is_none());
    }

    #[test]
    fn diagnostic_enrichment_is_an_authoritative_object_mixin() {
        let CoreValue::Dict(message) = crate::diagnostic::text_message(Some(7), "careful") else {
            unreachable!()
        };
        let CoreValue::Dict(interface) = message
            .get(&*crate::core::keys::MSG)
            .cloned()
            .expect("text diagnostic should provide msg")
        else {
            unreachable!()
        };
        let interface = interface.insert(
            (*crate::core::keys::SEVERITY).clone(),
            (*crate::core::keys::ERROR_VALUE).clone(),
        );
        let message = CoreValue::Dict(message.insert(
            (*crate::core::keys::MSG).clone(),
            CoreValue::Dict(interface),
        ));

        let trace = test_compilation_trace("test.g");
        let diagnostic = Diagnostic::from_compile(&trace, Severity::Warning, message);
        assert_eq!(diagnostic.severity(), Severity::Warning);

        let CoreValue::Dict(emission) = diagnostic.emission().as_core() else {
            panic!("raw diagnostic should be a dictionary");
        };
        let Some(CoreValue::Dict(interface)) = emission.get(&*crate::core::keys::MSG) else {
            panic!("raw diagnostic should provide msg");
        };
        assert_eq!(
            interface.get(&*crate::core::keys::SEVERITY),
            Some(&*crate::core::keys::ERROR_VALUE)
        );
        assert!(interface.get(&*crate::core::keys::ORIGIN).is_none());
        assert!(emission.get(&*crate::core::keys::SPEC).is_none());

        let enriched = diagnostic.enrich().expect("diagnostic should enrich");
        let CoreValue::Dict(enriched) = enriched.as_core() else {
            panic!("enriched diagnostic should be an object dictionary");
        };
        let Some(CoreValue::Dict(interface)) = enriched.get(&*crate::core::keys::MSG) else {
            panic!("enriched diagnostic should provide msg");
        };
        assert_eq!(
            interface.get(&*crate::core::keys::SEVERITY),
            Some(&*crate::core::keys::WARN_VALUE)
        );
        assert_eq!(
            interface
                .get(&*crate::core::keys::ORIGIN)
                .and_then(|origin| match origin {
                    CoreValue::Dict(origin) => origin.get(&*crate::core::keys::SOURCE),
                    _ => None,
                })
                .and_then(|source| match source {
                    CoreValue::Dict(source) => source.get(&*crate::core::keys::FILE),
                    _ => None,
                }),
            Some(&CoreValue::binary_from_text("test.g"))
        );

        let Some(CoreValue::Dict(spec)) = enriched.get(&*crate::core::keys::SPEC) else {
            panic!("each diagnostic mixin should update the object specification");
        };
        assert!(matches!(
            spec.get(&*crate::core::keys::DEFS),
            Some(CoreValue::PartialBuiltin(call))
                if call.builtin == Builtin::ObjectComposedDefs
        ));
    }

    #[test]
    fn viewers_can_inherit_one_diagnostic_independently() {
        let trace = test_compilation_trace("test.g");
        let diagnostic = Diagnostic::from_compile(
            &trace,
            Severity::Info,
            crate::diagnostic::text_message(Some(3), "hello"),
        );
        let viewer_key = Key::atom_from_text("viewer");
        let inherit = |name: &str| {
            diagnostic
                .enrich_with(Value::record([("viewer", Value::text(name))]))
                .expect("viewer mixin should apply")
        };

        let first = inherit("terminal");
        let second = inherit("ide");
        let CoreValue::Dict(original) = diagnostic.emission().as_core() else {
            unreachable!()
        };
        let CoreValue::Dict(first) = first.as_core() else {
            unreachable!()
        };
        let CoreValue::Dict(second) = second.as_core() else {
            unreachable!()
        };
        assert!(original.get(&viewer_key).is_none());
        assert_eq!(
            first.get(&viewer_key),
            Some(&CoreValue::binary_from_text("terminal"))
        );
        assert_eq!(
            second.get(&viewer_key),
            Some(&CoreValue::binary_from_text("ide"))
        );
        assert!(matches!(
            first
                .get(&*crate::core::keys::SPEC)
                .and_then(|spec| match spec {
                    CoreValue::Dict(spec) => spec.get(&*crate::core::keys::DEFS),
                    _ => None,
                }),
            Some(CoreValue::PartialBuiltin(call))
                if call.builtin == Builtin::ObjectComposedDefs
        ));
    }
}
