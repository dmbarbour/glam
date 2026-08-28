//! Isolated Phase I2 public-root representation prototype.
//!
//! Nothing in this module is part of the production `api::Value` facade. The
//! fixtures exercise the selected private inline-or-managed shape before the
//! production graph has exact tracing or collection enabled.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use glam_gc::{Gc, Root, Trace, Visitor};

use crate::core::{CoreValueAllocationScope, CoreValueDomainWitness, CoreValueFactory};
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

// Each module makes trait selection ambiguous if an opaque prototype handle
// gains the forbidden trait. This keeps the absence of identity-derived public
// relations as compile-time evidence without adding a production dependency or
// exposing the test-only prototype to an integration-test crate.
macro_rules! assert_prototype_does_not_implement {
    ($module:ident, $type:ident, $trait:path) => {
        mod $module {
            use super::$type;

            trait AmbiguousIfImplemented<Discriminator> {
                fn verify() {}
            }

            struct Implemented;

            impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}
            impl<T: ?Sized + $trait> AmbiguousIfImplemented<Implemented> for T {}

            const _: fn() = || {
                <$type as AmbiguousIfImplemented<_>>::verify();
            };
        }
    };
}

assert_prototype_does_not_implement!(prototype_value_not_partial_eq, PrototypeValue, PartialEq);
assert_prototype_does_not_implement!(prototype_value_not_eq, PrototypeValue, Eq);
assert_prototype_does_not_implement!(prototype_value_not_partial_ord, PrototypeValue, PartialOrd);
assert_prototype_does_not_implement!(prototype_value_not_ord, PrototypeValue, Ord);
assert_prototype_does_not_implement!(prototype_value_not_hash, PrototypeValue, std::hash::Hash);

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrototypeValueKind {
    Integer,
    Node,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PrototypeOwnedValue {
    Inline(i64),
    Managed(PrototypeOwnedNode),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PrototypeOwnedNode {
    value: u64,
    child: Option<Box<PrototypeOwnedNode>>,
}

#[derive(Debug, Eq, PartialEq)]
struct InaccessiblePrototypeValue;

/// Borrowed authority for observing values in exactly one live prototype
/// runtime domain.
#[derive(Clone, Copy)]
struct PrototypeRuntime<'runtime> {
    values: &'runtime CoreValueFactory,
}

/// Proof that one prototype value's outer representation was accessible and
/// already in weak-head normal form when evaluated.
///
/// This is another view of the same opaque handle, not a second root model and
/// not independent authority to inspect the value later.
#[derive(Clone)]
struct PrototypeEvaluatedValue(PrototypeValue);

impl fmt::Debug for PrototypeEvaluatedValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EvaluatedValue")
    }
}

assert_prototype_does_not_implement!(
    prototype_evaluated_value_not_partial_eq,
    PrototypeEvaluatedValue,
    PartialEq
);
assert_prototype_does_not_implement!(
    prototype_evaluated_value_not_eq,
    PrototypeEvaluatedValue,
    Eq
);
assert_prototype_does_not_implement!(
    prototype_evaluated_value_not_partial_ord,
    PrototypeEvaluatedValue,
    PartialOrd
);
assert_prototype_does_not_implement!(
    prototype_evaluated_value_not_ord,
    PrototypeEvaluatedValue,
    Ord
);
assert_prototype_does_not_implement!(
    prototype_evaluated_value_not_hash,
    PrototypeEvaluatedValue,
    std::hash::Hash
);

impl PrototypeValue {
    fn inline(values: &CoreValueFactory, value: i64) -> Self {
        Self::Inline {
            domain: values.managed_domain_witness(),
            value,
        }
    }

    fn managed_leaf(values: &CoreValueFactory, value: u64, drops: Arc<AtomicUsize>) -> Self {
        Self::managed_chain(values, &[value], drops)
    }

    fn managed_pair(values: &CoreValueFactory, drops: Arc<AtomicUsize>) -> Self {
        Self::managed_chain(values, &[1, 2], drops)
    }

    fn managed_chain(
        values: &CoreValueFactory,
        node_values: &[u64],
        drops: Arc<AtomicUsize>,
    ) -> Self {
        assert!(!node_values.is_empty(), "a managed chain needs one node");
        values.with_managed_values(|scope| {
            let allocator = scope
                .allocator::<PrototypeNode>()
                .expect("the managed prototype node should fit one collector slot");
            let mut child = None;
            for value in node_values.iter().rev() {
                child = Some(allocator.alloc(PrototypeNode {
                    value: *value,
                    child,
                    drops: Arc::clone(&drops),
                }));
            }
            Self::Managed {
                root: scope.root(child.expect("the nonempty chain has a root")),
            }
        })
    }

    fn structurally_equals(
        &self,
        runtime: &PrototypeRuntime<'_>,
        other: &Self,
    ) -> Result<bool, InaccessiblePrototypeValue> {
        runtime.structurally_equal(self, other)
    }
}

impl<'runtime> PrototypeRuntime<'runtime> {
    fn new(values: &'runtime CoreValueFactory) -> Self {
        Self { values }
    }

    fn require(&self, value: &PrototypeValue) -> Result<(), InaccessiblePrototypeValue> {
        let accessible = match value {
            PrototypeValue::Inline { domain, .. } => {
                self.values.owns_managed_domain_witness(domain)
            }
            PrototypeValue::Managed { root } => self.values.owns_managed_root(root),
        };
        accessible.then_some(()).ok_or(InaccessiblePrototypeValue)
    }

    fn evaluate(
        &self,
        value: &PrototypeValue,
    ) -> Result<PrototypeEvaluatedValue, InaccessiblePrototypeValue> {
        // Every isolated prototype representation is already in outer WHNF.
        // The real evaluator will perform demand before constructing this
        // witness; this checkpoint is concerned only with authority and roots.
        self.require(value)?;
        Ok(PrototypeEvaluatedValue(value.clone()))
    }

    fn structurally_equal(
        &self,
        left: &PrototypeValue,
        right: &PrototypeValue,
    ) -> Result<bool, InaccessiblePrototypeValue> {
        self.require(left)?;
        self.require(right)?;

        match (left, right) {
            (
                PrototypeValue::Inline {
                    value: left_value, ..
                },
                PrototypeValue::Inline {
                    value: right_value, ..
                },
            ) => Ok(left_value == right_value),
            (
                PrototypeValue::Managed { root: left_root },
                PrototypeValue::Managed { root: right_root },
            ) => Ok(self.values.with_managed_values(|scope| {
                let left = scope.get(left_root);
                let right = scope.get(right_root);
                prototype_nodes_equal(&scope, left, right)
            })),
            _ => Ok(false),
        }
    }

    fn kind(
        &self,
        value: &PrototypeValue,
    ) -> Result<PrototypeValueKind, InaccessiblePrototypeValue> {
        self.require(value)?;
        Ok(match value {
            PrototypeValue::Inline { .. } => PrototypeValueKind::Integer,
            PrototypeValue::Managed { .. } => PrototypeValueKind::Node,
        })
    }

    fn extract_owned(
        &self,
        value: &PrototypeValue,
    ) -> Result<PrototypeOwnedValue, InaccessiblePrototypeValue> {
        self.require(value)?;
        Ok(match value {
            PrototypeValue::Inline { value, .. } => PrototypeOwnedValue::Inline(*value),
            PrototypeValue::Managed { root } => self.values.with_managed_values(|scope| {
                PrototypeOwnedValue::Managed(prototype_node_to_owned(&scope, scope.get(root)))
            }),
        })
    }

    fn render(&self, value: &PrototypeValue) -> Result<String, InaccessiblePrototypeValue> {
        self.extract_owned(value).map(|value| match value {
            PrototypeOwnedValue::Inline(value) => format!("integer:{value}"),
            PrototypeOwnedValue::Managed(node) => prototype_node_render(&node),
        })
    }
}

fn prototype_nodes_equal(
    scope: &CoreValueAllocationScope<'_>,
    left: &PrototypeNode,
    right: &PrototypeNode,
) -> bool {
    if left.value != right.value {
        return false;
    }
    match (left.child, right.child) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            // SAFETY: both edges were allocated in the same class as their
            // rooted parents and are kept live by those parents' exact trace.
            let left = unsafe { scope.get_traced_edge(left) };
            // SAFETY: same proof as `left`.
            let right = unsafe { scope.get_traced_edge(right) };
            prototype_nodes_equal(scope, left, right)
        }
        _ => false,
    }
}

fn prototype_node_to_owned(
    scope: &CoreValueAllocationScope<'_>,
    node: &PrototypeNode,
) -> PrototypeOwnedNode {
    PrototypeOwnedNode {
        value: node.value,
        child: node.child.map(|child| {
            // SAFETY: this exact edge was allocated with and is kept live by
            // the rooted node graph currently borrowed under `scope`.
            let child = unsafe { scope.get_traced_edge(child) };
            Box::new(prototype_node_to_owned(scope, child))
        }),
    }
}

fn prototype_node_render(node: &PrototypeOwnedNode) -> String {
    match &node.child {
        Some(child) => format!("node:{}({})", node.value, prototype_node_render(child)),
        None => format!("node:{}", node.value),
    }
}

fn prototype_owned_chain(node_values: &[u64]) -> PrototypeOwnedNode {
    let (value, rest) = node_values
        .split_first()
        .expect("an owned prototype chain needs one node");
    PrototypeOwnedNode {
        value: *value,
        child: (!rest.is_empty()).then(|| Box::new(prototype_owned_chain(rest))),
    }
}

impl PrototypeEvaluatedValue {
    fn kind(
        &self,
        runtime: &PrototypeRuntime<'_>,
    ) -> Result<PrototypeValueKind, InaccessiblePrototypeValue> {
        runtime.kind(&self.0)
    }

    fn extract_owned(
        &self,
        runtime: &PrototypeRuntime<'_>,
    ) -> Result<PrototypeOwnedValue, InaccessiblePrototypeValue> {
        runtime.extract_owned(&self.0)
    }

    fn render(&self, runtime: &PrototypeRuntime<'_>) -> Result<String, InaccessiblePrototypeValue> {
        runtime.render(&self.0)
    }
}

fn prototype_factory() -> CoreValueFactory {
    CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
}

fn assert_transport_traits<T: Clone + Send + Sync>() {}

#[test]
fn prototype_root_moves_between_threads() {
    assert_transport_traits::<PrototypeValue>();
    assert_transport_traits::<PrototypeEvaluatedValue>();

    let values = prototype_factory();
    let drops = Arc::new(AtomicUsize::new(0));
    let value = PrototypeValue::managed_leaf(&values, 42, drops);
    let worker_value = value.clone();
    let worker_values = values.clone();

    let report = values
        .collect_managed_prototype()
        .expect("the isolated managed prototype should collect");
    assert_eq!(report.root_entries(), 1, "a clone shares one root cell");

    let observed = std::thread::spawn(move || {
        PrototypeRuntime::new(&worker_values).extract_owned(&worker_value)
    })
    .join()
    .expect("prototype observation worker should not panic");
    assert_eq!(
        observed,
        Ok(PrototypeOwnedValue::Managed(prototype_owned_chain(&[42])))
    );
    assert_eq!(
        PrototypeRuntime::new(&values).extract_owned(&value),
        observed
    );
}

#[test]
fn prototype_value_debug_is_opaque() {
    let values = prototype_factory();
    let first_inline = PrototypeValue::inline(&values, 1);
    let second_inline = PrototypeValue::inline(&values, -9);
    let managed = PrototypeValue::managed_leaf(&values, 42, Arc::new(AtomicUsize::new(0)));
    let evaluated = PrototypeRuntime::new(&values)
        .evaluate(&managed)
        .expect("the matching runtime should evaluate its value");

    assert_eq!(format!("{first_inline:?}"), "Value");
    assert_eq!(format!("{second_inline:?}"), "Value");
    assert_eq!(format!("{managed:?}"), "Value");
    assert_eq!(format!("{evaluated:?}"), "EvaluatedValue");

    drop(values);
    assert_eq!(format!("{first_inline:?}"), "Value");
    assert_eq!(format!("{managed:?}"), "Value");
}

#[test]
fn prototype_root_rejects_another_heap() {
    let owner = prototype_factory();
    let foreign = prototype_factory();
    let value = PrototypeValue::managed_leaf(&owner, 11, Arc::new(AtomicUsize::new(0)));

    assert_eq!(
        PrototypeRuntime::new(&owner).extract_owned(&value),
        Ok(PrototypeOwnedValue::Managed(prototype_owned_chain(&[11])))
    );
    assert_eq!(
        PrototypeRuntime::new(&foreign).extract_owned(&value),
        Err(InaccessiblePrototypeValue)
    );
}

#[test]
fn prototype_root_becomes_inert_after_domain_drop() {
    let owner = prototype_factory();
    let domain = Arc::downgrade(owner.value_domain());
    let value = PrototypeValue::managed_leaf(&owner, 17, Arc::new(AtomicUsize::new(0)));

    drop(owner);

    assert!(domain.upgrade().is_none());
    let foreign = prototype_factory();
    assert_eq!(
        PrototypeRuntime::new(&foreign).extract_owned(&value),
        Err(InaccessiblePrototypeValue)
    );
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
    let runtime = PrototypeRuntime::new(&values);
    for (expected, value) in inline.iter().enumerate() {
        assert_eq!(
            runtime.extract_owned(value),
            Ok(PrototypeOwnedValue::Inline(expected as i64))
        );
    }
}

#[test]
fn prototype_inline_value_rejects_another_domain() {
    let owner = prototype_factory();
    let foreign = prototype_factory();
    let domain = Arc::downgrade(owner.value_domain());
    let value = PrototypeValue::inline(&owner, -23);

    assert_eq!(
        PrototypeRuntime::new(&owner).extract_owned(&value),
        Ok(PrototypeOwnedValue::Inline(-23))
    );
    assert_eq!(
        PrototypeRuntime::new(&foreign).extract_owned(&value),
        Err(InaccessiblePrototypeValue)
    );

    drop(owner);
    assert!(domain.upgrade().is_none());
    assert_eq!(
        PrototypeRuntime::new(&foreign).extract_owned(&value),
        Err(InaccessiblePrototypeValue)
    );
}

#[test]
fn prototype_runtime_compares_live_structural_values() {
    let values = prototype_factory();
    let runtime = PrototypeRuntime::new(&values);

    let integer = PrototypeValue::inline(&values, 23);
    let integer_alias = integer.clone();
    let same_integer = PrototypeValue::inline(&values, 23);
    let other_integer = PrototypeValue::inline(&values, 24);

    assert_eq!(
        runtime.structurally_equal(&integer, &integer_alias),
        Ok(true)
    );
    assert_eq!(
        runtime.structurally_equal(&integer, &same_integer),
        Ok(true)
    );
    assert_eq!(
        integer.structurally_equals(&runtime, &other_integer),
        Ok(false)
    );

    let node = PrototypeValue::managed_pair(&values, Arc::new(AtomicUsize::new(0)));
    let node_alias = node.clone();
    let same_node = PrototypeValue::managed_pair(&values, Arc::new(AtomicUsize::new(0)));
    let other_node = PrototypeValue::managed_chain(&values, &[1, 3], Arc::new(AtomicUsize::new(0)));

    assert_eq!(runtime.structurally_equal(&node, &node_alias), Ok(true));
    assert_eq!(runtime.structurally_equal(&node, &same_node), Ok(true));
    assert_eq!(node.structurally_equals(&runtime, &other_node), Ok(false));
    assert_eq!(runtime.structurally_equal(&integer, &node), Ok(false));

    assert_eq!(runtime.kind(&integer), Ok(PrototypeValueKind::Integer));
    assert_eq!(runtime.kind(&node), Ok(PrototypeValueKind::Node));
    assert_eq!(runtime.render(&integer), Ok("integer:23".to_owned()));
    assert_eq!(runtime.render(&node), Ok("node:1(node:2)".to_owned()));

    let evaluated = runtime
        .evaluate(&node)
        .expect("the matching runtime should produce a WHNF witness");
    assert_eq!(evaluated.kind(&runtime), Ok(PrototypeValueKind::Node));
    assert_eq!(
        evaluated.extract_owned(&runtime),
        Ok(PrototypeOwnedValue::Managed(prototype_owned_chain(&[1, 2])))
    );
    assert_eq!(evaluated.render(&runtime), Ok("node:1(node:2)".to_owned()));
}

#[test]
fn prototype_runtime_observation_rejects_foreign_or_inaccessible_value() {
    let owner = prototype_factory();
    let foreign = prototype_factory();
    let domain = Arc::downgrade(owner.value_domain());
    let integer = PrototypeValue::inline(&owner, 5);
    let node = PrototypeValue::managed_leaf(&owner, 8, Arc::new(AtomicUsize::new(0)));
    let evaluated = PrototypeRuntime::new(&owner)
        .evaluate(&node)
        .expect("the owner should produce a WHNF witness");
    let foreign_runtime = PrototypeRuntime::new(&foreign);

    for value in [&integer, &node] {
        assert_eq!(foreign_runtime.kind(value), Err(InaccessiblePrototypeValue));
        assert_eq!(
            foreign_runtime.extract_owned(value),
            Err(InaccessiblePrototypeValue)
        );
        assert_eq!(
            foreign_runtime.render(value),
            Err(InaccessiblePrototypeValue)
        );
        assert!(matches!(
            foreign_runtime.evaluate(value),
            Err(InaccessiblePrototypeValue)
        ));
    }
    assert_eq!(
        integer.structurally_equals(&foreign_runtime, &integer),
        Err(InaccessiblePrototypeValue)
    );
    assert_eq!(
        evaluated.extract_owned(&foreign_runtime),
        Err(InaccessiblePrototypeValue)
    );

    drop(owner);
    assert!(domain.upgrade().is_none());
    assert_eq!(
        evaluated.kind(&foreign_runtime),
        Err(InaccessiblePrototypeValue)
    );
    assert_eq!(
        evaluated.render(&foreign_runtime),
        Err(InaccessiblePrototypeValue)
    );
}

#[test]
fn prototype_owned_extraction_outlives_domain() {
    let domain;
    let (integer, node, integer_rendering, node_rendering) = {
        let values = prototype_factory();
        domain = Arc::downgrade(values.value_domain());
        let runtime = PrototypeRuntime::new(&values);
        let integer = PrototypeValue::inline(&values, -41);
        let node =
            PrototypeValue::managed_chain(&values, &[97, 98, 99], Arc::new(AtomicUsize::new(0)));
        let integer = runtime
            .evaluate(&integer)
            .expect("the integer should be accessible");
        let node = runtime
            .evaluate(&node)
            .expect("the node should be accessible");

        (
            integer
                .extract_owned(&runtime)
                .expect("integer extraction should succeed"),
            node.extract_owned(&runtime)
                .expect("node extraction should succeed"),
            integer
                .render(&runtime)
                .expect("integer rendering should succeed"),
            node.render(&runtime)
                .expect("node rendering should succeed"),
        )
    };

    assert!(domain.upgrade().is_none());
    assert_eq!(integer, PrototypeOwnedValue::Inline(-41));
    assert_eq!(
        node,
        PrototypeOwnedValue::Managed(prototype_owned_chain(&[97, 98, 99]))
    );
    assert_eq!(integer_rendering, "integer:-41");
    assert_eq!(node_rendering, "node:97(node:98(node:99))");
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
