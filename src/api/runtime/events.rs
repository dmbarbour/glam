use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::num::NonZeroU64;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use rpds::RedBlackTreeMapSync;

use super::accepted::{AcceptedJournal, AcceptedRecord};
use super::flags::{FlagJournal, FlagRecord};
use super::ports::{IMPORTS, PortAddress, PortJournal, PortState, Wire, WireEnd};
use super::shares::{ShareJournal, ShareRecord};
use super::states::{StateJournal, StateRecord};
use super::{RuntimeSharedResources, publish_runtime_observation};
use crate::api::{Error, Value};
use crate::core::Key;
use crate::reflection::{RuntimeInputEndpointId, RuntimeInputSequence};
use crate::runtime::{EvaluationRuntimeId, RuntimeValueRoot};

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
    /// The machine input to which each diagnostic ingress routes.
    diagnostic_routes: BTreeMap<RuntimeInputEndpointId, PortAddress>,
    /// Machines' port buffers and wiring.
    pub(in crate::api) ports: PortState,
    /// Which machines have a share of their environment.
    pub(in crate::api) shares: ShareRecord,
    /// Which child machines are flagged, in error.
    pub(in crate::api) flags: FlagRecord,
    /// The child machines' local states.
    pub(in crate::api) states: StateRecord,
    /// The promises machines accepted (`obligations-are-accepted-promises`).
    pub(in crate::api) accepted: AcceptedRecord,
}

impl RuntimeEventState {
    pub(super) fn new() -> Self {
        Self {
            inputs: RedBlackTreeMapSync::new_sync(),
            outputs: RuntimeOutputState::default(),
            diagnostic_routes: BTreeMap::new(),
            ports: PortState::new(),
            shares: ShareRecord::new(),
            flags: FlagRecord::new(),
            states: StateRecord::new(),
            accepted: AcceptedRecord::new(),
        }
    }

    /// Each machine's input left unread, by machine and port, the port
    /// `None` for a child's task queue, with how many items: for a queue,
    /// the tasks that have not started, which, on a machine not in error,
    /// are those behind its head, the task it runs.
    pub(in crate::api) fn backlogs(&self) -> Vec<(Vec<Key>, Option<Key>, usize)> {
        self.ports
            .backlogs()
            .into_iter()
            .filter_map(|(address, held)| {
                let (port, unstarted) = if address.port == *TASKS {
                    let running = usize::from(!self.flags.is_set(&address.machine));
                    (None, held - running)
                } else {
                    (Some(address.port), held)
                };
                (unstarted > 0).then_some((address.machine, port, unstarted))
            })
            .collect()
    }

    pub(super) fn snapshot(&self, runtime: EvaluationRuntimeId) -> RuntimeEventSnapshot {
        RuntimeEventSnapshot {
            runtime,
            inputs: self.inputs.clone(),
            ports: self.ports.clone(),
            shares: self.shares.clone(),
            flags: self.flags.clone(),
            states: self.states.clone(),
            accepted: self.accepted.clone(),
        }
    }

    pub(super) fn validate(&self, journal: &RuntimeEventJournal) -> bool {
        if journal.snapshot.runtime != journal.runtime {
            return false;
        }
        let valid_input = |endpoint: &RuntimeInputEndpointId, cursor: &RuntimeInputCursor| {
            let Some(input) = self.inputs.get(endpoint) else {
                return false;
            };
            input.head_sequence == cursor.start
                && cursor.next.get().saturating_sub(cursor.start.get())
                    <= input.admitted.len() as u64
                && (!cursor.observed_empty || input.next_sequence == cursor.next)
        };
        let observations = journal
            .observations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        observations
            .iter()
            .all(|(endpoint, cursor)| valid_input(endpoint, cursor))
            && journal
                .cursors
                .iter()
                .all(|(endpoint, cursor)| valid_input(endpoint, cursor))
            && journal.outputs.iter().all(|intent| {
                self.outputs
                    .ready_by_endpoint
                    .contains_key(&intent.endpoint)
                    && !self.outputs.accepted.contains(&intent.delivery)
            })
            && self.ports.validate(&journal.ports)
            && self.flags.validate(&journal.flags)
            && self.states.validate(&journal.states)
            && self.accepted.validate(&journal.accepted)
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
        let ports_changed = self.ports.commit(&journal.ports);
        let shares_changed = self.shares.commit(&journal.shares);
        let flags_changed = self.flags.commit(&journal.flags);
        let states_changed = self.states.commit(&journal.states);
        let accepted_changed = self.accepted.commit(&journal.accepted);
        input_changed
            || !journal.outputs.is_empty()
            || ports_changed
            || shares_changed
            || flags_changed
            || states_changed
            || accepted_changed
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
}

/// Immutable admitted-input state captured with a reflection-store snapshot.
#[doc(hidden)]
#[derive(Clone)]
pub struct RuntimeEventSnapshot {
    runtime: EvaluationRuntimeId,
    pub(in crate::api) inputs: RedBlackTreeMapSync<RuntimeInputEndpointId, Arc<RuntimeInputBuffer>>,
    pub(in crate::api) ports: PortState,
    pub(in crate::api) shares: ShareRecord,
    pub(in crate::api) flags: FlagRecord,
    pub(in crate::api) states: StateRecord,
    pub(in crate::api) accepted: AcceptedRecord,
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
    /// Monotone input observations shared by every alternative forked from
    /// this transaction. Input claims and output intents remain branch-local.
    ///
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    observations: Arc<Mutex<BTreeMap<RuntimeInputEndpointId, RuntimeInputCursor>>>,
    cursors: BTreeMap<RuntimeInputEndpointId, RuntimeInputCursor>,
    outputs: Vec<RuntimeOutputIntent>,
    /// Machines' port actions in this transaction.
    pub(in crate::api) ports: PortJournal,
    /// Which machines this transaction gave a share, or took one from.
    pub(in crate::api) shares: ShareJournal,
    /// Which child machines' flags this transaction read or changed.
    pub(in crate::api) flags: FlagJournal,
    /// Which child machines' states this transaction read or wrote.
    pub(in crate::api) states: StateJournal,
    /// Which promises this transaction accepted, abandoned or read the
    /// acceptance of.
    pub(in crate::api) accepted: AcceptedJournal,
}

/// The wiring a machine owns, as [`RuntimeEventJournal::owned_wiring`]
/// reads it.
pub(crate) struct OwnedWires {
    pub(crate) wires: Vec<(WireEnd, WireEnd)>,
    pub(crate) defaults: bool,
    pub(crate) removed_defaults: Vec<(WireEnd, WireEnd)>,
}

/// A run an interrupt stopped: its machine, its item's sequence in the
/// machine's queue, its task's record, and the children its task waited
/// within by call, which the interrupt flagged with it.
pub(crate) struct StoppedRun {
    pub(crate) machine: Vec<Key>,
    pub(crate) sequence: u64,
    pub(crate) task: Value,
    pub(crate) callees: Vec<Arc<[Key]>>,
}

impl RuntimeEventJournal {
    #[doc(hidden)]
    pub fn new(snapshot: RuntimeEventSnapshot) -> Self {
        let ports = PortJournal::new(snapshot.ports.clone());
        let shares = ShareJournal::new(&snapshot.shares);
        let flags = FlagJournal::new(&snapshot.flags);
        let states = StateJournal::new(&snapshot.states);
        let accepted = AcceptedJournal::new(&snapshot.accepted);
        Self {
            runtime: snapshot.runtime,
            snapshot,
            observations: Arc::new(Mutex::new(BTreeMap::new())),
            cursors: BTreeMap::new(),
            outputs: Vec::new(),
            ports,
            shares,
            flags,
            states,
            accepted,
        }
    }

    /// Accepts `resolver`'s promise for the machine at `machine`, owing what
    /// `diagnostic` says (`obligations-are-accepted-promises`).
    pub(crate) fn accept_promise(
        &mut self,
        machine: Vec<Key>,
        resolver: crate::core::ManagedResolverRoot,
        diagnostic: RuntimeValueRoot,
    ) {
        self.accepted.accept(machine, resolver, diagnostic);
    }

    /// Removes the machine at `machine` from `promise`'s acceptors, without
    /// resolving it.
    pub(crate) fn abandon_promise(&mut self, machine: Vec<Key>, promise: crate::core::PromiseId) {
        self.accepted.abandon(machine, promise);
    }

    /// Whether any machine accepted `promise`, observing it.
    pub(crate) fn promise_accepted(&self, promise: crate::core::PromiseId) -> bool {
        self.accepted.accepted(promise)
    }

    /// Queues `task`, an `{op, reply}` record, on the child machine at
    /// `machine`, appending it to the machine's task input when this journal
    /// commits (`reflection-tasks-are-machine-tasks`).
    pub(crate) fn queue_task(&mut self, machine: Vec<Key>, task: Value) {
        self.ports.append(task_address(machine), task);
    }

    /// How many tasks the child machine at `machine` holds, queued or
    /// running, observing its task input's end.
    pub(crate) fn queued_tasks(&mut self, machine: &[Key]) -> usize {
        self.ports.available(&task_address(machine.to_vec()))
    }

    /// Whether the child machine at `machine` is flagged, observing it.
    pub(crate) fn machine_flagged(&mut self, machine: &[Key]) -> bool {
        self.flags.read(machine)
    }

    /// Whether this journal queues tasks on the child machine at `machine`,
    /// which reach it only when the journal commits. The task queue is no
    /// ordinary port, whose input waits for a turn to read it: the child
    /// drains it by itself, each task work forked onto the child, so a
    /// synchronous entry after a queueing would overtake the task.
    pub(crate) fn queues_tasks_on(&self, machine: &[Key]) -> bool {
        self.ports.appends_to(&task_address(machine.to_vec()))
    }

    /// The local state of the child machine at `machine`, with this
    /// journal's writes, observing it.
    pub(crate) fn machine_state(&mut self, machine: &[Key]) -> Option<Value> {
        self.states.read(machine).map(Value::from_runtime_root)
    }

    /// Writes `state` as the local state of the child machine at `machine`.
    pub(crate) fn set_machine_state(&mut self, machine: &[Key], state: Value) {
        self.states.set(machine.to_vec(), state.into_runtime_root());
    }

    /// Whether the child machine at `machine` is busy: unflagged, with a
    /// task queued or running, as this journal's snapshot shows it. The
    /// reads are observed.
    pub(crate) fn machine_busy(&mut self, machine: &[Key]) -> bool {
        !self.flags.read(machine) && self.queued_tasks(machine) > 0
    }

    /// Whether the child machine at `machine`, or one of its descendants, is
    /// busy, as this journal's snapshot shows them. The reads are observed,
    /// up to the first busy machine.
    pub(crate) fn machines_busy(&mut self, machine: &[Key]) -> bool {
        self.ports
            .buffers_below(machine, &TASKS)
            .iter()
            .any(|address| self.machine_busy(&address.machine))
    }

    /// Interrupts the tasks running on the child machine at `machine` and
    /// its descendants, as this journal sees them: takes each from its
    /// queue, whose head it is, and flags its machine with `diagnostic`, the
    /// interrupt's, and each child the task's run waits within by call,
    /// which `calls` names, since the call is work the interrupt halts
    /// (`quiescence-is-analyzed`). Returns each run it stopped; where
    /// nothing runs, on a flagged or an idle machine, it changes nothing.
    /// Its reads are observed.
    pub(crate) fn interrupt_tasks(
        &mut self,
        machine: &[Key],
        diagnostic: &RuntimeValueRoot,
        calls: impl Fn(&[Key], u64) -> Vec<Arc<[Key]>>,
    ) -> Vec<StoppedRun> {
        let mut stopped = Vec::new();
        for address in self.ports.buffers_below(machine, &TASKS) {
            if self.flags.read(&address.machine) {
                continue;
            }
            let sequence = self.ports.next_sequence(&address);
            let Some(task) = self
                .ports
                .claim(&address, 1)
                .and_then(|mut claimed| claimed.pop())
            else {
                continue;
            };
            self.flags.set(address.machine.clone(), diagnostic.clone());
            let callees = calls(&address.machine, sequence);
            for callee in &callees {
                self.flags.set(callee.to_vec(), diagnostic.clone());
            }
            stopped.push(StoppedRun {
                machine: address.machine,
                sequence,
                task,
                callees,
            });
        }
        stopped
    }

    /// Recovers the child machine at `machine` where it is flagged: clears
    /// the flag, and takes every task from its queue, observing its end, so
    /// that a task queued meanwhile conflicts. Returns the tasks it took, or
    /// `None`, changing nothing, where the machine is not flagged.
    pub(crate) fn recover_machine(&mut self, machine: &[Key]) -> Option<Vec<Value>> {
        if !self.flags.read(machine) {
            return None;
        }
        self.flags.clear(machine.to_vec());
        Some(self.ports.claim_all(&task_address(machine.to_vec())))
    }

    /// Takes the next `n` values of the input `port` of the machine at
    /// `machine`, in the order they arrived, observing the port; `None`,
    /// taking nothing, where fewer are there, which is pushback
    /// (`machines-communicate-through-wired-ports`).
    pub(crate) fn take_from_port(
        &mut self,
        machine: &[Key],
        port: Key,
        n: usize,
    ) -> Option<Vec<Value>> {
        self.ports.claim(
            &PortAddress {
                machine: machine.to_vec(),
                port,
            },
            n,
        )
    }

    /// How many values the input `port` of the machine at `machine` holds
    /// beyond what this journal took, observed.
    pub(crate) fn port_count(&mut self, machine: &[Key], port: Key) -> usize {
        self.ports.available(&PortAddress {
            machine: machine.to_vec(),
            port,
        })
    }

    /// The inputs of the machine at `machine` that hold values beyond what
    /// this journal took, with how many, in port order, observed.
    pub(crate) fn ports_pending(&mut self, machine: &[Key]) -> Vec<(Key, usize)> {
        self.ports.pending(machine)
    }

    /// Sends `values`, in order, from the output `port` of the machine at
    /// `machine` to every buffer its wiring reaches. An output with no wire
    /// is an error, which sends nothing.
    pub(crate) fn send_from_port(
        &mut self,
        machine: &[Key],
        port: &Key,
        values: &[Value],
    ) -> Result<(), String> {
        let buffers = self.ports.route(machine, port)?;
        for value in values {
            for buffer in &buffers {
                self.ports.append(buffer.clone(), value.clone());
            }
        }
        Ok(())
    }

    /// Adds, or removes, the wire from `source` to `destination` that the
    /// machine at `owner` owns.
    pub(crate) fn set_wire(
        &mut self,
        owner: &[Key],
        source: WireEnd,
        destination: WireEnd,
        present: bool,
    ) {
        let wire = Wire {
            source,
            destination,
        };
        if present {
            self.ports.wire(owner.to_vec(), wire);
        } else {
            self.ports.unwire(owner.to_vec(), wire);
        }
    }

    /// The wiring the machine at `owner` owns: its wires, whether the
    /// default rule applies to it, and the default wires it removed.
    pub(crate) fn owned_wiring(&mut self, owner: &[Key]) -> OwnedWires {
        let pairs = |wires: Vec<Wire>| {
            wires
                .into_iter()
                .map(|wire| (wire.source, wire.destination))
                .collect()
        };
        let owned = self.ports.owned_wiring(owner);
        OwnedWires {
            wires: pairs(owned.wires),
            defaults: owned.defaults,
            removed_defaults: pairs(owned.suppressed),
        }
    }

    /// Turns the default rule for the wires the machine at `owner` owns on
    /// or off.
    pub(crate) fn set_wire_defaults(&mut self, owner: &[Key], enabled: bool) {
        self.ports.set_defaults(owner.to_vec(), enabled);
    }

    /// Returns the child machine at `machine` and its descendants, none of
    /// them busy, to their initial transactional state: takes every task
    /// from their queues, which only flagged machines hold, observing their
    /// ends, so that a task queued meanwhile conflicts, drops the tasks this
    /// journal queued on them, returns their port state to the initial one,
    /// and drops their shares and flags. Returns the tasks it took or
    /// dropped, none of which ran.
    pub(crate) fn clear_machines(&mut self, machine: &[Key]) -> Vec<Value> {
        let mut cleared = self.ports.appended_below(machine, &TASKS);
        for address in self.ports.buffers_below(machine, &TASKS) {
            cleared.extend(self.ports.claim_all(&address));
        }
        self.ports.clear_below(machine);
        self.shares.clear_below(machine.to_vec());
        self.flags.clear_below(machine);
        self.states.clear_below(machine);
        self.accepted.clear_below(machine);
        cleared
    }

    /// The machines below `machine` that hold port state, anonymous roots
    /// included.
    #[cfg(test)]
    pub(in crate::api) fn present_below_for_test(&self, machine: &[Key]) -> BTreeSet<Vec<Key>> {
        self.ports.present_below(machine)
    }

    /// The machines below `machine` that hold state here: port state, or a
    /// share of their environment. Anonymous roots, and the child machines
    /// below them, are left out: they are unnamed in listings
    /// (`reflection-tasks-are-machine-tasks`).
    pub(in crate::api) fn present_below(&self, machine: &[Key]) -> BTreeSet<Vec<Key>> {
        let mut present = self.ports.present_below(machine);
        present.extend(self.shares.set_below(machine));
        present.retain(|path| !path.iter().any(crate::reflection::is_anonymous_key));
        present
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
        let result = if cursor.next == snapshot.next_sequence {
            cursor.observed_empty = true;
            None
        } else {
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
            Some(record.payload.value(self.runtime))
        };
        let observation = cursor.clone();
        let mut observations = self
            .observations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        observations
            .entry(input.endpoint)
            .and_modify(|prior| {
                debug_assert_eq!(prior.start, observation.start);
                if observation.next > prior.next {
                    prior.next = observation.next;
                }
                prior.observed_empty |= observation.observed_empty;
            })
            .or_insert(observation);
        Ok(result)
    }

    /// Reads and claims the next value of the input `port` of the machine at
    /// `machine`, as a turn's read of one does, or observes it empty, so
    /// that the transaction is disturbed when a value arrives. For the CLI's
    /// configured logger, a reflection task that reads the logger machine's
    /// diagnostics until loggers are turns.
    #[doc(hidden)]
    pub fn read_port(
        &mut self,
        machine: impl Into<crate::api::MachinePath>,
        port: impl Into<Key>,
    ) -> Option<Value> {
        let address = PortAddress {
            machine: machine.into().keys().to_vec(),
            port: port.into(),
        };
        self.ports
            .claim(&address, 1)
            .map(|mut values| values.pop().expect("a claim of one holds one"))
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
                    crate::core::panic_payload_message(panic.as_ref())
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

pub(in crate::api) fn register_runtime_diagnostic_route(
    resources: &Arc<RuntimeSharedResources>,
    endpoint: RuntimeInputEndpointId,
    port: PortAddress,
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
    state.events.diagnostic_routes.insert(endpoint, port);
    Ok(())
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
    let address = state
        .events
        .diagnostic_routes
        .get(&prepared.endpoint)
        .cloned()
        .ok_or_else(|| Error::new("diagnostic ingress route is not registered"))?;
    state.events.ports.admit(address, prepared.payload);
    Ok(())
}

/// Writes a request the runtime makes for a source into the root's
/// `imports` input at once, outside any transaction, from where it passes on
/// as a child's write would (`imports-are-source-requests`).
pub(in crate::api) fn admit_runtime_request(
    resources: &Arc<RuntimeSharedResources>,
    request: RuntimeValueRoot,
) {
    debug_assert_eq!(request.runtime_id(), resources.id);
    let mutation = resources.mutation_admission.mutation_guard();
    resources
        .transactions
        .state
        .lock()
        .expect("runtime transaction mutex should not be poisoned")
        .events
        .ports
        .admit(
            PortAddress {
                machine: Vec::new(),
                port: IMPORTS.clone(),
            },
            request,
        );
    publish_runtime_observation(resources, mutation);
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

/// The input on which a child machine's tasks queue, named by an abstract
/// global path no turn's port shares (`reflection-tasks-are-machine-tasks`).
pub(in crate::api) static TASKS: std::sync::LazyLock<Key> =
    std::sync::LazyLock::new(|| Key::abstract_global_path(["glam", "machine", "tasks"]));

/// The task input of the child machine at `machine`.
pub(in crate::api) fn task_address(machine: Vec<Key>) -> PortAddress {
    PortAddress {
        machine,
        port: TASKS.clone(),
    }
}
