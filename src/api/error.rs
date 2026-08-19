use std::fmt;
use std::sync::Arc;

use super::{Diagnostic, Value, Values};
use crate::core::{CoreValueFactory, EvaluationHalt, Value as CoreValue};
use crate::diagnostic::Severity;
use crate::eval;
use crate::evaluation::{EvaluationSessionId, EvaluationTaskId};
use crate::interaction_net::NetBuildError;
use crate::runtime::EvaluationRuntimeId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: Arc<str>,
    diagnostic: Option<Arc<Diagnostic>>,
    diagnostics: Vec<Diagnostic>,
}

impl Error {
    /// Constructs an embedding-boundary error from a plain diagnostic message.
    pub fn new(message: impl Into<Arc<str>>) -> Self {
        let message = message.into();
        Self {
            message,
            diagnostic: None,
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn from_eval(values: &CoreValueFactory, error: EvaluationHalt) -> Self {
        let message: Arc<str> = Arc::from(error.to_string());
        Self::from_eval_parts(
            values,
            eval::halt_diagnostic_value_with(values, &error),
            message,
        )
    }

    fn from_eval_parts(
        values: &CoreValueFactory,
        emission: Option<CoreValue>,
        message: Arc<str>,
    ) -> Self {
        let (message, diagnostic) = match emission {
            Some(emission) => {
                let message = crate::diagnostic::conventional_summary_with(values, &emission)
                    .1
                    .unwrap_or(message);
                (
                    message,
                    Diagnostic::from_emission(Severity::Error, Value::from_core(values, emission)),
                )
            }
            None => (
                message.clone(),
                Diagnostic::new_with_factory(values, Severity::Error, message),
            ),
        };
        Self {
            message,
            diagnostic: Some(Arc::new(diagnostic)),
            diagnostics: Vec::new(),
        }
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
        let emission = crate::diagnostic::prepend_contexts_with(
            &values.core,
            diagnostic.emission.as_core().clone(),
            &[context.into_core()],
        )
        .unwrap_or_else(|_| diagnostic.emission.as_core().clone());
        self.diagnostic = Some(Arc::new(Diagnostic::from_emission(
            diagnostic.severity,
            Value::from_core(&values.core, emission),
        )));
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

#[derive(Clone, PartialEq, Eq)]
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
