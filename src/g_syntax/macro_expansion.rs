//! Restricted effect execution used by the built-in compiler's source macros.
//!
//! It owns the isolated effect runner, branch-local cursor and output journals,
//! and the logical input/output structures used by staged source expansion.

mod effects;
mod host;
mod io;
mod runner;

pub(in crate::g_syntax) use io::{
    MacroDelimiter, MacroInput, MacroInputElement, MacroInputKind, MacroInputLayout, MacroOutput,
};
#[cfg(test)]
pub(in crate::g_syntax) use runner::{MacroFailure, MacroRun};
pub(in crate::g_syntax) use runner::{render_macro_case, run_macro_effect};

#[cfg(test)]
mod tests;
