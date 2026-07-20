mod api;

/// compiler and g_syntax are exposed for `--parse` and will be
/// removed from the public API in the future (when reflection is
/// implemented).
pub mod compiler;
mod core;
mod core_net;
pub mod diagnostic;
mod eval;
mod evaluation;
pub mod g_syntax;
mod interaction_net;
mod list;
mod number;
pub mod reflection;

pub use api::{
    Assembler, BuiltModule, Diagnostic, DiagnosticBus, DiagnosticCounts, DiagnosticEvent,
    DiagnosticSubscriber, DiagnosticSubscription, Error, EvaluationRuntime, Host, HostError,
    ModuleBuilder, ModuleInput, NetBind, NetBuilder, NetCopy, NetPort, ReasoningFailure,
    ReasoningReport, ReasoningStatus, ReasoningTask, ReasoningTaskState, ReasoningVolume,
    SystemHost, Value, ValueKind,
};
pub use core::Builtin;
pub use diagnostic::Severity;
