#[cfg(debug_assertions)]
use std::any::{TypeId, type_name};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use crate::{Mutator, arena::Arena};

/// One shareable, runtime-local managed-value domain.
///
/// C2A's heap owns aligned arena/run topology plus C1A's debug/test prototype
/// allocation registry. Prototype payloads remain deliberately leaked outside
/// those arenas, so the heap still has no managed payload allocator,
/// reclamation policy, or collector coordination state.
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
    /// C2A still has no collector to exclude. The token is nevertheless
    /// qualified by this heap and is required for every prototype allocation
    /// and access.
    pub fn with_mutator<R>(&self, operation: impl for<'heap> FnOnce(&Mutator<'heap>) -> R) -> R {
        let mutator = Mutator::new(&self.inner);
        operation(&mutator)
    }
}

#[derive(Default)]
pub(crate) struct HeapInner {
    #[allow(
        dead_code,
        reason = "C2A reserves heap-owned arenas before C2B class discovery consumes them"
    )]
    arena: Mutex<Arena>,
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

#[cfg(test)]
mod tests {
    use crate::arena::Arena;

    use super::Heap;

    #[test]
    fn heap_owned_arenas_reject_another_heaps_addresses() {
        let first = Heap::new();
        let second = Heap::new();

        let address = {
            let mut arena = first
                .inner
                .arena
                .lock()
                .expect("test arena should not be poisoned");
            let chunk = arena.reserve_chunk().unwrap();
            arena.run_address(chunk, 0).unwrap().address()
        };

        assert!(
            first
                .inner
                .arena
                .lock()
                .unwrap()
                .find_run(address)
                .is_some()
        );
        assert!(
            second
                .inner
                .arena
                .lock()
                .unwrap()
                .find_run(address)
                .is_none()
        );
    }

    const _: fn() = || {
        fn assert_send<T: Send>() {}
        assert_send::<Arena>();
    };
}
