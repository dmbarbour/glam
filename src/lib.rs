mod api;
pub mod cli;
mod compiler;
mod core;
mod core_net;
pub mod diagnostic;
mod eval;
mod evaluation;
mod g_source;
mod g_syntax;
mod interaction_net;
mod list;
mod number;
pub mod reflection;
mod runtime;
mod source;
mod text_pattern;

pub use api::{
    Assembler, AssemblerBuilder, BuiltModule, Diagnostic, DiagnosticBus, DiagnosticCounts,
    DiagnosticEvent, DiagnosticIngress, DiagnosticSubscriber, DiagnosticSubscription, Error,
    EvaluationRuntime, EvaluationRuntimeId, ModuleBuilder, ModuleInput, NetBind, NetBuilder,
    NetCopy, NetPort, PromiseResolver, ReasoningFailure, ReasoningReport, ReasoningStatus,
    ReasoningTask, ReasoningTaskState, ReasoningVolume, ReflectionEnvironmentBuilder,
    ReflectionInspector, RuntimeDeliveryFailure, RuntimeDeliveryFailureKind,
    RuntimeDeliveryFailureSnapshot, RuntimeDeliveryId, RuntimeDeliveryOutcome, RuntimeEventJournal,
    RuntimeEventSnapshot, RuntimeInputEndpoint, RuntimeInputReader, RuntimeInputSender,
    RuntimeLoggerSnapshot, RuntimeOutputDelivery, RuntimeOutputEndpoint, RuntimeOutputEndpointId,
    RuntimeOutputWriter, Value, ValueKind, Values,
};
pub use diagnostic::Severity;
pub use g_source::{
    GDeclarationKind, GDeclarationSummary, GSourceDiagnostic, GSourceInspection, inspect_g_source,
};
pub use reflection::{RuntimeInputEndpointId, RuntimeInputSequence};
pub use source::{
    CONTENT_DIGEST_ALGORITHM, ContentDigest, FileSourceSystem, Host, HostError, HostSourceSystem,
    ImportResolver, ManifestMismatch, RelativeSourcePath, SourceArtifact, SourceError,
    SourceIdentity, SourceSystem, SystemHost, check_local_manifest,
};
