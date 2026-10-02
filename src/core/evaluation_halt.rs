use std::fmt;
use std::sync::Arc;

use crate::core_net::CoreWaitToken;

use super::{EvaluationFailure, ManagedPromiseRoot, RuntimeValueAccess, Value};

/// Explains why a demand could not currently produce a value.
///
/// A permanent failure may enter a terminal cache. Blocked waits and
/// unassigned promises are retryable scheduler state and must not.
#[derive(Clone)]
pub struct EvaluationHalt {
    kind: EvaluationHaltKind,
}

// Evaluation halts travel through every recursive evaluator result frame.
// Keep uncommon ownership payloads indirect so a blocked-path proof cannot
// silently consume the evaluator's ordinary stack budget.
const _: () = assert!(std::mem::size_of::<EvaluationHalt>() <= 64);

#[derive(Clone)]
enum EvaluationHaltKind {
    Failure(Arc<EvaluationFailure>),
    Blocked(CoreWaitToken),
    UnassignedPromise {
        /// A halt may cross scheduler and client-demand boundaries before it
        /// is translated back into a dependency. Retain the promise's exact
        /// managed owner for that entire interval. Box the uncommon payload
        /// so ordinary evaluator result frames do not pay for its size.
        root: Box<ManagedPromiseRoot>,
    },
}

/// Direct semantic payload retained by one halted evaluation.
///
/// This crate-private classification keeps collector adapters exhaustive
/// without exposing scheduler wait identities as semantic values.
pub(crate) enum EvaluationHaltPayload<'payload> {
    Failure(&'payload EvaluationFailure),
    Blocked,
    UnassignedPromise,
}

impl EvaluationHalt {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self::failure(Arc::new(EvaluationFailure::message(message.into())))
    }

    #[cfg(test)]
    pub(crate) fn from_value(access: &RuntimeValueAccess<'_>, value: Value) -> Self {
        Self::failure(Arc::new(EvaluationFailure::emission_in(access, value)))
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

    pub(crate) fn with_context(self, access: &RuntimeValueAccess<'_>, context: Value) -> Self {
        match self.kind {
            EvaluationHaltKind::Failure(failure) => {
                Self::failure(Arc::new(failure.with_context_in(access, context)))
            }
            EvaluationHaltKind::Blocked(wait) => Self {
                kind: EvaluationHaltKind::Blocked(wait),
            },
            EvaluationHaltKind::UnassignedPromise { root } => Self {
                kind: EvaluationHaltKind::UnassignedPromise { root },
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
            EvaluationHaltKind::Blocked(_) | EvaluationHaltKind::UnassignedPromise { .. } => None,
        }
    }

    pub(crate) fn blocked_on(&self) -> Option<CoreWaitToken> {
        match &self.kind {
            EvaluationHaltKind::Blocked(wait) => Some(wait.clone()),
            EvaluationHaltKind::Failure(_) | EvaluationHaltKind::UnassignedPromise { .. } => None,
        }
    }

    pub(crate) fn unassigned_promise_root(&self) -> Option<&ManagedPromiseRoot> {
        match &self.kind {
            EvaluationHaltKind::UnassignedPromise { root } => Some(root),
            EvaluationHaltKind::Failure(_) | EvaluationHaltKind::Blocked(_) => None,
        }
    }

    pub(crate) fn unassigned_root(root: ManagedPromiseRoot) -> Self {
        Self {
            kind: EvaluationHaltKind::UnassignedPromise {
                root: Box::new(root),
            },
        }
    }

    pub(crate) fn payload(&self) -> EvaluationHaltPayload<'_> {
        match &self.kind {
            EvaluationHaltKind::Failure(failure) => {
                EvaluationHaltPayload::Failure(failure.as_ref())
            }
            EvaluationHaltKind::Blocked(_) => EvaluationHaltPayload::Blocked,
            EvaluationHaltKind::UnassignedPromise { .. } => {
                EvaluationHaltPayload::UnassignedPromise
            }
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
            EvaluationHaltKind::UnassignedPromise { .. } => {
                formatter.write_str("promised value was observed before initialization")
            }
        }
    }
}

impl fmt::Debug for EvaluationHalt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for EvaluationHalt {}
