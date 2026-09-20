//! Strict semantic interaction-net records and callback-free checked replay.

use std::sync::{Arc, LazyLock};

use crate::core::{EvaluationHalt, List, NetValue, RuntimeValueAccess, Value};
use crate::core_net::CoreSpecialization;
use crate::interaction_net::{NetBuilder, Port};
use crate::list::LogicalListPart;
use crate::number::Number;

use super::construction::{
    ConstructionBrand, ConstructionPortId, decode_construction_brand, decode_construction_port,
    encode_construction_brand, encode_construction_port,
};

static BIND_TAG: LazyLock<crate::core::Key> = LazyLock::new(|| {
    crate::core::Key::abstract_global_path(["builtin", "interaction_net", "netlist", "bind"])
});
static COPY_TAG: LazyLock<crate::core::Key> = LazyLock::new(|| {
    crate::core::Key::abstract_global_path(["builtin", "interaction_net", "netlist", "copy"])
});
static DATA_TAG: LazyLock<crate::core::Key> = LazyLock::new(|| {
    crate::core::Key::abstract_global_path(["builtin", "interaction_net", "netlist", "data"])
});
static WIRE_TAG: LazyLock<crate::core::Key> = LazyLock::new(|| {
    crate::core::Key::abstract_global_path(["builtin", "interaction_net", "netlist", "wire"])
});

/// Encodes the protected builder state as one strict semantic record:
/// `[brand, next_port, reverse_operations, user_state, sequence_stack]`.
pub(super) fn encode_builder_state(
    access: &RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
    next_port: u64,
    reverse_operations: Vec<Value>,
    user_state: Value,
) -> Value {
    Value::List(List::from_values(vec![
        encode_brand(access, brand),
        Value::Number(Number::from_u64(next_port)),
        Value::List(List::from_values(reverse_operations)),
        user_state,
        Value::List(List::from_values(Vec::new())),
    ]))
}

/// Encodes the one selected result as `[builder_state, exposed_port]`.
pub(super) fn encode_selected_netlist(
    access: &RuntimeValueAccess<'_>,
    state: Value,
    brand: &Arc<ConstructionBrand>,
    exposed: ConstructionPortId,
) -> Value {
    Value::List(List::from_values(vec![
        state,
        encode_port(access, brand, exposed),
    ]))
}

pub(super) fn encode_bind(
    access: &RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
    ports: [ConstructionPortId; 3],
) -> Value {
    operation(
        access,
        &BIND_TAG,
        ports
            .into_iter()
            .map(|port| encode_port(access, brand, port)),
    )
}

pub(super) fn encode_copy(
    access: &RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
    ports: &[ConstructionPortId],
) -> Value {
    operation(
        access,
        &COPY_TAG,
        ports
            .iter()
            .copied()
            .map(|port| encode_port(access, brand, port)),
    )
}

pub(super) fn encode_data(
    access: &RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
    port: ConstructionPortId,
    value: Value,
) -> Value {
    operation(access, &DATA_TAG, [encode_port(access, brand, port), value])
}

pub(super) fn encode_wire(
    access: &RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
    left: ConstructionPortId,
    right: ConstructionPortId,
) -> Value {
    operation(
        access,
        &WIRE_TAG,
        [
            encode_port(access, brand, left),
            encode_port(access, brand, right),
        ],
    )
}

fn operation(
    access: &RuntimeValueAccess<'_>,
    tag: &crate::core::Key,
    fields: impl IntoIterator<Item = Value>,
) -> Value {
    Value::List(List::from_values(
        std::iter::once(access.values().key_value(tag))
            .chain(fields)
            .collect(),
    ))
}

#[cfg(test)]
pub(super) fn encode_empty_copy_for_test(access: &RuntimeValueAccess<'_>) -> Value {
    operation(access, &COPY_TAG, [])
}

fn encode_brand(access: &RuntimeValueAccess<'_>, brand: &Arc<ConstructionBrand>) -> Value {
    encode_construction_brand(access, brand)
}

fn encode_port(
    access: &RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
    id: ConstructionPortId,
) -> Value {
    encode_construction_port(access, brand, id)
}

/// Validates and replays one already-selected strict semantic netlist.
///
/// The caller owns the only value-access region. This operation never demands
/// a value: even a lazy `.data` payload is copied into the resulting net as an
/// ordinary semantic edge without being evaluated.
pub(in crate::eval) fn interaction_net_from_netlist_in(
    access: &RuntimeValueAccess<'_>,
    selected: &Value,
) -> Result<Value, EvaluationHalt> {
    let selected = strict_record(access, selected, "selected netlist")?;
    let [state, exposed]: [Value; 2] = exact_record(access, selected, "selected netlist")?;
    let state = strict_record(access, &state, "builder state")?;
    let [
        brand,
        next_port,
        reverse_operations,
        user_state,
        sequence_stack,
    ]: [Value; 5] = exact_record(access, state, "builder state")?;

    let brand = decode_brand(access, &brand)?;
    let Value::Number(next_port) = next_port else {
        return Err(malformed("builder next-port must be a number"));
    };
    let next_port = next_port
        .to_u64_if_integer()
        .filter(|next| *next != 0)
        .ok_or_else(|| malformed("builder next-port must be a positive integer"))?;
    if !matches!(user_state, Value::Dict(_)) {
        return Err(malformed("builder user state must be a dictionary"));
    }
    if !strict_record(access, &sequence_stack, "builder sequence stack")?.is_empty() {
        return Err(malformed(
            "selected netlist retains an active builder sequence",
        ));
    }

    let reverse_operations = strict_record(access, &reverse_operations, "operation journal")?;
    let capacity =
        usize::try_from(next_port - 1).map_err(|_| malformed("port count exceeds this target"))?;
    let mut mapped = Vec::new();
    mapped
        .try_reserve_exact(capacity)
        .map_err(|_| malformed("replay allocation is too large"))?;
    let mut builder = NetBuilder::<CoreSpecialization>::new();

    for operation in reverse_operations.iter().rev() {
        replay_operation(access, &brand, operation, &mut mapped, &mut builder)?;
    }
    if mapped.len() != capacity {
        return Err(malformed(
            "builder next-port does not follow the allocated port sequence",
        ));
    }

    let exposed = decode_port_value(access, &exposed, &brand)?;
    let exposed = mapped_port(&mapped, exposed)?;
    let template = builder
        .try_finish(exposed)
        .map_err(|error| malformed(error.to_string()))?;
    let runtime = access
        .construct_managed_core_net(template.instantiate())
        .expect("managed core-net representation must fit one collector run");
    Ok(Value::Net(NetValue::new(runtime)))
}

fn replay_operation(
    access: &RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
    operation: &Value,
    mapped: &mut Vec<Port>,
    builder: &mut NetBuilder<CoreSpecialization>,
) -> Result<(), EvaluationHalt> {
    let operation = strict_record(access, operation, "operation")?;
    let Some((tag, fields)) = operation.split_first() else {
        return Err(malformed("operation record is empty"));
    };
    let Value::Atom(tag) = tag else {
        return Err(malformed("operation tag must be an atom"));
    };

    match tag.key() {
        key if key == &*BIND_TAG => {
            let [first, second, third]: [Value; 3] =
                exact_record(access, fields.to_vec(), "bind operation")?;
            let logical = [first, second, third]
                .iter()
                .map(|port| decode_port_value(access, port, brand))
                .collect::<Result<Vec<_>, _>>()?;
            append_ports(mapped, logical, builder.bind())
        }
        key if key == &*COPY_TAG => {
            if fields.is_empty() {
                return Err(malformed("copy operation must allocate its input port"));
            }
            let logical = fields
                .iter()
                .map(|port| decode_port_value(access, port, brand))
                .collect::<Result<Vec<_>, _>>()?;
            let copy = builder.copy(logical.len() - 1);
            append_ports(
                mapped,
                logical,
                std::iter::once(copy.input).chain(copy.outputs),
            )
        }
        key if key == &*DATA_TAG => {
            let [port, value]: [Value; 2] =
                exact_record(access, fields.to_vec(), "data operation")?;
            let port = decode_port_value(access, &port, brand)?;
            append_ports(
                mapped,
                [port],
                [builder.data(access.duplicate_value(&value))],
            )
        }
        key if key == &*WIRE_TAG => {
            let [left, right]: [Value; 2] =
                exact_record(access, fields.to_vec(), "wire operation")?;
            let left = decode_port_value(access, &left, brand)?;
            let right = decode_port_value(access, &right, brand)?;
            builder
                .try_wire(mapped_port(mapped, left)?, mapped_port(mapped, right)?)
                .map_err(|error| malformed(error.to_string()))
        }
        _ => Err(malformed("operation tag is not recognized")),
    }
}

pub(super) fn strict_record(
    access: &RuntimeValueAccess<'_>,
    value: &Value,
    role: &str,
) -> Result<Vec<Value>, EvaluationHalt> {
    let Value::List(list) = value else {
        return Err(malformed(format!("{role} must be a strict list")));
    };
    let mut values = Vec::new();
    let mut invalid = None;
    list.visit_logical_parts(&mut |part| match part {
        LogicalListPart::Values(items) => {
            values.extend(items.iter().map(|value| access.duplicate_value(value)))
        }
        LogicalListPart::Bytes(_) => {
            invalid.get_or_insert("contains a byte segment");
        }
        LogicalListPart::Thunk(_) => {
            invalid.get_or_insert("contains a deferred segment");
        }
    });
    if let Some(reason) = invalid {
        return Err(malformed(format!("{role} {reason}")));
    }
    Ok(values)
}

fn exact_record<const N: usize>(
    _access: &RuntimeValueAccess<'_>,
    values: Vec<Value>,
    role: &str,
) -> Result<[Value; N], EvaluationHalt> {
    values
        .try_into()
        .map_err(|_| malformed(format!("{role} has the wrong number of fields")))
}

fn decode_brand(
    access: &RuntimeValueAccess<'_>,
    value: &Value,
) -> Result<Arc<ConstructionBrand>, EvaluationHalt> {
    let Value::Opaque(brand) = value else {
        return Err(malformed(
            "builder brand must be an opaque construction brand",
        ));
    };
    decode_construction_brand(access.values(), brand).map_err(|error| malformed(error.to_string()))
}

fn decode_port_value(
    access: &RuntimeValueAccess<'_>,
    value: &Value,
    brand: &Arc<ConstructionBrand>,
) -> Result<ConstructionPortId, EvaluationHalt> {
    let Value::Opaque(port) = value else {
        return Err(malformed("operation field must be a construction port"));
    };
    decode_construction_port(access.values(), port, brand)
        .map_err(|error| malformed(error.to_string()))
}

fn append_ports(
    mapped: &mut Vec<Port>,
    logical: impl IntoIterator<Item = ConstructionPortId>,
    actual: impl IntoIterator<Item = Port>,
) -> Result<(), EvaluationHalt> {
    let mut logical = logical.into_iter();
    let mut actual = actual.into_iter();
    loop {
        match (logical.next(), actual.next()) {
            (Some(logical), Some(actual)) => {
                if logical.index()? != mapped.len() {
                    return Err(malformed("operation journal has nonsequential ports"));
                }
                mapped.push(actual);
            }
            (None, None) => return Ok(()),
            _ => return Err(malformed("operation journal port arity mismatch")),
        }
    }
}

fn mapped_port(mapped: &[Port], port: ConstructionPortId) -> Result<Port, EvaluationHalt> {
    mapped
        .get(port.index()?)
        .copied()
        .ok_or_else(|| malformed("operation journal refers to an unknown port"))
}

fn malformed(message: impl Into<String>) -> EvaluationHalt {
    EvaluationHalt::new(format!(
        "malformed interaction-net semantic netlist: {}",
        message.into()
    ))
}
