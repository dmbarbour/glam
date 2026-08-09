//! Stable embedding-oriented facade for assembling modules and observing values.
//!
//! This module owns staged source/reasoning construction and orchestration. Front-end syntax,
//! core values, evaluator topology, and interaction-net scheduling remain
//! implementation details behind the facade.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroU64;
use std::ops::{Deref, Range};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Condvar;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use bytes::Bytes;
use rpds::RedBlackTreeMapSync;

use crate::compiler::{
    BinaryFileLoader, BinaryLoadArgs, CompileContext, CompileDiagnosticEmitter, ModuleLoadArgs,
    ModuleLoader, import_failure,
};
use crate::core::Value as CoreValue;
use crate::core::{
    Builtin, CoreValueFactory, Dict, EvaluationFailure, EvaluationHalt, Key, List, NetValue,
    PromisedValue,
};
use crate::core_net::{CoreDataKey, CoreSpecialization};
use crate::diagnostic::{CompilationInvocationId, CompilationTrace, Severity};
use crate::eval;
use crate::evaluation::{
    EvalContext, EvaluationExecutor, EvaluationSession, EvaluationSessionId, EvaluationSessionRun,
    EvaluationTaskId, EvaluationUnfinishedState, EvaluationWorkCoordinator, ExitIntent,
    ReflectionTaskProfile, RuntimeCoordinatorReadiness, RuntimeDeadlockWorkSnapshot,
    RuntimeDependencySnapshot, RuntimeExitSnapshot, RuntimeObservationEpoch,
    RuntimeObservationState, RuntimeWorkKindSnapshot, RuntimeWorkStateSnapshot,
};
use crate::g_syntax::compile_source;
use crate::interaction_net::{NetBuildError, NetBuilder as CoreNetBuilder, Port as CorePort};
use crate::number::Number;
use crate::reflection::{
    CommitResult, ConflictAddress, ConflictAnalysisStrategy, ConflictObservationIndex,
    ExactConflictAnalysis, HostSnapshot, ReasoningSessionId, ReflectionEffects,
    ReflectionQueryMutation, ReflectionQueryWriter, ReflectionServices, ReflectionStore,
    RuntimeInputEndpointId, RuntimeInputSequence, TaskCommit, TaskEnvironment, TaskHost, VolumeId,
    task_launcher, volume_effects,
};
use crate::runtime::{
    EvaluationRuntimeId, RuntimeIds, RuntimeMutationAdmission, RuntimeMutationGuard,
    RuntimeSettlementGuard, RuntimeValueRoot, allocate_evaluation_runtime_id,
};
use crate::source::{
    FileSourceSystem, Host, HostSourceSystem, SourceArtifact, SourceIdentity, SourceSystem,
};

const GLAM_COMPATIBILITY_VERSION: &str = "0.1.0";
const IMPLEMENTATION_NAME: &str = "rust-bootstrap";

/// Runtime-local identity of one buffered-output endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeOutputEndpointId(NonZeroU64);

impl RuntimeOutputEndpointId {
    pub fn get(self) -> u64 {
        self.0.get()
    }

    fn from_u64(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }
}

/// Runtime-local identity of one accepted output-delivery obligation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeDeliveryId(NonZeroU64);

impl RuntimeDeliveryId {
    pub fn get(self) -> u64 {
        self.0.get()
    }

    fn from_u64(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }
}

/// An assembly-time value rooted in exactly one [`EvaluationRuntime`].
///
/// Values cannot be transferred between runtimes. Construct them through
/// [`Values`], obtained from the target runtime or assembler.
#[derive(Clone, PartialEq, Eq)]
pub struct Value(RuntimeValueRoot);

/// Runtime-selected construction service for Glam values.
///
/// Every value produced here carries this factory's runtime provenance.
/// Composite constructors reject members from another runtime instead of
/// implicitly adopting them.
#[derive(Clone)]
pub struct Values {
    runtime: EvaluationRuntimeId,
    core: CoreValueFactory,
}

impl Values {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub(crate) fn core(&self) -> &CoreValueFactory {
        &self.core
    }

    fn wrap(&self, value: CoreValue) -> Value {
        debug_assert_eq!(self.runtime, self.core.runtime_id());
        Value(RuntimeValueRoot::new(&self.core, value))
    }

    fn require(&self, value: &Value) -> Result<(), Error> {
        value.require_runtime(self.runtime)
    }

    pub fn binary(&self, bytes: impl Into<Bytes>) -> Value {
        self.wrap(CoreValue::Binary(bytes.into()))
    }

    pub fn text(&self, text: impl AsRef<str>) -> Value {
        self.wrap(CoreValue::binary_from_text(text.as_ref()))
    }

    pub fn atom_from_text(&self, text: impl AsRef<str>) -> Value {
        let key = Key::binary_from_text(text.as_ref());
        self.wrap(CoreValue::Atom(crate::core::Atom::from_key(&key)))
    }

    pub fn integer(&self, value: i64) -> Value {
        self.wrap(CoreValue::Number(Number::integer(value)))
    }

    pub fn rational(&self, numerator: i64, denominator: i64) -> Option<Value> {
        Number::from_ratio_i64(numerator, denominator)
            .map(|number| self.wrap(CoreValue::Number(number)))
    }

    pub fn number_from_f64(&self, value: f64) -> Option<Value> {
        Number::from_f64(value).map(|number| self.wrap(CoreValue::Number(number)))
    }

    pub fn number_from_text(&self, text: impl AsRef<str>) -> Result<Value, Error> {
        Number::parse(text.as_ref())
            .map(|number| self.wrap(CoreValue::Number(number)))
            .map_err(Error::new)
    }

    pub fn list(&self, values: impl IntoIterator<Item = Value>) -> Result<Value, Error> {
        let values = values
            .into_iter()
            .map(|value| {
                self.require(&value)?;
                Ok(value.into_core())
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(self.wrap(CoreValue::List(List::from_values(values))))
    }

    pub fn record<I, S>(&self, entries: I) -> Result<Value, Error>
    where
        I: IntoIterator<Item = (S, Value)>,
        S: AsRef<str>,
    {
        let mut dict = Dict::new_sync();
        for (name, value) in entries {
            self.require(&value)?;
            dict = dict.insert(Key::atom_from_text(name), value.into_core());
        }
        Ok(self.wrap(CoreValue::Dict(dict)))
    }

    pub fn dictionary(
        &self,
        entries: impl IntoIterator<Item = (Value, Value)>,
    ) -> Result<Value, Error> {
        let mut dict = Dict::new_sync();
        for (key, value) in entries {
            self.require(&key)?;
            self.require(&value)?;
            let key = Key::from_value(key.as_core())
                .ok_or_else(|| Error::new("dictionary key is not immediately keyable"))?;
            dict = dict.insert(key, value.into_core());
        }
        Ok(self.wrap(CoreValue::Dict(dict)))
    }

    pub fn empty_record(&self) -> Value {
        self.wrap(CoreValue::Dict(Dict::new_sync()))
    }

    /// Constructs the ordinary lazy `base.[key]` semantic accessor.
    ///
    /// Neither operand is evaluated while constructing the value. Demand on
    /// the returned value evaluates the key and follows the same dictionary
    /// access semantics as `.g` source, including returning `{}` for a
    /// missing key.
    pub fn access(&self, base: &Value, key: Value) -> Result<Value, Error> {
        self.require(base)?;
        self.require(&key)?;
        Ok(
            self.wrap(CoreValue::Lazy(crate::core::LazyValue::from_access(
                &self.core,
                Arc::from([CoreDataKey::Index]),
                Arc::from([base.as_core().clone(), key.into_core()]),
            ))),
        )
    }

    /// Constructs the ordinary lazy `anno Annotation Target` semantic value.
    ///
    /// Annotation interpretation occurs only when the returned value is
    /// demanded; this method does not provide a separate host-side annotation
    /// interpreter.
    pub fn annotate(&self, annotation: Value, target: Value) -> Result<Value, Error> {
        self.require(&annotation)?;
        self.require(&target)?;
        Ok(self.wrap(CoreValue::builtin_call(
            &self.core,
            Builtin::Anno,
            vec![annotation.into_core(), target.into_core()],
        )))
    }

    pub fn empty_object(&self, name: Value) -> Result<Value, Error> {
        self.require(&name)?;
        let spec = CoreValue::Dict(
            Dict::new_sync()
                .insert(Key::atom_from_text("name"), name.into_core())
                .insert(
                    Key::atom_from_text("deps"),
                    CoreValue::List(List::from_values(Vec::new())),
                )
                .insert(
                    Key::atom_from_text("defs"),
                    CoreValue::Builtin(Builtin::ObjectDefaultDefs),
                ),
        );
        Ok(self.wrap(CoreValue::builtin_call(
            &self.core,
            Builtin::ObjectInstance,
            vec![spec],
        )))
    }

    pub fn after_reflection(&self, effect: Value, target: Value) -> Result<Value, Error> {
        self.require(&effect)?;
        self.require(&target)?;
        let annotation = CoreValue::Dict(
            Dict::new_sync().insert(Key::atom_from_text("refl"), effect.into_core()),
        );
        Ok(self.wrap(CoreValue::builtin_call(
            &self.core,
            Builtin::Anno,
            vec![annotation, target.into_core()],
        )))
    }

    pub fn abstract_global_path<I, S>(&self, parts: I) -> Value
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.wrap(CoreValue::Atom(crate::core::Atom::from_key(
            &Key::abstract_global_path(parts),
        )))
    }
}

impl Value {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.0.runtime_id()
    }

    fn require_runtime(&self, runtime: EvaluationRuntimeId) -> Result<(), Error> {
        if self.runtime_id() == runtime {
            Ok(())
        } else {
            Err(Error::new(format!(
                "value belongs to evaluation runtime {}, expected evaluation runtime {}",
                self.runtime_id().get(),
                runtime.get()
            )))
        }
    }

    pub fn is_undefined(&self) -> bool {
        matches!(self.0.as_core(), CoreValue::Dict(dict) if dict.is_empty())
    }

    pub fn kind(&self) -> ValueKind {
        match self.0.as_core() {
            CoreValue::Atom(_) => ValueKind::Atom,
            CoreValue::Number(_) => ValueKind::Number,
            CoreValue::Binary(_) => ValueKind::Binary,
            CoreValue::List(_) => ValueKind::List,
            CoreValue::Dict(_) => ValueKind::Dict,
            CoreValue::Builtin(_) | CoreValue::PartialBuiltin(_) | CoreValue::Function(_) => {
                ValueKind::Function
            }
            CoreValue::Net(_) => ValueKind::Net,
            CoreValue::Lazy(_) | CoreValue::Promised(_) => ValueKind::Lazy,
            CoreValue::Metadata(_) => ValueKind::Sealed,
            CoreValue::Opaque(_) => ValueKind::Opaque,
        }
    }

    pub fn as_binary(&self) -> Option<&[u8]> {
        match self.0.as_core() {
            CoreValue::Binary(bytes) => Some(bytes.as_ref()),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self.0.as_core() {
            CoreValue::Number(number) => number.to_i64_if_integer(),
            _ => None,
        }
    }

    pub fn as_rational_i64(&self) -> Option<(i64, i64)> {
        match self.0.as_core() {
            CoreValue::Number(number) => number.to_ratio_i64(),
            _ => None,
        }
    }

    /// Converts a number lossily to a finite `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        match self.0.as_core() {
            CoreValue::Number(number) => number.to_f64(),
            _ => None,
        }
    }

    /// Returns the canonical exact integer or `numerator/denominator` text.
    pub fn as_number_text(&self) -> Option<String> {
        match self.0.as_core() {
            CoreValue::Number(number) => Some(number.to_string()),
            _ => None,
        }
    }

    pub(crate) fn from_core(values: &CoreValueFactory, value: CoreValue) -> Self {
        Self::from_runtime(values.runtime_id(), value)
    }

    fn from_runtime(runtime: EvaluationRuntimeId, value: CoreValue) -> Self {
        Self(RuntimeValueRoot::from_runtime(runtime, value))
    }

    pub(crate) fn as_core(&self) -> &CoreValue {
        self.0.as_core()
    }

    pub(crate) fn into_core(self) -> CoreValue {
        self.0.into_core()
    }
}

/// The unique host capability for completing one promised [`Value`].
///
/// This handle is affine: it cannot be cloned and is consumed by
/// [`resolve`](Self::resolve), [`fail`](Self::fail), or
/// [`fail_message`](Self::fail_message). Dropping it unresolved permanently
/// fails the promised value.
///
/// Completion wakes every same-runtime work item currently blocked on the
/// unresolved promise. Sharing the value and registering that exact wake do
/// not keep an observing session alive.
///
/// The resolver is consumed by every terminal operation, so attempting to
/// complete the same public promise twice is a compile-time error:
///
/// ```compile_fail
/// let assembler = glam::Assembler::default();
/// let (_, resolver) = assembler.promise("host input");
/// resolver.resolve(assembler.values().integer(1)).unwrap();
/// resolver.fail_message("too late").unwrap();
/// ```
#[must_use = "dropping an unresolved promise resolver fails its value"]
pub struct PromiseResolver {
    runtime: EvaluationRuntimeId,
    promise: Option<PromisedValue>,
}

impl PromiseResolver {
    /// Completes the promise successfully with `value`.
    pub fn resolve(mut self, value: Value) -> Result<(), Error> {
        if let Err(error) = value.require_runtime(self.runtime) {
            self.promise.take();
            return Err(error);
        }
        let promise = self
            .promise
            .take()
            .expect("a live promise resolver must retain its promise");
        let label = promise.label().clone();
        promise
            .set(value.into_core())
            .map_err(|_| Error::new(format!("promise `{label}` was already completed")))?;
        Ok(())
    }

    /// Completes the promise with an arbitrary Glam value as its permanent
    /// producer error.
    pub fn fail(self, failure: Value) -> Result<(), Error> {
        let mut resolver = self;
        if let Err(error) = failure.require_runtime(resolver.runtime) {
            resolver.promise.take();
            return Err(error);
        }
        resolver.fail_with(Arc::new(EvaluationFailure::emission(failure.into_core())))
    }

    /// Completes the promise with a conventional textual producer error.
    pub fn fail_message(self, message: impl Into<Arc<str>>) -> Result<(), Error> {
        self.fail_with(Arc::new(EvaluationFailure::message(message.into())))
    }

    fn fail_with(mut self, failure: Arc<EvaluationFailure>) -> Result<(), Error> {
        let promise = self
            .promise
            .take()
            .expect("a live promise resolver must retain its promise");
        let label = promise.label().clone();
        promise
            .fail(failure)
            .map_err(|_| Error::new(format!("promise `{label}` was already completed")))?;
        Ok(())
    }
}

impl Drop for PromiseResolver {
    fn drop(&mut self) {
        let Some(promise) = self.promise.take() else {
            return;
        };
        let message = format!(
            "promise resolver for `{}` was dropped before completion",
            promise.label()
        );
        let _ = promise.fail_message(message);
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
    Sealed,
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
    values: Values,
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

    pub fn data(&mut self, value: Value) -> Result<NetPort<'net>, Error> {
        self.values.require(&value)?;
        let port = self.builder.data(value.into_core());
        Ok(self.port(port))
    }

    pub fn wire(&mut self, left: NetPort<'net>, right: NetPort<'net>) -> Result<(), Error> {
        self.builder
            .try_wire(left.port, right.port)
            .map_err(net_build_error)
    }

    fn new(values: Values) -> Self {
        Self {
            values,
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
    pub fn new(values: &Values, severity: Severity, message: impl Into<Arc<str>>) -> Self {
        Self::new_with_factory(values.core(), severity, message)
    }

    pub(crate) fn new_with_factory(
        values: &CoreValueFactory,
        severity: Severity,
        message: impl Into<Arc<str>>,
    ) -> Self {
        let message = message.into();
        Self::from_parts(
            values.runtime_id(),
            None,
            severity,
            crate::diagnostic::text_message(None, &message),
            None,
        )
    }

    /// Wraps an arbitrary diagnostic value with separately supplied severity.
    /// Assembler and viewer metadata remain unapplied until enrichment.
    pub fn from_emission(severity: Severity, emission: Value) -> Self {
        let runtime = emission.runtime_id();
        Self::from_parts(runtime, None, severity, emission.into_core(), None)
    }

    pub fn with_source_location(self, source: impl Into<Arc<str>>, line: usize) -> Self {
        let source = source.into();
        let identity = SourceIdentity::file(Path::new(source.as_ref()));
        let origin = CoreValue::Dict(
            Dict::new_sync().insert((*crate::core::keys::SOURCE).clone(), identity.value()),
        );
        Self::from_parts(
            self.emission.runtime_id(),
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
    pub fn enrich(&self, values: &Values) -> Result<Value, Error> {
        self.emission.require_runtime(values.runtime)?;
        self.enrich_with_factory(&values.core)
    }

    pub(crate) fn enrich_with_factory(&self, values: &CoreValueFactory) -> Result<Value, Error> {
        crate::diagnostic::enrich(
            values,
            self.emission.as_core().clone(),
            self.severity,
            self.origin.as_ref().map(|origin| origin.as_core().clone()),
        )
        .map(|value| Value::from_core(values, value))
        .map_err(|error| Error::from_eval(values, error))
    }

    /// Applies assembler metadata followed by observer-specific object updates.
    /// The raw emission and other enriched views remain unchanged.
    pub fn enrich_with(&self, values: &Values, updates: Value) -> Result<Value, Error> {
        updates.require_runtime(values.runtime)?;
        let enriched = self.enrich(values)?;
        crate::diagnostic::apply_updates(&values.core, enriched.into_core(), updates.into_core())
            .map(|value| Value::from_core(&values.core, value))
            .map_err(|error| Error::from_eval(&values.core, error))
    }

    /// Applies observer-owned updates to an arbitrary diagnostic-style value.
    ///
    /// Unlike [`Self::enrich`] and [`Self::enrich_with`], this does not inject
    /// assembler severity or origin metadata. This supports recursive context
    /// messages whose enrichment policy belongs entirely to the observer.
    pub fn apply_updates(values: &Values, message: &Value, updates: Value) -> Result<Value, Error> {
        message.require_runtime(values.runtime)?;
        updates.require_runtime(values.runtime)?;
        crate::diagnostic::apply_emission_updates(
            &values.core,
            message.as_core().clone(),
            updates.into_core(),
        )
        .map(|value| Value::from_core(&values.core, value))
        .map_err(|error| Error::from_eval(&values.core, error))
    }

    /// Prepends one structured frame describing why this diagnostic was
    /// produced or propagated. The original emission remains otherwise
    /// unchanged.
    pub fn with_context(self, context: Value) -> Result<Self, Error> {
        context.require_runtime(self.emission.runtime_id())?;
        let emission = crate::diagnostic::prepend_context(
            self.emission.as_core().clone(),
            context.into_core(),
        )
        .unwrap_or_else(|_| self.emission.as_core().clone());
        Ok(Self::from_parts(
            self.emission.runtime_id(),
            self.source,
            self.severity,
            emission,
            self.origin.map(Value::into_core),
        ))
    }

    /// Encodes this Rust envelope as one runtime-local value for buffered
    /// transport. The configured logger decodes the envelope before applying
    /// its own enrichment and viewing policy.
    #[doc(hidden)]
    pub fn transport_value(&self, values: &Values) -> Result<Value, Error> {
        self.emission.require_runtime(values.runtime)?;
        if let Some(origin) = &self.origin {
            origin.require_runtime(values.runtime)?;
        }
        let mut fields = Dict::new_sync()
            .insert(
                Key::atom_from_text("emission"),
                self.emission.as_core().clone(),
            )
            .insert(
                Key::atom_from_text("severity"),
                self.severity.value(values.core()),
            );
        if let Some(origin) = &self.origin {
            fields = fields.insert(Key::atom_from_text("origin"), origin.as_core().clone());
        }
        if let Some(source) = &self.source {
            fields = fields.insert(
                Key::atom_from_text("source"),
                CoreValue::binary_from_text(source.as_ref()),
            );
        }
        if let Some(line) = self.line {
            fields = fields.insert(
                Key::atom_from_text("line"),
                CoreValue::Number(Number::integer(line as i64)),
            );
        }
        Ok(values.wrap(CoreValue::Dict(fields)))
    }

    #[doc(hidden)]
    pub fn from_transport_value(value: &Value) -> Result<Self, Error> {
        let runtime = value.runtime_id();
        let ValueKind::Dict = value.kind() else {
            return Err(Error::new("diagnostic transport requires a dictionary"));
        };
        let field = |name: &str| {
            let CoreValue::Dict(fields) = value.as_core() else {
                unreachable!()
            };
            fields
                .get(&Key::atom_from_text(name))
                .cloned()
                .map(|field| Value::from_runtime(runtime, field))
        };
        let emission = field("emission")
            .ok_or_else(|| Error::new("diagnostic transport is missing `emission`"))?;
        let severity = match field("severity").and_then(|value| Key::from_value(value.as_core())) {
            Some(value) if value == *crate::core::keys::INFO => Severity::Info,
            Some(value) if value == *crate::core::keys::WARN => Severity::Warning,
            Some(value) if value == *crate::core::keys::ERROR => Severity::Error,
            _ => return Err(Error::new("diagnostic transport has an invalid severity")),
        };
        let source = field("source")
            .map(|source| {
                source
                    .as_binary()
                    .and_then(|source| std::str::from_utf8(source).ok())
                    .map(Arc::<str>::from)
                    .ok_or_else(|| Error::new("diagnostic transport source must be text"))
            })
            .transpose()?;
        let line = field("line")
            .map(|line| {
                line.as_i64()
                    .and_then(|line| usize::try_from(line).ok())
                    .ok_or_else(|| Error::new("diagnostic transport line must be nonnegative"))
            })
            .transpose()?;
        let origin = field("origin");
        let (projected_line, message) = crate::diagnostic::conventional_summary(emission.as_core());
        Ok(Self {
            emission,
            origin,
            source,
            severity,
            line: line.or(projected_line),
            message: message
                .unwrap_or_else(|| Arc::from("<diagnostic has no immediate text view>")),
        })
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

    fn from_compile(
        values: &CoreValueFactory,
        trace: &CompilationTrace,
        severity: Severity,
        message: CoreValue,
    ) -> Self {
        Self::from_parts(
            values.runtime_id(),
            Some(Arc::from(trace.source_label())),
            severity,
            message,
            Some(trace.origin_value()),
        )
    }

    fn from_parts(
        runtime: EvaluationRuntimeId,
        source: Option<Arc<str>>,
        severity: Severity,
        message: CoreValue,
        origin: Option<CoreValue>,
    ) -> Self {
        let (line, text) = crate::diagnostic::conventional_summary(&message);
        Self {
            emission: Value::from_runtime(runtime, message),
            origin: origin.map(|origin| Value::from_runtime(runtime, origin)),
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
    runtime: Option<EvaluationRuntimeId>,
    ingress: Option<Weak<DiagnosticIngressInner>>,
    ingress_installed: bool,
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
                    runtime: None,
                    ingress: None,
                    ingress_installed: false,
                }),
            }),
        }
    }

    /// Constructs a diagnostic bus whose values belong to `runtime`.
    pub fn for_runtime(runtime: &EvaluationRuntime) -> Self {
        let bus = Self::new();
        bus.bind_runtime(runtime)
            .expect("a fresh diagnostic bus accepts its runtime");
        bus
    }

    /// Binds this bus to one evaluation runtime. Repeating the same binding is
    /// harmless; attempting to move a bus to another runtime is rejected.
    pub fn bind_runtime(&self, runtime: &EvaluationRuntime) -> Result<(), Error> {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("diagnostic bus mutex should not be poisoned");
        match state.runtime {
            Some(owner) if owner != runtime.id() => Err(Error::new(format!(
                "diagnostic bus belongs to evaluation runtime {}, not {}",
                owner.get(),
                runtime.id().get()
            ))),
            Some(_) => Ok(()),
            None => {
                state.runtime = Some(runtime.id());
                Ok(())
            }
        }
    }

    /// Installs the single ordered runtime ingress for this bus.
    ///
    /// The ingress receives publications before ordinary subscribers, admits
    /// only runtime-rooted values to its FIFO, and remains registered weakly so
    /// neither the bus nor an escaping handle keeps the runtime alive.
    pub fn diagnostic_ingress(
        &self,
        runtime: &EvaluationRuntime,
    ) -> Result<(DiagnosticIngress, RuntimeInputReader), Error> {
        self.bind_runtime(runtime)?;
        let mut state = self
            .inner
            .state
            .lock()
            .expect("diagnostic bus mutex should not be poisoned");
        if state.runtime != Some(runtime.id()) {
            return Err(Error::new(
                "diagnostic ingress runtime changed during setup",
            ));
        }
        if state.ingress_installed {
            return Err(Error::new("diagnostic bus already has a runtime ingress"));
        }
        // Setup holds the bus lock so no publication can obtain a sequence
        // between the baseline capture and ingress installation. Endpoint
        // registration invokes no host callback and no runtime path acquires
        // the bus in the opposite direction.
        let endpoint = runtime.input_endpoint::<Value, _>(Ok)?;
        let (sender, reader) = endpoint.into_parts();
        let ingress = DiagnosticIngress {
            inner: Arc::new(DiagnosticIngressInner {
                sender,
                values: runtime.values(),
                state: Mutex::new(DiagnosticIngressState {
                    next_sequence: state.counts.next_sequence,
                    pending: BTreeMap::new(),
                    failure: None,
                }),
            }),
        };
        state.ingress = Some(Arc::downgrade(&ingress.inner));
        state.ingress_installed = true;
        runtime
            .state
            .diagnostic_ingresses
            .lock()
            .expect("runtime diagnostic-ingress mutex should not be poisoned")
            .push(ingress.inner.clone());
        Ok((ingress, reader))
    }

    /// Publishes one event, updating authoritative counts before notifying the
    /// subscribers present at publication time. Subscriber calls occur outside
    /// the bus lock; sequence numbers, rather than callback completion order,
    /// define the order of concurrent publications.
    pub fn publish(&self, diagnostic: Diagnostic) -> Result<DiagnosticEvent, Error> {
        let runtime = diagnostic.emission.runtime_id();
        self.publish_validated(runtime, diagnostic)
    }

    fn publish_local(&self, diagnostic: Diagnostic) -> DiagnosticEvent {
        self.publish(diagnostic)
            .expect("diagnostic and bus must belong to the same evaluation runtime")
    }

    /// Publishes a diagnostic produced by runtime-owned work after checking
    /// both the stated runtime and the diagnostic value's provenance against
    /// this bus.
    pub fn publish_from_runtime(
        &self,
        runtime: EvaluationRuntimeId,
        diagnostic: Diagnostic,
    ) -> Result<DiagnosticEvent, Error> {
        if diagnostic.emission.runtime_id() != runtime {
            return Err(Error::new(format!(
                "diagnostic belongs to evaluation runtime {}, not {}",
                diagnostic.emission.runtime_id().get(),
                runtime.get()
            )));
        }
        self.publish_validated(runtime, diagnostic)
    }

    fn publish_validated(
        &self,
        runtime: EvaluationRuntimeId,
        diagnostic: Diagnostic,
    ) -> Result<DiagnosticEvent, Error> {
        let (event, ingress, subscribers) = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("diagnostic bus mutex should not be poisoned");
            match state.runtime {
                Some(owner) if owner != runtime => {
                    return Err(Error::new(format!(
                        "diagnostic bus belongs to evaluation runtime {}, not {}",
                        owner.get(),
                        runtime.get()
                    )));
                }
                Some(_) => {}
                None => state.runtime = Some(runtime),
            }
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
            let ingress = state.ingress.as_ref().and_then(Weak::upgrade);
            let subscribers = state.subscribers.values().cloned().collect::<Vec<_>>();
            (event, ingress, subscribers)
        };
        if let Some(ingress) = ingress {
            ingress.receive(event.clone());
        }
        for subscriber in subscribers {
            subscriber.receive(event.clone());
        }
        Ok(event)
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

struct DiagnosticIngressState {
    next_sequence: u64,
    pending: BTreeMap<u64, RuntimePreparedInput>,
    failure: Option<Error>,
}

struct DiagnosticIngressInner {
    sender: RuntimeInputSender<Value>,
    values: Values,
    state: Mutex<DiagnosticIngressState>,
}

impl DiagnosticIngressInner {
    fn receive(&self, event: DiagnosticEvent) {
        let sequence = event.sequence();
        let value = match event.diagnostic().transport_value(&self.values) {
            Ok(value) => value,
            Err(error) => {
                self.state
                    .lock()
                    .expect("diagnostic ingress mutex should not be poisoned")
                    .failure = Some(error);
                return;
            }
        };
        let prepared = match self.sender.prepare(value) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.state
                    .lock()
                    .expect("diagnostic ingress mutex should not be poisoned")
                    .failure = Some(error);
                return;
            }
        };
        let mut state = self
            .state
            .lock()
            .expect("diagnostic ingress mutex should not be poisoned");
        if state.failure.is_some() || sequence < state.next_sequence {
            return;
        }
        state.pending.insert(sequence, prepared);
        loop {
            let next = state.next_sequence;
            let Some(prepared) = state.pending.remove(&next) else {
                break;
            };
            match prepared.admit() {
                Ok(_) => {
                    state.next_sequence = next
                        .checked_add(1)
                        .expect("diagnostic sequence numbers exhausted");
                }
                Err(error) => {
                    state.failure = Some(error);
                    break;
                }
            }
        }
    }
}

/// Keeps one diagnostic bus routed to its runtime FIFO.
///
/// The bus retains this ingress weakly and the runtime retains its routing
/// state. Dropping this escaping handle therefore neither detaches nor permits
/// a replacement lifecycle.
#[derive(Clone)]
pub struct DiagnosticIngress {
    inner: Arc<DiagnosticIngressInner>,
}

impl DiagnosticIngress {
    /// Returns the first terminal admission failure, if the runtime vanished
    /// while a publication was being routed.
    pub fn failure(&self) -> Option<Error> {
        self.inner
            .state
            .lock()
            .expect("diagnostic ingress mutex should not be poisoned")
            .failure
            .clone()
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
    resources: Arc<RuntimeSharedResources>,
    reasoning_session: ReasoningSessionId,
    reflection_environment: OnceLock<RuntimeValueRoot>,
    diagnostics: DiagnosticBus,
}

/// Execution resources shared by every source and recursive import in one
/// top-level module build.
///
/// Macro lookup runs in the assembler reasoning session. Macro effects and
/// explicit reflection annotations run in a separate demand session on the
/// same runtime, sharing its reflection heap while retaining their own task
/// and diagnostic state.
pub(crate) struct CompilationExecution {
    lookup: EvalContext,
    macros: EvalContext,
    _macro_owner: Arc<EvaluationSession>,
    #[cfg(test)]
    macro_host: Arc<AssemblerReflectionHost>,
    macro_diagnostics: DiagnosticBus,
    _diagnostic_forwarder: DiagnosticSubscription,
}

impl CompilationExecution {
    fn new(
        reasoning: &ReasoningSession,
        build_diagnostics: Arc<Mutex<Vec<Diagnostic>>>,
    ) -> Result<Self, Error> {
        let diagnostics = DiagnosticBus::for_runtime(&reasoning.runtime());
        let host = Arc::new(AssemblerReflectionHost::new_unsealed(
            &reasoning.runtime(),
            diagnostics.clone(),
        ));
        host.seal_environment(reflection_environment_for_role(
            &reasoning.environment(),
            "macro",
        ))?;
        let task_profile = Arc::new(ReflectionTaskProfile::sealed(task_launcher(
            ReflectionEffects,
            host.clone(),
        )));
        let evaluation = reasoning.runtime.new_evaluation_session()?;

        let assembler_diagnostics = reasoning.diagnostics();
        let forwarder = diagnostics.subscribe(DiagnosticCallback(move |event: DiagnosticEvent| {
            let diagnostic = macro_reflection_diagnostic(event.diagnostic());
            build_diagnostics
                .lock()
                .expect("build diagnostic mutex should not be poisoned")
                .push(diagnostic.clone());
            assembler_diagnostics.publish_local(diagnostic);
        }));

        Ok(Self {
            lookup: reasoning.eval_context(),
            macros: EvalContext::patient_with_task_profile(&evaluation, task_profile),
            _macro_owner: evaluation,
            #[cfg(test)]
            macro_host: host,
            macro_diagnostics: diagnostics,
            _diagnostic_forwarder: forwarder,
        })
    }

    pub(crate) fn lookup_context(&self) -> &EvalContext {
        &self.lookup
    }

    pub(crate) fn macro_context(&self) -> &EvalContext {
        &self.macros
    }

    #[cfg(test)]
    pub(crate) fn macro_diagnostic_counts(&self) -> DiagnosticCounts {
        self.macro_diagnostics.counts()
    }

    #[cfg(test)]
    pub(crate) fn macro_heap(&self) -> Value {
        self.macro_host.resources.reflection_root()
    }

    fn drain(&self) -> bool {
        let values = self.macros.values();
        let run = self.macros.run_until_quiescent();
        let (kind, report) = match run {
            EvaluationSessionRun::Complete(report) => (None, report),
            EvaluationSessionRun::Quiescent(report) => (Some("became quiescent"), report),
            EvaluationSessionRun::Deadlocked(report) => (Some("deadlocked"), report),
        };
        for (task, error) in report.failures.iter() {
            self.macro_diagnostics
                .publish_local(Diagnostic::new_with_factory(
                    values,
                    Severity::Error,
                    format!("macro reflection task {} failed: {}", task.get(), error),
                ));
        }
        if let Some(kind) = kind {
            let mut details = Vec::new();
            for task in report.unfinished {
                let dependency = task
                    .dependency
                    .map(|dependency| format!(" waiting on task {}", dependency.get()))
                    .unwrap_or_default();
                details.push(format!(
                    "task {} is {:?}{dependency}",
                    task.task.get(),
                    task.state
                ));
            }
            self.macro_diagnostics
                .publish_local(Diagnostic::new_with_factory(
                    values,
                    Severity::Error,
                    format!(
                        "macro reflection scheduler {kind} with {} unfinished task{}{}",
                        details.len(),
                        if details.len() == 1 { "" } else { "s" },
                        if details.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", details.join("; "))
                        }
                    ),
                ));
        }
        self.macro_diagnostics.counts().errors() != 0
    }

    #[cfg(test)]
    pub(crate) fn drain_for_test(&self) -> bool {
        self.drain()
    }
}

fn macro_reflection_diagnostic(diagnostic: &Diagnostic) -> Diagnostic {
    let reasoning = CoreValue::Dict(Dict::new_sync().insert(
        Key::atom_from_text("role"),
        CoreValue::Atom(crate::core::Atom::from_key(&Key::binary_from_text("macro"))),
    ));
    let origin =
        CoreValue::Dict(Dict::new_sync().insert(Key::atom_from_text("reasoning"), reasoning));
    Diagnostic::from_parts(
        diagnostic.emission.runtime_id(),
        diagnostic.source.clone(),
        diagnostic.severity,
        diagnostic.emission.as_core().clone(),
        Some(origin),
    )
}

impl AssemblerReflectionHost {
    fn new_unsealed(runtime: &EvaluationRuntime, diagnostics: DiagnosticBus) -> Self {
        let resources = runtime.shared_resources();
        Self {
            reasoning_session: resources.allocate_reasoning_session_id(),
            resources,
            reflection_environment: OnceLock::new(),
            diagnostics,
        }
    }

    fn values(&self) -> Values {
        self.resources.values()
    }

    fn seal_environment(&self, environment: Value) -> Result<(), Error> {
        self.reflection_environment
            .set(self.resources.root_value(environment)?)
            .map_err(|_| Error::new("reflection environment was already configured"))
    }

    fn create_volume(&self, initial: Value) -> Result<(VolumeId, Value), Error> {
        let volume = self.resources.create_volume(initial)?;
        Ok((volume, volume_effects(self.resources.values.core(), volume)))
    }
}

impl TaskEnvironment for AssemblerReflectionHost {
    fn reflection_environment(&self) -> Value {
        self.reflection_environment
            .get()
            .expect("reasoning host must be sealed before it runs tasks")
            .value(self.resources.id)
    }
}

impl ReflectionServices for AssemblerReflectionHost {
    fn emit_diagnostic(&self, diagnostic: Diagnostic) {
        self.diagnostics.publish_local(diagnostic);
    }

    fn query_writer(&self) -> Option<Arc<dyn ReflectionQueryWriter>> {
        Some(self.resources.clone())
    }
}

impl ReflectionQueryWriter for RuntimeSharedResources {
    fn update_query_guarded(
        &self,
        mutation: ReflectionQueryMutation<'_, '_>,
        handle: &Arc<crate::reflection::EvaluationQueryHandle>,
        result: Value,
    ) -> Box<dyn FnOnce() + Send> {
        result
            .require_runtime(self.id)
            .expect("reflection query results belong to the runtime");
        let updated = self
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .reflection
            .update_query(handle, result);
        assert!(
            updated,
            "task status query must remain in its runtime domain"
        );

        let epoch = self.observations.advance();
        let work = self.work.upgrade();
        let scheduler_changed = work
            .as_ref()
            .map(|work| work.publish_runtime_observation_guarded(mutation.guard(), epoch));
        let observations = self.observations.clone();
        Box::new(move || {
            observations.notify_all();
            if let (Some(work), Some(changed)) = (work, scheduler_changed) {
                work.notify_runtime_observation(changed);
            }
        })
    }
}

impl TaskHost<ReflectionEffects> for AssemblerReflectionHost {
    fn snapshot(&self) -> HostSnapshot<ReflectionEffects> {
        let (generation, store) = self.resources.reflection_snapshot();
        HostSnapshot::new(generation, store, ())
    }

    fn commit(&self, commit: TaskCommit<ReflectionEffects>) -> CommitResult {
        let (store, _extra_snapshot, extra) = commit.into_parts();
        match self.resources.commit_reflection(&store) {
            crate::reflection::StoreCommitResult::Committed => {}
            crate::reflection::StoreCommitResult::Conflict => {
                return CommitResult::Conflict;
            }
            crate::reflection::StoreCommitResult::MissingVolume(volume) => {
                return CommitResult::MissingVolume(volume);
            }
        }
        let diagnostics = extra.diagnostics().to_vec();
        for diagnostic in diagnostics {
            self.diagnostics.publish_local(diagnostic);
        }
        extra.commit_updates();
        CommitResult::Committed
    }

    fn reasoning_session_id(&self) -> Option<ReasoningSessionId> {
        Some(self.reasoning_session)
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        self.resources.wait_for_change(observed_generation)
    }
}

/// Opaque background execution resources shared by related evaluation
/// sessions, including the assembler, logger, and future IDE services.
#[derive(Clone)]
pub struct EvaluationRuntime {
    state: Arc<RuntimeState>,
    default_reflection_profile: Arc<ReflectionTaskProfile>,
}

/// Stable, observational classification of one runtime instant.
#[doc(hidden)]
#[derive(Clone)]
pub enum RuntimeReadiness {
    Busy,
    Ready(QuiescenceSnapshot),
    Deadlocked(DeadlockSnapshot),
}

impl fmt::Debug for RuntimeReadiness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("Busy"),
            Self::Ready(snapshot) => formatter.debug_tuple("Ready").field(snapshot).finish(),
            Self::Deadlocked(snapshot) => {
                formatter.debug_tuple("Deadlocked").field(snapshot).finish()
            }
        }
    }
}

/// Authoritative revisions captured by a readiness probe.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeReadinessStamp {
    work_generation: u64,
    observation_epoch: u64,
}

impl RuntimeReadinessStamp {
    pub fn work_generation(&self) -> u64 {
        self.work_generation
    }

    pub fn observation_epoch(&self) -> u64 {
        self.observation_epoch
    }
}

/// One task disposition proposed by a stable exit vote.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeDisposition {
    work_id: u64,
    session_id: u64,
    task_id: u64,
    kind: RuntimeDispositionKind,
}

impl RuntimeDisposition {
    pub fn work_id(&self) -> u64 {
        self.work_id
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub fn task_id(&self) -> u64 {
        self.task_id
    }

    pub fn kind(&self) -> &RuntimeDispositionKind {
        &self.kind
    }
}

/// Payload of one proposed runtime disposition.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeDispositionKind {
    ExitSuccess,
    ExitError(Value),
}

/// Retained evidence that every unfinished participant voted to exit.
#[doc(hidden)]
#[derive(Clone)]
pub struct QuiescenceSnapshot {
    runtime: EvaluationRuntime,
    stamp: RuntimeReadinessStamp,
    dispositions: Vec<RuntimeDisposition>,
    reflection: crate::reflection::StoreSnapshot,
}

impl fmt::Debug for QuiescenceSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuiescenceSnapshot")
            .field("runtime", &self.runtime.id())
            .field("stamp", &self.stamp)
            .field("dispositions", &self.dispositions)
            .finish_non_exhaustive()
    }
}

impl QuiescenceSnapshot {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime.id()
    }

    pub fn stamp(&self) -> RuntimeReadinessStamp {
        self.stamp
    }

    pub fn dispositions(&self) -> &[RuntimeDisposition] {
        &self.dispositions
    }

    pub fn reflection(&self) -> &crate::reflection::StoreSnapshot {
        &self.reflection
    }
}

/// Kind of unfinished work retained in a deadlock report.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeWorkKind {
    ReflectionTask,
    DeferredEvaluation,
    ClientDemand,
    Spark,
}

/// Stable non-runnable state retained in a deadlock report.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeWorkState {
    Dormant,
    Reserved,
    Blocked,
}

/// Producer edge for one blocked runtime participant.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeDependency {
    TaskWait {
        wait_id: u64,
        task_id: u64,
        session_id: u64,
    },
    Promise {
        promise_id: u64,
        producer: Option<RuntimeTaskWait>,
    },
    Synthetic {
        id: u64,
    },
}

/// Task-producing wait attached to a promise dependency.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeTaskWait {
    wait_id: u64,
    task_id: u64,
    session_id: u64,
}

impl RuntimeTaskWait {
    pub fn wait_id(&self) -> u64 {
        self.wait_id
    }

    pub fn task_id(&self) -> u64 {
        self.task_id
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }
}

/// One unfinished participant retained by a deadlock snapshot.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeDeadlockWork {
    work_id: u64,
    session_id: u64,
    task_id: Option<u64>,
    kind: RuntimeWorkKind,
    state: RuntimeWorkState,
    dependency: Option<RuntimeDependency>,
    observed_epoch: Option<u64>,
}

impl RuntimeDeadlockWork {
    pub fn work_id(&self) -> u64 {
        self.work_id
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub fn task_id(&self) -> Option<u64> {
        self.task_id
    }

    pub fn kind(&self) -> RuntimeWorkKind {
        self.kind
    }

    pub fn state(&self) -> RuntimeWorkState {
        self.state
    }

    pub fn dependency(&self) -> Option<&RuntimeDependency> {
        self.dependency.as_ref()
    }

    pub fn observed_epoch(&self) -> Option<u64> {
        self.observed_epoch
    }
}

/// Retained stable evidence that at least one participant cannot progress.
#[doc(hidden)]
#[derive(Clone)]
pub struct DeadlockSnapshot {
    runtime: EvaluationRuntime,
    stamp: RuntimeReadinessStamp,
    dispositions: Vec<RuntimeDisposition>,
    unfinished: Vec<RuntimeDeadlockWork>,
    reflection: crate::reflection::StoreSnapshot,
}

impl fmt::Debug for DeadlockSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeadlockSnapshot")
            .field("runtime", &self.runtime.id())
            .field("stamp", &self.stamp)
            .field("dispositions", &self.dispositions)
            .field("unfinished", &self.unfinished)
            .finish_non_exhaustive()
    }
}

impl DeadlockSnapshot {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime.id()
    }

    pub fn stamp(&self) -> RuntimeReadinessStamp {
        self.stamp
    }

    pub fn dispositions(&self) -> &[RuntimeDisposition] {
        &self.dispositions
    }

    pub fn unfinished(&self) -> &[RuntimeDeadlockWork] {
        &self.unfinished
    }

    pub fn reflection(&self) -> &crate::reflection::StoreSnapshot {
        &self.reflection
    }
}

fn runtime_disposition_from_snapshot(snapshot: RuntimeExitSnapshot) -> RuntimeDisposition {
    RuntimeDisposition {
        work_id: snapshot.work.get(),
        session_id: snapshot.session.get(),
        task_id: snapshot.task.get(),
        kind: match snapshot.intent {
            ExitIntent::Success => RuntimeDispositionKind::ExitSuccess,
            ExitIntent::Error(message) => RuntimeDispositionKind::ExitError(Value(message)),
        },
    }
}

fn runtime_dependency_from_snapshot(snapshot: RuntimeDependencySnapshot) -> RuntimeDependency {
    match snapshot {
        RuntimeDependencySnapshot::Wait {
            wait,
            producer,
            session,
        } => RuntimeDependency::TaskWait {
            wait_id: wait,
            task_id: producer.get(),
            session_id: session.get(),
        },
        RuntimeDependencySnapshot::Promise { promise, producer } => RuntimeDependency::Promise {
            promise_id: promise,
            producer: producer.map(|(wait_id, task, session)| RuntimeTaskWait {
                wait_id,
                task_id: task.get(),
                session_id: session.get(),
            }),
        },
        #[cfg(test)]
        RuntimeDependencySnapshot::Test(id) => RuntimeDependency::Synthetic { id },
    }
}

fn runtime_deadlock_work_from_snapshot(
    snapshot: RuntimeDeadlockWorkSnapshot,
) -> RuntimeDeadlockWork {
    RuntimeDeadlockWork {
        work_id: snapshot.work.get(),
        session_id: snapshot.session.get(),
        task_id: snapshot.task.map(EvaluationTaskId::get),
        kind: match snapshot.kind {
            RuntimeWorkKindSnapshot::ReflectionTask => RuntimeWorkKind::ReflectionTask,
            RuntimeWorkKindSnapshot::DeferredEvaluation => RuntimeWorkKind::DeferredEvaluation,
            RuntimeWorkKindSnapshot::ClientDemand => RuntimeWorkKind::ClientDemand,
            RuntimeWorkKindSnapshot::Spark => RuntimeWorkKind::Spark,
        },
        state: match snapshot.state {
            RuntimeWorkStateSnapshot::Dormant => RuntimeWorkState::Dormant,
            RuntimeWorkStateSnapshot::Reserved => RuntimeWorkState::Reserved,
            RuntimeWorkStateSnapshot::Blocked => RuntimeWorkState::Blocked,
        },
        dependency: snapshot.dependency.map(runtime_dependency_from_snapshot),
        observed_epoch: snapshot.observed_epoch.map(RuntimeObservationEpoch::get),
    }
}

struct RuntimeState {
    executor: Arc<EvaluationExecutor>,
    work: Arc<EvaluationWorkCoordinator>,
    shared_resources: Arc<RuntimeSharedResources>,
    diagnostic_ingresses: Mutex<Vec<Arc<DiagnosticIngressInner>>>,
}

/// Acyclic runtime infrastructure needed by evaluation and reflection work.
///
/// The coordinator route is deliberately weak: retaining these resources must
/// not retain the runtime scheduler, executor, public runtime wrapper, or
/// default reflection profile.
#[doc(hidden)]
pub struct RuntimeSharedResources {
    id: EvaluationRuntimeId,
    values: RuntimeValueFactory,
    transactions: RuntimeTransactionState,
    observations: Arc<RuntimeObservationState>,
    ids: Arc<RuntimeIds>,
    mutation_admission: Arc<RuntimeMutationAdmission>,
    work: Weak<EvaluationWorkCoordinator>,
}

struct RuntimeTransactionState {
    state: Mutex<RuntimeTransactionData>,
}

struct RuntimeTransactionData {
    reflection: ReflectionStore,
    events: RuntimeEventState,
    logger_lifecycle: RuntimeLoggerLifecycleState,
}

#[derive(Clone)]
struct RuntimeInputRecord {
    sequence: RuntimeInputSequence,
    payload: RuntimeValueRoot,
}

#[derive(Clone)]
struct RuntimeInputBuffer {
    head_sequence: RuntimeInputSequence,
    next_sequence: RuntimeInputSequence,
    admitted: std::collections::VecDeque<RuntimeInputRecord>,
}

impl Default for RuntimeInputBuffer {
    fn default() -> Self {
        Self {
            head_sequence: RuntimeInputSequence::from_u64(0),
            next_sequence: RuntimeInputSequence::from_u64(0),
            admitted: std::collections::VecDeque::new(),
        }
    }
}

#[derive(Clone)]
struct RuntimeOutputIntent {
    delivery: RuntimeDeliveryId,
    endpoint: RuntimeOutputEndpointId,
    payload: RuntimeValueRoot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeDeliveryState {
    Queued,
    Running,
}

struct RuntimeDeliveryRecord {
    endpoint: RuntimeOutputEndpointId,
    payload: RuntimeValueRoot,
    state: RuntimeDeliveryState,
}

#[derive(Default)]
struct RuntimeOutputState {
    accepted: BTreeSet<RuntimeDeliveryId>,
    records: BTreeMap<RuntimeDeliveryId, RuntimeDeliveryRecord>,
    ready_by_endpoint:
        BTreeMap<RuntimeOutputEndpointId, std::collections::VecDeque<RuntimeDeliveryId>>,
    failures: RedBlackTreeMapSync<RuntimeDeliveryId, Arc<RuntimeDeliveryFailure>>,
}

/// Stage of external output delivery which failed after semantic commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeDeliveryFailureKind {
    Decode,
    Adapter,
    Panic,
}

/// Durable Rust-layer evidence for one failed external delivery.
#[derive(Clone, Debug)]
pub struct RuntimeDeliveryFailure {
    runtime: EvaluationRuntimeId,
    delivery: RuntimeDeliveryId,
    endpoint: RuntimeOutputEndpointId,
    kind: RuntimeDeliveryFailureKind,
    error: Error,
}

impl RuntimeDeliveryFailure {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub fn delivery_id(&self) -> RuntimeDeliveryId {
        self.delivery
    }

    pub fn endpoint_id(&self) -> RuntimeOutputEndpointId {
        self.endpoint
    }

    pub fn kind(&self) -> RuntimeDeliveryFailureKind {
        self.kind
    }

    pub fn error(&self) -> &Error {
        &self.error
    }
}

/// Persistent view of delivery failures retained at one instant.
#[derive(Clone)]
pub struct RuntimeDeliveryFailureSnapshot {
    runtime: EvaluationRuntimeId,
    failures: RedBlackTreeMapSync<RuntimeDeliveryId, Arc<RuntimeDeliveryFailure>>,
}

impl RuntimeDeliveryFailureSnapshot {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub fn is_empty(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn get(&self, delivery: RuntimeDeliveryId) -> Option<Arc<RuntimeDeliveryFailure>> {
        self.failures.get(&delivery).cloned()
    }

    pub fn failures(&self) -> Vec<Arc<RuntimeDeliveryFailure>> {
        self.failures
            .iter()
            .map(|(_, failure)| failure.clone())
            .collect()
    }
}

/// Terminal result of one claimed output delivery.
#[derive(Clone, Debug)]
pub enum RuntimeDeliveryOutcome {
    Delivered(RuntimeDeliveryId),
    Failed(Arc<RuntimeDeliveryFailure>),
}

struct RuntimeEventState {
    inputs: BTreeMap<RuntimeInputEndpointId, Arc<RuntimeInputBuffer>>,
    outputs: RuntimeOutputState,
    revision: u64,
    latest_changes: BTreeMap<ConflictAddress, u64>,
    strategy: Arc<dyn ConflictAnalysisStrategy>,
}

impl RuntimeEventState {
    fn new(strategy: Arc<dyn ConflictAnalysisStrategy>) -> Self {
        Self {
            inputs: BTreeMap::new(),
            outputs: RuntimeOutputState::default(),
            revision: 0,
            latest_changes: BTreeMap::new(),
            strategy,
        }
    }

    fn snapshot(&self, runtime: EvaluationRuntimeId) -> RuntimeEventSnapshot {
        RuntimeEventSnapshot {
            runtime,
            revision: self.revision,
            inputs: Arc::new(
                self.inputs
                    .iter()
                    .map(|(endpoint, input)| (*endpoint, input.clone()))
                    .collect(),
            ),
            strategy: self.strategy.clone(),
        }
    }

    fn conflicts(&self, journal: &RuntimeEventJournal) -> bool {
        self.latest_changes.iter().any(|(changed, revision)| {
            *revision > journal.snapshot.revision && journal.observations.may_conflict(changed)
        })
    }

    fn validate(&self, journal: &RuntimeEventJournal) -> bool {
        if journal.snapshot.runtime != journal.runtime {
            return false;
        }
        if self.conflicts(journal) {
            return false;
        }
        let inputs_valid = journal.cursors.iter().all(|(endpoint, cursor)| {
            if cursor.next == cursor.start {
                return true;
            }
            let Some(input) = self.inputs.get(endpoint) else {
                return false;
            };
            input.head_sequence == cursor.start
                && cursor.next.get().saturating_sub(cursor.start.get())
                    <= input.admitted.len() as u64
        });
        inputs_valid
            && journal.outputs.iter().all(|intent| {
                self.outputs
                    .ready_by_endpoint
                    .contains_key(&intent.endpoint)
                    && !self.outputs.accepted.contains(&intent.delivery)
            })
    }

    fn commit_validated(&mut self, journal: &RuntimeEventJournal) -> bool {
        let mut consumed = Vec::new();
        for (endpoint, cursor) in &journal.cursors {
            let count = cursor.next.get() - cursor.start.get();
            if count == 0 {
                continue;
            }
            let input = self
                .inputs
                .get_mut(endpoint)
                .expect("event validation retained every claimed endpoint");
            let input = Arc::make_mut(input);
            for _ in 0..count {
                let record = input
                    .admitted
                    .pop_front()
                    .expect("event validation retained every claimed input");
                debug_assert_eq!(record.sequence, input.head_sequence);
                consumed.push(ConflictAddress::input_slot(*endpoint, record.sequence));
                input.head_sequence = input
                    .head_sequence
                    .checked_next()
                    .expect("an admitted input always has a successor boundary");
            }
        }
        let input_changed = !consumed.is_empty();
        if input_changed {
            self.revision = self.revision.wrapping_add(1);
            for address in consumed {
                self.latest_changes.insert(address, self.revision);
            }
        }
        for intent in &journal.outputs {
            assert!(
                self.outputs.accepted.insert(intent.delivery),
                "validated delivery IDs remain unique"
            );
            let replaced = self.outputs.records.insert(
                intent.delivery,
                RuntimeDeliveryRecord {
                    endpoint: intent.endpoint,
                    payload: intent.payload.clone(),
                    state: RuntimeDeliveryState::Queued,
                },
            );
            assert!(replaced.is_none(), "validated delivery IDs remain unique");
            self.outputs
                .ready_by_endpoint
                .get_mut(&intent.endpoint)
                .expect("event validation retained every output endpoint")
                .push_back(intent.delivery);
        }
        input_changed || !journal.outputs.is_empty()
    }
}

/// Immutable admitted-input state captured with a reflection-store snapshot.
#[doc(hidden)]
#[derive(Clone)]
pub struct RuntimeEventSnapshot {
    runtime: EvaluationRuntimeId,
    revision: u64,
    inputs: Arc<BTreeMap<RuntimeInputEndpointId, Arc<RuntimeInputBuffer>>>,
    strategy: Arc<dyn ConflictAnalysisStrategy>,
}

#[derive(Clone)]
struct RuntimeInputCursor {
    start: RuntimeInputSequence,
    next: RuntimeInputSequence,
    claimed: Vec<RuntimeValueRoot>,
}

/// Transaction-local observations and FIFO input claims.
///
/// Dropping this value abandons every claim; input is removed only by a
/// successful combined runtime commit.
#[doc(hidden)]
#[derive(Clone)]
pub struct RuntimeEventJournal {
    runtime: EvaluationRuntimeId,
    snapshot: RuntimeEventSnapshot,
    observations: Box<dyn ConflictObservationIndex>,
    cursors: BTreeMap<RuntimeInputEndpointId, RuntimeInputCursor>,
    outputs: Vec<RuntimeOutputIntent>,
}

impl RuntimeEventJournal {
    #[doc(hidden)]
    pub fn new(snapshot: RuntimeEventSnapshot) -> Self {
        Self {
            runtime: snapshot.runtime,
            observations: snapshot.strategy.begin(),
            snapshot,
            cursors: BTreeMap::new(),
            outputs: Vec::new(),
        }
    }

    /// Reads and claims the next value from `input` in this transaction's
    /// frozen FIFO view. `None` is a precise observation of the next absent
    /// slot and does not advance the local cursor.
    pub fn read(&mut self, input: &RuntimeInputReader) -> Result<Option<Value>, Error> {
        input.validate_runtime(self.runtime)?;
        let snapshot = self
            .snapshot
            .inputs
            .get(&input.endpoint)
            .ok_or_else(|| Error::new("runtime input endpoint is not registered"))?;
        let cursor = self
            .cursors
            .entry(input.endpoint)
            .or_insert_with(|| RuntimeInputCursor {
                start: snapshot.head_sequence,
                next: snapshot.head_sequence,
                claimed: Vec::new(),
            });
        let address = ConflictAddress::input_slot(input.endpoint, cursor.next);
        self.observations.observe(&address);
        if cursor.next == snapshot.next_sequence {
            return Ok(None);
        }
        let offset = cursor.next.get() - snapshot.head_sequence.get();
        let record = snapshot
            .admitted
            .get(offset as usize)
            .expect("the snapshot sequence range and admitted roots agree");
        debug_assert_eq!(record.sequence, cursor.next);
        cursor.claimed.push(record.payload.clone());
        cursor.next = cursor
            .next
            .checked_next()
            .expect("an admitted input always has a successor boundary");
        Ok(Some(record.payload.value(self.runtime)))
    }

    /// Buffers an external output intent in this transaction. The delivery ID
    /// is reserved immediately and may be burned if the journal is abandoned.
    /// No output becomes visible before combined commit.
    pub fn write(
        &mut self,
        output: &RuntimeOutputWriter,
        value: Value,
    ) -> Result<RuntimeDeliveryId, Error> {
        let owner = output.validate_runtime(self.runtime)?;
        let id = owner.ids.delivery().map_err(Error::new)?;
        let delivery =
            RuntimeDeliveryId::from_u64(id.get()).expect("runtime delivery IDs start at one");
        self.outputs.push(RuntimeOutputIntent {
            delivery,
            endpoint: output.endpoint,
            payload: owner.values.root(value)?,
        });
        Ok(delivery)
    }
}

/// Runtime-bound transactional read authority for one admitted-input FIFO.
#[derive(Clone)]
pub struct RuntimeInputReader {
    runtime: EvaluationRuntimeId,
    owner: Weak<RuntimeSharedResources>,
    endpoint: RuntimeInputEndpointId,
}

impl RuntimeInputReader {
    pub fn id(&self) -> RuntimeInputEndpointId {
        self.endpoint
    }

    fn validate_runtime(&self, runtime: EvaluationRuntimeId) -> Result<(), Error> {
        if self.runtime != runtime {
            return Err(Error::new(format!(
                "input endpoint {} belongs to evaluation runtime {}, not {}",
                self.endpoint.get(),
                self.runtime.get(),
                runtime.get()
            )));
        }
        if self.owner.upgrade().is_none() {
            return Err(Error::new(format!(
                "evaluation runtime {} for input endpoint {} has been dropped",
                self.runtime.get(),
                self.endpoint.get()
            )));
        }
        Ok(())
    }
}

type RuntimeInputConverter<T> = dyn Fn(T) -> Result<Value, Error> + Send + Sync;

/// Typed host-side sender for one runtime input FIFO.
pub struct RuntimeInputSender<T> {
    runtime: EvaluationRuntimeId,
    owner: Weak<RuntimeSharedResources>,
    endpoint: RuntimeInputEndpointId,
    convert: Arc<RuntimeInputConverter<T>>,
    marker: PhantomData<fn(T)>,
}

impl<T> Clone for RuntimeInputSender<T> {
    fn clone(&self) -> Self {
        Self {
            runtime: self.runtime,
            owner: self.owner.clone(),
            endpoint: self.endpoint,
            convert: self.convert.clone(),
            marker: PhantomData,
        }
    }
}

impl<T> RuntimeInputSender<T> {
    pub fn id(&self) -> RuntimeInputEndpointId {
        self.endpoint
    }

    /// Converts and admits one host value. Conversion happens before runtime
    /// mutation admission, so failure publishes neither state nor a wake.
    pub fn admit(&self, input: T) -> Result<RuntimeInputSequence, Error> {
        self.prepare(input)?.admit()
    }

    /// Converts and roots one input without admitting it. This supports
    /// ordering adapters which must briefly retain out-of-order values without
    /// placing typed host payloads in runtime-owned state.
    fn prepare(&self, input: T) -> Result<RuntimePreparedInput, Error> {
        let owner = self.owner.upgrade().ok_or_else(|| {
            Error::new(format!(
                "evaluation runtime {} for input endpoint {} has been dropped",
                self.runtime.get(),
                self.endpoint.get()
            ))
        })?;
        let value = (self.convert)(input)?;
        Ok(RuntimePreparedInput {
            runtime: self.runtime,
            owner: Arc::downgrade(&owner),
            endpoint: self.endpoint,
            payload: owner.values.root(value)?,
        })
    }
}

struct RuntimePreparedInput {
    runtime: EvaluationRuntimeId,
    owner: Weak<RuntimeSharedResources>,
    endpoint: RuntimeInputEndpointId,
    payload: RuntimeValueRoot,
}

impl RuntimePreparedInput {
    fn admit(self) -> Result<RuntimeInputSequence, Error> {
        let owner = self.owner.upgrade().ok_or_else(|| {
            Error::new(format!(
                "evaluation runtime {} for input endpoint {} has been dropped",
                self.runtime.get(),
                self.endpoint.get()
            ))
        })?;
        admit_runtime_input(&owner, self.endpoint, self.payload)
    }
}

/// Host-facing halves of one runtime input FIFO.
pub struct RuntimeInputEndpoint<T> {
    sender: RuntimeInputSender<T>,
    reader: RuntimeInputReader,
}

impl<T> RuntimeInputEndpoint<T> {
    pub fn sender(&self) -> RuntimeInputSender<T> {
        self.sender.clone()
    }

    pub fn reader(&self) -> RuntimeInputReader {
        self.reader.clone()
    }

    pub fn into_parts(self) -> (RuntimeInputSender<T>, RuntimeInputReader) {
        (self.sender, self.reader)
    }
}

/// Runtime-bound authority to add an output intent to an event journal.
#[derive(Clone)]
pub struct RuntimeOutputWriter {
    runtime: EvaluationRuntimeId,
    owner: Weak<RuntimeSharedResources>,
    endpoint: RuntimeOutputEndpointId,
}

impl RuntimeOutputWriter {
    pub fn id(&self) -> RuntimeOutputEndpointId {
        self.endpoint
    }

    fn validate_runtime(
        &self,
        runtime: EvaluationRuntimeId,
    ) -> Result<Arc<RuntimeSharedResources>, Error> {
        if self.runtime != runtime {
            return Err(Error::new(format!(
                "output endpoint {} belongs to evaluation runtime {}, not {}",
                self.endpoint.get(),
                self.runtime.get(),
                runtime.get()
            )));
        }
        self.owner.upgrade().ok_or_else(|| {
            Error::new(format!(
                "evaluation runtime {} for output endpoint {} has been dropped",
                self.runtime.get(),
                self.endpoint.get()
            ))
        })
    }
}

type RuntimeOutputDecoder<T> = dyn Fn(Value) -> Result<T, Error> + Send + Sync;
type RuntimeOutputAdapter<T> = dyn Fn(T) -> Result<(), Error> + Send + Sync;

/// Host-side claimant and adapter for one runtime output endpoint.
pub struct RuntimeOutputDelivery<T> {
    runtime: EvaluationRuntimeId,
    owner: Weak<RuntimeSharedResources>,
    endpoint: RuntimeOutputEndpointId,
    decode: Arc<RuntimeOutputDecoder<T>>,
    adapter: Arc<RuntimeOutputAdapter<T>>,
}

impl<T> Clone for RuntimeOutputDelivery<T> {
    fn clone(&self) -> Self {
        Self {
            runtime: self.runtime,
            owner: self.owner.clone(),
            endpoint: self.endpoint,
            decode: self.decode.clone(),
            adapter: self.adapter.clone(),
        }
    }
}

impl<T> RuntimeOutputDelivery<T> {
    pub fn id(&self) -> RuntimeOutputEndpointId {
        self.endpoint
    }

    /// Claims and terminally delivers the next committed output, if one is
    /// ready. Decode and adapter failures are retained by the runtime and
    /// returned as ordinary terminal outcomes; caught panics follow the same
    /// path instead of unwinding through runtime state.
    pub fn deliver_next(&self) -> Result<Option<RuntimeDeliveryOutcome>, Error> {
        let owner = self.owner.upgrade().ok_or_else(|| {
            Error::new(format!(
                "evaluation runtime {} for output endpoint {} has been dropped",
                self.runtime.get(),
                self.endpoint.get()
            ))
        })?;
        if owner.id != self.runtime {
            return Err(Error::new("output endpoint runtime provenance mismatch"));
        }
        let Some(ticket) = claim_runtime_delivery(
            owner,
            self.endpoint,
            self.decode.clone(),
            self.adapter.clone(),
        )?
        else {
            return Ok(None);
        };
        ticket.deliver().map(Some)
    }

    /// Returns retained failures belonging to this endpoint.
    pub fn failure_snapshot(&self) -> Result<RuntimeDeliveryFailureSnapshot, Error> {
        let owner = self.owner.upgrade().ok_or_else(|| {
            Error::new(format!(
                "evaluation runtime {} for output endpoint {} has been dropped",
                self.runtime.get(),
                self.endpoint.get()
            ))
        })?;
        Ok(runtime_delivery_failure_snapshot(
            &owner,
            Some(self.endpoint),
        ))
    }
}

/// Host-facing transactional and delivery halves of one output endpoint.
pub struct RuntimeOutputEndpoint<T> {
    writer: RuntimeOutputWriter,
    delivery: RuntimeOutputDelivery<T>,
}

impl<T> RuntimeOutputEndpoint<T> {
    pub fn writer(&self) -> RuntimeOutputWriter {
        self.writer.clone()
    }

    pub fn delivery(&self) -> RuntimeOutputDelivery<T> {
        self.delivery.clone()
    }

    pub fn into_parts(self) -> (RuntimeOutputWriter, RuntimeOutputDelivery<T>) {
        (self.writer, self.delivery)
    }
}

struct RuntimeDeliveryTicket<T> {
    resources: Arc<RuntimeSharedResources>,
    delivery: RuntimeDeliveryId,
    endpoint: RuntimeOutputEndpointId,
    payload: RuntimeValueRoot,
    decode: Arc<RuntimeOutputDecoder<T>>,
    adapter: Arc<RuntimeOutputAdapter<T>>,
}

impl<T> RuntimeDeliveryTicket<T> {
    fn deliver(self) -> Result<RuntimeDeliveryOutcome, Error> {
        let Self {
            resources,
            delivery,
            endpoint,
            payload,
            decode,
            adapter,
        } = self;
        let invocation = catch_unwind(AssertUnwindSafe(|| {
            let decoded = decode(payload.value(resources.id))
                .map_err(|error| (RuntimeDeliveryFailureKind::Decode, error))?;
            adapter(decoded).map_err(|error| (RuntimeDeliveryFailureKind::Adapter, error))
        }));
        let failure = match invocation {
            Ok(Ok(())) => None,
            Ok(Err(failure)) => Some(failure),
            Err(panic) => Some((
                RuntimeDeliveryFailureKind::Panic,
                Error::new(format!(
                    "output delivery panicked: {}",
                    panic_payload_message(panic.as_ref())
                )),
            )),
        };
        let failure = terminalize_runtime_delivery(&resources, endpoint, delivery, failure)?;
        drop(payload);
        Ok(match failure {
            Some(failure) => RuntimeDeliveryOutcome::Failed(failure),
            None => RuntimeDeliveryOutcome::Delivered(delivery),
        })
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

#[derive(Default)]
struct RuntimeLoggerLifecycleState {
    revision: u64,
    input_closed: bool,
    cancelled: bool,
}

/// Temporary logger lifecycle state retained until coordinated runtime
/// settlement replaces explicit close and cancellation in Phase 10D.
#[doc(hidden)]
#[derive(Clone)]
pub struct RuntimeLoggerSnapshot {
    events: RuntimeEventSnapshot,
    input_closed: bool,
    cancelled: bool,
    lifecycle_revision: u64,
}

impl RuntimeLoggerSnapshot {
    #[doc(hidden)]
    pub fn events(&self) -> &RuntimeEventSnapshot {
        &self.events
    }

    #[doc(hidden)]
    pub fn input_closed(&self) -> bool {
        self.input_closed
    }

    #[doc(hidden)]
    pub fn cancelled(&self) -> bool {
        self.cancelled
    }
}

#[derive(Clone)]
struct RuntimeValueFactory {
    runtime: EvaluationRuntimeId,
    core: CoreValueFactory,
}

impl RuntimeValueFactory {
    fn root(&self, value: Value) -> Result<RuntimeValueRoot, Error> {
        value.require_runtime(self.runtime)?;
        Ok(value.0)
    }

    fn core(&self) -> &CoreValueFactory {
        &self.core
    }
}

impl RuntimeValueRoot {
    fn value(&self, runtime: EvaluationRuntimeId) -> Value {
        debug_assert_eq!(self.runtime_id(), runtime);
        Value(self.clone())
    }
}

fn admit_runtime_input(
    resources: &Arc<RuntimeSharedResources>,
    endpoint: RuntimeInputEndpointId,
    payload: RuntimeValueRoot,
) -> Result<RuntimeInputSequence, Error> {
    debug_assert_eq!(payload.runtime_id(), resources.id);
    let mutation = resources.mutation_admission.mutation_guard();
    let sequence = {
        let mut state = resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        let input = state.events.inputs.get_mut(&endpoint).ok_or_else(|| {
            Error::new(format!(
                "runtime input endpoint {} is not registered",
                endpoint.get()
            ))
        })?;
        let input = Arc::make_mut(input);
        let sequence = input.next_sequence;
        let next = sequence.checked_next().ok_or_else(|| {
            Error::new(format!(
                "input sequence exhausted for endpoint {}",
                endpoint.get()
            ))
        })?;
        input
            .admitted
            .push_back(RuntimeInputRecord { sequence, payload });
        input.next_sequence = next;
        state.events.revision = state.events.revision.wrapping_add(1);
        let revision = state.events.revision;
        state
            .events
            .latest_changes
            .insert(ConflictAddress::input_slot(endpoint, sequence), revision);
        sequence
    };
    publish_runtime_observation(resources, mutation);
    Ok(sequence)
}

fn claim_runtime_delivery<T>(
    resources: Arc<RuntimeSharedResources>,
    endpoint: RuntimeOutputEndpointId,
    decode: Arc<RuntimeOutputDecoder<T>>,
    adapter: Arc<RuntimeOutputAdapter<T>>,
) -> Result<Option<RuntimeDeliveryTicket<T>>, Error> {
    let mutation = resources.mutation_admission.mutation_guard();
    let claimed = {
        let mut state = resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        let Some(delivery) = state
            .events
            .outputs
            .ready_by_endpoint
            .get(&endpoint)
            .ok_or_else(|| {
                Error::new(format!(
                    "runtime output endpoint {} is not registered",
                    endpoint.get()
                ))
            })?
            .front()
            .copied()
        else {
            return Ok(None);
        };
        let record = state
            .events
            .outputs
            .records
            .get_mut(&delivery)
            .expect("every ready delivery has a record");
        if record.state == RuntimeDeliveryState::Running {
            return Ok(None);
        }
        debug_assert_eq!(record.endpoint, endpoint);
        record.state = RuntimeDeliveryState::Running;
        Some((delivery, record.payload.clone()))
    };
    drop(mutation);
    Ok(claimed.map(|(delivery, payload)| RuntimeDeliveryTicket {
        resources,
        delivery,
        endpoint,
        payload,
        decode,
        adapter,
    }))
}

fn terminalize_runtime_delivery(
    resources: &RuntimeSharedResources,
    endpoint: RuntimeOutputEndpointId,
    delivery: RuntimeDeliveryId,
    failure: Option<(RuntimeDeliveryFailureKind, Error)>,
) -> Result<Option<Arc<RuntimeDeliveryFailure>>, Error> {
    let failure = failure.map(|(kind, error)| {
        Arc::new(RuntimeDeliveryFailure {
            runtime: resources.id,
            delivery,
            endpoint,
            kind,
            error,
        })
    });
    let mutation = resources.mutation_admission.mutation_guard();
    let retired =
        {
            let mut state = resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            let record =
                state.events.outputs.records.get(&delivery).ok_or_else(|| {
                    Error::new("delivery record disappeared before terminalization")
                })?;
            if record.endpoint != endpoint || record.state != RuntimeDeliveryState::Running {
                return Err(Error::new("delivery record changed before terminalization"));
            }
            let queued = state
                .events
                .outputs
                .ready_by_endpoint
                .get_mut(&endpoint)
                .expect("a running delivery retains its endpoint queue");
            if queued.front().copied() != Some(delivery) {
                return Err(Error::new(
                    "running delivery is no longer the endpoint queue head",
                ));
            }
            queued.pop_front();
            let retired = state
                .events
                .outputs
                .records
                .remove(&delivery)
                .expect("the validated running delivery remains present");
            if let Some(failure) = &failure {
                state
                    .events
                    .outputs
                    .failures
                    .insert_mut(delivery, failure.clone());
            }
            retired
        };
    publish_runtime_observation(resources, mutation);
    drop(retired);
    Ok(failure)
}

fn runtime_delivery_failure_snapshot(
    resources: &RuntimeSharedResources,
    endpoint: Option<RuntimeOutputEndpointId>,
) -> RuntimeDeliveryFailureSnapshot {
    let state = resources
        .transactions
        .state
        .lock()
        .expect("runtime transaction mutex should not be poisoned");
    let failures = match endpoint {
        Some(endpoint) => state
            .events
            .outputs
            .failures
            .iter()
            .filter(|(_, failure)| failure.endpoint == endpoint)
            .fold(
                RedBlackTreeMapSync::new_sync(),
                |failures, (id, failure)| failures.insert(*id, failure.clone()),
            ),
        None => state.events.outputs.failures.clone(),
    };
    RuntimeDeliveryFailureSnapshot {
        runtime: resources.id,
        failures,
    }
}

fn publish_runtime_observation(
    resources: &RuntimeSharedResources,
    mutation: RuntimeMutationGuard<'_>,
) {
    let epoch = resources.observations.advance();
    let work = resources.work.upgrade();
    let scheduler_changed = work
        .as_ref()
        .map(|work| work.publish_runtime_observation_guarded(&mutation, epoch));
    drop(mutation);
    resources.observations.notify_all();
    if let (Some(work), Some(changed)) = (work, scheduler_changed) {
        work.notify_runtime_observation(changed);
    }
}

impl RuntimeSharedResources {
    #[doc(hidden)]
    pub fn id(&self) -> EvaluationRuntimeId {
        self.id
    }

    #[doc(hidden)]
    pub fn values(&self) -> Values {
        Values {
            runtime: self.id,
            core: self.values.core().clone(),
        }
    }

    fn root_value(&self, value: Value) -> Result<RuntimeValueRoot, Error> {
        self.values.root(value)
    }

    fn allocate_reasoning_session_id(&self) -> ReasoningSessionId {
        ReasoningSessionId::from_u64(self.ids.reasoning_session().get())
            .expect("reasoning session IDs start at one")
    }

    fn mutation_guard(&self) -> RuntimeMutationGuard<'_> {
        self.mutation_admission.mutation_guard()
    }

    fn publish_observation(&self, mutation: RuntimeMutationGuard<'_>) {
        publish_runtime_observation(self, mutation);
    }

    fn reflection_snapshot(&self) -> (u64, crate::reflection::StoreSnapshot) {
        let generation = self.observations.current().get();
        let store = self
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .reflection
            .snapshot();
        (generation, store)
    }

    fn commit_reflection(
        &self,
        journal: &crate::reflection::StoreJournal,
    ) -> crate::reflection::StoreCommitResult {
        let mutation = self.mutation_guard();
        let result = {
            self.transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned")
                .reflection
                .try_commit(journal)
        };
        if matches!(result, crate::reflection::StoreCommitResult::Committed) {
            self.publish_observation(mutation);
        }
        result
    }

    fn create_volume(&self, initial: Value) -> Result<VolumeId, Error> {
        initial.require_runtime(self.id)?;
        let _mutation = self.mutation_guard();
        self.transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .reflection
            .create_volume(initial)
            .map_err(|error| Error::new(error.as_ref()))
    }

    fn revoke_volume(&self, volume: VolumeId) -> Result<Value, Error> {
        let mutation = self.mutation_guard();
        let value = {
            self.transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned")
                .reflection
                .revoke_volume(volume)
                .ok_or_else(|| {
                    Error::new(format!(
                        "reflection volume {} has already been revoked",
                        volume.get()
                    ))
                })?
        };
        self.publish_observation(mutation);
        Ok(value)
    }

    #[doc(hidden)]
    pub fn update_query(
        &self,
        handle: &Arc<crate::reflection::EvaluationQueryHandle>,
        result: Value,
    ) -> Result<(), Error> {
        result.require_runtime(self.id)?;
        let mutation = self.mutation_guard();
        let updated = {
            self.transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned")
                .reflection
                .update_query(handle, result)
        };
        if updated {
            self.publish_observation(mutation);
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn wait_for_change(&self, observed_generation: u64) -> bool {
        self.observations
            .wait_for_change(RuntimeObservationEpoch::from_raw(observed_generation));
        true
    }

    #[doc(hidden)]
    pub fn logger_transaction_snapshot(
        &self,
    ) -> (u64, crate::reflection::StoreSnapshot, RuntimeLoggerSnapshot) {
        let generation = self.observations.current().get();
        let state = self
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        (
            generation,
            state.reflection.snapshot(),
            RuntimeLoggerSnapshot {
                events: state.events.snapshot(self.id),
                input_closed: state.logger_lifecycle.input_closed,
                cancelled: state.logger_lifecycle.cancelled,
                lifecycle_revision: state.logger_lifecycle.revision,
            },
        )
    }

    #[doc(hidden)]
    pub fn try_commit_logger_transaction(
        &self,
        store: &crate::reflection::StoreJournal,
        snapshot: &RuntimeLoggerSnapshot,
        observed_lifecycle: bool,
        events: &RuntimeEventJournal,
    ) -> crate::reflection::StoreCommitResult {
        if events.runtime != self.id || snapshot.events.runtime != self.id {
            return crate::reflection::StoreCommitResult::Conflict;
        }
        let mutation = self.mutation_guard();
        let (result, changed) = {
            let mut state = self
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            if observed_lifecycle && state.logger_lifecycle.revision != snapshot.lifecycle_revision
            {
                return crate::reflection::StoreCommitResult::Conflict;
            }
            let result = state.reflection.validate(store);
            if !matches!(result, crate::reflection::StoreCommitResult::Committed) {
                return result;
            }
            if !state.events.validate(events) {
                return crate::reflection::StoreCommitResult::Conflict;
            }
            let reflection_changed = state.reflection.commit_validated(store);
            let event_changed = state.events.commit_validated(events);
            (result, reflection_changed || event_changed)
        };
        if changed {
            self.publish_observation(mutation);
        }
        result
    }

    #[cfg(test)]
    fn reflection_root(&self) -> Value {
        self.transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .reflection
            .root()
            .clone()
    }

    fn has_running_delivery(&self) -> bool {
        self.transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .events
            .outputs
            .records
            .values()
            .any(|record| record.state == RuntimeDeliveryState::Running)
    }
}

impl EvaluationRuntime {
    pub fn new(worker_threads: usize) -> Result<Self, Error> {
        Self::with_conflict_analysis(worker_threads, Arc::new(ExactConflictAnalysis))
    }

    /// Constructs a runtime with its immutable reflection conflict policy.
    /// Assemblers attached later observe this policy and cannot replace it.
    pub fn with_conflict_analysis(
        worker_threads: usize,
        conflict_analysis: Arc<dyn ConflictAnalysisStrategy>,
    ) -> Result<Self, Error> {
        let id = allocate_evaluation_runtime_id();
        let ids = RuntimeIds::new();
        let values = RuntimeValueFactory {
            runtime: id,
            core: CoreValueFactory::new(id, ids.clone()),
        };
        let event_conflict_analysis = conflict_analysis.clone();
        let mutation_admission = RuntimeMutationAdmission::new();
        let observations = RuntimeObservationState::new();
        let work = EvaluationWorkCoordinator::new(
            id,
            ids.clone(),
            mutation_admission.clone(),
            observations.clone(),
        );
        values.core().attach_work_coordinator(&work);
        let executor = EvaluationExecutor::new(worker_threads, &work)
            .map_err(|error| Error::new(error.as_ref()))?;
        let shared_resources = Arc::new(RuntimeSharedResources {
            id,
            values: values.clone(),
            transactions: RuntimeTransactionState {
                state: Mutex::new(RuntimeTransactionData {
                    reflection: ReflectionStore::new(values.core().clone(), conflict_analysis),
                    events: RuntimeEventState::new(event_conflict_analysis),
                    logger_lifecycle: RuntimeLoggerLifecycleState::default(),
                }),
            },
            observations,
            ids,
            mutation_admission,
            work: Arc::downgrade(&work),
        });
        Ok(Self {
            state: Arc::new(RuntimeState {
                executor,
                work,
                shared_resources,
                diagnostic_ingresses: Mutex::new(Vec::new()),
            }),
            default_reflection_profile: Arc::new(ReflectionTaskProfile::unsealed()),
        })
    }

    pub fn id(&self) -> EvaluationRuntimeId {
        self.state.shared_resources.id()
    }

    #[doc(hidden)]
    pub fn shared_resources(&self) -> Arc<RuntimeSharedResources> {
        self.state.shared_resources.clone()
    }

    pub fn worker_threads(&self) -> usize {
        self.state.executor.worker_count()
    }

    /// Returns this runtime's explicit value-construction service.
    pub fn values(&self) -> Values {
        self.state.shared_resources.values()
    }

    /// Registers a runtime-local FIFO input boundary.
    ///
    /// The converter is host policy: it runs before mutation admission and
    /// leaves the runtime untouched when it fails. The returned sender and
    /// reader retain only weak links to this runtime.
    pub fn input_endpoint<T, F>(&self, convert: F) -> Result<RuntimeInputEndpoint<T>, Error>
    where
        F: Fn(T) -> Result<Value, Error> + Send + Sync + 'static,
    {
        let id = self
            .state
            .shared_resources
            .ids
            .input_endpoint()
            .map_err(Error::new)?;
        let endpoint = RuntimeInputEndpointId::from_u64(id.get())
            .expect("runtime input endpoint IDs start at one");
        let _mutation = self.mutation_guard();
        self.state
            .shared_resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .events
            .inputs
            .insert(endpoint, Arc::new(RuntimeInputBuffer::default()));
        let owner = Arc::downgrade(&self.state.shared_resources);
        Ok(RuntimeInputEndpoint {
            sender: RuntimeInputSender {
                runtime: self.id(),
                owner: owner.clone(),
                endpoint,
                convert: Arc::new(convert),
                marker: PhantomData,
            },
            reader: RuntimeInputReader {
                runtime: self.id(),
                owner,
                endpoint,
            },
        })
    }

    /// Registers a buffered output endpoint with separate typed decoding and
    /// external delivery policy.
    pub fn output_endpoint<T, D, A>(
        &self,
        decode: D,
        adapter: A,
    ) -> Result<RuntimeOutputEndpoint<T>, Error>
    where
        D: Fn(Value) -> Result<T, Error> + Send + Sync + 'static,
        A: Fn(T) -> Result<(), Error> + Send + Sync + 'static,
    {
        let id = self
            .state
            .shared_resources
            .ids
            .output_endpoint()
            .map_err(Error::new)?;
        let endpoint = RuntimeOutputEndpointId::from_u64(id.get())
            .expect("runtime output endpoint IDs start at one");
        let _mutation = self.mutation_guard();
        self.state
            .shared_resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .events
            .outputs
            .ready_by_endpoint
            .insert(endpoint, std::collections::VecDeque::new());
        let owner = Arc::downgrade(&self.state.shared_resources);
        Ok(RuntimeOutputEndpoint {
            writer: RuntimeOutputWriter {
                runtime: self.id(),
                owner: owner.clone(),
                endpoint,
            },
            delivery: RuntimeOutputDelivery {
                runtime: self.id(),
                owner,
                endpoint,
                decode: Arc::new(decode),
                adapter: Arc::new(adapter),
            },
        })
    }

    /// Captures every currently retained external delivery failure.
    pub fn delivery_failure_snapshot(&self) -> RuntimeDeliveryFailureSnapshot {
        runtime_delivery_failure_snapshot(&self.state.shared_resources, None)
    }

    /// Acknowledges one retained delivery failure. This changes reporting
    /// state only and therefore does not advance the semantic observation
    /// epoch.
    pub fn acknowledge_delivery_failure(&self, delivery: RuntimeDeliveryId) -> bool {
        let mutation = self.mutation_guard();
        let removed = {
            let mut state = self
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            let removed = state.events.outputs.failures.get(&delivery).cloned();
            if removed.is_some() {
                state.events.outputs.failures.remove_mut(&delivery);
            }
            removed
        };
        drop(mutation);
        let acknowledged = removed.is_some();
        drop(removed);
        acknowledged
    }

    /// Pumps useful lifecycle work across every evaluation session until the
    /// runtime reaches a stable instant.
    ///
    /// This transitional internal API does not construct a readiness report.
    /// It waits for work currently owned by a worker or delivery callback,
    /// abandons only unclaimed best-effort sparks, and leaves queued external
    /// output for its host adapter.
    #[doc(hidden)]
    pub fn pump_until_stable(&self) {
        let admission = &self.state.shared_resources.mutation_admission;
        let activity = admission.activity();
        loop {
            if self.state.work.poll_runtime_work() {
                continue;
            }

            if self.state.work.abandon_quiescent_sparks() != 0 {
                // Releasing a spark's lazy claim may make lifecycle work
                // runnable, so always begin another ordinary pump pass.
                continue;
            }

            let observed_activity = activity.current();
            let Some(settlement) = admission.try_settlement_guard() else {
                activity.wait_for_change(observed_activity);
                continue;
            };
            let work = self.state.work.runtime_pump_snapshot();
            let running_delivery = self.state.shared_resources.has_running_delivery();
            drop(settlement);

            if work.useful_ready || work.abandonable_sparks {
                continue;
            }
            if work.progress_owned || running_delivery {
                activity.wait_for_change(observed_activity);
                continue;
            }
            return;
        }
    }

    /// Observes one stable runtime instant without pumping, abandoning, or
    /// terminalizing any work.
    ///
    /// Call [`Self::pump_until_stable`] first when the client wants queued work
    /// and best-effort spark normalization to run before classification.
    #[doc(hidden)]
    pub fn readiness(&self) -> RuntimeReadiness {
        let Some(settlement) = self.try_settlement_guard() else {
            return RuntimeReadiness::Busy;
        };
        let coordinator = self.state.work.runtime_readiness_snapshot();
        if matches!(coordinator, RuntimeCoordinatorReadiness::Busy) {
            drop(settlement);
            return RuntimeReadiness::Busy;
        }

        let reflection = {
            let state = self
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            if !state.events.outputs.records.is_empty() {
                drop(state);
                drop(settlement);
                return RuntimeReadiness::Busy;
            }
            state.reflection.snapshot()
        };
        let observation_epoch = self.state.shared_resources.observations.current().get();
        drop(settlement);

        match coordinator {
            RuntimeCoordinatorReadiness::Busy => unreachable!("handled above"),
            RuntimeCoordinatorReadiness::Ready {
                work_generation,
                exits,
            } => RuntimeReadiness::Ready(QuiescenceSnapshot {
                runtime: self.clone(),
                stamp: RuntimeReadinessStamp {
                    work_generation,
                    observation_epoch,
                },
                dispositions: exits
                    .into_iter()
                    .map(runtime_disposition_from_snapshot)
                    .collect(),
                reflection,
            }),
            RuntimeCoordinatorReadiness::Deadlocked {
                work_generation,
                exits,
                unfinished,
            } => RuntimeReadiness::Deadlocked(DeadlockSnapshot {
                runtime: self.clone(),
                stamp: RuntimeReadinessStamp {
                    work_generation,
                    observation_epoch,
                },
                dispositions: exits
                    .into_iter()
                    .map(runtime_disposition_from_snapshot)
                    .collect(),
                unfinished: unfinished
                    .into_iter()
                    .map(runtime_deadlock_work_from_snapshot)
                    .collect(),
                reflection,
            }),
        }
    }

    /// Internal activity inspection for the scheduler pump. Retained failures
    /// and buffered input are reporting/state, not active delivery work.
    #[doc(hidden)]
    pub fn has_delivery_activity(&self) -> bool {
        !self
            .state
            .shared_resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .events
            .outputs
            .records
            .is_empty()
    }

    /// Starts this runtime's worker pool exactly once. A runtime constructed
    /// with zero workers remains dormant until this method is called.
    pub fn activate_workers(&self, worker_threads: usize) -> Result<(), Error> {
        self.state
            .executor
            .activate_workers(worker_threads)
            .map_err(|error| Error::new(error.as_ref()))
    }

    pub(crate) fn new_evaluation_session(&self) -> Result<Arc<EvaluationSession>, Error> {
        if !self.has_default_reflection_profile() {
            return Err(Error::new(
                "evaluation runtime default reflection task profile must be sealed before creating a session",
            ));
        }
        Ok(EvaluationSession::shared_with_default_profile(
            &self.state.work,
            self.state.shared_resources.values.core().clone(),
            self.default_reflection_profile.clone(),
        ))
    }

    fn seal_default_reflection_profile(
        &self,
        launcher: Arc<dyn crate::evaluation::ReflectionTaskLauncher>,
    ) -> Result<(), Error> {
        self.default_reflection_profile
            .seal(launcher)
            .map_err(Error::new)
    }

    fn has_default_reflection_profile(&self) -> bool {
        self.default_reflection_profile.is_sealed()
    }

    pub(crate) fn allocate_cli_invocation_id(&self) -> u64 {
        self.state.shared_resources.ids.cli_invocation().get()
    }

    fn mutation_guard(&self) -> RuntimeMutationGuard<'_> {
        self.state
            .shared_resources
            .mutation_admission
            .mutation_guard()
    }

    fn try_settlement_guard(&self) -> Option<RuntimeSettlementGuard<'_>> {
        self.state
            .shared_resources
            .mutation_admission
            .try_settlement_guard()
    }

    /// Reports whether exclusive runtime mutation admission can be acquired
    /// immediately. This is a transitional readiness probe; it does not retain
    /// or settle a snapshot.
    #[doc(hidden)]
    pub fn exclusive_admission_available(&self) -> bool {
        self.try_settlement_guard().is_some()
    }

    #[doc(hidden)]
    pub fn reflection_snapshot(&self) -> (u64, crate::reflection::StoreSnapshot) {
        self.state.shared_resources.reflection_snapshot()
    }

    /// Captures the reflection store and admitted-input state under the same
    /// transactional-state lock.
    #[doc(hidden)]
    pub fn transaction_snapshot(
        &self,
    ) -> (u64, crate::reflection::StoreSnapshot, RuntimeEventSnapshot) {
        // As with `reflection_snapshot`, reading the epoch first prevents a
        // waiter from retaining a new epoch beside stale transactional state.
        let generation = self.state.shared_resources.observations.current().get();
        let state = self
            .state
            .shared_resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        (
            generation,
            state.reflection.snapshot(),
            state.events.snapshot(self.id()),
        )
    }

    /// Atomically validates and applies one reflection-store journal and its
    /// admitted-input claims. Neither side is applied if either conflicts.
    #[doc(hidden)]
    pub fn try_commit_transaction(
        &self,
        store: &crate::reflection::StoreJournal,
        events: &RuntimeEventJournal,
    ) -> crate::reflection::StoreCommitResult {
        if events.runtime != self.id() {
            return crate::reflection::StoreCommitResult::Conflict;
        }
        let mutation = self.mutation_guard();
        let (result, changed) = {
            let mut state = self
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            let result = state.reflection.validate(store);
            if !matches!(result, crate::reflection::StoreCommitResult::Committed) {
                return result;
            }
            if !state.events.validate(events) {
                return crate::reflection::StoreCommitResult::Conflict;
            }
            let reflection_changed = state.reflection.commit_validated(store);
            let event_changed = state.events.commit_validated(events);
            (
                crate::reflection::StoreCommitResult::Committed,
                reflection_changed || event_changed,
            )
        };
        if changed {
            self.publish_observation(mutation);
        }
        result
    }

    #[doc(hidden)]
    pub fn commit_reflection(
        &self,
        journal: &crate::reflection::StoreJournal,
    ) -> crate::reflection::StoreCommitResult {
        self.state.shared_resources.commit_reflection(journal)
    }

    #[doc(hidden)]
    pub fn update_query(
        &self,
        handle: &Arc<crate::reflection::EvaluationQueryHandle>,
        result: Value,
    ) -> Result<(), Error> {
        self.state.shared_resources.update_query(handle, result)
    }

    fn publish_observation(&self, mutation: RuntimeMutationGuard<'_>) {
        publish_runtime_observation(&self.state.shared_resources, mutation);
    }

    #[doc(hidden)]
    pub fn wait_for_change(&self, observed_generation: u64) -> bool {
        self.state
            .shared_resources
            .wait_for_change(observed_generation)
    }

    /// Captures the reflection store, generic events, and temporary logger
    /// lifecycle flags under one transaction lock.
    #[doc(hidden)]
    pub fn logger_transaction_snapshot(
        &self,
    ) -> (u64, crate::reflection::StoreSnapshot, RuntimeLoggerSnapshot) {
        self.state.shared_resources.logger_transaction_snapshot()
    }

    /// Atomically validates and applies one logger transaction across the
    /// reflection store and generic event endpoints. Lifecycle validation is
    /// included only when `.log_status` observed the close state.
    #[doc(hidden)]
    pub fn try_commit_logger_transaction(
        &self,
        store: &crate::reflection::StoreJournal,
        snapshot: &RuntimeLoggerSnapshot,
        observed_lifecycle: bool,
        events: &RuntimeEventJournal,
    ) -> crate::reflection::StoreCommitResult {
        self.state.shared_resources.try_commit_logger_transaction(
            store,
            snapshot,
            observed_lifecycle,
            events,
        )
    }

    #[doc(hidden)]
    pub fn close_logger_input(&self) {
        let mutation = self.mutation_guard();
        {
            let mut state = self
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            state.logger_lifecycle.input_closed = true;
            state.logger_lifecycle.revision = state.logger_lifecycle.revision.wrapping_add(1);
        }
        self.publish_observation(mutation);
    }

    #[doc(hidden)]
    pub fn cancel_logger(&self) {
        let mutation = self.mutation_guard();
        {
            let mut state = self
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            state.logger_lifecycle.cancelled = true;
            state.logger_lifecycle.revision = state.logger_lifecycle.revision.wrapping_add(1);
        }
        self.publish_observation(mutation);
    }

    #[cfg(test)]
    fn reflection_root(&self) -> Value {
        self.state.shared_resources.reflection_root()
    }

    fn conflict_analysis(&self) -> Arc<dyn ConflictAnalysisStrategy> {
        self.state
            .shared_resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .reflection
            .strategy()
    }
}

#[cfg(test)]
pub(crate) fn compiler_test_runtime() -> EvaluationRuntime {
    static RUNTIME: std::sync::LazyLock<EvaluationRuntime> = std::sync::LazyLock::new(|| {
        let core = crate::compiler::test_value_factory();
        let id = core.runtime_id();
        let ids = core.ids().clone();
        let work = core.work_coordinator().unwrap_or_else(|| {
            let candidate = EvaluationWorkCoordinator::new(
                id,
                ids.clone(),
                RuntimeMutationAdmission::new(),
                RuntimeObservationState::new(),
            );
            core.work_coordinator_or_attach(candidate)
        });
        let mutation_admission = work.shared_mutation_admission();
        let observations = work.shared_observations();
        let values = RuntimeValueFactory {
            runtime: id,
            core: core.clone(),
        };
        let executor = EvaluationExecutor::new(0, &work)
            .expect("compiler test executor should be constructible");
        let shared_resources = Arc::new(RuntimeSharedResources {
            id,
            values: values.clone(),
            transactions: RuntimeTransactionState {
                state: Mutex::new(RuntimeTransactionData {
                    reflection: ReflectionStore::new(core, Arc::new(ExactConflictAnalysis)),
                    events: RuntimeEventState::new(Arc::new(ExactConflictAnalysis)),
                    logger_lifecycle: RuntimeLoggerLifecycleState::default(),
                }),
            },
            observations,
            ids,
            mutation_admission,
            work: Arc::downgrade(&work),
        });
        EvaluationRuntime {
            state: Arc::new(RuntimeState {
                executor,
                work,
                shared_resources,
                diagnostic_ingresses: Mutex::new(Vec::new()),
            }),
            default_reflection_profile: Arc::new(ReflectionTaskProfile::unsealed()),
        }
    });
    RUNTIME.clone()
}

#[derive(Clone)]
struct ReasoningSession {
    host: Arc<AssemblerReflectionHost>,
    task_profile: Arc<ReflectionTaskProfile>,
    diagnostics: DiagnosticBus,
    runtime: EvaluationRuntime,
    evaluation: Arc<EvaluationSession>,
}

impl ReasoningSession {
    fn from_host(
        host: Arc<AssemblerReflectionHost>,
        diagnostics: DiagnosticBus,
        runtime: EvaluationRuntime,
    ) -> Result<Self, Error> {
        let task_profile = Arc::new(ReflectionTaskProfile::sealed(task_launcher(
            ReflectionEffects,
            host.clone(),
        )));
        let evaluation = runtime.new_evaluation_session()?;
        Ok(Self {
            host,
            task_profile,
            diagnostics,
            runtime,
            evaluation,
        })
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
        EvalContext::patient_with_task_profile(&self.evaluation, self.task_profile.clone())
    }

    fn conflict_analysis(&self) -> Arc<dyn ConflictAnalysisStrategy> {
        self.runtime.conflict_analysis()
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
    execution: Arc<CompilationExecution>,
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
    diagnostic: Option<Arc<Diagnostic>>,
    diagnostics: Vec<Diagnostic>,
}

impl Error {
    /// Constructs an embedding-boundary error from a plain diagnostic message.
    pub fn new(message: impl Into<Arc<str>>) -> Self {
        let message = message.into();
        Self {
            message,
            diagnostic: None,
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn from_eval(values: &CoreValueFactory, error: EvaluationHalt) -> Self {
        let message: Arc<str> = Arc::from(error.to_string());
        Self::from_eval_parts(
            values,
            eval::halt_diagnostic_value_with(values, &error),
            message,
        )
    }

    fn from_eval_parts(
        values: &CoreValueFactory,
        emission: Option<CoreValue>,
        message: Arc<str>,
    ) -> Self {
        let (message, diagnostic) = match emission {
            Some(emission) => {
                let message = crate::diagnostic::conventional_summary_with(values, &emission)
                    .1
                    .unwrap_or(message);
                (
                    message,
                    Diagnostic::from_emission(Severity::Error, Value::from_core(values, emission)),
                )
            }
            None => (
                message.clone(),
                Diagnostic::new_with_factory(values, Severity::Error, message),
            ),
        };
        Self {
            message,
            diagnostic: Some(Arc::new(diagnostic)),
            diagnostics: Vec::new(),
        }
    }

    fn with_diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Prepends one structured frame describing why this failed value was
    /// demanded. The primary diagnostic text and ad hoc fields remain intact.
    pub fn with_context(mut self, values: &Values, context: Value) -> Result<Self, Error> {
        context.require_runtime(values.runtime)?;
        let Some(diagnostic) = &self.diagnostic else {
            self.diagnostic = Some(Arc::new(Diagnostic::new(
                values,
                Severity::Error,
                self.message.clone(),
            )));
            return self.with_context(values, context);
        };
        diagnostic.emission.require_runtime(values.runtime)?;
        let emission = crate::diagnostic::prepend_contexts_with(
            &values.core,
            diagnostic.emission.as_core().clone(),
            &[context.into_core()],
        )
        .unwrap_or_else(|_| diagnostic.emission.as_core().clone());
        self.diagnostic = Some(Arc::new(Diagnostic::from_emission(
            diagnostic.severity,
            Value::from_core(&values.core, emission),
        )));
        Ok(self)
    }

    /// Returns the primary failure as a structured diagnostic.
    ///
    /// Permanent evaluator failures retain their original Glam emission and
    /// `msg.context`; ordinary host failures use a conventional text message.
    pub fn diagnostic(&self, values: &Values) -> Result<Diagnostic, Error> {
        match &self.diagnostic {
            Some(diagnostic) => {
                diagnostic.emission.require_runtime(values.runtime)?;
                Ok(diagnostic.as_ref().clone())
            }
            None => Ok(Diagnostic::new(
                values,
                Severity::Error,
                self.message.clone(),
            )),
        }
    }

    pub(crate) fn structured_diagnostic(&self) -> Option<&Diagnostic> {
        self.diagnostic.as_deref()
    }

    /// Returns additional diagnostics emitted while attempting the operation.
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

fn path_lookup_context(path: &str) -> CoreValue {
    eval::evaluation_context_frame_with_args(
        "path_lookup",
        Dict::new_sync().insert(
            (*crate::core::keys::PATH).clone(),
            CoreValue::binary_from_text(path),
        ),
    )
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
    /// Stable local quiescence while another live demand session in the same
    /// runtime may still satisfy an unfinished dependency.
    Quiescent,
    Deadlocked,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ReasoningFailure {
    runtime: EvaluationRuntimeId,
    task: EvaluationTaskId,
    diagnostic: Diagnostic,
    session: EvaluationSessionId,
}

impl fmt::Debug for ReasoningFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReasoningFailure")
            .field("task_id", &self.task_id())
            .field("diagnostic", &self.diagnostic)
            .finish_non_exhaustive()
    }
}

impl ReasoningFailure {
    pub fn task_id(&self) -> u64 {
        self.task.get()
    }

    pub fn message(&self) -> &str {
        self.diagnostic.message()
    }

    /// Returns the structured terminal failure retained by the reasoning
    /// session.
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningTask {
    task_id: u64,
    state: ReasoningTaskState,
    waiting_on_task: Option<u64>,
    waiting_on_session: Option<u64>,
    wait_id: Option<u64>,
    observed_epoch: Option<u64>,
    blocked_diagnostic: Option<Diagnostic>,
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

    /// The evaluation session that owns `waiting_on_task`.
    pub fn waiting_on_session(&self) -> Option<u64> {
        self.waiting_on_session
    }

    pub fn wait_id(&self) -> Option<u64> {
        self.wait_id
    }

    pub fn observed_epoch(&self) -> Option<u64> {
        self.observed_epoch
    }

    /// The evaluation error retained while this task waits for an observed
    /// state change that can retry its current reasoning checkpoint.
    pub fn blocked_error(&self) -> Option<&str> {
        self.blocked_diagnostic.as_ref().map(Diagnostic::message)
    }

    /// The structured evaluation failure retained while this task waits for
    /// an observed state change that can retry its current reasoning
    /// checkpoint.
    pub fn blocked_diagnostic(&self) -> Option<&Diagnostic> {
        self.blocked_diagnostic.as_ref()
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
    let runtime = environment.runtime_id();
    let CoreValue::Dict(root) = environment.into_core() else {
        return Err(Error::new("reflection environment must be a dictionary"));
    };
    let glam_key = Key::atom_from_text("glam");
    let replaced_glam = root.get(&glam_key).is_some();
    Ok((
        Value::from_runtime(
            runtime,
            CoreValue::Dict(root.insert(glam_key, authoritative_glam_environment(role))),
        ),
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
                CoreValue::Atom(crate::core::Atom::from_key(&Key::binary_from_text(role))),
            )),
        )
        .insert(
            Key::atom_from_text("origin"),
            CoreValue::Dict(Dict::new_sync().insert(
                Key::atom_from_text("inspect"),
                CoreValue::Builtin(Builtin::InspectOrigin),
            )),
        );
    CoreValue::Dict(glam)
}

fn reflection_environment_for_role(environment: &Value, role: &str) -> Value {
    let CoreValue::Dict(root) = environment.as_core() else {
        unreachable!("authoritative reflection environment must be a dictionary")
    };
    Value::from_runtime(
        environment.runtime_id(),
        CoreValue::Dict(root.insert(
            Key::atom_from_text("glam"),
            authoritative_glam_environment(role),
        )),
    )
}

/// Owner handle for one protected volume in an evaluation runtime.
///
/// Capability values may be cloned freely, but only this handle can remove the
/// volume and recover its final unforced value. Dropping the handle does not
/// revoke the volume.
pub struct ReasoningVolume {
    resources: Arc<RuntimeSharedResources>,
    volume: VolumeId,
    effects: Value,
}

impl ReasoningVolume {
    /// Returns the closed `{get, set, rewrite}` effect capability value.
    pub fn effects(&self) -> Value {
        debug_assert_eq!(self.effects.runtime_id(), self.resources.id);
        self.effects.clone()
    }

    /// Removes the volume and returns its final value without forcing it.
    /// Further uses of any capability for this volume produce
    /// use-after-revoke errors.
    pub fn revoke(self) -> Result<Value, Error> {
        self.resources.revoke_volume(self.volume)
    }
}

/// Privileged structural observation of values in one assembler session.
///
/// Ordinary [`Value`] accessors never drive evaluation. This facade is the
/// embedding equivalent of reflection capabilities: every operation may
/// demand the observed value through this assembler's evaluation session.
/// Container members themselves remain lazy unless an operation explicitly
/// observes them.
#[derive(Clone, Copy)]
pub struct ReflectionInspector<'a> {
    assembler: &'a Assembler,
}

impl ReflectionInspector<'_> {
    /// Evaluates a value to weak-head normal form.
    pub fn evaluate(&self, value: &Value) -> Result<Value, Error> {
        self.assembler.evaluate(value)
    }

    /// Returns a sealed carrier's associated metadata without evaluating it.
    ///
    /// The supplied value is evaluated only far enough to recognize its outer
    /// kind. Ordinary values return `None`; a failure while reaching that kind
    /// remains an evaluation error rather than a metadata mismatch.
    pub fn associated_metadata(&self, value: &Value) -> Result<Option<Value>, Error> {
        value.require_runtime(self.assembler.reasoning.runtime.id())?;
        let values = self.assembler.core_values();
        let value = self
            .assembler
            .eval_context()
            .evaluate_whnf(value.as_core())
            .map_err(|error| self.assembler.evaluation_error(error))?;
        Ok(value
            .associated_metadata()
            .map(|value| Value::from_core(&values, value)))
    }

    /// Returns the elements of a list without evaluating the elements.
    ///
    /// The list spine and any deferred concatenation segments are evaluated
    /// far enough to enumerate the elements.
    pub fn list_items(&self, value: &Value) -> Result<Vec<Value>, Error> {
        value.require_runtime(self.assembler.reasoning.runtime.id())?;
        let values = self.assembler.core_values();
        let value = self
            .assembler
            .eval_context()
            .evaluate_whnf(value.as_core())
            .map_err(|error| self.assembler.evaluation_error(error))?;
        let CoreValue::List(list) = value else {
            return Err(Error::new(format!(
                "reflection list inspection requires a list, received {}",
                value.diagnostic_kind_name()
            )));
        };
        eval::list_to_value_items(&self.assembler.eval_context(), &list)
            .map(|items| {
                items
                    .into_iter()
                    .map(|value| Value::from_core(&values, value))
                    .collect()
            })
            .map_err(|error| self.assembler.evaluation_error(error))
    }

    /// Returns dictionary entries in canonical key order without evaluating
    /// their values. Keys are reified as ordinary keyable [`Value`]s.
    pub fn dictionary_items(&self, value: &Value) -> Result<Vec<(Value, Value)>, Error> {
        value.require_runtime(self.assembler.reasoning.runtime.id())?;
        let values = self.assembler.core_values();
        let value = self
            .assembler
            .eval_context()
            .evaluate_whnf(value.as_core())
            .map_err(|error| self.assembler.evaluation_error(error))?;
        let CoreValue::Dict(dict) = value else {
            return Err(Error::new(format!(
                "reflection dictionary inspection requires a dictionary, received {}",
                value.diagnostic_kind_name()
            )));
        };
        Ok(dict
            .iter()
            .map(|(key, value)| {
                (
                    Value::from_core(&values, key.to_value_with(&values)),
                    Value::from_core(&values, value.clone()),
                )
            })
            .collect())
    }

    /// Returns the key value that gives an atom its identity.
    pub fn atom_key(&self, value: &Value) -> Result<Value, Error> {
        value.require_runtime(self.assembler.reasoning.runtime.id())?;
        let values = self.assembler.core_values();
        let value = self
            .assembler
            .eval_context()
            .evaluate_whnf(value.as_core())
            .map_err(|error| self.assembler.evaluation_error(error))?;
        let CoreValue::Atom(atom) = value else {
            return Err(Error::new(format!(
                "reflection atom inspection requires an atom, received {}",
                value.diagnostic_kind_name()
            )));
        };
        Ok(Value::from_core(
            &values,
            atom.key().to_value_with(&self.assembler.core_values()),
        ))
    }
}

impl fmt::Debug for ReflectionInspector<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReflectionInspector")
            .finish_non_exhaustive()
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
    runtime_locked: bool,
    runtime_supplied: bool,
    conflict_analysis_requested: bool,
    construction_error: Option<Arc<str>>,
}

/// Capabilities available while constructing the immutable reflection
/// environment. The borrow cannot escape the construction closure.
pub struct ReflectionEnvironmentBuilder<'a> {
    host: &'a Arc<AssemblerReflectionHost>,
}

impl ReflectionEnvironmentBuilder<'_> {
    /// Returns the selected runtime's value-construction service.
    pub fn values(&self) -> Values {
        self.host.values()
    }

    /// Creates a protected volume belonging to the selected evaluation runtime.
    pub fn create_volume(&mut self, initial: Value) -> Result<ReasoningVolume, Error> {
        create_reasoning_volume(self.host, initial)
    }

    /// Creates a promised environment value and its affine host resolver.
    /// Same-runtime work subscribes directly when it blocks on the value, so
    /// the resolver needs no later assembler-specific arming step.
    pub fn promise(&mut self, label: impl Into<Arc<str>>) -> (Value, PromiseResolver) {
        let values = self.host.values();
        let promise = PromisedValue::new(&values.core, label);
        (
            Value::from_core(&values.core, CoreValue::Promised(promise.clone())),
            PromiseResolver {
                runtime: self.host.resources.id,
                promise: Some(promise),
            },
        )
    }
}

impl Default for AssemblerBuilder {
    fn default() -> Self {
        let diagnostics = DiagnosticBus::new();
        let runtime = EvaluationRuntime::new(0)
            .expect("zero-worker evaluation runtime must be constructible");
        let host = Arc::new(AssemblerReflectionHost::new_unsealed(
            &runtime,
            diagnostics.clone(),
        ));
        Self {
            source_system: Arc::new(FileSourceSystem::default()),
            runtime,
            host,
            diagnostics,
            reflection_environment: None,
            diagnostic_attachments: Vec::new(),
            pending_diagnostics: Vec::new(),
            runtime_locked: false,
            runtime_supplied: false,
            conflict_analysis_requested: false,
            construction_error: None,
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
        if self.runtime_locked {
            self.record_construction_error(
                "the evaluation runtime must be selected before creating protected volumes or the reflection environment",
            );
            return self;
        }
        if self.conflict_analysis_requested {
            self.record_construction_error(
                "an attached evaluation runtime already owns its conflict-analysis strategy",
            );
            return self;
        }
        self.runtime = runtime;
        self.host = Arc::new(AssemblerReflectionHost::new_unsealed(
            &self.runtime,
            self.diagnostics.clone(),
        ));
        self.runtime_supplied = true;
        self
    }

    /// Selects the diagnostic bus used by this assembler.
    ///
    /// This is a construction-time boundary because the reflection host keeps
    /// the bus as part of its immutable task profile.
    pub fn diagnostic_bus(mut self, diagnostics: DiagnosticBus) -> Self {
        if self.runtime_locked || !self.diagnostic_attachments.is_empty() {
            self.record_construction_error(
                "the diagnostic bus must be selected before reflection environment construction or subscriber attachment",
            );
            return self;
        }
        self.diagnostics = diagnostics;
        self.host = Arc::new(AssemblerReflectionHost::new_unsealed(
            &self.runtime,
            self.diagnostics.clone(),
        ));
        self
    }

    pub fn conflict_analysis(mut self, strategy: Arc<dyn ConflictAnalysisStrategy>) -> Self {
        if self.runtime_locked {
            self.record_construction_error(
                "the conflict-analysis strategy must be selected before creating protected volumes or the reflection environment",
            );
            return self;
        }
        if self.runtime_supplied {
            self.record_construction_error(
                "an attached evaluation runtime already owns its conflict-analysis strategy",
            );
            return self;
        }
        match EvaluationRuntime::with_conflict_analysis(self.runtime.worker_threads(), strategy) {
            Ok(runtime) => {
                self.runtime = runtime;
                self.host = Arc::new(AssemblerReflectionHost::new_unsealed(
                    &self.runtime,
                    self.diagnostics.clone(),
                ));
                self.conflict_analysis_requested = true;
            }
            Err(error) => self.record_construction_error(error.to_string()),
        }
        self
    }

    fn record_construction_error(&mut self, message: impl Into<Arc<str>>) {
        if self.construction_error.is_none() {
            self.construction_error = Some(message.into());
        }
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
        self.runtime_locked = true;
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
        self.runtime_locked = true;
        let environment = build(&mut ReflectionEnvironmentBuilder { host: &self.host })?;
        environment.require_runtime(self.runtime.id())?;
        let (environment, replaced_glam) =
            authoritative_reflection_environment(environment, "assembler")?;
        self.reflection_environment = Some(environment);
        if replaced_glam {
            self.pending_diagnostics.push(Diagnostic::new(
                &self.runtime.values(),
                Severity::Warning,
                "reflection environment namespace `glam` is reserved; supplied value was ignored",
            ));
        }
        Ok(self)
    }

    pub fn build(mut self) -> Result<Assembler, Error> {
        if let Some(error) = self.construction_error.take() {
            return Err(Error::new(error));
        }
        self.diagnostics.bind_runtime(&self.runtime)?;
        let environment = match self.reflection_environment.take() {
            Some(environment) => environment,
            None => {
                authoritative_reflection_environment(
                    self.runtime.values().empty_record(),
                    "assembler",
                )?
                .0
            }
        };
        self.host.seal_environment(environment)?;
        if !self.runtime.has_default_reflection_profile() {
            match self.runtime.seal_default_reflection_profile(task_launcher(
                ReflectionEffects,
                self.host.clone(),
            )) {
                Ok(()) => {}
                Err(error) if self.runtime.has_default_reflection_profile() => {
                    // Another builder sealed the same dormant runtime first.
                    // This assembler reuses that immutable default profile.
                    drop(error);
                }
                Err(error) => return Err(error),
            }
        }
        let reasoning =
            ReasoningSession::from_host(self.host, self.diagnostics.clone(), self.runtime)?;
        for diagnostic in self.pending_diagnostics {
            self.diagnostics.publish_local(diagnostic);
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
        resources: host.resources.clone(),
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

    /// Returns this assembler's privileged reflection-inspection facade.
    ///
    /// Executable and IDE clients are reflection observers, not ordinary Glam
    /// programs. Client policy that needs to inspect opaque value structure
    /// belongs behind this explicit boundary rather than in evaluator
    /// builtins.
    pub fn reflection(&self) -> ReflectionInspector<'_> {
        ReflectionInspector { assembler: self }
    }

    #[cfg(test)]
    pub(crate) fn test_compilation_execution(&self) -> Arc<CompilationExecution> {
        Arc::new(
            CompilationExecution::new(&self.reasoning, Arc::new(Mutex::new(Vec::new())))
                .expect("test compilation execution must be constructible"),
        )
    }

    #[cfg(test)]
    pub(crate) fn test_reflection_heap(&self) -> Value {
        self.reasoning.runtime.reflection_root()
    }

    /// Creates a host-resolved promised value and its unique resolver.
    ///
    /// Resolving, failing, or dropping the resolver wakes each same-runtime
    /// work item currently blocked on the unresolved value.
    pub fn promise(&self, label: impl Into<Arc<str>>) -> (Value, PromiseResolver) {
        let promise = PromisedValue::new(self.eval_context().values(), label);
        (
            Value::from_core(&self.core_values(), CoreValue::Promised(promise.clone())),
            PromiseResolver {
                runtime: self.reasoning.runtime.id(),
                promise: Some(promise),
            },
        )
    }

    /// Creates a protected reflection volume initialized with `initial`.
    /// Possession of the returned Glam capability value is the authority to
    /// access it from any reasoning session in this runtime; ordinary
    /// `.heap.*` requests cannot address the volume.
    pub fn create_volume(&self, initial: Value) -> Result<ReasoningVolume, Error> {
        create_reasoning_volume(&self.reasoning.host, initial)
    }

    /// Returns the cached closed Glam function used by the executable's
    /// default terminal logger. It expects an enriched diagnostic containing
    /// the conventional `msg` and `viewer` fields, including the observer's
    /// complete textual `viewer.header`, and returns bytes.
    pub fn default_diagnostic_formatter(&self) -> Value {
        let values = self.core_values();
        Value::from_core(
            &values,
            crate::g_syntax::default_diagnostic_formatter(&values),
        )
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

    /// Returns this assembler runtime's explicit value-construction service.
    pub fn values(&self) -> Values {
        self.reasoning.runtime.values()
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

    pub(crate) fn core_values(&self) -> CoreValueFactory {
        self.eval_context().values().clone()
    }

    pub(crate) fn allocate_cli_invocation_id(&self) -> u64 {
        self.reasoning.runtime.allocate_cli_invocation_id()
    }

    /// Runs scheduled reflection reasoning without imposing a step or time
    /// limit. A runnable infinite task therefore keeps this call running.
    pub fn drain_reasoning(&self) -> ReasoningReport {
        let context = self.eval_context();
        let runtime = context.values().runtime_id();
        let session = context.session_id();
        let values = context.values().clone();
        let run = context.run_until_quiescent();
        let (status, report) = match run {
            EvaluationSessionRun::Complete(report) => (ReasoningStatus::Complete, report),
            EvaluationSessionRun::Quiescent(report) => (ReasoningStatus::Quiescent, report),
            EvaluationSessionRun::Deadlocked(report) => (ReasoningStatus::Deadlocked, report),
        };
        ReasoningReport {
            status,
            failures: report
                .failures
                .iter()
                .map(|(task, error)| ReasoningFailure {
                    runtime,
                    task: *task,
                    diagnostic: reasoning_diagnostic(&values, error),
                    session,
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
                    waiting_on_session: task.dependency_session.map(|session| session.get()),
                    wait_id: task.wait,
                    observed_epoch: task.observed_epoch.map(RuntimeObservationEpoch::get),
                    blocked_diagnostic: task
                        .error
                        .as_deref()
                        .map(|error| reasoning_diagnostic(&values, error)),
                })
                .collect(),
        }
    }

    /// Acknowledges a failed task previously returned by
    /// [`Self::drain_reasoning`].
    ///
    /// Acknowledgement removes the failure from later reasoning reports but
    /// does not change the task's terminal result. Repeated acknowledgement is
    /// harmless. Any assembler view of the same evaluation runtime may
    /// acknowledge the producer's failure; a report from another runtime is
    /// rejected.
    pub fn acknowledge_reasoning_failure(&self, failure: &ReasoningFailure) -> Result<(), Error> {
        let context = self.eval_context();
        if context.values().runtime_id() != failure.runtime {
            return Err(Error::new(
                "reasoning failure belongs to a different evaluation runtime",
            ));
        }
        context.acknowledge_task_failure(failure.session, failure.task);
        Ok(())
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
            initial_definitions: self.values().empty_record(),
        }
    }

    pub(crate) fn record_diagnostic(&self, diagnostic: Diagnostic) {
        self.reasoning.diagnostics().publish_local(diagnostic);
    }

    fn next_compilation_invocation(&self) -> CompilationInvocationId {
        let id = self
            .next_compilation_invocation
            .fetch_add(1, Ordering::Relaxed);
        assert!(id != u64::MAX, "compilation invocation IDs exhausted");
        CompilationInvocationId::new(id)
    }

    fn evaluation_error(&self, error: EvaluationHalt) -> Error {
        Error::from_eval(&self.core_values(), error)
    }

    /// Evaluates a value far enough to expose its outer semantic value.
    pub fn evaluate(&self, value: &Value) -> Result<Value, Error> {
        value.require_runtime(self.reasoning.runtime.id())?;
        let values = self.core_values();
        self.eval_context()
            .evaluate_whnf(value.as_core())
            .map(|value| Value::from_core(&values, value))
            .map_err(|error| self.evaluation_error(error))
    }

    /// Applies all supplied arguments while preserving evaluator laziness.
    /// Call [`Self::evaluate`] when the result itself must be observed.
    pub fn apply(
        &self,
        function: &Value,
        arguments: impl IntoIterator<Item = Value>,
    ) -> Result<Value, Error> {
        function.require_runtime(self.reasoning.runtime.id())?;
        let mut core_arguments = Vec::new();
        for argument in arguments {
            argument.require_runtime(self.reasoning.runtime.id())?;
            core_arguments.push(argument.into_core());
        }
        let values = self.core_values();
        eval::apply_values(
            &self.eval_context(),
            function.as_core().clone(),
            core_arguments,
        )
        .map(|value| Value::from_core(&values, value))
        .map_err(|error| self.evaluation_error(error))
    }

    /// Builds one closed interaction-net value through a checked, effect-style
    /// API. The callback's returned port becomes the sole exposed port.
    pub fn net(
        &self,
        build: impl for<'net> FnOnce(&mut NetBuilder<'net>) -> Result<NetPort<'net>, Error>,
    ) -> Result<Value, Error> {
        let mut builder = NetBuilder::new(self.values());
        let exposed = build(&mut builder)?.port;
        let template = builder
            .builder
            .try_finish(exposed)
            .map_err(net_build_error)?;
        Ok(Value::from_core(
            &self.core_values(),
            CoreValue::Net(NetValue::new(template.instantiate_shared())),
        ))
    }

    // TODO: add reflection snapshots and event subscriptions here. Reflection
    // producers should feed the same bounded history rather than print.

    /// Demands an ordinary atom-path accessor and rejects an undefined result.
    ///
    /// This is a presence-oriented compatibility helper. New code which wants
    /// ordinary Glam access semantics should compose [`Values::access`]
    /// directly, where an absent member evaluates to `{}`.
    pub fn get(&self, root: &Value, path: &str) -> Result<Value, Error> {
        self.get_optional(root, path)?
            .ok_or_else(|| Error::new(format!("module did not define `{path}`")))
    }

    /// Returns a value at an atom path, distinguishing an absent path from a
    /// failure while demanding an intermediate value.
    ///
    /// This presence-oriented compatibility helper uses generic WHNF client
    /// demand for each intermediate container but deliberately leaves the
    /// final member lazy. New semantic code should construct
    /// [`Values::access`] instead.
    pub fn get_optional(&self, root: &Value, path: &str) -> Result<Option<Value>, Error> {
        root.require_runtime(self.reasoning.runtime.id())?;
        let values = self.core_values();
        let mut current = root.as_core().clone();
        for part in path.split('.') {
            let evaluated = self
                .eval_context()
                .evaluate_whnf(&current)
                .map_err(|error| {
                    self.evaluation_error(error.with_context(path_lookup_context(path)))
                })?;
            let CoreValue::Dict(dict) = evaluated else {
                return Ok(None);
            };
            let Some(next) = dict.get(&Key::atom_from_text(part)) else {
                return Ok(None);
            };
            current = next.clone();
        }
        Ok(Some(Value::from_core(&values, current)))
    }

    /// Applies the ordinary `anno 'binary` semantics and extracts host bytes.
    ///
    /// Invalid source values fail as structured evaluation errors produced by
    /// the annotation; byte extraction itself is the only host-side step.
    pub fn to_binary(&self, value: &Value) -> Result<Bytes, Error> {
        value.require_runtime(self.reasoning.runtime.id())?;
        let values = self.values();
        let binary = values.annotate(values.atom_from_text("binary"), value.clone())?;
        let evaluated = self.evaluate(&binary)?;
        match evaluated.as_core() {
            CoreValue::Binary(bytes) => Ok(bytes.clone()),
            other => Err(self.evaluation_error(EvaluationHalt::new(format!(
                "`binary` annotation returned {}, expected Binary",
                other.diagnostic_kind_name()
            )))),
        }
    }

    /// Extracts a byte range from compact binary data or a byte-valued list.
    /// Lazy list chunks are evaluated as required to locate the range.
    pub fn binary_slice(&self, value: &Value, range: Range<usize>) -> Result<Bytes, Error> {
        value.require_runtime(self.reasoning.runtime.id())?;
        self.core_value_binary_slice(value.as_core(), range, "value")
    }

    fn build_module(
        &self,
        module_path: Arc<[String]>,
        inputs: Vec<ModuleInput>,
        initial_definitions: Value,
    ) -> Result<BuiltModule, Error> {
        initial_definitions.require_runtime(self.reasoning.runtime.id())?;
        let session = Arc::new(Mutex::new(Vec::new()));
        let execution = Arc::new(CompilationExecution::new(&self.reasoning, session.clone())?);
        let result = self.build_module_inner(
            module_path,
            inputs,
            initial_definitions.into_core(),
            session.clone(),
            execution.clone(),
        );
        let execution_failed = execution.drain();
        let diagnostics = session
            .lock()
            .expect("build diagnostic mutex should not be poisoned")
            .clone();

        match (result, execution_failed) {
            (Ok(_), true) => {
                Err(Error::new("module macro reasoning failed").with_diagnostics(diagnostics))
            }
            (Ok(value), false) => Ok(BuiltModule {
                value: Value::from_core(self.reasoning.runtime.values().core(), value),
                diagnostics,
            }),
            (Err(error), _) => Err(error.with_diagnostics(diagnostics)),
        }
    }

    fn build_module_inner(
        &self,
        module_path: Arc<[String]>,
        inputs: Vec<ModuleInput>,
        mut definitions: CoreValue,
        session: Arc<Mutex<Vec<Diagnostic>>>,
        execution: Arc<CompilationExecution>,
    ) -> Result<CoreValue, Error> {
        let module_loader = self.module_loader(session.clone(), execution.clone());
        let binary_loader = self.binary_loader();
        let module_context = CompileContext::from_module_path_with_values(
            self.core_values(),
            module_path.iter().cloned(),
        )
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
                    execution: execution.clone(),
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
            .map_err(|error| self.evaluation_error(error))
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
            execution,
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
                let context = CompileContext::from_module_path_with_values(
                    self.core_values(),
                    module_path.iter().cloned(),
                )
                .with_importer_source(source.clone())
                .with_compilation_trace(trace.clone())
                .with_prior_defs(prior_defs)
                .with_final_defs(final_defs)
                .with_local_module_loader(module_loader)
                .with_local_binary_loader(binary_loader)
                .with_compilation_execution(execution)
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
                let context = CompileContext::from_module_path_with_values(
                    self.core_values(),
                    module_path.iter().cloned(),
                )
                .with_compilation_trace(trace.clone())
                .with_prior_defs(prior_defs)
                .with_final_defs(final_defs)
                .with_local_module_loader(module_loader)
                .with_local_binary_loader(binary_loader)
                .with_compilation_execution(execution)
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

    fn module_loader(
        &self,
        session: Arc<Mutex<Vec<Diagnostic>>>,
        execution: Arc<CompilationExecution>,
    ) -> ModuleLoader {
        let assembler = self.clone();
        Arc::new(move |args| assembler.load_local_module(args, session.clone(), execution.clone()))
    }

    fn binary_loader(&self) -> BinaryFileLoader {
        let assembler = self.clone();
        Arc::new(move |args| assembler.load_local_binary(args))
    }

    fn load_local_module(
        &self,
        args: ModuleLoadArgs,
        session: Arc<Mutex<Vec<Diagnostic>>>,
        execution: Arc<CompilationExecution>,
    ) -> Result<CoreValue, Arc<EvaluationFailure>> {
        let importer = args.importer_source.as_ref().ok_or_else(|| {
            import_failure(
                format!(
                    "local import `{}` cannot be loaded from a source without an import resolver",
                    args.request.as_str()
                ),
                args.request.as_str(),
                args.importer_trace.as_deref(),
                None,
            )
        })?;
        let source = Arc::new(importer.load_relative(&args.request).map_err(|error| {
            import_failure(
                format!(
                    "local import `{}` could not be loaded: {error}",
                    args.request.as_str()
                ),
                args.request.as_str(),
                args.importer_trace.as_deref(),
                Some(importer),
            )
        })?);
        let module_loader = self.module_loader(session.clone(), execution.clone());
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
        let context = CompileContext::from_module_path_with_values(
            self.core_values(),
            args.module_path.iter().cloned(),
        )
        .with_importer_source(source.clone())
        .with_compilation_trace(trace.clone())
        .with_prior_defs(args.prior_defs)
        .with_final_defs(args.final_defs)
        .with_local_module_loader(module_loader)
        .with_local_binary_loader(binary_loader)
        .with_compilation_execution(execution)
        .with_diagnostic_emitter(self.compile_diagnostic_emitter(
            trace.clone(),
            session,
            had_errors.clone(),
        ));
        let definitions = compile_source(source.bytes(), &context);

        if had_errors.load(Ordering::Relaxed) {
            Err(import_failure(
                format!(
                    "local import `{}` failed to compile",
                    source.identity().label()
                ),
                args.request.as_str(),
                Some(&trace),
                Some(&source),
            ))
        } else {
            Ok(definitions)
        }
    }

    fn load_local_binary(&self, args: BinaryLoadArgs) -> Result<CoreValue, Arc<EvaluationFailure>> {
        let importer = args.importer_source.as_ref().ok_or_else(|| {
            import_failure(
                format!(
                    "binary import `{}` cannot be loaded from a source without an import resolver",
                    args.request.as_str()
                ),
                args.request.as_str(),
                args.importer_trace.as_deref(),
                None,
            )
        })?;
        importer
            .load_relative(&args.request)
            .map(|artifact| CoreValue::Binary(artifact.bytes().clone()))
            .map_err(|error| {
                import_failure(
                    format!(
                        "binary import `{}` could not be loaded: {error}",
                        args.request.as_str()
                    ),
                    args.request.as_str(),
                    args.importer_trace.as_deref(),
                    Some(importer),
                )
            })
    }

    fn seal_module(&self, context: &CompileContext, definitions: &CoreValue) -> CoreValue {
        let CoreValue::Promised(final_defs) = context.final_defs() else {
            panic!("CompileContext.final_defs must be a promised value");
        };
        final_defs
            .set(definitions.clone())
            .expect("CompileContext.final_defs future must be unassigned");
        definitions.clone()
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
            CoreValue::List(list) => eval::list_output_bytes_range(
                &self.eval_context(),
                list,
                range.clone(),
                &format!("`{label}`"),
            )
            .map(|bytes| bytes.map(Bytes::from))
            .map_err(|error| self.evaluation_error(error))?,
            CoreValue::Lazy(_) | CoreValue::Promised(_) | CoreValue::Net(_) => {
                let value = eval::eval_value(&self.eval_context(), value).map_err(|error| {
                    self.evaluation_error(
                        error.with_context(eval::evaluation_context_frame("binary_extraction")),
                    )
                })?;
                return self.core_value_binary_slice(&value, range, label);
            }
            CoreValue::Atom(_)
            | CoreValue::Dict(_)
            | CoreValue::Number(_)
            | CoreValue::Function(_)
            | CoreValue::Builtin(_)
            | CoreValue::PartialBuiltin(_)
            | CoreValue::Metadata(_)
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
            let diagnostic =
                Diagnostic::from_compile(assembler.values().core(), &trace, severity, message);
            session
                .lock()
                .expect("build diagnostic mutex should not be poisoned")
                .push(diagnostic.clone());
            assembler.record_diagnostic(diagnostic);
        })
    }
}

fn reasoning_diagnostic(values: &CoreValueFactory, failure: &EvaluationFailure) -> Diagnostic {
    Diagnostic::from_emission(
        Severity::Error,
        Value::from_core(values, eval::failure_diagnostic_value_with(values, failure)),
    )
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
    use crate::core::{LazyValue, OpaqueValue, keys};
    use crate::evaluation::{EvaluationMachinePoll, EvaluationTaskMachine, EvaluationWaitPoll};

    struct FailedReasoningTask;

    fn access_path(assembler: &Assembler, root: &Value, path: &str) -> Result<Value, Error> {
        let values = assembler.values();
        let mut value = root.clone();
        for part in path.split('.') {
            value = values.access(&value, values.atom_from_text(part))?;
        }
        Ok(value)
    }

    fn binary_at(assembler: &Assembler, root: &Value, path: &str) -> Result<Bytes, Error> {
        assembler.to_binary(&access_path(assembler, root, path)?)
    }

    impl EvaluationTaskMachine for FailedReasoningTask {
        fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Failed(Arc::new(crate::core::EvaluationFailure::message(
                "public reasoning failure",
            )))
        }
    }

    fn test_compilation_trace(source: &str) -> CompilationTrace {
        let source =
            SourceArtifact::new(Bytes::from_static(b"source"), SourceIdentity::file(source));
        CompilationTrace::root(
            CompilationInvocationId::new(1),
            &source,
            Arc::from(["test".to_owned()]),
        )
    }

    fn definition_context(value: &CoreValue) -> Option<&Dict> {
        let CoreValue::Dict(frame) = value else {
            return None;
        };
        let CoreValue::Dict(context) = frame.get(&*keys::G)? else {
            return None;
        };
        Some(context)
    }

    fn diagnostic_contexts(assembler: &Assembler, diagnostic: &Diagnostic) -> Vec<CoreValue> {
        let emission = eval::eval_value(&assembler.eval_context(), diagnostic.emission().as_core())
            .expect("diagnostic emission should evaluate");
        let CoreValue::Dict(emission) = emission else {
            panic!("diagnostic emission should be a dictionary");
        };
        let message = eval::eval_value(
            &assembler.eval_context(),
            emission
                .get(&*keys::MSG)
                .expect("diagnostic should define msg"),
        )
        .expect("diagnostic msg should evaluate");
        let CoreValue::Dict(message) = message else {
            panic!("diagnostic msg should be a dictionary");
        };
        let contexts = eval::eval_value(
            &assembler.eval_context(),
            message
                .get(&*keys::CONTEXT)
                .expect("diagnostic msg should define context"),
        )
        .expect("diagnostic context should evaluate");
        let CoreValue::List(contexts) = contexts else {
            panic!("diagnostic context should be a list");
        };
        eval::list_to_value_items(&assembler.eval_context(), &contexts)
            .expect("diagnostic contexts should be concrete values")
    }

    #[test]
    fn runtimes_own_independent_local_identity_domains_and_value_factories() {
        let first = EvaluationRuntime::new(0).expect("first runtime should build");
        let second = EvaluationRuntime::new(0).expect("second runtime should build");
        assert_ne!(first.id(), second.id());

        let allocate_one_of_each = |runtime: &EvaluationRuntime| {
            let ids = &runtime.state.shared_resources.ids;
            (
                ids.evaluation_session().get(),
                ids.evaluation_task().unwrap().get(),
                ids.evaluation_wait().unwrap().get(),
                ids.deferred_value().get(),
                ids.reasoning_session().get(),
                ids.cli_invocation().get(),
                ids.input_endpoint().unwrap().get(),
                ids.output_endpoint().unwrap().get(),
                ids.delivery().unwrap().get(),
            )
        };
        assert_eq!(allocate_one_of_each(&first), allocate_one_of_each(&second));

        assert_eq!(first.values().runtime_id(), first.id());
        assert_eq!(second.values().runtime_id(), second.id());
        let assembler = Assembler::builder()
            .evaluation_runtime(first.clone())
            .build()
            .expect("assembler should retain its selected runtime");
        assert_eq!(assembler.values().runtime_id(), first.id());

        let first_values = first.values().core;
        let first_unit = first_values.unit();
        let first_lazy = LazyValue::deferred(&first_values, "first runtime", move |_| {
            Ok(first_unit.clone())
        });
        let second_values = second.values().core;
        let second_unit = second_values.unit();
        let second_lazy = LazyValue::deferred(&second_values, "second runtime", move |_| {
            Ok(second_unit.clone())
        });
        assert_eq!(first_lazy.id().get(), second_lazy.id().get());
    }

    #[test]
    fn runtime_shared_resources_do_not_retain_runtime_lifecycle_owners() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let runtime_id = runtime.id();
        let state = Arc::downgrade(&runtime.state);
        let coordinator = Arc::downgrade(&runtime.state.work);
        let executor = Arc::downgrade(&runtime.state.executor);
        let profile = Arc::downgrade(&runtime.default_reflection_profile);
        let resources = runtime.state.shared_resources.clone();
        let retained_resources = Arc::downgrade(&resources);

        drop(runtime);

        assert!(state.upgrade().is_none());
        assert!(coordinator.upgrade().is_none());
        assert!(executor.upgrade().is_none());
        assert!(profile.upgrade().is_none());
        assert!(resources.work.upgrade().is_none());
        assert_eq!(resources.id, runtime_id);
        assert_eq!(resources.values.core().runtime_id(), runtime_id);

        let before = resources.observations.current();
        let mutation = resources.mutation_admission.mutation_guard();
        publish_runtime_observation(&resources, mutation);
        assert!(resources.observations.current() > before);
        assert!(resources.ids.reasoning_session().get() > 0);
        let _snapshot = resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .reflection
            .snapshot();

        drop(resources);
        assert!(retained_resources.upgrade().is_none());
    }

    #[test]
    fn public_value_factories_reject_foreign_composite_members() {
        let owner = EvaluationRuntime::new(0).expect("owner runtime should build");
        let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
        let values = owner.values();
        let foreign_value = foreign.values().integer(7);

        assert!(values.list([foreign_value.clone()]).is_err());
        assert!(values.record([("value", foreign_value.clone())]).is_err());
        assert!(
            values
                .dictionary([(values.atom_from_text("key"), foreign_value.clone())])
                .is_err()
        );
        assert!(values.empty_object(foreign_value.clone()).is_err());
        assert!(
            values
                .access(&values.empty_record(), foreign_value.clone())
                .is_err()
        );
        assert!(values.access(&foreign_value, values.text("key")).is_err());
        assert!(
            values
                .annotate(foreign_value.clone(), values.empty_record())
                .is_err()
        );
        assert!(
            values
                .annotate(values.atom_from_text("binary"), foreign_value.clone())
                .is_err()
        );
        assert!(
            values
                .after_reflection(foreign_value.clone(), values.text("target"))
                .is_err()
        );
        assert!(
            values
                .after_reflection(values.empty_record(), foreign_value)
                .is_err()
        );
    }

    #[test]
    fn assembler_boundaries_reject_foreign_values_before_evaluation_or_storage() {
        let runtime = EvaluationRuntime::new(0).expect("owner runtime should build");
        let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
        let assembler = Assembler::builder()
            .evaluation_runtime(runtime.clone())
            .build()
            .expect("owner assembler should build");
        let foreign_value = foreign.values().text("foreign");

        assert!(assembler.evaluate(&foreign_value).is_err());
        assert!(
            assembler
                .apply(&assembler.values().integer(0), [foreign_value.clone()])
                .is_err()
        );
        assert!(assembler.get(&foreign_value, "member").is_err());
        assert!(assembler.get_optional(&foreign_value, "member").is_err());
        assert!(assembler.to_binary(&foreign_value).is_err());
        assert!(assembler.binary_slice(&foreign_value, 0..1).is_err());
        assert!(access_path(&assembler, &foreign_value, "member").is_err());
        assert!(assembler.create_volume(foreign_value.clone()).is_err());
        assert!(
            assembler
                .net(|builder| builder.data(foreign_value.clone()))
                .is_err()
        );
        assert!(
            assembler
                .module(["foreign_initial_definitions"])
                .initial_definitions(foreign.values().empty_record())
                .build()
                .is_err()
        );

        let (promise, resolver) = assembler.promise("foreign assignment");
        assert!(resolver.resolve(foreign_value.clone()).is_err());
        let CoreValue::Promised(unassigned) = promise.as_core() else {
            panic!("public promise should retain its core promise cell")
        };
        assert!(
            unassigned.assignment().is_none(),
            "rejecting a foreign value must not terminalize the promise"
        );
        assert!(
            assembler
                .evaluate(&promise)
                .expect_err("a rejected foreign resolution must leave the promise pending")
                .to_string()
                .contains("before initialization")
        );
        let (failed, resolver) = assembler.promise("foreign failure");
        assert!(resolver.fail(foreign_value).is_err());
        let CoreValue::Promised(unassigned) = failed.as_core() else {
            panic!("public promise should retain its core promise cell")
        };
        assert!(
            unassigned.assignment().is_none(),
            "rejecting a foreign failure must not terminalize the promise"
        );
    }

    #[test]
    fn runtime_event_boundaries_reject_foreign_converted_and_output_values() {
        let runtime = EvaluationRuntime::new(0).expect("owner runtime should build");
        let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
        let foreign_values = foreign.values();
        let input = runtime
            .input_endpoint(move |_: ()| Ok(foreign_values.integer(1)))
            .expect("input endpoint should register");
        assert!(input.sender().admit(()).is_err());

        let output = runtime
            .output_endpoint(decode_test_integer, |_: i64| Ok(()))
            .expect("output endpoint should register");
        let (_, mut events) = input_transaction(&runtime);
        assert!(
            events
                .write(&output.writer(), foreign.values().integer(2))
                .is_err()
        );
        assert!(!runtime.has_delivery_activity());
    }

    #[test]
    fn public_error_contexts_prepend_without_rewriting_the_message() {
        let assembler = Assembler::new();
        let values = assembler.values();
        let inner = values.record([("inner", values.text("first"))]).unwrap();
        let outer = values.record([("outer", values.text("second"))]).unwrap();

        let error = Error::new("original")
            .with_context(&assembler.values(), inner.clone())
            .unwrap()
            .with_context(&assembler.values(), outer.clone())
            .unwrap();

        assert_eq!(error.to_string(), "original");
        assert_eq!(
            diagnostic_contexts(&assembler, &error.diagnostic(&values).unwrap()),
            [outer.into_core(), inner.into_core()]
        );
    }

    #[test]
    fn binary_annotation_preserves_a_nested_failure_context() {
        let assembler = Assembler::new();
        let module = assembler
            .module(["binary_context"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "result = anno 'error {msg:{text:\"original\"}, detail:\"kept\"}\n",
                ),
            )
            .build()
            .expect("binary context fixture should compile");

        let error = binary_at(&assembler, module.value(), "result")
            .expect_err("binary observation should demand the failed definition");

        assert_eq!(error.to_string(), "original");
        let contexts =
            diagnostic_contexts(&assembler, &error.diagnostic(&assembler.values()).unwrap());
        assert!(
            contexts.first().and_then(definition_context).is_some(),
            "semantic binary conversion should preserve the target's source context without a host-only frame"
        );
        assert_eq!(
            assembler
                .get(
                    error.diagnostic(&assembler.values()).unwrap().emission(),
                    "detail",
                )
                .expect("ad hoc diagnostic fields should survive contextualization")
                .as_binary(),
            Some(b"kept".as_slice())
        );
    }

    #[test]
    fn path_lookup_contextualizes_only_demand_failures() {
        let assembler = Assembler::new();
        let root = Value::from_core(
            &assembler.core_values(),
            CoreValue::Dict(Dict::new_sync().insert(
                Key::atom_from_text("broken"),
                CoreValue::error(&assembler.core_values(), "path target failed"),
            )),
        );

        let error = assembler
            .get(&root, "broken.member")
            .expect_err("forcing an intermediate path value should fail");
        assert_eq!(error.to_string(), "path target failed");
        assert_eq!(
            diagnostic_contexts(&assembler, &error.diagnostic(&assembler.values()).unwrap()),
            [path_lookup_context("broken.member")]
        );

        assert!(
            assembler
                .get_optional(&root, "missing.member")
                .expect("an absent path should not be an evaluation failure")
                .is_none()
        );
    }

    #[test]
    fn semantic_binary_conversion_preserves_structured_failures() {
        let assembler = Assembler::new();
        let missing = binary_at(&assembler, &assembler.values().empty_record(), "missing")
            .expect_err("missing binary path should fail");
        assert!(missing.to_string().contains("requires a list or binary"));
        assert!(missing.structured_diagnostic().is_some());

        let invalid = assembler
            .to_binary(&assembler.values().integer(42))
            .expect_err("a number is not binary text data");
        assert!(invalid.to_string().contains("requires a list or binary"));
        assert!(invalid.structured_diagnostic().is_some());

        let invalid_item = assembler
            .to_binary(
                &assembler
                    .values()
                    .list([assembler.values().integer(256)])
                    .expect("invalid byte fixture should still be a list"),
            )
            .expect_err("an out-of-range list member is not binary text data");
        assert!(
            invalid_item
                .to_string()
                .contains("cannot encode number `256`")
        );
        assert!(invalid_item.structured_diagnostic().is_some());
    }

    #[test]
    fn reflection_environment_explicitly_projects_compilation_origins() {
        let assembler = Assembler::new();
        let trace = test_compilation_trace("/workspace/source.g");
        let origin = crate::diagnostic::opaque_compilation_origin(&trace);
        assert_eq!(
            Value::from_core(&assembler.core_values(), origin.clone()).kind(),
            ValueKind::Opaque
        );

        let inspect = assembler
            .get(&assembler.reflection_environment(), "glam.origin.inspect")
            .expect("the reflection environment should expose origin inspection");
        let projected = assembler
            .apply(
                &inspect,
                [Value::from_core(&assembler.core_values(), origin)],
            )
            .and_then(|value| assembler.evaluate(&value))
            .expect("the origin capability should inspect compilation origins");

        assert_eq!(projected.as_core(), &trace.origin_value());
    }

    #[test]
    fn public_values_describe_metadata_carriers_only_as_sealed() {
        let assembler = Assembler::new();
        let value = Value::from_core(
            &assembler.core_values(),
            CoreValue::metadata_carrier(CoreValue::binary_from_text("private trace")),
        );

        assert_eq!(value.kind(), ValueKind::Sealed);
        assert_eq!(format!("{value:?}"), "Value { kind: Sealed, .. }");
    }

    #[test]
    fn origin_inspection_rejects_unrelated_opaque_values() {
        let assembler = Assembler::new();
        let inspect = assembler
            .get(&assembler.reflection_environment(), "glam.origin.inspect")
            .expect("the reflection environment should expose origin inspection");
        let unrelated = Value::from_core(
            &assembler.core_values(),
            CoreValue::Opaque(OpaqueValue::new(Arc::new(42_u64))),
        );

        let error = assembler
            .apply(&inspect, [unrelated])
            .and_then(|value| assembler.evaluate(&value))
            .expect_err("unrelated opaque values must not be disclosed");
        assert!(
            error
                .to_string()
                .contains("origin inspection requires an opaque compilation origin"),
            "{error}"
        );
    }

    #[test]
    fn source_definitions_add_shallow_opaque_origin_context() {
        let assembler = Assembler::new();
        let module = assembler
            .module(["definition_context"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "broken = 1 / 0\n",
                    "later x = x / 0\n",
                    "object container with\n",
                    "  broken = 1 / 0\n",
                    "manual = anno context:{manual:module_origin} (1 / 0)\n",
                ),
            )
            .build()
            .expect("definition context fixture should compile");

        let broken = assembler
            .get(module.value(), "broken")
            .expect("fixture should define broken");
        let error = eval::eval_value(&assembler.eval_context(), broken.as_core())
            .expect_err("the broken definition should fail");
        let failure = error.into_permanent_failure();
        let context = failure
            .contexts()
            .iter()
            .find_map(definition_context)
            .expect("definition initialization should carry source context");
        assert_eq!(
            context.get(&*keys::DEFINITION),
            Some(&CoreValue::binary_from_text("broken"))
        );
        assert_eq!(
            context.get(&*keys::LINE),
            Some(&CoreValue::Number(Number::from_usize(3)))
        );
        let automatic_origin = context
            .get(&*keys::ORIGIN)
            .expect("source context should contain an origin")
            .clone();
        assert!(
            matches!(&automatic_origin, CoreValue::Opaque(_)),
            "source origins should remain opaque until a reflection capability inspects them"
        );

        let later = assembler
            .get(module.value(), "later")
            .expect("fixture should define later");
        let call = assembler
            .apply(&later, [assembler.values().integer(1)])
            .expect("calling a source function should remain lazy");
        let error = eval::eval_value(&assembler.eval_context(), call.as_core())
            .expect_err("the function body should fail when called");
        let failure = error.into_permanent_failure();
        assert!(
            failure
                .contexts()
                .iter()
                .all(|context| definition_context(context).is_none()),
            "shallow definition context must not capture arguments or follow later calls"
        );

        let object_member = assembler
            .get(module.value(), "container.broken")
            .expect("fixture should define the nested object member");
        let error = eval::eval_value(&assembler.eval_context(), object_member.as_core())
            .expect_err("the nested object member should fail");
        let failure = error.into_permanent_failure();
        let context = failure
            .contexts()
            .iter()
            .find_map(definition_context)
            .expect("object member initialization should carry source context");
        assert_eq!(
            context.get(&*keys::DEFINITION),
            Some(&CoreValue::binary_from_text("broken"))
        );
        assert_eq!(
            context.get(&*keys::LINE),
            Some(&CoreValue::Number(Number::from_usize(6)))
        );

        let manual = assembler
            .get(module.value(), "manual")
            .expect("fixture should define a manual context");
        let error = eval::eval_value(&assembler.eval_context(), manual.as_core())
            .expect_err("the manually contextualized expression should fail");
        let failure = error.into_permanent_failure();
        let manual_origin = failure.contexts().iter().find_map(|frame| {
            let CoreValue::Dict(frame) = eval::eval_value(&assembler.eval_context(), frame).ok()?
            else {
                return None;
            };
            frame.get(&Key::atom_from_text("manual")).cloned()
        });
        assert_eq!(
            manual_origin.as_ref(),
            Some(&automatic_origin),
            "module_origin should expose the same opaque token used by automatic frames; contexts: {:?}",
            failure.contexts()
        );
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
            .reflection_environment(|environment| {
                let values = environment.values();
                values.record([("client", values.text("new environment"))])
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
    fn builder_environment_promise_can_resolve_after_early_observation() {
        let mut resolver = None;
        let assembler = Assembler::builder()
            .reflection_environment(|environment| {
                let (value, promise_resolver) = environment.promise("late environment value");
                resolver = Some(promise_resolver);
                environment.values().record([("late", value)])
            })
            .expect("environment should build")
            .build()
            .expect("assembler should build");
        let promised = assembler
            .get(&assembler.reflection_environment(), "late")
            .expect("promise should be present");

        assert!(assembler.evaluate(&promised).is_err());
        resolver
            .take()
            .expect("resolver should escape the builder")
            .resolve(assembler.values().text("ready"))
            .expect("promise should resolve once");
        assert_eq!(
            assembler
                .evaluate(&promised)
                .expect("resolved promise should evaluate")
                .as_binary(),
            Some(b"ready".as_slice())
        );
    }

    #[test]
    fn dropped_builder_environment_resolver_fails_its_promise() {
        let assembler = Assembler::builder()
            .reflection_environment(|environment| {
                let (value, resolver) = environment.promise("abandoned environment value");
                drop(resolver);
                environment.values().record([("abandoned", value)])
            })
            .expect("environment should build")
            .build()
            .expect("assembler should build");
        let promised = assembler
            .get(&assembler.reflection_environment(), "abandoned")
            .expect("promise should be present");

        assert!(
            assembler
                .evaluate(&promised)
                .expect_err("dropped resolver must fail its promise")
                .to_string()
                .contains("was dropped before completion")
        );
    }

    #[test]
    fn builder_environment_promise_does_not_complete_through_self_dependency() {
        let mut resolver = None;
        let assembler = Assembler::builder()
            .reflection_environment(|environment| {
                let (value, promise_resolver) = environment.promise("self-dependent value");
                resolver = Some(promise_resolver);
                environment.values().record([("self", value)])
            })
            .expect("environment should build")
            .build()
            .expect("assembler should build");
        let promised = assembler
            .get(&assembler.reflection_environment(), "self")
            .expect("promise should be present");
        resolver
            .take()
            .expect("resolver should escape the builder")
            .resolve(promised.clone())
            .expect("the host may assign a self-dependent value");

        let error = assembler
            .evaluate(&promised)
            .expect_err("self dependency cannot reach weak head normal form");
        assert!(
            error.to_string().contains("blocked on wait token"),
            "{error}"
        );
    }

    #[test]
    fn evaluation_runtime_workers_activate_only_once() {
        let runtime = EvaluationRuntime::new(0).expect("dormant runtime should build");
        assert_eq!(runtime.worker_threads(), 0);
        runtime
            .activate_workers(1)
            .expect("dormant runtime should activate");
        assert_eq!(runtime.worker_threads(), 1);
        assert!(runtime.activate_workers(1).is_err());
    }

    #[test]
    fn evaluation_runtime_ids_are_process_unique() {
        let first = EvaluationRuntime::new(0).expect("first runtime should build");
        let second = EvaluationRuntime::new(0).expect("second runtime should build");

        assert_ne!(first.id(), second.id());
    }

    fn input_transaction(
        runtime: &EvaluationRuntime,
    ) -> (crate::reflection::StoreJournal, RuntimeEventJournal) {
        let (_, store, events) = runtime.transaction_snapshot();
        (
            crate::reflection::StoreJournal::new(store),
            RuntimeEventJournal::new(events),
        )
    }

    fn integer_converter(
        runtime: &EvaluationRuntime,
    ) -> impl Fn(i64) -> Result<Value, Error> + Send + Sync + 'static {
        let values = runtime.values();
        move |value| Ok(values.integer(value))
    }

    #[test]
    fn runtime_input_endpoints_are_local_monotonic_capabilities() {
        let owner = EvaluationRuntime::new(0).expect("owner runtime should build");
        let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
        let first = owner
            .input_endpoint(integer_converter(&owner))
            .expect("first endpoint should register");
        let second = owner
            .input_endpoint(integer_converter(&owner))
            .expect("second endpoint should register");

        assert_eq!(second.reader().id().get(), first.reader().id().get() + 1);
        assert_eq!(first.sender().id(), first.reader().id());

        let (_, _, snapshot) = foreign.transaction_snapshot();
        let mut journal = RuntimeEventJournal::new(snapshot);
        let error = journal
            .read(&first.reader())
            .expect_err("an input capability must reject a foreign runtime");
        assert!(error.to_string().contains("belongs to evaluation runtime"));
    }

    #[test]
    fn runtime_input_conversion_precedes_admission_and_stores_only_roots() {
        struct HostPayload(Arc<()>);

        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let converter_runtime = runtime.clone();
        let values = runtime.values();
        let endpoint = runtime
            .input_endpoint(move |payload: HostPayload| {
                assert!(converter_runtime.exclusive_admission_available());
                let HostPayload(lease) = payload;
                drop(lease);
                Ok(values.text("rooted"))
            })
            .expect("endpoint should register");
        let host_payload = Arc::new(());
        let retained = Arc::downgrade(&host_payload);

        let sequence = endpoint
            .sender()
            .admit(HostPayload(host_payload))
            .expect("input should be admitted");
        assert_eq!(sequence.get(), 0);
        assert!(retained.upgrade().is_none());

        let state = runtime
            .state
            .shared_resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        let record = state
            .events
            .inputs
            .get(&endpoint.reader().id())
            .expect("endpoint should remain registered")
            .admitted
            .front()
            .expect("the converted root should be buffered");
        assert_eq!(record.payload.runtime_id(), runtime.id());
        assert_eq!(
            record.payload.value(runtime.id()).as_binary(),
            Some(b"rooted".as_slice())
        );
    }

    #[test]
    fn failed_runtime_input_conversion_publishes_nothing() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let endpoint = runtime
            .input_endpoint(|_: ()| -> Result<Value, Error> { Err(Error::new("rejected")) })
            .expect("endpoint should register");
        let (generation, _, before) = runtime.transaction_snapshot();

        assert!(endpoint.sender().admit(()).is_err());

        let (after_generation, _, after) = runtime.transaction_snapshot();
        assert_eq!(after_generation, generation);
        assert_eq!(after.revision, before.revision);
        let mut journal = RuntimeEventJournal::new(after);
        assert_eq!(
            journal
                .read(&endpoint.reader())
                .expect("empty endpoint should be readable"),
            None
        );
    }

    #[test]
    fn runtime_input_identity_exhaustion_changes_no_state() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let endpoint = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("endpoint should register");
        {
            let mut state = runtime
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            let input = Arc::make_mut(
                state
                    .events
                    .inputs
                    .get_mut(&endpoint.reader().id())
                    .expect("endpoint should remain registered"),
            );
            input.head_sequence = RuntimeInputSequence::from_u64(u64::MAX);
            input.next_sequence = RuntimeInputSequence::from_u64(u64::MAX);
        }
        let (generation, _, before) = runtime.transaction_snapshot();

        assert!(endpoint.sender().admit(1).is_err());

        let (after_generation, _, after) = runtime.transaction_snapshot();
        assert_eq!(after_generation, generation);
        assert_eq!(after.revision, before.revision);
        assert!(
            after
                .inputs
                .get(&endpoint.reader().id())
                .expect("endpoint should remain registered")
                .admitted
                .is_empty()
        );

        let endpoint_count = after.inputs.len();
        runtime.state.shared_resources.ids.exhaust_input_endpoints();
        assert!(runtime.input_endpoint(integer_converter(&runtime)).is_err());
        let (_, _, after_id_failure) = runtime.transaction_snapshot();
        assert_eq!(after_id_failure.inputs.len(), endpoint_count);
    }

    #[test]
    fn runtime_input_reads_and_commits_a_fifo_prefix() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let endpoint = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("endpoint should register");
        assert_eq!(endpoint.sender().admit(10).unwrap().get(), 0);
        assert_eq!(endpoint.sender().admit(20).unwrap().get(), 1);
        let (store, mut events) = input_transaction(&runtime);

        assert_eq!(
            events
                .read(&endpoint.reader())
                .unwrap()
                .and_then(|value| value.as_number_text()),
            Some("10".to_owned())
        );
        assert_eq!(
            events
                .read(&endpoint.reader())
                .unwrap()
                .and_then(|value| value.as_number_text()),
            Some("20".to_owned())
        );
        assert_eq!(events.read(&endpoint.reader()).unwrap(), None);
        assert_eq!(
            runtime.try_commit_transaction(&store, &events),
            crate::reflection::StoreCommitResult::Committed
        );

        let (_, mut empty) = input_transaction(&runtime);
        assert_eq!(empty.read(&endpoint.reader()).unwrap(), None);
    }

    #[test]
    fn empty_runtime_input_observation_is_stable_and_precise() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let left = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("left endpoint should register");
        let right = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("right endpoint should register");
        let (unrelated_store, mut unrelated_events) = input_transaction(&runtime);
        assert_eq!(unrelated_events.read(&left.reader()).unwrap(), None);
        assert_eq!(unrelated_events.read(&left.reader()).unwrap(), None);

        right.sender().admit(1).expect("right input should admit");
        assert_eq!(
            runtime.try_commit_transaction(&unrelated_store, &unrelated_events),
            crate::reflection::StoreCommitResult::Committed
        );

        let (stale_store, mut stale_events) = input_transaction(&runtime);
        assert_eq!(stale_events.read(&left.reader()).unwrap(), None);
        left.sender().admit(2).expect("left input should admit");
        assert_eq!(
            runtime.try_commit_transaction(&stale_store, &stale_events),
            crate::reflection::StoreCommitResult::Conflict
        );
    }

    #[test]
    fn runtime_input_uses_the_configured_conflict_strategy() {
        let strategies: [Arc<dyn ConflictAnalysisStrategy>; 3] = [
            Arc::new(crate::reflection::ExactConflictAnalysis),
            Arc::new(crate::reflection::FingerprintConflictAnalysis),
            Arc::new(crate::reflection::CoarseConflictAnalysis),
        ];
        for strategy in strategies {
            let runtime = EvaluationRuntime::with_conflict_analysis(0, strategy)
                .expect("runtime should build");
            let endpoint = runtime
                .input_endpoint(integer_converter(&runtime))
                .expect("endpoint should register");
            let (store, mut events) = input_transaction(&runtime);
            assert_eq!(events.read(&endpoint.reader()).unwrap(), None);
            endpoint.sender().admit(1).expect("input should admit");
            assert_eq!(
                runtime.try_commit_transaction(&store, &events),
                crate::reflection::StoreCommitResult::Conflict
            );
        }
    }

    #[test]
    fn competing_runtime_input_consumers_conflict_but_independent_ones_commit() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let left = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("left endpoint should register");
        let right = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("right endpoint should register");
        left.sender().admit(1).unwrap();
        right.sender().admit(2).unwrap();
        let (_, store, events) = runtime.transaction_snapshot();
        let left_store = crate::reflection::StoreJournal::new(store.clone());
        let right_store = crate::reflection::StoreJournal::new(store.clone());
        let competing_store = crate::reflection::StoreJournal::new(store);
        let mut left_events = RuntimeEventJournal::new(events.clone());
        let mut right_events = RuntimeEventJournal::new(events.clone());
        let mut competing_events = RuntimeEventJournal::new(events);
        assert!(left_events.read(&left.reader()).unwrap().is_some());
        assert!(right_events.read(&right.reader()).unwrap().is_some());
        assert!(competing_events.read(&left.reader()).unwrap().is_some());

        assert_eq!(
            runtime.try_commit_transaction(&left_store, &left_events),
            crate::reflection::StoreCommitResult::Committed
        );
        assert_eq!(
            runtime.try_commit_transaction(&right_store, &right_events),
            crate::reflection::StoreCommitResult::Committed
        );
        assert_eq!(
            runtime.try_commit_transaction(&competing_store, &competing_events),
            crate::reflection::StoreCommitResult::Conflict
        );
    }

    #[test]
    fn abandoned_runtime_input_claim_does_not_consume() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let endpoint = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("endpoint should register");
        endpoint.sender().admit(7).unwrap();
        {
            let (_, mut abandoned) = input_transaction(&runtime);
            assert!(abandoned.read(&endpoint.reader()).unwrap().is_some());
        }
        let (store, mut events) = input_transaction(&runtime);
        assert_eq!(
            events
                .read(&endpoint.reader())
                .unwrap()
                .and_then(|value| value.as_number_text()),
            Some("7".to_owned())
        );
        assert_eq!(
            runtime.try_commit_transaction(&store, &events),
            crate::reflection::StoreCommitResult::Committed
        );
    }

    #[test]
    fn combined_heap_conflict_rolls_back_runtime_input_consumption() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let endpoint = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("endpoint should register");
        endpoint.sender().admit(9).unwrap();
        let (_, store_snapshot, event_snapshot) = runtime.transaction_snapshot();
        let mut combined_store = crate::reflection::StoreJournal::new(store_snapshot.clone());
        combined_store.observe_read(&[Key::atom_from_text("combined")]);
        combined_store.write(
            vec![Key::atom_from_text("combined")],
            runtime.values().text("stale"),
        );
        let mut combined_events = RuntimeEventJournal::new(event_snapshot);
        assert!(combined_events.read(&endpoint.reader()).unwrap().is_some());

        let mut winner = crate::reflection::StoreJournal::new(store_snapshot);
        winner.write(
            vec![Key::atom_from_text("combined")],
            runtime.values().text("winner"),
        );
        assert_eq!(
            runtime.commit_reflection(&winner),
            crate::reflection::StoreCommitResult::Committed
        );
        assert_eq!(
            runtime.try_commit_transaction(&combined_store, &combined_events),
            crate::reflection::StoreCommitResult::Conflict
        );

        let (_, mut retry) = input_transaction(&runtime);
        assert_eq!(
            retry
                .read(&endpoint.reader())
                .unwrap()
                .and_then(|value| value.as_number_text()),
            Some("9".to_owned())
        );
    }

    #[test]
    fn runtime_input_admission_wakes_after_releasing_mutation_admission() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let endpoint = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("endpoint should register");
        let (generation, _, _) = runtime.transaction_snapshot();
        let waiting_runtime = runtime.clone();
        let waiter = std::thread::spawn(move || {
            assert!(waiting_runtime.wait_for_change(generation));
            assert!(waiting_runtime.exclusive_admission_available());
        });

        endpoint.sender().admit(1).expect("input should admit");
        waiter.join().expect("broad observer should wake cleanly");
    }

    #[test]
    fn runtime_pump_waits_for_in_flight_mutation_admission() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let activity = runtime.state.shared_resources.mutation_admission.activity();
        let mutation = runtime.mutation_guard();
        assert!(matches!(runtime.readiness(), RuntimeReadiness::Busy));
        let pumping_runtime = runtime.clone();
        let (finished, observed) = std::sync::mpsc::channel();
        let pump = std::thread::spawn(move || {
            pumping_runtime.pump_until_stable();
            finished.send(()).expect("pump receiver should remain live");
        });

        assert!(
            observed
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "an in-flight guarded publication must prevent a stable pump result"
        );
        drop(mutation);
        observed
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("releasing mutation admission should wake the runtime pump");
        pump.join().expect("runtime pump should finish cleanly");
        assert!(
            activity.wait_count() > 0,
            "in-flight admission should park the pump on runtime activity"
        );
    }

    #[test]
    fn runtime_activity_cannot_lose_a_wake_before_parking() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let activity = runtime.state.shared_resources.mutation_admission.activity();
        let observed = activity.current();

        // Publish after the pump-like snapshot but before its wait call.
        drop(runtime.mutation_guard());
        let (finished, wake_observed) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            activity.wait_for_change(observed);
            finished.send(()).expect("wake receiver should remain live");
        });
        wake_observed
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("a publication between classification and parking must not be lost");
        waiter
            .join()
            .expect("activity waiter should finish cleanly");
    }

    #[test]
    fn readiness_stamp_tracks_heap_query_and_event_observations() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let RuntimeReadiness::Ready(initial) = runtime.readiness() else {
            panic!("new runtime should be ready")
        };

        let (_, heap_snapshot) = runtime.reflection_snapshot();
        let mut heap = crate::reflection::StoreJournal::new(heap_snapshot);
        heap.write(
            vec![Key::atom_from_text("readiness_root")],
            runtime.values().text("installed"),
        );
        assert_eq!(
            runtime.commit_reflection(&heap),
            crate::reflection::StoreCommitResult::Committed
        );
        let RuntimeReadiness::Ready(after_heap) = runtime.readiness() else {
            panic!("heap state without work should remain ready")
        };
        assert!(after_heap.stamp().observation_epoch() > initial.stamp().observation_epoch());
        assert_ne!(after_heap.reflection().root(), initial.reflection().root());

        let input = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("input endpoint should register");
        input.sender().admit(1).expect("input should admit");
        let RuntimeReadiness::Ready(after_input) = runtime.readiness() else {
            panic!("unused buffered input is state rather than activity")
        };
        assert!(after_input.stamp().observation_epoch() > after_heap.stamp().observation_epoch());
        assert_eq!(
            after_input.reflection().root(),
            after_heap.reflection().root()
        );

        let (_, query_snapshot) = runtime.reflection_snapshot();
        let mut query_reservation = crate::reflection::StoreJournal::new(query_snapshot);
        let query = query_reservation
            .reserve_query()
            .expect("query should reserve");
        assert_eq!(
            runtime.commit_reflection(&query_reservation),
            crate::reflection::StoreCommitResult::Committed
        );
        let RuntimeReadiness::Ready(before_query_update) = runtime.readiness() else {
            panic!("pending protected query is state rather than scheduler work")
        };
        runtime
            .update_query(&query, runtime.values().integer(0))
            .expect("protected query should update");
        let RuntimeReadiness::Ready(after_query_update) = runtime.readiness() else {
            panic!("completed protected query without work should remain ready")
        };
        assert!(
            after_query_update.stamp().observation_epoch()
                > before_query_update.stamp().observation_epoch()
        );

        assert_eq!(
            initial.stamp().work_generation(),
            after_heap.stamp().work_generation()
        );
        assert_eq!(
            after_heap.stamp().work_generation(),
            after_input.stamp().work_generation()
        );
        assert_eq!(
            after_input.stamp().work_generation(),
            after_query_update.stamp().work_generation()
        );
    }

    fn decode_test_integer(value: Value) -> Result<i64, Error> {
        value
            .as_number_text()
            .and_then(|text| text.parse().ok())
            .ok_or_else(|| Error::new("integer output expected"))
    }

    #[test]
    fn abandoned_output_intents_burn_ids_without_publishing_work() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
        let endpoint = runtime
            .output_endpoint(decode_test_integer, |_: i64| Ok(()))
            .expect("output endpoint should register");
        let next = runtime
            .output_endpoint(decode_test_integer, |_: i64| Ok(()))
            .expect("second output endpoint should register");
        assert_eq!(next.writer().id().get(), endpoint.writer().id().get() + 1);

        let (_, mut abandoned) = input_transaction(&runtime);
        let burned = abandoned
            .write(&endpoint.writer(), runtime.values().integer(1))
            .expect("intent should reserve an ID");
        drop(abandoned);
        assert!(!runtime.has_delivery_activity());
        assert!(endpoint.delivery().deliver_next().unwrap().is_none());

        let (_, _, foreign_snapshot) = foreign.transaction_snapshot();
        let mut foreign_events = RuntimeEventJournal::new(foreign_snapshot);
        assert!(
            foreign_events
                .write(&endpoint.writer(), foreign.values().integer(2))
                .is_err()
        );

        let (store, mut events) = input_transaction(&runtime);
        let committed = events
            .write(&endpoint.writer(), runtime.values().integer(3))
            .expect("second intent should reserve an ID");
        assert!(committed.get() > burned.get());
        assert_eq!(
            runtime.try_commit_transaction(&store, &events),
            crate::reflection::StoreCommitResult::Committed
        );
        assert!(runtime.has_delivery_activity());
    }

    #[test]
    fn runtime_pump_waits_for_running_output_delivery() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let activity = runtime.state.shared_resources.mutation_admission.activity();
        let release = Arc::new(std::sync::Barrier::new(2));
        let callback_release = release.clone();
        let (entered, callback_entered) = std::sync::mpsc::channel();
        let endpoint = runtime
            .output_endpoint(decode_test_integer, move |_: i64| {
                entered
                    .send(())
                    .expect("delivery observer should remain live");
                callback_release.wait();
                Ok(())
            })
            .expect("output endpoint should register");
        let (store, mut events) = input_transaction(&runtime);
        events
            .write(&endpoint.writer(), runtime.values().integer(1))
            .expect("output intent should reserve a delivery");
        assert_eq!(
            runtime.try_commit_transaction(&store, &events),
            crate::reflection::StoreCommitResult::Committed
        );
        assert!(matches!(runtime.readiness(), RuntimeReadiness::Busy));

        let delivery = endpoint.delivery();
        let delivery_thread = std::thread::spawn(move || delivery.deliver_next().unwrap());
        callback_entered
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("delivery callback should begin");
        assert!(matches!(runtime.readiness(), RuntimeReadiness::Busy));

        let pumping_runtime = runtime.clone();
        let (finished, pump_finished) = std::sync::mpsc::channel();
        let pump = std::thread::spawn(move || {
            pumping_runtime.pump_until_stable();
            finished.send(()).expect("pump receiver should remain live");
        });
        assert!(
            pump_finished
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "a running delivery must keep the runtime pump parked"
        );

        release.wait();
        assert!(delivery_thread.join().unwrap().is_some());
        pump_finished
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("delivery terminalization should wake the runtime pump");
        pump.join().expect("runtime pump should finish cleanly");
        assert!(!runtime.has_delivery_activity());
        assert!(matches!(runtime.readiness(), RuntimeReadiness::Ready(_)));
        assert!(
            activity.wait_count() > 0,
            "running delivery should park the pump rather than busy-polling"
        );
    }

    #[test]
    fn output_identity_exhaustion_changes_no_runtime_state() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let endpoint = runtime
            .output_endpoint(decode_test_integer, |_: i64| Ok(()))
            .expect("output endpoint should register");
        runtime.state.shared_resources.ids.exhaust_deliveries();
        let (_, mut events) = input_transaction(&runtime);
        assert!(
            events
                .write(&endpoint.writer(), runtime.values().integer(1))
                .is_err()
        );
        assert!(!runtime.has_delivery_activity());

        let endpoint_count = runtime
            .state
            .shared_resources
            .transactions
            .state
            .lock()
            .unwrap()
            .events
            .outputs
            .ready_by_endpoint
            .len();
        runtime
            .state
            .shared_resources
            .ids
            .exhaust_output_endpoints();
        assert!(
            runtime
                .output_endpoint(decode_test_integer, |_: i64| Ok(()))
                .is_err()
        );
        assert_eq!(
            runtime
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .unwrap()
                .events
                .outputs
                .ready_by_endpoint
                .len(),
            endpoint_count
        );
    }

    #[test]
    fn combined_heap_conflict_rolls_back_output_admission() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let endpoint = runtime
            .output_endpoint(decode_test_integer, |_: i64| Ok(()))
            .expect("output endpoint should register");
        let (_, store_snapshot, event_snapshot) = runtime.transaction_snapshot();
        let mut combined_store = crate::reflection::StoreJournal::new(store_snapshot.clone());
        combined_store.observe_read(&[Key::atom_from_text("output_atomic")]);
        combined_store.write(
            vec![Key::atom_from_text("output_atomic")],
            runtime.values().text("stale"),
        );
        let mut combined_events = RuntimeEventJournal::new(event_snapshot);
        combined_events
            .write(&endpoint.writer(), runtime.values().integer(1))
            .unwrap();

        let mut winner = crate::reflection::StoreJournal::new(store_snapshot);
        winner.write(
            vec![Key::atom_from_text("output_atomic")],
            runtime.values().text("winner"),
        );
        assert_eq!(
            runtime.commit_reflection(&winner),
            crate::reflection::StoreCommitResult::Committed
        );
        assert_eq!(
            runtime.try_commit_transaction(&combined_store, &combined_events),
            crate::reflection::StoreCommitResult::Conflict
        );
        assert!(!runtime.has_delivery_activity());
        assert!(endpoint.delivery().deliver_next().unwrap().is_none());
    }

    #[test]
    fn output_claim_is_unique_and_callbacks_run_outside_runtime_guards() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let decode_runtime = runtime.clone();
        let adapter_runtime = runtime.clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let callback_barrier = barrier.clone();
        let (entered, waiting) = std::sync::mpsc::channel();
        let endpoint = runtime
            .output_endpoint(
                move |value| {
                    assert!(decode_runtime.exclusive_admission_available());
                    decode_test_integer(value)
                },
                move |_: i64| {
                    assert!(adapter_runtime.exclusive_admission_available());
                    entered.send(()).expect("test receiver should remain live");
                    callback_barrier.wait();
                    Ok(())
                },
            )
            .expect("output endpoint should register");
        let (store, mut events) = input_transaction(&runtime);
        let delivery = events
            .write(&endpoint.writer(), runtime.values().integer(1))
            .unwrap();
        assert_eq!(
            runtime.try_commit_transaction(&store, &events),
            crate::reflection::StoreCommitResult::Committed
        );
        let worker_delivery = endpoint.delivery();
        let worker = std::thread::spawn(move || worker_delivery.deliver_next().unwrap());
        waiting.recv().expect("callback should begin");

        assert!(runtime.has_delivery_activity());
        assert!(endpoint.delivery().deliver_next().unwrap().is_none());
        barrier.wait();
        assert!(matches!(
            worker.join().expect("delivery thread should finish"),
            Some(RuntimeDeliveryOutcome::Delivered(id)) if id == delivery
        ));
        assert!(!runtime.has_delivery_activity());
    }

    #[test]
    fn output_delivery_preserves_endpoint_order_and_allows_endpoint_concurrency() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let ordered = Arc::new(Mutex::new(Vec::new()));
        let ordered_sink = ordered.clone();
        let sequential = runtime
            .output_endpoint(decode_test_integer, move |value| {
                ordered_sink.lock().unwrap().push(value);
                Ok(())
            })
            .expect("sequential endpoint should register");
        let (store, mut events) = input_transaction(&runtime);
        events
            .write(&sequential.writer(), runtime.values().integer(1))
            .unwrap();
        events
            .write(&sequential.writer(), runtime.values().integer(2))
            .unwrap();
        assert_eq!(
            runtime.try_commit_transaction(&store, &events),
            crate::reflection::StoreCommitResult::Committed
        );
        assert!(sequential.delivery().deliver_next().unwrap().is_some());
        assert!(sequential.delivery().deliver_next().unwrap().is_some());
        assert_eq!(*ordered.lock().unwrap(), [1, 2]);

        let barrier = Arc::new(std::sync::Barrier::new(3));
        let left_barrier = barrier.clone();
        let right_barrier = barrier.clone();
        let left = runtime
            .output_endpoint(decode_test_integer, move |_: i64| {
                left_barrier.wait();
                Ok(())
            })
            .expect("left endpoint should register");
        let right = runtime
            .output_endpoint(decode_test_integer, move |_: i64| {
                right_barrier.wait();
                Ok(())
            })
            .expect("right endpoint should register");
        let (store, mut events) = input_transaction(&runtime);
        events
            .write(&left.writer(), runtime.values().integer(3))
            .unwrap();
        events
            .write(&right.writer(), runtime.values().integer(4))
            .unwrap();
        assert_eq!(
            runtime.try_commit_transaction(&store, &events),
            crate::reflection::StoreCommitResult::Committed
        );
        let left_delivery = left.delivery();
        let right_delivery = right.delivery();
        let left_worker = std::thread::spawn(move || left_delivery.deliver_next().unwrap());
        let right_worker = std::thread::spawn(move || right_delivery.deliver_next().unwrap());
        barrier.wait();
        assert!(left_worker.join().unwrap().is_some());
        assert!(right_worker.join().unwrap().is_some());
        assert!(!runtime.has_delivery_activity());
    }

    #[test]
    fn output_delivery_orders_by_commit_not_reservation() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let sink = delivered.clone();
        let endpoint = runtime
            .output_endpoint(decode_test_integer, move |value| {
                sink.lock().unwrap().push(value);
                Ok(())
            })
            .expect("output endpoint should register");
        let (_, store_snapshot, event_snapshot) = runtime.transaction_snapshot();
        let first_store = crate::reflection::StoreJournal::new(store_snapshot.clone());
        let second_store = crate::reflection::StoreJournal::new(store_snapshot);
        let mut first_events = RuntimeEventJournal::new(event_snapshot.clone());
        let mut second_events = RuntimeEventJournal::new(event_snapshot);
        first_events
            .write(&endpoint.writer(), runtime.values().integer(1))
            .unwrap();
        second_events
            .write(&endpoint.writer(), runtime.values().integer(2))
            .unwrap();

        assert_eq!(
            runtime.try_commit_transaction(&second_store, &second_events),
            crate::reflection::StoreCommitResult::Committed
        );
        assert_eq!(
            runtime.try_commit_transaction(&first_store, &first_events),
            crate::reflection::StoreCommitResult::Committed
        );
        assert!(endpoint.delivery().deliver_next().unwrap().is_some());
        assert!(endpoint.delivery().deliver_next().unwrap().is_some());
        assert_eq!(*delivered.lock().unwrap(), [2, 1]);
    }

    #[test]
    fn cloned_output_intent_cannot_republish_a_terminal_delivery_id() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let endpoint = runtime
            .output_endpoint(decode_test_integer, |_: i64| Ok(()))
            .expect("output endpoint should register");
        let (_, store_snapshot, event_snapshot) = runtime.transaction_snapshot();
        let first_store = crate::reflection::StoreJournal::new(store_snapshot.clone());
        let replay_store = crate::reflection::StoreJournal::new(store_snapshot);
        let mut first_events = RuntimeEventJournal::new(event_snapshot);
        first_events
            .write(&endpoint.writer(), runtime.values().integer(1))
            .unwrap();
        let replay_events = first_events.clone();

        assert_eq!(
            runtime.try_commit_transaction(&first_store, &first_events),
            crate::reflection::StoreCommitResult::Committed
        );
        assert!(endpoint.delivery().deliver_next().unwrap().is_some());
        assert_eq!(
            runtime.try_commit_transaction(&replay_store, &replay_events),
            crate::reflection::StoreCommitResult::Conflict
        );
        assert!(!runtime.has_delivery_activity());
    }

    #[test]
    fn output_failures_are_terminal_durable_and_acknowledgeable() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let decode = runtime
            .output_endpoint(
                |_: Value| -> Result<(), Error> { Err(Error::new("decode failure")) },
                |()| Ok(()),
            )
            .expect("decode endpoint should register");
        let adapter = runtime
            .output_endpoint(decode_test_integer, |_: i64| {
                Err(Error::new("adapter failure"))
            })
            .expect("adapter endpoint should register");
        let panic = runtime
            .output_endpoint(decode_test_integer, |_: i64| -> Result<(), Error> {
                panic!("adapter panic")
            })
            .expect("panic endpoint should register");
        let (store, mut events) = input_transaction(&runtime);
        let decode_id = events
            .write(&decode.writer(), runtime.values().integer(1))
            .unwrap();
        let adapter_id = events
            .write(&adapter.writer(), runtime.values().integer(2))
            .unwrap();
        let panic_id = events
            .write(&panic.writer(), runtime.values().integer(3))
            .unwrap();
        assert_eq!(
            runtime.try_commit_transaction(&store, &events),
            crate::reflection::StoreCommitResult::Committed
        );

        let outcomes = [
            decode.delivery().deliver_next().unwrap().unwrap(),
            adapter.delivery().deliver_next().unwrap().unwrap(),
            panic.delivery().deliver_next().unwrap().unwrap(),
        ];
        let kinds = outcomes.map(|outcome| match outcome {
            RuntimeDeliveryOutcome::Failed(failure) => failure.kind(),
            RuntimeDeliveryOutcome::Delivered(_) => panic!("delivery should fail"),
        });
        assert_eq!(
            kinds,
            [
                RuntimeDeliveryFailureKind::Decode,
                RuntimeDeliveryFailureKind::Adapter,
                RuntimeDeliveryFailureKind::Panic,
            ]
        );
        assert!(!runtime.has_delivery_activity());
        let snapshot = runtime.delivery_failure_snapshot();
        assert_eq!(snapshot.failures().len(), 3);
        assert_eq!(
            decode
                .delivery()
                .failure_snapshot()
                .unwrap()
                .failures()
                .len(),
            1
        );
        assert!(snapshot.get(decode_id).is_some());
        assert!(snapshot.get(adapter_id).is_some());
        assert!(snapshot.get(panic_id).is_some());
        assert!(matches!(runtime.readiness(), RuntimeReadiness::Ready(_)));

        let (generation, _, _) = runtime.transaction_snapshot();
        assert!(runtime.acknowledge_delivery_failure(adapter_id));
        assert!(!runtime.acknowledge_delivery_failure(adapter_id));
        let (after_acknowledgement, _, _) = runtime.transaction_snapshot();
        assert_eq!(after_acknowledgement, generation);
        assert!(
            runtime
                .delivery_failure_snapshot()
                .get(adapter_id)
                .is_none()
        );
        assert!(snapshot.get(adapter_id).is_some());
    }

    #[test]
    fn output_callback_response_reenters_as_later_admitted_input() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let input = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("input endpoint should register");
        let response = input.sender();
        let output = runtime
            .output_endpoint(decode_test_integer, move |value| {
                response.admit(value)?;
                Ok(())
            })
            .expect("output endpoint should register");
        let (producing_store, mut producing_events) = input_transaction(&runtime);
        assert_eq!(producing_events.read(&input.reader()).unwrap(), None);
        producing_events
            .write(&output.writer(), runtime.values().integer(42))
            .unwrap();
        assert_eq!(
            runtime.try_commit_transaction(&producing_store, &producing_events),
            crate::reflection::StoreCommitResult::Committed
        );
        let (stale_store, mut stale_events) = input_transaction(&runtime);
        assert_eq!(stale_events.read(&input.reader()).unwrap(), None);

        assert!(output.delivery().deliver_next().unwrap().is_some());
        assert_eq!(
            runtime.try_commit_transaction(&stale_store, &stale_events),
            crate::reflection::StoreCommitResult::Conflict
        );
        let (_, mut fresh_events) = input_transaction(&runtime);
        assert_eq!(
            fresh_events
                .read(&input.reader())
                .unwrap()
                .and_then(|value| value.as_number_text()),
            Some("42".to_owned())
        );
    }

    #[test]
    fn output_payload_is_retained_through_callback_and_dropped_after_locks() {
        struct DeliveryLease {
            resources: Weak<RuntimeSharedResources>,
            dropped: Arc<AtomicBool>,
        }

        impl Drop for DeliveryLease {
            fn drop(&mut self) {
                if let Some(resources) = self.resources.upgrade() {
                    assert!(
                        resources
                            .mutation_admission
                            .try_settlement_guard()
                            .is_some()
                    );
                }
                self.dropped.store(true, Ordering::Release);
            }
        }

        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let dropped = Arc::new(AtomicBool::new(false));
        let lease = Arc::new(DeliveryLease {
            resources: Arc::downgrade(&runtime.state.shared_resources),
            dropped: dropped.clone(),
        });
        let retained = Arc::downgrade(&lease);
        let callback_retained = retained.clone();
        let endpoint = runtime
            .output_endpoint(
                |value| {
                    assert!(matches!(value.as_core(), CoreValue::Opaque(_)));
                    Ok(())
                },
                move |()| {
                    assert!(callback_retained.upgrade().is_some());
                    Ok(())
                },
            )
            .expect("output endpoint should register");
        let (store, mut events) = input_transaction(&runtime);
        events
            .write(
                &endpoint.writer(),
                Value::from_core(
                    runtime.values().core(),
                    CoreValue::Opaque(OpaqueValue::new(lease)),
                ),
            )
            .unwrap();
        assert_eq!(
            runtime.try_commit_transaction(&store, &events),
            crate::reflection::StoreCommitResult::Committed
        );
        drop(events);
        drop(store);
        assert!(retained.upgrade().is_some());
        assert!(endpoint.delivery().deliver_next().unwrap().is_some());
        assert!(retained.upgrade().is_none());
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn running_delivery_retains_shared_resources_until_terminal_publication() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let runtime_state = Arc::downgrade(&runtime.state);
        let resources = Arc::downgrade(&runtime.state.shared_resources);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let callback_barrier = barrier.clone();
        let (entered, waiting) = std::sync::mpsc::channel();
        let endpoint = runtime
            .output_endpoint(decode_test_integer, move |_: i64| {
                entered.send(()).unwrap();
                callback_barrier.wait();
                Ok(())
            })
            .expect("output endpoint should register");
        let (store, mut events) = input_transaction(&runtime);
        events
            .write(&endpoint.writer(), runtime.values().integer(1))
            .unwrap();
        assert_eq!(
            runtime.try_commit_transaction(&store, &events),
            crate::reflection::StoreCommitResult::Committed
        );
        drop(events);
        drop(store);
        let delivery = endpoint.delivery();
        let worker = std::thread::spawn(move || delivery.deliver_next().unwrap());
        waiting.recv().expect("callback should begin");

        drop(endpoint);
        drop(runtime);
        assert!(runtime_state.upgrade().is_none());
        assert!(resources.upgrade().is_some());
        barrier.wait();
        assert!(worker.join().unwrap().is_some());
        assert!(resources.upgrade().is_none());
    }

    #[test]
    fn independent_runtimes_have_independent_reflection_heaps() {
        let owner = Assembler::default();
        let foreign = Assembler::default();
        let module = owner
            .module(["runtime_heap_isolation"])
            .script(
                "g",
                "language g0\nimport 'std\nresult = anno refl:(.heap.set '.runtime_only \"yes\") \"done\"\n",
            )
            .build()
            .expect("heap isolation fixture should compile");
        owner
            .evaluate(
                &owner
                    .get(module.value(), "result")
                    .expect("fixture should define result"),
            )
            .expect("reflection gate should complete");

        assert!(
            owner
                .get(&owner.test_reflection_heap(), "runtime_only")
                .is_ok()
        );
        assert!(
            foreign
                .get(&foreign.test_reflection_heap(), "runtime_only")
                .is_err()
        );
    }

    #[test]
    fn runtime_combines_reflection_and_logger_event_commit() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let assembler = Assembler::builder()
            .evaluation_runtime(runtime.clone())
            .build()
            .expect("assembler should build");
        let input = runtime
            .input_endpoint(integer_converter(&runtime))
            .expect("input endpoint should register");
        let (initial_generation, store, snapshot) = runtime.logger_transaction_snapshot();
        let mut stale = crate::reflection::StoreJournal::new(store);
        stale.write(
            vec![Key::atom_from_text("atomic")],
            runtime.values().text("stale"),
        );
        let mut stale_events = RuntimeEventJournal::new(snapshot.events().clone());
        assert_eq!(stale_events.read(&input.reader()).unwrap(), None);
        input.sender().admit(7).expect("input should be admitted");
        let (input_generation, _, _) = runtime.logger_transaction_snapshot();
        assert_ne!(input_generation, initial_generation);

        assert_eq!(
            runtime.try_commit_logger_transaction(&stale, &snapshot, false, &stale_events),
            crate::reflection::StoreCommitResult::Conflict
        );
        assert!(assembler.get(&runtime.reflection_root(), "atomic").is_err());

        let (_, store, snapshot) = runtime.logger_transaction_snapshot();
        let mut committed = crate::reflection::StoreJournal::new(store);
        committed.write(
            vec![Key::atom_from_text("atomic")],
            runtime.values().text("committed"),
        );
        let mut events = RuntimeEventJournal::new(snapshot.events().clone());
        assert_eq!(
            events
                .read(&input.reader())
                .expect("admitted input should be readable")
                .and_then(|value| value.as_i64()),
            Some(7)
        );
        assert_eq!(
            runtime.try_commit_logger_transaction(&committed, &snapshot, false, &events),
            crate::reflection::StoreCommitResult::Committed
        );
        let (committed_generation, _, snapshot) = runtime.logger_transaction_snapshot();
        assert_ne!(committed_generation, input_generation);
        let mut empty = RuntimeEventJournal::new(snapshot.events().clone());
        assert_eq!(empty.read(&input.reader()).unwrap(), None);
        assert_eq!(
            assembler
                .get(&runtime.reflection_root(), "atomic")
                .expect("the store edit should commit")
                .as_binary(),
            Some(b"committed".as_slice())
        );
    }

    #[test]
    fn exclusive_admission_probe_rejects_an_active_mutation() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let mutation = runtime.mutation_guard();

        assert!(!runtime.exclusive_admission_available());
        drop(mutation);
        assert!(runtime.exclusive_admission_available());
    }

    #[test]
    fn runtime_store_publication_wakes_broad_observers() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let (generation, store) = runtime.reflection_snapshot();
        let waiting_runtime = runtime.clone();
        let (awake, observed) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            awake
                .send(waiting_runtime.wait_for_change(generation))
                .expect("test should still receive the wake result");
        });
        let mut journal = crate::reflection::StoreJournal::new(store);
        journal.write(
            vec![Key::atom_from_text("wake")],
            runtime.values().empty_record(),
        );

        assert_eq!(
            runtime.commit_reflection(&journal),
            crate::reflection::StoreCommitResult::Committed
        );
        assert!(
            observed
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("store publication should wake the observer")
        );
        waiter.join().expect("observer thread should finish");
    }

    #[test]
    fn coordinator_transitions_do_not_advance_the_semantic_observation_epoch() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let (before, _) = runtime.reflection_snapshot();
        let session = EvaluationSession::shared_with_default_profile(
            &runtime.state.work,
            runtime.state.shared_resources.values.core().clone(),
            Arc::new(ReflectionTaskProfile::unsealed()),
        );
        let context = EvalContext::new(&session);
        let _task = context
            .schedule_task(|_| Ok(Box::new(FailedReasoningTask)))
            .expect("test task should enter the coordinator ready queue");
        let (after_ready, _) = runtime.reflection_snapshot();
        assert_eq!(after_ready, before);

        drop(context);
        drop(session);
        let (after_close, _) = runtime.reflection_snapshot();
        assert_eq!(after_close, before);
    }

    #[test]
    fn diagnostic_callbacks_run_after_runtime_mutation_admission_is_released() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let callback_runtime = runtime.clone();
        let callback_observed = Arc::new(AtomicBool::new(false));
        let callback_result = callback_observed.clone();
        let assembler = Assembler::builder()
            .evaluation_runtime(runtime)
            .diagnostic_callback(move |_| {
                callback_result.store(
                    callback_runtime.exclusive_admission_available(),
                    Ordering::Relaxed,
                );
            })
            .build()
            .expect("assembler should build");
        let module = assembler
            .module(["runtime_callback_admission"])
            .script(
                "g",
                "language g0\nimport 'std\nresult = anno refl:(.log 'info {msg:{text:\"callback\"}}) \"done\"\n",
            )
            .build()
            .expect("callback fixture should compile");
        assembler
            .evaluate(
                &assembler
                    .get(module.value(), "result")
                    .expect("fixture should define result"),
            )
            .expect("reflection gate should complete");

        assert!(callback_observed.load(Ordering::Relaxed));
    }

    #[test]
    fn reasoning_failure_acknowledgement_is_idempotent_and_runtime_bound() {
        let runtime = EvaluationRuntime::new(0).expect("dormant runtime should build");
        let assembler = Assembler::builder()
            .evaluation_runtime(runtime.clone())
            .build()
            .expect("assembler should build");
        let peer = Assembler::builder()
            .evaluation_runtime(runtime)
            .build()
            .expect("same-runtime peer assembler should build");
        let foreign = Assembler::builder()
            .evaluation_runtime(EvaluationRuntime::new(0).expect("foreign runtime should build"))
            .build()
            .expect("foreign-runtime assembler should build");
        let task = assembler
            .eval_context()
            .schedule_task(|_| Ok(Box::new(FailedReasoningTask)))
            .expect("failing task should schedule");

        let report = assembler.drain_reasoning();
        assert_eq!(report.status(), ReasoningStatus::Complete);
        let [failure] = report.failures() else {
            panic!("drain should report exactly one task failure")
        };
        assert_eq!(failure.task_id(), task.id().get());
        assert_eq!(failure.message(), "public reasoning failure");
        let failure = failure.clone();

        let error = foreign
            .acknowledge_reasoning_failure(&failure)
            .expect_err("a foreign runtime must reject the acknowledgement capability");
        assert!(error.to_string().contains("different evaluation runtime"));
        assert_eq!(
            assembler.drain_reasoning().failures(),
            std::slice::from_ref(&failure),
            "foreign-runtime acknowledgement must not alter the originating ledger"
        );

        peer.acknowledge_reasoning_failure(&failure)
            .expect("a same-runtime assembler should route to the producer ledger");
        assembler
            .acknowledge_reasoning_failure(&failure)
            .expect("repeated acknowledgement should be harmless");
        assert!(assembler.drain_reasoning().failures().is_empty());
        assert!(matches!(
            assembler.eval_context().poll_reflection_task(&task),
            EvaluationWaitPoll::Failed(error)
                if error.to_string() == "public reasoning failure"
        ));
    }

    #[test]
    fn synchronous_assembler_evaluation_waits_for_a_worker_claim() {
        let runtime = EvaluationRuntime::new(1).expect("worker runtime should build");
        let assembler = Assembler::builder()
            .evaluation_runtime(runtime)
            .build()
            .expect("assembler should build");
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let producer_release = release.clone();
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let lazy = crate::core::LazyValue::deferred(
            &assembler.core_values(),
            "worker-claimed public value",
            move |_| {
                started_sender
                    .send(())
                    .expect("test should still await the worker claim");
                let (lock, changed) = &*producer_release;
                let mut released = lock.lock().expect("test release lock was poisoned");
                while !*released {
                    released = changed
                        .wait(released)
                        .expect("test release lock was poisoned");
                }
                Ok(CoreValue::Number(42.into()))
            },
        );
        let value = Value::from_core(&assembler.core_values(), CoreValue::Lazy(lazy));
        assembler.eval_context().spark(value.as_core().clone());
        started_receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("worker should claim the sparked value");

        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        let evaluator = std::thread::spawn({
            let assembler = assembler.clone();
            let value = value.clone();
            move || {
                result_sender
                    .send(assembler.evaluate(&value))
                    .expect("test should still await the result");
            }
        });
        assert!(
            result_receiver
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "synchronous evaluation must wait while the worker owns the value"
        );

        let (lock, changed) = &*release;
        *lock.lock().expect("test release lock was poisoned") = true;
        changed.notify_all();
        let result = result_receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("worker completion should wake synchronous evaluation")
            .expect("worker-computed value should succeed");
        assert_eq!(result, assembler.values().number_from_text("42").unwrap());
        evaluator.join().expect("evaluator thread should finish");
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
    fn attached_runtime_conflict_analysis_cannot_be_replaced() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let result = Assembler::builder()
            .evaluation_runtime(runtime.clone())
            .conflict_analysis(Arc::new(crate::reflection::CoarseConflictAnalysis))
            .build();
        let Err(error) = result else {
            panic!("an attached runtime must retain its conflict policy")
        };

        assert!(error.to_string().contains("already owns"));
        assert_eq!(runtime.conflict_analysis().name(), "exact");
    }

    #[test]
    fn attached_runtime_default_reflection_profile_cannot_be_replaced() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let error = runtime
            .new_evaluation_session()
            .expect_err("an unsealed runtime must not expose a runnable session");
        assert!(error.to_string().contains("must be sealed"));
        let assembler = Assembler::builder()
            .evaluation_runtime(runtime.clone())
            .build()
            .expect("first assembler should seal the runtime profile");
        let replacement = Arc::new(AssemblerReflectionHost::new_unsealed(
            &runtime,
            DiagnosticBus::new(),
        ));
        replacement
            .seal_environment(
                authoritative_reflection_environment(
                    runtime.values().empty_record(),
                    "replacement",
                )
                .unwrap()
                .0,
            )
            .unwrap();

        let error = runtime
            .seal_default_reflection_profile(task_launcher(ReflectionEffects, replacement))
            .expect_err("a sealed runtime profile must reject replacement");
        assert!(error.to_string().contains("already sealed"));

        let runtime_state = Arc::downgrade(&runtime.state);
        drop(assembler);
        drop(runtime);
        assert!(
            runtime_state.upgrade().is_none(),
            "the sealed launcher must not form an Arc cycle through its host"
        );
    }

    #[test]
    fn retained_reflection_profile_keeps_only_shared_resources_alive() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let state = Arc::downgrade(&runtime.state);
        let coordinator = Arc::downgrade(&runtime.state.work);
        let executor = Arc::downgrade(&runtime.state.executor);
        let default_profile = Arc::downgrade(&runtime.default_reflection_profile);
        let resources = Arc::downgrade(&runtime.state.shared_resources);
        let host = Arc::new(AssemblerReflectionHost::new_unsealed(
            &runtime,
            DiagnosticBus::for_runtime(&runtime),
        ));
        host.seal_environment(
            authoritative_reflection_environment(runtime.values().empty_record(), "retained")
                .unwrap()
                .0,
        )
        .unwrap();
        let profile = Arc::new(ReflectionTaskProfile::sealed(task_launcher(
            ReflectionEffects,
            host.clone(),
        )));
        drop(host);
        drop(runtime);

        assert!(state.upgrade().is_none());
        assert!(coordinator.upgrade().is_none());
        assert!(executor.upgrade().is_none());
        assert!(default_profile.upgrade().is_none());

        let retained = resources
            .upgrade()
            .expect("the retained profile host should keep runtime resources alive");
        let (_, snapshot) = retained.reflection_snapshot();
        assert_eq!(snapshot.root(), &retained.values().empty_record());
        let initial = retained.values().empty_record();
        let volume = retained
            .create_volume(initial.clone())
            .expect("retained resources should still create volumes");
        assert_eq!(retained.revoke_volume(volume).unwrap(), initial);
        drop(retained);

        drop(profile);
        assert!(resources.upgrade().is_none());
    }

    #[test]
    fn evaluation_context_retains_runtime_cache_and_profile_without_a_cycle() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let state = Arc::downgrade(&runtime.state);
        let resources = Arc::downgrade(&runtime.state.shared_resources);
        let profile = Arc::downgrade(&runtime.default_reflection_profile);
        let assembler = Assembler::builder()
            .evaluation_runtime(runtime.clone())
            .build()
            .expect("assembler should seal the runtime profile");
        let context = assembler.eval_context();
        let unit = context.values().unit();

        drop(assembler);
        drop(runtime);
        assert!(state.upgrade().is_none());
        assert!(resources.upgrade().is_some());
        assert!(profile.upgrade().is_some());
        assert_eq!(eval::eval_value(&context, &unit).unwrap(), unit);

        drop(context);
        assert!(resources.upgrade().is_none());
        assert!(profile.upgrade().is_none());
    }

    #[test]
    fn builder_selects_runtime_before_exposing_runtime_bound_state() {
        let mut builder = Assembler::builder();
        let initial = builder.runtime.values().empty_record();
        let _volume = builder
            .create_volume(initial)
            .expect("the initial runtime should create the volume");
        let replacement = EvaluationRuntime::new(0).expect("replacement runtime should build");
        let result = builder.evaluation_runtime(replacement).build();
        let Err(error) = result else {
            panic!("runtime replacement after state construction must be rejected")
        };

        assert!(error.to_string().contains("must be selected before"));
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
                .contains("reflection annotation result: unit expected, received Binary")
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
            binary_at(&assembler, module.value(), "result")
                .expect("winning reflection branch should complete"),
            b"ready".as_slice()
        );
        assert_eq!(assembler.diagnostic_bus().counts().total(), 0);
    }

    #[test]
    fn diagnostic_bus_sequences_counts_and_delivers_only_to_current_subscribers() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let values = runtime.values();
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

        let first = bus.publish_local(Diagnostic::new(&values, Severity::Info, "first"));
        let late = Arc::new(Mutex::new(Vec::new()));
        let late_events = late.clone();
        let _late_subscription = bus.subscribe(DiagnosticCallback(move |event| {
            late_events
                .lock()
                .expect("late diagnostic collector should not be poisoned")
                .push(event);
        }));
        let second = bus.publish_local(Diagnostic::new(&values, Severity::Warning, "second"));
        drop(early_subscription);
        let third = bus.publish_local(Diagnostic::new(&values, Severity::Error, "third"));

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
    fn diagnostic_ingress_is_runtime_bound_and_installed_once() {
        let owner = EvaluationRuntime::new(0).expect("owner runtime should build");
        let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
        let bus = DiagnosticBus::for_runtime(&owner);
        let (_ingress, _reader) = bus
            .diagnostic_ingress(&owner)
            .expect("first ingress should attach");

        assert!(bus.diagnostic_ingress(&owner).is_err());
        assert!(bus.bind_runtime(&foreign).is_err());
        assert!(
            bus.publish(Diagnostic::new(
                &foreign.values(),
                Severity::Error,
                "foreign diagnostic",
            ))
            .is_err()
        );
        assert!(
            bus.publish_from_runtime(
                foreign.id(),
                Diagnostic::new(&foreign.values(), Severity::Error, "foreign diagnostic"),
            )
            .is_err()
        );
        assert_eq!(bus.counts().total(), 0);
    }

    #[test]
    fn diagnostic_value_operations_reject_foreign_runtime_views() {
        let owner = EvaluationRuntime::new(0).expect("owner runtime should build");
        let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
        let diagnostic = Diagnostic::new(&owner.values(), Severity::Error, "owner");

        assert!(diagnostic.enrich(&foreign.values()).is_err());
        assert!(
            diagnostic
                .clone()
                .with_context(foreign.values().text("foreign context"))
                .is_err()
        );
        assert!(diagnostic.transport_value(&foreign.values()).is_err());
        assert!(
            Diagnostic::apply_updates(
                &owner.values(),
                diagnostic.emission(),
                foreign.values().empty_record(),
            )
            .is_err()
        );
    }

    #[test]
    fn diagnostic_ingress_admits_in_bus_sequence_order() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let bus = DiagnosticBus::for_runtime(&runtime);
        let (_ingress, reader) = bus
            .diagnostic_ingress(&runtime)
            .expect("ingress should attach");
        let before = runtime.transaction_snapshot().0;
        let values = runtime.values();
        let published = (0..24)
            .map(|index| {
                let bus = bus.clone();
                let values = values.clone();
                std::thread::spawn(move || {
                    let message = format!("message {index}");
                    let event = bus.publish_local(Diagnostic::new(
                        &values,
                        Severity::Info,
                        message.clone(),
                    ));
                    (event.sequence(), message)
                })
            })
            .map(|thread| thread.join().expect("publisher should not panic"))
            .collect::<BTreeMap<_, _>>();
        assert_ne!(runtime.transaction_snapshot().0, before);

        let (_, store, snapshot) = runtime.transaction_snapshot();
        let mut journal = RuntimeEventJournal::new(snapshot);
        let mut received = Vec::new();
        while let Some(value) = journal.read(&reader).expect("ingress should be readable") {
            received.push(
                Diagnostic::from_transport_value(&value)
                    .expect("ingress should retain diagnostic envelopes")
                    .message()
                    .to_owned(),
            );
        }
        assert_eq!(
            received,
            published.values().cloned().collect::<Vec<_>>(),
            "runtime FIFO order must follow bus sequence, not callback arrival"
        );
        assert_eq!(
            runtime.try_commit_transaction(&crate::reflection::StoreJournal::new(store), &journal),
            crate::reflection::StoreCommitResult::Committed
        );
    }

    #[test]
    fn runtime_retains_the_installed_diagnostic_ingress() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let bus = DiagnosticBus::for_runtime(&runtime);
        let (ingress, reader) = bus
            .diagnostic_ingress(&runtime)
            .expect("ingress should attach");
        drop(ingress);

        bus.publish_local(Diagnostic::new(
            &runtime.values(),
            Severity::Info,
            "still routed",
        ));
        let (_, _, snapshot) = runtime.transaction_snapshot();
        let mut journal = RuntimeEventJournal::new(snapshot);
        let value = journal
            .read(&reader)
            .expect("retained ingress should remain readable")
            .expect("publication should reach the stable ingress");
        assert_eq!(
            Diagnostic::from_transport_value(&value)
                .expect("ingress should retain a diagnostic envelope")
                .message(),
            "still routed"
        );
    }

    #[test]
    fn diagnostic_subscribers_run_after_runtime_admission_is_released() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let bus = DiagnosticBus::for_runtime(&runtime);
        let (_ingress, _reader) = bus
            .diagnostic_ingress(&runtime)
            .expect("ingress should attach");
        let callback_runtime = runtime.clone();
        let _subscription = bus.subscribe(DiagnosticCallback(move |_| {
            assert!(
                callback_runtime.exclusive_admission_available(),
                "ordinary callbacks must run outside runtime mutation admission"
            );
        }));

        bus.publish_local(Diagnostic::new(
            &runtime.values(),
            Severity::Info,
            "ordered",
        ));
    }

    #[test]
    fn diagnostic_bus_and_ingress_do_not_retain_the_runtime() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let retained = Arc::downgrade(&runtime.state);
        let bus = DiagnosticBus::for_runtime(&runtime);
        let (ingress, _reader) = bus
            .diagnostic_ingress(&runtime)
            .expect("ingress should attach");

        let diagnostic = Diagnostic::new(&runtime.values(), Severity::Info, "after runtime");
        drop(runtime);
        assert!(retained.upgrade().is_none());
        bus.publish_local(diagnostic);
        assert!(ingress.failure().is_some());
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

        assembler.record_diagnostic(Diagnostic::new(
            &assembler.values(),
            Severity::Info,
            "hello",
        ));

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
            crate::core::test_value_factory().error(),
        );
        let message = CoreValue::Dict(message.insert(
            (*crate::core::keys::MSG).clone(),
            CoreValue::Dict(interface),
        ));

        let values = EvaluationRuntime::new(0).unwrap().values();
        let trace = test_compilation_trace("test.g");
        let diagnostic =
            Diagnostic::from_compile(values.core(), &trace, Severity::Warning, message);
        assert_eq!(diagnostic.severity(), Severity::Warning);

        let CoreValue::Dict(emission) = diagnostic.emission().as_core() else {
            panic!("raw diagnostic should be a dictionary");
        };
        let Some(CoreValue::Dict(interface)) = emission.get(&*crate::core::keys::MSG) else {
            panic!("raw diagnostic should provide msg");
        };
        assert_eq!(
            interface.get(&*crate::core::keys::SEVERITY),
            Some(&crate::core::test_value_factory().error())
        );
        assert!(interface.get(&*crate::core::keys::ORIGIN).is_none());
        assert!(emission.get(&*crate::core::keys::SPEC).is_none());

        let enriched = diagnostic
            .enrich(&values)
            .expect("diagnostic should enrich");
        let CoreValue::Dict(enriched) = enriched.as_core() else {
            panic!("enriched diagnostic should be an object dictionary");
        };
        let Some(CoreValue::Dict(interface)) = enriched.get(&*crate::core::keys::MSG) else {
            panic!("enriched diagnostic should provide msg");
        };
        assert_eq!(
            interface.get(&*crate::core::keys::SEVERITY),
            Some(&values.core.warn())
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
        let values = EvaluationRuntime::new(0).unwrap().values();
        let diagnostic = Diagnostic::from_compile(
            values.core(),
            &trace,
            Severity::Info,
            crate::diagnostic::text_message(Some(3), "hello"),
        );
        let viewer_key = Key::atom_from_text("viewer");
        let inherit = |name: &str| {
            diagnostic
                .enrich_with(
                    &values,
                    values
                        .record([("viewer", values.text(name))])
                        .expect("viewer value is local"),
                )
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
