use super::super::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::core::{Builtin, Key, TestAccess as _, Value as CoreValue};
use crate::diagnostic::Severity;
use crate::evaluation::EvalContext;
use crate::reflection::{StoreCommitResult, StoreJournal};

use super::test_compilation_trace;
use crate::api::diagnostics::DiagnosticCallback;

#[test]
fn diagnostic_bus_sequences_counts_and_delivers_only_to_current_subscribers() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let values = runtime.values();
    let bus = DiagnosticBus::new();
    assert_eq!(bus.counts().latest_sequence(), 0);
    let early = Arc::new(Mutex::new(Vec::new()));
    let early_events = early.clone();
    let early_subscription = bus.subscribe(DiagnosticCallback(move |event| {
        early_events
            .lock()
            .expect("early diagnostic collector should not be poisoned")
            .push(event);
    }));

    let first = bus.publish_local(Diagnostic::new(&values, Severity::Info, "first"));
    let late = Arc::new(Mutex::new(Vec::new()));
    let late_events = late.clone();
    let _late_subscription = bus.subscribe(DiagnosticCallback(move |event| {
        late_events
            .lock()
            .expect("late diagnostic collector should not be poisoned")
            .push(event);
    }));
    let second = bus.publish_local(Diagnostic::new(&values, Severity::Warning, "second"));
    drop(early_subscription);
    let third = bus.publish_local(Diagnostic::new(&values, Severity::Error, "third"));

    assert_eq!(first.sequence(), 1);
    assert_eq!(second.sequence(), 2);
    assert_eq!(third.sequence(), 3);
    assert_eq!(
        bus.counts(),
        DiagnosticCounts {
            next_sequence: 4,
            info: 1,
            warnings: 1,
            errors: 1,
        }
    );

    let early = early
        .lock()
        .expect("early diagnostic collector should not be poisoned");
    assert_eq!(early.len(), 2);
    assert_eq!(early[0].message(), "first");
    assert_eq!(early[1].message(), "second");
    let late = late
        .lock()
        .expect("late diagnostic collector should not be poisoned");
    assert_eq!(
        late.iter()
            .map(|event| (event.sequence(), event.message()))
            .collect::<Vec<_>>(),
        [(2, "second"), (3, "third")]
    );
}

#[test]
fn panicking_subscriber_destructor_leaves_the_bus_usable() {
    struct PanicOnDrop;
    impl DiagnosticSubscriber for PanicOnDrop {
        fn receive(&self, _event: DiagnosticEvent) {}
    }
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("forced subscriber destructor panic");
        }
    }

    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let values = runtime.values();
    let bus = DiagnosticBus::new();
    let received = Arc::new(Mutex::new(Vec::new()));
    let observed = received.clone();
    let _survivor = bus.subscribe(DiagnosticCallback(move |event| {
        observed
            .lock()
            .expect("diagnostic collector should not be poisoned")
            .push(event);
    }));
    let panicking = bus.subscribe(PanicOnDrop);

    // The subscriber is dropped after the bus lock is released, so its
    // panic reaches the client without poisoning the bus.
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(panicking)));
    assert!(unwind.is_err());

    let event = bus.publish_local(Diagnostic::new(&values, Severity::Info, "after"));
    assert_eq!(event.sequence(), 1);
    assert_eq!(
        received
            .lock()
            .expect("diagnostic collector should not be poisoned")
            .len(),
        1
    );
}

#[test]
fn unrecognized_annotations_warn_once_through_the_bus() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let observed = warnings.clone();
    let _subscription =
        assembler
            .diagnostic_bus()
            .subscribe(DiagnosticCallback(move |event: DiagnosticEvent| {
                observed
                    .lock()
                    .expect("warning collector should not be poisoned")
                    .push(event.message().to_string());
            }));
    let annotated = values
        .anno(values.atom_from_text("unknown_marker"), values.integer(42))
        .expect("same-runtime annotation construction should succeed");

    // Evaluation records the annotation; the assembler publishes one warning
    // per runtime, not one per evaluation.
    for _ in 0..2 {
        let result = assembler
            .evaluator()
            .eval(&annotated)
            .expect("an unrecognized annotation is transparent");
        assert_eq!(result.as_i64().expect("result should be numeric"), Some(42));
    }
    let warnings = warnings
        .lock()
        .expect("warning collector should not be poisoned")
        .clone();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("unrecognized annotation"));
    assert!(warnings[0].contains("unknown_marker"));
}

#[test]
fn panicking_subscriber_is_skipped_for_that_event() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let values = runtime.values();
    let bus = DiagnosticBus::new();
    let _panicking = bus.subscribe(DiagnosticCallback(|_| panic!("forced subscriber panic")));
    let received = Arc::new(Mutex::new(Vec::new()));
    let observed = received.clone();
    let _survivor = bus.subscribe(DiagnosticCallback(move |event| {
        observed
            .lock()
            .expect("diagnostic collector should not be poisoned")
            .push(event);
    }));

    // Publication, which may be part of a runtime commit, is never
    // interrupted by a subscriber's panic.
    let first = bus.publish_local(Diagnostic::new(&values, Severity::Info, "first"));
    let second = bus.publish_local(Diagnostic::new(&values, Severity::Info, "second"));
    assert_eq!((first.sequence(), second.sequence()), (1, 2));
    assert_eq!(
        received
            .lock()
            .expect("diagnostic collector should not be poisoned")
            .len(),
        2
    );
}

#[test]
fn diagnostic_events_retain_emission_and_origin_roots_until_retirement() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let domain = EffectTokenDomain::new(&runtime.values());
    let retained_events = Arc::new(Mutex::new(Vec::new()));
    let callback_events = retained_events.clone();
    let bus = DiagnosticBus::for_runtime(&runtime);
    let _subscription = bus.subscribe(DiagnosticCallback(move |event| {
        callback_events
            .lock()
            .expect("diagnostic retention callback should not be poisoned")
            .push(event);
    }));
    let emission_payload = Arc::new(());
    let origin_payload = Arc::new(());
    let retained_emission = Arc::downgrade(&emission_payload);
    let retained_origin = Arc::downgrade(&origin_payload);
    let diagnostic = Diagnostic {
        emission: domain.issue(emission_payload),
        origin: Some(domain.issue(origin_payload)),
        source: None,
        severity: Severity::Info,
        line: None,
        message: Arc::from("retained diagnostic"),
    };
    let event = bus
        .publish(diagnostic)
        .expect("same-runtime diagnostic should publish");

    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(retained_emission.upgrade().is_some());
    assert!(retained_origin.upgrade().is_some());
    drop(event);
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(retained_emission.upgrade().is_some());
    assert!(retained_origin.upgrade().is_some());
    retained_events
        .lock()
        .expect("diagnostic retention callback should not be poisoned")
        .clear();
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(retained_emission.upgrade().is_none());
    assert!(retained_origin.upgrade().is_none());
}

#[test]
fn diagnostic_ingress_retains_roots_through_port_and_committed_read() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let domain = EffectTokenDomain::new(&runtime.values());
    let bus = DiagnosticBus::for_runtime(&runtime);
    let _ingress = bus
        .route_to_port(&runtime, (), "diagnostics")
        .expect("diagnostic ingress should attach");
    let emission_payload = Arc::new(());
    let retained = Arc::downgrade(&emission_payload);
    let event = bus
        .publish(Diagnostic {
            emission: domain.issue(emission_payload),
            origin: None,
            source: None,
            severity: Severity::Info,
            line: None,
            message: Arc::from("retained ingress diagnostic"),
        })
        .expect("same-runtime diagnostic should publish");

    drop(event);
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(retained.upgrade().is_some());

    let (_, store, snapshot) = runtime.transaction_snapshot();
    let mut journal = RuntimeEventJournal::new(snapshot);
    let transported = journal
        .read_port((), "diagnostics")
        .expect("the port should retain its diagnostic value");
    assert_eq!(
        runtime.try_commit_transaction(&StoreJournal::new(store), &journal),
        StoreCommitResult::Committed
    );
    drop(journal);
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(retained.upgrade().is_some());

    drop(transported);
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(retained.upgrade().is_none());
}

#[test]
fn diagnostic_ingress_is_runtime_bound_and_installed_once() {
    let owner = EvaluationRuntime::new(0).expect("owner runtime should build");
    let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
    let bus = DiagnosticBus::for_runtime(&owner);
    let _ingress = bus
        .route_to_port(&owner, (), "diagnostics")
        .expect("first ingress should attach");

    assert!(bus.route_to_port(&owner, (), "diagnostics").is_err());
    assert!(bus.bind_runtime(&foreign).is_err());
    assert!(
        bus.publish(Diagnostic::new(
            &foreign.values(),
            Severity::Error,
            "foreign diagnostic",
        ))
        .is_err()
    );
    assert!(
        bus.publish_from_runtime(
            foreign.id(),
            Diagnostic::new(&foreign.values(), Severity::Error, "foreign diagnostic"),
        )
        .is_err()
    );
    assert_eq!(bus.counts().total(), 0);
}

#[test]
fn diagnostic_ingress_preparation_failure_still_counts_the_bus_publication() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let values = runtime.values();
    let bus = DiagnosticBus::for_runtime(&runtime);
    let ingress = bus
        .route_to_port(&runtime, (), "diagnostics")
        .expect("diagnostic ingress should attach");
    drop(runtime);

    let event = bus.publish_local(Diagnostic::new(
        &values,
        Severity::Error,
        "publication outlived its runtime",
    ));

    assert_eq!(event.sequence(), 1);
    assert_eq!(bus.counts().errors(), 1);
    assert!(
        ingress
            .failure()
            .expect("transport failure should remain observable")
            .to_string()
            .contains("has been dropped")
    );
}

#[test]
fn diagnostic_value_operations_reject_foreign_runtime_views() {
    let owner = EvaluationRuntime::new(0).expect("owner runtime should build");
    let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
    let diagnostic = Diagnostic::new(&owner.values(), Severity::Error, "owner");

    assert!(diagnostic.enrich(&foreign.values()).is_err());
    assert!(
        diagnostic
            .clone()
            .with_context(&owner.values(), foreign.values().text("foreign context"))
            .is_err()
    );
    assert!(diagnostic.transport_value(&foreign.values()).is_err());
    assert!(
        Diagnostic::apply_updates(
            &owner.values(),
            diagnostic.emission(),
            foreign.values().empty_dict(),
        )
        .is_err()
    );
}

#[test]
fn diagnostic_ingress_admits_in_bus_sequence_order() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let bus = DiagnosticBus::for_runtime(&runtime);
    let _ingress = bus
        .route_to_port(&runtime, (), "diagnostics")
        .expect("ingress should attach");
    let before = runtime.transaction_snapshot().0;
    let values = runtime.values();
    let published = (0..24)
        .map(|index| {
            let bus = bus.clone();
            let values = values.clone();
            std::thread::spawn(move || {
                let message = format!("message {index}");
                let event =
                    bus.publish_local(Diagnostic::new(&values, Severity::Info, message.clone()));
                (event.sequence(), message)
            })
        })
        .map(|thread| thread.join().expect("publisher should not panic"))
        .collect::<BTreeMap<_, _>>();
    assert_ne!(runtime.transaction_snapshot().0, before);

    let (_, store, snapshot) = runtime.transaction_snapshot();
    let mut journal = RuntimeEventJournal::new(snapshot);
    let mut received = Vec::new();
    while let Some(value) = journal.read_port((), "diagnostics") {
        received.push(Diagnostic::message_text_for_test(&runtime.values(), &value));
    }
    assert_eq!(
        received,
        published.values().cloned().collect::<Vec<_>>(),
        "the port's order must follow bus sequence, not callback arrival"
    );
    assert_eq!(
        runtime.try_commit_transaction(&crate::reflection::StoreJournal::new(store), &journal),
        crate::reflection::StoreCommitResult::Committed
    );
}

#[test]
fn runtime_retains_the_installed_diagnostic_ingress() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let bus = DiagnosticBus::for_runtime(&runtime);
    let ingress = bus
        .route_to_port(&runtime, (), "diagnostics")
        .expect("ingress should attach");
    drop(ingress);

    bus.publish_local(Diagnostic::new(
        &runtime.values(),
        Severity::Info,
        "still routed",
    ));
    let (_, _, snapshot) = runtime.transaction_snapshot();
    let mut journal = RuntimeEventJournal::new(snapshot);
    let value = journal
        .read_port((), "diagnostics")
        .expect("publication should reach the stable ingress");
    assert_eq!(
        Diagnostic::message_text_for_test(&runtime.values(), &value),
        "still routed"
    );
}

#[test]
fn diagnostic_subscribers_run_after_runtime_admission_is_released() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let bus = DiagnosticBus::for_runtime(&runtime);
    let _ingress = bus
        .route_to_port(&runtime, (), "diagnostics")
        .expect("ingress should attach");
    let callback_runtime = runtime.clone();
    let _subscription = bus.subscribe(DiagnosticCallback(move |_| {
        callback_runtime
            .values()
            .core()
            .collect_managed_for_test()
            .expect("diagnostic subscribers must not inherit managed access");
        assert!(
            callback_runtime.exclusive_admission_available(),
            "ordinary callbacks must run outside runtime mutation admission"
        );
    }));

    bus.publish_local(Diagnostic::new(
        &runtime.values(),
        Severity::Info,
        "ordered",
    ));
}

#[test]
fn diagnostic_bus_and_ingress_do_not_retain_the_runtime() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let retained = Arc::downgrade(&runtime.state);
    let bus = DiagnosticBus::for_runtime(&runtime);
    let ingress = bus
        .route_to_port(&runtime, (), "diagnostics")
        .expect("ingress should attach");

    let diagnostic = Diagnostic::new(&runtime.values(), Severity::Info, "after runtime");
    drop(runtime);
    assert!(retained.upgrade().is_none());
    bus.publish_local(diagnostic);
    assert!(ingress.failure().is_some());
}

#[test]
fn diagnostic_callback_subscribes_to_the_existing_session() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let callback_values = received.clone();
    let assembler = Assembler::default().with_diagnostic_callback(move |diagnostic| {
        callback_values
            .lock()
            .expect("callback collection mutex should not be poisoned")
            .push(diagnostic);
    });

    assembler.record_diagnostic(Diagnostic::new(
        &assembler.values(),
        Severity::Info,
        "hello",
    ));

    assert_eq!(
        received
            .lock()
            .expect("callback collection mutex should not be poisoned")[0]
            .message(),
        "hello"
    );
    let received = received
        .lock()
        .expect("callback collection mutex should not be poisoned");
    let emission = received[0].emission().clone_core_for_test();
    let CoreValue::Dict(emission) = &emission else {
        unreachable!()
    };
    assert!(emission.get(&*crate::core::keys::SPEC).is_none());
}

#[test]
fn diagnostic_enrichment_is_an_authoritative_object_mixin() {
    let CoreValue::Dict(message) = crate::diagnostic::text_message(Some(7), "careful") else {
        unreachable!()
    };
    let CoreValue::Dict(interface) = message
        .get(&*crate::core::keys::MSG)
        .map(|value| value.duplicate_for_test(&crate::core::test_value_factory()))
        .expect("text diagnostic should provide msg")
    else {
        unreachable!()
    };
    let fixture_values = crate::core::test_value_factory();
    let interface = interface.insert(
        (*crate::core::keys::SEVERITY).clone(),
        fixture_values.with_boundary_access(|access| access.error()),
    );
    let message = CoreValue::Dict(message.insert(
        (*crate::core::keys::MSG).clone(),
        CoreValue::Dict(interface),
    ));

    let values = EvaluationRuntime::new(0).unwrap().values();
    let trace = test_compilation_trace("test.g");
    let message = crate::compiler::CompileDiagnostic {
        emission: values.core().construct_runtime_value_root(|_| message),
        line: Some(7),
        text: Arc::from("careful"),
    };
    let diagnostic = Diagnostic::from_compile(values.core(), &trace, Severity::Warning, &message);
    assert_eq!(diagnostic.severity(), Severity::Warning);

    let emission = diagnostic.emission().clone_core_for_test();
    let CoreValue::Dict(emission) = &emission else {
        panic!("raw diagnostic should be a dictionary");
    };
    let Some(CoreValue::Dict(interface)) = emission.get(&*crate::core::keys::MSG) else {
        panic!("raw diagnostic should provide msg");
    };
    fixture_values.with_boundary_access(|access| {
        access.assert_same_representation_for_test(
            &interface.get(&*crate::core::keys::SEVERITY),
            &Some(&access.error()),
        );
    });
    assert!(interface.get(&*crate::core::keys::ORIGIN).is_none());
    assert!(emission.get(&*crate::core::keys::SPEC).is_none());

    let enriched = diagnostic
        .enrich(&values)
        .expect("diagnostic should enrich");
    // A renderer evaluates the composed enrichment.
    let enriched = crate::evaluation::EvalContext::isolated(values.core().clone())
        .evaluate_compatibility_whnf(&enriched.clone_core_for_test())
        .unwrap_or_else(|_| panic!("the enriched diagnostic should evaluate"));
    let CoreValue::Dict(enriched) = &enriched else {
        panic!("enriched diagnostic should be an object dictionary");
    };
    let Some(CoreValue::Dict(interface)) = enriched.get(&*crate::core::keys::MSG) else {
        panic!("enriched diagnostic should provide msg");
    };
    values.core.with_boundary_access(|access| {
        access.assert_same_representation_for_test(
            &interface.get(&*crate::core::keys::SEVERITY),
            &Some(&access.warn()),
        );
    });
    values.core().assert_same_representation_for_test(
        &interface
            .get(&*crate::core::keys::ORIGIN)
            .and_then(|origin| match origin {
                CoreValue::Dict(origin) => origin.get(&*crate::core::keys::SOURCE),
                _ => None,
            })
            .and_then(|source| match source {
                CoreValue::Dict(source) => source.get(&*crate::core::keys::FILE),
                _ => None,
            }),
        &Some(&CoreValue::binary_from_text("test.g")),
    );

    let Some(CoreValue::Dict(spec)) = enriched.get(&*crate::core::keys::SPEC) else {
        panic!("each diagnostic mixin should update the object specification");
    };
    assert!(matches!(
        spec.get(&*crate::core::keys::DEFS),
        Some(CoreValue::PartialBuiltin(call))
            if call.builtin == Builtin::ObjectComposedDefs
    ));
}

/// Enrichment, updates and context compose mixins without evaluating the
/// diagnostic; its renderer evaluates it
/// (`senders-never-evaluate-diagnostics`).
#[test]
fn diagnostic_enrichment_composes_without_evaluating() {
    let runtime = EvaluationRuntime::new(0).expect("test runtime should build");
    let values = runtime.values();
    let diagnostic = Diagnostic::new(&values, Severity::Warning, "composed warning");
    let updates = values
        .record([("viewer", values.text("terminal"))])
        .expect("viewer updates are local");
    let composed = [
        diagnostic
            .enrich(&values)
            .expect("enrichment should compose"),
        diagnostic
            .enrich_with(&values, updates.clone())
            .expect("enrichment with updates should compose"),
        Diagnostic::apply_updates(&values, diagnostic.emission(), updates)
            .expect("updates should compose"),
        diagnostic
            .clone()
            .with_context(&values, values.text("context"))
            .expect("context should compose")
            .emission()
            .clone(),
    ];
    for value in composed {
        assert!(matches!(value.clone_core_for_test(), CoreValue::Lazy(_)));
    }
}

#[test]
fn viewers_can_inherit_one_diagnostic_independently() {
    let trace = test_compilation_trace("test.g");
    let values = EvaluationRuntime::new(0).unwrap().values();
    let message = crate::compiler::CompileDiagnostic {
        emission: values
            .core()
            .construct_runtime_value_root(|_| crate::diagnostic::text_message(Some(3), "hello")),
        line: Some(3),
        text: Arc::from("hello"),
    };
    let diagnostic = Diagnostic::from_compile(values.core(), &trace, Severity::Info, &message);
    let viewer_key = Key::atom_from_text("viewer");
    let inherit = |name: &str| {
        diagnostic
            .enrich_with(
                &values,
                values
                    .record([("viewer", values.text(name))])
                    .expect("viewer value is local"),
            )
            .expect("viewer mixin should apply")
    };

    let first = inherit("terminal");
    let second = inherit("ide");
    let original = diagnostic.emission().clone_core_for_test();
    let CoreValue::Dict(original) = &original else {
        unreachable!()
    };
    // Each view is composed; its renderer evaluates it.
    let context = EvalContext::isolated(values.core().clone());
    let render = |view: Value| {
        context
            .evaluate_compatibility_whnf(&view.clone_core_for_test())
            .unwrap_or_else(|_| panic!("a viewer's diagnostic should evaluate"))
    };
    let first = render(first);
    let CoreValue::Dict(first) = &first else {
        unreachable!()
    };
    let second = render(second);
    let CoreValue::Dict(second) = &second else {
        unreachable!()
    };
    assert!(original.get(&viewer_key).is_none());
    values.core().assert_same_representation_for_test(
        &first.get(&viewer_key),
        &Some(&CoreValue::binary_from_text("terminal")),
    );
    values.core().assert_same_representation_for_test(
        &second.get(&viewer_key),
        &Some(&CoreValue::binary_from_text("ide")),
    );
    assert!(matches!(
        first
            .get(&*crate::core::keys::SPEC)
            .and_then(|spec| match spec {
                CoreValue::Dict(spec) => spec.get(&*crate::core::keys::DEFS),
                _ => None,
            }),
        Some(CoreValue::PartialBuiltin(call))
            if call.builtin == Builtin::ObjectComposedDefs
    ));
}

/// The `msg.text` and `msg.context` of an evaluated diagnostic, evaluating
/// each as a renderer would.
fn evaluated_message_fields(
    context: &EvalContext,
    diagnostic: &CoreValue,
) -> (CoreValue, Vec<CoreValue>) {
    let values = context.values();
    let field = |value: &CoreValue, key: &Key| -> CoreValue {
        let CoreValue::Dict(dict) = value else {
            panic!("an evaluated diagnostic level should be a dictionary");
        };
        let member = dict
            .get(key)
            .map(|member| member.duplicate_for_test(values))
            .unwrap_or_else(|| CoreValue::Dict(crate::core::Dict::new_sync()));
        context
            .evaluate_compatibility_whnf(&member)
            .expect("a diagnostic member should evaluate")
    };
    let message = field(diagnostic, &crate::core::keys::MSG);
    let text = field(&message, &crate::core::keys::TEXT);
    let CoreValue::List(frames) = field(&message, &crate::core::keys::CONTEXT) else {
        panic!("a diagnostic's context should evaluate to a list");
    };
    let mut items = Vec::new();
    frames
        .for_each_segment(
            &mut |bytes| panic!("diagnostic context must not hold bytes: {bytes:?}"),
            &mut |segment| {
                items.extend(segment.iter().map(|frame| frame.duplicate_for_test(values)));
                Ok::<_, ()>(())
            },
        )
        .expect("diagnostic context frames should be materialized");
    (text, items)
}

/// A failure's diagnostic is composed without evaluating anything, and,
/// once a renderer evaluates it, it carries the emission's text with the
/// failure's frames before the emission's own context: a list follows them,
/// a single frame counts as one, and an absent context adds nothing
/// (`senders-never-evaluate-diagnostics`).
#[test]
fn failure_diagnostics_compose_lazily_and_prepend_their_frames() {
    let runtime = EvaluationRuntime::new(0).expect("test runtime should build");
    let values = runtime.values();
    let core = values.core();
    let context = EvalContext::isolated(core.clone());
    let frame = |name: &str| crate::diagnostic::evaluation_context_frame(name);
    let message = |context: Option<CoreValue>| {
        let mut interface = crate::core::Dict::new_sync().insert(
            crate::core::keys::TEXT.clone(),
            CoreValue::binary_from_text("emitted"),
        );
        if let Some(context) = context {
            interface = interface.insert(crate::core::keys::CONTEXT.clone(), context);
        }
        CoreValue::Dict(
            crate::core::Dict::new_sync()
                .insert(crate::core::keys::MSG.clone(), CoreValue::Dict(interface)),
        )
    };
    let cases = [
        (None, vec![frame("outer"), frame("inner")], "plain"),
        (
            Some(message(Some(CoreValue::List(
                crate::core::List::from_values(vec![frame("own")]),
            )))),
            vec![frame("outer"), frame("own")],
            "emitted",
        ),
        (
            Some(message(Some(frame("own")))),
            vec![frame("outer"), frame("own")],
            "emitted",
        ),
        (Some(message(None)), vec![frame("outer")], "emitted"),
        (
            Some(CoreValue::binary_from_text("binary")),
            vec![frame("outer")],
            "binary",
        ),
    ];
    for (emission, expected_frames, expected_text) in cases {
        let diagnostic = core.construct_runtime_value_root(|access| {
            let failure = match &emission {
                Some(emission) => crate::core::EvaluationFailure::emission_in(
                    access,
                    access.duplicate_value(emission),
                ),
                None => crate::core::EvaluationFailure::message("plain"),
            };
            let failure = if expected_frames.len() == 2 && emission.is_none() {
                failure
                    .with_context_in(access, frame("inner"))
                    .with_context_in(access, frame("outer"))
            } else {
                failure.with_context_in(access, frame("outer"))
            };
            crate::diagnostic::failure_diagnostic_in(access, &failure)
        });
        let composed = diagnostic.clone_core_for_test();
        assert!(
            matches!(composed, CoreValue::Lazy(_)),
            "a failure's diagnostic should be composed, not evaluated"
        );
        let evaluated = context
            .evaluate_compatibility_whnf(&composed)
            .expect("the composed diagnostic should evaluate");
        let (text, frames) = evaluated_message_fields(&context, &evaluated);
        core.assert_same_representation_for_test(
            &text,
            &CoreValue::binary_from_text(expected_text),
        );
        assert_eq!(frames.len(), expected_frames.len(), "{expected_text}");
        for (frame, expected) in frames.iter().zip(&expected_frames) {
            core.assert_same_representation_for_test(frame, expected);
        }
    }
}

/// A message that fails to evaluate fails its renderer, under a
/// `diagnostic:{from:Frames}` frame that carries where the diagnostic came
/// from, rather than falling back to other text.
#[test]
fn a_failing_diagnostic_message_fails_its_renderer_with_its_origin() {
    let runtime = EvaluationRuntime::new(0).expect("test runtime should build");
    let values = runtime.values();
    let core = values.core();
    let diagnostic = core.construct_runtime_value_root(|access| {
        let broken = CoreValue::builtin_call_in(
            access,
            Builtin::Add,
            vec![
                CoreValue::Dict(crate::core::Dict::new_sync()),
                CoreValue::Number(1.into()),
            ],
        );
        let failure = crate::core::EvaluationFailure::emission_in(access, broken).with_context_in(
            access,
            crate::diagnostic::evaluation_context_frame("origin"),
        );
        crate::diagnostic::failure_diagnostic_in(access, &failure)
    });
    let context = EvalContext::isolated(core.clone());
    let Err(halt) = context.evaluate_compatibility_whnf(&diagnostic.clone_core_for_test()) else {
        panic!("a diagnostic whose message fails should fail to evaluate");
    };
    let failure = halt
        .permanent_failure()
        .expect("a failing message is a permanent failure");
    let expected = CoreValue::Dict(crate::core::Dict::new_sync().insert(
        crate::core::keys::DIAGNOSTIC.clone(),
        CoreValue::Dict(crate::core::Dict::new_sync().insert(
            crate::core::keys::FROM.clone(),
            CoreValue::List(crate::core::List::from_values(vec![
                crate::diagnostic::evaluation_context_frame("origin"),
            ])),
        )),
    ));
    assert!(
        core.test_access(|access| failure
            .contexts()
            .iter()
            .any(|frame| access.same_representation(frame, &expected))),
        "the failure should carry the diagnostic's origin frames"
    );
}
