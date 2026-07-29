use std::fmt;
use std::sync::Arc;

use crate::core_net::CoreWaitToken;

use super::{EvaluationFailure, PromisedValue, Value};

/// Explains why a demand could not currently produce a value.
///
/// A permanent failure may enter a terminal cache. Blocked waits and
/// unassigned promises are retryable scheduler state and must not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationHalt {
    kind: EvaluationHaltKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvaluationHaltKind {
    Failure(Arc<EvaluationFailure>),
    Blocked(CoreWaitToken),
    UnassignedPromise(PromisedValue),
}

impl EvaluationHalt {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self::failure(Arc::new(EvaluationFailure::message(message.into())))
    }

    pub(crate) fn from_value(value: Value) -> Self {
        Self::failure(Arc::new(EvaluationFailure::emission(value)))
    }

    pub(crate) fn failure(failure: Arc<EvaluationFailure>) -> Self {
        Self {
            kind: EvaluationHaltKind::Failure(failure),
        }
    }

    pub(crate) fn blocked(wait: CoreWaitToken) -> Self {
        Self {
            kind: EvaluationHaltKind::Blocked(wait),
        }
    }

    pub(crate) fn with_context(self, context: Value) -> Self {
        match self.kind {
            EvaluationHaltKind::Failure(failure) => {
                Self::failure(Arc::new(failure.with_context(context)))
            }
            EvaluationHaltKind::Blocked(wait) => Self {
                kind: EvaluationHaltKind::Blocked(wait),
            },
            EvaluationHaltKind::UnassignedPromise(promise) => Self {
                kind: EvaluationHaltKind::UnassignedPromise(promise),
            },
        }
    }

    pub(crate) fn into_permanent_failure(self) -> Arc<EvaluationFailure> {
        match self.kind {
            EvaluationHaltKind::Failure(failure) => failure,
            other => Arc::new(EvaluationFailure::message(Self { kind: other }.to_string())),
        }
    }

    pub(crate) fn permanent_failure(&self) -> Option<&Arc<EvaluationFailure>> {
        match &self.kind {
            EvaluationHaltKind::Failure(failure) => Some(failure),
            EvaluationHaltKind::Blocked(_) | EvaluationHaltKind::UnassignedPromise(_) => None,
        }
    }

    pub(crate) fn blocked_on(&self) -> Option<CoreWaitToken> {
        match &self.kind {
            EvaluationHaltKind::Blocked(wait) => Some(wait.clone()),
            EvaluationHaltKind::Failure(_) | EvaluationHaltKind::UnassignedPromise(_) => None,
        }
    }

    pub(crate) fn unassigned_promise(&self) -> Option<&PromisedValue> {
        match &self.kind {
            EvaluationHaltKind::UnassignedPromise(promise) => Some(promise),
            EvaluationHaltKind::Failure(_) | EvaluationHaltKind::Blocked(_) => None,
        }
    }

    pub(crate) fn unassigned(promise: PromisedValue) -> Self {
        Self {
            kind: EvaluationHaltKind::UnassignedPromise(promise),
        }
    }
}

impl fmt::Display for EvaluationHalt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            EvaluationHaltKind::Failure(failure) => failure.fmt(formatter),
            EvaluationHaltKind::Blocked(wait) => {
                write!(
                    formatter,
                    "evaluation is blocked on wait token {}",
                    wait.wait_id()
                )
            }
            EvaluationHaltKind::UnassignedPromise(_) => {
                formatter.write_str("promised value was observed before initialization")
            }
        }
    }
}

impl std::error::Error for EvaluationHalt {}
