use crate::compiler::CompileContext;
use crate::core::{Atom, CoreValueFactory, Dict, Key, RuntimeValueAccess, Value};
use crate::core::{Builtin, keys};
use crate::diagnostic::Severity;
use crate::runtime::RuntimeValueRoot;

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

use analysis::{
    warn_unused_body_lets, warn_unused_file_lets, warn_unused_locals, warn_unused_with_alias,
};
pub use ast::*;
use module_lowering::*;
use name_analysis::check_file_global_local_shadowing;
use resolve::*;

#[cfg(test)]
use net_lowering::ResolvedNetLowerer;
#[cfg(test)]
use net_lowering::lower_resolved_expr;
use net_lowering::lower_resolved_expr_in;
pub(crate) use parser::inspect_source;
#[cfg(test)]
pub use parser::parse_source;
use resolved::{BindingId, ResolvedExpr, ResolvedPathPart};

pub struct LoweredSource {
    pub diagnostics: Vec<Diagnostic>,
    /// Keeps every managed edge in `definitions` live until the lowered
    /// source is either consumed by `compile_source` or dropped by a caller.
    definitions_root: RuntimeValueRoot,
}

#[cfg(test)]
impl LoweredSource {
    fn definitions(&self, access: &RuntimeValueAccess<'_>) -> Value {
        self.definitions_root.clone_core_with(access)
    }
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
        diagnostics,
        definitions_root,
    } = lower_source(source, context);
    for diagnostic in diagnostics {
        let severity = diagnostic.severity;
        let line = diagnostic.line;
        let text = std::sync::Arc::from(diagnostic.message.as_str());
        context.emit_diagnostic(
            severity,
            crate::compiler::CompileDiagnostic {
                emission: diagnostic.into_emission(context.values()),
                line: Some(line),
                text,
            },
        );
    }
    definitions_root
}

pub(crate) fn default_diagnostic_formatter_root(values: &CoreValueFactory) -> RuntimeValueRoot {
    diagnostic_formatter::root(values)
}

pub(crate) fn defined_or_root(values: &CoreValueFactory) -> RuntimeValueRoot {
    compiler_values::defined_or_root(values)
}

/// A machine's environment, composed from what its machines' shares store,
/// the most specific first (`machines-inherit-an-environment`): an anonymous
/// object extending each share as `object _ extends ...` would, so a share
/// overrides those after it. Each share is wrapped in an anonymous object of
/// its own, so named and anonymous shares mix in any order. A bound share is
/// stored as a list of one, and one never bound reads `{}`, standing for an
/// empty object: a list is never logically `{}`, so telling them apart
/// forces nothing within the share, such as a promised value. Nothing is
/// evaluated.
pub(crate) fn composed_environment_root(
    values: &CoreValueFactory,
    stored: Vec<RuntimeValueRoot>,
) -> RuntimeValueRoot {
    let empty_defs = compiler_values::empty_object_defs_root(values);
    values.with_boundary_access(|access| {
        let anonymous = |deps: Vec<Value>| {
            Value::builtin_call_in(
                &access,
                Builtin::ObjectInstanceFromParts,
                vec![
                    Value::Dict(Dict::new_sync()),
                    Value::List(crate::core::List::from_values(deps)),
                    empty_defs.clone_core_with(&access),
                ],
            )
        };
        let spec =
            |object: Value| Value::builtin_call_in(&access, Builtin::ObjectSpec, vec![object]);
        let levels = stored
            .iter()
            .map(|stored| {
                let bound = Value::builtin_call_in(
                    &access,
                    Builtin::Default,
                    vec![
                        Value::List(crate::core::List::from_values(vec![anonymous(Vec::new())])),
                        stored.clone_core_with(&access),
                    ],
                );
                let share = Value::builtin_call_in(&access, Builtin::ListHead, vec![bound]);
                spec(anonymous(vec![spec(share)]))
            })
            .collect();
        access.root_runtime_value(anonymous(levels))
    })
}

pub(crate) fn require_defined_root(values: &CoreValueFactory) -> RuntimeValueRoot {
    compiler_values::require_defined_root(values)
}

pub(crate) fn fail_effect_root(values: &CoreValueFactory) -> RuntimeValueRoot {
    compiler_values::fail_effect_root(values)
}

#[cfg(test)]
pub(crate) fn initialize_cached_compiler_values(values: &CoreValueFactory) {
    compiler_values::prepare(values);
    values.with_boundary_access(|access| {
        let _ = compiler_values::builtin_module(&access, "std")
            .expect("the cached g compiler must provide `std`");
    });
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

    fn with_emission_root(mut self, emission: RuntimeValueRoot) -> Self {
        self.emission = Some(emission);
        self
    }

    fn into_emission(self, values: &CoreValueFactory) -> RuntimeValueRoot {
        self.emission.unwrap_or_else(|| {
            values.construct_runtime_value_root(|access| {
                crate::diagnostic::text_message_in(access, Some(self.line), &self.message)
            })
        })
    }
}

#[cfg(test)]
mod tests;
