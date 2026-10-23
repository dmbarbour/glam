//! Source macros for the built-in compiler: the logical input and output
//! structures of staged source expansion, and the macro's run by the pure
//! handler, whose operations live in `eval::macro_machine`
//! (`pure-handler-macro-run`).

mod io;
mod runner;

pub(in crate::g_syntax) use io::{
    MacroDelimiter, MacroInput, MacroInputElement, MacroInputKind, MacroInputLayout, MacroOutput,
};
#[cfg(test)]
pub(in crate::g_syntax) use runner::{MacroFailure, MacroRun};
pub(in crate::g_syntax) use runner::{evaluation_cause, render_macro_case, run_macro_effect};

#[cfg(test)]
mod tests;
