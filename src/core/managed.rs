use glam_gc::{Allocator, Gc, Mutator, Root, Trace, UnsupportedLayout};
#[cfg(test)]
use std::sync::{Arc, Weak};

use super::CoreValueFactory;
#[cfg(test)]
use super::RuntimeValueDomain;

/// Initial minimum slot extent for Glam-owned managed representations.
///
/// This pointer-sized baseline avoids pathological sub-word typed runs without
/// selecting the eventual tagged-value alignment or a larger padding policy.
/// The value-representation plan owns any later measured change.
const MANAGED_SLOT_SIZE_FLOOR: usize = std::mem::size_of::<usize>();

const _: () = assert!(MANAGED_SLOT_SIZE_FLOOR.is_power_of_two());

/// Returns the total collector slot extent requested by one Glam-managed Rust
/// representation before the collector applies its normal alignment rounding.
///
/// Every production `Trace` implementation introduced by the integration
/// plan uses this policy instead of selecting ad hoc padding. Rust type layout
/// remains authoritative when it is larger than the current floor.
pub(crate) const fn managed_slot_extent<T>() -> usize {
    let natural = std::mem::size_of::<T>();
    assert!(natural != 0, "Glam does not admit zero-sized managed nodes");
    if natural < MANAGED_SLOT_SIZE_FLOOR {
        MANAGED_SLOT_SIZE_FLOOR
    } else {
        natural
    }
}

const _: () = assert!(managed_slot_extent::<usize>() == MANAGED_SLOT_SIZE_FLOOR);

/// Factory-qualified access to one admitted managed-allocation region.
///
/// The scope deliberately exposes neither the runtime heap nor the collector
/// mutator. Managed allocation classes remain borrowed from this region. A
/// managed pointer may leave only as an exactly traced managed edge, while a
/// root is the only standalone access handle intended to survive the region.
pub(crate) struct CoreValueAllocationScope<'scope> {
    mutator: &'scope Mutator<'scope>,
}

/// One type's allocation path borrowed from a factory allocation scope.
///
/// Callers may reuse this value for a batch of allocations without repeating
/// class discovery. Its lifetime prevents retaining an allocator after the
/// current mutator region closes.
pub(crate) struct CoreValueAllocator<'scope, T: Trace> {
    allocator: Allocator<'scope, T>,
}

/// Non-owning provenance for an inline value in the isolated public-root
/// prototype.
///
/// This is deliberately private verification scaffolding until I2 fixes the
/// production wrapper. Pointer identity is authoritative inside one process;
/// the weak reference neither preserves nor revives the value domain.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct CoreValueDomainWitness(Weak<RuntimeValueDomain>);

impl CoreValueFactory {
    /// Runs one bounded managed-allocation region in this factory's value
    /// domain.
    ///
    /// This is the sole factory-level bridge to the collector. The callback
    /// receives only allocation, rooting, and rooted-access operations; it
    /// cannot retain the heap, mutator, or a typed allocator.
    #[allow(
        dead_code,
        reason = "Phase I1C installs the allocation seam before production managed values"
    )]
    pub(crate) fn with_managed_values<R>(
        &self,
        operation: impl for<'scope> FnOnce(CoreValueAllocationScope<'scope>) -> R,
    ) -> R {
        self.domain
            .heap
            .with_mutator(|mutator| operation(CoreValueAllocationScope { mutator }))
    }

    #[cfg(test)]
    pub(crate) fn managed_domain_witness(&self) -> CoreValueDomainWitness {
        CoreValueDomainWitness(Arc::downgrade(&self.domain))
    }

    #[cfg(test)]
    pub(crate) fn owns_managed_domain_witness(&self, witness: &CoreValueDomainWitness) -> bool {
        Weak::ptr_eq(&witness.0, &Arc::downgrade(&self.domain))
    }

    #[cfg(test)]
    pub(crate) fn owns_managed_root<T: Trace>(&self, root: &Root<T>) -> bool {
        self.domain.heap.owns(root)
    }

    #[cfg(test)]
    pub(crate) fn managed_statistics(&self) -> glam_gc::HeapStatistics {
        self.domain.heap.statistics()
    }

    #[cfg(test)]
    pub(crate) fn collect_managed_prototype(
        &self,
    ) -> Result<glam_gc::CollectionReport, glam_gc::CollectionError> {
        self.domain.heap.collect_full()
    }
}

impl CoreValueAllocationScope<'_> {
    /// Discovers or reuses one heap-local allocation class for this region.
    #[allow(
        dead_code,
        reason = "Phase I1C installs the allocation seam before production managed values"
    )]
    pub(crate) fn allocator<T: Trace>(
        &self,
    ) -> Result<CoreValueAllocator<'_, T>, UnsupportedLayout> {
        self.mutator
            .allocator()
            .map(|allocator| CoreValueAllocator { allocator })
    }

    /// Publishes an external root before a managed pointer leaves this region.
    #[allow(
        dead_code,
        reason = "Phase I1C installs the rooting seam before public roots migrate in I2"
    )]
    pub(crate) fn root<T: Trace>(&self, value: Gc<T>) -> Root<T> {
        self.mutator.root(value)
    }

    /// Borrows one same-domain root while this region supplies access
    /// authority.
    #[allow(
        dead_code,
        reason = "Phase I1C installs the root-access seam before public roots migrate in I2"
    )]
    pub(crate) fn get<'access, T: Trace>(&'access self, root: &Root<T>) -> &'access T {
        root.get(self.mutator)
    }

    /// Borrows one exact managed edge discovered from an already authorized,
    /// rooted prototype node.
    ///
    /// # Safety
    ///
    /// `value` must be a live, exactly typed edge in this scope's heap. This
    /// test-only gateway models the internal trace invariant; unlike a root,
    /// `Gc<T>` does not carry independently checkable release-build
    /// provenance.
    #[cfg(test)]
    pub(crate) unsafe fn get_traced_edge<T: Trace>(&self, value: Gc<T>) -> &T {
        // SAFETY: the caller supplies the exact same-heap traced-edge proof;
        // this scope's mutator excludes collection for the returned borrow.
        unsafe { value.get_unchecked(self.mutator) }
    }
}

impl<T: Trace> CoreValueAllocator<'_, T> {
    /// Allocates through the class already selected for this region.
    #[allow(
        dead_code,
        reason = "Phase I1C installs the allocation seam before production managed values"
    )]
    pub(crate) fn alloc(&self, value: T) -> Gc<T> {
        self.allocator.alloc(value)
    }
}
