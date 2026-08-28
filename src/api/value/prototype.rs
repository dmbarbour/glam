//! Isolated Phase I2 public-root representation prototype.
//!
//! Nothing in this module is part of the production `api::Value` facade. The
//! fixtures exercise the selected private inline-or-managed shape before the
//! production graph has exact tracing or collection enabled.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use glam_gc::{Gc, Root, Trace, Visitor};

use crate::core::{CoreValueDomainWitness, CoreValueFactory};
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

#[derive(Clone)]
enum PrototypeValue {
    Inline {
        domain: CoreValueDomainWitness,
        value: i64,
    },
    Managed {
        root: Root<PrototypeNode>,
    },
}

impl fmt::Debug for PrototypeValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Value")
    }
}

// Each module makes trait selection ambiguous if `PrototypeValue` gains the
// forbidden trait. This keeps the absence of identity-derived public relations
// as compile-time evidence without adding a production dependency or exposing
// the test-only prototype to an integration-test crate.
macro_rules! assert_prototype_does_not_implement {
    ($module:ident, $trait:path) => {
        mod $module {
            use super::PrototypeValue;

            trait AmbiguousIfImplemented<Discriminator> {
                fn verify() {}
            }

            struct Implemented;

            impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}
            impl<T: ?Sized + $trait> AmbiguousIfImplemented<Implemented> for T {}

            const _: fn() = || {
                <PrototypeValue as AmbiguousIfImplemented<_>>::verify();
            };
        }
    };
}

assert_prototype_does_not_implement!(prototype_value_not_partial_eq, PartialEq);
assert_prototype_does_not_implement!(prototype_value_not_eq, Eq);
assert_prototype_does_not_implement!(prototype_value_not_partial_ord, PartialOrd);
assert_prototype_does_not_implement!(prototype_value_not_ord, Ord);
assert_prototype_does_not_implement!(prototype_value_not_hash, std::hash::Hash);

struct PrototypeNode {
    value: u64,
    child: Option<Gc<PrototypeNode>>,
    drops: Arc<AtomicUsize>,
}

// SAFETY: `child` is the node's only managed edge. The atomic counter is an
// external test observer and neither contains nor manufactures managed data.
unsafe impl Trace for PrototypeNode {
    fn trace(&self, visitor: &mut Visitor<'_>) {
        if let Some(child) = self.child {
            visitor.visit(child);
        }
    }
}

impl Drop for PrototypeNode {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Eq, PartialEq)]
enum PrototypeObservation {
    Inline(i64),
    Managed(u64),
}

#[derive(Debug, Eq, PartialEq)]
struct InaccessiblePrototypeValue;

impl PrototypeValue {
    fn inline(values: &CoreValueFactory, value: i64) -> Self {
        Self::Inline {
            domain: values.managed_domain_witness(),
            value,
        }
    }

    fn managed_leaf(values: &CoreValueFactory, value: u64, drops: Arc<AtomicUsize>) -> Self {
        values.with_managed_values(|scope| {
            let allocator = scope
                .allocator::<PrototypeNode>()
                .expect("the managed prototype node should fit one collector slot");
            Self::Managed {
                root: scope.root(allocator.alloc(PrototypeNode {
                    value,
                    child: None,
                    drops,
                })),
            }
        })
    }

    fn managed_pair(values: &CoreValueFactory, drops: Arc<AtomicUsize>) -> Self {
        values.with_managed_values(|scope| {
            let allocator = scope
                .allocator::<PrototypeNode>()
                .expect("the recursive prototype node should fit one collector slot");
            let child = allocator.alloc(PrototypeNode {
                value: 2,
                child: None,
                drops: drops.clone(),
            });
            let parent = allocator.alloc(PrototypeNode {
                value: 1,
                child: Some(child),
                drops,
            });
            Self::Managed {
                root: scope.root(parent),
            }
        })
    }

    fn observe(
        &self,
        values: &CoreValueFactory,
    ) -> Result<PrototypeObservation, InaccessiblePrototypeValue> {
        match self {
            Self::Inline { domain, value } => values
                .owns_managed_domain_witness(domain)
                .then_some(PrototypeObservation::Inline(*value))
                .ok_or(InaccessiblePrototypeValue),
            Self::Managed { root } => {
                if !values.owns_managed_root(root) {
                    return Err(InaccessiblePrototypeValue);
                }
                Ok(values.with_managed_values(|scope| {
                    PrototypeObservation::Managed(scope.get(root).value)
                }))
            }
        }
    }

    fn inline_domain_is_live(&self) -> bool {
        match self {
            Self::Inline { domain, .. } => domain.is_live(),
            Self::Managed { .. } => {
                unreachable!("only inline prototype values carry a domain witness")
            }
        }
    }
}

fn prototype_factory() -> CoreValueFactory {
    CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
}

fn assert_transport_traits<T: Clone + Send + Sync>() {}

#[test]
fn prototype_root_moves_between_threads() {
    assert_transport_traits::<PrototypeValue>();

    let values = prototype_factory();
    let drops = Arc::new(AtomicUsize::new(0));
    let value = PrototypeValue::managed_leaf(&values, 42, drops);
    let worker_value = value.clone();
    let worker_values = values.clone();

    let report = values
        .collect_managed_prototype()
        .expect("the isolated managed prototype should collect");
    assert_eq!(report.root_entries(), 1, "a clone shares one root cell");

    let observed = std::thread::spawn(move || worker_value.observe(&worker_values))
        .join()
        .expect("prototype observation worker should not panic");
    assert_eq!(observed, Ok(PrototypeObservation::Managed(42)));
    assert_eq!(value.observe(&values), observed);
}

#[test]
fn prototype_value_debug_is_opaque() {
    let values = prototype_factory();
    let first_inline = PrototypeValue::inline(&values, 1);
    let second_inline = PrototypeValue::inline(&values, -9);
    let managed = PrototypeValue::managed_leaf(&values, 42, Arc::new(AtomicUsize::new(0)));

    assert_eq!(format!("{first_inline:?}"), "Value");
    assert_eq!(format!("{second_inline:?}"), "Value");
    assert_eq!(format!("{managed:?}"), "Value");

    drop(values);
    assert_eq!(format!("{first_inline:?}"), "Value");
    assert_eq!(format!("{managed:?}"), "Value");
}

#[test]
fn prototype_root_rejects_another_heap() {
    let owner = prototype_factory();
    let foreign = prototype_factory();
    let value = PrototypeValue::managed_leaf(&owner, 11, Arc::new(AtomicUsize::new(0)));

    assert_eq!(value.observe(&owner), Ok(PrototypeObservation::Managed(11)));
    assert_eq!(value.observe(&foreign), Err(InaccessiblePrototypeValue));
}

#[test]
fn prototype_root_becomes_inert_after_domain_drop() {
    let owner = prototype_factory();
    let domain = Arc::downgrade(owner.value_domain());
    let value = PrototypeValue::managed_leaf(&owner, 17, Arc::new(AtomicUsize::new(0)));

    drop(owner);

    assert!(domain.upgrade().is_none());
    let foreign = prototype_factory();
    assert_eq!(value.observe(&foreign), Err(InaccessiblePrototypeValue));
    drop(value);
}

#[test]
fn prototype_inline_values_allocate_no_managed_slots() {
    let values = prototype_factory();
    let before = values.managed_statistics();
    assert_eq!(before.assigned_runs(), 0);

    let inline = (0..1024)
        .map(|value| PrototypeValue::inline(&values, value))
        .collect::<Vec<_>>();

    assert_eq!(values.managed_statistics(), before);
    for (expected, value) in inline.iter().enumerate() {
        assert_eq!(
            value.observe(&values),
            Ok(PrototypeObservation::Inline(expected as i64))
        );
    }
}

#[test]
fn prototype_inline_value_rejects_another_domain() {
    let owner = prototype_factory();
    let foreign = prototype_factory();
    let value = PrototypeValue::inline(&owner, -23);

    assert!(value.inline_domain_is_live());
    assert_eq!(value.observe(&owner), Ok(PrototypeObservation::Inline(-23)));
    assert_eq!(value.observe(&foreign), Err(InaccessiblePrototypeValue));

    drop(owner);
    assert!(!value.inline_domain_is_live());
    assert_eq!(value.observe(&foreign), Err(InaccessiblePrototypeValue));
}

#[test]
fn prototype_recursive_root_traces_child() {
    let values = prototype_factory();
    let drops = Arc::new(AtomicUsize::new(0));
    let value = PrototypeValue::managed_pair(&values, drops.clone());

    let live = values
        .collect_managed_prototype()
        .expect("the rooted recursive prototype should collect");
    assert_eq!(live.root_entries(), 1);
    assert_eq!(live.marked_slots(), 2);
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    drop(value);
    let dead = values
        .collect_managed_prototype()
        .expect("the unrooted recursive prototype should collect");
    assert_eq!(dead.root_entries(), 0);
    assert_eq!(dead.finalized_slots(), 2);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}
