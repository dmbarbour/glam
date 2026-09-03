//! Private production-shaped managed storage for one core value.
//!
//! I4F.2c prepares this node and its rooting gateway without routing normal
//! value construction through it. The atomic production switch in I4F.2d is
//! the first checkpoint allowed to publish these roots outside this module.

use std::fmt;

use glam_gc::{Root, Trace, UnsupportedLayout, Visitor};

use super::{
    ManagedDropRecord, ManagedFamily, RuntimeValueAccess, RuntimeValueObserver, managed_slot_extent,
};
use crate::core::{CoreValueFactory, Value};
use crate::number::Number;

#[allow(
    dead_code,
    reason = "I4F.2c prepares the production node before I4F.2d activates it"
)]
pub(crate) struct ManagedValueNode {
    value: Value,
}

/// Prepared private representation for the production runtime value root.
///
/// Small integer values preserve I2's allocation-free inline opportunity.
/// Every other current value is kept in the exact production managed shell.
/// Inline provenance is a weak value-domain witness; managed provenance is
/// the collector root's heap identity. Neither arm keeps its domain alive.
#[derive(Clone)]
#[allow(
    dead_code,
    reason = "I4F.2c prepares the root representation before I4F.2d activates it"
)]
pub(crate) enum PreparedRuntimeValueRoot {
    InlineInteger {
        observer: RuntimeValueObserver,
        value: i64,
    },
    Managed {
        root: Root<ManagedValueNode>,
    },
}

impl fmt::Debug for PreparedRuntimeValueRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RuntimeValueRoot")
    }
}

#[allow(
    dead_code,
    reason = "I4F.2c prepares the root representation before I4F.2d activates it"
)]
impl PreparedRuntimeValueRoot {
    /// Selects the private inline-or-root representation without publishing
    /// it through the production facade.
    pub(crate) fn prepare(values: &CoreValueFactory, value: Value) -> Self {
        if let Value::Number(number) = &value
            && let Some(value) = number.to_i64_if_integer()
        {
            return Self::InlineInteger {
                observer: values.runtime_value_observer(),
                value,
            };
        }

        values.with_runtime_value_access(|access| Self::managed(&access, value))
    }

    fn managed(access: &RuntimeValueAccess<'_>, value: Value) -> Self {
        Self::Managed {
            root: access
                .root_managed_value(value)
                .expect("the production managed value node must fit one collector slot"),
        }
    }

    /// Projects the core shell only while matching runtime access is active.
    ///
    /// The temporary inline shell and managed borrow cannot escape because
    /// callers receive only the operation's result.
    pub(crate) fn with_value<R>(
        &self,
        access: &RuntimeValueAccess<'_>,
        operation: impl FnOnce(&Value) -> R,
    ) -> Option<R> {
        match self {
            Self::InlineInteger { observer, value } => access
                .admits(observer)
                .then(|| operation(&Value::Number(Number::integer(*value)))),
            Self::Managed { root } => access
                .admits_root(root)
                .then(|| operation(access.get(root).value())),
        }
    }
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

    /// Reports the exact managed edges of the current production shell.
    ///
    /// All arms are deliberately zero-edge while their payload families use
    /// compatibility Rust ownership. I5-I8 replace the affected arm in the
    /// same checkpoint that its payload first stores `Gc` pointers.
    fn trace_managed_edges(&self, _visitor: &mut Visitor<'_>) {
        match &self.value {
            Value::Atom(_) => {}
            Value::Number(_) => {}
            Value::Binary(_) => {}
            Value::List(_) => {}
            Value::Dict(_) => {}
            Value::Builtin(_) => {}
            Value::PartialBuiltin(_) => {}
            Value::Function(_) => {}
            Value::Net(_) => {}
            Value::Lazy(_) => {}
            Value::Promised(_) => {}
            Value::Metadata(_) => {}
            Value::Opaque(_) => {}
        }
    }
}

// SAFETY: the wildcard-free dispatch above classifies every current `Value`
// variant. Before the I4F.2d production switch, compatibility payloads own no
// `Gc` pointer, so every arm reports zero edges. I5-I8 must replace an arm in
// the same checkpoint that its payload first stores a managed pointer.
unsafe impl Trace for ManagedValueNode {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        self.trace_managed_edges(visitor);
    }
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
    use std::sync::Arc;

    use glam_gc::Trace;

    use super::*;
    use crate::core::CoreValueFactory;
    use crate::core::managed::active_owner_inventory::{
        closed_compatibility_variants, compatibility_variant_name,
    };
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn values() -> CoreValueFactory {
        CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
    }

    fn project(value: &PreparedRuntimeValueRoot, values: &CoreValueFactory) -> Option<Value> {
        values.with_runtime_value_access(|access| value.with_value(&access, Clone::clone))
    }

    // Trait selection becomes ambiguous if the private opaque root gains a
    // representation-derived semantic relation.
    macro_rules! assert_prepared_root_does_not_implement {
        ($module:ident, $trait:path) => {
            mod $module {
                use super::PreparedRuntimeValueRoot;

                trait AmbiguousIfImplemented<Discriminator> {
                    fn verify() {}
                }

                struct Implemented;

                impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}
                impl<T: ?Sized + $trait> AmbiguousIfImplemented<Implemented> for T {}

                const _: fn() = || {
                    <PreparedRuntimeValueRoot as AmbiguousIfImplemented<_>>::verify();
                };
            }
        };
    }

    assert_prepared_root_does_not_implement!(prepared_root_not_partial_eq, PartialEq);
    assert_prepared_root_does_not_implement!(prepared_root_not_eq, Eq);
    assert_prepared_root_does_not_implement!(prepared_root_not_partial_ord, PartialOrd);
    assert_prepared_root_does_not_implement!(prepared_root_not_ord, Ord);
    assert_prepared_root_does_not_implement!(prepared_root_not_hash, std::hash::Hash);

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

    #[test]
    fn managed_value_node_dispatches_every_real_variant_as_zero_edge() {
        let values = values();
        let active_drops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let variants = closed_compatibility_variants(&values, &active_drops);
        let roots = values.with_runtime_value_access(|access| {
            variants
                .into_iter()
                .map(|value| {
                    access
                        .root_managed_value(value)
                        .expect("every compatibility shell should fit the production node")
                })
                .collect::<Vec<_>>()
        });

        values.with_runtime_value_access(|access| {
            assert_eq!(
                roots
                    .iter()
                    .map(|root| compatibility_variant_name(access.get(root).value()))
                    .collect::<Vec<_>>(),
                [
                    "atom",
                    "number",
                    "binary",
                    "list",
                    "dict",
                    "builtin",
                    "partial builtin",
                    "function",
                    "net",
                    "lazy",
                    "promised",
                    "metadata",
                    "opaque",
                ]
            );
        });

        let live = values
            .collect_managed_for_test()
            .expect("rooted production nodes should trace without hidden edges");
        assert_eq!(live.root_entries(), roots.len());
        assert_eq!(live.marked_slots(), roots.len());

        drop(roots);
        let dead = values
            .collect_managed_for_test()
            .expect("unrooted production nodes should reclaim every shell");
        assert_eq!(dead.finalized_slots(), 13);
        assert_eq!(
            active_drops.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "exact shell dispatch must not retire external owners"
        );
        assert_eq!(values.drain_external_owners_for_test(), 2);
    }

    #[test]
    fn prepared_root_preserves_inline_values_without_managed_allocation() {
        let values = values();
        let before = values.managed_statistics();
        let roots = (-512..512)
            .map(|value| PreparedRuntimeValueRoot::prepare(&values, Value::Number(value.into())))
            .collect::<Vec<_>>();

        assert_eq!(values.managed_statistics(), before);
        assert!(roots.iter().enumerate().all(|(offset, root)| {
            project(root, &values)
                == Some(Value::Number(Number::integer(
                    i64::try_from(offset).unwrap() - 512,
                )))
        }));
    }

    #[test]
    fn prepared_root_uses_one_registered_root_for_managed_clones() {
        fn assert_transport<T: Clone + Send + Sync>() {}
        assert_transport::<PreparedRuntimeValueRoot>();

        let values = values();
        let large_integer = Number::from_u64(u64::MAX);
        let root = PreparedRuntimeValueRoot::prepare(&values, Value::Number(large_integer.clone()));
        let alias = root.clone();
        let worker_values = values.clone();
        let worker = std::thread::spawn(move || project(&alias, &worker_values))
            .join()
            .expect("managed-root projection worker should not panic");
        assert_eq!(worker, Some(Value::Number(large_integer)));
        assert_eq!(format!("{root:?}"), "RuntimeValueRoot");

        let live = values
            .collect_managed_for_test()
            .expect("the prepared managed root should survive collection");
        assert_eq!(live.root_entries(), 1, "clones share one root cell");
        assert_eq!(live.marked_slots(), 1);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("dropping the final prepared root should permit reclamation");
        assert_eq!(dead.root_entries(), 0);
        assert_eq!(dead.finalized_slots(), 1);
    }

    #[test]
    fn prepared_root_rejects_other_runtime_access_for_both_arms() {
        let owner = values();
        let other = values();
        let inline = PreparedRuntimeValueRoot::prepare(&owner, Value::Number(42.into()));
        let managed =
            PreparedRuntimeValueRoot::prepare(&owner, Value::Dict(crate::core::Dict::new_sync()));

        assert_eq!(project(&inline, &owner), Some(Value::Number(42.into())));
        assert!(matches!(project(&managed, &owner), Some(Value::Dict(dict)) if dict.is_empty()));
        assert_eq!(project(&inline, &other), None);
        assert_eq!(project(&managed, &other), None);
    }

    #[test]
    fn prepared_root_becomes_inaccessible_when_its_domain_is_dropped() {
        let owner = values();
        let domain = Arc::downgrade(owner.value_domain());
        let inline = PreparedRuntimeValueRoot::prepare(&owner, Value::Number((-7).into()));
        let managed =
            PreparedRuntimeValueRoot::prepare(&owner, Value::Dict(crate::core::Dict::new_sync()));

        drop(owner);
        assert!(domain.upgrade().is_none());

        let other = values();
        assert_eq!(project(&inline, &other), None);
        assert_eq!(project(&managed, &other), None);
    }

    #[test]
    fn prepared_root_projection_nests_inside_one_runtime_access_region() {
        let values = values();
        let root =
            PreparedRuntimeValueRoot::prepare(&values, Value::Dict(crate::core::Dict::new_sync()));

        values.with_runtime_value_access(|outer| {
            root.with_value(&outer, |before| {
                let before = std::ptr::from_ref(before);
                let nested = values.with_runtime_value_access(|inner| {
                    root.with_value(&inner, |value| {
                        assert_eq!(std::ptr::from_ref(value), before);
                        matches!(value, Value::Dict(dict) if dict.is_empty())
                    })
                });
                assert_eq!(nested, Some(true));
            })
            .expect("the outer projection should accept its own runtime");
        });
    }
}
