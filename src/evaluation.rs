//! Session-scoped capabilities threaded through semantic evaluation.
//!
//! The session is intentionally empty in this plumbing spike. It is the
//! shared ownership boundary for the reflection executor, heap, diagnostics,
//! cancellation, and blocked-work registry introduced by later slices.

use std::sync::Arc;

#[derive(Debug, Default)]
pub(crate) struct EvaluationSession {
    _private: (),
}

impl EvaluationSession {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// Cheap per-evaluation handle to one shared assembler session.
///
/// Narrower provenance can be added to this handle without duplicating the
/// session-owned scheduler and reflection state.
#[derive(Debug, Clone)]
pub(crate) struct EvalContext {
    #[allow(dead_code)] // Session-owned facilities arrive in subsequent reflection spikes.
    session: Arc<EvaluationSession>,
}

impl EvalContext {
    pub(crate) fn new(session: Arc<EvaluationSession>) -> Self {
        Self { session }
    }

    /// Creates a session for internal clients that do not yet run under an
    /// assembler, notably standalone reflection tasks and focused tests.
    pub(crate) fn standalone() -> Self {
        Self::new(Arc::new(EvaluationSession::new()))
    }

    #[cfg(test)]
    pub(crate) fn shares_session_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.session, &other.session)
    }
}
