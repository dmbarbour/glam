//! Volumes as first-class capabilities (`requests-travel-on-ports`): a
//! volume's handle is an opaque glam value naming one volume of one
//! runtime, whose holder may read and write it.

use std::sync::Arc;

use crate::core::{
    OpaquePayloadFamily, OpaquePayloadRecord, OpaqueValue, RuntimeValueAccess, Value,
};
use crate::runtime::EvaluationRuntimeId;

use super::VolumeId;

/// A handle's payload: which volume of which runtime it names.
struct VolumeHandle {
    runtime: EvaluationRuntimeId,
    volume: VolumeId,
}

// SAFETY: the payload holds a runtime's identity and a volume's. It contains
// no Glam value, runtime root, managed pointer, or active runtime capability:
// the volume's root stays in the runtime's store.
unsafe impl OpaquePayloadFamily for VolumeHandle {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::edge_free("volume handle", "src/reflection/volume_handle.rs");
}

/// The handle of `volume` in the runtime that `access` belongs to.
pub(crate) fn volume_handle_in(access: &RuntimeValueAccess<'_>, volume: VolumeId) -> Value {
    let values = access.values();
    Value::Opaque(OpaqueValue::new(
        values,
        Arc::new(VolumeHandle {
            runtime: values.runtime_id(),
            volume,
        }),
    ))
}

/// The volume that `value`, evaluated, names, where it is a handle of this
/// runtime's volume.
pub(crate) fn handle_volume(access: &RuntimeValueAccess<'_>, value: &Value) -> Option<VolumeId> {
    let Value::Opaque(opaque) = value else {
        return None;
    };
    let values = access.values();
    opaque
        .downcast::<VolumeHandle>(values)
        .filter(|handle| handle.runtime == values.runtime_id())
        .map(|handle| handle.volume)
}
