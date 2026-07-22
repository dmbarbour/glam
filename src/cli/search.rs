use std::sync::Arc;

use crate::api::{Assembler, Diagnostic, Value};
use crate::core::keys;
use crate::reflection::{IsolatedEffectSearch, IsolatedSearchBranch, IsolatedSearchPoll};

use super::completion::{
    CliCompletion, CompletionCandidate, CompletionExpectation, CompletionRequest, Frontier,
};
use super::effects::CliEffects;
use super::host::{CliHost, CliInvocation, CliJournal};
use super::model::{CliArguments, CliError, CommandPlan, CommandPlanBuilder};

const SEARCH_STEP_BUDGET: usize = 256;

pub(super) struct CliSearchResult {
    pub(super) plan: CommandPlan,
    pub(super) diagnostics: Vec<Diagnostic>,
}

struct SuccessfulBranch {
    result: CliSearchResult,
    explanations: Vec<Value>,
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
    select_branch(assembler, arguments, &branches)
}

pub(super) fn run_cli_completion(
    assembler: &Assembler,
    effect: &Value,
    request: CompletionRequest,
) -> Result<CliCompletion, CliError> {
    let Some(active_argument) = request.active_argument() else {
        return Ok(CliCompletion::new(Vec::new(), Vec::new(), Vec::new()));
    };
    let active = request.arguments_before().len();
    let arguments = request.arguments();
    let branches = run_search(
        assembler,
        effect,
        CliInvocation::for_completion(
            arguments.clone(),
            active,
            active_argument.prefix().to_owned(),
            active_argument.suffix().to_owned(),
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
        if let Some(existing) = candidates.iter_mut().find(|candidate| {
            candidate.replacement() == item.candidate.replacement()
                && candidate.kind() == item.candidate.kind()
        }) {
            existing.merge_explanations(&item.candidate);
        } else {
            candidates.push(item.candidate);
        }
    }
    let mut expectations = Vec::<CompletionExpectation>::new();
    for item in branches
        .iter()
        .flat_map(|branch| branch.journal().expectations.iter())
        .filter(|item| item.frontier == furthest)
    {
        let item = item.public();
        if let Some(existing) = expectations.iter_mut().find(|expectation| {
            expectation.argument() == item.argument()
                && expectation.token_offset() == item.token_offset()
                && expectation.label() == item.label()
        }) {
            existing.merge_explanations(&item);
        } else {
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
    assembler: &Assembler,
    arguments: CliArguments,
    branches: &[crate::reflection::IsolatedSearchBranch<CliEffects>],
) -> Result<CliSearchResult, CliError> {
    let mut successful = Vec::<SuccessfulBranch>::new();
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
                assembler,
                &mut best_invalid,
                journal,
                CliError::new("configured `conf.cli` must return unit"),
            );
            continue;
        }
        if journal.cursor != arguments.args().len() {
            retain_invalid(
                assembler,
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
                retain_invalid(assembler, &mut best_invalid, journal, error);
                continue;
            }
        };
        successful.push(SuccessfulBranch {
            result: CliSearchResult {
                plan,
                diagnostics: journal.reflection.diagnostics().to_vec(),
            },
            explanations: journal.visited_cases.clone(),
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
            .map(|journal| expectation_detail(assembler, journal))
            .unwrap_or_default();
        let explanations = best_failure.map(failure_explanations).unwrap_or_default();
        return Err(CliError::new(format!(
            "configured `conf.cli` did not match the command line{detail}"
        ))
        .with_diagnostics(diagnostics)
        .with_explanations(explanations));
    };
    if successful
        .iter()
        .skip(1)
        .any(|candidate| candidate.result.plan != selected.result.plan)
    {
        let explanations = unique_values(
            successful
                .iter()
                .flat_map(|candidate| candidate.explanations.iter().cloned()),
        );
        let detail = render_explanation_detail(assembler, &explanations);
        return Err(CliError::new(format!(
            "configured `conf.cli` produced more than one distinct command{detail}"
        ))
        .with_explanations(explanations));
    }

    Ok(successful.remove(0).result)
}

fn retain_invalid(
    assembler: &Assembler,
    best: &mut Option<(Frontier, CliError)>,
    journal: &CliJournal,
    error: CliError,
) {
    let frontier = journal_frontier(journal);
    if best
        .as_ref()
        .is_none_or(|(existing, _)| frontier > *existing)
    {
        let explanations = unique_values(journal.active_cases.iter().cloned());
        let detail = render_explanation_detail(assembler, &explanations);
        *best = Some((
            frontier,
            CliError::new(format!("{error}{detail}"))
                .with_diagnostics(journal.reflection.diagnostics().to_vec())
                .with_explanations(explanations),
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

fn expectation_detail(assembler: &Assembler, journal: &CliJournal) -> String {
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
        return render_explanation_detail(assembler, &failure_explanations(journal));
    }
    let expected = format!(
        " at argument {}, byte {}: expected {}",
        frontier.argument + 1,
        frontier.token_offset,
        labels.join(" or ")
    );
    format!(
        "{expected}{}",
        render_explanation_detail(assembler, &failure_explanations(journal))
    )
}

fn failure_explanations(journal: &CliJournal) -> Vec<Value> {
    let frontier = journal_frontier(journal);
    let explanations = unique_values(
        journal
            .expectations
            .iter()
            .filter(|item| item.frontier == frontier)
            .flat_map(|item| item.explanations.iter().cloned()),
    );
    if !explanations.is_empty() {
        return explanations;
    }
    if !journal.active_cases.is_empty() {
        return unique_values(journal.active_cases.iter().cloned());
    }
    Vec::new()
}

fn unique_values(values: impl IntoIterator<Item = Value>) -> Vec<Value> {
    let mut unique = Vec::new();
    for value in values {
        if !unique.contains(&value) {
            unique.push(value);
        }
    }
    unique
}

fn render_explanation_detail(assembler: &Assembler, explanations: &[Value]) -> String {
    explanations
        .iter()
        .map(|explanation| {
            format!(
                "\n  while parsing: {}",
                render_explanation(assembler, explanation)
            )
        })
        .collect()
}

fn render_explanation(assembler: &Assembler, explanation: &Value) -> String {
    let explanation = match assembler.evaluate(explanation) {
        Ok(value) => value,
        Err(error) => return format!("explanation unavailable ({error})"),
    };
    if let Some(text) = utf8_text(&explanation) {
        return text;
    }

    let usage = explanation_field(assembler, &explanation, "usage");
    let summary = explanation_field(assembler, &explanation, "summary");
    let details = explanation_field(assembler, &explanation, "details");
    match (usage, summary, details) {
        (Some(usage), Some(summary), _) => format!("{usage} — {summary}"),
        (Some(usage), None, _) => usage,
        (None, Some(summary), _) => summary,
        (None, None, Some(details)) => details,
        (None, None, None) => {
            "explanation has no textual `usage`, `summary`, or `details`".to_owned()
        }
    }
}

fn explanation_field(assembler: &Assembler, explanation: &Value, field: &str) -> Option<String> {
    let value = assembler.get(explanation, field).ok()?;
    if value.is_undefined() {
        return None;
    }
    let value = assembler.evaluate(&value).ok()?;
    utf8_text(&value)
}

fn utf8_text(value: &Value) -> Option<String> {
    String::from_utf8(value.as_binary()?.to_vec()).ok()
}
