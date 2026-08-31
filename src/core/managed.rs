use glam_gc::{Allocator, Gc, Mutator, Root, Trace, UnsupportedLayout};
use std::sync::{Arc, Weak};

use super::{CoreValueFactory, RuntimeValueDomain};
use crate::runtime::EvaluationRuntimeId;

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
/// mutator. Managed allocation classes and borrows remain bounded by this
/// region. Because bare `Gc<T>` is intentionally lifetime-free for use as an
/// interior edge, integration must still ensure that any pointer leaving the
/// region is installed as an exactly traced edge or published as a root.
pub(crate) struct CoreValueAllocationScope<'scope> {
    mutator: &'scope Mutator<'scope>,
}

/// Domain-qualified managed access for one bounded runtime operation.
///
/// This is the foundational I3 authority. It combines I1's narrow allocation
/// scope with the exact value domain which admitted its mutator. Subsystems
/// derive shorter-lived views from this carrier rather than entering the heap
/// independently.
#[allow(
    dead_code,
    reason = "I3A.1 establishes the carrier before I3B.1 migrates evaluator access"
)]
pub(crate) struct RuntimeValueAccess<'scope> {
    domain: &'scope RuntimeValueDomain,
    scope: CoreValueAllocationScope<'scope>,
}

/// Weak, non-retaining authority to reopen bounded observation in one value
/// domain.
///
/// Public evaluated-value witnesses carry this handle so extraction can remain
/// ergonomic without retaining the heap or holding a mutator between calls.
/// Bare public values deliberately do not expose it.
#[derive(Clone)]
pub(crate) struct RuntimeValueObserver {
    runtime: EvaluationRuntimeId,
    domain: Weak<RuntimeValueDomain>,
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
/// This is deliberately private verification scaffolding for I2's selected
/// wrapper contract. I4F.2 enacts that contract in the production facade.
/// Pointer identity is authoritative inside one process; the weak reference
/// neither preserves nor revives the value domain.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct CoreValueDomainWitness(Weak<RuntimeValueDomain>);

impl CoreValueFactory {
    /// Runs one bounded managed-allocation region in this factory's value
    /// domain.
    ///
    /// This is I1's construction-oriented factory bridge to the collector.
    /// The callback receives only allocation, rooting, and rooted-access
    /// operations; it cannot retain the heap, mutator, or a typed allocator.
    /// I3 evaluator work instead derives its domain-qualified authority through
    /// [`Self::with_runtime_value_access`].
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

    /// Opens one domain-qualified managed-access region.
    ///
    /// The higher-ranked callback prevents the access carrier, its mutator,
    /// allocators, and managed borrows from escaping. I3 scheduler poll
    /// contexts use this entry to derive evaluator-specific authority.
    #[allow(
        dead_code,
        reason = "I3A establishes scoped authority before I3B migrates production evaluator substeps"
    )]
    pub(crate) fn with_runtime_value_access<R>(
        &self,
        operation: impl for<'scope> FnOnce(RuntimeValueAccess<'scope>) -> R,
    ) -> R {
        self.domain.heap.with_mutator(|mutator| {
            operation(RuntimeValueAccess {
                domain: self.domain.as_ref(),
                scope: CoreValueAllocationScope { mutator },
            })
        })
    }

    /// Issues one weak observer for values successfully evaluated by this
    /// exact domain.
    pub(crate) fn runtime_value_observer(&self) -> RuntimeValueObserver {
        RuntimeValueObserver {
            runtime: self.runtime_id(),
            domain: Arc::downgrade(&self.domain),
        }
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
    pub(crate) fn collect_managed_for_test(
        &self,
    ) -> Result<glam_gc::CollectionReport, glam_gc::CollectionError> {
        self.domain.heap.collect_full()
    }
}

impl RuntimeValueObserver {
    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    /// Returns whether this observer was issued by `values`' exact domain.
    pub(crate) fn belongs_to(&self, values: &CoreValueFactory) -> bool {
        Weak::ptr_eq(&self.domain, &Arc::downgrade(&values.domain))
    }

    /// Returns whether both observers name the exact same value domain.
    ///
    /// Runtime IDs are globally unique and remain the architectural identity.
    /// Comparing the already-held weak routes directly is merely a cheap
    /// internal consistency check and does not turn domain pointers into a
    /// second public identity scheme.
    pub(crate) fn same_domain(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.domain, &other.domain)
    }

    #[cfg(test)]
    pub(crate) fn is_live(&self) -> bool {
        self.domain.strong_count() != 0
    }

    /// Temporarily upgrades observation authority without creating a durable
    /// heap owner. The returned factory has no compilation-local extensions;
    /// scalar and structural extraction needs only runtime-owned values.
    pub(crate) fn upgrade(&self) -> Option<CoreValueFactory> {
        self.domain.upgrade().map(|domain| CoreValueFactory {
            domain,
            local_extensions: None,
        })
    }
}

#[allow(
    dead_code,
    reason = "I3A.1 establishes carrier operations before I3B.1 migrates evaluator access"
)]
impl RuntimeValueAccess<'_> {
    /// Returns whether `values` is another authorized view of this exact value
    /// domain. Runtime IDs are globally unique; direct domain comparison keeps
    /// this private check aligned with the heap authority already in hand and
    /// avoids treating an integer ID as the capability itself.
    pub(crate) fn belongs_to(&self, values: &CoreValueFactory) -> bool {
        std::ptr::eq(self.domain, values.domain.as_ref())
    }

    /// Discovers or reuses one heap-local allocation class for this region.
    pub(crate) fn allocator<T: Trace>(
        &self,
    ) -> Result<CoreValueAllocator<'_, T>, UnsupportedLayout> {
        self.scope.allocator()
    }

    /// Publishes a root before a managed pointer leaves this region.
    pub(crate) fn root<T: Trace>(&self, value: Gc<T>) -> Root<T> {
        self.scope.root(value)
    }

    /// Borrows one same-domain root under this region's mutator authority.
    pub(crate) fn get<'access, T: Trace>(&'access self, root: &Root<T>) -> &'access T {
        self.scope.get(root)
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
        reason = "Phase I1C installs the rooting seam before durable and public roots migrate in I4F"
    )]
    pub(crate) fn root<T: Trace>(&self, value: Gc<T>) -> Root<T> {
        self.mutator.root(value)
    }

    /// Borrows one same-domain root while this region supplies access
    /// authority.
    #[allow(
        dead_code,
        reason = "Phase I1C installs the root-access seam before the public-root switch in I4F.2"
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
