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
    Assembler, BuiltModule, DEFAULT_DIAGNOSTIC_CAPACITY, Diagnostic, DiagnosticBuffer,
    DiagnosticSink, DiagnosticSnapshot, Error, Host, HostError, ModuleBuilder, ModuleInput,
    NetBind, NetBuilder, NetCopy, NetPort, SystemHost, Value, ValueKind,
};
pub use core::Builtin;
pub use diagnostic::Severity;
