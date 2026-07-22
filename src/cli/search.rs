use std::sync::Arc;

use crate::api::{Assembler, Diagnostic, Value};
use crate::core::keys;
use crate::reflection::{IsolatedEffectSearch, IsolatedSearchPoll};

use super::effects::CliEffects;
use super::host::{CliHost, CliInvocation, CliJournal};
use super::model::{CliArguments, CliError, CommandPlan, CommandPlanBuilder};

const SEARCH_STEP_BUDGET: usize = 256;

pub(super) struct CliSearchResult {
    pub(super) plan: CommandPlan,
    pub(super) diagnostics: Vec<Diagnostic>,
}

pub(super) fn run_cli_search(
    assembler: &Assembler,
    effect: &Value,
    arguments: CliArguments,
) -> Result<CliSearchResult, CliError> {
    let host = Arc::new(CliHost::new(
        assembler.reflection_environment(),
        CliInvocation::new(arguments.shared_args()),
    ));
    let mut search =
        IsolatedEffectSearch::new_in_context(effect, CliEffects, host, assembler.eval_context())
            .map_err(|error| CliError::new(format!("configured CLI could not start: {error}")))?;

    loop {
        match search.poll(SEARCH_STEP_BUDGET) {
            IsolatedSearchPoll::Yielded => {}
            IsolatedSearchPoll::Blocked(blocked) => {
                let detail = if let Some(error) = blocked.error() {
                    format!(": {error}")
                } else if blocked.waiting_on_dependency() {
                    ": it is waiting on an unavailable lazy dependency".to_owned()
                } else {
                    ": it is waiting on state unavailable to the isolated CLI session".to_owned()
                };
                return Err(CliError::new(format!(
                    "configured CLI became blocked{detail}"
                )));
            }
            IsolatedSearchPoll::Complete(branches) => {
                return select_branch(arguments, &branches);
            }
            IsolatedSearchPoll::Failed(error) => {
                return Err(CliError::new(format!("configured CLI failed: {error}")));
            }
            IsolatedSearchPoll::Cancelled => {
                return Err(CliError::new("configured CLI was cancelled"));
            }
        }
    }
}

fn select_branch(
    arguments: CliArguments,
    branches: &[crate::reflection::IsolatedSearchBranch<CliEffects>],
) -> Result<CliSearchResult, CliError> {
    let mut successful = Vec::<CliSearchResult>::new();
    let mut best_failure: Option<&CliJournal> = None;
    let mut best_invalid: Option<(usize, CliError)> = None;

    for branch in branches {
        let journal = branch.journal();
        let Some(value) = branch.value() else {
            if best_failure.is_none_or(|best| journal.cursor > best.cursor) {
                best_failure = Some(journal);
            }
            continue;
        };
        if value.as_core() != &*keys::UNIT_VALUE {
            retain_invalid(
                &mut best_invalid,
                journal,
                CliError::new("configured `conf.cli` must return unit"),
            );
            continue;
        }
        if journal.cursor != arguments.args().len() {
            retain_invalid(
                &mut best_invalid,
                journal,
                CliError::new(format!(
                    "configured `conf.cli` left {} command-line argument(s) unconsumed",
                    arguments.args().len() - journal.cursor
                )),
            );
            continue;
        }

        let mut builder = CommandPlanBuilder::default();
        let mut invalid = None;
        for edit in journal.edits.iter().cloned() {
            if let Err(error) = builder.push(edit) {
                invalid = Some(error);
                break;
            }
        }
        let plan = match invalid.map_or_else(|| builder.finish_configured(arguments.clone()), Err) {
            Ok(plan) => plan,
            Err(error) => {
                retain_invalid(&mut best_invalid, journal, error);
                continue;
            }
        };
        successful.push(CliSearchResult {
            plan,
            diagnostics: journal.reflection.diagnostics().to_vec(),
        });
    }

    let Some(selected) = successful.first() else {
        if let Some((_, error)) = best_invalid {
            return Err(error);
        }
        let diagnostics = best_failure
            .map(|journal| journal.reflection.diagnostics().to_vec())
            .unwrap_or_default();
        return Err(
            CliError::new("configured `conf.cli` did not match the command line")
                .with_diagnostics(diagnostics),
        );
    };
    if successful
        .iter()
        .skip(1)
        .any(|candidate| candidate.plan != selected.plan)
    {
        return Err(CliError::new(
            "configured `conf.cli` produced more than one distinct command",
        ));
    }

    Ok(successful.remove(0))
}

fn retain_invalid(best: &mut Option<(usize, CliError)>, journal: &CliJournal, error: CliError) {
    if best
        .as_ref()
        .is_none_or(|(cursor, _)| journal.cursor > *cursor)
    {
        *best = Some((
            journal.cursor,
            error.with_diagnostics(journal.reflection.diagnostics().to_vec()),
        ));
    }
}
