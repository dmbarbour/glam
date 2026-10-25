use std::sync::Arc;

use crate::api::{Diagnostic, Value};
use crate::core::{Atom, Dict, Key, RuntimeValueAccess, Value as CoreValue, keys};
use crate::diagnostic::Severity;
use crate::evaluation::{
    EvalContext, EvaluationTaskId, MachineHost, MachineTaskOutcome, QueuedTask, RunHalt,
    TaskMachine,
};
use crate::list::{ListFrontStep, ListItem};
use crate::number::Number;
use crate::runtime::RuntimeFailureRoot;

use super::protocol::{
    CommitResult, EffectRequestSpec, RequestContext, RequestResult, SpecializationRequestInput,
    SpecializationRequestPoll, SpecializationRequestWork, TaskCommit, TaskEnvironment, TaskHalt,
    TaskHost, TaskSpecialization,
};
use super::store::StoreJournal;

/// Requests shared by every full reflection task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectionRequest {
    Environment,
    DictItems,
    MetadataInspect,
    Log,
    ChildTask,
    ChildStatus,
    ChildRecover,
    ChildInterrupt,
    ChildClear,
    ChildCall,
    /// The close of a `.child.call`, which Glam code cannot name.
    ChildCallLeave,
    PromiseAccept,
    PromiseAbandon,
    PromiseAccepted,
    /// `.TEST_ONLY.queue_now Key Op`: queues `Op` on the child `Key`,
    /// committing at once even within a transaction, as another writer
    /// would, so that a test orders a commit against a transaction.
    #[cfg(test)]
    TestQueueNow,
    /// `.TEST_ONLY.accept_now Resolver Diagnostic`: accepts the promise for
    /// the current machine, committing at once even within a transaction,
    /// as another machine would.
    #[cfg(test)]
    TestAcceptNow,
}

/// Durable interpretation state for one reusable reflection request.
///
/// Each operation owns its completed preparation and any outstanding WHNF or
/// shared-completion demand, so dependency wakeup resumes the operation rather
/// than re-entering request dispatch.
pub struct ReflectionRequestWork {
    operation: ReflectionRequestOperation,
}

enum ReflectionRequestOperation {
    Environment(EnvironmentRequestWork),
    Inspection(InspectionRequestWork),
    Log(LogRequestWork),
    Child(ChildRequestWork),
    ChildCallLeave,
    Acceptance(AcceptanceRequestWork),
    Poisoned,
}

/// `.promise.accept Resolver Diagnostic`, `.promise.abandon Resolver` and
/// `.promise.accepted Resolver`, which evaluate their resolver first
/// (`obligations-are-accepted-promises`).
enum AcceptanceRequestWork {
    Start {
        request: AcceptanceRequest,
        arguments: Vec<Value>,
    },
    Awaiting {
        request: AcceptanceRequest,
        /// What an acceptance owes, kept as given.
        diagnostic: Option<Value>,
    },
}

#[derive(Clone, Copy)]
enum AcceptanceRequest {
    Accept,
    Abandon,
    Accepted,
    #[cfg(test)]
    AcceptNow,
}

impl AcceptanceRequest {
    fn name(self) -> &'static str {
        match self {
            Self::Accept => ".promise.accept",
            Self::Abandon => ".promise.abandon",
            Self::Accepted => ".promise.accepted",
            #[cfg(test)]
            Self::AcceptNow => ".TEST_ONLY.accept_now",
        }
    }

    /// Whether the request takes a diagnostic after its resolver.
    fn takes_diagnostic(self) -> bool {
        #[cfg(test)]
        if matches!(self, Self::AcceptNow) {
            return true;
        }
        matches!(self, Self::Accept)
    }
}

/// `.child.task Key Op`, `.child.status Key`, `.child.recover Key`,
/// `.child.interrupt Key Reason`, `.child.clear Key` and `.child.call Key
/// Op`, which convert their key first (`reflection-tasks-are-machine-tasks`).
struct ChildRequestWork {
    request: ChildRequest,
    state: ChildRequestState,
}

#[derive(Clone, Copy)]
enum ChildRequest {
    Task,
    Status,
    Recover,
    Interrupt,
    Clear,
    Call,
    #[cfg(test)]
    QueueNow,
}

impl ChildRequest {
    /// Whether the request takes a value after its key: a task's or a call's
    /// operation, or an interrupt's reason.
    fn takes_value(self) -> bool {
        #[cfg(test)]
        if matches!(self, Self::QueueNow) {
            return true;
        }
        matches!(self, Self::Task | Self::Interrupt | Self::Call)
    }
}

enum ChildRequestState {
    Start(Vec<Value>),
    Key {
        key: KeyConversionRequestWork,
        /// The value after the key, where the request takes one.
        value: Option<Value>,
    },
}

enum InspectionRequestWork {
    Start {
        request: InspectionRequest,
        arguments: Vec<Value>,
    },
    Awaiting(InspectionRequest),
}

#[derive(Clone, Copy)]
enum InspectionRequest {
    DictItems,
    Metadata,
}

enum LogRequestWork {
    Start(Vec<Value>),
    Message {
        severity: Value,
    },
    MessageInterface {
        severity: Value,
        message: crate::api::EvaluatedValue,
    },
    Severity {
        message: Value,
    },
}

enum EnvironmentRequestWork {
    Start(Vec<Value>),
    KeyPath(KeyListRequestWork),
    ValuePath(ValuePathRequestWork),
}

pub(crate) struct ValuePathRequestWork {
    path: Vec<Key>,
    next: usize,
    current: Value,
    awaiting: bool,
}

struct KeyConversionRequestWork {
    state: KeyConversionRequestState,
}

enum KeyConversionRequestState {
    Demand(Value),
    Awaiting,
    List(Box<KeyListRequestWork>),
    Dict(Box<DictKeyRequestWork>),
    Poisoned,
}

struct DictKeyRequestWork {
    members: Vec<(Key, Value)>,
    next: usize,
    converted: Vec<(Key, Key)>,
    child: Option<KeyConversionRequestWork>,
}

pub(crate) struct KeyListRequestWork {
    pending: Option<ListDemand>,
    lists: Vec<crate::api::EvaluatedValue>,
    child: Option<Box<KeyConversionRequestWork>>,
    converted: Vec<Key>,
}

enum KeyListStep {
    Empty,
    Byte { byte: u8, tail: Value },
    Value { value: Value, tail: Value },
    Deferred { deferred: Value, suffix: Value },
}

enum ListDemand {
    Start(Value),
    Awaiting,
}

pub(crate) enum PreparationPoll<T> {
    Progress,
    Demand(Value),
    Ready(T),
}

impl ReflectionRequestWork {
    pub fn new(request: ReflectionRequest, arguments: Vec<Value>) -> Self {
        let operation = match request {
            ReflectionRequest::Environment => {
                ReflectionRequestOperation::Environment(EnvironmentRequestWork::Start(arguments))
            }
            ReflectionRequest::DictItems => {
                ReflectionRequestOperation::Inspection(InspectionRequestWork::Start {
                    request: InspectionRequest::DictItems,
                    arguments,
                })
            }
            ReflectionRequest::MetadataInspect => {
                ReflectionRequestOperation::Inspection(InspectionRequestWork::Start {
                    request: InspectionRequest::Metadata,
                    arguments,
                })
            }
            ReflectionRequest::Log => {
                ReflectionRequestOperation::Log(LogRequestWork::Start(arguments))
            }
            ReflectionRequest::ChildTask => ReflectionRequestOperation::Child(ChildRequestWork {
                request: ChildRequest::Task,
                state: ChildRequestState::Start(arguments),
            }),
            ReflectionRequest::ChildStatus => ReflectionRequestOperation::Child(ChildRequestWork {
                request: ChildRequest::Status,
                state: ChildRequestState::Start(arguments),
            }),
            ReflectionRequest::ChildRecover => {
                ReflectionRequestOperation::Child(ChildRequestWork {
                    request: ChildRequest::Recover,
                    state: ChildRequestState::Start(arguments),
                })
            }
            ReflectionRequest::ChildInterrupt => {
                ReflectionRequestOperation::Child(ChildRequestWork {
                    request: ChildRequest::Interrupt,
                    state: ChildRequestState::Start(arguments),
                })
            }
            ReflectionRequest::ChildClear => ReflectionRequestOperation::Child(ChildRequestWork {
                request: ChildRequest::Clear,
                state: ChildRequestState::Start(arguments),
            }),
            ReflectionRequest::ChildCall => ReflectionRequestOperation::Child(ChildRequestWork {
                request: ChildRequest::Call,
                state: ChildRequestState::Start(arguments),
            }),
            ReflectionRequest::ChildCallLeave => ReflectionRequestOperation::ChildCallLeave,
            ReflectionRequest::PromiseAccept => {
                ReflectionRequestOperation::Acceptance(AcceptanceRequestWork::Start {
                    request: AcceptanceRequest::Accept,
                    arguments,
                })
            }
            ReflectionRequest::PromiseAbandon => {
                ReflectionRequestOperation::Acceptance(AcceptanceRequestWork::Start {
                    request: AcceptanceRequest::Abandon,
                    arguments,
                })
            }
            ReflectionRequest::PromiseAccepted => {
                ReflectionRequestOperation::Acceptance(AcceptanceRequestWork::Start {
                    request: AcceptanceRequest::Accepted,
                    arguments,
                })
            }
            #[cfg(test)]
            ReflectionRequest::TestAcceptNow => {
                ReflectionRequestOperation::Acceptance(AcceptanceRequestWork::Start {
                    request: AcceptanceRequest::AcceptNow,
                    arguments,
                })
            }
            #[cfg(test)]
            ReflectionRequest::TestQueueNow => {
                ReflectionRequestOperation::Child(ChildRequestWork {
                    request: ChildRequest::QueueNow,
                    state: ChildRequestState::Start(arguments),
                })
            }
        };
        Self { operation }
    }
}

impl<S> SpecializationRequestWork<S> for ReflectionRequestWork
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    fn poll(
        &mut self,
        _specialization: &S,
        input: Option<SpecializationRequestInput>,
        context: &mut RequestContext<'_, S>,
    ) -> Result<SpecializationRequestPoll, TaskHalt> {
        let operation =
            std::mem::replace(&mut self.operation, ReflectionRequestOperation::Poisoned);
        match operation {
            ReflectionRequestOperation::Child(ChildRequestWork {
                request,
                state: ChildRequestState::Start(arguments),
            }) => {
                assert!(
                    input.is_none(),
                    "new `.child.*` work cannot have demand input"
                );
                let name = child_request_name(request);
                let mut arguments = arguments.into_iter();
                let key = arguments.next().ok_or_else(|| {
                    TaskHalt::new(format!("`{name}` received the wrong number of arguments"))
                })?;
                let value = arguments.next();
                if arguments.next().is_some() || value.is_some() != request.takes_value() {
                    return Err(TaskHalt::new(format!(
                        "`{name}` received the wrong number of arguments"
                    )));
                }
                self.operation = ReflectionRequestOperation::Child(ChildRequestWork {
                    request,
                    state: ChildRequestState::Key {
                        key: KeyConversionRequestWork::new(key),
                        value,
                    },
                });
                Ok(SpecializationRequestPoll::Continue)
            }
            ReflectionRequestOperation::Child(ChildRequestWork {
                request,
                state:
                    ChildRequestState::Key {
                        key: mut conversion,
                        value,
                    },
            }) => match conversion.poll(input, context)? {
                PreparationPoll::Progress => {
                    self.operation = ReflectionRequestOperation::Child(ChildRequestWork {
                        request,
                        state: ChildRequestState::Key {
                            key: conversion,
                            value,
                        },
                    });
                    Ok(SpecializationRequestPoll::Continue)
                }
                PreparationPoll::Demand(demand) => {
                    self.operation = ReflectionRequestOperation::Child(ChildRequestWork {
                        request,
                        state: ChildRequestState::Key {
                            key: conversion,
                            value,
                        },
                    });
                    Ok(SpecializationRequestPoll::Demand(demand))
                }
                PreparationPoll::Ready(key) => {
                    let result = match (request, value) {
                        (ChildRequest::Task, Some(operation)) => {
                            queue_child_task(key, operation, context)?
                        }
                        (ChildRequest::Interrupt, Some(reason)) => {
                            interrupt_child(key, reason, context)?
                        }
                        (ChildRequest::Call, Some(operation)) => {
                            call_child(key, operation, context)?
                        }
                        #[cfg(test)]
                        (ChildRequest::QueueNow, Some(operation)) => {
                            let path = child_machine(key, context)?;
                            queue_on_child(
                                ".TEST_ONLY.queue_now",
                                path,
                                operation,
                                context,
                                Queueing::AtOnce,
                                |_| Ok(()),
                            )?
                        }
                        (ChildRequest::Recover, _) => recover_child(key, context)?,
                        (ChildRequest::Clear, _) => clear_child(key, context)?,
                        _ => child_status(key, context)?,
                    };
                    Ok(SpecializationRequestPoll::Complete(result))
                }
            },
            ReflectionRequestOperation::Environment(EnvironmentRequestWork::Start(arguments)) => {
                assert!(input.is_none(), "new `.env` work cannot have demand input");
                let [path]: [Value; 1] = arguments
                    .try_into()
                    .map_err(|_| TaskHalt::new("`.env` received the wrong number of arguments"))?;
                self.operation = ReflectionRequestOperation::Environment(
                    EnvironmentRequestWork::KeyPath(KeyListRequestWork::new(path)),
                );
                Ok(SpecializationRequestPoll::Continue)
            }
            ReflectionRequestOperation::Environment(EnvironmentRequestWork::KeyPath(mut path)) => {
                match path.poll(input, context)? {
                    PreparationPoll::Progress => {
                        self.operation = ReflectionRequestOperation::Environment(
                            EnvironmentRequestWork::KeyPath(path),
                        );
                        Ok(SpecializationRequestPoll::Continue)
                    }
                    PreparationPoll::Demand(value) => {
                        self.operation = ReflectionRequestOperation::Environment(
                            EnvironmentRequestWork::KeyPath(path),
                        );
                        Ok(SpecializationRequestPoll::Demand(value))
                    }
                    PreparationPoll::Ready(path) => {
                        self.operation = ReflectionRequestOperation::Environment(
                            EnvironmentRequestWork::ValuePath(ValuePathRequestWork::new(
                                context.host().reflection_environment(),
                                path,
                            )),
                        );
                        Ok(SpecializationRequestPoll::Continue)
                    }
                }
            }
            ReflectionRequestOperation::Environment(EnvironmentRequestWork::ValuePath(
                mut path,
            )) => match path.poll(input, context)? {
                PreparationPoll::Progress => {
                    self.operation = ReflectionRequestOperation::Environment(
                        EnvironmentRequestWork::ValuePath(path),
                    );
                    Ok(SpecializationRequestPoll::Continue)
                }
                PreparationPoll::Demand(value) => {
                    self.operation = ReflectionRequestOperation::Environment(
                        EnvironmentRequestWork::ValuePath(path),
                    );
                    Ok(SpecializationRequestPoll::Demand(value))
                }
                PreparationPoll::Ready(value) => Ok(SpecializationRequestPoll::Complete(
                    RequestResult::Return(value),
                )),
            },
            ReflectionRequestOperation::Inspection(InspectionRequestWork::Start {
                request,
                arguments,
            }) => {
                assert!(
                    input.is_none(),
                    "new inspection work cannot have demand input"
                );
                let [value]: [Value; 1] = arguments.try_into().map_err(|_| {
                    TaskHalt::new(match request {
                        InspectionRequest::DictItems => {
                            "`.dict_items` received the wrong number of arguments"
                        }
                        InspectionRequest::Metadata => {
                            "`.meta.inspect` received the wrong number of arguments"
                        }
                    })
                })?;
                self.operation = ReflectionRequestOperation::Inspection(
                    InspectionRequestWork::Awaiting(request),
                );
                Ok(SpecializationRequestPoll::Demand(value))
            }
            ReflectionRequestOperation::Inspection(InspectionRequestWork::Awaiting(request)) => {
                let value = demand_value(input, "resumed inspection work")?;
                let result = match request {
                    InspectionRequest::DictItems => inspect_dict_items(context, &value)?,
                    InspectionRequest::Metadata => inspect_metadata(context, &value)?,
                };
                Ok(SpecializationRequestPoll::Complete(result))
            }
            ReflectionRequestOperation::Log(LogRequestWork::Start(arguments)) => {
                assert!(input.is_none(), "new `.log` work cannot have demand input");
                let [severity, message]: [Value; 2] = arguments
                    .try_into()
                    .map_err(|_| TaskHalt::new("`.log` received the wrong number of arguments"))?;
                self.operation =
                    ReflectionRequestOperation::Log(LogRequestWork::Message { severity });
                Ok(SpecializationRequestPoll::Demand(message))
            }
            ReflectionRequestOperation::Log(LogRequestWork::Message { severity }) => {
                let message = contextual_demand_value(input, "log_message", context)?;
                let interface = message.with_core_access(|value, access| {
                    let CoreValue::Dict(message) = value else {
                        return Err(TaskHalt::new("`.log` message must evaluate to an object"));
                    };
                    Ok(message.get(&*keys::MSG).map(|interface| {
                        context
                            .values()
                            .wrap_in(access, access.duplicate_value(interface))
                    }))
                })??;
                if let Some(interface) = interface {
                    self.operation =
                        ReflectionRequestOperation::Log(LogRequestWork::MessageInterface {
                            severity,
                            message,
                        });
                    Ok(SpecializationRequestPoll::Demand(interface))
                } else {
                    let message = message.into_value();
                    self.operation =
                        ReflectionRequestOperation::Log(LogRequestWork::Severity { message });
                    Ok(SpecializationRequestPoll::Demand(severity))
                }
            }
            ReflectionRequestOperation::Log(LogRequestWork::MessageInterface {
                severity,
                message,
            }) => {
                let interface = contextual_demand_value(input, "log_message", context)?;
                let values = context.values();
                let message = message.with_core_access(|value, access| {
                    let CoreValue::Dict(message) = value else {
                        unreachable!("validated log message must remain a dictionary")
                    };
                    Ok::<_, TaskHalt>(values.wrap_in(
                        access,
                        CoreValue::Dict(message.insert(
                            (*keys::MSG).clone(),
                            values.clone_core_with(access, interface.as_value())?,
                        )),
                    ))
                })??;
                self.operation =
                    ReflectionRequestOperation::Log(LogRequestWork::Severity { message });
                Ok(SpecializationRequestPoll::Demand(severity))
            }
            ReflectionRequestOperation::Log(LogRequestWork::Severity { message }) => {
                let severity = contextual_demand_value(input, "log_severity", context)?;
                let severity = parse_evaluated_severity(&severity)?;
                emit_log(context, severity, message)?;
                Ok(SpecializationRequestPoll::Complete(
                    RequestResult::ReturnUnit,
                ))
            }
            ReflectionRequestOperation::ChildCallLeave => {
                assert!(input.is_none(), "a call's close cannot have demand input");
                Ok(SpecializationRequestPoll::Complete(leave_child_call(
                    context,
                )?))
            }
            ReflectionRequestOperation::Acceptance(AcceptanceRequestWork::Start {
                request,
                arguments,
            }) => {
                assert!(
                    input.is_none(),
                    "new acceptance work cannot have demand input"
                );
                let mut arguments = arguments.into_iter();
                let resolver = arguments.next();
                let diagnostic = arguments.next();
                let expected = request.takes_diagnostic();
                let (Some(resolver), true) = (
                    resolver,
                    diagnostic.is_some() == expected && arguments.next().is_none(),
                ) else {
                    return Err(TaskHalt::new(format!(
                        "`{}` received the wrong number of arguments",
                        request.name()
                    )));
                };
                self.operation =
                    ReflectionRequestOperation::Acceptance(AcceptanceRequestWork::Awaiting {
                        request,
                        diagnostic,
                    });
                Ok(SpecializationRequestPoll::Demand(resolver))
            }
            ReflectionRequestOperation::Acceptance(AcceptanceRequestWork::Awaiting {
                request,
                diagnostic,
            }) => {
                let resolver = demand_value(input, "resumed acceptance work")?;
                let resolver = resolver.with_core_access(|value, access| match value {
                    CoreValue::Resolver(resolver) => Ok(resolver.root_in(access)),
                    _ => Err(TaskHalt::new(format!(
                        "`{}` expects a resolver",
                        request.name()
                    ))),
                })??;
                Ok(SpecializationRequestPoll::Complete(act_on_acceptance(
                    request, resolver, diagnostic, context,
                )?))
            }
            ReflectionRequestOperation::Poisoned => {
                panic!("completed reflection request work was polled again")
            }
        }
    }
}

fn child_request_name(request: ChildRequest) -> &'static str {
    match request {
        ChildRequest::Task => ".child.task",
        ChildRequest::Status => ".child.status",
        ChildRequest::Recover => ".child.recover",
        ChildRequest::Interrupt => ".child.interrupt",
        ChildRequest::Clear => ".child.clear",
        ChildRequest::Call => ".child.call",
        #[cfg(test)]
        ChildRequest::QueueNow => ".TEST_ONLY.queue_now",
    }
}

/// The machine a task without one runs on, as its own anonymous root for
/// the children it tasks: under `refl`, keyed by its task id, by an abstract
/// path that names no turn's machine (`reflection-tasks-are-machine-tasks`).
pub(crate) fn anonymous_root(task: EvaluationTaskId) -> Vec<Key> {
    vec![
        Key::atom_from_text("refl"),
        Key::abstract_global_path(["glam", "anonymous", &task.get().to_string()]),
    ]
}

/// Whether `key` keys an anonymous root, which listings leave out.
pub(crate) fn is_anonymous_key(key: &Key) -> bool {
    matches!(key, Key::AbstractGlobalPath(parts)
        if parts.len() == 3 && parts[0] == "glam" && parts[1] == "anonymous")
}

/// The machine the current branch runs on: within a call, the call's
/// child, and otherwise its task's machine, or its anonymous root.
fn branch_machine<S>(context: &RequestContext<'_, S>) -> Result<Vec<Key>, TaskHalt>
where
    S: TaskSpecialization,
{
    let eval_context = context.eval_context();
    Ok(match (context.called_machine(), eval_context.machine()) {
        (Some(machine), _) => machine.to_vec(),
        (None, Some(machine)) => machine.path().to_vec(),
        (None, None) => anonymous_root(
            eval_context
                .task_id()
                .map_err(|error| TaskHalt::new(error.as_ref()))?,
        ),
    })
}

/// The child `key` of the machine the current branch runs on.
fn child_machine<S>(key: Key, context: &RequestContext<'_, S>) -> Result<Vec<Key>, TaskHalt>
where
    S: TaskSpecialization,
{
    let mut path = branch_machine(context)?;
    path.push(key);
    Ok(path)
}

/// Accepts, abandons or reads the acceptance of `resolver`'s promise for
/// the machine the current branch runs on
/// (`obligations-are-accepted-promises`). An acceptance or an abandonment
/// is an edit, staged within a cut and committed at once outside one; a
/// read answers unit where any machine accepted the promise and fails
/// otherwise, observed, so a cut can choose with it.
fn act_on_acceptance<S>(
    request: AcceptanceRequest,
    resolver: crate::core::ManagedResolverRoot,
    diagnostic: Option<Value>,
    context: &mut RequestContext<'_, S>,
) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let machine = branch_machine(context)?;
    match request {
        AcceptanceRequest::Accept => {
            let diagnostic = diagnostic
                .expect("an acceptance carries its diagnostic")
                .into_runtime_root();
            transact_on_machines(request.name(), context, |_, events, _| {
                events.accept_promise(machine.clone(), resolver.clone(), diagnostic.clone());
                Ok(())
            })
        }
        AcceptanceRequest::Abandon => {
            transact_on_machines(request.name(), context, |_, events, _| {
                events.abandon_promise(machine.clone(), resolver.id());
                Ok(())
            })
        }
        #[cfg(test)]
        AcceptanceRequest::AcceptNow => {
            let diagnostic = diagnostic
                .expect("an acceptance carries its diagnostic")
                .into_runtime_root();
            let snapshot = context.host().snapshot();
            let store = StoreJournal::new(snapshot.store().clone());
            let mut journal = S::begin_journal(snapshot.extra());
            journal
                .reflection_journal()
                .events
                .as_mut()
                .ok_or_else(|| TaskHalt::new("`.TEST_ONLY.accept_now` needs machines"))?
                .accept_promise(machine, resolver, diagnostic);
            match context
                .host()
                .commit(TaskCommit::new(store, snapshot.extra().clone(), journal))
            {
                CommitResult::Committed => Ok(RequestResult::ReturnUnit),
                _ => Err(TaskHalt::new("`.TEST_ONLY.accept_now` did not commit")),
            }
        }
        AcceptanceRequest::Accepted => {
            let no_machines = || TaskHalt::new("`.promise.accepted` needs a host with machines");
            let generation = context.transaction_generation();
            let accepted = if let Some(mut transaction) = context.transaction() {
                let events = transaction
                    .parts()
                    .1
                    .reflection_journal()
                    .events
                    .as_mut()
                    .ok_or_else(no_machines)?;
                events.promise_accepted(resolver.id())
            } else {
                let snapshot = context.host().snapshot();
                let mut journal = S::begin_journal(snapshot.extra());
                let events = journal
                    .reflection_journal()
                    .events
                    .as_mut()
                    .ok_or_else(no_machines)?;
                let accepted = events.promise_accepted(resolver.id());
                context.observe_host_generation(snapshot.generation());
                accepted
            };
            if let Some(generation) = generation {
                context.observe_host_generation(generation);
            }
            Ok(if accepted {
                RequestResult::ReturnUnit
            } else {
                RequestResult::Fail
            })
        }
    }
}

/// `.child.task Key Op`: queues `Op` on the child `Key` and answers the
/// promise that its result fulfills or its failure breaks. The task reaches
/// the child when the queueing commits: within a cut, with the outermost
/// cut, and at once outside one.
fn queue_child_task<S>(
    key: Key,
    operation: Value,
    context: &mut RequestContext<'_, S>,
) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let path = child_machine(key, context)?;
    queue_on_child(
        ".child.task",
        path,
        operation,
        context,
        Queueing::WithTransaction,
        |_| Ok(()),
    )
}

/// How a queueing commits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Queueing {
    /// With the transaction it is made in, or at once outside one.
    WithTransaction,
    /// At once even within a transaction, as another writer's would.
    #[cfg(test)]
    AtOnce,
}

/// Queues `operation` on the child machine at `path`, for the request
/// `name`, and answers the promise of its reply: staged with the
/// transaction it is made in, as `queueing` allows, and otherwise committed
/// at once, once `admit` accepts the child as the commit's journal reads
/// it, so that what it read changing meanwhile runs the queueing again.
fn queue_on_child<S>(
    name: &str,
    path: Vec<Key>,
    operation: Value,
    context: &mut RequestContext<'_, S>,
    queueing: Queueing,
    admit: impl Fn(&mut crate::api::RuntimeEventJournal) -> Result<(), TaskHalt>,
) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let no_machines = || TaskHalt::new(format!("`{name}` needs a host with machines"));
    let Machines(host) = context.host().machines().ok_or_else(no_machines)?;
    if context.eval_context().machine().is_none() {
        context.note_anonymous_children(host.clone());
    }
    let machine: Arc<[Key]> = Arc::from(path.clone());
    let values = context.values();
    let promises = match context.transaction() {
        Some(mut transaction) => transaction.store().snapshot().promises().clone(),
        None => context.host().snapshot().store().promises().clone(),
    };
    let (promise, promised, resolver) = values.with_access(|scoped| {
        let (promise, promised, resolver) = promises.allocate(scoped.runtime_access());
        (promise, scoped.wrap(promised), scoped.wrap(resolver))
    });
    let task = match values.record([("op", operation), ("reply", resolver)]) {
        Ok(task) => task,
        Err(error) => {
            promises.forget(promise);
            return Err(TaskHalt::new(error.to_string()));
        }
    };
    let wake = ReflectionUpdate::Wake {
        host,
        machine,
        context: context.eval_context().clone(),
    };
    let queue = |events: Option<&mut crate::api::RuntimeEventJournal>,
                 updates: &mut Vec<ReflectionUpdate>| {
        let events = events.ok_or_else(no_machines)?;
        admit(events)?;
        events.queue_task(path.clone(), task.clone());
        updates.push(wake.clone());
        Ok::<_, TaskHalt>(())
    };
    if queueing == Queueing::WithTransaction
        && let Some(mut transaction) = context.transaction()
    {
        let journal = transaction.parts().1.reflection_journal();
        if let Err(error) = queue(journal.events.as_mut(), &mut journal.updates) {
            promises.forget(promise);
            return Err(error);
        }
        return Ok(RequestResult::Return(promised));
    }
    loop {
        let snapshot = context.host().snapshot();
        let store = StoreJournal::new(snapshot.store().clone());
        let mut journal = S::begin_journal(snapshot.extra());
        let reflection = journal.reflection_journal();
        if let Err(error) = queue(reflection.events.as_mut(), &mut reflection.updates) {
            promises.forget(promise);
            return Err(error);
        }
        match context
            .host()
            .commit(TaskCommit::new(store, snapshot.extra().clone(), journal))
        {
            CommitResult::Committed => {
                if context.transaction_generation().is_none() {
                    context.committed();
                }
                return Ok(RequestResult::Return(promised));
            }
            CommitResult::Conflict => {}
            CommitResult::MissingVolume(volume) => {
                return Err(TaskHalt::new(format!(
                    "reflection volume {} is unavailable",
                    volume.get()
                )));
            }
            CommitResult::Closed => return Ok(RequestResult::Cancelled),
        }
    }
}

/// `.child.status Key`: `'error` while the child is flagged, `'busy` while
/// it holds a task, queued or running, and `'idle` otherwise. Within a cut
/// the read is observed, so a cut that fails on it runs again when the
/// status changes.
fn child_status<S>(key: Key, context: &mut RequestContext<'_, S>) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    fn read(events: &mut crate::api::RuntimeEventJournal, machine: &[Key]) -> &'static str {
        if events.machine_flagged(machine) {
            "error"
        } else if events.queued_tasks(machine) > 0 {
            "busy"
        } else {
            "idle"
        }
    }
    let machine = child_machine(key, context)?;
    let no_machines = || TaskHalt::new("`.child.status` needs a host with machines");
    let generation = context.transaction_generation();
    let status = if let Some(mut transaction) = context.transaction() {
        let events = transaction
            .parts()
            .1
            .reflection_journal()
            .events
            .as_mut()
            .ok_or_else(no_machines)?;
        read(events, &machine)
    } else {
        let snapshot = context.host().snapshot();
        let mut journal = S::begin_journal(snapshot.extra());
        let events = journal
            .reflection_journal()
            .events
            .as_mut()
            .ok_or_else(no_machines)?;
        read(events, &machine)
    };
    if let Some(generation) = generation {
        context.observe_host_generation(generation);
    }
    Ok(RequestResult::Return(
        context.values().atom_from_text(status),
    ))
}

/// The child an edit acts on is busy, so the edit waits, as `.child.call`
/// does: for the child to finish what was committed to it.
struct ChildBusy;

/// Runs `edit` on the store and machine journals of the current
/// transaction, within a cut, or else on journals of its own, which it
/// commits at once, again on a conflict. Answers unit. Where the child is
/// busy, the edit fails having observed it, so it runs again once the
/// child's state changes: a cut waits, unless an alternative succeeds, and
/// outside a cut the task waits.
fn transact_on_machines<S>(
    name: &str,
    context: &mut RequestContext<'_, S>,
    mut edit: impl FnMut(
        &mut StoreJournal,
        &mut crate::api::RuntimeEventJournal,
        &mut Vec<ReflectionUpdate>,
    ) -> Result<(), ChildBusy>,
) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let no_machines = || TaskHalt::new(format!("`{name}` needs a host with machines"));
    if let Some(generation) = context.transaction_generation() {
        let mut transaction = context
            .transaction()
            .expect("a transaction has a generation");
        let (store, journal) = transaction.journals();
        let journal = journal.reflection_journal();
        let events = journal.events.as_mut().ok_or_else(no_machines)?;
        if edit(store, events, &mut journal.updates).is_err() {
            context.observe_host_generation(generation);
            return Ok(RequestResult::Fail);
        }
        return Ok(RequestResult::ReturnUnit);
    }
    loop {
        let snapshot = context.host().snapshot();
        let mut store = StoreJournal::new(snapshot.store().clone());
        let mut journal = S::begin_journal(snapshot.extra());
        {
            let reflection = journal.reflection_journal();
            let events = reflection.events.as_mut().ok_or_else(no_machines)?;
            if edit(&mut store, events, &mut reflection.updates).is_err() {
                context.observe_host_generation(snapshot.generation());
                return Ok(RequestResult::Fail);
            }
        }
        match context
            .host()
            .commit(TaskCommit::new(store, snapshot.extra().clone(), journal))
        {
            CommitResult::Committed => {
                context.committed();
                return Ok(RequestResult::ReturnUnit);
            }
            CommitResult::Conflict => {}
            CommitResult::MissingVolume(volume) => {
                return Err(TaskHalt::new(format!(
                    "reflection volume {} is unavailable",
                    volume.get()
                )));
            }
            CommitResult::Closed => return Ok(RequestResult::Cancelled),
        }
    }
}

/// `.child.recover Key`: where the child is flagged, clears its flag and
/// its queue, breaking the replies of the tasks it drops. Its local state
/// stays as the failure left it, for the parent's next tasks to inspect and
/// repair. It waits while the child is busy, as `.child.call` does, and
/// leaves an idle child as it is.
fn recover_child<S>(
    key: Key,
    context: &mut RequestContext<'_, S>,
) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let machine = child_machine(key, context)?;
    let values = context.values();
    transact_on_machines(".child.recover", context, |store, events, _| {
        if events.machine_busy(&machine) {
            return Err(ChildBusy);
        }
        let Some(tasks) = events.recover_machine(&machine) else {
            return Ok(());
        };
        values.with_access(|scoped| {
            let access = scoped.runtime_access();
            let reason = crate::api::machine_diagnostic(
                access,
                "the task's machine was recovered from an error before the task ran",
            );
            crate::api::break_replies(access, store, tasks, &reason);
        });
        Ok(())
    })
}

/// `.child.interrupt Key Reason`: stops the tasks running on the child and
/// its descendants, breaks their replies with `Reason`, and flags each
/// machine it stopped a task on, whose state the task may have left
/// half-done. Where nothing runs, it changes nothing.
fn interrupt_child<S>(
    key: Key,
    reason: Value,
    context: &mut RequestContext<'_, S>,
) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let Machines(host) = context
        .host()
        .machines()
        .ok_or_else(|| TaskHalt::new("`.child.interrupt` needs a host with machines"))?;
    let machine = child_machine(key, context)?;
    let values = context.values();
    let diagnostic = values.with_access(|scoped| {
        crate::api::reason_diagnostic(scoped.runtime_access(), reason.runtime_root())
    });
    transact_on_machines(".child.interrupt", context, |store, events, updates| {
        let stopped = events.interrupt_tasks(&machine, &diagnostic);
        let mut tasks = Vec::with_capacity(stopped.len());
        for (machine, sequence, task) in stopped {
            tasks.push(task);
            updates.push(ReflectionUpdate::Interrupt {
                host: host.clone(),
                machine: Arc::from(machine),
                sequence,
            });
        }
        values.with_access(|scoped| {
            crate::api::break_replies(scoped.runtime_access(), store, tasks, reason.runtime_root());
        });
        Ok(())
    })
}

/// `.child.clear Key`: empties the child and its descendants, as a turn's
/// clear does its machine, breaking the replies of every task it drops. It
/// stops no work: it waits while the child or a descendant is busy, as
/// `.child.call` does, so a parent interrupts first what it means to stop.
/// A cleared child reads `'idle`.
fn clear_child<S>(key: Key, context: &mut RequestContext<'_, S>) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let Machines(host) = context
        .host()
        .machines()
        .ok_or_else(|| TaskHalt::new("`.child.clear` needs a host with machines"))?;
    let machine = child_machine(key, context)?;
    let path: Arc<[Key]> = Arc::from(machine.clone());
    let values = context.values();
    transact_on_machines(".child.clear", context, |store, events, updates| {
        if events.machines_busy(&machine) {
            return Err(ChildBusy);
        }
        let cleared = crate::api::clear_child_machines(store, events, &machine);
        values.with_access(|scoped| {
            let access = scoped.runtime_access();
            let reason = crate::api::machine_diagnostic(
                access,
                "the task's machine was cleared before the task ran",
            );
            crate::api::break_replies(access, store, cleared, &reason);
        });
        updates.push(ReflectionUpdate::Clear {
            host: host.clone(),
            machine: path.clone(),
        });
        Ok(())
    })
}

/// `.child.call Key Op`: runs `Op` on the child `Key` and answers its
/// result. Outside a transaction, a plain remote procedure call
/// (`child-call-plain`): it queues `Op` on the child, as `.child.task`
/// does, and waits for its reply, so it answers `Op`'s result after what
/// was queued before it, or fails with its failure, which flags the child
/// as any task's does. A child in error it fails at once, rather than queue
/// behind a stopped queue.
///
/// Within a transaction, it runs `Op` on the child within the caller's
/// transaction (`child-call-frame`). `Op` runs on the child's local state,
/// and its requests act on the child's children; as the call returns, the
/// state `Op` left becomes the child's, committing or aborting with the
/// caller. The call waits for the child to finish its task queue, reading
/// the child's flag, queue and state, all observed:
///
/// - a child in error it cannot enter, as recovery alone does, so it fails
///   with an error;
/// - tasks the transaction queued on the child, work forked onto it, the
///   child starts only once the transaction commits, so the wait could
///   never end, and the call fails with an error; other input it would
///   only buffer, for a task to read;
/// - a child busy with what was committed to it, as the transaction's
///   snapshot shows it, the call waits for, failing having observed it.
///
/// An explicit failure in `Op` backtracks, the caller's state and machine
/// with it. An error that ends `Op` is undone with the transaction: `.try`
/// answers it, undoing what it tried, and a cut, having observed the child,
/// waits for a change. The error flag means a machine's state is bad, and
/// an undone error leaves the child's as it was, so the call flags nothing.
fn call_child<S>(
    key: Key,
    operation: Value,
    context: &mut RequestContext<'_, S>,
) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let in_error = || {
        TaskHalt::new("`.child.call` cannot enter a child in error until its parent recovers it")
    };
    let Machines(host) = context
        .host()
        .machines()
        .ok_or_else(|| TaskHalt::new("`.child.call` needs a host with machines"))?;
    let machine = child_machine(key, context)?;
    let Some(generation) = context.transaction_generation() else {
        let check = machine.clone();
        let queued = queue_on_child(
            ".child.call",
            machine,
            operation,
            context,
            Queueing::WithTransaction,
            |events| {
                if events.machine_flagged(&check) {
                    Err(in_error())
                } else {
                    Ok(())
                }
            },
        )?;
        return Ok(match queued {
            RequestResult::Return(reply) => RequestResult::Await(reply),
            result => result,
        });
    };
    context.observe_host_generation(generation);
    let state = {
        let mut transaction = context
            .transaction()
            .expect("a transaction has a generation");
        let events = transaction
            .parts()
            .1
            .reflection_journal()
            .events
            .as_mut()
            .ok_or_else(|| TaskHalt::new("`.child.call` needs a host with machines"))?;
        if events.machine_flagged(&machine) {
            return Err(in_error());
        }
        if events.queues_tasks_on(&machine) {
            return Err(TaskHalt::new(
                "`.child.call` would wait for good: the child starts the tasks \
                 this transaction queued on it only once the transaction commits",
            ));
        }
        if events.queued_tasks(&machine) > 0 {
            return Ok(RequestResult::Fail);
        }
        events.machine_state(&machine)
    };
    if context.eval_context().machine().is_none() {
        context.note_anonymous_children(host);
    }
    let values = context.values();
    let state = state.unwrap_or_else(|| values.empty_dict());
    let close = child_call_leave_spec().effect(&values, [])?;
    context.enter_call(Arc::from(machine), state.into_runtime_root());
    Ok(RequestResult::Scoped { operation, close })
}

/// The close of a `.child.call`: the state its operation left becomes the
/// child's, in the caller's transaction, and the caller takes back its own.
fn leave_child_call<S>(context: &mut RequestContext<'_, S>) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let (machine, state) = context
        .leave_call()
        .ok_or_else(|| TaskHalt::new("a call's close found no call to leave"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskHalt::new("a call returns within its caller's transaction"))?;
    let events = transaction
        .parts()
        .1
        .reflection_journal()
        .events
        .as_mut()
        .ok_or_else(|| TaskHalt::new("`.child.call` needs a host with machines"))?;
    events.set_machine_state(&machine, Value::from_runtime_root(state));
    Ok(RequestResult::ReturnUnit)
}

/// The hidden request that closes a `.child.call`.
fn child_call_leave_spec() -> EffectRequestSpec<ReflectionRequest> {
    EffectRequestSpec::hidden(
        [
            "reflection_runtime",
            "v0",
            "request",
            "child",
            "call",
            "leave",
        ],
        0,
        ReflectionRequest::ChildCallLeave,
    )
}

/// Starts each task the child machine at `machine` runs next, from `task`
/// on: a task that cannot launch fails, as a task does, and the next goes
/// on.
pub(super) fn launch_machine_tasks(
    context: &EvalContext,
    host: &Arc<dyn MachineHost>,
    machine: &Arc<[Key]>,
    mut task: Option<QueuedTask>,
) {
    while let Some(QueuedTask { sequence, record }) = task.take() {
        let operation = context.values().construct_runtime_value_root(|access| {
            match record.clone_core_with(access) {
                CoreValue::Dict(record) => record
                    .get(&Key::atom_from_text("op"))
                    .map(|operation| access.duplicate_value(operation))
                    .unwrap_or_else(|| CoreValue::Dict(Dict::new_sync())),
                _ => CoreValue::Dict(Dict::new_sync()),
            }
        });
        match context.reserve_reflection_task(operation) {
            Ok(pending) => {
                let handle = pending.handle().clone();
                let halt = {
                    let (context, host, machine) = (context.clone(), host.clone(), machine.clone());
                    Box::new(move |how| {
                        let outcome = match how {
                            RunHalt::Stopped => {
                                MachineTaskOutcome::Failed(RuntimeFailureRoot::new(
                                    context.values(),
                                    Arc::new(crate::core::EvaluationFailure::message(
                                        "the task halted before it finished",
                                    )),
                                ))
                            }
                            RunHalt::Exited => MachineTaskOutcome::Exited,
                        };
                        let next = host.finish(&machine, sequence, outcome, None);
                        launch_machine_tasks(&context, &host, &machine, next);
                    })
                };
                pending.launch_on(Arc::new(TaskMachine::new(
                    machine.clone(),
                    host.clone(),
                    sequence,
                    halt,
                )));
                host.attach(machine, sequence, &handle);
            }
            Err(reason) => {
                let failure = RuntimeFailureRoot::new(
                    context.values(),
                    Arc::new(crate::core::EvaluationFailure::message(reason.as_ref())),
                );
                task = host.finish(machine, sequence, MachineTaskOutcome::Failed(failure), None);
            }
        }
    }
}

impl ValuePathRequestWork {
    pub(crate) fn new(current: Value, path: Vec<Key>) -> Self {
        Self {
            path,
            next: 0,
            current,
            awaiting: false,
        }
    }

    pub(crate) fn poll<S: TaskSpecialization>(
        &mut self,
        input: Option<SpecializationRequestInput>,
        context: &RequestContext<'_, S>,
    ) -> Result<PreparationPoll<Value>, TaskHalt> {
        if self.next == self.path.len() {
            assert!(
                input.is_none(),
                "completed value path cannot have demand input"
            );
            return Ok(PreparationPoll::Ready(self.current.clone()));
        }
        if !self.awaiting {
            assert!(input.is_none(), "new value-path demand cannot have input");
            self.awaiting = true;
            return Ok(PreparationPoll::Demand(self.current.clone()));
        }
        let current = demand_value(input, "resumed value-path work")?;
        let key = &self.path[self.next];
        let walked = &self.path[..=self.next];
        let selected = current.with_core_access(|value, access| {
            let CoreValue::Dict(dict) = value else {
                return Err(TaskHalt::new("state path traverses a non-dictionary value"));
            };
            // Reading what is undefined is an error, at the read
            // (`dict-empty-is-a-value`).
            let value = dict.get(key).ok_or_else(|| {
                TaskHalt::failure(Arc::new(crate::core::EvaluationFailure::undefined(walked)))
            })?;
            Ok(context
                .values()
                .wrap_in(access, access.duplicate_value(value)))
        })??;
        self.current = selected;
        self.next += 1;
        self.awaiting = false;
        Ok(PreparationPoll::Progress)
    }
}

impl KeyConversionRequestWork {
    fn new(value: Value) -> Self {
        Self {
            state: KeyConversionRequestState::Demand(value),
        }
    }

    fn poll<S: TaskSpecialization>(
        &mut self,
        input: Option<SpecializationRequestInput>,
        context: &RequestContext<'_, S>,
    ) -> Result<PreparationPoll<Key>, TaskHalt> {
        let state = std::mem::replace(&mut self.state, KeyConversionRequestState::Poisoned);
        match state {
            KeyConversionRequestState::Demand(value) => {
                assert!(input.is_none(), "new key demand cannot have input");
                self.state = KeyConversionRequestState::Awaiting;
                Ok(PreparationPoll::Demand(value))
            }
            KeyConversionRequestState::Awaiting => {
                let value = demand_value(input, "resumed key conversion")?;
                match classify_key_value(&value)? {
                    ClassifiedRequestKey::Ready(key) => Ok(PreparationPoll::Ready(key)),
                    ClassifiedRequestKey::List => {
                        self.state = KeyConversionRequestState::List(Box::new(
                            KeyListRequestWork::from_ready(value),
                        ));
                        Ok(PreparationPoll::Progress)
                    }
                    ClassifiedRequestKey::Dict(members) => {
                        self.state =
                            KeyConversionRequestState::Dict(Box::new(DictKeyRequestWork {
                                members,
                                next: 0,
                                converted: Vec::new(),
                                child: None,
                            }));
                        Ok(PreparationPoll::Progress)
                    }
                    ClassifiedRequestKey::Invalid => Err(TaskHalt::new(
                        "dictionary keys must evaluate to keyable values",
                    )),
                }
            }
            KeyConversionRequestState::List(mut list) => match list.poll(input, context)? {
                PreparationPoll::Progress => {
                    self.state = KeyConversionRequestState::List(list);
                    Ok(PreparationPoll::Progress)
                }
                PreparationPoll::Demand(value) => {
                    self.state = KeyConversionRequestState::List(list);
                    Ok(PreparationPoll::Demand(value))
                }
                PreparationPoll::Ready(items) => {
                    Ok(PreparationPoll::Ready(Key::List(Arc::from(items))))
                }
            },
            KeyConversionRequestState::Dict(mut dict) => match dict.poll(input, context)? {
                PreparationPoll::Progress => {
                    self.state = KeyConversionRequestState::Dict(dict);
                    Ok(PreparationPoll::Progress)
                }
                PreparationPoll::Demand(value) => {
                    self.state = KeyConversionRequestState::Dict(dict);
                    Ok(PreparationPoll::Demand(value))
                }
                PreparationPoll::Ready(items) => {
                    Ok(PreparationPoll::Ready(Key::Dict(Arc::from(items))))
                }
            },
            KeyConversionRequestState::Poisoned => {
                panic!("completed key conversion was polled again")
            }
        }
    }
}

enum ClassifiedRequestKey {
    Ready(Key),
    List,
    Dict(Vec<(Key, Value)>),
    Invalid,
}

fn classify_key_value(
    value: &crate::api::EvaluatedValue,
) -> Result<ClassifiedRequestKey, TaskHalt> {
    value
        .with_core_access(|value, access| match value {
            CoreValue::Atom(atom) => ClassifiedRequestKey::Ready(Key::Atom(*atom)),
            CoreValue::Number(number) => ClassifiedRequestKey::Ready(Key::Number(number.clone())),
            CoreValue::Binary(bytes) => ClassifiedRequestKey::Ready(Key::Binary(bytes.clone())),
            CoreValue::List(_) => ClassifiedRequestKey::List,
            CoreValue::Dict(dict) => ClassifiedRequestKey::Dict(
                dict.iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            Value::from_runtime_root(
                                access.root_runtime_value(access.duplicate_value(value)),
                            ),
                        )
                    })
                    .collect(),
            ),
            CoreValue::Builtin(_)
            | CoreValue::PartialBuiltin(_)
            | CoreValue::Function(_)
            | CoreValue::Net(_)
            | CoreValue::Lazy(_)
            | CoreValue::Promised(_)
            | CoreValue::Metadata(_)
            | CoreValue::Opaque(_)
            | CoreValue::Volume(_)
            | CoreValue::Resolver(_) => ClassifiedRequestKey::Invalid,
        })
        .map_err(TaskHalt::from)
}

impl DictKeyRequestWork {
    fn poll<S: TaskSpecialization>(
        &mut self,
        input: Option<SpecializationRequestInput>,
        context: &RequestContext<'_, S>,
    ) -> Result<PreparationPoll<Vec<(Key, Key)>>, TaskHalt> {
        if let Some(child) = self.child.as_mut() {
            return match child.poll(input, context)? {
                PreparationPoll::Progress => Ok(PreparationPoll::Progress),
                PreparationPoll::Demand(value) => Ok(PreparationPoll::Demand(value)),
                PreparationPoll::Ready(value) => {
                    let (key, _) = &self.members[self.next - 1];
                    self.converted.push((key.clone(), value));
                    self.child = None;
                    Ok(PreparationPoll::Progress)
                }
            };
        }
        assert!(
            input.is_none(),
            "idle dictionary key work cannot have input"
        );
        let Some((_, value)) = self.members.get(self.next) else {
            return Ok(PreparationPoll::Ready(std::mem::take(&mut self.converted)));
        };
        self.next += 1;
        self.child = Some(KeyConversionRequestWork::new(value.clone()));
        Ok(PreparationPoll::Progress)
    }
}

impl KeyListRequestWork {
    pub(crate) fn new(value: Value) -> Self {
        Self {
            pending: Some(ListDemand::Start(value)),
            lists: Vec::new(),
            child: None,
            converted: Vec::new(),
        }
    }

    fn from_ready(value: crate::api::EvaluatedValue) -> Self {
        Self {
            pending: None,
            lists: vec![value],
            child: None,
            converted: Vec::new(),
        }
    }

    pub(crate) fn poll<S: TaskSpecialization>(
        &mut self,
        input: Option<SpecializationRequestInput>,
        context: &RequestContext<'_, S>,
    ) -> Result<PreparationPoll<Vec<Key>>, TaskHalt> {
        if let Some(child) = self.child.as_mut() {
            return match child.poll(input, context)? {
                PreparationPoll::Progress => Ok(PreparationPoll::Progress),
                PreparationPoll::Demand(value) => Ok(PreparationPoll::Demand(value)),
                PreparationPoll::Ready(key) => {
                    self.converted.push(key);
                    self.child = None;
                    Ok(PreparationPoll::Progress)
                }
            };
        }
        if let Some(pending) = self.pending.take() {
            return match pending {
                ListDemand::Start(value) => {
                    assert!(input.is_none(), "new list demand cannot have input");
                    self.pending = Some(ListDemand::Awaiting);
                    Ok(PreparationPoll::Demand(value))
                }
                ListDemand::Awaiting => {
                    let value = demand_value(input, "resumed key-list demand")?;
                    let is_list =
                        value.with_core_access(|value, _| matches!(value, CoreValue::List(_)))?;
                    if !is_list {
                        return Err(TaskHalt::new(
                            "path-list operand must evaluate to a list value",
                        ));
                    }
                    self.lists.push(value);
                    Ok(PreparationPoll::Progress)
                }
            };
        }
        assert!(input.is_none(), "idle key-list work cannot have input");
        let Some(list) = self.lists.pop() else {
            return Ok(PreparationPoll::Ready(std::mem::take(&mut self.converted)));
        };
        let step = list.with_core_access(|value, access| {
            let CoreValue::List(list) = value else {
                unreachable!("key-list work retains only validated lists")
            };
            let step = list
                .pop_front_step_by(&mut |value| access.duplicate_value(value), &mut |thunk| {
                    thunk.duplicate_as_value_in(access)
                });
            match step {
                ListFrontStep::Empty => KeyListStep::Empty,
                ListFrontStep::Item {
                    item: ListItem::Byte(byte),
                    tail,
                } => KeyListStep::Byte {
                    byte,
                    tail: context.values().wrap_in(access, CoreValue::List(tail)),
                },
                ListFrontStep::Item {
                    item: ListItem::Value(value),
                    tail,
                } => KeyListStep::Value {
                    value: context.values().wrap_in(access, value),
                    tail: context.values().wrap_in(access, CoreValue::List(tail)),
                },
                ListFrontStep::Deferred { deferred, suffix } => KeyListStep::Deferred {
                    deferred: context.values().wrap_in(access, deferred),
                    suffix: context.values().wrap_in(access, CoreValue::List(suffix)),
                },
            }
        })?;
        match step {
            KeyListStep::Empty => Ok(PreparationPoll::Progress),
            KeyListStep::Byte { byte, tail } => {
                self.lists.push(crate::api::EvaluatedValue::from_whnf(
                    &context.values(),
                    tail,
                ));
                self.converted.push(Key::Number(Number::from_u8(byte)));
                Ok(PreparationPoll::Progress)
            }
            KeyListStep::Value { value, tail } => {
                self.lists.push(crate::api::EvaluatedValue::from_whnf(
                    &context.values(),
                    tail,
                ));
                self.child = Some(Box::new(KeyConversionRequestWork::new(value)));
                Ok(PreparationPoll::Progress)
            }
            KeyListStep::Deferred { deferred, suffix } => {
                self.lists.push(crate::api::EvaluatedValue::from_whnf(
                    &context.values(),
                    suffix,
                ));
                self.pending = Some(ListDemand::Start(deferred));
                Ok(PreparationPoll::Progress)
            }
        }
    }
}

fn demand_value(
    input: Option<SpecializationRequestInput>,
    resumed: &str,
) -> Result<crate::api::EvaluatedValue, TaskHalt> {
    match input.expect(resumed) {
        SpecializationRequestInput::Value(value) => Ok(value),
        SpecializationRequestInput::Failed(error) => Err(error),
    }
}

fn contextual_demand_value<S: TaskSpecialization>(
    input: Option<SpecializationRequestInput>,
    operation: &str,
    context: &RequestContext<'_, S>,
) -> Result<crate::api::EvaluatedValue, TaskHalt> {
    demand_value(input, "resumed log preparation").map_err(|error| {
        context
            .eval_context()
            .values()
            .with_boundary_access(|access| {
                error.with_core_context_in(
                    &access,
                    crate::diagnostic::evaluation_context_frame_in(&access, operation),
                )
            })
    })
}

fn emit_log<S>(
    context: &mut RequestContext<'_, S>,
    severity: Severity,
    message: Value,
) -> Result<(), TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let diagnostic = Diagnostic::from_emission(&context.values(), severity, message)
        .map_err(|error| TaskHalt::new(error.to_string()))?;
    if let Some(mut transaction) = context.transaction() {
        transaction
            .parts()
            .1
            .reflection_journal()
            .diagnostics
            .push(diagnostic);
    } else {
        context.host().emit_diagnostic(diagnostic);
        context.committed();
    }
    Ok(())
}

pub(crate) fn parse_evaluated_severity(
    value: &crate::api::EvaluatedValue,
) -> Result<Severity, TaskHalt> {
    let (info, warn, error) = value.with_core_access(|value, access| {
        (
            severity_matches(access, value, "info", &keys::INFO),
            severity_matches(access, value, "warn", &keys::WARN),
            severity_matches(access, value, "error", &keys::ERROR),
        )
    })?;
    if info {
        Ok(Severity::Info)
    } else if warn {
        Ok(Severity::Warning)
    } else if error {
        Ok(Severity::Error)
    } else {
        Err(TaskHalt::new(
            "`.log` severity must be `'info`, `'warn`, or `'error`",
        ))
    }
}

fn inspect_dict_items<S: TaskSpecialization>(
    context: &RequestContext<'_, S>,
    dict: &crate::api::EvaluatedValue,
) -> Result<RequestResult, TaskHalt> {
    let items = dict.with_core_access(|value, access| {
        let CoreValue::Dict(dict) = value else {
            return Err(TaskHalt::new("`.dict_items` requires a dictionary"));
        };
        Ok(context.values().wrap_in(
            access,
            CoreValue::List(crate::core::List::from_values(
                dict.iter()
                    .map(|(key, value)| {
                        CoreValue::Dict(
                            Dict::new_sync()
                                .insert((*keys::KEY).clone(), key.to_value_in(access))
                                .insert((*keys::VALUE).clone(), access.duplicate_value(value)),
                        )
                    })
                    .collect(),
            )),
        ))
    })??;
    Ok(RequestResult::Return(items))
}

fn inspect_metadata<S: TaskSpecialization>(
    context: &RequestContext<'_, S>,
    value: &crate::api::EvaluatedValue,
) -> Result<RequestResult, TaskHalt> {
    let Some(metadata) = value.with_core_access(|value, access| {
        CoreValue::associated_metadata(value, access)
            .map(|metadata| context.values().wrap_in(access, metadata))
    })?
    else {
        return Ok(RequestResult::Fail);
    };
    Ok(RequestResult::Return(metadata))
}

#[derive(Clone)]
enum ReflectionUpdate {
    /// Starts the child machine's next task, where it is idle, once the
    /// task queued on it commits.
    Wake {
        host: Arc<dyn MachineHost>,
        machine: Arc<[Key]>,
        context: EvalContext,
    },
    /// Stops the child machine's run, once its interrupt commits.
    Interrupt {
        host: Arc<dyn MachineHost>,
        machine: Arc<[Key]>,
        sequence: u64,
    },
    /// Empties the child machine and its descendants, once their clear
    /// commits.
    Clear {
        host: Arc<dyn MachineHost>,
        machine: Arc<[Key]>,
    },
}

/// Transactional writes and deferred observations for reflection requests.
#[derive(Clone, Default)]
pub struct ReflectionJournal {
    diagnostics: Vec<Diagnostic>,
    updates: Vec<ReflectionUpdate>,
    /// The machines' port actions, which commit with the store journal,
    /// where the host has machines (`api-refl-child-tasks`).
    events: Option<crate::api::RuntimeEventJournal>,
}

impl ReflectionJournal {
    pub(super) fn begin(snapshot: Option<&crate::api::RuntimeEventSnapshot>) -> Self {
        Self {
            events: snapshot.cloned().map(crate::api::RuntimeEventJournal::new),
            ..Self::default()
        }
    }

    /// The machines' port actions, where the host has machines.
    #[doc(hidden)]
    pub fn events(&self) -> Option<&crate::api::RuntimeEventJournal> {
        self.events.as_ref()
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[doc(hidden)]
    pub fn commit_updates(&self) {
        for update in &self.updates {
            match update {
                ReflectionUpdate::Wake {
                    host,
                    machine,
                    context,
                } => {
                    let task = host.start(machine);
                    launch_machine_tasks(context, host, machine, task);
                }
                ReflectionUpdate::Interrupt {
                    host,
                    machine,
                    sequence,
                } => host.interrupt(machine, *sequence),
                ReflectionUpdate::Clear { host, machine } => host.clear(machine),
            }
        }
    }
}

/// Gives a composed task journal access to its reflection portion.
pub trait ReflectionTransaction {
    fn reflection_journal(&mut self) -> &mut ReflectionJournal;
}

impl ReflectionTransaction for ReflectionJournal {
    fn reflection_journal(&mut self) -> &mut ReflectionJournal {
        self
    }
}

/// Specialization-independent services used by reusable reflection requests.
pub trait ReflectionServices: Send + Sync {
    fn emit_diagnostic(&self, diagnostic: Diagnostic);

    /// The runtime's child machines, which `.child.*` reaches: a host with
    /// machines has them, and a test host need not
    /// (`reflection-tasks-are-machine-tasks`).
    #[doc(hidden)]
    fn machines(&self) -> Option<Machines> {
        None
    }
}

/// A host's child machines, which only the runtime makes.
#[doc(hidden)]
#[derive(Clone)]
pub struct Machines(pub(crate) Arc<dyn MachineHost>);

/// A task host that combines specialization transactions with reflection
/// services. The blanket implementation avoids repeating those services for
/// every specialization hosted by the same concrete type.
pub trait ReflectionHost<S: TaskSpecialization>: TaskHost<S> + ReflectionServices {}

impl<S, H> ReflectionHost<S> for H
where
    S: TaskSpecialization,
    H: TaskHost<S> + ReflectionServices + ?Sized,
{
}

/// API constructors contributed by the reusable reflection request family.
pub fn reflection_request_specs() -> Vec<EffectRequestSpec<ReflectionRequest>> {
    environment_diagnostic_request_specs()
        .into_iter()
        .chain([
            EffectRequestSpec::new(
                "dict_items",
                ["reflection_runtime", "v0", "request", "dict_items"],
                1,
                ReflectionRequest::DictItems,
            ),
            EffectRequestSpec::at_path(
                ["meta", "inspect"],
                ["reflection_runtime", "v0", "request", "meta", "inspect"],
                1,
                ReflectionRequest::MetadataInspect,
            ),
            EffectRequestSpec::at_path(
                ["child", "task"],
                ["reflection_runtime", "v0", "request", "child", "task"],
                2,
                ReflectionRequest::ChildTask,
            ),
            EffectRequestSpec::at_path(
                ["child", "status"],
                ["reflection_runtime", "v0", "request", "child", "status"],
                1,
                ReflectionRequest::ChildStatus,
            ),
            EffectRequestSpec::at_path(
                ["child", "recover"],
                ["reflection_runtime", "v0", "request", "child", "recover"],
                1,
                ReflectionRequest::ChildRecover,
            ),
            EffectRequestSpec::at_path(
                ["child", "interrupt"],
                ["reflection_runtime", "v0", "request", "child", "interrupt"],
                2,
                ReflectionRequest::ChildInterrupt,
            ),
            EffectRequestSpec::at_path(
                ["child", "clear"],
                ["reflection_runtime", "v0", "request", "child", "clear"],
                1,
                ReflectionRequest::ChildClear,
            ),
            EffectRequestSpec::at_path(
                ["child", "call"],
                ["reflection_runtime", "v0", "request", "child", "call"],
                2,
                ReflectionRequest::ChildCall,
            ),
            child_call_leave_spec(),
            EffectRequestSpec::at_path(
                ["promise", "accept"],
                ["reflection_runtime", "v0", "request", "promise", "accept"],
                2,
                ReflectionRequest::PromiseAccept,
            ),
            EffectRequestSpec::at_path(
                ["promise", "abandon"],
                ["reflection_runtime", "v0", "request", "promise", "abandon"],
                1,
                ReflectionRequest::PromiseAbandon,
            ),
            EffectRequestSpec::at_path(
                ["promise", "accepted"],
                ["reflection_runtime", "v0", "request", "promise", "accepted"],
                1,
                ReflectionRequest::PromiseAccepted,
            ),
            #[cfg(test)]
            EffectRequestSpec::at_path(
                ["TEST_ONLY", "queue_now"],
                [
                    "reflection_runtime",
                    "v0",
                    "request",
                    "TEST_ONLY",
                    "queue_now",
                ],
                2,
                ReflectionRequest::TestQueueNow,
            ),
            #[cfg(test)]
            EffectRequestSpec::at_path(
                ["TEST_ONLY", "accept_now"],
                [
                    "reflection_runtime",
                    "v0",
                    "request",
                    "TEST_ONLY",
                    "accept_now",
                ],
                2,
                ReflectionRequest::TestAcceptNow,
            ),
        ])
        .collect()
}

/// Reusable `.env` and `.log` request subset for isolated host interpreters.
pub fn environment_diagnostic_request_specs() -> Vec<EffectRequestSpec<ReflectionRequest>> {
    vec![
        EffectRequestSpec::new(
            "env",
            ["reflection_runtime", "v0", "request", "env"],
            1,
            ReflectionRequest::Environment,
        ),
        EffectRequestSpec::new(
            "log",
            ["reflection_runtime", "v0", "request", "log"],
            2,
            ReflectionRequest::Log,
        ),
    ]
}

fn severity_matches(
    access: &RuntimeValueAccess<'_>,
    value: &CoreValue,
    name: &str,
    canonical: &Key,
) -> bool {
    access.key_from_value(value).as_ref() == Some(canonical)
        || access.same_representation(
            value,
            &CoreValue::Atom(Atom::from_key(&Key::binary_from_text(name))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Weak;

    use crate::api::{EffectTokenDomain, Values};

    fn assert_reflection_request_inventory(request: &ReflectionRequest) {
        match request {
            ReflectionRequest::Environment
            | ReflectionRequest::DictItems
            | ReflectionRequest::MetadataInspect
            | ReflectionRequest::Log
            | ReflectionRequest::ChildTask
            | ReflectionRequest::ChildStatus
            | ReflectionRequest::ChildRecover
            | ReflectionRequest::ChildInterrupt
            | ReflectionRequest::ChildClear
            | ReflectionRequest::ChildCall
            | ReflectionRequest::ChildCallLeave
            | ReflectionRequest::PromiseAccept
            | ReflectionRequest::PromiseAbandon
            | ReflectionRequest::PromiseAccepted
            | ReflectionRequest::TestQueueNow
            | ReflectionRequest::TestAcceptNow => {}
        }
    }

    fn assert_reflection_journal_inventory(update: &ReflectionUpdate, journal: &ReflectionJournal) {
        match update {
            ReflectionUpdate::Wake {
                host,
                machine,
                context,
            } => {
                let _: &Arc<dyn MachineHost> = host;
                let _: &Arc<[Key]> = machine;
                let _: &EvalContext = context;
            }
            ReflectionUpdate::Interrupt {
                host,
                machine,
                sequence,
            } => {
                let _: &Arc<dyn MachineHost> = host;
                let _: &Arc<[Key]> = machine;
                let _: &u64 = sequence;
            }
            ReflectionUpdate::Clear { host, machine } => {
                let _: &Arc<dyn MachineHost> = host;
                let _: &Arc<[Key]> = machine;
            }
        }
        let ReflectionJournal {
            diagnostics,
            updates,
            events,
        } = journal;
        let _: &Vec<Diagnostic> = diagnostics;
        let _: &Vec<ReflectionUpdate> = updates;
        // Port actions hold handle values, roots of their own.
        let _: &Option<crate::api::RuntimeEventJournal> = events;
    }

    fn assert_reflection_work_inventory(work: &ReflectionRequestWork) {
        match &work.operation {
            ReflectionRequestOperation::Environment(_)
            | ReflectionRequestOperation::Inspection(_)
            | ReflectionRequestOperation::Log(_)
            | ReflectionRequestOperation::ChildCallLeave
            | ReflectionRequestOperation::Poisoned => {}
            ReflectionRequestOperation::Acceptance(work) => match work {
                AcceptanceRequestWork::Start {
                    request: _,
                    arguments,
                } => {
                    let _: &Vec<Value> = arguments;
                }
                AcceptanceRequestWork::Awaiting {
                    request: _,
                    diagnostic,
                } => {
                    let _: &Option<Value> = diagnostic;
                }
            },
            ReflectionRequestOperation::Child(work) => {
                let ChildRequestWork { request, state } = work;
                let _: &ChildRequest = request;
                match state {
                    ChildRequestState::Start(arguments) => {
                        let _: &Vec<Value> = arguments;
                    }
                    ChildRequestState::Key { key, value } => {
                        let _: &KeyConversionRequestWork = key;
                        let _: &Option<Value> = value;
                    }
                }
            }
        }
    }

    #[test]
    fn reflection_request_root_inventory_is_complete() {
        let _: fn(&ReflectionRequest) = assert_reflection_request_inventory;
        let _: fn(&ReflectionUpdate, &ReflectionJournal) = assert_reflection_journal_inventory;
        let _: fn(&ReflectionRequestWork) = assert_reflection_work_inventory;
    }

    fn retained_request_value(domain: &EffectTokenDomain<Arc<()>>) -> (Value, Weak<()>) {
        let payload = Arc::new(());
        let retained = Arc::downgrade(&payload);
        (domain.issue(payload), retained)
    }

    #[test]
    fn request_journal_and_decoded_results_retain_public_roots_until_retirement() {
        let core = crate::core::private_test_value_factory();
        let values = Values::from_core_factory(core);
        let domain = EffectTokenDomain::new(&values);

        let (emission, retained) = retained_request_value(&domain);
        let journal = ReflectionJournal {
            diagnostics: vec![
                Diagnostic::from_emission(&values, Severity::Error, emission)
                    .expect("retained request fixture uses one runtime"),
            ],
            updates: Vec::new(),
            events: None,
        };
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained.upgrade().is_some());
        drop(journal);
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained.upgrade().is_none());
    }
}
