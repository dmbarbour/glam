use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::num::NonZeroU64;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Weak};

use rpds::RedBlackTreeMapSync;

use super::{
    RuntimeObservationNotification, RuntimeSharedResources, prepare_runtime_observation,
    publish_runtime_observation,
};
use crate::api::{Error, Value};
use crate::reflection::{RuntimeInputEndpointId, RuntimeInputSequence};
use crate::runtime::{EvaluationRuntimeId, RuntimeMutationAuthority, RuntimeValueRoot};

/// Runtime-local identity of one buffered-output endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeOutputEndpointId(NonZeroU64);

impl RuntimeOutputEndpointId {
    pub fn get(self) -> u64 {
        self.0.get()
    }

    pub(super) fn from_u64(id: u64) -> Option<Self> {
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

    pub(super) fn from_u64(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }
}

#[derive(Clone)]
pub(in crate::api) struct RuntimeInputRecord {
    pub(in crate::api) sequence: RuntimeInputSequence,
    pub(in crate::api) payload: RuntimeValueRoot,
}

#[derive(Clone)]
pub(in crate::api) struct RuntimeInputBuffer {
    pub(in crate::api) head_sequence: RuntimeInputSequence,
    pub(in crate::api) next_sequence: RuntimeInputSequence,
    pub(in crate::api) admitted: std::collections::VecDeque<RuntimeInputRecord>,
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
pub(in crate::api) enum RuntimeDeliveryState {
    Queued,
    Running,
}

pub(in crate::api) struct RuntimeDeliveryRecord {
    pub(in crate::api) endpoint: RuntimeOutputEndpointId,
    pub(in crate::api) payload: RuntimeValueRoot,
    pub(in crate::api) state: RuntimeDeliveryState,
}

#[derive(Default)]
pub(in crate::api) struct RuntimeOutputState {
    pub(in crate::api) accepted: BTreeSet<RuntimeDeliveryId>,
    pub(in crate::api) records: BTreeMap<RuntimeDeliveryId, RuntimeDeliveryRecord>,
    pub(in crate::api) ready_by_endpoint:
        BTreeMap<RuntimeOutputEndpointId, std::collections::VecDeque<RuntimeDeliveryId>>,
    pub(in crate::api) failures:
        RedBlackTreeMapSync<RuntimeDeliveryId, Arc<RuntimeDeliveryFailure>>,
    pub(in crate::api) pending_failure_reports:
        RedBlackTreeMapSync<RuntimeDeliveryId, Arc<RuntimeDeliveryFailure>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::api) enum RuntimeDiagnosticRouteMode {
    Active,
    Fallback,
}

struct RuntimeDiagnosticRoute {
    fallback: Option<RuntimeOutputEndpointId>,
    mode: RuntimeDiagnosticRouteMode,
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
    pub(super) runtime: EvaluationRuntimeId,
    pub(super) failures: RedBlackTreeMapSync<RuntimeDeliveryId, Arc<RuntimeDeliveryFailure>>,
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

pub(in crate::api) struct RuntimeEventState {
    pub(in crate::api) inputs: RedBlackTreeMapSync<RuntimeInputEndpointId, Arc<RuntimeInputBuffer>>,
    pub(in crate::api) outputs: RuntimeOutputState,
    diagnostic_routes: BTreeMap<RuntimeInputEndpointId, RuntimeDiagnosticRoute>,
}

impl RuntimeEventState {
    pub(super) fn new() -> Self {
        Self {
            inputs: RedBlackTreeMapSync::new_sync(),
            outputs: RuntimeOutputState::default(),
            diagnostic_routes: BTreeMap::new(),
        }
    }

    pub(super) fn snapshot(&self, runtime: EvaluationRuntimeId) -> RuntimeEventSnapshot {
        RuntimeEventSnapshot {
            runtime,
            inputs: self.inputs.clone(),
        }
    }

    pub(super) fn validate(&self, journal: &RuntimeEventJournal) -> bool {
        if journal.snapshot.runtime != journal.runtime {
            return false;
        }
        let inputs_valid = journal.cursors.iter().all(|(endpoint, cursor)| {
            let Some(input) = self.inputs.get(endpoint) else {
                return false;
            };
            input.head_sequence == cursor.start
                && cursor.next.get().saturating_sub(cursor.start.get())
                    <= input.admitted.len() as u64
                && (!cursor.observed_empty || input.next_sequence == cursor.next)
        });
        inputs_valid
            && journal.outputs.iter().all(|intent| {
                self.outputs
                    .ready_by_endpoint
                    .contains_key(&intent.endpoint)
                    && !self.outputs.accepted.contains(&intent.delivery)
            })
    }

    pub(super) fn commit_validated(&mut self, journal: &RuntimeEventJournal) -> bool {
        let mut input_changed = false;
        for (endpoint, cursor) in &journal.cursors {
            let count = cursor.next.get() - cursor.start.get();
            if count == 0 {
                continue;
            }
            input_changed = true;
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
                input.head_sequence = input
                    .head_sequence
                    .checked_next()
                    .expect("an admitted input always has a successor boundary");
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

    fn admit_input(
        &mut self,
        endpoint: RuntimeInputEndpointId,
        payload: RuntimeValueRoot,
    ) -> Result<RuntimeInputSequence, Error> {
        let input = self.inputs.get_mut(&endpoint).ok_or_else(|| {
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
        Ok(sequence)
    }

    fn queue_output(
        &mut self,
        endpoint: RuntimeOutputEndpointId,
        delivery: RuntimeDeliveryId,
        payload: RuntimeValueRoot,
    ) -> Result<(), Error> {
        let ready = self
            .outputs
            .ready_by_endpoint
            .get_mut(&endpoint)
            .ok_or_else(|| {
                Error::new(format!(
                    "runtime output endpoint {} is not registered",
                    endpoint.get()
                ))
            })?;
        if !self.outputs.accepted.insert(delivery) {
            return Err(Error::new(format!(
                "runtime delivery {} was already accepted",
                delivery.get()
            )));
        }
        let replaced = self.outputs.records.insert(
            delivery,
            RuntimeDeliveryRecord {
                endpoint,
                payload,
                state: RuntimeDeliveryState::Queued,
            },
        );
        assert!(replaced.is_none(), "accepted delivery IDs remain unique");
        ready.push_back(delivery);
        Ok(())
    }
}

/// Immutable admitted-input state captured with a reflection-store snapshot.
#[doc(hidden)]
#[derive(Clone)]
pub struct RuntimeEventSnapshot {
    runtime: EvaluationRuntimeId,
    pub(in crate::api) inputs: RedBlackTreeMapSync<RuntimeInputEndpointId, Arc<RuntimeInputBuffer>>,
}

#[derive(Clone)]
struct RuntimeInputCursor {
    start: RuntimeInputSequence,
    next: RuntimeInputSequence,
    observed_empty: bool,
}

/// Transaction-local observations and FIFO input claims.
///
/// Dropping this value abandons every claim; input is removed only by a
/// successful combined runtime commit.
#[doc(hidden)]
#[derive(Clone)]
pub struct RuntimeEventJournal {
    pub(super) runtime: EvaluationRuntimeId,
    snapshot: RuntimeEventSnapshot,
    cursors: BTreeMap<RuntimeInputEndpointId, RuntimeInputCursor>,
    outputs: Vec<RuntimeOutputIntent>,
}

impl RuntimeEventJournal {
    #[doc(hidden)]
    pub fn new(snapshot: RuntimeEventSnapshot) -> Self {
        Self {
            runtime: snapshot.runtime,
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
                observed_empty: false,
            });
        if cursor.next == snapshot.next_sequence {
            cursor.observed_empty = true;
            return Ok(None);
        }
        let offset = cursor.next.get() - snapshot.head_sequence.get();
        let record = snapshot
            .admitted
            .get(offset as usize)
            .expect("the snapshot sequence range and admitted roots agree");
        debug_assert_eq!(record.sequence, cursor.next);
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
    pub(super) runtime: EvaluationRuntimeId,
    pub(super) owner: Weak<RuntimeSharedResources>,
    pub(super) endpoint: RuntimeInputEndpointId,
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

pub(super) type RuntimeInputConverter<T> = dyn Fn(T) -> Result<Value, Error> + Send + Sync;

/// Typed host-side sender for one runtime input FIFO.
pub struct RuntimeInputSender<T> {
    pub(in crate::api) runtime: EvaluationRuntimeId,
    pub(in crate::api) owner: Weak<RuntimeSharedResources>,
    pub(in crate::api) endpoint: RuntimeInputEndpointId,
    pub(super) convert: Arc<RuntimeInputConverter<T>>,
    pub(super) marker: PhantomData<fn(T)>,
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
    pub(in crate::api) fn prepare(&self, input: T) -> Result<RuntimePreparedInput, Error> {
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

pub(in crate::api) struct RuntimePreparedInput {
    pub(in crate::api) runtime: EvaluationRuntimeId,
    pub(in crate::api) owner: Weak<RuntimeSharedResources>,
    pub(in crate::api) endpoint: RuntimeInputEndpointId,
    pub(in crate::api) payload: RuntimeValueRoot,
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
    pub(super) sender: RuntimeInputSender<T>,
    pub(super) reader: RuntimeInputReader,
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
    pub(super) runtime: EvaluationRuntimeId,
    pub(super) owner: Weak<RuntimeSharedResources>,
    pub(super) endpoint: RuntimeOutputEndpointId,
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

pub(super) type RuntimeOutputDecoder<T> = dyn Fn(Value) -> Result<T, Error> + Send + Sync;
pub(super) type RuntimeOutputAdapter<T> = dyn Fn(T) -> Result<(), Error> + Send + Sync;

/// Host-side claimant and adapter for one runtime output endpoint.
pub struct RuntimeOutputDelivery<T> {
    pub(super) runtime: EvaluationRuntimeId,
    pub(super) owner: Weak<RuntimeSharedResources>,
    pub(super) endpoint: RuntimeOutputEndpointId,
    pub(super) decode: Arc<RuntimeOutputDecoder<T>>,
    pub(super) adapter: Arc<RuntimeOutputAdapter<T>>,
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
    pub(super) writer: RuntimeOutputWriter,
    pub(super) delivery: RuntimeOutputDelivery<T>,
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

pub(in crate::api) fn register_runtime_diagnostic_route(
    resources: &Arc<RuntimeSharedResources>,
    endpoint: RuntimeInputEndpointId,
) -> Result<(), Error> {
    let _mutation = resources.mutation_admission.mutation_guard();
    let mut state = resources
        .transactions
        .state
        .lock()
        .expect("runtime transaction mutex should not be poisoned");
    if !state.events.inputs.contains_key(&endpoint) {
        return Err(Error::new(format!(
            "runtime input endpoint {} is not registered",
            endpoint.get()
        )));
    }
    if state.events.diagnostic_routes.contains_key(&endpoint) {
        return Err(Error::new(format!(
            "runtime input endpoint {} already has a diagnostic route",
            endpoint.get()
        )));
    }
    state.events.diagnostic_routes.insert(
        endpoint,
        RuntimeDiagnosticRoute {
            fallback: None,
            mode: RuntimeDiagnosticRouteMode::Active,
        },
    );
    Ok(())
}

pub(in crate::api) fn configure_runtime_diagnostic_fallback(
    input: &RuntimeInputSender<Value>,
    output: &RuntimeOutputWriter,
) -> Result<(), Error> {
    let resources = input.owner.upgrade().ok_or_else(|| {
        Error::new(format!(
            "evaluation runtime {} for input endpoint {} has been dropped",
            input.runtime.get(),
            input.endpoint.get()
        ))
    })?;
    let output_owner = output.validate_runtime(input.runtime)?;
    if !Arc::ptr_eq(&resources, &output_owner) {
        return Err(Error::new(
            "diagnostic input and fallback output do not share one runtime",
        ));
    }
    let settlement = resources.mutation_admission.settlement_guard();
    {
        let mut state = resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        if !state
            .events
            .outputs
            .ready_by_endpoint
            .contains_key(&output.endpoint)
        {
            return Err(Error::new(format!(
                "runtime output endpoint {} is not registered",
                output.endpoint.get()
            )));
        }
        let route = state
            .events
            .diagnostic_routes
            .get_mut(&input.endpoint)
            .ok_or_else(|| Error::new("diagnostic ingress route is not registered"))?;
        match route.fallback {
            Some(existing) if existing != output.endpoint => {
                return Err(Error::new(
                    "diagnostic ingress already has a different fallback output",
                ));
            }
            Some(_) => {}
            None => route.fallback = Some(output.endpoint),
        }
    }
    drop(settlement);
    resources.mutation_admission.notify_settlement();
    Ok(())
}

pub(in crate::api) fn set_runtime_diagnostic_route(
    input: &RuntimeInputSender<Value>,
    mode: RuntimeDiagnosticRouteMode,
) -> Result<usize, Error> {
    let resources = input.owner.upgrade().ok_or_else(|| {
        Error::new(format!(
            "evaluation runtime {} for input endpoint {} has been dropped",
            input.runtime.get(),
            input.endpoint.get()
        ))
    })?;
    let settlement = resources.mutation_admission.settlement_guard();
    let (transferred, notification) =
        set_runtime_diagnostic_route_guarded(&resources, input.endpoint, mode, &settlement)?;
    drop(settlement);
    resources.mutation_admission.notify_settlement();
    if let Some(notification) = notification {
        notification.notify();
    }
    Ok(transferred)
}

pub(in crate::api) fn set_runtime_diagnostic_route_guarded(
    resources: &Arc<RuntimeSharedResources>,
    input: RuntimeInputEndpointId,
    mode: RuntimeDiagnosticRouteMode,
    mutation: &dyn RuntimeMutationAuthority,
) -> Result<(usize, Option<RuntimeObservationNotification>), Error> {
    let mut transferred = 0;
    let mut changed = false;
    {
        let mut state = resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        let fallback = {
            let route = state
                .events
                .diagnostic_routes
                .get(&input)
                .ok_or_else(|| Error::new("diagnostic ingress route is not registered"))?;
            route.fallback
        };
        if mode == RuntimeDiagnosticRouteMode::Fallback {
            let fallback = fallback.ok_or_else(|| {
                Error::new("diagnostic ingress has no configured fallback output")
            })?;
            if !state
                .events
                .outputs
                .ready_by_endpoint
                .contains_key(&fallback)
            {
                return Err(Error::new(format!(
                    "runtime output endpoint {} is not registered",
                    fallback.get()
                )));
            }
            let count = state
                .events
                .inputs
                .get(&input)
                .ok_or_else(|| Error::new("diagnostic ingress input is not registered"))?
                .admitted
                .len();
            let deliveries = (0..count)
                .map(|_| {
                    resources.ids.delivery().map_err(Error::new).map(|id| {
                        RuntimeDeliveryId::from_u64(id.get())
                            .expect("runtime delivery IDs start at one")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let records = {
                let buffered = state
                    .events
                    .inputs
                    .get_mut(&input)
                    .expect("validated diagnostic input remains registered");
                let buffered = Arc::make_mut(buffered);
                let records = buffered.admitted.drain(..).collect::<Vec<_>>();
                buffered.head_sequence = buffered.next_sequence;
                records
            };
            transferred = records.len();
            for (record, delivery) in records.into_iter().zip(deliveries) {
                state
                    .events
                    .queue_output(fallback, delivery, record.payload)?;
            }
            changed = transferred != 0;
        }
        state
            .events
            .diagnostic_routes
            .get_mut(&input)
            .expect("validated diagnostic route remains registered")
            .mode = mode;
    }
    let notification = changed.then(|| prepare_runtime_observation(resources, mutation));
    Ok((transferred, notification))
}

pub(in crate::api) fn route_runtime_diagnostic_guarded(
    resources: &Arc<RuntimeSharedResources>,
    prepared: RuntimePreparedInput,
) -> Result<(), Error> {
    debug_assert_eq!(prepared.runtime, resources.id);
    debug_assert_eq!(prepared.payload.runtime_id(), resources.id);
    let mut state = resources
        .transactions
        .state
        .lock()
        .expect("runtime transaction mutex should not be poisoned");
    let route = state
        .events
        .diagnostic_routes
        .get(&prepared.endpoint)
        .ok_or_else(|| Error::new("diagnostic ingress route is not registered"))?;
    match route.mode {
        RuntimeDiagnosticRouteMode::Active => {
            state
                .events
                .admit_input(prepared.endpoint, prepared.payload)?;
        }
        RuntimeDiagnosticRouteMode::Fallback => {
            let fallback = route.fallback.ok_or_else(|| {
                Error::new("diagnostic ingress has no configured fallback output")
            })?;
            let id = resources.ids.delivery().map_err(Error::new)?;
            let delivery =
                RuntimeDeliveryId::from_u64(id.get()).expect("runtime delivery IDs start at one");
            state
                .events
                .queue_output(fallback, delivery, prepared.payload)?;
        }
    }
    Ok(())
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
        state.events.admit_input(endpoint, payload)?
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
                state
                    .events
                    .outputs
                    .pending_failure_reports
                    .insert_mut(delivery, failure.clone());
            }
            retired
        };
    publish_runtime_observation(resources, mutation);
    drop(retired);
    Ok(failure)
}

pub(super) fn runtime_delivery_failure_snapshot(
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
