//! Volumes as first-class capabilities (`requests-travel-on-ports`): a
//! volume's handle is a glam value naming one volume, whose holder may read
//! and write it. It is one edge to the volume's managed cell, which keeps the
//! volume (`api-volume-cells`).

use crate::core::{RuntimeValueAccess, Value};

use super::VolumeId;

/// The volume that `value`, evaluated, names, where it is a volume's
/// handle. A handle names a volume of its own runtime, since no value
/// crosses heaps.
pub(crate) fn handle_volume(access: &RuntimeValueAccess<'_>, value: &Value) -> Option<VolumeId> {
    let Value::Volume(handle) = value else {
        return None;
    };
    Some(handle.id_in(access))
}
