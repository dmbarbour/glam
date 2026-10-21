use super::*;
use crate::api::{Assembler, TestValueFacade};

fn same_representation(store: &ReflectionStore, left: &PublicValue, right: &PublicValue) -> bool {
    let values = crate::api::Values::from_core_factory(store.values.clone());
    let left = values.clone_core(left).unwrap();
    let right = values.clone_core(right).unwrap();
    store
        .values
        .with_boundary_access(|access| access.same_representation(&left, &right))
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
    assembler.core_values().assert_same_representation_for_test(
        &crate::eval::list_to_value_items(&assembler.eval_context(), actual).unwrap(),
        &crate::eval::list_to_value_items(&assembler.eval_context(), expected).unwrap(),
    );
}

fn evaluate_query_state(assembler: &Assembler, value: PublicValue) -> Option<EvaluationQueryState> {
    let value = assembler.evaluator().eval(&value).unwrap();
    value
        .with_core_access(|value, access| decode_query_state(access, value))
        .expect("query state belongs to the fixture runtime")
}

/// Compile-exhaustive ownership latch for the durable reflection-store
/// state. Public values are compatibility roots; committed versions are
/// rooted ephemerons, which keep their values only while their volumes'
/// cells are reachable otherwise, and kept cells are roots that keep their
/// volumes (`api-volume-cells`); the promises hold each pending promise's
/// rooted owner until it resolves or its resolver dies, and the settled
/// resolutions hold them until their committer publishes; death notices,
/// query identity, conflict metadata, and revision state are edge-free
/// companions.
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
        environment_volume,
        next_volume,
        promises,
        kept,
        query_domain,
        versions,
        strategy,
        values,
    } = snapshot;
    let _: &Arc<()> = identity;
    let _: &u64 = revision;
    let _: &VolumeId = heap_volume;
    let _: &VolumeId = runtime_volume;
    let _: &VolumeId = environment_volume;
    let _: &Arc<AtomicU64> = next_volume;
    let _: &Arc<Promises> = promises;
    let _: &RedBlackTreeMapSync<VolumeId, ManagedVolumeRoot> = kept;
    let _: &Arc<QueryDomain> = query_domain;
    let _: &RedBlackTreeMapSync<VolumeId, ManagedVolumeVersion> = versions;
    let _: &Arc<dyn ConflictAnalysisStrategy> = strategy;
    let _: &CoreValueFactory = values;

    match edit {
        StoreEdit::Set { address, value } => {
            let _: &ConflictAddress = address;
            let _: &Option<PublicValue> = value;
        }
        StoreEdit::Rewrite { address, updater } => {
            let _: &ConflictAddress = address;
            let _: &Option<PublicValue> = updater;
        }
        StoreEdit::Create {
            address,
            initial,
            volume,
        } => {
            let _: &ConflictAddress = address;
            let _: &PublicValue = initial;
            let _: &ManagedVolumeRoot = volume;
        }
        StoreEdit::Resolve {
            address,
            promise,
            resolution,
        } => {
            let _: &ConflictAddress = address;
            let _: &PromiseId = promise;
            let _: &Resolution = resolution;
        }
    }

    let StoreJournal {
        snapshot,
        views,
        cells,
        observations,
        edits,
        branch,
    } = journal;
    let _: &StoreSnapshot = snapshot;
    let _: &RedBlackTreeMapSync<VolumeId, ManagedVolumeDraft> = views;
    let _: &RedBlackTreeMapSync<VolumeId, ManagedVolumeRoot> = cells;
    let _: &Arc<Mutex<Box<dyn ConflictObservationIndex>>> = observations;
    let _: &Vec<StoreEdit> = edits;
    if let Some(Branch { base, reads }) = branch {
        let _: &usize = base;
        let _: &Arc<Mutex<Vec<ConflictAddress>>> = reads;
    }

    let ReflectionStore {
        identity,
        heap_volume,
        runtime_volume,
        environment_volume,
        query_domain,
        query_retirements,
        next_volume,
        promises,
        death_notice_receiver,
        kept,
        revoked,
        settled,
        versions,
        pruned_at,
        revision,
        latest_changes,
        strategy,
        values,
    } = store;
    let _: &Arc<()> = identity;
    let _: &VolumeId = heap_volume;
    let _: &VolumeId = runtime_volume;
    let _: &VolumeId = environment_volume;
    let _: &Arc<QueryDomain> = query_domain;
    let _: &Receiver<EvaluationQueryId> = query_retirements;
    let _: &Arc<AtomicU64> = next_volume;
    let _: &Arc<Promises> = promises;
    let _: &Receiver<DeathNotice> = death_notice_receiver;
    let _: &RedBlackTreeMapSync<VolumeId, ManagedVolumeRoot> = kept;
    let _: &BTreeSet<VolumeId> = revoked;
    let _: &Vec<Settled> = settled;
    let _: &RedBlackTreeMapSync<VolumeId, ManagedVolumeVersion> = versions;
    let _: &u64 = pruned_at;
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
) -> (PublicValue, Weak<()>, Arc<std::sync::atomic::AtomicBool>) {
    let retained = Arc::new(());
    let weak = Arc::downgrade(&retained);
    let forced = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let forced_by_thunk = forced.clone();
    (
        PublicValue::from_runtime_root(values.construct_runtime_value_root(|access| {
            Value::Lazy(LazyValue::semantic_thunk_in(access, move |_| {
                let _ = &retained;
                forced_by_thunk.store(true, Ordering::Release);
                panic!("reflection-store root retention must not force its value")
            }))
        })),
        weak,
        forced,
    )
}

#[test]
fn snapshot_journal_edits_and_protected_volumes_retain_roots_without_forcing() {
    let mut store = store_with(crate::core::private_test_value_factory());
    let collector = store.values.clone();
    let (heap_root, heap_retained, heap_forced) = unforced_store_value(&store.values);
    let (replacement_root, replacement_retained, replacement_forced) =
        unforced_store_value(&store.values);
    let (volume_root, volume_retained, volume_forced) = unforced_store_value(&store.values);
    let (edit_root, edit_retained, edit_forced) = unforced_store_value(&store.values);
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
    let (result, retained, forced) = unforced_store_value(&store.values);
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
    let root = store.volume_root(store.runtime_volume).unwrap();
    let retired =
        crate::api::Value::from_runtime_root(store.values.construct_runtime_value_root(|access| {
            lazy_core_value_path(access, root.clone_core_in(access), &query_path(id))
        }));
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
    assert!(snapshot.committed(store.runtime_volume).is_some());
}

#[test]
fn journal_caches_its_view_and_uncontended_commit_installs_it() {
    let mut store = store();
    let mut journal = StoreJournal::new(store.snapshot());
    journal.write(path(&["value"]), integer(&store, 1));

    let cached_view = journal.view();
    assert!(same_representation(&store, &journal.view(), &cached_view));
    assert_eq!(store.try_commit(&journal), StoreCommitResult::Committed);
    assert!(same_representation(&store, &store.root(), &cached_view));
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
        &store.root(),
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
    journal.write_by_id(first, path(&["value"]), integer(&store, 1));
    journal.write_by_id(second, path(&["value"]), integer(&store, 2));

    assert_eq!(store.try_commit(&journal), StoreCommitResult::Committed);
    assert!(same_representation(
        &store,
        &store.volume_root(first).unwrap(),
        journal.volume_view(first).as_ref().unwrap()
    ));
    assert!(same_representation(
        &store,
        &store.volume_root(second).unwrap(),
        journal.volume_view(second).as_ref().unwrap()
    ));
}

#[test]
fn revoked_volume_rejects_staged_blind_edits_without_partial_commit() {
    let mut store = store();
    let initial = empty(&store);
    let revoked = store.create_volume(initial.clone()).unwrap();
    let surviving = store.create_volume(initial).unwrap();
    let original_surviving = store.volume_root(surviving).unwrap();
    let mut journal = StoreJournal::new(store.snapshot());
    journal.write_by_id(revoked, Vec::new(), integer(&store, 1));
    journal.write_by_id(surviving, Vec::new(), integer(&store, 2));
    assert!(store.revoke_volume(revoked).is_some());

    assert_eq!(
        store.try_commit(&journal),
        StoreCommitResult::MissingVolume(revoked)
    );
    assert!(same_representation(
        &store,
        &store.volume_root(surviving).unwrap(),
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
    assert!(journal.observe_kept_volume_read(volume, &[]));
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
    journal.write_by_id(volume, Vec::new(), integer(&store, 1));

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
fn forked_transaction_views_share_observations_but_not_speculative_edits() {
    let mut store = store();
    let snapshot = store.snapshot();
    let mut failed_alternative = StoreJournal::new(snapshot.clone());
    let winning_alternative = failed_alternative.clone();

    assert!(failed_alternative.observe_read(&path(&["observed"])));
    failed_alternative.write(path(&["discarded"]), text(&store, "bad"));
    assert!(same_representation(
        &store,
        &winning_alternative.view(),
        &empty(&store),
    ));

    let mut concurrent = StoreJournal::new(snapshot);
    concurrent.write(path(&["observed"]), text(&store, "changed"));
    assert_eq!(store.try_commit(&concurrent), StoreCommitResult::Committed);
    assert_eq!(
        store.try_commit(&winning_alternative),
        StoreCommitResult::Conflict,
        "a later alternative must retain reads performed by an earlier failed alternative",
    );
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
        assembler.evaluate(&store.root()).unwrap()
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
fn virtual_writes_change_no_volume_but_conflict_with_their_readers() {
    let mut store = store();
    let runtime_root = store.volume_root(store.runtime_volume).unwrap().clone();
    let heap_root = store.root().clone();
    let snapshot = store.snapshot();
    let mut reader = StoreJournal::new(snapshot.clone());
    reader.observe_virtual_read(&path(&["fs"]));
    let mut heap_reader = StoreJournal::new(snapshot.clone());
    heap_reader.observe_read(&[]);
    let mut first = StoreJournal::new(snapshot.clone());
    first.write_virtual(&path(&["fs", "a"]));
    let mut second = StoreJournal::new(snapshot);
    second.rewrite_virtual(&path(&["net"]));

    // The first installs its cached view; the second, from a stale
    // snapshot, replays its edits.
    for writer in [&first, &second] {
        assert_eq!(
            store.try_commit_with_change(writer),
            (StoreCommitResult::Committed, true),
            "a virtual write is a change"
        );
    }
    let runtime_volume = store.volume_root(store.runtime_volume).unwrap();
    assert!(same_representation(&store, &runtime_volume, &runtime_root));
    assert!(same_representation(&store, &store.root(), &heap_root));
    assert_eq!(store.try_commit(&heap_reader), StoreCommitResult::Committed);
    assert_eq!(store.try_commit(&reader), StoreCommitResult::Conflict);
}

#[test]
fn branches_merge_in_a_serial_order_or_not_at_all() {
    let store = store();
    let mut journal = StoreJournal::new(store.snapshot());
    journal.write(path(&["x"]), integer(&store, 1));
    let mut branch = journal.branch();
    assert!(
        !branch.observe_read(&path(&["x"])),
        "the transaction's write covers the read for its validation"
    );
    branch.write(path(&["y"]), integer(&store, 2));
    let mut sibling = journal.branch();
    sibling.write(path(&["z"]), integer(&store, 3));
    assert!(journal.merge_branch(&sibling), "the sibling read nothing");
    assert!(
        journal.merge_branch(&branch),
        "nothing the branch read changed after its fork"
    );
    assert_eq!(journal.edits.len(), 3, "both branches' edits replayed");

    let mut late = journal.branch();
    late.observe_read(&path(&["x"]));
    journal.write(path(&["x"]), integer(&store, 4));
    assert!(
        !journal.merge_branch(&late),
        "the transaction wrote what the branch read, after its fork"
    );
    assert_eq!(
        journal.edits.len(),
        4,
        "an inconsistent branch merges nothing"
    );

    let mut child = journal.branch();
    child.write(path(&["w"]), integer(&store, 5));
    let mut grandchild = child.branch();
    grandchild.observe_read(&path(&["w"]));
    assert!(child.merge_branch(&grandchild));
    journal.write(path(&["w"]), integer(&store, 6));
    assert!(
        journal.merge_branch(&child),
        "the child's own write covered what its branch read"
    );
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

/// Reads `volume` within `journal`'s transaction, through its cell, as a
/// machine's request does, rooting the result.
fn read_volume(journal: &mut StoreJournal, volume: VolumeId) -> Option<PublicValue> {
    journal.with_access(|journal, access| {
        let cell = journal.volume_cell_in(access, volume)?;
        let contents = journal.read_volume_in(access, cell)?;
        Some(PublicValue::from_runtime_root(
            access.root_runtime_value(contents_at_path(access, contents, &[])),
        ))
    })
}

/// A store whose value domain is its own, which tests may collect.
fn collectable_store() -> ReflectionStore {
    store_with(crate::core::private_test_value_factory())
}

fn collect(store: &ReflectionStore) {
    store
        .values
        .collect_managed_for_test()
        .expect("the store's heap collects");
}

/// Creates a volume through a committed journal, returning its ID and
/// handle (`api-volume-cells`).
fn glam_volume(store: &mut ReflectionStore, initial: &str) -> (VolumeId, PublicValue) {
    let mut creator = StoreJournal::new(store.snapshot());
    let handle = creator
        .create_volume(text(store, initial))
        .expect("a volume allocates");
    let volume = store.values.with_boundary_access(|access| {
        crate::reflection::handle_volume(&access, &handle.runtime_root().clone_core_with(&access))
            .expect("a volume's handle")
    });
    assert_eq!(store.try_commit(&creator), StoreCommitResult::Committed);
    (volume, handle)
}

fn reads(store: &ReflectionStore, volume: VolumeId) -> bool {
    store.snapshot().volume_value(volume, &[]).is_some()
}

#[test]
fn a_volume_reaching_its_own_handle_is_collected() {
    let mut store = collectable_store();
    let (volume, handle) = glam_volume(&mut store, "first");
    let mut writer = StoreJournal::new(store.snapshot());
    writer.write_by_id(volume, path(&["self"]), handle.clone());
    assert_eq!(store.try_commit(&writer), StoreCommitResult::Committed);
    drop(writer);

    collect(&store);
    assert!(reads(&store, volume), "its handle keeps it");
    drop(handle);
    collect(&store);
    assert!(
        !reads(&store, volume),
        "its contents reach its handle only through its version"
    );
}

#[test]
fn volumes_reaching_each_other_s_handles_are_collected_together() {
    let mut store = collectable_store();
    let (first, first_handle) = glam_volume(&mut store, "first");
    let (second, second_handle) = glam_volume(&mut store, "second");
    let mut writer = StoreJournal::new(store.snapshot());
    writer.write_by_id(first, path(&["other"]), second_handle.clone());
    writer.write_by_id(second, path(&["other"]), first_handle.clone());
    assert_eq!(store.try_commit(&writer), StoreCommitResult::Committed);
    drop(writer);

    drop(first_handle);
    collect(&store);
    assert!(
        reads(&store, first) && reads(&store, second),
        "the second's handle reaches the second, whose contents reach the first"
    );
    drop(second_handle);
    collect(&store);
    assert!(!reads(&store, first) && !reads(&store, second));
}

#[test]
fn a_transaction_writing_a_volume_keeps_it_until_it_commits() {
    let mut store = collectable_store();
    let (volume, handle) = glam_volume(&mut store, "first");
    let mut writer = StoreJournal::new(store.snapshot());
    writer.write_by_id(volume, Vec::new(), text(&store, "second"));
    drop(handle);

    collect(&store);
    assert!(reads(&store, volume), "the writer keeps the volume's cell");
    assert_eq!(store.try_commit(&writer), StoreCommitResult::Committed);
    let snapshot = store.snapshot();
    assert!(snapshot.volume_value(volume, &[]).is_some());
    drop(writer);
    drop(snapshot);
    collect(&store);
    assert!(!reads(&store, volume), "nothing keeps it once committed");
}

#[test]
fn a_transaction_reading_a_volume_keeps_it_until_it_ends() {
    let mut store = collectable_store();
    let (volume, handle) = glam_volume(&mut store, "first");
    let mut reader = StoreJournal::new(store.snapshot());
    assert!(read_volume(&mut reader, volume).is_some());
    drop(handle);

    collect(&store);
    assert!(reads(&store, volume), "the reader keeps the volume's cell");
    drop(reader);
    collect(&store);
    assert!(!reads(&store, volume));
}

#[test]
fn reads_root_only_their_results() {
    let mut store = collectable_store();
    let (volume, handle) = glam_volume(&mut store, "first");
    let registrations = || store.values.managed_root_registrations_for_test();

    let before = registrations();
    assert!(store.snapshot().volume_value(volume, &[]).is_some());
    assert_eq!(
        registrations() - before,
        1,
        "a read outside a transaction roots its result alone"
    );

    let mut reader = StoreJournal::new(store.snapshot());
    let before = registrations();
    assert!(read_volume(&mut reader, volume).is_some());
    assert_eq!(
        registrations() - before,
        2,
        "a transaction's first read keeps the volume's cell too"
    );
    let before = registrations();
    assert!(read_volume(&mut reader, volume).is_some());
    assert_eq!(registrations() - before, 1, "and only the first");
    drop(handle);
}

#[test]
fn a_snapshot_keeps_the_version_it_began_from() {
    let assembler = Assembler::default();
    let mut store = store_with(assembler.core_values());
    let (volume, handle) = glam_volume(&mut store, "first");
    let before = store.snapshot();
    let mut writer = StoreJournal::new(store.snapshot());
    writer.write_by_id(volume, Vec::new(), text(&store, "second"));
    assert_eq!(store.try_commit(&writer), StoreCommitResult::Committed);
    drop(writer);

    collect(&store);
    let read = |snapshot: &StoreSnapshot| {
        let value = snapshot.volume_value(volume, &[]).unwrap();
        assembler
            .evaluator()
            .eval(&value)
            .unwrap()
            .as_bytes()
            .unwrap()
    };
    assert_eq!(read(&before).as_deref(), Some(b"first".as_slice()));
    assert_eq!(
        read(&store.snapshot()).as_deref(),
        Some(b"second".as_slice())
    );
    drop(handle);
}

#[test]
fn the_store_keeps_its_own_volumes_and_a_host_s() {
    let mut store = collectable_store();
    let hosted = store
        .create_volume(text(&store, "hosted"))
        .expect("a volume allocates");
    collect(&store);
    for volume in [
        store.heap_volume,
        store.runtime_volume,
        store.environment_volume,
        hosted,
    ] {
        assert!(reads(&store, volume));
    }
    assert!(store.revoke_volume(hosted).is_some());
    assert!(!reads(&store, hosted));
}

#[test]
fn collected_volumes_are_pruned_as_a_volume_is_created_after_a_collection() {
    let mut store = collectable_store();
    let kept = store.volume_count();
    let (first, handle) = glam_volume(&mut store, "first");
    drop(handle);
    let (_, handle) = glam_volume(&mut store, "second");
    assert_eq!(
        store.volume_count(),
        kept + 2,
        "nothing collected, nothing pruned"
    );

    collect(&store);
    assert!(!reads(&store, first));
    assert_eq!(
        store.volume_count(),
        kept + 2,
        "a collection clears, and prunes nothing itself"
    );
    let (_, third) = glam_volume(&mut store, "third");
    assert_eq!(
        store.volume_count(),
        kept + 2,
        "creating the third pruned the first"
    );
    let (_, fourth) = glam_volume(&mut store, "fourth");
    assert_eq!(
        store.volume_count(),
        kept + 3,
        "no collection since the last prune, so none again"
    );
    drop((handle, third, fourth));
}

#[test]
fn a_revoked_volume_s_late_writes_stay_an_error() {
    let mut store = store();
    let revoked = store
        .create_volume(text(&store, "owned"))
        .expect("a volume allocates");
    let mut late = StoreJournal::new(store.snapshot());
    late.write_by_id(revoked, Vec::new(), text(&store, "late"));
    assert!(store.revoke_volume(revoked).is_some());
    assert_eq!(
        store.try_commit(&late),
        StoreCommitResult::MissingVolume(revoked),
        "a revoked volume's late writes stay an error"
    );
}

#[test]
fn of_two_transactions_resolving_one_promise_the_second_conflicts() {
    let mut store = store();
    let values = store.values.clone();
    let promise = values.with_boundary_access(|access| store.promises().allocate(&access).0);
    let public = crate::api::Values::from_core_factory(values.clone());
    let resolution = |value: &str| {
        Resolution::Fulfill(
            public
                .wrap(Value::binary_from_text(value))
                .into_runtime_root(),
        )
    };
    let mut first = StoreJournal::new(store.snapshot());
    let mut second = StoreJournal::new(store.snapshot());
    first
        .resolve(promise, resolution("first"))
        .expect("the promise is pending");
    assert!(
        first.resolve(promise, resolution("again")).is_err(),
        "a transaction resolves a promise once"
    );
    assert_eq!(first.staged_status(promise), Some(PromiseStatus::Fulfilled));
    second
        .resolve(promise, resolution("second"))
        .expect("the promise is still pending");

    assert_eq!(store.try_commit(&first), StoreCommitResult::Committed);
    assert_eq!(store.take_settled().len(), 1, "the commit settled it");
    assert_eq!(
        store.promises().status(promise),
        Some(PromiseStatus::Fulfilled)
    );
    assert_eq!(
        store.try_commit(&second),
        StoreCommitResult::Conflict,
        "the second resolution lost the race"
    );
    assert!(store.take_settled().is_empty());
    let mut late = StoreJournal::new(store.snapshot());
    assert!(
        late.resolve(promise, resolution("late")).is_err(),
        "running again, it finds the promise resolved"
    );
}
