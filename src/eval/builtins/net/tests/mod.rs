//! PNC1 semantic-netlist conformance fixtures.

use std::sync::Arc;

use bytes::Bytes;

use crate::core::{Builtin, Dict, LazyValue, RuntimeValueAccess, Value};
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
