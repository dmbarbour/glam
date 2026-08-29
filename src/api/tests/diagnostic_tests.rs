use super::super::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::core::{Builtin, Key, Value as CoreValue};
use crate::diagnostic::Severity;
use crate::evaluation::{EvalContext, EvaluationMachinePoll, EvaluationTaskMachine};
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
fn diagnostic_ingress_is_runtime_bound_and_installed_once() {
    let owner = EvaluationRuntime::new(0).expect("owner runtime should build");
    let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
    let bus = DiagnosticBus::for_runtime(&owner);
    let (_ingress, _reader) = bus
        .diagnostic_ingress(&owner)
        .expect("first ingress should attach");

    assert!(bus.diagnostic_ingress(&owner).is_err());
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
    let (ingress, _reader) = bus
        .diagnostic_ingress(&runtime)
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
fn diagnostic_consumer_activation_hides_route_root_intermediate_state() {
    struct CompleteTask;

    impl EvaluationTaskMachine for CompleteTask {
        fn poll(
            &mut self,
            _context: &crate::evaluation::EvaluationPollContext,
            _step_budget: usize,
        ) -> EvaluationMachinePoll {
            EvaluationMachinePoll::Complete(crate::core::keys::unit_value())
        }
    }

    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let _assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("runtime reflection profile should seal");
    let bus = DiagnosticBus::for_runtime(&runtime);
    let (ingress, reader) = bus
        .diagnostic_ingress(&runtime)
        .expect("diagnostic ingress should attach");
    let fallback = runtime
        .output_endpoint(Ok::<Value, Error>, |_value| Ok(()))
        .expect("fallback endpoint should register");
    ingress
        .set_fallback_output(&fallback.writer())
        .expect("fallback endpoint should belong to the ingress runtime");
    ingress
        .fallback()
        .expect("diagnostic route should begin on fallback");

    let session = runtime
        .new_evaluation_session()
        .expect("diagnostic consumer session should open");
    let context = EvalContext::new(&session);
    let prepared = context
        .prepare_machine(None, |_| Ok(Box::new(CompleteTask)))
        .expect("diagnostic consumer should prepare");

    let activation_runtime = runtime.clone();
    let activation_ingress = ingress.clone();
    let (inside, inside_wait) = std::sync::mpsc::channel();
    let (release, release_wait) = std::sync::mpsc::channel();
    let activation = std::thread::spawn(move || {
        activation_runtime
            .activate_diagnostic_consumer(&activation_ingress, |mutation| {
                inside
                    .send(())
                    .expect("activation should expose its deterministic barrier");
                release_wait
                    .recv()
                    .expect("activation barrier should be released");
                prepared.activate_guarded(mutation)
            })
            .expect("diagnostic consumer activation should commit");
        prepared.finish_guarded_activation(true);
        prepared.into_handle()
    });
    inside_wait
        .recv()
        .expect("activation should reach the route/root barrier");

    assert!(
        matches!(runtime.readiness(), RuntimeReadiness::Busy),
        "readiness must conservatively hide the active route before its root"
    );
    let publishing_bus = bus.clone();
    let publishing_values = runtime.values();
    let (publication_started, publication_start_wait) = std::sync::mpsc::channel();
    let (publication_done, publication_wait) = std::sync::mpsc::channel();
    let publisher = std::thread::spawn(move || {
        publication_started
            .send(())
            .expect("publisher start should be received");
        publication_done
            .send(publishing_bus.publish_local(Diagnostic::new(
                &publishing_values,
                Severity::Info,
                "after atomic activation",
            )))
            .expect("publication result should be received");
    });
    publication_start_wait
        .recv()
        .expect("publisher should reach the guarded publication");
    assert!(
        publication_wait
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err(),
        "publishers must not route input through the intermediate state"
    );

    release
        .send(())
        .expect("activation barrier should be releasable");
    let _task = activation.join().expect("activation should not panic");
    publication_wait
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("publication should resume after activation");
    publisher.join().expect("publisher should not panic");

    let (_, store, events) = runtime.transaction_snapshot();
    let mut journal = RuntimeEventJournal::new(events);
    let admitted = journal
        .read(&reader)
        .expect("diagnostic input should be readable")
        .expect("post-activation diagnostic should enter the input FIFO");
    assert_eq!(
        Diagnostic::from_transport_value(&admitted)
            .expect("input should retain a diagnostic envelope")
            .message(),
        "after atomic activation"
    );
    assert_eq!(
        runtime.try_commit_transaction(&StoreJournal::new(store), &journal),
        StoreCommitResult::Committed
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
    let (_ingress, reader) = bus
        .diagnostic_ingress(&runtime)
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
    while let Some(value) = journal.read(&reader).expect("ingress should be readable") {
        received.push(
            Diagnostic::from_transport_value(&value)
                .expect("ingress should retain diagnostic envelopes")
                .message()
                .to_owned(),
        );
    }
    assert_eq!(
        received,
        published.values().cloned().collect::<Vec<_>>(),
        "runtime FIFO order must follow bus sequence, not callback arrival"
    );
    assert_eq!(
        runtime.try_commit_transaction(&crate::reflection::StoreJournal::new(store), &journal),
        crate::reflection::StoreCommitResult::Committed
    );
}

#[test]
fn diagnostic_ingress_transfers_buffered_and_later_values_to_fallback() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let bus = DiagnosticBus::for_runtime(&runtime);
    let (ingress, reader) = bus
        .diagnostic_ingress(&runtime)
        .expect("ingress should attach");
    let received = Arc::new(Mutex::new(Vec::new()));
    let callback_values = received.clone();
    let callback_runtime = runtime.clone();
    let fallback = runtime
        .output_endpoint(
            |value| Diagnostic::from_transport_value(&value),
            move |diagnostic| {
                assert!(
                    callback_runtime.exclusive_admission_available(),
                    "fallback rendering must run outside runtime admission"
                );
                callback_values
                    .lock()
                    .expect("fallback collection mutex should not be poisoned")
                    .push(diagnostic.message().to_owned());
                Ok(())
            },
        )
        .expect("fallback output should register");
    let (writer, delivery) = fallback.into_parts();
    ingress
        .set_fallback_output(&writer)
        .expect("fallback should share the ingress runtime");

    for message in ["buffered one", "buffered two"] {
        bus.publish_local(Diagnostic::new(&runtime.values(), Severity::Info, message));
    }
    assert_eq!(ingress.fallback().expect("fallback should activate"), 2);
    let (_, _, snapshot) = runtime.transaction_snapshot();
    assert!(
        RuntimeEventJournal::new(snapshot)
            .read(&reader)
            .expect("transferred input should remain readable as empty")
            .is_none()
    );
    while delivery
        .deliver_next()
        .expect("fallback delivery should remain usable")
        .is_some()
    {}

    bus.publish_local(Diagnostic::new(
        &runtime.values(),
        Severity::Warning,
        "later fallback",
    ));
    let (_, _, snapshot) = runtime.transaction_snapshot();
    assert!(
        RuntimeEventJournal::new(snapshot)
            .read(&reader)
            .expect("fallback publication should not enter the logger FIFO")
            .is_none()
    );
    assert!(
        delivery
            .deliver_next()
            .expect("later fallback delivery should remain usable")
            .is_some()
    );
    assert!(
        delivery
            .deliver_next()
            .expect("fallback delivery should become empty")
            .is_none()
    );
    assert_eq!(
        ingress.fallback().expect("fallback should be idempotent"),
        0
    );
    assert_eq!(
        *received
            .lock()
            .expect("fallback collection mutex should not be poisoned"),
        ["buffered one", "buffered two", "later fallback"]
    );

    ingress.activate().expect("ingress should rearm");
    bus.publish_local(Diagnostic::new(
        &runtime.values(),
        Severity::Info,
        "rearmed",
    ));
    let (_, _, snapshot) = runtime.transaction_snapshot();
    let value = RuntimeEventJournal::new(snapshot)
        .read(&reader)
        .expect("rearmed input should be readable")
        .expect("rearmed publication should enter the logger FIFO");
    assert_eq!(
        Diagnostic::from_transport_value(&value)
            .expect("rearmed input should remain a diagnostic")
            .message(),
        "rearmed"
    );
}

#[test]
fn diagnostic_fallback_drain_invalidates_a_stale_fifo_claim() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let bus = DiagnosticBus::for_runtime(&runtime);
    let (ingress, reader) = bus
        .diagnostic_ingress(&runtime)
        .expect("ingress should attach");
    let fallback = runtime
        .output_endpoint(
            |value| Diagnostic::from_transport_value(&value),
            |_: Diagnostic| Ok(()),
        )
        .expect("fallback output should register");
    let (writer, delivery) = fallback.into_parts();
    ingress
        .set_fallback_output(&writer)
        .expect("fallback should share the ingress runtime");
    bus.publish_local(Diagnostic::new(
        &runtime.values(),
        Severity::Info,
        "stale logger claim",
    ));
    let (_, store, snapshot) = runtime.transaction_snapshot();
    let mut events = RuntimeEventJournal::new(snapshot);
    assert!(events.read(&reader).unwrap().is_some());

    assert_eq!(ingress.fallback().expect("fallback should activate"), 1);
    assert_eq!(
        runtime.try_commit_transaction(&StoreJournal::new(store), &events),
        StoreCommitResult::Conflict
    );
    assert!(
        delivery
            .deliver_next()
            .expect("fallback delivery should remain valid")
            .is_some()
    );
}

#[test]
fn diagnostic_publication_racing_fallback_is_delivered_once_in_sequence_order() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let bus = DiagnosticBus::for_runtime(&runtime);
    let (ingress, _reader) = bus
        .diagnostic_ingress(&runtime)
        .expect("ingress should attach");
    let received = Arc::new(Mutex::new(Vec::new()));
    let callback_values = received.clone();
    let fallback = runtime
        .output_endpoint(
            |value| Diagnostic::from_transport_value(&value),
            move |diagnostic| {
                callback_values
                    .lock()
                    .expect("fallback collection mutex should not be poisoned")
                    .push(diagnostic.message().to_owned());
                Ok(())
            },
        )
        .expect("fallback output should register");
    let (writer, delivery) = fallback.into_parts();
    ingress
        .set_fallback_output(&writer)
        .expect("fallback should share the ingress runtime");

    let values = runtime.values();
    let barrier = Arc::new(std::sync::Barrier::new(49));
    let publishers = (0..48)
        .map(|index| {
            let bus = bus.clone();
            let values = values.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let message = format!("racing {index}");
                let event =
                    bus.publish_local(Diagnostic::new(&values, Severity::Info, message.clone()));
                (event.sequence(), message)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    ingress
        .fallback()
        .expect("the route switch may race publications");
    let published = publishers
        .into_iter()
        .map(|publisher| publisher.join().expect("publisher should not panic"))
        .collect::<BTreeMap<_, _>>();
    ingress
        .fallback()
        .expect("a second pass should transfer any pre-switch admission");
    while delivery
        .deliver_next()
        .expect("fallback delivery should remain usable")
        .is_some()
    {}

    assert_eq!(
        *received
            .lock()
            .expect("fallback collection mutex should not be poisoned"),
        published.values().cloned().collect::<Vec<_>>(),
        "every racing publication must select exactly one route and preserve bus order"
    );
}

#[test]
fn runtime_retains_the_installed_diagnostic_ingress() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let bus = DiagnosticBus::for_runtime(&runtime);
    let (ingress, reader) = bus
        .diagnostic_ingress(&runtime)
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
        .read(&reader)
        .expect("retained ingress should remain readable")
        .expect("publication should reach the stable ingress");
    assert_eq!(
        Diagnostic::from_transport_value(&value)
            .expect("ingress should retain a diagnostic envelope")
            .message(),
        "still routed"
    );
}

#[test]
fn diagnostic_subscribers_run_after_runtime_admission_is_released() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let bus = DiagnosticBus::for_runtime(&runtime);
    let (_ingress, _reader) = bus
        .diagnostic_ingress(&runtime)
        .expect("ingress should attach");
    let callback_runtime = runtime.clone();
    let _subscription = bus.subscribe(DiagnosticCallback(move |_| {
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
    let (ingress, _reader) = bus
        .diagnostic_ingress(&runtime)
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
    let CoreValue::Dict(emission) = received[0].emission().as_core() else {
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
        .cloned()
        .expect("text diagnostic should provide msg")
    else {
        unreachable!()
    };
    let interface = interface.insert(
        (*crate::core::keys::SEVERITY).clone(),
        crate::core::test_value_factory().error(),
    );
    let message = CoreValue::Dict(message.insert(
        (*crate::core::keys::MSG).clone(),
        CoreValue::Dict(interface),
    ));

    let values = EvaluationRuntime::new(0).unwrap().values();
    let trace = test_compilation_trace("test.g");
    let diagnostic = Diagnostic::from_compile(values.core(), &trace, Severity::Warning, message);
    assert_eq!(diagnostic.severity(), Severity::Warning);

    let CoreValue::Dict(emission) = diagnostic.emission().as_core() else {
        panic!("raw diagnostic should be a dictionary");
    };
    let Some(CoreValue::Dict(interface)) = emission.get(&*crate::core::keys::MSG) else {
        panic!("raw diagnostic should provide msg");
    };
    assert_eq!(
        interface.get(&*crate::core::keys::SEVERITY),
        Some(&crate::core::test_value_factory().error())
    );
    assert!(interface.get(&*crate::core::keys::ORIGIN).is_none());
    assert!(emission.get(&*crate::core::keys::SPEC).is_none());

    let enriched = diagnostic
        .enrich(&values)
        .expect("diagnostic should enrich");
    let CoreValue::Dict(enriched) = enriched.as_core() else {
        panic!("enriched diagnostic should be an object dictionary");
    };
    let Some(CoreValue::Dict(interface)) = enriched.get(&*crate::core::keys::MSG) else {
        panic!("enriched diagnostic should provide msg");
    };
    assert_eq!(
        interface.get(&*crate::core::keys::SEVERITY),
        Some(&values.core.warn())
    );
    assert_eq!(
        interface
            .get(&*crate::core::keys::ORIGIN)
            .and_then(|origin| match origin {
                CoreValue::Dict(origin) => origin.get(&*crate::core::keys::SOURCE),
                _ => None,
            })
            .and_then(|source| match source {
                CoreValue::Dict(source) => source.get(&*crate::core::keys::FILE),
                _ => None,
            }),
        Some(&CoreValue::binary_from_text("test.g"))
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

#[test]
fn viewers_can_inherit_one_diagnostic_independently() {
    let trace = test_compilation_trace("test.g");
    let values = EvaluationRuntime::new(0).unwrap().values();
    let diagnostic = Diagnostic::from_compile(
        values.core(),
        &trace,
        Severity::Info,
        crate::diagnostic::text_message(Some(3), "hello"),
    );
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
    let CoreValue::Dict(original) = diagnostic.emission().as_core() else {
        unreachable!()
    };
    let CoreValue::Dict(first) = first.as_core() else {
        unreachable!()
    };
    let CoreValue::Dict(second) = second.as_core() else {
        unreachable!()
    };
    assert!(original.get(&viewer_key).is_none());
    assert_eq!(
        first.get(&viewer_key),
        Some(&CoreValue::binary_from_text("terminal"))
    );
    assert_eq!(
        second.get(&viewer_key),
        Some(&CoreValue::binary_from_text("ide"))
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
