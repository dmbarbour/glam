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
) -> Result<MacroRun, Box<Diagnostic>> {
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
    .map_err(|error| macro_error(format!("macro effect could not start: {error}")))?;

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

    let mut successful = branches.iter().filter(|branch| branch.value().is_some());
    let Some(branch) = successful.next() else {
        return Err(macro_error("macro effect produced no successful result"));
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
    let value = force_result(execution, value.clone())?;
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
) -> Result<Value, Box<Diagnostic>> {
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

fn macro_error(message: impl Into<std::sync::Arc<str>>) -> Box<Diagnostic> {
    Box::new(Diagnostic::new(Severity::Error, message))
}
