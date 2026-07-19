#[cfg(test)]
use chumsky::Parser;

use crate::compiler::CompileContext;
use crate::core::{Atom, Dict, Key, Value};
use crate::core::{Builtin, keys};
use crate::diagnostic::Severity;

mod analysis;
mod ast;
mod compiler_values;
mod diagnostic_formatter;
mod module_lowering;
mod net_lowering;
mod parser;
mod resolve;
mod resolved;

use analysis::{warn_unused_locals, warn_unused_with_alias};
pub use ast::*;
pub use module_lowering::lower_to_core_with_context;
use module_lowering::*;
use resolve::*;

#[cfg(test)]
use net_lowering::ResolvedNetLowerer;
use net_lowering::lower_resolved_expr;
#[cfg(test)]
use parser::definition_decl;
use parser::definition_target_parts;
#[cfg(test)]
use parser::parse_expr;
pub use parser::parse_source;
use resolved::{BindingId, ResolvedExpr, ResolvedPathPart};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredSource {
    pub definitions: Value, // open fixpoint, i.e. \ self -> Dict
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub line: usize,
    pub message: String,
}

pub(crate) fn compile_source(source: &[u8], context: &CompileContext) -> Value {
    let LoweredSource {
        definitions,
        diagnostics,
    } = lower_to_core_with_context(parse_source(source), context);
    for diagnostic in diagnostics {
        let message = crate::diagnostic::text_message(Some(diagnostic.line), &diagnostic.message);
        context.emit_diagnostic(diagnostic.severity, message);
    }
    definitions
}

pub(crate) fn default_diagnostic_formatter() -> Value {
    diagnostic_formatter::value()
}

impl Diagnostic {
    fn warn(line: usize, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            line,
            message: message.into(),
        }
    }

    fn error(line: usize, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            line,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests;
