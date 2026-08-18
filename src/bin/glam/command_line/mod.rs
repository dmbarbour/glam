//! Host-facing command-line parsing and command-plan construction.
//!
//! This module parses the bootstrap CLI without performing filesystem or
//! process I/O. `main` remains responsible for executing the returned command.

mod adapters;
mod basic;
mod bootstrap;
mod completion;
mod configured;
mod model;
mod output;

pub(crate) use adapters::builtin_completion_script;
pub(crate) use basic::{CompletionRoute, complete_basic, route_completion};
pub(crate) use bootstrap::{dispatch_bootstrap, parse_worker_count};
#[cfg(test)]
pub(crate) use completion::CompletionKind;
pub(crate) use completion::{CliCompletion, CompletionRequest};
pub(crate) use configured::{complete_configured, expand_configured};
pub(crate) use model::{
    CliArguments, CommandPlan, CommandPlanParts, ParseVerbosity, TopLevelCommand,
};
pub(crate) use output::{
    HELP_TEXT, format_completion_replacements, format_configured_arguments, format_parse_summary,
};

#[cfg(test)]
mod tests;
