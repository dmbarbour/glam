mod api;
mod compiler;
mod core;
mod core_net;
mod counted_condvar;
pub mod diagnostic;
mod eval;
mod evaluation;
mod g_source;
mod g_syntax;
mod interaction_net;
mod list;
mod number;
#[cfg(any(test, feature = "glam-prof"))]
#[cfg_attr(
    all(test, not(feature = "glam-prof")),
    expect(
        dead_code,
        reason = "without the profiling feature, tests compile this module only for its counter macro, which the net profile shares"
    )
)]
mod profiling;
pub mod reflection;
mod runtime;
mod source;
#[cfg(test)]
mod test_support;
mod text_pattern;
mod trusted_hash;
mod walk_cycle;

pub use api::{
    Assembler, AssemblerBuilder, BackgroundPumpReport, BackgroundPumpState, BuiltModule,
    DeadlockSnapshot, Diagnostic, DiagnosticBus, DiagnosticCounts, DiagnosticEvent,
    DiagnosticIngress, DiagnosticSubscriber, DiagnosticSubscription, EffectTokenDomain, Error,
    ErrorKind, EvaluatedValue, EvaluationRuntime, ModuleBuilder, ModuleInput, NetBind, NetBuilder,
    NetCopy, NetPort, PromiseResolver, QuiescenceReport, QuiescenceSnapshot, ReasoningFailure,
    ReasoningVolume, ReflectionEnvironmentBuilder, ReflectionInspector, RuntimeDeadlockWork,
    RuntimeDeliveryFailure, RuntimeDeliveryFailureKind, RuntimeDeliveryFailureSnapshot,
    RuntimeDeliveryId, RuntimeDeliveryOutcome, RuntimeDependency, RuntimeDisposition,
    RuntimeDispositionKind, RuntimeEventJournal, RuntimeEventSnapshot, RuntimeInputEndpoint,
    RuntimeInputReader, RuntimeInputSender, RuntimeKillReason, RuntimeMaintenanceError,
    RuntimeMaintenanceErrorKind, RuntimeMaintenanceFailure, RuntimeMaintenanceFailureKind,
    RuntimeMaintenanceReport, RuntimeMaintenanceSnapshot, RuntimeMaintenanceState,
    RuntimeOutputDelivery, RuntimeOutputEndpoint, RuntimeOutputEndpointId, RuntimeOutputWriter,
    RuntimeReadiness, RuntimeReadinessStamp, RuntimeSettlementError, RuntimeTaskCapability,
    RuntimeTaskWait, RuntimeWorkKind, RuntimeWorkState, Value, ValueEvaluator, ValueKind, Values,
};
#[cfg(feature = "glam-prof")]
pub use api::{
    CoordinatorMutationCounts, CoordinatorNotificationCallCounts,
    CoordinatorNotificationProfileSnapshot, CoordinatorWaiterOutcomeCounts,
    EvaluationReductionCounts, ExactRouteMutationProfileSnapshot, InteractionNetProfileSnapshot,
    NetDriverCounts, NetReductionCounts, PhaseTimes, RuntimeProfileSnapshot,
};
pub use diagnostic::Severity;
pub use g_source::{
    GDeclarationKind, GDeclarationSummary, GSourceDiagnostic, GSourceInspection, inspect_g_source,
};
pub use reflection::{RuntimeInputEndpointId, RuntimeInputSequence};
pub use runtime::EvaluationRuntimeId;
pub use source::{
    CONTENT_DIGEST_ALGORITHM, ContentDigest, FileSourceSystem, ImportResolver, ManifestMismatch,
    RelativeSourcePath, SourceArtifact, SourceError, SourceIdentity, SourceSystem,
    check_local_manifest,
};
pub use text_pattern::{MAX_PATTERN_BYTES, MAX_PATTERN_GROUP_DEPTH, TextPattern};
