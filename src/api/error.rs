use std::fmt;
use std::sync::Arc;

use super::{Diagnostic, Value, Values};
use crate::core::{CoreValueFactory, EvaluationHalt, EvaluationPanic};
use crate::diagnostic::Severity;
use crate::interaction_net::NetBuildError;

#[derive(Debug, Clone)]
pub struct Error {
    message: Arc<str>,
    diagnostic: Option<Arc<Diagnostic>>,
    diagnostics: Vec<Diagnostic>,
    panic: Option<EvaluationPanic>,
    backtrack: bool,
    retry: bool,
    conflict: bool,
    interrupted: bool,
    /// What an evaluation that waited was waiting on.
    awaited: Option<Awaited>,
}

/// What a waiting evaluation waited on: a promise, or work that produces
/// what it needs.
#[derive(Clone)]
pub(crate) struct Awaited(crate::evaluation::WorkDependency);

impl fmt::Debug for Awaited {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Awaited(..)")
    }
}

/// How an operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Invalid input, a Glam evaluation failure, or a host error.
    Failure,
    /// Scheduled evaluation was interrupted by a panic. This means a runtime
    /// implementation error or a client contract violation, never a Glam
    /// semantic outcome. The runtime remains usable; a lazy whose evaluation
    /// panicked keeps that state, so a later demand of it reports the same
    /// panic instead of running it again.
    Panic,
    /// An explicit failure, from `Machine::fail`: the intent to backtrack
    /// to another alternative. It undoes the cuts it passes through, and the
    /// outermost cut it reaches, having no alternative left, runs again once
    /// what it observed has changed. Other errors also undo the cuts they
    /// pass, but end the outermost one.
    Backtrack,
    /// Pushback, a divergence that a cut recognizes: what it needs is not
    /// there yet, such as items on a port, or `Machine::retry` says so. It
    /// undoes the cuts it passes through, no alternative takes it, and the
    /// outermost cut runs again once what it observed has changed.
    Retry,
    /// A transaction run once, by `Machine::atomic`, conflicted at its commit
    /// with another's and was undone: running it again now may succeed,
    /// where after [`ErrorKind::Backtrack`] or [`ErrorKind::Retry`] it would
    /// wait for a change.
    Conflict,
    /// A host's interrupt (`EvaluationRuntime::interrupt`) reached a turn at
    /// one of its waits: a cut waiting for a change, a blocking read, or an
    /// effect it runs, between quanta. It ends the cuts it passes, as a
    /// failure does, and carries the interrupt's reason as its diagnostic.
    /// Whether the machine is then in error is the turn's to say: it is
    /// where the turn ends with this error, as with any other.
    Interrupted,
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
            backtrack: false,
            retry: false,
            conflict: false,
            interrupted: false,
            awaited: None,
        }
    }

    pub(crate) fn from_eval(values: &CoreValueFactory, error: EvaluationHalt) -> Self {
        if let Some(report) = error.panic_report() {
            return Self::panicked(report.clone());
        }
        let message: Arc<str> = Arc::from("glam evaluation failed");
        let diagnostic = match error.permanent_failure() {
            Some(failure) => Diagnostic::from_failure(
                &Values::from_core_factory(values.clone()),
                Severity::Error,
                failure,
            ),
            None => Diagnostic::new_with_factory(values, Severity::Error, Arc::clone(&message)),
        };
        Self {
            message,
            diagnostic: Some(Arc::new(diagnostic)),
            diagnostics: Vec::new(),
            panic: None,
            backtrack: false,
            // An evaluation that waits on what is not known yet, such as an
            // unresolved promise, is pushback.
            retry: error.permanent_failure().is_none(),
            conflict: false,
            interrupted: false,
            awaited: error
                .unassigned_promise_root()
                .map(|root| crate::evaluation::WorkDependency::Promise(root.clone()))
                .or_else(|| {
                    error
                        .blocked_on()
                        .map(|wait| crate::evaluation::WorkDependency::Wait(wait.0))
                })
                .map(Awaited),
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
            backtrack: false,
            retry: false,
            conflict: false,
            interrupted: false,
            awaited: None,
        }
    }

    pub fn kind(&self) -> ErrorKind {
        if self.panic.is_some() {
            ErrorKind::Panic
        } else if self.interrupted {
            ErrorKind::Interrupted
        } else if self.backtrack {
            ErrorKind::Backtrack
        } else if self.retry {
            ErrorKind::Retry
        } else if self.conflict {
            ErrorKind::Conflict
        } else {
            ErrorKind::Failure
        }
    }

    /// What an evaluation that waited was waiting on.
    pub(crate) fn awaited(&self) -> Option<&crate::evaluation::WorkDependency> {
        self.awaited.as_ref().map(|Awaited(dependency)| dependency)
    }

    /// The cell of the promise that an evaluation which waited ultimately
    /// waited on, following the work that would produce what it needed.
    pub(crate) fn awaited_promise(
        &self,
        values: &CoreValueFactory,
    ) -> Option<crate::core::PromiseId> {
        let Awaited(dependency) = self.awaited.as_ref()?;
        match dependency {
            crate::evaluation::WorkDependency::Promise(promise) => Some(promise.id()),
            _ => values.work_coordinator()?.awaited_promise(dependency),
        }
    }

    /// An explicit failure ([`ErrorKind::Backtrack`]).
    pub(crate) fn fail() -> Self {
        Self {
            backtrack: true,
            ..Self::new("an explicit failure reached no cut")
        }
    }

    /// A transaction run once that conflicted at its commit
    /// ([`ErrorKind::Conflict`]).
    pub(crate) fn conflict() -> Self {
        Self {
            conflict: true,
            ..Self::new("the transaction conflicted with another's commit")
        }
    }

    /// An interrupt that reached a turn ([`ErrorKind::Interrupted`]), with
    /// its reason as `diagnostic`.
    pub(crate) fn interrupted(diagnostic: Diagnostic) -> Self {
        Self {
            interrupted: true,
            diagnostic: Some(Arc::new(diagnostic)),
            ..Self::new("the turn was interrupted")
        }
    }

    /// Pushback, a divergence a cut waits out ([`ErrorKind::Retry`]).
    pub(crate) fn retry(message: impl Into<Arc<str>>) -> Self {
        Self {
            retry: true,
            awaited: None,
            ..Self::new(message)
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
