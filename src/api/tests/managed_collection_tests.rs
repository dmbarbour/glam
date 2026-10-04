use super::super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

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
        _step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        self.entered
            .take()
            .expect("host worker fixture should run once")
            .send(())
            .expect("host-worker observer should remain live");
        self.release
            .recv()
            .expect("host-worker release should remain live");
        EvaluationMachinePoll::Complete(poll_context.root_unit())
    }
}

impl EvaluationTaskMachine for PausedManagedWorker {
    fn poll(
        &mut self,
        poll_context: &crate::evaluation::EvaluationPollContext,
        _step_budget: &mut crate::evaluation::EvaluationStepBudget,
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
        EvaluationMachinePoll::Complete(poll_context.root_unit())
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
        after_quiescent_collection.stamp().work_generation(),
        before_quiescent_collection.stamp().work_generation(),
        "collection is not semantic coordinator work"
    );
    assert_eq!(
        after_quiescent_collection.stamp().observation_epoch(),
        before_quiescent_collection.stamp().observation_epoch(),
        "collection is not a reflection or event observation"
    );
    assert!(
        after_quiescent_collection.stamp().gc_maintenance_revision()
            > before_quiescent_collection
                .stamp()
                .gc_maintenance_revision(),
        "collection advances the GC maintenance revision"
    );
    // The collector is root-preserving, so a collection that ran after this
    // snapshot leaves its settlement-relevant instant unchanged: the snapshot
    // still validates even though the maintenance revision advanced.
    before_quiescent_collection
        .validate_without_settling()
        .expect("a root-preserving collection must not invalidate an earlier settlement snapshot");
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
    // The collection is root-preserving, so the snapshot taken before it still
    // settles the same quiescent instant rather than reporting RuntimeChanged.
    let report = settlement
        .settle()
        .expect("a root-preserving collection must not invalidate an earlier settlement snapshot");
    // The report records the committed instant: the snapshot's work and
    // observations, with the maintenance revision current at commit.
    assert_eq!(
        report.stamp().work_generation(),
        settlement.stamp().work_generation()
    );
    assert_eq!(
        report.stamp().observation_epoch(),
        settlement.stamp().observation_epoch()
    );
    assert!(
        report.stamp().gc_maintenance_revision() > settlement.stamp().gc_maintenance_revision()
    );
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
    let before = runtime
        .collect_managed_for_maintenance()
        .expect("pre-worker serial collection should complete");
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
        .recv_timeout(Duration::from_secs(2))
        .expect("worker should pause while holding managed access");
    let immediately_before = runtime.completed_managed_collection_epoch_for_test();
    assert!(
        immediately_before >= before.epoch(),
        "verification-only aggressive entries may add collections before the worker pause"
    );

    let wait_probe = runtime.install_synchronous_collection_wait_probe_for_test();
    let (collection_tx, collection_rx) = mpsc::channel();
    let collector_runtime = runtime.clone();
    let collector = std::thread::spawn(move || {
        let report = collector_runtime
            .collect_managed_for_maintenance()
            .expect("collection should resume after worker access ends");
        collection_tx
            .send(report)
            .expect("collection observer should remain live");
    });
    assert!(
        wait_probe.wait_until_reached(Duration::from_secs(2)),
        "collector must observe the active worker before worker release"
    );
    assert!(
        matches!(collection_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a collector ordered after active managed access must not complete first"
    );

    release_tx
        .send(())
        .expect("managed worker should be releasable");
    let during = collection_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("collector should complete after worker release");
    assert_eq!(
        during.epoch(),
        immediately_before + 1,
        "exactly one collection should occur while the worker is active"
    );
    collector.join().expect("collector thread should not panic");
    runtime.pump_until_stable();
    assert!(matches!(
        context.poll_reflection_task(&task),
        EvaluationWaitPoll::Complete(_)
    ));

    let immediately_after_worker = runtime.completed_managed_collection_epoch_for_test();
    let after = runtime
        .collect_managed_for_maintenance()
        .expect("post-worker serial collection should complete");
    assert_eq!(
        after.epoch(),
        immediately_after_worker + 1,
        "the post-worker explicit collection must be exactly one new pass"
    );

    let values = runtime.values();
    let domain = Arc::downgrade(values.core().value_domain());
    let escaped = values.empty_dict();
    drop(values);
    drop(context);
    drop(assembler);
    drop(runtime);
    assert!(domain.upgrade().is_none());
    assert!(
        escaped.clone_core_in_own_domain().is_err(),
        "a public value must become inert after its collected worker runtime retires"
    );
}

#[test]
fn runtime_retirement_before_collection_leaves_public_value_inert() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let values = runtime.values();
    let domain = Arc::downgrade(values.core().value_domain());
    let escaped = values.empty_dict();
    let runtime_id = escaped.runtime_id();

    drop(values);
    drop(runtime);

    assert!(domain.upgrade().is_none());
    assert_eq!(escaped.runtime_id(), runtime_id);
    assert!(
        escaped.clone_core_in_own_domain().is_err(),
        "a public value retains provenance but no observation authority"
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
    let allocated_before = core_values.allocated_managed_slots_for_test();
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
    let allocated_after = core_values.allocated_managed_slots_for_test();
    assert_eq!(
        allocated_after
            .checked_add(report.reclaimed_slots())
            .expect("managed allocation count should remain representable"),
        allocated_before,
        "passive finalization must retire the reported slots without allocating replacements"
    );
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
    // `readiness` is an instantaneous observational probe. The live executor
    // may briefly hold mutation admission again while parking after publishing
    // the task result, so an immediate probe may legitimately report `Busy`.
    // The fixed scheduler inventory above and this terminal task observation
    // are the authoritative no-new-work assertions for passive finalization.
}

#[test]
fn external_request_during_finalization_is_coalesced() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let output_values = runtime.values().core().clone();
    let (requested_tx, requested_rx) = mpsc::channel();
    let output = runtime
        .output_endpoint(
            |_| Ok(()),
            move |()| {
                output_values.request_managed_collection_for_test();
                requested_tx
                    .send(())
                    .expect("request observer should remain live");
                Ok(())
            },
        )
        .expect("requesting output endpoint should register");
    runtime
        .collect_managed_for_maintenance()
        .expect("baseline collection should complete");

    let (store, mut events) = input_transaction(&runtime);
    events
        .write(&output.writer(), runtime.values().unit())
        .expect("requesting output should journal");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        StoreCommitResult::Committed
    );
    drop((store, events));
    let dead_shell = runtime.values().empty_dict();
    drop(dead_shell);
    let epoch_before = runtime
        .values()
        .core()
        .completed_collection_epoch_for_test();
    let probe = runtime.install_finalizing_phase_probe_for_test();

    let collector_runtime = runtime.clone();
    let collector = std::thread::spawn(move || {
        collector_runtime
            .collect_managed_for_maintenance()
            .expect("paused collection should complete after release")
    });
    probe.wait_until_reached();
    assert!(
        runtime
            .values()
            .core()
            .managed_statistics()
            .pending_finalizers()
            > 0,
        "the pause must occur after finalizer obligations become durable"
    );

    assert!(matches!(
        output
            .delivery()
            .deliver_next()
            .expect("requesting delivery should run without managed access"),
        Some(RuntimeDeliveryOutcome::Delivered(_))
    ));
    requested_rx
        .recv()
        .expect("the external delivery should issue its request");
    assert!(
        runtime
            .values()
            .core()
            .managed_statistics()
            .collection_requested(),
        "a request issued during Finalizing must remain visible until completion"
    );

    probe.release();
    let completed = collector.join().expect("collector thread should not panic");
    assert_eq!(completed.epoch(), epoch_before + 1);
    assert_eq!(completed.finalized_slots(), 1);
    assert!(
        !runtime
            .values()
            .core()
            .managed_statistics()
            .collection_requested(),
        "successful completion should coalesce a request from its Finalizing window"
    );
    assert_eq!(
        runtime
            .values()
            .core()
            .completed_collection_epoch_for_test(),
        completed.epoch(),
        "the Finalizing request must not recurse before another managed entry"
    );

    let surviving_entry = runtime.values().empty_dict();
    let epoch_before_later = runtime
        .values()
        .core()
        .completed_collection_epoch_for_test();
    let later = runtime
        .collect_managed_for_maintenance()
        .expect("one later explicit collection should complete");
    assert_eq!(
        later.epoch(),
        epoch_before_later + 1,
        "the later explicit request should produce exactly one collection"
    );
    drop(surviving_entry);
}

#[test]
fn runtime_request_during_finalization_coalesces_and_lease_release_wakes_pump() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    runtime
        .service_managed_collection()
        .expect("baseline collection should complete");
    let dead_shell = runtime.values().empty_dict();
    drop(dead_shell);
    let probe = runtime.install_finalizing_phase_probe_for_test();

    let collector_runtime = runtime.clone();
    let collector = std::thread::spawn(move || {
        collector_runtime
            .service_managed_collection()
            .expect("paused collection should complete after release")
    });
    probe.wait_until_reached();
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Busy));

    runtime
        .request_managed_collection()
        .expect("request during finalization should publish without waiting");
    let (pump_tx, pump_rx) = mpsc::channel();
    let pump_runtime = runtime.clone();
    let pump = std::thread::spawn(move || {
        pump_runtime.pump_until_stable();
        pump_tx.send(()).unwrap();
    });
    assert!(
        pump_rx.recv_timeout(Duration::from_millis(25)).is_err(),
        "the pump must wait while the collection lease owns finalization"
    );

    probe.release();
    collector.join().expect("collector thread should not panic");
    pump_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("lease retirement must wake the parked pump");
    pump.join().expect("runtime pump should finish cleanly");
    assert!(
        matches!(runtime.readiness(), RuntimeReadiness::Ready(_)),
        "a request linearized before successful completion should be coalesced"
    );

    runtime
        .request_managed_collection()
        .expect("post-completion request should publish");
    assert!(matches!(
        runtime.readiness(),
        RuntimeReadiness::MaintenanceRequired(_)
    ));
}

#[test]
fn recoverable_trace_panic_becomes_retryable_maintenance_and_durable_failure() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let trace_root = runtime
        .values()
        .core()
        .install_recoverable_trace_panic_for_test();

    let first = runtime
        .service_managed_collection()
        .expect_err("the first trace attempt should inject a recoverable panic");
    assert_eq!(first.kind(), RuntimeMaintenanceErrorKind::CollectorPanic);
    let RuntimeReadiness::MaintenanceRequired(retry) = runtime.readiness() else {
        panic!("a reversible trace panic should require explicit retry")
    };
    assert_eq!(retry.state(), RuntimeMaintenanceState::RetryRequired);

    retry
        .service()
        .expect("the trace fixture should succeed on its second attempt");
    let RuntimeReadiness::Ready(snapshot) = runtime.readiness() else {
        panic!("a successful retry should restore ordinary readiness")
    };
    let mut report = snapshot.settle().expect("retry result should settle");
    assert_eq!(report.maintenance_failures().len(), 1);
    assert_eq!(
        report.maintenance_failures()[0].kind(),
        RuntimeMaintenanceFailureKind::CollectorPanic
    );
    assert_eq!(report.pending_maintenance_failure_reports().len(), 1);
    report.mark_reports_enqueued();
    assert!(report.pending_maintenance_failure_reports().is_empty());
    let RuntimeReadiness::Ready(repeated) = runtime.readiness() else {
        panic!("reported maintenance failure should remain retained state")
    };
    let repeated = repeated
        .settle()
        .expect("retained maintenance failure should settle repeatedly");
    assert_eq!(repeated.maintenance_failures().len(), 1);
    assert!(repeated.pending_maintenance_failure_reports().is_empty());
    drop(trace_root);
}

#[test]
fn recoverable_finalizer_panic_retains_retry_work_and_failure_history() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    runtime
        .values()
        .core()
        .install_recoverable_finalizer_panic_for_test();

    let first = runtime
        .service_managed_collection()
        .expect_err("the first finalizer should inject a recoverable panic");
    assert_eq!(first.kind(), RuntimeMaintenanceErrorKind::FinalizerPanic);
    let RuntimeReadiness::MaintenanceRequired(retry) = runtime.readiness() else {
        panic!("untouched finalizers should remain an actionable retry")
    };
    assert_eq!(retry.state(), RuntimeMaintenanceState::RetryRequired);

    let completed = retry
        .service()
        .expect("the untouched finalizer suffix should retry successfully");
    assert_eq!(completed.pending_finalizers(), 0);
    let RuntimeReadiness::Ready(snapshot) = runtime.readiness() else {
        panic!("successful finalizer retry should restore readiness")
    };
    let report = snapshot.settle().expect("finalizer retry should settle");
    assert_eq!(report.maintenance_failures().len(), 1);
    assert_eq!(
        report.maintenance_failures()[0].kind(),
        RuntimeMaintenanceFailureKind::FinalizerPanic
    );
}

#[test]
fn poisoned_maintenance_disposition_is_terminal_readiness() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let lease = runtime
        .state
        .shared_resources
        .mutation_admission
        .begin_gc_activity();
    lease.finish(crate::runtime::RuntimeGcLeaseOutcome::failure(
        glam_gc::HeapMaintenanceSnapshot::Poisoned,
        crate::runtime::RuntimeGcMaintenanceFailureKind::Poisoned,
        "injected terminal maintenance poison",
    ));

    let RuntimeReadiness::MaintenanceFailed(failed) = runtime.readiness() else {
        panic!("poisoned runtime maintenance must not become ready or anonymous busy")
    };
    assert_eq!(failed.state(), RuntimeMaintenanceState::Poisoned);
    assert_eq!(
        failed.failure().map(RuntimeMaintenanceFailure::message),
        Some("injected terminal maintenance poison")
    );
}

#[test]
fn irreversible_collection_panic_becomes_terminal_runtime_maintenance() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    runtime
        .values()
        .core()
        .inject_irreversible_collection_panic_for_test();

    let failure = runtime
        .service_managed_collection()
        .expect_err("irreversible topology panic must interrupt maintenance");
    assert_eq!(failure.kind(), RuntimeMaintenanceErrorKind::Poisoned);
    let RuntimeReadiness::MaintenanceFailed(snapshot) = runtime.readiness() else {
        panic!("irreversible collector poison must become terminal readiness")
    };
    assert_eq!(snapshot.state(), RuntimeMaintenanceState::Poisoned);
    assert_eq!(
        snapshot.failure().map(RuntimeMaintenanceFailure::kind),
        Some(RuntimeMaintenanceFailureKind::Poisoned)
    );
    assert!(
        runtime.request_managed_collection().is_err(),
        "a poisoned heap must reject later requests without losing terminal readiness"
    );
    assert!(matches!(
        runtime.readiness(),
        RuntimeReadiness::MaintenanceFailed(_)
    ));
}

#[test]
fn runtime_no_auto_pressure_requires_explicit_service() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let before = runtime
        .service_managed_collection()
        .expect("baseline collection should complete");
    let retained = runtime
        .values()
        .core()
        .cross_managed_pressure_threshold_for_test();
    assert!(
        runtime
            .values()
            .core()
            .managed_statistics()
            .collection_requested(),
        "typed-run publication should cross the collector pressure threshold"
    );

    for value in 0..16 {
        drop(runtime.values().integer(value));
    }
    assert_eq!(
        runtime
            .values()
            .core()
            .completed_collection_epoch_for_test(),
        before.epoch(),
        "ordinary NoAuto outer entries must not service advisory pressure"
    );
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Ready(_)));

    let serviced = runtime
        .service_managed_collection()
        .expect("explicit runtime maintenance should service pressure");
    assert_eq!(serviced.epoch(), before.epoch() + 1);
    assert!(
        !runtime
            .values()
            .core()
            .managed_statistics()
            .collection_requested()
    );
    drop(retained);
}

#[test]
fn runtime_manual_maintenance_never_mutates_heap_policy() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    assert_eq!(
        runtime
            .values()
            .core()
            .managed_statistics()
            .collection_policy(),
        glam_gc::CollectionPolicy::NoAuto
    );
    runtime
        .request_managed_collection()
        .expect("manual request should publish");
    runtime
        .service_managed_collection()
        .expect("manual service should complete");
    assert_eq!(
        runtime
            .values()
            .core()
            .managed_statistics()
            .collection_policy(),
        glam_gc::CollectionPolicy::NoAuto
    );
}

// Primarily tests the NoAuto pressure-promotion protocol, which aggressive
// verification replaces; see the 2026-10-03 regression plan, D11.
#[cfg(not(feature = "aggressive-gc-verification"))]
#[test]
fn stable_pump_without_pressure_changes_no_maintenance_state() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let RuntimeReadiness::Ready(before) = runtime.readiness() else {
        panic!("new runtime should be ready")
    };

    runtime.pump_until_stable();

    let RuntimeReadiness::Ready(after) = runtime.readiness() else {
        panic!("a pressure-free pump should leave the runtime ready")
    };
    assert_eq!(
        after.stamp().gc_maintenance_revision(),
        before.stamp().gc_maintenance_revision(),
        "a pressure-free stable pump must not publish maintenance work"
    );
}

// Primarily tests the NoAuto pressure-promotion protocol, which aggressive
// verification replaces; see the 2026-10-03 regression plan, D11.
#[cfg(not(feature = "aggressive-gc-verification"))]
#[test]
fn stable_pump_promotes_pressure_once_without_changing_policy() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    runtime
        .service_managed_collection()
        .expect("baseline collection should complete");
    let RuntimeReadiness::Ready(before_pressure) = runtime.readiness() else {
        panic!("baseline runtime should be ready")
    };
    let retained = runtime
        .values()
        .core()
        .cross_managed_pressure_threshold_for_test();
    let RuntimeReadiness::Ready(advisory_only) = runtime.readiness() else {
        panic!("collector-local pressure should remain advisory before a stable pump")
    };
    assert_eq!(
        advisory_only.stamp().gc_maintenance_revision(),
        before_pressure.stamp().gc_maintenance_revision()
    );

    runtime.pump_until_stable();
    let RuntimeReadiness::MaintenanceRequired(first) = runtime.readiness() else {
        panic!("a stable pump should promote pending pressure")
    };
    assert_eq!(first.state(), RuntimeMaintenanceState::Requested);
    runtime.pump_until_stable();
    let RuntimeReadiness::MaintenanceRequired(repeated) = runtime.readiness() else {
        panic!("repeated pumping should retain the one promoted request")
    };
    assert_eq!(
        repeated.revision(),
        first.revision(),
        "repeated pumps must coalesce one pending pressure request"
    );

    repeated
        .service()
        .expect("promoted pressure should use ordinary snapshot service");
    assert_eq!(
        runtime
            .values()
            .core()
            .managed_statistics()
            .collection_policy(),
        glam_gc::CollectionPolicy::NoAuto
    );
    assert!(
        !runtime
            .values()
            .core()
            .managed_statistics()
            .collection_requested()
    );
    drop(retained);
}

// Primarily tests the NoAuto pressure-promotion protocol, which aggressive
// verification replaces; see the 2026-10-03 regression plan, D11.
#[cfg(not(feature = "aggressive-gc-verification"))]
#[test]
fn pressure_after_one_stable_snapshot_waits_for_the_next_pump() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    runtime.pump_until_stable();
    let RuntimeReadiness::Ready(before_pressure) = runtime.readiness() else {
        panic!("pressure-free runtime should be ready")
    };

    let retained = runtime
        .values()
        .core()
        .cross_managed_pressure_threshold_for_test();
    let RuntimeReadiness::Ready(before_next_pump) = runtime.readiness() else {
        panic!("pressure after the prior pump snapshot must remain advisory")
    };
    assert_eq!(
        before_next_pump.stamp().gc_maintenance_revision(),
        before_pressure.stamp().gc_maintenance_revision()
    );

    runtime.pump_until_stable();
    assert!(matches!(
        runtime.readiness(),
        RuntimeReadiness::MaintenanceRequired(_)
    ));
    drop(retained);
}

// Primarily tests the NoAuto pressure-promotion protocol, which aggressive
// verification replaces; see the 2026-10-03 regression plan, D11.
#[cfg(not(feature = "aggressive-gc-verification"))]
#[test]
fn explicit_request_and_pressure_promotion_coalesce_in_both_orders() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    runtime
        .service_managed_collection()
        .expect("baseline collection should complete");

    let pressure_before_request = runtime
        .values()
        .core()
        .cross_managed_pressure_threshold_for_test();
    runtime
        .request_managed_collection()
        .expect("explicit request should publish");
    let RuntimeReadiness::MaintenanceRequired(explicit_first) = runtime.readiness() else {
        panic!("explicit request should be actionable")
    };
    runtime.pump_until_stable();
    let RuntimeReadiness::MaintenanceRequired(after_first_pump) = runtime.readiness() else {
        panic!("the stable pump should retain the explicit request")
    };
    assert_eq!(after_first_pump.revision(), explicit_first.revision());
    drop(pressure_before_request);
    after_first_pump
        .service()
        .expect("the explicit-first request should collect");

    let pressure_before_pump = runtime
        .values()
        .core()
        .cross_managed_pressure_threshold_for_test();
    runtime.pump_until_stable();
    let RuntimeReadiness::MaintenanceRequired(promoted_first) = runtime.readiness() else {
        panic!("pressure should promote before the later explicit request")
    };
    runtime
        .request_managed_collection()
        .expect("requesting already-promoted maintenance should coalesce");
    let RuntimeReadiness::MaintenanceRequired(after_explicit) = runtime.readiness() else {
        panic!("the promoted request should remain actionable")
    };
    assert_eq!(
        after_explicit.revision(),
        promoted_first.revision(),
        "an explicit request after promotion must not publish a second revision"
    );
    drop(pressure_before_pump);
    after_explicit
        .service()
        .expect("the promoted-first request should collect");
}

#[test]
fn pressure_after_collection_snapshot_survives_older_outcome_publication() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    runtime
        .service_managed_collection()
        .expect("baseline collection should complete");
    runtime
        .request_managed_collection()
        .expect("explicit request should publish");
    let RuntimeReadiness::MaintenanceRequired(maintenance) = runtime.readiness() else {
        panic!("explicit request should be actionable")
    };
    let probe = runtime.install_gc_outcome_publication_probe_for_test();
    let service = std::thread::spawn(move || maintenance.service());

    assert!(
        probe.wait_until_reached(Duration::from_secs(2)),
        "collection should pause after reading its completed heap snapshot"
    );
    let retained = runtime
        .values()
        .core()
        .cross_managed_pressure_threshold_for_test();
    assert!(
        runtime
            .values()
            .core()
            .managed_statistics()
            .collection_requested(),
        "later allocation pressure should be latched before the older outcome publishes"
    );
    probe.release();
    service
        .join()
        .expect("maintenance service should not panic")
        .expect("the older collection should publish successfully");

    assert!(
        runtime
            .values()
            .core()
            .managed_statistics()
            .collection_requested(),
        "publishing an older successful outcome must not clear later collector pressure"
    );
    // Promoting the latch is NoAuto protocol, which aggressive verification
    // replaces; see the 2026-10-03 regression plan, D11.
    #[cfg(not(feature = "aggressive-gc-verification"))]
    {
        runtime.pump_until_stable();
        assert!(matches!(
            runtime.readiness(),
            RuntimeReadiness::MaintenanceRequired(_)
        ));
    }
    drop(retained);
}

#[test]
fn parked_pump_promotes_pressure_after_activity_wake() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let activity = runtime.state.shared_resources.mutation_admission.activity();
    let prior_waits = activity.wait_count();
    let lease = runtime
        .state
        .shared_resources
        .mutation_admission
        .begin_gc_activity();
    let pumping_runtime = runtime.clone();
    let (finished_tx, finished_rx) = mpsc::channel();
    let pump = std::thread::spawn(move || {
        pumping_runtime.pump_until_stable();
        finished_tx
            .send(())
            .expect("pump completion observer should remain live");
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while activity.wait_count() == prior_waits {
        assert!(
            std::time::Instant::now() < deadline,
            "the pump should park behind the active collection lease"
        );
        std::thread::yield_now();
    }
    let retained = runtime
        .values()
        .core()
        .cross_managed_pressure_threshold_for_test();
    assert!(
        finished_rx.recv_timeout(Duration::from_millis(25)).is_err(),
        "collector pressure alone must not bypass active runtime activity"
    );

    lease.finish(crate::runtime::RuntimeGcLeaseOutcome::success(
        runtime.values().core().managed_maintenance_snapshot(),
    ));
    finished_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("lease retirement should wake the pump to promote pressure");
    pump.join().expect("runtime pump should finish cleanly");
    // Under aggressive verification the woken pump also services its own
    // promotion; see the 2026-10-03 regression plan, D7 and D11.
    #[cfg(not(feature = "aggressive-gc-verification"))]
    assert!(matches!(
        runtime.readiness(),
        RuntimeReadiness::MaintenanceRequired(_)
    ));
    #[cfg(feature = "aggressive-gc-verification")]
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Ready(_)));
    drop(retained);
}

// Primarily tests the NoAuto pressure-promotion protocol, which aggressive
// verification replaces; see the 2026-10-03 regression plan, D11.
#[cfg(not(feature = "aggressive-gc-verification"))]
#[test]
fn promoted_pressure_reclaims_dead_allocations_and_preserves_assembly_result() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should build");
    let module = assembler
        .module(["i12b_pressure_promotion"])
        .script("g", "language g0\nresult = \"preserved\"\n")
        .build()
        .expect("module should compile");
    let assembly_result =
        access_path(&assembler, module.value(), "result").expect("module should define result");
    runtime
        .service_managed_collection()
        .expect("baseline collection should complete");

    let pressure = runtime
        .values()
        .core()
        .cross_managed_pressure_threshold_for_test();
    drop(pressure);
    runtime.pump_until_stable();
    let RuntimeReadiness::MaintenanceRequired(maintenance) = runtime.readiness() else {
        panic!("stable pump should expose pressure maintenance")
    };
    let report = maintenance
        .service()
        .expect("pressure-triggered snapshot service should collect");

    assert!(report.reclaimed_slots() >= 128);
    assert!(report.reclaimed_runs() >= 112);
    assert_eq!(
        assembler
            .evaluator()
            .eval(&assembly_result)
            .expect("retained assembly result should survive pressure collection")
            .as_bytes()
            .expect("assembly result should remain a binary")
            .as_deref(),
        Some(b"preserved".as_slice())
    );
    assert_eq!(
        runtime
            .values()
            .core()
            .managed_statistics()
            .collection_policy(),
        glam_gc::CollectionPolicy::NoAuto
    );
}

#[cfg(feature = "aggressive-gc-verification")]
#[test]
fn repository_aggressive_mode_services_new_allocations_at_each_stable_pump() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let epoch = || {
        runtime
            .values()
            .core()
            .completed_collection_epoch_for_test()
    };
    runtime.pump_until_stable();
    let settled = epoch();

    runtime.pump_until_stable();
    assert_eq!(
        epoch(),
        settled,
        "a stable pump without new allocations must not collect again"
    );

    let retained = runtime
        .values()
        .core()
        .cross_managed_pressure_threshold_for_test();
    assert_eq!(
        epoch(),
        settled,
        "aggressive verification must not collect at value-domain entries"
    );
    runtime.pump_until_stable();
    assert_eq!(
        epoch(),
        settled + 1,
        "the next stable pump must service the new allocations exactly once"
    );
    assert!(
        matches!(runtime.readiness(), RuntimeReadiness::Ready(_)),
        "readiness after an aggressive pump should match ordinary mode"
    );
    assert_eq!(
        runtime
            .values()
            .core()
            .managed_statistics()
            .collection_policy(),
        glam_gc::CollectionPolicy::NoAuto,
        "repository verification must leave production collection policy immutable"
    );
    drop(retained);
}
