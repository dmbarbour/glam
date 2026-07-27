//! Restricted effect execution used by the built-in compiler's source macros.
//!
//! Source recognition and rewriting arrive in later phases. This module first
//! establishes the isolated effect, journal, and compilation-execution
//! boundaries they will use.

#![expect(
    dead_code,
    reason = "Phase 3 establishes the runner before Phase 4 invokes it from source expansion"
)]

mod effects;
mod host;
mod runner;

#[cfg(test)]
pub(in crate::g_syntax) use runner::{MacroRun, run_macro_effect};

#[cfg(test)]
mod tests;
