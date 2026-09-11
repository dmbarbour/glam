use super::super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use crate::core::{Key, OpaqueValue, Value as CoreValue};
use crate::diagnostic::Severity;
use crate::evaluation::{
    EvalContext, EvaluationMachinePoll, EvaluationTaskMachine, EvaluationWaitPoll,
};
use crate::reflection::{StoreCommitResult, StoreJournal};

use super::runtime_tests::{decode_test_integer, input_transaction};
use super::{OpaqueRetentionProbe, access_path, public_value};

fn net_topology_revision(runtime: &EvaluationRuntime, value: &Value) -> u64 {
    let CoreValue::Net(net) = value.clone_core_for_test() else {
        panic!("production net fixture must remain a net value")
    };
    net.runtime()
        .test_with_revisions(runtime.values().core(), |_| ())
        .1
        .topology_revision()
}

fn assert_production_cycle_reclaimed(
    label: &str,
    live_managed_slots: usize,
    construct: impl FnOnce(&Assembler) -> Value,
) {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should build");
    let baseline = runtime
        .collect_managed_for_maintenance()
        .unwrap_or_else(|failure| panic!("{label}: baseline collection failed: {failure}"));

    let retained = construct(&assembler);
    let live = runtime
        .collect_managed_for_maintenance()
        .unwrap_or_else(|failure| panic!("{label}: rooted collection failed: {failure}"));
    assert_eq!(
        live.root_entries(),
        baseline.root_entries() + 1,
        "{label}: the fixture should retain exactly one final public root"
    );
    assert_eq!(
        live.marked_slots(),
        baseline.marked_slots() + live_managed_slots,
        "{label}: every member of the closed graph should be traced"
    );

    drop(retained);
    let reclaimed = runtime
        .collect_managed_for_maintenance()
        .unwrap_or_else(|failure| panic!("{label}: reclamation collection failed: {failure}"));
    assert_eq!(reclaimed.root_entries(), baseline.root_entries(), "{label}");
    assert_eq!(reclaimed.marked_slots(), baseline.marked_slots(), "{label}");
    assert_eq!(
        reclaimed.finalized_slots(),
        live_managed_slots,
        "{label}: the exact unrooted closed graph should be reclaimed"
    );
}

struct PausedManagedWorker {
    context: EvalContext,
    entered: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
}

struct PausedHostWorker {
    entered: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
}

impl EvaluationTaskMachine for PausedHostWorker {
    fn poll(
        &mut self,
        poll_context: &crate::evaluation::EvaluationPollContext,
        _step_budget: usize,
    ) -> EvaluationMachinePoll {
        self.entered
            .take()
            .expect("host worker fixture should run once")
            .send(())
            .expect("host-worker observer should remain live");
        self.release
            .recv()
            .expect("host-worker release should remain live");
        EvaluationMachinePoll::Complete(poll_context.root_value(crate::core::keys::unit_value()))
    }
}

impl EvaluationTaskMachine for PausedManagedWorker {
    fn poll(
        &mut self,
        poll_context: &crate::evaluation::EvaluationPollContext,
        _step_budget: usize,
    ) -> EvaluationMachinePoll {
        poll_context.with_value_access(&self.context, |_| {
            self.entered
                .take()
                .expect("worker fixture should enter managed access once")
                .send(())
                .expect("worker-entry observer should remain live");
            self.release
                .recv()
                .expect("worker release should remain live");
        });
        EvaluationMachinePoll::Complete(poll_context.root_value(crate::core::keys::unit_value()))
    }
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

#[test]
fn production_runtime_reclaims_each_recursive_identity_family() {
    assert_production_cycle_reclaimed("promise self-cycle", 2, |assembler| {
        let (promise, resolver) = assembler.promise("I11B promise self-cycle");
        resolver
            .resolve(promise.clone())
            .expect("promise should accept its own semantic value");
        promise
    });

    assert_production_cycle_reclaimed("lazy/promise cycle", 3, |assembler| {
        let values = assembler.values();
        let (promise, resolver) = assembler.promise("I11B lazy/promise cycle");
        let lazy = values
            .access(&promise, values.atom_from_text("member"))
            .expect("ordinary access should construct a managed lazy");
        resolver
            .resolve(lazy.clone())
            .expect("promise should close the lazy cycle");
        drop(promise);
        lazy
    });

    assert_production_cycle_reclaimed("core-net/promise cycle", 3, |assembler| {
        let (promise, resolver) = assembler.promise("I11B net/promise cycle");
        let net = assembler
            .net(|builder| builder.data(promise.clone()))
            .expect("production net should contain the promise");
        resolver
            .resolve(net.clone())
            .expect("promise should close the net cycle");
        drop(promise);
        net
    });
}

#[test]
fn production_runtime_reclaims_compatibility_aggregate_cycles() {
    assert_production_cycle_reclaimed("list compatibility cycle", 2, |assembler| {
        let values = assembler.values();
        let (promise, resolver) = assembler.promise("I11B list cycle");
        let list = values
            .list([promise.clone()])
            .expect("list should contain the promise");
        resolver
            .resolve(list.clone())
            .expect("promise should close the list cycle");
        drop(promise);
        list
    });

    assert_production_cycle_reclaimed("dictionary compatibility cycle", 2, |assembler| {
        let values = assembler.values();
        let (promise, resolver) = assembler.promise("I11B dictionary cycle");
        let dictionary = values
            .record([("self", promise.clone())])
            .expect("dictionary should contain the promise");
        resolver
            .resolve(dictionary.clone())
            .expect("promise should close the dictionary cycle");
        drop(promise);
        dictionary
    });

    assert_production_cycle_reclaimed("application compatibility cycle", 3, |assembler| {
        let values = assembler.values();
        let (promise, resolver) = assembler.promise("I11B application cycle");
        let application = values
            .apply(&promise, [values.integer(1)])
            .expect("application should construct a managed lazy");
        resolver
            .resolve(application.clone())
            .expect("promise should close the application cycle");
        drop(promise);
        application
    });
}

#[test]
fn production_runtime_preserves_external_owners_until_explicit_retirement() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should build");
    let values = assembler.values();
    let core_values = assembler.core_values();
    let domain = EffectTokenDomain::new(&values);
    let drops = Arc::new(AtomicUsize::new(0));
    let payload = public_value(
        &core_values,
        CoreValue::Opaque(OpaqueValue::new(
            &core_values,
            Arc::new(OpaqueRetentionProbe(Arc::clone(&drops))),
        )),
    );
    let token = domain.issue(payload.clone());
    drop(payload);

    runtime
        .collect_managed_for_maintenance()
        .expect("a live external token owner should be safe to collect around");
    assert_eq!(
        drops.load(Ordering::Relaxed),
        0,
        "collection must not reinterpret the token's external root as garbage"
    );

    drop(token);
    runtime
        .collect_managed_for_maintenance()
        .expect("the retired token shell should collect");
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert_eq!(
        core_values.drain_external_owners_for_test(),
        1,
        "explicit retirement should release the token owner's payload root"
    );
    runtime
        .collect_managed_for_maintenance()
        .expect("the payload shell should collect after its external root retires");
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert_eq!(
        core_values.drain_external_owners_for_test(),
        1,
        "the collected opaque shell should retire its external payload owner"
    );
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn collection_interleaves_with_worker_quantum_without_lost_work() {
    let runtime = EvaluationRuntime::new(1).expect("worker runtime should build");
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should build");
    let context = assembler.eval_context();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let task = context
        .schedule_task(move |task_context| {
            Ok(Box::new(PausedManagedWorker {
                context: task_context,
                entered: Some(entered_tx),
                release: release_rx,
            }))
        })
        .expect("worker fixture should schedule");
    entered_rx
        .recv()
        .expect("worker should pause while holding managed access");

    let (collector_started_tx, collector_started_rx) = mpsc::channel();
    let (collection_tx, collection_rx) = mpsc::channel();
    let collector_runtime = runtime.clone();
    let collector = std::thread::spawn(move || {
        collector_started_tx
            .send(())
            .expect("collector-start observer should remain live");
        let report = collector_runtime
            .collect_managed_for_maintenance()
            .expect("collection should resume after worker access ends");
        collection_tx
            .send(report)
            .expect("collection observer should remain live");
    });
    collector_started_rx
        .recv()
        .expect("collector thread should begin");
    assert!(
        matches!(collection_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a collector ordered after active managed access must not complete first"
    );

    release_tx
        .send(())
        .expect("managed worker should be releasable");
    let during = collection_rx
        .recv()
        .expect("collector should complete after worker release");
    collector.join().expect("collector thread should not panic");
    runtime.pump_until_stable();
    assert!(matches!(
        context.poll_reflection_task(&task),
        EvaluationWaitPoll::Complete(_)
    ));

    let after = runtime
        .collect_managed_for_maintenance()
        .expect("post-worker serial collection should complete");
    assert_eq!(
        after.epoch(),
        during.epoch() + 1,
        "worker completion must not lose work or trigger an unrequested collection"
    );
}

#[test]
fn passive_finalization_produces_no_runtime_work() {
    let runtime = EvaluationRuntime::new(1).expect("worker runtime should build");
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
    let core_values = assembler.core_values();

    bus.publish_local(Diagnostic::new(
        &runtime.values(),
        Severity::Info,
        "preserved across passive finalization",
    ));
    runtime
        .collect_managed_for_maintenance()
        .expect("live logger state should survive a baseline collection");

    let drops = Arc::new(AtomicUsize::new(0));
    let passive_shell = public_value(
        &core_values,
        CoreValue::Opaque(OpaqueValue::new(
            &core_values,
            Arc::new(OpaqueRetentionProbe(Arc::clone(&drops))),
        )),
    );
    drop(passive_shell);

    let context = assembler.eval_context();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let task = context
        .schedule_task(|_| {
            Ok(Box::new(PausedHostWorker {
                entered: Some(entered_tx),
                release: release_rx,
            }))
        })
        .expect("host worker fixture should schedule");
    entered_rx
        .recv()
        .expect("worker should pause outside managed access");

    let diagnostic_counts = bus.counts();
    let (observation_epoch, _, _) = runtime.transaction_snapshot();
    let scheduler = runtime.state.work.scheduler_inventory_for_test();
    let managed_before = core_values.managed_statistics();
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Busy));

    let report = runtime
        .collect_managed_for_maintenance()
        .expect("passive shell finalization should complete beside host work");

    assert_eq!(report.finalized_slots(), 1);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert_eq!(bus.counts(), diagnostic_counts);
    assert_eq!(runtime.transaction_snapshot().0, observation_epoch);
    assert_eq!(
        runtime.state.work.scheduler_inventory_for_test(),
        scheduler,
        "passive managed destruction must not publish or retire scheduler work"
    );
    let managed_after = core_values.managed_statistics();
    assert!(managed_after.assigned_runs() <= managed_before.assigned_runs());
    assert_eq!(managed_after.pending_finalizers(), 0);
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Busy));

    assert_eq!(
        core_values.drain_external_owners_for_test(),
        1,
        "active opaque payload retirement remains an explicit external-registry operation"
    );
    assert_eq!(drops.load(Ordering::Relaxed), 1);

    let (_, store, events) = runtime.transaction_snapshot();
    let mut events = RuntimeEventJournal::new(events);
    let transported = events
        .read(&diagnostic_reader)
        .expect("logger-facing ingress should remain readable")
        .expect("the diagnostic should remain queued");
    assert!(
        events
            .read(&diagnostic_reader)
            .expect("the diagnostic FIFO should remain readable")
            .is_none(),
        "collection must not inject another logger event"
    );
    assert_eq!(
        runtime.try_commit_transaction(&StoreJournal::new(store), &events),
        StoreCommitResult::Committed
    );
    let transported = Diagnostic::from_transport_value(&logger.values(), &transported)
        .expect("logger service should decode the retained diagnostic");
    assert_eq!(
        transported.message(),
        "preserved across passive finalization"
    );

    release_tx
        .send(())
        .expect("host worker should be releasable");
    runtime.pump_until_stable();
    assert!(matches!(
        context.poll_reflection_task(&task),
        EvaluationWaitPoll::Complete(_)
    ));
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Ready(_)));
}

#[test]
fn aggressive_debug_collection_runs_before_outer_runtime_entry() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let _assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should build");
    let before = runtime
        .collect_managed_for_maintenance()
        .expect("baseline collection should complete");
    runtime.enable_collection_before_outer_entry_for_test();

    let value = runtime.values().empty_dict();
    assert_eq!(value.runtime_id(), runtime.id());
    let after = runtime
        .collect_managed_for_maintenance()
        .expect("explicit post-entry collection should complete");
    assert_eq!(
        after.epoch(),
        before.epoch() + 2,
        "one outer value entry should force exactly one intervening collection"
    );
    assert_eq!(
        runtime
            .values()
            .core()
            .managed_statistics()
            .collection_policy(),
        glam_gc::CollectionPolicy::NoAuto,
        "aggressive verification must not mutate heap policy"
    );
}
