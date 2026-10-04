use std::fmt;
use std::sync::Arc;

use super::{Diagnostic, Value, Values};
use crate::core::{CoreValueFactory, EvaluationHalt, EvaluationPanic};
use crate::diagnostic::Severity;
use crate::evaluation::{EvaluationSessionId, EvaluationTaskId};
use crate::interaction_net::NetBuildError;
use crate::runtime::EvaluationRuntimeId;

#[derive(Debug, Clone)]
pub struct Error {
    message: Arc<str>,
    diagnostic: Option<Arc<Diagnostic>>,
    diagnostics: Vec<Diagnostic>,
    panic: Option<EvaluationPanic>,
}

/// How an operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Invalid input, a Glam evaluation failure, or a host error.
    Failure,
    /// Scheduled evaluation was interrupted by a panic. This means a runtime
    /// implementation error or a client contract violation, never a Glam
    /// semantic outcome. The runtime remains usable, and a later demand of
    /// the same value may run it again.
    Panic,
}

impl Error {
    /// Constructs an embedding-boundary error from a plain diagnostic message.
    pub fn new(message: impl Into<Arc<str>>) -> Self {
        let message = message.into();
        Self {
            message,
            diagnostic: None,
            diagnostics: Vec::new(),
            panic: None,
        }
    }

    pub(crate) fn from_eval(values: &CoreValueFactory, error: EvaluationHalt) -> Self {
        if let Some(report) = error.panic_report() {
            return Self::panicked(report.clone());
        }
        Self::from_eval_parts(
            values,
            crate::diagnostic::halt_diagnostic_root_with(values, &error),
        )
    }

    fn from_eval_parts(
        values: &CoreValueFactory,
        emission: Option<crate::runtime::RuntimeValueRoot>,
    ) -> Self {
        let message: Arc<str> = Arc::from("glam evaluation failed");
        let diagnostic = match emission {
            Some(emission) => {
                let public_values = Values::from_core_factory(values.clone());
                Diagnostic::from_parts(
                    &public_values,
                    None,
                    Severity::Error,
                    Value::from_runtime_root(emission),
                    None,
                )
            }
            None => Diagnostic::new_with_factory(values, Severity::Error, Arc::clone(&message)),
        };
        Self {
            message,
            diagnostic: Some(Arc::new(diagnostic)),
            diagnostics: Vec::new(),
            panic: None,
        }
    }

    /// Reports interrupted evaluation. A panic carries no Glam diagnostic,
    /// because it is never a semantic failure.
    fn panicked(report: EvaluationPanic) -> Self {
        Self {
            message: Arc::from(report.to_string()),
            diagnostic: None,
            diagnostics: Vec::new(),
            panic: Some(report),
        }
    }

    pub fn kind(&self) -> ErrorKind {
        if self.panic.is_some() {
            ErrorKind::Panic
        } else {
            ErrorKind::Failure
        }
    }

    /// Returns the panic message when evaluation was interrupted by a panic.
    pub fn panic_message(&self) -> Option<&str> {
        self.panic.as_ref().map(EvaluationPanic::message)
    }

    pub(crate) fn panic_report(&self) -> Option<&EvaluationPanic> {
        self.panic.as_ref()
    }

    pub(super) fn with_diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Prepends one structured frame describing why this failed value was
    /// demanded. The primary diagnostic text and ad hoc fields remain intact.
    pub fn with_context(mut self, values: &Values, context: Value) -> Result<Self, Error> {
        context.require_runtime(values.runtime)?;
        let Some(diagnostic) = &self.diagnostic else {
            self.diagnostic = Some(Arc::new(Diagnostic::new(
                values,
                Severity::Error,
                self.message.clone(),
            )));
            return self.with_context(values, context);
        };
        diagnostic.emission.require_runtime(values.runtime)?;
        self.diagnostic = Some(Arc::new(
            diagnostic.as_ref().clone().with_context(values, context)?,
        ));
        Ok(self)
    }

    /// Returns the primary failure as a structured diagnostic.
    ///
    /// Permanent evaluator failures retain their original Glam emission and
    /// `msg.context`; ordinary host failures use a conventional text message.
    pub fn diagnostic(&self, values: &Values) -> Result<Diagnostic, Error> {
        match &self.diagnostic {
            Some(diagnostic) => {
                diagnostic.emission.require_runtime(values.runtime)?;
                Ok(diagnostic.as_ref().clone())
            }
            None => Ok(Diagnostic::new(
                values,
                Severity::Error,
                self.message.clone(),
            )),
        }
    }

    pub(crate) fn structured_diagnostic(&self) -> Option<&Diagnostic> {
        self.diagnostic.as_deref()
    }

    /// Returns additional diagnostics emitted while attempting the operation.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub(super) fn net_build_error(error: NetBuildError) -> Error {
    Error::new(format!("invalid interaction net: {error}"))
}

#[derive(Clone)]
pub struct ReasoningFailure {
    pub(super) runtime: EvaluationRuntimeId,
    pub(super) task: EvaluationTaskId,
    pub(super) diagnostic: Diagnostic,
    pub(super) session: EvaluationSessionId,
}

impl fmt::Debug for ReasoningFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReasoningFailure")
            .field("task_id", &self.task_id())
            .field("diagnostic", &self.diagnostic)
            .finish_non_exhaustive()
    }
}

impl ReasoningFailure {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub fn session_id(&self) -> u64 {
        self.session.get()
    }

    pub fn task_id(&self) -> u64 {
        self.task.get()
    }

    pub fn message(&self) -> &str {
        self.diagnostic.message()
    }

    /// Returns the structured terminal failure retained by the reasoning
    /// session.
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }
}
