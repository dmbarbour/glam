use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use crate::api::{Diagnostic, Value};
use crate::core::{Atom, Dict, Key, OpaqueValue, Value as CoreValue, keys};
use crate::diagnostic::Severity;
use crate::eval;
use crate::evaluation::{
    EvalContext, EvaluationQueryCompletion, EvaluationTaskCancellation, EvaluationTaskHandle,
    EvaluationTaskId, EvaluationTaskPoll, PendingReflectionTask,
};
use crate::number::Number;

use super::{
    CommitResult, EffectRequestSpec, EvaluationQueryHandle, EvaluationQueryPoll,
    EvaluationQueryState, RequestContext, RequestResult, StoreJournal, TaskCommit, TaskEnvironment,
    TaskError, TaskHost, TaskSpecialization, decode_query_state, evaluate, get_value_path,
};

/// Requests shared by every full reflection task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectionRequest {
    Environment,
    DictItems,
    Eval,
    Log,
    ReflTask,
    JoinTask,
    TaskResult,
    TaskError,
    QueryTask,
    QueryResult,
    CancelTask,
}

#[derive(Clone)]
enum ReflectionUpdate {
    Launch(PendingReflectionTask),
    Cancel(EvalContext, EvaluationTaskId),
    Query {
        context: EvalContext,
        task: EvaluationTaskId,
        query: PendingQuery,
    },
}

#[derive(Clone)]
struct PendingQuery {
    inner: Arc<PendingQueryInner>,
}

struct PendingQueryInner {
    completion: Arc<dyn EvaluationQueryCompletion>,
    activated: AtomicBool,
}

impl PendingQuery {
    fn new<H>(host: Arc<H>, handle: Arc<EvaluationQueryHandle>) -> Self
    where
        H: ReflectionServices + ?Sized + 'static,
    {
        let completion = Arc::new(QueryCompletion {
            host,
            handle: handle.clone(),
        });
        Self {
            inner: Arc::new(PendingQueryInner {
                completion,
                activated: AtomicBool::new(false),
            }),
        }
    }

    fn activate(&self, context: &EvalContext, task: EvaluationTaskId) {
        if self.inner.activated.swap(true, Ordering::AcqRel) {
            return;
        }
        context.schedule_query(task, self.inner.completion.clone());
    }
}

struct QueryCompletion<H: ?Sized> {
    host: Arc<H>,
    handle: Arc<EvaluationQueryHandle>,
}

impl<H> EvaluationQueryCompletion for QueryCompletion<H>
where
    H: ReflectionServices + ?Sized,
{
    fn complete(&self, result: EvaluationTaskPoll) {
        self.host
            .complete_query(&self.handle, Value::from_core(task_query_snapshot(result)));
    }
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
        for update in &self.updates {
            match update {
                ReflectionUpdate::Launch(task) => task.activate(),
                ReflectionUpdate::Cancel(context, task) => {
                    context.cancel_reflection_task_id(*task);
                }
                ReflectionUpdate::Query {
                    context,
                    task,
                    query,
                } => query.activate(context, *task),
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

    #[doc(hidden)]
    fn complete_query(&self, handle: &Arc<EvaluationQueryHandle>, result: Value);
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
    vec![
        EffectRequestSpec::new(
            "env",
            ["reflection_runtime", "v0", "request", "env"],
            1,
            ReflectionRequest::Environment,
        ),
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
        EffectRequestSpec::new(
            "log",
            ["reflection_runtime", "v0", "request", "log"],
            2,
            ReflectionRequest::Log,
        ),
        EffectRequestSpec::new(
            "refl_task",
            ["reflection_runtime", "v0", "request", "refl_task"],
            1,
            ReflectionRequest::ReflTask,
        ),
        EffectRequestSpec::new(
            "join_task",
            ["reflection_runtime", "v0", "request", "join_task"],
            1,
            ReflectionRequest::JoinTask,
        ),
        EffectRequestSpec::new(
            "task_result",
            ["reflection_runtime", "v0", "request", "task_result"],
            1,
            ReflectionRequest::TaskResult,
        ),
        EffectRequestSpec::new(
            "task_error",
            ["reflection_runtime", "v0", "request", "task_error"],
            1,
            ReflectionRequest::TaskError,
        ),
        EffectRequestSpec::new(
            "query_task",
            ["reflection_runtime", "v0", "request", "query_task"],
            1,
            ReflectionRequest::QueryTask,
        ),
        EffectRequestSpec::new(
            "query_result",
            ["reflection_runtime", "v0", "request", "query_result"],
            1,
            ReflectionRequest::QueryResult,
        ),
        EffectRequestSpec::new(
            "cancel_task",
            ["reflection_runtime", "v0", "request", "cancel_task"],
            1,
            ReflectionRequest::CancelTask,
        ),
    ]
}

/// Handles one reusable reflection request inside a composed task.
pub fn handle_reflection_request<S>(
    request: ReflectionRequest,
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, S>,
) -> Result<RequestResult, TaskError>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    match request {
        ReflectionRequest::Environment => {
            let [path]: [Value; 1] = arguments
                .try_into()
                .map_err(|_| TaskError::new("`.env` received the wrong number of arguments"))?;
            let path = eval::eval_key_path_list(context.eval_context(), path.as_core())
                .map_err(|error| TaskError::new(error.to_string()))?;
            let environment = context.host().reflection_environment().into_core();
            let value = get_value_path(context.eval_context(), &environment, &path)?;
            Ok(RequestResult::Return(Value::from_core(value)))
        }
        ReflectionRequest::DictItems => {
            let [dict]: [Value; 1] = arguments.try_into().map_err(|_| {
                TaskError::new("`.dict_items` received the wrong number of arguments")
            })?;
            let CoreValue::Dict(dict) = evaluate(context.eval_context(), dict.into_core())? else {
                return Err(TaskError::new("`.dict_items` requires a dictionary"));
            };
            Ok(RequestResult::Return(Value::from_core(CoreValue::List(
                crate::core::List::from_values(
                    dict.iter()
                        .map(|(key, value)| {
                            CoreValue::Dict(
                                Dict::new_sync()
                                    .insert((*keys::KEY).clone(), key_value(key))
                                    .insert((*keys::VALUE).clone(), value.clone()),
                            )
                        })
                        .collect(),
                ),
            ))))
        }
        ReflectionRequest::Eval => evaluate_request(arguments, context.eval_context()),
        ReflectionRequest::Log => {
            let [severity, message]: [Value; 2] = arguments
                .try_into()
                .map_err(|_| TaskError::new("`.log` received the wrong number of arguments"))?;
            let message = prepare_message(context.eval_context(), message)?;
            let diagnostic = Diagnostic::from_emission(
                parse_severity(context.eval_context(), severity)?,
                message,
            );
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
            Ok(RequestResult::ReturnUnit)
        }
        ReflectionRequest::ReflTask => {
            let [effect]: [Value; 1] = arguments.try_into().map_err(|_| {
                TaskError::new("`.refl_task` received the wrong number of arguments")
            })?;
            let eval_context = context.eval_context().clone();
            let handle = if let Some(mut transaction) = context.transaction() {
                let pending = eval_context
                    .reserve_reflection_task(effect.into_core())
                    .map_err(|error| TaskError::new(error.as_ref()))?;
                let handle = pending.handle().clone();
                transaction
                    .parts()
                    .1
                    .reflection_journal()
                    .updates
                    .push(ReflectionUpdate::Launch(pending));
                handle
            } else {
                let handle = eval_context
                    .start_joinable_reflection_task(effect.into_core())
                    .map_err(|error| TaskError::new(error.as_ref()))?;
                context.committed();
                handle
            };
            Ok(RequestResult::Return(task_handle_value(&handle)))
        }
        ReflectionRequest::JoinTask => {
            let handle = task_handle_argument(context.eval_context(), arguments, "join_task")?;
            match context.eval_context().poll_reflection_task_id(handle) {
                EvaluationTaskPoll::Pending(wait) => {
                    context.observe_task_wait(wait);
                    Ok(RequestResult::Fail)
                }
                EvaluationTaskPoll::Complete(value) => {
                    Ok(RequestResult::Return(Value::from_core(value)))
                }
                EvaluationTaskPoll::Failed(error) => Err(TaskError::new(error)),
                EvaluationTaskPoll::Cancelled => {
                    Err(TaskError::new("joined reflection task was cancelled"))
                }
                EvaluationTaskPoll::ForeignSession => Err(TaskError::new(
                    "task handle does not belong to this evaluation session",
                )),
            }
        }
        ReflectionRequest::TaskResult => {
            let handle = task_handle_argument(context.eval_context(), arguments, "task_result")?;
            match context.eval_context().poll_reflection_task_id(handle) {
                EvaluationTaskPoll::Pending(wait) => {
                    context.observe_task_wait(wait);
                    Ok(RequestResult::Fail)
                }
                EvaluationTaskPoll::Complete(value) => {
                    Ok(RequestResult::Return(Value::from_core(value)))
                }
                EvaluationTaskPoll::Failed(_) | EvaluationTaskPoll::Cancelled => {
                    Ok(RequestResult::Fail)
                }
                EvaluationTaskPoll::ForeignSession => Err(TaskError::new(
                    "task handle does not belong to this evaluation session",
                )),
            }
        }
        ReflectionRequest::TaskError => {
            let handle = task_handle_argument(context.eval_context(), arguments, "task_error")?;
            match context.eval_context().poll_reflection_task_id(handle) {
                EvaluationTaskPoll::Pending(wait) => {
                    context.observe_task_wait(wait);
                    Ok(RequestResult::Fail)
                }
                EvaluationTaskPoll::Failed(error) => {
                    Ok(RequestResult::Return(Value::text(error.as_ref())))
                }
                EvaluationTaskPoll::Complete(_) | EvaluationTaskPoll::Cancelled => {
                    Ok(RequestResult::Fail)
                }
                EvaluationTaskPoll::ForeignSession => Err(TaskError::new(
                    "task handle does not belong to this evaluation session",
                )),
            }
        }
        ReflectionRequest::QueryTask => {
            let task = task_handle_argument(context.eval_context(), arguments, "query_task")?;
            let eval_context = context.eval_context().clone();
            let host = context.host_arc();
            let handle;
            if let Some(mut transaction) = context.transaction() {
                handle = transaction
                    .store()
                    .reserve_query()
                    .map_err(|error| TaskError::new(error.as_ref()))?;
                let query = PendingQuery::new(host, handle.clone());
                transaction
                    .parts()
                    .1
                    .reflection_journal()
                    .updates
                    .push(ReflectionUpdate::Query {
                        context: eval_context,
                        task,
                        query,
                    });
            } else {
                let snapshot = context.host().snapshot();
                let mut store = StoreJournal::new(snapshot.store().clone());
                handle = store
                    .reserve_query()
                    .map_err(|error| TaskError::new(error.as_ref()))?;
                let query = PendingQuery::new(host, handle.clone());
                let mut journal = S::Journal::default();
                journal
                    .reflection_journal()
                    .updates
                    .push(ReflectionUpdate::Query {
                        context: eval_context,
                        task,
                        query,
                    });
                match context.host().commit(TaskCommit::new(
                    store,
                    snapshot.extra().clone(),
                    journal,
                )) {
                    CommitResult::Committed => context.committed(),
                    CommitResult::Conflict => {
                        return Err(TaskError::new("fresh query reservation conflicted"));
                    }
                    CommitResult::MissingVolume(volume) => {
                        return Err(TaskError::new(format!(
                            "private query volume {} is unavailable",
                            volume.get()
                        )));
                    }
                    CommitResult::Closed => return Ok(RequestResult::Cancelled),
                }
            }
            Ok(RequestResult::Return(query_handle_value(&handle)))
        }
        ReflectionRequest::QueryResult => {
            let query = query_handle_argument(context.eval_context(), arguments, "query_result")?;
            let transaction_generation = context.transaction_generation();
            let (result, generation) = if let Some(mut transaction) = context.transaction() {
                let generation = transaction_generation
                    .expect("active transaction must have a snapshot generation");
                (transaction.store().poll_query(&query), generation)
            } else {
                let snapshot = context.host().snapshot();
                (snapshot.store().poll_query(&query), snapshot.generation())
            };
            match result {
                EvaluationQueryPoll::State { value, observed } => {
                    let state = evaluate(context.eval_context(), value.into_core())?;
                    match decode_query_state(&state) {
                        Some(EvaluationQueryState::Pending) => {
                            if observed {
                                context.observe_host_generation(generation);
                            }
                            Ok(RequestResult::Fail)
                        }
                        Some(EvaluationQueryState::Complete(result)) => {
                            Ok(RequestResult::Return(result))
                        }
                        None => Err(TaskError::new("query handle has been retired")),
                    }
                }
                EvaluationQueryPoll::ForeignSession => Err(TaskError::new(
                    "query handle does not belong to this reasoning session",
                )),
            }
        }
        ReflectionRequest::CancelTask => {
            let handle = task_handle_argument(context.eval_context(), arguments, "cancel_task")?;
            let eval_context = context.eval_context().clone();
            if let Some(mut transaction) = context.transaction() {
                transaction
                    .parts()
                    .1
                    .reflection_journal()
                    .updates
                    .push(ReflectionUpdate::Cancel(eval_context, handle));
            } else {
                match eval_context.cancel_reflection_task_id(handle) {
                    EvaluationTaskCancellation::Requested => context.committed(),
                    EvaluationTaskCancellation::Late
                    | EvaluationTaskCancellation::ForeignSession => {}
                }
            }
            Ok(RequestResult::ReturnUnit)
        }
    }
}

fn evaluate_request(
    arguments: Vec<Value>,
    context: &EvalContext,
) -> Result<RequestResult, TaskError> {
    let [value]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.eval` received the wrong number of arguments"))?;
    let mut value = value.into_core();
    while matches!(value, CoreValue::Lazy(_)) {
        value = match eval::eval_value(context, &value) {
            Ok(value) => value,
            Err(error) => {
                if let Some(wait) = error.blocked_on() {
                    return Err(TaskError::retry_after(wait.0));
                }
                return Ok(RequestResult::Return(tagged_result(
                    &keys::ERR,
                    Value::text(error.to_string()),
                )));
            }
        };
    }
    Ok(RequestResult::Return(tagged_result(
        &keys::OK,
        Value::from_core(value),
    )))
}

fn tagged_result(tag: &Key, value: Value) -> Value {
    Value::from_core(CoreValue::Dict(
        Dict::new_sync().insert(tag.clone(), value.into_core()),
    ))
}

static TASK_HANDLE_TAG: LazyLock<Key> = LazyLock::new(|| {
    Key::abstract_global_path(["reflection_runtime", "v0", "value", "task_handle"])
});

fn task_handle_value(handle: &EvaluationTaskHandle) -> Value {
    Value::from_core(task_handle_core_value(handle))
}

fn task_handle_core_value(handle: &EvaluationTaskHandle) -> CoreValue {
    CoreValue::Dict(Dict::new_sync().insert(
        TASK_HANDLE_TAG.clone(),
        CoreValue::Number(Number::from_u64(handle.id().get())),
    ))
}

fn query_handle_value(handle: &Arc<EvaluationQueryHandle>) -> Value {
    Value::from_core(CoreValue::Opaque(OpaqueValue::new(handle.clone())))
}

fn task_query_snapshot(task: EvaluationTaskPoll) -> CoreValue {
    let (tag, value) = match task {
        EvaluationTaskPoll::Pending(_) => (&*keys::PENDING, (*keys::UNIT_VALUE).clone()),
        EvaluationTaskPoll::Complete(value) => (&*keys::COMPLETE, value),
        EvaluationTaskPoll::Failed(error) => {
            (&*keys::ERROR, CoreValue::binary_from_text(error.as_ref()))
        }
        EvaluationTaskPoll::Cancelled => (&*keys::CANCELED, (*keys::UNIT_VALUE).clone()),
        EvaluationTaskPoll::ForeignSession => (&*keys::FOREIGN, (*keys::UNIT_VALUE).clone()),
    };
    CoreValue::Dict(Dict::new_sync().insert(tag.clone(), value))
}

fn key_value(key: &Key) -> CoreValue {
    match key {
        Key::Atom(atom) => CoreValue::Atom(*atom),
        Key::Number(number) => CoreValue::Number(number.clone()),
        Key::Binary(bytes) => CoreValue::Binary(bytes.clone()),
        Key::AbstractGlobalPath(parts) => {
            CoreValue::Atom(Atom::from_key(&Key::AbstractGlobalPath(parts.clone())))
        }
        Key::List(items) => CoreValue::List(crate::core::List::from_values(
            items.iter().map(key_value).collect(),
        )),
        Key::Dict(entries) => {
            CoreValue::Dict(entries.iter().fold(Dict::new_sync(), |dict, (key, value)| {
                dict.insert(key.clone(), key_value(value))
            }))
        }
    }
}

fn task_handle_argument(
    context: &EvalContext,
    arguments: Vec<Value>,
    request: &str,
) -> Result<EvaluationTaskId, TaskError> {
    let [handle]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskError::new(format!(
            "`.{request}` received the wrong number of arguments"
        ))
    })?;
    let CoreValue::Dict(handle) = evaluate(context, handle.into_core())? else {
        return Err(TaskError::new(format!(
            "`.{request}` requires a reflection task handle"
        )));
    };
    if handle.iter().count() != 1 {
        return Err(TaskError::new(format!(
            "`.{request}` requires a reflection task handle"
        )));
    }
    let Some(CoreValue::Number(id)) = handle.get(&TASK_HANDLE_TAG) else {
        return Err(TaskError::new(format!(
            "`.{request}` requires a reflection task handle"
        )));
    };
    id.to_u64_if_integer()
        .and_then(EvaluationTaskId::from_u64)
        .ok_or_else(|| TaskError::new(format!("`.{request}` received an invalid task handle")))
}

fn query_handle_argument(
    context: &EvalContext,
    arguments: Vec<Value>,
    request: &str,
) -> Result<Arc<EvaluationQueryHandle>, TaskError> {
    let [handle]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskError::new(format!(
            "`.{request}` received the wrong number of arguments"
        ))
    })?;
    let CoreValue::Opaque(handle) = evaluate(context, handle.into_core())? else {
        return Err(TaskError::new(format!(
            "`.{request}` requires a reflection query handle"
        )));
    };
    handle
        .downcast::<EvaluationQueryHandle>()
        .ok_or_else(|| TaskError::new(format!("`.{request}` requires a reflection query handle")))
}

fn prepare_message(context: &EvalContext, message: Value) -> Result<Value, TaskError> {
    let CoreValue::Dict(mut message) = evaluate(context, message.into_core())? else {
        return Err(TaskError::new("`.log` message must evaluate to an object"));
    };
    if let Some(interface) = message.get(&*keys::MSG) {
        message = message.insert((*keys::MSG).clone(), evaluate(context, interface.clone())?);
    }
    Ok(Value::from_core(CoreValue::Dict(message)))
}

fn parse_severity(context: &EvalContext, value: Value) -> Result<Severity, TaskError> {
    let value = evaluate(context, value.into_core())?;
    if severity_matches(&value, "info", &keys::INFO_VALUE) {
        Ok(Severity::Info)
    } else if severity_matches(&value, "warn", &keys::WARN_VALUE) {
        Ok(Severity::Warning)
    } else if severity_matches(&value, "error", &keys::ERROR_VALUE) {
        Ok(Severity::Error)
    } else {
        Err(TaskError::new(
            "`.log` severity must be `'info`, `'warn`, or `'error`",
        ))
    }
}

fn severity_matches(value: &CoreValue, name: &str, canonical: &CoreValue) -> bool {
    value == canonical || value == &CoreValue::Atom(Atom::from_key(&Key::binary_from_text(name)))
}
