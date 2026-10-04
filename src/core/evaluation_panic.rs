use std::fmt;
use std::sync::Arc;

/// A panic that interrupted one unit of scheduled evaluation work.
///
/// A panic is a runtime implementation error or a client contract violation,
/// never Glam semantics. This report is task-layer state: it never becomes an
/// `EvaluationFailure`, a lazy result, or a program diagnostic. Only the
/// scheduler's poll boundary creates one. It holds no managed edges, so it may
/// cross every scheduler boundary.
#[derive(Clone)]
pub(crate) struct EvaluationPanic(Arc<EvaluationPanicReport>);

struct EvaluationPanicReport {
    message: Box<str>,
    origin: EvaluationPanicOrigin,
}

/// The scheduled work whose poll caught the panic, by runtime work ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EvaluationPanicOrigin {
    ReflectionTask(u64),
    DeferredWork(u64),
    LazyRoute(u64),
    ClientDemand(u64),
}

impl EvaluationPanic {
    /// Reports a panic caught at `origin`.
    ///
    /// A payload that is already a report was re-raised by [`Self::resume`],
    /// so it keeps the origin that first caught it.
    pub(crate) fn from_payload(
        payload: &(dyn std::any::Any + Send),
        origin: EvaluationPanicOrigin,
    ) -> Self {
        if let Some(report) = payload.downcast_ref::<Self>() {
            return report.clone();
        }
        Self(Arc::new(EvaluationPanicReport {
            message: panic_payload_message(payload).into(),
            origin,
        }))
    }

    pub(crate) fn message(&self) -> &str {
        &self.0.message
    }

    pub(crate) fn origin(&self) -> EvaluationPanicOrigin {
        self.0.origin
    }

    /// Re-raises this panic instead of converting it into a failure.
    ///
    /// A conversion site that would otherwise turn a panic into semantics
    /// calls this. Inside a scheduled poll, the poll boundary catches it again
    /// and keeps this report. `resume_unwind` does not rerun the panic hook,
    /// so the panic prints once.
    pub(crate) fn resume(&self) -> ! {
        std::panic::resume_unwind(Box::new(self.clone()))
    }

    /// Whether two reports describe the same caught panic.
    pub(crate) fn same_panic(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Display for EvaluationPanic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (unit, id) = match self.0.origin {
            EvaluationPanicOrigin::ReflectionTask(id) => ("reflection task", id),
            EvaluationPanicOrigin::DeferredWork(id) => ("deferred work", id),
            EvaluationPanicOrigin::LazyRoute(id) => ("lazy route", id),
            EvaluationPanicOrigin::ClientDemand(id) => ("client demand", id),
        };
        write!(
            formatter,
            "evaluation panicked in {unit} {id}: {}",
            self.0.message
        )
    }
}

impl PartialEq for EvaluationPanic {
    fn eq(&self, other: &Self) -> bool {
        self.same_panic(other)
    }
}

impl Eq for EvaluationPanic {}

impl fmt::Debug for EvaluationPanic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationPanic")
            .field("message", &self.message())
            .field("origin", &self.origin())
            .finish()
    }
}

/// Extracts the text of a panic payload.
pub(crate) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(report) = payload.downcast_ref::<EvaluationPanic>() {
        return report.message();
    }
    payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-text panic payload")
}
