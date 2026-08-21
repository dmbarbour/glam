use crate::{Gc, Mutator, Trace, trace::ErasedGc};

impl Mutator<'_> {
    /// Runs one replacement of a managed edge through the collector gateway.
    ///
    /// The initial stop-the-world collector performs no barrier action. The
    /// explicit owner/old/new shape preserves one audited integration point
    /// for a future incremental or generational collector without imposing
    /// bookkeeping on the current design.
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
    pub unsafe fn replace_edge<Owner: Trace, Edge: Trace, Result>(
        &self,
        owner: Gc<Owner>,
        old: Option<Gc<Edge>>,
        new: Option<Gc<Edge>>,
        replace: impl FnOnce() -> Result,
    ) -> Result {
        #[cfg(debug_assertions)]
        {
            owner.debug_assert_owned_by(self);
            if let Some(old) = old {
                old.debug_assert_owned_by(self);
            }
            if let Some(new) = new {
                new.debug_assert_owned_by(self);
            }
        }

        self.before_edge_replacement(owner.erase(), old.map(Gc::erase), new.map(Gc::erase));
        replace()
    }

    /// Collector action for the structural replacement gateway.
    ///
    /// Keeping this as a separate always-inlined operation makes the initial
    /// no-op policy explicit. Optimized STW builds erase the call and all three
    /// pointer arguments.
    #[inline(always)]
    fn before_edge_replacement(
        &self,
        _owner: ErasedGc,
        _old: Option<ErasedGc>,
        _new: Option<ErasedGc>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

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
            let edge = *self
                .edge
                .lock()
                .expect("test edge mutex should not be poisoned");
            edge.trace(visitor);
        }
    }

    #[test]
    fn replacement_gateway_executes_the_reported_edge_update_once() {
        let heap = Heap::new();
        heap.with_mutator(|mutator| {
            let old = mutator.alloc(Leaf { _value: 1 });
            let new = mutator.alloc(Leaf { _value: 2 });
            let owner = mutator.alloc(MutableNode {
                edge: Mutex::new(Some(old)),
            });
            // SAFETY: `owner` was allocated in this live prototype heap with
            // representation `MutableNode`.
            let owner_value = unsafe { owner.get_unchecked(mutator) };

            let mut replacements = 0;
            // SAFETY: all pointers belong to `heap`; `owner_value.edge`
            // contains `old` before the closure and `new` after its single
            // replacement.
            unsafe {
                mutator.replace_edge(owner, Some(old), Some(new), || {
                    replacements += 1;
                    *owner_value
                        .edge
                        .lock()
                        .expect("test edge mutex should not be poisoned") = Some(new);
                });
            }

            assert_eq!(replacements, 1);
            assert_eq!(
                *owner_value
                    .edge
                    .lock()
                    .expect("test edge mutex should not be poisoned"),
                Some(new)
            );
        });
    }

    #[cfg(debug_assertions)]
    #[test]
    fn replacement_gateway_rejects_a_foreign_heap_before_mutation() {
        let owner_heap = Heap::new();
        let other_heap = Heap::new();
        let owner = owner_heap.with_mutator(|mutator| {
            mutator.alloc(MutableNode {
                edge: Mutex::new(None),
            })
        });
        let ran = AtomicBool::new(false);

        let panic = catch_unwind(AssertUnwindSafe(|| {
            other_heap.with_mutator(|mutator| {
                // SAFETY: this deliberately violates the owner-heap
                // precondition to verify rejection before the closure runs.
                unsafe {
                    mutator.replace_edge::<_, Leaf, _>(owner, None, None, || {
                        ran.store(true, Ordering::Relaxed);
                    });
                }
            });
        }));

        assert!(panic.is_err());
        assert!(!ran.load(Ordering::Relaxed));
    }
}
