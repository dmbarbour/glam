//! The CLI's bridge between turns and the handle API, while the CLI moves to
//! turns piecemeal (`runtime-api-turns-on-machines`): what has moved and what
//! has not share one runtime. Only the CLI uses it, and it goes once nothing
//! in the CLI needs a handle.

use crate::api::{Assembler, Error, Machine, Runtime, Value};

/// A runtime around an assembler the caller built. Used by the CLI's
/// `configuration` module, which builds its assembler with a reflection
/// environment and a diagnostic bus that `RuntimeConfig` does not offer.
pub fn runtime_around(assembler: Assembler) -> Runtime {
    Runtime::around(assembler)
}

/// The assembler a runtime holds. Used by the CLI's `main`, `batch`,
/// `configuration` and `configuration::logger` modules for what has not
/// moved to turns.
pub fn assembler(runtime: &Runtime) -> &Assembler {
    runtime.assembler()
}

/// Pops a machine's top value without evaluating it. Used by the CLI's
/// `configuration` module to hand the configuration, and the logger it
/// selects, to what has not moved to turns.
pub fn pop_value(machine: &mut Machine<'_>) -> Result<Value, Error> {
    machine.pop()
}
