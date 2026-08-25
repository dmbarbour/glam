# Glam GC Gate G1 Audit — 2026-08-25

Baseline: `73e0b1c`. This is the C6D.3 certification audit of the isolated
`glam-gc` collector after the mandatory C6 review and all GC6 follow-ups.

Status: complete. Gate G1 passes; production ownership and API integration may
begin with collection disabled.

## Scope

Gate G1 asks whether the isolated crate is a sound basis for beginning
production ownership migration. It does not enable production collection.
The gate requires:

1. a working non-moving, stop-the-world full collector;
2. explicit external roots and regional mutator authority;
3. deterministic race and irreversible-failure evidence;
4. Miri coverage and a reconciled unsafe inventory; and
5. no production dependency on Glam runtime semantics.

The audit treats the reviewed C6 design as fixed. C7 worker-scale stress,
metrics, and C8 tuning remain downstream verification/performance work rather
than missing G1 semantics.

## Dependency and Unsafe Boundary Audit

`cargo tree -p glam-gc` reports no normal dependency. `loom` and its graph are
dev dependencies only. The collector therefore has no production dependency
on Glam values, the evaluator, scheduler, reflection, or configuration.

The crate denies unsafe code by default and admits it through 11 exact module
expectations. `scripts/unsafe-sites.txt` records 138 unsafe constructs,
including test `Trace` implementations and unsafe test calls. The checked-in
inventory matches the source. The safety ledger accounts for these constructs
under the following enduring boundaries:

- arena allocation, aligned chunk/run recovery, header and bitmap access;
- canonical metadata, erased trace dispatch, and erased destruction;
- pointer construction/access plus `Send`/`Sync` proofs;
- worker-local allocation cursor writes and publication;
- root reconstruction/access under a matching mutator;
- the structural edge-replacement gateway; and
- collector trace, sweep, finalization, and terminal destruction dispatch.

The C6D.3 corrections below change no production unsafe site. Strict-
provenance construction in one negative fixture replaces exposed-provenance
integer casts with safe `NonNull::map_addr` operations.

## Soundness Accounting

### Liveness and access

A bare `Gc<T>` is one typed address and carries no liveness. A registered live
root cell or a traced edge from one preserves the exact allocation during a
full mark. Pending finalization records preserve initialized drop-bearing
allocations without making them rootable. A matching regional mutator excludes
collection while client code borrows a managed value.

Root publication precedes return to the caller. Exclusive collection walks a
stable weak registry, prunes failed upgrades, and seeds each upgraded root;
the allocation mark bit deduplicates cloned roots and graph joins. Escaped
roots and allocators do not retain the heap.

### Collection and allocator publication

An elected collector drains every outer mutator before taking exclusive
authority. Mark words are private ordinary words used only under that
authority. Checked owner lookup validates the owning chunk, run topology,
exact slot start, allocation bit, class membership, and canonical metadata
before erased trace dispatch. Mark-before-enqueue and a non-recursive worklist
terminate cycles and shared graphs.

After successful marking, planning and capacity reservation precede the first
selector withdrawal. Eager no-drop sweep, whole-run reset/recycling,
finalization-batch installation, exact lease reconstruction, and eligible
frontier publication complete before one final Release epoch invalidates every
old worker cursor. No mutator can observe an intermediate allocator view.

The finalized-word protocol clears the allocation bit before releasing its
lease bit. GC6-003's Loom model proves neighboring lease bits survive, the
released bit has one winner, and an overlapping winner observes retirement.
The production forced schedule proves an already prepared allocator can safely
claim and initialize the retired slot before the later class-frontier store.

### Finalization and failure

Attached pending runs remain class-owned with exact words reserved; wholly dead
drop-bearing runs transfer their stable run records to the durable finalization
map. Every pending identity is non-rootable. Destructors run outside collector
locks under the installed finalizer mutator.

A normal destructor return and a payload-destructor panic both reach durable
terminal retirement: allocation retirement precedes pending-mask removal, and
the untouched suffix remains indexed after a panic. A later collection retries
only untouched identities. An invariant panic after selector withdrawal or
after erased dispatch but before durable finalizer commit permanently poisons
the heap rather than reopening uncertain topology.

Last-owner teardown is passive resource destruction. It supplies no mutator,
walks detached records before attached class allocations, never revisits a
cleared identity, and stops at the first destructor panic. This restricted
terminal behavior is part of the public managed-`Drop` contract, not a hidden
evaluation stage.

## Audit Findings

### G1-001 — Intentional leak was not isolated in the Miri harness

**Classification:** verification harness defect  
**Gate status:** resolved

The complete Miri command ran
`forgotten_scoped_allocator_does_not_retain_its_heap` with leak checking even
though that fixture deliberately forgets one inert 24-byte class-frontier
cell. All 184 runnable tests passed, after which Miri correctly rejected the
process for that leak.

`check-miri.sh` now follows the already documented ASan policy: the complete
suite keeps leak checking while skipping that exact fixture, then the fixture
runs separately with only leak checking disabled. Strict provenance,
aliasing, initialization, and access checks remain enabled in both runs.

### G1-002 — Invalid-address fixture weakened Miri provenance coverage

**Classification:** verification quality gap  
**Gate status:** resolved

`collector_lookup_rejects_foreign_interior_unallocated_and_unknown_class_slots`
constructed four deliberately invalid addresses with integer-to-pointer casts.
Miri passed the behavior but warned that those casts could hide provenance
defects. The fixture now derives every address from its owning `NonNull` using
strict-provenance `map_addr`. Its focused Miri run passes with
`-Zmiri-strict-provenance` and no warning.

Neither finding changes collector semantics or production unsafe code. The
audit found no soundness defect, missing C6 owner, or duplicate authoritative
representation.

## Verification Matrix

| Evidence | Gate claim | Result |
| --- | --- | --- |
| `scripts/check.sh` | formatting, Clippy, native unit/doctest matrix, exact unsafe inventory | passed |
| complete Loom scaffold | admission, lease uniqueness, handoff, finalized-word release | passed: 7 models |
| `scripts/check-scale.sh` | non-recursive million-node depth and million-edge width | passed |
| corrected strict-provenance Miri gate | provenance, aliasing, initialization, drop/retry, terminal traversal | passed: 183 main-suite tests, 3 Miri-only ignores, 1 isolated fixture; no warnings |
| ASan with isolated intentional leak | memory safety and complete-suite leak detection | passed: 184 main-suite tests, 2 scale ignores, 1 isolated fixture |
| TSan | concurrent publication and admission data races | passed: 185 tests, 2 scale ignores |
| complete workspace checks | integration-neutral repository regression | passed: formatting, warnings-denied Clippy, and every test target |

## Gate Decision

Gate G1 passes on 2026-08-25. The isolated collector is a sound basis for
beginning production API and ownership migration. The audit found no collector
soundness defect, missing C6 owner, or duplicate authority; it corrected two
verification-harness defects without changing production unsafe code.

This decision does not certify the production Glam graph. Automatic and
explicit production reclamation remain disabled until integration closes the
whole graph and passes the later roadmap gates. C7 and C8 remain useful
collector stress, metric, and tuning work, but they are not retroactive G1
requirements.
