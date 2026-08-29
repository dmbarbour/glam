use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use glam::{
    Assembler, AssemblerBuilder, CONTENT_DIGEST_ALGORITHM, ContentDigest, Diagnostic,
    DiagnosticEvent, EvaluatedValue, EvaluationRuntime, ImportResolver, ModuleInput,
    QuiescenceReport, RelativeSourcePath, RuntimeDispositionKind, RuntimeReadiness, Severity,
    SourceArtifact, SourceError, SourceIdentity, SourceSystem, Value, ValueKind,
};

fn record<I, S>(values: &glam::Values, entries: I) -> Value
where
    I: IntoIterator<Item = (S, Value)>,
    S: AsRef<str>,
{
    values.record(entries).expect("test record should be local")
}

fn list(values: &glam::Values, items: impl IntoIterator<Item = Value>) -> Value {
    values.list(items).expect("test list should be local")
}

fn dictionary(values: &glam::Values, entries: impl IntoIterator<Item = (Value, Value)>) -> Value {
    values
        .dictionary(entries)
        .expect("test dictionary should be local and keyable")
}

fn access_path(assembler: &Assembler, root: &Value, path: &str) -> Result<Value, glam::Error> {
    let values = assembler.values();
    let mut value = root.clone();
    for part in path.split('.') {
        value = values.access(&value, values.atom_from_text(part))?;
    }
    Ok(value)
}

fn binary_at(assembler: &Assembler, root: &Value, path: &str) -> Result<Bytes, glam::Error> {
    binary_value(assembler, access_path(assembler, root, path)?)
}

fn evaluate(assembler: &Assembler, value: &Value) -> Result<EvaluatedValue, glam::Error> {
    assembler.evaluator().eval(value)
}

fn evaluated_value(assembler: &Assembler, value: &Value) -> Result<Value, glam::Error> {
    evaluate(assembler, value).map(EvaluatedValue::into_value)
}

fn binary_value(assembler: &Assembler, value: Value) -> Result<Bytes, glam::Error> {
    let values = assembler.values();
    let binary = values.anno_binary(value)?;
    evaluate(assembler, &binary).and_then(|binary| {
        binary
            .as_bytes()?
            .ok_or_else(|| glam::Error::new("value did not evaluate to binary data"))
    })
}

fn required_access(assembler: &Assembler, root: &Value, path: &str) -> Result<Value, glam::Error> {
    let values = assembler.values();
    let candidate = access_path(assembler, root, path)?;
    let required = values.apply(
        &values.require_defined_function(),
        [values.text(path), candidate],
    )?;
    evaluated_value(assembler, &required)
}

fn is_logically_undefined(assembler: &Assembler, value: Value) -> Result<bool, glam::Error> {
    let values = assembler.values();
    let marker = values.atom_from_text("public_api.undefined_marker");
    let selected = values.apply(&values.defined_or_function(), [marker.clone(), value])?;
    evaluate(assembler, &selected).map(|selected| selected.as_value() == &marker)
}

#[derive(Clone)]
struct PublicConflictIndex(bool);

impl glam::reflection::ConflictObservationIndex for PublicConflictIndex {
    fn clone_box(&self) -> Box<dyn glam::reflection::ConflictObservationIndex> {
        Box::new(self.clone())
    }

    fn observe(&mut self, _address: &glam::reflection::ConflictAddress) {
        self.0 = true;
    }

    fn may_conflict(&self, _changed: &glam::reflection::ConflictAddress) -> bool {
        self.0
    }
}

struct PublicConflictStrategy;

impl glam::reflection::ConflictAnalysisStrategy for PublicConflictStrategy {
    fn begin(&self) -> Box<dyn glam::reflection::ConflictObservationIndex> {
        Box::new(PublicConflictIndex(false))
    }

    fn name(&self) -> &'static str {
        "public-test"
    }
}

#[test]
fn custom_conflict_analysis_remains_available_through_the_public_facade() {
    let assembler = Assembler::builder()
        .conflict_analysis(Arc::new(PublicConflictStrategy))
        .build()
        .expect("public conflict strategy should configure an assembler");

    assert_eq!(assembler.conflict_analysis().name(), "public-test");
}

fn settle_ready_reasoning(assembler: &Assembler) -> QuiescenceReport {
    match assembler.drain_reasoning() {
        RuntimeReadiness::Ready(snapshot) => snapshot
            .settle()
            .expect("unchanged runtime readiness should settle"),
        RuntimeReadiness::Busy => panic!("draining should return a stable readiness snapshot"),
        RuntimeReadiness::Deadlocked(deadlock) => panic!(
            "reasoning unexpectedly deadlocked with {} unfinished work items",
            deadlock.unfinished().len()
        ),
    }
}

type DiagnosticEvents = Arc<Mutex<Vec<DiagnosticEvent>>>;

fn collecting_builder() -> (AssemblerBuilder, DiagnosticEvents) {
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let received = diagnostics.clone();
    let builder = Assembler::builder().diagnostic_callback(move |event| {
        received
            .lock()
            .expect("diagnostic collector should not be poisoned")
            .push(event);
    });
    (builder, diagnostics)
}

fn collecting_assembler() -> (Assembler, DiagnosticEvents) {
    let (builder, diagnostics) = collecting_builder();
    (
        builder.build().expect("collector assembler should build"),
        diagnostics,
    )
}

fn take_diagnostics(diagnostics: &DiagnosticEvents) -> Vec<DiagnosticEvent> {
    std::mem::take(
        &mut *diagnostics
            .lock()
            .expect("diagnostic collector should not be poisoned"),
    )
}

fn absolute_path_text(path: impl AsRef<Path>) -> String {
    std::path::absolute(path)
        .expect("test path should become absolute")
        .display()
        .to_string()
}

fn diagnostic_contexts(assembler: &Assembler, diagnostic: &Diagnostic) -> Vec<Value> {
    let values = assembler.values();
    let contexts = required_access(assembler, diagnostic.emission(), "msg.context")
        .expect("structured evaluation failure should define msg.context");
    let array = values
        .anno_array(contexts)
        .expect("diagnostic context array should construct");
    assembler
        .evaluator()
        .eval(&array)
        .expect("diagnostic contexts should form a list")
        .array_items()
        .expect("array extraction should use the matching runtime")
        .expect("array annotation should produce a strict value array")
}

fn import_context<'a>(assembler: &Assembler, contexts: &'a [Value], request: &str) -> &'a Value {
    contexts
        .iter()
        .find(|context| {
            required_access(assembler, context, "import.request.file")
                .ok()
                .and_then(|request| binary_value(assembler, request).ok())
                .as_deref()
                == Some(request.as_bytes())
        })
        .expect("diagnostic should retain its import request context")
}

#[test]
fn public_api_builds_a_script_module_and_extracts_binary_data() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["example"])
        .script("g", "language g0\nasm.result = \"Hello, library!\"\n")
        .build()
        .expect("script module should build");

    assert_eq!(module.diagnostics(), []);
    assert_eq!(
        binary_at(&assembler, module.value(), "asm.result").expect("asm.result should be binary"),
        b"Hello, library!".as_slice()
    );
}

#[test]
fn public_values_construct_semantic_access_and_annotations() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let static_record = record(&values, [("member", list(&values, [values.integer(65)]))]);
    let static_access = values
        .access(&static_record, values.atom_from_text("member"))
        .expect("static accessor should be constructed lazily");
    assert_eq!(
        binary_value(&assembler, static_access)
            .expect("binary annotation should normalize the selected list"),
        b"A".as_slice()
    );

    let dynamic_key = values.text("selected");
    let dynamic_dict = dictionary(&values, [(dynamic_key.clone(), values.text("dynamic"))]);
    let dynamic_access = values
        .access(&dynamic_dict, dynamic_key)
        .expect("computed accessor should retain its key expression");
    assert_eq!(
        binary_value(&assembler, dynamic_access)
            .expect("computed accessor should select its binary member"),
        b"dynamic".as_slice()
    );

    let annotated = values
        .anno(
            values.atom_from_text("binary"),
            list(&values, [values.integer(66)]),
        )
        .expect("generic annotation should be constructed lazily");
    assert_eq!(
        evaluate(&assembler, &annotated)
            .expect("binary annotation should evaluate")
            .as_bytes()
            .unwrap()
            .as_deref(),
        Some(b"B".as_slice()),
    );
}

#[test]
fn public_evaluation_errors_preserve_their_structured_diagnostic() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let module = assembler
        .module(["structured_failure"])
        .script(
            "g",
            concat!(
                "language g0\n",
                "import 'std as std\n",
                "asm.result = std.anno 'error ",
                "{msg:{text:\"structured failure\"}, detail:\"preserved\"}\n",
            ),
        )
        .build()
        .expect("structured failure fixture should build lazily");

    let error = binary_at(&assembler, module.value(), "asm.result")
        .expect_err("observing asm.result should raise its structured failure");

    assert_eq!(error.to_string(), "structured failure");
    assert!(error.diagnostics().is_empty());
    let diagnostic = error
        .diagnostic(&values)
        .expect("failure diagnostic should belong to the assembler runtime");
    assert_eq!(diagnostic.message(), "structured failure");
    let enriched = diagnostic
        .enrich(&assembler.values())
        .expect("the primary failure diagnostic should remain enrichable");
    assert_eq!(
        binary_value(
            &assembler,
            required_access(&assembler, &enriched, "detail")
                .expect("ad hoc failure fields should survive the public error boundary"),
        )
        .ok()
        .as_deref(),
        Some(b"preserved".as_slice())
    );
    assert_eq!(
        assembler
            .reflection()
            .kind(
                &required_access(&assembler, &enriched, "msg.context")
                    .expect("evaluation contexts should survive the public error boundary"),
            )
            .expect("context belongs to this runtime"),
        glam::ValueKind::List
    );
}

#[test]
fn public_api_reports_an_empty_reasoning_session_as_complete() {
    let assembler = Assembler::default();
    let report = settle_ready_reasoning(&assembler);

    assert!(report.task_failures().is_empty());
    assert!(report.killed_work().is_empty());
}

#[test]
fn public_runtime_profile_exposes_exit_to_scheduled_reflection() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["public_exit"])
        .script("g", "language g0\nrefl.exit = .exit.success\nvalue = ()\n")
        .build()
        .expect("reflection exit fixture should compile");
    evaluate(
        &assembler,
        &required_access(&assembler, module.value(), "value")
            .expect("fixture should define its ordinary value"),
    )
    .expect("ordinary demand should schedule automatic reflection");

    let RuntimeReadiness::Ready(snapshot) = assembler.drain_reasoning() else {
        panic!("an exit vote should make the runtime ready for settlement")
    };
    assert!(
        snapshot.dispositions().iter().any(|disposition| {
            matches!(disposition.kind(), RuntimeDispositionKind::ExitSuccess)
        })
    );
    let report = snapshot
        .settle()
        .expect("unchanged exit readiness should settle");
    assert!(report.task_failures().is_empty());
    assert!(report.killed_work().is_empty());
}

#[test]
fn public_promise_resolver_completes_a_cloneable_consumer() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let (promise, resolver) = assembler.promise("public input");
    let cloned = promise.clone();

    let pending =
        evaluate(&assembler, &promise).expect_err("an unresolved promise should fail fast");
    assert!(pending.to_string().contains("before initialization"));

    resolver
        .resolve(values.integer(42))
        .expect("the unique resolver should complete its promise");
    assert_eq!(
        evaluate(&assembler, &cloned)
            .expect("the cloned consumer should observe completion")
            .as_i64()
            .unwrap(),
        Some(42)
    );
}

#[test]
fn public_promise_resolver_can_fail_or_be_abandoned() {
    let assembler = Assembler::default();
    let (failed, resolver) = assembler.promise("explicit failure");
    resolver
        .fail_message("host operation failed")
        .expect("the unique resolver should fail its promise");
    assert!(
        evaluate(&assembler, &failed)
            .expect_err("a failed promise should expose its producer error")
            .to_string()
            .contains("host operation failed")
    );

    let (abandoned, resolver) = assembler.promise("abandoned input");
    drop(resolver);
    let error = evaluate(&assembler, &abandoned)
        .expect_err("dropping a resolver should permanently fail its promise");
    assert!(error.to_string().contains("abandoned input"));
    assert!(error.to_string().contains("dropped before completion"));
}

#[test]
fn public_promise_resolver_preserves_structured_failures() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let (failed, resolver) = assembler.promise("structured failure");
    let context = record(
        &values,
        [(
            "host",
            record(&values, [("operation", values.atom_from_text("read"))]),
        )],
    );
    let failure = record(
        &values,
        [
            (
                "msg",
                record(
                    &values,
                    [
                        ("text", values.text("host operation failed structurally")),
                        ("context", list(&values, [context.clone()])),
                    ],
                ),
            ),
            ("detail", values.integer(7)),
        ],
    );

    resolver
        .fail(failure)
        .expect("the unique resolver should accept a structured failure");
    let error = evaluate(&assembler, &failed)
        .expect_err("a failed promise should expose its structured producer error");

    assert_eq!(error.to_string(), "host operation failed structurally");
    let diagnostic = error
        .diagnostic(&values)
        .expect("promise failure should belong to the assembler runtime");
    assert_eq!(
        evaluate(
            &assembler,
            &required_access(&assembler, diagnostic.emission(), "detail")
                .expect("the structured diagnostic should retain its ad hoc field"),
        )
        .expect("detail should evaluate")
        .as_i64()
        .unwrap(),
        Some(7)
    );
    assert_eq!(
        diagnostic_contexts(&assembler, &diagnostic),
        [context],
        "the resolver must preserve existing structured diagnostic context"
    );
}

#[test]
fn public_promise_completion_resumes_blocked_reasoning_in_its_session() {
    let mut resolver = None;
    let (builder, diagnostics) = collecting_builder();
    let assembler = builder
        .reflection_environment(|environment| {
            let values = environment.values();
            let (late, promise_resolver) = environment.promise("late reflection input");
            resolver = Some(promise_resolver);
            Ok(record(
                &values,
                [("client", record(&values, [("late", late)]))],
            ))
        })
        .expect("promised reflection environment should build")
        .build()
        .expect("assembler should build");
    let values = assembler.values();
    let module = assembler
        .module(["promise_wakeup"])
        .script(
            "g",
            "language g0\nimport 'std\nrefl.wait = .env ['client,'late] >>= (\\input -> (input == \"ready\") =>> .log 'info {msg:{text:\"promise resumed\"}})\nvalue = ()\n",
        )
        .build()
        .expect("reflection fixture should compile");
    evaluate(
        &assembler,
        &required_access(&assembler, module.value(), "value")
            .expect("fixture should define its ordinary value"),
    )
    .expect("ordinary demand should schedule automatic reflection");

    let RuntimeReadiness::Deadlocked(blocked) = assembler.drain_reasoning() else {
        panic!("the unresolved host promise should deadlock its reflection task")
    };
    assert!(
        !blocked.unfinished().is_empty(),
        "runtime-wide readiness should retain the promise-dependent work"
    );

    resolver
        .take()
        .expect("resolver should escape environment construction")
        .resolve(values.text("ready"))
        .expect("host completion should succeed");
    let resumed = settle_ready_reasoning(&assembler);
    assert!(resumed.task_failures().is_empty());
    assert!(resumed.killed_work().is_empty());

    let diagnostics = take_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message(), "promise resumed");
}

#[test]
fn public_reasoning_report_exposes_retryable_blocked_errors() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["blocked_error"])
        .script(
            "g",
            "language g0\nimport 'std\nrefl.error = .heap.get ['observed] >>= (\\_ -> anno context:\"retry context\" (anno 'error {msg:{text:\"structured retryable failure\"}, detail:7}))\nvalue = \"value\"\n",
        )
        .build()
        .expect("reflection fixture should compile");
    assert_eq!(
        binary_at(&assembler, module.value(), "value")
            .expect("ordinary value should schedule reflection"),
        b"value".as_slice()
    );

    let RuntimeReadiness::Deadlocked(first) = assembler.drain_reasoning() else {
        panic!("retryable failure should leave the runtime deadlocked")
    };
    let blocked = first
        .unfinished()
        .iter()
        .find(|task| task.blocked_diagnostic().is_some())
        .expect("retryable failure should remain structured in the report");
    let diagnostic = blocked
        .blocked_diagnostic()
        .expect("blocked task should expose its diagnostic");
    assert_eq!(blocked.blocked_error(), None);
    assert_eq!(
        evaluate(
            &assembler,
            &required_access(&assembler, diagnostic.emission(), "detail")
                .expect("the retryable diagnostic should retain ad hoc fields"),
        )
        .expect("diagnostic detail should evaluate")
        .as_i64()
        .unwrap(),
        Some(7)
    );
    let projected = blocked
        .project_blocked_diagnostic(&assembler.values())
        .expect("blocked diagnostic projection should use the owning runtime")
        .expect("blocked task should retain its structured failure");
    assert_eq!(projected.message(), "structured retryable failure");
    assert_eq!(
        evaluate(
            &assembler,
            &required_access(&assembler, projected.emission(), "detail")
                .expect("the projected diagnostic should retain ad hoc fields"),
        )
        .expect("projected diagnostic detail should evaluate")
        .as_i64()
        .unwrap(),
        Some(7)
    );

    let RuntimeReadiness::Deadlocked(second) = assembler.drain_reasoning() else {
        panic!("an unchanged retryable failure should remain deadlocked")
    };
    let repeated = second
        .unfinished()
        .iter()
        .find_map(|task| task.blocked_diagnostic())
        .expect("repeated reporting should retain the failure");
    assert_eq!(repeated, diagnostic);
}

fn volume_write_annotation(assembler: &Assembler, effects: Value, value: Value) -> Value {
    let values = assembler.values();
    let set = access_path(assembler, &effects, "set").expect("volume capability should expose set");
    let effect = values
        .apply(&set, [list(&values, []), value])
        .expect("volume set should construct an effect");
    reflection_annotation(assembler, effect)
}

fn reflection_annotation(assembler: &Assembler, effect: Value) -> Value {
    let values = assembler.values();
    values
        .after_reflection(effect, values.text("done"))
        .expect("reflection annotation values should share one runtime")
}

#[test]
fn protected_volume_capability_updates_and_returns_client_state() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let volume = assembler
        .create_volume(values.text("initial"))
        .expect("protected volume should be created");
    let annotated = volume_write_annotation(&assembler, volume.effects(), values.text("updated"));
    assert_eq!(
        binary_value(&assembler, annotated).expect("volume write annotation should complete"),
        b"done".as_slice()
    );

    let final_value = volume
        .revoke()
        .expect("volume owner should recover its final value");
    assert_eq!(
        binary_value(&assembler, final_value).expect("final volume value should remain binary"),
        b"updated".as_slice()
    );
}

#[test]
fn assembler_clones_share_protected_volume_capabilities() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let clone = assembler.clone();
    let volume = assembler
        .create_volume(values.text("initial"))
        .expect("protected volume should be created");
    let annotated =
        volume_write_annotation(&clone, volume.effects(), values.text("shared session"));
    binary_value(&clone, annotated).expect("assembler clone should accept the capability");

    assert_eq!(
        binary_value(&assembler, volume.revoke().unwrap()).unwrap(),
        b"shared session".as_slice()
    );
}

#[test]
fn protected_volume_rewrite_uses_the_commit_time_value() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let module = assembler
        .module(["volume_rewrite"])
        .script("g", "language g0\nincrement = \\value -> value + 1\n")
        .build()
        .expect("volume updater should compile");
    let increment = access_path(&assembler, module.value(), "increment")
        .expect("volume updater should be defined");
    let volume = assembler
        .create_volume(values.integer(1))
        .expect("protected volume should be created");
    let rewrite = access_path(&assembler, &volume.effects(), "rewrite")
        .expect("volume capability should expose rewrite");
    let effect = values
        .apply(&rewrite, [list(&values, []), increment])
        .expect("volume rewrite should construct an effect");
    binary_value(&assembler, reflection_annotation(&assembler, effect))
        .expect("volume rewrite annotation should complete");

    let final_value = volume.revoke().unwrap();
    assert_eq!(
        evaluate(&assembler, &final_value)
            .unwrap()
            .as_i64()
            .unwrap(),
        Some(2)
    );
}

#[test]
fn protected_volume_get_is_an_ordinary_effect_result() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let module = assembler
        .module(["volume_get"])
        .script(
            "g",
            "language g0\nimport 'std\ndiscard = \\operation -> operation >>= (\\_value -> .r ())\n",
        )
        .build()
        .expect("effect result discarder should compile");
    let discard = access_path(&assembler, module.value(), "discard")
        .expect("effect result discarder should be defined");
    let volume = assembler
        .create_volume(values.text("unforced"))
        .expect("protected volume should be created");
    let get = access_path(&assembler, &volume.effects(), "get")
        .expect("volume capability should expose get");
    let get_effect = values
        .apply(&get, [list(&values, [])])
        .expect("volume get should construct an effect");
    let discard_effect = values
        .apply(&discard, [get_effect])
        .expect("get result should compose as an ordinary effect value");

    assert_eq!(
        binary_value(
            &assembler,
            reflection_annotation(&assembler, discard_effect),
        )
        .expect("volume get should complete"),
        b"done".as_slice()
    );
    assert_eq!(
        binary_value(&assembler, volume.revoke().unwrap()).unwrap(),
        b"unforced".as_slice()
    );
}

#[test]
fn revoked_volume_get_exposes_a_lazy_error_through_reflection_eval() {
    let (assembler, diagnostics) = collecting_assembler();
    let values = assembler.values();
    let module = assembler
        .module(["missing_volume_get"])
        .script(
            "g",
            "language g0\nimport 'std\ninspect = \\operation -> operation >>= (\\value -> .eval value >>= (\\result -> .log 'info result.err))\n",
        )
        .build()
        .expect("missing-volume inspector should compile");
    let inspect = access_path(&assembler, module.value(), "inspect")
        .expect("missing-volume inspector should be defined");
    let volume = assembler
        .create_volume(values.text("initial"))
        .expect("protected volume should be created");
    let effects = volume.effects();
    volume.revoke().unwrap();
    let get =
        access_path(&assembler, &effects, "get").expect("stale capability should still expose get");
    let get_effect = values
        .apply(&get, [list(&values, [])])
        .expect("stale get should remain a lazy effect request");
    let inspect_effect = values
        .apply(&inspect, [get_effect])
        .expect("missing-volume inspector should accept the effect");
    binary_value(
        &assembler,
        reflection_annotation(&assembler, inspect_effect),
    )
    .expect("`.eval` should contain the missing-volume error");

    let diagnostics = take_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    let message = binary_at(&assembler, diagnostics[0].emission(), "msg.text")
        .expect("lazy diagnostic text should be observable");
    assert!(String::from_utf8_lossy(&message).contains("has been revoked"));
}

#[test]
fn protected_volume_capabilities_reject_foreign_runtimes() {
    let owner = Assembler::default();
    let foreign = Assembler::default();
    let values = owner.values();
    let volume = owner
        .create_volume(values.text("initial"))
        .expect("protected volume should be created");
    let error = access_path(&foreign, &volume.effects(), "set")
        .expect_err("a foreign runtime must reject the capability");
    assert!(error.to_string().contains("belongs to evaluation runtime"));
    assert_eq!(
        binary_value(&owner, volume.revoke().unwrap())
            .expect("foreign use must not modify the volume"),
        b"initial".as_slice()
    );
}

#[test]
fn protected_volume_capabilities_cross_sessions_in_one_runtime() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let owner = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("owner assembler should build");
    let observer = Assembler::builder()
        .evaluation_runtime(runtime)
        .build()
        .expect("observer assembler should build");
    let values = owner.values();
    let volume = owner
        .create_volume(values.text("initial"))
        .expect("protected volume should be created");
    let annotated =
        volume_write_annotation(&observer, volume.effects(), values.text("shared runtime"));

    binary_value(&observer, annotated)
        .expect("same-runtime reasoning sessions should accept the capability");
    assert_eq!(
        binary_value(&owner, volume.revoke().unwrap()).unwrap(),
        b"shared runtime".as_slice()
    );
}

#[test]
fn revoked_volume_capability_cannot_recreate_its_volume() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let volume = assembler
        .create_volume(values.text("initial"))
        .expect("protected volume should be created");
    let effects = volume.effects();
    assert_eq!(
        binary_value(&assembler, volume.revoke().unwrap()).unwrap(),
        b"initial".as_slice()
    );
    let annotated = volume_write_annotation(&assembler, effects, values.text("resurrected"));

    let error =
        binary_value(&assembler, annotated).expect_err("stale blind write must fail at commit");
    assert!(
        error
            .to_string()
            .contains("revoked before its edits committed")
    );
}

#[test]
fn worker_configuration_is_shared_by_assembler_clones() {
    let assembler = Assembler::builder()
        .evaluation_runtime(EvaluationRuntime::new(3).expect("test worker threads should start"))
        .build()
        .expect("test assembler should build");
    let clone = assembler.clone();

    assert_eq!(assembler.evaluation_runtime().worker_threads(), 3);
    assert_eq!(clone.evaluation_runtime().worker_threads(), 3);
}

#[test]
fn public_api_exposes_the_default_diagnostic_formatter_as_a_function() {
    let assembler = Assembler::default();
    assert_eq!(
        assembler
            .reflection()
            .kind(&assembler.default_diagnostic_formatter())
            .expect("default formatter should belong to its assembler runtime"),
        glam::ValueKind::Function
    );
}

#[test]
fn diagnostic_value_updates_preserve_the_source_and_add_no_authoritative_metadata() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let message = record(
        &values,
        [("msg", record(&values, [("text", values.text("nested"))]))],
    );
    let enriched = Diagnostic::apply_updates(
        &assembler.values(),
        &message,
        record(
            &values,
            [(
                "viewer",
                record(&values, [("kind", values.text("terminal"))]),
            )],
        ),
    )
    .expect("an observer should be able to enrich a context message");

    assert_eq!(
        binary_value(
            &assembler,
            required_access(&assembler, &enriched, "msg.text")
                .expect("message text should remain available"),
        )
        .ok()
        .as_deref(),
        Some(b"nested".as_slice())
    );
    assert_eq!(
        binary_value(
            &assembler,
            required_access(&assembler, &enriched, "viewer.kind")
                .expect("viewer update should be applied"),
        )
        .ok()
        .as_deref(),
        Some(b"terminal".as_slice())
    );
    assert!(
        required_access(&assembler, &enriched, "msg.severity").is_err(),
        "neutral observer updates must not invent diagnostic severity"
    );
    assert!(
        required_access(&assembler, &message, "viewer").is_err(),
        "the original diagnostic-style value must stay unchanged"
    );
}

#[test]
fn diagnostic_value_updates_preserve_structured_evaluation_failures() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let (failed_update, resolver) = assembler.promise("diagnostic viewer update");
    resolver
        .fail(record(
            &values,
            [
                (
                    "msg",
                    record(
                        &values,
                        [
                            ("text", values.text("viewer update failed")),
                            (
                                "context",
                                list(
                                    &values,
                                    [record(
                                        &values,
                                        [(
                                            "viewer",
                                            record(
                                                &values,
                                                [("operation", values.atom_from_text("update"))],
                                            ),
                                        )],
                                    )],
                                ),
                            ),
                        ],
                    ),
                ),
                ("detail", values.integer(9)),
            ],
        ))
        .expect("host should install the structured update failure");
    let message = record(
        &values,
        [("msg", record(&values, [("text", values.text("nested"))]))],
    );

    let error = Diagnostic::apply_updates(&assembler.values(), &message, failed_update)
        .expect_err("demanding the viewer update should preserve its failure");
    assert_eq!(error.to_string(), "viewer update failed");
    let diagnostic = error
        .diagnostic(&values)
        .expect("viewer failure should belong to the assembler runtime");
    assert_eq!(
        evaluate(
            &assembler,
            &required_access(&assembler, diagnostic.emission(), "detail")
                .expect("diagnostic update failure should retain ad hoc fields"),
        )
        .expect("diagnostic detail should evaluate")
        .as_i64()
        .unwrap(),
        Some(9)
    );
    let contexts = diagnostic_contexts(&assembler, &diagnostic);
    assert_eq!(contexts.len(), 1);
    assert_eq!(
        required_access(&assembler, &contexts[0], "viewer.operation")
            .expect("diagnostic update failure should retain its context"),
        values.atom_from_text("update")
    );
}

#[test]
fn public_reflection_inspects_container_structure_and_atom_identity() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let reflection = assembler.reflection();

    let array = values
        .anno_array(list(&values, [values.integer(1), values.text("two")]))
        .expect("array annotation should construct");
    let items = assembler
        .evaluator()
        .eval(&array)
        .expect("array annotation should evaluate")
        .array_items()
        .unwrap()
        .expect("strict array should enumerate values");
    assert_eq!(
        evaluate(&assembler, &items[0]).unwrap().as_i64().unwrap(),
        Some(1)
    );
    assert_eq!(
        evaluate(&assembler, &items[1])
            .unwrap()
            .as_bytes()
            .unwrap()
            .as_deref(),
        Some(b"two".as_slice()),
    );

    let entries = reflection
        .dictionary_items(&record(&values, [("field", values.integer(7))]))
        .expect("reflection should enumerate dictionary entries");
    let [(key, value)] = entries.as_slice() else {
        panic!("the singleton record should have one reflected entry");
    };
    assert_eq!(
        evaluate(&assembler, value).unwrap().as_i64().unwrap(),
        Some(7)
    );
    assert_eq!(
        binary_value(
            &assembler,
            reflection
                .atom_key(key)
                .expect("record keys should remain atoms"),
        )
        .ok()
        .as_deref(),
        Some(b"field".as_slice())
    );
}

#[test]
fn public_reflection_recognizes_sealed_metadata_without_forcing_it() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["metadata_reflection"])
        .script(
            "g",
            concat!(
                "language g0\n",
                "import 'std\n",
                "carrier = anno 'meta_init ()\n",
                "failed = list.at 0 (anno meta_pure:(\\_ -> [1 / 0]) [carrier])\n",
                "ordinary = 42\n",
            ),
        )
        .build()
        .expect("metadata reflection fixture should compile");
    let reflection = assembler.reflection();

    let carrier = access_path(&assembler, module.value(), "carrier")
        .expect("fixture should define its initial carrier");
    assert_eq!(
        assembler
            .reflection()
            .kind(
                evaluate(&assembler, &carrier)
                    .expect("the carrier definition should evaluate")
                    .as_value(),
            )
            .expect("evaluated carrier should belong to the assembler runtime"),
        ValueKind::Sealed
    );
    let initial = reflection
        .associated_metadata(&carrier)
        .expect("metadata recognition should evaluate the carrier shell")
        .expect("the carrier should expose metadata to reflection");
    assert!(is_logically_undefined(&assembler, initial).unwrap());

    let failed = access_path(&assembler, module.value(), "failed")
        .expect("fixture should define its derived carrier");
    let hidden_failure = reflection
        .associated_metadata(&failed)
        .expect("metadata recognition should evaluate the derived carrier shell")
        .expect("the derived carrier should expose metadata to reflection");
    assert_eq!(reflection.kind(&hidden_failure).unwrap(), ValueKind::Lazy);
    assert!(
        evaluate(&assembler, &hidden_failure).is_err(),
        "metadata recognition must not demand the hidden failure"
    );
    assert!(
        reflection
            .associated_metadata(&carrier)
            .expect("the original carrier should remain inspectable")
            .and_then(|metadata| is_logically_undefined(&assembler, metadata).ok())
            .unwrap_or(false),
        "deriving another carrier must not alter the original snapshot"
    );

    let ordinary = access_path(&assembler, module.value(), "ordinary")
        .expect("fixture should define an ordinary value");
    assert!(
        reflection
            .associated_metadata(&ordinary)
            .expect("ordinary mismatch should not be an error")
            .is_none()
    );
}

#[test]
fn semantic_path_lookup_leaves_required_and_fallback_policy_to_glam_helpers() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let value = record(&values, [("present", values.integer(1))]);

    assert_eq!(
        evaluate(
            &assembler,
            &access_path(&assembler, &value, "present")
                .expect("present path access should construct"),
        )
        .expect("present value should evaluate")
        .as_i64()
        .unwrap(),
        Some(1)
    );
    assert!(
        is_logically_undefined(
            &assembler,
            access_path(&assembler, &value, "missing")
                .expect("missing path access should construct"),
        )
        .expect("missing path should evaluate to logical undefined")
    );
    assert!(required_access(&assembler, &value, "missing").is_err());
}

#[test]
fn assembler_owns_an_authoritative_reflection_environment() {
    let (builder, diagnostics) = collecting_builder();
    let assembler = builder
        .reflection_environment(|environment| {
            let values = environment.values();
            Ok(record(
                &values,
                [
                    (
                        "glam",
                        record(
                            &values,
                            [
                                ("version", values.text("spoofed")),
                                ("client_field", values.text("must disappear")),
                            ],
                        ),
                    ),
                    (
                        "client",
                        record(&values, [("name", values.text("embedded"))]),
                    ),
                ],
            ))
        })
        .expect("reflection environment should accept a dictionary")
        .build()
        .expect("test assembler should build");
    let values = assembler.values();
    let environment = assembler.reflection_environment();

    assert_eq!(
        binary_at(&assembler, &environment, "glam.version")
            .expect("assembler should inject its real version"),
        b"0.1.0".as_slice()
    );
    assert_eq!(
        binary_at(&assembler, &environment, "glam.implementation.name")
            .expect("assembler should identify its implementation"),
        b"rust-bootstrap".as_slice()
    );
    assert_eq!(
        binary_at(&assembler, &environment, "glam.implementation.version")
            .expect("assembler should expose its implementation version"),
        env!("CARGO_PKG_VERSION").as_bytes()
    );
    assert_eq!(
        required_access(&assembler, &environment, "glam.reasoning.role")
            .expect("assembler should identify its reasoning role"),
        values.atom_from_text("assembler")
    );
    assert_eq!(
        binary_at(&assembler, &environment, "client.name")
            .expect("client environment fields should remain visible"),
        b"embedded".as_slice()
    );
    assert!(required_access(&assembler, &environment, "glam.client_field").is_err());
    let diagnostics = take_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity(), Severity::Warning);
    assert!(diagnostics[0].message().contains("reserved"));
    assert!(
        Assembler::builder()
            .reflection_environment(|environment| Ok(environment.values().integer(1)))
            .is_err()
    );
}

#[test]
fn service_reflection_environments_have_independent_roles() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let logger = assembler.reflection_environment_for_role("logger");

    assert_eq!(
        required_access(&assembler, &logger, "glam.reasoning.role")
            .expect("service environment should contain its role"),
        values.atom_from_text("logger")
    );
    assert_eq!(
        required_access(
            &assembler,
            &assembler.reflection_environment(),
            "glam.reasoning.role",
        )
        .expect("deriving a service environment must not change the assembler role"),
        values.atom_from_text("assembler")
    );
}

#[test]
fn public_evaluation_leaves_automatic_reflection_tasks_for_explicit_drain() {
    let (assembler, diagnostics) = collecting_assembler();
    let module = assembler
        .module(["automatic_refl"])
        .script(
            "g",
            "language g0\nrefl.notice = .log 'info { msg:{ text:\"automatic reflection\" } }\nvalue = \"value\"\n",
        )
        .build()
        .expect("reflection module should build");
    assert_eq!(
        binary_at(&assembler, module.value(), "value").expect("ordinary value should evaluate"),
        b"value".as_slice()
    );
    assert!(take_diagnostics(&diagnostics).is_empty());

    let report = settle_ready_reasoning(&assembler);
    assert!(report.task_failures().is_empty());

    let diagnostics = take_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message(), "automatic reflection");
}

#[test]
fn reflection_environment_can_retain_a_builder_created_volume_handle() {
    let mut retained_volume = None;
    let assembler = Assembler::builder()
        .reflection_environment(|environment| {
            let values = environment.values();
            let volume = environment.create_volume(values.text("initial"))?;
            let effects = volume.effects();
            retained_volume = Some(volume);
            Ok(record(&values, [("client_state", effects)]))
        })
        .expect("reflection environment should be constructed")
        .build()
        .expect("assembler should build");
    let values = assembler.values();
    let effects = required_access(
        &assembler,
        &assembler.reflection_environment(),
        "client_state",
    )
    .expect("environment should contain the protected capability");
    let annotated = volume_write_annotation(&assembler, effects, values.text("updated"));
    assert_eq!(
        binary_value(&assembler, annotated).unwrap(),
        b"done".as_slice()
    );

    let final_value = retained_volume
        .expect("closure should retain the owner handle")
        .revoke()
        .expect("retained volume should be revocable");
    assert_eq!(
        binary_value(&assembler, final_value).unwrap(),
        b"updated".as_slice()
    );
}

#[test]
fn replacing_retained_diagnostic_subscriber_preserves_scheduled_reasoning() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["subscriber_replacement"])
        .script(
            "g",
            "language g0\nrefl.notice = .log 'info { msg:{ text:\"survived subscriber replacement\" } }\nvalue = \"value\"\n",
        )
        .build()
        .expect("reflection module should build");
    assert_eq!(
        binary_at(&assembler, module.value(), "value")
            .expect("ordinary value should schedule automatic reflection"),
        b"value".as_slice()
    );

    let received = Arc::new(Mutex::new(Vec::new()));
    let callback_values = received.clone();
    let assembler = assembler.with_diagnostic_callback(move |event| {
        callback_values
            .lock()
            .expect("callback collection mutex should not be poisoned")
            .push(event);
    });
    assert!(
        settle_ready_reasoning(&assembler)
            .task_failures()
            .is_empty()
    );

    let received = received
        .lock()
        .expect("callback collection mutex should not be poisoned");
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].message(), "survived subscriber replacement");
}

#[test]
fn public_api_loads_top_level_sources_and_relative_binaries_from_a_source_system() {
    let sources = MemorySourceSystem::new([
        (
            "main.g",
            b"language g0\nimport \"payload.bin\" binary as payload\nasm.result = payload\n"
                .as_slice(),
        ),
        ("payload.bin", b"artifact bytes".as_slice()),
    ]);
    let assembler = Assembler::builder()
        .source_system(sources)
        .build()
        .expect("custom-source assembler should build");
    let module = assembler
        .module(["artifact_source"])
        .file("main.g")
        .build()
        .expect("artifact source should build");

    assert_eq!(
        binary_at(&assembler, module.value(), "asm.result").unwrap(),
        b"artifact bytes".as_slice()
    );
}

#[test]
fn client_reflection_environment_is_visible_to_reflection_annotations() {
    let (builder, diagnostics) = collecting_builder();
    let assembler = builder
        .reflection_environment(|environment| {
            let values = environment.values();
            let process_environment = dictionary(
                &values,
                [(
                    values.text("GLAM_PUBLIC_API_TEST"),
                    values.text("HOST VALUE"),
                )],
            );
            Ok(record(
                &values,
                [(
                    "process",
                    record(
                        &values,
                        [
                            (
                                "args",
                                list(
                                    &values,
                                    ["embedded-glam", "inspect"].map(|text| values.text(text)),
                                ),
                            ),
                            ("env", process_environment),
                        ],
                    ),
                )],
            ))
        })
        .expect("test reflection environment should be a dictionary")
        .build()
        .expect("test assembler should build");
    let module = assembler
        .module(["reflection_host"])
        .script(
            "g",
            "language g0\nimport 'std\nvalue = anno {refl:(.env ['process,'env] >>= (\\environment -> (environment.[\"GLAM_PUBLIC_API_TEST\"] == \"HOST VALUE\") =>> .log 'info { msg:{ text:\"HOST VALUE\" } }))} \"done\"\n",
        )
        .build()
        .expect("reflection host fixture should build");
    let value =
        access_path(&assembler, module.value(), "value").expect("fixture should define value");
    let value = evaluated_value(&assembler, &value).expect("reflection annotation should complete");
    assert_eq!(
        binary_value(&assembler, value).expect("annotation target should remain observable"),
        b"done".as_slice()
    );
    let diagnostics = take_diagnostics(&diagnostics);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message() == "HOST VALUE")
    );
}

#[test]
fn top_level_file_inputs_may_be_absolute() {
    let source_path = absolute_path_text("absolute-input.g");
    let assembler = Assembler::builder()
        .source_system(MemorySourceSystem::new([(
            source_path.as_str(),
            b"language g0\nasm.result = \"absolute\"\n".as_slice(),
        )]))
        .build()
        .expect("test assembler should build");
    let module = assembler
        .module(["absolute"])
        .file(&source_path)
        .build()
        .expect("top-level callers may supply an absolute source path");
    assert_eq!(
        binary_at(&assembler, module.value(), "asm.result")
            .expect("absolute-path module should assemble"),
        b"absolute".as_slice()
    );
}

#[test]
fn source_compiler_reports_invalid_utf8_with_assembler_provenance() {
    let assembler = Assembler::builder()
        .source_system(MemorySourceSystem::new([(
            "invalid.g",
            b"language g0\nvalue = \xff\n".as_slice(),
        )]))
        .build()
        .expect("test assembler should build");

    let error = assembler
        .module(["invalid"])
        .file("invalid.g")
        .build()
        .expect_err("the built-in g compiler should reject invalid UTF-8");

    assert_eq!(error.diagnostics().len(), 1);
    let diagnostic = &error.diagnostics()[0];
    assert_eq!(diagnostic.source(), Some("memory:invalid.g"));
    assert_eq!(diagnostic.line(), Some(1));
    assert_eq!(diagnostic.severity(), Severity::Error);
    assert!(diagnostic.message().contains("not valid UTF-8"));
    let enriched = diagnostic
        .enrich(&assembler.values())
        .expect("assembler metadata should enrich the diagnostic");
    assert_eq!(
        binary_value(
            &assembler,
            required_access(&assembler, &enriched, "msg.text")
                .expect("diagnostic text should be available"),
        )
        .ok()
        .as_deref(),
        Some(diagnostic.message().as_bytes())
    );
    assert_eq!(
        binary_value(
            &assembler,
            required_access(&assembler, &enriched, "msg.origin.source.memory")
                .expect("assembler source provenance should be mixed in"),
        )
        .ok()
        .as_deref(),
        Some(b"invalid.g".as_slice())
    );
    let expected_digest = ContentDigest::of(b"language g0\nvalue = \xff\n");
    let digest_path = format!("msg.origin.digest.{CONTENT_DIGEST_ALGORITHM}");
    assert_eq!(
        binary_value(
            &assembler,
            required_access(&assembler, &enriched, &digest_path)
                .expect("assembler provenance should include the consumed digest"),
        )
        .ok()
        .as_deref(),
        Some(expected_digest.as_bytes().as_slice())
    );
    assert_eq!(
        assembler
            .reflection()
            .kind(
                &required_access(&assembler, &enriched, "spec")
                    .expect("diagnostic enrichment should update its object spec"),
            )
            .expect("diagnostic spec should belong to this runtime"),
        glam::ValueKind::Dict
    );
}

#[test]
fn repeated_source_compilations_have_distinct_invocations() {
    let assembler = Assembler::builder()
        .source_system(MemorySourceSystem::new([(
            "invalid.g",
            b"language g0\nvalue = \xff\n".as_slice(),
        )]))
        .build()
        .expect("test assembler should build");
    let error = assembler
        .module(["repeated"])
        .inputs([
            ModuleInput::file("invalid.g"),
            ModuleInput::file("invalid.g"),
        ])
        .build()
        .expect_err("both source invocations should report their error");

    assert_eq!(error.diagnostics().len(), 2);
    let invocation = |diagnostic: &glam::Diagnostic| {
        let enriched = diagnostic
            .enrich(&assembler.values())
            .expect("assembler metadata should enrich the diagnostic");
        evaluate(
            &assembler,
            &required_access(&assembler, &enriched, "msg.origin.invocation")
                .expect("diagnostic should identify its compilation invocation"),
        )
        .expect("compilation invocation should evaluate")
        .as_i64()
        .unwrap()
        .expect("small invocation ID should fit i64")
    };
    assert_ne!(
        invocation(&error.diagnostics()[0]),
        invocation(&error.diagnostics()[1])
    );
    assert!(
        error
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.source() == Some("memory:invalid.g"))
    );
}

#[test]
fn imported_source_diagnostics_include_the_import_chain() {
    let (builder, diagnostics) = collecting_builder();
    let assembler = builder
        .source_system(MemorySourceSystem::new([
            (
                "main.g",
                b"language g0\nimport \"child.g\" as child\nasm.result = child.value\n".as_slice(),
            ),
            ("child.g", b"language g0\nvalue = \xff\n".as_slice()),
        ]))
        .build()
        .expect("test assembler should build");
    let module = assembler
        .module(["imports"])
        .file("main.g")
        .build()
        .expect("the lazy imported source is not observed during module construction");

    let error = binary_at(&assembler, module.value(), "asm.result")
        .expect_err("observing the imported definition should compile and reject child.g");
    let diagnostic = error
        .diagnostic(&assembler.values())
        .expect("import failure should belong to the assembler runtime");
    let primary_contexts = diagnostic_contexts(&assembler, &diagnostic);
    let primary_import = import_context(&assembler, &primary_contexts, "child.g");
    let child_origin = required_access(&assembler, primary_import, "import.origin")
        .expect("failed child compilation should retain its origin");
    assert_eq!(
        assembler.reflection().kind(&child_origin).unwrap(),
        ValueKind::Dict
    );
    let child_source = binary_value(
        &assembler,
        required_access(&assembler, &child_origin, "source.memory")
            .expect("child origin should retain its source identity"),
    )
    .unwrap();
    assert_eq!(child_source, b"child.g".as_slice());

    binary_at(&assembler, module.value(), "asm.result")
        .expect_err("the cached imported failure should remain observable");
    let diagnostics = take_diagnostics(&diagnostics);
    assert_eq!(
        diagnostics.len(),
        1,
        "forcing a cached import failure must not duplicate compile diagnostics"
    );
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.source(), Some("memory:child.g"));
    let enriched = diagnostic
        .enrich(&assembler.values())
        .expect("assembler metadata should enrich the diagnostic");
    assert_eq!(
        assembler
            .reflection()
            .kind(
                &required_access(&assembler, &enriched, "msg.origin.import_chain")
                    .expect("imported diagnostic should carry its parent chain"),
            )
            .expect("import chain should belong to this runtime"),
        glam::ValueKind::List
    );
}

#[test]
fn missing_module_and_binary_imports_retain_requesting_origin() {
    for (declaration, request) in [
        (
            "import \"missing.g\" as missing\nasm.result = missing.value\n",
            "missing.g",
        ),
        (
            "import \"missing.bin\" binary as missing\nasm.result = missing\n",
            "missing.bin",
        ),
    ] {
        let sources =
            MemorySourceSystem::new([("main.g", format!("language g0\n{declaration}").as_bytes())]);
        let assembler = Assembler::builder()
            .source_system(sources)
            .build()
            .expect("test assembler should build");
        let module = assembler
            .module(["missing_import"])
            .file("main.g")
            .build()
            .expect("missing imports should remain lazy until observed");

        let error = binary_at(&assembler, module.value(), "asm.result")
            .expect_err("observing the missing import should fail");
        let diagnostic = error
            .diagnostic(&assembler.values())
            .expect("import failure should belong to the assembler runtime");
        let contexts = diagnostic_contexts(&assembler, &diagnostic);
        let context = import_context(&assembler, &contexts, request);
        assert_eq!(
            assembler
                .reflection()
                .kind(
                    &required_access(&assembler, context, "import.origin")
                        .expect("missing import should retain the requesting origin"),
                )
                .expect("import origin should belong to this runtime"),
            ValueKind::Dict
        );
    }
}

#[test]
fn caller_selected_module_path_scopes_abstract_global_paths() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["client", "root"])
        .script("g", "language g0\nunique Marker\n")
        .build()
        .expect("module should build");

    assert_eq!(
        required_access(&assembler, module.value(), "Marker")
            .expect("unique declaration should define Marker"),
        assembler
            .values()
            .abstract_global_path(["client", "root", "Marker"])
    );
}

#[test]
fn public_values_convert_numbers_without_exposing_big_number_types() {
    let runtime = EvaluationRuntime::new(0).expect("value runtime should build");
    let values = runtime.values();
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime)
        .build()
        .expect("value evaluator should build");
    let integer = values.integer(-42);
    let integer = evaluate(&assembler, &integer).unwrap();
    assert_eq!(integer.as_i64().unwrap(), Some(-42));
    assert_eq!(integer.as_rational_i64().unwrap(), Some((-42, 1)));
    assert_eq!(integer.as_f64().unwrap(), Some(-42.0));
    assert_eq!(integer.number_text().unwrap().as_deref(), Some("-42"));

    let ratio = values
        .number_from_text("-6/4")
        .expect("exact rational should parse");
    let ratio = evaluate(&assembler, &ratio).unwrap();
    assert_eq!(ratio.number_text().unwrap().as_deref(), Some("-3/2"));
    assert_eq!(ratio.as_rational_i64().unwrap(), Some((-3, 2)));
    assert_eq!(ratio.as_i64().unwrap(), None);
    assert_eq!(ratio.as_f64().unwrap(), Some(-1.5));
    assert_eq!(values.rational(1, 0), None);

    assert_eq!(values.number_from_f64(1.5), values.rational(3, 2));
    assert_eq!(values.number_from_f64(f64::NAN), None);
    assert_eq!(values.number_from_f64(f64::INFINITY), None);
    assert!(values.number_from_text("1/0").is_err());
}

#[test]
fn owned_extraction_survives_mutator_exit() {
    let runtime = EvaluationRuntime::new(0).expect("value runtime should build");
    let values = runtime.values();
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("value evaluator should build");

    let binary = evaluate(&assembler, &values.bytes(Bytes::from_static(b"owned"))).unwrap();
    let number = evaluate(
        &assembler,
        &values
            .number_from_text("123456789012345678901234567890")
            .unwrap(),
    )
    .unwrap();
    let array = values
        .anno_array(list(&values, [values.integer(1), values.integer(2)]))
        .and_then(|array| evaluate(&assembler, &array))
        .unwrap();

    let bytes = binary.as_bytes().unwrap().unwrap();
    let number_text = number.number_text().unwrap().unwrap();
    let items = array.array_items().unwrap().unwrap();
    assert_eq!(items.len(), 2);

    let expired_observer = binary.clone();

    drop(items);
    drop(array);
    drop(number);
    drop(binary);
    drop(assembler);
    drop(values);
    drop(runtime);

    assert!(expired_observer.as_bytes().is_err());
    assert_eq!(bytes, Bytes::from_static(b"owned"));
    assert_eq!(number_text, "123456789012345678901234567890");
}

#[test]
fn assembler_applies_and_evaluates_functions() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let module = assembler
        .module(["application"])
        .script("g", "language g0\nadd = \\x y -> x + y\n")
        .build()
        .expect("function module should build");
    let add = access_path(&assembler, module.value(), "add").expect("module should define add");
    let sum = values
        .apply(&add, [values.integer(20), values.integer(22)])
        .expect("application should be accepted lazily");

    assert_eq!(
        assembler.reflection().kind(&sum).unwrap(),
        glam::ValueKind::Lazy
    );
    assert_eq!(
        evaluate(&assembler, &sum)
            .expect("application should evaluate")
            .as_i64()
            .unwrap(),
        Some(42)
    );
}

#[test]
fn semantic_list_slices_extract_compact_and_list_binary_data() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let compact = values.bytes(Bytes::from_static(b"abcdef"));
    assert_eq!(
        binary_value(
            &assembler,
            values
                .list_slice(&compact, 1..5)
                .expect("compact slice should construct"),
        )
        .expect("compact binary should slice"),
        b"bcde".as_slice()
    );

    let listed = list(
        &values,
        [
            values.integer(b'a'.into()),
            values.integer(b'b'.into()),
            values.integer(b'c'.into()),
            values.integer(b'd'.into()),
        ],
    );
    assert_eq!(
        binary_value(
            &assembler,
            values
                .list_slice(&listed, 1..3)
                .expect("list slice should construct"),
        )
        .expect("byte-valued list should slice"),
        b"bc".as_slice()
    );
    assert!(
        values
            .list_slice(&listed, 3..5)
            .and_then(|slice| binary_value(&assembler, slice))
            .is_err()
    );
}

#[test]
fn checked_net_builder_constructs_an_opaque_identity_net() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let identity = assembler
        .net(|net| {
            let bind = net.bind();
            net.wire(bind.argument, bind.result)?;
            Ok(bind.application)
        })
        .expect("identity net should be closed");
    let application = values
        .apply(&identity, [values.integer(42)])
        .expect("application construction should not demand the net");
    let error = evaluate(&assembler, &application)
        .expect_err("raw nets require an explicit lambda-style arity bridge");
    assert_eq!(
        error.to_string(),
        "application requires a function value, received Net"
    );
}

#[test]
fn checked_net_builder_keeps_data_exposing_nets_opaque() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let net = assembler
        .net(|net| {
            let data = net.data(values.text("copied"))?;
            let copy = net.copy(1);
            net.wire(data, copy.input)?;
            Ok(copy.outputs[0])
        })
        .expect("one-output copy should normalize to a tunnel");

    assert_eq!(
        evaluate(&assembler, &net)
            .expect("an opaque net is already in weak-head normal form")
            .into_value(),
        net
    );
}

#[test]
fn checked_net_builder_reports_wiring_and_finalization_errors() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let unwired = assembler
        .net(|net| {
            let bind = net.bind();
            Ok(bind.application)
        })
        .expect_err("unwired ports must reject the net");
    assert!(unwired.to_string().contains("is unwired"));

    let duplicate = assembler
        .net(|net| {
            let left = net.data(values.integer(1))?;
            let right = net.data(values.integer(2))?;
            let other = net.data(values.integer(3))?;
            net.wire(left, right)?;
            net.wire(left, other)?;
            Ok(other)
        })
        .expect_err("a port cannot be wired twice");
    assert!(duplicate.to_string().contains("wired more than once"));
}

#[derive(Clone)]
struct MemorySourceSystem {
    files: Arc<HashMap<PathBuf, Bytes>>,
}

impl MemorySourceSystem {
    fn new<const N: usize>(files: [(&str, &[u8]); N]) -> Self {
        Self {
            files: Arc::new(
                files
                    .into_iter()
                    .map(|(path, bytes)| (PathBuf::from(path), Bytes::copy_from_slice(bytes)))
                    .collect(),
            ),
        }
    }

    fn load_path(&self, path: &Path) -> Result<SourceArtifact, SourceError> {
        let bytes = self.files.get(path).cloned().ok_or_else(|| {
            SourceError::new(format!("missing memory source `{}`", path.display()))
        })?;
        let base = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Ok(SourceArtifact::new(
            bytes,
            SourceIdentity::new(
                format!("memory:{}", path.display()),
                "memory",
                Bytes::copy_from_slice(path.display().to_string().as_bytes()),
            ),
        )
        .with_import_resolver(MemoryImportResolver {
            sources: self.clone(),
            base,
        }))
    }
}

impl SourceSystem for MemorySourceSystem {
    fn load_top_level(&self, path: &Path) -> Result<SourceArtifact, SourceError> {
        self.load_path(path)
    }
}

#[derive(Clone)]
struct MemoryImportResolver {
    sources: MemorySourceSystem,
    base: PathBuf,
}

impl ImportResolver for MemoryImportResolver {
    fn load_relative(&self, request: &RelativeSourcePath) -> Result<SourceArtifact, SourceError> {
        self.sources.load_path(&self.base.join(request.as_str()))
    }
}
