use std::collections::BTreeMap;
use std::sync::Arc;

use crate::api::{Diagnostic, Value};
use crate::core::{Atom, CoreValueFactory, Dict, Key, OpaqueValue, Value as CoreValue, keys};
use crate::diagnostic::Severity;
use crate::eval;
use crate::evaluation::{
    EvalContext, EvaluationTaskCancellation, EvaluationTaskHandle, EvaluationTaskId,
    EvaluationTaskStatus, EvaluationTaskStatusSink, EvaluationWaitPoll, PendingReflectionTask,
    PendingTaskPolicy,
};
use crate::number::Number;

use super::{
    CommitResult, EffectRequestSpec, EvaluationQueryHandle, EvaluationQueryPoll,
    EvaluationQueryState, RequestContext, RequestResult, StoreJournal, TaskCommit, TaskEnvironment,
    TaskHalt, TaskHost, TaskSpecialization, decode_query_state, evaluate, get_value_path,
    task_eval_error,
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

#[derive(Clone)]
enum ReflectionUpdate {
    Launch {
        task: PendingReflectionTask,
        publisher: Arc<dyn EvaluationTaskStatusSink>,
    },
    Cancel(EvalContext, EvaluationTaskHandle),
    AcknowledgeError(EvalContext, EvaluationTaskHandle),
}

struct TaskStatusPublisher {
    writer: Arc<dyn ReflectionQueryWriter>,
    handle: Arc<EvaluationQueryHandle>,
    values: CoreValueFactory,
}

impl EvaluationTaskStatusSink for TaskStatusPublisher {
    fn update(&self, status: EvaluationTaskStatus) {
        self.writer.update_query(
            &self.handle,
            Value::from_core(&self.values, task_status_query_value(&self.values, status)),
        );
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
                ReflectionUpdate::Cancel(_, task) => {
                    if let Some(policy) = pending_policies.get_mut(&task.id()) {
                        policy.cancel();
                    }
                }
                ReflectionUpdate::AcknowledgeError(_, task) => {
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
                ReflectionUpdate::Cancel(context, task) => {
                    if !pending_policies.contains_key(&task.id()) {
                        context.cancel_reflection_task(task);
                    }
                }
                ReflectionUpdate::AcknowledgeError(context, task) => {
                    if !pending_policies.contains_key(&task.id()) {
                        context.acknowledge_reflection_task_error(task);
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
    fn update_query(&self, handle: &Arc<EvaluationQueryHandle>, result: Value);
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
    environment_log_request_specs()
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

pub(crate) fn environment_log_request_specs() -> Vec<EffectRequestSpec<ReflectionRequest>> {
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
pub fn handle_reflection_request<S>(
    request: ReflectionRequest,
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, S>,
) -> Result<RequestResult, TaskHalt>
where
    S: TaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    match request {
        ReflectionRequest::Environment => {
            let [path]: [Value; 1] = arguments
                .try_into()
                .map_err(|_| TaskHalt::new("`.env` received the wrong number of arguments"))?;
            let path = eval::eval_key_path_list(context.eval_context(), path.as_core())
                .map_err(task_eval_error)?;
            let environment = context.host().reflection_environment().into_core();
            let value = get_value_path(context.eval_context(), &environment, &path)?;
            Ok(RequestResult::Return(Value::from_core(
                context.eval_context().values(),
                value,
            )))
        }
        ReflectionRequest::DictItems => {
            let [dict]: [Value; 1] = arguments.try_into().map_err(|_| {
                TaskHalt::new("`.dict_items` received the wrong number of arguments")
            })?;
            let CoreValue::Dict(dict) = evaluate(context.eval_context(), dict.into_core())? else {
                return Err(TaskHalt::new("`.dict_items` requires a dictionary"));
            };
            Ok(RequestResult::Return(Value::from_core(
                context.eval_context().values(),
                CoreValue::List(crate::core::List::from_values(
                    dict.iter()
                        .map(|(key, value)| {
                            CoreValue::Dict(
                                Dict::new_sync()
                                    .insert(
                                        (*keys::KEY).clone(),
                                        key.to_value_with(context.eval_context().values()),
                                    )
                                    .insert((*keys::VALUE).clone(), value.clone()),
                            )
                        })
                        .collect(),
                )),
            )))
        }
        ReflectionRequest::Eval => evaluate_request(arguments, context.eval_context()),
        ReflectionRequest::MetadataInspect => {
            let [value]: [Value; 1] = arguments.try_into().map_err(|_| {
                TaskHalt::new("`.meta.inspect` received the wrong number of arguments")
            })?;
            let value = evaluate(context.eval_context(), value.into_core())?;
            let Some(metadata) = value.associated_metadata() else {
                return Ok(RequestResult::Fail);
            };
            Ok(RequestResult::Return(Value::from_core(
                context.eval_context().values(),
                metadata,
            )))
        }
        ReflectionRequest::Log => {
            let [severity, message]: [Value; 2] = arguments
                .try_into()
                .map_err(|_| TaskHalt::new("`.log` received the wrong number of arguments"))?;
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
        ReflectionRequest::TaskNew => {
            let [effect]: [Value; 1] = arguments
                .try_into()
                .map_err(|_| TaskHalt::new("`.task.new` received the wrong number of arguments"))?;
            let eval_context = context.eval_context().clone();
            let query_writer = context.host().query_writer().ok_or_else(|| {
                TaskHalt::new("current reflection host does not support task status queries")
            })?;
            let effect = effect.into_core();
            let handle =
                if let Some(mut transaction) = context.transaction() {
                    let result = transaction
                        .store()
                        .reserve_query_with(Value::from_core(
                            eval_context.values(),
                            task_status_query_value(
                                eval_context.values(),
                                EvaluationTaskStatus::Launched,
                            ),
                        ))
                        .map_err(|error| TaskHalt::new(error.as_ref()))?;
                    let pending = eval_context
                        .reserve_reflection_task(effect)
                        .map_err(|error| TaskHalt::new(error.as_ref()))?;
                    let handle = Arc::new(ReflectionTaskHandle {
                        runtime: eval_context.values().runtime_id(),
                        task: pending.handle().clone(),
                        status: result.clone(),
                    });
                    let publisher = Arc::new(TaskStatusPublisher {
                        writer: query_writer,
                        handle: result,
                        values: eval_context.values().clone(),
                    });
                    transaction.parts().1.reflection_journal().updates.push(
                        ReflectionUpdate::Launch {
                            task: pending,
                            publisher,
                        },
                    );
                    handle
                } else {
                    let snapshot = context.host().snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    let result = store
                        .reserve_query_with(Value::from_core(
                            eval_context.values(),
                            task_status_query_value(
                                eval_context.values(),
                                EvaluationTaskStatus::Launched,
                            ),
                        ))
                        .map_err(|error| TaskHalt::new(error.as_ref()))?;
                    let pending = eval_context
                        .reserve_reflection_task(effect)
                        .map_err(|error| TaskHalt::new(error.as_ref()))?;
                    let handle = Arc::new(ReflectionTaskHandle {
                        runtime: eval_context.values().runtime_id(),
                        task: pending.handle().clone(),
                        status: result.clone(),
                    });
                    let publisher = Arc::new(TaskStatusPublisher {
                        writer: query_writer,
                        handle: result,
                        values: eval_context.values().clone(),
                    });
                    let mut journal = S::Journal::default();
                    journal
                        .reflection_journal()
                        .updates
                        .push(ReflectionUpdate::Launch {
                            task: pending,
                            publisher,
                        });
                    match context.host().commit(TaskCommit::new(
                        store,
                        snapshot.extra().clone(),
                        journal,
                    )) {
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
        ReflectionRequest::TaskJoin => {
            let handle = task_handle_argument(context.eval_context(), arguments, "task.join")?;
            if !context.eval_context().owns_task(&handle.task) {
                return Err(TaskHalt::new(
                    "task handle does not belong to this evaluation session",
                ));
            }
            match context.eval_context().poll_reflection_task(&handle.task) {
                EvaluationWaitPoll::Pending(wait) => Err(TaskHalt::blocked(wait)),
                EvaluationWaitPoll::Complete(value) => Ok(RequestResult::Return(Value::from_core(
                    context.eval_context().values(),
                    value,
                ))),
                EvaluationWaitPoll::Failed(error) => {
                    Err(TaskHalt::failure(error)
                        .with_core_context(task_join_context(handle.task.id())))
                }
                EvaluationWaitPoll::Cancelled => {
                    Err(TaskHalt::new("joined reflection task was cancelled"))
                }
                EvaluationWaitPoll::Abandoned => Err(TaskHalt::new(
                    "joined reflection task was abandoned when its evaluation session closed",
                )
                .with_core_context(task_join_context(handle.task.id()))),
            }
        }
        ReflectionRequest::TaskStatus => {
            let (handle, query) = read_task_status(context, arguments, "task.status")?;
            let Some(state) = query.value else {
                observe_query_change(context, &handle.status, query.generation);
                return Ok(RequestResult::Fail);
            };
            Ok(RequestResult::Return(state))
        }
        ReflectionRequest::TaskValue => {
            let (handle, query) = read_task_status(context, arguments, "task.value")?;
            let Some(state) = query.value else {
                observe_query_change(context, &handle.status, query.generation);
                return Ok(RequestResult::Fail);
            };
            match tagged_task_state(context.eval_context().values(), &state)? {
                TaggedTaskState::Complete(value) => Ok(RequestResult::Return(value)),
                TaggedTaskState::Launched | TaggedTaskState::Blocked => {
                    observe_query_change(context, &handle.status, query.generation);
                    Ok(RequestResult::Fail)
                }
                TaggedTaskState::Failed(_)
                | TaggedTaskState::Cancelled
                | TaggedTaskState::Abandoned => Ok(RequestResult::Fail),
            }
        }
        ReflectionRequest::TaskHalt => {
            let (handle, query) = read_task_status(context, arguments, "task.error")?;
            let Some(state) = query.value else {
                observe_query_change(context, &handle.status, query.generation);
                return Ok(RequestResult::Fail);
            };
            match tagged_task_state(context.eval_context().values(), &state)? {
                TaggedTaskState::Failed(error) => Ok(RequestResult::Return(error)),
                TaggedTaskState::Cancelled => Ok(RequestResult::Return(Value::from_core(
                    context.eval_context().values(),
                    CoreValue::binary_from_text("reflection task was cancelled"),
                ))),
                TaggedTaskState::Launched | TaggedTaskState::Blocked => {
                    observe_query_change(context, &handle.status, query.generation);
                    Ok(RequestResult::Fail)
                }
                TaggedTaskState::Complete(_) | TaggedTaskState::Abandoned => {
                    Ok(RequestResult::Fail)
                }
            }
        }
        ReflectionRequest::TaskAcknowledgeError => {
            let handle = task_handle_argument(context.eval_context(), arguments, "task.ack_error")?;
            ensure_local_task(context.eval_context(), &handle)?;
            let eval_context = context.eval_context().clone();
            if let Some(mut transaction) = context.transaction() {
                transaction.parts().1.reflection_journal().updates.push(
                    ReflectionUpdate::AcknowledgeError(eval_context, handle.task.clone()),
                );
            } else {
                let acknowledged = eval_context.acknowledge_reflection_task_error(&handle.task);
                debug_assert!(acknowledged, "task locality was checked above");
                context.committed();
            }
            Ok(RequestResult::ReturnUnit)
        }
        ReflectionRequest::TaskCancel => {
            let handle = task_handle_argument(context.eval_context(), arguments, "task.cancel")?;
            let eval_context = context.eval_context().clone();
            if let Some(mut transaction) = context.transaction() {
                transaction
                    .parts()
                    .1
                    .reflection_journal()
                    .updates
                    .push(ReflectionUpdate::Cancel(eval_context, handle.task.clone()));
            } else {
                match eval_context.cancel_reflection_task(&handle.task) {
                    EvaluationTaskCancellation::Requested => context.committed(),
                    EvaluationTaskCancellation::Late
                    | EvaluationTaskCancellation::NotOwnerSession => {}
                }
            }
            Ok(RequestResult::ReturnUnit)
        }
    }
}

fn evaluate_request(
    arguments: Vec<Value>,
    context: &EvalContext,
) -> Result<RequestResult, TaskHalt> {
    let [value]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("`.eval` received the wrong number of arguments"))?;
    let mut value = value.into_core();
    while matches!(value, CoreValue::Lazy(_) | CoreValue::Promised(_)) {
        value = match eval::eval_value(context, &value) {
            Ok(value) => value,
            Err(error) => {
                if let Some(wait) = error.blocked_on() {
                    return Err(TaskHalt::blocked(wait.0));
                }
                return Ok(RequestResult::Return(tagged_result(
                    context,
                    &keys::ERR,
                    Value::from_core(
                        context.values(),
                        eval::halt_diagnostic_value_with(context.values(), &error)
                            .expect("non-blocked evaluator error must have a failure value"),
                    ),
                )));
            }
        };
    }
    Ok(RequestResult::Return(tagged_result(
        context,
        &keys::OK,
        Value::from_core(context.values(), value),
    )))
}

fn tagged_result(context: &EvalContext, tag: &Key, value: Value) -> Value {
    Value::from_core(
        context.values(),
        CoreValue::Dict(Dict::new_sync().insert(tag.clone(), value.into_core())),
    )
}

struct ReflectionTaskHandle {
    runtime: crate::runtime::EvaluationRuntimeId,
    task: EvaluationTaskHandle,
    status: Arc<EvaluationQueryHandle>,
}

fn task_handle_value(context: &EvalContext, handle: Arc<ReflectionTaskHandle>) -> Value {
    debug_assert_eq!(handle.runtime, context.values().runtime_id());
    Value::from_core(
        context.values(),
        CoreValue::Opaque(OpaqueValue::new(handle)),
    )
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

fn task_status_query_value(values: &CoreValueFactory, status: EvaluationTaskStatus) -> CoreValue {
    match status {
        EvaluationTaskStatus::Launched => values.key_value(&keys::LAUNCHED),
        EvaluationTaskStatus::Blocked => values.key_value(&keys::BLOCKED),
        EvaluationTaskStatus::Complete(value) => {
            debug_assert_eq!(value.runtime_id(), values.runtime_id());
            CoreValue::Dict(Dict::new_sync().insert((*keys::OK).clone(), value.as_core().clone()))
        }
        EvaluationTaskStatus::Failed(error) => CoreValue::Dict(Dict::new_sync().insert(
            (*keys::ERR).clone(),
            eval::failure_diagnostic_value_with(values, &error),
        )),
        EvaluationTaskStatus::Cancelled => values.key_value(&keys::CANCELED),
        EvaluationTaskStatus::Abandoned => values.key_value(&keys::ABANDONED),
    }
}

enum TaggedTaskState {
    Launched,
    Blocked,
    Complete(Value),
    Failed(Value),
    Cancelled,
    Abandoned,
}

fn tagged_task_state(
    values: &CoreValueFactory,
    value: &Value,
) -> Result<TaggedTaskState, TaskHalt> {
    if value.as_core() == &values.key_value(&keys::LAUNCHED) {
        return Ok(TaggedTaskState::Launched);
    }
    if value.as_core() == &values.key_value(&keys::BLOCKED) {
        return Ok(TaggedTaskState::Blocked);
    }
    if value.as_core() == &values.key_value(&keys::CANCELED) {
        return Ok(TaggedTaskState::Cancelled);
    }
    if value.as_core() == &values.key_value(&keys::ABANDONED) {
        return Ok(TaggedTaskState::Abandoned);
    }
    let CoreValue::Dict(state) = value.as_core() else {
        return Err(TaskHalt::new("reflection task status is malformed"));
    };
    if state.iter().count() != 1 {
        return Err(TaskHalt::new("reflection task status is malformed"));
    }
    if let Some(value) = state.get(&*keys::OK) {
        return Ok(TaggedTaskState::Complete(Value::from_core(
            values,
            value.clone(),
        )));
    }
    if let Some(error) = state.get(&*keys::ERR) {
        return Ok(TaggedTaskState::Failed(Value::from_core(
            values,
            error.clone(),
        )));
    }
    Err(TaskHalt::new("reflection task status is malformed"))
}

fn task_handle_argument(
    context: &EvalContext,
    arguments: Vec<Value>,
    request: &str,
) -> Result<Arc<ReflectionTaskHandle>, TaskHalt> {
    let [handle]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskHalt::new(format!(
            "`.{request}` received the wrong number of arguments"
        ))
    })?;
    let CoreValue::Opaque(handle) = evaluate(context, handle.into_core())? else {
        return Err(TaskHalt::new(format!(
            "`.{request}` requires a reflection task handle"
        )));
    };
    handle
        .downcast::<ReflectionTaskHandle>()
        .ok_or_else(|| TaskHalt::new(format!("`.{request}` requires a reflection task handle")))
}

fn ensure_local_task(context: &EvalContext, handle: &ReflectionTaskHandle) -> Result<(), TaskHalt> {
    if handle.runtime != context.values().runtime_id() {
        Err(TaskHalt::new(
            "task handle belongs to a different evaluation runtime",
        ))
    } else if !context.owns_task(&handle.task) {
        Err(TaskHalt::new(
            "task handle does not belong to this evaluation session",
        ))
    } else {
        Ok(())
    }
}

struct QueryRead {
    value: Option<Value>,
    generation: u64,
}

fn read_task_status<S: TaskSpecialization>(
    context: &mut RequestContext<'_, S>,
    arguments: Vec<Value>,
    request: &str,
) -> Result<(Arc<ReflectionTaskHandle>, QueryRead), TaskHalt> {
    let handle = task_handle_argument(context.eval_context(), arguments, request)?;
    ensure_local_task(context.eval_context(), &handle)?;
    let status = read_query(context, &handle.status)?;
    Ok((handle, status))
}

fn read_query<S: TaskSpecialization>(
    context: &mut RequestContext<'_, S>,
    handle: &Arc<EvaluationQueryHandle>,
) -> Result<QueryRead, TaskHalt> {
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
    let state = evaluate(context.eval_context(), value.into_core())?;
    let value = match decode_query_state(context.eval_context().values(), &state) {
        Some(EvaluationQueryState::Pending) => None,
        Some(EvaluationQueryState::Complete(result)) => Some(result),
        None => return Err(TaskHalt::new("query handle has been retired")),
    };
    Ok(QueryRead { value, generation })
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

pub(crate) fn prepare_message(context: &EvalContext, message: Value) -> Result<Value, TaskHalt> {
    let log_message_context = || eval::evaluation_context_frame("log_message");
    let CoreValue::Dict(mut message) = evaluate(context, message.into_core())
        .map_err(|error| error.with_core_context(log_message_context()))?
    else {
        return Err(TaskHalt::new("`.log` message must evaluate to an object"));
    };
    if let Some(interface) = message.get(&*keys::MSG) {
        message = message.insert(
            (*keys::MSG).clone(),
            evaluate(context, interface.clone())
                .map_err(|error| error.with_core_context(log_message_context()))?,
        );
    }
    Ok(Value::from_core(context.values(), CoreValue::Dict(message)))
}

pub(crate) fn parse_severity(context: &EvalContext, value: Value) -> Result<Severity, TaskHalt> {
    let value = evaluate(context, value.into_core())
        .map_err(|error| error.with_core_context(eval::evaluation_context_frame("log_severity")))?;
    if severity_matches(&value, "info", &keys::INFO) {
        Ok(Severity::Info)
    } else if severity_matches(&value, "warn", &keys::WARN) {
        Ok(Severity::Warning)
    } else if severity_matches(&value, "error", &keys::ERROR) {
        Ok(Severity::Error)
    } else {
        Err(TaskHalt::new(
            "`.log` severity must be `'info`, `'warn`, or `'error`",
        ))
    }
}

fn severity_matches(value: &CoreValue, name: &str, canonical: &Key) -> bool {
    Key::from_value(value).as_ref() == Some(canonical)
        || value == &CoreValue::Atom(Atom::from_key(&Key::binary_from_text(name)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct TestQueryWriter {
        store: Arc<Mutex<crate::reflection::ReflectionStore>>,
        updates: Arc<Mutex<Vec<Value>>>,
    }

    struct TestRoleHost {
        writer: Arc<dyn ReflectionQueryWriter>,
    }

    impl ReflectionQueryWriter for TestQueryWriter {
        fn update_query(&self, handle: &Arc<EvaluationQueryHandle>, result: Value) {
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
        }
    }

    #[test]
    fn abandoned_task_status_has_a_distinct_round_trip() {
        let values = crate::core::test_value_factory();
        let encoded = Value::from_core(
            &values,
            task_status_query_value(&values, EvaluationTaskStatus::Abandoned),
        );

        assert_eq!(encoded.as_core(), &values.key_value(&keys::ABANDONED));
        assert!(matches!(
            tagged_task_state(&values, &encoded).expect("abandoned status should decode"),
            TaggedTaskState::Abandoned
        ));
    }

    #[test]
    fn task_status_publisher_does_not_retain_its_role_host() {
        let values = crate::core::test_value_factory();
        let store = Arc::new(Mutex::new(crate::reflection::ReflectionStore::new(
            values.clone(),
            Arc::new(crate::reflection::ExactConflictAnalysis),
        )));
        let handle = {
            let mut store = store.lock().expect("test query store was poisoned");
            let mut journal = StoreJournal::new(store.snapshot());
            let handle = journal
                .reserve_query_with(Value::from_core(
                    &values,
                    task_status_query_value(&values, EvaluationTaskStatus::Launched),
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
        let publisher = TaskStatusPublisher {
            writer: role_host.writer.clone(),
            handle: handle.clone(),
            values: values.clone(),
        };
        drop(role_host);
        assert!(
            role_host_weak.upgrade().is_none(),
            "the status publisher must not retain its originating role host"
        );

        publisher.update(EvaluationTaskStatus::Blocked);
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
        assert_eq!(updates[0].as_core(), &values.key_value(&keys::BLOCKED));
    }
}
