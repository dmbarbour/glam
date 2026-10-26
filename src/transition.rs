//! The CLI's bridge between turns and the handle API, while the CLI moves to
//! turns piecemeal (`runtime-api-turns-on-machines`): what has moved and what
//! has not share one runtime. Only the CLI uses it, and it goes once nothing
//! in the CLI needs a handle.

use crate::api::{
    Assembler, Diagnostic, DiagnosticsWiring, Error, Machine, ModuleInput, Runtime, Value,
};

/// A runtime around an assembler the caller built, with the default
/// diagnostics wiring, as `Runtime::open` makes one. Used by the CLI's
/// `configuration` module, which builds its assembler with a reflection
/// environment and a diagnostic bus that `RuntimeConfig` does not offer.
pub fn runtime_around(assembler: Assembler) -> Result<Runtime, Error> {
    Runtime::around(assembler, DiagnosticsWiring::ToRoot)
}

/// The assembler a runtime holds. Used by the CLI's `main`, `batch`,
/// `configuration` and `configuration::logger` modules for what has not
/// moved to turns.
pub fn assembler(runtime: &Runtime) -> &Assembler {
    runtime.assembler()
}

/// Compiles `inputs` at `module_path`, eagerly, through the handle API's
/// assembler, pushing the module and returning its warnings, as the turn
/// API's compile did before `api-compile-ops`. Used by the CLI's
/// `configuration` module for its prelude, until `api-cli-sources`.
pub fn compile<I, S>(
    machine: &mut Machine<'_, '_>,
    module_path: I,
    inputs: impl IntoIterator<Item = ModuleInput>,
) -> Result<Vec<Diagnostic>, Error>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    machine.compile_inputs(module_path, inputs, false)
}

/// As [`compile`], over initial definitions popped from the stack. Used by
/// the CLI's `configuration` and `batch` modules for the configuration and
/// the assembly, whose files the handle API's source system reads until
/// `api-cli-sources`.
pub fn compile_over<I, S>(
    machine: &mut Machine<'_, '_>,
    module_path: I,
    inputs: impl IntoIterator<Item = ModuleInput>,
) -> Result<Vec<Diagnostic>, Error>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    machine.compile_inputs(module_path, inputs, true)
}

/// Pops a machine's top value without evaluating it. Used by the CLI's
/// `configuration` module to hand the configuration to what has not moved
/// to turns, and by its `configuration::logger` module to render the
/// diagnostics its default logger reads and write the binaries `stderr`
/// reads.
pub fn pop_value(machine: &mut Machine<'_, '_>) -> Result<Value, Error> {
    machine.pop()
}
