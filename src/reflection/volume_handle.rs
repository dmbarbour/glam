//! Volumes as first-class capabilities (`requests-travel-on-ports`): a
//! volume's handle is an opaque glam value naming one volume of one
//! runtime, whose holder may read and write it.
//!
//! Each volume has one payload, which every copy of its handle shares, so
//! the payload dies with the last copy and reports it to the store, which
//! reclaims the volume (`opaque-resources-reclaimed-when-unreachable`).

use std::sync::Arc;
use std::sync::mpsc::Sender;

use crate::core::{
    OpaquePayloadFamily, OpaquePayloadRecord, OpaqueValue, RuntimeValueAccess, Value,
};
use crate::runtime::EvaluationRuntimeId;

use super::VolumeId;
use super::store::DeathNotice;

/// A handle's payload: which volume of which runtime it names, and where to
/// report the volume's death, unless it lives as long as its store.
pub(crate) struct VolumeHandle {
    runtime: EvaluationRuntimeId,
    volume: VolumeId,
    notices: Option<Sender<DeathNotice>>,
}

impl VolumeHandle {
    pub(super) fn new(
        runtime: EvaluationRuntimeId,
        volume: VolumeId,
        notices: Option<Sender<DeathNotice>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            volume,
            notices,
        })
    }
}

impl Drop for VolumeHandle {
    fn drop(&mut self) {
        if let Some(notices) = &self.notices {
            // A store already gone has nothing left to reclaim.
            let _ = notices.send(DeathNotice::Volume(self.volume));
        }
    }
}

// SAFETY: the payload holds a runtime's identity, a volume's, and a channel's
// sending end. It contains no Glam value, runtime root, managed pointer, or
// active runtime capability: the volume's root stays in the runtime's store,
// and its `Drop` only queues a notice for the store, which acts on it later,
// at a safe point.
unsafe impl OpaquePayloadFamily for VolumeHandle {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::external("volume handle", "src/reflection/volume_handle.rs");
}

/// A handle value for `payload`, sharing it with every other copy.
pub(super) fn handle_value(access: &RuntimeValueAccess<'_>, payload: &Arc<VolumeHandle>) -> Value {
    Value::Opaque(OpaqueValue::new(access.values(), payload.clone()))
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
