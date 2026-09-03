use std::collections::BTreeMap;
use std::sync::Arc;

use crate::api::{Diagnostic, Value, Values};
use crate::core::{Atom, CoreValueFactory, Dict, Key, OpaqueValue, Value as CoreValue, keys};
use crate::diagnostic::Severity;
use crate::eval;
use crate::evaluation::{
    EvalContext, EvaluationTaskCancellation, EvaluationTaskHandle, EvaluationTaskId,
    EvaluationTaskStatus, EvaluationWaitPoll, PendingReflectionTask, PendingTaskPolicy,
    TaskStatusPublisher, TaskStatusWake,
};
use crate::number::Number;

use super::protocol::{
    CommitResult, EffectRequestSpec, RequestContext, RequestResult, TaskCommit, TaskEnvironment,
    TaskHalt, TaskHost, TaskSpecialization,
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
            let path = context.evaluate_key_path(&path)?;
            let environment = context.host().reflection_environment();
            Ok(RequestResult::Return(
                context.evaluate_path(&environment, &path)?,
            ))
        }
        ReflectionRequest::DictItems => {
            let [dict]: [Value; 1] = arguments.try_into().map_err(|_| {
                TaskHalt::new("`.dict_items` received the wrong number of arguments")
            })?;
            let dict = context.evaluate(&dict)?;
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
        ReflectionRequest::Eval => evaluate_request(arguments, context),
        ReflectionRequest::MetadataInspect => {
            let [value]: [Value; 1] = arguments.try_into().map_err(|_| {
                TaskHalt::new("`.meta.inspect` received the wrong number of arguments")
            })?;
            let value = context.evaluate(&value)?;
            let Some(metadata) = value.with_core(CoreValue::associated_metadata)? else {
                return Ok(RequestResult::Fail);
            };
            Ok(RequestResult::Return(context.values().wrap(metadata)))
        }
        ReflectionRequest::Log => {
            let [severity, message]: [Value; 2] = arguments
                .try_into()
                .map_err(|_| TaskHalt::new("`.log` received the wrong number of arguments"))?;
            let message = prepare_message(context, message)?;
            let diagnostic = Diagnostic::from_emission(
                &context.values(),
                parse_severity(context, severity)?,
                message,
            )
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
            let values = context.values();
            let effect = values.clone_core(&effect)?;
            let launched = values.wrap(task_status_query_value(
                &values,
                EvaluationTaskStatus::Launched,
            ));
            let handle =
                if let Some(mut transaction) = context.transaction() {
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
                    let publisher =
                        task_status_publisher(query_writer, result, eval_context.values().clone());
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
                    let publisher =
                        task_status_publisher(query_writer, result, eval_context.values().clone());
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
            let handle = task_handle_argument(context, arguments, "task.join")?;
            ensure_runtime_task(context.eval_context(), &handle)?;
            match context.eval_context().poll_reflection_task(&handle.task) {
                EvaluationWaitPoll::Pending(wait) => Err(TaskHalt::blocked(wait)),
                EvaluationWaitPoll::Complete(value) => {
                    Ok(RequestResult::Return(Value::from_runtime_root(*value)))
                }
                EvaluationWaitPoll::Failed(error) => {
                    handle.task.acknowledge_propagated_failure();
                    Err(TaskHalt::rooted_failure(error)
                        .with_core_context(task_join_context(handle.task.id())))
                }
                EvaluationWaitPoll::Cancelled => {
                    Err(TaskHalt::new("joined reflection task was cancelled"))
                }
                EvaluationWaitPoll::Abandoned => Err(TaskHalt::new(
                    "joined reflection task was abandoned when its evaluation session closed",
                )
                .with_core_context(task_join_context(handle.task.id()))),
                EvaluationWaitPoll::Exited => Err(TaskHalt::new(
                    "joined reflection task exited without producing a result",
                )
                .with_core_context(task_join_context(handle.task.id()))),
                EvaluationWaitPoll::Killed(error) => Err(TaskHalt::rooted_failure(error)
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
            match tagged_task_state(&context.values(), &state)? {
                TaggedTaskState::Complete(value) => Ok(RequestResult::Return(value)),
                TaggedTaskState::Launched | TaggedTaskState::Blocked => {
                    observe_query_change(context, &handle.status, query.generation);
                    Ok(RequestResult::Fail)
                }
                TaggedTaskState::Failed(_)
                | TaggedTaskState::Cancelled
                | TaggedTaskState::Abandoned
                | TaggedTaskState::Exited
                | TaggedTaskState::Killed => Ok(RequestResult::Fail),
            }
        }
        ReflectionRequest::TaskHalt => {
            let (handle, query) = read_task_status(context, arguments, "task.error")?;
            let Some(state) = query.value else {
                observe_query_change(context, &handle.status, query.generation);
                return Ok(RequestResult::Fail);
            };
            match tagged_task_state(&context.values(), &state)? {
                TaggedTaskState::Failed(error) => Ok(RequestResult::Return(error)),
                TaggedTaskState::Cancelled => Ok(RequestResult::Return(
                    context.values().text("reflection task was cancelled"),
                )),
                TaggedTaskState::Launched | TaggedTaskState::Blocked => {
                    observe_query_change(context, &handle.status, query.generation);
                    Ok(RequestResult::Fail)
                }
                TaggedTaskState::Complete(_)
                | TaggedTaskState::Abandoned
                | TaggedTaskState::Exited
                | TaggedTaskState::Killed => Ok(RequestResult::Fail),
            }
        }
        ReflectionRequest::TaskAcknowledgeError => {
            let handle = task_handle_argument(context, arguments, "task.ack_error")?;
            ensure_runtime_task(context.eval_context(), &handle)?;
            if let Some(mut transaction) = context.transaction() {
                transaction
                    .parts()
                    .1
                    .reflection_journal()
                    .updates
                    .push(ReflectionUpdate::AcknowledgeError(handle.task.clone()));
            } else {
                handle.task.acknowledge_failure();
                context.committed();
            }
            Ok(RequestResult::ReturnUnit)
        }
        ReflectionRequest::TaskCancel => {
            let handle = task_handle_argument(context, arguments, "task.cancel")?;
            ensure_runtime_task(context.eval_context(), &handle)?;
            if let Some(mut transaction) = context.transaction() {
                transaction
                    .parts()
                    .1
                    .reflection_journal()
                    .updates
                    .push(ReflectionUpdate::Cancel(handle.task.clone()));
            } else {
                match handle.task.cancel() {
                    EvaluationTaskCancellation::Requested => context.committed(),
                    EvaluationTaskCancellation::Late => {}
                }
            }
            Ok(RequestResult::ReturnUnit)
        }
    }
}

fn evaluate_request<S: TaskSpecialization>(
    arguments: Vec<Value>,
    context: &RequestContext<'_, S>,
) -> Result<RequestResult, TaskHalt> {
    let [value]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("`.eval` received the wrong number of arguments"))?;
    let value = match context.evaluate(&value) {
        Ok(value) => value,
        Err(error) if error.blocked_on().is_some() => return Err(error),
        Err(error) => {
            let failure = error
                .permanent_failure()
                .expect("non-blocked request evaluation must retain a permanent failure");
            return Ok(RequestResult::Return(tagged_result(
                &context.values(),
                &keys::ERR,
                context.values().wrap(eval::failure_diagnostic_value_with(
                    context.eval_context().values(),
                    failure,
                )),
            )));
        }
    };
    Ok(RequestResult::Return(tagged_result(
        &context.values(),
        &keys::OK,
        value.into_value(),
    )))
}

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

// SAFETY: the handle contains no bare core value, runtime value root, or
// managed pointer. It is an external lifecycle capability over coordinator
// and query state, so I9/I10 must retain its active-retirement classification
// rather than treating it as a managed leaf.
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
            eval::failure_diagnostic_value_with(values.core(), error.as_failure()),
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

fn task_handle_argument<S: TaskSpecialization>(
    context: &RequestContext<'_, S>,
    arguments: Vec<Value>,
    request: &str,
) -> Result<Arc<TaskHandleCell>, TaskHalt> {
    let [handle]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskHalt::new(format!(
            "`.{request}` received the wrong number of arguments"
        ))
    })?;
    let handle = context.evaluate(&handle)?;
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

fn ensure_runtime_task(context: &EvalContext, handle: &TaskHandleCell) -> Result<(), TaskHalt> {
    if handle.runtime != context.values().runtime_id() {
        Err(TaskHalt::new(
            "task handle belongs to a different evaluation runtime",
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
) -> Result<(Arc<TaskHandleCell>, QueryRead), TaskHalt> {
    let handle = task_handle_argument(context, arguments, request)?;
    ensure_runtime_task(context.eval_context(), &handle)?;
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
    let state = context.evaluate(&value)?;
    let values = context.values();
    let value = match state.with_core(|state| decode_query_state(&values, state))? {
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

pub(crate) fn prepare_message<S: TaskSpecialization>(
    context: &RequestContext<'_, S>,
    message: Value,
) -> Result<Value, TaskHalt> {
    let log_message_context = || eval::evaluation_context_frame("log_message");
    let evaluated_message = context
        .evaluate(&message)
        .map_err(|error| error.with_core_context(log_message_context()))?;
    let values = context.values();
    let mut message = evaluated_message.with_core(|value| {
        let CoreValue::Dict(message) = value else {
            return Err(TaskHalt::new("`.log` message must evaluate to an object"));
        };
        Ok(message.clone())
    })??;
    if let Some(interface) = message.get(&*keys::MSG) {
        let interface = values.wrap(interface.clone());
        let evaluated = context
            .evaluate(&interface)
            .map_err(|error| error.with_core_context(log_message_context()))?;
        message = message.insert(
            (*keys::MSG).clone(),
            values.clone_core(evaluated.as_value())?,
        );
    }
    Ok(values.wrap(CoreValue::Dict(message)))
}

pub(crate) fn parse_severity<S: TaskSpecialization>(
    context: &RequestContext<'_, S>,
    value: Value,
) -> Result<Severity, TaskHalt> {
    let value = context
        .evaluate(&value)
        .map_err(|error| error.with_core_context(eval::evaluation_context_frame("log_severity")))?;
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

    fn assert_task_handle_inventory(handle: &TaskHandleCell) {
        let TaskHandleCell {
            runtime,
            task,
            status,
        } = handle;
        let _: &crate::runtime::EvaluationRuntimeId = runtime;
        let _: &EvaluationTaskHandle = task;
        let _: &Arc<EvaluationQueryHandle> = status;
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

    fn assert_query_read_inventory(read: &QueryRead) {
        let QueryRead { value, generation } = read;
        let _: &Option<Value> = value;
        let _: &u64 = generation;
    }

    fn assert_query_mutation_inventory(mutation: &ReflectionQueryMutation<'_>) {
        let ReflectionQueryMutation { mutation } = mutation;
        let _: &&dyn crate::runtime::RuntimeMutationAuthority = mutation;
    }

    #[test]
    fn reflection_request_root_inventory_is_complete() {
        let _: fn(&ReflectionRequest) = assert_reflection_request_inventory;
        let _: fn(&ReflectionUpdate, &ReflectionJournal) = assert_reflection_journal_inventory;
        let _: fn(&TaskHandleCell) = assert_task_handle_inventory;
        let _: fn(&TaggedTaskState) = assert_tagged_task_state_inventory;
        let _: fn(&QueryRead) = assert_query_read_inventory;
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
            assert!(retained.upgrade().is_some());
            drop(state);
            domain.collect_and_drain_retired_external_owners_for_test();
            assert!(retained.upgrade().is_none());
        }

        let (value, retained) = retained_request_value(&domain);
        let read = QueryRead {
            value: Some(value),
            generation: 1,
        };
        assert!(retained.upgrade().is_some());
        drop(read);
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
