use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use super::runtime::{
    RuntimeDiagnosticRouteMode, RuntimeInputReader, RuntimeInputSender, RuntimeOutputWriter,
    RuntimePreparedInput, RuntimeSharedResources, configure_runtime_diagnostic_fallback,
    prepare_runtime_observation, register_runtime_diagnostic_route,
    route_runtime_diagnostic_guarded, set_runtime_diagnostic_route,
    set_runtime_diagnostic_route_guarded,
};
use super::{Error, EvaluationRuntime, Value, Values};
use crate::core::{CoreValueFactory, Dict, Key, Value as CoreValue};
use crate::diagnostic::{CompilationTrace, Severity};
use crate::evaluation::{TaskStatusPublisher, TaskStatusWake};
use crate::number::Number;
use crate::reflection::EffectLifecycleTerminal;
use crate::runtime::EvaluationRuntimeId;
use crate::source::SourceIdentity;

/// One raw diagnostic emission retained or dispatched by an [`Assembler`].
///
/// The emission stays unchanged in the envelope. Observers may explicitly
/// apply assembler provenance, then add viewer-specific context, without
/// affecting other observers of the same diagnostic.
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct Diagnostic {
    pub(super) emission: Value,
    pub(super) origin: Option<Value>,
    // Transitional projections for simple embedding clients that do not yet
    // inspect the object message.
    pub(super) source: Option<Arc<str>>,
    pub(super) severity: Severity,
    pub(super) line: Option<usize>,
    pub(super) message: Arc<str>,
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
            values,
            None,
            severity,
            crate::diagnostic::text_message(None, &message),
            None,
        )
    }

    /// Wraps an arbitrary diagnostic value with separately supplied severity.
    /// Assembler and viewer metadata remain unapplied until enrichment.
    pub fn from_emission(
        values: &Values,
        severity: Severity,
        emission: Value,
    ) -> Result<Self, Error> {
        let emission = values.clone_core(&emission)?;
        Ok(Self::from_parts(
            values.core(),
            None,
            severity,
            emission,
            None,
        ))
    }

    pub fn with_source_location(
        self,
        values: &Values,
        source: impl Into<Arc<str>>,
        line: usize,
    ) -> Result<Self, Error> {
        let source = source.into();
        let identity = SourceIdentity::file(Path::new(source.as_ref()));
        let origin = CoreValue::Dict(
            Dict::new_sync().insert((*crate::core::keys::SOURCE).clone(), identity.value()),
        );
        Ok(Self::from_parts(
            values.core(),
            Some(source.clone()),
            self.severity,
            crate::diagnostic::text_message(Some(line), &self.message),
            Some(origin),
        ))
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
        let public_values = Values::from_core_factory(values.clone());
        crate::diagnostic::enrich(
            values,
            public_values.clone_core(&self.emission)?,
            self.severity,
            self.origin
                .as_ref()
                .map(|origin| public_values.clone_core(origin))
                .transpose()?,
        )
        .map(|value| public_values.wrap(value))
        .map_err(|error| Error::from_eval(values, error))
    }

    /// Applies assembler metadata followed by observer-specific object updates.
    /// The raw emission and other enriched views remain unchanged.
    pub fn enrich_with(&self, values: &Values, updates: Value) -> Result<Value, Error> {
        updates.require_runtime(values.runtime)?;
        let enriched = self.enrich(values)?;
        crate::diagnostic::apply_updates(
            &values.core,
            values.clone_core(&enriched)?,
            values.clone_core(&updates)?,
        )
        .map(|value| values.wrap(value))
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
            values.clone_core(message)?,
            values.clone_core(&updates)?,
        )
        .map(|value| values.wrap(value))
        .map_err(|error| Error::from_eval(&values.core, error))
    }

    /// Prepends one structured frame describing why this diagnostic was
    /// produced or propagated. The original emission remains otherwise
    /// unchanged.
    pub fn with_context(self, values: &Values, context: Value) -> Result<Self, Error> {
        self.emission.require_runtime(values.runtime)?;
        context.require_runtime(values.runtime)?;
        let emission = crate::diagnostic::prepend_contexts_with(
            &values.core,
            values.clone_core(&self.emission)?,
            &[values.clone_core(&context)?],
        )
        .unwrap_or_else(|_| {
            values
                .clone_core(&self.emission)
                .expect("the diagnostic runtime was checked")
        });
        Ok(Self::from_parts(
            values.core(),
            self.source,
            self.severity,
            emission,
            self.origin
                .as_ref()
                .map(|origin| values.clone_core(origin))
                .transpose()?,
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
        values.with_access(|access| {
            let mut fields = Dict::new_sync()
                .insert(
                    Key::atom_from_text("emission"),
                    access.core_value(&self.emission)?.clone(),
                )
                .insert(
                    Key::atom_from_text("severity"),
                    self.severity.value(values.core()),
                );
            if let Some(origin) = &self.origin {
                fields = fields.insert(
                    Key::atom_from_text("origin"),
                    access.core_value(origin)?.clone(),
                );
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
            Ok(access.wrap(CoreValue::Dict(fields)))
        })
    }

    #[doc(hidden)]
    pub fn from_transport_value(values: &Values, value: &Value) -> Result<Self, Error> {
        value.require_runtime(values.runtime)?;
        values.with_access(|access| {
            let CoreValue::Dict(fields) = access.core_value(value)? else {
                return Err(Error::new("diagnostic transport requires a dictionary"));
            };
            let field = |name: &str| fields.get(&Key::atom_from_text(name));
            let emission = field("emission")
                .cloned()
                .ok_or_else(|| Error::new("diagnostic transport is missing `emission`"))?;
            let severity = match field("severity").and_then(Key::from_value) {
                Some(value) if value == *crate::core::keys::INFO => Severity::Info,
                Some(value) if value == *crate::core::keys::WARN => Severity::Warning,
                Some(value) if value == *crate::core::keys::ERROR => Severity::Error,
                _ => return Err(Error::new("diagnostic transport has an invalid severity")),
            };
            let source = field("source")
                .map(|source| {
                    let CoreValue::Binary(source) = source else {
                        return Err(Error::new("diagnostic transport source must be text"));
                    };
                    std::str::from_utf8(source)
                        .map(Arc::<str>::from)
                        .map_err(|_| Error::new("diagnostic transport source must be text"))
                })
                .transpose()?;
            let line = field("line")
                .map(|line| {
                    let CoreValue::Number(line) = line else {
                        return Err(Error::new("diagnostic transport line must be nonnegative"));
                    };
                    line.to_i64_if_integer()
                        .and_then(|line| usize::try_from(line).ok())
                        .ok_or_else(|| Error::new("diagnostic transport line must be nonnegative"))
                })
                .transpose()?;
            let origin = field("origin").cloned().map(|origin| access.wrap(origin));
            let (projected_line, message) = crate::diagnostic::conventional_summary(&emission);
            Ok(Self {
                emission: access.wrap(emission),
                origin,
                source,
                severity,
                line: line.or(projected_line),
                message: message
                    .unwrap_or_else(|| Arc::from("<diagnostic has no immediate text view>")),
            })
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

    pub(super) fn from_compile(
        values: &CoreValueFactory,
        trace: &CompilationTrace,
        severity: Severity,
        message: CoreValue,
    ) -> Self {
        Self::from_parts(
            values,
            Some(Arc::from(trace.source_label())),
            severity,
            message,
            Some(trace.origin_value()),
        )
    }

    pub(crate) fn from_parts(
        values: &CoreValueFactory,
        source: Option<Arc<str>>,
        severity: Severity,
        message: CoreValue,
        origin: Option<CoreValue>,
    ) -> Self {
        let (line, text) = crate::diagnostic::conventional_summary(&message);
        let public_values = Values::from_core_factory(values.clone());
        Self {
            emission: public_values.wrap(message),
            origin: origin.map(|origin| public_values.wrap(origin)),
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
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq, Eq))]
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
    pub(super) next_sequence: u64,
    pub(super) info: u64,
    pub(super) warnings: u64,
    pub(super) errors: u64,
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
        // between the baseline capture and ingress installation. Guarded
        // publishers acquire runtime admission before returning to this lock,
        // but only after `state.ingress` is installed below. A publisher that
        // wins this lock before setup takes the direct path; one that arrives
        // during setup waits without runtime admission, then observes the
        // installed ingress. The opposite lock order is therefore unreachable
        // during the one-time installation.
        let endpoint = runtime.input_endpoint::<Value, _>(Ok)?;
        let (sender, reader) = endpoint.into_parts();
        register_runtime_diagnostic_route(&runtime.state.shared_resources, sender.id())?;
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

    pub(super) fn publish_local(&self, diagnostic: Diagnostic) -> DiagnosticEvent {
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
        let mut diagnostic = Some(diagnostic);
        let (ingress, direct) = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("diagnostic bus mutex should not be poisoned");
            Self::validate_runtime_locked(&mut state, runtime)?;
            let ingress = state.ingress.as_ref().and_then(Weak::upgrade);
            let direct = ingress.is_none().then(|| {
                Self::record_event_locked(
                    &mut state,
                    diagnostic
                        .take()
                        .expect("a direct publication still owns its diagnostic"),
                )
            });
            (ingress, direct)
        };
        let (event, subscribers) = match (ingress, direct) {
            (Some(ingress), None) => ingress.publish_guarded(
                &self.inner,
                runtime,
                diagnostic
                    .take()
                    .expect("an ingress publication still owns its diagnostic"),
            )?,
            (None, Some(direct)) => direct,
            _ => unreachable!("a diagnostic selects exactly one publication route"),
        };
        for subscriber in subscribers {
            subscriber.receive(event.clone());
        }
        Ok(event)
    }

    fn validate_runtime_locked(
        state: &mut DiagnosticBusState,
        runtime: EvaluationRuntimeId,
    ) -> Result<(), Error> {
        match state.runtime {
            Some(owner) if owner != runtime => Err(Error::new(format!(
                "diagnostic bus belongs to evaluation runtime {}, not {}",
                owner.get(),
                runtime.get()
            ))),
            Some(_) => Ok(()),
            None => {
                state.runtime = Some(runtime);
                Ok(())
            }
        }
    }

    fn record_event_locked(
        state: &mut DiagnosticBusState,
        diagnostic: Diagnostic,
    ) -> (DiagnosticEvent, Vec<Arc<dyn DiagnosticSubscriber>>) {
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
        (
            DiagnosticEvent {
                sequence,
                diagnostic: Arc::new(diagnostic),
            },
            state.subscribers.values().cloned().collect(),
        )
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

pub(super) struct DiagnosticIngressInner {
    pub(super) sender: RuntimeInputSender<Value>,
    values: Values,
    state: Mutex<DiagnosticIngressState>,
}

impl DiagnosticIngressInner {
    fn publish_guarded(
        &self,
        bus: &DiagnosticBusInner,
        runtime: EvaluationRuntimeId,
        diagnostic: Diagnostic,
    ) -> Result<(DiagnosticEvent, Vec<Arc<dyn DiagnosticSubscriber>>), Error> {
        let prepared = match self.prepare(&diagnostic) {
            Ok(prepared) => prepared,
            Err(error) => {
                let event = self.record_failed_publication(bus, runtime, diagnostic, error)?;
                return Ok(event);
            }
        };
        let owner = match prepared.owner.upgrade() {
            Some(owner) => owner,
            None => {
                let error = Error::new(format!(
                    "evaluation runtime {} for input endpoint {} has been dropped",
                    self.sender.runtime.get(),
                    self.sender.endpoint.get()
                ));
                let event = self.record_failed_publication(bus, runtime, diagnostic, error)?;
                return Ok(event);
            }
        };
        // The shared guard makes bus sequence assignment and ingress route
        // selection one publication with respect to an exclusive fallback or
        // rearm boundary. Subscriber callbacks run only after it is dropped.
        let mutation = owner.mutation_admission.mutation_guard();
        let (event, subscribers) = {
            let mut bus = bus
                .state
                .lock()
                .expect("diagnostic bus mutex should not be poisoned");
            DiagnosticBus::validate_runtime_locked(&mut bus, runtime)?;
            DiagnosticBus::record_event_locked(&mut bus, diagnostic)
        };
        let changed = self.receive_prepared_guarded(event.clone(), prepared, &owner);
        let notification = changed.then(|| prepare_runtime_observation(&owner, &mutation));
        drop(mutation);
        if let Some(notification) = notification {
            notification.notify();
        }
        Ok((event, subscribers))
    }

    fn prepare(&self, diagnostic: &Diagnostic) -> Result<RuntimePreparedInput, Error> {
        let value = diagnostic.transport_value(&self.values)?;
        self.sender.prepare(value)
    }

    fn record_failed_publication(
        &self,
        bus: &DiagnosticBusInner,
        runtime: EvaluationRuntimeId,
        diagnostic: Diagnostic,
        error: Error,
    ) -> Result<(DiagnosticEvent, Vec<Arc<dyn DiagnosticSubscriber>>), Error> {
        let event = {
            let mut bus = bus
                .state
                .lock()
                .expect("diagnostic bus mutex should not be poisoned");
            DiagnosticBus::validate_runtime_locked(&mut bus, runtime)?;
            DiagnosticBus::record_event_locked(&mut bus, diagnostic)
        };
        self.state
            .lock()
            .expect("diagnostic ingress mutex should not be poisoned")
            .failure = Some(error);
        Ok(event)
    }

    fn receive_prepared_guarded(
        &self,
        event: DiagnosticEvent,
        prepared: RuntimePreparedInput,
        owner: &Arc<RuntimeSharedResources>,
    ) -> bool {
        let sequence = event.sequence();
        debug_assert!(Arc::ptr_eq(
            &prepared
                .owner
                .upgrade()
                .expect("guarded diagnostic ingress retains its runtime resources"),
            owner
        ));
        let mut state = self
            .state
            .lock()
            .expect("diagnostic ingress mutex should not be poisoned");
        if state.failure.is_some() || sequence < state.next_sequence {
            return false;
        }
        state.pending.insert(sequence, prepared);
        let mut changed = false;
        loop {
            let next = state.next_sequence;
            let Some(prepared) = state.pending.remove(&next) else {
                break;
            };
            match route_runtime_diagnostic_guarded(owner, prepared) {
                Ok(()) => {
                    changed = true;
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
        changed
    }
}

/// Keeps one diagnostic bus routed to its runtime FIFO.
///
/// The bus retains this ingress weakly and the runtime retains its routing
/// state. Dropping this escaping handle therefore neither detaches nor permits
/// a replacement lifecycle.
#[derive(Clone)]
pub struct DiagnosticIngress {
    pub(super) inner: Arc<DiagnosticIngressInner>,
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

    /// Selects the output endpoint used when no configured logger lifecycle
    /// owns this ingress. The endpoint must belong to the same runtime.
    pub fn set_fallback_output(&self, output: &RuntimeOutputWriter) -> Result<(), Error> {
        configure_runtime_diagnostic_fallback(&self.inner.sender, output)
    }

    /// Rearms this ingress for a configured logger lifecycle. Already queued
    /// fallback obligations retain their route; only later publications enter
    /// the logger input FIFO.
    pub fn activate(&self) -> Result<(), Error> {
        set_runtime_diagnostic_route(&self.inner.sender, RuntimeDiagnosticRouteMode::Active)
            .map(|_| ())
    }

    /// Atomically selects fallback and transfers every buffered logger input
    /// to ordered output obligations. Later publications remain on fallback
    /// until [`Self::activate`] is called.
    pub fn fallback(&self) -> Result<usize, Error> {
        set_runtime_diagnostic_route(&self.inner.sender, RuntimeDiagnosticRouteMode::Fallback)
    }

    /// Constructs the guarded terminal transition for one coordinator-owned
    /// logger root. Route selection and buffered-input transfer happen inside
    /// the root's existing runtime mutation publication; `after` runs only
    /// after that guard and the logger demand-session lease have been
    /// released.
    #[doc(hidden)]
    pub fn logger_terminal(
        &self,
        after: impl Fn() + Send + Sync + 'static,
    ) -> EffectLifecycleTerminal {
        let ingress = self.inner.clone();
        let runtime = ingress.sender.runtime;
        let after = Arc::new(after);
        EffectLifecycleTerminal::new(
            runtime,
            TaskStatusPublisher::new(move |mutation, _status| {
                let notification = match ingress.sender.owner.upgrade() {
                    Some(resources) => match set_runtime_diagnostic_route_guarded(
                        &resources,
                        ingress.sender.endpoint,
                        RuntimeDiagnosticRouteMode::Fallback,
                        mutation,
                    ) {
                        Ok((_transferred, notification)) => notification,
                        Err(error) => {
                            ingress
                                .state
                                .lock()
                                .expect("diagnostic ingress mutex should not be poisoned")
                                .failure = Some(error);
                            None
                        }
                    },
                    None => {
                        let failure = Error::new(format!(
                            "evaluation runtime {} for input endpoint {} has been dropped",
                            ingress.sender.runtime.get(),
                            ingress.sender.endpoint.get()
                        ));
                        ingress
                            .state
                            .lock()
                            .expect("diagnostic ingress mutex should not be poisoned")
                            .failure = Some(failure);
                        None
                    }
                };
                let after = after.clone();
                TaskStatusWake::new(move || {
                    if let Some(notification) = notification {
                        notification.notify();
                    }
                    after();
                })
            }),
        )
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

pub(super) struct DiagnosticCallback<F>(pub(super) F);

impl<F> DiagnosticSubscriber for DiagnosticCallback<F>
where
    F: Fn(DiagnosticEvent) + Send + Sync,
{
    fn receive(&self, event: DiagnosticEvent) {
        (self.0)(event);
    }
}
