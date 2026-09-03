use std::sync::Arc;

use crate::api::{CompilationExecution, Diagnostic, Value as PublicValue, Values};
use crate::core::CoreValueFactory;
use crate::core::Value;
use crate::diagnostic::Severity;
use crate::evaluation::EvaluationPumpOutcome;
use crate::reflection::{IsolatedEffectSearch, IsolatedSearchPoll};

use super::effects::MacroEffects;
use super::host::{MacroHost, MacroSnapshot};
use super::io::{MacroInput, MacroOutput};

const STEP_BUDGET: usize = 256;

#[derive(Debug)]
pub(in crate::g_syntax) struct MacroRun {
    diagnostics: Vec<Diagnostic>,
    #[cfg(test)]
    visited_cases: Vec<PublicValue>,
    consumed_end: usize,
    output: Vec<MacroOutput>,
}

#[derive(Debug)]
pub(in crate::g_syntax) struct MacroFailure {
    diagnostic: Diagnostic,
    frontier: Option<usize>,
    cases: Vec<PublicValue>,
}

impl MacroFailure {
    pub(in crate::g_syntax) fn message(&self) -> &str {
        self.diagnostic.message()
    }

    pub(in crate::g_syntax) fn frontier(&self) -> Option<usize> {
        self.frontier
    }

    pub(in crate::g_syntax) fn cases(&self) -> &[PublicValue] {
        &self.cases
    }

    fn with_context(
        mut self: Box<Self>,
        values: &CoreValueFactory,
        frontier: usize,
        cases: impl IntoIterator<Item = PublicValue>,
    ) -> Box<Self> {
        self.frontier = Some(frontier);
        self.cases = unique_values(values, cases);
        self
    }
}

impl MacroRun {
    pub(in crate::g_syntax) fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[cfg(test)]
    pub(super) fn visited_cases(&self) -> &[PublicValue] {
        &self.visited_cases
    }

    pub(in crate::g_syntax) fn consumed_end(&self) -> usize {
        self.consumed_end
    }

    pub(in crate::g_syntax) fn output(&self) -> &[MacroOutput] {
        &self.output
    }
}

pub(in crate::g_syntax) fn run_macro_effect(
    execution: &CompilationExecution,
    effect: Value,
    environment: Value,
    input: MacroInput,
) -> Result<MacroRun, Box<MacroFailure>> {
    let values = execution.macro_context().values();
    let public_values = Values::from_core_factory(values.clone());
    let effect = public_values.wrap(effect);
    let environment = public_values.wrap(environment);
    let host = Arc::new(MacroHost::new_core(
        values.clone(),
        environment.clone(),
        MacroSnapshot {
            environment,
            input: Arc::new(input.clone()),
        },
    ));
    let mut search = IsolatedEffectSearch::new_in_context(
        &effect,
        MacroEffects,
        host,
        execution.macro_context().clone(),
    )
    .map_err(|error| {
        macro_error(
            values,
            format!("selected macro value is not a runnable effect: {error}"),
        )
    })?;

    let branches = loop {
        match search.poll(STEP_BUDGET) {
            IsolatedSearchPoll::Yielded => {}
            IsolatedSearchPoll::Blocked(blocked) => {
                let Some(dependency) = blocked.dependency().cloned() else {
                    let detail = blocked.error().map_or_else(
                        || "without a lazy dependency".to_owned(),
                        |error| format!("after evaluation failed: {error}"),
                    );
                    return Err(macro_error(
                        values,
                        format!("macro effect became blocked {detail}"),
                    ));
                };
                match execution
                    .macro_context()
                    .pump_wait(&dependency, STEP_BUDGET)
                {
                    EvaluationPumpOutcome::TargetReady
                    | EvaluationPumpOutcome::Busy
                    | EvaluationPumpOutcome::BudgetExhausted => {}
                    EvaluationPumpOutcome::NoProgress => {
                        return Err(macro_error(
                            values,
                            "macro effect is waiting on a lazy producer unavailable to the macro demand session",
                        ));
                    }
                }
            }
            IsolatedSearchPoll::Complete(branches) => break branches,
            IsolatedSearchPoll::Failed(error) => {
                return Err(macro_error(values, format!("macro effect failed: {error}")));
            }
            IsolatedSearchPoll::Cancelled => {
                return Err(macro_error(values, "macro effect was cancelled"));
            }
        }
    };

    // Do not replay a macro merely to collect failure diagnostics: demanded
    // reflection annotations commit outside the branch journal and would run
    // twice. Cases remain lazy handles, so retaining the furthest branch
    // frontier in this single execution is the inexpensive safe path.
    let mut successful = branches.iter().filter(|branch| branch.value().is_some());
    let Some(branch) = successful.next() else {
        let frontier = branches
            .iter()
            .map(|branch| branch.journal().cursor.consumed_end(&input))
            .max()
            .unwrap_or_else(|| input.start());
        let cases = branches
            .iter()
            .filter(|branch| branch.journal().cursor.consumed_end(&input) == frontier)
            .flat_map(|branch| branch.journal().active_cases.iter().cloned());
        return Err(macro_error(
            values,
            "macro input did not match any successful alternative",
        )
        .with_context(values, frontier, cases));
    };
    if successful.next().is_some() {
        return Err(macro_error(
            values,
            "macro effect produced multiple results; use `.cut` to select one",
        ));
    }
    let value = branch
        .value()
        .expect("successful branch was selected above");
    let value = public_values.clone_core(value).map_err(|error| {
        macro_error(
            values,
            format!("macro result belongs to another runtime: {error}"),
        )
    })?;
    let value = force_result(execution, value).map_err(|error| {
        error.with_context(
            execution.macro_context().values(),
            branch.journal().cursor.consumed_end(&input),
            branch.journal().active_cases.iter().cloned(),
        )
    })?;
    if value != execution.macro_context().values().unit() {
        return Err(macro_error(
            values,
            format!(
                "macro effect terminated with {}, expected unit",
                value.diagnostic_kind_name()
            ),
        ));
    }
    if !branch.journal().cursor.balanced() {
        return Err(macro_error(
            values,
            "macro reader left an input delimiter unclosed",
        ));
    }
    if !branch.journal().output_is_complete() {
        return Err(macro_error(
            values,
            "macro writer left an empty or unclosed layout item",
        ));
    }
    if branch.journal().is_anchor_expansion() && !branch.journal().cursor.at_end(&input) {
        return Err(macro_error(
            values,
            "anchored macro output requires consuming the complete input item",
        ));
    }
    Ok(MacroRun {
        diagnostics: branch.journal().diagnostics().to_vec(),
        #[cfg(test)]
        visited_cases: branch.journal().visited_cases.clone(),
        consumed_end: branch.journal().cursor.consumed_end(&input),
        output: branch.journal().output().to_vec(),
    })
}

fn force_result(
    execution: &CompilationExecution,
    value: Value,
) -> Result<Value, Box<MacroFailure>> {
    execution
        .macro_context()
        .evaluate_whnf(&value)
        .map_err(|error| {
            let detail = if error.blocked_on().is_some() {
                "macro result is waiting on a lazy producer unavailable to the macro demand session"
                    .to_owned()
            } else {
                format!("macro result evaluation failed: {error}")
            };
            macro_error(execution.macro_context().values(), detail)
        })
}

pub(in crate::g_syntax) fn render_macro_case(
    execution: &CompilationExecution,
    value: &PublicValue,
) -> String {
    let values = Values::from_core_factory(execution.macro_context().values().clone());
    let value = match values
        .clone_core(value)
        .map_err(|error| macro_error(execution.macro_context().values(), error.to_string()))
        .and_then(|value| force_result(execution, value))
    {
        Ok(value) => value,
        Err(error) => return format!("explanation unavailable ({})", error.message()),
    };
    if let Value::Binary(bytes) = &value {
        return String::from_utf8(bytes.to_vec())
            .unwrap_or_else(|_| "explanation is non-UTF-8 binary data".to_owned());
    }
    let Value::Dict(dict) = &value else {
        return format!("explanation has kind {}", value.diagnostic_kind_name());
    };
    let field = |name| {
        dict.get(&crate::core::Key::atom_from_text(name))
            .and_then(|value| force_result(execution, value.clone()).ok())
            .and_then(|value| match value {
                Value::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok(),
                _ => None,
            })
    };
    let usage = field("usage");
    let summary = field("summary");
    let details = field("details");
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

fn unique_values(
    factory: &CoreValueFactory,
    values: impl IntoIterator<Item = PublicValue>,
) -> Vec<PublicValue> {
    let public_values = Values::from_core_factory(factory.clone());
    let mut unique = Vec::new();
    for value in values {
        let core_value = public_values
            .clone_core(&value)
            .expect("macro case values belong to the compilation runtime");
        if !unique.iter().any(|prior| {
            let prior = public_values
                .clone_core(prior)
                .expect("macro case values belong to the compilation runtime");
            core_value == prior
        }) {
            unique.push(value);
        }
    }
    unique
}

fn macro_error(
    values: &CoreValueFactory,
    message: impl Into<std::sync::Arc<str>>,
) -> Box<MacroFailure> {
    Box::new(MacroFailure {
        diagnostic: Diagnostic::new_with_factory(values, Severity::Error, message),
        frontier: None,
        cases: Vec::new(),
    })
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    use crate::api::{EffectTokenDomain, Values};
    use std::sync::Weak;

    fn assert_macro_run_owner(run: &MacroRun) {
        let MacroRun {
            diagnostics,
            visited_cases,
            consumed_end,
            output,
        } = run;
        let _: &Vec<Diagnostic> = diagnostics;
        let _: &Vec<PublicValue> = visited_cases;
        let _: &usize = consumed_end;
        let _: &Vec<MacroOutput> = output;
    }

    fn assert_macro_failure_owner(failure: &MacroFailure) {
        let MacroFailure {
            diagnostic,
            frontier,
            cases,
        } = failure;
        let _: &Diagnostic = diagnostic;
        let _: &Option<usize> = frontier;
        let _: &Vec<PublicValue> = cases;
    }

    fn retained_value(domain: &EffectTokenDomain<Arc<()>>) -> (PublicValue, Weak<()>) {
        let payload = Arc::new(());
        let retained = Arc::downgrade(&payload);
        (domain.issue(payload), retained)
    }

    #[test]
    fn macro_result_owner_inventory_is_compile_exhaustive() {
        let _: fn(&MacroRun) = assert_macro_run_owner;
        let _: fn(&MacroFailure) = assert_macro_failure_owner;
    }

    #[test]
    fn macro_results_and_failures_retain_public_values_until_retirement() {
        let core = crate::core::test_value_factory();
        let values = Values::from_core_factory(core.clone());
        let domain = EffectTokenDomain::new(&values);
        let (visited, retained_visited) = retained_value(&domain);
        let (output, retained_output) = retained_value(&domain);
        let run = MacroRun {
            diagnostics: Vec::new(),
            visited_cases: vec![visited],
            consumed_end: 0,
            output: vec![MacroOutput::Data(output)],
        };

        assert!(retained_visited.upgrade().is_some());
        assert!(retained_output.upgrade().is_some());
        drop(run);
        domain.drain_retired_external_owners_for_test();
        assert!(retained_visited.upgrade().is_none());
        assert!(retained_output.upgrade().is_none());

        let (case, retained_case) = retained_value(&domain);
        let failure = MacroFailure {
            diagnostic: Diagnostic::new_with_factory(&core, Severity::Error, "failed macro"),
            frontier: Some(0),
            cases: vec![case],
        };
        assert!(retained_case.upgrade().is_some());
        drop(failure);
        domain.drain_retired_external_owners_for_test();
        assert!(retained_case.upgrade().is_none());
    }
}
