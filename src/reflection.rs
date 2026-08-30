//! External freer-effect task interpreter used by reflection clients.
//!
//! Effect requests are ordinary core values sealed by private abstract-global
//! tags. Interaction-net operators only construct those values; this module
//! performs the state, control, transaction, and host operations.

mod lifecycle;
mod machine;
mod protocol;
mod requests;
mod search;
mod store;

pub(crate) use lifecycle::coordinator_task_launcher;
pub use lifecycle::{
    EffectLifecycle, EffectLifecycleStatus, EffectLifecycleTerminal, EffectRun, ScheduledEffectRun,
    run, run_standard,
};
pub(crate) use machine::{task_eval_error, volume_effects};

pub use protocol::{
    CommitResult, EffectRequestSpec, HostSnapshot, ReasoningSessionId, ReflectionEffects,
    RequestContext, RequestResult, StandardEffects, TaskCommit, TaskEnvironment, TaskHalt,
    TaskHost, TaskOutcome, TaskSpecialization, TransactionContext,
};
pub use search::{
    IsolatedEffectSearch, IsolatedSearchBlock, IsolatedSearchBranch, IsolatedSearchPoll,
    IsolatedTaskHost,
};

pub use requests::{
    ReflectionHost, ReflectionJournal, ReflectionQueryMutation, ReflectionQueryWriter,
    ReflectionRequest, ReflectionServices, ReflectionTransaction,
    environment_diagnostic_request_specs, handle_reflection_request, reflection_request_specs,
};
pub(crate) use requests::{parse_severity, prepare_message};
pub use store::{
    CoarseConflictAnalysis, ConflictAddress, ConflictAnalysisStrategy, ConflictObservationIndex,
    ConflictPath, EvaluationQueryHandle, ExactConflictAnalysis, FingerprintConflictAnalysis,
    ReflectionStore, RuntimeInputEndpointId, RuntimeInputSequence, StoreCommitResult, StoreJournal,
    StoreSnapshot, VolumeId,
};
