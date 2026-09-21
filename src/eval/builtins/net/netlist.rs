//! Strict semantic interaction-net records and callback-free checked replay.

use std::sync::{Arc, LazyLock};

use crate::core::{EvaluationHalt, List, NetValue, RuntimeValueAccess, Value};
use crate::core_net::CoreSpecialization;
use crate::interaction_net::{NetBuilder, Port};
use crate::list::LogicalListPart;
use crate::number::Number;

use super::identity::{
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

/// Encodes the protected builder state as one strict semantic record:
/// `[brand, next_port, reverse_constructors, reverse_wires, user_state,
/// sequence_stack]`.
pub(super) fn encode_builder_state(
    access: &RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
    next_port: u64,
    reverse_constructors: Vec<Value>,
    reverse_wires: Vec<Value>,
    user_state: Value,
) -> Value {
    Value::List(List::from_values(vec![
        encode_brand(access, brand),
        Value::Number(Number::from_u64(next_port)),
        Value::List(List::from_values(reverse_constructors)),
        Value::List(List::from_values(reverse_wires)),
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

pub(super) fn encode_bind(access: &RuntimeValueAccess<'_>) -> Value {
    access.values().key_value(&BIND_TAG)
}

pub(super) fn encode_copy(access: &RuntimeValueAccess<'_>, output_count: usize) -> Value {
    operation(
        access,
        &COPY_TAG,
        [Value::Number(Number::from_usize(output_count))],
    )
}

pub(super) fn encode_data(access: &RuntimeValueAccess<'_>, value: Value) -> Value {
    operation(access, &DATA_TAG, [value])
}

pub(super) fn encode_wire(
    _access: &RuntimeValueAccess<'_>,
    left: ConstructionPortId,
    right: ConstructionPortId,
) -> Value {
    Value::List(List::from_values(vec![
        Value::Number(Number::from_u64(left.get())),
        Value::Number(Number::from_u64(right.get())),
    ]))
}

#[cfg(test)]
pub(super) fn encode_empty_copy_for_test(access: &RuntimeValueAccess<'_>) -> Value {
    operation(access, &COPY_TAG, [])
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
        reverse_constructors,
        reverse_wires,
        user_state,
        sequence_stack,
    ]: [Value; 6] = exact_record(access, state, "builder state")?;

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

    let reverse_constructors = strict_record(access, &reverse_constructors, "constructor journal")?;
    let reverse_wires = strict_record(access, &reverse_wires, "wire journal")?;
    let capacity =
        usize::try_from(next_port - 1).map_err(|_| malformed("port count exceeds this target"))?;
    let mut mapped = Vec::new();
    mapped
        .try_reserve_exact(capacity)
        .map_err(|_| malformed("replay allocation is too large"))?;
    let mut builder = NetBuilder::<CoreSpecialization>::new();

    for constructor in reverse_constructors.iter().rev() {
        replay_constructor(access, constructor, capacity, &mut mapped, &mut builder)?;
    }
    if mapped.len() != capacity {
        return Err(malformed(
            "builder next-port does not match the constructor program",
        ));
    }

    for wire in reverse_wires.iter().rev() {
        replay_wire(access, wire, &mapped, &mut builder)?;
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

fn replay_constructor(
    access: &RuntimeValueAccess<'_>,
    constructor: &Value,
    capacity: usize,
    mapped: &mut Vec<Port>,
    builder: &mut NetBuilder<CoreSpecialization>,
) -> Result<(), EvaluationHalt> {
    if matches!(constructor, Value::Atom(tag) if tag.key() == &*BIND_TAG) {
        validate_constructor_capacity(mapped.len(), 3, capacity)?;
        mapped.extend(builder.bind());
        return Ok(());
    }

    let descriptor = strict_record(access, constructor, "constructor descriptor")?;
    let Some((tag, fields)) = descriptor.split_first() else {
        return Err(malformed("constructor descriptor is empty"));
    };
    let Value::Atom(tag) = tag else {
        return Err(malformed("constructor descriptor tag must be an atom"));
    };

    match tag.key() {
        key if key == &*COPY_TAG => {
            let [output_count]: [Value; 1] =
                exact_record(access, fields.to_vec(), "copy descriptor")?;
            let Value::Number(output_count) = output_count else {
                return Err(malformed("copy output count must be a number"));
            };
            let output_count = output_count
                .to_usize_if_integer()
                .ok_or_else(|| malformed("copy output count must be a nonnegative integer"))?;
            let port_count = output_count
                .checked_add(1)
                .ok_or_else(|| malformed("copy output count is too large"))?;
            validate_constructor_capacity(mapped.len(), port_count, capacity)?;
            let copy = builder.copy(output_count);
            mapped.extend(std::iter::once(copy.input).chain(copy.outputs));
            Ok(())
        }
        key if key == &*DATA_TAG => {
            let [value]: [Value; 1] = exact_record(access, fields.to_vec(), "data descriptor")?;
            validate_constructor_capacity(mapped.len(), 1, capacity)?;
            mapped.push(builder.data(access.duplicate_value(&value)));
            Ok(())
        }
        _ => Err(malformed("constructor descriptor tag is not recognized")),
    }
}

fn validate_constructor_capacity(
    allocated: usize,
    additional: usize,
    capacity: usize,
) -> Result<(), EvaluationHalt> {
    if allocated
        .checked_add(additional)
        .is_none_or(|next| next > capacity)
    {
        return Err(malformed(
            "constructor program allocates beyond builder next-port",
        ));
    }
    Ok(())
}

fn replay_wire(
    access: &RuntimeValueAccess<'_>,
    wire: &Value,
    mapped: &[Port],
    builder: &mut NetBuilder<CoreSpecialization>,
) -> Result<(), EvaluationHalt> {
    let wire = strict_record(access, wire, "wire pair")?;
    let [left, right]: [Value; 2] = exact_record(access, wire, "wire pair")?;
    let left = decode_wire_port_id(access, &left)?;
    let right = decode_wire_port_id(access, &right)?;
    builder
        .try_wire(mapped_port(mapped, left)?, mapped_port(mapped, right)?)
        .map_err(|error| malformed(error.to_string()))
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

fn decode_wire_port_id(
    _access: &RuntimeValueAccess<'_>,
    value: &Value,
) -> Result<ConstructionPortId, EvaluationHalt> {
    let Value::Number(id) = value else {
        return Err(malformed("wire port ID must be a number"));
    };
    let id = id
        .to_u64_if_integer()
        .and_then(ConstructionPortId::new)
        .ok_or_else(|| malformed("wire port ID must be a positive integer"))?;
    Ok(id)
}

fn mapped_port(mapped: &[Port], port: ConstructionPortId) -> Result<Port, EvaluationHalt> {
    mapped
        .get(port.index()?)
        .copied()
        .ok_or_else(|| malformed("wire or exposed port ID is out of range"))
}

fn malformed(message: impl Into<String>) -> EvaluationHalt {
    EvaluationHalt::new(format!(
        "malformed interaction-net semantic netlist: {}",
        message.into()
    ))
}
