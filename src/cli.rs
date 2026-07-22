//! Host-facing command-line parsing and command-plan construction.
//!
//! This module parses the bootstrap CLI without performing filesystem or
//! process I/O. `main` remains responsible for executing the returned command.

mod bootstrap;
mod model;
mod output;

pub use bootstrap::{dispatch_bootstrap, parse_worker_count};
pub use model::{
    CliArguments, CliError, CommandPlan, CommandPlanParts, ParseVerbosity, TopLevelCommand,
};
pub use output::{HELP_TEXT, format_parse_summary};

#[cfg(test)]
mod tests;
