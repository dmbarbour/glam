//! PNC1 semantic-netlist conformance fixtures.

use std::sync::Arc;

use bytes::Bytes;

use crate::core::{Builtin, BuiltinCall, Dict, LazyValue, List, RuntimeValueAccess, Value};
use crate::evaluation::EvalContext;

use super::construction::{ConstructionBrand, ConstructionPortId};
use super::netlist::{
    encode_bind, encode_builder_state, encode_copy, encode_data, encode_empty_copy_for_test,
    encode_selected_netlist, encode_wire, interaction_net_from_netlist_in,
};

fn with_access<R>(
    context: &EvalContext,
    body: impl for<'access> FnOnce(&RuntimeValueAccess<'access>) -> R,
) -> R {
    context
        .values()
        .with_runtime_value_access(|access| body(&access))
}

fn port(id: u64) -> ConstructionPortId {
    ConstructionPortId::new(id).expect("fixture port IDs are positive")
}

fn selected(
    access: &RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
    next_port: u64,
    mut source_operations: Vec<Value>,
    exposed: ConstructionPortId,
) -> Value {
    source_operations.reverse();
    let state = encode_builder_state(
        access,
        brand,
        next_port,
        source_operations,
        Value::Dict(Dict::new_sync()),
    );
    encode_selected_netlist(access, state, brand, exposed)
}

fn valid_all_operations(access: &RuntimeValueAccess<'_>) -> Value {
    let brand = Arc::new(ConstructionBrand::default());
    selected(
        access,
        &brand,
        8,
        vec![
            encode_bind(access, &brand, [port(1), port(2), port(3)]),
            encode_data(access, &brand, port(4), access.values().unit()),
            encode_copy(access, &brand, &[port(5), port(6), port(7)]),
            encode_wire(access, &brand, port(1), port(4)),
            encode_wire(access, &brand, port(2), port(5)),
            encode_wire(access, &brand, port(3), port(6)),
        ],
        port(7),
    )
}

#[test]
fn hidden_builtin_replays_all_strict_operation_forms() {
    let context = EvalContext::standalone();
    let call = with_access(&context, |access| {
        Value::builtin_call_in(
            access,
            Builtin::InteractionNetFromNetlist,
            vec![valid_all_operations(access)],
        )
    });
    assert!(matches!(
        crate::eval::eval_value(&context, &call).expect("valid netlist should replay"),
        Value::Net(_)
    ));
}

#[test]
fn replay_rejects_malformed_nonsequential_foreign_and_invalid_records() {
    let context = EvalContext::standalone();
    with_access(&context, |access| {
        let brand = Arc::new(ConstructionBrand::default());
        let foreign = Arc::new(ConstructionBrand::default());
        let cases = [
            (
                Value::Dict(Dict::new_sync()),
                "selected netlist must be a strict list",
            ),
            (
                Value::List(crate::core::List::from_bytes(Bytes::from_static(b"record"))),
                "selected netlist contains a byte segment",
            ),
            (
                Value::List(crate::core::List::from_thunk(
                    LazyValue::error_in(access, "structural thunk must remain undemanded").into(),
                )),
                "selected netlist contains a deferred segment",
            ),
            (
                selected(
                    access,
                    &brand,
                    5,
                    vec![encode_bind(access, &brand, [port(1), port(3), port(4)])],
                    port(1),
                ),
                "nonsequential ports",
            ),
            (
                selected(
                    access,
                    &brand,
                    2,
                    vec![encode_data(
                        access,
                        &foreign,
                        port(1),
                        access.values().unit(),
                    )],
                    port(1),
                ),
                "belongs to another invocation",
            ),
            (
                selected(
                    access,
                    &brand,
                    4,
                    vec![encode_bind(access, &brand, [port(1), port(2), port(3)])],
                    port(1),
                ),
                "unwired",
            ),
            (
                selected(
                    access,
                    &brand,
                    1,
                    vec![encode_empty_copy_for_test(access)],
                    port(1),
                ),
                "allocate its input port",
            ),
        ];

        for (record, expected) in cases {
            let error = interaction_net_from_netlist_in(access, &record)
                .expect_err("malformed netlist must fail");
            assert!(
                error.to_string().contains(expected),
                "expected `{expected}` in `{error}`"
            );
        }
    });
}

#[test]
fn replay_copies_a_lazy_data_payload_without_demanding_it() {
    let context = EvalContext::standalone();
    with_access(&context, |access| {
        let brand = Arc::new(ConstructionBrand::default());
        let payload = Value::Lazy(LazyValue::error_in(
            access,
            "lazy data payload must remain undemanded",
        ));
        let record = selected(
            access,
            &brand,
            2,
            vec![encode_data(access, &brand, port(1), payload)],
            port(1),
        );
        assert!(matches!(
            interaction_net_from_netlist_in(access, &record)
                .expect("lazy data payload is valid net data"),
            Value::Net(_)
        ));
    });
}

fn partial_builder(
    _access: &RuntimeValueAccess<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Value {
    assert!(arguments.len() < builtin.arity());
    Value::PartialBuiltin(BuiltinCall {
        builtin,
        arguments: Arc::from(arguments),
    })
}

fn constant_builder(access: &RuntimeValueAccess<'_>, value: Value, state: Value) -> Value {
    crate::eval::test_support::closed_function_value_in(
        access.values(),
        1,
        crate::eval::test_support::TestExpr::Value(Value::List(List::from_values(vec![
            super::builder::outcome(access, value, state),
        ]))),
    )
}

fn constant_builder_continuation(access: &RuntimeValueAccess<'_>, builder: Value) -> Value {
    crate::eval::test_support::closed_function_value_in(
        access.values(),
        1,
        crate::eval::test_support::TestExpr::Value(builder),
    )
}

fn run_builder_at(
    context: &EvalContext,
    operation: Value,
    state: Value,
    index: usize,
) -> [Value; 2] {
    let results = Value::Lazy(LazyValue::from_application(
        context.values(),
        operation,
        Arc::from([state]),
    ));
    let selected = Value::builtin_call(
        context.values(),
        Builtin::ListAt,
        vec![Value::Number((index as i64).into()), results],
    );
    let outcome = crate::eval::eval_value(context, &selected)
        .expect("builder outcome should evaluate at the requested index");
    with_access(context, |access| {
        super::builder::decode_outcome(access, &outcome)
            .expect("builder outcome should use the strict record schema")
    })
}

fn builder_result_at(
    context: &EvalContext,
    operation: Value,
    state: Value,
    index: usize,
) -> Result<Value, crate::core::EvaluationHalt> {
    let results = Value::Lazy(LazyValue::from_application(
        context.values(),
        operation,
        Arc::from([state]),
    ));
    let selected = Value::builtin_call(
        context.values(),
        Builtin::ListAt,
        vec![Value::Number((index as i64).into()), results],
    );
    crate::eval::eval_value(context, &selected)
}

fn path(
    values: &crate::core::CoreValueFactory,
    keys: impl IntoIterator<Item = crate::core::Key>,
) -> Value {
    Value::List(List::from_values(
        keys.into_iter()
            .map(|key| key.to_value_with(values))
            .collect(),
    ))
}

#[test]
fn hidden_builder_composition_threads_branch_local_state_through_list_search() {
    let context = EvalContext::standalone();
    let (returned, initial) = with_access(&context, |access| {
        let initial = Value::Number(10.into());
        let returned = partial_builder(
            access,
            Builtin::InteractionNetBuilderReturn,
            vec![Value::binary_from_text("returned")],
        );
        (returned, initial)
    });
    assert_eq!(
        run_builder_at(&context, returned, initial.clone(), 0),
        [Value::binary_from_text("returned"), initial.clone()]
    );

    let choice = with_access(&context, |access| {
        let bad_state = Value::Number(99.into());
        let mutate_then_fail = partial_builder(
            access,
            Builtin::InteractionNetBuilderSeq,
            vec![
                constant_builder(access, access.values().unit(), bad_state),
                constant_builder_continuation(
                    access,
                    Value::Builtin(Builtin::InteractionNetBuilderFail),
                ),
            ],
        );
        let right = partial_builder(
            access,
            Builtin::InteractionNetBuilderReturn,
            vec![Value::binary_from_text("right")],
        );
        partial_builder(
            access,
            Builtin::InteractionNetBuilderAlt,
            vec![mutate_then_fail, right],
        )
    });
    assert_eq!(
        run_builder_at(&context, choice, initial.clone(), 0),
        [Value::binary_from_text("right"), initial.clone()],
        "a failed left alternative must not leak its branch state"
    );

    let (cut, selected_state) = with_access(&context, |access| {
        let selected_state = Value::Number(20.into());
        let discarded_state = Value::Number(30.into());
        let alternatives = partial_builder(
            access,
            Builtin::InteractionNetBuilderAlt,
            vec![
                constant_builder(
                    access,
                    Value::binary_from_text("selected"),
                    access.duplicate_value(&selected_state),
                ),
                constant_builder(
                    access,
                    Value::binary_from_text("discarded"),
                    discarded_state,
                ),
            ],
        );
        let cut = partial_builder(
            access,
            Builtin::InteractionNetBuilderCut,
            vec![alternatives],
        );
        (cut, selected_state)
    });
    assert_eq!(
        run_builder_at(&context, cut, initial, 0),
        [Value::binary_from_text("selected"), selected_state],
        "cut must retain the selected alternative's state"
    );
}

#[test]
fn hidden_builder_state_paths_preserve_control_and_whole_state_semantics() {
    let context = EvalContext::standalone();
    let visible = crate::core::Key::atom_from_text("visible");
    let (initial, get_all, set_visible, set_all, invalid_set) = with_access(&context, |access| {
        let brand = Arc::new(ConstructionBrand::default());
        let initial = encode_builder_state(
            access,
            &brand,
            1,
            Vec::new(),
            super::builder::initial_user_state(access),
        );
        let get_all = partial_builder(
            access,
            Builtin::InteractionNetBuilderGet,
            vec![path(access.values(), [])],
        );
        let set_visible = partial_builder(
            access,
            Builtin::InteractionNetBuilderSet,
            vec![
                path(access.values(), [visible.clone()]),
                Value::binary_from_text("kept"),
            ],
        );
        let set_all = partial_builder(
            access,
            Builtin::InteractionNetBuilderSet,
            vec![path(access.values(), []), Value::Dict(Dict::new_sync())],
        );
        let invalid_set = partial_builder(
            access,
            Builtin::InteractionNetBuilderSet,
            vec![path(access.values(), []), Value::Number(42.into())],
        );
        (initial, get_all, set_visible, set_all, invalid_set)
    });

    let [whole, unchanged] = run_builder_at(&context, get_all.clone(), initial.clone(), 0);
    let Value::Dict(whole) = whole else {
        panic!("whole-state get must return the user dictionary")
    };
    assert!(whole.get(&super::builder::control_key_for_test()).is_some());
    assert_eq!(unchanged, initial);

    let [_unit, nested_state] = run_builder_at(&context, set_visible, initial.clone(), 0);
    let [nested_whole, _] = run_builder_at(&context, get_all.clone(), nested_state, 0);
    let Value::Dict(nested_whole) = nested_whole else {
        panic!("nested state update must retain a dictionary")
    };
    assert_eq!(
        nested_whole.get(&visible),
        Some(&Value::binary_from_text("kept"))
    );
    assert!(
        nested_whole
            .get(&super::builder::control_key_for_test())
            .is_some(),
        "a nonempty user path must preserve hidden control state"
    );

    let invalid = builder_result_at(&context, invalid_set, initial.clone(), 0)
        .expect_err("whole-state replacement must remain a dictionary");
    assert!(invalid.to_string().contains("must be a dictionary"));

    let [_unit, replaced_state] = run_builder_at(&context, set_all, initial, 0);
    let [replacement, _] = run_builder_at(&context, get_all, replaced_state, 0);
    assert_eq!(replacement, Value::Dict(Dict::new_sync()));
}

#[test]
fn hidden_builder_get_resumes_lazy_paths_and_intermediates_and_rejects_invalid_ones() {
    let context = EvalContext::standalone();
    let outer = crate::core::Key::atom_from_text("outer");
    let inner = crate::core::Key::atom_from_text("inner");
    let source_path = path(context.values(), [outer.clone(), inner.clone()]);
    let lazy_path = Value::Lazy(LazyValue::semantic_thunk(
        context.values(),
        "lazy builder state path",
        move |_| Ok(source_path.clone()),
    ));
    let inner_dict =
        Value::Dict(Dict::new_sync().insert(inner.clone(), Value::binary_from_text("ready")));
    let lazy_inner = Value::Lazy(LazyValue::semantic_thunk(
        context.values(),
        "lazy builder state intermediate",
        move |_| Ok(inner_dict.clone()),
    ));
    let (state, get, get_missing) = with_access(&context, |access| {
        let Value::Dict(user_state) = super::builder::initial_user_state(access) else {
            unreachable!()
        };
        let state = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Value::Dict(user_state.insert(outer.clone(), lazy_inner)),
        );
        let get = partial_builder(access, Builtin::InteractionNetBuilderGet, vec![lazy_path]);
        let get_missing = partial_builder(
            access,
            Builtin::InteractionNetBuilderGet,
            vec![path(
                access.values(),
                [crate::core::Key::atom_from_text("missing")],
            )],
        );
        (state, get, get_missing)
    });
    assert_eq!(
        run_builder_at(&context, get, state.clone(), 0)[0],
        Value::binary_from_text("ready")
    );
    assert_eq!(
        run_builder_at(&context, get_missing, state, 0)[0],
        Value::Dict(Dict::new_sync())
    );

    let (invalid_state, invalid_get) = with_access(&context, |access| {
        let Value::Dict(user_state) = super::builder::initial_user_state(access) else {
            unreachable!()
        };
        let state = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Value::Dict(user_state.insert(outer.clone(), Value::Number(42.into()))),
        );
        let get = partial_builder(
            access,
            Builtin::InteractionNetBuilderGet,
            vec![path(access.values(), [outer, inner])],
        );
        (state, get)
    });
    let error = builder_result_at(&context, invalid_get, invalid_state, 0)
        .expect_err("a non-dictionary path intermediate must fail");
    assert!(error.to_string().contains("not a dictionary"), "{error}");
}
