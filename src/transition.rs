//! The CLI's bridge between turns and the handle API, while the CLI moves to
//! turns piecemeal (`runtime-api-turns-on-machines`): what has moved and what
//! has not share one runtime. Only the CLI uses it, and it goes once nothing
//! in the CLI needs a handle.

use crate::{Assembler, Runtime};

/// A runtime around an assembler the caller built. Used by the CLI's
/// `configuration` module, which builds its assembler with a reflection
/// environment and a diagnostic bus that `RuntimeConfig` does not offer.
pub fn runtime_around(assembler: Assembler) -> Runtime {
    Runtime::around(assembler)
}

/// The assembler a runtime holds. Used by the CLI's `main`, `batch` and
/// `configuration` modules for what has not moved to turns.
pub fn assembler(runtime: &Runtime) -> &Assembler {
    runtime.assembler()
}
