//! Transitive compatibility walk to the first managed identity boundary.
//!
//! Raw lazy, promise, and core-net handles remain `Arc`-owned until I5D. They
//! are nevertheless stop points now: the compatibility walk must never enter
//! those cells. I5D changes what the stop adapter reports, not which
//! compatibility structures the walk crosses.

use glam_gc::Visitor;

use super::CompatibilityValueEdges;
use crate::core::Value;

trait ManagedIdentityStops {
    /// Reports `value` if it denotes a managed identity and returns whether
    /// recursive compatibility traversal must stop at this value.
    fn visit_stop(&self, value: &Value, visitor: &mut Visitor<'_>) -> bool;
}

struct RawIdentityStops;

impl ManagedIdentityStops for RawIdentityStops {
    fn visit_stop(&self, value: &Value, _visitor: &mut Visitor<'_>) -> bool {
        matches!(
            value,
            Value::Lazy(_) | Value::Promised(_) | Value::Function(_) | Value::Net(_)
        )
    }
}

fn visit_value_with(value: &Value, visitor: &mut Visitor<'_>, stops: &impl ManagedIdentityStops) {
    if stops.visit_stop(value, visitor) {
        return;
    }
    value.visit_compatibility_value_edges(&mut |child| {
        visit_value_with(child, visitor, stops);
    });
}

/// Walks raw compatibility-owned structure to the first recursive identity.
///
/// The current raw identities report no managed pointer, but remain hard stop
/// points. This function performs no semantic operation and crosses no
/// registered root. I5D replaces `RawIdentityStops` with the prepared managed
/// identity adapter as part of the atomic representation cutover.
pub(crate) fn visit_compatibility_managed_edges(value: &Value, visitor: &mut Visitor<'_>) {
    visit_value_with(value, visitor, &RawIdentityStops);
}

/// Walks one compatibility payload to the same first managed-identity
/// boundary used by the production value shell.
pub(crate) fn visit_compatibility_payload_managed_edges(
    payload: &impl CompatibilityValueEdges,
    visitor: &mut Visitor<'_>,
) {
    payload.visit_compatibility_value_edges(&mut |value| {
        visit_compatibility_managed_edges(value, visitor);
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use glam_gc::{Gc, Trace};

    use super::*;
    use crate::core::{
        Builtin, BuiltinCall, CoreValueFactory, Dict, FunctionValue, LazyValue, List,
        ManagedDropRecord, ManagedFamily, MetadataCarrier, PromisedValue,
    };
    use crate::core_net::CoreSpecialization;
    use crate::interaction_net::NetBuilder;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    struct SyntheticManagedLeaf {
        drops: Arc<AtomicUsize>,
    }

    impl Drop for SyntheticManagedLeaf {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: the leaf has no managed edge. Drop updates only an external
    // atomic counter.
    unsafe impl Trace for SyntheticManagedLeaf {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    // SAFETY: direct Drop updates only an external atomic, and the leaf has no
    // managed or runtime-owned field.
    unsafe impl ManagedFamily for SyntheticManagedLeaf {
        const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
            "I5B synthetic compatibility leaf",
            "src/core/managed/payload_edges/managed.rs",
            "direct Drop updates only an external atomic counter",
            "no transitive fields",
        );
    }

    struct SyntheticStops {
        leaf: Gc<SyntheticManagedLeaf>,
        marker: Value,
        marker_visits: Arc<AtomicUsize>,
        identity_visits: Arc<AtomicUsize>,
    }

    impl ManagedIdentityStops for SyntheticStops {
        fn visit_stop(&self, value: &Value, visitor: &mut Visitor<'_>) -> bool {
            if value == &self.marker {
                self.marker_visits.fetch_add(1, Ordering::Relaxed);
                visitor.visit(self.leaf);
                return true;
            }
            if matches!(
                value,
                Value::Lazy(_) | Value::Promised(_) | Value::Function(_) | Value::Net(_)
            ) {
                self.identity_visits.fetch_add(1, Ordering::Relaxed);
                visitor.visit(self.leaf);
                return true;
            }
            false
        }
    }

    struct SyntheticCompatibilityOwner {
        value: Value,
        stops: SyntheticStops,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for SyntheticCompatibilityOwner {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: `visit_value_with` reports the fixture's sole managed pointer
    // whenever it encounters the synthetic marker or a declared recursive
    // identity. The fixture mutates no state after publication.
    unsafe impl Trace for SyntheticCompatibilityOwner {
        fn trace(&self, visitor: &mut Visitor<'_>) {
            visit_value_with(&self.value, visitor, &self.stops);
        }
    }

    // SAFETY: direct Drop updates only an external atomic. Compatibility
    // values and the inert Gc pointer destroy without observing managed state.
    unsafe impl ManagedFamily for SyntheticCompatibilityOwner {
        const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
            "I5B synthetic compatibility owner",
            "src/core/managed/payload_edges/managed.rs",
            "direct Drop updates only an external atomic counter",
            "compatibility values and the inert Gc edge drop passively",
        );
    }

    fn values() -> CoreValueFactory {
        CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
    }

    fn marker() -> Value {
        Value::Number(9_876_543.into())
    }

    fn owner(
        value: Value,
        leaf: Gc<SyntheticManagedLeaf>,
        marker_visits: &Arc<AtomicUsize>,
        identity_visits: &Arc<AtomicUsize>,
        drops: &Arc<AtomicUsize>,
    ) -> SyntheticCompatibilityOwner {
        SyntheticCompatibilityOwner {
            value,
            stops: SyntheticStops {
                leaf,
                marker: marker(),
                marker_visits: Arc::clone(marker_visits),
                identity_visits: Arc::clone(identity_visits),
            },
            drops: Arc::clone(drops),
        }
    }

    #[test]
    fn nested_compatibility_owners_report_the_synthetic_managed_leaf() {
        let values = values();
        let baseline = values
            .collect_managed_for_test()
            .expect("canonical roots should collect before the I5B fixture");
        let leaf_drops = Arc::new(AtomicUsize::new(0));
        let owner_drops = Arc::new(AtomicUsize::new(0));
        let marker_visits = Arc::new(AtomicUsize::new(0));
        let identity_visits = Arc::new(AtomicUsize::new(0));
        let root = values.with_managed_values(|scope| {
            let leaf_allocator = scope
                .allocator::<SyntheticManagedLeaf>()
                .expect("the synthetic leaf should fit a managed slot");
            let owner_allocator = scope
                .allocator::<SyntheticCompatibilityOwner>()
                .expect("the synthetic owner should fit a managed slot");
            let leaf = leaf_allocator.alloc(SyntheticManagedLeaf {
                drops: Arc::clone(&leaf_drops),
            });

            let nested = Value::Dict(
                Dict::new_sync()
                    .insert(crate::core::Key::binary_from_text("direct"), marker())
                    .insert(
                        crate::core::Key::binary_from_text("list"),
                        Value::List(List::from_values(vec![marker()])),
                    )
                    .insert(
                        crate::core::Key::binary_from_text("dict"),
                        Value::Dict(
                            Dict::new_sync()
                                .insert(crate::core::Key::binary_from_text("nested"), marker()),
                        ),
                    )
                    .insert(
                        crate::core::Key::binary_from_text("partial"),
                        Value::PartialBuiltin(BuiltinCall {
                            builtin: Builtin::Append,
                            arguments: Arc::from([marker()]),
                        }),
                    )
                    .insert(
                        crate::core::Key::binary_from_text("metadata"),
                        Value::Metadata(MetadataCarrier::new(marker())),
                    ),
            );
            let node = owner_allocator.alloc(owner(
                nested,
                leaf,
                &marker_visits,
                &identity_visits,
                &owner_drops,
            ));
            scope.root(node)
        });

        let live = values
            .collect_managed_for_test()
            .expect("the nested synthetic leaf should remain reachable");
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 2);
        assert_eq!(marker_visits.load(Ordering::Relaxed), 5);
        assert_eq!(identity_visits.load(Ordering::Relaxed), 0);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("the unrooted synthetic graph should be reclaimed");
        assert_eq!(dead.finalized_slots(), 2);
        assert_eq!(leaf_drops.load(Ordering::Relaxed), 1);
        assert_eq!(owner_drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn recursive_identity_stops_do_not_enter_raw_cells_or_nets() {
        let values = values();
        let leaf_drops = Arc::new(AtomicUsize::new(0));
        let owner_drops = Arc::new(AtomicUsize::new(0));
        let marker_visits = Arc::new(AtomicUsize::new(0));
        let identity_visits = Arc::new(AtomicUsize::new(0));

        let lazy = LazyValue::semantic_computation(
            &values,
            "I5B raw lazy stop",
            [marker()],
            |_context, captures| Ok(captures[0].clone()),
        );
        let promise = PromisedValue::new(&values, "I5B raw promise stop");
        promise
            .set(marker())
            .expect("the fresh raw promise should accept one assignment");
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let exposed = builder.data(marker());
        let net = crate::core::NetValue::new(values.instantiate_core_net(&builder.finish(exposed)));
        let stopped = Value::List(List::from_values(vec![
            Value::Lazy(lazy),
            Value::Promised(promise),
            Value::Function(FunctionValue::new(net.clone(), 1)),
            Value::Net(net),
        ]));

        let root = values.with_managed_values(|scope| {
            let leaf = scope
                .allocator::<SyntheticManagedLeaf>()
                .expect("the synthetic leaf should fit a managed slot")
                .alloc(SyntheticManagedLeaf {
                    drops: Arc::clone(&leaf_drops),
                });
            let node = scope
                .allocator::<SyntheticCompatibilityOwner>()
                .expect("the synthetic owner should fit a managed slot")
                .alloc(owner(
                    stopped,
                    leaf,
                    &marker_visits,
                    &identity_visits,
                    &owner_drops,
                ));
            scope.root(node)
        });

        values
            .collect_managed_for_test()
            .expect("raw identity stops should report the synthetic leaf");
        assert_eq!(identity_visits.load(Ordering::Relaxed), 4);
        assert_eq!(
            marker_visits.load(Ordering::Relaxed),
            0,
            "the compatibility walk must not enter a raw identity"
        );

        drop(root);
        values
            .collect_managed_for_test()
            .expect("the raw-stop fixture should reclaim after its root drops");
        assert_eq!(leaf_drops.load(Ordering::Relaxed), 1);
        assert_eq!(owner_drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn production_managed_node_delegates_to_the_central_walk() {
        let source = include_str!("../value_node.rs");
        assert_eq!(
            source
                .matches("visit_compatibility_managed_edges(&self.value, visitor)")
                .count(),
            1,
            "ManagedValueNode must have exactly one authoritative transitive compatibility walk"
        );
    }
}
