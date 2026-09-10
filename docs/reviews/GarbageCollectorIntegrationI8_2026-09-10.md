# Glam GC Integration Phase I8 Review — 2026-09-10

Baseline: `d8c896a`, the completed grouped post-I6/I7 review. The reviewed I8
implementation spans `f57599a` through `9d17b2b`.

Status: complete. No open finding invalidates the managed core-net boundary or
blocks I9. Production remains `CollectionPolicy::NoAuto`; this review does not
authorize collection over a complete production runtime.

## Scope and Method

This review compared I8's implementation with the GC integration plan,
collector contracts, architecture record, ownership ledger, I5 review, and
post-I6/I7 baseline. It audited:

- semantic-handle, durable-root, and temporary root-holder ownership;
- the complete runtime-net topology and non-reducing trace projection;
- exact leaving/adding edge publication for every semantic rewrite family;
- cursor claim, source-frontier, dependency, convergence, unwind, revision,
  and disturbance chronology;
- managed destruction and lock/callback boundaries;
- rooted survival and exact reclamation for baseline and I8-only cycles;
- retirement of net-specific compatibility owners and adapters; and
- I9-I13 entry assumptions after the net cutover.

The review used source-backed and compile-exhaustive inventories as change
detectors. It did not treat repeated execution as evidence for a disputed
ordering; I8's new edge observations are synchronous under the net mutex, and
the concurrent cursor publication orderings retain the deterministic Cursor
WHNF barrier fixtures established before I8.

## Plan-to-Implementation Accounting

| Checkpoint | Implemented disposition and evidence |
| --- | --- |
| I8A.0 | `CoreRuntimeNet` is one non-rooting `ManagedCoreNetEdge`; `ManagedCoreNetRoot` is one registered root and projects an edge only through matching `RuntimeValueAccess`. Access-free self-rooting and weak-domain observers are absent. Compile-time size latches and scoped-access tests cover the final facade shape. |
| I8A.1 | `runtime_payload_owner_inventory_is_compile_exhaustive` destructures templates, runtime entries, all node/copy/cursor/active/stuck states, and transition address sets. `RuntimeNet::visit_logical_payloads` reports data, operators, copy/dependency sources, and specialization stuck reasons without reducing, following, claiming, or materializing. |
| I8A.2 | `RuntimeNetMutationGateway` separates edge-free coordination edits from exact semantic transitions. Fan duplication, erasure, call-to-copy, call-to-operator, operator completion, specialization failure, cursor materialization/convergence, and dependency resolution use bounded address sets resolved beneath the existing net lock. Deterministic probes cover each value-changing family and the stale no-change case. |
| I8A.3 | Trace uses nonblocking acquisition during collector-exclusive access. Topology revision and disturbance publish after the coupled mutation, while callbacks and semantic evaluation remain outside the net lock. Claim release and unwind restore edge-free owner state; source installation/removal and blocked source dependencies use exact deltas. |
| I8B.1 | `managed_core_net_stuck_reason_self_cycle_is_traced_and_reclaimed` closes the I8-only specialization-stuck topology. |
| I8B.2 | `managed_operator_payload_cycle_survives_ready_and_claimed_work_then_reclaims` proves the same exact semantic graph survives both ready and claimed work states, whose coordination records add no hidden edge. |
| I8B.3 | The existing I5F.3c three-cell remote-cursor source cycle remains the exact fixture for unchanged copy/source topology. The compile-exhaustive mapping classifies local cursor state as edge-free and source observations as the existing source-net edge; no duplicate fixture was warranted. |
| I8C | Six net-specific compatibility projections, the bounded `CoreRuntimeNetPayload` layer, obsolete trace counters, and the test-only shared-net adoption bridge are gone. `managed_core_net_has_no_legacy_owner` rejects their return. The central compatibility `Value` walk remains deliberately because raw collection, metadata, failure, and argument interiors still use it. |

## Ownership, Trace, Mutation, and Lifecycle Review

The production core net has one managed identity. Interior `CoreRuntimeNet`
values do not root or reopen the value domain; durable work descriptors use
`ManagedCoreNetRoot`, and bounded access reconstructs the interior facade.
`CorePreparedCopySource` retains its source root until the source edge is
installed, while `CoreFrontierObservation` and `NormalizationRequest` retain
only one root plus scalar topology data. Generic `SharedRuntimeNet` remains an
external owner only for non-core specializations and tests.

`ManagedCoreNetCell::trace` obtains one stable logical snapshot while
collector exclusion prevents a managed core mutation. The visitor reaches raw
semantic values through the central compatibility walk and terminates nested
net relationships at exact managed edges. Function-call lazy sources and
function/computation-capture operators report their nested net directly; no
retired net-compatibility projection is needed. Structural ports, IDs, wait
tokens, normalization state, local cursor dependencies, and no-rule failures
remain edge-free.

Post-publication semantic changes are coupled to the owning managed cell.
Address sets contain representation locations rather than cloned payloads and
are resolved immediately on the appropriate side of the edit while the net
mutex is held. Edge-free claim, wait, retry, release, and normalization changes
still publish ordinary topology revisions without pretending to change the
managed graph. No production core writer or cursor/reduction helper bypasses
the gateway inventory.

Managed destruction remains passive under the I4.0 contract. The cell's only
direct destruction action closes an edge-free disturbance companion; it does
not enter the evaluator, scheduler, runtime, host, or reflection services.
Trace does not invoke semantic code, and semantic call/operator evaluation
occurs only after its net claim has left the locked access batch.

## Drift Classification and Resolution

### Intentional and justified

- Runtime-net agents remain ordinary allocations inside one managed outer
  cell. Per-agent GC allocation was not required to close cycles and would
  enlarge this correctness-first transition.
- Exact deltas identify current representation owners rather than cloning or
  recursively tracing payloads. This keeps current `NoAuto` cost negligible
  and leaves a precise barrier seam for a later concurrent collector.
- Generic non-core `SharedRuntimeNet` support remains independent of
  `glam-gc`; only the production core specialization is managed.
- I8B reused I5F.3c for unchanged cursor/source topology. Adding a second
  identically shaped cycle would increase test volume without increasing
  coverage.
- The central compatibility-value walk remains until Value Representation
  Refinement. I8 retired only adapters whose sole role was net identity.

### Corrective information recorded

- I8A.0's observer retirement did not create a root family, but it changed the
  representation of existing net-root holders. I9 now explicitly audits the
  root-only `ManagedCoreNetRoot` in `CorePreparedCopySource`,
  `CoreFrontierObservation`, and `NormalizationRequest` against I4F's owner
  baseline.
- The architecture and ownership ledger still described the whole-net
  pre/post correctness bridge and final I8 audit in future tense. They now
  record the implemented exact-delta boundary and completed audit.

### Accidental drift

No unresolved accidental implementation drift was found. The documentation
drift above was repaired during this review.

## Future-Phase Review

- **I9 remains necessary and now has a concrete I8 delta.** It should verify
  retirement of the existing net-root holders after their observer removal,
  while recording the other I4F owner families as unchanged. Discovering a
  new root family there remains a chronology failure.
- **I10 remains unchanged.** External host/compiler closures and opaque `Any`
  storage are still the unclosed containment boundaries; the net cutover does
  not make either traceable.
- **I11 Gate G2 can consume I8's final inventories.** Whole-production-graph
  collection remains blocked on I9 and I10, not on another net migration. Its
  source closure should retain the exact-writer, topology, durable-owner,
  cycle-mapping, and no-legacy-owner latches named by I8.
- **I12 remains correctly deferred.** Exact mutation deltas prepare a future
  barrier, but they do not select automatic collection or define runtime GC
  readiness.
- **I13 must not repeat I8C.** It may retire the remaining central
  compatibility walk only after its raw structural `Value` interiors receive
  exact replacements, and should otherwise limit net work to documentation or
  measured representation cleanup.

## Verification

The stage is covered by:

- compile-exhaustive payload, topology, operator, durable-owner, and cycle
  inventories;
- exact-delta probes for every semantic rewrite family, including the real
  erase path, final cursor convergence, and stale dependency resolution;
- non-reducing/no-forcing trace sentinels;
- deterministic Cursor WHNF publication, dependency, claim-release, unwind,
  and normalization fixtures;
- isolated rooted-survival and exact-unrooted-reclamation fixtures for the
  final net topology matrix;
- the source-backed no-legacy-owner and writer-gateway latches; and
- I8A.0's focused strict-provenance Miri probe over root projection and net
  access.

The post-review routine repository checks passed on 2026-09-10. No unsafe
boundary was added by the documentation corrections, and no unresolved
finding blocks Phase I9.
