use glam_gc::{Allocator, Gc, Mutator, Root, Trace, UnsupportedLayout};
use std::any::Any;
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

/// The reviewed destruction policy for one Glam-managed representation.
///
/// This record is deliberately smaller than the durable ownership ledger. It
/// is the compile-time admission token proving that the ledger's direct and
/// transitive destruction fields were completed before a type reached Glam's
/// collector gateway.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "managed drop records are compile-time admission evidence, inspected by family audits"
)]
pub(crate) struct ManagedDropRecord {
    family: &'static str,
    source: &'static str,
    direct_review: &'static str,
    transitive_review: &'static str,
}

impl ManagedDropRecord {
    /// Records a representation with no Rust drop glue.
    pub(crate) const fn no_drop(family: &'static str, source: &'static str) -> Self {
        Self::reviewed(family, source, "no drop glue", "no transitive drop glue")
    }

    /// Records reviewed passive direct and transitive destruction.
    #[allow(
        dead_code,
        reason = "I4.0 establishes the constructor before production drop-bearing families migrate"
    )]
    pub(crate) const fn passive(
        family: &'static str,
        source: &'static str,
        direct_review: &'static str,
        transitive_review: &'static str,
    ) -> Self {
        Self::reviewed(family, source, direct_review, transitive_review)
    }

    const fn reviewed(
        family: &'static str,
        source: &'static str,
        direct_review: &'static str,
        transitive_review: &'static str,
    ) -> Self {
        assert!(!family.is_empty(), "managed family name must be recorded");
        assert!(!source.is_empty(), "managed family source must be recorded");
        assert!(
            !direct_review.is_empty(),
            "managed direct destruction must be reviewed"
        );
        assert!(
            !transitive_review.is_empty(),
            "managed transitive destruction must be reviewed"
        );
        Self {
            family,
            source,
            direct_review,
            transitive_review,
        }
    }

    #[cfg(test)]
    const fn fields(self) -> (&'static str, &'static str, &'static str, &'static str) {
        (
            self.family,
            self.source,
            self.direct_review,
            self.transitive_review,
        )
    }
}

/// Admits one reviewed representation family to Glam's managed heap.
///
/// `Trace` alone describes edges to the generic collector. This additional
/// private boundary proves that Glam has also completed the family's stable
/// destruction record. All allocation through a runtime value domain requires
/// this trait, so a mechanically traceable type cannot accidentally become a
/// Glam-managed family before its lifecycle review.
///
/// # Safety
///
/// The associated record must identify the stable family and source owning
/// the representation. Direct `Drop` and every transitive field destructor
/// must be passive: they may release ordinary Rust resources, but must not
/// obtain or invoke a Glam runtime, value domain, heap, evaluator, scheduler,
/// diagnostic/event service, host callback, or equivalent active semantic
/// capability. Destruction must not observe or preserve a `Gc` edge held by
/// the dying representation.
///
/// Any external owner which performs active retirement must remain outside
/// the managed graph and hold its runtime capability and registered roots
/// independently. Adding such an owner, or any exception to passive managed
/// destruction, requires a separate design review.
pub(crate) unsafe trait ManagedFamily: Trace {
    const DROP_RECORD: ManagedDropRecord;
}

/// Test-only admission gate for the complete compatibility value shell.
///
/// This wrapper deliberately reports no managed edges: before I4F.2d, every
/// recursive compatibility edge remains ordinary Rust ownership. Its only
/// purpose is to prove that those owners now have passive destruction before
/// the production managed node is introduced.
#[cfg(test)]
pub(crate) struct ClosedCompatibilityValue {
    value: super::Value,
    drops: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl ClosedCompatibilityValue {
    pub(crate) fn new(value: super::Value, drops: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self {
            value,
            drops: Arc::clone(drops),
        }
    }

    pub(crate) fn value(&self) -> &super::Value {
        &self.value
    }
}

#[cfg(test)]
impl Drop for ClosedCompatibilityValue {
    fn drop(&mut self) {
        self.drops
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

// SAFETY: the compatibility representation contains no `Gc` edge before the
// production switch. I4B-I4E exhaustively inventory its ordinary Rust value
// and net ownership; I4F.2b.1-.3 moved every active destructor behind passive
// external-owner handles. The test fixture therefore has zero managed edges.
#[cfg(test)]
unsafe impl Trace for ClosedCompatibilityValue {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, _visitor: &mut glam_gc::Visitor<'_>) {}
}

// SAFETY: direct destruction only updates an external atomic counter. The
// wrapped compatibility value recursively releases passive Rust ownership;
// callbacks, reservations, and opaque payloads remain in the runtime's
// external-owner registry and are not retired by this destructor.
#[cfg(test)]
unsafe impl ManagedFamily for ClosedCompatibilityValue {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "I4F.2b closed compatibility value fixture",
        "src/core/managed.rs",
        "direct Drop updates only an external atomic counter",
        "compatibility Value ownership is passive after active-owner extraction",
    );
}

/// The reviewed containment policy for one type-erased opaque payload.
///
/// Unlike [`ManagedDropRecord`], this is not collector admission. It prevents
/// `OpaqueValue`'s `Any` boundary from accepting a new family merely because
/// the Rust type is `Send + Sync`. I10 decides whether any admitted external
/// family remains outside the managed graph or receives a separate exact
/// managed representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "opaque records are compile-time admission evidence, inspected by containment audits"
)]
pub(crate) struct OpaquePayloadRecord {
    family: &'static str,
    source: &'static str,
    ownership: &'static str,
}

impl OpaquePayloadRecord {
    /// Records a payload containing identity/data only, with no Glam value or
    /// runtime capability reachable through its fields.
    pub(crate) const fn edge_free(family: &'static str, source: &'static str) -> Self {
        Self::reviewed(family, source, "edge-free token")
    }

    /// Records an external capability whose lifecycle remains outside the
    /// collector and therefore requires the later I9/I10 ownership audit.
    pub(crate) const fn external(family: &'static str, source: &'static str) -> Self {
        Self::reviewed(family, source, "external capability")
    }

    const fn reviewed(family: &'static str, source: &'static str, ownership: &'static str) -> Self {
        assert!(!family.is_empty(), "opaque payload family must be recorded");
        assert!(!source.is_empty(), "opaque payload source must be recorded");
        assert!(
            !ownership.is_empty(),
            "opaque payload ownership must be recorded"
        );
        Self {
            family,
            source,
            ownership,
        }
    }

    #[cfg(test)]
    const fn fields(self) -> (&'static str, &'static str, &'static str) {
        (self.family, self.source, self.ownership)
    }
}

/// Admits one reviewed family to `OpaqueValue`'s type-erased storage.
///
/// # Safety
///
/// The payload must contain no bare `Gc`, unrooted recursive `core::Value`,
/// `RuntimeValueRoot`, or other unreported managed edge. `edge_free` families
/// contain no Glam value/runtime capability at all. `external` families may
/// carry an audited host lifecycle capability, but must not be treated as a
/// collector-managed leaf; I9/I10 must reconcile their ownership before the
/// production managed value switch.
pub(crate) unsafe trait OpaquePayloadFamily: Any + Send + Sync {
    const PAYLOAD_RECORD: OpaquePayloadRecord;
}

mod external_owners;
pub(crate) use external_owners::{ExternalOwnerHandle, ExternalOwnerRegistry};
mod value_node;

// SAFETY: this is the existing scalar collector-access probe. It has no
// managed edge, no drop glue, and no active capability. Production value
// families receive their own explicit admissions in their migration phases.
unsafe impl ManagedFamily for u64 {
    const DROP_RECORD: ManagedDropRecord =
        ManagedDropRecord::no_drop("collector access scalar probe", "src/core/managed.rs");
}

#[cfg(test)]
// SAFETY: scalar opaque fixtures contain no managed edge or runtime
// capability. Production opaque families have explicit implementations at
// their owning modules.
unsafe impl OpaquePayloadFamily for u64 {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::edge_free("opaque scalar test fixture", "src/core/managed.rs");
}

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
pub(crate) struct RuntimeValueAccess<'scope> {
    domain: &'scope RuntimeValueDomain,
    #[allow(
        dead_code,
        reason = "I4 introduces production managed allocation, rooting, and borrowing through this scope"
    )]
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
pub(crate) struct CoreValueAllocator<'scope, T: ManagedFamily> {
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
    pub(crate) fn owns_managed_root<T: ManagedFamily>(&self, root: &Root<T>) -> bool {
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

impl RuntimeValueAccess<'_> {
    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.domain.runtime
    }

    /// Returns whether `values` is another authorized view of this exact value
    /// domain. Runtime IDs are globally unique; direct domain comparison keeps
    /// this private check aligned with the heap authority already in hand and
    /// avoids treating an integer ID as the capability itself.
    pub(crate) fn belongs_to(&self, values: &CoreValueFactory) -> bool {
        std::ptr::eq(self.domain, values.domain.as_ref())
    }

    /// Returns whether `observer` routes back to this admitted value domain.
    pub(crate) fn admits(&self, observer: &RuntimeValueObserver) -> bool {
        std::ptr::eq(self.domain, observer.domain.as_ptr())
    }

    /// Discovers or reuses one heap-local allocation class for this region.
    #[allow(
        dead_code,
        reason = "I4 introduces production managed allocation through runtime-qualified access"
    )]
    pub(crate) fn allocator<T: ManagedFamily>(
        &self,
    ) -> Result<CoreValueAllocator<'_, T>, UnsupportedLayout> {
        self.scope.allocator()
    }

    /// Publishes a root before a managed pointer leaves this region.
    #[allow(
        dead_code,
        reason = "I4F introduces production managed roots through runtime-qualified access"
    )]
    pub(crate) fn root<T: ManagedFamily>(&self, value: Gc<T>) -> Root<T> {
        self.scope.root(value)
    }

    /// Borrows one same-domain root under this region's mutator authority.
    #[allow(
        dead_code,
        reason = "I4F introduces production managed-root observation through runtime-qualified access"
    )]
    pub(crate) fn get<'access, T: ManagedFamily>(&'access self, root: &Root<T>) -> &'access T {
        self.scope.get(root)
    }
}

impl CoreValueAllocationScope<'_> {
    /// Discovers or reuses one heap-local allocation class for this region.
    #[allow(
        dead_code,
        reason = "Phase I1C installs the allocation seam before production managed values"
    )]
    pub(crate) fn allocator<T: ManagedFamily>(
        &self,
    ) -> Result<CoreValueAllocator<'_, T>, UnsupportedLayout> {
        // Naming the mandatory associated record keeps admission tied to
        // class discovery without adding runtime work after optimization.
        let _ = T::DROP_RECORD;
        self.mutator
            .allocator()
            .map(|allocator| CoreValueAllocator { allocator })
    }

    /// Publishes an external root before a managed pointer leaves this region.
    #[allow(
        dead_code,
        reason = "Phase I1C installs the rooting seam before durable and public roots migrate in I4F"
    )]
    pub(crate) fn root<T: ManagedFamily>(&self, value: Gc<T>) -> Root<T> {
        self.mutator.root(value)
    }

    /// Borrows one same-domain root while this region supplies access
    /// authority.
    #[allow(
        dead_code,
        reason = "Phase I1C installs the root-access seam before the public-root switch in I4F.2"
    )]
    pub(crate) fn get<'access, T: ManagedFamily>(&'access self, root: &Root<T>) -> &'access T {
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
    pub(crate) unsafe fn get_traced_edge<T: ManagedFamily>(&self, value: Gc<T>) -> &T {
        // SAFETY: the caller supplies the exact same-heap traced-edge proof;
        // this scope's mutator excludes collection for the returned borrow.
        unsafe { value.get_unchecked(self.mutator) }
    }
}

impl<T: ManagedFamily> CoreValueAllocator<'_, T> {
    /// Allocates through the class already selected for this region.
    #[allow(
        dead_code,
        reason = "Phase I1C installs the allocation seam before production managed values"
    )]
    pub(crate) fn alloc(&self, value: T) -> Gc<T> {
        self.allocator.alloc(value)
    }
}

#[cfg(test)]
mod value_shell;

#[cfg(test)]
mod containment_inventory;

#[cfg(test)]
mod active_owner_inventory;

#[cfg(test)]
mod durable_owner_inventory;

mod payload_edges;

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Weak};

    use glam_gc::{Gc, Root, Trace, Visitor};

    use super::{ManagedDropRecord, ManagedFamily, OpaquePayloadFamily, OpaquePayloadRecord};
    use crate::core::{CoreValueFactory, RuntimeValueDomain, RuntimeValueObserver, Value};
    use crate::runtime::{RuntimeIds, RuntimeValueRoot, allocate_evaluation_runtime_id};

    // Trait selection becomes ambiguous if one of these known active
    // capabilities is ever admitted as a managed family. This is compile-time
    // evidence that Glam's private allocator cannot accept the capability
    // itself or a merely mechanical, unreviewed `Trace` implementation.
    macro_rules! assert_not_managed_family {
        ($module:ident, $type:ty) => {
            mod $module {
                trait AmbiguousIfManaged<Discriminator> {
                    fn verify() {}
                }

                struct Managed;

                impl<T: ?Sized> AmbiguousIfManaged<()> for T {}
                impl<T: ?Sized + super::ManagedFamily> AmbiguousIfManaged<Managed> for T {}

                const _: fn() = || {
                    <$type as AmbiguousIfManaged<_>>::verify();
                };
            }
        };
    }

    // The same compile-time ambiguity latch closes OpaqueValue's private Any
    // boundary. The named tests below make each required negative proof
    // visible in ordinary verification output.
    macro_rules! assert_not_opaque_payload {
        ($module:ident, $type:ty) => {
            mod $module {
                trait AmbiguousIfOpaque<Discriminator> {
                    fn verify() {}
                }

                struct Opaque;

                impl<T: ?Sized> AmbiguousIfOpaque<()> for T {}
                impl<T: ?Sized + super::OpaquePayloadFamily> AmbiguousIfOpaque<Opaque> for T {}

                const _: fn() = || {
                    <$type as AmbiguousIfOpaque<_>>::verify();
                };

                pub(super) fn verified() {}
            }
        };
    }

    struct UnreviewedTrace;

    // SAFETY: this negative fixture contains no managed edge. It deliberately
    // lacks `ManagedFamily`, proving that `Trace` is not sufficient admission.
    unsafe impl Trace for UnreviewedTrace {
        fn trace(&self, _visitor: &mut Visitor<'_>) {}
    }

    assert_not_managed_family!(unreviewed_trace_is_not_admitted, super::UnreviewedTrace);
    assert_not_managed_family!(heap_is_not_admitted, glam_gc::Heap);
    assert_not_managed_family!(factory_is_not_admitted, super::CoreValueFactory);
    assert_not_managed_family!(domain_is_not_admitted, super::RuntimeValueDomain);
    assert_not_managed_family!(observer_is_not_admitted, super::RuntimeValueObserver);
    assert_not_managed_family!(compatibility_value_is_not_admitted, super::Value);
    assert_not_opaque_payload!(bare_managed_pointer_is_not_admitted, glam_gc::Gc<u64>);
    assert_not_opaque_payload!(raw_core_value_is_not_admitted, super::Value);
    assert_not_opaque_payload!(runtime_root_is_not_admitted, super::RuntimeValueRoot);

    #[test]
    fn opaque_payload_rejects_bare_managed_pointer() {
        bare_managed_pointer_is_not_admitted::verified();
    }

    #[test]
    fn opaque_payload_rejects_unrooted_core_value() {
        raw_core_value_is_not_admitted::verified();
    }

    #[test]
    fn opaque_payload_rejects_foreign_root() {
        runtime_root_is_not_admitted::verified();
    }

    #[test]
    fn opaque_payload_requires_a_reviewed_family_record() {
        fn requires_admission<T: OpaquePayloadFamily>() -> OpaquePayloadRecord {
            T::PAYLOAD_RECORD
        }

        assert_eq!(
            requires_admission::<u64>().fields(),
            (
                "opaque scalar test fixture",
                "src/core/managed.rs",
                "edge-free token",
            )
        );
        let _ = std::any::TypeId::of::<Value>();
        let _ = std::any::TypeId::of::<RuntimeValueRoot>();
    }

    struct PassiveResource(Arc<AtomicUsize>);

    impl Drop for PassiveResource {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct PassiveManagedFixture {
        child: Option<Gc<Self>>,
        direct_drops: Arc<AtomicUsize>,
        resource: PassiveResource,
    }

    // SAFETY: `child` is the only managed edge. The counters are passive Rust
    // resources used solely to observe destruction after collection.
    unsafe impl Trace for PassiveManagedFixture {
        fn trace(&self, visitor: &mut Visitor<'_>) {
            self.child.trace(visitor);
        }
    }

    impl Drop for PassiveManagedFixture {
        fn drop(&mut self) {
            self.direct_drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: direct destruction updates one atomic counter. Transitive
    // destruction releases an ordinary Arc and `PassiveResource`; neither can
    // invoke Glam, enter the heap, or preserve the spoiled `child` edge.
    unsafe impl ManagedFamily for PassiveManagedFixture {
        const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
            "I4.0 passive managed destruction fixture",
            "src/core/managed.rs",
            "direct Drop updates only an external atomic counter",
            "Gc is inert on drop; Arc and PassiveResource release ordinary Rust resources",
        );
    }

    /// Compile-exhaustive field inventory for the passive fixture.
    ///
    /// Adding a field requires classifying its destruction here as well as in
    /// `DROP_RECORD`; no runtime, heap, or evaluator capability is present.
    fn assert_passive_managed_fixture_fields(value: &PassiveManagedFixture) {
        let PassiveManagedFixture {
            child,
            direct_drops,
            resource,
        } = value;
        let _: &Option<Gc<PassiveManagedFixture>> = child;
        let _: &Arc<AtomicUsize> = direct_drops;
        let _: &PassiveResource = resource;
    }

    struct ExternalRetirementOwner {
        values: CoreValueFactory,
        root: Option<Root<PassiveManagedFixture>>,
        retirements: Arc<AtomicUsize>,
        liveness: Arc<()>,
    }

    impl ExternalRetirementOwner {
        fn retire(&mut self) {
            if self.root.take().is_some() {
                self.retirements.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    impl Drop for ExternalRetirementOwner {
        fn drop(&mut self) {
            self.retire();
        }
    }

    fn values() -> CoreValueFactory {
        CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
    }

    fn allocate_fixture(
        values: &CoreValueFactory,
        direct_drops: &Arc<AtomicUsize>,
        resource_drops: &Arc<AtomicUsize>,
    ) -> Root<PassiveManagedFixture> {
        values.with_managed_values(|scope| {
            let allocator = scope
                .allocator::<PassiveManagedFixture>()
                .expect("the I4.0 fixture should fit one collector slot");
            scope.root(allocator.alloc(PassiveManagedFixture {
                child: None,
                direct_drops: Arc::clone(direct_drops),
                resource: PassiveResource(Arc::clone(resource_drops)),
            }))
        })
    }

    #[test]
    fn managed_family_collection_requires_completed_drop_record() {
        fn requires_admission<T: ManagedFamily>() -> ManagedDropRecord {
            T::DROP_RECORD
        }

        assert_eq!(
            requires_admission::<u64>().fields(),
            (
                "collector access scalar probe",
                "src/core/managed.rs",
                "no drop glue",
                "no transitive drop glue",
            )
        );
        assert_eq!(
            requires_admission::<PassiveManagedFixture>().fields(),
            (
                "I4.0 passive managed destruction fixture",
                "src/core/managed.rs",
                "direct Drop updates only an external atomic counter",
                "Gc is inert on drop; Arc and PassiveResource release ordinary Rust resources",
            )
        );

        // The compile-time negative assertions above establish that the same
        // generic bound rejects `UnreviewedTrace` and active capabilities.
        let _ = std::any::TypeId::of::<UnreviewedTrace>();
    }

    #[test]
    fn managed_drop_has_no_runtime_or_heap_capability() {
        let values = values();
        let direct_drops = Arc::new(AtomicUsize::new(0));
        let resource_drops = Arc::new(AtomicUsize::new(0));
        let root = allocate_fixture(&values, &direct_drops, &resource_drops);

        values.with_managed_values(|scope| {
            assert_passive_managed_fixture_fields(scope.get(&root));
        });
        let live = values
            .collect_managed_for_test()
            .expect("the rooted fixture should survive collection");
        assert_eq!(live.marked_slots(), 1);
        assert_eq!(direct_drops.load(Ordering::Relaxed), 0);
        assert_eq!(resource_drops.load(Ordering::Relaxed), 0);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("passive managed destruction should complete");
        assert_eq!(dead.finalized_slots(), 1);
        assert_eq!(direct_drops.load(Ordering::Relaxed), 1);
        assert_eq!(resource_drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn external_raii_owner_is_not_reachable_from_managed_graph() {
        let values = values();
        let domain = Arc::downgrade(values.value_domain());
        let direct_drops = Arc::new(AtomicUsize::new(0));
        let resource_drops = Arc::new(AtomicUsize::new(0));
        let retirements = Arc::new(AtomicUsize::new(0));

        let explicit_liveness = Arc::new(());
        let explicit_liveness_weak: Weak<()> = Arc::downgrade(&explicit_liveness);
        let mut explicit = ExternalRetirementOwner {
            values: values.clone(),
            root: Some(allocate_fixture(&values, &direct_drops, &resource_drops)),
            retirements: Arc::clone(&retirements),
            liveness: explicit_liveness,
        };
        explicit.retire();
        explicit.retire();
        assert_eq!(retirements.load(Ordering::Relaxed), 1);
        drop(explicit);
        assert!(explicit_liveness_weak.upgrade().is_none());
        assert_eq!(retirements.load(Ordering::Relaxed), 1);

        let fallback_liveness = Arc::new(());
        let fallback_liveness_weak: Weak<()> = Arc::downgrade(&fallback_liveness);
        let fallback = ExternalRetirementOwner {
            values: values.clone(),
            root: Some(allocate_fixture(&values, &direct_drops, &resource_drops)),
            retirements: Arc::clone(&retirements),
            liveness: fallback_liveness,
        };
        assert_eq!(fallback.values.runtime_id(), values.runtime_id());
        assert_eq!(Arc::strong_count(&fallback.liveness), 1);
        drop(fallback);
        assert!(fallback_liveness_weak.upgrade().is_none());
        assert_eq!(retirements.load(Ordering::Relaxed), 2);

        let report = values
            .collect_managed_for_test()
            .expect("retired external roots should permit collection");
        assert_eq!(report.finalized_slots(), 2);
        assert_eq!(direct_drops.load(Ordering::Relaxed), 2);
        assert_eq!(resource_drops.load(Ordering::Relaxed), 2);

        drop(values);
        assert!(domain.upgrade().is_none());
    }
}
