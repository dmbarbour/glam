use super::*;
use crate::api::{Assembler, TestValueFacade};

fn same_representation(store: &ReflectionStore, left: &PublicValue, right: &PublicValue) -> bool {
    let values = crate::api::Values::from_core_factory(store.values.clone());
    values.clone_core(left).unwrap() == values.clone_core(right).unwrap()
}

fn path(parts: &[&str]) -> Vec<Key> {
    parts.iter().map(Key::atom_from_text).collect()
}

fn store() -> ReflectionStore {
    ReflectionStore::new(
        crate::core::test_value_factory(),
        Arc::new(ExactConflictAnalysis),
    )
}

fn store_with(values: CoreValueFactory) -> ReflectionStore {
    ReflectionStore::new(values, Arc::new(ExactConflictAnalysis))
}

fn text(store: &ReflectionStore, value: &str) -> PublicValue {
    crate::api::Values::from_core_factory(store.values.clone()).wrap(Value::binary_from_text(value))
}

fn integer(store: &ReflectionStore, value: i64) -> PublicValue {
    crate::api::Values::from_core_factory(store.values.clone()).wrap(Value::Number(value.into()))
}

fn empty(store: &ReflectionStore) -> PublicValue {
    crate::api::Values::from_core_factory(store.values.clone()).wrap(Value::Dict(Dict::new_sync()))
}

fn builtin(store: &ReflectionStore, value: Builtin) -> PublicValue {
    crate::api::Values::from_core_factory(store.values.clone()).wrap(Value::Builtin(value))
}

fn assert_list_values(assembler: &Assembler, actual: &PublicValue, expected: &PublicValue) {
    let actual = assembler.evaluate(actual).unwrap();
    let actual = actual.clone_core_for_test();
    let Value::List(actual) = &actual else {
        panic!("actual value should be a list")
    };
    let expected = expected.clone_core_for_test();
    let Value::List(expected) = &expected else {
        panic!("expected value should be a list")
    };
    assert_eq!(
        crate::eval::list_to_value_items(&assembler.eval_context(), actual).unwrap(),
        crate::eval::list_to_value_items(&assembler.eval_context(), expected).unwrap(),
    );
}

fn evaluate_query_state(assembler: &Assembler, value: PublicValue) -> Option<EvaluationQueryState> {
    let value = assembler.evaluate(&value).unwrap();
    let values = assembler.values();
    let value = values.clone_core(&value).unwrap();
    decode_query_state(&values, &value)
}

/// Compile-exhaustive ownership latch for I4F.1d.1's durable reflection-store
/// state. Public values are compatibility roots; query identity, conflict
/// metadata, and revision state are edge-free companions.
fn assert_store_root_boundary_inventory(
    snapshot: &StoreSnapshot,
    edit: &StoreEdit,
    journal: &StoreJournal,
    store: &ReflectionStore,
) {
    let StoreSnapshot {
        identity,
        revision,
        heap_volume,
        runtime_volume,
        query_domain,
        roots,
        strategy,
        values,
    } = snapshot;
    let _: &Arc<()> = identity;
    let _: &u64 = revision;
    let _: &VolumeId = heap_volume;
    let _: &VolumeId = runtime_volume;
    let _: &Arc<QueryDomain> = query_domain;
    let _: &RedBlackTreeMapSync<VolumeId, PublicValue> = roots;
    let _: &Arc<dyn ConflictAnalysisStrategy> = strategy;
    let _: &CoreValueFactory = values;

    match edit {
        StoreEdit::Set { address, value } => {
            let _: &ConflictAddress = address;
            let _: &PublicValue = value;
        }
        StoreEdit::Rewrite { address, updater } => {
            let _: &ConflictAddress = address;
            let _: &PublicValue = updater;
        }
    }

    let StoreJournal {
        snapshot,
        views,
        observations,
        edits,
    } = journal;
    let _: &StoreSnapshot = snapshot;
    let _: &RedBlackTreeMapSync<VolumeId, PublicValue> = views;
    let _: &dyn ConflictObservationIndex = observations.as_ref();
    let _: &Vec<StoreEdit> = edits;

    let ReflectionStore {
        identity,
        heap_volume,
        runtime_volume,
        query_domain,
        query_retirements,
        next_volume,
        roots,
        revision,
        latest_changes,
        strategy,
        values,
    } = store;
    let _: &Arc<()> = identity;
    let _: &VolumeId = heap_volume;
    let _: &VolumeId = runtime_volume;
    let _: &Arc<QueryDomain> = query_domain;
    let _: &Receiver<EvaluationQueryId> = query_retirements;
    let _: &u64 = next_volume;
    let _: &RedBlackTreeMapSync<VolumeId, PublicValue> = roots;
    let _: &u64 = revision;
    let _: &BTreeMap<ConflictAddress, u64> = latest_changes;
    let _: &Arc<dyn ConflictAnalysisStrategy> = strategy;
    let _: &CoreValueFactory = values;
}

fn assert_query_root_boundary_inventory(
    query_poll: &EvaluationQueryPoll,
    query_state: &EvaluationQueryState,
    handle: &EvaluationQueryHandle,
    domain: &QueryDomain,
) {
    match query_poll {
        EvaluationQueryPoll::State { value, observed } => {
            let _: &PublicValue = value;
            let _: &bool = observed;
        }
        EvaluationQueryPoll::ForeignQueryDomain => {}
    }

    match query_state {
        EvaluationQueryState::Pending => {}
        EvaluationQueryState::Complete(value) => {
            let _: &PublicValue = value;
        }
    }

    let EvaluationQueryHandle {
        id,
        domain: handle_domain,
    } = handle;
    let _: &EvaluationQueryId = id;
    let _: &Weak<QueryDomain> = handle_domain;
    let QueryDomain { next_id, retired } = domain;
    let _: &AtomicU64 = next_id;
    let _: &Sender<EvaluationQueryId> = retired;
}

#[test]
fn store_root_boundary_inventory_is_complete() {
    let _: fn(&StoreSnapshot, &StoreEdit, &StoreJournal, &ReflectionStore) =
        assert_store_root_boundary_inventory;
    let _: fn(&EvaluationQueryPoll, &EvaluationQueryState, &EvaluationQueryHandle, &QueryDomain) =
        assert_query_root_boundary_inventory;
}

fn unforced_store_value(
    values: &CoreValueFactory,
    label: &'static str,
) -> (PublicValue, Weak<()>, Arc<std::sync::atomic::AtomicBool>) {
    let retained = Arc::new(());
    let weak = Arc::downgrade(&retained);
    let forced = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let forced_by_thunk = forced.clone();
    let value = Value::Lazy(LazyValue::semantic_thunk(values, label, move |_| {
        let _ = &retained;
        forced_by_thunk.store(true, Ordering::Release);
        panic!("reflection-store root retention must not force its value")
    }));
    (
        crate::api::Values::from_core_factory(values.clone()).wrap(value),
        weak,
        forced,
    )
}

#[test]
fn snapshot_journal_edits_and_protected_volumes_retain_roots_without_forcing() {
    let mut store = store();
    let collector = store.values.clone();
    let (heap_root, heap_retained, heap_forced) =
        unforced_store_value(&store.values, "snapshot heap root");
    let (replacement_root, replacement_retained, replacement_forced) =
        unforced_store_value(&store.values, "store replacement root");
    let (volume_root, volume_retained, volume_forced) =
        unforced_store_value(&store.values, "snapshot protected volume");
    let (edit_root, edit_retained, edit_forced) =
        unforced_store_value(&store.values, "journal edit root");
    store.replace_root(heap_root);
    let volume = store
        .create_volume(volume_root)
        .expect("the protected volume should be created");
    let snapshot = store.snapshot();
    let mut journal = StoreJournal::new(snapshot.clone());
    journal.write(path(&["edit"]), edit_root);
    store.replace_root(replacement_root);

    collector.collect_and_drain_external_owners_for_test();
    assert_eq!(
        journal.view().runtime_id(),
        journal.snapshot.values.runtime_id()
    );
    assert_eq!(
        journal
            .volume_view(volume)
            .expect("the journal should retain the protected volume")
            .runtime_id(),
        journal.snapshot.values.runtime_id()
    );
    for (retained, forced) in [
        (&heap_retained, &heap_forced),
        (&replacement_retained, &replacement_forced),
        (&volume_retained, &volume_forced),
        (&edit_retained, &edit_forced),
    ] {
        assert!(retained.upgrade().is_some());
        assert!(!forced.load(Ordering::Acquire));
    }

    drop(journal);
    collector.collect_and_drain_external_owners_for_test();
    assert!(edit_retained.upgrade().is_none());
    assert!(heap_retained.upgrade().is_some());
    assert!(volume_retained.upgrade().is_some());
    assert!(replacement_retained.upgrade().is_some());

    drop(store);
    collector.collect_and_drain_external_owners_for_test();
    assert!(replacement_retained.upgrade().is_none());
    assert!(heap_retained.upgrade().is_some());
    assert!(volume_retained.upgrade().is_some());

    drop(snapshot);
    collector.collect_and_drain_external_owners_for_test();
    assert!(heap_retained.upgrade().is_none());
    assert!(volume_retained.upgrade().is_none());
}

#[test]
fn query_result_remains_rooted_after_store_and_handle_retirement() {
    let assembler = Assembler::default();
    let mut store = store_with(assembler.core_values());
    let (result, retained, forced) = unforced_store_value(&store.values, "retained query result");
    let mut reservation = StoreJournal::new(store.snapshot());
    let handle = reservation
        .reserve_query_with(result)
        .expect("the query should reserve");
    assert_eq!(store.try_commit(&reservation), StoreCommitResult::Committed);
    let EvaluationQueryPoll::State { value, observed } = store.snapshot().poll_query(&handle)
    else {
        panic!("the committed query should remain in its query domain")
    };
    assert!(observed);
    let EvaluationQueryState::Complete(result) =
        evaluate_query_state(&assembler, value).expect("the query state should decode")
    else {
        panic!("the query should be complete")
    };

    drop(handle);
    drop(reservation);
    drop(store);
    assembler
        .core_values()
        .collect_and_drain_external_owners_for_test();
    assert!(retained.upgrade().is_some());
    assert!(!forced.load(Ordering::Acquire));
    assert_eq!(result.runtime_id(), assembler.values().runtime_id());

    drop(result);
    assembler
        .core_values()
        .collect_and_drain_external_owners_for_test();
    assert!(retained.upgrade().is_none());
}

#[test]
fn query_state_is_transactional_and_retired_after_the_last_handle() {
    let assembler = Assembler::default();
    let mut store = store_with(assembler.core_values());
    let mut reservation = StoreJournal::new(store.snapshot());
    let handle = reservation.reserve_query().unwrap();
    assert!(matches!(
        reservation.poll_query(&handle),
        EvaluationQueryPoll::State {
            observed: false,
            ..
        }
    ));
    assert_eq!(store.try_commit(&reservation), StoreCommitResult::Committed);

    let EvaluationQueryPoll::State { value, observed } = store.snapshot().poll_query(&handle)
    else {
        panic!("committed query should belong to its store")
    };
    assert!(observed);
    assert!(matches!(
        evaluate_query_state(&assembler, value),
        Some(EvaluationQueryState::Pending)
    ));

    assert!(store.update_query(&handle, text(&store, "snapshot")));
    let EvaluationQueryPoll::State { value, .. } = store.snapshot().poll_query(&handle) else {
        panic!("completed query should remain available")
    };
    assert!(matches!(
        evaluate_query_state(&assembler, value),
        Some(EvaluationQueryState::Complete(value))
            if assembler.evaluator().eval(&value).unwrap().as_bytes().unwrap().as_deref()
                == Some(b"snapshot".as_slice())
    ));

    assert!(store.update_query(&handle, text(&store, "updated")));
    let EvaluationQueryPoll::State { value, .. } = store.snapshot().poll_query(&handle) else {
        panic!("updated query should remain available")
    };
    assert!(matches!(
        evaluate_query_state(&assembler, value),
        Some(EvaluationQueryState::Complete(value))
            if assembler.evaluator().eval(&value).unwrap().as_bytes().unwrap().as_deref()
                == Some(b"updated".as_slice())
    ));

    let id = handle.id;
    drop(handle);
    let maintenance = StoreJournal::new(store.snapshot());
    assert_eq!(store.try_commit(&maintenance), StoreCommitResult::Committed);
    let root = store.roots.get(&store.runtime_volume).unwrap();
    let retired = crate::api::Values::from_core_factory(store.values.clone()).wrap(
        lazy_core_value_path(&store.values, root.clone_core_for_test(), &query_path(id)),
    );
    let retired = assembler.evaluate(&retired).unwrap();
    assert!(
        assembler
            .reflection()
            .same_representation(&retired, &assembler.values().empty_dict())
            .unwrap()
    );
}

#[test]
fn runtime_coordination_volume_is_not_client_revocable() {
    let mut store = store();
    assert!(store.revoke_volume(store.runtime_volume).is_none());

    let snapshot = store.snapshot();
    assert!(snapshot.volume(store.runtime_volume).is_some());
}

#[test]
fn journal_caches_its_view_and_uncontended_commit_installs_it() {
    let mut store = store();
    let mut journal = StoreJournal::new(store.snapshot());
    journal.write(path(&["value"]), integer(&store, 1));

    let cached_view = journal.view();
    assert!(same_representation(&store, &journal.view(), &cached_view));
    assert_eq!(store.try_commit(&journal), StoreCommitResult::Committed);
    assert!(same_representation(&store, store.root(), &cached_view));
}

#[test]
fn concurrent_commit_rebases_instead_of_installing_cached_view() {
    let mut store = store();
    let snapshot = store.snapshot();
    let mut first = StoreJournal::new(snapshot.clone());
    first.write(path(&["first"]), integer(&store, 1));
    let mut second = StoreJournal::new(snapshot);
    second.write(path(&["second"]), integer(&store, 2));
    let stale_cached_view = second.view();

    assert_eq!(store.try_commit(&first), StoreCommitResult::Committed);
    assert_eq!(store.try_commit(&second), StoreCommitResult::Committed);
    assert!(!same_representation(
        &store,
        store.root(),
        &stale_cached_view
    ));
}

#[test]
fn one_journal_updates_multiple_volumes_atomically() {
    let mut store = store();
    let initial = empty(&store);
    let first = store.create_volume(initial.clone()).unwrap();
    let second = store.create_volume(initial).unwrap();
    let mut journal = StoreJournal::new(store.snapshot());
    journal.write_volume(first, path(&["value"]), integer(&store, 1));
    journal.write_volume(second, path(&["value"]), integer(&store, 2));

    assert_eq!(store.try_commit(&journal), StoreCommitResult::Committed);
    assert!(same_representation(
        &store,
        store.volume_root(first).unwrap(),
        journal.volume_view(first).as_ref().unwrap()
    ));
    assert!(same_representation(
        &store,
        store.volume_root(second).unwrap(),
        journal.volume_view(second).as_ref().unwrap()
    ));
}

#[test]
fn revoked_volume_rejects_staged_blind_edits_without_partial_commit() {
    let mut store = store();
    let initial = empty(&store);
    let revoked = store.create_volume(initial.clone()).unwrap();
    let surviving = store.create_volume(initial).unwrap();
    let original_surviving = store.volume_root(surviving).cloned().unwrap();
    let mut journal = StoreJournal::new(store.snapshot());
    journal.write_volume(revoked, Vec::new(), integer(&store, 1));
    journal.write_volume(surviving, Vec::new(), integer(&store, 2));
    assert!(store.revoke_volume(revoked).is_some());

    assert_eq!(
        store.try_commit(&journal),
        StoreCommitResult::MissingVolume(revoked)
    );
    assert!(same_representation(
        &store,
        store.volume_root(surviving).unwrap(),
        &original_surviving
    ));
    assert!(store.volume_root(revoked).is_none());
}

#[test]
fn revoked_volume_conflicts_with_an_earlier_read() {
    let mut store = store();
    let initial = empty(&store);
    let volume = store.create_volume(initial).unwrap();
    let mut journal = StoreJournal::new(store.snapshot());
    assert!(journal.observe_volume_read(volume, &[]));
    assert!(store.revoke_volume(volume).is_some());

    assert_eq!(store.try_commit(&journal), StoreCommitResult::Conflict);
}

#[test]
fn writes_never_materialize_a_missing_volume() {
    let mut store = store();
    let initial = empty(&store);
    let volume = store.create_volume(initial).unwrap();
    assert!(store.revoke_volume(volume).is_some());
    let mut journal = StoreJournal::new(store.snapshot());
    journal.write_volume(volume, Vec::new(), integer(&store, 1));

    assert!(journal.volume_view(volume).is_none());
    assert_eq!(
        store.try_commit(&journal),
        StoreCommitResult::MissingVolume(volume)
    );
    assert!(store.volume_root(volume).is_none());
}

#[test]
fn revoked_volume_ids_are_not_reused() {
    let mut store = store();
    let initial = empty(&store);
    let first = store.create_volume(initial.clone()).unwrap();
    assert!(store.revoke_volume(first).is_some());
    let second = store.create_volume(initial).unwrap();

    assert_ne!(first, second);
}

#[test]
fn covering_set_keeps_a_later_rewrite_read_internal() {
    let mut store = store();
    let snapshot = store.snapshot();
    let mut local = StoreJournal::new(snapshot.clone());
    local.write(path(&["x"]), empty(&store));
    local.rewrite(path(&["x", "y"]), builtin(&store, Builtin::Seq));
    assert!(!local.observe_read(&path(&["x", "y", "z"])));

    let mut concurrent = StoreJournal::new(snapshot);
    concurrent.write(path(&["x", "other"]), integer(&store, 1));
    assert_eq!(store.try_commit(&concurrent), StoreCommitResult::Committed);
    assert_eq!(store.try_commit(&local), StoreCommitResult::Committed);
}

#[test]
fn rewrite_widens_a_descendant_read_dependency() {
    let mut store = store();
    let snapshot = store.snapshot();
    let mut local = StoreJournal::new(snapshot.clone());
    local.rewrite(path(&["x", "y"]), builtin(&store, Builtin::Seq));
    assert!(local.observe_read(&path(&["x", "y", "z"])));

    let mut concurrent = StoreJournal::new(snapshot);
    concurrent.write(path(&["x", "y", "sibling"]), integer(&store, 1));
    assert_eq!(store.try_commit(&concurrent), StoreCommitResult::Committed);
    assert_eq!(store.try_commit(&local), StoreCommitResult::Conflict);
}

#[test]
fn rebased_rewrites_apply_in_commit_order() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["reflection_store_test"])
        .script(
            "g",
            "language g0\nappend_a = \\items -> items ++ [\"A\"]\nappend_b = \\items -> items ++ [\"B\"]\n",
        )
        .build()
        .expect("rewrite fixture should compile");
    let append_a = assembler
        .evaluate(&assembler.get(module.value(), "append_a").unwrap())
        .unwrap();
    let append_b = assembler
        .evaluate(&assembler.get(module.value(), "append_b").unwrap())
        .unwrap();

    let apply_in_order = |first: PublicValue, second: PublicValue| {
        let mut store = store_with(assembler.core_values());
        store.replace_root(
            assembler
                .values()
                .list([assembler.values().text("base")])
                .unwrap(),
        );
        let snapshot = store.snapshot();
        let mut first_edit = StoreJournal::new(snapshot.clone());
        first_edit.rewrite(Vec::new(), first);
        let mut second_edit = StoreJournal::new(snapshot);
        second_edit.rewrite(Vec::new(), second);
        assert_eq!(store.try_commit(&first_edit), StoreCommitResult::Committed);
        assert_eq!(store.try_commit(&second_edit), StoreCommitResult::Committed);
        assembler.evaluate(store.root()).unwrap()
    };

    assert_list_values(
        &assembler,
        &apply_in_order(append_a.clone(), append_b.clone()),
        &assembler
            .values()
            .list([
                assembler.values().text("base"),
                assembler.values().text("A"),
                assembler.values().text("B"),
            ])
            .unwrap(),
    );
    assert_list_values(
        &assembler,
        &apply_in_order(append_b, append_a),
        &assembler
            .values()
            .list([
                assembler.values().text("base"),
                assembler.values().text("B"),
                assembler.values().text("A"),
            ])
            .unwrap(),
    );
}

#[test]
fn disjoint_writes_rebase_and_exact_blind_writes_replace() {
    let mut store = store();
    let snapshot = store.snapshot();
    let mut left = StoreJournal::new(snapshot.clone());
    left.write(path(&["left"]), integer(&store, 1));
    let mut right = StoreJournal::new(snapshot.clone());
    right.write(path(&["right"]), integer(&store, 2));
    let mut later_left = StoreJournal::new(snapshot);
    later_left.write(path(&["left"]), integer(&store, 3));

    assert_eq!(store.try_commit(&left), StoreCommitResult::Committed);
    assert_eq!(store.try_commit(&right), StoreCommitResult::Committed);
    assert_eq!(store.try_commit(&later_left), StoreCommitResult::Committed);
}

#[test]
fn disjoint_nested_siblings_rebase() {
    let mut store = store();
    let mut establish_parent = StoreJournal::new(store.snapshot());
    establish_parent.write(path(&["tree"]), empty(&store));
    assert_eq!(
        store.try_commit(&establish_parent),
        StoreCommitResult::Committed
    );

    let snapshot = store.snapshot();
    let mut left = StoreJournal::new(snapshot.clone());
    left.write(path(&["tree", "left"]), integer(&store, 1));
    let mut right = StoreJournal::new(snapshot);
    right.write(path(&["tree", "right"]), integer(&store, 2));

    assert_eq!(store.try_commit(&left), StoreCommitResult::Committed);
    assert_eq!(store.try_commit(&right), StoreCommitResult::Committed);
}

#[test]
fn root_observation_conflicts_with_every_write() {
    let mut store = store();
    let snapshot = store.snapshot();
    let mut reader = StoreJournal::new(snapshot.clone());
    reader.observe_read(&[]);
    let mut writer = StoreJournal::new(snapshot);
    writer.write(path(&["anywhere"]), integer(&store, 1));

    assert_eq!(store.try_commit(&writer), StoreCommitResult::Committed);
    assert_eq!(store.try_commit(&reader), StoreCommitResult::Conflict);
}

#[test]
fn reads_conflict_while_nested_blind_writes_serialize() {
    let mut store = store();
    let snapshot = store.snapshot();
    let mut reader = StoreJournal::new(snapshot.clone());
    reader.observe_read(&path(&["missing", "child"]));
    let mut nested_writer = StoreJournal::new(snapshot.clone());
    nested_writer.write(path(&["tree", "child"]), integer(&store, 1));
    let mut parent_writer = StoreJournal::new(snapshot.clone());
    parent_writer.write(path(&["tree"]), empty(&store));
    let mut missing_writer = StoreJournal::new(snapshot);
    missing_writer.write(path(&["missing"]), empty(&store));

    assert_eq!(
        store.try_commit(&nested_writer),
        StoreCommitResult::Committed
    );
    assert_eq!(
        store.try_commit(&parent_writer),
        StoreCommitResult::Committed
    );
    assert_eq!(
        store.try_commit(&missing_writer),
        StoreCommitResult::Committed
    );
    assert_eq!(store.try_commit(&reader), StoreCommitResult::Conflict);
}

#[test]
fn overlapping_blind_writes_serialize_in_commit_order() {
    let mut store = store();
    let snapshot = store.snapshot();
    let mut child = StoreJournal::new(snapshot.clone());
    child.write(path(&["tree", "child"]), integer(&store, 1));
    let mut parent = StoreJournal::new(snapshot);
    parent.write(path(&["tree"]), integer(&store, 2));

    assert_eq!(store.try_commit(&child), StoreCommitResult::Committed);
    assert_eq!(store.try_commit(&parent), StoreCommitResult::Committed);
}

#[test]
fn reads_after_covering_writes_do_not_observe_the_snapshot() {
    let mut store = store();
    let snapshot = store.snapshot();
    let mut local = StoreJournal::new(snapshot.clone());
    local.write(path(&["value"]), integer(&store, 1));
    assert!(!local.observe_read(&path(&["value"])));

    let mut concurrent = StoreJournal::new(snapshot);
    concurrent.write(path(&["value"]), integer(&store, 2));
    assert_eq!(store.try_commit(&concurrent), StoreCommitResult::Committed);
    assert_eq!(store.try_commit(&local), StoreCommitResult::Committed);
}

#[test]
fn writes_do_not_erase_earlier_read_dependencies() {
    let mut store = store();
    let snapshot = store.snapshot();
    let mut local = StoreJournal::new(snapshot.clone());
    assert!(local.observe_read(&path(&["value"])));
    local.write(path(&["value"]), integer(&store, 1));

    let mut concurrent = StoreJournal::new(snapshot);
    concurrent.write(path(&["value"]), integer(&store, 2));
    assert_eq!(store.try_commit(&concurrent), StoreCommitResult::Committed);
    assert_eq!(store.try_commit(&local), StoreCommitResult::Conflict);
}
