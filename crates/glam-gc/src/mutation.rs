use crate::{Gc, Mutator, Trace, Visitor, trace::ErasedGc};

impl Mutator<'_> {
    /// Reports mutable representation state immediately before and after one
    /// post-publication edge transition.
    ///
    /// This form is useful when both edge sets are properties of a larger
    /// synchronized representation rather than standalone old/new values. The
    /// collector invokes `leaving` against the pre-write state and `adding`
    /// against the post-write state only when its active policy needs those
    /// sides. No semantic graph snapshot is required merely to satisfy Rust's
    /// borrowing rules.
    ///
    /// # Safety
    ///
    /// `owner` must be a live allocation in this mutator's heap. If invoked,
    /// each visitor must synchronously report every managed edge represented
    /// by `state` at that point. `transition` must perform the one logical
    /// transition and leave `state` valid and traceable if it returns.
    #[inline(always)]
    pub unsafe fn with_edge_state_transition<Owner, State, Leaving, Adding, Result>(
        &self,
        owner: &Gc<Owner>,
        state: &mut State,
        leaving: Leaving,
        adding: Adding,
        transition: impl FnOnce(&mut State) -> Result,
    ) -> Result
    where
        Owner: Trace,
        Leaving: for<'visit> Fn(&State, &mut Visitor<'visit>),
        Adding: for<'visit> Fn(&State, &mut Visitor<'visit>),
    {
        #[cfg(debug_assertions)]
        owner.debug_assert_owned_by(self);

        #[cfg(feature = "deterministic-test-hooks")]
        if let Some(probe) = self.heap().edge_transition_probe() {
            return probe.observe_state_transition(self, state, &leaving, &adding, transition);
        }

        #[cfg(not(feature = "deterministic-test-hooks"))]
        let _ = (leaving, adding);

        transition(state)
    }

    /// Reports one managed edge-set transition while a closure performs it.
    ///
    /// `leaving` and `adding` synchronously describe the complete sets of
    /// managed edges removed and installed by `transition`. They may borrow
    /// representation state for this call, but must neither retain `visitor`
    /// nor perform semantic work. The collector decides which side, if any,
    /// it needs to visit. The initial stop-the-world collector visits neither.
    ///
    /// A future SATB collector can visit `leaving` before publication without
    /// paying to walk `adding`; other collector policies may use both sides.
    /// Calling the gateway does not itself prescribe one such policy.
    ///
    /// # Safety
    ///
    /// `owner` must be a live allocation in this mutator's heap. If invoked,
    /// each edge visitor must synchronously report all and only the live
    /// managed pointers represented by its side of this transition. The
    /// closure must perform that one logical transition without letting an
    /// unreported managed edge escape. If it panics after changing the graph,
    /// the containing representation must remain valid; a future barrier may
    /// conservatively retain both edge sets.
    #[inline(always)]
    pub unsafe fn with_edge_transition<Owner, Leaving, Adding, Result>(
        &self,
        owner: &Gc<Owner>,
        leaving: Leaving,
        adding: Adding,
        transition: impl FnOnce() -> Result,
    ) -> Result
    where
        Owner: Trace,
        Leaving: for<'visit> Fn(&mut Visitor<'visit>),
        Adding: for<'visit> Fn(&mut Visitor<'visit>),
    {
        #[cfg(debug_assertions)]
        owner.debug_assert_owned_by(self);

        self.observe_edge_transition(owner.erase(), leaving, adding);
        transition()
    }

    /// Reports one optional managed-edge replacement while a closure performs
    /// it.
    ///
    /// This compatibility convenience delegates to the same edge-set
    /// transition contract. New aggregate representations should use
    /// [`Mutator::with_edge_transition`] directly instead of flattening their
    /// semantic edges into repeated unrelated calls.
    ///
    /// # Safety
    ///
    /// `owner`, `old`, and `new` must be live allocations in this mutator's
    /// heap. `old` must describe the edge represented immediately before
    /// `replace`, and `new` must describe it if `replace` returns. The closure
    /// must perform that one logical replacement without letting an unreported
    /// managed edge escape. If it panics after changing the edge, the caller
    /// must leave the containing representation valid; a future barrier may
    /// conservatively retain both the old and new targets.
    #[inline(always)]
    pub unsafe fn with_edge_replacement<Owner: Trace, Edge: Trace, Result>(
        &self,
        owner: &Gc<Owner>,
        old: Option<&Gc<Edge>>,
        new: Option<&Gc<Edge>>,
        replace: impl FnOnce() -> Result,
    ) -> Result {
        // SAFETY: the caller supplies the owner and exact optional edge before
        // and after the replacement. Each optional borrowed edge is reported
        // precisely once when its side is selected.
        unsafe {
            self.with_edge_transition(
                owner,
                |visitor| {
                    if let Some(old) = old {
                        visitor.visit(old);
                    }
                },
                |visitor| {
                    if let Some(new) = new {
                        visitor.visit(new);
                    }
                },
                replace,
            )
        }
    }

    /// Collector action for the structural edge-transition gateway.
    ///
    /// Keeping this as a separate always-inlined operation makes the initial
    /// no-op policy explicit. Optimized STW builds erase the call, visitor
    /// closures, and pointer arguments.
    #[inline(always)]
    fn observe_edge_transition<Leaving, Adding>(
        &self,
        _owner: ErasedGc,
        leaving: Leaving,
        adding: Adding,
    ) where
        Leaving: for<'visit> Fn(&mut Visitor<'visit>),
        Adding: for<'visit> Fn(&mut Visitor<'visit>),
    {
        #[cfg(feature = "deterministic-test-hooks")]
        if let Some(probe) = self.heap().edge_transition_probe() {
            probe.observe(self, &leaving, &adding);
        }

        #[cfg(not(feature = "deterministic-test-hooks"))]
        let _ = (leaving, adding);
    }

    #[cfg(test)]
    fn observe_edge_transition_for_test<Leaving, Adding>(
        &self,
        observe_leaving: bool,
        observe_adding: bool,
        leaving: Leaving,
        adding: Adding,
    ) -> ObservedEdgeTransition
    where
        Leaving: for<'visit> Fn(&mut Visitor<'visit>),
        Adding: for<'visit> Fn(&mut Visitor<'visit>),
    {
        let mut observed = ObservedEdgeTransition::default();
        if observe_leaving {
            let mut visit = |edge| observed.leaving.push(edge);
            leaving(&mut Visitor::new(&mut visit));
        }
        if observe_adding {
            let mut visit = |edge| observed.adding.push(edge);
            adding(&mut Visitor::new(&mut visit));
        }
        observed
    }
}

#[cfg(test)]
#[derive(Default)]
struct ObservedEdgeTransition {
    leaving: Vec<ErasedGc>,
    adding: Vec<ErasedGc>,
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[cfg(feature = "deterministic-test-hooks")]
    use std::sync::atomic::AtomicUsize;

    #[cfg(feature = "deterministic-test-hooks")]
    use crate::EdgeTransitionObservation;
    use crate::{Gc, Heap, Trace, Visitor};

    struct Leaf {
        _value: u64,
    }

    // SAFETY: `Leaf` has no managed fields.
    unsafe impl Trace for Leaf {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    struct MutableNode {
        edge: Mutex<Option<Gc<Leaf>>>,
    }

    // SAFETY: the mutex contains the node's only managed edge. Tracing is
    // observational with respect to the represented graph and reports the
    // complete synchronized snapshot.
    unsafe impl Trace for MutableNode {
        fn trace(&self, visitor: &mut Visitor<'_>) {
            self.edge
                .lock()
                .expect("test edge mutex should not be poisoned")
                .trace(visitor);
        }
    }

    #[test]
    fn replacement_gateway_executes_the_reported_edge_update_once() {
        let heap = Heap::new();
        heap.with_mutator(|mutator| {
            let leaves = mutator.allocator::<Leaf>().unwrap();
            let nodes = mutator.allocator::<MutableNode>().unwrap();
            let old = leaves.alloc(Leaf { _value: 1 });
            let new = leaves.alloc(Leaf { _value: 2 });
            let owner = nodes.alloc(MutableNode {
                edge: Mutex::new(Some(old.duplicate_in(mutator))),
            });
            // SAFETY: `owner` was allocated in this live arena heap with
            // representation `MutableNode`.
            let owner_value = unsafe { owner.get_unchecked(mutator) };

            let mut replacements = 0;
            // SAFETY: all pointers belong to `heap`; `owner_value.edge`
            // contains `old` before the closure and `new` after its single
            // replacement.
            unsafe {
                mutator.with_edge_replacement(&owner, Some(&old), Some(&new), || {
                    replacements += 1;
                    *owner_value
                        .edge
                        .lock()
                        .expect("test edge mutex should not be poisoned") =
                        Some(new.duplicate_in(mutator));
                });
            }

            assert_eq!(replacements, 1);
            let installed = owner_value
                .edge
                .lock()
                .expect("test edge mutex should not be poisoned");
            assert!(
                installed
                    .as_ref()
                    .is_some_and(|edge| edge.same_allocation_in(&new, mutator))
            );
        });
    }

    #[cfg(feature = "deterministic-test-hooks")]
    #[test]
    fn state_transition_observes_the_locked_pre_and_post_write_graphs() {
        let heap = Heap::new();
        let probe = heap.install_edge_transition_probe(EdgeTransitionObservation::Both);
        let transitions = AtomicUsize::new(0);
        heap.with_mutator(|mutator| {
            let leaves = mutator.allocator::<Leaf>().unwrap();
            let nodes = mutator.allocator::<MutableNode>().unwrap();
            let old = leaves.alloc(Leaf { _value: 1 });
            let new = leaves.alloc(Leaf { _value: 2 });
            let owner = nodes.alloc(MutableNode {
                edge: Mutex::new(Some(old.duplicate_in(mutator))),
            });
            // SAFETY: `owner` is live in this exact heap. Its locked option is
            // its sole outgoing edge before and after the coupled update.
            unsafe {
                let owner_value = owner.get_unchecked(mutator);
                let mut edge = owner_value
                    .edge
                    .lock()
                    .expect("test edge mutex should not be poisoned");
                mutator.with_edge_state_transition(
                    &owner,
                    &mut *edge,
                    |edge, visitor| edge.trace(visitor),
                    |edge, visitor| edge.trace(visitor),
                    |edge| {
                        transitions.fetch_add(1, Ordering::Relaxed);
                        *edge = Some(new.duplicate_in(mutator));
                    },
                );
            }
        });

        assert_eq!(transitions.load(Ordering::Relaxed), 1);
        assert_eq!(probe.records().len(), 1);
        assert_eq!(probe.records()[0].leaving_edges(), 1);
        assert_eq!(probe.records()[0].adding_edges(), 1);
    }

    #[test]
    fn stw_transition_gateway_does_not_walk_either_edge_set() {
        let heap = Heap::new();
        heap.with_mutator(|mutator| {
            let leaves = mutator.allocator::<Leaf>().unwrap();
            let nodes = mutator.allocator::<MutableNode>().unwrap();
            let old = leaves.alloc(Leaf { _value: 1 });
            let new = leaves.alloc(Leaf { _value: 2 });
            let owner = nodes.alloc(MutableNode {
                edge: Mutex::new(Some(old.duplicate_in(mutator))),
            });
            // SAFETY: `owner` is live in this heap and the two visitors
            // describe the exact old and new singleton edges. The closure
            // performs that replacement once.
            unsafe {
                mutator.with_edge_transition(
                    &owner,
                    |_| panic!("STW must not walk leaving edges"),
                    |_| panic!("STW must not walk adding edges"),
                    || {
                        *owner.get_unchecked(mutator).edge.lock().unwrap() =
                            Some(new.duplicate_in(mutator));
                    },
                );
            }
        });
    }

    #[test]
    fn synthetic_observer_selects_distinct_empty_singleton_and_multi_edge_sets() {
        let heap = Heap::new();
        heap.with_mutator(|mutator| {
            let leaves = mutator.allocator::<Leaf>().unwrap();
            let first = leaves.alloc(Leaf { _value: 1 });
            let second = leaves.alloc(Leaf { _value: 2 });
            let third = leaves.alloc(Leaf { _value: 3 });

            let neither = mutator.observe_edge_transition_for_test(
                false,
                false,
                |visitor| {
                    visitor.visit(&first);
                    visitor.visit(&second);
                },
                |visitor| visitor.visit(&third),
            );
            assert!(neither.leaving.is_empty());
            assert!(neither.adding.is_empty());

            let satb = mutator.observe_edge_transition_for_test(
                true,
                false,
                |visitor| {
                    visitor.visit(&first);
                    visitor.visit(&second);
                },
                |visitor| visitor.visit(&third),
            );
            assert_eq!(satb.leaving, [first.erase(), second.erase()]);
            assert!(satb.adding.is_empty());

            let both = mutator.observe_edge_transition_for_test(
                true,
                true,
                |_visitor| (),
                |visitor| visitor.visit(&third),
            );
            assert!(both.leaving.is_empty());
            assert_eq!(both.adding, [third.erase()]);
        });
    }

    #[test]
    fn proposed_addition_may_be_observed_even_when_publication_loses() {
        let heap = Heap::new();
        heap.with_mutator(|mutator| {
            let leaves = mutator.allocator::<Leaf>().unwrap();
            let proposed = leaves.alloc(Leaf { _value: 1 });
            let observed = mutator.observe_edge_transition_for_test(
                false,
                true,
                |_visitor| (),
                |visitor| visitor.visit(&proposed),
            );
            let publication = Err::<(), _>(proposed.duplicate_in(mutator));

            assert_eq!(observed.adding, [proposed.erase()]);
            assert!(
                publication
                    .as_ref()
                    .is_err_and(|edge| edge.same_allocation_in(&proposed, mutator))
            );
        });
    }

    #[cfg(debug_assertions)]
    #[test]
    fn replacement_gateway_rejects_a_foreign_heap_before_mutation() {
        let owner_heap = Heap::new();
        let other_heap = Heap::new();
        let owner = owner_heap.with_mutator(|mutator| {
            mutator
                .allocator::<MutableNode>()
                .unwrap()
                .alloc(MutableNode {
                    edge: Mutex::new(None),
                })
        });
        let ran = AtomicBool::new(false);

        let panic = catch_unwind(AssertUnwindSafe(|| {
            other_heap.with_mutator(|mutator| {
                // SAFETY: this deliberately violates the owner-heap
                // precondition to verify rejection before the closure runs.
                unsafe {
                    mutator.with_edge_replacement::<_, Leaf, _>(&owner, None, None, || {
                        ran.store(true, Ordering::Relaxed);
                    });
                }
            });
        }));

        assert!(panic.is_err());
        assert!(!ran.load(Ordering::Relaxed));
    }
}
