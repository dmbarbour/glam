use super::*;
use crate::api::{Assembler, TestValueFacade};

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
    PublicValue::from_core(&store.values, Value::binary_from_text(value))
}

fn integer(store: &ReflectionStore, value: i64) -> PublicValue {
    PublicValue::from_core(&store.values, Value::Number(value.into()))
}

fn empty(store: &ReflectionStore) -> PublicValue {
    PublicValue::from_core(&store.values, Value::Dict(Dict::new_sync()))
}

fn builtin(store: &ReflectionStore, value: Builtin) -> PublicValue {
    PublicValue::from_core(&store.values, Value::Builtin(value))
}

fn assert_list_values(assembler: &Assembler, actual: &PublicValue, expected: &PublicValue) {
    let actual = assembler.evaluate(actual).unwrap();
    let Value::List(actual) = actual.as_core() else {
        panic!("actual value should be a list")
    };
    let Value::List(expected) = expected.as_core() else {
        panic!("expected value should be a list")
    };
    assert_eq!(
        crate::eval::list_to_value_items(&assembler.eval_context(), actual).unwrap(),
        crate::eval::list_to_value_items(&assembler.eval_context(), expected).unwrap(),
    );
}

fn evaluate_query_state(assembler: &Assembler, value: PublicValue) -> Option<EvaluationQueryState> {
    let value = assembler.evaluate(&value).unwrap();
    decode_query_state(&assembler.core_values(), value.as_core())
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
            if value.as_binary() == Some(b"snapshot".as_slice())
    ));

    assert!(store.update_query(&handle, text(&store, "updated")));
    let EvaluationQueryPoll::State { value, .. } = store.snapshot().poll_query(&handle) else {
        panic!("updated query should remain available")
    };
    assert!(matches!(
        evaluate_query_state(&assembler, value),
        Some(EvaluationQueryState::Complete(value))
            if value.as_binary() == Some(b"updated".as_slice())
    ));

    let id = handle.id;
    drop(handle);
    let maintenance = StoreJournal::new(store.snapshot());
    assert_eq!(store.try_commit(&maintenance), StoreCommitResult::Committed);
    let root = store.roots.get(&store.runtime_volume).unwrap();
    let retired = PublicValue::from_core(
        &store.values,
        lazy_core_value_path(&store.values, root.as_core().clone(), &query_path(id)),
    );
    assert!(assembler.evaluate(&retired).unwrap().is_undefined());
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
    assert_eq!(journal.view(), cached_view);
    assert_eq!(store.try_commit(&journal), StoreCommitResult::Committed);
    assert_eq!(store.root(), &cached_view);
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
    assert_ne!(store.root(), &stale_cached_view);
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
    assert_eq!(
        store.volume_root(first),
        journal.volume_view(first).as_ref()
    );
    assert_eq!(
        store.volume_root(second),
        journal.volume_view(second).as_ref()
    );
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
    assert_eq!(store.volume_root(surviving), Some(&original_surviving));
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
