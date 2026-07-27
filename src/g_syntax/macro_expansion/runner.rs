use std::sync::Arc;

use crate::api::{CompilationExecution, Diagnostic, Value as PublicValue};
use crate::core::{Value, keys};
use crate::diagnostic::Severity;
use crate::eval;
use crate::evaluation::EvaluationPumpOutcome;
use crate::reflection::{IsolatedEffectSearch, IsolatedSearchPoll};

use super::effects::MacroEffects;
use super::host::MacroHost;
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
        frontier: usize,
        cases: impl IntoIterator<Item = PublicValue>,
    ) -> Box<Self> {
        self.frontier = Some(frontier);
        self.cases = unique_values(cases);
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
    let effect = PublicValue::from_core(effect);
    let host = Arc::new(MacroHost::new(
        PublicValue::from_core(environment),
        input.clone(),
    ));
    let mut search = IsolatedEffectSearch::new_in_context(
        &effect,
        MacroEffects,
        host,
        execution.macro_context().clone(),
    )
    .map_err(|error| {
        macro_error(format!(
            "selected macro value is not a runnable effect: {error}"
        ))
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
                    return Err(macro_error(format!("macro effect became blocked {detail}")));
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
                            "macro effect is waiting on a foreign or unavailable lazy producer",
                        ));
                    }
                }
            }
            IsolatedSearchPoll::Complete(branches) => break branches,
            IsolatedSearchPoll::Failed(error) => {
                return Err(macro_error(format!("macro effect failed: {error}")));
            }
            IsolatedSearchPoll::Cancelled => {
                return Err(macro_error("macro effect was cancelled"));
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
        return Err(
            macro_error("macro input did not match any successful alternative")
                .with_context(frontier, cases),
        );
    };
    if successful.next().is_some() {
        return Err(macro_error(
            "macro effect produced multiple results; use `.cut` to select one",
        ));
    }
    let value = branch
        .value()
        .expect("successful branch was selected above")
        .as_core();
    let value = force_result(execution, value.clone()).map_err(|error| {
        error.with_context(
            branch.journal().cursor.consumed_end(&input),
            branch.journal().active_cases.iter().cloned(),
        )
    })?;
    if value != *keys::UNIT_VALUE {
        return Err(macro_error(format!(
            "macro effect terminated with {}, expected unit",
            value.diagnostic_kind_name()
        )));
    }
    if !branch.journal().cursor.balanced() {
        return Err(macro_error("macro reader left an input delimiter unclosed"));
    }
    if !branch.journal().output_is_complete() {
        return Err(macro_error(
            "macro writer left an empty or unclosed layout item",
        ));
    }
    if branch.journal().is_anchor_expansion() && !branch.journal().cursor.at_end(&input) {
        return Err(macro_error(
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
    mut value: Value,
) -> Result<Value, Box<MacroFailure>> {
    loop {
        match eval::eval_value(execution.macro_context(), &value) {
            Ok(next @ (Value::Lazy(_) | Value::Promised(_))) => value = next,
            Ok(value) => return Ok(value),
            Err(error) => {
                let Some(wait) = error.blocked_on() else {
                    return Err(macro_error(format!(
                        "macro result evaluation failed: {error}"
                    )));
                };
                match execution.macro_context().pump_wait(&wait.0, STEP_BUDGET) {
                    EvaluationPumpOutcome::TargetReady
                    | EvaluationPumpOutcome::Busy
                    | EvaluationPumpOutcome::BudgetExhausted => {}
                    EvaluationPumpOutcome::NoProgress => {
                        return Err(macro_error(
                            "macro result is waiting on a foreign or unavailable lazy producer",
                        ));
                    }
                }
            }
        }
    }
}

pub(in crate::g_syntax) fn render_macro_case(
    execution: &CompilationExecution,
    value: &PublicValue,
) -> String {
    let value = match force_result(execution, value.as_core().clone()) {
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

fn unique_values(values: impl IntoIterator<Item = PublicValue>) -> Vec<PublicValue> {
    let mut unique = Vec::new();
    for value in values {
        if !unique.contains(&value) {
            unique.push(value);
        }
    }
    unique
}

fn macro_error(message: impl Into<std::sync::Arc<str>>) -> Box<MacroFailure> {
    Box::new(MacroFailure {
        diagnostic: Diagnostic::new(Severity::Error, message),
        frontier: None,
        cases: Vec::new(),
    })
}
