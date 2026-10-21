//! External freer-effect task interpreter used by reflection clients.
//!
//! Effect requests are ordinary core values sealed by private abstract-global
//! tags. Interaction-net operators only construct those values; this module
//! performs the state, control, transaction, and host operations.

mod lifecycle;
mod machine;
mod promises;
mod protocol;
mod requests;
mod search;
mod store;
mod volume_handle;

pub(crate) use lifecycle::coordinator_task_launcher;
pub use lifecycle::{
    EffectLifecycle, EffectLifecycleStatus, EffectRun, ScheduledEffectRun, run, run_standard,
};
pub(crate) use machine::ContinuationKey;
pub(crate) use machine::volume_effects;
pub use promises::PromiseStatus;
pub(crate) use promises::{
    Promises, Resolution, SELF_AWAITED, Settled, publish_settled, resolver_promise,
};
pub(crate) use store::{DeathNotice, contents_at_path};
#[cfg(test)]
pub(crate) use volume_handle::handle_volume;

pub use protocol::{
    CommitResult, EffectRequestSpec, HostSnapshot, ReasoningSessionId, ReflectionEffects,
    RequestContext, RequestResult, SpecializationRequestInput, SpecializationRequestPoll,
    SpecializationRequestWait, SpecializationRequestWork, StandardEffects, TaskCommit,
    TaskEnvironment, TaskHalt, TaskHost, TaskOutcome, TaskSpecialization, TaskValidation,
    TransactionContext, ValidationResult,
};
pub use search::{
    IsolatedEffectSearch, IsolatedSearchBlock, IsolatedSearchBranch, IsolatedSearchPoll,
    IsolatedTaskHost,
};

#[cfg(test)]
pub(crate) use requests::assert_task_handle_family_shape;
#[cfg(test)]
pub(crate) use requests::task_status_public_value_for_test;
pub(crate) use requests::{
    KeyListRequestWork, PreparationPoll, ValuePathRequestWork, parse_evaluated_severity,
};
pub use requests::{
    ReflectionHost, ReflectionJournal, ReflectionQueryMutation, ReflectionQueryWriter,
    ReflectionRequest, ReflectionRequestWork, ReflectionServices, ReflectionTransaction,
    environment_diagnostic_request_specs, reflection_request_specs,
};
pub use store::{
    CoarseConflictAnalysis, ConflictAddress, ConflictAnalysisStrategy, ConflictObservationIndex,
    ConflictPath, EvaluationQueryHandle, ExactConflictAnalysis, FingerprintConflictAnalysis,
    ReflectionStore, RuntimeInputEndpointId, RuntimeInputSequence, StoreCommitResult, StoreJournal,
    StoreSnapshot, VolumeId,
};
