use crate::api::{Diagnostic, Value};
use crate::core::{Atom, Key, Value as CoreValue, keys};
use crate::diagnostic::Severity;
use crate::evaluation::EvalContext;

use super::{
    EffectRequestSpec, RequestContext, RequestResult, TaskError, TaskHost, TaskSpecialization,
    evaluate,
};

/// Requests shared by every full reflection task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectionRequest {
    Log,
}

/// Diagnostics produced transactionally by reflection requests.
#[derive(Clone, Default)]
pub struct ReflectionJournal {
    diagnostics: Vec<Diagnostic>,
}

impl ReflectionJournal {
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
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
    vec![EffectRequestSpec::new(
        "log",
        ["reflection_runtime", "v0", "request", "log"],
        2,
        ReflectionRequest::Log,
    )]
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
    }
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
