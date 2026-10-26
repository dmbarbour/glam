//! `.port.*` and `.wire.*`: a task's ports and wiring, on the machine its
//! branch runs on, as a turn's are on its machine
//! (`machines-communicate-through-wired-ports`). Too few values to read is
//! pushback, which no alternative takes: the outermost cut waits for the
//! port to change, and outside every cut the read waits. A write to an
//! output with no wire is an error.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::api::{RuntimeEventJournal, Value, WireEnd};
use crate::core::{Key, Value as CoreValue};
use crate::list::{ListFrontStep, ListItem};
use crate::number::Number;

use super::super::protocol::{
    CommitResult, RequestContext, RequestResult, SpecializationRequestInput,
    SpecializationRequestPoll, TaskCommit, TaskHalt, TaskHost, TaskSpecialization,
};
use super::super::store::StoreJournal;
use super::{
    KeyConversionRequestWork, PreparationPoll, ReflectionHost, ReflectionTransaction,
    branch_machine, demand_value,
};

/// One request of the port family.
#[derive(Clone, Copy)]
pub(super) enum PortRequest {
    /// `.port.read Port N`: the next `N` values of the input, as a list.
    Read,
    /// `.port.unread Port List`: puts the list's values back at the
    /// input's head, in order, for the next read.
    Unread,
    /// `.port.write Port List`: sends the list's values, in order.
    Write,
    /// `.port.available Port`: how many values the input holds, observed.
    Available,
    /// `.port.pending`: the inputs that hold values, with how many.
    Pending,
    /// `.port.ask Port Request`: writes `{request, reply}` and returns the
    /// reply's promise (`requests-travel-on-ports`).
    Ask,
    /// `.wire.add Source Destination`.
    WireAdd,
    /// `.wire.del Source Destination`.
    WireDel,
    /// `.wire.list`: the wiring the machine owns.
    WireList,
    /// `.wire.defaults 'on` or `'off`: the default rule for its wires.
    WireDefaults,
}

impl PortRequest {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Read => ".port.read",
            Self::Unread => ".port.unread",
            Self::Write => ".port.write",
            Self::Available => ".port.available",
            Self::Pending => ".port.pending",
            Self::Ask => ".port.ask",
            Self::WireAdd => ".wire.add",
            Self::WireDel => ".wire.del",
            Self::WireList => ".wire.list",
            Self::WireDefaults => ".wire.defaults",
        }
    }

    /// How many leading arguments convert to keys: a port, a count, an
    /// endpoint or a setting.
    fn keys(self) -> usize {
        match self {
            Self::Read | Self::WireAdd | Self::WireDel => 2,
            Self::Unread | Self::Write | Self::Available | Self::Ask | Self::WireDefaults => 1,
            Self::Pending | Self::WireList => 0,
        }
    }

    /// The operand after the keys, where the request takes one.
    fn operand(self) -> Option<Operand> {
        match self {
            Self::Unread | Self::Write => Some(Operand::List),
            Self::Ask => Some(Operand::Value),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operand {
    /// A list, whose spine the request walks.
    List,
    /// A value, kept as given.
    Value,
}

/// A port request's preparation: its keys, then its operand.
pub(super) struct PortRequestWork {
    request: PortRequest,
    state: PortRequestState,
}

enum PortRequestState {
    Start(Vec<Value>),
    Keys {
        rest: VecDeque<Value>,
        converting: Option<KeyConversionRequestWork>,
        keys: Vec<Key>,
        operand: Option<Value>,
    },
    List {
        keys: Vec<Key>,
        list: ValueListRequestWork,
    },
    Done,
}

impl PortRequestWork {
    pub(super) fn new(request: PortRequest, arguments: Vec<Value>) -> Self {
        Self {
            request,
            state: PortRequestState::Start(arguments),
        }
    }

    pub(super) fn poll<S>(
        &mut self,
        input: Option<SpecializationRequestInput>,
        context: &mut RequestContext<'_, S>,
    ) -> Result<SpecializationRequestPoll, TaskHalt>
    where
        S: TaskSpecialization,
        S::Host: ReflectionHost<S>,
        S::Journal: ReflectionTransaction,
    {
        let request = self.request;
        match std::mem::replace(&mut self.state, PortRequestState::Done) {
            PortRequestState::Start(arguments) => {
                assert!(input.is_none(), "new port work cannot have demand input");
                let arity = request.keys() + usize::from(request.operand().is_some());
                if arguments.len() != arity {
                    return Err(TaskHalt::new(format!(
                        "`{}` received the wrong number of arguments",
                        request.name()
                    )));
                }
                let mut rest: VecDeque<Value> = arguments.into();
                let operand = request
                    .operand()
                    .map(|_| rest.pop_back().expect("the arity was checked"));
                self.state = PortRequestState::Keys {
                    rest,
                    converting: None,
                    keys: Vec::new(),
                    operand,
                };
                Ok(SpecializationRequestPoll::Continue)
            }
            PortRequestState::Keys {
                rest,
                converting: Some(mut conversion),
                mut keys,
                operand,
            } => {
                let (converting, poll) = match conversion.poll(input, context)? {
                    PreparationPoll::Ready(key) => {
                        keys.push(key);
                        (None, SpecializationRequestPoll::Continue)
                    }
                    PreparationPoll::Progress => {
                        (Some(conversion), SpecializationRequestPoll::Continue)
                    }
                    PreparationPoll::Demand(value) => {
                        (Some(conversion), SpecializationRequestPoll::Demand(value))
                    }
                };
                self.state = PortRequestState::Keys {
                    rest,
                    converting,
                    keys,
                    operand,
                };
                Ok(poll)
            }
            PortRequestState::Keys {
                mut rest,
                converting: None,
                keys,
                operand,
            } => {
                assert!(input.is_none(), "idle port work cannot have input");
                if let Some(next) = rest.pop_front() {
                    self.state = PortRequestState::Keys {
                        rest,
                        converting: Some(KeyConversionRequestWork::new(next)),
                        keys,
                        operand,
                    };
                    return Ok(SpecializationRequestPoll::Continue);
                }
                match (request.operand(), operand) {
                    (Some(Operand::List), Some(list)) => {
                        self.state = PortRequestState::List {
                            keys,
                            list: ValueListRequestWork::new(list),
                        };
                        Ok(SpecializationRequestPoll::Continue)
                    }
                    (_, operand) => Ok(SpecializationRequestPoll::Complete(act(
                        request,
                        keys,
                        operand,
                        Vec::new(),
                        context,
                    )?)),
                }
            }
            PortRequestState::List { keys, mut list } => match list.poll(input, context)? {
                PreparationPoll::Ready(values) => Ok(SpecializationRequestPoll::Complete(act(
                    request, keys, None, values, context,
                )?)),
                PreparationPoll::Progress => {
                    self.state = PortRequestState::List { keys, list };
                    Ok(SpecializationRequestPoll::Continue)
                }
                PreparationPoll::Demand(value) => {
                    self.state = PortRequestState::List { keys, list };
                    Ok(SpecializationRequestPoll::Demand(value))
                }
            },
            PortRequestState::Done => panic!("completed port work was polled again"),
        }
    }
}

/// What a port action made of the journal.
enum PortAct<T> {
    Done(T),
    /// Too few values to read.
    Pushback,
}

/// How a port action ended.
enum PortOutcome<T> {
    Done(T),
    /// Pushback, at the generation of the state the action read.
    Pushback(u64),
    Cancelled,
}

/// What a port action does with the ports.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PortAccess {
    /// Reads them.
    Observe,
    /// Reads and edits them.
    Edit,
    /// Edits them, reading nothing: a putback.
    Put,
}

/// Runs `action` on the event journal: within the current transaction, or,
/// outside one, in a transaction of its own that commits at once, again on a
/// conflict, where it edits. Within a transaction an action that reads is
/// observed, so a cut that fails after it waits for what it read to change.
fn act_on_ports<S, T>(
    name: &str,
    context: &mut RequestContext<'_, S>,
    access: PortAccess,
    mut action: impl FnMut(&mut RuntimeEventJournal) -> Result<PortAct<T>, TaskHalt>,
) -> Result<PortOutcome<T>, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let no_machines = || TaskHalt::new(format!("`{name}` needs a host with machines"));
    if let Some(generation) = context.transaction_generation() {
        let acted = {
            let mut transaction = context
                .transaction()
                .expect("a transaction has a generation");
            let events = transaction
                .parts()
                .1
                .reflection_journal()
                .events
                .as_mut()
                .ok_or_else(no_machines)?;
            action(events)?
        };
        if access != PortAccess::Put {
            context.observe_host_generation(generation);
        }
        return Ok(match acted {
            PortAct::Done(value) => PortOutcome::Done(value),
            PortAct::Pushback => PortOutcome::Pushback(generation),
        });
    }
    loop {
        let snapshot = context.host().snapshot();
        let generation = snapshot.generation();
        let store = StoreJournal::new(snapshot.store().clone());
        let mut journal = S::begin_journal(snapshot.extra());
        let events = journal
            .reflection_journal()
            .events
            .as_mut()
            .ok_or_else(no_machines)?;
        let value = match action(events)? {
            PortAct::Done(value) => value,
            PortAct::Pushback => return Ok(PortOutcome::Pushback(generation)),
        };
        if access == PortAccess::Observe {
            return Ok(PortOutcome::Done(value));
        }
        match context
            .host()
            .commit(TaskCommit::new(store, snapshot.extra().clone(), journal))
        {
            CommitResult::Committed => {
                context.committed();
                return Ok(PortOutcome::Done(value));
            }
            CommitResult::Conflict => {}
            CommitResult::MissingVolume(volume) => {
                return Err(TaskHalt::new(format!(
                    "reflection volume {} is unavailable",
                    volume.get()
                )));
            }
            CommitResult::Closed => return Ok(PortOutcome::Cancelled),
        }
    }
}

/// Runs `request` on the machine the current branch runs on, with its
/// converted `keys`, its `operand` where it takes a value, and the `list`
/// it walked where it takes one.
fn act<S>(
    request: PortRequest,
    keys: Vec<Key>,
    operand: Option<Value>,
    list: Vec<Value>,
    context: &mut RequestContext<'_, S>,
) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let name = request.name();
    let machine = branch_machine(context)?;
    let values = context.values();
    let mut keys = keys.into_iter();
    let mut key = || keys.next().expect("the keys were converted by arity");
    let outcome = match request {
        PortRequest::Read => {
            let port = key();
            let count = count(name, &key())?;
            act_on_ports(name, context, PortAccess::Edit, |events| {
                Ok(match events.take_from_port(&machine, port.clone(), count) {
                    Some(read) => PortAct::Done(read),
                    None => PortAct::Pushback,
                })
            })?
            .map(|read| values.list(read))
        }
        PortRequest::Available => {
            let port = key();
            act_on_ports(name, context, PortAccess::Observe, |events| {
                Ok(PortAct::Done(events.port_count(&machine, port.clone())))
            })?
            .map(|count| Ok(integer(&values, count)))
        }
        PortRequest::Pending => act_on_ports(name, context, PortAccess::Observe, |events| {
            Ok(PortAct::Done(events.ports_pending(&machine)))
        })?
        .map(|pending| {
            values.dictionary(
                pending
                    .into_iter()
                    .map(|(port, count)| (values.key(&port), integer(&values, count))),
            )
        }),
        PortRequest::Unread => {
            let port = key();
            act_on_ports(name, context, PortAccess::Put, |events| {
                events.put_back_to_port(&machine, port.clone(), &list);
                Ok(PortAct::Done(()))
            })?
            .map(|()| Ok(values.unit()))
        }
        PortRequest::Write => {
            let port = key();
            act_on_ports(name, context, PortAccess::Edit, |events| {
                events
                    .send_from_port(&machine, &port, &list)
                    .map_err(|error| TaskHalt::new(format!("`{name}`: {error}")))?;
                Ok(PortAct::Done(()))
            })?
            .map(|()| Ok(values.unit()))
        }
        PortRequest::Ask => return ask(name, &machine, key(), operand, context),
        PortRequest::WireAdd | PortRequest::WireDel => {
            let source = endpoint(name, &key())?;
            let destination = endpoint(name, &key())?;
            let present = matches!(request, PortRequest::WireAdd);
            act_on_ports(name, context, PortAccess::Edit, |events| {
                events.set_wire(&machine, source.clone(), destination.clone(), present);
                Ok(PortAct::Done(()))
            })?
            .map(|()| Ok(values.unit()))
        }
        PortRequest::WireList => act_on_ports(name, context, PortAccess::Observe, |events| {
            Ok(PortAct::Done(events.owned_wiring(&machine)))
        })?
        .map(|owned| {
            let wires = |wires: Vec<(WireEnd, WireEnd)>| {
                values.list(wires.into_iter().map(|(source, destination)| {
                    values
                        .record([
                            ("from", values.key(&endpoint_key(source))),
                            ("to", values.key(&endpoint_key(destination))),
                        ])
                        .expect("a wire's record is plain data")
                }))
            };
            values.record([
                ("wires", wires(owned.wires)?),
                (
                    "defaults",
                    values.atom_from_text(if owned.defaults { "on" } else { "off" }),
                ),
                ("unwired", wires(owned.removed_defaults)?),
            ])
        }),
        PortRequest::WireDefaults => {
            let enabled = setting(name, &key())?;
            act_on_ports(name, context, PortAccess::Edit, |events| {
                events.set_wire_defaults(&machine, enabled);
                Ok(PortAct::Done(()))
            })?
            .map(|()| Ok(values.unit()))
        }
    };
    Ok(match outcome {
        PortOutcome::Done(value) => {
            RequestResult::Return(value.map_err(|error| TaskHalt::new(error.to_string()))?)
        }
        PortOutcome::Pushback(generation) => RequestResult::Pushback { generation },
        PortOutcome::Cancelled => RequestResult::Cancelled,
    })
}

impl<T> PortOutcome<T> {
    fn map<U>(self, map: impl FnOnce(T) -> U) -> PortOutcome<U> {
        match self {
            Self::Done(value) => PortOutcome::Done(map(value)),
            Self::Pushback(generation) => PortOutcome::Pushback(generation),
            Self::Cancelled => PortOutcome::Cancelled,
        }
    }
}

/// `.port.ask Port Request`: allocates the reply's promise, writes
/// `{request, reply}` from the output `port`, and answers the promise;
/// where the write fails, it forgets the promise.
fn ask<S>(
    name: &str,
    machine: &[Key],
    port: Key,
    request: Option<Value>,
    context: &mut RequestContext<'_, S>,
) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let request = request.expect("`.port.ask` takes its request after its port");
    let values = context.values();
    let promises = match context.transaction() {
        Some(mut transaction) => transaction.store().snapshot().promises().clone(),
        None => context.host().snapshot().store().promises().clone(),
    };
    let (promise, promised, resolver) = values.with_access(|scoped| {
        let (promise, promised, resolver) = promises.allocate(scoped.runtime_access());
        (promise, scoped.wrap(promised), scoped.wrap(resolver))
    });
    let posted = values
        .record([("request", request), ("reply", resolver)])
        .map_err(|error| TaskHalt::new(error.to_string()))
        .and_then(|posted| {
            act_on_ports(name, context, PortAccess::Edit, |events| {
                events
                    .send_from_port(machine, &port, std::slice::from_ref(&posted))
                    .map_err(|error| TaskHalt::new(format!("`{name}`: {error}")))?;
                Ok(PortAct::Done(()))
            })
        });
    match posted {
        Ok(PortOutcome::Done(())) => Ok(RequestResult::Return(promised)),
        Ok(PortOutcome::Pushback(generation)) => {
            promises.forget(promise);
            Ok(RequestResult::Pushback { generation })
        }
        Ok(PortOutcome::Cancelled) => {
            promises.forget(promise);
            Ok(RequestResult::Cancelled)
        }
        Err(error) => {
            promises.forget(promise);
            Err(error)
        }
    }
}

fn integer(values: &crate::api::Values, count: usize) -> Value {
    values.integer(i64::try_from(count).expect("a port's count fits an integer"))
}

/// A read's count, a natural number.
fn count(name: &str, key: &Key) -> Result<usize, TaskHalt> {
    match key {
        Key::Number(number) => number.to_usize_if_integer(),
        _ => None,
    }
    .ok_or_else(|| TaskHalt::new(format!("`{name}` reads a natural number of values")))
}

/// `'on` or `'off`.
fn setting(name: &str, key: &Key) -> Result<bool, TaskHalt> {
    if *key == Key::atom_from_text("on") {
        Ok(true)
    } else if *key == Key::atom_from_text("off") {
        Ok(false)
    } else {
        Err(TaskHalt::new(format!("`{name}` takes `'on` or `'off`")))
    }
}

/// A wire's end: `in:Port`, `out:Port` or `child:{key:Key, port:Port}`.
fn endpoint(name: &str, key: &Key) -> Result<WireEnd, TaskHalt> {
    let wrong = || {
        TaskHalt::new(format!(
            "`{name}` takes `in:Port`, `out:Port` or `child:{{key:Key, port:Port}}`"
        ))
    };
    let Key::Dict(entries) = key else {
        return Err(wrong());
    };
    let [(tag, value)] = entries.as_ref() else {
        return Err(wrong());
    };
    if *tag == Key::atom_from_text("in") {
        return Ok(WireEnd::In(value.clone()));
    }
    if *tag == Key::atom_from_text("out") {
        return Ok(WireEnd::Out(value.clone()));
    }
    if *tag != Key::atom_from_text("child") {
        return Err(wrong());
    }
    let Key::Dict(child) = value else {
        return Err(wrong());
    };
    let field = |name: &str| {
        child
            .iter()
            .find(|(field, _)| *field == Key::atom_from_text(name))
            .map(|(_, value)| value.clone())
    };
    match (field("key"), field("port"), child.len()) {
        (Some(key), Some(port), 2) => Ok(WireEnd::Child(key, port)),
        _ => Err(wrong()),
    }
}

/// A wire's end as the key `endpoint` reads.
fn endpoint_key(end: WireEnd) -> Key {
    let tagged = |tag: &str, value: Key| Key::Dict(Arc::from([(Key::atom_from_text(tag), value)]));
    match end {
        WireEnd::In(port) => tagged("in", port),
        WireEnd::Out(port) => tagged("out", port),
        WireEnd::Child(child, port) => tagged(
            "child",
            Key::Dict(Arc::from([
                (Key::atom_from_text("key"), child),
                (Key::atom_from_text("port"), port),
            ])),
        ),
    }
}

/// Walks a list's spine, evaluating it as far as its last cell, and keeps
/// its values as they are, unevaluated.
struct ValueListRequestWork {
    pending: Option<ListDemand>,
    lists: Vec<crate::api::EvaluatedValue>,
    values: Vec<Value>,
}

enum ListDemand {
    Start(Value),
    Awaiting,
}

impl ValueListRequestWork {
    fn new(value: Value) -> Self {
        Self {
            pending: Some(ListDemand::Start(value)),
            lists: Vec::new(),
            values: Vec::new(),
        }
    }

    fn poll<S: TaskSpecialization>(
        &mut self,
        input: Option<SpecializationRequestInput>,
        context: &RequestContext<'_, S>,
    ) -> Result<PreparationPoll<Vec<Value>>, TaskHalt> {
        if let Some(pending) = self.pending.take() {
            return match pending {
                ListDemand::Start(value) => {
                    assert!(input.is_none(), "new list demand cannot have input");
                    self.pending = Some(ListDemand::Awaiting);
                    Ok(PreparationPoll::Demand(value))
                }
                ListDemand::Awaiting => {
                    let value = demand_value(input, "resumed value-list demand")?;
                    let is_list =
                        value.with_core_access(|value, _| matches!(value, CoreValue::List(_)))?;
                    if !is_list {
                        return Err(TaskHalt::new("`.port.write` writes a list of values"));
                    }
                    self.lists.push(value);
                    Ok(PreparationPoll::Progress)
                }
            };
        }
        assert!(input.is_none(), "idle value-list work cannot have input");
        let Some(list) = self.lists.pop() else {
            return Ok(PreparationPoll::Ready(std::mem::take(&mut self.values)));
        };
        let (item, rest, deferred) = list.with_core_access(|value, access| {
            let CoreValue::List(list) = value else {
                unreachable!("value-list work retains only validated lists")
            };
            let step = list
                .pop_front_step_by(&mut |value| access.duplicate_value(value), &mut |thunk| {
                    thunk.duplicate_as_value_in(access)
                });
            let wrap = |value| context.values().wrap_in(access, value);
            match step {
                ListFrontStep::Empty => (None, None, None),
                ListFrontStep::Item { item, tail } => (
                    Some(match item {
                        ListItem::Byte(byte) => wrap(CoreValue::Number(Number::from_u8(byte))),
                        ListItem::Value(value) => wrap(value),
                    }),
                    Some(wrap(CoreValue::List(tail))),
                    None,
                ),
                ListFrontStep::Deferred { deferred, suffix } => (
                    None,
                    Some(wrap(CoreValue::List(suffix))),
                    Some(wrap(deferred)),
                ),
            }
        })?;
        if let Some(rest) = rest {
            self.lists.push(crate::api::EvaluatedValue::from_whnf(
                &context.values(),
                rest,
            ));
        }
        if let Some(item) = item {
            self.values.push(item);
        }
        if let Some(deferred) = deferred {
            self.pending = Some(ListDemand::Start(deferred));
        }
        Ok(PreparationPoll::Progress)
    }
}
