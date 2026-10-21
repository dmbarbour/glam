//! Volumes as cells in the managed heap (`api-volume-cells`,
//! `opaque-resources-reclaimed-when-unreachable`).
//!
//! A volume's identity is a managed cell holding its ID, and a handle is one
//! edge to it. The reflection store holds each committed version of a volume
//! through an ephemeron keyed on its cell, whose value is a contents cell.
//! Contents reach the cell only through that ephemeron's value, which does
//! not count, so a volume whose handles are all unreachable is collected
//! with its contents, even where they reach its handles. Durable owners that
//! must keep a volume, such as the store for its own volumes, hold a root on
//! its cell.

use glam_gc::{Ephemeron, Gc, GcRef, Root, Trace, Visitor};

use super::{ManagedDropRecord, ManagedFamily, RuntimeValueAccess, managed_slot_extent};
use crate::core::{Value, VolumeId};

use super::payload_edges::visit_compatibility_managed_edges;

/// A volume's identity in the managed heap.
pub(crate) struct ManagedVolumeCell {
    id: VolumeId,
}

/// One committed version of a volume: its value. It is the per-version
/// record, which may later keep a short history of the volume's commits
/// (`perf-volume-histories`).
pub(crate) struct ManagedVolumeContents {
    value: Value,
}

/// A handle's edge to its volume's cell.
pub(crate) struct ManagedVolumeEdge(Gc<ManagedVolumeCell>);

/// A volume's cell, viewed while an access region lasts, which keeps the
/// volume as long: the capability by which a request reads and writes it.
#[derive(Clone, Copy)]
pub(crate) struct VolumeCellView<'access>(GcRef<'access, ManagedVolumeCell>);

/// A durable owner's hold on a volume's cell, which keeps the volume: the
/// store's on its own volumes and a host's, and a transaction's on each
/// volume it writes or creates.
#[derive(Clone)]
pub(crate) struct ManagedVolumeRoot {
    root: Root<ManagedVolumeCell>,
}

/// A committed version of a volume, which keeps its contents only while some
/// other path reaches the volume's cell.
#[derive(Clone)]
pub(crate) struct ManagedVolumeVersion {
    root: Root<Ephemeron<ManagedVolumeCell, ManagedVolumeContents>>,
}

// SAFETY: a volume cell holds only its ID: no managed edge.
unsafe impl Trace for ManagedVolumeCell {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, _visitor: &mut Visitor<'_>) {}
}

// SAFETY: a volume cell has no drop glue.
unsafe impl ManagedFamily for ManagedVolumeCell {
    const DROP_RECORD: ManagedDropRecord =
        ManagedDropRecord::no_drop("managed volume cell", "src/core/managed/volume_cells.rs");
}

// SAFETY: the contents report every managed edge of their value through the
// central compatibility walk, which stops at each managed identity.
unsafe impl Trace for ManagedVolumeContents {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        visit_compatibility_managed_edges(&self.value, visitor);
    }
}

// SAFETY: the contents hold one immutable compatibility value, whose shells
// destroy passively, as `ManagedValueNode`'s does.
unsafe impl ManagedFamily for ManagedVolumeContents {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "managed volume contents",
        "src/core/managed/volume_cells.rs",
        "no direct Drop implementation",
        "the value's compatibility shells destroy passively",
    );
}

impl ManagedVolumeEdge {
    pub(crate) fn trace(&self, visitor: &mut Visitor<'_>) {
        visitor.visit(&self.0);
    }

    #[inline(always)]
    pub(crate) fn duplicate_in(&self, access: &RuntimeValueAccess<'_>) -> Self {
        Self(access.duplicate_edge(&self.0))
    }

    #[inline(always)]
    pub(crate) fn same_allocation_in(&self, other: &Self, access: &RuntimeValueAccess<'_>) -> bool {
        access.same_edge(&self.0, &other.0)
    }

    /// The volume this edge names. The caller reaches the edge from a live
    /// owner under `access`.
    pub(crate) fn id(&self, access: &RuntimeValueAccess<'_>) -> VolumeId {
        access.get_edge(&self.0).id
    }

    /// The volume's cell, viewed. The caller reaches the edge from a live
    /// owner under `access`.
    pub(crate) fn view<'access>(
        &self,
        access: &'access RuntimeValueAccess<'_>,
    ) -> VolumeCellView<'access> {
        VolumeCellView(access.view_edge(&self.0, super::EdgeLiveness::HeldValue))
    }
}

impl VolumeCellView<'_> {
    pub(crate) fn id(self) -> VolumeId {
        self.0.get().id
    }
}

impl ManagedVolumeRoot {
    /// The volume's cell, viewed.
    pub(crate) fn view_in<'access>(
        &self,
        access: &'access RuntimeValueAccess<'_>,
    ) -> VolumeCellView<'access> {
        VolumeCellView(access.view_root(&self.root))
    }

    /// A handle to the volume.
    pub(crate) fn handle_in(&self, access: &RuntimeValueAccess<'_>) -> Value {
        Value::Volume(crate::core::VolumeValue::from_edge(ManagedVolumeEdge(
            access.project_root(&self.root),
        )))
    }
}

impl ManagedVolumeVersion {
    /// The version's value, borrowed while `access` lasts, where `volume`
    /// is the cell it is keyed on, which a live view keeps from clearing.
    pub(crate) fn value_of<'access>(
        &self,
        access: &'access RuntimeValueAccess<'_>,
        volume: VolumeCellView<'_>,
    ) -> Option<&'access Value> {
        let (cell, contents) = access.ephemeron_entry(&self.root)?;
        assert!(
            cell.same_allocation(volume.0),
            "a volume's version is keyed on its cell"
        );
        Some(&contents.get().value)
    }

    /// The version's cell, viewed, or `None` once a collection found the
    /// volume unreachable.
    pub(crate) fn cell_in<'access>(
        &self,
        access: &'access RuntimeValueAccess<'_>,
    ) -> Option<VolumeCellView<'access>> {
        let (cell, _) = access.ephemeron_entry(&self.root)?;
        Some(VolumeCellView(cell))
    }

    /// The version's value, or `None` once a collection found the volume
    /// unreachable.
    pub(crate) fn value_in(&self, access: &RuntimeValueAccess<'_>) -> Option<Value> {
        let (_, contents) = access.ephemeron_entry(&self.root)?;
        Some(access.duplicate_value(&contents.get().value))
    }

    /// Whether a collection found the volume unreachable.
    pub(crate) fn is_cleared_in(&self, access: &RuntimeValueAccess<'_>) -> bool {
        access.ephemeron_entry(&self.root).is_none()
    }
}

impl RuntimeValueAccess<'_> {
    /// The cell that `value`, a volume's handle, names, or `None` for
    /// another value. The caller holds `value` under this access.
    pub(crate) fn volume_cell(&self, value: &Value) -> Option<VolumeCellView<'_>> {
        let Value::Volume(handle) = value else {
            return None;
        };
        Some(handle.cell_in(self))
    }

    /// A root on `volume`'s cell, which keeps the volume.
    pub(crate) fn keep_volume(&self, volume: VolumeCellView<'_>) -> ManagedVolumeRoot {
        ManagedVolumeRoot {
            root: self.root(volume.0),
        }
    }

    /// Allocates the cell of a new volume, `id`, and roots it.
    pub(crate) fn new_volume(&self, id: VolumeId) -> ManagedVolumeRoot {
        let cell = self
            .allocator::<ManagedVolumeCell>()
            .expect("a volume cell fits one collector slot")
            .alloc_view(ManagedVolumeCell { id });
        ManagedVolumeRoot {
            root: self.root(cell),
        }
    }

    /// Commits `value` as a version of `volume`, held through an ephemeron
    /// keyed on its cell.
    pub(crate) fn volume_version(
        &self,
        volume: &ManagedVolumeRoot,
        value: Value,
    ) -> ManagedVolumeVersion {
        ManagedVolumeVersion {
            root: self.root_ephemeron(&volume.root, ManagedVolumeContents { value }),
        }
    }
}
