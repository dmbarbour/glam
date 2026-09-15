use std::collections::BTreeMap;
use std::sync::Arc;

use crate::api::{Diagnostic, Value, Values};
use crate::core::{Atom, CoreValueFactory, Dict, Key, OpaqueValue, Value as CoreValue, keys};
use crate::diagnostic::Severity;
use crate::evaluation::{
    EvalContext, EvaluationTaskCancellation, EvaluationTaskHandle, EvaluationTaskId,
    EvaluationTaskStatus, EvaluationWaitPoll, PendingReflectionTask, PendingTaskPolicy,
    TaskStatusPublisher, TaskStatusWake,
};
use crate::list::{ListFrontStep, ListItem};
use crate::number::Number;

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
    Eval,
    MetadataInspect,
    Log,
    TaskNew,
    TaskJoin,
    TaskStatus,
    TaskValue,
    TaskHalt,
    TaskAcknowledgeError,
    TaskCancel,
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
    Eval(EvalRequestWork),
    Inspection(InspectionRequestWork),
    Log(LogRequestWork),
    TaskCreate(Vec<Value>),
    TaskControl(TaskControlRequestWork),
    TaskJoin(TaskJoinRequestWork),
    TaskQuery(TaskQueryRequestWork),
    Poisoned,
}

enum EvalRequestWork {
    Start(Vec<Value>),
    Awaiting,
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
            ReflectionRequest::Eval => {
                ReflectionRequestOperation::Eval(EvalRequestWork::Start(arguments))
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
            ReflectionRequestOperation::Eval(EvalRequestWork::Start(arguments)) => {
                assert!(input.is_none(), "new `.eval` work cannot have demand input");
                let [value]: [Value; 1] = arguments
                    .try_into()
                    .map_err(|_| TaskHalt::new("`.eval` received the wrong number of arguments"))?;
                self.operation = ReflectionRequestOperation::Eval(EvalRequestWork::Awaiting);
                Ok(SpecializationRequestPoll::Demand(value))
            }
            ReflectionRequestOperation::Eval(EvalRequestWork::Awaiting) => {
                let input = input.expect("resumed `.eval` work must receive demand input");
                let result = match input {
                    SpecializationRequestInput::Value(value) => {
                        tagged_result(&context.values(), &keys::OK, value.into_value())
                    }
                    SpecializationRequestInput::Failed(error) => {
                        let failure = error.permanent_failure().expect(
                            "completed `.eval` demand failure must retain a permanent failure",
                        );
                        let values = context.values();
                        let diagnostic =
                            values.wrap(crate::diagnostic::failure_diagnostic_value_with(
                                context.eval_context().values(),
                                failure,
                            ));
                        tagged_result(&values, &keys::ERR, diagnostic)
                    }
                };
                Ok(SpecializationRequestPoll::Complete(RequestResult::Return(
                    result,
                )))
            }
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
                let message = contextual_demand_value(input, "log_message")?;
                let interface = message.with_core(|value| {
                    let CoreValue::Dict(message) = value else {
                        return Err(TaskHalt::new("`.log` message must evaluate to an object"));
                    };
                    Ok(message.get(&*keys::MSG).cloned())
                })??;
                if let Some(interface) = interface {
                    let interface = context.values().wrap(interface);
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
                let interface = contextual_demand_value(input, "log_message")?;
                let values = context.values();
                let message = message.with_core(|value| {
                    let CoreValue::Dict(message) = value else {
                        unreachable!("validated log message must remain a dictionary")
                    };
                    Ok::<_, TaskHalt>(values.wrap(CoreValue::Dict(message.insert(
                        (*keys::MSG).clone(),
                        values.clone_core(interface.as_value())?,
                    ))))
                })??;
                self.operation =
                    ReflectionRequestOperation::Log(LogRequestWork::Severity { message });
                Ok(SpecializationRequestPoll::Demand(severity))
            }
            ReflectionRequestOperation::Log(LogRequestWork::Severity { message }) => {
                let severity = contextual_demand_value(input, "log_severity")?;
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
                let values = context.values();
                let state = match state.with_core(|state| decode_query_state(&values, state))? {
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
            Err(TaskHalt::rooted_failure(error)
                .with_core_context(task_join_context(handle.task.id())))
        }
        EvaluationWaitPoll::Cancelled => Err(TaskHalt::new("joined reflection task was cancelled")),
        EvaluationWaitPoll::Abandoned => Err(TaskHalt::new(
            "joined reflection task was abandoned when its evaluation session closed",
        )
        .with_core_context(task_join_context(handle.task.id()))),
        EvaluationWaitPoll::Exited => Err(TaskHalt::new(
            "joined reflection task exited without producing a result",
        )
        .with_core_context(task_join_context(handle.task.id()))),
        EvaluationWaitPoll::Killed(error) => {
            Err(TaskHalt::rooted_failure(error)
                .with_core_context(task_join_context(handle.task.id())))
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
        (TaskQueryRequest::Halt, TaggedTaskState::Failed(error)) => {
            Ok(RequestResult::Return(error))
        }
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
            | TaggedTaskState::Killed,
        )
        | (
            TaskQueryRequest::Halt,
            TaggedTaskState::Complete(_)
            | TaggedTaskState::Abandoned
            | TaggedTaskState::Exited
            | TaggedTaskState::Killed,
        ) => Ok(RequestResult::Fail),
        (TaskQueryRequest::Status, _) => unreachable!("status returns before tagged decoding"),
    }
}

fn evaluated_task_handle<S: TaskSpecialization>(
    context: &RequestContext<'_, S>,
    handle: &crate::api::EvaluatedValue,
    request: &str,
) -> Result<Arc<TaskHandleCell>, TaskHalt> {
    handle.with_core(|value| {
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
    let effect = values.clone_core(&effect)?;
    let launched = values.wrap(task_status_query_value(
        &values,
        EvaluationTaskStatus::Launched,
    ));
    let handle = if let Some(mut transaction) = context.transaction() {
        let result = transaction
            .store()
            .reserve_query_with(launched.clone())
            .map_err(|error| TaskHalt::new(error.as_ref()))?;
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
        let selected = current.with_core(|value| {
            let CoreValue::Dict(dict) = value else {
                return Err(TaskHalt::new("state path traverses a non-dictionary value"));
            };
            Ok(dict
                .get(key)
                .cloned()
                .unwrap_or_else(|| CoreValue::Dict(Dict::new_sync())))
        })??;
        self.current = context.values().wrap(selected);
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
                        let values = context.values();
                        self.state =
                            KeyConversionRequestState::Dict(Box::new(DictKeyRequestWork {
                                members: members
                                    .into_iter()
                                    .map(|(key, value)| (key, values.wrap(value)))
                                    .collect(),
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
    Dict(Vec<(Key, CoreValue)>),
    Invalid,
}

fn classify_key_value(
    value: &crate::api::EvaluatedValue,
) -> Result<ClassifiedRequestKey, TaskHalt> {
    value
        .with_core(|value| match value {
            CoreValue::Atom(atom) => ClassifiedRequestKey::Ready(Key::Atom(*atom)),
            CoreValue::Number(number) => ClassifiedRequestKey::Ready(Key::Number(number.clone())),
            CoreValue::Binary(bytes) => ClassifiedRequestKey::Ready(Key::Binary(bytes.clone())),
            CoreValue::List(_) => ClassifiedRequestKey::List,
            CoreValue::Dict(dict) => ClassifiedRequestKey::Dict(
                dict.iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            ),
            CoreValue::Builtin(_)
            | CoreValue::PartialBuiltin(_)
            | CoreValue::Function(_)
            | CoreValue::Net(_)
            | CoreValue::Lazy(_)
            | CoreValue::Promised(_)
            | CoreValue::Metadata(_)
            | CoreValue::Opaque(_) => ClassifiedRequestKey::Invalid,
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
                    if !matches!(&value, Key::Dict(entries) if entries.is_empty()) {
                        self.converted.push((key.clone(), value));
                    }
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
                    let is_list = value.with_core(|value| matches!(value, CoreValue::List(_)))?;
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
            list.pop_front_step_by(&mut |value| access.duplicate_value(value), &mut |thunk| {
                thunk.duplicate_as_value_in(access)
            })
        })?;
        match step {
            ListFrontStep::Empty => Ok(PreparationPoll::Progress),
            ListFrontStep::Item { item, tail } => {
                let tail = context.values().wrap(CoreValue::List(tail));
                self.lists.push(crate::api::EvaluatedValue::from_whnf(
                    &context.values(),
                    tail,
                ));
                match item {
                    ListItem::Byte(byte) => self.converted.push(Key::Number(Number::from_u8(byte))),
                    ListItem::Value(value) => {
                        self.child = Some(Box::new(KeyConversionRequestWork::new(
                            context.values().wrap(value),
                        )));
                    }
                }
                Ok(PreparationPoll::Progress)
            }
            ListFrontStep::Deferred { deferred, suffix } => {
                let suffix = context.values().wrap(CoreValue::List(suffix));
                self.lists.push(crate::api::EvaluatedValue::from_whnf(
                    &context.values(),
                    suffix,
                ));
                self.pending = Some(ListDemand::Start(context.values().wrap(deferred)));
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

fn contextual_demand_value(
    input: Option<SpecializationRequestInput>,
    operation: &str,
) -> Result<crate::api::EvaluatedValue, TaskHalt> {
    demand_value(input, "resumed log preparation").map_err(|error| {
        error.with_core_context(crate::diagnostic::evaluation_context_frame(operation))
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
    let (info, warn, error) = value.with_core(|value| {
        (
            severity_matches(value, "info", &keys::INFO),
            severity_matches(value, "warn", &keys::WARN),
            severity_matches(value, "error", &keys::ERROR),
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
    let values = context.values();
    let items = dict.with_core(|value| {
        let CoreValue::Dict(dict) = value else {
            return Err(TaskHalt::new("`.dict_items` requires a dictionary"));
        };
        Ok(CoreValue::List(crate::core::List::from_values(
            dict.iter()
                .map(|(key, value)| {
                    CoreValue::Dict(
                        Dict::new_sync()
                            .insert((*keys::KEY).clone(), key.to_value_with(values.core()))
                            .insert((*keys::VALUE).clone(), value.clone()),
                    )
                })
                .collect(),
        )))
    })??;
    Ok(RequestResult::Return(values.wrap(items)))
}

fn inspect_metadata<S: TaskSpecialization>(
    context: &RequestContext<'_, S>,
    value: &crate::api::EvaluatedValue,
) -> Result<RequestResult, TaskHalt> {
    let Some(metadata) = value.with_core(CoreValue::associated_metadata)? else {
        return Ok(RequestResult::Fail);
    };
    Ok(RequestResult::Return(context.values().wrap(metadata)))
}

#[derive(Clone)]
enum ReflectionUpdate {
    Launch {
        task: PendingReflectionTask,
        publisher: TaskStatusPublisher,
    },
    Cancel(EvaluationTaskHandle),
    AcknowledgeError(EvaluationTaskHandle),
}

/// Transactional writes and deferred observations for reflection requests.
#[derive(Clone, Default)]
pub struct ReflectionJournal {
    diagnostics: Vec<Diagnostic>,
    updates: Vec<ReflectionUpdate>,
}

impl ReflectionJournal {
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
                ReflectionUpdate::Launch { .. } => {}
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
}

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
            EffectRequestSpec::new(
                "eval",
                ["reflection_runtime", "v0", "request", "eval"],
                1,
                ReflectionRequest::Eval,
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
fn tagged_result(values: &Values, tag: &Key, value: Value) -> Value {
    values.wrap(CoreValue::Dict(
        Dict::new_sync().insert(
            tag.clone(),
            values
                .clone_core(&value)
                .expect("tagged result belongs to its request runtime"),
        ),
    ))
}

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
    values.wrap(CoreValue::Opaque(OpaqueValue::new(values.core(), handle)))
}

fn task_join_context(task: EvaluationTaskId) -> CoreValue {
    let operation = CoreValue::Atom(Atom::from_key(&Key::binary_from_text("join")));
    let detail = Dict::new_sync()
        .insert(Key::atom_from_text("operation"), operation)
        .insert(
            Key::atom_from_text("id"),
            CoreValue::Number(Number::from_u64(task.get())),
        );
    CoreValue::Dict(Dict::new_sync().insert(Key::atom_from_text("task"), CoreValue::Dict(detail)))
}

fn task_status_query_value(values: &Values, status: EvaluationTaskStatus) -> CoreValue {
    match status {
        EvaluationTaskStatus::Launched => values.core().key_value(&keys::LAUNCHED),
        EvaluationTaskStatus::Blocked => values.core().key_value(&keys::BLOCKED),
        EvaluationTaskStatus::Complete(value) => CoreValue::Dict(
            Dict::new_sync().insert(
                (*keys::OK).clone(),
                values
                    .clone_runtime_root(&value)
                    .expect("completed task value belongs to its query runtime"),
            ),
        ),
        EvaluationTaskStatus::Failed(error) => CoreValue::Dict(Dict::new_sync().insert(
            (*keys::ERR).clone(),
            crate::diagnostic::failure_diagnostic_value_with(values.core(), error.as_failure()),
        )),
        EvaluationTaskStatus::Cancelled => values.core().key_value(&keys::CANCELED),
        EvaluationTaskStatus::Abandoned => values.core().key_value(&keys::ABANDONED),
        EvaluationTaskStatus::Exited => values.core().key_value(&keys::EXITED),
        EvaluationTaskStatus::Killed(_) => values.core().key_value(&keys::KILLED),
    }
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
            values.wrap(task_status_query_value(&values, status)),
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
    Killed,
}

fn tagged_task_state(values: &Values, value: &Value) -> Result<TaggedTaskState, TaskHalt> {
    let value = values.clone_core(value)?;
    if value == values.core().key_value(&keys::LAUNCHED) {
        return Ok(TaggedTaskState::Launched);
    }
    if value == values.core().key_value(&keys::BLOCKED) {
        return Ok(TaggedTaskState::Blocked);
    }
    if value == values.core().key_value(&keys::CANCELED) {
        return Ok(TaggedTaskState::Cancelled);
    }
    if value == values.core().key_value(&keys::ABANDONED) {
        return Ok(TaggedTaskState::Abandoned);
    }
    if value == values.core().key_value(&keys::EXITED) {
        return Ok(TaggedTaskState::Exited);
    }
    if value == values.core().key_value(&keys::KILLED) {
        return Ok(TaggedTaskState::Killed);
    }
    let CoreValue::Dict(state) = value else {
        return Err(TaskHalt::new("reflection task status is malformed"));
    };
    if state.iter().count() != 1 {
        return Err(TaskHalt::new("reflection task status is malformed"));
    }
    if let Some(value) = state.get(&*keys::OK) {
        return Ok(TaggedTaskState::Complete(values.wrap(value.clone())));
    }
    if let Some(error) = state.get(&*keys::ERR) {
        return Ok(TaggedTaskState::Failed(values.wrap(error.clone())));
    }
    Err(TaskHalt::new("reflection task status is malformed"))
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

fn severity_matches(value: &CoreValue, name: &str, canonical: &Key) -> bool {
    Key::from_value(value).as_ref() == Some(canonical)
        || value == &CoreValue::Atom(Atom::from_key(&Key::binary_from_text(name)))
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
            | ReflectionRequest::Eval
            | ReflectionRequest::MetadataInspect
            | ReflectionRequest::Log
            | ReflectionRequest::TaskNew
            | ReflectionRequest::TaskJoin
            | ReflectionRequest::TaskStatus
            | ReflectionRequest::TaskValue
            | ReflectionRequest::TaskHalt
            | ReflectionRequest::TaskAcknowledgeError
            | ReflectionRequest::TaskCancel => {}
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
        }
        let ReflectionJournal {
            diagnostics,
            updates,
        } = journal;
        let _: &Vec<Diagnostic> = diagnostics;
        let _: &Vec<ReflectionUpdate> = updates;
    }

    fn assert_tagged_task_state_inventory(state: &TaggedTaskState) {
        match state {
            TaggedTaskState::Complete(value) | TaggedTaskState::Failed(value) => {
                let _: &Value = value;
            }
            TaggedTaskState::Launched
            | TaggedTaskState::Blocked
            | TaggedTaskState::Cancelled
            | TaggedTaskState::Abandoned
            | TaggedTaskState::Exited
            | TaggedTaskState::Killed => {}
        }
    }

    fn assert_reflection_work_inventory(work: &ReflectionRequestWork) {
        match &work.operation {
            ReflectionRequestOperation::Environment(_)
            | ReflectionRequestOperation::Eval(_)
            | ReflectionRequestOperation::Inspection(_)
            | ReflectionRequestOperation::Log(_)
            | ReflectionRequestOperation::TaskCreate(_)
            | ReflectionRequestOperation::TaskControl(_)
            | ReflectionRequestOperation::TaskJoin(_)
            | ReflectionRequestOperation::TaskQuery(_)
            | ReflectionRequestOperation::Poisoned => {}
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
        let core = crate::core::test_value_factory();
        let values = Values::from_core_factory(core);
        let domain = EffectTokenDomain::new(&values);

        let (emission, retained) = retained_request_value(&domain);
        let journal = ReflectionJournal {
            diagnostics: vec![
                Diagnostic::from_emission(&values, Severity::Error, emission)
                    .expect("retained request fixture uses one runtime"),
            ],
            updates: Vec::new(),
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

        let (value, retained) = retained_request_value(&domain);
        let work = ReflectionRequestWork::new(ReflectionRequest::Eval, vec![value]);
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained.upgrade().is_some());
        drop(work);
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained.upgrade().is_none());
    }

    impl EvaluationTaskMachine for CompleteTask {
        fn poll(
            &mut self,
            _context: &crate::evaluation::EvaluationPollContext,
            _step_budget: usize,
        ) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Complete(_context.root_value(self.0.clone()))
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
        let encoded = values.wrap(task_status_query_value(
            &values,
            EvaluationTaskStatus::Abandoned,
        ));

        assert_eq!(
            values.clone_core(&encoded).unwrap(),
            values.core().key_value(&keys::ABANDONED)
        );
        assert!(matches!(
            tagged_task_state(&values, &encoded).expect("abandoned status should decode"),
            TaggedTaskState::Abandoned
        ));
    }

    #[test]
    fn exited_and_killed_task_statuses_have_distinct_round_trips() {
        let core = crate::core::test_value_factory();
        let values = Values::from_core_factory(core.clone());
        let exited = values.wrap(task_status_query_value(
            &values,
            EvaluationTaskStatus::Exited,
        ));
        assert_eq!(
            values.clone_core(&exited).unwrap(),
            values.core().key_value(&keys::EXITED)
        );
        assert!(matches!(
            tagged_task_state(&values, &exited).expect("exited status should decode"),
            TaggedTaskState::Exited
        ));

        let killed = values.wrap(task_status_query_value(
            &values,
            EvaluationTaskStatus::Killed(crate::runtime::RuntimeFailureRoot::new(
                &core,
                Arc::new(crate::core::EvaluationFailure::message("killed fixture")),
            )),
        ));
        assert_eq!(
            values.clone_core(&killed).unwrap(),
            values.core().key_value(&keys::KILLED)
        );
        assert!(matches!(
            tagged_task_state(&values, &killed).expect("killed status should decode"),
            TaggedTaskState::Killed
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
                .reserve_query_with(public_values.wrap(task_status_query_value(
                    &public_values,
                    EvaluationTaskStatus::Launched,
                )))
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
        assert_eq!(
            public_values.clone_core(&updates[0]).unwrap(),
            values.key_value(&keys::BLOCKED)
        );
    }

    #[test]
    fn terminal_task_handle_cell_releases_the_final_query_lease() {
        let context = EvalContext::standalone();
        let values = context.values().clone();
        let public_values = Values::from_core_factory(values.clone());
        let task = context
            .schedule_task(|task_context| Ok(Box::new(CompleteTask(task_context.values().unit()))))
            .expect("terminal task-handle fixture should schedule");
        let store = Arc::new(Mutex::new(crate::reflection::ReflectionStore::new(
            values.clone(),
            Arc::new(crate::reflection::ExactConflictAnalysis),
        )));
        let status = {
            let mut store = store.lock().expect("test query store was poisoned");
            let mut journal = StoreJournal::new(store.snapshot());
            let status = journal
                .reserve_query_with(public_values.wrap(task_status_query_value(
                    &public_values,
                    EvaluationTaskStatus::Launched,
                )))
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
        let context = EvalContext::standalone();
        let values = context.values().clone();
        let public_values = Values::from_core_factory(values.clone());
        let task = context
            .schedule_task(|task_context| Ok(Box::new(CompleteTask(task_context.values().unit()))))
            .expect("task-handle backedge fixture should schedule");
        let status = {
            let mut store = crate::reflection::ReflectionStore::new(
                values.clone(),
                Arc::new(crate::reflection::ExactConflictAnalysis),
            );
            let mut journal = StoreJournal::new(store.snapshot());
            let status = journal
                .reserve_query_with(public_values.wrap(task_status_query_value(
                    &public_values,
                    EvaluationTaskStatus::Launched,
                )))
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
            .schedule_task(|task_context| Ok(Box::new(CompleteTask(task_context.values().unit()))))
            .expect("foreign-runtime task-handle fixture should schedule");
        let mut store = crate::reflection::ReflectionStore::new(
            values.clone(),
            Arc::new(crate::reflection::ExactConflictAnalysis),
        );
        let status = {
            let mut journal = StoreJournal::new(store.snapshot());
            let status = journal
                .reserve_query_with(public_values.wrap(task_status_query_value(
                    &public_values,
                    EvaluationTaskStatus::Launched,
                )))
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
