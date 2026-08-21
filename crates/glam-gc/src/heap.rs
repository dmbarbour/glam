#[cfg(debug_assertions)]
use std::any::{TypeId, type_name};
use std::ptr::NonNull;
use std::sync::Arc;
#[cfg(debug_assertions)]
use std::sync::Mutex;

use crate::Mutator;

/// One shareable, runtime-local managed-value domain.
///
/// C1A's heap owns only a debug/test allocation registry. Prototype payloads
/// are deliberately leaked, so this still does not commit the collector to an
/// allocator, a reclamation policy, or coordination state.
#[derive(Clone, Default)]
pub struct Heap {
    inner: Arc<HeapInner>,
}

impl Heap {
    /// Creates an empty prototype heap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `operation` inside a scoped mutator region for this heap.
    ///
    /// C1A has no collector to exclude. The token is nevertheless qualified by
    /// this heap and is required for every prototype allocation and access.
    pub fn with_mutator<R>(&self, operation: impl for<'heap> FnOnce(&Mutator<'heap>) -> R) -> R {
        let mutator = Mutator::new(&self.inner);
        operation(&mutator)
    }
}

#[derive(Default)]
pub(crate) struct HeapInner {
    #[cfg(debug_assertions)]
    prototype_allocations: Mutex<Vec<PrototypeAllocationRecord>>,
}

impl HeapInner {
    pub(crate) fn register_prototype<T: 'static>(&self, pointer: NonNull<T>) {
        #[cfg(debug_assertions)]
        self.prototype_allocations
            .lock()
            .expect("prototype allocation registry should not be poisoned")
            .push(PrototypeAllocationRecord {
                address: pointer.as_ptr().cast::<()>() as usize,
                type_id: TypeId::of::<T>(),
                type_name: type_name::<T>(),
            });

        #[cfg(not(debug_assertions))]
        let _ = pointer;
    }

    pub(crate) fn debug_assert_access<T: 'static>(&self, pointer: NonNull<T>) {
        #[cfg(debug_assertions)]
        {
            let address = pointer.as_ptr().cast::<()>() as usize;
            let allocations = self
                .prototype_allocations
                .lock()
                .expect("prototype allocation registry should not be poisoned");
            let record = allocations
                .iter()
                .find(|record| record.address == address)
                .copied();
            drop(allocations);
            let record =
                record.unwrap_or_else(|| panic!("managed pointer does not belong to this heap"));

            assert!(
                record.type_id == TypeId::of::<T>(),
                "managed pointer has representation `{}`, not requested `{}`",
                record.type_name,
                type_name::<T>()
            );
        }

        #[cfg(not(debug_assertions))]
        let _ = pointer;
    }
}

#[cfg(debug_assertions)]
#[derive(Clone, Copy)]
struct PrototypeAllocationRecord {
    address: usize,
    type_id: TypeId,
    type_name: &'static str,
}
