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
    Assembler, AssemblerBuilder, BuiltModule, DeadlockSnapshot, Diagnostic, DiagnosticBus,
    DiagnosticCounts, DiagnosticEvent, DiagnosticIngress, DiagnosticSubscriber,
    DiagnosticSubscription, Error, EvaluationRuntime, ModuleBuilder, ModuleInput, NetBind,
    NetBuilder, NetCopy, NetPort, PromiseResolver, QuiescenceReport, QuiescenceSnapshot,
    ReasoningFailure, ReasoningVolume, ReflectionEnvironmentBuilder, ReflectionInspector,
    RuntimeDeadlockWork, RuntimeDeliveryFailure, RuntimeDeliveryFailureKind,
    RuntimeDeliveryFailureSnapshot, RuntimeDeliveryId, RuntimeDeliveryOutcome, RuntimeDependency,
    RuntimeDisposition, RuntimeDispositionKind, RuntimeEventJournal, RuntimeEventSnapshot,
    RuntimeInputEndpoint, RuntimeInputReader, RuntimeInputSender, RuntimeKillReason,
    RuntimeOutputDelivery, RuntimeOutputEndpoint, RuntimeOutputEndpointId, RuntimeOutputWriter,
    RuntimeReadiness, RuntimeReadinessStamp, RuntimeSettlementError, RuntimeSharedResources,
    RuntimeTaskWait, RuntimeWorkKind, RuntimeWorkState, Value, ValueKind, Values,
};
pub use diagnostic::Severity;
pub use g_source::{
    GDeclarationKind, GDeclarationSummary, GSourceDiagnostic, GSourceInspection, inspect_g_source,
};
pub use reflection::{RuntimeInputEndpointId, RuntimeInputSequence};
pub use runtime::EvaluationRuntimeId;
pub use source::{
    CONTENT_DIGEST_ALGORITHM, ContentDigest, FileSourceSystem, Host, HostError, HostSourceSystem,
    ImportResolver, ManifestMismatch, RelativeSourcePath, SourceArtifact, SourceError,
    SourceIdentity, SourceSystem, SystemHost, check_local_manifest,
};
