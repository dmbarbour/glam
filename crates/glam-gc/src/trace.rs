use std::ptr::NonNull;

use crate::Gc;

/// Describes every managed edge stored by one managed representation.
///
/// # Safety
///
/// An implementation must synchronously pass every `Gc<_>` edge represented
/// by `self` to `visitor`. It may report the same edge more than once, but may
/// not omit an edge, invent an invalid edge, retain the visitor, or hide a
/// managed pointer in an untraced representation.
///
/// Tracing is observational. It must not mutate the managed graph, collector
/// metadata, or any interior state which changes later trace results. If
/// tracing or its visitor panics, the value must remain valid and safe to trace
/// again from the beginning. Implementations may inspect immediate fields only
/// as needed to select the represented edges.
///
/// This contract does not permit allocation, reclamation, callbacks, or
/// destruction. Later collector phases invoke it only while mutation is
/// excluded.
pub unsafe trait Trace: Send + Sync + 'static {
    /// Optional total collector slot extent requested by this representation.
    ///
    /// `None` requests [`std::mem::size_of::<Self>()`]. `Some(bytes)` requests
    /// that total number of bytes per slot before alignment rounding; it is
    /// not an amount added to the Rust representation. The request must be at
    /// least `size_of::<Self>()`, and metadata discovery rejects a smaller
    /// request during const evaluation. The actual slot stride rounds the
    /// request upward to `align_of::<Self>()`, so it may be larger.
    ///
    /// This policy does not change the Rust layout or alignment of `Self`. One
    /// Rust type has one canonical object-metadata descriptor, so a caller
    /// needing a different policy must use a distinct wrapper type.
    ///
    /// ```compile_fail,E0080
    /// use glam_gc::{Heap, Trace, Visitor};
    ///
    /// struct InvalidRequest(u64);
    ///
    /// // SAFETY: `InvalidRequest` contains no managed edge. Its slot request
    /// // is nevertheless invalid because it is smaller than the value.
    /// unsafe impl Trace for InvalidRequest {
    ///     const REQUESTED_SLOT_SIZE: Option<usize> = Some(1);
    ///
    ///     fn trace(&self, _visitor: &mut Visitor<'_>) {}
    /// }
    ///
    /// let heap = Heap::new();
    /// let _ = heap.allocation_class::<InvalidRequest>();
    /// ```
    const REQUESTED_SLOT_SIZE: Option<usize> = None;

    /// Reports the managed edges represented by this value.
    fn trace(&self, visitor: &mut Visitor<'_>);
}

/// A synchronous receiver for managed edges discovered by [`Trace`].
///
/// The visitor erases only the Rust pointer type. It does not add heap, class,
/// or allocation metadata to the pointer; later typed runs recover those facts
/// from the managed address.
pub struct Visitor<'visit> {
    visit: &'visit mut dyn FnMut(ErasedGc),
}

impl<'visit> Visitor<'visit> {
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "visitor construction remains collector-private until marking"
        )
    )]
    pub(crate) fn new(visit: &'visit mut dyn FnMut(ErasedGc)) -> Self {
        Self { visit }
    }

    /// Reports one managed edge.
    pub fn visit<T: Trace>(&mut self, edge: Gc<T>) {
        (self.visit)(edge.erase());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ErasedGc {
    pointer: NonNull<()>,
}

impl ErasedGc {
    pub(crate) fn new(pointer: NonNull<()>) -> Self {
        Self { pointer }
    }

    #[allow(
        dead_code,
        reason = "typed-run lookup first consumes erased addresses in C2"
    )]
    pub(crate) fn as_ptr(self) -> NonNull<()> {
        self.pointer
    }
}

// SAFETY: `Gc<T>` contains exactly one managed edge, which is reported once.
unsafe impl<T: Trace> Trace for Gc<T> {
    fn trace(&self, visitor: &mut Visitor<'_>) {
        visitor.visit(*self);
    }
}

// SAFETY: `Option<T>` contains exactly the edges represented by its present
// `T`, if any, and tracing does not change the discriminant or payload.
unsafe impl<T: Trace> Trace for Option<T> {
    fn trace(&self, visitor: &mut Visitor<'_>) {
        if let Some(value) = self {
            value.trace(visitor);
        }
    }
}

// SAFETY: the array represents exactly the concatenated edges of its elements;
// immutable iteration visits every element once.
unsafe impl<T: Trace, const N: usize> Trace for [T; N] {
    fn trace(&self, visitor: &mut Visitor<'_>) {
        for value in self {
            value.trace(visitor);
        }
    }
}

// SAFETY: a pair represents exactly the concatenated edges of its two fields.
unsafe impl<First: Trace, Second: Trace> Trace for (First, Second) {
    fn trace(&self, visitor: &mut Visitor<'_>) {
        self.0.trace(visitor);
        self.1.trace(visitor);
    }
}

// SAFETY: these immediate values cannot contain managed edges.
unsafe impl Trace for () {
    fn trace(&self, _visitor: &mut Visitor<'_>) {}
}

// SAFETY: `u32` cannot contain managed edges.
unsafe impl Trace for u32 {
    fn trace(&self, _visitor: &mut Visitor<'_>) {}
}

// SAFETY: `u64` cannot contain managed edges.
unsafe impl Trace for u64 {
    fn trace(&self, _visitor: &mut Visitor<'_>) {}
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use crate::{Gc, Heap};

    use super::{ErasedGc, Trace, Visitor};

    struct Leaf {
        _value: u64,
    }

    // SAFETY: `Leaf` has no managed fields.
    unsafe impl Trace for Leaf {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    enum Node {
        Leaf(Gc<Leaf>),
        Branch {
            children: [Gc<Node>; 2],
            ornaments: (Option<Gc<Leaf>>, [Gc<Leaf>; 2]),
        },
    }

    // SAFETY: each variant reports all and only its explicitly represented
    // managed fields through already-reviewed structural implementations.
    unsafe impl Trace for Node {
        fn trace(&self, visitor: &mut Visitor<'_>) {
            match self {
                Self::Leaf(leaf) => leaf.trace(visitor),
                Self::Branch {
                    children,
                    ornaments,
                } => {
                    children.trace(visitor);
                    ornaments.trace(visitor);
                }
            }
        }
    }

    struct RepresentativeStruct {
        root: Gc<Node>,
        fallback: Option<Gc<Node>>,
    }

    // SAFETY: both managed fields are reported exactly through their
    // structural Trace implementations.
    unsafe impl Trace for RepresentativeStruct {
        fn trace(&self, visitor: &mut Visitor<'_>) {
            self.root.trace(visitor);
            self.fallback.trace(visitor);
        }
    }

    #[test]
    fn manual_struct_and_recursive_enum_traces_match_expected_edges() {
        let heap = Heap::new();
        let leaf_class = heap.allocation_class::<Leaf>().unwrap();
        let node_class = heap.allocation_class::<Node>().unwrap();
        heap.with_mutator(|mutator| {
            let first_leaf = mutator.alloc(&leaf_class, Leaf { _value: 1 });
            let second_leaf = mutator.alloc(&leaf_class, Leaf { _value: 2 });
            let first_node = mutator.alloc(&node_class, Node::Leaf(first_leaf));
            let second_node = mutator.alloc(&node_class, Node::Leaf(second_leaf));

            let branch = Node::Branch {
                children: [first_node, second_node],
                ornaments: (Some(first_leaf), [first_leaf, second_leaf]),
            };
            assert_eq!(
                collect_edges(&branch),
                vec![
                    first_node.erase(),
                    second_node.erase(),
                    first_leaf.erase(),
                    first_leaf.erase(),
                    second_leaf.erase(),
                ]
            );

            let record = RepresentativeStruct {
                root: first_node,
                fallback: Some(second_node),
            };
            assert_eq!(
                collect_edges(&record),
                vec![first_node.erase(), second_node.erase()]
            );

            let leaf_variant = Node::Leaf(first_leaf);
            assert_eq!(collect_edges(&leaf_variant), vec![first_leaf.erase()]);
        });
    }

    #[test]
    fn visitor_panic_leaves_the_value_traceable_from_the_beginning() {
        let heap = Heap::new();
        let leaf_class = heap.allocation_class::<Leaf>().unwrap();
        let node_class = heap.allocation_class::<Node>().unwrap();
        heap.with_mutator(|mutator| {
            let leaf = mutator.alloc(&leaf_class, Leaf { _value: 1 });
            let node = mutator.alloc(&node_class, Node::Leaf(leaf));
            let branch = Node::Branch {
                children: [node, node],
                ornaments: (Some(leaf), [leaf, leaf]),
            };

            let panic = catch_unwind(AssertUnwindSafe(|| {
                let mut visits = 0;
                let mut fail_on_second = |_edge| {
                    visits += 1;
                    assert_ne!(visits, 2, "injected trace visitor panic");
                };
                let mut visitor = Visitor::new(&mut fail_on_second);
                branch.trace(&mut visitor);
            }));
            assert!(panic.is_err());

            assert_eq!(
                collect_edges(&branch),
                vec![
                    node.erase(),
                    node.erase(),
                    leaf.erase(),
                    leaf.erase(),
                    leaf.erase(),
                ]
            );
        });
    }

    fn collect_edges(value: &impl Trace) -> Vec<ErasedGc> {
        let mut edges = Vec::new();
        let mut collect = |edge| edges.push(edge);
        let mut visitor = Visitor::new(&mut collect);
        value.trace(&mut visitor);
        edges
    }
}
