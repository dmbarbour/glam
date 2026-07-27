use std::sync::{Arc, Mutex};

use crate::api::{
    Assembler, CompilationExecution, Diagnostic, DiagnosticEvent, DiagnosticSubscriber,
    Value as PublicValue,
};
use crate::core::{Dict, Key, List, Value, keys};
use crate::diagnostic::Severity;
use crate::eval;

use super::{MacroRun, run_macro_effect};

struct DiagnosticMessages(Arc<Mutex<Vec<String>>>);

impl DiagnosticSubscriber for DiagnosticMessages {
    fn receive(&self, event: DiagnosticEvent) {
        self.0
            .lock()
            .expect("diagnostic observation mutex should not be poisoned")
            .push(event.diagnostic().message().to_owned());
    }
}

fn compile_effects(source: &str) -> (Assembler, PublicValue) {
    let assembler = Assembler::default();
    let module = assembler
        .module(["macro_runner_test"])
        .script(
            "g",
            format!("language g0\nimport 'std\nmeta.effects = {source}\n"),
        )
        .build()
        .expect("macro effect fixture should compile");
    let effects = assembler
        .get(module.value(), "meta.effects")
        .expect("macro effect fixture should define `meta.effects`");
    (assembler, effects)
}

fn run(
    execution: &CompilationExecution,
    effect: &PublicValue,
    environment: Value,
) -> Result<MacroRun, Box<Diagnostic>> {
    run_macro_effect(execution, effect.as_core().clone(), environment)
}

fn request_effect(path: &[&str], arguments: Vec<Value>) -> Value {
    eval::constant_effect(Value::Dict(Dict::new_sync().insert(
        Key::atom_from_key(&Key::abstract_global_path(path.iter().copied())),
        Value::List(List::from_values(arguments)),
    )))
}

fn return_effect(value: Value) -> Value {
    request_effect(&["reflection_runtime", "v0", "request", "r"], vec![value])
}

#[test]
fn macro_runner_exposes_environment_log_and_scoped_cases() {
    let (assembler, effect) = compile_effects(
        ".env '.flag >>= (\\_ -> .case \"flag case\" (.log 'info { msg:{ text:\"visible\" } } =>> .r ()))",
    );
    let execution = assembler.test_compilation_execution();
    let environment = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("flag"),
        Value::binary_from_text("visible"),
    ));

    let result = run(&execution, &effect, environment).expect("macro effect should succeed");
    assert_eq!(result.diagnostics().len(), 1);
    assert_eq!(result.diagnostics()[0].severity(), Severity::Info);
    assert_eq!(result.diagnostics()[0].message(), "visible");
    assert_eq!(result.visited_cases().len(), 1);
}

#[test]
fn macro_runner_selects_one_unit_branch_and_discards_other_journals() {
    let (assembler, effect) = compile_effects(
        ".alt (.log 'warn { msg:{ text:\"discarded\" } } =>> .fail) (.log 'info { msg:{ text:\"selected\" } } =>> .r ())",
    );
    let execution = assembler.test_compilation_execution();

    let result = run(&execution, &effect, Value::Dict(Dict::new_sync()))
        .expect("one successful macro branch should be accepted");
    assert_eq!(result.diagnostics().len(), 1);
    assert_eq!(result.diagnostics()[0].message(), "selected");
}

#[test]
fn macro_runner_rejects_zero_multiple_and_nonunit_results() {
    for (source, expected) in [
        (".fail", "no successful result"),
        (
            ".alt (.r ()) (.r ())",
            "multiple results; use `.cut` to select one",
        ),
        (".r 42", "terminated with Number, expected unit"),
    ] {
        let (assembler, effect) = compile_effects(source);
        let execution = assembler.test_compilation_execution();
        let error = run(&execution, &effect, Value::Dict(Dict::new_sync()))
            .expect_err("invalid macro result policy should fail");
        assert!(
            error.message().contains(expected),
            "unexpected diagnostic: {}",
            error.message()
        );
    }
}

#[test]
fn macro_api_omits_heap_tasks_and_full_reflection_requests() {
    for source in [".heap.get []", ".task.status {}", ".dict_items {}"] {
        let (assembler, effect) = compile_effects(source);
        let execution = assembler.test_compilation_execution();
        run(&execution, &effect, Value::Dict(Dict::new_sync()))
            .expect_err("restricted macro API request should be unavailable");
    }
}

#[test]
fn unstarted_reflection_gate_runs_inside_the_macro_session() {
    let assembler = Assembler::default();
    let execution = assembler.test_compilation_execution();
    let reflection = return_effect((*keys::UNIT_VALUE).clone());
    let gate = Value::reflection_gate(reflection, (*keys::UNIT_VALUE).clone());
    let macro_effect = PublicValue::from_core(return_effect(gate));

    run(&execution, &macro_effect, Value::Dict(Dict::new_sync()))
        .expect("macro session should launch and complete its reflection gate");
}

#[test]
fn assembler_owned_reflection_gate_is_foreign_to_macro_execution() {
    let (assembler, reflection) = compile_effects(".cut (.heap.get '.missing >>= (\\_ -> .fail))");
    let execution = assembler.test_compilation_execution();
    let gate = Value::reflection_gate(reflection.as_core().clone(), (*keys::UNIT_VALUE).clone());
    let error = eval::eval_value(&assembler.eval_context(), &gate)
        .expect_err("assembler observation should start and block the gate");
    assert!(error.blocked_on().is_some());
    let macro_effect = PublicValue::from_core(return_effect(gate));

    let error = run(&execution, &macro_effect, Value::Dict(Dict::new_sync()))
        .expect_err("macro execution must not migrate a foreign gate");
    assert!(error.message().contains("foreign or unavailable"));
}

#[test]
fn committed_reflection_log_survives_failed_macro_alternative() {
    let (assembler, effect) = compile_effects(
        ".alt ((anno refl:(.log 'warn { msg:{ text:\"reflection survived\" } }) (.r ())) =>> .fail) (.r ())",
    );
    let execution = assembler.test_compilation_execution();

    let result = run(&execution, &effect, Value::Dict(Dict::new_sync()))
        .expect("fallback macro branch should succeed");
    assert!(result.diagnostics().is_empty());
    assert_eq!(execution.macro_diagnostic_counts().warnings(), 1);
}

#[test]
fn committed_reflection_heap_and_children_outlive_macro_alternatives() {
    let (assembler, effects) = compile_effects(
        "{ write:(.alt ((anno refl:(.heap.set '.saved \"yes\") (.r ())) =>> .fail) (.r ())), read:(anno refl:(.heap.get '.saved >>= (\\saved -> .log 'info { msg:{ text:saved } })) (.r ())), child:(.alt ((anno refl:(.task.new (.log 'warn { msg:{ text:\"child survived\" } }) >>= (\\_ -> .r ())) (.r ())) =>> .fail) (.r ())) }",
    );
    assembler.record_diagnostic(Diagnostic::new(Severity::Info, "assembler-only diagnostic"));
    let execution = assembler.test_compilation_execution();
    let empty = Value::Dict(Dict::new_sync());

    for name in ["write", "read", "child"] {
        let effect = assembler
            .get(&effects, name)
            .expect("effect fixture member should exist");
        run(&execution, &effect, empty.clone()).expect("macro effect should select its fallback");
    }
    assert!(!execution.drain_for_test());
    let counts = execution.macro_diagnostic_counts();
    assert_eq!(counts.info(), 1);
    assert_eq!(counts.warnings(), 1);
    let assembler_counts = assembler.diagnostic_bus().counts();
    assert_eq!(assembler_counts.info(), 2);
    assert_eq!(assembler_counts.warnings(), 1);
}

#[test]
fn macro_reflection_heap_is_private_to_the_compilation_execution() {
    let (assembler, effect) = compile_effects("anno refl:(.heap.set '.macro_only \"yes\") (.r ())");
    let execution = assembler.test_compilation_execution();

    run(&execution, &effect, Value::Dict(Dict::new_sync()))
        .expect("macro reflection heap write should complete");
    assert!(!execution.drain_for_test());

    let macro_value = assembler
        .get(&execution.macro_heap(), "macro_only")
        .expect("macro heap read should succeed");
    let assembler_value = assembler.get(&assembler.test_reflection_heap(), "macro_only");
    assert_eq!(macro_value.as_core(), &Value::binary_from_text("yes"));
    assert!(assembler_value.is_err());
}

#[test]
fn compilation_execution_drain_reports_detached_failure_and_deadlock() {
    for (source, expected) in [
        (
            "anno refl:(.task.new (.fail) >>= (\\_ -> .r ())) (.r ())",
            "failed",
        ),
        (
            "anno refl:(.task.new (.heap.get '.never >>= (\\_ -> .fail)) >>= (\\_ -> .r ())) (.r ())",
            "deadlocked",
        ),
    ] {
        let (assembler, effect) = compile_effects(source);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let _subscription = assembler
            .diagnostic_bus()
            .subscribe(DiagnosticMessages(observed.clone()));
        let execution = assembler.test_compilation_execution();

        run(&execution, &effect, Value::Dict(Dict::new_sync()))
            .expect("detached child should not fail its completed parent");
        assert!(execution.drain_for_test());
        assert!(
            observed
                .lock()
                .expect("diagnostic observation mutex should not be poisoned")
                .iter()
                .any(|message| message.contains(expected)),
            "expected a macro reflection diagnostic containing `{expected}`"
        );
    }
}
