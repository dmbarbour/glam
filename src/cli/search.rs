use std::sync::Arc;

use crate::api::{Assembler, Diagnostic, Value};
use crate::core::keys;
use crate::reflection::{IsolatedEffectSearch, IsolatedSearchBranch, IsolatedSearchPoll};

use super::completion::{CliCompletion, CompletionCandidate, CompletionRequest, Frontier};
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
    let branches = run_search(
        assembler,
        effect,
        CliInvocation::new(arguments.shared_args()),
    )?;
    select_branch(arguments, &branches)
}

pub(super) fn run_cli_completion(
    assembler: &Assembler,
    effect: &Value,
    request: CompletionRequest,
) -> Result<CliCompletion, CliError> {
    let (arguments, active) = request.flattened();
    let branches = run_search(
        assembler,
        effect,
        CliInvocation::for_completion(
            arguments.clone(),
            active,
            request.active_prefix().to_owned(),
            request.active_suffix().to_owned(),
        ),
    )?;
    let furthest = branches
        .iter()
        .flat_map(|branch| {
            let journal = branch.journal();
            journal
                .candidates
                .iter()
                .map(|item| item.frontier)
                .chain(journal.expectations.iter().map(|item| item.frontier))
        })
        .max();
    let Some(furthest) = furthest else {
        return Ok(CliCompletion::new(Vec::new(), Vec::new(), Vec::new()));
    };

    let mut evidence = branches
        .iter()
        .flat_map(|branch| branch.journal().candidates.iter())
        .filter(|item| item.frontier == furthest)
        .cloned()
        .collect::<Vec<_>>();
    evidence.retain(|item| {
        !item.complete_reader
            || completion_candidate_viable(
                assembler,
                effect,
                &arguments,
                active,
                item.candidate.replacement(),
            )
            .unwrap_or(false)
    });

    let mut candidates = Vec::<CompletionCandidate>::new();
    for item in evidence {
        if !candidates.contains(&item.candidate) {
            candidates.push(item.candidate);
        }
    }
    let mut expectations = Vec::new();
    for item in branches
        .iter()
        .flat_map(|branch| branch.journal().expectations.iter())
        .filter(|item| item.frontier == furthest)
    {
        let item = item.public();
        if !expectations.contains(&item) {
            expectations.push(item);
        }
    }
    let diagnostics = branches
        .iter()
        .filter(|branch| journal_frontier(branch.journal()) == furthest)
        .flat_map(|branch| branch.journal().reflection.diagnostics().iter().cloned())
        .collect();
    Ok(CliCompletion::new(candidates, expectations, diagnostics))
}

fn run_search(
    assembler: &Assembler,
    effect: &Value,
    invocation: CliInvocation,
) -> Result<Arc<[IsolatedSearchBranch<CliEffects>]>, CliError> {
    let host = Arc::new(CliHost::new(assembler.reflection_environment(), invocation));
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
                return Ok(branches);
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

fn completion_candidate_viable(
    assembler: &Assembler,
    effect: &Value,
    arguments: &[std::ffi::OsString],
    active: usize,
    replacement: &std::ffi::OsStr,
) -> Result<bool, CliError> {
    let mut arguments = arguments.to_vec();
    arguments[active] = replacement.to_owned();
    let argument_count = arguments.len();
    let cli_arguments = CliArguments::new(arguments.clone().into());
    let branches = run_search(assembler, effect, CliInvocation::new(arguments.into()))?;
    Ok(branches.iter().any(|branch| {
        let journal = branch.journal();
        branch.value().is_some_and(|value| {
            value.as_core() == &*keys::UNIT_VALUE
                && journal.cursor == argument_count
                && plan_is_valid(journal, cli_arguments.clone())
        }) || journal
            .expectations
            .iter()
            .any(|item| item.frontier.argument >= argument_count)
    }))
}

fn plan_is_valid(journal: &CliJournal, arguments: CliArguments) -> bool {
    let mut builder = CommandPlanBuilder::default();
    journal
        .edits
        .iter()
        .cloned()
        .all(|edit| builder.push(edit).is_ok())
        && builder.finish_configured(arguments).is_ok()
}

fn select_branch(
    arguments: CliArguments,
    branches: &[crate::reflection::IsolatedSearchBranch<CliEffects>],
) -> Result<CliSearchResult, CliError> {
    let mut successful = Vec::<CliSearchResult>::new();
    let mut best_failure: Option<&CliJournal> = None;
    let mut best_invalid: Option<(Frontier, CliError)> = None;

    for branch in branches {
        let journal = branch.journal();
        let Some(value) = branch.value() else {
            if best_failure.is_none_or(|best| journal_frontier(journal) > journal_frontier(best)) {
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
        let detail = best_failure
            .and_then(expectation_detail)
            .unwrap_or_default();
        return Err(CliError::new(format!(
            "configured `conf.cli` did not match the command line{detail}"
        ))
        .with_diagnostics(diagnostics));
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

fn retain_invalid(best: &mut Option<(Frontier, CliError)>, journal: &CliJournal, error: CliError) {
    let frontier = journal_frontier(journal);
    if best
        .as_ref()
        .is_none_or(|(existing, _)| frontier > *existing)
    {
        *best = Some((
            frontier,
            error.with_diagnostics(journal.reflection.diagnostics().to_vec()),
        ));
    }
}

fn journal_frontier(journal: &CliJournal) -> Frontier {
    journal
        .expectations
        .iter()
        .map(|item| item.frontier)
        .chain(journal.candidates.iter().map(|item| item.frontier))
        .max()
        .unwrap_or(Frontier {
            argument: journal.cursor,
            token_offset: 0,
        })
}

fn expectation_detail(journal: &CliJournal) -> Option<String> {
    let frontier = journal_frontier(journal);
    let mut labels = journal
        .expectations
        .iter()
        .filter(|item| item.frontier == frontier)
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    labels.sort_unstable();
    labels.dedup();
    if labels.is_empty() {
        return None;
    }
    Some(format!(
        " at argument {}, byte {}: expected {}",
        frontier.argument + 1,
        frontier.token_offset,
        labels.join(" or ")
    ))
}
