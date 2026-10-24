mod effects;
mod host;
pub(super) mod path;
mod search;
mod token;

use glam::{Assembler, Error, HandleValue, Values};

use super::completion::{CliCompletion, CompletionRequest};
use super::model::{CliArguments, CliError, CliExpansion};
use search::{run_cli_completion, run_cli_search};

/// Expands one bare command through the already-loaded configuration's
/// `conf.cli` effect. This operation constructs a command plan but does not
/// execute it or activate worker threads.
pub(crate) fn expand_configured(
    assembler: &Assembler,
    configuration: &HandleValue,
    arguments: CliArguments,
) -> Result<CliExpansion, CliError> {
    let values = assembler.values();
    let effect = configured_cli(&values, configuration).map_err(CliError::from_error)?;
    let result = run_cli_search(assembler, &effect, arguments)?;
    Ok(CliExpansion::new(result.plan, result.diagnostics))
}

/// Analyzes one configured command at a shell-neutral completion cursor.
/// Missing `conf.cli` behaves like `.fail` and returns no candidates.
pub(crate) fn complete_configured(
    assembler: &Assembler,
    configuration: &HandleValue,
    request: CompletionRequest,
) -> Result<CliCompletion, CliError> {
    let values = assembler.values();
    let effect = configured_cli(&values, configuration).map_err(CliError::from_error)?;
    run_cli_completion(assembler, &effect, request)
}

/// The configuration's `conf.cli`, or `.fail` where it is undefined
/// (`dict-empty-is-a-value`), a failure in it naming the path looked up.
fn configured_cli(values: &Values, configuration: &HandleValue) -> Result<HandleValue, Error> {
    let path = values.list([values.atom_from_text("conf"), values.atom_from_text("cli")])?;
    let selected = values.apply(
        &values.default_function(),
        [path, values.fail_effect(), configuration.clone()],
    )?;
    with_path_lookup_context(values, selected, "conf.cli")
}

fn with_path_lookup_context(
    values: &Values,
    value: HandleValue,
    path: &str,
) -> Result<HandleValue, Error> {
    let frame = values.record([(
        "eval",
        values.record([
            ("op", values.atom_from_text("path_lookup")),
            ("args", values.record([("path", values.text(path))])?),
        ])?,
    )])?;
    values.anno(values.record([("context", frame)])?, value)
}
