//! PNC1 semantic-netlist conformance fixtures.

use std::sync::Arc;

use bytes::Bytes;

use crate::core::{Builtin, BuiltinCall, Dict, LazyValue, List, RuntimeValueAccess, Value};
use crate::evaluation::EvalContext;

use super::construction::{ConstructionBrand, ConstructionPortId, decode_construction_brand};
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
    mut source_constructors: Vec<Value>,
    mut source_wires: Vec<Value>,
    exposed: ConstructionPortId,
) -> Value {
    source_constructors.reverse();
    source_wires.reverse();
    let state = encode_builder_state(
        access,
        brand,
        next_port,
        source_constructors,
        source_wires,
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
            encode_bind(access),
            encode_data(access, access.values().unit()),
            encode_copy(access, 2),
        ],
        vec![
            encode_wire(port(1), port(4)),
            encode_wire(port(2), port(5)),
            encode_wire(port(3), port(6)),
        ],
        port(7),
    )
}

#[test]
fn initial_pure_builder_state_has_one_brand_and_empty_journals() {
    let context = EvalContext::standalone();
    with_access(&context, |access| {
        let brand = Arc::new(ConstructionBrand::default());
        let state = super::builder::initial_builder_state(access, &brand);
        let [
            encoded_brand,
            next_port,
            constructors,
            wires,
            user_state,
            sequence,
        ]: [Value; 6] = super::netlist::strict_record(access, &state, "initial builder state")
            .expect("initial builder state must be strict")
            .try_into()
            .expect("initial builder state must retain fixed arity");

        let Value::Opaque(encoded_brand) = encoded_brand else {
            panic!("initial builder brand must be opaque")
        };
        let decoded_brand = decode_construction_brand(access.values(), &encoded_brand)
            .expect("initial builder brand must decode");
        assert!(Arc::ptr_eq(&decoded_brand, &brand));
        assert!(
            matches!(next_port, Value::Number(number) if number.to_u64_if_integer() == Some(1))
        );
        for (name, journal) in [("constructor", constructors), ("wire", wires)] {
            assert!(
                super::netlist::strict_record(access, &journal, name)
                    .expect("initial journal must be strict")
                    .is_empty(),
                "initial {name} journal must be empty"
            );
        }
        assert!(matches!(user_state, Value::Dict(_)));
        assert!(
            super::netlist::strict_record(access, &sequence, "initial sequence")
                .expect("initial sequence must be strict")
                .is_empty()
        );
    });
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
fn builder_state_has_fixed_arity_and_terminal_replay_requires_an_empty_sequence() {
    let context = EvalContext::standalone();
    with_access(&context, |access| {
        let brand = Arc::new(ConstructionBrand::default());
        let state = encode_builder_state(
            access,
            &brand,
            1,
            Vec::new(),
            Vec::new(),
            Value::Dict(Dict::new_sync()),
        );
        let fields = super::netlist::strict_record(access, &state, "fixture builder state")
            .expect("encoded builder state must be strict");
        assert_eq!(fields.len(), 6, "builder state must have fixed arity");
        assert!(
            super::netlist::strict_record(access, &fields[5], "fixture sequence stack")
                .expect("the sequence stack must be strict")
                .is_empty(),
            "an inactive sequence must remain an explicit empty field"
        );

        let mut missing_sequence = fields.clone();
        missing_sequence.pop();
        let selected = encode_selected_netlist(
            access,
            Value::List(List::from_values(missing_sequence)),
            &brand,
            port(1),
        );
        let error = interaction_net_from_netlist_in(access, &selected)
            .expect_err("terminal replay must reject the old five-field shape");
        assert!(
            error
                .to_string()
                .contains("builder state has the wrong number of fields"),
            "{error}"
        );

        let mut active_sequence = fields;
        active_sequence[5] = Value::List(List::from_values(vec![access.values().unit()]));
        let selected = encode_selected_netlist(
            access,
            Value::List(List::from_values(active_sequence)),
            &brand,
            port(1),
        );
        let error = interaction_net_from_netlist_in(access, &selected)
            .expect_err("terminal replay must reject unfinished builder control");
        assert!(
            error
                .to_string()
                .contains("selected netlist retains an active builder sequence"),
            "{error}"
        );
    });
}

#[test]
fn replay_rejects_malformed_compact_records() {
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
                    vec![encode_bind(access)],
                    Vec::new(),
                    port(1),
                ),
                "next-port",
            ),
            (
                {
                    let state = encode_builder_state(
                        access,
                        &brand,
                        2,
                        vec![encode_data(access, access.values().unit())],
                        Vec::new(),
                        Value::Dict(Dict::new_sync()),
                    );
                    encode_selected_netlist(access, state, &foreign, port(1))
                },
                "belongs to another invocation",
            ),
            (
                selected(
                    access,
                    &brand,
                    4,
                    vec![encode_bind(access)],
                    Vec::new(),
                    port(1),
                ),
                "unwired",
            ),
            (
                selected(
                    access,
                    &brand,
                    4,
                    vec![encode_bind(access)],
                    vec![encode_wire(port(1), port(4))],
                    port(3),
                ),
                "out of range",
            ),
            (
                selected(
                    access,
                    &brand,
                    1,
                    vec![encode_empty_copy_for_test(access)],
                    Vec::new(),
                    port(1),
                ),
                "wrong number of fields",
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
            vec![encode_data(access, payload)],
            Vec::new(),
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

fn strict_fields(context: &EvalContext, value: &Value, name: &str) -> Vec<Value> {
    with_access(context, |access| {
        super::netlist::strict_record(access, value, name)
            .unwrap_or_else(|error| panic!("{name} must be strict: {error}"))
    })
}

fn duplicate(context: &EvalContext, value: &Value) -> Value {
    with_access(context, |access| access.duplicate_value(value))
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

fn apply_expr(
    function: crate::eval::test_support::TestExpr,
    argument: crate::eval::test_support::TestExpr,
) -> crate::eval::test_support::TestExpr {
    crate::eval::test_support::TestExpr::Apply(Arc::new(function), Arc::new(argument))
}

fn invoke_continuation_with(access: &RuntimeValueAccess<'_>, value: Value) -> Value {
    crate::eval::test_support::closed_function_value_in(
        access.values(),
        1,
        apply_expr(
            crate::eval::test_support::TestExpr::Local(0),
            crate::eval::test_support::TestExpr::Value(value),
        ),
    )
}

fn return_continuation(access: &RuntimeValueAccess<'_>) -> Value {
    crate::eval::test_support::closed_function_value_in(
        access.values(),
        1,
        apply_expr(
            crate::eval::test_support::TestExpr::Value(Value::Builtin(
                Builtin::InteractionNetBuilderReturn,
            )),
            crate::eval::test_support::TestExpr::Local(0),
        ),
    )
}

#[test]
fn hidden_builder_composition_threads_branch_local_state_through_list_search() {
    let context = EvalContext::standalone();
    let visible = crate::core::Key::atom_from_text("visible");
    let (returned, initial) = with_access(&context, |access| {
        let initial = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Vec::new(),
            super::builder::initial_user_state(access),
        );
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
        let mutate = partial_builder(
            access,
            Builtin::InteractionNetBuilderSet,
            vec![
                path(access.values(), [visible.clone()]),
                Value::Number(99.into()),
            ],
        );
        let mutate_then_fail = partial_builder(
            access,
            Builtin::InteractionNetBuilderSeq,
            vec![
                mutate,
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

    let cut = with_access(&context, |access| {
        let selected = partial_builder(
            access,
            Builtin::InteractionNetBuilderSeq,
            vec![
                partial_builder(
                    access,
                    Builtin::InteractionNetBuilderSet,
                    vec![
                        path(access.values(), [visible.clone()]),
                        Value::Number(20.into()),
                    ],
                ),
                constant_builder_continuation(
                    access,
                    partial_builder(
                        access,
                        Builtin::InteractionNetBuilderReturn,
                        vec![Value::binary_from_text("selected")],
                    ),
                ),
            ],
        );
        let discarded = partial_builder(
            access,
            Builtin::InteractionNetBuilderSeq,
            vec![
                partial_builder(
                    access,
                    Builtin::InteractionNetBuilderSet,
                    vec![
                        path(access.values(), [visible.clone()]),
                        Value::Number(30.into()),
                    ],
                ),
                constant_builder_continuation(
                    access,
                    partial_builder(
                        access,
                        Builtin::InteractionNetBuilderReturn,
                        vec![Value::binary_from_text("discarded")],
                    ),
                ),
            ],
        );
        let alternatives = partial_builder(
            access,
            Builtin::InteractionNetBuilderAlt,
            vec![selected, discarded],
        );
        partial_builder(
            access,
            Builtin::InteractionNetBuilderCut,
            vec![alternatives],
        )
    });
    let [selected, selected_state] = run_builder_at(&context, cut, initial, 0);
    assert_eq!(selected, Value::binary_from_text("selected"));
    let get_visible = with_access(&context, |access| {
        partial_builder(
            access,
            Builtin::InteractionNetBuilderGet,
            vec![path(access.values(), [visible])],
        )
    });
    assert_eq!(
        run_builder_at(&context, get_visible, selected_state, 0)[0],
        Value::Number(20.into()),
        "cut must retain the selected alternative's state"
    );
}

#[test]
fn hidden_builder_bind_and_data_append_compact_constructors_without_demanding_payloads() {
    let context = EvalContext::standalone();
    let (initial, data) = with_access(&context, |access| {
        let brand = Arc::new(ConstructionBrand::default());
        (
            super::builder::initial_builder_state(access, &brand),
            partial_builder(
                access,
                Builtin::InteractionNetBuilderData,
                vec![Value::Lazy(LazyValue::error_in(
                    access,
                    "pure builder data payload must remain undemanded",
                ))],
            ),
        )
    });

    let [bind_ports, after_bind] = run_builder_at(
        &context,
        Value::Builtin(Builtin::InteractionNetBuilderBind),
        initial,
        0,
    );
    assert_eq!(strict_fields(&context, &bind_ports, "bind result").len(), 3);
    let bind_state = strict_fields(&context, &after_bind, "bind state");
    assert!(
        matches!(&bind_state[1], Value::Number(number) if number.to_u64_if_integer() == Some(4))
    );
    assert_eq!(
        strict_fields(&context, &bind_state[2], "bind constructor journal").len(),
        1
    );
    assert!(strict_fields(&context, &bind_state[3], "bind wire journal").is_empty());

    let [data_ports, after_data] = run_builder_at(&context, data, after_bind, 0);
    assert_eq!(strict_fields(&context, &data_ports, "data result").len(), 1);
    let data_state = strict_fields(&context, &after_data, "data state");
    assert!(
        matches!(&data_state[1], Value::Number(number) if number.to_u64_if_integer() == Some(5))
    );
    assert_eq!(
        strict_fields(&context, &data_state[2], "data constructor journal").len(),
        2
    );
    assert!(strict_fields(&context, &data_state[3], "data wire journal").is_empty());
}

#[test]
fn hidden_builder_copy_and_wire_complete_one_replayable_compact_netlist() {
    let context = EvalContext::standalone();
    let initial = with_access(&context, |access| {
        super::builder::initial_builder_state(access, &Arc::new(ConstructionBrand::default()))
    });
    let [bind_ports, state] = run_builder_at(
        &context,
        Value::Builtin(Builtin::InteractionNetBuilderBind),
        initial,
        0,
    );
    let bind_ports = strict_fields(&context, &bind_ports, "bind ports");
    let data = with_access(&context, |access| {
        partial_builder(
            access,
            Builtin::InteractionNetBuilderData,
            vec![Value::Number(42.into())],
        )
    });
    let [data_ports, state] = run_builder_at(&context, data, state, 0);
    let data_ports = strict_fields(&context, &data_ports, "data ports");
    let copy = with_access(&context, |access| {
        partial_builder(
            access,
            Builtin::InteractionNetBuilderCopy,
            vec![Value::Number(2.into())],
        )
    });
    let [copy_ports, mut state] = run_builder_at(&context, copy, state, 0);
    let copy_ports = strict_fields(&context, &copy_ports, "copy ports");
    assert_eq!(copy_ports.len(), 3);

    for (left, right) in [
        (
            duplicate(&context, &bind_ports[0]),
            duplicate(&context, &data_ports[0]),
        ),
        (
            duplicate(&context, &bind_ports[1]),
            duplicate(&context, &copy_ports[0]),
        ),
        (
            duplicate(&context, &bind_ports[2]),
            duplicate(&context, &copy_ports[1]),
        ),
    ] {
        let wire = with_access(&context, |access| {
            partial_builder(
                access,
                Builtin::InteractionNetBuilderWire,
                vec![left, right],
            )
        });
        let [unit, next_state] = run_builder_at(&context, wire, state, 0);
        assert_eq!(unit, with_access(&context, |access| access.values().unit()));
        state = next_state;
    }

    let fields = strict_fields(&context, &state, "completed builder state");
    assert_eq!(
        strict_fields(&context, &fields[2], "constructor journal").len(),
        3
    );
    assert_eq!(strict_fields(&context, &fields[3], "wire journal").len(), 3);
    let selected = Value::List(List::from_values(vec![
        state,
        duplicate(&context, &copy_ports[2]),
    ]));
    assert!(matches!(
        with_access(&context, |access| interaction_net_from_netlist_in(
            access, &selected
        ))
        .expect("the pure builder must emit a replayable compact netlist"),
        Value::Net(_)
    ));
}

#[test]
fn hidden_builder_construction_rejects_invalid_counts_tokens_and_port_exhaustion() {
    let context = EvalContext::standalone();
    let (initial, exhausted, valid_port, foreign_port) = with_access(&context, |access| {
        let brand = Arc::new(ConstructionBrand::default());
        let foreign = Arc::new(ConstructionBrand::default());
        (
            super::builder::initial_builder_state(access, &brand),
            encode_builder_state(
                access,
                &brand,
                u64::MAX,
                Vec::new(),
                Vec::new(),
                super::builder::initial_user_state(access),
            ),
            super::construction::encode_construction_port(access, &brand, port(1)),
            super::construction::encode_construction_port(access, &foreign, port(1)),
        )
    });

    for (count, expected) in [
        (Value::binary_from_text("two"), "must be a number"),
        (Value::Number((-1).into()), "nonnegative integer"),
        (
            Value::Number(
                crate::number::Number::from_ratio_i64(1, 2)
                    .expect("fixture denominator is nonzero"),
            ),
            "nonnegative integer",
        ),
        (
            Value::Number(crate::number::Number::from_u64(u64::MAX)),
            "too large",
        ),
    ] {
        let copy = with_access(&context, |access| {
            partial_builder(access, Builtin::InteractionNetBuilderCopy, vec![count])
        });
        let error = builder_result_at(&context, copy, duplicate(&context, &initial), 0)
            .expect_err("invalid copy counts must fail before state publication");
        assert!(error.to_string().contains(expected), "{error}");
    }

    let error = builder_result_at(
        &context,
        Value::Builtin(Builtin::InteractionNetBuilderBind),
        exhausted,
        0,
    )
    .expect_err("port allocation must reject cursor exhaustion");
    assert!(error.to_string().contains("port IDs exhausted"), "{error}");

    for (left, expected) in [
        (Value::Number(1.into()), "requires a construction port"),
        (foreign_port, "belongs to another invocation"),
    ] {
        let wire = with_access(&context, |access| {
            partial_builder(
                access,
                Builtin::InteractionNetBuilderWire,
                vec![left, duplicate(&context, &valid_port)],
            )
        });
        let error = builder_result_at(&context, wire, duplicate(&context, &initial), 0)
            .expect_err("invalid wire tokens must fail before journal insertion");
        assert!(error.to_string().contains(expected), "{error}");
    }
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

#[test]
fn hidden_builder_reset_shift_handles_nested_keys_cut_and_missing_scope() {
    let context = EvalContext::standalone();
    let outer = Value::binary_from_text("outer");
    let inner = Value::binary_from_text("inner");
    let (state, nested, cut_then_shift, missing) = with_access(&context, |access| {
        let state = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Vec::new(),
            super::builder::initial_user_state(access),
        );
        let shift_outer = partial_builder(
            access,
            Builtin::InteractionNetBuilderShift,
            vec![
                outer.clone(),
                invoke_continuation_with(access, Value::binary_from_text("nested resumed")),
            ],
        );
        let reset_inner = partial_builder(
            access,
            Builtin::InteractionNetBuilderReset,
            vec![inner, shift_outer],
        );
        let nested = partial_builder(
            access,
            Builtin::InteractionNetBuilderReset,
            vec![outer.clone(), reset_inner],
        );

        let cut = partial_builder(
            access,
            Builtin::InteractionNetBuilderCut,
            vec![partial_builder(
                access,
                Builtin::InteractionNetBuilderReturn,
                vec![access.values().unit()],
            )],
        );
        let shift_after_cut = partial_builder(
            access,
            Builtin::InteractionNetBuilderShift,
            vec![
                outer.clone(),
                invoke_continuation_with(access, Value::binary_from_text("after cut")),
            ],
        );
        let cut_then_shift_body = partial_builder(
            access,
            Builtin::InteractionNetBuilderSeq,
            vec![cut, constant_builder_continuation(access, shift_after_cut)],
        );
        let cut_then_shift = partial_builder(
            access,
            Builtin::InteractionNetBuilderReset,
            vec![outer.clone(), cut_then_shift_body],
        );
        let missing = partial_builder(
            access,
            Builtin::InteractionNetBuilderShift,
            vec![
                outer,
                invoke_continuation_with(access, Value::binary_from_text("wrong")),
            ],
        );
        (state, nested, cut_then_shift, missing)
    });

    assert_eq!(
        run_builder_at(&context, nested, state.clone(), 0)[0],
        Value::binary_from_text("nested resumed")
    );
    assert_eq!(
        run_builder_at(&context, cut_then_shift, state.clone(), 0)[0],
        Value::binary_from_text("after cut")
    );
    let error =
        builder_result_at(&context, missing, state, 0).expect_err("shift outside reset must fail");
    assert!(error.to_string().contains("not in reset scope"), "{error}");
}

#[test]
fn hidden_builder_reset_shift_resumes_lazy_keys_and_captured_cut() {
    let context = EvalContext::standalone();
    let prompt = Value::binary_from_text("lazy prompt");
    let reset_key = {
        let prompt = prompt.clone();
        Value::Lazy(LazyValue::semantic_thunk(
            context.values(),
            "lazy reset key",
            move |_| Ok(prompt.clone()),
        ))
    };
    let shift_key = {
        let prompt = prompt.clone();
        Value::Lazy(LazyValue::semantic_thunk(
            context.values(),
            "lazy shift key",
            move |_| Ok(prompt.clone()),
        ))
    };
    let (state, capture) = with_access(&context, |access| {
        let state = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Vec::new(),
            super::builder::initial_user_state(access),
        );
        let shift = partial_builder(
            access,
            Builtin::InteractionNetBuilderShift,
            vec![shift_key, return_continuation(access)],
        );
        let cut = partial_builder(access, Builtin::InteractionNetBuilderCut, vec![shift]);
        let capture = partial_builder(
            access,
            Builtin::InteractionNetBuilderReset,
            vec![reset_key, cut],
        );
        (state, capture)
    });

    let [continuation, state] = run_builder_at(&context, capture, state, 0);
    let resumed = Value::Lazy(LazyValue::from_application(
        context.values(),
        continuation,
        Arc::from([Value::binary_from_text("captured cut resumed")]),
    ));
    assert_eq!(
        run_builder_at(&context, resumed, state, 0)[0],
        Value::binary_from_text("captured cut resumed")
    );
}

#[test]
fn hidden_builder_captured_continuation_is_reusable_only_with_its_invocation() {
    let context = EvalContext::standalone();
    let prompt = Value::binary_from_text("prompt");
    let (first_state, second_state, capture) = with_access(&context, |access| {
        let first_state = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Vec::new(),
            super::builder::initial_user_state(access),
        );
        let second_state = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Vec::new(),
            super::builder::initial_user_state(access),
        );
        let shift = partial_builder(
            access,
            Builtin::InteractionNetBuilderShift,
            vec![prompt.clone(), return_continuation(access)],
        );
        let capture = partial_builder(
            access,
            Builtin::InteractionNetBuilderReset,
            vec![prompt, shift],
        );
        (first_state, second_state, capture)
    });

    let [continuation, first_state] = run_builder_at(&context, capture, first_state, 0);
    let resumed = Value::Lazy(LazyValue::from_application(
        context.values(),
        continuation.clone(),
        Arc::from([Value::binary_from_text("same invocation")]),
    ));
    assert_eq!(
        run_builder_at(&context, resumed, first_state.clone(), 0)[0],
        Value::binary_from_text("same invocation")
    );

    let resumed_again = Value::Lazy(LazyValue::from_application(
        context.values(),
        continuation.clone(),
        Arc::from([Value::binary_from_text("same invocation again")]),
    ));
    assert_eq!(
        run_builder_at(&context, resumed_again, first_state, 0)[0],
        Value::binary_from_text("same invocation again"),
        "captured builder continuations are deliberately non-affine"
    );

    let foreign = Value::Lazy(LazyValue::from_application(
        context.values(),
        continuation,
        Arc::from([Value::binary_from_text("foreign")]),
    ));
    let error = builder_result_at(&context, foreign, second_state, 0)
        .expect_err("captured continuation must reject another builder invocation");
    assert!(
        error.to_string().contains("belongs to another invocation"),
        "{error}"
    );
}

#[test]
fn hidden_builder_reset_scope_is_branch_local_across_alternatives() {
    let context = EvalContext::standalone();
    let prompt = Value::binary_from_text("branch-local prompt");
    let (state, operation) = with_access(&context, |access| {
        let state = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Vec::new(),
            super::builder::initial_user_state(access),
        );
        let left = partial_builder(
            access,
            Builtin::InteractionNetBuilderShift,
            vec![
                prompt.clone(),
                constant_builder_continuation(
                    access,
                    Value::Builtin(Builtin::InteractionNetBuilderFail),
                ),
            ],
        );
        let right = partial_builder(
            access,
            Builtin::InteractionNetBuilderShift,
            vec![
                prompt.clone(),
                invoke_continuation_with(access, Value::binary_from_text("right retained reset")),
            ],
        );
        let alternatives =
            partial_builder(access, Builtin::InteractionNetBuilderAlt, vec![left, right]);
        let operation = partial_builder(
            access,
            Builtin::InteractionNetBuilderReset,
            vec![prompt, alternatives],
        );
        (state, operation)
    });

    assert_eq!(
        run_builder_at(&context, operation, state, 0)[0],
        Value::binary_from_text("right retained reset"),
        "a failed alternative must not leak its consumed reset frame into its sibling"
    );
}

#[test]
fn hidden_builder_whole_state_clear_does_not_erase_the_active_sequence() {
    let context = EvalContext::standalone();
    let prompt = Value::binary_from_text("prompt");
    let (state, operation) = with_access(&context, |access| {
        let state = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Vec::new(),
            super::builder::initial_user_state(access),
        );
        let clear = partial_builder(
            access,
            Builtin::InteractionNetBuilderSet,
            vec![path(access.values(), []), Value::Dict(Dict::new_sync())],
        );
        let shift = partial_builder(
            access,
            Builtin::InteractionNetBuilderShift,
            vec![
                prompt.clone(),
                invoke_continuation_with(access, Value::binary_from_text("wrong")),
            ],
        );
        let body = partial_builder(
            access,
            Builtin::InteractionNetBuilderSeq,
            vec![clear, constant_builder_continuation(access, shift)],
        );
        let operation = partial_builder(
            access,
            Builtin::InteractionNetBuilderReset,
            vec![prompt, body],
        );
        (state, operation)
    });
    let error = builder_result_at(&context, operation, state, 0)
        .expect_err("clearing whole state must clear reset scope but retain sequence execution");
    assert!(error.to_string().contains("not in reset scope"), "{error}");
}

#[test]
fn hidden_builder_whole_state_checkpoint_restores_reset_scope() {
    let context = EvalContext::standalone();
    let prompt = Value::binary_from_text("prompt");
    let (state, capture) = with_access(&context, |access| {
        let state = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Vec::new(),
            super::builder::initial_user_state(access),
        );
        let capture = partial_builder(
            access,
            Builtin::InteractionNetBuilderReset,
            vec![
                prompt.clone(),
                partial_builder(
                    access,
                    Builtin::InteractionNetBuilderGet,
                    vec![path(access.values(), [])],
                ),
            ],
        );
        (state, capture)
    });
    let [checkpoint, state] = run_builder_at(&context, capture, state, 0);

    let restore = with_access(&context, |access| {
        let clear = partial_builder(
            access,
            Builtin::InteractionNetBuilderSet,
            vec![path(access.values(), []), Value::Dict(Dict::new_sync())],
        );
        let shift = partial_builder(
            access,
            Builtin::InteractionNetBuilderShift,
            vec![
                prompt.clone(),
                invoke_continuation_with(access, Value::binary_from_text("restored checkpoint")),
            ],
        );
        let restore = partial_builder(
            access,
            Builtin::InteractionNetBuilderSeq,
            vec![
                partial_builder(
                    access,
                    Builtin::InteractionNetBuilderSet,
                    vec![path(access.values(), []), checkpoint],
                ),
                constant_builder_continuation(access, shift),
            ],
        );
        let clear_then_restore = partial_builder(
            access,
            Builtin::InteractionNetBuilderSeq,
            vec![clear, constant_builder_continuation(access, restore)],
        );
        partial_builder(
            access,
            Builtin::InteractionNetBuilderReset,
            vec![prompt, clear_then_restore],
        )
    });

    assert_eq!(
        run_builder_at(&context, restore, state, 0)[0],
        Value::binary_from_text("restored checkpoint")
    );
}

#[test]
fn hidden_builder_rejects_malformed_control_records() {
    let context = EvalContext::standalone();
    let (malformed_reset, malformed_reset_key, malformed_sequence, missing_sequence, returned) =
        with_access(&context, |access| {
            let brand = Arc::new(ConstructionBrand::default());
            let malformed_reset = encode_builder_state(
                access,
                &brand,
                1,
                Vec::new(),
                Vec::new(),
                Value::Dict(Dict::new_sync().insert(
                    super::builder::control_key_for_test(),
                    Value::Number(1.into()),
                )),
            );
            let malformed_reset_key = encode_builder_state(
                access,
                &brand,
                1,
                Vec::new(),
                Vec::new(),
                Value::Dict(Dict::new_sync().insert(
                    super::builder::control_key_for_test(),
                    Value::List(List::from_values(vec![Value::List(List::from_values(
                        vec![
                        access
                            .values()
                            .key_value(&super::builder::reset_tag_for_test()),
                        Value::Builtin(Builtin::InteractionNetBuilderReturn),
                        Value::List(List::empty()),
                    ],
                    ))])),
                )),
            );
            let initial = encode_builder_state(
                access,
                &brand,
                1,
                Vec::new(),
                Vec::new(),
                super::builder::initial_user_state(access),
            );
            let mut fields = super::netlist::strict_record(access, &initial, "fixture state")
                .expect("encoded builder state must be strict");
            assert_eq!(fields.len(), 6, "builder state must have fixed arity");
            let sequence = fields
                .pop()
                .expect("fixed builder state must retain its sequence stack");
            assert_eq!(
                super::netlist::strict_record(access, &sequence, "fixture sequence")
                    .expect("initial sequence stack must be strict"),
                Vec::<Value>::new(),
                "initial sequence stack must be represented explicitly"
            );
            let missing_sequence = Value::List(List::from_values(fields.clone()));
            fields.push(Value::Number(2.into()));
            let malformed_sequence = Value::List(List::from_values(fields));
            let returned = partial_builder(
                access,
                Builtin::InteractionNetBuilderReturn,
                vec![access.values().unit()],
            );
            (
                malformed_reset,
                malformed_reset_key,
                malformed_sequence,
                missing_sequence,
                returned,
            )
        });

    for (state, expected) in [
        (malformed_reset, "builder reset stack must be a strict list"),
        (malformed_reset_key, "builder reset frame key must be a key"),
        (
            malformed_sequence,
            "builder sequence stack must be a strict list",
        ),
        (
            missing_sequence,
            "builder state has the wrong number of fields",
        ),
    ] {
        let error = builder_result_at(&context, returned.clone(), state, 0)
            .expect_err("malformed hidden control state must fail at the evaluator boundary");
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn hidden_builder_fix_uses_independent_alternatives_and_restores_control() {
    let context = EvalContext::standalone();
    let prompt = Value::binary_from_text("fix prompt");
    let (state, alternatives, restored, hidden) = with_access(&context, |access| {
        let state = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Vec::new(),
            super::builder::initial_user_state(access),
        );
        let choices = partial_builder(
            access,
            Builtin::InteractionNetBuilderAlt,
            vec![
                partial_builder(
                    access,
                    Builtin::InteractionNetBuilderReturn,
                    vec![Value::Number(61.into())],
                ),
                partial_builder(
                    access,
                    Builtin::InteractionNetBuilderReturn,
                    vec![Value::Number(62.into())],
                ),
            ],
        );
        let alternatives = partial_builder(
            access,
            Builtin::InteractionNetBuilderFix,
            vec![constant_builder_continuation(access, choices)],
        );

        let fixed_unit = partial_builder(
            access,
            Builtin::InteractionNetBuilderFix,
            vec![constant_builder_continuation(
                access,
                partial_builder(
                    access,
                    Builtin::InteractionNetBuilderReturn,
                    vec![access.values().unit()],
                ),
            )],
        );
        let shift = partial_builder(
            access,
            Builtin::InteractionNetBuilderShift,
            vec![
                prompt.clone(),
                invoke_continuation_with(access, Value::binary_from_text("restored")),
            ],
        );
        let restored = partial_builder(
            access,
            Builtin::InteractionNetBuilderReset,
            vec![
                prompt.clone(),
                partial_builder(
                    access,
                    Builtin::InteractionNetBuilderSeq,
                    vec![fixed_unit, constant_builder_continuation(access, shift)],
                ),
            ],
        );

        let hidden_shift = partial_builder(
            access,
            Builtin::InteractionNetBuilderShift,
            vec![
                prompt.clone(),
                invoke_continuation_with(access, Value::binary_from_text("wrong")),
            ],
        );
        let hidden_fix = partial_builder(
            access,
            Builtin::InteractionNetBuilderFix,
            vec![constant_builder_continuation(access, hidden_shift)],
        );
        let hidden = partial_builder(
            access,
            Builtin::InteractionNetBuilderReset,
            vec![prompt, hidden_fix],
        );
        (state, alternatives, restored, hidden)
    });

    assert_eq!(
        run_builder_at(&context, alternatives.clone(), state.clone(), 0)[0],
        Value::Number(61.into())
    );
    assert_eq!(
        run_builder_at(&context, alternatives, state.clone(), 1)[0],
        Value::Number(62.into())
    );
    assert_eq!(
        run_builder_at(&context, restored, state.clone(), 0)[0],
        Value::binary_from_text("restored")
    );
    let error = builder_result_at(&context, hidden, state, 0)
        .expect_err("a builder fix body must not inherit its caller's reset scope");
    assert!(error.to_string().contains("not in reset scope"), "{error}");
}

#[test]
fn hidden_builder_fix_reports_recursive_future_observation() {
    let context = EvalContext::standalone();
    let (state, fixed) = with_access(&context, |access| {
        let state = encode_builder_state(
            access,
            &Arc::new(ConstructionBrand::default()),
            1,
            Vec::new(),
            Vec::new(),
            super::builder::initial_user_state(access),
        );
        let function = crate::eval::test_support::closed_function_value_in(
            access.values(),
            1,
            apply_expr(
                crate::eval::test_support::TestExpr::Value(Value::Builtin(
                    Builtin::InteractionNetBuilderReturn,
                )),
                crate::eval::test_support::TestExpr::Local(0),
            ),
        );
        let fixed = partial_builder(access, Builtin::InteractionNetBuilderFix, vec![function]);
        (state, fixed)
    });

    let [future, _state] = run_builder_at(&context, fixed, state, 0);
    let error = crate::eval::eval_value(&context, &future)
        .expect_err("strictly observing a fixpoint's own value must report a cycle");
    assert!(error.to_string().contains("cycle"), "{error}");
}
