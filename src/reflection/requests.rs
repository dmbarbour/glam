use std::collections::BTreeMap;
use std::sync::Arc;

use crate::api::{Diagnostic, Value, Values};
use crate::core::{
    Atom, CoreValueFactory, Dict, Key, OpaqueValue, RuntimeValueAccess, Value as CoreValue, keys,
};
use crate::diagnostic::Severity;
use crate::evaluation::{
    EvalContext, EvaluationTaskCancellation, EvaluationTaskHandle, EvaluationTaskId,
    EvaluationTaskStatus, EvaluationWaitPoll, MachineHost, MachineTaskOutcome,
    PendingReflectionTask, PendingTaskPolicy, QueuedTask, TaskMachine, TaskStatusPublisher,
    TaskStatusWake,
};
use crate::list::{ListFrontStep, ListItem};
use crate::number::Number;
use crate::runtime::RuntimeFailureRoot;

use super::protocol::{
    CommitResult, EffectRequestSpec, RequestContext, RequestResult, SpecializationRequestInput,
    SpecializationRequestPoll, SpecializationRequestWork, TaskCommit, TaskEnvironment, TaskHalt,
    TaskHost, TaskSpecialization,
};
use super::store::{
    EvaluationQueryHandle, EvaluationQueryPoll, EvaluationQueryState, StoreJournal,
    decode_query_state,
};

/// Requests shared by every full reflection task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectionRequest {
    Environment,
    DictItems,
    MetadataInspect,
    Log,
    TaskNew,
    TaskJoin,
    TaskStatus,
    TaskValue,
    TaskHalt,
    TaskAcknowledgeError,
    TaskCancel,
    ChildTask,
    ChildStatus,
    ChildRecover,
    ChildInterrupt,
    ChildClear,
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
    TaskCreate(Vec<Value>),
    TaskControl(TaskControlRequestWork),
    TaskJoin(TaskJoinRequestWork),
    TaskQuery(TaskQueryRequestWork),
    Child(ChildRequestWork),
    Poisoned,
}

/// `.child.task Key Op`, `.child.status Key`, `.child.recover Key`,
/// `.child.interrupt Key Reason` and `.child.clear Key`, which convert their
/// key first (`reflection-tasks-are-machine-tasks`).
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
}

impl ChildRequest {
    /// Whether the request takes a value after its key: a task's operation,
    /// or an interrupt's reason.
    fn takes_value(self) -> bool {
        matches!(self, Self::Task | Self::Interrupt)
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

enum TaskControlRequestWork {
    Start {
        request: TaskControlRequest,
        arguments: Vec<Value>,
    },
    Awaiting(TaskControlRequest),
}

#[derive(Clone, Copy)]
enum TaskControlRequest {
    AcknowledgeError,
    Cancel,
}

enum TaskQueryRequestWork {
    Start {
        request: TaskQueryRequest,
        arguments: Vec<Value>,
    },
    Handle(TaskQueryRequest),
    State {
        request: TaskQueryRequest,
        handle: Arc<TaskHandleCell>,
        generation: u64,
    },
}

#[derive(Clone, Copy)]
enum TaskQueryRequest {
    Status,
    Value,
    Halt,
}

enum TaskJoinRequestWork {
    Start(Vec<Value>),
    Handle,
    Waiting(Arc<TaskHandleCell>),
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
            ReflectionRequest::TaskNew => ReflectionRequestOperation::TaskCreate(arguments),
            ReflectionRequest::TaskJoin => {
                ReflectionRequestOperation::TaskJoin(TaskJoinRequestWork::Start(arguments))
            }
            ReflectionRequest::TaskAcknowledgeError => {
                ReflectionRequestOperation::TaskControl(TaskControlRequestWork::Start {
                    request: TaskControlRequest::AcknowledgeError,
                    arguments,
                })
            }
            ReflectionRequest::TaskCancel => {
                ReflectionRequestOperation::TaskControl(TaskControlRequestWork::Start {
                    request: TaskControlRequest::Cancel,
                    arguments,
                })
            }
            ReflectionRequest::TaskStatus => {
                ReflectionRequestOperation::TaskQuery(TaskQueryRequestWork::Start {
                    request: TaskQueryRequest::Status,
                    arguments,
                })
            }
            ReflectionRequest::TaskValue => {
                ReflectionRequestOperation::TaskQuery(TaskQueryRequestWork::Start {
                    request: TaskQueryRequest::Value,
                    arguments,
                })
            }
            ReflectionRequest::TaskHalt => {
                ReflectionRequestOperation::TaskQuery(TaskQueryRequestWork::Start {
                    request: TaskQueryRequest::Halt,
                    arguments,
                })
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
            ReflectionRequestOperation::TaskCreate(arguments) => {
                assert!(input.is_none(), "task creation cannot have demand input");
                let result = create_task(arguments, context)?;
                Ok(SpecializationRequestPoll::Complete(result))
            }
            ReflectionRequestOperation::TaskJoin(TaskJoinRequestWork::Start(arguments)) => {
                assert!(input.is_none(), "new task join cannot have demand input");
                let [handle]: [Value; 1] = arguments.try_into().map_err(|_| {
                    TaskHalt::new("`.task.join` received the wrong number of arguments")
                })?;
                self.operation = ReflectionRequestOperation::TaskJoin(TaskJoinRequestWork::Handle);
                Ok(SpecializationRequestPoll::Demand(handle))
            }
            ReflectionRequestOperation::TaskJoin(TaskJoinRequestWork::Handle) => {
                let handle = demand_value(input, "resumed task join handle")?;
                let handle = evaluated_task_handle(context, &handle, "task.join")?;
                ensure_runtime_task(context.eval_context(), &handle)?;
                poll_task_join(&mut self.operation, context, handle)
            }
            ReflectionRequestOperation::TaskJoin(TaskJoinRequestWork::Waiting(handle)) => {
                assert!(
                    input.is_none(),
                    "shared task wake does not carry demand input"
                );
                poll_task_join(&mut self.operation, context, handle)
            }
            ReflectionRequestOperation::TaskControl(TaskControlRequestWork::Start {
                request,
                arguments,
            }) => {
                assert!(input.is_none(), "new task control cannot have demand input");
                let name = task_control_name(request);
                let [handle]: [Value; 1] = arguments.try_into().map_err(|_| {
                    TaskHalt::new(format!("`.{name}` received the wrong number of arguments"))
                })?;
                self.operation = ReflectionRequestOperation::TaskControl(
                    TaskControlRequestWork::Awaiting(request),
                );
                Ok(SpecializationRequestPoll::Demand(handle))
            }
            ReflectionRequestOperation::TaskControl(TaskControlRequestWork::Awaiting(request)) => {
                let handle = demand_value(input, "resumed task control")?;
                let handle = evaluated_task_handle(context, &handle, task_control_name(request))?;
                ensure_runtime_task(context.eval_context(), &handle)?;
                apply_task_control(context, request, handle)?;
                Ok(SpecializationRequestPoll::Complete(
                    RequestResult::ReturnUnit,
                ))
            }
            ReflectionRequestOperation::TaskQuery(TaskQueryRequestWork::Start {
                request,
                arguments,
            }) => {
                assert!(input.is_none(), "new task query cannot have demand input");
                let name = task_query_name(request);
                let [handle]: [Value; 1] = arguments.try_into().map_err(|_| {
                    TaskHalt::new(format!("`.{name}` received the wrong number of arguments"))
                })?;
                self.operation =
                    ReflectionRequestOperation::TaskQuery(TaskQueryRequestWork::Handle(request));
                Ok(SpecializationRequestPoll::Demand(handle))
            }
            ReflectionRequestOperation::TaskQuery(TaskQueryRequestWork::Handle(request)) => {
                let handle = demand_value(input, "resumed task query handle")?;
                let handle = evaluated_task_handle(context, &handle, task_query_name(request))?;
                ensure_runtime_task(context.eval_context(), &handle)?;
                let (state, generation) = read_query_value(context, &handle.status)?;
                self.operation =
                    ReflectionRequestOperation::TaskQuery(TaskQueryRequestWork::State {
                        request,
                        handle,
                        generation,
                    });
                Ok(SpecializationRequestPoll::Demand(state))
            }
            ReflectionRequestOperation::TaskQuery(TaskQueryRequestWork::State {
                request,
                handle,
                generation,
            }) => {
                let state = demand_value(input, "resumed task query state")?;
                let state = match state
                    .with_core_access(|state, access| decode_query_state(access, state))?
                {
                    Some(EvaluationQueryState::Pending) => None,
                    Some(EvaluationQueryState::Complete(result)) => Some(result),
                    None => return Err(TaskHalt::new("query handle has been retired")),
                };
                let result = finish_task_query(context, request, &handle, state, generation)?;
                Ok(SpecializationRequestPoll::Complete(result))
            }
            ReflectionRequestOperation::Poisoned => {
                panic!("completed reflection request work was polled again")
            }
        }
    }
}

fn poll_task_join<S: TaskSpecialization>(
    operation: &mut ReflectionRequestOperation,
    context: &RequestContext<'_, S>,
    handle: Arc<TaskHandleCell>,
) -> Result<SpecializationRequestPoll, TaskHalt> {
    match context.eval_context().poll_reflection_task(&handle.task) {
        EvaluationWaitPoll::Pending(wait) => {
            *operation = ReflectionRequestOperation::TaskJoin(TaskJoinRequestWork::Waiting(handle));
            Ok(SpecializationRequestPoll::Wait(
                super::protocol::SpecializationRequestWait::new(wait),
            ))
        }
        EvaluationWaitPoll::Complete(value) => Ok(SpecializationRequestPoll::Complete(
            RequestResult::Return(Value::from_runtime_root(*value)),
        )),
        EvaluationWaitPoll::Failed(error) => {
            handle.task.acknowledge_propagated_failure();
            Err(with_task_join_context(
                context.eval_context(),
                TaskHalt::rooted_failure(error),
                handle.task.id(),
            ))
        }
        EvaluationWaitPoll::Cancelled => Err(TaskHalt::new("joined reflection task was cancelled")),
        EvaluationWaitPoll::Abandoned => Err(with_task_join_context(
            context.eval_context(),
            TaskHalt::new(
                "joined reflection task was abandoned when its evaluation session closed",
            ),
            handle.task.id(),
        )),
        EvaluationWaitPoll::Exited => Err(with_task_join_context(
            context.eval_context(),
            TaskHalt::new("joined reflection task exited without producing a result"),
            handle.task.id(),
        )),
        EvaluationWaitPoll::Killed(error) => Err(with_task_join_context(
            context.eval_context(),
            TaskHalt::rooted_failure(error),
            handle.task.id(),
        )),
        // A panic is never a joinable result. The joiner stays waiting on the
        // panicked task, and the scheduler's poll boundary halts it.
        EvaluationWaitPoll::Panicked(_) => {
            let wait = handle.task.wait().clone();
            *operation = ReflectionRequestOperation::TaskJoin(TaskJoinRequestWork::Waiting(handle));
            Ok(SpecializationRequestPoll::Wait(
                super::protocol::SpecializationRequestWait::new(wait),
            ))
        }
    }
}

fn task_control_name(request: TaskControlRequest) -> &'static str {
    match request {
        TaskControlRequest::AcknowledgeError => "task.ack_error",
        TaskControlRequest::Cancel => "task.cancel",
    }
}

fn task_query_name(request: TaskQueryRequest) -> &'static str {
    match request {
        TaskQueryRequest::Status => "task.status",
        TaskQueryRequest::Value => "task.value",
        TaskQueryRequest::Halt => "task.error",
    }
}

fn read_query_value<S: TaskSpecialization>(
    context: &mut RequestContext<'_, S>,
    handle: &Arc<EvaluationQueryHandle>,
) -> Result<(Value, u64), TaskHalt> {
    let transaction_generation = context.transaction_generation();
    let (result, generation) = if let Some(mut transaction) = context.transaction() {
        let generation =
            transaction_generation.expect("active transaction must have a snapshot generation");
        (transaction.store().peek_query(handle), generation)
    } else {
        let snapshot = context.host().snapshot();
        (snapshot.store().poll_query(handle), snapshot.generation())
    };
    let EvaluationQueryPoll::State { value, .. } = result else {
        return Err(TaskHalt::new(
            "query handle does not belong to this runtime's protected query domain",
        ));
    };
    Ok((value, generation))
}

fn finish_task_query<S: TaskSpecialization>(
    context: &mut RequestContext<'_, S>,
    request: TaskQueryRequest,
    handle: &Arc<TaskHandleCell>,
    state: Option<Value>,
    generation: u64,
) -> Result<RequestResult, TaskHalt> {
    let Some(state) = state else {
        observe_query_change(context, &handle.status, generation);
        return Ok(RequestResult::Fail);
    };
    if matches!(request, TaskQueryRequest::Status) {
        return Ok(RequestResult::Return(state));
    }
    match (request, tagged_task_state(&context.values(), &state)?) {
        (TaskQueryRequest::Value, TaggedTaskState::Complete(value)) => {
            Ok(RequestResult::Return(value))
        }
        (
            TaskQueryRequest::Halt,
            TaggedTaskState::Failed(error) | TaggedTaskState::Killed(error),
        ) => Ok(RequestResult::Return(error)),
        (TaskQueryRequest::Halt, TaggedTaskState::Cancelled) => Ok(RequestResult::Return(
            context.values().text("reflection task was cancelled"),
        )),
        (
            TaskQueryRequest::Value | TaskQueryRequest::Halt,
            TaggedTaskState::Launched | TaggedTaskState::Blocked,
        ) => {
            observe_query_change(context, &handle.status, generation);
            Ok(RequestResult::Fail)
        }
        (
            TaskQueryRequest::Value,
            TaggedTaskState::Failed(_)
            | TaggedTaskState::Cancelled
            | TaggedTaskState::Abandoned
            | TaggedTaskState::Exited
            | TaggedTaskState::Killed(_)
            | TaggedTaskState::Panicked,
        )
        | (
            TaskQueryRequest::Halt,
            TaggedTaskState::Complete(_)
            | TaggedTaskState::Abandoned
            | TaggedTaskState::Exited
            | TaggedTaskState::Panicked,
        ) => Ok(RequestResult::Fail),
        (TaskQueryRequest::Status, _) => unreachable!("status returns before tagged decoding"),
    }
}

fn evaluated_task_handle<S: TaskSpecialization>(
    context: &RequestContext<'_, S>,
    handle: &crate::api::EvaluatedValue,
    request: &str,
) -> Result<Arc<TaskHandleCell>, TaskHalt> {
    handle.with_core_access(|value, _| {
        let CoreValue::Opaque(handle) = value else {
            return Err(TaskHalt::new(format!(
                "`.{request}` requires a reflection task handle"
            )));
        };
        handle
            .downcast::<TaskHandleCell>(context.eval_context().values())
            .ok_or_else(|| TaskHalt::new(format!("`.{request}` requires a reflection task handle")))
    })?
}

fn apply_task_control<S>(
    context: &mut RequestContext<'_, S>,
    request: TaskControlRequest,
    handle: Arc<TaskHandleCell>,
) -> Result<(), TaskHalt>
where
    S: TaskSpecialization,
    S::Journal: ReflectionTransaction,
{
    if let Some(mut transaction) = context.transaction() {
        transaction
            .parts()
            .1
            .reflection_journal()
            .updates
            .push(match request {
                TaskControlRequest::AcknowledgeError => {
                    ReflectionUpdate::AcknowledgeError(handle.task.clone())
                }
                TaskControlRequest::Cancel => ReflectionUpdate::Cancel(handle.task.clone()),
            });
    } else {
        match request {
            TaskControlRequest::AcknowledgeError => {
                handle.task.acknowledge_failure();
                context.committed();
            }
            TaskControlRequest::Cancel => {
                if matches!(handle.task.cancel(), EvaluationTaskCancellation::Requested) {
                    context.committed();
                }
            }
        }
    }
    Ok(())
}

fn child_request_name(request: ChildRequest) -> &'static str {
    match request {
        ChildRequest::Task => ".child.task",
        ChildRequest::Status => ".child.status",
        ChildRequest::Recover => ".child.recover",
        ChildRequest::Interrupt => ".child.interrupt",
        ChildRequest::Clear => ".child.clear",
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

/// The child `key` of the machine the current task runs on.
fn child_machine<S>(key: Key, context: &RequestContext<'_, S>) -> Result<Vec<Key>, TaskHalt>
where
    S: TaskSpecialization,
{
    let eval_context = context.eval_context();
    let mut path = match eval_context.machine() {
        Some(machine) => machine.path().to_vec(),
        None => anonymous_root(
            eval_context
                .task_id()
                .map_err(|error| TaskHalt::new(error.as_ref()))?,
        ),
    };
    path.push(key);
    Ok(path)
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
    let Machines(host) = context
        .host()
        .machines()
        .ok_or_else(|| TaskHalt::new("`.child.task` needs a host with machines"))?;
    let path = child_machine(key, context)?;
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
    let no_machines = || TaskHalt::new("`.child.task` needs a host with machines");
    if let Some(mut transaction) = context.transaction() {
        let journal = transaction.parts().1.reflection_journal();
        let Some(events) = journal.events.as_mut() else {
            promises.forget(promise);
            return Err(no_machines());
        };
        events.queue_task(path, task);
        journal.updates.push(wake);
    } else {
        let snapshot = context.host().snapshot();
        let store = StoreJournal::new(snapshot.store().clone());
        let mut journal = S::begin_journal(snapshot.extra());
        {
            let reflection = journal.reflection_journal();
            let Some(events) = reflection.events.as_mut() else {
                promises.forget(promise);
                return Err(no_machines());
            };
            events.queue_task(path, task);
            reflection.updates.push(wake);
        }
        // Appending to a buffer conflicts with nothing a commit validates.
        match context
            .host()
            .commit(TaskCommit::new(store, snapshot.extra().clone(), journal))
        {
            CommitResult::Committed => context.committed(),
            CommitResult::Conflict => {
                return Err(TaskHalt::new("queueing a child's task conflicted"));
            }
            CommitResult::MissingVolume(volume) => {
                return Err(TaskHalt::new(format!(
                    "reflection volume {} is unavailable",
                    volume.get()
                )));
            }
            CommitResult::Closed => return Ok(RequestResult::Cancelled),
        }
    }
    Ok(RequestResult::Return(promised))
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
                pending.launch_on(Arc::new(TaskMachine::new(
                    machine.clone(),
                    host.clone(),
                    sequence,
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

fn create_task<S>(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, S>,
) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    let [effect]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("`.task.new` received the wrong number of arguments"))?;
    let eval_context = context.eval_context().clone();
    let query_writer = context.host().query_writer().ok_or_else(|| {
        TaskHalt::new("current reflection host does not support task status queries")
    })?;
    let values = context.values();
    let effect = effect.into_runtime_root();
    let launched = task_status_public_value(&values, EvaluationTaskStatus::Launched);
    let handle = if let Some(mut transaction) = context.transaction() {
        let result = transaction
            .store()
            .reserve_query_with(launched.clone())
            .map_err(|error| TaskHalt::new(error.as_ref()))?;
        let effect = effect.clone();
        let pending = eval_context
            .reserve_reflection_task(effect)
            .map_err(|error| TaskHalt::new(error.as_ref()))?;
        let handle = Arc::new(TaskHandleCell {
            runtime: eval_context.values().runtime_id(),
            task: pending.handle().clone(),
            status: result.clone(),
        });
        let publisher = task_status_publisher(query_writer, result, eval_context.values().clone());
        transaction
            .parts()
            .1
            .reflection_journal()
            .updates
            .push(ReflectionUpdate::Launch {
                task: pending,
                publisher,
            });
        handle
    } else {
        let snapshot = context.host().snapshot();
        let mut store = StoreJournal::new(snapshot.store().clone());
        let result = store
            .reserve_query_with(launched)
            .map_err(|error| TaskHalt::new(error.as_ref()))?;
        let effect = effect.clone();
        let pending = eval_context
            .reserve_reflection_task(effect)
            .map_err(|error| TaskHalt::new(error.as_ref()))?;
        let handle = Arc::new(TaskHandleCell {
            runtime: eval_context.values().runtime_id(),
            task: pending.handle().clone(),
            status: result.clone(),
        });
        let publisher = task_status_publisher(query_writer, result, eval_context.values().clone());
        let mut journal = S::Journal::default();
        journal
            .reflection_journal()
            .updates
            .push(ReflectionUpdate::Launch {
                task: pending,
                publisher,
            });
        match context
            .host()
            .commit(TaskCommit::new(store, snapshot.extra().clone(), journal))
        {
            CommitResult::Committed => context.committed(),
            CommitResult::Conflict => {
                return Err(TaskHalt::new("fresh task reservation conflicted"));
            }
            CommitResult::MissingVolume(volume) => {
                return Err(TaskHalt::new(format!(
                    "private query volume {} is unavailable",
                    volume.get()
                )));
            }
            CommitResult::Closed => return Ok(RequestResult::Cancelled),
        }
        handle
    };
    Ok(RequestResult::Return(task_handle_value(
        context.eval_context(),
        handle,
    )))
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
    Launch {
        task: PendingReflectionTask,
        publisher: TaskStatusPublisher,
    },
    Cancel(EvaluationTaskHandle),
    AcknowledgeError(EvaluationTaskHandle),
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
        let mut pending_policies = BTreeMap::new();
        for update in &self.updates {
            if let ReflectionUpdate::Launch { task, .. } = update {
                assert!(
                    pending_policies
                        .insert(task.handle().id(), PendingTaskPolicy::default())
                        .is_none(),
                    "a pending reflection task must be launched exactly once"
                );
            }
        }
        for update in &self.updates {
            match update {
                ReflectionUpdate::Cancel(task) => {
                    if let Some(policy) = pending_policies.get_mut(&task.id()) {
                        policy.cancel();
                    }
                }
                ReflectionUpdate::AcknowledgeError(task) => {
                    if let Some(policy) = pending_policies.get_mut(&task.id()) {
                        policy.acknowledge_error();
                    }
                }
                ReflectionUpdate::Launch { .. }
                | ReflectionUpdate::Wake { .. }
                | ReflectionUpdate::Interrupt { .. }
                | ReflectionUpdate::Clear { .. } => {}
            }
        }
        for update in &self.updates {
            match update {
                ReflectionUpdate::Launch { task, publisher } => task.commit(
                    publisher.clone(),
                    *pending_policies
                        .get(&task.handle().id())
                        .expect("every pending launch must have a policy"),
                ),
                ReflectionUpdate::Cancel(task) => {
                    if !pending_policies.contains_key(&task.id()) {
                        task.cancel();
                    }
                }
                ReflectionUpdate::AcknowledgeError(task) => {
                    if !pending_policies.contains_key(&task.id()) {
                        task.acknowledge_failure();
                    }
                }
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

    /// Returns the runtime-owned writer for protected asynchronous queries.
    ///
    /// Full reflection hosts return a writer which does not retain their
    /// role-specific environment, diagnostics, launcher, or demand lease.
    /// Restricted effect profiles which expose no query-producing operation
    /// may leave this unavailable.
    #[doc(hidden)]
    fn query_writer(&self) -> Option<Arc<dyn ReflectionQueryWriter>> {
        None
    }

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

/// Narrow runtime capability for completing protected asynchronous queries.
#[doc(hidden)]
pub trait ReflectionQueryWriter: Send + Sync {
    fn update_query_guarded(
        &self,
        mutation: ReflectionQueryMutation<'_>,
        handle: &Arc<EvaluationQueryHandle>,
        result: Value,
    ) -> Box<dyn FnOnce() + Send>;
}

/// Opaque proof that a protected query update participates in the caller's
/// current runtime mutation admission.
#[doc(hidden)]
pub struct ReflectionQueryMutation<'guard> {
    mutation: &'guard dyn crate::runtime::RuntimeMutationAuthority,
}

impl<'guard> ReflectionQueryMutation<'guard> {
    fn new(mutation: &'guard dyn crate::runtime::RuntimeMutationAuthority) -> Self {
        Self { mutation }
    }

    pub(crate) fn guard(&self) -> &dyn crate::runtime::RuntimeMutationAuthority {
        self.mutation
    }
}

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
                ["task", "new"],
                ["reflection_runtime", "v0", "request", "task", "new"],
                1,
                ReflectionRequest::TaskNew,
            ),
            EffectRequestSpec::at_path(
                ["task", "join"],
                ["reflection_runtime", "v0", "request", "task", "join"],
                1,
                ReflectionRequest::TaskJoin,
            ),
            EffectRequestSpec::at_path(
                ["task", "status"],
                ["reflection_runtime", "v0", "request", "task", "status"],
                1,
                ReflectionRequest::TaskStatus,
            ),
            EffectRequestSpec::at_path(
                ["task", "value"],
                ["reflection_runtime", "v0", "request", "task", "value"],
                1,
                ReflectionRequest::TaskValue,
            ),
            EffectRequestSpec::at_path(
                ["task", "error"],
                ["reflection_runtime", "v0", "request", "task", "error"],
                1,
                ReflectionRequest::TaskHalt,
            ),
            EffectRequestSpec::at_path(
                ["task", "ack_error"],
                ["reflection_runtime", "v0", "request", "task", "ack_error"],
                1,
                ReflectionRequest::TaskAcknowledgeError,
            ),
            EffectRequestSpec::at_path(
                ["task", "cancel"],
                ["reflection_runtime", "v0", "request", "task", "cancel"],
                1,
                ReflectionRequest::TaskCancel,
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

/// Handles one reusable reflection request inside a composed task.
/// Runtime-local opaque task capability shared by every clone of the Glam
/// handle value.
///
/// The nested task identity retains only scalar runtime/owner provenance, the
/// terminal wait cell, and a weak coordinator reporting route. It cannot keep
/// the originating demand state or external owner lease alive. The protected
/// query handle remains the sole transactional status/value/error view and
/// queues its own retirement after the final task-cell and publisher clone are
/// dropped.
struct TaskHandleCell {
    runtime: crate::runtime::EvaluationRuntimeId,
    task: EvaluationTaskHandle,
    status: Arc<EvaluationQueryHandle>,
}

#[cfg(test)]
pub(crate) fn assert_task_handle_family_shape() {
    fn inspect(handle: &TaskHandleCell) {
        let TaskHandleCell {
            runtime,
            task,
            status,
        } = handle;
        let _: &crate::runtime::EvaluationRuntimeId = runtime;
        let _: &EvaluationTaskHandle = task;
        let _: &Arc<EvaluationQueryHandle> = status;
    }

    let _: fn(&TaskHandleCell) = inspect;
    assert_eq!(
        <TaskHandleCell as crate::core::OpaquePayloadFamily>::PAYLOAD_RECORD
            .fields()
            .2,
        "external capability"
    );
}

// SAFETY: the handle contains no bare core value, runtime value root, or
// managed pointer. It is an external lifecycle capability over coordinator
// and query state, so the active-retirement inventory retains its external
// classification rather than treating it as a managed leaf.
unsafe impl crate::core::OpaquePayloadFamily for TaskHandleCell {
    const PAYLOAD_RECORD: crate::core::OpaquePayloadRecord =
        crate::core::OpaquePayloadRecord::external(
            "reflection task handle",
            "src/reflection/requests.rs",
        );
}

fn task_handle_value(context: &EvalContext, handle: Arc<TaskHandleCell>) -> Value {
    let values = Values::from_core_factory(context.values().clone());
    debug_assert_eq!(handle.runtime, values.runtime_id());
    debug_assert_eq!(handle.runtime, handle.task.runtime_id());
    values.with_access(|access| {
        access.wrap(CoreValue::Opaque(OpaqueValue::new(values.core(), handle)))
    })
}

fn task_join_context(_access: &RuntimeValueAccess<'_>, task: EvaluationTaskId) -> CoreValue {
    let operation = CoreValue::Atom(Atom::from_key(&Key::binary_from_text("join")));
    let detail = Dict::new_sync()
        .insert(Key::atom_from_text("operation"), operation)
        .insert(
            Key::atom_from_text("id"),
            CoreValue::Number(Number::from_u64(task.get())),
        );
    CoreValue::Dict(Dict::new_sync().insert(Key::atom_from_text("task"), CoreValue::Dict(detail)))
}

fn with_task_join_context(
    context: &EvalContext,
    error: TaskHalt,
    task: EvaluationTaskId,
) -> TaskHalt {
    context.values().with_boundary_access(|access| {
        error.with_core_context_in(&access, task_join_context(&access, task))
    })
}

fn task_status_query_value(
    access: &RuntimeValueAccess<'_>,
    status: EvaluationTaskStatus,
    failure_diagnostic: Option<CoreValue>,
) -> CoreValue {
    match status {
        EvaluationTaskStatus::Launched => access.value_from_key(&keys::LAUNCHED),
        EvaluationTaskStatus::Blocked => access.value_from_key(&keys::BLOCKED),
        EvaluationTaskStatus::Complete(value) => CoreValue::Dict(
            Dict::new_sync().insert((*keys::OK).clone(), value.clone_core_with(access)),
        ),
        EvaluationTaskStatus::Failed(_) => CoreValue::Dict(Dict::new_sync().insert(
            (*keys::ERR).clone(),
            failure_diagnostic.expect("failed task status precomputes its diagnostic"),
        )),
        // A killed task carries its kill reason, so a status reader sees why
        // without joining. The kill is published while the runtime settles a
        // deadlock, where nothing can evaluate, so its emission is stored as
        // built rather than normalized like a failure's diagnostic.
        EvaluationTaskStatus::Killed(error) => {
            let failure = error.as_failure();
            let diagnostic = match failure.emission_value_in(access) {
                Some(emission) => access.duplicate_value(emission),
                None => crate::diagnostic::text_message_in(access, None, failure.to_string()),
            };
            CoreValue::Dict(Dict::new_sync().insert((*keys::KILLED).clone(), diagnostic))
        }
        EvaluationTaskStatus::Cancelled => access.value_from_key(&keys::CANCELED),
        EvaluationTaskStatus::Abandoned => access.value_from_key(&keys::ABANDONED),
        EvaluationTaskStatus::Exited => access.value_from_key(&keys::EXITED),
        EvaluationTaskStatus::Panicked(_) => access.value_from_key(&keys::PANICKED),
    }
}

/// The value a `.task.status` query publishes for `status`.
#[cfg(test)]
pub(crate) fn task_status_public_value_for_test(
    values: &Values,
    status: EvaluationTaskStatus,
) -> Value {
    task_status_public_value(values, status)
}

fn task_status_public_value(values: &Values, status: EvaluationTaskStatus) -> Value {
    let failure_diagnostic = match &status {
        EvaluationTaskStatus::Failed(error) => Some(error.diagnostic(values.core())),
        _ => None,
    };
    Value::from_runtime_root(values.core().construct_runtime_value_root(|access| {
        let failure_diagnostic = failure_diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.clone_core_with(access));
        task_status_query_value(access, status, failure_diagnostic)
    }))
}

fn task_status_publisher(
    writer: Arc<dyn ReflectionQueryWriter>,
    handle: Arc<EvaluationQueryHandle>,
    values: CoreValueFactory,
) -> TaskStatusPublisher {
    TaskStatusPublisher::new(move |mutation, status| {
        let values = Values::from_core_factory(values.clone());
        let notify = writer.update_query_guarded(
            ReflectionQueryMutation::new(mutation),
            &handle,
            task_status_public_value(&values, status),
        );
        TaskStatusWake::new(notify)
    })
}

enum TaggedTaskState {
    Launched,
    Blocked,
    Complete(Value),
    Failed(Value),
    Cancelled,
    Abandoned,
    Exited,
    Killed(Value),
    Panicked,
}

fn tagged_task_state(values: &Values, value: &Value) -> Result<TaggedTaskState, TaskHalt> {
    values.with_access(|access| {
        let value = access.clone_core(value)?;
        if access
            .runtime_access()
            .same_representation(&value, &access.runtime_access().key_value(&keys::LAUNCHED))
        {
            return Ok(TaggedTaskState::Launched);
        }
        if access
            .runtime_access()
            .same_representation(&value, &access.runtime_access().key_value(&keys::BLOCKED))
        {
            return Ok(TaggedTaskState::Blocked);
        }
        if access
            .runtime_access()
            .same_representation(&value, &access.runtime_access().key_value(&keys::CANCELED))
        {
            return Ok(TaggedTaskState::Cancelled);
        }
        if access
            .runtime_access()
            .same_representation(&value, &access.runtime_access().key_value(&keys::ABANDONED))
        {
            return Ok(TaggedTaskState::Abandoned);
        }
        if access
            .runtime_access()
            .same_representation(&value, &access.runtime_access().key_value(&keys::EXITED))
        {
            return Ok(TaggedTaskState::Exited);
        }
        if access
            .runtime_access()
            .same_representation(&value, &access.runtime_access().key_value(&keys::PANICKED))
        {
            return Ok(TaggedTaskState::Panicked);
        }
        let CoreValue::Dict(state) = value else {
            return Err(TaskHalt::new("reflection task status is malformed"));
        };
        if state.iter().count() != 1 {
            return Err(TaskHalt::new("reflection task status is malformed"));
        }
        if let Some(value) = state.get(&*keys::OK) {
            return Ok(TaggedTaskState::Complete(
                access.wrap(access.runtime_access().duplicate_value(value)),
            ));
        }
        if let Some(error) = state.get(&*keys::ERR) {
            return Ok(TaggedTaskState::Failed(
                access.wrap(access.runtime_access().duplicate_value(error)),
            ));
        }
        if let Some(error) = state.get(&*keys::KILLED) {
            return Ok(TaggedTaskState::Killed(
                access.wrap(access.runtime_access().duplicate_value(error)),
            ));
        }
        Err(TaskHalt::new("reflection task status is malformed"))
    })
}

fn ensure_runtime_task(context: &EvalContext, handle: &TaskHandleCell) -> Result<(), TaskHalt> {
    if handle.runtime != context.values().runtime_id() {
        Err(TaskHalt::new(
            "task handle belongs to a different evaluation runtime",
        ))
    } else {
        Ok(())
    }
}

fn observe_query_change<S: TaskSpecialization>(
    context: &mut RequestContext<'_, S>,
    handle: &Arc<EvaluationQueryHandle>,
    generation: u64,
) {
    let observed = if let Some(mut transaction) = context.transaction() {
        transaction.store().observe_query(handle)
    } else {
        true
    };
    if observed {
        context.observe_host_generation(generation);
    }
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
    use std::sync::{Mutex, Weak};

    use crate::api::{EffectTokenDomain, Values};
    use crate::evaluation::{EvaluationMachinePoll, EvaluationTaskMachine};

    struct TestQueryWriter {
        store: Arc<Mutex<crate::reflection::ReflectionStore>>,
        updates: Arc<Mutex<Vec<Value>>>,
    }

    struct TestRoleHost {
        writer: Arc<dyn ReflectionQueryWriter>,
    }

    struct CompleteTask(CoreValue);

    fn assert_reflection_request_inventory(request: &ReflectionRequest) {
        match request {
            ReflectionRequest::Environment
            | ReflectionRequest::DictItems
            | ReflectionRequest::MetadataInspect
            | ReflectionRequest::Log
            | ReflectionRequest::TaskNew
            | ReflectionRequest::TaskJoin
            | ReflectionRequest::TaskStatus
            | ReflectionRequest::TaskValue
            | ReflectionRequest::TaskHalt
            | ReflectionRequest::TaskAcknowledgeError
            | ReflectionRequest::TaskCancel
            | ReflectionRequest::ChildTask
            | ReflectionRequest::ChildStatus
            | ReflectionRequest::ChildRecover
            | ReflectionRequest::ChildInterrupt
            | ReflectionRequest::ChildClear => {}
        }
    }

    fn assert_reflection_journal_inventory(update: &ReflectionUpdate, journal: &ReflectionJournal) {
        match update {
            ReflectionUpdate::Launch { task, publisher } => {
                let _: &PendingReflectionTask = task;
                let _: &TaskStatusPublisher = publisher;
            }
            ReflectionUpdate::Cancel(task) | ReflectionUpdate::AcknowledgeError(task) => {
                let _: &EvaluationTaskHandle = task;
            }
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

    fn assert_tagged_task_state_inventory(state: &TaggedTaskState) {
        match state {
            TaggedTaskState::Complete(value)
            | TaggedTaskState::Failed(value)
            | TaggedTaskState::Killed(value) => {
                let _: &Value = value;
            }
            TaggedTaskState::Launched
            | TaggedTaskState::Blocked
            | TaggedTaskState::Cancelled
            | TaggedTaskState::Abandoned
            | TaggedTaskState::Exited
            | TaggedTaskState::Panicked => {}
        }
    }

    fn assert_reflection_work_inventory(work: &ReflectionRequestWork) {
        match &work.operation {
            ReflectionRequestOperation::Environment(_)
            | ReflectionRequestOperation::Inspection(_)
            | ReflectionRequestOperation::Log(_)
            | ReflectionRequestOperation::TaskCreate(_)
            | ReflectionRequestOperation::TaskControl(_)
            | ReflectionRequestOperation::TaskJoin(_)
            | ReflectionRequestOperation::TaskQuery(_)
            | ReflectionRequestOperation::Poisoned => {}
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

    fn assert_query_mutation_inventory(mutation: &ReflectionQueryMutation<'_>) {
        let ReflectionQueryMutation { mutation } = mutation;
        let _: &&dyn crate::runtime::RuntimeMutationAuthority = mutation;
    }

    #[test]
    fn reflection_request_root_inventory_is_complete() {
        let _: fn(&ReflectionRequest) = assert_reflection_request_inventory;
        let _: fn(&ReflectionUpdate, &ReflectionJournal) = assert_reflection_journal_inventory;
        assert_task_handle_family_shape();
        let _: fn(&TaggedTaskState) = assert_tagged_task_state_inventory;
        let _: fn(&ReflectionRequestWork) = assert_reflection_work_inventory;
        let _: fn(&ReflectionQueryMutation<'_>) = assert_query_mutation_inventory;
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

        for build in [
            TaggedTaskState::Complete as fn(Value) -> TaggedTaskState,
            TaggedTaskState::Failed,
        ] {
            let (value, retained) = retained_request_value(&domain);
            let state = build(value);
            domain.collect_and_drain_retired_external_owners_for_test();
            assert!(retained.upgrade().is_some());
            drop(state);
            domain.collect_and_drain_retired_external_owners_for_test();
            assert!(retained.upgrade().is_none());
        }
    }

    impl EvaluationTaskMachine for CompleteTask {
        fn poll(
            &mut self,
            _context: &mut crate::evaluation::EvaluationPollContext,
            _step_budget: &mut crate::evaluation::EvaluationStepBudget,
        ) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Complete(_context.duplicate_and_root_value(&self.0))
        }
    }

    impl ReflectionQueryWriter for TestQueryWriter {
        fn update_query_guarded(
            &self,
            _mutation: ReflectionQueryMutation<'_>,
            handle: &Arc<EvaluationQueryHandle>,
            result: Value,
        ) -> Box<dyn FnOnce() + Send> {
            self.updates
                .lock()
                .expect("test query updates were poisoned")
                .push(result.clone());
            assert!(
                self.store
                    .lock()
                    .expect("test query store was poisoned")
                    .update_query(handle, result)
            );
            Box::new(|| {})
        }
    }

    #[test]
    fn abandoned_task_status_has_a_distinct_round_trip() {
        let core = crate::core::test_value_factory();
        let values = Values::from_core_factory(core);
        let encoded = task_status_public_value(&values, EvaluationTaskStatus::Abandoned);

        values.core().with_boundary_access(|access| {
            access.assert_same_representation_for_test(
                &encoded.clone_core_with(&access).unwrap(),
                &access.key_value(&keys::ABANDONED),
            );
        });
        assert!(matches!(
            tagged_task_state(&values, &encoded).expect("abandoned status should decode"),
            TaggedTaskState::Abandoned
        ));
    }

    #[test]
    fn exited_and_killed_task_statuses_have_distinct_round_trips() {
        let core = crate::core::test_value_factory();
        let values = Values::from_core_factory(core.clone());
        let exited = task_status_public_value(&values, EvaluationTaskStatus::Exited);
        values.core().with_boundary_access(|access| {
            access.assert_same_representation_for_test(
                &exited.clone_core_with(&access).unwrap(),
                &access.key_value(&keys::EXITED),
            );
        });
        assert!(matches!(
            tagged_task_state(&values, &exited).expect("exited status should decode"),
            TaggedTaskState::Exited
        ));

        let killed = task_status_public_value(
            &values,
            EvaluationTaskStatus::Killed(crate::runtime::RuntimeFailureRoot::new(
                &core,
                Arc::new(crate::core::EvaluationFailure::message("killed fixture")),
            )),
        );
        // Like a failure, a kill carries its diagnostic as `{killed: Diagnostic}`.
        let TaggedTaskState::Killed(diagnostic) =
            tagged_task_state(&values, &killed).expect("killed status should decode")
        else {
            panic!("a killed status decodes as killed")
        };
        values.core().with_boundary_access(|access| {
            let CoreValue::Dict(diagnostic) = diagnostic.clone_core_with(&access).unwrap() else {
                panic!("a kill diagnostic is a dictionary")
            };
            let Some(CoreValue::Dict(message)) = diagnostic.get(&*keys::MSG) else {
                panic!("a kill diagnostic has a message interface")
            };
            access.assert_same_representation_for_test(
                message.get(&*keys::TEXT).expect("the kill keeps its text"),
                &CoreValue::binary_from_text("killed fixture"),
            );
        });
    }

    #[test]
    fn panicked_task_status_round_trips_without_a_failure_payload() {
        let core = crate::core::test_value_factory();
        let values = Values::from_core_factory(core.clone());
        let panicked = task_status_public_value(
            &values,
            EvaluationTaskStatus::Panicked(crate::core::EvaluationPanic::from_payload(
                &"panicked fixture",
                crate::core::EvaluationPanicOrigin::ReflectionTask(1),
            )),
        );
        values.core().with_boundary_access(|access| {
            access.assert_same_representation_for_test(
                &panicked.clone_core_with(&access).unwrap(),
                &access.key_value(&keys::PANICKED),
            );
        });
        assert!(matches!(
            tagged_task_state(&values, &panicked).expect("panicked status should decode"),
            TaggedTaskState::Panicked
        ));
    }

    #[test]
    fn task_status_publisher_does_not_retain_its_role_host() {
        let values = crate::core::test_value_factory();
        let public_values = Values::from_core_factory(values.clone());
        let store = Arc::new(Mutex::new(crate::reflection::ReflectionStore::new(
            values.clone(),
            Arc::new(crate::reflection::ExactConflictAnalysis),
        )));
        let handle = {
            let mut store = store.lock().expect("test query store was poisoned");
            let mut journal = StoreJournal::new(store.snapshot());
            let handle = journal
                .reserve_query_with(task_status_public_value(
                    &public_values,
                    EvaluationTaskStatus::Launched,
                ))
                .expect("test status query should reserve");
            assert!(matches!(
                store.try_commit(&journal),
                crate::reflection::StoreCommitResult::Committed
            ));
            handle
        };

        let updates = Arc::new(Mutex::new(Vec::new()));
        let role_host = Arc::new(TestRoleHost {
            writer: Arc::new(TestQueryWriter {
                store: store.clone(),
                updates: updates.clone(),
            }),
        });
        let role_host_weak = Arc::downgrade(&role_host);
        let publisher =
            task_status_publisher(role_host.writer.clone(), handle.clone(), values.clone());
        drop(role_host);
        assert!(
            role_host_weak.upgrade().is_none(),
            "the status publisher must not retain its originating role host"
        );

        let admission = crate::runtime::RuntimeMutationAdmission::new();
        let mutation = admission.mutation_guard();
        let wake = publisher.publish_guarded(&mutation, EvaluationTaskStatus::Blocked);
        drop(mutation);
        wake.notify();
        let query = store
            .lock()
            .expect("test query store was poisoned")
            .snapshot()
            .poll_query(&handle);
        let EvaluationQueryPoll::State { .. } = query else {
            panic!("status publisher should retain its query domain")
        };
        let updates = updates.lock().expect("test query updates were poisoned");
        assert_eq!(updates.len(), 1);
        values.with_boundary_access(|access| {
            access.assert_same_representation_for_test(
                &updates[0].clone_core_with(&access).unwrap(),
                &access.key_value(&keys::BLOCKED),
            );
        });
    }

    #[test]
    fn terminal_task_handle_cell_releases_the_final_query_lease() {
        let context = EvalContext::isolated(crate::core::private_test_value_factory());
        let values = context.values().clone();
        let public_values = Values::from_core_factory(values.clone());
        let task = context
            .schedule_task(|task_context| {
                let unit = task_context
                    .values()
                    .with_boundary_access(|access| access.unit());
                Ok(Box::new(CompleteTask(unit)))
            })
            .expect("terminal task-handle fixture should schedule");
        let store = Arc::new(Mutex::new(crate::reflection::ReflectionStore::new(
            values.clone(),
            Arc::new(crate::reflection::ExactConflictAnalysis),
        )));
        let status = {
            let mut store = store.lock().expect("test query store was poisoned");
            let mut journal = StoreJournal::new(store.snapshot());
            let status = journal
                .reserve_query_with(task_status_public_value(
                    &public_values,
                    EvaluationTaskStatus::Launched,
                ))
                .expect("task status query should reserve");
            assert!(matches!(
                store.try_commit(&journal),
                crate::reflection::StoreCommitResult::Committed
            ));
            status
        };
        let status_weak = Arc::downgrade(&status);
        let writer: Arc<dyn ReflectionQueryWriter> = Arc::new(TestQueryWriter {
            store,
            updates: Arc::new(Mutex::new(Vec::new())),
        });
        assert!(context.attach_task_status_publisher(
            &task,
            task_status_publisher(writer, status.clone(), values.clone()),
        ));
        let handle = Arc::new(TaskHandleCell {
            runtime: context.values().runtime_id(),
            task,
            status: status.clone(),
        });
        let opaque = task_handle_value(&context, handle);
        drop(status);

        assert!(matches!(
            context.run_until_quiescent(),
            crate::evaluation::EvaluationSessionRun::Complete(_)
        ));
        assert!(
            status_weak.upgrade().is_some(),
            "the terminal opaque handle must retain its transactional query"
        );

        drop(opaque);
        values.collect_and_drain_external_owners_for_test();
        assert!(
            status_weak.upgrade().is_none(),
            "dropping the final opaque handle must release the final query lease"
        );
    }

    #[test]
    fn task_handle_root_backedge_is_conservatively_external() {
        let context = EvalContext::isolated(crate::core::private_test_value_factory());
        let values = context.values().clone();
        let public_values = Values::from_core_factory(values.clone());
        let task = context
            .schedule_task(|task_context| {
                let unit = task_context
                    .values()
                    .with_boundary_access(|access| access.unit());
                Ok(Box::new(CompleteTask(unit)))
            })
            .expect("task-handle backedge fixture should schedule");
        let status = {
            let mut store = crate::reflection::ReflectionStore::new(
                values.clone(),
                Arc::new(crate::reflection::ExactConflictAnalysis),
            );
            let mut journal = StoreJournal::new(store.snapshot());
            let status = journal
                .reserve_query_with(task_status_public_value(
                    &public_values,
                    EvaluationTaskStatus::Launched,
                ))
                .expect("task status query should reserve");
            assert!(matches!(
                store.try_commit(&journal),
                crate::reflection::StoreCommitResult::Committed
            ));
            status
        };
        let status_weak = Arc::downgrade(&status);
        let opaque = task_handle_value(
            &context,
            Arc::new(TaskHandleCell {
                runtime: values.runtime_id(),
                task: task.clone(),
                status: status.clone(),
            }),
        );
        let opaque_core = public_values
            .clone_core(&opaque)
            .expect("task handle belongs to the fixture runtime");

        context.complete_wait_with_value(task.wait(), opaque_core);
        drop(task);
        drop(status);
        drop(opaque);
        values.collect_and_drain_external_owners_for_test();

        assert!(
            status_weak.upgrade().is_some(),
            "a terminal task result pointing to its own opaque handle is conservatively retained"
        );
    }

    #[test]
    fn task_requests_reject_an_artificial_foreign_runtime_handle_before_dispatch() {
        let context = EvalContext::standalone();
        let values = context.values().clone();
        let public_values = Values::from_core_factory(values.clone());
        let task = context
            .schedule_task(|task_context| {
                let unit = task_context
                    .values()
                    .with_boundary_access(|access| access.unit());
                Ok(Box::new(CompleteTask(unit)))
            })
            .expect("foreign-runtime task-handle fixture should schedule");
        let mut store = crate::reflection::ReflectionStore::new(
            values.clone(),
            Arc::new(crate::reflection::ExactConflictAnalysis),
        );
        let status = {
            let mut journal = StoreJournal::new(store.snapshot());
            let status = journal
                .reserve_query_with(task_status_public_value(
                    &public_values,
                    EvaluationTaskStatus::Launched,
                ))
                .expect("foreign-runtime task status query should reserve");
            assert!(matches!(
                store.try_commit(&journal),
                crate::reflection::StoreCommitResult::Committed
            ));
            status
        };
        let handle = TaskHandleCell {
            runtime: crate::runtime::allocate_evaluation_runtime_id(),
            task,
            status,
        };

        let error = ensure_runtime_task(&context, &handle)
            .expect_err("an artificial foreign-runtime task handle must be rejected");
        assert_eq!(
            error.to_string(),
            "task handle belongs to a different evaluation runtime"
        );
    }
}
