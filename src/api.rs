//! Stable embedding-oriented facade for assembling modules and observing values.
//!
//! This module owns staged source/reasoning construction and orchestration. Front-end syntax,
//! core values, evaluator topology, and interaction-net scheduling remain
//! implementation details behind the facade.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::ops::{Deref, Range};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use bytes::Bytes;

use crate::compiler::{
    BinaryFileLoader, BinaryLoadArgs, CompileContext, CompileDiagnosticEmitter, ModuleLoadArgs,
    ModuleLoader,
};
use crate::core::Value as CoreValue;
use crate::core::{Builtin, Dict, Key, List, NetValue};
use crate::core_net::CoreSpecialization;
use crate::diagnostic::{CompilationInvocationId, CompilationTrace, Severity};
use crate::eval;
use crate::evaluation::{
    EvalContext, EvaluationExecutor, EvaluationSession, EvaluationSessionRun,
    EvaluationUnfinishedState,
};
use crate::g_syntax::compile_source;
use crate::interaction_net::{NetBuildError, NetBuilder as CoreNetBuilder, Port as CorePort};
use crate::number::Number;
use crate::reflection::{
    CommitResult, ConflictAnalysisStrategy, ExactConflictAnalysis, HostSnapshot,
    ReasoningSessionId, ReflectionEffects, ReflectionServices, ReflectionStore, TaskCommit,
    TaskEnvironment, TaskHost, VolumeId, task_launcher, volume_effects,
};
use crate::source::{
    FileSourceSystem, Host, HostSourceSystem, SourceArtifact, SourceIdentity, SourceSystem,
};

const GLAM_COMPATIBILITY_VERSION: &str = "0.1.0";
const IMPLEMENTATION_NAME: &str = "rust-bootstrap";
static NEXT_REASONING_SESSION_ID: AtomicU64 = AtomicU64::new(1);

fn allocate_reasoning_session_id() -> ReasoningSessionId {
    let id = NEXT_REASONING_SESSION_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("reasoning session IDs exhausted");
    ReasoningSessionId::from_u64(id).expect("reasoning session IDs start at one")
}

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

    pub(crate) fn builtin(builtin: Builtin) -> Self {
        Self(CoreValue::Builtin(builtin))
    }

    pub(crate) fn builtin_call(
        builtin: Builtin,
        arguments: impl IntoIterator<Item = Value>,
    ) -> Self {
        Self(CoreValue::builtin_call(
            builtin,
            arguments.into_iter().map(Value::into_core).collect(),
        ))
    }

    /// Constructs an empty named Glam object without exposing bootstrap object
    /// implementation builtins through the embedding API.
    pub fn empty_object(name: Value) -> Self {
        let spec = Self::record([
            ("name", name),
            ("deps", Self::list(std::iter::empty())),
            ("defs", Self::builtin(Builtin::ObjectDefaultDefs)),
        ]);
        Self::builtin_call(Builtin::ObjectInstance, [spec])
    }

    /// Returns `target` after running `effect` as a reflection annotation.
    pub fn after_reflection(effect: Value, target: Value) -> Self {
        Self::builtin_call(Builtin::Anno, [Self::record([("refl", effect)]), target])
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
            CoreValue::Opaque(_) => ValueKind::Opaque,
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
    Opaque,
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
        let identity = SourceIdentity::file(Path::new(source.as_ref()));
        let origin = CoreValue::Dict(Dict::new_sync().insert(
            (*crate::core::keys::SOURCE).clone(),
            identity.value().as_core().clone(),
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
            Some(Arc::from(trace.source_label())),
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

/// One committed diagnostic publication within a reasoning session.
///
/// Sequence numbers are local to a [`DiagnosticBus`] and increase in commit
/// order. The diagnostic itself is shared across subscribers without copying
/// its value graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticEvent {
    sequence: u64,
    diagnostic: Arc<Diagnostic>,
}

impl DiagnosticEvent {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }
}

impl Deref for DiagnosticEvent {
    type Target = Diagnostic;

    fn deref(&self) -> &Self::Target {
        self.diagnostic()
    }
}

/// A coherent snapshot of all committed emissions on one diagnostic bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticCounts {
    next_sequence: u64,
    info: u64,
    warnings: u64,
    errors: u64,
}

impl DiagnosticCounts {
    /// Returns zero before the first publication.
    pub fn latest_sequence(&self) -> u64 {
        self.next_sequence - 1
    }

    pub fn info(&self) -> u64 {
        self.info
    }

    pub fn warnings(&self) -> u64 {
        self.warnings
    }

    pub fn errors(&self) -> u64 {
        self.errors
    }

    pub fn total(&self) -> u64 {
        self.info
            .checked_add(self.warnings)
            .and_then(|total| total.checked_add(self.errors))
            .expect("diagnostic count overflow")
    }
}

impl Default for DiagnosticCounts {
    fn default() -> Self {
        Self {
            next_sequence: 1,
            info: 0,
            warnings: 0,
            errors: 0,
        }
    }
}

/// Receiver for committed diagnostic events. Implementations may be called
/// concurrently and own any retention or rendering policy they need.
pub trait DiagnosticSubscriber: Send + Sync {
    fn receive(&self, event: DiagnosticEvent);
}

impl<T: DiagnosticSubscriber + ?Sized> DiagnosticSubscriber for Arc<T> {
    fn receive(&self, event: DiagnosticEvent) {
        (**self).receive(event);
    }
}

struct DiagnosticBusState {
    next_subscriber: u64,
    counts: DiagnosticCounts,
    subscribers: BTreeMap<u64, Arc<dyn DiagnosticSubscriber>>,
}

struct DiagnosticBusInner {
    state: Mutex<DiagnosticBusState>,
}

/// Non-buffering publication boundary for one reasoning session.
#[derive(Clone)]
pub struct DiagnosticBus {
    inner: Arc<DiagnosticBusInner>,
}

impl Default for DiagnosticBus {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticBus {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DiagnosticBusInner {
                state: Mutex::new(DiagnosticBusState {
                    next_subscriber: 1,
                    counts: DiagnosticCounts::default(),
                    subscribers: BTreeMap::new(),
                }),
            }),
        }
    }

    /// Publishes one event, updating authoritative counts before notifying the
    /// subscribers present at publication time. Subscriber calls occur outside
    /// the bus lock; sequence numbers, rather than callback completion order,
    /// define the order of concurrent publications.
    pub fn publish(&self, diagnostic: Diagnostic) -> DiagnosticEvent {
        let (event, subscribers) = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("diagnostic bus mutex should not be poisoned");
            let sequence = state.counts.next_sequence;
            state.counts.next_sequence = sequence
                .checked_add(1)
                .expect("diagnostic sequence numbers exhausted");
            let count = match diagnostic.severity() {
                Severity::Info => &mut state.counts.info,
                Severity::Warning => &mut state.counts.warnings,
                Severity::Error => &mut state.counts.errors,
            };
            *count = count.checked_add(1).expect("diagnostic count overflow");
            let event = DiagnosticEvent {
                sequence,
                diagnostic: Arc::new(diagnostic),
            };
            let subscribers = state.subscribers.values().cloned().collect::<Vec<_>>();
            (event, subscribers)
        };
        for subscriber in subscribers {
            subscriber.receive(event.clone());
        }
        event
    }

    pub fn counts(&self) -> DiagnosticCounts {
        self.inner
            .state
            .lock()
            .expect("diagnostic bus mutex should not be poisoned")
            .counts
    }

    pub fn subscribe(
        &self,
        subscriber: impl DiagnosticSubscriber + 'static,
    ) -> DiagnosticSubscription {
        self.subscribe_shared(Arc::new(subscriber))
    }

    pub fn subscribe_shared(
        &self,
        subscriber: Arc<dyn DiagnosticSubscriber>,
    ) -> DiagnosticSubscription {
        let id = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("diagnostic bus mutex should not be poisoned");
            let id = state.next_subscriber;
            state.next_subscriber = id
                .checked_add(1)
                .expect("diagnostic subscriber IDs exhausted");
            state.subscribers.insert(id, subscriber);
            id
        };
        DiagnosticSubscription {
            _inner: Arc::new(DiagnosticSubscriptionInner {
                bus: Arc::downgrade(&self.inner),
                id,
            }),
        }
    }
}

/// Keeps one diagnostic subscription registered until its last clone drops.
#[derive(Clone)]
pub struct DiagnosticSubscription {
    _inner: Arc<DiagnosticSubscriptionInner>,
}

struct DiagnosticSubscriptionInner {
    bus: Weak<DiagnosticBusInner>,
    id: u64,
}

impl Drop for DiagnosticSubscriptionInner {
    fn drop(&mut self) {
        let Some(bus) = self.bus.upgrade() else {
            return;
        };
        bus.state
            .lock()
            .expect("diagnostic bus mutex should not be poisoned")
            .subscribers
            .remove(&self.id);
    }
}

struct DiagnosticCallback<F>(F);

impl<F> DiagnosticSubscriber for DiagnosticCallback<F>
where
    F: Fn(DiagnosticEvent) + Send + Sync,
{
    fn receive(&self, event: DiagnosticEvent) {
        (self.0)(event);
    }
}

struct AssemblerReflectionHost {
    reasoning_session: ReasoningSessionId,
    reflection_environment: OnceLock<Value>,
    diagnostics: DiagnosticBus,
    state: Mutex<AssemblerReflectionState>,
    changed: Condvar,
}

struct AssemblerReflectionState {
    wake_generation: u64,
    store: ReflectionStore,
}

impl AssemblerReflectionHost {
    fn new_unsealed(
        reasoning_session: ReasoningSessionId,
        diagnostics: DiagnosticBus,
        conflict_analysis: Arc<dyn ConflictAnalysisStrategy>,
    ) -> Self {
        Self {
            reasoning_session,
            reflection_environment: OnceLock::new(),
            diagnostics,
            state: Mutex::new(AssemblerReflectionState {
                wake_generation: 1,
                store: ReflectionStore::new(conflict_analysis),
            }),
            changed: Condvar::new(),
        }
    }

    fn seal_environment(&self, environment: Value) -> Result<(), Error> {
        self.reflection_environment
            .set(environment)
            .map_err(|_| Error::new("reflection environment was already configured"))
    }

    fn set_conflict_analysis(&self, strategy: Arc<dyn ConflictAnalysisStrategy>) {
        self.state
            .lock()
            .expect("assembler reflection host mutex should not be poisoned")
            .store
            .set_strategy(strategy);
    }

    fn create_volume(&self, initial: Value) -> Result<(VolumeId, Value), Error> {
        let volume = {
            let mut state = self
                .state
                .lock()
                .expect("assembler reflection host mutex should not be poisoned");
            state
                .store
                .create_volume(initial)
                .map_err(|error| Error::new(error.as_ref()))?
        };
        Ok((volume, volume_effects(self.reasoning_session, volume)))
    }

    fn revoke_volume(&self, volume: VolumeId) -> Result<Value, Error> {
        let value = {
            let mut state = self
                .state
                .lock()
                .expect("assembler reflection host mutex should not be poisoned");
            let value = state.store.revoke_volume(volume).ok_or_else(|| {
                Error::new(format!(
                    "reflection volume {} has already been revoked",
                    volume.get()
                ))
            })?;
            state.wake_generation = state.wake_generation.wrapping_add(1);
            self.changed.notify_all();
            value
        };
        Ok(value)
    }
}

impl TaskEnvironment for AssemblerReflectionHost {
    fn reflection_environment(&self) -> Value {
        self.reflection_environment
            .get()
            .expect("reasoning host must be sealed before it runs tasks")
            .clone()
    }
}

impl ReflectionServices for AssemblerReflectionHost {
    fn emit_diagnostic(&self, diagnostic: Diagnostic) {
        self.diagnostics.publish(diagnostic);
    }

    fn complete_query(
        &self,
        handle: &Arc<crate::reflection::EvaluationQueryHandle>,
        result: Value,
    ) {
        let mut state = self
            .state
            .lock()
            .expect("assembler reflection host mutex should not be poisoned");
        if state.store.complete_query(handle, result) {
            state.wake_generation = state.wake_generation.wrapping_add(1);
            self.changed.notify_all();
        }
    }
}

impl TaskHost<ReflectionEffects> for AssemblerReflectionHost {
    fn snapshot(&self) -> HostSnapshot<ReflectionEffects> {
        let state = self
            .state
            .lock()
            .expect("assembler reflection host mutex should not be poisoned");
        HostSnapshot::new(state.wake_generation, state.store.snapshot(), ())
    }

    fn commit(&self, commit: TaskCommit<ReflectionEffects>) -> CommitResult {
        let (store, _extra_snapshot, extra) = commit.into_parts();
        let diagnostics = {
            let mut state = self
                .state
                .lock()
                .expect("assembler reflection host mutex should not be poisoned");
            match state.store.try_commit(&store) {
                crate::reflection::StoreCommitResult::Committed => {}
                crate::reflection::StoreCommitResult::Conflict => {
                    return CommitResult::Conflict;
                }
                crate::reflection::StoreCommitResult::MissingVolume(volume) => {
                    return CommitResult::MissingVolume(volume);
                }
            }
            state.wake_generation = state.wake_generation.wrapping_add(1);
            self.changed.notify_all();
            extra.diagnostics().to_vec()
        };
        for diagnostic in diagnostics {
            self.diagnostics.publish(diagnostic);
        }
        extra.commit_updates();
        CommitResult::Committed
    }

    fn reasoning_session_id(&self) -> Option<ReasoningSessionId> {
        Some(self.reasoning_session)
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("assembler reflection host mutex should not be poisoned");
        while state.wake_generation == observed_generation {
            state = self
                .changed
                .wait(state)
                .expect("assembler reflection host mutex should not be poisoned");
        }
        true
    }
}

/// Opaque background execution resources shared by related evaluation
/// sessions, including the assembler, logger, and future IDE services.
#[derive(Clone)]
pub struct EvaluationRuntime {
    executor: Arc<EvaluationExecutor>,
}

impl EvaluationRuntime {
    pub fn new(worker_threads: usize) -> Result<Self, Error> {
        Ok(Self {
            executor: EvaluationExecutor::new(worker_threads)
                .map_err(|error| Error::new(error.as_ref()))?,
        })
    }

    pub fn worker_threads(&self) -> usize {
        self.executor.worker_count()
    }

    pub(crate) fn executor(&self) -> &Arc<EvaluationExecutor> {
        &self.executor
    }
}

#[derive(Clone)]
struct ReasoningSession {
    host: Arc<AssemblerReflectionHost>,
    diagnostics: DiagnosticBus,
    runtime: EvaluationRuntime,
    evaluation: Arc<EvaluationSession>,
}

impl ReasoningSession {
    fn from_host(
        host: Arc<AssemblerReflectionHost>,
        diagnostics: DiagnosticBus,
        runtime: EvaluationRuntime,
    ) -> Self {
        let evaluation = EvaluationSession::shared(runtime.executor());
        evaluation
            .install_reflection_launcher(task_launcher(ReflectionEffects, host.clone()))
            .expect("fresh evaluation session must accept its reflection launcher");
        Self {
            host,
            diagnostics,
            runtime,
            evaluation,
        }
    }

    fn environment(&self) -> Value {
        self.host.reflection_environment()
    }

    fn diagnostics(&self) -> DiagnosticBus {
        self.diagnostics.clone()
    }

    fn runtime(&self) -> EvaluationRuntime {
        self.runtime.clone()
    }

    fn eval_context(&self) -> EvalContext {
        EvalContext::new(self.evaluation.clone())
    }

    fn conflict_analysis(&self) -> Arc<dyn ConflictAnalysisStrategy> {
        self.host
            .state
            .lock()
            .expect("assembler reflection host mutex should not be poisoned")
            .store
            .strategy()
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
    source: Arc<SourceArtifact>,
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

fn authoritative_reflection_environment(
    environment: Value,
    role: &str,
) -> Result<(Value, bool), Error> {
    let CoreValue::Dict(root) = environment.into_core() else {
        return Err(Error::new("reflection environment must be a dictionary"));
    };
    let glam_key = Key::atom_from_text("glam");
    let replaced_glam = root.get(&glam_key).is_some();
    Ok((
        Value(CoreValue::Dict(
            root.insert(glam_key, authoritative_glam_environment(role)),
        )),
        replaced_glam,
    ))
}

fn authoritative_glam_environment(role: &str) -> CoreValue {
    let implementation = Dict::new_sync()
        .insert(
            Key::atom_from_text("name"),
            CoreValue::binary_from_text(IMPLEMENTATION_NAME),
        )
        .insert(
            Key::atom_from_text("version"),
            CoreValue::binary_from_text(env!("CARGO_PKG_VERSION")),
        );
    let glam = Dict::new_sync()
        .insert(
            Key::atom_from_text("version"),
            CoreValue::binary_from_text(GLAM_COMPATIBILITY_VERSION),
        )
        .insert(
            Key::atom_from_text("implementation"),
            CoreValue::Dict(implementation),
        )
        .insert(
            Key::atom_from_text("reasoning"),
            CoreValue::Dict(Dict::new_sync().insert(
                Key::atom_from_text("role"),
                Value::atom_from_text(role).into_core(),
            )),
        );
    CoreValue::Dict(glam)
}

fn reflection_environment_for_role(environment: &Value, role: &str) -> Value {
    let CoreValue::Dict(root) = environment.as_core() else {
        unreachable!("authoritative reflection environment must be a dictionary")
    };
    Value(CoreValue::Dict(root.insert(
        Key::atom_from_text("glam"),
        authoritative_glam_environment(role),
    )))
}

/// Owner handle for one protected volume in a reasoning session.
///
/// Capability values may be cloned freely, but only this handle can remove the
/// volume and recover its final unforced value. Dropping the handle does not
/// revoke the volume.
pub struct ReasoningVolume {
    host: Arc<AssemblerReflectionHost>,
    volume: VolumeId,
    effects: Value,
}

impl ReasoningVolume {
    /// Returns the closed `{get, set, rewrite}` effect capability value.
    pub fn effects(&self) -> Value {
        self.effects.clone()
    }

    /// Removes the volume and returns its final value without forcing it.
    /// Further uses of any capability for this volume produce
    /// use-after-revoke errors.
    pub fn revoke(self) -> Result<Value, Error> {
        self.host.revoke_volume(self.volume)
    }
}

#[derive(Clone)]
pub struct Assembler {
    source_system: Arc<dyn SourceSystem>,
    next_compilation_invocation: Arc<AtomicU64>,
    reasoning: ReasoningSession,
    diagnostic_attachments: Vec<DiagnosticAttachment>,
}

#[derive(Clone)]
struct DiagnosticAttachment {
    _subscription: DiagnosticSubscription,
}

/// Staged construction of one assembler and its single reasoning session.
pub struct AssemblerBuilder {
    source_system: Arc<dyn SourceSystem>,
    runtime: EvaluationRuntime,
    host: Arc<AssemblerReflectionHost>,
    diagnostics: DiagnosticBus,
    reflection_environment: Option<Value>,
    diagnostic_attachments: Vec<DiagnosticAttachment>,
    pending_diagnostics: Vec<Diagnostic>,
}

/// Capabilities available while constructing the immutable reflection
/// environment. The borrow cannot escape the construction closure.
pub struct ReflectionEnvironmentBuilder<'a> {
    host: &'a Arc<AssemblerReflectionHost>,
}

impl ReflectionEnvironmentBuilder<'_> {
    /// Creates a protected volume belonging to the future reasoning session.
    pub fn create_volume(&mut self, initial: Value) -> Result<ReasoningVolume, Error> {
        create_reasoning_volume(self.host, initial)
    }
}

impl Default for AssemblerBuilder {
    fn default() -> Self {
        let diagnostics = DiagnosticBus::new();
        let host = Arc::new(AssemblerReflectionHost::new_unsealed(
            allocate_reasoning_session_id(),
            diagnostics.clone(),
            Arc::new(ExactConflictAnalysis),
        ));
        Self {
            source_system: Arc::new(FileSourceSystem::default()),
            runtime: EvaluationRuntime::new(0)
                .expect("zero-worker evaluation runtime must be constructible"),
            host,
            diagnostics,
            reflection_environment: None,
            diagnostic_attachments: Vec::new(),
            pending_diagnostics: Vec::new(),
        }
    }
}

impl AssemblerBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn source_system(mut self, source_system: impl SourceSystem + 'static) -> Self {
        self.source_system = Arc::new(source_system);
        self
    }

    /// Adapts the previous byte-host API to the artifact-oriented source API.
    pub fn host(self, host: impl Host + 'static) -> Self {
        self.source_system(HostSourceSystem::new(host))
    }

    pub fn evaluation_runtime(mut self, runtime: EvaluationRuntime) -> Self {
        self.runtime = runtime;
        self
    }

    pub fn conflict_analysis(self, strategy: Arc<dyn ConflictAnalysisStrategy>) -> Self {
        self.host.set_conflict_analysis(strategy);
        self
    }

    pub fn diagnostic_subscriber(
        mut self,
        subscriber: impl DiagnosticSubscriber + 'static,
    ) -> Self {
        let subscriber: Arc<dyn DiagnosticSubscriber> = Arc::new(subscriber);
        let subscription = self.diagnostics.subscribe_shared(subscriber.clone());
        self.diagnostic_attachments.push(DiagnosticAttachment {
            _subscription: subscription,
        });
        self
    }

    pub fn diagnostic_callback<F>(self, callback: F) -> Self
    where
        F: Fn(DiagnosticEvent) + Send + Sync + 'static,
    {
        self.diagnostic_subscriber(DiagnosticCallback(callback))
    }

    pub fn create_volume(&mut self, initial: Value) -> Result<ReasoningVolume, Error> {
        create_reasoning_volume(&self.host, initial)
    }

    /// Constructs the client portion of the reflection environment. The
    /// closure may create session-bound protected volumes before the session
    /// becomes runnable.
    pub fn reflection_environment<F>(mut self, build: F) -> Result<Self, Error>
    where
        F: FnOnce(&mut ReflectionEnvironmentBuilder<'_>) -> Result<Value, Error>,
    {
        if self.reflection_environment.is_some() {
            return Err(Error::new("reflection environment was already configured"));
        }
        let environment = build(&mut ReflectionEnvironmentBuilder { host: &self.host })?;
        let (environment, replaced_glam) =
            authoritative_reflection_environment(environment, "assembler")?;
        self.reflection_environment = Some(environment);
        if replaced_glam {
            self.pending_diagnostics.push(Diagnostic::new(
                Severity::Warning,
                "reflection environment namespace `glam` is reserved; supplied value was ignored",
            ));
        }
        Ok(self)
    }

    pub fn build(mut self) -> Result<Assembler, Error> {
        let environment = match self.reflection_environment.take() {
            Some(environment) => environment,
            None => authoritative_reflection_environment(Value::empty_record(), "assembler")?.0,
        };
        self.host.seal_environment(environment)?;
        let reasoning =
            ReasoningSession::from_host(self.host, self.diagnostics.clone(), self.runtime);
        for diagnostic in self.pending_diagnostics {
            self.diagnostics.publish(diagnostic);
        }
        Ok(Assembler {
            source_system: self.source_system,
            next_compilation_invocation: Arc::new(AtomicU64::new(1)),
            reasoning,
            diagnostic_attachments: self.diagnostic_attachments,
        })
    }
}

fn create_reasoning_volume(
    host: &Arc<AssemblerReflectionHost>,
    initial: Value,
) -> Result<ReasoningVolume, Error> {
    let (volume, effects) = host.create_volume(initial)?;
    Ok(ReasoningVolume {
        host: host.clone(),
        volume,
        effects,
    })
}

impl Default for Assembler {
    fn default() -> Self {
        AssemblerBuilder::default()
            .build()
            .expect("the default assembler must be constructible")
    }
}

impl Assembler {
    pub fn builder() -> AssemblerBuilder {
        AssemblerBuilder::new()
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a protected reflection volume initialized with `initial`.
    /// Possession of the returned Glam capability value is the authority to
    /// access it; ordinary `.heap.*` requests cannot address the volume.
    pub fn create_volume(&self, initial: Value) -> Result<ReasoningVolume, Error> {
        create_reasoning_volume(&self.reasoning.host, initial)
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
        self.reasoning.environment()
    }

    /// Returns this session environment with another authoritative reasoning
    /// role. Service sessions retain the client-provided environment while
    /// identifying themselves independently from the assembler session.
    pub fn reflection_environment_for_role(&self, role: impl AsRef<str>) -> Value {
        reflection_environment_for_role(&self.reasoning.environment(), role.as_ref())
    }

    /// Returns the shared execution resources used by this assembler and any
    /// service evaluation sessions explicitly attached to it.
    pub fn evaluation_runtime(&self) -> EvaluationRuntime {
        self.reasoning.runtime()
    }

    /// Returns the read-conflict strategy fixed for this reasoning session.
    pub fn conflict_analysis(&self) -> Arc<dyn ConflictAnalysisStrategy> {
        self.reasoning.conflict_analysis()
    }

    /// Returns this reasoning session's non-buffering diagnostic bus.
    pub fn diagnostic_bus(&self) -> DiagnosticBus {
        self.reasoning.diagnostics()
    }

    pub(crate) fn eval_context(&self) -> EvalContext {
        self.reasoning.eval_context()
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

    /// Installs another retained diagnostic subscription
    /// without rebuilding or otherwise disturbing its reasoning session.
    pub fn with_diagnostic_subscriber(
        mut self,
        subscriber: impl DiagnosticSubscriber + 'static,
    ) -> Self {
        let subscriber: Arc<dyn DiagnosticSubscriber> = Arc::new(subscriber);
        let subscription = self
            .reasoning
            .diagnostics()
            .subscribe_shared(subscriber.clone());
        self.diagnostic_attachments.push(DiagnosticAttachment {
            _subscription: subscription,
        });
        self
    }

    pub fn with_diagnostic_callback<F>(self, callback: F) -> Self
    where
        F: Fn(DiagnosticEvent) + Send + Sync + 'static,
    {
        self.with_diagnostic_subscriber(DiagnosticCallback(callback))
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

    pub(crate) fn record_diagnostic(&self, diagnostic: Diagnostic) {
        self.reasoning.diagnostics().publish(diagnostic);
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
        eval::eval_value(&self.eval_context(), value.as_core())
            .map(Value::from_core)
            .map_err(|error| Error::new(error.to_string()))
    }

    /// Applies all supplied arguments while preserving evaluator laziness.
    /// Call [`Self::evaluate`] when the result itself must be observed.
    pub fn apply(
        &self,
        function: &Value,
        arguments: impl IntoIterator<Item = Value>,
    ) -> Result<Value, Error> {
        eval::apply_values(
            &self.eval_context(),
            function.as_core().clone(),
            arguments.into_iter().map(Value::into_core).collect(),
        )
        .map(Value::from_core)
        .map_err(|error| Error::new(error.to_string()))
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
        self.core_value_at_path(root.as_core(), path)
            .map(Value::from_core)
    }

    pub fn to_binary(&self, value: &Value) -> Result<Bytes, Error> {
        self.core_value_bytes(value.as_core(), "value")
    }

    /// Extracts a byte range from compact binary data or a byte-valued list.
    /// Lazy list chunks are evaluated as required to locate the range.
    pub fn binary_slice(&self, value: &Value, range: Range<usize>) -> Result<Bytes, Error> {
        self.core_value_binary_slice(value.as_core(), range, "value")
    }

    pub fn binary_at(&self, root: &Value, path: &str) -> Result<Bytes, Error> {
        self.core_value_at_path(root.as_core(), path)
            .and_then(|value| self.core_value_bytes(&value, path))
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
            definitions = compile_source(prepared.source.bytes(), &prepared.context);
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
                let source = Arc::new(
                    self.source_system
                        .load_top_level(path)
                        .map_err(|error| Error::new(error.to_string()))?,
                );
                let trace = Arc::new(CompilationTrace::root(
                    self.next_compilation_invocation(),
                    &source,
                    module_path.clone(),
                ));
                let had_errors = Arc::new(AtomicBool::new(false));
                let context = CompileContext::from_module_path(module_path.iter().cloned())
                    .with_importer_source(source.clone())
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
                    source,
                    context,
                    had_errors,
                })
            }
            ModuleInput::Script { extension, body } => {
                let label: Arc<str> = Arc::from(format!("<script.{extension}>"));
                let source = Arc::new(SourceArtifact::new(
                    body.clone(),
                    SourceIdentity::script(label, body.clone()),
                ));
                let trace = Arc::new(CompilationTrace::root(
                    self.next_compilation_invocation(),
                    &source,
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
                    source,
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
        let importer = args.importer_source.as_ref().ok_or_else(|| {
            format!(
                "local import `{}` cannot be loaded from a source without an import resolver",
                args.request.as_str()
            )
        })?;
        let source = Arc::new(
            importer
                .load_relative(&args.request)
                .map_err(|error| error.to_string())?,
        );
        let module_loader = self.module_loader(session.clone());
        let binary_loader = self.binary_loader();
        let had_errors = Arc::new(AtomicBool::new(false));
        let trace = match args.importer_trace {
            Some(parent) => Arc::new(CompilationTrace::imported(
                self.next_compilation_invocation(),
                &source,
                args.module_path.clone(),
                parent,
                Arc::from(args.request.as_str()),
                args.extends.clone(),
            )),
            None => Arc::new(CompilationTrace::root(
                self.next_compilation_invocation(),
                &source,
                args.module_path.clone(),
            )),
        };
        let context = CompileContext::from_module_path(args.module_path.iter().cloned())
            .with_importer_source(source.clone())
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
        let definitions = compile_source(source.bytes(), &context);

        if had_errors.load(Ordering::Relaxed) {
            Err(format!(
                "local import `{}` failed to compile",
                source.identity().label()
            ))
        } else {
            Ok(definitions)
        }
    }

    fn load_local_binary(&self, args: BinaryLoadArgs) -> Result<CoreValue, String> {
        let importer = args.importer_source.as_ref().ok_or_else(|| {
            format!(
                "binary import `{}` cannot be loaded from a source without an import resolver",
                args.request.as_str()
            )
        })?;
        importer
            .load_relative(&args.request)
            .map(|artifact| CoreValue::Binary(artifact.bytes().clone()))
            .map_err(|error| error.to_string())
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
            | CoreValue::PartialBuiltin(_)
            | CoreValue::Opaque(_) => Err(Error::new(format!("`{label}` is not binary text data"))),
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
            | CoreValue::PartialBuiltin(_)
            | CoreValue::Opaque(_) => {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_compilation_trace(source: &str) -> CompilationTrace {
        let source =
            SourceArtifact::new(Bytes::from_static(b"source"), SourceIdentity::file(source));
        CompilationTrace::root(
            CompilationInvocationId::new(1),
            &source,
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
    fn builder_seals_the_environment_into_one_reasoning_session() {
        let assembler = Assembler::builder()
            .reflection_environment(|_| {
                Ok(Value::record([("client", Value::text("new environment"))]))
            })
            .expect("configured environment should be valid");
        let assembler = assembler.build().expect("assembler should build");

        assert_eq!(
            assembler
                .get(&assembler.reflection_environment(), "client")
                .expect("configured environment should be installed")
                .as_binary(),
            Some(b"new environment".as_slice())
        );
    }

    #[test]
    fn builder_fixes_conflict_analysis_before_reasoning_starts() {
        let assembler = Assembler::builder()
            .conflict_analysis(Arc::new(crate::reflection::CoarseConflictAnalysis))
            .build()
            .expect("assembler should build");

        assert_eq!(assembler.conflict_analysis().name(), "coarse");
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
    fn reflection_annotation_logs_use_the_assembler_diagnostic_bus() {
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
    fn failed_reflection_branch_does_not_publish_its_diagnostic() {
        let assembler = Assembler::new();
        let module = assembler
            .module(["annotation_test"])
            .script(
                "g",
                "language g0\nimport 'std\neffect = .cut (.alt ((.log 'error { msg:{ text:\"discarded\" } }) =>> .fail) (.r ()))\nresult = anno { refl:effect } \"ready\"\n",
            )
            .build()
            .expect("reflection annotation fixture should compile");

        assert_eq!(
            assembler
                .binary_at(module.value(), "result")
                .expect("winning reflection branch should complete"),
            b"ready".as_slice()
        );
        assert_eq!(assembler.diagnostic_bus().counts().total(), 0);
    }

    #[test]
    fn diagnostic_bus_sequences_counts_and_delivers_only_to_current_subscribers() {
        let bus = DiagnosticBus::new();
        assert_eq!(bus.counts().latest_sequence(), 0);
        let early = Arc::new(Mutex::new(Vec::new()));
        let early_events = early.clone();
        let early_subscription = bus.subscribe(DiagnosticCallback(move |event| {
            early_events
                .lock()
                .expect("early diagnostic collector should not be poisoned")
                .push(event);
        }));

        let first = bus.publish(Diagnostic::new(Severity::Info, "first"));
        let late = Arc::new(Mutex::new(Vec::new()));
        let late_events = late.clone();
        let _late_subscription = bus.subscribe(DiagnosticCallback(move |event| {
            late_events
                .lock()
                .expect("late diagnostic collector should not be poisoned")
                .push(event);
        }));
        let second = bus.publish(Diagnostic::new(Severity::Warning, "second"));
        drop(early_subscription);
        let third = bus.publish(Diagnostic::new(Severity::Error, "third"));

        assert_eq!(first.sequence(), 1);
        assert_eq!(second.sequence(), 2);
        assert_eq!(third.sequence(), 3);
        assert_eq!(
            bus.counts(),
            DiagnosticCounts {
                next_sequence: 4,
                info: 1,
                warnings: 1,
                errors: 1,
            }
        );

        let early = early
            .lock()
            .expect("early diagnostic collector should not be poisoned");
        assert_eq!(early.len(), 2);
        assert_eq!(early[0].message(), "first");
        assert_eq!(early[1].message(), "second");
        let late = late
            .lock()
            .expect("late diagnostic collector should not be poisoned");
        assert_eq!(
            late.iter()
                .map(|event| (event.sequence(), event.message()))
                .collect::<Vec<_>>(),
            [(2, "second"), (3, "third")]
        );
    }

    #[test]
    fn diagnostic_callback_subscribes_to_the_existing_session() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let callback_values = received.clone();
        let assembler = Assembler::default().with_diagnostic_callback(move |diagnostic| {
            callback_values
                .lock()
                .expect("callback collection mutex should not be poisoned")
                .push(diagnostic);
        });

        assembler.record_diagnostic(Diagnostic::new(Severity::Info, "hello"));

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
