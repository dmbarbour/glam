# Garbage Collector Integration Review — 2026-08-25

Baseline: `bb205d9`. This is a review-only audit of the production-integration
roadmap after the isolated `glam-gc` collector passed Gate G1. No production or
collector implementation changed as part of this review.

Status: complete. Integration is the recommended next workstream, but the
current Phase I1 should not begin verbatim. The findings below need to be
reflected in the plan or explicitly resolved before their affected checkpoints
begin. Gate G1 remains passed; this review does not authorize production
collection.

## Scope

The review compares:

- [`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md);
- [`GarbageCollectorOwnershipLedger_2026-08-20.md`](../plans/GarbageCollectorOwnershipLedger_2026-08-20.md);
- [`GarbageCollectionRoadmap_2026-08-19.md`](../plans/GarbageCollectionRoadmap_2026-08-19.md);
- the Gate G1 collector API and behavior; and
- the current runtime, value, evaluation, reflection, persistent-collection,
  and interaction-net ownership boundaries.

Drift is not automatically a defect. The review distinguishes a genuine
semantic or safety conflict from a plan which merely needs smaller checkpoints
now that the collector and runtime boundaries are concrete.

## Summary

The integration direction and major gate order remain sound:

1. establish one managed value domain per `EvaluationRuntime`;
2. introduce bounded mutator regions before managed pointer access;
3. close every exact trace, closure, opaque, collection, net, and external-root
   boundary before Gate G2;
4. force whole-production-graph collection only after G2; and
5. enable controlled and then automatic runtime collection only after G3.

The main corrections are:

1. provide a real manual/non-automatic collector policy before production
   allocations begin;
2. preserve the resolved boundary between opaque public value handles and
   runtime-mediated observation;
3. establish an explicit `RuntimeValueDomain` owner matrix;
4. add one narrow, fallible heap-provenance check for roots;
5. stop treating private, discovery-order collector identifiers as durable
   integration-ledger data; and
6. design the scoped evaluation-quantum authority before attempting broad I3
   mutator plumbing.

Several later phases also need smaller checkpoints, exact synchronization
wording, and verification deferred to the first legal whole-graph collection.

## Findings

### GCI-001 — Collection-disabled integration has no enforceable collector mode

**Classification:** gate violation and missing prerequisite  
**Priority:** critical  
**Confidence:** high  
**Status:** resolved 2026-08-25

The integration plan says production automatic and explicit collection remain
disabled until the later graph-closure gates, and I1 calls for runtime-local
tuning with collection disabled by default
([integration plan](../plans/GarbageCollectorIntegration_2026-08-19.md#phase-i1--runtime-heap-ownership-collection-disabled)).

The current collector has only `Heap::new()`. Allocation pressure sets the
same collection-request flag used by explicit requests. A later outer
`Heap::with_mutator` entry may elect and run a full collection when that flag
is set ([`heap.rs`](../../crates/glam-gc/src/heap.rs#L182),
[`heap.rs`](../../crates/glam-gc/src/heap.rs#L2029)). Consequently, merely
omitting calls to `collect_full` does not keep a partially migrated production
graph safe.

**Recommended resolution:** add an I1A collector prerequisite with an explicit
manual/non-automatic scheduling policy. Pressure may still be recorded and
`collect_full` may remain available to isolated tests, but mutator admission
must not service pressure or queued requests automatically while the
production runtime is in migration mode. Do not emulate this solely in a Glam
wrapper around an otherwise self-collecting heap.

**Required verification:** cross the pressure threshold in manual mode, enter
the heap again, and prove no collection occurred; separately prove an explicit
test-only full collection still works. The production integration must use the
manual mode until the phase which deliberately enables automatic collection.

**Resolution:** `glam_gc::CollectionPolicy` is immutable per heap and provides
`Automatic` and `NoAuto`. Under `NoAuto`, pressure and explicit requests remain
latched and visible, but outer mutator entry cannot elect collection;
`Heap::collect_full` remains the deliberate synchronous path. Public
`HeapStatistics` reports assigned runs, the current high-water mark/headroom,
the request latch, finalization-batch run count, and queued/running finalizers
without scanning heap allocations. Focused tests cover both pressure and
explicit requests across repeated entries, explicit acknowledgement, and
queued/running finalization snapshots.

### GCI-002 — Weak inert roots conflict with public `Value` equality and observation

**Classification:** public semantic contract
**Priority:** high  
**Confidence:** high  
**Status:** resolved 2026-08-26

The selected collector root is weak with respect to the heap. It remains
cloneable and droppable after heap teardown but cannot be read
([`root.rs`](../../crates/glam-gc/src/root.rs#L7)). The integration plan adopts
that property for escaped public values while also requiring current public
equality and debug semantics to remain intact
([integration plan](../plans/GarbageCollectorIntegration_2026-08-19.md#phase-i2--external-root-and-public-value-prototype)).

Today `api::Value` derives structural `PartialEq` and `Eq`, and its non-forcing
`Debug` implementation calls `kind()` on the retained core value
([`value.rs`](../../src/api/value.rs#L22),
[`value.rs`](../../src/api/value.rs#L750)). After the last authorized value-
domain owner disappears, a weak root cannot reconstruct structural equality or
outer kind. Pointer identity is not equivalent to current `core::Value`
equality, and `PartialEq::eq` cannot report that the domain is gone.

The same lifecycle question applies to every infallible observer and to
borrowed extractors on `EvaluatedValue`, not just formatting.

**Recommended resolution:** keep a Glam-owned public wrapper rather than
exposing `glam_gc::Root` directly, but make that wrapper an opaque transport
handle. Resolve I2 around the following contract:

- roots remain weak and do not preserve the value domain;
- bare `Value` and `EvaluatedValue` support transport operations such as
  cloning, dropping, and same-runtime thread transfer, but expose no semantic
  equality, ordering, hashing, kind, contents, or runtime identity;
- consequently public values do not implement `PartialEq`, `Eq`, `PartialOrd`,
  `Ord`, or `Hash`, and clients cannot use the handles themselves as semantic
  lookup keys;
- any retained `Debug` implementation is deliberately opaque and reveals no
  kind, contents, provenance, or accessibility state;
- structural comparison, kind inspection, extraction, and value rendering are
  fallible operations authorized by a live matching runtime service rather
  than observations a handle can perform by itself. The Rust API may pass the
  value to that service, pass a scoped service/mutator to a value method, or
  offer both forms when useful;
- runtime provenance validation remains private boundary enforcement, not a
  public observation; and
- runtime-mediated extraction returns owned host data which may outlive the
  value domain. Managed borrows do not escape their matching access region.

Retaining the heap from every public `Value` would preserve current equality,
but contradicts the chosen value-domain teardown model and is not recommended.

**Required verification:** use compile-time API checks to establish that
public values have no equality, ordering, or hash contract; exercise
runtime-mediated comparison of live equal, unequal, cloned, and separately
constructed structurally equal values; verify that opaque debug cannot reveal
value state; cover matching-runtime and foreign/inaccessible-domain
observation failures; and prove owned extraction results outlive the final
authorized value-domain owner.

**Resolution:** public values are opaque handles outside a runtime service.
They remain convenient to clone, drop, and transfer, but cannot themselves be
observed or used as lookup keys. `EvaluatedValue` remains the WHNF witness, not
an observation capability: semantic equality, kind, scalar/binary extraction,
and rendering all require live matching runtime authority, regardless of which
side owns the ergonomic Rust method. Internal root identity is not exposed as
a substitute equality relation. The existing compatibility representation
remains unchanged until the isolated I2 prototype fixes the new facade and I4F
performs the production switch; those checkpoints now own removal of the
unauthorized direct observation traits and methods.

### GCI-003 — The authorized value-domain owner matrix is not concrete

**Classification:** ownership architecture gap  
**Priority:** high  
**Confidence:** high  
**Status:** resolved 2026-08-25

I1 correctly says that only explicitly authorized non-root capabilities retain
the value domain, but it does not name those capabilities. Current ownership
is already distributed: `EvaluationRuntime` owns state and the immutable
profile as sibling roots, `RuntimeSharedResources` owns a value factory, and
`CoreValueFactory` is cloned into evaluation sessions, stores, compilers, and
caches ([`runtime.rs`](../../src/api/runtime.rs#L41),
[`core.rs`](../../src/core.rs#L271)). Public `Values` is also independently
cloneable and can construct values after the runtime facade has been dropped.

Without a fixed matrix, adding `Heap` to each convenient holder risks either:

- making `Value` unexpectedly preserve the complete runtime;
- making `Values` or an evaluation context become inert despite current use;
- retaining the scheduler/profile along with the value domain; or
- forming `heap -> managed closure -> factory/domain -> heap`.

**Recommended resolution:** introduce one internal
`Arc<RuntimeValueDomain>` containing the heap and value-domain facilities.
Explicitly authorize at least the public construction service, core factory,
runtime shared resources, and active evaluation/compiler owners as strong
domain leases where current behavior requires it. Public value roots and
managed nodes remain weak/non-owning with respect to the domain. Scheduler and
profile ownership stays outside the domain except for reviewed weak routes.

Every managed closure, opaque node, or other heap resident must be forbidden
from retaining a factory/domain strongly. I4B and I10 should audit that exact
backedge.

**Required verification:** latch facade drop with retained `Values`, shared
resources, evaluation context, profile, compiler cache, and bare public values
in separate tests. Each test must state whether the domain remains usable and
prove scheduler/profile cycles are not introduced.

**Resolution:** `CoreValueFactory` now has one strong
`Arc<RuntimeValueDomain>` rather than four independently cloned runtime-value
facilities. The domain owns the no-auto collector heap, runtime-local IDs,
canonical/extension cache, and a weak coordinator binding. Public construction
services, runtime shared resources, evaluation contexts, reflection
stores/snapshots, and active compiler views retain it through their factory.
Public values do not. Managed payloads and cache entries are explicitly barred
from retaining the domain strongly.

The owner matrix and conditional service-profile route are recorded in Phase
I1B. Focused lifecycle tests cover retained shared resources, public `Values`,
a bare public value, a retained service profile, an evaluation context, and a
populated compiler cache. The tests also prove that keeping the domain does not
retain runtime state, the coordinator, executor, or default profile, and that
the compiler cache does not create an internal ownership cycle. Production
values are not heap-managed yet, and collection remains disabled by policy.

### GCI-004 — Fallible same-heap root provenance is missing

**Classification:** API integration prerequisite  
**Priority:** high  
**Confidence:** high  
**Status:** open

`Root::get` performs a release-build heap comparison and panics on mismatch.
That is an appropriate last safety check before the private typed-pointer
gateway, but it is not sufficient for Glam's public error contract. Composite
constructors and runtime stores currently reject foreign-runtime values with a
normal `Result` before evaluation.

The collector exposes no non-panicking `Root::belongs_to(&Heap)` or
`Heap::owns(&Root)` operation. Its root cell's weak heap association is private
([`root.rs`](../../crates/glam-gc/src/root.rs#L20)).

**Recommended resolution:** add one narrow constant-time ownership predicate
which compares the root's recorded weak heap identity with a live heap without
exposing an address or forgeable token. Keep `Root::get`'s assertion as the
unsafe-boundary backstop. Glam's wrapper uses the predicate to produce ordinary
cross-runtime errors before access.

**Required verification:** same-heap acceptance, foreign-heap rejection, an
inert root after heap teardown, concurrent clone/drop, and preservation of the
existing `Root::get` mismatch panic as a collector invariant check.

### GCI-005 — The ownership ledger requires private and unstable collector identities

**Classification:** documentation and verification-model drift  
**Priority:** medium  
**Confidence:** high  
**Status:** resolved 2026-08-25

The ownership ledger requires every managed row to record its canonical
`ObjectMetadata` pointer and dense heap-local class ID, among other geometry
([ownership ledger](../plans/GarbageCollectorOwnershipLedger_2026-08-20.md#gate-g2-blockers-and-reconciliation)).
Those identifiers are intentionally private to `glam-gc`. A metadata pointer
is process-instance data, while a dense class ID depends on discovery order
within one heap. Neither is durable type documentation or a useful production
integration contract.

**Recommended resolution:** record stable integration facts instead:

- Rust type and source owner;
- `Trace` reviewer and exact outgoing-edge policy;
- requested slot extent, Rust size/alignment, and drop/finalization policy;
- whether allocator discovery accepts the layout; and
- the mutation gateway and external-root classification.

Keep final stride, slots per run, and dense class identity in collector tests
or an explicitly test-only diagnostic surface. Do not widen the production
collector API merely to copy its internal topology into a document.

**Resolution:** the ownership ledger and I0/I1D integration contract now use a
stable representation-family reconciliation record. Each managed family is
identified by its concrete Rust type and source owner and must record its exact
trace review, requested extent plus Rust layout, allocator-discovery result,
drop/finalization policy, mutation gateways, external-root classification, and
authorizing verification. Metadata addresses, `TypeId`, dense class IDs,
frontiers, final stride, and slots-per-run geometry are explicitly excluded
from the production ledger.

Derived topology remains covered by `glam-gc` class/layout tests and may later
be exposed only through test or profiling diagnostics. Gate G2 instead checks
that every source-inventoried graph-bearing representation maps to one complete
stable family record and that every requested layout has passed collector
verification. No collector API change was needed.

### GCI-006 — I3 lacks a concrete scoped evaluation-quantum carrier

**Classification:** oversized phase and authority-design gap  
**Priority:** high  
**Confidence:** high  
**Status:** open

I3 asks mutator authority to travel through construction, evaluation,
workers, reflection, nets, compiler/macros, events, and diagnostics. The
collector's `Mutator<'heap>` is deliberately lifetime-bound and neither
`Send` nor `Sync`. Current `EvalContext` is cloneable and may be parked, and
`EvaluationTaskMachine::poll` receives only a step budget
([`session.rs`](../../src/evaluation/session.rs#L296),
[`task.rs`](../../src/evaluation/coordinator/task.rs#L132)). A mutator therefore
cannot simply become another stored context field.

Attempting the complete I3 as one phase would mix a new authority protocol,
worker scheduling boundaries, semantic lock audits, and mechanical call-site
migration.

**Recommended resolution:** begin with I3A, defining an explicit scoped
carrier such as `EvaluationQuantum<'mutator>` or `ManagedAccess<'mutator>`.
Machine polling and other internal boundaries receive or derive that carrier
only for the active quantum. A checked TLS convenience may support recursive
same-heap entry, but may not become the safety basis for dereference.

Then partition migration into:

1. public construction and synchronous evaluator/assembler access;
2. coordinator machine polls and worker quanta;
3. reflection, stores, events, diagnostics, and host conversion;
4. compiler, macro, and closed-value construction;
5. interaction-net construction and reduction; and
6. forced-order checks for sleeping, blocking, nested runtimes, and lock
   release.

Compile-fail tests should prove that mutators, allocators, managed borrows, and
quantum authority cannot escape their regions.

### GCI-007 — I4, I7, and I8 do not cleanly assign exact trace responsibility

**Classification:** trace-soundness sequencing ambiguity  
**Priority:** high  
**Confidence:** high  
**Status:** open

I4 says every then-current `core::Value` variant and transitive structure
receives an exact trace, while I7 and I8 later “audit and extend” persistent
collection and net tracing. This is safe only if later phases never introduce
the first missing adapter after a managed edge could already occupy that
structure.

The collector's worklist bounds recursion between `Gc` allocations, but it
does not bound arbitrary recursive traversal performed inside one `Trace`
implementation. A logical RPDS/FingerTree adapter can therefore be edge-exact
yet still overflow the Rust stack on a deep external spine.

**Recommended resolution:** make phase ownership explicit:

- I4 supplies exact, non-recursive structural adapters for every container
  which may later contain a `Gc`, even while those adapters initially report
  no managed edge;
- every I5–I10 representation migration updates its adapter in the same
  checkpoint that introduces the edge;
- I7 reconciles concrete persistent-node coverage and duplicate-work cost; and
- I8 reconciles concrete net storage, synchronization, and mutation gateways.

No later “audit” may be permission to leave a known placeholder in an unsafe
`Trace` implementation.

### GCI-008 — The interaction-net trace lock rule is incorrect

**Classification:** synchronization and trace-safety defect in plan  
**Priority:** high  
**Confidence:** high  
**Status:** open

I8 currently says that acquiring a net lock while tracing should be
unnecessary because collection waits for all mutators
([integration plan](../plans/GarbageCollectorIntegration_2026-08-19.md#phase-i8--interaction-net-migration-and-trace-audit)).
Stop-the-world exclusion can prove that no legitimate mutator contends for the
lock; it does not grant safe Rust access to data still stored behind a
`Mutex`.

**Recommended resolution:** trace the exact net state under its semantic lock.
Prefer `try_lock().expect(...)` if I3 proves every legitimate net lock is
released before mutator exit: this diagnoses a missing mutator boundary rather
than deadlocking the collector. A blocking lock is acceptable only with an
equally explicit no-contention proof. Never omit edges or inspect mutex-owned
data unsafely merely because the world is stopped.

Decide whether `SharedRuntimeNet` remains an external synchronized owner or
becomes a managed outer allocation before its migration begins; that choice
does not need to rewrite generic net topology.

### GCI-009 — Several pre-G2 checks require production collection too early

**Classification:** verification chronology defect  
**Priority:** high  
**Confidence:** high  
**Status:** open

Gate G2 does not pass until closure, opaque, cache, collection, net, and
runtime-root inventories are closed through I10. Nevertheless, I7 asks a
public persistent collection to survive a full collection and permit a
backedge cycle to be reclaimed, while I9 asks for eventual reclamation after
dropping owners. Running a real full collection over the production runtime at
either point could traverse still-unclassified graph families.

I5 already uses the correct distinction: collector-ready isolated fixtures may
force reclamation, while the complete production graph does not collect.

**Recommended resolution:** apply the same distinction throughout I5–I10.
Each phase may use an isolated, closed fixture to prove its own trace and
reclamation behavior. Production tests during those phases latch ownership,
trace construction, and drop behavior with collection disabled. I11 owns the
first forced full collection over an actual complete runtime and repeats each
family's reclamation case there.

### GCI-010 — Later integration phases are too large for safe verification

**Classification:** implementation-risk partitioning  
**Priority:** medium  
**Confidence:** high  
**Status:** resolved 2026-08-25

I1, I3, I4, I6, I9, I10, and I11 each cross several independently risky
ownership or synchronization boundaries. This conflicts with the roadmap's
policy to divide a checkpoint before implementation when it spans several
unsafe or scheduler boundaries.

**Recommended resolution:** partition at least:

- **I1:** collection policy/dependency; value-domain topology; factory and
  scoped allocation; layout/ledger reconciliation; lifecycle regression;
- **I2:** public wrapper/provenance; opaque handles and runtime-mediated
  observation; prototype and production-switch inventory;
- **I4:** value shell/leaves; argument/failure structures; persistent
  adapters; net adapter; public-root switch;
- **I6:** functions/applications/fixpoints; metadata; failures/reflection/net
  construction;
- **I9:** runtime caches; coordinator/evaluation; reflection store;
  diagnostics/events; assembly/compiler/CLI; final source inventory;
- **I10:** deferred closures; opaque registration; scoped opaque access and
  finalization; final containment audit; and
- **I11:** Gate G2 audit; controlled forced collection; concurrency/finalizer
  schedules; Gate G3 certification.

Each checkpoint should name the representation migrated, exact tests latched,
and collection mode permitted at its end.

**Resolution:** the integration plan now partitions every identified oversized
phase. I1 separates policy, domain topology, scoped allocation, layout/ledger,
and lifecycle work; I2 separates provenance, opaque-handle observation, and
the production-switch inventory; I3 has six authority/boundary checkpoints;
I4 separates shell/leaves, closure containment, argument/failure structures,
persistent adapters, net adapters, and the public-root switch; and I6 separates
functions, metadata, failures, and reflection/net-construction payloads.

Runtime integration is divided into cache, coordinator/evaluation, reflection
store, diagnostics/events, assembly/compiler/CLI, and final inventory
checkpoints in I9. I10 separates deferred closures, opaque registration,
scoped finalizable access, and the final containment audit. I11 now begins with
Gate G2 certification, then controlled production collection, deterministic
worker/finalizer schedules, and Gate G3 certification.

Each checkpoint identifies its representation or authority boundary,
verification fixtures, and permitted collection mode. Production remains
`NoAuto` through I11A; I11B-I11D permit only explicit controlled collection.
Passing G3 authorizes later I12 maintenance but does not silently enable
automatic collection. No implementation change was required for this finding.

### GCI-011 — Finalizer access to Glam runtime services is unspecified

**Classification:** lifecycle and ownership authority gap  
**Priority:** high  
**Confidence:** medium-high  
**Status:** open

I10 permits managed opaque destructors to allocate, evaluate, schedule work,
and emit diagnostics while the collector has installed its finalizer mutator.
The collector intentionally does not expose a globally discoverable “current
mutator,” and arbitrary `Drop` receives no Glam runtime context. Giving a
managed opaque allocation a strong factory or runtime-domain owner would form
the heap ownership cycle which I1 is intended to prevent.

**Recommended resolution:** add an I10 design checkpoint. Select either:

- a weak `RuntimeValueDomain`/`RuntimeSharedResources` capability stored in the
  managed payload and upgraded only during ordinary finalization; or
- a narrowly scoped Glam TLS bridge installed alongside the collector's
  finalizer mutator.

The capability must fail harmlessly during last-owner terminal teardown, must
not make a managed allocation own its heap, and must not permit rooting or
observing the allocation whose `Drop` is already running. Tests must cover
ordinary finalization, domain teardown, diagnostics/tasks emitted by a
destructor, and a destructor panic with untouched work retried later.

## Recommended Resolution Order

Before implementing the remaining I1 ownership checkpoints:

1. use GCI-001's completed no-auto collection mode for the production heap;
2. preserve GCI-003's completed value-domain topology and authorized owner
   matrix;
3. preserve GCI-005's completed stable ownership-ledger reconciliation
   contract; and
4. preserve GCI-010's completed checkpoint partition and collection-mode
   boundaries.

Before the public-root prototype or production switch:

5. preserve GCI-002's completed opaque-value and runtime-observation contract;
6. resolve GCI-004's fallible provenance operation; and
7. partition I2 around those decisions.

Before managed recursive nodes:

8. implement the I3A authority-carrier spike from GCI-006;
9. clarify exact trace ownership under GCI-007; and
10. close I4B before I5 introduces a managed edge into any type-erased
    boundary.

Before Gate G2 and production forced collection:

11. correct the net-lock protocol in GCI-008;
12. defer whole-runtime reclamation checks as required by GCI-009;
13. resolve finalizer runtime authority under GCI-011; and
14. finish the partitioned root/closure/opaque source audit.

## Review Decision

Shift work from isolated collector development to integration. Gate G1's
collector is sufficient for that transition, and C7/C8 stress and tuning can
continue later in response to production use.

Do not begin the current I1 as one checkpoint. First revise the integration
plan and ownership ledger to resolve or schedule the findings above. The first
implementation checkpoint should be the collector's manual/non-automatic
collection policy, followed by the runtime value-domain topology and its
latched lifetime matrix.
