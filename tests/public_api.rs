use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use glam::{
    Assembler, AssemblerBuilder, CONTENT_DIGEST_ALGORITHM, ContentDigest, DiagnosticEvent,
    EvaluationRuntime, Host, HostError, ImportResolver, ModuleInput, ReasoningStatus,
    RelativeSourcePath, Severity, SourceArtifact, SourceError, SourceIdentity, SourceSystem, Value,
};

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
        assembler
            .binary_at(module.value(), "asm.result")
            .expect("asm.result should be binary"),
        b"Hello, library!".as_slice()
    );
}

#[test]
fn public_api_reports_an_empty_reasoning_session_as_complete() {
    let report = Assembler::default().drain_reasoning();

    assert_eq!(report.status(), ReasoningStatus::Complete);
    assert!(report.failures().is_empty());
    assert!(report.unfinished().is_empty());
}

#[test]
fn public_reasoning_report_exposes_retryable_blocked_errors() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["blocked_error"])
        .script(
            "g",
            "language g0\nrefl.error = .heap.get ['observed] >>= (\\_ -> 1 2)\nvalue = \"value\"\n",
        )
        .build()
        .expect("reflection fixture should compile");
    assert_eq!(
        assembler
            .binary_at(module.value(), "value")
            .expect("ordinary value should schedule reflection"),
        b"value".as_slice()
    );

    let report = assembler.drain_reasoning();
    assert_eq!(report.status(), ReasoningStatus::Deadlocked);
    assert!(report.failures().is_empty());
    assert!(report.unfinished().iter().any(|task| {
        task.blocked_error()
            .is_some_and(|error| error.contains("requires a function value"))
    }));
}

fn volume_write_annotation(assembler: &Assembler, effects: Value, value: Value) -> Value {
    let set = assembler
        .get(&effects, "set")
        .expect("volume capability should expose set");
    let effect = assembler
        .apply(&set, [Value::list([]), value])
        .expect("volume set should construct an effect");
    reflection_annotation(effect)
}

fn reflection_annotation(effect: Value) -> Value {
    Value::after_reflection(effect, Value::text("done"))
}

#[test]
fn protected_volume_capability_updates_and_returns_client_state() {
    let assembler = Assembler::default();
    let volume = assembler
        .create_volume(Value::text("initial"))
        .expect("protected volume should be created");
    let annotated = volume_write_annotation(&assembler, volume.effects(), Value::text("updated"));
    assert_eq!(
        assembler
            .to_binary(&annotated)
            .expect("volume write annotation should complete"),
        b"done".as_slice()
    );

    let final_value = volume
        .revoke()
        .expect("volume owner should recover its final value");
    assert_eq!(
        assembler
            .to_binary(&final_value)
            .expect("final volume value should remain binary"),
        b"updated".as_slice()
    );
}

#[test]
fn assembler_clones_share_protected_volume_capabilities() {
    let assembler = Assembler::default();
    let clone = assembler.clone();
    let volume = assembler
        .create_volume(Value::text("initial"))
        .expect("protected volume should be created");
    let annotated =
        volume_write_annotation(&clone, volume.effects(), Value::text("shared session"));
    clone
        .to_binary(&annotated)
        .expect("assembler clone should accept the capability");

    assert_eq!(
        assembler.to_binary(&volume.revoke().unwrap()).unwrap(),
        b"shared session".as_slice()
    );
}

#[test]
fn protected_volume_rewrite_uses_the_commit_time_value() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["volume_rewrite"])
        .script("g", "language g0\nincrement = \\value -> value + 1\n")
        .build()
        .expect("volume updater should compile");
    let increment = assembler
        .get(module.value(), "increment")
        .expect("volume updater should be defined");
    let volume = assembler
        .create_volume(Value::integer(1))
        .expect("protected volume should be created");
    let rewrite = assembler
        .get(&volume.effects(), "rewrite")
        .expect("volume capability should expose rewrite");
    let effect = assembler
        .apply(&rewrite, [Value::list([]), increment])
        .expect("volume rewrite should construct an effect");
    assembler
        .to_binary(&reflection_annotation(effect))
        .expect("volume rewrite annotation should complete");

    let final_value = volume.revoke().unwrap();
    assert_eq!(assembler.evaluate(&final_value).unwrap().as_i64(), Some(2));
}

#[test]
fn protected_volume_get_is_an_ordinary_effect_result() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["volume_get"])
        .script(
            "g",
            "language g0\nimport 'std\ndiscard = \\operation -> operation >>= (\\_value -> .r ())\n",
        )
        .build()
        .expect("effect result discarder should compile");
    let discard = assembler
        .get(module.value(), "discard")
        .expect("effect result discarder should be defined");
    let volume = assembler
        .create_volume(Value::text("unforced"))
        .expect("protected volume should be created");
    let get = assembler
        .get(&volume.effects(), "get")
        .expect("volume capability should expose get");
    let get_effect = assembler
        .apply(&get, [Value::list([])])
        .expect("volume get should construct an effect");
    let discard_effect = assembler
        .apply(&discard, [get_effect])
        .expect("get result should compose as an ordinary effect value");

    assert_eq!(
        assembler
            .to_binary(&reflection_annotation(discard_effect))
            .expect("volume get should complete"),
        b"done".as_slice()
    );
    assert_eq!(
        assembler.to_binary(&volume.revoke().unwrap()).unwrap(),
        b"unforced".as_slice()
    );
}

#[test]
fn revoked_volume_get_exposes_a_lazy_error_through_reflection_eval() {
    let (assembler, diagnostics) = collecting_assembler();
    let module = assembler
        .module(["missing_volume_get"])
        .script(
            "g",
            "language g0\nimport 'std\ninspect = \\operation -> operation >>= (\\value -> .eval value >>= (\\result -> .log 'info { msg:{ text:result.err } }))\n",
        )
        .build()
        .expect("missing-volume inspector should compile");
    let inspect = assembler
        .get(module.value(), "inspect")
        .expect("missing-volume inspector should be defined");
    let volume = assembler
        .create_volume(Value::text("initial"))
        .expect("protected volume should be created");
    let effects = volume.effects();
    volume.revoke().unwrap();
    let get = assembler
        .get(&effects, "get")
        .expect("stale capability should still expose get");
    let get_effect = assembler
        .apply(&get, [Value::list([])])
        .expect("stale get should remain a lazy effect request");
    let inspect_effect = assembler
        .apply(&inspect, [get_effect])
        .expect("missing-volume inspector should accept the effect");
    assembler
        .to_binary(&reflection_annotation(inspect_effect))
        .expect("`.eval` should contain the missing-volume error");

    let diagnostics = take_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    let message = assembler
        .binary_at(diagnostics[0].emission(), "msg.text")
        .expect("lazy diagnostic text should be observable");
    assert!(String::from_utf8_lossy(&message).contains("has been revoked"));
}

#[test]
fn protected_volume_capabilities_are_reasoning_session_local() {
    let owner = Assembler::default();
    let foreign = Assembler::default();
    let volume = owner
        .create_volume(Value::text("initial"))
        .expect("protected volume should be created");
    let annotated = volume_write_annotation(&foreign, volume.effects(), Value::text("forbidden"));

    let error = foreign
        .to_binary(&annotated)
        .expect_err("foreign reasoning session must reject the capability");
    assert!(error.to_string().contains("foreign reflection volume"));
    assert_eq!(
        owner
            .to_binary(&volume.revoke().unwrap())
            .expect("foreign use must not modify the volume"),
        b"initial".as_slice()
    );
}

#[test]
fn revoked_volume_capability_cannot_recreate_its_volume() {
    let assembler = Assembler::default();
    let volume = assembler
        .create_volume(Value::text("initial"))
        .expect("protected volume should be created");
    let effects = volume.effects();
    assert_eq!(
        assembler.to_binary(&volume.revoke().unwrap()).unwrap(),
        b"initial".as_slice()
    );
    let annotated = volume_write_annotation(&assembler, effects, Value::text("resurrected"));

    let error = assembler
        .to_binary(&annotated)
        .expect_err("stale blind write must fail at commit");
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
    assert_eq!(
        Assembler::default().default_diagnostic_formatter().kind(),
        glam::ValueKind::Function
    );
}

#[test]
fn assembler_owns_an_authoritative_reflection_environment() {
    let (builder, diagnostics) = collecting_builder();
    let assembler = builder
        .reflection_environment(|_| {
            Ok(Value::record([
                (
                    "glam",
                    Value::record([
                        ("version", Value::text("spoofed")),
                        ("client_field", Value::text("must disappear")),
                    ]),
                ),
                ("client", Value::record([("name", Value::text("embedded"))])),
            ]))
        })
        .expect("reflection environment should accept a dictionary")
        .build()
        .expect("test assembler should build");
    let environment = assembler.reflection_environment();

    assert_eq!(
        assembler
            .binary_at(&environment, "glam.version")
            .expect("assembler should inject its real version"),
        b"0.1.0".as_slice()
    );
    assert_eq!(
        assembler
            .binary_at(&environment, "glam.implementation.name")
            .expect("assembler should identify its implementation"),
        b"rust-bootstrap".as_slice()
    );
    assert_eq!(
        assembler
            .binary_at(&environment, "glam.implementation.version")
            .expect("assembler should expose its implementation version"),
        env!("CARGO_PKG_VERSION").as_bytes()
    );
    assert_eq!(
        assembler
            .get(&environment, "glam.reasoning.role")
            .expect("assembler should identify its reasoning role"),
        Value::atom_from_text("assembler")
    );
    assert_eq!(
        assembler
            .binary_at(&environment, "client.name")
            .expect("client environment fields should remain visible"),
        b"embedded".as_slice()
    );
    assert!(assembler.get(&environment, "glam.client_field").is_err());
    let diagnostics = take_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity(), Severity::Warning);
    assert!(diagnostics[0].message().contains("reserved"));
    assert!(
        Assembler::builder()
            .reflection_environment(|_| Ok(Value::integer(1)))
            .is_err()
    );
}

#[test]
fn service_reflection_environments_have_independent_roles() {
    let assembler = Assembler::default();
    let logger = assembler.reflection_environment_for_role("logger");

    assert_eq!(
        assembler
            .get(&logger, "glam.reasoning.role")
            .expect("service environment should contain its role"),
        Value::atom_from_text("logger")
    );
    assert_eq!(
        assembler
            .get(&assembler.reflection_environment(), "glam.reasoning.role")
            .expect("deriving a service environment must not change the assembler role"),
        Value::atom_from_text("assembler")
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
        assembler
            .binary_at(module.value(), "value")
            .expect("ordinary value should evaluate"),
        b"value".as_slice()
    );
    assert!(take_diagnostics(&diagnostics).is_empty());

    let report = assembler.drain_reasoning();
    assert_eq!(report.status(), ReasoningStatus::Complete);

    let diagnostics = take_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message(), "automatic reflection");
}

#[test]
fn reflection_environment_can_retain_a_builder_created_volume_handle() {
    let mut retained_volume = None;
    let assembler = Assembler::builder()
        .reflection_environment(|environment| {
            let volume = environment.create_volume(Value::text("initial"))?;
            let effects = volume.effects();
            retained_volume = Some(volume);
            Ok(Value::record([("client_state", effects)]))
        })
        .expect("reflection environment should be constructed")
        .build()
        .expect("assembler should build");
    let effects = assembler
        .get(&assembler.reflection_environment(), "client_state")
        .expect("environment should contain the protected capability");
    let annotated = volume_write_annotation(&assembler, effects, Value::text("updated"));
    assert_eq!(assembler.to_binary(&annotated).unwrap(), b"done".as_slice());

    let final_value = retained_volume
        .expect("closure should retain the owner handle")
        .revoke()
        .expect("retained volume should be revocable");
    assert_eq!(
        assembler.to_binary(&final_value).unwrap(),
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
        assembler
            .binary_at(module.value(), "value")
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
    assert_eq!(
        assembler.drain_reasoning().status(),
        ReasoningStatus::Complete
    );

    let received = received
        .lock()
        .expect("callback collection mutex should not be poisoned");
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].message(), "survived subscriber replacement");
}

#[test]
fn public_api_can_load_sources_and_binaries_from_a_custom_host() {
    let host = MemoryHost::new([
        (
            "main.g",
            b"language g0\nimport \"payload.bin\" binary as payload\nasm.result = payload\n"
                .as_slice(),
        ),
        ("payload.bin", b"virtual bytes".as_slice()),
    ]);
    let assembler = Assembler::builder()
        .host(host)
        .build()
        .expect("custom-host assembler should build");
    let module = assembler
        .module(["virtual"])
        .inputs([ModuleInput::file("main.g")])
        .build()
        .expect("virtual module should build");

    assert_eq!(
        assembler
            .binary_at(module.value(), "asm.result")
            .expect("virtual binary import should evaluate"),
        b"virtual bytes".as_slice()
    );
}

#[test]
fn public_api_can_load_from_an_artifact_source_system() {
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
        assembler.binary_at(module.value(), "asm.result").unwrap(),
        b"artifact bytes".as_slice()
    );
}

#[test]
fn client_reflection_environment_is_visible_to_reflection_annotations() {
    let process_environment = Value::dictionary([(
        Value::text("GLAM_PUBLIC_API_TEST"),
        Value::text("HOST VALUE"),
    )])
    .expect("test environment key should be keyable");
    let reflection_environment = Value::record([(
        "process",
        Value::record([
            (
                "args",
                Value::list(["embedded-glam", "inspect"].map(Value::text)),
            ),
            ("env", process_environment),
        ]),
    )]);
    let (builder, diagnostics) = collecting_builder();
    let assembler = builder
        .host(MemoryHost::new([]))
        .reflection_environment(|_| Ok(reflection_environment))
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
    let value = assembler
        .get(module.value(), "value")
        .expect("fixture should define value");
    let value = assembler
        .evaluate(&value)
        .expect("reflection annotation should complete");
    assert_eq!(
        assembler
            .to_binary(&value)
            .expect("annotation target should remain observable"),
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
        .host(MemoryHost::new([(
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
        assembler
            .binary_at(module.value(), "asm.result")
            .expect("absolute-path module should assemble"),
        b"absolute".as_slice()
    );
}

#[test]
fn source_compiler_reports_invalid_utf8_with_assembler_provenance() {
    let assembler = Assembler::builder()
        .host(MemoryHost::new([(
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
    let source_path = absolute_path_text("invalid.g");
    assert_eq!(diagnostic.source(), Some(source_path.as_str()));
    assert_eq!(diagnostic.line(), Some(1));
    assert_eq!(diagnostic.severity(), Severity::Error);
    assert!(diagnostic.message().contains("not valid UTF-8"));
    let enriched = diagnostic
        .enrich()
        .expect("assembler metadata should enrich the diagnostic");
    assert_eq!(
        assembler
            .get(&enriched, "msg.text")
            .expect("diagnostic text should be available")
            .as_binary(),
        Some(diagnostic.message().as_bytes())
    );
    assert_eq!(
        assembler
            .get(&enriched, "msg.origin.source.file")
            .expect("assembler source provenance should be mixed in")
            .as_binary(),
        Some(source_path.as_bytes())
    );
    let expected_digest = ContentDigest::of(b"language g0\nvalue = \xff\n");
    let digest_path = format!("msg.origin.digest.{CONTENT_DIGEST_ALGORITHM}");
    assert_eq!(
        assembler
            .get(&enriched, &digest_path)
            .expect("assembler provenance should include the consumed digest")
            .as_binary(),
        Some(expected_digest.as_bytes().as_slice())
    );
    assert_eq!(
        assembler
            .get(&enriched, "spec")
            .expect("diagnostic enrichment should update its object spec")
            .kind(),
        glam::ValueKind::Dict
    );
}

#[test]
fn repeated_source_compilations_have_distinct_invocations() {
    let assembler = Assembler::builder()
        .host(MemoryHost::new([(
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
            .enrich()
            .expect("assembler metadata should enrich the diagnostic");
        assembler
            .get(&enriched, "msg.origin.invocation")
            .expect("diagnostic should identify its compilation invocation")
            .as_i64()
            .expect("small invocation ID should fit i64")
    };
    assert_ne!(
        invocation(&error.diagnostics()[0]),
        invocation(&error.diagnostics()[1])
    );
    let source_path = absolute_path_text("invalid.g");
    assert!(
        error
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.source() == Some(source_path.as_str()))
    );
}

#[test]
fn imported_source_diagnostics_include_the_import_chain() {
    let (builder, diagnostics) = collecting_builder();
    let assembler = builder
        .host(MemoryHost::new([
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

    assembler
        .binary_at(module.value(), "asm.result")
        .expect_err("observing the imported definition should compile and reject child.g");
    let diagnostics = take_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    let source_path = absolute_path_text("child.g");
    assert_eq!(diagnostic.source(), Some(source_path.as_str()));
    let enriched = diagnostic
        .enrich()
        .expect("assembler metadata should enrich the diagnostic");
    assert_eq!(
        assembler
            .get(&enriched, "msg.origin.import_chain")
            .expect("imported diagnostic should carry its parent chain")
            .kind(),
        glam::ValueKind::List
    );
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
        assembler
            .get(module.value(), "Marker")
            .expect("unique declaration should define Marker"),
        Value::abstract_global_path(["client", "root", "Marker"])
    );
}

#[test]
fn public_values_convert_numbers_without_exposing_big_number_types() {
    let integer = Value::integer(-42);
    assert_eq!(integer.as_i64(), Some(-42));
    assert_eq!(integer.as_rational_i64(), Some((-42, 1)));
    assert_eq!(integer.as_f64(), Some(-42.0));
    assert_eq!(integer.as_number_text().as_deref(), Some("-42"));

    let ratio = Value::number_from_text("-6/4").expect("exact rational should parse");
    assert_eq!(ratio.as_number_text().as_deref(), Some("-3/2"));
    assert_eq!(ratio.as_rational_i64(), Some((-3, 2)));
    assert_eq!(ratio.as_i64(), None);
    assert_eq!(ratio.as_f64(), Some(-1.5));
    assert_eq!(Value::rational(1, 0), None);

    assert_eq!(Value::number_from_f64(1.5), Value::rational(3, 2));
    assert_eq!(Value::number_from_f64(f64::NAN), None);
    assert_eq!(Value::number_from_f64(f64::INFINITY), None);
    assert!(Value::number_from_text("1/0").is_err());
}

#[test]
fn assembler_applies_and_evaluates_functions() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["application"])
        .script("g", "language g0\nadd = \\x y -> x + y\n")
        .build()
        .expect("function module should build");
    let add = assembler
        .get(module.value(), "add")
        .expect("module should define add");
    let sum = assembler
        .apply(&add, [Value::integer(20), Value::integer(22)])
        .expect("application should be accepted lazily");

    assert_eq!(sum.kind(), glam::ValueKind::Lazy);
    assert_eq!(
        assembler
            .evaluate(&sum)
            .expect("application should evaluate")
            .as_i64(),
        Some(42)
    );
}

#[test]
fn assembler_extracts_ranges_from_compact_and_list_binary_data() {
    let assembler = Assembler::default();
    let compact = Value::binary(Bytes::from_static(b"abcdef"));
    assert_eq!(
        assembler
            .binary_slice(&compact, 1..5)
            .expect("compact binary should slice"),
        b"bcde".as_slice()
    );

    let listed = Value::list([
        Value::integer(b'a'.into()),
        Value::integer(b'b'.into()),
        Value::integer(b'c'.into()),
        Value::integer(b'd'.into()),
    ]);
    assert_eq!(
        assembler
            .binary_slice(&listed, 1..3)
            .expect("byte-valued list should slice"),
        b"bc".as_slice()
    );
    assert!(assembler.binary_slice(&listed, 3..5).is_err());
}

#[test]
fn checked_net_builder_constructs_an_identity_function() {
    let assembler = Assembler::default();
    let identity = assembler
        .net(|net| {
            let bind = net.bind();
            net.wire(bind.argument, bind.result)?;
            Ok(bind.application)
        })
        .expect("identity net should be closed");
    let result = assembler
        .apply(&identity, [Value::integer(42)])
        .and_then(|value| assembler.evaluate(&value))
        .expect("identity net should return its argument");

    assert_eq!(result.as_i64(), Some(42));
}

#[test]
fn checked_net_builder_exposes_data_through_copy_helpers() {
    let assembler = Assembler::default();
    let net = assembler
        .net(|net| {
            let data = net.data(Value::text("copied"));
            let copy = net.copy(1);
            net.wire(data, copy.input)?;
            Ok(copy.outputs[0])
        })
        .expect("one-output copy should normalize to a tunnel");

    assert_eq!(
        assembler
            .evaluate(&net)
            .expect("net should expose its data")
            .as_binary(),
        Some(b"copied".as_slice())
    );
}

#[test]
fn checked_net_builder_reports_wiring_and_finalization_errors() {
    let assembler = Assembler::default();
    let unwired = assembler
        .net(|net| {
            let bind = net.bind();
            Ok(bind.application)
        })
        .expect_err("unwired ports must reject the net");
    assert!(unwired.to_string().contains("is unwired"));

    let duplicate = assembler
        .net(|net| {
            let left = net.data(Value::integer(1));
            let right = net.data(Value::integer(2));
            let other = net.data(Value::integer(3));
            net.wire(left, right)?;
            net.wire(left, other)?;
            Ok(other)
        })
        .expect_err("a port cannot be wired twice");
    assert!(duplicate.to_string().contains("wired more than once"));
}

struct MemoryHost {
    files: HashMap<PathBuf, Bytes>,
}

impl MemoryHost {
    fn new<const N: usize>(files: [(&str, &[u8]); N]) -> Self {
        Self {
            files: files
                .into_iter()
                .map(|(path, bytes)| (PathBuf::from(path), Bytes::copy_from_slice(bytes)))
                .collect(),
        }
    }
}

impl Host for MemoryHost {
    fn read(&self, path: &Path) -> Result<Bytes, HostError> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| HostError::new(format!("missing virtual file `{}`", path.display())))
    }
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
                Value::record([("memory", Value::text(path.display().to_string()))]),
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
