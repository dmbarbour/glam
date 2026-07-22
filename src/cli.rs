//! Host-facing command-line parsing and command-plan construction.
//!
//! This module parses the bootstrap CLI without performing filesystem or
//! process I/O. `main` remains responsible for executing the returned command.

mod bootstrap;
mod configured;
mod effects;
mod host;
mod model;
mod output;
mod search;

pub use bootstrap::{dispatch_bootstrap, parse_worker_count};
pub use configured::expand_configured;
pub use model::{
    CliArguments, CliError, CliExpansion, CommandPlan, CommandPlanParts, ParseVerbosity,
    TopLevelCommand,
};
pub use output::{HELP_TEXT, format_configured_arguments, format_parse_summary};

#[cfg(test)]
mod tests;
