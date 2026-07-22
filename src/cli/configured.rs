use crate::api::{Assembler, Value};

use super::model::{CliArguments, CliError, CliExpansion};
use super::search::run_cli_search;

/// Expands one bare command through the already-loaded configuration's
/// `conf.cli` effect. This operation constructs a command plan but does not
/// execute it or activate worker threads.
pub fn expand_configured(
    assembler: &Assembler,
    configuration: &Value,
    arguments: CliArguments,
) -> Result<CliExpansion, CliError> {
    let effect = assembler
        .get(configuration, "conf.cli")
        .ok()
        .filter(|value| !value.is_undefined())
        .ok_or_else(|| CliError::new("configured `conf.cli` did not match the command line"))?;
    let result = run_cli_search(assembler, &effect, arguments)?;
    Ok(CliExpansion::new(result.plan, result.diagnostics))
}
