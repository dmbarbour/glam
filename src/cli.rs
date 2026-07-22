//! Host-facing command-line parsing and command-plan construction.
//!
//! This module parses the bootstrap CLI without performing filesystem or
//! process I/O. `main` remains responsible for executing the returned command.

mod adapters;
mod basic;
mod bootstrap;
mod completion;
mod configured;
mod effects;
mod host;
mod model;
mod output;
mod path;
mod search;
mod token;

pub use adapters::{BUILTIN_COMPLETION_SCRIPTS, builtin_completion_script};
pub use basic::{CompletionRoute, complete_basic, route_completion};
pub use bootstrap::{dispatch_bootstrap, parse_worker_count};
pub use completion::{
    ActiveArgument, CliCompletion, CompletionCandidate, CompletionExpectation, CompletionKind,
    CompletionRequest,
};
pub use configured::{complete_configured, expand_configured};
pub use model::{
    CliArguments, CliError, CliExpansion, CommandPlan, CommandPlanParts, ParseVerbosity,
    TopLevelCommand,
};
pub use output::{
    HELP_TEXT, format_completion_replacements, format_configured_arguments, format_parse_summary,
};

#[cfg(test)]
mod tests;
