use std::sync::Arc;

use crate::Mutator;

/// One shareable, runtime-local managed-value domain.
///
/// The C0 heap is deliberately empty. Its private shared allocation establishes
/// ownership and thread-sharing shape without committing the collector to an
/// allocator or coordination representation.
#[derive(Clone, Default)]
pub struct Heap {
    inner: Arc<HeapInner>,
}

impl Heap {
    /// Creates an empty heap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `operation` inside a scoped mutator region for this heap.
    ///
    /// C0 has no managed storage and therefore no collector to exclude. This
    /// method establishes only the lifetime and entry shape which C1 and C3
    /// will harden.
    pub fn with_mutator<R>(&self, operation: impl for<'heap> FnOnce(&Mutator<'heap>) -> R) -> R {
        let mutator = Mutator::new(&self.inner);
        operation(&mutator)
    }
}

#[derive(Default)]
pub(crate) struct HeapInner;
