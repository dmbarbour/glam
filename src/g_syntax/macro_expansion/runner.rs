//! Runs a source macro by the pure handler (`pure-handler-macro-run`): its
//! successes, at most one accepted, and otherwise its failures' furthest
//! frontier and active cases, read from the run's two views.

use std::sync::Arc;

use crate::api::{CompilationExecution, Diagnostic, EvaluatedValue, Value as PublicValue, Values};
use crate::core::{
    Builtin, BuiltinCall, CoreValueFactory, Dict, EvaluationHalt, Key, LazyValue,
    ManagedPromiseRoot, Value, keys,
};
use crate::diagnostic::Severity;
use crate::eval::{MacroCursor, MacroShape};
use crate::runtime::RuntimeValueRoot;

use super::io::{MacroInput, MacroOutput};

#[derive(Debug)]
pub(in crate::g_syntax) struct MacroRun {
    diagnostics: Vec<Diagnostic>,
    consumed_end: usize,
    output: Vec<MacroOutput>,
}

#[derive(Debug)]
pub(in crate::g_syntax) struct MacroFailure {
    diagnostic: Diagnostic,
    /// The structured failure raised by the macro's own evaluation. The
    /// compiler diagnostic carries it as a nested context message, so the
    /// headline states only the compiler's own finding.
    cause: Option<Diagnostic>,
    frontier: Option<usize>,
    cases: Vec<PublicValue>,
    /// The pending reply the macro's evaluation waited on, for which the
    /// compile waits and runs again (`compile-yields-an-open-mixin`).
    wait: Option<ManagedPromiseRoot>,
}

impl MacroFailure {
    pub(in crate::g_syntax) fn wait(&self) -> Option<&ManagedPromiseRoot> {
        self.wait.as_ref()
    }

    fn waiting_on(mut self: Box<Self>, wait: Option<ManagedPromiseRoot>) -> Box<Self> {
        self.wait = wait;
        self
    }

    pub(in crate::g_syntax) fn message(&self) -> &str {
        self.diagnostic.message()
    }

    pub(in crate::g_syntax) fn cause(&self) -> Option<&Diagnostic> {
        self.cause.as_ref()
    }

    pub(in crate::g_syntax) fn frontier(&self) -> Option<usize> {
        self.frontier
    }

    fn caused_by(mut self: Box<Self>, cause: Option<Diagnostic>) -> Box<Self> {
        self.cause = cause;
        self
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

    pub(in crate::g_syntax) fn consumed_end(&self) -> usize {
        self.consumed_end
    }

    pub(in crate::g_syntax) fn output(&self) -> &[MacroOutput] {
        &self.output
    }
}

/// Runs `effect`, a macro, over `input` with `environment`.
///
/// The run is a lazy value in the macro demand session, so a demanded
/// `anno refl:` within a helper runs once, however many branches read it:
/// reflection work commits outside the branches, so no branch is replayed
/// to collect failure diagnostics.
pub(in crate::g_syntax) fn run_macro_effect(
    execution: &CompilationExecution,
    effect: RuntimeValueRoot,
    environment: RuntimeValueRoot,
    input: MacroInput,
) -> Result<MacroRun, Box<MacroFailure>> {
    let values = execution.macro_context().values();
    let (text, data) = input.split();
    let text = Arc::new(text);
    let run = values.construct_runtime_value_root(|access| {
        let data = data
            .iter()
            .map(|value| value.clone().into_runtime_root().clone_core_with(access))
            .collect();
        Value::Lazy(LazyValue::from_builtin_in(
            access,
            BuiltinCall {
                builtin: Builtin::Handler,
                arguments: Arc::from([
                    crate::eval::macro_api_in(access),
                    Value::Dict(Dict::new_sync()),
                    crate::eval::macro_state_in(
                        access,
                        values,
                        Arc::clone(&text),
                        data,
                        environment.clone_core_with(access),
                    ),
                    effect.clone_core_with(access),
                ]),
            },
        ))
    });
    let reader = Reader { execution };
    let results = reader.whnf(run)?;
    let successes = reader.items(reader.field(&results, &keys::OK)?)?;

    let mut successes = successes.into_iter();
    let Some(success) = successes.next() else {
        let mut branches = Vec::new();
        for failure in reader.items(reader.field(&results, &keys::FAILED)?)? {
            let failure = reader.whnf(failure)?;
            let handler = reader.whnf(reader.field(&failure, &keys::HANDLER)?)?;
            let end = reader.cursor(&handler)?.consumed_end(&text);
            branches.push((end, reader.cases(&handler)?));
        }
        let frontier = branches
            .iter()
            .map(|(end, _)| *end)
            .max()
            .unwrap_or_else(|| text.start());
        let cases = branches
            .into_iter()
            .filter(|(end, _)| *end == frontier)
            .flat_map(|(_, cases)| cases);
        return Err(macro_error(
            values,
            "macro input did not match any successful alternative",
        )
        .with_context(values, frontier, cases));
    };
    if successes.next().is_some() {
        return Err(macro_error(
            values,
            "macro effect produced multiple results; use `.cut` to select one",
        ));
    }
    let success = reader.whnf(success)?;
    let handler = reader.whnf(reader.field(&success, &keys::HANDLER)?)?;
    let cursor = reader.cursor(&handler)?;
    let shape = reader.shape(&handler)?;
    let value = match force_result(execution, reader.field(&success, &keys::VALUE)?) {
        Ok(value) => value,
        Err(error) => {
            return Err(error.with_context(
                values,
                cursor.consumed_end(&text),
                reader.cases(&handler)?,
            ));
        }
    };
    let (is_unit, kind) = values.with_boundary_access(|access| {
        value
            .with_core(&access, |value| {
                (
                    access.same_representation(value, &access.unit()),
                    access.diagnostic_kind_name(value),
                )
            })
            .expect("macro result belongs to the macro runtime")
    });
    if !is_unit {
        return Err(macro_error(
            values,
            format!("macro effect terminated with {kind}, expected unit"),
        ));
    }
    if !cursor.balanced() {
        return Err(macro_error(
            values,
            "macro reader left an input delimiter unclosed",
        ));
    }
    if !shape.output_is_complete() {
        return Err(macro_error(
            values,
            "macro writer left an empty or unclosed layout item",
        ));
    }
    if shape.is_anchor_expansion() && !cursor.at_end(&text) {
        return Err(macro_error(
            values,
            "anchored macro output requires consuming the complete input item",
        ));
    }
    let mut output = Vec::new();
    for item in reader.items(reader.field(&handler, &keys::OUTPUT)?)? {
        output.push(reader.output(item)?);
    }
    output.reverse();
    let mut diagnostics = Vec::new();
    for entry in reader.items(reader.field(&handler, &keys::LOGS)?)? {
        diagnostics.push(reader.log(entry)?);
    }
    diagnostics.reverse();
    Ok(MacroRun {
        diagnostics,
        consumed_end: cursor.consumed_end(&text),
        output,
    })
}

/// Reads a macro's run, evaluating as it goes in the macro demand session.
struct Reader<'a> {
    execution: &'a CompilationExecution,
}

impl Reader<'_> {
    fn values(&self) -> &CoreValueFactory {
        self.execution.macro_context().values()
    }

    /// `root` evaluated: a failure of the macro's evaluation is the macro's,
    /// and a wait the compile's.
    fn whnf(&self, root: RuntimeValueRoot) -> Result<RuntimeValueRoot, Box<MacroFailure>> {
        self.execution
            .macro_context()
            .evaluate_root_whnf(root)
            .map_err(|error| self.failure(&error))
    }

    fn failure(&self, error: &EvaluationHalt) -> Box<MacroFailure> {
        // A panic is never a macro failure: it re-raises to the enclosing
        // poll boundary instead.
        if let Some(report) = error.panic_report() {
            report.resume();
        }
        let values = self.values();
        if error.blocked_on().is_some() {
            return macro_error(
                values,
                "macro effect is waiting on a lazy producer unavailable to the macro demand session",
            )
            .waiting_on(self.execution.awaited_reply(error));
        }
        match evaluation_cause(values, error) {
            Some(cause) => macro_error(values, "macro effect failed").caused_by(Some(cause)),
            None => macro_error(values, format!("macro effect failed: {error}")),
        }
    }

    /// The member `key` of `dict`, evaluated already.
    fn field(
        &self,
        dict: &RuntimeValueRoot,
        key: &Key,
    ) -> Result<RuntimeValueRoot, Box<MacroFailure>> {
        self.values()
            .with_boundary_access(|access| {
                dict.with_core(&access, |value| match value {
                    Value::Dict(dict) => dict
                        .get(key)
                        .map(|value| access.root_runtime_value(access.duplicate_value(value))),
                    _ => None,
                })
                .flatten()
            })
            .ok_or_else(|| macro_error(self.values(), format!("macro run has no `{key:?}`")))
    }

    /// The items of the list `list`, each unevaluated.
    fn items(&self, list: RuntimeValueRoot) -> Result<Vec<RuntimeValueRoot>, Box<MacroFailure>> {
        let public = Values::from_core_factory(self.values().clone());
        let array = public
            .anno_array(PublicValue::from_runtime_root(list))
            .map_err(|error| macro_error(self.values(), error.to_string()))?;
        let array = self.whnf(array.into_runtime_root())?;
        self.values()
            .with_boundary_access(|access| {
                array
                    .with_core(&access, |value| match value {
                        Value::List(list) => list.value_slice().map(|items| {
                            items
                                .iter()
                                .map(|item| access.root_runtime_value(access.duplicate_value(item)))
                                .collect::<Vec<_>>()
                        }),
                        _ => None,
                    })
                    .flatten()
            })
            .ok_or_else(|| macro_error(self.values(), "macro run's results are not a list"))
    }

    fn opaque<T: std::any::Any + Send + Sync>(
        &self,
        handler: &RuntimeValueRoot,
        key: &Key,
    ) -> Result<Arc<T>, Box<MacroFailure>> {
        let field = self.field(handler, key)?;
        let values = self.values();
        values
            .with_boundary_access(|access| {
                field
                    .with_core(&access, |value| match value {
                        Value::Opaque(value) => value.downcast::<T>(values),
                        _ => None,
                    })
                    .flatten()
            })
            .ok_or_else(|| macro_error(values, "macro run's state is not its own"))
    }

    fn cursor(&self, handler: &RuntimeValueRoot) -> Result<Arc<MacroCursor>, Box<MacroFailure>> {
        self.opaque(handler, &keys::CURSOR)
    }

    fn shape(&self, handler: &RuntimeValueRoot) -> Result<Arc<MacroShape>, Box<MacroFailure>> {
        self.opaque(handler, &keys::SHAPE)
    }

    /// A branch's active cases, outermost first.
    fn cases(&self, handler: &RuntimeValueRoot) -> Result<Vec<PublicValue>, Box<MacroFailure>> {
        let mut cases: Vec<_> = self
            .items(self.field(handler, &keys::CASES)?)?
            .into_iter()
            .map(PublicValue::from_runtime_root)
            .collect();
        cases.reverse();
        Ok(cases)
    }

    /// One written item, `{text}`, `{data}`, `{sep}`, `{anchor}`,
    /// `{layout_start}` or `{layout_end}`.
    fn output(&self, item: RuntimeValueRoot) -> Result<MacroOutput, Box<MacroFailure>> {
        let item = self.whnf(item)?;
        let values = self.values();
        let kind = values.with_boundary_access(|access| {
            item.with_core(&access, |value| match value {
                Value::Dict(item) => [
                    &*keys::TEXT,
                    &*keys::DATA,
                    &*keys::SEP,
                    &*keys::ANCHOR,
                    &*keys::LAYOUT_START,
                    &*keys::LAYOUT_END,
                ]
                .into_iter()
                .find(|key| item.get(key).is_some())
                .cloned(),
                _ => None,
            })
            .flatten()
        });
        let foreign = || macro_error(values, "macro run's output is not its own");
        let kind = kind.ok_or_else(foreign)?;
        Ok(if kind == *keys::TEXT {
            let text = self.whnf(self.field(&item, &keys::TEXT)?)?;
            let text = values
                .with_boundary_access(|access| {
                    text.with_core(&access, |value| match value {
                        Value::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok(),
                        _ => None,
                    })
                    .flatten()
                })
                .ok_or_else(foreign)?;
            MacroOutput::Text(text)
        } else if kind == *keys::DATA {
            MacroOutput::Data(PublicValue::from_runtime_root(
                self.field(&item, &keys::DATA)?,
            ))
        } else if kind == *keys::SEP {
            MacroOutput::Separator
        } else if kind == *keys::ANCHOR {
            MacroOutput::Anchor
        } else if kind == *keys::LAYOUT_START {
            MacroOutput::LayoutStart
        } else {
            MacroOutput::LayoutEnd
        })
    }

    /// One `.log` entry, `{severity, message}`, as its diagnostic: the
    /// message an object, its `msg` evaluated.
    fn log(&self, entry: RuntimeValueRoot) -> Result<Diagnostic, Box<MacroFailure>> {
        let entry = self.whnf(entry)?;
        let values = self.values();
        let public = Values::from_core_factory(values.clone());
        let invalid = |message: String| {
            macro_error(values, "macro effect failed").caused_by(Some(
                Diagnostic::new_with_factory(values, Severity::Error, message),
            ))
        };
        let severity = self.whnf(self.field(&entry, &keys::SEVERITY)?)?;
        let severity = EvaluatedValue::from_whnf(&public, PublicValue::from_runtime_root(severity));
        let severity = crate::reflection::parse_evaluated_severity(&severity)
            .map_err(|error| invalid(error.to_string()))?;
        let message = self.whnf(self.field(&entry, &keys::MESSAGE)?)?;
        let interface = values.with_boundary_access(|access| {
            message
                .with_core(&access, |value| match value {
                    Value::Dict(message) => Some(message.get(&keys::MSG).map(|interface| {
                        access.root_runtime_value(access.duplicate_value(interface))
                    })),
                    _ => None,
                })
                .flatten()
        });
        let Some(interface) = interface else {
            return Err(invalid(
                "`.log` message must evaluate to an object".to_owned(),
            ));
        };
        let message = match interface {
            None => message,
            Some(interface) => {
                let interface = self.whnf(interface)?;
                values.construct_runtime_value_root(|access| {
                    let Value::Dict(dict) = message.clone_core_with(access) else {
                        unreachable!("checked to be a dictionary above")
                    };
                    Value::Dict(dict.insert(keys::MSG.clone(), interface.clone_core_with(access)))
                })
            }
        };
        Diagnostic::from_emission(&public, severity, PublicValue::from_runtime_root(message))
            .map_err(|error| invalid(error.to_string()))
    }
}

fn force_result(
    execution: &CompilationExecution,
    value: RuntimeValueRoot,
) -> Result<RuntimeValueRoot, Box<MacroFailure>> {
    execution
        .macro_context()
        .evaluate_root_whnf(value)
        .map_err(|error| {
            // A panic is never a macro failure: it re-raises to the
            // enclosing poll boundary instead.
            if let Some(report) = error.panic_report() {
                report.resume();
            }
            let values = execution.macro_context().values();
            let cause = evaluation_cause(values, &error);
            let detail = if error.blocked_on().is_some() {
                "macro result is waiting on a lazy producer unavailable to the macro demand session"
                    .to_owned()
            } else if cause.is_some() {
                "macro result evaluation failed".to_owned()
            } else {
                format!("macro result evaluation failed: {error}")
            };
            macro_error(values, detail)
                .caused_by(cause)
                .waiting_on(execution.awaited_reply(&error))
        })
}

/// Projects an evaluator failure into the structured diagnostic that a macro
/// compiler diagnostic carries as its cause. Waits and unassigned promises
/// have no permanent failure, so they have no cause.
pub(in crate::g_syntax) fn evaluation_cause(
    values: &CoreValueFactory,
    halt: &EvaluationHalt,
) -> Option<Diagnostic> {
    halt.permanent_failure().map(|failure| {
        Diagnostic::from_failure(
            &Values::from_core_factory(values.clone()),
            Severity::Error,
            failure,
        )
    })
}

pub(in crate::g_syntax) fn render_macro_case(
    execution: &CompilationExecution,
    value: &PublicValue,
) -> String {
    let value = match force_result(execution, value.clone().into_runtime_root()) {
        Ok(value) => value,
        Err(error) => return format!("explanation unavailable ({})", error.message()),
    };
    let immediate = execution
        .macro_context()
        .values()
        .with_boundary_access(|access| {
            value
                .with_core(&access, |value| match value {
                    Value::Binary(bytes) => Ok(String::from_utf8(bytes.to_vec())
                        .unwrap_or_else(|_| "explanation is non-UTF-8 binary data".to_owned())),
                    Value::Dict(_) => Err(None),
                    value => Err(Some(access.diagnostic_kind_name(value))),
                })
                .expect("macro explanation belongs to the macro runtime")
        });
    match immediate {
        Ok(text) => return text,
        Err(Some(kind)) => return format!("explanation has kind {kind}"),
        Err(None) => {}
    }
    let field = |name| {
        select_field_root(execution, &value, name)
            .and_then(|value| force_result(execution, value).ok())
            .and_then(|value| {
                execution
                    .macro_context()
                    .values()
                    .with_boundary_access(|access| {
                        value
                            .with_core(&access, |value| match value {
                                Value::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok(),
                                _ => None,
                            })
                            .flatten()
                    })
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

fn select_field_root(
    execution: &CompilationExecution,
    value: &RuntimeValueRoot,
    name: &str,
) -> Option<RuntimeValueRoot> {
    execution
        .macro_context()
        .values()
        .with_boundary_access(|access| {
            value
                .with_core(&access, |value| {
                    let Value::Dict(dict) = value else {
                        return None;
                    };
                    dict.get(&crate::core::Key::atom_from_text(name))
                        .map(|value| access.root_runtime_value(access.duplicate_value(value)))
                })
                .flatten()
        })
}

fn unique_values(
    factory: &CoreValueFactory,
    values: impl IntoIterator<Item = PublicValue>,
) -> Vec<PublicValue> {
    let public_values = Values::from_core_factory(factory.clone());
    let mut unique = Vec::new();
    for value in values {
        let duplicate = public_values.with_access(|access| {
            let core_value = access
                .clone_core(&value)
                .expect("macro case values belong to the compilation runtime");
            unique.iter().any(|prior| {
                let prior = access
                    .clone_core(prior)
                    .expect("macro case values belong to the compilation runtime");
                access
                    .runtime_access()
                    .same_representation(&core_value, &prior)
            })
        });
        if !duplicate {
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
        cause: None,
        frontier: None,
        cases: Vec::new(),
        wait: None,
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
            consumed_end,
            output,
        } = run;
        let _: &Vec<Diagnostic> = diagnostics;
        let _: &usize = consumed_end;
        let _: &Vec<MacroOutput> = output;
    }

    fn assert_macro_failure_owner(failure: &MacroFailure) {
        let MacroFailure {
            diagnostic,
            cause,
            frontier,
            cases,
            wait,
        } = failure;
        let _: &Diagnostic = diagnostic;
        // A cause is a public diagnostic, which roots its emission.
        let _: &Option<Diagnostic> = cause;
        let _: &Option<usize> = frontier;
        let _: &Vec<PublicValue> = cases;
        // The awaited reply is a managed promise root.
        let _: &Option<ManagedPromiseRoot> = wait;
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
        let core = crate::core::private_test_value_factory();
        let values = Values::from_core_factory(core.clone());
        let domain = EffectTokenDomain::new(&values);
        let (output, retained_output) = retained_value(&domain);
        let run = MacroRun {
            diagnostics: Vec::new(),
            consumed_end: 0,
            output: vec![MacroOutput::Data(output)],
        };

        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained_output.upgrade().is_some());
        drop(run);
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained_output.upgrade().is_none());

        let (case, retained_case) = retained_value(&domain);
        let failure = MacroFailure {
            diagnostic: Diagnostic::new_with_factory(&core, Severity::Error, "failed macro"),
            cause: None,
            frontier: Some(0),
            cases: vec![case],
            wait: None,
        };
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained_case.upgrade().is_some());
        drop(failure);
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained_case.upgrade().is_none());
    }
}
