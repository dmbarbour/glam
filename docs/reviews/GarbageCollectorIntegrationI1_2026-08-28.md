# Glam GC Integration Phase I1 Review — 2026-08-28

Baseline: `3d40e77`, including the completed Phase I1E regression and plan
record.

Status: complete. Phase I1 satisfies its runtime-heap ownership boundary with
production collection disabled. No finding blocks Phase I2.

## Scope

This is the mandatory post-implementation review for integration Phase I1. It
audits I1A-I1E against:

- the ownership, policy, and verification requirements in
  [`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md);
- the isolated collector contract certified by
  [`GarbageCollectorGateG1_2026-08-25.md`](GarbageCollectorGateG1_2026-08-25.md);
- the value-domain and gate rules in
  [`GarbageCollectionRoadmap_2026-08-19.md`](../plans/GarbageCollectionRoadmap_2026-08-19.md); and
- the stable representation and external-owner inventory in
  [`GarbageCollectorOwnershipLedger_2026-08-20.md`](../plans/GarbageCollectorOwnershipLedger_2026-08-20.md).

The review asks whether Glam now has one correctly contained heap per runtime
value domain, a bounded allocation/rooting seam, and an acyclic ownership
topology while no production value has yet migrated. It does not certify the
production value graph or enable explicit or automatic production collection;
those remain Gate G2 and Gate G3 work.

## Outcome

The implemented boundary matches the intended phase:

- every `CoreValueFactory` retains one `Arc<RuntimeValueDomain>`;
- that domain owns one immutable `CollectionPolicy::NoAuto` heap, runtime IDs,
  the canonical/compiler cache, and only a weak coordinator binding;
- scoped factories share the domain and add only compilation-local cache state;
- `CoreValueFactory::with_managed_values` is the only production collector
  entry in the root crate and exposes a higher-ranked, non-`Send` mutator scope
  through narrow allocation, rooting, and rooted-access operations;
- typed allocators cannot survive that scope, while direct roots retain only a
  weak heap association;
- runtime state and the immutable default reflection profile remain internal
  sibling roots, and profile hosts may retain only the acyclic shared-resource
  bundle rather than runtime lifecycle owners; and
- no production `core::Value` or other semantic representation is managed.

The runtime cannot request or force collection through its public or internal
service facades at I1. `NoAuto` prevents mutator admission from servicing
latched pressure; the private heap remains reachable only inside the value
domain. Terminal heap destruction still destroys private test allocations, as
required by the collector contract, but this is not production reclamation.

## Ownership and Lifecycle Accounting

The strong ownership paths are deliberate capabilities:

```text
EvaluationRuntime
├── RuntimeState
│   └── RuntimeSharedResources
│       └── CoreValueFactory
│           └── RuntimeValueDomain → Heap(NoAuto)
└── ReflectionTaskProfile
    └── sealed launcher → runtime host → RuntimeSharedResources
```

Public `Values`, active evaluation demand state, stores/snapshots, and active
compiler views may independently retain the value domain because they still
construct values. Public values and direct collector roots do not. Both the
shared-resource bundle and value domain route back to the coordinator weakly,
so retaining construction authority does not retain the scheduler, executor,
runtime state, or default profile.

The compiler cache is contained inside the domain and its installed values do
not capture a factory/domain backedge. The composed I1E regression proves the
same property with a sealed profile and a real collector root present.

## Scoped Authority and Representation Accounting

The higher-ranked callback structurally prevents `Mutator`,
`CoreValueAllocationScope`, `CoreValueAllocator`, and managed borrows from
escaping their regions. A bare `Gc<T>` intentionally has no lifetime and may
eventually leave construction as an exact managed edge; preventing it from
being parked unrooted is therefore a representation/source-inventory rule,
not a promise made by the pointer type. At I1 no production caller stores or
returns a bare managed pointer.

`core::managed::managed_slot_extent<T>` is the only Glam-owned requested-slot
policy. It rejects zero-sized representations at compile time, uses a
pointer-sized initial floor, and leaves Rust alignment authoritative. The
private one-byte leaf and `u64` lifecycle probes are verification machinery,
not production representation families or Gate G2 ledger entries.

Internal rooted access currently treats a mismatched root as an invariant
panic inherited from `Root::get`. I2 owns the fallible public provenance check
through `Heap::owns`; I1 exposes no public root/access operation and therefore
does not prematurely select that facade.

## Drift Assessment

### Intentional and justified

1. **No production managed shell in I1D.** The phase centralized layout policy
   and used a private leaf probe rather than pre-empting I2's public-root
   prototype or the separate value-representation plan.
2. **Direct roots remain weak domain references.** Authorized construction
   services, not value handles, own the domain. An escaped root becomes inert
   after domain teardown, matching the roadmap.
3. **The default collector policy remains `Automatic`.** Only Glam runtime
   domains explicitly select `NoAuto`; isolated collector clients and tests
   retain the certified default behavior.
4. **Bare managed pointers are not lifetime-bound.** This is necessary for
   managed graph edges. Later phases must keep enforcing the ledger rule that
   no bare pointer is parked in external or erased state.
5. **Runtime state and profile remain sibling roots.** The collector domain is
   internal beneath shared resources; it does not require merging the siblings
   or moving either outside `EvaluationRuntime`.

### Corrective new information

None.

### Accidental or convenience-driven drift

None found.

## Review Finding

### I1R-001 — The lifecycle matrix omitted the composed I1E regression

**Classification:** documentation/verification-index drift  
**Status:** resolved

The integration phase correctly added
`runtime_value_domain_has_no_scheduler_or_profile_backedge`, but the ownership
ledger's semantic verification matrix still listed only the earlier component
fixtures. The matrix now includes the composed test. No implementation or
semantic change was required.

## Verification

| Boundary | Evidence |
| --- | --- |
| immutable no-auto production policy | `scoped_factories_share_one_no_auto_runtime_value_domain`; `no_auto_policy_retains_pressure_until_explicit_collection`; `no_auto_policy_retains_explicit_request_across_mutator_entries` |
| shared domain and bounded allocation/root access | `factory_scoped_allocation_uses_current_mutator`; `scoped_factory_does_not_retain_allocator_or_scheduler`; collector compile-fail doctests for mutator, allocator, and managed-borrow escape |
| centralized requested layout | `managed_family_requested_layout_is_accepted`; collector requested-slot, undersized-request, and unsupported-layout tests |
| runtime/profile/resource acyclicity | `runtime_shared_resources_do_not_retain_runtime_lifecycle_owners`; `public_values_retain_only_the_runtime_value_domain`; `bare_public_values_do_not_retain_the_runtime_value_domain`; `retained_reflection_profile_keeps_only_shared_resources_alive`; `evaluation_context_retains_runtime_cache_and_profile_without_a_cycle` |
| cache and managed-root backedges | `compiler_cache_does_not_form_a_value_domain_cycle`; `runtime_value_domain_has_no_scheduler_or_profile_backedge` |
| repository compatibility | warnings-denied all-target/all-feature Clippy and the complete workspace test suite |

The Phase I1E run passed formatting, Clippy, 1,129 root-library tests, and every
integration and doctest target. The review changed documentation only after
that run; final review verification checks formatting/diff integrity and
reruns the complete routine checks.

## Decision

Phase I1 is complete. The runtime owns one acyclic, non-collecting managed
value domain and a private bounded construction seam. Production values remain
on their legacy representations, no production collection path is enabled,
and Gate G2 remains closed.

Phase I2 may begin with the external-root/public-`Value` prototype. It must not
reinterpret this review as permission to reclaim production values or weaken
the later graph inventory, mutator-scope, provenance, and root-conversion
gates.
