//! Invocation-local identities for pure interaction-net construction.

use std::num::NonZeroU64;
use std::sync::Arc;

use crate::core::{
    CoreValueFactory, EvaluationHalt, OpaquePayloadFamily, OpaquePayloadRecord, OpaqueValue,
    RuntimeValueAccess, Value,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct ConstructionPortId(NonZeroU64);

impl ConstructionPortId {
    pub(super) fn new(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }

    pub(super) fn get(self) -> u64 {
        self.0.get()
    }

    pub(super) fn index(self) -> Result<usize, EvaluationHalt> {
        usize::try_from(self.0.get() - 1)
            .map_err(|_| EvaluationHalt::new("interaction-net port index exceeds this target"))
    }
}

#[derive(Default)]
pub(super) struct ConstructionBrand {
    #[cfg(test)]
    pub(super) probe: std::sync::OnceLock<Arc<super::construction::ConstructionProbe>>,
}

struct ConstructionToken {
    brand: Arc<ConstructionBrand>,
    port: Option<ConstructionPortId>,
}

#[cfg(test)]
pub(crate) fn assert_construction_port_family_shape() {
    fn inspect(token: &ConstructionToken) {
        let ConstructionToken { brand, port } = token;
        let _: &Arc<ConstructionBrand> = brand;
        let _: &Option<ConstructionPortId> = port;
    }

    let _: fn(&ConstructionToken) = inspect;
    assert_eq!(
        <ConstructionToken as OpaquePayloadFamily>::PAYLOAD_RECORD
            .fields()
            .2,
        "edge-free token"
    );
}

// SAFETY: a construction token contains only one construction-local brand and
// an optional scalar port ID. Neither field can reach a Glam value or root.
unsafe impl OpaquePayloadFamily for ConstructionToken {
    const PAYLOAD_RECORD: OpaquePayloadRecord = OpaquePayloadRecord::edge_free(
        "interaction-net construction token",
        "src/eval/builtins/net/identity.rs",
    );
}

pub(super) fn encode_construction_brand(
    access: &RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
) -> Value {
    Value::Opaque(construction_token(access.values(), brand, None))
}

pub(super) fn encode_construction_port(
    access: &RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
    id: ConstructionPortId,
) -> Value {
    Value::Opaque(construction_token(access.values(), brand, Some(id)))
}

pub(super) fn construction_token(
    values: &CoreValueFactory,
    brand: &Arc<ConstructionBrand>,
    port: Option<ConstructionPortId>,
) -> OpaqueValue {
    OpaqueValue::new(
        values,
        Arc::new(ConstructionToken {
            brand: Arc::clone(brand),
            port,
        }),
    )
}

pub(super) fn decode_construction_port(
    values: &CoreValueFactory,
    port: &OpaqueValue,
    brand: &Arc<ConstructionBrand>,
) -> Result<ConstructionPortId, EvaluationHalt> {
    let token = decode_construction_token(values, port).ok_or_else(|| {
        EvaluationHalt::new("interaction-net operation requires a construction port")
    })?;
    if !Arc::ptr_eq(&token.brand, brand) {
        return Err(EvaluationHalt::new(
            "interaction-net construction port belongs to another invocation",
        ));
    }
    token.port.ok_or_else(|| {
        EvaluationHalt::new("interaction-net operation requires a construction port")
    })
}

pub(super) fn decode_construction_brand(
    values: &CoreValueFactory,
    value: &OpaqueValue,
) -> Result<Arc<ConstructionBrand>, EvaluationHalt> {
    let token = decode_construction_token(values, value).ok_or_else(|| {
        EvaluationHalt::new("interaction-net builder brand must be a construction brand")
    })?;
    if token.port.is_some() {
        return Err(EvaluationHalt::new(
            "interaction-net builder brand must be a brand token",
        ));
    }
    Ok(Arc::clone(&token.brand))
}

fn decode_construction_token(
    values: &CoreValueFactory,
    token: &OpaqueValue,
) -> Option<Arc<ConstructionToken>> {
    token.downcast::<ConstructionToken>(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_ports_are_scoped_to_one_invocation() {
        assert_construction_port_family_shape();
        let values = crate::core::test_value_factory();
        let local = Arc::new(ConstructionBrand::default());
        let foreign = Arc::new(ConstructionBrand::default());
        let port = construction_token(&values, &foreign, Some(ConstructionPortId::new(1).unwrap()));
        let error = decode_construction_port(&values, &port, &local).unwrap_err();
        assert_eq!(
            error.to_string(),
            "interaction-net construction port belongs to another invocation"
        );
    }
}
