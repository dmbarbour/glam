mod api;

pub mod compiler;
pub mod core;
pub mod core_net;
pub mod diagnostic;
pub mod eval;
pub mod g_syntax;
pub mod interaction_net;
pub mod list;
pub mod number;

pub use api::{
    Assembler, BuiltModule, DEFAULT_DIAGNOSTIC_CAPACITY, Diagnostic, DiagnosticBuffer,
    DiagnosticSink, DiagnosticSnapshot, Error, Host, HostError, ModuleBuilder, ModuleInput,
    SystemHost, Value, ValueKind,
};
pub use diagnostic::Severity;
