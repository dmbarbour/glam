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
mod token_parse;
mod volume_handle;

pub(crate) use lifecycle::coordinator_task_launcher;
pub use lifecycle::{
    EffectLifecycle, EffectLifecycleStatus, EffectRun, EffectRunStop, ScheduledEffectRun, run,
    run_standard,
};
pub(crate) use machine::ContinuationKey;
pub(crate) use machine::volume_effects;
pub use promises::PromiseStatus;
pub(crate) use promises::{Promises, Resolution, SELF_AWAITED, Settled, publish_settled};
pub(crate) use store::contents_at_path;
#[cfg(test)]
pub(crate) use volume_handle::handle_volume;

pub(crate) use machine::HostControl;
pub use protocol::{
    CommitResult, EffectRequestSpec, HostSnapshot, ReasoningSessionId, ReflectionEffects,
    RequestContext, RequestResult, SpecializationRequestInput, SpecializationRequestPoll,
    SpecializationRequestWork, StandardEffects, TaskCommit, TaskEnvironment, TaskHalt, TaskHost,
    TaskOutcome, TaskSpecialization, TaskValidation, TransactionContext, ValidationResult,
};
pub(crate) use search::{HostedEffectRun, HostedPoll};
pub use search::{
    IsolatedEffectSearch, IsolatedSearchBlock, IsolatedSearchBranch, IsolatedSearchPoll,
    IsolatedTaskHost,
};
pub use token_parse::{TokenCandidate, TokenExpectation, TokenRun, run_token_parser};

pub(crate) use requests::anonymous_root;
pub(crate) use requests::is_anonymous_key;
pub(crate) use requests::{KeyListRequestWork, PreparationPoll, parse_evaluated_severity};
pub use requests::{
    Machines, ReflectionHost, ReflectionJournal, ReflectionRequest, ReflectionRequestWork,
    ReflectionServices, ReflectionTransaction, environment_diagnostic_request_specs,
    reflection_request_specs,
};
pub use store::{
    CoarseConflictAnalysis, ConflictAddress, ConflictAnalysisStrategy, ConflictObservationIndex,
    ConflictPath, ExactConflictAnalysis, FingerprintConflictAnalysis, ReflectionStore,
    RuntimeInputEndpointId, RuntimeInputSequence, StoreCommitResult, StoreJournal, StoreSnapshot,
    VolumeId,
};
