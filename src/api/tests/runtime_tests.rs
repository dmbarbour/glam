use super::super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::core::{Key, OpaqueValue, Value as CoreValue};
use crate::evaluation::{EvalContext, EvaluationSession, ReflectionTaskProfile};
use crate::number::Number;
use crate::reflection::RuntimeInputSequence;

use super::{FailedReasoningTask, assert_unclaimed_lazy, public_value};

fn same_representation(runtime: &EvaluationRuntime, left: &Value, right: &Value) -> bool {
    runtime
        .values()
        .same_representation_for_test(left, right)
        .expect("test values should belong to the runtime")
}

#[test]
fn public_values_use_inline_or_shared_registered_roots() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let baseline = runtime
        .collect_managed_for_maintenance()
        .expect("canonical roots should collect before the public-value fixture");

    let inline = runtime.values().integer(42);
    let after_inline = runtime
        .collect_managed_for_maintenance()
        .expect("an inline public value should not add a managed root");
    assert_eq!(after_inline.root_entries(), baseline.root_entries());
    assert_eq!(after_inline.marked_slots(), baseline.marked_slots());
    drop(inline);

    let managed = runtime.values().empty_dict();
    let alias = managed.clone();
    let after_clone = runtime
        .collect_managed_for_maintenance()
        .expect("a managed public value should survive collection");
    assert_eq!(after_clone.root_entries(), baseline.root_entries() + 1);
    assert_eq!(after_clone.marked_slots(), baseline.marked_slots() + 1);

    drop(managed);
    let after_one_drop = runtime
        .collect_managed_for_maintenance()
        .expect("a public clone should retain the shared root cell");
    assert_eq!(after_one_drop.root_entries(), baseline.root_entries() + 1);

    drop(alias);
    let reclaimed = runtime
        .collect_managed_for_maintenance()
        .expect("dropping the last public clone should permit reclamation");
    assert_eq!(reclaimed.root_entries(), baseline.root_entries());
    assert_eq!(reclaimed.finalized_slots(), 1);
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
fn runtime_input_roots_follow_buffer_journal_and_committed_result_owners() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let domain = EffectTokenDomain::new(&runtime.values());
    let endpoint = runtime
        .input_endpoint::<Value, _>(Ok)
        .expect("value input endpoint should register");

    let journal_payload = Arc::new(());
    let journal_retained = Arc::downgrade(&journal_payload);
    endpoint
        .sender()
        .admit(domain.issue(journal_payload))
        .expect("input should be admitted");
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(journal_retained.upgrade().is_some());

    let (store, mut events) = input_transaction(&runtime);
    let consumed = events
        .read(&endpoint.reader())
        .expect("input read should succeed")
        .expect("admitted input should be present");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    drop(consumed);
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(journal_retained.upgrade().is_some());
    drop(events);
    drop(store);
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(journal_retained.upgrade().is_none());

    let result_payload = Arc::new(());
    let result_retained = Arc::downgrade(&result_payload);
    endpoint
        .sender()
        .admit(domain.issue(result_payload))
        .expect("second input should be admitted");
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(result_retained.upgrade().is_some());

    let (store, mut events) = input_transaction(&runtime);
    let consumed = events
        .read(&endpoint.reader())
        .expect("input read should succeed")
        .expect("second admitted input should be present");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    drop(events);
    drop(store);
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(result_retained.upgrade().is_some());
    drop(consumed);
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(result_retained.upgrade().is_none());
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
fn forked_runtime_event_journals_share_reads_but_not_input_claims() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .input_endpoint(integer_converter(&runtime))
        .expect("input endpoint should register");

    let (_, store, snapshot) = runtime.transaction_snapshot();
    let mut failed = RuntimeEventJournal::new(snapshot);
    let winner = failed.clone();
    assert!(failed.read(&endpoint.reader()).unwrap().is_none());
    endpoint
        .sender()
        .admit(1)
        .expect("input should admit after the failed alternative's read");
    assert_eq!(
        runtime.try_commit_transaction(&crate::reflection::StoreJournal::new(store), &winner),
        crate::reflection::StoreCommitResult::Conflict,
        "the winning sibling must retain the failed sibling's empty-tail observation"
    );

    let (_, store, snapshot) = runtime.transaction_snapshot();
    let mut failed = RuntimeEventJournal::new(snapshot);
    let winner = failed.clone();
    assert!(failed.read(&endpoint.reader()).unwrap().is_some());
    assert_eq!(
        runtime.try_commit_transaction(&crate::reflection::StoreJournal::new(store), &winner),
        crate::reflection::StoreCommitResult::Committed,
        "a failed sibling's input claim must not become the winner's edit"
    );
    let (_, mut current) = input_transaction(&runtime);
    assert_eq!(
        current
            .read(&endpoint.reader())
            .unwrap()
            .and_then(|value| value_i64(&runtime, &value)),
        Some(1),
        "the branch-local claim must remain uncommitted"
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
fn readiness_stamp_tracks_heap_and_event_observations() {
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
        &after_heap.reflection().root(),
        &initial.reflection().root()
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
        &after_input.reflection().root(),
        &after_heap.reflection().root()
    ));

    assert_eq!(
        initial.stamp().work_generation(),
        after_heap.stamp().work_generation()
    );
    assert_eq!(
        after_heap.stamp().work_generation(),
        after_input.stamp().work_generation()
    );
}

#[test]
fn gc_activity_invalidates_readiness_and_remains_busy_until_retired() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let RuntimeReadiness::Ready(before) = runtime.readiness() else {
        panic!("new runtime should be ready")
    };
    let lease = runtime
        .state
        .shared_resources
        .mutation_admission
        .begin_gc_activity();

    assert!(matches!(runtime.readiness(), RuntimeReadiness::Busy));
    assert_eq!(
        before.validate_without_settling(),
        Err(RuntimeSettlementError::RuntimeChanged),
        "a lease admitted after observation must stale the earlier snapshot"
    );

    let heap = runtime.values().core().managed_maintenance_snapshot();
    lease.finish(crate::runtime::RuntimeGcLeaseOutcome::success(heap));
    let RuntimeReadiness::Ready(after) = runtime.readiness() else {
        panic!("retiring the only maintenance lease should restore readiness")
    };
    assert!(after.stamp().gc_maintenance_revision() > before.stamp().gc_maintenance_revision());
}

#[test]
fn explicit_managed_collection_request_is_actionable_runtime_state() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let RuntimeReadiness::Ready(before) = runtime.readiness() else {
        panic!("new runtime should be ready")
    };
    drop(runtime.values().text("ordinary NoAuto access"));
    let RuntimeReadiness::Ready(after_access) = runtime.readiness() else {
        panic!("ordinary NoAuto access should not create maintenance work")
    };
    assert_eq!(
        after_access.stamp().gc_maintenance_revision(),
        before.stamp().gc_maintenance_revision(),
        "ordinary NoAuto value access must remain outside the lease path"
    );

    runtime
        .request_managed_collection()
        .expect("usable runtime should accept a collection request");
    let RuntimeReadiness::MaintenanceRequired(maintenance) = runtime.readiness() else {
        panic!("explicit request should become actionable maintenance")
    };
    assert_eq!(maintenance.state(), RuntimeMaintenanceState::Requested);
    assert_eq!(maintenance.runtime_id(), runtime.id());
    assert!(maintenance.revision() > before.stamp().gc_maintenance_revision());
    assert_eq!(
        before.validate_without_settling(),
        Err(RuntimeSettlementError::RuntimeChanged)
    );

    let report = maintenance
        .service()
        .expect("explicit NoAuto maintenance should complete");
    assert_ne!(report.epoch(), 0);
    let stale = maintenance
        .service()
        .expect_err("the consumed readiness action must not collect twice");
    assert_eq!(stale.kind(), RuntimeMaintenanceErrorKind::RuntimeChanged);
    assert_eq!(
        runtime
            .values()
            .core()
            .completed_collection_epoch_for_test(),
        report.epoch()
    );
    let RuntimeReadiness::Ready(after) = runtime.readiness() else {
        panic!("successful maintenance should consume the explicit request")
    };
    assert!(after.stamp().gc_maintenance_revision() > maintenance.revision());
}

#[test]
fn one_readiness_snapshot_admits_exactly_one_maintenance_service() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    runtime
        .request_managed_collection()
        .expect("runtime should accept a request");
    let RuntimeReadiness::MaintenanceRequired(maintenance) = runtime.readiness() else {
        panic!("request should produce one actionable snapshot")
    };
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let threads = (0..2)
        .map(|_| {
            let maintenance = maintenance.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                maintenance.service().map(|report| report.epoch())
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = threads
        .into_iter()
        .map(|thread| thread.join().expect("maintenance worker should not panic"))
        .collect::<Vec<_>>();

    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| {
                result
                    .as_ref()
                    .is_err_and(|error| error.kind() == RuntimeMaintenanceErrorKind::RuntimeChanged)
            })
            .count(),
        1
    );
    assert!(matches!(runtime.readiness(), RuntimeReadiness::Ready(_)));
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
    let lazy = super::public_semantic_thunk(&assembler.core_values(), move |_| {
        producer_evaluations.fetch_add(1, Ordering::SeqCst);
        Ok(CoreValue::Number(Number::integer(42)))
    });
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
    assert_unclaimed_lazy(&assembler, &lazy);
    assert_eq!(evaluations.load(Ordering::SeqCst), 0);
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert_unclaimed_lazy(&assembler, &lazy);
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
fn an_analysis_counts_a_pending_delivery_as_work() {
    // `quiescence-is-analyzed`: output committed for a Rust endpoint and
    // not yet delivered is work in progress.
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let endpoint = runtime
        .output_endpoint(decode_test_integer(runtime.values()), |_: i64| Ok(()))
        .expect("output endpoint should register");
    assert!(runtime.analysis().unwrap().quiescent());
    let (store, mut events) = input_transaction(&runtime);
    events
        .write(&endpoint.writer(), runtime.values().integer(1))
        .expect("the intent should reserve an ID");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    assert!(
        !runtime.analysis().unwrap().quiescent(),
        "a delivery is pending"
    );
    endpoint
        .delivery()
        .deliver_next()
        .expect("the delivery should run")
        .expect("one delivery is pending");
    assert!(runtime.analysis().unwrap().quiescent());
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
    // `quiescence-is-analyzed`: an analysis lists each retained failure,
    // until the host acknowledges it.
    let analyzed_failures = || {
        runtime
            .analysis()
            .unwrap()
            .issues()
            .iter()
            .filter(|issue| issue.family() == RuntimeIssueFamily::DeliveryFailure)
            .count()
    };
    assert_eq!(analyzed_failures(), 3);

    let (generation, _, _) = runtime.transaction_snapshot();
    assert!(runtime.acknowledge_delivery_failure(adapter_id));
    assert!(!runtime.acknowledge_delivery_failure(adapter_id));
    assert_eq!(analyzed_failures(), 2);
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

/// A pump that finds the settlement guard held parks until the runtime's
/// activity changes, which releasing that guard must change: two pumps at
/// once, as two waiting cuts make, otherwise leave one parked for good.
#[test]
fn releasing_a_settlement_wakes_a_pump_that_found_it_held() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let admission = runtime.state.shared_resources.mutation_admission.clone();
    let activity = admission.activity();
    let (want, wanted) = std::sync::mpsc::channel();
    let (held, holding) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    // Just as the pump tries the guard, another thread takes it.
    admission.set_settlement_probe_for_test(move || {
        want.send(()).expect("the holder waits for the pump");
        holding.recv().expect("the holder takes the guard");
    });
    let holder_admission = admission.clone();
    let holder = std::thread::spawn(move || {
        wanted.recv().expect("the pump reaches the guard");
        let guard = holder_admission.settlement_guard();
        held.send(()).expect("the pump waits for the holder");
        released.recv().expect("the test releases the guard");
        drop(guard);
    });
    let parked = activity.wait_count();
    let (done, finished) = std::sync::mpsc::channel();
    let pumper = runtime.clone();
    let pump = std::thread::spawn(move || {
        pumper.pump_until_stable();
        done.send(()).expect("the test keeps its receiver");
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while activity.wait_count() == parked {
        assert!(
            std::time::Instant::now() < deadline,
            "the pump should park on the held settlement guard"
        );
        std::thread::yield_now();
    }
    release.send(()).expect("the holder waits to release");
    holder.join().expect("the holder should finish");
    finished
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("releasing the settlement guard must wake the parked pump");
    pump.join().expect("the pump should finish");
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
    let callback_collector = runtime.values().core().clone();
    let endpoint = runtime
        .output_endpoint(
            |value| {
                assert!(matches!(value.clone_core_for_test(), CoreValue::Opaque(_)));
                Ok(())
            },
            move |()| {
                callback_collector.collect_and_drain_external_owners_for_test();
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
                CoreValue::Opaque(OpaqueValue::new(runtime.values().core(), lease)),
            ),
        )
        .unwrap();
    runtime
        .values()
        .core()
        .collect_and_drain_external_owners_for_test();
    assert!(retained.upgrade().is_some());
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    drop(events);
    drop(store);
    runtime
        .values()
        .core()
        .collect_and_drain_external_owners_for_test();
    assert!(retained.upgrade().is_some());
    assert!(endpoint.delivery().deliver_next().unwrap().is_some());
    // Delivery releases its retained runtime value outside the callback and
    // locks, but opaque payload destruction is deliberately deferred to a
    // known-safe external-owner drain.
    assert!(retained.upgrade().is_some());
    assert_eq!(
        runtime
            .values()
            .core()
            .collect_and_drain_external_owners_for_test(),
        1
    );
    assert!(retained.upgrade().is_none());
    assert!(dropped.load(Ordering::Acquire));
}

#[test]
fn delivery_failure_roots_follow_ledger_and_snapshot_owners() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let domain = EffectTokenDomain::new(&runtime.values());
    let ledger_payload = Arc::new(());
    let snapshot_payload = Arc::new(());
    let ledger_retained = Arc::downgrade(&ledger_payload);
    let snapshot_retained = Arc::downgrade(&snapshot_payload);
    let contexts = Arc::new(Mutex::new(std::collections::VecDeque::from([
        domain.issue(ledger_payload),
        domain.issue(snapshot_payload),
    ])));
    let callback_contexts = contexts.clone();
    let error_values = runtime.values();
    let endpoint = runtime
        .output_endpoint(
            |_: Value| Ok(()),
            move |()| {
                let context = callback_contexts
                    .lock()
                    .expect("failure context queue should not be poisoned")
                    .pop_front()
                    .expect("each delivery should have one structured context");
                Err(Error::new("structured delivery failure")
                    .with_context(&error_values, context)
                    .expect("the failure context should belong to its runtime"))
            },
        )
        .expect("failure endpoint should register");
    let (store, mut events) = input_transaction(&runtime);
    let ledger_id = events
        .write(&endpoint.writer(), runtime.values().unit())
        .expect("first failure should journal");
    let snapshot_id = events
        .write(&endpoint.writer(), runtime.values().unit())
        .expect("second failure should journal");
    assert_eq!(
        runtime.try_commit_transaction(&store, &events),
        crate::reflection::StoreCommitResult::Committed
    );
    drop(events);
    drop(store);

    let RuntimeDeliveryOutcome::Failed(ledger_failure) = endpoint
        .delivery()
        .deliver_next()
        .expect("first delivery should run")
        .expect("first delivery should exist")
    else {
        panic!("the first delivery should fail")
    };
    assert_eq!(ledger_failure.delivery_id(), ledger_id);
    drop(ledger_failure);
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(ledger_retained.upgrade().is_some());
    assert!(runtime.acknowledge_delivery_failure(ledger_id));
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(ledger_retained.upgrade().is_none());

    let RuntimeDeliveryOutcome::Failed(snapshot_failure) = endpoint
        .delivery()
        .deliver_next()
        .expect("second delivery should run")
        .expect("second delivery should exist")
    else {
        panic!("the second delivery should fail")
    };
    assert_eq!(snapshot_failure.delivery_id(), snapshot_id);
    drop(snapshot_failure);
    let snapshot = runtime.delivery_failure_snapshot();
    assert!(runtime.acknowledge_delivery_failure(snapshot_id));
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(snapshot_retained.upgrade().is_some());
    assert!(snapshot.get(snapshot_id).is_some());
    drop(snapshot);
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(snapshot_retained.upgrade().is_none());
    assert!(
        contexts
            .lock()
            .expect("failure context queue should not be poisoned")
            .is_empty()
    );
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
            .is_err(),
        "reading what is undefined is an error"
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
        assembler.get(&runtime.reflection_root(), "atomic").is_err(),
        "the conflicted write left `atomic` undefined"
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

/// Runs each of `names`, annotation tasks that write text to the heap under
/// their own names, then drains reflection and returns the heap's texts.
fn run_reflection_texts(source: &str, names: &[&str]) -> Vec<String> {
    let assembler = Assembler::default();
    let module = assembler
        .module(["child_tasks"])
        .script("g", source)
        .build()
        .expect("the child task fixture should compile");
    for name in names {
        assembler
            .evaluate(&assembler.get(module.value(), name).unwrap())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    let _ = assembler.drain_reasoning();
    let heap = assembler.test_reflection_heap();
    names
        .iter()
        .map(|name| {
            let text = assembler
                .get(&heap, name)
                .unwrap_or_else(|error| panic!("{name} wrote nothing: {error}"));
            String::from_utf8_lossy(&assembler.to_binary(&text).unwrap()).into_owned()
        })
        .collect()
}

#[test]
fn child_tasks_queue_in_order_and_keep_their_machine_s_state() {
    // `reflection-tasks-are-machine-tasks`: `.child.task` queues a task on a
    // child machine and answers the promise of its result; the child runs
    // its tasks one at a time, in order, its local state persisting between
    // them. A failure breaks the task's promise and flags the child; a
    // status is `'idle`, `'busy` while a task is queued or running, and
    // `'error` while flagged.
    let source = r#"language g0
import 'std
queued = anno refl:(.child.task 'a (.set ['n] 41) >>= (\_ -> .child.task 'a (.get ['n] >>= (\n -> .r (n + 1))) >>= (\p -> seq p (.heap.set ['queued] (if p == 42 then "answered" else "wrong"))))) ()
failed = anno refl:(.child.task 'b (1 / 0) >>= (\p -> .try (seq p (.r ())) >>= (\r -> .child.status 'b >>= (\s -> .heap.set ['failed] (if dict.defines ['err] r then (if s == 'error then "flagged" else "unflagged") else "unbroken"))))) ()
ordered = anno refl:(.heap.set ['log] [] =>> .child.task 'o (.heap.rewrite ['log] (\l -> l ++ ["first"])) >>= (\_ -> .child.task 'o (.heap.rewrite ['log] (\l -> l ++ ["second"])) >>= (\p -> seq p (.heap.get ['log] >>= (\log -> .heap.set ['ordered] (if log == ["first", "second"] then "in order" else "out of order")))))) ()
statuses = anno refl:(.child.status 'untouched >>= (\idle -> .child.task 'c (.cut (.heap.try_get ['gate])) >>= (\p -> .child.status 'c >>= (\busy -> .heap.set ['gate] 7 =>> seq p (.child.status 'c >>= (\after -> .heap.set ['statuses] (if [idle, busy, after, p] == ['idle, 'busy, 'idle, 7] then "tracked" else "untracked"))))))) ()
"#;
    assert_eq!(
        run_reflection_texts(source, &["queued", "failed", "ordered", "statuses"]),
        ["answered", "flagged", "in order", "tracked"]
    );
}

#[test]
fn a_task_queued_within_a_cut_reaches_its_child_when_the_cut_commits() {
    // A failed cut's task never reaches its child. Awaiting a task's promise
    // within the cut that queued it can only wait, so the cut drops the
    // promise with `>>=`, not `=>>`, which requires and so awaits unit.
    let source = r#"language g0
import 'std
undone = anno refl:(.alt (.cut (.child.task 'a (.heap.set ['ran] "undone task") >>= (\_ -> .fail))) (.r ()) =>> .child.task 'a (.r ()) >>= (\p -> seq p (.alt (.heap.try_get ['ran] =>> .heap.set ['undone] "ran") (.heap.set ['undone] "never ran")))) ()
committed = anno refl:(.cut (.child.task 'b (.r "committed")) >>= (\p -> seq p (.heap.set ['committed] p))) ()
"#;
    assert_eq!(
        run_reflection_texts(source, &["undone", "committed"]),
        ["never ran", "committed"]
    );
}

#[test]
fn an_interrupt_flags_its_child_and_recovery_drops_the_queue() {
    // `reflection-tasks-are-machine-tasks`: `.child.interrupt` stops the
    // running task, breaks its promise with the reason, and flags the child,
    // whose state the task left; the stopped task's end claims nothing
    // more, so the task queued behind it waits. `.child.recover` clears the
    // flag and the queue, breaking the waiting task's promise, and the next
    // task finds the state as the interrupted one left it.
    let source = r#"language g0
import 'std
interrupted = anno refl:(.child.task 'c (.set ['n] 1 =>> .heap.set ['c_started] () =>> .cut (.heap.try_get ['c_gate])) >>= (\p -> .child.task 'c (.r "dropped") >>= (\q -> .cut (.heap.try_get ['c_started]) =>> .child.interrupt 'c "stop" =>> .try (seq p (.r ())) >>= (\rp -> .child.status 'c >>= (\flagged -> .child.recover 'c =>> .try (seq q (.r ())) >>= (\rq -> .child.status 'c >>= (\recovered -> .child.task 'c (.get ['n]) >>= (\n -> seq n (.heap.set ['interrupted] (if [flagged, recovered, n] == ['error, 'idle, 1] then (if rp.err.msg.text == "stop" then rq.err.msg.text else rp.err.msg.text) else "unflagged")))))))))) ()
repaired = anno refl:(.child.task 'f (.set ['n] 2 =>> .cut (.set ['n] 3 =>> (1 / 0))) >>= (\p -> .try (seq p (.r ())) >>= (\_ -> .child.recover 'f =>> .child.task 'f (.get ['n]) >>= (\n -> seq n (.heap.set ['repaired] (if n == 2 then "as the failure left it" else "changed")))))) ()
untouched = anno refl:(.child.interrupt 'z "unheard" =>> .child.recover 'z =>> .child.task 'z (.r "ran") >>= (\p -> seq p (.child.status 'z >>= (\s -> .heap.set ['untouched] (if s == 'idle then p else "changed"))))) ()
"#;
    assert_eq!(
        run_reflection_texts(source, &["interrupted", "repaired", "untouched"]),
        [
            "the task's machine was recovered from an error before the task ran",
            "as the failure left it",
            "ran",
        ]
    );
}

#[test]
fn a_task_leaves_its_machine_none_of_its_reset_frames() {
    // Control is a task's own: a task that fails within a `.reset` leaves
    // its machine the state it reached, without its reset frames, so the
    // next task, once the machine is recovered, ends as itself.
    let source = r#"language g0
import 'std
reset = anno refl:(.child.task 's (.reset 'k (.set ['n] 1 =>> (1 / 0)) >>= (\_ -> .r "the failed task's continuation")) >>= (\p -> .try (seq p (.r ())) >>= (\_ -> .child.recover 's =>> .child.task 's (.get ['n] >>= (\n -> .r (if n == 1 then "its own" else "another state"))) >>= (\q -> .try (seq q (.r q)) >>= (\r -> .heap.set ['reset] (if dict.defines ['ok] r then r.ok else r.err.msg.text)))))) ()
"#;
    assert_eq!(run_reflection_texts(source, &["reset"]), ["its own"]);
}

#[test]
fn a_clear_empties_a_child_and_its_descendants_once_none_is_busy() {
    // `.child.clear` stops no work: it waits while the child or a
    // descendant is busy, so a parent interrupts first, and an interrupt
    // reaches the descendants' running tasks, flagging their machines. The
    // clear then drops the waiting tasks, breaking their promises, and
    // leaves the child idle, its descendants' state gone. A task queued
    // within the clear's own transaction is dropped as a waiting one.
    let source = r#"language g0
import 'std
cleared = anno refl:(.child.task 'k (.child.task 'g (.set ['m] 1 =>> .heap.set ['g_started] () =>> .cut (.heap.try_get ['g_gate])) >>= (\g -> .r {g:g})) >>= (\p -> seq p (.r p.g) >>= (\g -> .child.task 'k (.set ['n] 1 =>> .heap.set ['k_started] () =>> .cut (.heap.try_get ['k_gate])) >>= (\running -> .child.task 'k (.r "waiting") >>= (\waiting -> .cut (.heap.try_get ['g_started] =>> .heap.try_get ['k_started]) =>> .try (.child.clear 'k) >>= (\busy -> .child.interrupt 'k "stop" =>> .child.clear 'k =>> .try (seq g (.r ())) >>= (\rg -> .try (seq running (.r ())) >>= (\rr -> .try (seq waiting (.r ())) >>= (\rw -> .child.status 'k >>= (\s -> .child.task 'k (.child.task 'g (.try (.get ['m])) >>= (\q -> .r {q:q})) >>= (\p2 -> seq p2 (.r p2.q) >>= (\q -> seq q (.heap.set ['cleared] (if busy == 'fail then (if [rg.err.msg.text, rr.err.msg.text] == ["stop", "stop"] then (if s == 'idle then (if dict.defines ['err] q then rw.err.msg.text else "state kept") else "busy") else rg.err.msg.text) else "cleared while busy")))))))))))))) ()
deep = anno refl:(.child.task 'd (.child.task 'e (.heap.set ['e_started] () =>> .cut (.heap.try_get ['e_gate])) >>= (\e -> .r {e:e})) >>= (\p -> seq p (.r p.e) >>= (\e -> .cut (.heap.try_get ['e_started]) =>> .child.status 'd >>= (\s -> .try (.child.clear 'd) >>= (\busy -> .child.interrupt 'd "halt" =>> .child.status 'd >>= (\after -> .child.clear 'd =>> .try (seq e (.r ())) >>= (\re -> .heap.set ['deep] (if [s, after] == ['idle, 'idle] then (if busy == 'fail then re.err.msg.text else "cleared a busy grandchild") else "child busy")))))))) ()
dropped = anno refl:(.cut (.child.task 'x (.r "ran") >>= (\p -> .child.clear 'x =>> .r {p:p})) >>= (\d -> .try (seq d.p (.r ())) >>= (\r -> .heap.set ['dropped] r.err.msg.text))) ()
"#;
    assert_eq!(
        run_reflection_texts(source, &["cleared", "deep", "dropped"]),
        [
            "the task's machine was cleared before the task ran",
            "halt",
            "the task's machine was cleared before the task ran",
        ]
    );
}

#[test]
fn recovery_and_clearing_wait_while_the_child_is_busy() {
    // As `.child.call` does, `.child.recover` and `.child.clear` wait while
    // the child is busy: `.try`, which never waits, reports them failed, and
    // a clear outside a cut acts once the child's task has finished, whose
    // promise it leaves fulfilled.
    let source = r#"language g0
import 'std
waited = anno refl:(.child.task 'w (.cut (.heap.try_get ['w_gate]) =>> .set ['n] 5 =>> .r "done") >>= (\p -> .try (.child.clear 'w) >>= (\early -> .try (.child.recover 'w) >>= (\recovered -> .child.task 'o (.cut (.heap.try_get ['w_waiting]) =>> .heap.set ['w_gate] ()) >>= (\_ -> .heap.set ['w_waiting] () =>> .child.clear 'w =>> seq p (.child.task 'w (.try (.get ['n])) >>= (\n -> seq n (.heap.set ['waited] (if [early, recovered] == ['fail, 'fail] then (if dict.defines ['err] n then p else "state kept") else "acted while busy"))))))))) ()
"#;
    assert_eq!(run_reflection_texts(source, &["waited"]), ["done"]);
}

#[test]
fn a_call_runs_on_its_child_s_state_within_the_caller_s_transaction() {
    // `child-call-frame`: `.child.call` runs its operation on the child's
    // local state, not the caller's, and answers its result; the state it
    // leaves commits with the caller's cut, and a failed cut undoes it. The
    // caller's own state returns as the call does.
    let source = r#"language g0
import 'std
called = anno refl:(.set ['who] "caller" =>> .child.task 'a (.set ['n] 1) >>= (\p -> seq p (.cut (.child.call 'a (.try (.get ['who]) >>= (\w -> .get ['n] >>= (\n -> .set ['n] (n + 1) =>> .r {w:w, n:n})))) >>= (\r -> .alt (.cut (.child.call 'a (.set ['n] 100) =>> .fail)) (.r ()) =>> .get ['who] >>= (\who -> .child.task 'a (.get ['n]) >>= (\q -> seq q (.heap.set ['called] (if [r.n, q, who] == [1, 2, "caller"] then (if dict.defines ['err] r.w then "committed" else "saw the caller's state") else "wrong")))))))) ()
"#;
    assert_eq!(run_reflection_texts(source, &["called"]), ["committed"]);
}

#[test]
fn a_task_committed_to_the_child_meanwhile_runs_the_call_s_cut_again() {
    // A call reads its child's queue and state in the caller's
    // transaction, so a task committed to the child after the caller's
    // snapshot, here by `.TEST_ONLY.queue_now` as another writer would,
    // conflicts with it: the cut runs again, waits for the child's task,
    // and reads the state it left.
    let source = r#"language g0
import 'std
raced = anno refl:(.child.task 'c (.set ['n] 1) >>= (\p -> seq p (.cut (.child.call 'c (.get ['n]) >>= (\n -> if n == 1 then .TEST_ONLY.queue_now 'c (.set ['n] 2) >>= (\_ -> .r n) else .r n)) >>= (\n -> .heap.set ['raced] (if n == 2 then "ran again" else "committed a stale read"))))) ()
"#;
    assert_eq!(run_reflection_texts(source, &["raced"]), ["ran again"]);
}

#[test]
fn a_call_waits_while_its_child_is_busy() {
    // A call waits until the child has finished what was committed to it:
    // `.try`, which never waits, reports it failed, and a cut enters once
    // the child's task has ended, on the state that task left.
    let source = r#"language g0
import 'std
waited = anno refl:(.child.task 'w (.cut (.heap.try_get ['w_gate]) =>> .set ['n] 5) >>= (\_ -> .try (.child.call 'w (.get ['n])) >>= (\early -> .child.task 'o (.cut (.heap.try_get ['w_waiting]) =>> .heap.set ['w_gate] ()) >>= (\_ -> .heap.set ['w_waiting] () =>> .cut (.child.call 'w (.get ['n])) >>= (\n -> .heap.set ['waited] (if early == 'fail then (if n == 5 then "waited" else "entered early") else "entered while busy")))))) ()
"#;
    assert_eq!(run_reflection_texts(source, &["waited"]), ["waited"]);
}

#[test]
fn a_call_enters_neither_a_child_in_error_nor_one_its_transaction_queued_on() {
    // Recovery is the only way into a child in error, so a call fails with
    // an error until the parent recovers it, then finds the state the
    // failure left. A call waits for the child to finish its queue, and the
    // child starts a task the caller's transaction queued only once the
    // transaction commits: the wait could never end, which is an error.
    let source = r#"language g0
import 'std
flagged = anno refl:(.child.task 'f (.set ['n] 1 =>> (1 / 0)) >>= (\p -> .try (seq p (.r ())) >>= (\_ -> .try (.child.call 'f (.get ['n])) >>= (\r -> .child.recover 'f =>> .cut (.child.call 'f (.get ['n])) >>= (\n -> .heap.set ['flagged] (if n == 1 then r.err.msg.text else "unrecovered")))))) ()
pending = anno refl:(.try (.child.task 'q (.r ()) >>= (\_ -> .child.call 'q (.r ()))) >>= (\r -> .heap.set ['pending] r.err.msg.text)) ()
"#;
    assert_eq!(
        run_reflection_texts(source, &["flagged", "pending"]),
        [
            "`.child.call` cannot enter a child in error until its parent recovers it",
            "`.child.call` would wait for good: the child starts the tasks \
             this transaction queued on it only once the transaction commits",
        ]
    );
}

#[test]
fn an_error_in_a_call_undoes_with_the_caller_s_transaction() {
    // An error that ends a call's operation is the caller's: `.try`
    // answers it, undoing what it tried, the child's state with it, and
    // leaves the child unflagged, as it was.
    let source = r#"language g0
import 'std
tried = anno refl:(.child.task 'e (.set ['n] 1) >>= (\p -> seq p (.try (.child.call 'e (.set ['n] 2 =>> (1 / 0))) >>= (\r -> .child.status 'e >>= (\s -> .cut (.child.call 'e (.get ['n])) >>= (\n -> .heap.set ['tried] (if [s, n] == ['idle, 1] then (if dict.defines ['err] r then "undone" else "no error") else "flagged or changed"))))))) ()
"#;
    assert_eq!(run_reflection_texts(source, &["tried"]), ["undone"]);
}

#[test]
fn a_plain_call_waits_for_its_task_on_the_child() {
    // `child-call-plain`: outside a transaction, `.child.call` queues its
    // operation on the child, after what was queued before it, and waits
    // for the reply: it answers the operation's result, on the child's
    // state, or fails with its failure, which flags the child. A child in
    // error it fails at once.
    let source = r#"language g0
import 'std
plain = anno refl:(.heap.set ['log] [] =>> .child.task 'a (.set ['n] 1 =>> .heap.rewrite ['log] (\l -> l ++ ["queued"])) >>= (\_ -> .child.call 'a (.heap.rewrite ['log] (\l -> l ++ ["called"]) =>> .get ['n]) >>= (\n -> .heap.get ['log] >>= (\log -> .heap.set ['plain] (if [n, log] == [1, ["queued", "called"]] then "after its queue" else "out of order"))))) ()
failing = anno refl:(.child.task 'p (.child.call 'c (1 / 0) >>= (\_ -> .r "went on")) >>= (\p -> .try (seq p (.r ())) >>= (\r -> .child.task 'd (1 / 0) >>= (\d -> .try (seq d (.r ())) >>= (\rd -> .child.recover 'p =>> .child.task 'p (.child.status 'c) >>= (\s -> seq s (.heap.set ['failing] (if s == 'error then (if r.err.msg.text == rd.err.msg.text then "flagged" else r.err.msg.text) else "unflagged")))))))) ()
refused = anno refl:(.child.task 'p (.child.task 'f (1 / 0) >>= (\x -> .try (seq x (.r ())) >>= (\_ -> .child.call 'f (.r "entered")))) >>= (\p -> .try (seq p (.r ())) >>= (\r -> .heap.set ['refused] r.err.msg.text))) ()
"#;
    assert_eq!(
        run_reflection_texts(source, &["plain", "failing", "refused"]),
        [
            "after its queue",
            "flagged",
            "`.child.call` cannot enter a child in error until its parent recovers it",
        ]
    );
}

#[test]
fn calls_nest_on_their_children_s_children() {
    // Within a call, requests act on the child's children: a call nests in
    // a call, on the grandchild, and the caller's child of the same key is
    // another machine. So it goes for a task on a machine of its own.
    let source = r#"language g0
import 'std
nested = anno refl:(.cut (.child.call 'a (.set ['n] 1 =>> .child.call 'b (.set ['m] 2) =>> .child.status 'b)) >>= (\s -> .child.task 'a (.get ['n] >>= (\n -> .child.task 'b (.get ['m]) >>= (\m -> seq m (.r [n, m])))) >>= (\q -> seq q (.cut (.child.call 'b (.try (.try_get ['m]))) >>= (\own -> .heap.set ['nested] (if [s, q, own] == ['idle, [1, 2], 'fail] then "nested" else "wrong")))))) ()
tasked = anno refl:(.child.task 'p (.cut (.child.call 'a (.child.call 'b (.set ['m] 2))) =>> .child.task 'a (.child.task 'b (.get ['m]) >>= (\m -> seq m (.r m))) >>= (\q -> seq q (.r q))) >>= (\p -> seq p (.heap.set ['tasked] (if p == 2 then "nested" else "wrong")))) ()
"#;
    assert_eq!(
        run_reflection_texts(source, &["nested", "tasked"]),
        ["nested", "nested"]
    );
}

#[test]
fn settlement_reports_the_children_left_in_error() {
    // A flagged child fails settlement, with what flagged it, until its
    // parent recovers it: a failed task's diagnostic, or an interrupt's
    // reason.
    let source = r#"language g0
import 'std
recovered = anno refl:(.child.task 'r (.fail) >>= (\_ -> .cut (.child.status 'r >>= (\s -> if s == 'error then .r () else .fail)) =>> .child.recover 'r =>> .heap.set ['recovered] "recovered")) ()
failed = anno refl:(.child.task 'f (.fail) >>= (\_ -> .cut (.child.status 'f >>= (\s -> if s == 'error then .r () else .fail)) =>> .heap.set ['failed] "failed")) ()
stopped = anno refl:(.child.task 's (.heap.set ['s_started] () =>> .cut (.heap.try_get ['s_gate])) >>= (\_ -> .cut (.heap.try_get ['s_started]) =>> .child.interrupt 's "stopped by its parent" =>> .heap.set ['stopped] "stopped")) ()
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["failed_children"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    for name in ["recovered", "failed", "stopped"] {
        assembler
            .evaluate(&assembler.get(module.value(), name).unwrap())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    let RuntimeReadiness::Ready(snapshot) = assembler.drain_reasoning() else {
        panic!("the children should leave the runtime ready")
    };
    let report = snapshot.settle().expect("the runtime should settle");
    let failed: Vec<(Key, String)> = report
        .failed_machines()
        .iter()
        .map(|machine| {
            (
                machine.path().keys().last().cloned().unwrap(),
                machine
                    .diagnostic()
                    .rendered_text_for_test(&assembler.values()),
            )
        })
        .collect();
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert!(failed.contains(&(
        Key::atom_from_text("f"),
        "reflection task failed permanently".to_owned()
    )));
    assert!(failed.contains(&(Key::atom_from_text("s"), "stopped by its parent".to_owned())));
}

#[test]
fn an_annotation_task_s_failure_flags_its_anonymous_root() {
    // `reflection-tasks-are-machine-tasks`: an annotation task's failure
    // flags its anonymous root, which no parent recovers, so settlement
    // reports it even though the failure reached the demander.
    let source = r#"language g0
import 'std
observed = anno refl:(.fail) "never"
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["failed_annotation"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    let observed = assembler
        .get(module.value(), "observed")
        .and_then(|value| assembler.evaluate(&value));
    assert!(observed.is_err(), "the demander observes the failure");
    let RuntimeReadiness::Ready(snapshot) = assembler.drain_reasoning() else {
        panic!("the failed annotation should leave the runtime ready")
    };
    let report = snapshot.settle().expect("the runtime should settle");
    let [failed] = report.failed_machines() else {
        panic!("one root should fail: {:?}", report.failed_machines())
    };
    let path = failed.path().keys();
    assert_eq!(path.len(), 2, "{path:?}");
    assert_eq!(path[0], Key::atom_from_text("refl"));
    assert!(crate::reflection::is_anonymous_key(&path[1]));
    assert_eq!(
        failed
            .diagnostic()
            .rendered_text_for_test(&assembler.values()),
        "reflection task failed permanently"
    );
}

#[test]
fn a_panicking_child_task_halts_with_its_machine_flagged() {
    // `child-task-halts`: a task that stops without finishing, as a panic
    // stops one, halts its run: its reply breaks and its machine is flagged,
    // so its parent reads `'error` and recovers it, and the queue serves the
    // next task.
    let source = r#"language g0
import 'std
launch = \bomb -> anno refl:(.child.task 'p (seq bomb (.r ())) >>= (\p -> .cut (.child.status 'p >>= (\s -> if s == 'error then .r () else .fail)) =>> .try (seq p (.r ())) >>= (\r -> .child.recover 'p =>> .child.task 'p (.r "recovered") >>= (\q -> seq q (.heap.set ['halted] (if q == "recovered" then r.err.msg.text else "unrecovered")))))) "launched"
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["panicking_child"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    let values = assembler.values();
    let bomb = Value::from_runtime_root(values.core().construct_runtime_value_root(|access| {
        CoreValue::Lazy(crate::core::LazyValue::semantic_thunk_in(access, |_| {
            panic!("a child task's deliberate panic")
        }))
    }));
    let launch = assembler.get(module.value(), "launch").unwrap();
    let launched = values.apply(&launch, [bomb]).unwrap();
    assembler
        .evaluate(&launched)
        .expect("the parent should see its child halt");
    let _ = assembler.drain_reasoning();
    let heap = assembler.test_reflection_heap();
    let halted = assembler.get(&heap, "halted").expect("the parent wrote");
    assert_eq!(
        String::from_utf8_lossy(&assembler.to_binary(&halted).unwrap()),
        "the task halted before it finished"
    );
}

#[test]
fn an_anonymous_root_s_subtree_is_reclaimed_once_nothing_runs_there() {
    // `anonymous-root-reclamation`: once an anonymous root's task has ended
    // and no task runs or waits in its subtree, nothing reaches the subtree,
    // and the runtime drops its machines, whichever ends last: the root,
    // after the children it awaited, or a child, after its root. A child a
    // root only called goes with it. A failed machine stays whole for
    // settlement.
    let source = r#"language g0
import 'std
awaited = anno refl:(.child.task 'a (.set ['n] 1 =>> .r 1) >>= (\p -> seq p (.child.task 'a (.r 2) >>= (\q -> seq q (.heap.set ['awaited] "done"))))) ()
detached = anno refl:(.child.task 'd (.cut (.heap.try_get ['d_gate]) =>> .r ()) >>= (\_ -> .heap.set ['detached] "done")) ()
opener = anno refl:(.cut (.heap.try_get ['detached] >>= (\_ -> .r ())) =>> .heap.set ['d_gate] () =>> .heap.set ['opener] "done") ()
failed = anno refl:(.child.task 'f (.fail) >>= (\_ -> .child.task 'g (.r ()) >>= (\q -> seq q (.heap.set ['failed] "done")))) ()
called = anno refl:(.cut (.child.call 'c (.set ['n] 1)) =>> .heap.set ['called] "done") ()
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["reclaimed_roots"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    let names = ["awaited", "detached", "opener", "failed", "called"];
    for name in names {
        assembler
            .evaluate(&assembler.get(module.value(), name).unwrap())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    let _ = assembler.drain_reasoning();
    let heap = assembler.test_reflection_heap();
    for name in names {
        assert!(assembler.get(&heap, name).is_ok(), "{name} finished");
    }
    let left = assembler
        .evaluation_runtime()
        .state
        .shared_resources
        .anonymous_machines_for_test();
    let failed = Key::atom_from_text("f");
    assert!(
        !left.is_empty() && left.iter().all(|path| path.last() == Some(&failed)),
        "only the failed machine stays: {left:?}"
    );
}

#[test]
fn an_interrupt_within_a_failed_cut_stops_nothing() {
    // An interrupt is a write like any other: a cut that fails undoes it,
    // and the task runs on to its end.
    let source = r#"language g0
import 'std
undone = anno refl:(.child.task 'u (.heap.set ['u_started] () =>> .cut (.heap.try_get ['u_gate]) =>> .r "finished") >>= (\p -> .cut (.heap.try_get ['u_started]) =>> .alt (.cut (.child.interrupt 'u "never" =>> .fail)) (.r ()) =>> .child.status 'u >>= (\s -> .heap.set ['u_gate] () =>> seq p (.heap.set ['undone] (if s == 'busy then p else "interrupted"))))) ()
"#;
    assert_eq!(run_reflection_texts(source, &["undone"]), ["finished"]);
}

#[test]
fn an_analysis_tells_idle_waits_from_what_is_owed() {
    // `quiescence-is-analyzed`: an analysis observes the runtime at any
    // time, changing nothing. Queued work is not quiescent. At quiescence a
    // wait on observed state is idle; a wait on a promise no one resolves,
    // or on a task that waits on state, is owed, once at its chain's head,
    // naming where the chain ends; a cut waiting after an error is an erred
    // wait; and a failed child is a machine in error, as is the anonymous
    // root of a failed annotation task.
    let source = r#"language g0
import 'std
inner = anno refl:(.cut (.heap.try_get ['gate])) "inner"
idle = anno refl:(.child.task 'i (.cut (.heap.try_get ['never])) >>= (\_ -> .heap.set ['idle] "queued")) ()
owed = anno refl:(.promise.new >>= (\pr -> .heap.set ['kept] pr.resolver =>> .child.task 'o (seq pr.promise (.r ())) >>= (\_ -> .heap.set ['owed] "queued"))) ()
erred = anno refl:(.child.task 'e (.cut (.heap.get ['missing])) >>= (\_ -> .heap.set ['erred] "queued")) ()
failed = anno refl:(.child.task 'f (1 / 0) >>= (\_ -> .heap.set ['failed] "queued")) ()
chained = anno refl:(.child.task 'c (seq inner (.r ())) >>= (\_ -> .heap.set ['chained] "queued")) ()
broken = anno refl:(1 / 0) ()
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["analyzed"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    for name in ["idle", "owed", "erred", "failed", "chained"] {
        assembler
            .evaluate(&assembler.get(module.value(), name).unwrap())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    assert!(
        assembler
            .get(module.value(), "broken")
            .and_then(|broken| assembler.evaluate(&broken))
            .is_err(),
        "the annotation task fails"
    );
    let runtime = assembler.evaluation_runtime();
    assert!(
        !runtime.analysis().unwrap().quiescent(),
        "the children's tasks are queued"
    );
    let _ = assembler.drain_reasoning();
    let analysis = runtime.analysis().unwrap();
    assert!(analysis.quiescent());
    let families: Vec<_> = analysis.issues().iter().map(RuntimeIssue::family).collect();
    assert_eq!(
        families,
        [
            RuntimeIssueFamily::PromiseWait,
            RuntimeIssueFamily::ErredWait,
            RuntimeIssueFamily::PromiseWait,
            RuntimeIssueFamily::MachineInError,
            RuntimeIssueFamily::MachineInError,
        ]
    );
    let family_of = |work: u64| {
        analysis
            .waits()
            .iter()
            .find(|wait| wait.work().work_id() == work)
            .map(RuntimeWait::family)
    };
    let causes: Vec<_> = analysis
        .issues()
        .iter()
        .filter_map(|issue| match issue {
            RuntimeIssue::Wait(wait) if wait.family() == RuntimeWaitFamily::PromiseWait => {
                Some(wait.cause_work_id().and_then(family_of))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        causes,
        [
            Some(RuntimeWaitFamily::PromiseWait),
            Some(RuntimeWaitFamily::Idle),
        ],
        "one chain ends at a wait on a promise, the other at an idle task"
    );
    assert_eq!(
        analysis
            .waits()
            .iter()
            .filter(|wait| wait.family() == RuntimeWaitFamily::Idle)
            .count(),
        2,
        "the idle child and the task the chain ends at"
    );
    let erred = analysis
        .waits()
        .iter()
        .find(|wait| wait.family() == RuntimeWaitFamily::ErredWait)
        .and_then(|wait| wait.work().blocked_diagnostic())
        .expect("the erred wait keeps its error")
        .rendered_text_for_test(&assembler.values());
    assert!(erred.contains("missing"), "{erred}");
    let failed: Vec<_> = analysis
        .failed_machines()
        .iter()
        .map(|machine| machine.path().keys().to_vec())
        .collect();
    assert!(
        failed
            .iter()
            .any(|path| path.last() == Some(&Key::atom_from_text("f"))),
        "{failed:?}"
    );
    assert!(
        failed
            .iter()
            .any(|path| path.len() == 2 && crate::reflection::is_anonymous_key(&path[1])),
        "the failed annotation task's root: {failed:?}"
    );
    let again = runtime.analysis().unwrap();
    assert_eq!(
        again
            .issues()
            .iter()
            .map(RuntimeIssue::family)
            .collect::<Vec<_>>(),
        families,
        "an analysis changes nothing"
    );
    assert!(matches!(
        assembler.drain_reasoning(),
        RuntimeReadiness::Deadlocked(_)
    ));
}

#[test]
fn an_analysis_names_the_machine_each_waiting_task_runs_on() {
    // `quiescence-is-analyzed`: the analysis's entries carry what a host's
    // options need. A waiting task names the machine it runs on: a child,
    // by the path its queue has, or an annotation task's own anonymous
    // root, reachable only so; a lazy value forced on the way, which is no
    // task, names none.
    let source = r#"language g0
import 'std
inner = anno refl:(.cut (.heap.try_get ['gate])) "inner"
waits = anno refl:(.child.task 'w (.cut (.heap.try_get ['never])) >>= (\_ -> .child.task 'v (seq inner (.r ())) >>= (\_ -> .heap.set ['waits] "queued"))) ()
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["named"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    assembler
        .evaluate(&assembler.get(module.value(), "waits").unwrap())
        .expect("the root queues its children");
    let _ = assembler.drain_reasoning();
    let analysis = assembler.evaluation_runtime().analysis().unwrap();
    assert!(analysis.quiescent());
    let mut children = Vec::new();
    let mut roots = Vec::new();
    for wait in analysis.waits() {
        let work = wait.work();
        assert_eq!(
            wait.machine().is_some(),
            work.kind() == RuntimeWorkKind::ReflectionTask,
            "{work:?}"
        );
        let Some(machine) = wait.machine() else {
            continue;
        };
        let keys = machine.keys();
        assert!(crate::reflection::is_anonymous_key(&keys[1]), "{keys:?}");
        match keys.len() {
            2 => roots.push((keys.to_vec(), work.task_id())),
            3 => children.push((keys[..2].to_vec(), keys[2].clone())),
            _ => panic!("an unexpected machine: {keys:?}"),
        }
    }
    assert!(
        analysis.waits().iter().any(|wait| wait.machine().is_none()),
        "the forced lazy value is no task"
    );
    let [(root, task)] = roots.as_slice() else {
        panic!("one waiting annotation task: {roots:?}")
    };
    let task = task.expect("an annotation task has a task id");
    assert_eq!(
        root,
        &crate::reflection::anonymous_root(crate::evaluation::EvaluationTaskId::from_nonzero(
            std::num::NonZeroU64::new(task).expect("task ids are nonzero")
        )),
        "an annotation task runs on its own root"
    );
    children.sort_by(|left, right| left.1.cmp(&right.1));
    assert_eq!(
        children
            .iter()
            .map(|(_, key)| key.clone())
            .collect::<Vec<_>>(),
        [Key::atom_from_text("v"), Key::atom_from_text("w")]
    );
    assert_eq!(
        children[0].0, children[1].0,
        "both under the root that queued them"
    );
    assert_ne!(
        &children[0].0, root,
        "a root apart from the annotation task's"
    );
}

#[test]
fn a_host_interrupt_flags_what_it_halts_and_only_that() {
    // `quiescence-is-analyzed`: a host interrupts by a machine's path, as
    // the analysis names it. An idle machine has nothing to halt and stays
    // unflagged; a waiting child's run stops, its machine flagged with the
    // reason and its reply broken with it; an annotation task stops, its
    // root flagged with the reason, as its failure would flag it.
    let source = r#"language g0
import 'std
inner = anno refl:(.cut (.heap.try_get ['gate])) "inner"
roots = anno refl:(.child.task 'i (.r ()) >>= (\q -> seq q (.child.task 'w (.cut (.heap.try_get ['never])) >>= (\p -> .heap.set ['w_reply] p =>> .child.task 'v (seq inner (.r ())) >>= (\_ -> .heap.set ['roots] "queued"))))) ()
check = anno refl:(.heap.get ['w_reply] >>= (\p -> seq p (.r ()))) ()
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["interrupted"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    assembler
        .evaluate(&assembler.get(module.value(), "roots").unwrap())
        .expect("the root queues its children");
    let _ = assembler.drain_reasoning();
    let runtime = assembler.evaluation_runtime();
    let analysis = runtime.analysis().unwrap();
    let machine_of = |key: &str| {
        analysis
            .waits()
            .iter()
            .filter_map(RuntimeWait::machine)
            .find(|machine| machine.keys().last() == Some(&Key::atom_from_text(key)))
            .cloned()
            .unwrap_or_else(|| panic!("a wait on '{key}"))
    };
    let waiting = machine_of("w");
    let idle = MachinePath::from([&waiting.keys()[..2], &[Key::atom_from_text("i")]].concat());
    let inner_root = analysis
        .waits()
        .iter()
        .filter_map(RuntimeWait::machine)
        .find(|machine| machine.keys().len() == 2)
        .cloned()
        .expect("the annotation task waits on its root");
    let values = assembler.values();
    assert!(
        runtime
            .interrupt(&idle, values.text("idle"))
            .unwrap()
            .is_empty(),
        "an idle machine has nothing to halt"
    );
    assert_eq!(
        runtime
            .interrupt(&waiting, values.text("host stops w"))
            .unwrap(),
        std::slice::from_ref(&waiting)
    );
    assert_eq!(
        runtime
            .interrupt(&inner_root, values.text("host stops inner"))
            .unwrap(),
        std::slice::from_ref(&inner_root)
    );
    let broken = assembler
        .get(module.value(), "check")
        .and_then(|check| assembler.evaluate(&check))
        .expect_err("the stopped task's reply is broken");
    let broken = broken
        .diagnostic(&values)
        .unwrap()
        .rendered_text_for_test(&values);
    assert!(broken.contains("host stops w"), "{broken}");
    let _ = assembler.drain_reasoning();
    let analysis = runtime.analysis().unwrap();
    let failure = |machine: &MachinePath| {
        analysis
            .failed_machines()
            .iter()
            .find(|failed| failed.path() == machine)
            .map(|failed| failed.diagnostic().rendered_text_for_test(&values))
    };
    assert!(failure(&idle).is_none(), "the idle machine is unflagged");
    assert!(
        failure(&waiting).is_some_and(|text| text.contains("host stops w")),
        "{:?}",
        failure(&waiting)
    );
    assert!(
        failure(&inner_root).is_some_and(|text| text.contains("host stops inner")),
        "{:?}",
        failure(&inner_root)
    );
    assert!(
        analysis
            .waits()
            .iter()
            .all(|wait| wait.machine() != Some(&waiting) && wait.machine() != Some(&inner_root)),
        "neither stopped task waits"
    );
}

#[test]
fn an_interrupt_flags_the_children_its_run_waits_within_by_call() {
    // `quiescence-is-analyzed`: an interrupt is transitive. A run waiting
    // on a promise within a call into its child halts the call's work too,
    // so the callee is flagged with the run's machine, whether a host
    // interrupts the run or its parent does, as is an annotation task's
    // callee with its root.
    let source = r#"language g0
import 'std
held = anno refl:(.promise.new >>= (\pr -> .heap.set ['held_r] pr.resolver =>> .cut (.child.call 'k (seq pr.promise (.r ()))))) ()
calls = anno refl:(.promise.new >>= (\pr -> .heap.set ['kept] pr.resolver =>> .child.task 'h (.cut (.child.call 'c (seq pr.promise (.r ())))) >>= (\_ -> .child.task 'm (.child.task 'p (.cut (.child.call 'q (seq pr.promise (.r ())))) >>= (\_ -> .cut (.heap.try_get ['go]) =>> .child.interrupt 'p "the parent stops p" =>> .heap.set ['calls] "interrupted")) >>= (\_ -> .child.task 'f (seq held (.r ())) >>= (\_ -> .heap.set ['queued] "queued"))))) ()
go = anno refl:(.heap.set ['go] ()) ()
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["called"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    assembler
        .evaluate(&assembler.get(module.value(), "calls").unwrap())
        .expect("the root queues its children");
    let _ = assembler.drain_reasoning();
    let runtime = assembler.evaluation_runtime();
    let analysis = runtime.analysis().unwrap();
    assert!(analysis.quiescent());
    let caller = analysis
        .waits()
        .iter()
        .filter_map(RuntimeWait::machine)
        .find(|machine| machine.keys().last() == Some(&Key::atom_from_text("h")))
        .cloned()
        .expect("h waits within its call");
    let child = |machine: &MachinePath, key: &str| {
        MachinePath::from([machine.keys(), &[Key::atom_from_text(key)]].concat())
    };
    let values = assembler.values();
    let held = analysis
        .waits()
        .iter()
        .filter_map(RuntimeWait::machine)
        .find(|machine| machine.keys().len() == 2)
        .cloned()
        .expect("the annotation task waits within its call");
    assert_eq!(
        runtime
            .interrupt(&caller, values.text("the host stops h"))
            .unwrap(),
        [caller.clone(), child(&caller, "c")]
    );
    assert_eq!(
        runtime
            .interrupt(&held, values.text("the host stops held"))
            .unwrap(),
        [held.clone(), child(&held, "k")]
    );
    assembler
        .evaluate(&assembler.get(module.value(), "go").unwrap())
        .expect("the gate opens");
    let _ = assembler.drain_reasoning();
    let heap = assembler.test_reflection_heap();
    assert!(
        assembler.get(&heap, "calls").is_ok(),
        "the parent interrupted p"
    );
    let root = MachinePath::from(caller.keys()[..2].to_vec());
    let analysis = runtime.analysis().unwrap();
    let failed: Vec<(MachinePath, String)> = analysis
        .failed_machines()
        .iter()
        .map(|failed| {
            (
                failed.path().clone(),
                failed.diagnostic().rendered_text_for_test(&values),
            )
        })
        .collect();
    for (machine, reason) in [
        (caller.clone(), "the host stops h"),
        (child(&caller, "c"), "the host stops h"),
        (child(&child(&root, "m"), "p"), "the parent stops p"),
        (
            child(&child(&child(&root, "m"), "p"), "q"),
            "the parent stops p",
        ),
        (held.clone(), "the host stops held"),
        (child(&held, "k"), "the host stops held"),
    ] {
        assert!(
            failed
                .iter()
                .any(|(path, text)| path == &machine && text.contains(reason)),
            "{machine:?} with {reason:?}: {failed:?}"
        );
    }
    // The child that awaited the stopped annotation task fails of it.
    let f = Key::atom_from_text("f");
    assert_eq!(
        failed
            .iter()
            .filter(|(path, _)| path.keys().last() != Some(&f))
            .count(),
        6,
        "{failed:?}"
    );
}

#[test]
fn a_host_clear_takes_an_idle_subtree_and_refuses_a_busy_one() {
    // `quiescence-is-analyzed`: a host clears any machine whose subtree is
    // not busy, in error or not, under an ended root or a live one, as
    // `.child.clear` does a child. A machine where a task waits is refused
    // until interrupted. A clear breaks the replies of the tasks it drops,
    // which never ran, abandons what the machines accepted, drops a failed
    // annotation root, and leaves nothing behind the ended roots.
    let source = r#"language g0
import 'std
failed = anno refl:(.child.task 'f (1 / 0) >>= (\p -> .child.task 'f (.r "never") >>= (\q -> .heap.set ['f_later] q =>> .try (seq p (.r ())) >>= (\_ -> .heap.set ['failed] "failed")))) ()
broken = anno refl:(1 / 0) ()
owing = anno refl:(.promise.new >>= (\pr -> .heap.set ['r1] pr.resolver =>> .child.task 'o (.promise.accept pr.resolver {msg:{text:"o owes"}}) >>= (\p -> seq p (.heap.set ['owing] "owing")))) ()
live = anno refl:(.child.task 'w (.cut (.heap.try_get ['never])) >>= (\_ -> .heap.set ['live] "queued")) ()
parent = anno refl:(.child.task 'e (1 / 0) >>= (\p -> .try (seq p (.r ())) >>= (\_ -> .cut (.heap.try_get ['gate]) =>> .child.status 'e >>= (\s -> .heap.set ['parent] (if s == 'idle then "idle" else "kept"))))) "parent"
forcing = anno refl:(.child.task 'x (seq parent (.r ())) >>= (\_ -> .heap.set ['forcing] "queued")) ()
later = anno refl:(.heap.get ['f_later] >>= (\q -> seq q (.r ()))) ()
gate = anno refl:(.heap.set ['gate] ()) ()
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["cleared"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    for name in ["failed", "owing", "live", "forcing"] {
        assembler
            .evaluate(&assembler.get(module.value(), name).unwrap())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    assert!(
        assembler
            .get(module.value(), "broken")
            .and_then(|broken| assembler.evaluate(&broken))
            .is_err()
    );
    let _ = assembler.drain_reasoning();
    let runtime = assembler.evaluation_runtime();
    let resources = &runtime.state.shared_resources;
    let analysis = runtime.analysis().unwrap();
    let ends_with =
        |machine: &MachinePath, key: &str| machine.keys().last() == Some(&Key::atom_from_text(key));
    let failed = |key: &str| {
        analysis
            .failed_machines()
            .iter()
            .map(|failed| failed.path().clone())
            .find(|path| ends_with(path, key))
            .unwrap_or_else(|| panic!("'{key} is in error"))
    };
    let (f, e) = (failed("f"), failed("e"));
    let broken_root = analysis
        .failed_machines()
        .iter()
        .map(|failed| failed.path().clone())
        .find(|path| path.keys().len() == 2)
        .expect("the annotation task's root is in error");
    let owing = analysis.obligations()[0].acceptors()[0].0.clone();
    let waiting = analysis
        .waits()
        .iter()
        .filter_map(RuntimeWait::machine)
        .find(|machine| ends_with(machine, "w"))
        .cloned()
        .expect("'w waits");
    let values = assembler.values();

    let parent_root = analysis
        .waits()
        .iter()
        .filter_map(RuntimeWait::machine)
        .find(|machine| machine.keys().len() == 2)
        .cloned()
        .expect("the parent's annotation task waits");

    let refused = runtime.clear(&waiting).expect_err("a task waits there");
    assert!(
        refused.to_string().contains("interrupt it first"),
        "{refused}"
    );
    runtime
        .clear(&parent_root)
        .expect_err("an annotation task waits on its root");
    runtime.interrupt(&waiting, values.text("stop")).unwrap();
    for machine in [&waiting, &f, &e, &owing] {
        runtime
            .clear(machine)
            .unwrap_or_else(|error| panic!("{machine:?}: {error}"));
    }
    assert!(
        runtime
            .analysis()
            .unwrap()
            .failed_machines()
            .iter()
            .any(|failed| failed.path() == &broken_root),
        "a clear elsewhere leaves a failed root"
    );
    runtime.clear(&broken_root).unwrap();
    let dropped = assembler
        .get(module.value(), "later")
        .and_then(|later| assembler.evaluate(&later))
        .expect_err("the dropped task's reply is broken")
        .diagnostic(&values)
        .unwrap()
        .rendered_text_for_test(&values);
    assert!(dropped.contains("cleared before the task ran"), "{dropped}");
    // The probe's own root failed of it, and clears as any failed root.
    let probe = runtime
        .analysis()
        .unwrap()
        .failed_machines()
        .iter()
        .map(|failed| failed.path().clone())
        .find(|path| path.keys().len() == 2)
        .expect("the probe's root is in error");
    runtime.clear(&probe).unwrap();
    assembler
        .evaluate(&assembler.get(module.value(), "gate").unwrap())
        .unwrap();
    let _ = assembler.drain_reasoning();
    let heap = assembler.test_reflection_heap();
    let parent = assembler.get(&heap, "parent").unwrap();
    assert_eq!(
        assembler.to_binary(&parent).unwrap().as_ref(),
        b"idle",
        "the live parent finds its cleared child idle"
    );
    let analysis = runtime.analysis().unwrap();
    assert!(analysis.failed_machines().is_empty(), "nothing is in error");
    assert!(analysis.obligations().is_empty(), "nothing is owed");
    assert!(analysis.backlogs().is_empty(), "nothing waits unread");
    assert_eq!(resources.ended_roots_for_test(), 0, "no root waits");
    assert!(
        resources.anonymous_machines_for_test().is_empty(),
        "{:?}",
        resources.anonymous_machines_for_test()
    );
}

/// The anonymous root `path` lies under, and the keys below it.
fn below_root(path: &[Key]) -> usize {
    path.len().saturating_sub(2)
}

#[test]
fn accepted_promises_are_obligations_until_resolved() {
    // `obligations-are-accepted-promises`: a machine accepts a promise with
    // a diagnostic saying what it owes, a tracked read says whether any
    // machine did, and abandoning or a failed cut takes it back. A cut
    // reads its own acceptances, and one that read a promise unaccepted
    // conflicts with another's acceptance meanwhile, so servers checking
    // before accepting serialize. An acceptance whose promise another
    // resolved before it committed owes nothing. The
    // analysis reports each pending acceptance with all its acceptors; a
    // resolution discharges it, later or within the accepting transaction.
    let source = r#"language g0
import 'std
accepted = anno refl:(.promise.new >>= (\pr -> .heap.set ['r1] pr.resolver =>> .alt (.promise.accepted pr.resolver =>> .r "early") (.r "unaccepted") >>= (\before -> .promise.accept pr.resolver {msg:{text:"the root owes r1"}} =>> .alt (.promise.accepted pr.resolver =>> .r "accepted") (.r "unaccepted") >>= (\after -> .heap.set ['accepted] (if [before, after] == ["unaccepted", "accepted"] then "tracked" else "untracked"))))) ()
abandoned = anno refl:(.promise.new >>= (\pr -> .heap.set ['r2] pr.resolver =>> .promise.accept pr.resolver {msg:{text:"abandoned"}} =>> .promise.abandon pr.resolver =>> .alt (.promise.accepted pr.resolver =>> .r "accepted") (.r "abandoned") >>= (\a -> .heap.set ['abandoned] a))) ()
undone = anno refl:(.promise.new >>= (\pr -> .heap.set ['r3] pr.resolver =>> .alt (.cut (.promise.accept pr.resolver {msg:{text:"undone"}} =>> .fail)) (.r ()) =>> .alt (.promise.accepted pr.resolver =>> .r "accepted") (.r "undone") >>= (\a -> .heap.set ['undone] a))) ()
shared = anno refl:(.promise.new >>= (\pr -> .heap.set ['r4] pr.resolver =>> .promise.accept pr.resolver {msg:{text:"the root owes r4"}} =>> .child.task 'k (.promise.accept pr.resolver {msg:{text:"k owes r4"}}) >>= (\p -> seq p (.heap.set ['shared] "shared")))) ()
same = anno refl:(.promise.new >>= (\pr -> .cut (.promise.accept pr.resolver {msg:{text:"same"}} =>> .promise.fulfill pr.resolver 1) =>> .heap.set ['same] "resolved")) ()
viewed = anno refl:(.promise.new >>= (\pr -> .heap.set ['r5] pr.resolver =>> .cut (.promise.accept pr.resolver {msg:{text:"viewed"}} =>> .alt (.promise.accepted pr.resolver =>> .r "seen") (.r "unseen")) >>= (\v -> .heap.set ['viewed] v))) ()
raced = anno refl:(.promise.new >>= (\pr -> .heap.set ['r6] pr.resolver =>> .cut (.alt (.promise.accepted pr.resolver =>> .r "taken") (.TEST_ONLY.accept_now pr.resolver {msg:{text:"another took r6"}} =>> .promise.accept pr.resolver {msg:{text:"raced"}} =>> .r "served")) >>= (\v -> .heap.set ['raced] v))) ()
resolve = anno refl:(.heap.get ['r1] >>= (\r -> .promise.fulfill r 1) =>> .heap.set ['resolve] "resolved") ()
resolved_in_a_cut = anno refl:(.heap.get ['r4] >>= (\r -> .cut (.promise.fulfill r 1)) =>> .heap.set ['resolved_in_a_cut] "resolved") ()
stale = anno refl:(.promise.new >>= (\pr -> .cut (.promise.accept pr.resolver {msg:{text:"stale"}} =>> .TEST_ONLY.queue_now 'k (.promise.fulfill pr.resolver 1) >>= (\p -> seq p (.r ()))) =>> .heap.set ['stale] "resolved")) ()
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["accepted"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    let run = |names: &[&str]| {
        for name in names {
            assembler
                .evaluate(&assembler.get(module.value(), name).unwrap())
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        let _ = assembler.drain_reasoning();
    };
    run(&[
        "accepted",
        "abandoned",
        "undone",
        "shared",
        "same",
        "viewed",
        "raced",
        "stale",
    ]);
    let text = |name: &str| {
        let value = assembler
            .get(&assembler.test_reflection_heap(), name)
            .unwrap_or_else(|error| panic!("{name} wrote nothing: {error}"));
        String::from_utf8_lossy(&assembler.to_binary(&value).unwrap()).into_owned()
    };
    assert_eq!(
        [
            text("accepted"),
            text("abandoned"),
            text("undone"),
            text("same"),
            text("viewed"),
            text("raced"),
            text("stale"),
        ],
        [
            "tracked",
            "abandoned",
            "undone",
            "resolved",
            "seen",
            "taken",
            "resolved",
        ]
    );
    let owed = |assembler: &Assembler| {
        let analysis = assembler.evaluation_runtime().analysis().unwrap();
        let mut owed: Vec<Vec<(usize, String)>> = analysis
            .obligations()
            .iter()
            .map(|obligation| {
                obligation
                    .acceptors()
                    .iter()
                    .map(|(path, diagnostic)| {
                        (
                            below_root(path.keys()),
                            diagnostic.rendered_text_for_test(&assembler.values()),
                        )
                    })
                    .collect()
            })
            .collect();
        owed.sort();
        let families = analysis
            .issues()
            .iter()
            .filter(|issue| issue.family() == RuntimeIssueFamily::PendingObligation)
            .count();
        assert_eq!(families, owed.len());
        owed
    };
    assert_eq!(
        owed(&assembler),
        [
            vec![(0, "another took r6".to_owned())],
            vec![(0, "the root owes r1".to_owned())],
            vec![
                (0, "the root owes r4".to_owned()),
                (1, "k owes r4".to_owned())
            ],
            vec![(0, "viewed".to_owned())],
        ]
    );
    run(&["resolve", "resolved_in_a_cut"]);
    assert_eq!(text("resolve"), "resolved");
    assert_eq!(text("resolved_in_a_cut"), "resolved");
    assert_eq!(
        owed(&assembler),
        [
            vec![(0, "another took r6".to_owned())],
            vec![(0, "viewed".to_owned())],
        ],
        "a resolution, within a cut or not, discharges the promise for every acceptor"
    );
}

#[test]
fn clearing_abandons_what_a_machine_accepted_and_owing_keeps_it() {
    // Clearing a machine abandons what it accepted; a machine in error
    // keeps it, for recovery; and a machine that owes stays visible, as one
    // in error does, when its root's subtree is reclaimed.
    let source = r#"language g0
import 'std
cleared = anno refl:(.promise.new >>= (\pr -> .heap.set ['r1] pr.resolver =>> .child.task 'c (.promise.accept pr.resolver {msg:{text:"c owes"}}) >>= (\p -> seq p (.child.clear 'c =>> .heap.set ['cleared] "cleared")))) ()
flagged = anno refl:(.promise.new >>= (\pr -> .heap.set ['r2] pr.resolver =>> .child.task 'f (.promise.accept pr.resolver {msg:{text:"f owes"}} =>> (1 / 0)) >>= (\p -> .try (seq p (.r ())) >>= (\_ -> .heap.set ['flagged] "flagged")))) ()
owing = anno refl:(.promise.new >>= (\pr -> .heap.set ['r3] pr.resolver =>> .child.task 'o (.set ['n] 1 =>> .promise.accept pr.resolver {msg:{text:"o owes"}}) >>= (\p -> seq p (.child.task 'q (.set ['n] 2) >>= (\q -> seq q (.heap.set ['owing] "owing")))))) ()
"#;
    assert_eq!(
        run_reflection_texts(source, &["cleared", "flagged", "owing"]),
        ["cleared", "flagged", "owing"]
    );
    let assembler = Assembler::default();
    let module = assembler
        .module(["child_tasks"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    for name in ["cleared", "flagged", "owing"] {
        assembler
            .evaluate(&assembler.get(module.value(), name).unwrap())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    let _ = assembler.drain_reasoning();
    let analysis = assembler.evaluation_runtime().analysis().unwrap();
    let mut acceptors: Vec<String> = analysis
        .obligations()
        .iter()
        .flat_map(|obligation| obligation.acceptors())
        .map(|(_, diagnostic)| diagnostic.rendered_text_for_test(&assembler.values()))
        .collect();
    acceptors.sort();
    assert_eq!(acceptors, ["f owes", "o owes"]);
    let left = assembler
        .evaluation_runtime()
        .state
        .shared_resources
        .anonymous_machines_for_test();
    let last = |key: &str| {
        left.iter()
            .any(|path| path.last() == Some(&Key::atom_from_text(key)))
    };
    assert!(last("o"), "the owing child stays: {left:?}");
    assert!(last("f"), "the child in error stays: {left:?}");
    assert!(!last("q"), "its clear sibling goes: {left:?}");
}

#[test]
fn an_owing_machine_is_reclaimed_once_its_obligation_is_discharged() {
    // What is not clear stays visible until it is: a machine kept for
    // owing under an ended root goes once a resolution discharges what it
    // owes, whether the resolution commits alone or with a cut, while a
    // machine in error stays, owing or not.
    let source = r#"language g0
import 'std
alone = anno refl:(.promise.new >>= (\pr -> .heap.set ['r1] pr.resolver =>> .child.task 'a (.set ['n] 1 =>> .promise.accept pr.resolver {msg:{text:"a owes"}}) >>= (\p -> seq p (.heap.set ['alone] "owing")))) ()
in_a_cut = anno refl:(.promise.new >>= (\pr -> .heap.set ['r2] pr.resolver =>> .child.task 'c (.set ['n] 1 =>> .promise.accept pr.resolver {msg:{text:"c owes"}}) >>= (\p -> seq p (.heap.set ['in_a_cut] "owing")))) ()
flagged = anno refl:(.promise.new >>= (\pr -> .heap.set ['r3] pr.resolver =>> .child.task 'f (.promise.accept pr.resolver {msg:{text:"f owes"}} =>> (1 / 0)) >>= (\p -> .try (seq p (.r ())) >>= (\_ -> .heap.set ['flagged] "flagged")))) ()
resolve_in_a_cut = anno refl:(.heap.get ['r2] >>= (\r -> .cut (.promise.fulfill r 2)) =>> .heap.set ['resolve_in_a_cut] "resolved") ()
resolve_alone = anno refl:(.heap.get ['r1] >>= (\r -> .promise.fulfill r 1) =>> .heap.get ['r3] >>= (\r -> .promise.fulfill r 3) =>> .heap.set ['resolve_alone] "resolved") ()
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["reclaimed_once_clear"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    let run = |names: &[&str]| {
        for name in names {
            assembler
                .evaluate(&assembler.get(module.value(), name).unwrap())
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        let _ = assembler.drain_reasoning();
    };
    // The children left below anonymous roots.
    let left = || -> std::collections::BTreeSet<Key> {
        assembler
            .evaluation_runtime()
            .state
            .shared_resources
            .anonymous_machines_for_test()
            .into_iter()
            .filter(|path| path.len() > 2)
            .filter_map(|path| path.last().cloned())
            .collect()
    };
    let keys = |names: &[&str]| names.iter().map(Key::atom_from_text).collect();
    let ended = || {
        assembler
            .evaluation_runtime()
            .state
            .shared_resources
            .ended_roots_for_test()
    };
    run(&["alone", "in_a_cut", "flagged"]);
    assert_eq!(
        left(),
        keys(&["a", "c", "f"]),
        "owing and failed machines stay"
    );
    assert_eq!(ended(), 3, "each root waits for what it owes");
    run(&["resolve_in_a_cut"]);
    assert_eq!(left(), keys(&["a", "f"]), "a cut's resolution discharges");
    run(&["resolve_alone"]);
    let analysis = assembler.evaluation_runtime().analysis().unwrap();
    assert!(analysis.obligations().is_empty(), "every promise resolved");
    assert_eq!(left(), keys(&["f"]), "the machine in error stays");
    assert_eq!(ended(), 0, "nothing is owed, and no root waits");
}

#[test]
fn an_analysis_lists_queued_tasks_that_have_not_started() {
    // `quiescence-is-analyzed`: tasks queued behind a task waiting idle,
    // or on a child in error whose queue stopped, have not started: a
    // backlog, gone once they run or a recovery drops them.
    let source = r#"language g0
import 'std
behind = anno refl:(.child.task 'w (.cut (.heap.try_get ['w_gate])) >>= (\_ -> .child.task 'w (.r "second") >>= (\_ -> .heap.set ['behind] "queued"))) ()
stopped = anno refl:(.child.task 'f (1 / 0) >>= (\p -> .try (seq p (.r ())) >>= (\_ -> .child.task 'f (.r "never") >>= (\_ -> .heap.set ['stopped] "queued")))) ()
release = anno refl:(.heap.set ['w_gate] ()) ()
"#;
    let assembler = Assembler::default();
    let module = assembler
        .module(["backlogs"])
        .script("g", source)
        .build()
        .expect("the fixture should compile");
    let run = |name: &str| {
        assembler
            .evaluate(&assembler.get(module.value(), name).unwrap())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let _ = assembler.drain_reasoning();
    };
    let backlogs = || {
        let mut backlogs: Vec<_> = assembler
            .evaluation_runtime()
            .analysis()
            .unwrap()
            .backlogs()
            .iter()
            .map(|backlog| {
                assert!(backlog.port().is_none(), "a task queue's backlog");
                (backlog.machine().keys().last().cloned(), backlog.count())
            })
            .collect();
        backlogs.sort();
        backlogs
    };
    run("behind");
    run("stopped");
    assert_eq!(
        backlogs(),
        [
            (Some(Key::atom_from_text("f")), 1),
            (Some(Key::atom_from_text("w")), 1),
        ]
    );
    run("release");
    assert_eq!(
        backlogs(),
        [(Some(Key::atom_from_text("f")), 1)],
        "the waiting task's successor ran"
    );
}
