#[cfg(test)]
use chumsky::Parser;

use crate::compiler::CompileContext;
use crate::core::Builtin;
use crate::core::{Atom, Dict, Key, Value};
use crate::diagnostic::Severity;

mod analysis;
mod ast;
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
pub use parser::{parse_source, parse_source_with_context};
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
