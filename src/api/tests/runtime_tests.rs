use super::super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::core::{Key, LazyValue, OpaqueValue, Value as CoreValue};
use crate::evaluation::{
    EvalContext, EvaluationMachinePoll, EvaluationSession, EvaluationTaskMachine,
    EvaluationWaitPoll, ReflectionTaskProfile,
};
use crate::number::Number;
use crate::reflection::RuntimeInputSequence;

use super::{FailedReasoningTask, assert_unclaimed_lazy, public_value};

fn same_representation(runtime: &EvaluationRuntime, left: &Value, right: &Value) -> bool {
    runtime
        .values()
        .with_access(|values| values.core_value(left).unwrap() == values.core_value(right).unwrap())
}

fn value_i64(runtime: &EvaluationRuntime, value: &Value) -> Option<i64> {
    let CoreValue::Number(number) = runtime.values().clone_core(value).unwrap() else {
        return None;
    };
    number.to_i64_if_integer()
}

fn value_number_text(runtime: &EvaluationRuntime, value: &Value) -> Option<String> {
    let CoreValue::Number(number) = runtime.values().clone_core(value).unwrap() else {
        return None;
    };
    Some(number.to_string())
}

fn value_bytes(runtime: &EvaluationRuntime, value: &Value) -> Option<bytes::Bytes> {
    let CoreValue::Binary(bytes) = runtime.values().clone_core(value).unwrap() else {
        return None;
    };
    Some(bytes)
}

fn value_is_undefined(runtime: &EvaluationRuntime, value: &Value) -> bool {
    same_representation(runtime, value, &runtime.values().empty_dict())
}

#[test]
fn evaluation_runtime_workers_activate_only_once() {
    let runtime = EvaluationRuntime::new(0).expect("dormant runtime should build");
    assert_eq!(runtime.worker_threads(), 0);
    runtime
        .activate_workers(1)
        .expect("dormant runtime should activate");
    assert_eq!(runtime.worker_threads(), 1);
    assert!(runtime.activate_workers(1).is_err());
}

#[test]
fn evaluation_runtime_ids_are_process_unique() {
    let first = EvaluationRuntime::new(0).expect("first runtime should build");
    let second = EvaluationRuntime::new(0).expect("second runtime should build");

    assert_ne!(first.id(), second.id());
}

pub(super) fn input_transaction(
    runtime: &EvaluationRuntime,
) -> (crate::reflection::StoreJournal, RuntimeEventJournal) {
    let (_, store, events) = runtime.transaction_snapshot();
    (
        crate::reflection::StoreJournal::new(store),
        RuntimeEventJournal::new(events),
    )
}

fn integer_converter(
    runtime: &EvaluationRuntime,
) -> impl Fn(i64) -> Result<Value, Error> + Send + Sync + 'static {
    let values = runtime.values();
    move |value| Ok(values.integer(value))
}

#[test]
fn runtime_input_endpoints_are_local_monotonic_capabilities() {
    let owner = EvaluationRuntime::new(0).expect("owner runtime should build");
    let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
    let first = owner
        .input_endpoint(integer_converter(&owner))
        .expect("first endpoint should register");
    let second = owner
        .input_endpoint(integer_converter(&owner))
        .expect("second endpoint should register");

    assert_eq!(second.reader().id().get(), first.reader().id().get() + 1);
    assert_eq!(first.sender().id(), first.reader().id());

    let (_, _, snapshot) = foreign.transaction_snapshot();
    let mut journal = RuntimeEventJournal::new(snapshot);
    let error = journal
        .read(&first.reader())
        .expect_err("an input capability must reject a foreign runtime");
    assert!(error.to_string().contains("belongs to evaluation runtime"));
}

#[test]
fn runtime_event_snapshots_preserve_persistent_input_roots() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let (_, _, before_registration) = runtime.transaction_snapshot();
    let endpoint = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("endpoint should register");

    assert!(
        !before_registration
            .inputs
            .contains_key(&endpoint.reader().id()),
        "a retained snapshot must not observe later endpoint registration"
    );
    let (_, _, before_admission) = runtime.transaction_snapshot();
    endpoint.sender().admit(11).expect("input should admit");
    let mut stale = RuntimeEventJournal::new(before_admission);
    assert!(
        stale.read(&endpoint.reader()).unwrap().is_none(),
        "a retained snapshot must not observe later input admission"
    );

    let (_, store, admitted) = runtime.transaction_snapshot();
    let retained = admitted.clone();
    let mut consumer = RuntimeEventJournal::new(admitted);
    assert!(consumer.read(&endpoint.reader()).unwrap().is_some());
    let store = crate::reflection::StoreJournal::new(store);
    assert_eq!(
        runtime.try_commit_transaction(&store, &consumer),
        crate::reflection::StoreCommitResult::Committed
    );

    let mut historical = RuntimeEventJournal::new(retained);
    assert_eq!(
        historical
            .read(&endpoint.reader())
            .unwrap()
            .and_then(|value| value_i64(&runtime, &value)),
        Some(11),
        "consumption must not mutate a retained input snapshot"
    );
    let (_, mut current) = input_transaction(&runtime);
    assert!(current.read(&endpoint.reader()).unwrap().is_none());
}

#[test]
fn runtime_input_conversion_precedes_admission_and_stores_only_roots() {
    struct HostPayload(Arc<()>);

    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let converter_runtime = runtime.clone();
    let values = runtime.values();
    let endpoint = runtime
        .input_endpoint(move |payload: HostPayload| {
            assert!(converter_runtime.exclusive_admission_available());
            converter_runtime
                .values()
                .core()
                .collect_managed_for_test()
                .expect("input conversion must not inherit a managed-access region");
            let HostPayload(lease) = payload;
            drop(lease);
            Ok(values.text("rooted"))
        })
        .expect("endpoint should register");
    let host_payload = Arc::new(());
    let retained = Arc::downgrade(&host_payload);

    let sequence = endpoint
        .sender()
        .admit(HostPayload(host_payload))
        .expect("input should be admitted");
    assert_eq!(sequence.get(), 0);
    assert!(retained.upgrade().is_none());

    let state = runtime
        .state
        .shared_resources
        .transactions
        .state
        .lock()
        .expect("runtime transaction mutex should not be poisoned");
    let record = state
        .events
        .inputs
        .get(&endpoint.reader().id())
        .expect("endpoint should remain registered")
        .admitted
        .front()
        .expect("the converted root should be buffered");
    assert_eq!(record.payload.runtime_id(), runtime.id());
    assert_eq!(
        value_bytes(&runtime, &record.payload.value(runtime.id())),
        Some(bytes::Bytes::from_static(b"rooted"))
    );
}

#[test]
fn consumed_runtime_input_retires_its_root_after_commit_and_result_drop() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let domain = EffectTokenDomain::new(&runtime.values());
    let endpoint = runtime
        .input_endpoint::<Value, _>(Ok)
        .expect("value input endpoint should register");
    let payload = Arc::new(());
    let retained = Arc::downgrade(&payload);
    endpoint
        .sender()
        .admit(domain.issue(payload))
        .expect("input should be admitted");
    assert!(retained.upgrade().is_some());

    let (store, mut events) = input_transaction(&runtime);
    let consumed = events
        .read(&endpoint.reader())
        .expect("input read should succeed")
        .expect("admitted input should be present");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    drop(events);
    drop(store);
    assert!(retained.upgrade().is_some());
    drop(consumed);
    assert!(retained.upgrade().is_none());
}

#[test]
fn event_delivery_invokes_callback_without_mutator() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let decode_values = runtime.values().core().clone();
    let adapter_values = decode_values.clone();
    let callback_order = Arc::new(Mutex::new(Vec::new()));
    let decode_order = callback_order.clone();
    let adapter_order = callback_order.clone();
    let endpoint = runtime
        .output_endpoint(
            move |value| {
                decode_values
                    .collect_managed_for_test()
                    .expect("output decode must not inherit a managed-access region");
                decode_order
                    .lock()
                    .expect("callback-order mutex should not be poisoned")
                    .push("decode");
                decode_test_integer(Values::from_core_factory(decode_values.clone()))(value)
            },
            move |value| {
                adapter_values
                    .collect_managed_for_test()
                    .expect("output adapter must not inherit a managed-access region");
                adapter_order
                    .lock()
                    .expect("callback-order mutex should not be poisoned")
                    .push("adapter");
                assert_eq!(value, 42);
                Ok(())
            },
        )
        .expect("output endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    events
        .write(&endpoint.writer(), runtime.values().integer(42))
        .expect("output should journal");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );

    assert!(matches!(
        endpoint.delivery().deliver_next().unwrap(),
        Some(RuntimeDeliveryOutcome::Delivered(_))
    ));
    assert_eq!(
        *callback_order
            .lock()
            .expect("callback-order mutex should not be poisoned"),
        ["decode", "adapter"]
    );
}

#[test]
fn failed_runtime_input_conversion_publishes_nothing() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .input_endpoint(|_: ()| -> Result<Value, Error> { Err(Error::new("rejected")) })
        .expect("endpoint should register");
    let (generation, _, _) = runtime.transaction_snapshot();

    assert!(endpoint.sender().admit(()).is_err());

    let (after_generation, _, after) = runtime.transaction_snapshot();
    assert_eq!(after_generation, generation);
    let mut journal = RuntimeEventJournal::new(after);
    assert!(
        journal
            .read(&endpoint.reader())
            .expect("empty endpoint should be readable")
            .is_none()
    );
}

#[test]
fn runtime_input_identity_exhaustion_changes_no_state() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("endpoint should register");
    {
        let mut state = runtime
            .state
            .shared_resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        let input = Arc::make_mut(
            state
                .events
                .inputs
                .get_mut(&endpoint.reader().id())
                .expect("endpoint should remain registered"),
        );
        input.head_sequence = RuntimeInputSequence::from_u64(u64::MAX);
        input.next_sequence = RuntimeInputSequence::from_u64(u64::MAX);
    }
    let (generation, _, _) = runtime.transaction_snapshot();

    assert!(endpoint.sender().admit(1).is_err());

    let (after_generation, _, after) = runtime.transaction_snapshot();
    assert_eq!(after_generation, generation);
    assert!(
        after
            .inputs
            .get(&endpoint.reader().id())
            .expect("endpoint should remain registered")
            .admitted
            .is_empty()
    );

    let endpoint_count = after.inputs.size();
    runtime.state.shared_resources.ids.exhaust_input_endpoints();
    assert!(runtime.input_endpoint(integer_converter(&runtime)).is_err());
    let (_, _, after_id_failure) = runtime.transaction_snapshot();
    assert_eq!(after_id_failure.inputs.size(), endpoint_count);
}

#[test]
fn runtime_input_reads_and_commits_a_fifo_prefix() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("endpoint should register");
    assert_eq!(endpoint.sender().admit(10).unwrap().get(), 0);
    assert_eq!(endpoint.sender().admit(20).unwrap().get(), 1);
    let (store, mut events) = input_transaction(&runtime);

    assert_eq!(
        events
            .read(&endpoint.reader())
            .unwrap()
            .and_then(|value| value_number_text(&runtime, &value)),
        Some("10".to_owned())
    );
    assert_eq!(
        events
            .read(&endpoint.reader())
            .unwrap()
            .and_then(|value| value_number_text(&runtime, &value)),
        Some("20".to_owned())
    );
    assert!(events.read(&endpoint.reader()).unwrap().is_none());
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );

    let (_, mut empty) = input_transaction(&runtime);
    assert!(empty.read(&endpoint.reader()).unwrap().is_none());
}

#[test]
fn empty_runtime_input_observation_is_stable_and_precise() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let left = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("left endpoint should register");
    let right = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("right endpoint should register");
    let (unrelated_store, mut unrelated_events) = input_transaction(&runtime);
    assert!(unrelated_events.read(&left.reader()).unwrap().is_none());
    assert!(unrelated_events.read(&left.reader()).unwrap().is_none());

    right.sender().admit(1).expect("right input should admit");
    assert_eq!(
        runtime.try_commit_transaction(&unrelated_store, &unrelated_events),
        crate::reflection::StoreCommitResult::Committed
    );

    let (stale_store, mut stale_events) = input_transaction(&runtime);
    assert!(stale_events.read(&left.reader()).unwrap().is_none());
    left.sender().admit(2).expect("left input should admit");
    assert_eq!(
        runtime.try_commit_transaction(&stale_store, &stale_events),
        crate::reflection::StoreCommitResult::Conflict
    );
}

#[test]
fn append_then_consume_does_not_restore_a_stale_empty_observation() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("endpoint should register");
    let (stale_store, mut stale_events) = input_transaction(&runtime);
    assert!(stale_events.read(&endpoint.reader()).unwrap().is_none());

    endpoint.sender().admit(1).expect("input should admit");
    let (consumer_store, mut consumer_events) = input_transaction(&runtime);
    assert!(consumer_events.read(&endpoint.reader()).unwrap().is_some());
    assert_eq!(
        runtime.try_commit_transaction(&consumer_store, &consumer_events),
        crate::reflection::StoreCommitResult::Committed
    );

    assert_eq!(
        runtime.try_commit_transaction(&stale_store, &stale_events),
        crate::reflection::StoreCommitResult::Conflict,
        "monotonic FIFO boundaries must prevent empty-state ABA"
    );
}

#[test]
fn nonempty_fifo_read_survives_a_concurrent_append_under_coarse_heap_analysis() {
    let runtime = EvaluationRuntime::with_conflict_analysis(
        0,
        Arc::new(crate::reflection::CoarseConflictAnalysis),
    )
    .expect("runtime should build");
    let endpoint = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("endpoint should register");
    endpoint
        .sender()
        .admit(1)
        .expect("first input should admit");
    let (store, mut events) = input_transaction(&runtime);
    assert!(events.read(&endpoint.reader()).unwrap().is_some());

    endpoint
        .sender()
        .admit(2)
        .expect("concurrent append should admit");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed,
        "FIFO conflicts must not inherit the reflection heap's coarse analysis"
    );

    let (store, mut events) = input_transaction(&runtime);
    assert_eq!(
        events
            .read(&endpoint.reader())
            .unwrap()
            .and_then(|value| value_i64(&runtime, &value)),
        Some(2)
    );
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
}

#[test]
fn append_conflicts_after_a_fifo_reader_observes_the_tail_empty() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("endpoint should register");
    endpoint
        .sender()
        .admit(1)
        .expect("first input should admit");
    let (store, mut events) = input_transaction(&runtime);
    assert!(events.read(&endpoint.reader()).unwrap().is_some());
    assert!(events.read(&endpoint.reader()).unwrap().is_none());

    endpoint
        .sender()
        .admit(2)
        .expect("concurrent append should admit");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Conflict
    );
}

#[test]
fn competing_runtime_input_consumers_conflict_but_independent_ones_commit() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let left = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("left endpoint should register");
    let right = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("right endpoint should register");
    left.sender().admit(1).unwrap();
    right.sender().admit(2).unwrap();
    let (_, store, events) = runtime.transaction_snapshot();
    let left_store = crate::reflection::StoreJournal::new(store.clone());
    let right_store = crate::reflection::StoreJournal::new(store.clone());
    let competing_store = crate::reflection::StoreJournal::new(store);
    let mut left_events = RuntimeEventJournal::new(events.clone());
    let mut right_events = RuntimeEventJournal::new(events.clone());
    let mut competing_events = RuntimeEventJournal::new(events);
    assert!(left_events.read(&left.reader()).unwrap().is_some());
    assert!(right_events.read(&right.reader()).unwrap().is_some());
    assert!(competing_events.read(&left.reader()).unwrap().is_some());

    assert_eq!(
        runtime.try_commit_transaction(&left_store, &left_events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert_eq!(
        runtime.try_commit_transaction(&right_store, &right_events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert_eq!(
        runtime.try_commit_transaction(&competing_store, &competing_events),
        crate::reflection::StoreCommitResult::Conflict
    );
}

#[test]
fn cloned_fifo_claim_cannot_reconsume_a_committed_prefix() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("endpoint should register");
    endpoint.sender().admit(1).expect("input should admit");
    let (_, store, snapshot) = runtime.transaction_snapshot();
    let committed_store = crate::reflection::StoreJournal::new(store.clone());
    let replay_store = crate::reflection::StoreJournal::new(store);
    let mut committed_events = RuntimeEventJournal::new(snapshot);
    assert!(committed_events.read(&endpoint.reader()).unwrap().is_some());
    let replay_events = committed_events.clone();

    assert_eq!(
        runtime.try_commit_transaction(&committed_store, &committed_events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert_eq!(
        runtime.try_commit_transaction(&replay_store, &replay_events),
        crate::reflection::StoreCommitResult::Conflict
    );
}

#[test]
fn abandoned_runtime_input_claim_does_not_consume() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("endpoint should register");
    endpoint.sender().admit(7).unwrap();
    let retained = {
        let (_, mut abandoned) = input_transaction(&runtime);
        abandoned
            .read(&endpoint.reader())
            .unwrap()
            .expect("the admitted value should be readable")
    };
    assert_eq!(
        value_number_text(&runtime, &retained),
        Some("7".to_owned()),
        "the returned value must retain its own runtime root after the journal is dropped"
    );
    let (store, mut events) = input_transaction(&runtime);
    assert_eq!(
        events
            .read(&endpoint.reader())
            .unwrap()
            .and_then(|value| value_number_text(&runtime, &value)),
        Some("7".to_owned())
    );
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
}

#[test]
fn combined_heap_conflict_rolls_back_runtime_input_consumption() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("endpoint should register");
    endpoint.sender().admit(9).unwrap();
    let (_, store_snapshot, event_snapshot) = runtime.transaction_snapshot();
    let mut combined_store = crate::reflection::StoreJournal::new(store_snapshot.clone());
    combined_store.observe_read(&[Key::atom_from_text("combined")]);
    combined_store.write(
        vec![Key::atom_from_text("combined")],
        runtime.values().text("stale"),
    );
    let mut combined_events = RuntimeEventJournal::new(event_snapshot);
    assert!(combined_events.read(&endpoint.reader()).unwrap().is_some());

    let mut winner = crate::reflection::StoreJournal::new(store_snapshot);
    winner.write(
        vec![Key::atom_from_text("combined")],
        runtime.values().text("winner"),
    );
    assert_eq!(
        runtime.commit_reflection(&winner),
        crate::reflection::StoreCommitResult::Committed
    );
    assert_eq!(
        runtime.try_commit_transaction(&combined_store, &combined_events),
        crate::reflection::StoreCommitResult::Conflict
    );

    let (_, mut retry) = input_transaction(&runtime);
    assert_eq!(
        retry
            .read(&endpoint.reader())
            .unwrap()
            .and_then(|value| value_number_text(&runtime, &value)),
        Some("9".to_owned())
    );
}

#[test]
fn runtime_input_admission_wakes_after_releasing_mutation_admission() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("endpoint should register");
    let (generation, _, _) = runtime.transaction_snapshot();
    let prior_waits = runtime.observation_wait_count();
    let waiting_runtime = runtime.clone();
    let waiter = std::thread::spawn(move || {
        assert!(waiting_runtime.wait_for_change(generation));
        assert!(waiting_runtime.exclusive_admission_available());
    });

    while runtime.observation_wait_count() == prior_waits {
        std::thread::yield_now();
    }

    endpoint.sender().admit(1).expect("input should admit");
    waiter.join().expect("broad observer should wake cleanly");
}

#[test]
fn runtime_pump_waits_for_in_flight_mutation_admission() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let activity = runtime.state.shared_resources.mutation_admission.activity();
    let mutation = runtime.mutation_guard();
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Busy));
    let pumping_runtime = runtime.clone();
    let (finished, observed) = std::sync::mpsc::channel();
    let pump = std::thread::spawn(move || {
        pumping_runtime.pump_until_stable();
        finished.send(()).expect("pump receiver should remain live");
    });

    assert!(
        observed
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err(),
        "an in-flight guarded publication must prevent a stable pump result"
    );
    drop(mutation);
    observed
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("releasing mutation admission should wake the runtime pump");
    pump.join().expect("runtime pump should finish cleanly");
    assert!(
        activity.wait_count() > 0,
        "in-flight admission should park the pump on runtime activity"
    );
}

#[test]
fn runtime_activity_cannot_lose_a_wake_before_parking() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let activity = runtime.state.shared_resources.mutation_admission.activity();
    let observed = activity.current();

    // Publish after the pump-like snapshot but before its wait call.
    drop(runtime.mutation_guard());
    let (finished, wake_observed) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        activity.wait_for_change(observed);
        finished.send(()).expect("wake receiver should remain live");
    });
    wake_observed
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("a publication between classification and parking must not be lost");
    waiter
        .join()
        .expect("activity waiter should finish cleanly");
}

#[test]
fn readiness_stamp_tracks_heap_query_and_event_observations() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let RuntimeReadiness::Ready(initial) = runtime.readiness() else {
        panic!("new runtime should be ready")
    };

    let (_, heap_snapshot) = runtime.reflection_snapshot();
    let mut heap = crate::reflection::StoreJournal::new(heap_snapshot);
    heap.write(
        vec![Key::atom_from_text("readiness_root")],
        runtime.values().text("installed"),
    );
    assert_eq!(
        runtime.commit_reflection(&heap),
        crate::reflection::StoreCommitResult::Committed
    );
    let RuntimeReadiness::Ready(after_heap) = runtime.readiness() else {
        panic!("heap state without work should remain ready")
    };
    assert!(after_heap.stamp().observation_epoch() > initial.stamp().observation_epoch());
    assert!(!same_representation(
        &runtime,
        after_heap.reflection().root(),
        initial.reflection().root()
    ));

    let input = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("input endpoint should register");
    input.sender().admit(1).expect("input should admit");
    let RuntimeReadiness::Ready(after_input) = runtime.readiness() else {
        panic!("unused buffered input is state rather than activity")
    };
    assert!(after_input.stamp().observation_epoch() > after_heap.stamp().observation_epoch());
    assert!(same_representation(
        &runtime,
        after_input.reflection().root(),
        after_heap.reflection().root()
    ));

    let (_, query_snapshot) = runtime.reflection_snapshot();
    let mut query_reservation = crate::reflection::StoreJournal::new(query_snapshot);
    let query = query_reservation
        .reserve_query()
        .expect("query should reserve");
    assert_eq!(
        runtime.commit_reflection(&query_reservation),
        crate::reflection::StoreCommitResult::Committed
    );
    let RuntimeReadiness::Ready(before_query_update) = runtime.readiness() else {
        panic!("pending protected query is state rather than scheduler work")
    };
    runtime
        .update_query(&query, runtime.values().integer(0))
        .expect("protected query should update");
    let RuntimeReadiness::Ready(after_query_update) = runtime.readiness() else {
        panic!("completed protected query without work should remain ready")
    };
    assert!(
        after_query_update.stamp().observation_epoch()
            > before_query_update.stamp().observation_epoch()
    );

    assert_eq!(
        initial.stamp().work_generation(),
        after_heap.stamp().work_generation()
    );
    assert_eq!(
        after_heap.stamp().work_generation(),
        after_input.stamp().work_generation()
    );
    assert_eq!(
        after_input.stamp().work_generation(),
        after_query_update.stamp().work_generation()
    );
}

#[test]
fn quiescence_validation_rejects_observation_and_delivery_changes() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let RuntimeReadiness::Ready(no_op_snapshot) = runtime.readiness() else {
        panic!("new runtime should be ready")
    };
    let (_, store) = runtime.reflection_snapshot();
    assert_eq!(
        runtime.commit_reflection(&crate::reflection::StoreJournal::new(store)),
        crate::reflection::StoreCommitResult::Committed
    );
    no_op_snapshot
        .validate_without_settling()
        .expect("a semantic no-op must preserve readiness");

    let RuntimeReadiness::Ready(observation_snapshot) = runtime.readiness() else {
        panic!("new runtime should be ready")
    };
    let admission = runtime.mutation_guard();
    let blocked_snapshot = observation_snapshot.clone();
    let (started, validation_started) = std::sync::mpsc::channel();
    let (finished, validation_finished) = std::sync::mpsc::channel();
    let validator = std::thread::spawn(move || {
        started
            .send(())
            .expect("validation observer should remain live");
        let result = blocked_snapshot.validate_without_settling();
        finished
            .send(result)
            .expect("validation receiver should remain live");
    });
    validation_started
        .recv()
        .expect("validator should begin before admission is released");
    assert!(
        validation_finished
            .recv_timeout(std::time::Duration::from_millis(25))
            .is_err(),
        "settlement validation should wait for in-flight mutation admission"
    );
    drop(admission);
    validation_finished
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("released admission should unblock validation")
        .expect("unchanged validation should pass after admission");
    validator.join().expect("validator should finish cleanly");

    let (_, store_snapshot) = runtime.reflection_snapshot();
    let mut journal = crate::reflection::StoreJournal::new(store_snapshot);
    journal.write(
        vec![Key::atom_from_text("settlement_stale")],
        runtime.values().integer(1),
    );
    assert_eq!(
        runtime.commit_reflection(&journal),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(matches!(
        observation_snapshot.validate_without_settling(),
        Err(RuntimeSettlementError::RuntimeChanged)
    ));

    let RuntimeReadiness::Ready(delivery_snapshot) = runtime.readiness() else {
        panic!("heap state without work should remain ready")
    };
    let endpoint = runtime
        .output_endpoint(decode_test_integer(runtime.values()), |_: i64| Ok(()))
        .expect("output endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    events
        .write(&endpoint.writer(), runtime.values().integer(2))
        .expect("delivery should reserve");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(matches!(
        delivery_snapshot.validate_without_settling(),
        Err(RuntimeSettlementError::RuntimeChanged)
    ));
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Busy));
    assert!(endpoint.delivery().deliver_next().unwrap().is_some());
}

#[test]
fn quiescence_report_snapshots_failures_independently_of_acknowledgement() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should seal the runtime profile");
    let context = assembler.eval_context();
    let acknowledged_task = context
        .schedule_task(|_| Ok(Box::new(FailedReasoningTask)))
        .expect("acknowledged failure task should schedule");
    let retained_task = context
        .schedule_task(|_| Ok(Box::new(FailedReasoningTask)))
        .expect("retained failure task should schedule");
    runtime.pump_until_stable();
    acknowledged_task.acknowledge_failure();

    let acknowledged_delivery = runtime
        .output_endpoint(decode_test_integer(runtime.values()), |_: i64| {
            Err(Error::new("acknowledged delivery failure"))
        })
        .expect("acknowledged delivery endpoint should register");
    let retained_delivery = runtime
        .output_endpoint(decode_test_integer(runtime.values()), |_: i64| {
            Err(Error::new("retained delivery failure"))
        })
        .expect("retained delivery endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    let acknowledged_delivery_id = events
        .write(&acknowledged_delivery.writer(), runtime.values().integer(1))
        .expect("acknowledged delivery should reserve");
    let retained_delivery_id = events
        .write(&retained_delivery.writer(), runtime.values().integer(2))
        .expect("retained delivery should reserve");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(matches!(
        acknowledged_delivery.delivery().deliver_next().unwrap(),
        Some(RuntimeDeliveryOutcome::Failed(_))
    ));
    assert!(matches!(
        retained_delivery.delivery().deliver_next().unwrap(),
        Some(RuntimeDeliveryOutcome::Failed(_))
    ));
    assert!(runtime.acknowledge_delivery_failure(acknowledged_delivery_id));

    let RuntimeReadiness::Ready(snapshot) = runtime.readiness() else {
        panic!("retained failures are state rather than activity")
    };
    let report = snapshot.settle().expect("stable runtime should settle");
    assert_eq!(report.task_failures().len(), 1);
    assert_eq!(report.pending_task_failure_reports().len(), 1);
    assert_eq!(
        report.task_failures()[0].task_id(),
        retained_task.id().get()
    );
    assert_eq!(
        report.task_failures()[0].session_id(),
        context.session_id().get()
    );
    assert_eq!(
        report.task_failures()[0].message(),
        "public reasoning failure"
    );
    assert!(
        report
            .delivery_failures()
            .get(acknowledged_delivery_id)
            .is_none()
    );
    assert!(
        report
            .delivery_failures()
            .get(retained_delivery_id)
            .is_some()
    );
    assert!(
        report
            .pending_delivery_failure_reports()
            .get(acknowledged_delivery_id)
            .is_none()
    );
    assert!(
        report
            .pending_delivery_failure_reports()
            .get(retained_delivery_id)
            .is_some()
    );

    let RuntimeReadiness::Ready(snapshot) = runtime.readiness() else {
        panic!("retained failures should remain stable reporting state")
    };
    let repeated = snapshot
        .settle()
        .expect("repeated stable runtime should settle");
    assert_eq!(repeated.task_failures().len(), 1);
    assert!(repeated.pending_task_failure_reports().is_empty());
    assert!(
        repeated
            .delivery_failures()
            .get(retained_delivery_id)
            .is_some(),
        "settlement must not acknowledge the persistent failure"
    );
    assert!(repeated.pending_delivery_failure_reports().is_empty());

    retained_task.acknowledge_failure();
    assert!(runtime.acknowledge_delivery_failure(retained_delivery_id));
    assert_eq!(report.task_failures().len(), 1);
    assert!(
        report
            .delivery_failures()
            .get(retained_delivery_id)
            .is_some(),
        "a retained report must not track later acknowledgements"
    );

    context
        .schedule_task(|_| {
            struct CompleteTask;
            impl EvaluationTaskMachine for CompleteTask {
                fn poll(
                    &mut self,
                    _context: &crate::evaluation::EvaluationPollContext,
                    _step_budget: usize,
                ) -> EvaluationMachinePoll {
                    EvaluationMachinePoll::Complete(
                        _context.root_value(crate::core::keys::unit_value()),
                    )
                }
            }
            Ok(Box::new(CompleteTask))
        })
        .expect("later work should remain admissible");
    runtime.pump_until_stable();
    assert_eq!(report.task_failures().len(), 1);
    assert_eq!(report.runtime_id(), runtime.id());
}

pub(super) fn decode_test_integer(
    values: Values,
) -> impl Fn(Value) -> Result<i64, Error> + Send + Sync + 'static {
    move |value| {
        let value = values.clone_core(&value)?;
        let CoreValue::Number(number) = value else {
            return Err(Error::new("integer output expected"));
        };
        number
            .to_i64_if_integer()
            .ok_or_else(|| Error::new("integer output expected"))
    }
}

#[test]
fn output_journaling_preserves_lazy_payload_until_decoder_demand() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should seal the runtime");
    let evaluations = Arc::new(AtomicUsize::new(0));
    let producer_evaluations = evaluations.clone();
    let lazy = public_value(
        &assembler.core_values(),
        CoreValue::Lazy(LazyValue::semantic_thunk(
            &assembler.core_values(),
            "lazy output payload",
            move |_| {
                producer_evaluations.fetch_add(1, Ordering::SeqCst);
                Ok(CoreValue::Number(Number::integer(42)))
            },
        )),
    );
    let decoder = assembler.clone();
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let delivered_values = delivered.clone();
    let endpoint = runtime
        .output_endpoint(
            move |value| {
                decoder
                    .evaluator()
                    .eval(&value)?
                    .as_i64()?
                    .ok_or_else(|| Error::new("integer output expected"))
            },
            move |value| {
                delivered_values
                    .lock()
                    .expect("delivered output mutex should not be poisoned")
                    .push(value);
                Ok(())
            },
        )
        .expect("output endpoint should register");

    let (store, mut events) = input_transaction(&runtime);
    events
        .write(&endpoint.writer(), lazy.clone())
        .expect("an unrestricted lazy output should journal");
    assert_unclaimed_lazy(&lazy);
    assert_eq!(evaluations.load(Ordering::SeqCst), 0);
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert_unclaimed_lazy(&lazy);
    assert_eq!(evaluations.load(Ordering::SeqCst), 0);

    assert!(matches!(
        endpoint.delivery().deliver_next().unwrap(),
        Some(RuntimeDeliveryOutcome::Delivered(_))
    ));
    assert_eq!(evaluations.load(Ordering::SeqCst), 1);
    assert_eq!(
        *delivered
            .lock()
            .expect("delivered output mutex should not be poisoned"),
        [42]
    );
}

#[test]
fn abandoned_output_intents_burn_ids_without_publishing_work() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
    let endpoint = runtime
        .output_endpoint(decode_test_integer(runtime.values()), |_: i64| Ok(()))
        .expect("output endpoint should register");
    let next = runtime
        .output_endpoint(decode_test_integer(runtime.values()), |_: i64| Ok(()))
        .expect("second output endpoint should register");
    assert_eq!(next.writer().id().get(), endpoint.writer().id().get() + 1);

    let (_, mut abandoned) = input_transaction(&runtime);
    let burned = abandoned
        .write(&endpoint.writer(), runtime.values().integer(1))
        .expect("intent should reserve an ID");
    drop(abandoned);
    assert!(!runtime.has_delivery_activity());
    assert!(endpoint.delivery().deliver_next().unwrap().is_none());

    let (_, _, foreign_snapshot) = foreign.transaction_snapshot();
    let mut foreign_events = RuntimeEventJournal::new(foreign_snapshot);
    assert!(
        foreign_events
            .write(&endpoint.writer(), foreign.values().integer(2))
            .is_err()
    );

    let (store, mut events) = input_transaction(&runtime);
    let committed = events
        .write(&endpoint.writer(), runtime.values().integer(3))
        .expect("second intent should reserve an ID");
    assert!(committed.get() > burned.get());
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(runtime.has_delivery_activity());
}

#[test]
fn runtime_pump_waits_for_running_output_delivery() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let activity = runtime.state.shared_resources.mutation_admission.activity();
    let release = Arc::new(std::sync::Barrier::new(2));
    let callback_release = release.clone();
    let (entered, callback_entered) = std::sync::mpsc::channel();
    let endpoint = runtime
        .output_endpoint(decode_test_integer(runtime.values()), move |_: i64| {
            entered
                .send(())
                .expect("delivery observer should remain live");
            callback_release.wait();
            Ok(())
        })
        .expect("output endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    events
        .write(&endpoint.writer(), runtime.values().integer(1))
        .expect("output intent should reserve a delivery");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Busy));

    let delivery = endpoint.delivery();
    let delivery_thread = std::thread::spawn(move || delivery.deliver_next().unwrap());
    callback_entered
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("delivery callback should begin");
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Busy));

    let pumping_runtime = runtime.clone();
    let (finished, pump_finished) = std::sync::mpsc::channel();
    let pump = std::thread::spawn(move || {
        pumping_runtime.pump_until_stable();
        finished.send(()).expect("pump receiver should remain live");
    });
    assert!(
        pump_finished
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err(),
        "a running delivery must keep the runtime pump parked"
    );

    release.wait();
    assert!(delivery_thread.join().unwrap().is_some());
    pump_finished
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("delivery terminalization should wake the runtime pump");
    pump.join().expect("runtime pump should finish cleanly");
    assert!(!runtime.has_delivery_activity());
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Ready(_)));
    assert!(
        activity.wait_count() > 0,
        "running delivery should park the pump rather than busy-polling"
    );
}

#[test]
fn output_identity_exhaustion_changes_no_runtime_state() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .output_endpoint(decode_test_integer(runtime.values()), |_: i64| Ok(()))
        .expect("output endpoint should register");
    runtime.state.shared_resources.ids.exhaust_deliveries();
    let (_, mut events) = input_transaction(&runtime);
    assert!(
        events
            .write(&endpoint.writer(), runtime.values().integer(1))
            .is_err()
    );
    assert!(!runtime.has_delivery_activity());

    let endpoint_count = runtime
        .state
        .shared_resources
        .transactions
        .state
        .lock()
        .unwrap()
        .events
        .outputs
        .ready_by_endpoint
        .len();
    runtime
        .state
        .shared_resources
        .ids
        .exhaust_output_endpoints();
    assert!(
        runtime
            .output_endpoint(decode_test_integer(runtime.values()), |_: i64| Ok(()))
            .is_err()
    );
    assert_eq!(
        runtime
            .state
            .shared_resources
            .transactions
            .state
            .lock()
            .unwrap()
            .events
            .outputs
            .ready_by_endpoint
            .len(),
        endpoint_count
    );
}

#[test]
fn combined_heap_conflict_rolls_back_output_admission() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .output_endpoint(decode_test_integer(runtime.values()), |_: i64| Ok(()))
        .expect("output endpoint should register");
    let (_, store_snapshot, event_snapshot) = runtime.transaction_snapshot();
    let mut combined_store = crate::reflection::StoreJournal::new(store_snapshot.clone());
    combined_store.observe_read(&[Key::atom_from_text("output_atomic")]);
    combined_store.write(
        vec![Key::atom_from_text("output_atomic")],
        runtime.values().text("stale"),
    );
    let mut combined_events = RuntimeEventJournal::new(event_snapshot);
    combined_events
        .write(&endpoint.writer(), runtime.values().integer(1))
        .unwrap();

    let mut winner = crate::reflection::StoreJournal::new(store_snapshot);
    winner.write(
        vec![Key::atom_from_text("output_atomic")],
        runtime.values().text("winner"),
    );
    assert_eq!(
        runtime.commit_reflection(&winner),
        crate::reflection::StoreCommitResult::Committed
    );
    assert_eq!(
        runtime.try_commit_transaction(&combined_store, &combined_events),
        crate::reflection::StoreCommitResult::Conflict
    );
    assert!(!runtime.has_delivery_activity());
    assert!(endpoint.delivery().deliver_next().unwrap().is_none());
}

#[test]
fn output_claim_is_unique_and_callbacks_run_outside_runtime_guards() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let decode_runtime = runtime.clone();
    let adapter_runtime = runtime.clone();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let callback_barrier = barrier.clone();
    let (entered, waiting) = std::sync::mpsc::channel();
    let endpoint = runtime
        .output_endpoint(
            move |value| {
                assert!(decode_runtime.exclusive_admission_available());
                decode_test_integer(decode_runtime.values())(value)
            },
            move |_: i64| {
                assert!(adapter_runtime.exclusive_admission_available());
                entered.send(()).expect("test receiver should remain live");
                callback_barrier.wait();
                Ok(())
            },
        )
        .expect("output endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    let delivery = events
        .write(&endpoint.writer(), runtime.values().integer(1))
        .unwrap();
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    let worker_delivery = endpoint.delivery();
    let worker = std::thread::spawn(move || worker_delivery.deliver_next().unwrap());
    waiting.recv().expect("callback should begin");

    assert!(runtime.has_delivery_activity());
    assert!(endpoint.delivery().deliver_next().unwrap().is_none());
    barrier.wait();
    assert!(matches!(
        worker.join().expect("delivery thread should finish"),
        Some(RuntimeDeliveryOutcome::Delivered(id)) if id == delivery
    ));
    assert!(!runtime.has_delivery_activity());
}

#[test]
fn output_delivery_preserves_endpoint_order_and_allows_endpoint_concurrency() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let ordered = Arc::new(Mutex::new(Vec::new()));
    let ordered_sink = ordered.clone();
    let sequential = runtime
        .output_endpoint(decode_test_integer(runtime.values()), move |value| {
            ordered_sink.lock().unwrap().push(value);
            Ok(())
        })
        .expect("sequential endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    events
        .write(&sequential.writer(), runtime.values().integer(1))
        .unwrap();
    events
        .write(&sequential.writer(), runtime.values().integer(2))
        .unwrap();
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(sequential.delivery().deliver_next().unwrap().is_some());
    assert!(sequential.delivery().deliver_next().unwrap().is_some());
    assert_eq!(*ordered.lock().unwrap(), [1, 2]);

    let barrier = Arc::new(std::sync::Barrier::new(3));
    let left_barrier = barrier.clone();
    let right_barrier = barrier.clone();
    let left = runtime
        .output_endpoint(decode_test_integer(runtime.values()), move |_: i64| {
            left_barrier.wait();
            Ok(())
        })
        .expect("left endpoint should register");
    let right = runtime
        .output_endpoint(decode_test_integer(runtime.values()), move |_: i64| {
            right_barrier.wait();
            Ok(())
        })
        .expect("right endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    events
        .write(&left.writer(), runtime.values().integer(3))
        .unwrap();
    events
        .write(&right.writer(), runtime.values().integer(4))
        .unwrap();
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    let left_delivery = left.delivery();
    let right_delivery = right.delivery();
    let left_worker = std::thread::spawn(move || left_delivery.deliver_next().unwrap());
    let right_worker = std::thread::spawn(move || right_delivery.deliver_next().unwrap());
    barrier.wait();
    assert!(left_worker.join().unwrap().is_some());
    assert!(right_worker.join().unwrap().is_some());
    assert!(!runtime.has_delivery_activity());
}

#[test]
fn output_delivery_orders_by_commit_not_reservation() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let sink = delivered.clone();
    let endpoint = runtime
        .output_endpoint(decode_test_integer(runtime.values()), move |value| {
            sink.lock().unwrap().push(value);
            Ok(())
        })
        .expect("output endpoint should register");
    let (_, store_snapshot, event_snapshot) = runtime.transaction_snapshot();
    let first_store = crate::reflection::StoreJournal::new(store_snapshot.clone());
    let second_store = crate::reflection::StoreJournal::new(store_snapshot);
    let mut first_events = RuntimeEventJournal::new(event_snapshot.clone());
    let mut second_events = RuntimeEventJournal::new(event_snapshot);
    first_events
        .write(&endpoint.writer(), runtime.values().integer(1))
        .unwrap();
    second_events
        .write(&endpoint.writer(), runtime.values().integer(2))
        .unwrap();

    assert_eq!(
        runtime.try_commit_transaction(&second_store, &second_events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert_eq!(
        runtime.try_commit_transaction(&first_store, &first_events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(endpoint.delivery().deliver_next().unwrap().is_some());
    assert!(endpoint.delivery().deliver_next().unwrap().is_some());
    assert_eq!(*delivered.lock().unwrap(), [2, 1]);
}

#[test]
fn cloned_output_intent_cannot_republish_a_terminal_delivery_id() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .output_endpoint(decode_test_integer(runtime.values()), |_: i64| Ok(()))
        .expect("output endpoint should register");
    let (_, store_snapshot, event_snapshot) = runtime.transaction_snapshot();
    let first_store = crate::reflection::StoreJournal::new(store_snapshot.clone());
    let replay_store = crate::reflection::StoreJournal::new(store_snapshot);
    let mut first_events = RuntimeEventJournal::new(event_snapshot);
    first_events
        .write(&endpoint.writer(), runtime.values().integer(1))
        .unwrap();
    let replay_events = first_events.clone();

    assert_eq!(
        runtime.try_commit_transaction(&first_store, &first_events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(endpoint.delivery().deliver_next().unwrap().is_some());
    assert_eq!(
        runtime.try_commit_transaction(&replay_store, &replay_events),
        crate::reflection::StoreCommitResult::Conflict
    );
    assert!(!runtime.has_delivery_activity());
}

#[test]
fn output_failures_are_terminal_durable_and_acknowledgeable() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let decode = runtime
        .output_endpoint(
            |_: Value| -> Result<(), Error> { Err(Error::new("decode failure")) },
            |()| Ok(()),
        )
        .expect("decode endpoint should register");
    let adapter = runtime
        .output_endpoint(decode_test_integer(runtime.values()), |_: i64| {
            Err(Error::new("adapter failure"))
        })
        .expect("adapter endpoint should register");
    let panic = runtime
        .output_endpoint(
            decode_test_integer(runtime.values()),
            |_: i64| -> Result<(), Error> { panic!("adapter panic") },
        )
        .expect("panic endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    let decode_id = events
        .write(&decode.writer(), runtime.values().integer(1))
        .unwrap();
    let adapter_id = events
        .write(&adapter.writer(), runtime.values().integer(2))
        .unwrap();
    let panic_id = events
        .write(&panic.writer(), runtime.values().integer(3))
        .unwrap();
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );

    let outcomes = [
        decode.delivery().deliver_next().unwrap().unwrap(),
        adapter.delivery().deliver_next().unwrap().unwrap(),
        panic.delivery().deliver_next().unwrap().unwrap(),
    ];
    let kinds = outcomes.map(|outcome| match outcome {
        RuntimeDeliveryOutcome::Failed(failure) => failure.kind(),
        RuntimeDeliveryOutcome::Delivered(_) => panic!("delivery should fail"),
    });
    assert_eq!(
        kinds,
        [
            RuntimeDeliveryFailureKind::Decode,
            RuntimeDeliveryFailureKind::Adapter,
            RuntimeDeliveryFailureKind::Panic,
        ]
    );
    assert!(!runtime.has_delivery_activity());
    let snapshot = runtime.delivery_failure_snapshot();
    assert_eq!(snapshot.failures().len(), 3);
    assert_eq!(
        decode
            .delivery()
            .failure_snapshot()
            .unwrap()
            .failures()
            .len(),
        1
    );
    assert!(snapshot.get(decode_id).is_some());
    assert!(snapshot.get(adapter_id).is_some());
    assert!(snapshot.get(panic_id).is_some());
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Ready(_)));

    let (generation, _, _) = runtime.transaction_snapshot();
    assert!(runtime.acknowledge_delivery_failure(adapter_id));
    assert!(!runtime.acknowledge_delivery_failure(adapter_id));
    let (after_acknowledgement, _, _) = runtime.transaction_snapshot();
    assert_eq!(after_acknowledgement, generation);
    assert!(
        runtime
            .delivery_failure_snapshot()
            .get(adapter_id)
            .is_none()
    );
    assert!(snapshot.get(adapter_id).is_some());
}

#[test]
fn output_callback_response_reenters_as_later_admitted_input() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let input = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("input endpoint should register");
    let response = input.sender();
    let output = runtime
        .output_endpoint(decode_test_integer(runtime.values()), move |value| {
            response.admit(value)?;
            Ok(())
        })
        .expect("output endpoint should register");
    let (producing_store, mut producing_events) = input_transaction(&runtime);
    assert!(producing_events.read(&input.reader()).unwrap().is_none());
    producing_events
        .write(&output.writer(), runtime.values().integer(42))
        .unwrap();
    assert_eq!(
        runtime.try_commit_transaction(&producing_store, &producing_events),
        crate::reflection::StoreCommitResult::Committed
    );
    let (stale_store, mut stale_events) = input_transaction(&runtime);
    assert!(stale_events.read(&input.reader()).unwrap().is_none());

    assert!(output.delivery().deliver_next().unwrap().is_some());
    assert_eq!(
        runtime.try_commit_transaction(&stale_store, &stale_events),
        crate::reflection::StoreCommitResult::Conflict
    );
    let (_, mut fresh_events) = input_transaction(&runtime);
    assert_eq!(
        fresh_events
            .read(&input.reader())
            .unwrap()
            .and_then(|value| value_number_text(&runtime, &value)),
        Some("42".to_owned())
    );
}

#[test]
fn output_payload_is_retained_through_callback_and_dropped_after_locks() {
    struct DeliveryLease {
        resources: Weak<RuntimeSharedResources>,
        dropped: Arc<AtomicBool>,
    }

    // SAFETY: this test-only external lifecycle probe carries weak runtime
    // coordination state but no core value, runtime root, or managed pointer.
    unsafe impl crate::core::OpaquePayloadFamily for DeliveryLease {
        const PAYLOAD_RECORD: crate::core::OpaquePayloadRecord =
            crate::core::OpaquePayloadRecord::external(
                "output delivery lease fixture",
                "src/api/tests/runtime_tests.rs",
            );
    }

    impl Drop for DeliveryLease {
        fn drop(&mut self) {
            if let Some(resources) = self.resources.upgrade() {
                assert!(
                    resources
                        .mutation_admission
                        .try_settlement_guard()
                        .is_some()
                );
            }
            self.dropped.store(true, Ordering::Release);
        }
    }

    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let dropped = Arc::new(AtomicBool::new(false));
    let lease = Arc::new(DeliveryLease {
        resources: Arc::downgrade(&runtime.state.shared_resources),
        dropped: dropped.clone(),
    });
    let retained = Arc::downgrade(&lease);
    let callback_retained = retained.clone();
    let endpoint = runtime
        .output_endpoint(
            |value| {
                assert!(matches!(value.as_core(), CoreValue::Opaque(_)));
                Ok(())
            },
            move |()| {
                assert!(callback_retained.upgrade().is_some());
                Ok(())
            },
        )
        .expect("output endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    events
        .write(
            &endpoint.writer(),
            public_value(
                runtime.values().core(),
                CoreValue::Opaque(OpaqueValue::new(lease)),
            ),
        )
        .unwrap();
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    drop(events);
    drop(store);
    assert!(retained.upgrade().is_some());
    assert!(endpoint.delivery().deliver_next().unwrap().is_some());
    assert!(retained.upgrade().is_none());
    assert!(dropped.load(Ordering::Acquire));
}

#[test]
fn running_delivery_retains_shared_resources_until_terminal_publication() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let runtime_state = Arc::downgrade(&runtime.state);
    let resources = Arc::downgrade(&runtime.state.shared_resources);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let callback_barrier = barrier.clone();
    let (entered, waiting) = std::sync::mpsc::channel();
    let endpoint = runtime
        .output_endpoint(decode_test_integer(runtime.values()), move |_: i64| {
            entered.send(()).unwrap();
            callback_barrier.wait();
            Ok(())
        })
        .expect("output endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    events
        .write(&endpoint.writer(), runtime.values().integer(1))
        .unwrap();
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    drop(events);
    drop(store);
    let delivery = endpoint.delivery();
    let worker = std::thread::spawn(move || delivery.deliver_next().unwrap());
    waiting.recv().expect("callback should begin");

    drop(endpoint);
    drop(runtime);
    assert!(runtime_state.upgrade().is_none());
    assert!(resources.upgrade().is_some());
    barrier.wait();
    assert!(worker.join().unwrap().is_some());
    assert!(resources.upgrade().is_none());
}

#[test]
fn independent_runtimes_have_independent_reflection_heaps() {
    let owner = Assembler::default();
    let foreign = Assembler::default();
    let module = owner
            .module(["runtime_heap_isolation"])
            .script(
                "g",
                "language g0\nimport 'std\nresult = anno refl:(.heap.set '.runtime_only \"yes\") \"done\"\n",
            )
            .build()
            .expect("heap isolation fixture should compile");
    owner
        .evaluate(
            &owner
                .get(module.value(), "result")
                .expect("fixture should define result"),
        )
        .expect("reflection gate should complete");

    assert!(
        owner
            .get(&owner.test_reflection_heap(), "runtime_only")
            .is_ok()
    );
    assert!(
        foreign
            .get(&foreign.test_reflection_heap(), "runtime_only")
            .is_ok_and(|value| value_is_undefined(&foreign.evaluation_runtime(), &value))
    );
}

#[test]
fn runtime_combines_reflection_and_event_commit() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should build");
    let input = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("input endpoint should register");
    let (initial_generation, store, snapshot) = runtime.transaction_snapshot();
    let mut stale = crate::reflection::StoreJournal::new(store);
    stale.write(
        vec![Key::atom_from_text("atomic")],
        runtime.values().text("stale"),
    );
    let mut stale_events = RuntimeEventJournal::new(snapshot);
    assert!(stale_events.read(&input.reader()).unwrap().is_none());
    input.sender().admit(7).expect("input should be admitted");
    let (input_generation, _, _) = runtime.transaction_snapshot();
    assert_ne!(input_generation, initial_generation);

    assert_eq!(
        runtime.try_commit_transaction(&stale, &stale_events),
        crate::reflection::StoreCommitResult::Conflict
    );
    assert!(
        assembler
            .get(&runtime.reflection_root(), "atomic")
            .is_ok_and(|value| value_is_undefined(&runtime, &value))
    );

    let (_, store, snapshot) = runtime.transaction_snapshot();
    let mut committed = crate::reflection::StoreJournal::new(store);
    committed.write(
        vec![Key::atom_from_text("atomic")],
        runtime.values().text("committed"),
    );
    let mut events = RuntimeEventJournal::new(snapshot);
    assert_eq!(
        events
            .read(&input.reader())
            .expect("admitted input should be readable")
            .and_then(|value| value_i64(&runtime, &value)),
        Some(7)
    );
    assert_eq!(
        runtime.try_commit_transaction(&committed, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    let (committed_generation, _, snapshot) = runtime.transaction_snapshot();
    assert_ne!(committed_generation, input_generation);
    let mut empty = RuntimeEventJournal::new(snapshot);
    assert!(empty.read(&input.reader()).unwrap().is_none());
    let committed = assembler
        .get(&runtime.reflection_root(), "atomic")
        .expect("the store edit should commit");
    assert_eq!(
        value_bytes(&runtime, &committed),
        Some(bytes::Bytes::from_static(b"committed"))
    );
}

#[test]
fn exclusive_admission_probe_rejects_an_active_mutation() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let mutation = runtime.mutation_guard();

    assert!(!runtime.exclusive_admission_available());
    drop(mutation);
    assert!(runtime.exclusive_admission_available());
}

#[test]
fn runtime_store_publication_wakes_broad_observers() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let (generation, store) = runtime.reflection_snapshot();
    let waiting_runtime = runtime.clone();
    let (awake, observed) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        awake
            .send(waiting_runtime.wait_for_change(generation))
            .expect("test should still receive the wake result");
    });
    let mut journal = crate::reflection::StoreJournal::new(store);
    journal.write(
        vec![Key::atom_from_text("wake")],
        runtime.values().empty_dict(),
    );

    assert_eq!(
        runtime.commit_reflection(&journal),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(
        observed
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("store publication should wake the observer")
    );
    waiter.join().expect("observer thread should finish");
}

#[test]
fn empty_reflection_commit_preserves_epoch_and_does_not_wake_broad_observers() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let (generation, store) = runtime.reflection_snapshot();
    let waiting_runtime = runtime.clone();
    let (started, waiting) = std::sync::mpsc::channel();
    let (awake, observed) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        started.send(()).expect("waiter should announce startup");
        let changed = waiting_runtime.wait_for_change(generation);
        awake
            .send(changed)
            .expect("test should still receive the wake result");
    });
    waiting.recv().expect("broad waiter should start");

    let empty = crate::reflection::StoreJournal::new(store);
    assert_eq!(
        runtime.commit_reflection(&empty),
        crate::reflection::StoreCommitResult::Committed
    );
    assert_eq!(runtime.reflection_snapshot().0, generation);
    assert!(
        observed
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err(),
        "a successful no-op commit must not wake a broad observer"
    );

    let (_, store) = runtime.reflection_snapshot();
    let mut changed = crate::reflection::StoreJournal::new(store);
    changed.write(
        vec![Key::atom_from_text("real_change")],
        runtime.values().integer(1),
    );
    assert_eq!(
        runtime.commit_reflection(&changed),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(
        observed
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("a real store change should wake the observer")
    );
    waiter.join().expect("observer thread should finish");
}

#[test]
fn empty_reflection_commit_publishes_queued_query_retirement() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let (_, store) = runtime.reflection_snapshot();
    let mut reservation = crate::reflection::StoreJournal::new(store);
    let query = reservation.reserve_query().expect("query should reserve");
    assert_eq!(
        runtime.commit_reflection(&reservation),
        crate::reflection::StoreCommitResult::Committed
    );
    drop(query);

    let (before_retirement, store) = runtime.reflection_snapshot();
    let maintenance = crate::reflection::StoreJournal::new(store);
    assert_eq!(
        runtime.commit_reflection(&maintenance),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(runtime.reflection_snapshot().0 > before_retirement);
}

#[test]
fn coordinator_transitions_do_not_advance_the_semantic_observation_epoch() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let (before, _) = runtime.reflection_snapshot();
    let session = EvaluationSession::shared_with_default_profile(
        &runtime.state.work,
        runtime.state.shared_resources.values.core().clone(),
        Arc::new(ReflectionTaskProfile::unsealed()),
    );
    let context = EvalContext::new(&session);
    let _task = context
        .schedule_task(|_| Ok(Box::new(FailedReasoningTask)))
        .expect("test task should enter the coordinator ready queue");
    let (after_ready, _) = runtime.reflection_snapshot();
    assert_eq!(after_ready, before);

    drop(context);
    drop(session);
    let (after_close, _) = runtime.reflection_snapshot();
    assert_eq!(after_close, before);
}

#[test]
fn diagnostic_callbacks_run_after_runtime_mutation_admission_is_released() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let callback_runtime = runtime.clone();
    let callback_observed = Arc::new(AtomicBool::new(false));
    let callback_result = callback_observed.clone();
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime)
        .diagnostic_callback(move |_| {
            callback_result.store(
                callback_runtime.exclusive_admission_available(),
                Ordering::Relaxed,
            );
        })
        .build()
        .expect("assembler should build");
    let module = assembler
            .module(["runtime_callback_admission"])
            .script(
                "g",
                "language g0\nimport 'std\nresult = anno refl:(.log 'info {msg:{text:\"callback\"}}) \"done\"\n",
            )
            .build()
            .expect("callback fixture should compile");
    assembler
        .evaluate(
            &assembler
                .get(module.value(), "result")
                .expect("fixture should define result"),
        )
        .expect("reflection gate should complete");

    assert!(callback_observed.load(Ordering::Relaxed));
}

#[test]
fn reasoning_failure_acknowledgement_is_idempotent_and_runtime_bound() {
    let runtime = EvaluationRuntime::new(0).expect("dormant runtime should build");
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should build");
    let peer = Assembler::builder()
        .evaluation_runtime(runtime)
        .build()
        .expect("same-runtime peer assembler should build");
    let foreign = Assembler::builder()
        .evaluation_runtime(EvaluationRuntime::new(0).expect("foreign runtime should build"))
        .build()
        .expect("foreign-runtime assembler should build");
    let task = assembler
        .eval_context()
        .schedule_task(|_| Ok(Box::new(FailedReasoningTask)))
        .expect("failing task should schedule");

    let settle = || match assembler.drain_reasoning() {
        RuntimeReadiness::Ready(snapshot) => snapshot
            .settle()
            .expect("unchanged runtime readiness should settle"),
        RuntimeReadiness::Busy => panic!("draining should reach a stable instant"),
        RuntimeReadiness::Deadlocked(deadlock) => panic!(
            "failing task unexpectedly deadlocked with {} unfinished work items",
            deadlock.unfinished().len()
        ),
    };
    let report = settle();
    let [failure] = report.task_failures() else {
        panic!("drain should report exactly one task failure")
    };
    assert_eq!(failure.task_id(), task.id().get());
    assert_eq!(failure.message(), "public reasoning failure");
    let failure = failure.clone();

    let error = foreign
        .acknowledge_reasoning_failure(&failure)
        .expect_err("a foreign runtime must reject the acknowledgement capability");
    assert!(error.to_string().contains("different evaluation runtime"));
    let retained_failures = settle();
    let [retained_failure] = retained_failures.task_failures() else {
        panic!("foreign-runtime acknowledgement must retain one originating failure")
    };
    assert_eq!(retained_failure.task_id(), failure.task_id());
    assert_eq!(retained_failure.message(), failure.message());

    peer.acknowledge_reasoning_failure(&failure)
        .expect("a same-runtime assembler should route to the producer ledger");
    assembler
        .acknowledge_reasoning_failure(&failure)
        .expect("repeated acknowledgement should be harmless");
    assert!(settle().task_failures().is_empty());
    assert!(matches!(
        assembler.eval_context().poll_reflection_task(&task),
        EvaluationWaitPoll::Failed(error)
            if error.to_string() == "public reasoning failure"
    ));
}
