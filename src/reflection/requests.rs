use std::sync::LazyLock;

use crate::api::{Diagnostic, Value};
use crate::core::{Atom, Dict, Key, Value as CoreValue, keys};
use crate::diagnostic::Severity;
use crate::evaluation::{
    EvalContext, EvaluationTaskHandle, EvaluationTaskId, EvaluationTaskPoll, PendingReflectionTask,
};
use crate::number::Number;

use super::{
    EffectRequestSpec, RequestContext, RequestResult, TaskError, TaskHost, TaskSpecialization,
    evaluate,
};

/// Requests shared by every full reflection task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectionRequest {
    Log,
    ReflTask,
    JoinTask,
    TaskError,
}

/// Diagnostics produced transactionally by reflection requests.
#[derive(Clone, Default)]
pub struct ReflectionJournal {
    diagnostics: Vec<Diagnostic>,
    pending_tasks: Vec<PendingReflectionTask>,
}

impl ReflectionJournal {
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[doc(hidden)]
    pub fn activate_pending_tasks(&self) {
        for task in &self.pending_tasks {
            task.activate();
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
}

/// API constructors contributed by the reusable reflection request family.
pub fn reflection_request_specs() -> Vec<EffectRequestSpec<ReflectionRequest>> {
    vec![
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
            "task_error",
            ["reflection_runtime", "v0", "request", "task_error"],
            1,
            ReflectionRequest::TaskError,
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
                    .pending_tasks
                    .push(pending);
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
    }
}

static TASK_HANDLE_TAG: LazyLock<Key> = LazyLock::new(|| {
    Key::abstract_global_path(["reflection_runtime", "v0", "value", "task_handle"])
});

fn task_handle_value(handle: &EvaluationTaskHandle) -> Value {
    Value::from_core(CoreValue::Dict(Dict::new_sync().insert(
        TASK_HANDLE_TAG.clone(),
        CoreValue::Number(Number::from_u64(handle.id().get())),
    )))
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
