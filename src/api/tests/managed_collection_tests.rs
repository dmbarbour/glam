use super::super::*;
use std::sync::{Arc, Mutex};

use crate::core::{Key, Value as CoreValue};
use crate::diagnostic::Severity;
use crate::reflection::{StoreCommitResult, StoreJournal};

use super::access_path;
use super::runtime_tests::{decode_test_integer, input_transaction};

fn net_topology_revision(runtime: &EvaluationRuntime, value: &Value) -> u64 {
    let CoreValue::Net(net) = value.clone_core_for_test() else {
        panic!("production net fixture must remain a net value")
    };
    net.runtime()
        .test_with_revisions(runtime.values().core(), |_| ())
        .1
        .topology_revision()
}

#[test]
fn production_collection_preserves_each_serial_boundary() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let bus = DiagnosticBus::for_runtime(&runtime);
    let (_ingress, diagnostic_reader) = bus
        .diagnostic_ingress(&runtime)
        .expect("diagnostic ingress should attach");
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .diagnostic_bus(bus.clone())
        .build()
        .expect("assembler should build");
    let logger = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .diagnostic_bus(bus.clone())
        .build()
        .expect("same-runtime logger service should build");

    let module = assembler
        .module(["i11b_serial_boundaries"])
        .script("g", "language g0\nresult = \"preserved\"\n")
        .build()
        .expect("module should compile");
    let assembly_result =
        access_path(&assembler, module.value(), "result").expect("module should define its result");
    let net = assembler
        .net(|builder| builder.data(assembly_result.clone()))
        .expect("closed production net should build");
    let net_revision = net_topology_revision(&runtime, &net);

    runtime
        .collect_managed_for_maintenance()
        .expect("the post-compilation serial boundary should collect");
    assert_eq!(
        assembler
            .evaluator()
            .eval(&assembly_result)
            .expect("assembly result should survive collection")
            .as_bytes()
            .expect("result extraction should succeed")
            .as_deref(),
        Some(b"preserved".as_slice())
    );
    assert_eq!(net_topology_revision(&runtime, &net), net_revision);

    let (_, reflection_snapshot) = runtime.reflection_snapshot();
    let mut reflection = StoreJournal::new(reflection_snapshot);
    reflection.write(
        vec![Key::atom_from_text("i11b_result")],
        assembly_result.clone(),
    );
    assert_eq!(
        runtime.commit_reflection(&reflection),
        StoreCommitResult::Committed
    );
    drop(reflection);
    runtime.pump_until_stable();
    let RuntimeReadiness::Ready(before_quiescent_collection) = runtime.readiness() else {
        panic!("committed reflection state without work should be ready")
    };
    runtime
        .collect_managed_for_maintenance()
        .expect("the reflection-quiescent serial boundary should collect");
    let RuntimeReadiness::Ready(after_quiescent_collection) = runtime.readiness() else {
        panic!("collection should not create runtime work")
    };
    assert_eq!(
        after_quiescent_collection.stamp(),
        before_quiescent_collection.stamp(),
        "collection is operational maintenance, not a semantic observation"
    );
    before_quiescent_collection
        .validate_without_settling()
        .expect("collection should not stale a readiness snapshot");
    let retained_reflection = access_path(
        &assembler,
        after_quiescent_collection.reflection().root(),
        "i11b_result",
    )
    .expect("committed reflection data should survive collection");
    assert_eq!(
        assembler
            .evaluator()
            .eval(&retained_reflection)
            .expect("retained reflection data should evaluate")
            .as_bytes()
            .expect("retained result extraction should succeed")
            .as_deref(),
        Some(b"preserved".as_slice())
    );

    let delivered = Arc::new(Mutex::new(Vec::new()));
    let delivered_values = delivered.clone();
    let output = runtime
        .output_endpoint(decode_test_integer(runtime.values()), move |value| {
            delivered_values
                .lock()
                .expect("delivery record should not be poisoned")
                .push(value);
            Ok(())
        })
        .expect("output endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    events
        .write(&output.writer(), runtime.values().integer(42))
        .expect("output should journal");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        StoreCommitResult::Committed
    );
    drop((store, events));

    let diagnostic_event = bus.publish_local(Diagnostic::new(
        &runtime.values(),
        Severity::Info,
        "preserved diagnostic",
    ));
    drop(diagnostic_event);
    runtime
        .collect_managed_for_maintenance()
        .expect("queued event and logger-ingress roots should collect safely");
    assert_eq!(net_topology_revision(&runtime, &net), net_revision);

    assert!(matches!(
        output
            .delivery()
            .deliver_next()
            .expect("delivery should run"),
        Some(RuntimeDeliveryOutcome::Delivered(_))
    ));
    assert_eq!(
        *delivered
            .lock()
            .expect("delivery record should not be poisoned"),
        [42]
    );
    let (_, store, snapshot) = runtime.transaction_snapshot();
    let mut diagnostic_events = RuntimeEventJournal::new(snapshot);
    let transported = diagnostic_events
        .read(&diagnostic_reader)
        .expect("logger-facing ingress should remain readable")
        .expect("published diagnostic should remain queued");
    assert_eq!(
        runtime.try_commit_transaction(&StoreJournal::new(store), &diagnostic_events),
        StoreCommitResult::Committed
    );
    drop(diagnostic_events);
    let transported = Diagnostic::from_transport_value(&logger.values(), &transported)
        .expect("logger service should decode the retained diagnostic");
    assert_eq!(transported.message(), "preserved diagnostic");

    runtime.pump_until_stable();
    let RuntimeReadiness::Ready(settlement) = runtime.readiness() else {
        panic!("delivered output should leave the runtime ready")
    };
    runtime
        .collect_managed_for_maintenance()
        .expect("the pre-settlement serial boundary should collect");
    let report = settlement
        .settle()
        .expect("collection should preserve settlement validation");
    assert_eq!(report.stamp(), settlement.stamp());
    assert_eq!(report.reflection().root().runtime_id(), runtime.id());
    assert_eq!(net_topology_revision(&runtime, &net), net_revision);

    runtime
        .collect_managed_for_maintenance()
        .expect("retained settlement report should survive later collection");
    let reported_result = access_path(&assembler, report.reflection().root(), "i11b_result")
        .expect("settled reflection data should remain retained");
    assert_eq!(
        assembler
            .evaluator()
            .eval(&reported_result)
            .expect("settlement report should retain an evaluable root")
            .as_bytes()
            .expect("reported result extraction should succeed")
            .as_deref(),
        Some(b"preserved".as_slice())
    );
}
