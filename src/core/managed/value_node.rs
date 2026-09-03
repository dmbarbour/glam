//! Private production-shaped managed storage for one core value.
//!
//! I4F.2c prepares this node and its rooting gateway without routing normal
//! value construction through it. The atomic production switch in I4F.2d is
//! the first checkpoint allowed to publish these roots outside this module.

use glam_gc::{Root, Trace, UnsupportedLayout, Visitor};

use super::{ManagedDropRecord, ManagedFamily, RuntimeValueAccess, managed_slot_extent};
use crate::core::Value;

#[allow(
    dead_code,
    reason = "I4F.2c prepares the production node before I4F.2d activates it"
)]
pub(crate) struct ManagedValueNode {
    value: Value,
}

#[allow(
    dead_code,
    reason = "I4F.2c prepares construction and projection before the production switch"
)]
impl ManagedValueNode {
    fn new(value: Value) -> Self {
        Self { value }
    }

    pub(crate) fn value(&self) -> &Value {
        &self.value
    }
}

// SAFETY: before the I4F.2d production switch, compatibility `Value` owns no
// `Gc` pointer. I4F.2b proved that its complete transitive Rust destruction is
// passive. I4F.2c.2 replaces this closed zero-edge implementation with
// compile-exhaustive per-variant dispatch before any recursive managed edge is
// introduced by I5-I8.
unsafe impl Trace for ManagedValueNode {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, _visitor: &mut Visitor<'_>) {}
}

// SAFETY: the node has no direct Drop implementation. Its sole compatibility
// payload was admitted by the I4F.2b passive-destruction closure gate; all
// active callback, reservation, and opaque retirement remains in the runtime
// external-owner registry.
unsafe impl ManagedFamily for ManagedValueNode {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "production managed core value node",
        "src/core/managed/value_node.rs",
        "no direct Drop implementation",
        "compatibility Value destruction passed the I4F.2b passive closure gate",
    );
}

impl RuntimeValueAccess<'_> {
    /// Allocates and roots one managed core value without exposing a bare
    /// pointer beyond this bounded access region.
    ///
    /// Production constructors deliberately do not call this gateway until
    /// the atomic root-representation switch in I4F.2d.
    #[allow(
        dead_code,
        reason = "I4F.2c verifies the gateway before I4F.2d routes production roots through it"
    )]
    pub(crate) fn root_managed_value(
        &self,
        value: Value,
    ) -> Result<Root<ManagedValueNode>, UnsupportedLayout> {
        let allocator = self.allocator::<ManagedValueNode>()?;
        Ok(self.root(allocator.alloc(ManagedValueNode::new(value))))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use glam_gc::Trace;

    use super::*;
    use crate::core::CoreValueFactory;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn values() -> CoreValueFactory {
        CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
    }

    #[test]
    fn managed_value_node_family_contract_and_lifecycle() {
        assert_eq!(
            <ManagedValueNode as Trace>::REQUESTED_SLOT_SIZE,
            Some(managed_slot_extent::<ManagedValueNode>())
        );
        assert_eq!(
            <ManagedValueNode as ManagedFamily>::DROP_RECORD.fields(),
            (
                "production managed core value node",
                "src/core/managed/value_node.rs",
                "no direct Drop implementation",
                "compatibility Value destruction passed the I4F.2b passive closure gate",
            )
        );

        let values = values();
        let root = values.with_runtime_value_access(|access| {
            access
                .root_managed_value(Value::Number(42.into()))
                .expect("the production value node should fit a managed run")
        });
        values.with_runtime_value_access(|access| {
            assert!(
                matches!(access.get(&root).value(), Value::Number(number) if number == &42.into())
            );
        });

        let live = values
            .collect_managed_for_test()
            .expect("a rooted production-shaped node should survive collection");
        assert_eq!(live.root_entries(), 1);
        assert_eq!(live.marked_slots(), 1);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("an unrooted production-shaped node should be reclaimed");
        assert_eq!(dead.root_entries(), 0);
        assert_eq!(dead.finalized_slots(), 1);
    }

    #[test]
    fn managed_value_gateway_has_no_production_call_site_before_switch() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut stack = vec![manifest.join("src")];
        let mut call_sites = Vec::new();
        while let Some(directory) = stack.pop() {
            for entry in fs::read_dir(directory).expect("source directory should be readable") {
                let path = entry.expect("source entry should be readable").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs")
                    || path == manifest.join("src/core/managed/value_node.rs")
                {
                    continue;
                }
                let source = fs::read_to_string(&path).expect("Rust source should be readable");
                if source.contains(".root_managed_value(") {
                    call_sites.push(
                        path.strip_prefix(manifest)
                            .expect("source should be below the manifest")
                            .to_path_buf(),
                    );
                }
            }
        }
        assert!(
            call_sites.is_empty(),
            "production managed-value gateway activated before I4F.2d: {call_sites:?}"
        );
    }
}
