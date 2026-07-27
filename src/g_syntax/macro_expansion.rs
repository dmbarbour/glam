//! Restricted effect execution used by the built-in compiler's source macros.
//!
//! Source recognition and rewriting arrive in later phases. This module first
//! establishes the isolated effect, journal, and compilation-execution
//! boundaries they will use.

mod effects;
mod host;
mod io;
mod runner;

pub(in crate::g_syntax) use io::{
    MacroDelimiter, MacroInput, MacroInputElement, MacroInputKind, MacroOutput,
};
#[cfg(test)]
pub(in crate::g_syntax) use runner::MacroRun;
pub(in crate::g_syntax) use runner::run_macro_effect;

#[cfg(test)]
mod tests;
