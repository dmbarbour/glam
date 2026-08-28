# Glam GC Integration Phase I2 Review — 2026-08-28

Baseline: `ea5377c`, including completed Phases I2A-I2C.

Status: complete. Phase I2 fixes the public-value/root contract and its
production migration inventory without changing the production
representation or enabling collection. No finding blocks Phase I3.

## Scope

This is the mandatory post-implementation review for integration Phase I2. It
audits the isolated public-root prototype and compatibility-access inventory
against:

- the I2 requirements in
  [`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md);
- the opaque-handle and provenance resolutions in
  [`GarbageCollectorIntegration_2026-08-25.md`](GarbageCollectorIntegration_2026-08-25.md);
- the root, domain, mutator, and collection gates in
  [`GarbageCollectionRoadmap_2026-08-19.md`](../plans/GarbageCollectionRoadmap_2026-08-19.md);
- the public-root and transient-owner records in
  [`GarbageCollectorOwnershipLedger_2026-08-20.md`](../plans/GarbageCollectorOwnershipLedger_2026-08-20.md); and
- the longer-term public representation constraints in
  [`ValueRepresentationRefinement_2026-08-19.md`](../plans/ValueRepresentationRefinement_2026-08-19.md).

The review asks whether I2 selected a coherent production contract, proved
that contract with an isolated collector-backed representation, and recorded
all current compatibility access which later phases must remove. It does not
certify a production `Trace`, migrate durable owners, change `api::Value`, or
authorize collection over the production graph.

## Outcome

The implemented prototype matches the selected contract:

- its private public-value shape contains either an allocation-free immediate
  plus a weak value-domain witness, or the collector's existing `Root<T>`;
- cloning a managed handle shares one root cell and registry entry;
- neither representation retains the value domain, and an escaped handle
  becomes inert after the final authorized domain owner is dropped;
- matching live runtime authority is required for structural comparison, kind
  inspection, rendering, evaluation, and owned extraction;
- foreign and inaccessible values fail before the typed-pointer gateway;
- the handle and evaluated witness support transport but no equality,
  ordering, hashing, identity, provenance, or content observation on their
  own;
- `EvaluatedValue` is the same opaque root plus an outer-WHNF witness, not a
  second root representation or an observation authority; and
- only owned Rust data escapes managed access.

The prototype remains entirely under `cfg(test)`. Production `api::Value`,
`EvaluatedValue`, and `RuntimeValueRoot` retain their compatibility behavior,
and the runtime heap remains `CollectionPolicy::NoAuto`.

## Representation and Provenance Accounting

The inline arm carries an `i64` solely to establish that eligible immediate
values need neither a managed allocation nor a root registration. Its private
`Weak<RuntimeValueDomain>` is non-forgeable outside the prototype boundary and
does not revive or preserve the domain. Matching uses `Weak::ptr_eq` against a
live factory-owned domain; the factory's strong domain lease makes the
subsequent scoped operation stable.

The managed arm reuses `glam_gc::Root<PrototypeNode>` directly. The root owns
one `Arc<RootCell>` and refers weakly to its heap. `Heap::owns` performs the
constant-time release-build provenance check; the matching factory then enters
that same heap before `Root::get` supplies the typed borrow. There is no
parallel runtime-ID authority, second registry, wrapper root cell, or public
pointer identity.

The prototype deliberately does not choose the final enum size, tag layout,
immediate range, erased managed root, or production node taxonomy. Those are
private representation choices for I4F.2 and the value-representation plan,
not omissions from the public contract.

## Observation and Scoped-Access Accounting

`PrototypeRuntime<'a>` borrows `CoreValueFactory` and is the sole observation
authority. Both supported Rust call directions—the runtime receiving a value
and the value receiving a runtime service—delegate to the same fallible
boundary. The value-side convenience method does not inspect the handle by
itself.

Recursive structural comparison and owned extraction run inside one managed
region. Their test-only `CoreValueAllocationScope::get_traced_edge` gateway is
unsafe because a bare `Gc<T>` has no independently checkable provenance. Its
callers prove that each child is an exact typed edge in a rooted graph whose
`Trace` implementation visits that child. The active mutator excludes
collection for all returned borrows. This gateway is `cfg(test)` and does not
authorize an equivalent unchecked production escape; I3 and each production
managed family must provide their own structurally justified access path.

Nested same-runtime observation reuses recursive mutator admission while an
outer managed borrow remains valid. Cross-thread observation transfers both
the opaque handle and an authorized factory clone, then enters the destination
thread's scoped mutator normally. No managed borrow crosses either boundary.

## Compatibility-Access Accounting

The I2C inventory now records 233 occurrences in 23 library source modules.
It covers the temporary `as_core`, `into_core`, facade construction, and
`RuntimeValueRoot` construction spellings used by public construction,
evaluation, storage, reflection, diagnostics, compiler/macro, interaction-net,
and scheduling paths. Every module names its I3 scoped-access owner and I4F
root/facade owner.

This is deliberately a source-module and known-surface inventory rather than
a Rust semantic index. Exact line entries would be noisy, while a new or moved
known compatibility spelling fails the regression. Binary-crate callers
cannot use the private conversions; their public API migration remains part of
I4F.2. Named test modules remain compatibility oracles rather than production
owner entries. I4F.2 still requires the stronger closing assertion that no
compatibility escape remains.

The review found that the scanner counted `Value::from_runtime(...)` but not
the facade-local `Self::from_runtime(...)` delegation already present in
`api/value.rs`. Extending the scanner before changing the baseline produced the
intended one-versus-two mismatch. The corrected total is 233.

## Drift Assessment

### Intentional and justified

1. **The prototype is private and test-only.** I2 selects and verifies the
   contract without creating the forbidden intermediate state where production
   values contain managed pointers but durable owners still use the old
   non-registering root.
2. **The managed prototype remains typed.** Final managed type erasure and tag
   decoding depend on the production node taxonomy and stay with I4F.2 and the
   value-representation plan.
3. **The immediate prototype uses `i64`.** This proves allocation-free inline
   provenance without selecting the final immediate set or bit encoding.
4. **A borrowed core factory stands in for the eventual public runtime
   service.** It supplies the exact live domain and recursive mutator admission
   needed to test authority; the public ergonomic placement remains private
   until the production switch.
5. **The inventory is module-granular and spelling-based.** Its purpose is to
   prevent growth of the known temporary conversion surface and assign
   migration ownership, not to replace the later durable-owner or no-escape
   gates.
6. **Rendering and owned extraction use a small recursive test node.** They
   prove the authority and lifetime contracts only; stack control and complete
   production graph behavior remain later integration concerns.

### Corrective new information

1. **Facade-local constructor delegation is part of the compatibility
   surface.** The inventory now recognizes `Self::from_runtime(...)` in
   addition to type-qualified calls.

### Accidental or convenience-driven drift

None remains. The stale roadmap status, verification index, and I1-era source
comments were corrected during this review.

## Review Findings

### I2R-001 — The compatibility scanner omitted a facade-local delegation

**Classification:** verification gap  
**Status:** resolved

`Value::from_core` delegates through `Self::from_runtime`, but the I2C scanner
only recognized type-qualified `Value::from_runtime` spellings. The omission
did not hide another module or durable owner, but it made the claimed exact
occurrence total false. The scanner now includes the facade-local spelling,
the expected `api/value.rs` count is two, and the recorded total is 233. The
test was latched before updating the expected baseline.

### I2R-002 — Durable review indexes did not name the completed I2 boundary

**Classification:** documentation and verification-index drift  
**Status:** resolved

The roadmap status still stopped at integration I0, and the ownership ledger's
semantic matrix named only the old public-value compatibility tests while
describing I2's future replacement. The roadmap now records completed I1/I2
reviews, and the matrix separately indexes the I2 prototype and access
inventory. The managed-scope comments now distinguish I2's selected contract
from I4F's eventual production migration.

## Verification

| Boundary | Evidence |
| --- | --- |
| weak domain and heap provenance | `prototype_root_rejects_another_heap`; `prototype_root_becomes_inert_after_domain_drop`; `prototype_inline_value_rejects_another_domain`; collector `Heap::owns` tests |
| root-cell reuse and thread transfer | `prototype_root_moves_between_threads`; its post-collection root-entry count remains one |
| allocation-free immediate arm | `prototype_inline_values_allocate_no_managed_slots` |
| exact recursive managed trace and reclamation | `prototype_recursive_root_traces_child` |
| opaque transport-only facade | compile-time negative trait fixtures for both prototype handles; `prototype_value_debug_is_opaque` |
| authorized semantic observation | `prototype_runtime_compares_live_structural_values`; `prototype_runtime_observation_rejects_foreign_or_inaccessible_value` |
| owned extraction lifetime | `prototype_owned_extraction_outlives_domain` |
| recursive mutator admission | `prototype_value_access_nests_in_one_mutator` |
| production-switch inventory | `public_value_compatibility_access_inventory_is_complete`, including the latched facade-local delegation mismatch |
| compatibility and repository behavior | unchanged public-value suite, warnings-denied all-target/all-feature Clippy, and the complete workspace test suite |

## Decision

Phase I2 is complete. It fixes the opaque public-value contract and the
authority/provenance model while leaving the production graph unchanged and
uncollectable. Phase I3 may establish bounded evaluator, worker, compiler, and
callback-free mutator regions using this contract.

This decision does not authorize production managed values, durable managed
roots, automatic collection, or unchecked access to bare managed edges. I4F.1
must convert durable owners before I4F.2 enacts the selected facade, and later
Gate G2/G3 reviews still govern production tracing and collection.
