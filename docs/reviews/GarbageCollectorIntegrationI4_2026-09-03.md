# Glam GC Integration Phase I4 Review — 2026-09-03

Baseline: `7bc765b`, including completed Phases I4.0-I4F.2f.3. Review
corrections are recorded below.

Status: complete. Phase I4 establishes the production managed outer value
node, the inline-or-registered-root public/runtime facade, exact durable-root
ownership, passive managed destruction, and the compatibility edge vocabulary
which I5-I8 must replace family by family. All review findings are resolved;
no finding blocks I5. Production collection remains disabled and Gate G2 is
not yet established.

## Scope

This is the mandatory post-implementation review for integration Phase I4. It
audits the implementation against:

- the I4 requirements in
  [`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md);
- the ownership, trace, destruction, and production-collection gates in
  [`GarbageCollectionRoadmap_2026-08-19.md`](../plans/GarbageCollectionRoadmap_2026-08-19.md);
- the stable family and durable-owner records in
  [`GarbageCollectorOwnershipLedger_2026-08-20.md`](../plans/GarbageCollectorOwnershipLedger_2026-08-20.md); and
- the collector safety contract certified by
  [`GarbageCollectorGateG1_2026-08-25.md`](GarbageCollectorGateG1_2026-08-25.md).

The review asks whether I4 provides an exact, passive, runtime-local managed
outer shell; whether every value which can survive a mutator region is already
a registered root or a deliberately bounded compatibility payload; whether
closures, opaque values, callbacks, caches, failures, synchronized nets, and
type-erased storage have explicit ownership; whether access and publication
gateways are closed; and whether the verification suite matches the resulting
production representation.

It does not certify recursive managed payloads or authorize a full collection
of the complete production graph. Lazies and promises remain I5, functions,
applications, metadata, and failures remain I6, persistent collections remain
I7, synchronized nets remain I8, and the lifecycle/opaque/whole-graph gates
remain I9-I11.

## Outcome

The implementation has one production root representation:

```text
RuntimeValueRoot / api::Value
  -> PreparedRuntimeValueRoot
       -> InlineInteger(i64) + weak RuntimeValueObserver
       -> Root<ManagedValueNode> + weak RuntimeValueObserver
            -> compatibility core::Value payload
```

The inline arm allocates no managed slot. The managed arm uses the collector's
existing shared root cell, so cloning a public/runtime handle adds neither a
managed allocation nor another root-registry entry. Both arms become
inaccessible when the value domain disappears, and neither retains that
domain. Managed access validates exact heap identity; inline access validates
the weak observer.

`ManagedValueNode` is the only production Glam managed family introduced by
I4. Its wildcard-free dispatch covers all thirteen current `core::Value`
variants. Every arm currently reports zero managed edges because recursive
payloads still use ordinary Rust ownership or independently registered roots.
That is an exact description of the current representation, not a tracing
placeholder: I5-I8 are source- and plan-latched to replace the affected arm in
the same checkpoint which first installs a `Gc` edge.

The node has passive destruction. Callback-bearing host calls, reflection
reservations, and opaque retirement state were extracted to the runtime's
external-owner registry before the production switch. Managed finalization
therefore releases only passive Rust ownership; active lifecycle work remains
in separately inventoried external owners and is retired outside collector
finalization.

Every durable value surface identified by the syntax-backed owner inventory
stores `RuntimeValueRoot`, public `Value`, or a separately reviewed external
owner. This includes canonical and extension caches, evaluator/coordinator
records, task and readiness reports, reflection store/protocol/machine state,
diagnostics, runtime input/output journals and deliveries, compiler/module and
macro records, CLI/configuration state, and synchronized-net facades. The
owner-level collection matrix constructs, publishes, observes, and retires
those roots in fresh collector-ready runtimes.

## Trace and Mutation Accounting

I4's `CompatibilityValueEdges` adapters are an executable migration ledger,
not the production node's current pointer tracer. They enumerate the exact
semantic children of:

- builtin/application/function and explicit semantic-computation payloads;
- failures and ordered context frames;
- lazy source/result, promise assignment, and metadata, while reflection
  computation records the external-root disposition selected by I4F.2b.2;
- list and dictionary contents through non-forcing logical iteration; and
- net/function identities and core-net payloads without reduction or
  materialization.

The adapters report direct edges only. They do not force, format, compare, or
recursively visit values. Persistent shared spines may be visited once per
logical occurrence, which is correct and deliberately leaves structural
deduplication to I7 or later profiling work.

One-write and replaceable compatibility state remains behind named gateways:
lazy caching, promise publication, reflection reservation/initialization, and
net mutation. Persistent immutable values require no mutation gateway. The
managed outer shell is immutable after allocation, and its allocate-and-root
gateway is private to the root representation.

## Ownership and Access Accounting

The source-backed durable-owner inventory classifies every production
declaration which stores a value/root, failure, synchronized net, managed
pointer, type-erased payload, or callback. It separately records:

- registered managed/public root surfaces;
- compatibility recursive payloads scheduled for exact I5-I8 migration;
- synchronized-net owners;
- admitted type-erased caches and opaque families;
- callback captures held by external owners;
- bounded evaluator/compiler/parser locals; and
- edge-free companion state.

`RuntimeValueCache` no longer accepts arbitrary `Any`: only a recorded
`RuntimeCacheFamily` may be installed, every retained root is enumerated and
same-runtime checked before publication, and candidate construction and loser
destruction remain outside the cache mutex. Opaque values similarly require a
reviewed `OpaquePayloadFamily`; the current production families carry either
edge-free identity/provenance or explicitly external lifecycle capabilities,
never a bare managed pointer or recursive core value.

The public `Value` and `EvaluatedValue` handles expose no representation-
derived equality, ordering, hashing, kind, provenance, or core projection.
Semantic observation requires matching live runtime authority. Internally,
bounded projections remain only where compatibility payload migrations still
need them. A repository-wide latch rejects the retired authority-free
`as_core`/`into_core` and non-registering root-constructor spellings, while a
separate exact inventory counts every legitimate `RuntimeValueRoot::new`
publication by source owner.

## Collection and Gate Accounting

All production runtime value domains still use `CollectionPolicy::NoAuto`.
I4's collection tests use fresh runtimes whose reachable graph is closed over
the owner or adapter under test. They prove rooted survival, publication and
observation through the real owner, retirement, and eventual unrooted
reclamation without claiming that an arbitrary production runtime graph is
closed.

Gate G2 remains closed for three substantive reasons:

1. external compiler/host callback environments still need the I10A backedge
   and lifecycle audit;
2. the synchronized `SharedRuntimeNet` owner still needs I8's managed outer
   cell, exact trace, durable-handle, and mutation-gateway closure; and
3. public opaque construction still needs the I10B.0 representation decision
   and final registration audit.

In addition, I5-I8 must perform the planned recursive-payload migrations
before I11 can force collection over a complete production runtime. I4 has
closed the durable root boundary early enough that no later phase may discover
its first long-lived bare value and defer it as ordinary cleanup.

## Verification Audit

The current suite covers:

- production node layout/drop admission, exhaustive variant dispatch, private
  allocation, rooted survival, and unrooted reclamation;
- inline construction without allocation, shared root-cell cloning across
  threads, same-runtime nested access, foreign-runtime rejection, and dead-
  domain behavior;
- compile-time absence of representation-derived public/private root traits;
- closure, host-call, opaque-family, and active-owner containment;
- exact non-forcing compatibility edge visitation for recursive payloads,
  persistent collections, and nets;
- complete canonical and extension-cache publication and retirement;
- the syntax-backed durable-owner and root-publication inventories;
- focused forced-collection fixtures for coordinator, report, reflection,
  diagnostic, event, delivery, compiler, macro, CLI, and net owners; and
- the repository-wide forbidden compatibility-escape latch.

The mandatory Rust checks pass after the review corrections. Repetition is not
used as evidence for schedule-sensitive behavior; I4's concurrency-sensitive
owners retain their existing barrier- and publication-order fixtures from I3
and the work-boundary transition.

## Drift Assessment

### Intentional and justified

1. **The monolithic shell became the production outer node.** I4A began with a
   closed representative fixture; I4F.2 promoted the same correctness-first
   granularity after containment and durable-root closure. Compact/split tags
   remain the separate value-representation project.
2. **Recursive payloads remain compatibility-owned after the root switch.** A
   registered outer root and zero-edge current visitor permit a narrow atomic
   public cutover without pretending later `Gc` interiors already exist.
   Named adapters and source latches preserve the exact I5-I8 migration work.
3. **Small integers remain inline.** This carries I2's allocation-free
   opportunity into production without selecting the final tag layout.
4. **Active destruction moved outside managed values.** The external-owner
   registry is a deliberate ownership split, not conservative tracing. It
   preserves callback/reservation/opaque lifecycle behavior while satisfying
   Glam's passive managed-drop contract.
5. **I4F was partitioned much more finely than first planned.** The additional
   checkpoints isolated cache admission, owner families, construction-order
   prerequisites, the atomic switch, and owner-level collection proofs. They
   changed verification granularity, not semantics.
6. **Compatibility edge adapters survive I4.** Removing them now would erase
   the migration oracle. Each has a named I5-I8 consumer and no public escape.

### Corrective new information

1. **Canonical roots require post-domain initialization.** The runtime cache
   now installs one complete canonical bundle through `OnceLock` only after
   the heap and value domain exist, preventing partial publication and making
   managed root construction possible.
2. **Late terminal publication requires weak domain authority.** Promise and
   coordinator publishers retain a weak observer rather than reconstructing
   authority from a numeric runtime ID.
3. **Durable owners needed real collection fixtures, not only type migration.**
   I4F.2e verifies owner-specific construction through retirement in isolated
   closed runtimes and exposed no remaining hidden owner.

### Accidental or convenience-driven drift

None remains after the findings below were resolved.

## Review Findings

### I4R-001 — Current plan indexes stopped before the completed production switch

**Classification:** documentation and verification-index drift

**Status:** resolved

The integration-plan and roadmap status summaries stopped partway through
I4F, while the ownership ledger still presented the deleted I2 prototype and
I4A representative shell as current stable families. Its verification matrix
named deleted prototype/shell tests, several current owner rows still called
registered roots compatibility roots, and two Gate G2 blockers described
I4F work which was already complete.

The status summaries now record reviewed I4. The ownership ledger records the
production `PreparedRuntimeValueRoot`/`ManagedValueNode`, current owner
terminology, current executable test names, the complete I4F owner matrix, and
only the remaining Gate G2 blockers. Historical phase narratives in the
integration plan remain as implementation history and explicitly lead to the
I4F deletion record rather than being rewritten as if the prototypes never
existed.

### I4R-002 — The production managed family lacked its stable layout latch

**Classification:** stable-ledger and verification gap

**Status:** resolved

The deleted representative shell had an exact x86-64 layout record, but the
production `ManagedValueNode` test checked only that its requested extent used
the central slot policy and that allocation succeeded. The stable ledger had
therefore retained the obsolete 72/8 fixture layout instead of recording the
production 64/8 node.

The production node now compile-time latches its x86-64 size and alignment at
64/8. `managed_value_node_family_contract_and_lifecycle` continues to check
the central requested extent and actual allocator acceptance, then proves
rooted survival and unrooted reclamation. A future representation change may
change the layout, but it must deliberately update both the assertion and the
stable ledger.

## Deferred Work

- I5-I8 replace compatibility recursive ownership and zero-edge production
  arms with exact managed edges one family at a time.
- I9 audits registered-root retirement and runtime lifecycle after those
  migrations.
- I10 closes external callback environments and the opaque representation.
- I11 reconciles Gate G2 and performs the first complete production-graph
  forced collections.
- I12 decides runtime maintenance/readiness integration and later collection
  policy. No I4 result preselects automatic collection.
- Compact tagged values, structural persistent nodes, and tracing-work
  optimizations remain their separately planned performance work.
