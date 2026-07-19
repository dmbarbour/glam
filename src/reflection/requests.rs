use std::ffi::OsString;
use std::sync::LazyLock;

use crate::api::{Diagnostic, Value};
use crate::core::{Atom, Dict, Key, Value as CoreValue, keys};
use crate::diagnostic::Severity;
use crate::eval;
use crate::evaluation::{
    EvalContext, EvaluationTaskCancellation, EvaluationTaskHandle, EvaluationTaskId,
    EvaluationTaskPoll, PendingReflectionTask,
};
use crate::number::Number;

use super::{
    EffectRequestSpec, RequestContext, RequestResult, TaskError, TaskHost, TaskSpecialization,
    evaluate,
};

/// Requests shared by every full reflection task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectionRequest {
    GlamVersion,
    OsEnvironment,
    CommandLineArguments,
    DictItems,
    Eval,
    Log,
    ReflTask,
    JoinTask,
    TaskResult,
    TaskError,
    TaskStatus,
    CancelTask,
}

#[derive(Clone)]
enum ReflectionTaskUpdate {
    Launch(PendingReflectionTask),
    Cancel(EvalContext, EvaluationTaskId),
}

/// Diagnostics produced transactionally by reflection requests.
#[derive(Clone, Default)]
pub struct ReflectionJournal {
    diagnostics: Vec<Diagnostic>,
    task_updates: Vec<ReflectionTaskUpdate>,
}

impl ReflectionJournal {
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[doc(hidden)]
    pub fn commit_task_updates(&self) {
        for update in &self.task_updates {
            match update {
                ReflectionTaskUpdate::Launch(task) => task.activate(),
                ReflectionTaskUpdate::Cancel(context, task) => {
                    context.cancel_reflection_task_id(*task);
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

/// Immediate host operations used by reflection requests outside `.cut`.
pub trait ReflectionHost<S: TaskSpecialization>: TaskHost<S> {
    fn emit_diagnostic(&self, diagnostic: Diagnostic);

    fn os_environment_variable(&self, _name: &str) -> Option<OsString> {
        None
    }

    fn command_line_arguments(&self) -> Vec<OsString> {
        Vec::new()
    }
}

/// API constructors contributed by the reusable reflection request family.
pub fn reflection_request_specs() -> Vec<EffectRequestSpec<ReflectionRequest>> {
    vec![
        EffectRequestSpec::new(
            "glam_ver",
            ["reflection_runtime", "v0", "request", "glam_ver"],
            0,
            ReflectionRequest::GlamVersion,
        ),
        EffectRequestSpec::new(
            "os_env",
            ["reflection_runtime", "v0", "request", "os_env"],
            1,
            ReflectionRequest::OsEnvironment,
        ),
        EffectRequestSpec::new(
            "cli_args",
            ["reflection_runtime", "v0", "request", "cli_args"],
            0,
            ReflectionRequest::CommandLineArguments,
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
            "task_status",
            ["reflection_runtime", "v0", "request", "task_status"],
            1,
            ReflectionRequest::TaskStatus,
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
        ReflectionRequest::GlamVersion => {
            require_no_arguments(arguments, "glam_ver")?;
            Ok(RequestResult::Return(Value::text(env!(
                "CARGO_PKG_VERSION"
            ))))
        }
        ReflectionRequest::OsEnvironment => {
            let name = text_argument(context.eval_context(), arguments, "os_env")?;
            Ok(context
                .host()
                .os_environment_variable(&name)
                .map(|value| RequestResult::Return(os_value(value)))
                .unwrap_or(RequestResult::Fail))
        }
        ReflectionRequest::CommandLineArguments => {
            require_no_arguments(arguments, "cli_args")?;
            Ok(RequestResult::Return(Value::list(
                context
                    .host()
                    .command_line_arguments()
                    .into_iter()
                    .map(os_value),
            )))
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
                    .task_updates
                    .push(ReflectionTaskUpdate::Launch(pending));
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
        ReflectionRequest::TaskStatus => {
            let handle = task_handle_argument(context.eval_context(), arguments, "task_status")?;
            let status = match context.eval_context().poll_reflection_task_id(handle) {
                EvaluationTaskPoll::Pending(wait) => {
                    context.observe_task_wait(wait);
                    "pending"
                }
                EvaluationTaskPoll::Complete(_) => "complete",
                EvaluationTaskPoll::Failed(_) => "error",
                EvaluationTaskPoll::Cancelled => "canceled",
                EvaluationTaskPoll::ForeignSession => "foreign",
            };
            Ok(RequestResult::Return(atom(status)))
        }
        ReflectionRequest::CancelTask => {
            let handle = task_handle_argument(context.eval_context(), arguments, "cancel_task")?;
            let eval_context = context.eval_context().clone();
            match eval_context.poll_reflection_task_id(handle) {
                EvaluationTaskPoll::ForeignSession => {
                    return Err(TaskError::new(
                        "task handle does not belong to this evaluation session",
                    ));
                }
                EvaluationTaskPoll::Complete(_)
                | EvaluationTaskPoll::Failed(_)
                | EvaluationTaskPoll::Cancelled => return Ok(RequestResult::ReturnUnit),
                EvaluationTaskPoll::Pending(_) => {}
            }
            if let Some(mut transaction) = context.transaction() {
                transaction
                    .parts()
                    .1
                    .reflection_journal()
                    .task_updates
                    .push(ReflectionTaskUpdate::Cancel(eval_context, handle));
            } else {
                match eval_context.cancel_reflection_task_id(handle) {
                    EvaluationTaskCancellation::Requested => context.committed(),
                    EvaluationTaskCancellation::Late => {}
                    EvaluationTaskCancellation::ForeignSession => {
                        return Err(TaskError::new(
                            "task handle does not belong to this evaluation session",
                        ));
                    }
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

fn require_no_arguments(arguments: Vec<Value>, request: &str) -> Result<(), TaskError> {
    if arguments.is_empty() {
        Ok(())
    } else {
        Err(TaskError::new(format!(
            "`.{request}` received the wrong number of arguments"
        )))
    }
}

fn text_argument(
    context: &EvalContext,
    arguments: Vec<Value>,
    request: &str,
) -> Result<String, TaskError> {
    let [value]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskError::new(format!(
            "`.{request}` received the wrong number of arguments"
        ))
    })?;
    let CoreValue::Binary(value) = evaluate(context, value.into_core())? else {
        return Err(TaskError::new(format!(
            "`.{request}` requires a text argument"
        )));
    };
    String::from_utf8(value.to_vec())
        .map_err(|_| TaskError::new(format!("`.{request}` requires UTF-8 text")))
}

fn os_value(value: OsString) -> Value {
    Value::binary(value.as_encoded_bytes().to_vec())
}

fn atom(name: &str) -> Value {
    Value::from_core(CoreValue::Atom(Atom::from_key(&Key::binary_from_text(
        name,
    ))))
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
