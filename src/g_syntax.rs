use crate::compiler::CompileContext;
use crate::core::{Atom, CoreValueFactory, Dict, Key, Value};
use crate::core::{Builtin, keys};
use crate::diagnostic::Severity;
use crate::runtime::RuntimeValueRoot;

#[cfg(test)]
mod access_inventory;
mod analysis;
mod ast;
mod compiler_values;
mod diagnostic_formatter;
mod keywords;
mod macro_expansion;
mod module_lowering;
mod name_analysis;
mod net_lowering;
mod parser;
mod recursive_do;
mod resolve;
mod resolved;

use analysis::{warn_unused_locals, warn_unused_with_alias};
pub use ast::*;
use module_lowering::*;
use name_analysis::check_file_global_local_shadowing;
use resolve::*;

#[cfg(test)]
use net_lowering::ResolvedNetLowerer;
use net_lowering::lower_resolved_expr;
pub(crate) use parser::inspect_source;
#[cfg(test)]
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
    emission: Option<RuntimeValueRoot>,
}

pub(crate) fn compile_source(source: &[u8], context: &CompileContext) -> RuntimeValueRoot {
    let LoweredSource {
        definitions,
        diagnostics,
    } = lower_source(source, context);
    let definitions = RuntimeValueRoot::new(context.values(), definitions);
    for diagnostic in diagnostics {
        let severity = diagnostic.severity;
        context.emit_diagnostic(severity, diagnostic.into_emission());
    }
    definitions
}

pub(crate) fn default_diagnostic_formatter(values: &CoreValueFactory) -> Value {
    diagnostic_formatter::value(values)
}

pub(crate) fn defined_or_value(values: &CoreValueFactory) -> Value {
    compiler_values::defined_or(values)
}

pub(crate) fn require_defined_value(values: &CoreValueFactory) -> Value {
    compiler_values::require_defined(values)
}

pub(crate) fn fail_effect_value(values: &CoreValueFactory) -> Value {
    compiler_values::effect_value(values, "fail")
}

#[cfg(test)]
pub(crate) fn initialize_cached_compiler_values(values: &CoreValueFactory) {
    let _ = compiler_values::builtin_module(values, "std")
        .expect("the cached g compiler must provide `std`");
}

impl Diagnostic {
    fn warn(line: usize, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            line,
            message: message.into(),
            emission: None,
        }
    }

    fn error(line: usize, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            line,
            message: message.into(),
            emission: None,
        }
    }

    fn with_emission(mut self, values: &CoreValueFactory, emission: Value) -> Self {
        self.emission = Some(RuntimeValueRoot::new(values, emission));
        self
    }

    fn into_emission(self) -> Value {
        self.emission
            .map(RuntimeValueRoot::into_core)
            .unwrap_or_else(|| crate::diagnostic::text_message(Some(self.line), &self.message))
    }
}

#[cfg(test)]
mod tests;
