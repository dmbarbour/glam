mod effects;
mod host;
pub(super) mod path;
mod search;
mod token;

use glam::{Assembler, Error, Value, Values};

use super::completion::{CliCompletion, CompletionRequest};
use super::model::{CliArguments, CliError, CliExpansion};
use search::{run_cli_completion, run_cli_search};

/// Expands one bare command through the already-loaded configuration's
/// `conf.cli` effect. This operation constructs a command plan but does not
/// execute it or activate worker threads.
pub(crate) fn expand_configured(
    assembler: &Assembler,
    configuration: &Value,
    arguments: CliArguments,
) -> Result<CliExpansion, CliError> {
    let values = assembler.values();
    let candidate = values
        .access_names(configuration, ["conf", "cli"])
        .and_then(|candidate| with_path_lookup_context(&values, candidate, "conf.cli"))
        .map_err(CliError::from_error)?;
    let fail = values.fail_effect();
    let effect = values
        .apply(&values.defined_or_function(), [fail, candidate])
        .map_err(CliError::from_error)?;
    let result = run_cli_search(assembler, &effect, arguments)?;
    Ok(CliExpansion::new(result.plan, result.diagnostics))
}

/// Analyzes one configured command at a shell-neutral completion cursor.
/// Missing `conf.cli` behaves like `.fail` and returns no candidates.
pub(crate) fn complete_configured(
    assembler: &Assembler,
    configuration: &Value,
    request: CompletionRequest,
) -> Result<CliCompletion, CliError> {
    let values = assembler.values();
    let candidate = values
        .access_names(configuration, ["conf", "cli"])
        .and_then(|candidate| with_path_lookup_context(&values, candidate, "conf.cli"))
        .map_err(CliError::from_error)?;
    let fail = values.fail_effect();
    let effect = values
        .apply(&values.defined_or_function(), [fail, candidate])
        .map_err(CliError::from_error)?;
    run_cli_completion(assembler, &effect, request)
}

fn with_path_lookup_context(values: &Values, value: Value, path: &str) -> Result<Value, Error> {
    let frame = values.record([(
        "eval",
        values.record([
            ("op", values.atom_from_text("path_lookup")),
            ("args", values.record([("path", values.text(path))])?),
        ])?,
    )])?;
    values.anno(values.record([("context", frame)])?, value)
}
