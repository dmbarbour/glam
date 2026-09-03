# Glam GC Integration Plan — 2026-08-19

Status: in progress; Phases I0 through I3 and their mandatory reviews plus I4.0
through I4E and I4F.1a through I4F.1c are complete. I4F.1d.1 and I4F.1d.2a
are complete; I4F.1d.2b is pending.
Collector Gate G1 passed on 2026-08-25. The remaining integration work follows
the completed owner-matrix, stable-ledger, and low-risk checkpoint corrections
from the integration review.

This plan integrates the collector defined by
[`GarbageCollectorImplementation_2026-08-19.md`](GarbageCollectorImplementation_2026-08-19.md)
into the runtime, public value facade, evaluator, workers, reflection, and
interaction nets. Cross-plan invariants and enablement gates live in
[`GarbageCollectionRoadmap_2026-08-19.md`](GarbageCollectionRoadmap_2026-08-19.md).

## Phase Status

| Phase | Status | Outcome |
| --- | --- | --- |
| I0 | complete | complete ownership and mutation ledger |
| I1A | complete | immutable no-auto collection policy and operational pressure statistics |
| I1B | complete | runtime value-domain topology and authorized owner matrix |
| I1C | complete | factory-scoped managed allocation and rooting authority |
| I1D | complete | centralized managed-slot policy and stable ledger contract |
| I1E | complete | runtime/profile/value-domain lifecycle reverification |
| I1 | complete | runtime-owned heap, collection disabled; post-I1 review passed |
| I2A | complete | opaque inline-or-managed public-root representation prototype and provenance checks |
| I2B.1 | complete | transport-only prototype surface with no handle-derived semantic relations |
| I2B.2 | complete | live-runtime-authorized comparison, observation, and owned extraction prototype |
| I2C | complete | nested scoped access and mechanically checked production compatibility-access inventory |
| I2 | complete | opaque public-root contract, runtime-authorized observation, access inventory, and post-I2 review |
| I3A.1 | complete | lifetime-bound runtime/evaluator access and mutator-free poll-context prototype |
| I3A.2 | complete | weak parked demand routes and checked temporary claim-owned domain access |
| I3A.3 | complete | claim-derived scheduler poll context, common task/client/spark routing, and mutator-free release/wait boundaries |
| I3A.4 | complete | runtime-rooted machine completion outcomes, preserved effect roots, and exhaustive deferred-interior inventory |
| I3B.1a | complete | thread-bound evaluator-step authority and source-latched direct-entry compatibility inventory |
| I3B.1b | complete | claimed value/application/sequence spine with one latched direct-compatibility gate |
| I3B.1c.1 | complete | scoped builtin dispatcher plus numeric and unit-assertion migration |
| I3B.1c.2 | complete | scoped recursive comparison and compiler-pattern inspection |
| I3B.1c.3 | complete | scoped dictionary and list transformation families |
| I3B.1c.4 | complete | scoped object and pure conditional/list-effect construction |
| I3B.1c.5 | complete | scoped pure annotations and source-latched durable builtin seams |
| I3B.1c | complete | ordinary builtin cluster migration |
| I3B.1d.1 | complete | scoped public value construction and nested helper reuse |
| I3B.1d.2 | complete | matching-runtime owned evaluated-value extraction |
| I3B.1d.3 | complete | public construction/extraction closure audit |
| I3B.1d.4 | complete | weak non-retaining evaluated-value observer ergonomics |
| I3B.1e | complete | evaluator context-surface inventory and closure verification |
| I3B.1 | complete | scoped construction and core evaluator migration |
| I3B.2 | complete | poll/wait driver separation |
| I3C.1 | complete | unified claimed/direct poll routing and scoped spark demand |
| I3C.2 | complete | rooted wait observations, scoped projection, and release audit |
| I3D.1 | complete | stable reflection reservations and post-evaluator one-time activation |
| I3D.2a | complete | bounded request-interpreter evaluation service |
| I3D.2b | complete | explicit request, interpreter, and continuation phases |
| I3D.2c | complete | forced-unfused reference path and bounded standard-effect fusion |
| I3D.2d | complete | effect-machine closure and callback-boundary audit |
| I3D.3a | complete | exact-domain private core-net facade |
| I3D.3b | complete | matching-runtime scoped core-net observation and mutation |
| I3D.3c | complete | closure-scoped same-net normalization batches |
| I3D.3d | complete | bracketed callable active-pair claims and exact replay fallback |
| I3D.3e | complete | bracketed operator active-pair claims and exact replay fallback |
| I3D.3f.1 | complete | private cursor-claim guard and pairless-step migration |
| I3D.3f.2 | complete | active-pair cursor terminalization before step publication |
| I3D.3f.3 | complete | manual handoff removal, forced lifecycle tests, and closure audit |
| I3D.3f | complete | guarded cursor lifecycles for both owner forms |
| I3D.3g | complete | one-shot contention handoff, blocked-before-park ordering, and local facade closure |
| I3D.4a | complete | scoped net builtin, access, and active-pair semantic helpers |
| I3D.4b | complete | bounded construction-callback demand and scoped result replay |
| I3D.4c | complete | reflection/net region inventory and lock/wait closure audit |
| I3D.4 | complete | reflection and net region audit |
| I3E.1a | complete | scoped semantic-thunk contract and list-effect migration |
| I3E.1b | complete | rooted host-call phase outside evaluator authority |
| I3E.1c | complete | module/binary loader migration and stable-content contract preservation |
| I3E.1d | complete | exhaustive lazy-producer and compatibility-access inventories |
| I3E.1 | complete | semantic thunks separated from deterministic deferred host calls |
| I3E.2a | complete | rooted closed-helper bundles and atomic cache publication |
| I3E.2b | complete | rooted macro rewrite state and client-demand lookup/result forcing |
| I3E.2c | complete | rooted recursive module setup, declaration state, import handoff, and result drain |
| I3E.2d | complete | compiler root/projection inventories and architecture reconciliation |
| I3E.2 | complete | bounded compiler, macro, and closed-value regions |
| I3E.3 | complete | bounded event, diagnostic, rendering, and executable callback regions |
| I3F | complete | multi-runtime admission, poll authority, worker-exit cache retirement, and managed-entry audit |
| I3 | complete | bounded evaluator/worker mutator regions; post-I3 review passed |
| I4 | pending | core trace vocabulary and leaf policy |
| I4.0 | complete | private managed-family admission, mandatory drop records, passive destruction fixture, and external-retirement separation |
| I4A | complete | initial monolithic shell selection, exhaustive variant dispatch, embedded leaf policy, and closed cycle fixture |
| I4B.1 | complete | constructor inventory and private admission boundaries |
| I4B.2 | complete | opaque payload sealing and provenance migration |
| I4B.3 | complete | explicit semantic computation captures and host-callback classification |
| I4B.4 | complete | containment inventory closure and verification |
| I4B | complete | closure and opaque managed-edge containment |
| I4C.1 | complete | immutable argument, application, and failure edge adapters |
| I4C.2 | complete | deferred source/cell compatibility adapters and mutation-gateway latches |
| I4C.3 | complete | cross-family exactness, no-forcing verification, and documentation closure |
| I4C | complete | recursive payload and failure structures |
| I4D.1 | complete | non-forcing list/map logical adapters and trace-work counters |
| I4D.2 | complete | shared-spine exactness, closed cycle reclamation, and documentation closure |
| I4D | complete | persistent collection adapters |
| I4E.1 | complete | generic runtime-net payload walk and core compatibility adapters |
| I4E.2 | complete | exhaustive no-reduction/materialization and closed-cycle verification |
| I4E | complete | non-reducing net value adapter |
| I4F.1a | complete | durable-owner inventory schema and executable baseline |
| I4F.1b.1 | complete | canonical root bundle and initialization seam |
| I4F.1b.2 | complete | admitted type-erased runtime attachments and compiler caches |
| I4F.1b | complete | canonical cache and type-erased attachment root surfaces |
| I4F.1c.1 | complete | non-forcing runtime failure-root compatibility boundary |
| I4F.1c.2 | complete | machine-poll, task, wait, terminal, and failure-ledger roots |
| I4F.1c.3 | complete | client-demand and remaining session-owner roots |
| I4F.1c.4 | complete | settlement, readiness, and evaluation-owner closure |
| I4F.1c | complete | evaluation, coordinator, and readiness root surfaces |
| I4F.1d.1 | complete | reflection store, snapshot, journal, and query roots |
| I4F.1d.2a | complete | reflection task failures and effect-lifecycle roots |
| I4F.1d.2b | complete | reflection protocol and host-snapshot roots |
| I4F.1d.2c | complete | isolated-search branch, block, and result roots |
| I4F.1d.2 | complete | reflection lifecycle, protocol, and search roots |
| I4F.1d.3a | complete | reflection request, journal, query, and task-handle roots |
| I4F.1d.3b | complete | reflection machine shell, terminal, and blocked-error roots |
| I4F.1d.3c.1 | complete | reflection branch effect/state roots and fused handoff |
| I4F.1d.3c.2 | complete | reflection work, outcome, cut, and retry root closure |
| I4F.1d.3c | complete | reflection branch, work, cut, and retry roots |
| I4F.1d.3d.1 | complete | reflection continuation, delimiter, and reset roots |
| I4F.1d.3d.2 | complete | reflection fixpoint roots and captured-control closure |
| I4F.1d.3d | complete | reflection continuation, reset, and fixpoint roots |
| I4F.1d.3e | complete | reflection decoded-request handoff and machine closure |
| I4F.1d.3 | complete | reflection machine frame and request roots |
| I4F.1d | complete | reflection store, protocol, and machine root surfaces |
| I4F.1e | complete | diagnostics, runtime-event, and delivery root surfaces |
| I4F.1f.1 | complete | assembly, compiler, module, and import roots |
| I4F.1f.2 | complete | macro, parser, and source-rewrite roots |
| I4F.1f.3a | complete | command-line search, completion, and token roots |
| I4F.1f.3b | complete | prepared and loaded configuration roots |
| I4F.1f.3c | complete | logger supervision, rendering, and callback roots |
| I4F.1f.3 | complete | binary configuration and logger roots |
| I4F.1f | complete | assembly, compiler, macro, configuration, and logger root surfaces |
| I4F.1g.1 | complete | synchronized-net owners and bounded driver work |
| I4F.1g.2 | complete | hidden-owner audit and closed inventory gate |
| I4F.1g | complete | net, hidden-owner, and gate closure |
| I4F.1 | complete | exhaustive durable root-surface conversion |
| I4F.2a.1a | complete | public handle semantic-trait and rendering retirement |
| I4F.2a.1b | complete | assembly, diagnostic, and error access retirement |
| I4F.2a.1c.1 | complete | test-only equality bridge retirement |
| I4F.2a.1c.2 | complete | test-only observer and constructor retirement |
| I4F.2a.1c | complete | public facade compatibility-test closure |
| I4F.2a.1 | complete | public facade, assembly, diagnostic, and error access retirement |
| I4F.2a.2a | complete | public evaluator and reflection-inspector access retirement |
| I4F.2a.2b | complete | evaluation driver and rooted-completion projection retirement |
| I4F.2a.2c | complete | core net and builtin construction adapter retirement |
| I4F.2a.2d | complete | core evaluation and net access closure |
| I4F.2a.2 | complete | core evaluator and net access retirement |
| I4F.2a.3a | complete | reflection protocol, store, and search access retirement |
| I4F.2a.3b | complete | reflection request access retirement |
| I4F.2a.3c.1 | complete | reflection branch and request-step access retirement |
| I4F.2a.3c.2 | complete | reflection transition and terminal access retirement |
| I4F.2a.3c.3 | complete | reflection wrapper, helper, and fixture access closure |
| I4F.2a.3c | complete | reflection machine access retirement |
| I4F.2a.3d | complete | compiler and module-lowering access retirement |
| I4F.2a.3e | complete | compiler-cache access retirement |
| I4F.2a.3f | complete | macro effect and runner access retirement |
| I4F.2a.3g | complete | parser replay and source access retirement |
| I4F.2a.3h | complete | lifecycle and host-integration closure |
| I4F.2a.3 | complete | reflection, compiler, macro, and host access retirement |
| I4F.2a.4 | complete | compatibility-access closure latch |
| I4F.2a | complete | compatibility observation and ownership-transfer retirement |
| I4F.2b.0 | complete | transitive destruction and active-owner inventory |
| I4F.2b.1 | complete | external host-call ownership extraction |
| I4F.2b.2 | complete | reflection-reservation ownership extraction |
| I4F.2b.3 | complete | opaque-payload ownership extraction |
| I4F.2b.4 | complete | passive-destruction closure gate |
| I4F.2b | complete | active-owner extraction before managed value ownership |
| I4F.2c.1 | complete | private production managed-node and family contract |
| I4F.2c.2 | complete | exact production-shell visitor composition |
| I4F.2c.3 | complete | private inline-or-root representation and provenance |
| I4F.2c | complete | private production-root and managed-shell preparation |
| I4F.2d.0 | complete | post-domain canonical root-bundle initialization |
| I4F.2d.1 | complete | weak terminal root-publication authority |
| I4F.2d.2 | complete | atomic production managed-root representation switch |
| I4F.2d | complete | atomic production managed-root switch |
| I4F.2e.1a | complete | runtime value-domain anchors and cache owners |
| I4F.2e.1b.1 | complete | coordinator task, wait, and blocked-record owners |
| I4F.2e.1b.2 | complete | client-demand operation and result owners |
| I4F.2e.1b.3 | complete | spark and pending session-activation owners |
| I4F.2e.1b | complete | coordinator, demand, spark, and session owners |
| I4F.2e.1c | pending | readiness, ledger, and report owners |
| I4F.2e.1 | pending | cache, evaluation, coordinator, and readiness collection fixtures |
| I4F.2e.2 | pending | reflection owner collection fixtures |
| I4F.2e.3 | pending | diagnostic, event, and delivery collection fixtures |
| I4F.2e.4 | pending | compiler, binary-host, net, and hidden-owner collection fixtures |
| I4F.2e | pending | closed durable-owner collection matrix |
| I4F.2f | pending | compatibility deletion and I4 reconciliation |
| I4F.2 | pending | public managed-root production switch |
| I5 | pending | managed lazy/promise cells, external lifecycle, and cycle reclamation |
| I6 | pending | functions, applications, metadata, failures |
| I7 | pending | persistent list and dictionary tracing |
| I8 | pending | managed core-net outer cells, exact tracing, and mutation gateways |
| I9 | pending | runtime-root lifecycle and retirement audits |
| I10 | pending | deferred closures and opaque boundaries |
| I10B.0 | pending | opaque representation decision review gate |
| I11 | pending | whole-production-graph forced collection |
| I12 | pending | runtime maintenance and threshold collection |
| I12A.0 | pending | GC operational-activity/readiness decision review gate |
| I12A | pending | explicit maintenance for immutable `NoAuto` runtimes |
| I12B.0 | pending | new-runtime collection-policy decision review gate |
| I13 | pending | redundant ownership removal and documentation |

## Major-Stage Review Policy

Every completed top-level integration phase, I1 through I13, ends with a
post-implementation review before work begins on the next top-level phase.
Subphases and low-risk checkpoints may proceed without separate reviews, but
adjacent top-level phases may be grouped into one major stage only when this
plan records that grouping before implementation of either phase begins.

Each review is a dated artifact under `docs/reviews/` and audits the
implementation against this plan, the collector implementation plan, the
roadmap, and the ownership ledger. It must:

- reconcile the implemented ownership, tracing, mutation-authority,
  synchronization, callback, destruction, and lifecycle boundaries;
- verify that the phase's source inventories, compile-time contracts,
  schedule-controlled tests, reclamation tests, and routine repository checks
  cover the boundary that was actually implemented;
- classify discovered drift as intentional and justified, corrective new
  information, or accidental/convenience-driven drift;
- update the plans, ledger, and durable documentation for intentional or
  corrective drift, rather than leaving the implementation as the only record;
- repair accidental drift, or turn it into an explicit later design/review
  gate with entry conditions, required evidence, and a hard blocking point;
  and
- record unresolved findings and the exact phase they block.

Drift from an earlier design is not inherently a defect. The review asks
whether the resulting boundary remains coherent, safe, testable, and faithful
to Glam's current needs. A phase is not marked complete, and the next
top-level phase does not begin, until its review has no unresolved finding
that invalidates the completed boundary.

An in-phase design gate such as I10B.0 or I12A.0 does not by itself satisfy
this policy: it reviews a decision before implementation. A dated gate that
also audits the completed implementation, such as the planned Gate G2 or G3
certification, may serve as the post-stage review when its artifact covers all
of the requirements above.

## Current Boundary

The isolated collector now supplies checked direct roots and a weak registry,
checked non-recursive graph tracing, eager non-moving sweep and run reuse,
durable non-rootable finalization with retry after payload panic, and restricted
last-owner terminal teardown. It completed C6D.3 and passed Gate G1. Production
API and ownership migration may now use that certified boundary, but no
production automatic or explicit collection may run before the complete graph
passes the later roadmap gates.

`RuntimeValueRoot` currently stores `{EvaluationRuntimeId, core::Value}`. It
protects provenance but does not register a collector root. Recursive core
ownership includes at least:

- `Arc<LazyCell>` and its source/result graph;
- `Arc<PromiseCell>` and successful assignments;
- metadata carriers;
- partial builtin arguments, lazy applications, fixpoints, reflection tasks,
  function stages, and net construction inputs;
- RPDS dictionary values and FingerTree/list chunks;
- shared mutable interaction nets containing core data;
- runtime value caches and compiler attachments;
- reflection store snapshots, volumes, queries, and transactions;
- task waits, client demands, sparks, deferred work, diagnostic values, and
  event input/output records; and
- type-erased opaque payloads and deferred Rust closures which may hide public
  roots.

No production collection may run until this graph is completely classified.

## Intended Value Shape

The provisional target keeps cheap scalar values inline and moves recursive
identity-bearing nodes behind managed pointers:

```text
core::Value
├── Atom / Builtin                 inline
├── Number / Binary                existing immutable leaf ownership initially
├── List / Dict                    persistent containers traced logically
├── Function / partial application traced recursive payloads
├── Net                            traced shared runtime-net payload
├── Lazy / Promised                Gc-managed identity cells
├── Metadata                       Gc-managed identity cell
└── Opaque                         conservative host boundary

api::Value
└── RuntimeValueRoot
    ├── optional Glam-owned inline scalar representation
    └── glam_gc::Root<core::Value>
```

Phase I2 decides whether `RuntimeValueRoot` is simply the collector's direct
managed root or a Glam-owned wrapper which keeps eligible scalars inline and
uses that same direct root for managed values. The collector does not accept an
alternate inline root cell or generalized registry payload merely to optimize
the public representation. A wrapper may privately retain a runtime ID during
migration for diagnostics or boundary enforcement, but live heap identity is
the authoritative provenance for managed access and neither identity is
publicly observable from the value handle.

Regardless of that representation choice, the public wrapper is an opaque
transport handle on its own. It supports cloning, dropping, and same-runtime
thread transfer, but no semantic equality, ordering, hashing, kind, extraction,
formatting of contents, or public runtime-identity observation. Those
operations require live matching runtime authority and may fail when the
domain is inaccessible or provenance does not match. This constrains authority,
not Rust call direction: an API may accept the value on a runtime service,
accept a scoped service or mutator on a value method, or support both ergonomic
forms. Clients which need a map key first obtain or compute ordinary host data
through an authorized operation; internal root identity is not exposed as a
substitute key.

This provisional shape is not the compact tagged-word transition. That work is
isolated in
[`ValueRepresentationRefinement_2026-08-19.md`](ValueRepresentationRefinement_2026-08-19.md)
and may begin only after the initial collector boundary is sound.

Every representation migrated in this plan must fit one slot in the
collector's fixed-size homogeneous typed-run geometry. The integration does
not request a large-object fallback, multi-run span, heterogeneous run, or
managed DST. Existing large/variable leaf storage remains audited external
ownership until a later representation plan chooses to decompose it.

## Phase I0 — Ownership and Mutation Ledger

Completed in
[`GarbageCollectorOwnershipLedger_2026-08-20.md`](GarbageCollectorOwnershipLedger_2026-08-20.md).
The ledger records the pre-GC graph, current layout baseline, synchronization
and mutation policy, semantic regression matrix, and the boundary defects that
must remain Gate G2 blockers. Concrete managed-layout and visitor decisions
remain provisional until their representation-migration phases; heap-local
class geometry is deliberately outside the integration ledger.

Create a dated graph inventory beside this plan. For every graph-bearing type,
record:

- stable Glam representation family, concrete Rust type, and source owner;
- outgoing managed-value edges, exact visitor policy, and trace-review
  checkpoint;
- whether it is immutable, replaceable, one-write, or freely mutable;
- current synchronization and lock order;
- whether it can live outside an evaluator call or worker quantum;
- whether it can cross threads;
- Rust size/alignment, requested total slot extent, and whether allocator
  discovery accepts that layout;
- managed-edge mutation gateway, if the edge is replaceable;
- `Drop` and ordinary/finalizing destruction policy;
- external-root classification and source-inventory evidence;
- confirmation that it fits the collector's documented managed-layout limit;
  and
- migration phase.

The inventory must explicitly cover every `core::Value` variant, every type
containing a core or public `Value`, every `Arc<dyn Fn...>` which can capture a
value, every `OpaqueValue` payload family owned by Glam, and every interaction-
net specialization carrying core data.

Until I10B.0 selects the bootstrap opaque representation, each arbitrary
type-erased payload family records either `no managed edge` or the exact
same-runtime public root wrapper it may retain. Discovering a bare `Gc<T>`,
unrooted recursive `core::Value`, foreign-runtime root, or equivalent internal
managed pointer in `Any` remains a boundary defect under either outcome. A
possible sealed managed arm is a separate statically registered representation
outside arbitrary `Any`, never a relaxation of this rule.

I0 can begin before collector class discovery exists. Record Rust type/layout,
stable representation family, and projected trace/drop policy. When a concrete
managed wrapper is selected, reconcile its requested extent, allocator-layout
acceptance, visitor review, mutation gateway, and external-root
classification. Canonical metadata addresses, dense class IDs, final stride,
slots per run, and other derived heap-local topology remain private collector
verification rather than integration-ledger fields.

Add tests which latch current semantics before changing representation:

- public value cloning, the current compatibility-equality baseline which I2
  deliberately removes, and cross-runtime rejection;
- fulfilled and unfulfilled lazy/promise behavior;
- lazy, promise, metadata, function, collection, and net cycles;
- value transfer between workers in one runtime;
- runtime drop with escaped public values, including their current supporting
  resource lifetime and access behavior;
- reflection store snapshots and task status after owner-session closure; and
- settlement and diagnostic/event retention.

This is an audit phase. Missing edges block later collection rather than being
classified optimistically.

## Phase I1 — Runtime Heap Ownership, Collection Disabled

### Phase I1A — No-Auto Collector Policy (complete)

Completed on 2026-08-25 in resolution of GCI-001. `glam_gc::Heap` now accepts
an immutable `CollectionPolicy`. The default remains `Automatic` for existing
collector clients and verification; production migration constructs its heap
with `CollectionPolicy::NoAuto`.

Under `NoAuto`, typed-run pressure and `Heap::request_collection` still latch
the ordinary request bit, but later outer mutator entries never elect a
collector. `Heap::collect_full` remains the sole deliberate acknowledgement
path, which permits isolated migration fixtures without allowing a partially
classified production graph to collect accidentally.

`Heap::statistics` exposes an O(1), observational snapshot of assigned-run
pressure, current high-water mark and headroom, the request latch, durable
finalization-batch run count, and queued/running finalizer obligations. It does
not scan allocations or trigger collection. Focused tests cross the automatic
pressure threshold and issue an explicit request under `NoAuto`, prove repeated
mutator entries do not collect, then prove an explicit full collection still
acknowledges the request. Existing panic/retry and running-finalizer fixtures
cover the public finalization statistics.

Verification: `no_auto_policy_retains_pressure_until_explicit_collection` and
`no_auto_policy_retains_explicit_request_across_mutator_entries`, plus the
existing panic/retry and running-finalizer statistics fixtures. Production has
no heap yet at I1A; the policy itself permits only explicit collection.

### Phase I1B — Value-Domain Topology and Owner Matrix (complete)

Completed on 2026-08-25 in resolution of GCI-003. The root crate now depends
on `glam-gc`, and every `CoreValueFactory` retains one internal
`Arc<RuntimeValueDomain>`. The domain owns the no-auto `Heap`, runtime-local ID
allocators, canonical and compiler-layer value cache, and the weak coordinator
binding. A scoped factory adds only its compilation-local extension lookup; it
shares the same domain.

The authoritative strong-owner matrix is:

| Capability | Domain ownership | Reason |
| --- | --- | --- |
| `EvaluationRuntime` / `RuntimeSharedResources` | strong, transitively through the core factory | the runtime and retained service hosts must continue constructing and storing values |
| public `Values` and crate-private `CoreValueFactory` | strong | they are explicit construction capabilities and remain usable after the facade is dropped |
| active `EvaluationDemandState` / `EvalContext` | strong, through the factory | an admitted evaluation context must retain its value cache and construction authority |
| `ReflectionStore` and `StoreSnapshot` | strong, through the factory | transactions and snapshots still construct path/query values after capture |
| active compiler contexts and scoped compiler factories | strong, through the factory; semantic state separately runtime-rooted | compilation-local construction and cache access must remain usable while prior/final definitions, origins, and imports survive waits only through roots |
| a sealed reflection profile's runtime host | conditionally strong through its retained `RuntimeSharedResources` | a retained service profile may continue using its host, but the domain itself never retains the profile |
| public `Value` / `RuntimeValueRoot` and future collector `Root` | non-owning | values may outlive the domain and become inaccessible rather than preserving the heap |
| managed nodes, closures, opaque payloads, and cache entries | non-owning | a strong backedge would form `domain -> heap/cache -> payload -> domain` |
| coordinator, executor, and scheduler records | non-owning | the domain stores only the reviewed weak coordinator route and cannot preserve runtime execution infrastructure |

The cache is contained *inside* the domain rather than constituting another
lease. A cached compiler bundle therefore must not capture a factory or domain
strongly. The immutable default profile remains a sibling root of runtime
state; service profiles may retain a host which retains shared resources, but
no inverse domain-to-profile edge exists.

Lifecycle tests separately latch retained public `Values`, shared resources,
an evaluation context, a service profile, the compiler cache, and a bare
public value. They prove authorized capabilities keep the domain useful after
facade drop, bare values do not keep it alive, and the coordinator, executor,
runtime state, and default profile remain acyclic. No production `core::Value`
is allocated in the heap yet.

Verification: `scoped_factories_share_one_no_auto_runtime_value_domain`,
`public_values_retain_only_the_runtime_value_domain`,
`bare_public_values_do_not_retain_the_runtime_value_domain`,
`runtime_shared_resources_do_not_retain_runtime_lifecycle_owners`,
`retained_reflection_profile_keeps_only_shared_resources_alive`, and
`evaluation_context_retains_runtime_cache_and_profile_without_a_cycle`.
Production uses `NoAuto`, and no production representation is collected.

### Phase I1C — Factory-Scoped Allocation

Completed on 2026-08-27. `CoreValueFactory::with_managed_values` is the sole
factory-level collector bridge. Its higher-ranked callback receives a
`CoreValueAllocationScope` which exposes narrow per-type allocator discovery,
root publication, and same-domain rooted access without exposing `Heap` or
`Mutator`.

`CoreValueAllocator<'_, T>` wraps the collector allocator borrowed from the
current scope. A caller can reuse it for a batch, so class discovery and its
`TypeId` lookup do not occur per object; its lifetime prevents the allocator
from surviving mutator exit. The collector's existing thread/heap cache owns
stable frontier reuse. No allocation handle owns the value domain, runtime
state, or scheduler, and `Root<T>` remains weak with respect to the heap.

There are not yet any production managed representation classes to
pre-discover. I4 and later representation phases may add reviewed common-class
discovery through this same bounded bridge rather than storing allocator
capabilities. Production collection remains `NoAuto`.

Verification: `factory_scoped_allocation_uses_current_mutator` and
`scoped_factory_does_not_retain_allocator_or_scheduler` fixtures, while
preserving `scoped_factories_share_one_no_auto_runtime_value_domain`. At this
checkpoint production collection remains `NoAuto`; an isolated factory fixture
may call `collect_full` only over representations introduced in this
checkpoint.

### Phase I1D — Layout Policy and Ownership-Ledger Reconciliation

Completed on 2026-08-27. GCI-005 had already established the stable ledger
schema: representation family, concrete Rust type/source owner, reviewed
visitor, requested extent and Rust layout, allocator acceptance, destruction
policy, mutation gateway, external-root classification, and authorizing
verification. Process-local metadata addresses, `TypeId`, dense class IDs,
frontiers, and derived run geometry remain collector-private.

`core::managed::managed_slot_extent<T>` is now Glam's single node-size policy
for managed representations. The initial floor is one machine pointer: this
avoids pathological sub-word typed runs without prematurely selecting the
eventual tagged-value alignment or a larger padding target. A representation
larger than that floor requests its natural Rust size. The collector then
applies the type's Rust alignment independently for its typed run; Glam does
not configure a mutable heap-wide alignment.

No production managed shell is selected here. I2 owns the public-root
prototype and the value-representation plan owns any later measured change to
alignment or padding, so inventing a semantic family merely to complete I1D
would pre-empt those decisions. A private one-byte leaf probe verifies that
the centralized pointer-sized request is accepted through the factory scope.
It is verification machinery, not a Gate G2 representation row. Every later
production family must use the centralized policy, pass its own allocator
fixture, and complete its stable ledger record when introduced.

Verification: `managed_family_requested_layout_is_accepted`, plus
`glam-gc`'s requested-total-slot-extent, undersized-request rejection, and
unsupported-layout publication suites. Production collection remains
`NoAuto`.

### Phase I1E — Lifecycle Reverification (complete)

Completed on 2026-08-28. This checkpoint reverified topology rather than
migrating a production value representation. `EvaluationRuntime` still owns
runtime state and the immutable default reflection profile as internal sibling
roots. A sealed profile may retain a host and the acyclic shared-resource
bundle, while both shared resources and the value domain retain only weak
routes back to scheduler infrastructure.

The composed `runtime_value_domain_has_no_scheduler_or_profile_backedge`
fixture seals the real runtime reflection profile, allocates a direct collector
root through a retained public factory, and then drops the assembler and
runtime. The authorized factory lease keeps only the value domain usable: the
runtime state, coordinator, executor, profile, and shared resources all retire.
Dropping that factory then retires the domain even while the collector root
still exists, proving that a root is not an ownership escape hatch.

Verification reran `public_values_retain_only_the_runtime_value_domain`,
`runtime_shared_resources_do_not_retain_runtime_lifecycle_owners`,
`retained_reflection_profile_keeps_only_shared_resources_alive`, and
`compiler_cache_does_not_form_a_value_domain_cycle` after I1C/I1D, plus the new
composed fixture. Production collection remains `NoAuto`; the managed `u64`
is private verification data, and no production `core::Value` is reclaimed.

The mandatory post-implementation audit is recorded in
[`GarbageCollectorIntegrationI1_2026-08-28.md`](../reviews/GarbageCollectorIntegrationI1_2026-08-28.md).
It found no blocking drift, so Phase I1 is complete and I2 may begin.

## Phase I2 — External Root and Public `Value` Prototype

### Phase I2A — Wrapper and Provenance Prototype

- Use GCI-004's completed `Heap::owns(&Root<T>)` provenance predicate, then
  prototype C4's direct managed root against scalars and one recursive test
  node. This direct root is a collector/provenance control, not a competing
  public API design.
- Select a Glam-owned opaque wrapper as the permanent public boundary. Its
  private prototype representation distinguishes an allocation-free inline arm
  from a managed-root arm. The inline arm contains only an internal value which
  requires no managed trace plus non-owning, non-forgeable value-domain
  provenance. The managed arm uses the collector's existing root cell; the
  wrapper adds neither another registry entry nor another root-cell
  representation.
- Preserve the opportunity for the value-representation plan's eventual tagged
  immediate-or-managed-pointer word. Constructing an inline public scalar must
  not allocate a managed payload or register a fresh root. A public managed
  value clone preserves its existing root cell while the value domain lives,
  while neither arm preserves the heap after the domain's authorized owners
  are dropped.
- Derive authoritative managed provenance from root heap identity rather than
  an independently forgeable runtime ID. Inline values initially use a private
  weak value-domain witness; managed values use root heap identity. Every safe
  access performs the release-build same-domain or same-heap check before the
  private typed-pointer gateway.
- Do not select the final wrapper size, tag bits, immediate range, managed node
  taxonomy, alignment, or erased-root decoding gateway here. Those remain
  private representation decisions for V0/V1/V4 of the value-representation
  plan, and may change without changing the public wrapper contract.

Verification: `prototype_root_moves_between_threads`,
`prototype_root_rejects_another_heap`, `prototype_root_becomes_inert_after_domain_drop`,
`prototype_inline_values_allocate_no_managed_slots`,
`prototype_inline_value_rejects_another_domain`, and
`prototype_recursive_root_traces_child`,
the collector's `heap_ownership_predicate_accepts_only_the_recorded_live_heap`
and `heap_ownership_predicate_tolerates_concurrent_root_clone_and_drop`, and
the existing mismatched-`Root::get` invariant test. Only the isolated prototype
heap may collect; production remains `NoAuto` and retains its compatibility
`RuntimeValueRoot`.

Completed 2026-08-28. The isolated test-only prototype selected a Glam-owned
opaque wrapper with two private arms: an allocation-free scalar plus weak
value-domain witness, or the collector's existing registered root cell. Its
managed recursive fixture traces one child exactly, clones share one root
registration across threads, both arms reject another runtime value domain,
and both become inaccessible after their owning domain is gone. Constructing
1,024 inline handles leaves heap statistics unchanged and assigns no managed
run. The prototype deliberately uses its own private node and default
collector layout: it tests representation and provenance only, not the later
production family slot policy. Production `api::Value`, `RuntimeValueRoot`,
and collection policy remain unchanged.

### Phase I2B.1 — Opaque Prototype Surface

- Apply GCI-002's resolved contract to I2A's isolated prototype wrapper. The
  prototype facade exposes only transport behavior: clone, drop, and `Send`
  plus `Sync` when its representation permits it. It implements no
  `PartialEq`, `Eq`, `PartialOrd`, `Ord`, or `Hash`, and exposes neither
  root-cell nor allocation identity as a replacement relation.
- If `Debug` remains available on the prototype handle, make it content-free:
  it reveals no kind, provenance, runtime liveness, managed identity, or other
  value state.
- Record that clients needing dictionary/set keys must first obtain or compute
  an ordinary host key through authorized observation. A value handle is
  deliberately unsuitable as a key.
- Do not change the production `api::Value` compatibility surface in this
  prototype checkpoint. I4F.2 owns removal of its direct traits and observers.

Verification: compile-time prototype-facade fixtures establish the absence of
equality, ordering, and hash contracts, and
`prototype_value_debug_is_opaque` checks the optional content-free debug form.
Only isolated prototype fixtures may collect; production remains `NoAuto` and
retains its compatibility `RuntimeValueRoot`.

Completed 2026-08-28. The prototype exposes `Clone`, `Send`, and `Sync` as its
transport contract and implements a constant, content-free `Debug` form.
Compile-time ambiguity fixtures reject any future `PartialEq`, `Eq`,
`PartialOrd`, `Ord`, or `Hash` implementation; the fixture was latched by
temporarily adding `PartialEq` and observing the intended trait-selection
failure. Consequently, a client must obtain or compute an ordinary host key
through later runtime-authorized observation before using value-derived data
in a Rust dictionary or set. No production public trait or observer changed.

### Phase I2B.2 — Runtime-Authorized Observation Prototype

- Require live matching runtime authority for structural equality, kind
  inspection, extraction, and value rendering. Prototype both natural Rust
  call directions where useful: a runtime service receiving a value and a
  value or evaluated-value method receiving a scoped service/mutator. The
  authority and failure behavior must be identical; method placement is an
  ergonomic choice. Runtime provenance checks remain private boundary
  enforcement.
- Preserve the prototype evaluated-value facade as the sole WHNF witness
  rather than a second root model, but do not treat that witness as independent
  authority to inspect its value. Authorized extractors return owned host
  values which may outlive the domain; no managed borrow escapes its matching
  mutator/access scope and no hidden domain lease is manufactured to preserve
  an old signature.

Verification: `prototype_runtime_compares_live_structural_values` covers
equal, unequal, cloned, and independently constructed values;
`prototype_runtime_observation_rejects_foreign_or_inaccessible_value` covers
the fallible boundary; and `prototype_owned_extraction_outlives_domain`
proves the owned-result rule. Only isolated prototype fixtures may collect;
production remains `NoAuto` and retains its compatibility public facade.

Completed 2026-08-28. A borrowed `PrototypeRuntime` now supplies the only
observation authority. It validates inline witnesses and managed roots before
kind inspection, recursive structural comparison, rendering, evaluation, or
owned extraction. `PrototypeEvaluatedValue` is a clone of the same opaque
handle and records only that the prototype's already-WHNF outer shell was
accessible when evaluated; it neither retains the domain nor authorizes later
inspection. Its observation methods require the same borrowed runtime service
and delegate to the same fallible checks. Recursive managed comparison and
extraction remain inside one mutator region and follow exact traced child
edges; only ordinary owned Rust data escapes. Tests cover aliases,
independently allocated equal graphs, unequal nested graphs, foreign and dead
domains, and extracted nested data surviving final domain teardown. The
test-only traced-edge gateway models the eventual internal scoped-access rule;
I2C still owns the production call-site inventory and nested-access contract.
Production values and collection policy remain unchanged.

### Phase I2C — Scoped-Access and Production-Switch Inventory

- Prototype scoped core access under the I3 authority shape and inventory every
  production `as_core`/`into_core` call site which I4 must migrate.
- Inventory constructor, composite-validation, storage, evaluator, reflection,
  diagnostic, and binary-extraction paths which assume the compatibility
  wrapper.
- Fix the selected public wrapper/provenance contract, but do not place
  production `core::Value` in a collector root until I4 supplies its exact
  trace, I4F.1 converts durable owner fields, and I4F.2 performs the switch.

Verification: the complete existing public `Value` suite stays unchanged;
`prototype_value_access_nests_in_one_mutator` exercises recursive entry; and
`public_value_compatibility_access_inventory_is_complete` assigns every access
to a later migration checkpoint. Production collection remains `NoAuto`.

Completed 2026-08-28. Nested prototype observation enters the same runtime
while an outer managed borrow remains live, completes recursive extraction,
and returns to the still-valid outer scope. The production inventory is
recorded in
[`GarbageCollectorPublicValueAccessInventory_2026-08-28.md`](GarbageCollectorPublicValueAccessInventory_2026-08-28.md)
and enforced by a source-scanning regression. Its baseline covers 233
compatibility occurrences in 23 non-test source modules across constructors,
composite validation, storage, evaluator/poll paths, reflection, diagnostics,
compiler/macro paths, and net construction. Each module is assigned to its I3
scoped-access and I4F root/facade migration owner. The regression was latched
by changing one expected count and observing the exact module mismatch.
Prototype and inventory sources are excluded deliberately; named test modules
are outside the production inventory, while colocated fixtures remain covered
with their owning module. No production representation or collection policy
changed. The mandatory post-I2 audit is recorded in
[`GarbageCollectorIntegrationI2_2026-08-28.md`](../reviews/GarbageCollectorIntegrationI2_2026-08-28.md).
It found no blocking drift after resolving the inventory and verification-index
findings, so Phase I2 is complete and I3 may begin.

## Phase I3 — Mutator Regions Across Evaluation and Construction

I3 distinguishes three related but non-interchangeable meanings of purity:

1. **Semantic purity/reproducibility:** the result is determined by the
   declared inputs. A securely validated local import or content-addressed
   remote import qualifies even though obtaining its bytes uses the host.
2. **Evaluator purity:** the evaluator itself performs no Glam-observable
   effect. It may reduce a value or suspend on an external producer, including
   promises, reflection gates, and validated import results, without
   interpreting that producer inside the pure step. This does not make the
   producer semantically pure or operationally callback-free.
3. **Operational callback-freedom:** the current Rust step invokes no host or
   user callback, performs no blocking semantic wait, and is therefore eligible
   to run while a scoped mutator is active.

Only the third property authorizes one continuous mutator region. A machine
poll is an orchestration and scheduling quantum which may open several bounded
callback-free evaluator regions. It does not itself imply that a mutator is
held for the complete poll.

### Phase I3A.1 — Authority Types and Non-Escape Contract

- Prototype one lifetime-bound managed-access carrier and its
  evaluation-specific view. The foundational carrier borrows the matching
  `glam_gc::Mutator`; the evaluation view pairs that authority with a reference
  to durable task/evaluation context without making the durable context itself
  lifetime-bound.
- Separately prototype an ephemeral scheduler-created poll context. The poll
  context holds the right domain/admission route but no continuously active
  mutator. A closure/HRTB-style method opens a `RuntimeValueAccess` and thin
  evaluation scope for one callback-free semantic substep, requires any
  escaping value to be converted to a root or exact traced edge, and releases
  the mutator before returning to poll orchestration. I3A.4 makes that outcome
  conversion exhaustive for production polls.
- Keep both constructors private. The poll context may create scoped access;
  durable contexts, individual machines, TLS, and runtime IDs may not. These
  are two layers of one authority model, not independently creatable
  capabilities.
- Treat subsystem capabilities which can inspect managed values as views
  derived from `RuntimeValueAccess`, not as alternative ways to enter the
  heap. In particular, I3D.3 introduces an authority-gated core-net view; the
  generic interaction-net implementation remains independent of `glam_gc`.
- Derive heap provenance from the admitted mutator/value domain and validate
  context agreement once when constructing the scoped view.
- Preserve durable machine state as owned, `Send`, and parkable. A checked TLS
  convenience may find an already active same-heap region, but it cannot become
  the safety basis for dereference.

Verification: the collector's compile-fail fixtures reject escaping its
mutator, allocator, and managed borrow. The private Glam carrier and evaluator
view retain no public construction surface merely to support an external
doctest; their higher-ranked entry signature supplies the non-escape proof,
while compile-time negative-trait fixtures prove neither can cross threads and
the durable context remains `Send`. `different_heap_authority_is_rejected`
covers provenance. A focused fixture proves a poll context can open two
separate evaluator scopes with a callback between them and that no mutator
remains active during that callback. No production call site changes yet,
production remains `NoAuto`, and no production representation is collected.

Completed 2026-08-28. `RuntimeValueAccess` now layers exact value-domain
provenance over I1's lifetime-bound `CoreValueAllocationScope`, and delegates
only scoped allocation, rooting, and root borrowing. The private
`EvaluationValueAccess` pairs that carrier with a borrowed durable
`EvalContext`, validating domain identity once at construction. Its provenance
check compares the domain authority already in hand. The regression combines
two normally allocated, distinct runtimes and rejects their combination;
`EvaluationRuntimeId` remains globally unique and identifies the value domain.

At this checkpoint, the private `EvaluationPollContext` contained only a
borrowed durable context. I3A.3 replaced that prototype representation with a
temporary strong route cloned from the detached claim so the scheduler can
mutably poll the machine without borrowing another field of the same claimed
enum. The machine still receives only a shared, non-cloneable view of the poll
context, and the context still contains no mutator or managed borrow.
Its higher-ranked `with_value_access` method admits the heap for one operation
and releases it before returning. The two-scope regression uses synchronous
collection as the admission probe: collection reports `ActiveMutator` inside
both evaluator regions, succeeds in the callback between them, and the second
region can publish a root which a third region reads. Trait-selection fixtures
prove both scoped carriers are neither `Send` nor `Sync`, while `EvalContext`
remains `Send`. Existing collector compile-fail doctests continue to cover the
underlying mutator, allocator, and managed-borrow escapes. Production machine
poll signatures remain unchanged until I3A.3, and collection remains `NoAuto`.

### Phase I3A.2 — Claimed-Work Domain Routing

- Make reflection, deferred, client-demand, and spark claims carry or obtain a
  temporary strong demand-session reference while claimed.
- Use that session's value domain as the sole heap-admission source. Validate
  runtime agreement at claim/poll boundaries without storing a new strong
  domain owner in the coordinator or durable work record.
- Preserve claim, release, cancellation, task-handle, and owner-session
  shutdown behavior before adding mutator entry.

Verification: forced owner-close and cross-session schedules prove a claimed
poll keeps its existing session resources alive, an unclaimed record does not,
and a mismatched demand session is rejected before machine execution. Existing
claim/release and shutdown suites remain semantic regressions. Production
remains `NoAuto`.

Completed 2026-08-28. Every reflection, deferred, client-demand, and spark
claim now carries `ClaimedDemandSession`, obtained by upgrading the
coordinator's weak demand registry before the work payload is detached. The
upgrade validates the indexed session identity and coordinator runtime; poll
adapters assert the same runtime again outside coordinator locks before
machine or operation execution. A malformed registration is rejected without
panicking under and poisoning the coordinator mutex.

Parked `SparkDemand` and `ClientDemandWork` now retain weak session routes;
their claim is the only new strong demand lease. Reflection and deferred work
already carry arbitrary opaque machines, and those machines may independently
contain an authorized strong `EvalContext` lease under I1B. This checkpoint
does not rewrite those internals before I3A.3 changes the poll contract:
"an unclaimed record does not" therefore means that the coordinator envelope
and domain-routing mechanism add no strong lease beyond semantic values or
contexts deliberately stored by the machine itself.

Forced regressions verify exact routing when two same-runtime sessions compete,
reject a registered same-session route backed by another runtime before its
machine is polled, prove an unclaimed spark cannot retain its demand domain,
and prove a detached spark claim keeps that domain alive across owner closure
until release. Existing coordinator and evaluator lifecycle suites remain
green. Production still uses `NoAuto` and opens no mutator in this checkpoint.

### Phase I3A.3 — Scheduler-Owned Poll Orchestration

Implementation checkpoints:

1. **I3A.3a — Claim-derived poll context and trait migration.** Derive the
   ephemeral poll context from the checked `ClaimedDemandSession`, pass it
   through `EvaluationTaskMachine::poll`, and migrate production and test
   implementations mechanically. The context may temporarily retain that
   already-authorized demand route to avoid a claim-field borrow conflict, but
   machines receive only a shared view and cannot extract or store the route.
2. **I3A.3b — Common poll-capability routing.** Route task, client-demand, and
   spark polling through scheduler adapters which construct the same
   claim-derived context. Only an explicitly bounded, callback-free substep
   may open value access. Existing opaque `eval_value`, lazy-source, effect,
   and spark calls can pump, wait, or invoke callbacks, so they receive or
   travel beside the capability but do not acquire one poll-wide mutator;
   I3B-I3D split and migrate those substeps.
3. **I3A.3c — Boundary verification and completion.** Add deterministic
   probes for recursive same-heap entry, two evaluator scopes around a
   mutator-free callback, release/destruction outside admission, and worker
   sleep without mutator authority. Then run the complete scheduler and
   repository verification suites.

- Change `EvaluationTaskMachine::poll` to receive the ephemeral poll context,
  then mechanically migrate production machines and test fixtures. A proven
  callback-free evaluation substep may open one bounded evaluator scope;
  effect machines will alternate several evaluator scopes with interpreter
  work after I3D. Existing unsplit evaluator calls must ignore the capability
  until I3B separates their polling and waiting paths. Test-only machines
  which manipulate no values may also ignore the context.
- Construct the poll context only after the coordinator has detached a claim
  from its locks. Each managed evaluator scope ends before host callbacks,
  claim release, terminal publication, cancellation/drop hooks, coordinator
  waits, or parked-machine publication. The poll context itself may remain on
  the stack between scopes because it contains no active mutator or managed
  borrow.
- Route cooperative pumping, executor workers, client demand, and sparks
  through the same admission helper. A nested same-heap poll reuses recursive
  collector admission only while a scoped evaluator region is actually
  active; nested orchestration does not manufacture another authority.
- Keep the scheduler or synchronous driver responsible for admission;
  individual machines do not silently enter their heap.

Verification: deterministic barriers observe an active mutator during each
evaluator substep and no active mutator during interpreter callbacks, release,
terminal publication, sleep, or machine destruction. A poll containing two
evaluator substeps around one callback proves entry is scoped rather than
poll-wide. Add
`evaluation_scope_reuses_recursive_same_heap_entry`,
`blocked_machine_parks_without_mutator`, and
`worker_releases_mutator_before_sleep`; retain existing task-order and shutdown
suites. Production remains `NoAuto`.

Completed 2026-08-29. Every detached reflection/deferred claim now constructs
one `EvaluationPollContext` before invoking the type-erased machine, and
`EvaluationTaskMachine::poll` carries that capability through all production
and test adapters. Client demands and sparks use the same coordinator-owned
poll helpers in cooperative and executor paths; spark evaluation no longer has
a second worker-local implementation. The poll context retains a temporary
strong `Arc<EvaluationDemandState>` cloned from the validated claim. It owns no
mutator, is not cloneable by machines, exposes no demand route, and is dropped
with the poll adapter.

The first implementation briefly wrapped whole lazy-source, client-demand,
and spark evaluator calls. A full-suite regression exposed why that was
incorrect: those calls may recursively pump work, wait, invoke deferred
thunks, or activate reflection, and recursive mutator-entry frames exhausted a
pattern fixture's ordinary test stack. The wrappers were removed rather than
raising the stack. Production opens no managed scope around an opaque call;
I3B.1/I3B.2 and I3D create the smaller callback-free steps which may safely
consume the routed capability.

`evaluation_scope_reuses_recursive_same_heap_entry` proves nested bounded
access shares the active same-heap admission until the outer scope exits. The
existing two-scope callback probe remains green, while
`blocked_machine_parks_without_mutator`,
`terminal_machine_destruction_occurs_without_mutator_authority`, and
`worker_releases_mutator_before_sleep` prove release, parking, destruction,
and idle worker waits occur after scoped access ends. The compatibility-access
inventory records centralizing spark evaluation from `executor.rs` into
`pump.rs`. Production remains `NoAuto`.

### Phase I3A.4 — Evaluator and Poll Outcome Ownership Boundaries

Implementation checkpoints:

1. **I3A.4a — Outcome type and compatibility-root seam.** Change
   `EvaluationMachinePoll::Complete` from bare `core::Value` to
   `RuntimeValueRoot`. Add one claim-derived construction seam which uses the
   checked poll domain while the compatibility root still contains
   `{runtime ID, Value}`. Remove late result reconstruction from reflection and
   deferred release.
2. **I3A.4b — Producer migration and root preservation.** Migrate every
   production and test task machine. Preserve a `PublicValue`'s existing
   `RuntimeValueRoot` when an effect task completes instead of extracting its
   core value and recreating a root; use the claim-derived seam only for
   currently bare evaluator results.
3. **I3A.4c — Exhaustive boundary inventory and provenance verification.** Add
   a compile-exhaustive inventory for every poll, block, exit, dependency, and
   failure payload, recording the exact I4-I6 checkpoint for each deferred
   interior migration. Verify same-runtime completion publication and that
   collection requests at substep/poll boundaries retain no active mutator.

There is no managed production `core::Value` payload at this checkpoint, so
I3A.4 cannot yet prove survival of a managed semantic completion by forcing
collection. The root-shaped outcome is nevertheless required now: I4F.2
changes `RuntimeValueRoot` to the managed representation atomically, and I3B
must move root construction inside the evaluator scope before introducing any
managed value which could cross this boundary.

- Inventory every `EvaluationMachinePoll`, task block, exit, and failure field
  which crosses the scoped region. Convert completed values to
  `RuntimeValueRoot` or the selected equivalent before leaving the evaluator
  scope which produced them.
- For payload families whose managed representation arrives only in I5-I10,
  record the exact later checkpoint which updates their *interior*
  representation in the same change that introduces its first managed edge.
  The durable outer owner is still converted by I4F.1; only a payload proven
  unable to contain a managed edge may remain in its old interior form until a
  later phase.
- Ensure every value crossing from an evaluator scope into poll orchestration
  is rooted before the mutator is released. Final poll-outcome conversion may
  then assemble only owned/rooted data, perform no callback, and give
  coordinator publication no scoped borrow.
- Treat disabled collection as irrelevant to pointer-lifetime correctness. A
  parked machine, coordinator record, callback payload, or type-erased holder
  may contain only a registered root, an exact managed edge owned by a traced
  allocation, or data proven local to the current bounded access scope.

Verification: force another thread to request collection both between two
substeps of one poll and at poll return, proving every already-migrated value
remains live through callback, release, and publication.
`evaluation_machine_poll_boundary_inventory_is_complete` covers every variant
and records each deliberate later migration. Production remains `NoAuto`.

Completed 2026-08-29. `EvaluationMachinePoll::Complete` now carries
`RuntimeValueRoot`; coordinator release publishes that root directly rather
than reconstructing one from a bare value. The claim-derived poll context owns
the single temporary constructor for bare evaluator results. Effect machines
preserve an existing public root through successful value and unit completion,
so the boundary no longer discards and recreates runtime provenance.

`evaluation_machine_poll_boundary_inventory_is_complete` destructures every
poll, block, exit, dependency, and failure-bearing variant and records the
remaining interior migrations: durable completion/exit/wait roots switch in
I4F.2, promise cells in I5B/I5C, and structured failure payloads in I6C.
`evaluation_failure_boundary_inventory_is_complete` separately latches the
private failure-kind and context fields. Cross-thread forced-collection probes
run between two scoped substeps and after terminal publication, confirming
that neither boundary retains a mutator. Because production `core::Value`
still contains no managed semantic pointer, this checkpoint deliberately does
not claim a managed completion-survival test; I4F.2 supplies that payload and
must extend the existing provenance fixture in the same change. Production
remains `NoAuto`.

### Phase I3B.1 — Scoped Construction and Core Evaluator Migration

Implementation checkpoints:

1. **I3B.1a — Evaluator-step authority and compatibility inventory.** Add one
   thread-bound, lifetime-borrowed evaluator-step context containing the
   durable `EvalContext` plus scheduler poll authority, but no active mutator.
   Its callback-free operations may open `EvaluationValueAccess`; waits and
   callbacks see only the durable context after such access has ended. Record
   every temporary direct/legacy evaluator entry owned by I3D or I3E rather
   than letting it manufacture scheduler authority implicitly.
2. **I3B.1b — Value/application/sequence spine.** Migrate the internal
   evaluator spine rooted at immediate `eval_value` dispatch, application,
   ordinary function sequencing, key/list traversal, and callback-free value
   construction to the evaluator-step context. Keep wait/pump operations
   outside active `EvaluationValueAccess`, and leave explicit net/reflection
   seams for I3D rather than threading access through them.
3. **I3B.1c — Ordinary builtin clusters.** Migrate callback-free numeric,
   comparison, dictionary, list, object, pattern, assertion, and pure
   annotation helpers in small cluster checkpoints. Effect interpretation,
   reflection annotations, net construction/driving, and externally supplied
   deferred computations remain explicit I3D/I3E boundaries.
   The concrete checkpoints are:
   - **I3B.1c.1 — Dispatch, numeric, and assertion.** Add one scoped builtin
     dispatcher behind the existing direct-compatibility wrapper, route the
     evaluator spine through it, and migrate numeric arithmetic plus unit
     assertion. Non-migrated families receive only the durable context at an
     explicit dispatcher branch.
   - **I3B.1c.2 — Comparison and pattern inspection.** Migrate recursive
     equality/ordering, list and dictionary traversal, and compiler-private
     pattern probes without retaining access across suspension.
   - **I3B.1c.3 — Dictionary and list transformation.** Migrate singleton,
     union/update/merge and list slicing, mapping, splitting, indexing, and
     text conversion, reusing the scoped sequence helpers from I3B.1b.
   - **I3B.1c.4 — Object and pure effect construction.** Migrate object
     specification/definition composition plus the callback-free conditional
     and list-effect constructors. Keep actual effect interpretation and net
     construction on their I3D boundaries.
   - **I3B.1c.5 — Annotation partition and closure.** Migrate only assertion,
     context, validity, and other demonstrably pure annotation branches;
     retain explicit durable seams for reflection, metadata-reflection,
     strategy, provenance, and externally supplied computation, then latch
     the remaining ownership assignments.
4. **I3B.1d — Public construction and owned extraction.** Give public
   `Values` composition one runtime-service-owned scoped construction path,
   batch nested helpers under one admission where practical, and make
   `EvaluatedValue` extraction borrow managed data only inside a matching
   scope while returning owned bytes/scalars/collections.
   The concrete checkpoints are:
   - **I3B.1d.1 — Scoped public construction.** Add one private
     runtime-qualified construction carrier beneath `Values`, route immediate
     and composite constructors through it, and make nested helpers call the
     already-admitted carrier instead of reopening the runtime. User callbacks
     and durable resolver/net state retain only `Values` or roots, never the
     scoped carrier.
   - **I3B.1d.2 — Matching-runtime owned extraction.** Keep
     `EvaluatedValue` as only the WHNF witness. Require a borrowed `Values`
     service for scalar, binary, and strict-array extraction; validate the
     exact runtime before inspection, borrow only inside one scoped access,
     and return owned `Bytes`, scalars, strings, or public roots. I3B.1d.4
     supersedes the public parameter shape without weakening this matching-
     domain or bounded-access requirement.
   - **I3B.1d.3 — Closure and verification.** Latch the single construction
     entry, nested helper reuse, foreign-runtime failures, and owned-result
     lifetime. Audit public construction/extraction compatibility escapes and
     update current architecture and ownership records without claiming the
     I4F.2 managed-root representation early.
   - **I3B.1d.4 — Weak evaluated observer adjustment.** Correct the public
     extraction ergonomics before I3B.1 closure. A successfully evaluated
     value carries a weak, exact value-domain observer issued by its evaluator;
     it does not retain the heap and does not hold a mutator between calls.
     Scalar, binary, canonical-number-text, and strict-array methods upgrade
     that observer, enter one bounded access region, and return owned results
     directly from `evaluated.as_*()`. If the value domain has disappeared,
     observation fails explicitly. `into_value` discards the observer, and
     bare `Value` remains transport-only. Do not introduce a public lifetime
     parameter, a strong `Values` back-reference, or a second public observer
     type unless implementation evidence requires one.
5. **I3B.1e — Inventory closure and verification.** Latch every evaluator
   function which may allocate or inspect managed data, every deliberate
   compatibility entry, and every I3D/I3E boundary. Run the recursive
   construction, provenance-error, and owned-extraction fixtures before
   declaring I3B.1 complete.

The evaluator-step context is intentionally distinct from
`EvaluationValueAccess`. The former may survive across an evaluator
orchestration step because it contains no mutator; the latter exists only
inside a callback-free closure. This preserves current patient evaluation
while allowing I3B.2 to separate its wait driver without letting a recursive
`eval_value` call hold collector admission across pumping or callbacks.

I3B.1a completed 2026-08-29. A claimed `EvaluatorStepContext` borrows one
checked `EvaluationPollContext` and durable `EvalContext`, contains no mutator,
and is statically neither `Send` nor `Sync`. Its managed operation delegates a
callback-free closure to `EvaluationValueAccess`; the existing two-region
probe now exercises this carrier and still permits cross-thread collection
between regions. I3B.1b adds one temporary direct-compatibility admission for
I3B.1c's internal builtin seams and the source-inventoried I3D/I3E callers. It
likewise activates no mutator until a bounded callback-free closure requests
access, and one source latch prevents that exceptional constructor from
spreading.

`direct_evaluator_compatibility_entries_are_complete` records all 38 direct
production calls to recursive evaluation, application, strategy demand, path
evaluation, and list materialization outside `src/eval`. Reflection entries
belong to I3D; assembly, compiler, `.g`, macro, and diagnostic entries belong
to I3E. No compatibility entry is treated as permission to inspect a managed
value without authority; production remains `NoAuto`, and the inventory must
shrink as those owners migrate.

I3B.1b completed 2026-08-29. Immediate value dispatch, recursive application,
function saturation, semantic tagged-dictionary selection, key conversion,
and list/key/binary traversal now share internal functions parameterized by
`EvaluatorStepContext`. Client demand, lazy production/following, and promise
following derive that context from their checked poll claim and publish their
terminal value through it. Existing public and compiler/reflection entries
enter the same spine only through `with_direct_evaluator`; the inventory both
counts those callers and asserts that there is exactly one compatibility gate.
Deferred host callbacks, reflection gates, net execution, fixpoint-object
construction, and builtin dispatch receive only the durable `EvalContext` at
explicit seams; I3B.1c and I3D own their narrower migrations. No active
`EvaluationValueAccess` crosses a wait, pump, callback, or machine poll. A
paired direct/claimed application test latches semantic equivalence, while
the production direct-entry count falls from 40 to 39. Production remains
`NoAuto`; managed semantic result survival remains an I4F.2 obligation.

I3B.1c.1 completed 2026-08-29. `apply_builtin_in` is now the scoped dispatcher
used by the value/application spine, while the legacy `apply_builtin` surface
is one direct-compatibility wrapper for test fixtures and not-yet-migrated
internal callers. Numeric arithmetic and unit assertion carry the existing
`EvaluatorStepContext` through all recursive demands. Every other builtin
family is deliberately visible as a dispatcher downgrade to the durable
`EvalContext`; no branch receives `EvaluationValueAccess`, and the later
I3B.1c/I3D checkpoints own those seams. Claimed arithmetic and assertion
fixtures preserve their existing results. Production remains `NoAuto`.

I3B.1c.2 completed 2026-08-29. Recursive equality and ordering, tuple-tag
inspection, list traversal, and all compiler-private pattern probes now carry
`EvaluatorStepContext`. Shared list/key helpers gained scoped variants rather
than reopening the direct-compatibility gate. Patient forcing remains outside
active `EvaluationValueAccess`: the carrier may survive a lazy/promise wait,
but it contains no mutator. Direct-versus-claimed comparison and pattern
fixtures match, the focused pattern/equality suites pass, and the full suite
passes. No semantic choice was required; production remains `NoAuto`.

I3B.1c.3 completed 2026-08-29. Dictionary singleton/union/update/merge and all
ordinary list transformations now retain `EvaluatorStepContext` through
recursive value, key, application, and lazy-list traversal. Sequence and index
helpers expose scoped variants, avoiding nested direct admission. The only
temporary dictionary-union wrapper serves the not-yet-migrated object family
and is removed by I3B.1c.4. No access region spans patient forcing. Focused
dictionary/list suites and the full suite pass without semantic changes;
production remains `NoAuto`.

I3B.1c.4 completed 2026-08-29. Object specification, C3 linearization,
definition composition, instance/fixpoint construction, conditional result
selection, and callback-free list-effect constructors now retain the
evaluator-step carrier. Object fixpoint production no longer downgrades and
re-enters compatibility admission. Actual list-effect handling remains inside
the deferred closures which already receive only durable `EvalContext`; this
is an intentional interpreter boundary, not incomplete constructor migration.
The obsolete dictionary-union compatibility wrapper was removed. Focused
object/list-effect tests and the full suite pass without semantic changes;
production remains `NoAuto`.

I3B.1c.5 completed 2026-08-29. Annotation recognition, assertion payloads,
context/error handling, metadata initialization and pure transformation, and
array/deque/binary conversion now retain the evaluator-step carrier through
their recursive demands. Metadata-reflection validates its carrier inputs on
that scoped route, then hands only the deferred reflection task to a named
durable seam. Reflection gates likewise use a named durable handoff, while
`seq` and `spark` retain their existing durable strategy boundary. A source
latch fixes the complete dispatch-time downgrade set at effects, strategies,
nets, and provenance and separately records the two annotation reflection
handoffs. The obsolete direct unit-assertion compatibility wrapper was removed
and the test-only binary-extraction wrapper was classified explicitly. Claimed
annotation, focused metadata/annotation, source-inventory, Clippy, and full
suite checks pass without semantic changes. This closes the ordinary builtin
cluster; production remains `NoAuto`.

I3B.1d.1 completed 2026-08-29. Every immediate and composite `Values`
constructor now enters through one private lifetime-bound `ScopedValues`
carrier. Nested path, annotation, and application helpers reuse that carrier
rather than recursively invoking public constructors; public net-building
callbacks retain only durable `Values` and open one bounded region for each
data insertion. Composite construction clones the compatibility core payload
inside the matching scope instead of consuming public roots through
`into_core`. The compatibility inventory records four fewer borrowed escapes
and eighteen fewer owned escapes in `api/value.rs`. A focused fixture proves
recursive construction has one active outer mutator and releases it on return;
the complete API unit suite and strict Clippy pass. No public semantics or
collection policy changed, and production remains `NoAuto`.

I3B.1d.2 completed 2026-08-29. `EvaluatedValue` remains only an outer-WHNF
witness: binary, scalar, canonical-number-text, and strict-array extraction
now require a borrowed matching `Values` service. Each operation validates
runtime provenance and inspects the compatibility payload only inside one
bounded runtime-value access region. Binary and text views return owned
`Bytes`/`String`; strict arrays return cloned public roots. Effect-token
resolution follows the same fallible authority boundary, so a token from a
different runtime is an error rather than an apparent domain miss. Output
adapters, configured effects, logging, rendering, and public fixtures retain
or obtain their runtime service explicitly.

I3B.1d.3 completed 2026-08-29. The compatibility inventory was deliberately
allowed to fail, then relatched at 198 occurrences after public construction
and extraction removed eleven more facade/evaluator escapes. Dedicated
fixtures cover one nested construction admission, foreign composite rejection,
foreign extraction authority, effect-token provenance, and owned bytes/text
surviving teardown of every source handle. Current architecture and ownership
records established the matching-domain observation boundary, whose public
parameter shape is refined by I3B.1d.4.
Focused tests, the full suite, formatting, and strict Clippy pass; production
still uses compatibility `RuntimeValueRoot` storage and remains `NoAuto` until
the I4F.2 representation switch.

I3B.1d.4 completed 2026-08-29. `EvaluatedValue` now pairs its WHNF value with
an exact weak observer for the evaluator's value domain. Public extractors no
longer require callers to retain and repeatedly pass `Values`; each call
briefly upgrades the observer, enters one bounded access region, and returns
only owned Rust data or public value roots. The observer retains neither the
runtime heap nor a mutator, and observation reports an error after the value
domain disappears. `into_value` deliberately discards this observation route,
leaving bare `Value` transport-only. Effect-token resolution retains its exact
token-domain check in addition to the weak value-domain check. Focused tests
cover successful extraction, post-call collection admission, dead-domain
failure, owned-result survival, and token provenance without changing the
198-entry compatibility-access inventory. Production remains `NoAuto`.

I3B.1e completed 2026-08-29. The closure audit now source-latches all 112
`EvaluatorStepContext` function surfaces under `src/eval` and all 50 remaining
durable `EvalContext` surfaces, assigning the latter explicitly to I3B.2,
I3C, I3D, I3E, or I10. This complements rather than replaces the existing
38-call external direct-entry inventory, the single compatibility-gate latch,
the exhaustive builtin downgrade check, and the 198-entry public-value
compatibility inventory. The recursive-construction, composite-provenance,
claimed/direct application, owned-extraction, weak-observer, and inventory
fixtures pass. No compatibility entry was silently promoted to permanent API:
poll/wait driving, strategies, effects/reflection, nets, deterministic external
demands, diagnostics, and opaque origin inspection retain their named later
owners. Formatting, strict Clippy, and the full suite pass; production remains
`NoAuto`. This closes I3B.1 without claiming I3B.2's wait separation or the
I3D/I3E callback and subsystem migrations.

- Introduce the scoped evaluator view selected in I3A.1 and migrate the
  strongly connected evaluator call graph rooted at `eval_value`. Persistent
  context remains only in machines and other parked state.
- Partition the mechanical migration by call-graph seams:
  evaluator/application/sequence first, followed by ordinary builtins.
  Reflection and interaction-net entry points remain assigned to their
  dedicated I3 checkpoints.
- Enclose public `Values` construction/composition, no-wait evaluator steps,
  WHNF extraction, and direct isolated steps in bounded access regions. Nested
  helpers reuse the current same-runtime region and heap-qualified TLS cursor
  cache rather than entering once per pointer operation.
- Complete payload initialization and allocation-bit publication before a
  managed pointer becomes observable, never at outer-region exit. Authorized
  extraction returns owned host data which can survive mutator exit.
- Rework callback-free semantic-thunk signatures only where scoped access
  requires it. Do not adapt host loaders to accept a scoped evaluator merely
  to preserve the current generic closure type; I3E.1 separates those external
  demands. Retain I4B/I10 ownership classification for captured values.

Verification: a source inventory accounts for every evaluator function which
can allocate or inspect managed data. Add
`recursive_construction_reuses_one_mutator`,
`composite_construction_preserves_provenance_errors`, and
`owned_extraction_survives_mutator_exit`; focused call-graph tests prove nested
helpers reuse one outer admission. Existing evaluator and builtin suites remain
behavioral regressions. Production remains `NoAuto`; isolated closed fixtures
may force collection.

### Phase I3B.2 — Poll/Wait Driver Separation

- Refactor synchronous and patient evaluation so a driver alternates bounded
  enter/poll/root/exit steps with waits outside managed access. Do not wrap an
  entire `eval_value` call in one mutator region when it may reach
  `wait_for_claimed_task` or another blocking coordinator operation.
  `wait_for_claimed_task` is an ordinary semantic/coordinator wait: a busy
  producer is not proof that it currently owns a callback-free same-heap
  evaluator region or can finish without reaching another dependency or
  collection boundary. Retain only the mutator-free `EvaluatorStepContext`
  across this wait and reopen value access after resumption.
- Keep resumable scheduler-visible machine paths nonblocking: dependencies
  return `Blocked`, the machine parks after the quantum ends, and another
  worker may resume it later. An opaque deferred-source Rust callback cannot
  yet suspend and resume; it may temporarily cooperatively pump a dependency,
  but only with the mutator-free step context. I3E.1's deferred-source family
  split owns removal of that compatibility path.
- Ensure budget exhaustion and nested pumping cannot extend an outer mutator
  across a wait. Direct isolated evaluation uses the same step driver rather
  than a separate long-lived authority path.
- Preserve the separately reviewed `NetContention::wait_for_disturbance`
  exception from I3D.3. That wait may retain same-runtime mutator admission
  only under its stronger bracketed-claim, acyclic handoff, and no-collection-
  needed-for-progress proof. It does not generalize to coordinator waits,
  promises, reflection gates, or imports.

Verification: injected barriers force busy producers, promises, budget
exhaustion, and patient waits, asserting zero active mutators while sleeping
and successful resumption in a later quantum, including on another worker. Add
`blocked_machine_parks_without_mutator` and retain direct-evaluation result and
failure regressions. Production remains `NoAuto`.

Completed 2026-08-29. Direct and patient evaluation retain their cooperative
pump driver, but its `wait_for_claimed_task` boundary carries only durable
`EvalContext`/`EvaluatorStepContext` state and reopens managed access after the
wait. `EvaluationValueAccess` consequently retains only the matching scoped
value capability; it no longer carries a redundant durable context route.

The audit also tested making every scheduled dependency immediately yield.
That is not yet sound for an opaque deferred-source Rust callback: the callback
has no resumable continuation, and restarting it may allocate a fresh
dependency indefinitely. The temporary scheduled compatibility path therefore
continues to cooperatively pump such dependencies, but
`scheduled_nested_dependency_runs_without_mutator` proves that it inherits no
managed-access region. I3E.1's deferred-source migration owns removing this
seam.
`patient_claimed_task_wait_releases_mutator` forces a worker-owned producer and
performs a full collection after the patient reaches the actual
condition-variable wait. `blocked_machine_parks_without_mutator`,
`worker_releases_mutator_before_sleep`, and the extended budget-exhaustion
probe cover the other release paths. The separately documented bracketed
interaction-net disturbance wait remains unchanged; it is not coordinator
wait policy. Production remains `NoAuto`.

### Phase I3C.1 — Cooperative, Patient, and Worker Poll Routing

- Route `ClaimedTask`, runtime pumping, patient demand, executor workers,
  client demand, sparks, direct effect runs, and isolated searches through the
  I3A.3 poll context. These are consumers of one orchestration carrier, not
  independent heap-entry policies.
- Keep coordinator selection/release, settlement, delivery activity, and
  condition-variable waits outside active evaluator scopes. A pure machine
  poll uses one scope unless it deliberately yields an owned intermediate
  result; an effect poll follows the phase rules in I3D.2.
- Make every collection request observable at a bounded evaluator-scope
  boundary without storing a mutator or poll context in scheduler records.

Verification: extend `workers_force_sparks_and_poll_ready_reflection_tasks`
with channel-synchronized worker observations, and cover ordinary cooperative
tasks, client demand, sparks, and patient pumping with deterministic route
counters. Add `worker_releases_mutator_before_sleep`,
`blocked_machine_parks_without_mutator`, and
`all_poll_routes_use_scheduler_context`. Production remains `NoAuto`.

I3C.1 completed 2026-08-29. `EvaluationPollContext` now has two explicit
admission sources: a checked detached coordinator claim for scheduled work,
and the already-owned demand session for caller-driven effect runs and
isolated searches. Both carriers are mutator-free and validate the exact
demand session before opening a bounded evaluator scope. Scheduled effect
wrappers consume the coordinator-provided carrier rather than silently
constructing another one, while their direct `EffectTask::poll` counterpart
constructs the same carrier from its explicit owner. Spark forcing now uses
the scoped strategy-demand implementation, removing the runtime pump from the
direct-evaluator compatibility inventory. The internal evaluate/interpret
partition of an effect quantum remains assigned to I3D.2; this checkpoint
unifies admission without pretending that the entire effect poll is one
callback-free scope.

`all_poll_routes_use_scheduler_context` latches cooperative, patient,
client-demand, and spark routes. The worker fixture independently observes
both a spark and a scheduled task after their poll carriers are constructed,
and `scheduled_effect_wrapper_rejects_an_unrelated_poll_context` proves that a
scheduled effect cannot substitute another demand session. Existing bounded
direct-effect and isolated-search fixtures exercise the explicit-owner route.
Production remains `NoAuto`.

### Phase I3C.2 — Poll Outcome and Release Audit

- Move completed-value rooting from reflection/deferred release into the
  evaluator substep which produces the value. Preserve already rooted public
  effect-task results instead of converting them to bare core values and
  recreating roots later.
- Restrict `EvaluationWaitPoll::Complete` bare projections to an active
  evaluator scope; non-evaluator observers retain or receive the owned root.
- Prove cancellation, machine destruction, release, terminal publication, and
  status wakes operate only on owned/rooted outcomes.

Verification: force collection requests between poll return and release for
every `EvaluationMachinePoll` variant; preserve failure identity tests and add
`completed_effect_root_is_not_recreated_after_scope` and
`wait_completion_projection_requires_scoped_access`. Production remains
`NoAuto`.

I3C.2 completed 2026-08-29. The I3A.4 audit had already moved
`EvaluationMachinePoll::Complete` rooting into the producing evaluator substep
and made coordinator release publish that root directly. This checkpoint
closed the remaining wait-observation hole: `EvaluationWaitPoll::Complete`
now returns an owned `RuntimeValueRoot`, and only
`EvaluatorStepContext::project_root` exposes its bare semantic value inside a
bounded managed-access region. Deferred lazy/promise evaluation uses that
scoped projection. Scheduled-effect lifecycle code and `.task.join` instead
transfer the root directly into `PublicValue`, without extracting and
recreating it. The compatibility-access inventory consequently shrank from
198 to 195 entries.

The current compatibility root embeds the still-large `core::Value`. Embedding
it directly widened recursive evaluator frames enough to reproduce a stack
overflow in `dictionary_tag_and_tuple_patterns_match_or_fall_through`.
`EvaluationWaitPoll` therefore boxes only its completed-root observation and
has a compile-time two-word size bound. The authoritative terminal record
continues to own the root inline. I4F.2 should remove the transitional box when
the managed root representation itself becomes pointer-sized.

`every_poll_outcome_releases_managed_access_before_publication` inserts a
test-only probe after machine poll return and before coordinator release, then
successfully forces collection for yielded, blocked, complete, failed,
cancelled, and exit outcomes. It also verifies runtime provenance for the two
value-bearing variants. `wait_completion_projection_requires_scoped_access`
latches the owned poll result and its only evaluator projection route, while
`completed_effect_root_is_not_recreated_after_scope` source-latches both
scheduled effect completion adapters. Existing failure-identity, parked
machine, terminal-publication, cancellation, and destruction fixtures remain
in force. Production remains `NoAuto`.

### Phase I3D.1 — Reflection-Gate Reservation and Activation Split

- Make pure evaluation of `anno refl:Task` reserve or discover a stable task
  handle and return an owned dependency without invoking
  `ReflectionTaskLauncher::build` or another interpreter callback.
- After the evaluator scope ends, let poll orchestration activate the reserved
  task exactly once. Concurrent first observers share the reservation and
  either observe activation or block on its task token; cancellation and
  abandoned reservations retain their existing terminal behavior.
- Generalize only the reserve/activate lifecycle needed later by deterministic
  import demands. Reflection and imports retain distinct semantic policy,
  environments, caching, and failure provenance.

Verification: `reflection_gate_reserves_inside_and_activates_outside_scope`
observes no mutator during launcher construction;
`reflection_gate_observer_and_activation_orderings_are_forced` explicitly
selects each first observer and each first activator; cancellation before and
during activation remains covered. Production remains `NoAuto`.

I3D.1 completed 2026-08-29. A `ReflectionComputation` now caches one
`ReflectionTaskReservation` rather than starting a machine from its
`OnceLock` initializer. Pure evaluation reserves or discovers the stable task
and records its activation on the thread-bound `EvaluatorStepContext`.
`EvaluationPollContext::evaluate` and the direct-compatibility wrapper end that
carrier before draining recorded activations, so
`ReflectionTaskLauncher::build` cannot inherit evaluator authority. Client
demand, deferred lazy/promise work, and spark demand use this explicit
evaluate-then-activate boundary.

The reservation's atomic first activation owns launcher construction;
concurrent observers retain the same handle and wait while later activation
requests become no-ops. A cancellation which is already terminal skips
construction. Cancellation or demand closure racing construction still wins
through the existing coordinator install/terminalization protocol, and the
unused machine is destroyed after locks are released. Bare test sessions keep
their prior dormant record behavior, and annotation tasks continue to select
the immutable runtime-default reflection profile rather than the observing
task's profile. The reservation roots its effect across this boundary and
projects it only during activation, adding two explicitly classified
compatibility-access sites until I4F converts the underlying representation.

`reflection_gate_reserves_inside_and_activates_outside_scope` latches both
zero builds inside the evaluator phase and successful forced collection from
launcher construction.
`reflection_gate_observer_and_activation_orderings_are_forced` runs the full
owner/observer reservation-by-owner/observer activation matrix. Each case
asserts the designated reservation owner, the shared task identity, and one
build while the winning launcher is blocked and the overlapping activation
returns. A separate forced-order fixture covers cancellation before and during
activation. Existing gate/result failure identity and metadata reflection
fixtures remain in force. Production remains `NoAuto`.

### Phase I3D.2 — Effect Evaluation and Interpreter Phases

This phase is partitioned because callback authority, machine-state ownership,
and control fusion have distinct failure modes. The unfused machine remains
the reference semantics throughout; no checkpoint combines this work with net
claims or import loading.

#### Phase I3D.2a — Bounded Interpreter Evaluation Service

- Thread the admitted `EvaluationPollContext` to specialized-request dispatch
  without opening a poll-wide evaluator scope.
- Permit an interpreter callback to request evaluation explicitly through a
  private bounded service on `RequestContext`. Each request opens and closes
  its own callback-free evaluator scope, roots its WHNF result before leaving
  that scope, and returns the existing owned public `EvaluatedValue` view.
  The callback never receives or retains a mutator or evaluator carrier.
- Keep `TaskHost::{snapshot, commit}` and
  `TaskSpecialization::handle_request` outside every evaluator scope. Do not
  alter request parsing, delivery, transactions, or branch control here.

Verification: forced callback probes for `TaskHost::{snapshot, commit}` and
`TaskSpecialization::handle_request` can request collection successfully,
while a specialized handler can explicitly evaluate a lazy argument and keep
the owned result after that bounded call. Production remains `NoAuto`.

I3D.2a completed 2026-08-29. `EffectTask::poll_with_context` now threads its
admitted poll authority only to the drive substep which dispatches a
specialized request; it does not open a poll-wide evaluator region.
`RequestContext::evaluate` re-enters through that authority, reduces the
requested value to WHNF, and roots the result before the evaluator carrier is
finished. The public handler receives the same owned `EvaluatedValue` API as
before and cannot obtain either the carrier or its scoped managed access.

`effect_interpreter_callbacks_do_not_inherit_evaluator_mutators` forces the
specialized handler and both host transaction callbacks to request collection.
Its handler explicitly evaluates a lazy concatenation, carries the result
through a cut transaction, and returns it after commit. All three callbacks
therefore demonstrate mutator-free entry, while the returned value demonstrates
the bounded re-entry path. Existing reflection-machine coverage remains in
force. Production remains `NoAuto`.

#### Phase I3D.2b — Explicit Effect-Machine Phases

- **I3D.2b.1 — Request boundary.** Refactor `EffectTask` so the beginning of a
  monadic step is explicit: evaluate and parse the next request in one
  callback-free evaluator scope, root every request value which leaves that
  scope, finish the scope, and only then interpret the owned request. Host and
  specialization callbacks remain wholly outside the evaluator phase.
- **I3D.2b.2 — Continuation boundary.** Apply continuations and perform every
  delivery-time demand in later callback-free evaluator scopes admitted by the
  current poll. Root results before they leave those scopes. Remove the
  corresponding direct-compatibility evaluator calls from request decoding,
  continuation application, and delivery.
- This checkpoint converts the values which cross the new phase boundaries;
  it does not duplicate I4F.1's source-wide conversion of the recursive
  `Branch`, `Control`, fixpoint, transaction, and outcome representations.
  Until I4F.1, those existing durable fields remain compatibility `Value`
  owners under production `NoAuto`. No evaluator carrier, managed borrow, or
  scoped access may enter any machine field. I4F.1 later replaces every such
  durable raw field with a stable root-shaped surface or exact managed edge;
  I4F.2c turns those surfaces into registered roots at the production
  managed-value switch.

Verification: phase-latched tests observe request parsing, interpreter entry,
and continuation delivery in order; callbacks continue to collect; blocked
evaluation resumes without retaining scoped authority. Production remains
`NoAuto`.

I3D.2b completed 2026-08-30. `EffectTask` now evaluates the effect object,
applies `eff`, reduces the request, materializes its payload, and parses request
IDs inside one poll-admitted `EvaluatorStepContext`. Every request argument is
converted to a compatibility `RuntimeValueRoot` before that context ends;
specialized arguments retain the same ownership through `PublicValue`.
Interpretation, including host snapshots, commits, and specialized handlers,
starts only after the decoding scope has finished. Standard request paths,
reset/shift/fix control, continuation application, and delivery-time demands
now re-enter only through bounded poll-context evaluator phases, rooting each
value result before it crosses back into orchestration.

`effect_interpreter_callbacks_do_not_inherit_evaluator_mutators` uses a strict
phase probe to require request parsing, interpreter entry, and continuation
delivery in that order while all three callback families collect successfully.
`isolated_search_reports_and_resumes_lazy_dependencies` collects after forced
request blocking and then resumes the exact promise, demonstrating that the
blocked machine retained neither its evaluator carrier nor active managed
access. The direct evaluator inventory no longer contains
`src/reflection/machine.rs`. The compatibility bare values recursively stored
inside branches remain explicitly assigned to I4F.1; production remains
`NoAuto`.

#### Phase I3D.2c — Reference Path and Standard-Effect Fusion

- Add a test-only forced-unfused mode and make that phase boundary the
  reference semantics.
- Fuse a standard request with adjacent evaluator work only when a pure runner
  could implement it as a deterministic transformation of branch-local state
  and control. Fuse bounded `.seq`/`.r` chains, their immediate Glam
  continuation application, and task-local `.get`/`.set`. A value consumed by
  that same evaluator phase remains unrooted; root only the applied effect,
  modified local state, pending continuation, or request argument which must
  cross the resulting machine boundary.
- Keep `.alt`, `.fail`, and `.cut` as explicit structural scheduling/control
  transitions: they have no adjacent evaluator demand to eliminate. Keep
  reset/shift/fix and non-Glam delivery control unfused because they publish
  captured control, promises, or delimiters whose replay obligations are more
  important than one fewer admission. Shared heap/volume state, task
  operations, logging, reflection, and every specialized request remain
  interpreter boundaries. Stop immediately after applying a continuation;
  the newly produced effect is rooted and published before it is demanded, so
  a later reflection-gate wait cannot replay the application.
- Fusion must preserve alternative rollback, retry observations, continuation
  order, and exactly the owned values retained by the unfused path.

Verification: run each fused standard family against forced-unfused execution
and compare results, permanent failures, branch order, retry state, task-local
state, and retained roots. An admission counter may demonstrate reduced scope
churn but is not a semantic assertion. Production remains `NoAuto`.

I3D.2c completed 2026-08-30. Test builds can force the original explicit
request/interpreter path per `EffectTask`; production uses a bounded fusion
budget of 32 requests. The fused path parses raw request values, consumes
`.seq` operations and `.r`/local-state deliveries inside one callback-free
evaluator phase, and applies at most the immediately available Glam
continuation before publishing a rooted effect. Pending continuations and a
modified local state are rooted only when they survive the phase. Every
specialized, shared-state, branching, captured-control, fixpoint, exit, and
non-Glam delivery request is converted to the same rooted request used by the
unfused reference before interpretation.

The equivalence fixtures compare successful values, permanent failures,
cut/alternative behavior, reset/shift/fix control, isolated-search branch
order, retry observations, and task-local state. A cooperative-budget fixture
crosses the 32-request boundary. The root probe additionally proves that
ordinary effect and state chains construct strictly fewer compatibility roots
when fused than under forced-unfused execution. Callback collection coverage
from I3D.2b remains in force.

The full suite also exposed the known recursive deferred-pump stack margin
when the new phase preparation changed debug-frame layout. The implementation
does not raise the test stack or accept repetition as evidence: drive
preparation and interpretation now have distinct Rust frames, while terminal
wait-result decoding is outside `await_deferred_task`'s recursive pump frame.
This changes neither wait policy nor scheduling and the previously failing
dictionary-pattern fixture passes on the ordinary test stack. Production
remains `NoAuto`.

#### Phase I3D.2d — Closure and Boundary Audit

- Audit every `EffectTask` evaluation call and interpreter callback against
  the explicit phase model. Remove the temporary direct-compatibility seams
  assigned to this phase and document any deliberately unfused family.
- Run the callback and equivalence matrices under forced scheduling and close
  the phase only when machine state contains no scoped evaluator authority.

Production remains `NoAuto`.

I3D.2d completed 2026-08-30. Every reusable reflection request and macro
effect callback now performs demand through `RequestContext`'s bounded
poll-admitted service. Path parsing and selection are also bounded services:
they return owned keys or a rooted public value before control returns to the
interpreter. The two `reflection::machine` helpers which manufactured an
`EvaluatorStepContext` directly have been removed, and the source inventory
now rejects durable-context evaluator calls anywhere in the effect machine,
request protocol, reusable requests, or macro specialization.

`EvaluationPollContext` is thread-bound as well as mutator-free. Because an
`EffectTask` must implement the `Send` evaluation-machine contract, neither a
poll context nor the already thread-bound evaluator context can enter its
durable state without defeating a compile-time bound. The callback collection
matrix continues to cover host snapshot, host commit, and specialization
dispatch. The forced-unfused equivalence matrix covers successful chains,
permanent failures, alternatives and cuts, reset/shift/fix control, isolated
branch order, retry observations, local state, the cooperative fusion budget,
and retained compatibility roots.

The audit keeps `.alt`, `.fail`, `.cut`, reset/shift/fix, specialized requests,
shared heap/volume state, task operations, logging, reflection, and non-Glam
deliveries deliberately unfused. Those operations publish choice,
transaction, captured-control, promise, or host obligations; their explicit
machine boundary is part of the reference semantics rather than an
optimization omission. One test fixture which paired an assembler value with
a synthetic coordinator from another runtime was corrected to use a
same-runtime session after bounded request evaluation began enforcing the
existing value-domain rule. Production remains `NoAuto`.

### Phase I3D.3 — Interaction-Net Claim and Contention Discipline

This phase is partitioned because the current `CoreRuntimeNet` alias, the
normalization lease, callable and operator claims, cursor claims, and the
contention wait each have different ownership and unwind behavior. The
generic topology remains collector-independent throughout. Production remains
`NoAuto`; subsystem-local closed fixtures may collect.

#### Phase I3D.3a — Exact-Domain Core-Net Facade

- Replace the `CoreRuntimeNet` type alias with a private newtype or equivalent
  facade over `SharedRuntimeNet<CoreSpecialization>`. The facade carries
  a non-retaining route to the `RuntimeValueDomain` which owns its semantic
  values. `EvaluationRuntimeId` remains the globally unique value-domain
  identity; the weak route supports future scoped heap access without
  retaining the runtime or requiring a process-global lookup.
- Route core-net construction through the matching `CoreValueFactory` or
  scoped runtime authority. Migrate `NetValue`, `FunctionCode`, net lowering,
  reflection-created functions, public assembly reconstruction, and test
  helpers without exposing the wrapped generic net.
- Keep clone, pointer identity, and other operations which neither lock nor
  inspect managed contents access-free. Do not yet change claim or
  normalization behavior in this checkpoint.

Verification: a foreign-runtime fixture rejects construction or use,
ordinary core callers cannot recover `SharedRuntimeNet<CoreSpecialization>`,
and all existing net construction and evaluation tests retain their behavior.

I3D.3a completed 2026-08-30. `CoreRuntimeNet` is now a private newtype pairing
the generic shared runtime with a weak observer for its exact
`RuntimeValueDomain`. Core templates are instantiated through
`CoreValueFactory` or an already qualified related net; `NetValue`, function
code, front-end lowering, source net construction, reflection request
functions, assembly construction, and evaluator fixtures no longer construct
raw core shared nets. The weak observer neither retains nor revives a dropped
runtime; it is a route back to the identified value domain rather than a
second identity scheme.

Core-specific frontier, cursor-dependency, step, and contention wrappers keep
generic shared owners from escaping indirectly. Prepared logical-copy sources
also carry the exact domain and cannot be installed into a target from another
domain. These wrappers preserve the existing raw claim and normalization
state machines for their assigned later checkpoints. Provenance tests cover
ordinary foreign-runtime rejection and non-retention; a source inventory
confines raw shared instantiation to the generic implementation and this
facade.
Production remains `NoAuto`.

#### Phase I3D.3b — Scoped Core-Net Observation and Mutation

- Derive a private core-net access view only from a matching
  `RuntimeValueAccess`. Move every ordinary operation which locks, inspects, or
  mutates core semantic net contents behind that view, including interface
  inspection, active-pair and cursor stepping, source-frontier inspection,
  stuck-pair diagnostics, and result extraction.
- Inventory the access-free facade surface explicitly. Durable net identity
  may remain in values and work descriptors, but no such descriptor carries a
  scoped access view or managed borrow.
- Keep the existing normalization lease, raw claimed dispositions, and
  contention handle as narrow, named transitional exceptions assigned to the
  following checkpoints. Do not broaden those exceptions while migrating call
  sites.

Verification: compile-fail, privacy, or source-inventory fixtures reject
ordinary `with`/`with_mut`-style inspection and semantic stepping without
matching access, while identity-only worklist operations remain usable after
the access scope ends.

I3D.3b completed 2026-08-31. `CoreRuntimeNetAccess` is a private,
non-`Send`/non-`Sync` view derived only from a matching
`RuntimeValueAccess`. Ordinary interface inspection, topology reads and
mutation, cursor and active-pair steps, prepared-copy inspection, stuck-pair
diagnostics, and result extraction now pass through that view. Evaluator net
work opens these regions through `EvaluatorStepContext` and closes them before
running Glam callables or operators. Durable `CoreRuntimeNet` values retain
only construction, identity, provenance, and the explicitly transitional
contention-wait surface assigned to I3D.3g.

Source-inventory tests reject an ordinary inspection surface on the durable
facade, scope tests prove the access view cannot escape to another thread, and
exact-domain tests reject an unrelated runtime while preserving identity work
after a scope closes. The migration also exposed test fixtures which compiled
or constructed a value in one runtime and evaluated it through another; those
fixtures now use the owning runtime rather than weakening domain validation.
Production remains `NoAuto`.

#### Phase I3D.3c — Scoped Normalization Batches

- Bind every core normalization lease which can lock on explicit close or
  `Drop` to the same access scope that admitted its batch. Reshape the net
  driver as needed so neither the lease nor its lock-capable fallback can
  enter durable machine or work-queue state.
- Preserve the existing rule that a normalization batch closes before Glam
  callable or operator evaluation. Do not conflate the batch lease with an
  active-pair claim: the lease serializes a local normalization quantum,
  whereas a claim records one durable semantic transition.

Verification: forced normal close, cross-net batch switching, contention, and
unwind all publish the expected disturbance. A latched callable/operator test
observes the batch closed before semantic evaluation begins.

I3D.3c completed 2026-08-31. The generic `NormalizationBatchLease` is no
longer re-exported to core callers. `CoreRuntimeNetAccess` instead exposes a
closure-scoped `with_normalization_batch`: it acquires and hides the lease,
closes it before returning the callback result, and retains the generic
lease's `Drop` fallback for unwind. The evaluator worklist processes all
immediately available work for one net inside that callback, closes on a
cross-net transition, and emits a durable semantic action only after the
scoped callback has returned. Callable, operator, coordinator-wait, and
contention handling therefore begin outside the current batch and managed
access region.

Normal close, dirty/contended publication, unwind, forced concurrent
followers, and cross-net switching have direct coverage. A thread-local test
witness checks the current evaluator's scope at callable and operator entry;
it deliberately does not assert that the shared net has no batch, because a
different evaluator may acquire the next batch immediately after close. The
forced compiler-helper concurrency fixture covers that handoff. Durable-facade
inventory prevents raw batch acquisition from returning. Production remains
`NoAuto`.

#### Phase I3D.3d — Callable Active-Pair Claims

- Replace manual `Claimed` bookkeeping for `Bind >< Data` calls with a private
  bracketed guard or closure returning an exhaustive `CallDisposition`.
  Cover resume with a copied net, resume with an operator, explicit blocking
  on an exact wait, permanent failure, and release. A stale initial claim or
  mismatched blocked-call retry fails before a guard is issued; it is not a
  terminal disposition of a claim the caller never owned.
- Confine the claim to one callback-free evaluator scope. Consuming terminal
  methods publish the selected durable state. `Drop`/unwind restores a safe
  replay state and publishes a disturbance: a fresh claim becomes ready again,
  while an exact retried claim restores its prior blocked wait. `#[must_use]`
  and private constructors supplement but do not replace that fallback.

Verification: forced schedules cover every disposition, exact blocked-call
reclamation, and panic/unwind fallback. Compile/privacy coverage prevents a
call claim from entering machine state or a poll outcome.

I3D.3d completed 2026-08-31. Core callable reduction now issues a private,
`#[must_use]`, non-`Send`/non-`Sync` `CoreCallClaim` only after an existing
fresh claim is still current or an exact blocked wait is atomically reclaimed.
It owns the callable clone and its replay fallback, but no managed-access view.
Lowering selects an exhaustive copied-net, operator, blocked-wait, permanent-
failure, or explicit-release disposition; all durable topology updates reopen
only a bounded matching-runtime access region.

Release and unwind restore fresh claims to `Ready` and retried claims to their
prior exact wait. Those restorations publish disturbances, whereas stale fresh
acquisition and mismatched blocked retries use conditional mutation and remain
quiet. Generic runtime tests cover release/restore rejection and replay.
Evaluator tests force all dispositions, both release paths, both unwind paths,
exact retry, mismatch, and stale acquisition. Compile-time negative trait
checks keep the guard thread-bound, and the durable-facade inventory prevents
claim operations from escaping scoped core-net access. Production remains
`NoAuto`.

#### Phase I3D.3e — Operator Active-Pair Claims

- Apply the proven claim shape to `Operator >< Data` without erasing the
  operator-specific completion which rewrites the topology. Keep successful
  data/operator yield, exact blocking and retry, permanent failure, release,
  and unwind as exhaustive dispositions. As with callable claims, stale
  acquisition and retry mismatch occur before ownership is issued.
- Share machinery with callable claims only where the resulting API remains
  clearer than the two state machines. Do not force cursor claims into the
  same abstraction.
- Preserve the production function-call client established by
  [`InteractionNetFunctionCalls_2026-08-31.md`](InteractionNetFunctionCalls_2026-08-31.md).
  Callable lowering directly splices `CoreOperator::Applicable(Function)`
  between the original argument and result neighbors; it must not recreate a
  synthetic `Bind >< Bind` pair merely to make operator claiming convenient.
  One explicit source `Bind` supplies one ordinary function argument, and a
  partial `Value::Function` is returned as `Data` rather than as
  `OperatorYield::Operator`.

Verification: force data and operator yields, retryable and permanent errors,
exact retry mismatch, and unwind. Preserve the structured operator failure in
the stuck pair. Keep the direct callable-splice tests and the source-level
unary, partial, explicitly chained, and captured-function regressions green.
Use `reflection_gate_blocks_and_resumes_an_exact_net_function_call` as a real
callable-to-operator client alongside the directly constructed operator
fixtures, and force its blocked/resumed ordering rather than relying on
repetition. Verify that the operator claim begins from the directly published
`Operator >< Data` pair and that no intermediate `BindJoin` becomes part of
the lifecycle protocol.

I3D.3e completed 2026-08-31. `Operator >< Data` reduction now issues a
private, `#[must_use]`, non-`Send`/non-`Sync` `CoreOperatorClaim` only while
the fresh claim remains current or after atomically reclaiming the exact
blocked wait. The guard owns cloned operator and data payloads plus a replay
fallback, but no managed-access view. Operator evaluation selects an
exhaustive data/operator yield, blocked wait, structured permanent failure,
or explicit release disposition before reopening bounded matching-runtime
access for the topology update.

Release and unwind restore a fresh operator call to `Ready` or a retried call
to its prior exact blocked wait, publishing one disturbance per transition.
Stale fresh acquisition and mismatched retry remain quiet and issue no guard.
Generic runtime tests cover release/restore acceptance and rejection;
evaluator tests force both success shapes, blocking, structured failure,
fresh and retried release, fresh and retried unwind, stale acquisition, and
exact-wait mismatch. Compile-time negative trait checks retain the
thread-bound guard, and the durable-facade inventory keeps all operator-claim
operations below scoped core-net access. The production function-call client
still splices directly to `Operator >< Data`; its forced reflection-gate
blocked/resumed regression remains green without a synthetic `BindJoin`.
Production remains `NoAuto`.

#### Phase I3D.3f — Cursor Claim Lifecycles

- Replace manual cursor claims with a `CursorDisposition` protocol covering
  both active-pair-owned cursors and pairless cursor obligations. Preserve the
  rule that source-frontier inspection and target completion never hold two
  net mutexes simultaneously.
- Make progressed, blocked on a local/source dependency, stable, disturbed or
  gone, and released/unwound outcomes exhaustive. A claim token, if retained
  internally, is invariant in the evaluator-scope lifetime and exposes only
  consuming terminal methods.

Verification: preserve the complete pairless-cursor, converging-cursor,
source-frontier, ownership-transfer, and cursor-WHNF regressions. Add forced
unwind and each terminal disposition, and prevent cursor claims from entering
the driver worklist.

Implement this phase through three bounded checkpoints:

- **I3D.3f.1 — Pairless guarded steps.** Introduce one private, `#[must_use]`,
  non-`Send`/non-`Sync` cursor-claim guard in the generic shared runtime. It
  retains the exact target owner and source-frontier coordinates, but no net
  mutex guard. Its consuming `Advance` disposition inspects the source before
  reopening the target; explicit release and `Drop` restore the target claim
  to ready work and publish the transition. Route pairless `step_cursor`
  acquisition and advancement through this guard.
- **I3D.3f.2 — Active-pair guarded steps.** When an exact ready active pair
  discovers a remote cursor, construct the same private guard before the
  locked step ends, inspect and finish it outside the target lock, and publish
  only the terminal `CursorProgress` in `ActivePairStep`. The generic
  low-level reducer may retain its explicit claimed result as a test utility,
  but no core/evaluator production step may receive it.
- **I3D.3f.3 — Closure and verification.** Remove the manual
  `advance_claimed_cursor` core/evaluator surface and the corresponding
  production branch. Force fresh pairless and pair-owned release and unwind,
  prove each restores exactly ready owner state with one transition, and keep
  the existing dependency, stable, disturbed, gone, materialized, joined,
  contention, and lock-separation matrix green. Source and privacy inventories
  must prevent a cursor guard or manual advancement operation from entering
  durable evaluator state.

I3D.3f completed 2026-08-31. Both pairless obligations and active-pair-owned
remote cursors now issue the same private, `#[must_use]`, non-`Send`/non-`Sync`
`CursorClaimGuard` while the target net is locked. The guard retains the
claimed owner and immutable source coordinates but no mutex guard. It inspects
the source frontier first, then reopens only the target net to publish
materialized, joined, dependency-blocked, or stable progress. Explicit release
and `Drop` restore fresh claims to ready owner state and publish exactly one
transition.

`SharedRuntimeNet::step_cursor` and cursor-producing
`SharedRuntimeNet::step_active_pair` now terminalize that guard before
returning. The latter rewrites its `RemoteCursor` reduction with terminal
progress, so the core evaluator no longer performs a separate manual advance.
The low-level mutable `RuntimeNet::reduce_pair` retains `Claimed` only as an
explicit generic test utility. Both core step conversion boundaries reject a
live claim, and the durable core-net facade no longer exposes
`advance_claimed_cursor`.

Forced tests cover explicit release and unwind for both owner forms, verify
one topology/disturbance publication and exact ready restoration, and retain
compile-time thread-bound checks. Existing materialized, joined, local/source
dependency, stable, disturbed, gone, contention, converging-frontier,
ownership-transfer, source/target lock-separation, deep-chain, and cursor-WHNF
tests remain green. The complete repository suite passes with 1,218 library
tests plus all auxiliary targets. Production remains `NoAuto`.

#### Phase I3D.3g — Contention Handoff and Local Closure Audit

- Treat `NetContention::wait_for_disturbance` as a narrow synchronization
  handoff, not a semantic dependency or deadlock edge. It may wait while the
  same-runtime mutator is held only because another active evaluator owns the
  normalization batch or structurally acyclic claim, collection is not needed
  for progress, and that owner must publish a disposition before any semantic
  park. Do not generalize this exception to promises, reflection gates,
  imports, or coordinator waits.
- Remove the transitional raw lease, claim, and contention entry points from
  ordinary core/evaluator callers. Keep worker saturation, delayed collection,
  and contention wake storms as later profiling concerns; they do not weaken
  the no-escaped-claim rule.

Verification: forced schedules cover a contending evaluator wake and a claim
owner encountering a semantic wait; the latter must publish `Blocked` before
the machine parks. Privacy and source inventories close the local core-net
facade and preserve existing contention-order regressions. I3D.4 retains the
subsequent system-wide reflection and net-region audit.

I3D.3g completed 2026-08-31. Core contention now crosses the scoped net-access
boundary as a private, non-cloneable, one-shot handoff. Consuming the handoff
waits only for the disturbance epoch observed when another evaluator owned the
normalization batch; it is not a scheduler dependency and cannot be retained
as reusable machine state. The generic interaction-net lease and contention
types remain available to the generic runtime and its tests, while source
inventories reject raw core-specialized owners, leases, and contention tokens
outside `core_net`.

The net driver retains no value-access or normalization scope across this
handoff. A schedule-controlled evaluator test holds a batch, latches the
follower exactly after it observes contention, proves it cannot return early,
then releases the owner and verifies successful resumption. A separate forced
unresolved-callable test verifies that semantic evaluation first replaces its
claim with the exact `Blocked` wait, closes the batch, and only then returns
the blocked driver result which the enclosing machine may park. Existing
reflection-gate call and operator regressions continue to cover later exact
resumption. Production remains `NoAuto`.

### Phase I3D.4 — Reflection and Net Region Audit

- Audit scheduled reflection, isolated searches, net construction, active-pair
  work, cursor work, and stuck-net exits against the preceding phase rules.
- Prove semantic net/store locks do not escape their intended region. No host
  callback runs while a net/store lock or mutator is inherited; the explicitly
  documented net-contention handoff is the only mutator-bearing wait.
- Establish here, rather than deferring to I8, that every legitimate ordinary
  holder of the core semantic-net mutex also holds the matching scoped runtime
  access. I8 still owns the collector-side lock operation and exact trace, but
  may rely on exclusive collection excluding every ordinary lock holder.

Implementation is partitioned after the entry audit:

- **I3D.4a — Scoped net semantic helpers.** Preserve the admitted
  `EvaluatorStepContext` through interaction-net builtin dispatch, active-pair
  operator application, and lazy access resolution. Keep durable wrappers only
  where a separately assigned effect/deferred boundary still requires one;
  ordinary net work must not discard scoped authority and re-enter through the
  compatibility gate.
- **I3D.4b — Net-construction callback boundary.** Route construction request
  argument demand through `RequestContext`'s bounded evaluator service and
  evaluate the final exposed construction port through the already-admitted
  owning evaluator step. Net-construction specialization callbacks must be
  able to force collection, proving that the enclosing lazy/effect-machine
  composition retains only mutator-free orchestration carriers.
- **I3D.4c — Reflection/net closure audit.** Reconcile scheduled reflection,
  isolated search, net construction, active-pair, cursor, contention, and
  stuck-net exits. Extend inventories to reject direct compatibility entry in
  net-construction callbacks and raw core-specialized net locking outside the
  facade. Preserve the one documented contention handoff as the only
  mutator-bearing wait.

I3D.4 completed 2026-08-31. Interaction-net builtin dispatch, active-pair
operator application, and lazy access resolution now retain their already
admitted `EvaluatorStepContext`; they no longer discard that authority and
immediately reopen the direct-compatibility gate. The now-unused durable
index-number and key-path wrappers were removed, while the durable access
wrapper remains solely for the separately assigned effect boundary.

Construction request callbacks demand `.copy` counts and `.wire` ports
through `RequestContext::evaluate`, and the owning lazy evaluator decodes the
completed branch's exposed port through its existing scoped evaluator step.
The isolated search still owns an `EvalContext` across polls, but neither that
context nor its poll/evaluator carriers retain a mutator. A callback-side
collection probe makes that distinction executable rather than relying on
type commentary alone.

The closure audit found no additional lock escape. Generic topology work runs
inside `CoreRuntimeNetAccess::with_normalization_batch`; callable/operator
semantics, reflection callbacks, isolated-search waits, and terminal
publication occur after the batch and transient managed access have closed.
Stuck-pair inspection stays entirely within scoped core-net access and invokes
no host policy. `CoreNetContention` remains the sole deliberate wait carrying
net/value-domain provenance: it is private, non-cloneable, one-shot, and owns
no live mutator or normalization lease while blocked.

Verification is latched by
`effect_interpreter_callbacks_do_not_inherit_evaluator_mutators`,
`isolated_search_reports_and_resumes_lazy_dependencies`,
`interaction_net_bind_calls_an_embedded_source_function`,
`semantic_wait_is_published_to_the_net_before_driver_parking`,
`contending_evaluator_hands_off_then_resumes_after_batch_publication`, and
`every_poll_outcome_releases_managed_access_before_publication`. Source
inventories reject direct evaluator compatibility in construction callbacks,
unclassified dispatcher downgrades, raw core-specialized net ownership
outside `core_net`, and ordinary durable facade inspection. Production
remains `NoAuto`; subsystem-local closed fixtures may collect.

### Phase I3E.1 — Semantic Thunks and Deterministic Deferred Host Calls

- Split the current generic `DeferredComputation` family by operational role.
  Internal semantic thunks remain evaluator-pure and callback-free, receive
  only scoped evaluation access, and may execute within an evaluator region.
  Module and binary loaders become deterministic deferred host calls:
  forcing one reserves or discovers its demand, exits scoped access, invokes
  the host loader, validates the declared content identity or stable secure
  hash, publishes a rooted result, and resumes evaluation through the demand
  token.
- Reuse the reflection gate's reserve/activate mechanics where helpful without
  describing imports as reflection. Imports are semantically reproducible;
  reflection remains outside reproducibility. Their producer capabilities,
  cache keys, environments, and error contexts stay distinct.
- Inventory any remaining `Fn(&EvalContext)` lazy producer. Classify it as a
  callback-free semantic thunk, an explicit host call, or a later
  traceable opaque boundary; no arbitrary host callback remains hidden in a
  pure lazy-machine evaluator scope.

Implementation is partitioned after the entry audit:

- **I3E.1a — Scoped semantic-thunk contract.** Rename the undifferentiated
  deferred source/type to a semantic thunk and give it only
  `EvaluatorStepContext`. Migrate the internal list-effect thunk and semantic
  fixtures to scoped evaluator helpers. No semantic thunk accepts a durable
  `EvalContext` or arbitrary host capability.
- **I3E.1b — Rooted host-call machine phase.** Add a distinct host-call source
  whose callback takes no evaluator context and returns a
  runtime root. The lazy machine first recognizes the source under its normal
  evaluator phase, yields, invokes the callback in a later mutator-free poll
  phase, then re-enters a bounded evaluator step to validate/project/cache the
  rooted result. Same-lazy concurrent observers continue to share the
  coordinator's one producer and wait token.
- **I3E.1c — Import-loader migration and content contract.** Make module and
  binary loaders publish roots from the compilation runtime and construct
  them as deferred host calls. Preserve `SourceArtifact` digest identity,
  `FileSourceSystem`'s stable repeated-read check, and final unchanged-file
  verification; no loader callback runs inside evaluator authority.
- **I3E.1d — Producer closure audit.** Add a source-backed inventory for every
  semantic-thunk and host-call constructor. Prohibit the removed generic
  deferred constructor and durable evaluator signatures, and reconcile tests,
  docs, and the compatibility-access inventories.

Verification: an import-loader collection probe in
`binary_import_forwards_hidden_source_provenance`,
`concurrent_host_calls_share_one_rooted_producer_without_parking`,
`host_call_rejects_a_foreign_runtime_root`, and
`file_system_detects_changes_after_a_read` cover the callback, sharing,
provenance, and stable-content contracts. A source-backed inventory classifies
every lazy producer; existing list/conditional lazy tests establish the
semantic-thunk path. Production remains `NoAuto`.

I3E.1 completed on 2026-09-01. `LazySource` now distinguishes a
callback-free `SemanticThunk`, which accepts only an `EvaluatorStepContext`,
from a `HostCall`, whose producer accepts no evaluator context and
returns a runtime root. The lazy machine recognizes a host-call source in one
bounded evaluator step, yields, runs the producer in a later mutator-free poll,
and re-enters bounded access only to validate, project, and cache the rooted
result. The existing lazy-work coordinator remains the sole producer election
and wait-token authority, so concurrent observers execute the callback once.

Module and binary import loaders now publish roots from the compilation
runtime. `SourceArtifact` digest identity, stable repeated local reads, and
final unchanged-file verification remain the content contract; custom source
systems remain trusted host capabilities that must return immutable source
artifacts. The internal list-effect thunk now uses scoped evaluator helpers.
Source inventories reject the old undifferentiated constructor, enumerate all
production semantic/host-call producers, and account for the new import-root
boundary.

### Phase I3E.2 — Compiler, Macro, and Closed-Value Regions

- Enclose compiler-value bundles, macro lookup/expansion, token searches,
  diagnostic formatter helpers, and recursive module-result construction in
  bounded evaluator scopes. Publish complete cache bundles and suspended
  compiler state using roots only.
- Source loading, recursive loader invocation, diagnostic publication, and
  macro/parser host policy execute outside inherited mutator access. A host
  component may explicitly call the evaluator service, obtaining another
  bounded scope.

Implementation is partitioned after the entry audit:

- **I3E.2a — Closed helpers and rooted cache publication.** Consolidate the
  built-in compiler and diagnostic-formatter closed evaluator, route it through
  the private client-demand service, and store every completed cached helper
  and lazily memoized effect path as a `RuntimeValueRoot`. Build candidates
  remain private until complete; cache locks never enclose evaluation or
  managed access. I4F.1b reconciles the durable cache surface and I4F.2c later
  changes the compatibility root representation, not this publication
  boundary.
- **I3E.2b — Macro driver and suspended expansion roots.** Route macro-result
  and macro-lookup WHNF demand through their existing runtime services rather
  than direct evaluator compatibility calls. Keep macro input, journal,
  output, and declaration-rewrite embedded data rooted across search polls,
  host request dispatch, waits, and diagnostics. Project embedded data only
  within the callback-free lexical/parser operation which consumes it.
- **I3E.2c — Recursive module-result handoff.** Keep prior/final definitions,
  opaque origins, per-input setup, declaration-to-declaration definitions, and
  completed module results in runtime roots whenever source loading, recursive
  import, macro execution, diagnostic publication, or compilation-execution
  drain may intervene. Each declaration's callback-free resolution/lowering
  receives a bounded value-access region and republishes its complete result
  before leaving it.
- **I3E.2d — Closure audit and documentation.** Remove the I3E.2 direct
  evaluator inventory entries, add source-backed inventories for durable
  compiler roots and root projection sites, and reconcile the architecture,
  ownership ledger, and public compatibility inventory. Raw semantic values
  may still exist within one bounded compiler operation; I4F.1/I4F.2 own the
  managed-root representation switch.

Verification: `compiler_suspension_parks_only_roots`,
`compiler_cache_publishes_complete_rooted_bundle`, and macro/import forced
schedules prove callback and wait separation. Production remains `NoAuto`.

I3E.2 completed on 2026-09-01. The built-in compiler and diagnostic formatter
publish complete per-runtime cache bundles whose members are
`RuntimeValueRoot`s; a cache lock never encloses evaluation or value access.
Macro lookup and result forcing now use the compilation execution's
client-demand services. Embedded macro data remains in public runtime roots
from output journaling through declaration replay and is projected only in a
bounded lexer/parser operation.

`CompileContext`, module-loader arguments, input setup, declaration lowering,
compiler diagnostics, and completed module results now retain roots whenever
source loading, macro execution, recursive import, diagnostic publication, or
compilation drain may intervene. Each declaration projects its prior rooted
state inside one callback-free lowering region and republishes the completed
state before the next declaration. Module sealing uses client demand rather
than the direct evaluator compatibility entry. Source-backed inventories now
account for both compiler roots/projections and all remaining direct evaluator
entries; only I3E.3 diagnostic helpers remain on the latter list.

### Phase I3E.3 — Event, Diagnostic, and Executable Callback Regions

- **I3E.3a — Diagnostic semantic demand.** Replace the remaining diagnostic
  helper calls through the direct evaluator compatibility API with ordinary
  client demand. Construct each semantic application in a bounded,
  callback-free value-access region, retain its runtime root across demand,
  and project only after completion. Diagnostic enrichment and contextual
  composition may share an evaluation session, but never a scoped mutator.
- **I3E.3b — Transactional event conversion and delivery.** Enclose runtime
  input/output value conversion in explicit bounded scopes. Retain the
  existing ordering in which input conversion and rooting finish before
  mutation admission, while output payloads are detached into a retained
  delivery ticket before decode and adapter callbacks run. Delivery
  terminalization reacquires guarded runtime state only after the callbacks.
- **I3E.3c — Rendering and executable callbacks.** Keep rendering evaluation
  and value extraction separate from terminal writers, diagnostic bus
  subscribers, source systems, and executable policy callbacks. Parked host
  records retain roots or owned host data, never scoped borrows, poll
  contexts, or mutators. Close the source-backed direct-evaluator inventory
  once the diagnostic remainder is gone.

Verification: `event_delivery_invokes_callback_without_mutator`,
`diagnostic_rendering_invokes_writer_without_mutator`, and input/output
forced-order tests prove retained values survive while no managed authority
crosses a host boundary. Production remains `NoAuto`.

I3E.3 completed on 2026-09-01. Diagnostic object construction now composes
builtin calls in bounded value-access regions and demands them through the
runtime's client-demand service; the source-tree latch records no remaining
external direct-evaluator entries. Diagnostic transport encoding and decoding
require the matching `Values` service and perform their complete structural
projection under scoped access.

Input conversion is forced before mutation admission, while output payloads
remain rooted through guarded claim and are decoded and delivered only after
the claim guard is released. Forced collections in input conversion, output
decode, output adaptation, diagnostic subscribers, and an injected terminal
writer verify that none inherits a mutator. The terminal writer receives
owned rendered bytes, and diagnostic/output callbacks reacquire guarded state
only after returning. Production remains `NoAuto`.

### Phase I3F — Multi-Runtime and Exit Audit

- **I3F.1 — Reference-collector multi-runtime admission.** A thread entering
  another runtime activates a separate heap-qualified TLS entry; recursive
  same-runtime entry reuses depth, epoch, and cache. Reuse the collector's
  forced reciprocal A-then-B/B-then-A test as the authoritative proof that two
  *uncommitted* stop-the-world collection requests do not make heap order a
  lock order. Add a Glam-domain test proving the evaluator consumes those
  independent TLS entries without inventing runtime-ID-based authority.
- **I3F.2 — Poll, wait, and worker exit boundaries.** A poll context without an
  active evaluator scope may orchestrate nested work in another runtime and
  confers no heap access by itself. Drop every active evaluator scope before a
  worker sleeps. On worker termination, explicitly release that thread's
  inactive collector caches; ordinary quantum exit need not discard reusable
  cursors. TLS eviction forgets cursors only; full collection recovers ranges.
- **I3F.3 — Admission closure audit.** Source-latch every production managed
  entry through the runtime-domain gateway and reject direct `Heap::with_mutator`
  use outside that owner. Reconcile existing patient-wait, blocked-machine,
  callback, worker-sleep, and nested-runtime tests with the final inventory.

I3F proves the current reference collector's bounded-authority and nested-heap
baseline; it does **not** prove starvation freedom. The successor concurrent
collector replaces idle-entry election with heap-local participant epochs.
Its CG0/CG1 checkpoints must preserve these tests, reinterpret worker exit as
participant retirement, and add continuously overlapping mutator schedules.

Verification: `reciprocal_nested_entries_pass_two_uncommitted_collection_requests`,
`runtime_tls_caches_remain_heap_qualified`,
`poll_context_without_scope_carries_no_heap_authority`, and
`all_managed_entries_have_bounded_mutator_regions`. Glam-level schedules also
cover `scheduled_nested_dependency_runs_without_mutator`,
`patient_claimed_task_wait_releases_mutator`,
`worker_releases_mutator_before_sleep`, and
`worker_termination_releases_inactive_collector_caches`. Production remains
`NoAuto`; passing I3 authorizes managed access, not production collection.

I3F completed on 2026-09-01. Nested runtime access creates independent
heap-qualified TLS cache records and preserves the matching value-domain
authority without treating `EvaluationRuntimeId` as heap identity. A bare poll
context was forced through full collection before and after its bounded access
region, proving that only the callback constitutes mutator admission. Worker
threads now retire every inactive per-heap cursor through an exit guard, while
ordinary quantums retain caches for reuse.

A source-backed inventory accounts for every managed construction/access
gateway and confines direct `Heap::with_mutator` calls to the core value-domain
owner. Existing forced schedules continue to cover worker sleep, patient
waiting, blocked machines, terminal destruction, and nested dependency work.
The concurrent collector plan records these as CG0/CG1 entry and retirement
baselines; continuously overlapping mutators remain its separate progress
proof. The mandatory post-I3 audit is recorded in
[`GarbageCollectorIntegrationI3_2026-09-02.md`](../reviews/GarbageCollectorIntegrationI3_2026-09-02.md).
It resolved the direct-admission latch, transitional lint, and durable-status
findings, so Phase I3 is complete and I4.0 may begin.

## Phase I4 — Core Trace Vocabulary and Leaf Policy

I4A-I4E develop and verify the managed shell, exact visitors, and adapters in
closed fixtures. They do not publish a production `core::Value` containing a
bare managed edge. I4F.1 first converts every durable production owner to a
root-safe shape; I4F.2 then switches the root facade and managed value shell
together. This ordering avoids any buildable interval in which production can
park an unrooted `Gc` merely because collection remains disabled.

### Phase I4.0 — Managed-Family Destruction Admission Contract

Establish the destruction contract before the first managed family or closed
managed fixture is allowed to collect. There are two distinct ownership
domains:

1. A collector-managed `Trace` allocation is passively droppable. Neither its
   direct `Drop` implementation nor transitive field destruction may obtain or
   invoke a Glam runtime, value domain, heap, evaluator, scheduler, diagnostic,
   event, host callback, or other active semantic capability. It may release
   ordinary Rust resources, but may not observe or preserve a `Gc` edge held by
   the dying representation.
2. An ordinary external/rooted Rust owner may perform active RAII cleanup
   through an independently owned runtime capability and registered roots. It
   is not reachable from the managed graph, is never reclaimed as a managed
   allocation, and therefore creates no heap backedge. Prefer an explicit,
   idempotent `retire` operation; where current semantics rely on scope exit,
   `Drop` may invoke that same operation as a fallback.

Every family checkpoint from I4A onward completes the stable ledger fields for
direct and transitive destruction *before* its first isolated collection. A
family with unresolved destructor authority, a transitive active `Arc` drop,
or an external lifecycle owner reachable from the managed graph is not
collector-ready. Any proposed managed exception requires a separate design
review and blocks the family checkpoint; a weak-domain or TLS bridge is not an
admissible local workaround.

Verification: add a compile-time/private-construction fixture proving managed
payloads cannot carry runtime/value-domain service capabilities, plus
`managed_family_collection_requires_completed_drop_record`,
`managed_drop_has_no_runtime_or_heap_capability`, and
`external_raii_owner_is_not_reachable_from_managed_graph`. The managed fixture
proves ordinary Rust resource release without runtime work; the external
fixture proves idempotent explicit retirement and a semantically equivalent
`Drop` fallback. No later isolated family fixture may collect unless this gate
and that family's drop record pass.

I4.0 completed on 2026-09-02. Glam's private runtime-value allocator now
requires `ManagedFamily` in addition to the collector's mechanical `Trace`
contract. Each unsafe family admission supplies a mandatory, non-empty
`ManagedDropRecord` naming the stable family/source and its reviewed direct and
transitive destruction. A merely traceable type and the known heap, value
factory, value-domain, and observer capabilities are compile-time-latched as
unadmitted; the fixture's compile-exhaustive field inventory contains only a
traced `Gc` edge and passive Rust resources.

The collection fixture proves direct and transitive passive destruction with
no runtime or heap capability. A separate external owner holds the value-domain
capability and registered root outside the managed graph, proves idempotent
explicit retirement, and proves the equivalent `Drop` fallback. Existing I1/I2
scalar, layout, and public-root prototype fixtures received explicit records.
This gate authorizes I4A family work, not a production managed value or
production collection.

### Phase I4A — Value Shell and Leaf Families

- Select the managed value shell/leaf granularity and implement
  representation-aware tracing for its then-current variants. Edge visitors
  report semantic edges rather than fixed field offsets.
- Treat bytes, numbers, atoms, static builtins, IDs, and similar external data
  as leaves unless the selected representation actually embeds a managed edge.
- Complete the stable family records for the shell and leaf nodes, including
  requested extent, layout acceptance, drop policy, and exact visitor review.

Verification: `managed_leaf_families_trace_zero_edges`,
`managed_value_shell_dispatches_every_variant`, and
`managed_value_shell_cycle_marks_once`. Invalid layout remains covered by the
I1D collector fixture. Production remains `NoAuto` and retains its
compatibility public root.

I4A completed on 2026-09-02. The initial bootstrap integration uses one
monolithic managed allocation shell corresponding exhaustively to the current
`Value` variants. Atom, number, binary, and static-builtin payloads are embedded
edge-free leaves. Recursive variants delegate through semantic edge visitation
rather than exposing field offsets. The opaque dispatch arm deliberately has
no payload: I4B and I10B.0 still own containment and representation of actual
opaque values.

The implemented shell remains a closed test fixture until I4B-I4F supply every
concrete recursive payload and root surface. Its exhaustive compatibility
mapping makes any new `Value` variant a compile error; its x86-64 Rust layout is
72 bytes with 8-byte alignment, and its requested 72-byte slot is accepted.
The I4.0 drop record classifies direct counter observation and transitive
Atom/Number/Bytes, mutex, `Gc`, and `Arc` destruction as passive. Rooted leaf
shells trace zero edges, all thirteen variants dispatch explicitly, and a
rooted self-cycle marks once before becoming reclaimable. This initial
monolithic choice does not constrain the later compact, split representation
owned by the value-representation refinement plan.

### Phase I4B — Closure and Opaque Managed-Edge Containment

Close non-traceable storage before the first recursive identity becomes a bare
managed pointer:

- use I0's constructor inventory to find every deferred Rust closure and opaque
  payload which can retain an internal `core::Value`;
- replace Glam-owned closure captures with explicit traceable computation state
  where practical;
- otherwise attach an explicit bundle of same-runtime public roots only when
  it represents genuine ownership outside the managed graph. A backedge from
  an internally owned closure or opaque payload through such a bundle is a
  retention defect, not an acceptable tracing substitute, because it hides
  the very lazy/promise/fixpoint cycles collection is intended to remove;
- narrow opaque construction so an arbitrary payload may contain no managed
  edge, while audited families may contain same-runtime public roots but never
  a bare `Gc`, unrooted recursive `core::Value`, or foreign-runtime root; and
- add construction-time and compile-time tests proving each admitted closure
  and opaque family cannot smuggle a bare managed pointer after I5 changes the
  representation of captured values.

This checkpoint need not move opaque storage into collector-owned finalizable
cells. It establishes edge safety; I10 later completes ownership, scoped
downcast, destruction, and conservative-retention policy. I5 is blocked until
I4B passes.

Verification: `closure_and_opaque_constructor_inventory_is_classified`,
`opaque_payload_rejects_bare_managed_pointer`,
`opaque_payload_rejects_unrooted_core_value`, and
`opaque_payload_rejects_foreign_root`. Production remains `NoAuto`; no opaque
finalization is enabled here.

This phase is implemented in four bounded checkpoints:

1. **I4B.1 — Inventory and admission shape.** Re-run the constructor/capture
   inventory, assign every production site to semantic computation, external
   host callback, or one named opaque family, and install the source-backed
   closure latch before changing representations.
2. **I4B.2 — Opaque containment.** Make opaque construction require a private
   unsafe family admission with an explicit edge/ownership record. Replace
   `CompilationOrigin`'s stored recursive value with non-value compilation
   provenance. Current production opaque tokens then contain no bare value,
   managed pointer, or public root; root-bearing opaque families remain
   forbidden until a later reviewed need supplies same-runtime validation.
3. **I4B.3 — Semantic computation containment.** Replace production
   capture-bearing `SemanticThunk` closures with a function pointer plus an
   explicit ordered value-capture array. Retain the closure constructor only
   as test scaffolding. Classify `HostCallProducer` separately as a
   mutator-free external callback whose Glam arguments and results are
   explicit runtime roots; I10A owns its final external-root/backedge
   reconciliation.
4. **I4B.4 — Closure.** Latch every constructor and family, run the four named
   negative/admission fixtures, update the ownership ledger, and verify that
   no production semantic thunk or unrestricted opaque constructor remains.

I4B completed on 2026-09-02. `OpaqueValue::new` now requires the private unsafe
`OpaquePayloadFamily` admission and a non-empty ownership record. The four
production families are source-latched: compilation provenance and
construction ports are edge-free; effect tokens and reflection task handles
are explicitly external lifecycle capabilities for I9/I10. Bare `Gc`, raw
`core::Value`, and `RuntimeValueRoot` do not satisfy the admission bound. This
currently forbids all root-bearing opaque payloads rather than relying on a
same-runtime exception that no production family needs.

`CompilationOrigin` now stores `CompilationTrace`, not a preconstructed core
value, and derives the public diagnostic frame only when the reflection
inspector asks for it. Production callback-free deferred evaluation uses
`SemanticComputation`, whose operation is a non-capturing function pointer and
whose ordered value captures are explicit. The list-effect implementation is
the sole production constructor; capture-bearing `SemanticThunk` remains
test-only scaffolding. Deferred imports use separately named
`external_host_call` constructors with mandatory source/capture records and
runtime-rooted Glam arguments/results. Their genuinely external closure
environments remain an explicit I10A backedge/lifecycle audit rather than
being misclassified as traceable here.

The source-backed containment inventory rejects new/unclassified semantic
closures, host calls, opaque constructors, and opaque admissions. Focused
tests exercise the exact capture order, mandatory host-call record, opaque
family record, all three negative opaque cases, unchanged origin inspection,
and list-effect behavior. Production collection remains `NoAuto`; I4B closes
constructor admission and does not enable a managed opaque representation.

### Phase I4C — Recursive Payload and Failure Structures

- Implement exact visitors for builtin argument arrays, access/application and
  function-call payloads, evaluation failures, and context frames without
  forcing or formatting contained values.
- Add exact compatibility visitors for the then-current `LazyCell` source and
  result, `PromiseCell` assignment/producer data, metadata carrier, fixpoint,
  and reflection-computation payloads. I5/I6 replace these adapters in the same
  checkpoint that each representation migrates.
- Immutable arrays need no mutation gateway; any one-write or replaceable edge
  receives a representation-local safe gateway.

Verification: `argument_and_application_visitors_enumerate_exact_edges`,
`compatibility_recursive_payload_visitors_enumerate_exact_edges`,
`shared_cyclic_failure_context_traces_exactly`, and
`failure_trace_invokes_no_semantic_service`. Only isolated closed fixtures may
force collection; production remains `NoAuto`.

Implement this phase in three bounded checkpoints:

1. **I4C.1 — Immutable and failure adapters.** Establish one semantic-value
   edge visitor over the compatibility representation. Cover builtin argument
   arrays, lazy application/access/function-call payloads, semantic captures,
   fixpoint payloads, failure emissions, and ordered context frames. The
   visitor reports direct edges only and never recursively evaluates them.
2. **I4C.2 — Deferred compatibility adapters.** Cover `LazySource`, the
   source-or-terminal-result state of `LazyCell`, `PromiseCell` assignment,
   metadata, and reflection computation. Treat successful reflection
   reservation state and promise producer/notification companions as external
   lifecycle state rather than duplicate semantic edges. Latch the existing
   `LazyValue::cache`, `PromiseCell::{publish,publish_guarded}`, and reflection
   task initialization sites as the sole replaceable/one-write gateways.
3. **I4C.3 — Verification and closure.** Exercise every recursive payload
   family, exact ordered arguments/contexts, dependency-cycle failure data,
   and lazy sentinels proving visitation performs no semantic work. Reconcile
   the ledger and phase status without enabling production collection.

I4C completed on 2026-09-02. The private
`CompatibilityValueEdges` vocabulary reports direct semantic `Value` edges
for immutable argument/application payloads, explicit semantic captures,
fixpoints, failures and ordered contexts, metadata, reflection computations,
lazy source-or-result state, and promise assignments. It neither recurses into
reported values nor evaluates, formats, or compares them. Net identities stay
outside this adapter for I4E/I8.

For one-write state, a successful reflection reservation and promise
producer/notification records remain external lifecycle state rather than
duplicate semantic edges; failed reservation/assignment values are reported
through their structured failure. A terminal lazy result supersedes its
source edge, matching publication-before-source-removal. The visitor takes a
stable source snapshot without invoking its callback while holding the source
mutex. Source-backed latches identify `LazyValue::cache`,
`PromiseCell::{publish,publish_guarded}`, and reflection task initialization as
the representation-local gateways that I5/I6 must preserve when the storage
becomes managed.

Focused fixtures cover exact argument and context order, every compatibility
family, shared failure context identity, dependency-cycle leaf data, and a
panic sentinel proving visitation performs no semantic work. Collection is
still disabled for production; these adapters are replaced atomically by each
family's managed visitor in I5-I8.

### Phase I4D — Persistent Collection Adapters

- Trace RPDS and FingerTree/list contents logically, including keys, chunks,
  thunks, shared slices, and mapped values. Use the libraries' existing
  iterators: RPDS red-black traversal uses an explicit logarithmic navigation
  stack and FingerTree traversal uses explicit iterator frames, so I4 need not
  duplicate their structural algorithms merely to avoid call-stack recursion.
- Traverse Glam's potentially unbalanced `ListNode::Concat` shell with a small
  explicit local worklist and report thunk edges without forcing them. This is
  a trace-adapter detail, not a commitment to a new list representation. The
  broader evaluator stack-size limitation remains the pre-GC observation
  recorded by Gate G0.
- Duplicate traversal of shared spines is correct but measured; do not fork or
  replace persistent collections.
- Add trace-count instrumentation for the later I7 performance audit.

Verification: `persistent_adapter_traces_empty_singleton_and_shared_spines`
and `persistent_adapter_cycle_reclaims_in_isolated_heap`. Production remains
`NoAuto`; I7 later reconciles these adapters against the final concrete
persistent representations.

Implement this phase in two bounded checkpoints:

1. **I4D.1 — Logical adapters and instrumentation.** Give the generic list
   representation one non-forcing logical-part walk. Traverse unbalanced
   `Concat` nodes with an explicit local worklist, use the FingerTree iterator
   for finger chunks, report strict value slices and deferred thunks without
   forcing either, and count node/chunk/segment/edge visits. Visit RPDS map
   entries through its iterator in key order; exhaustively inspect Glam's
   recursively structured `Key` leaves while reporting only mapped values as
   semantic edges. Counters measure logical trace work and do not deduplicate
   shared physical spines.
2. **I4D.2 — Closed verification and reconciliation.** Cover empty, singleton,
   byte, strict-value, thunk, concat, finger, nested-key, and shared-spine
   shapes. A closed managed fixture must use the same logical adapters to prove
   rooted survival and unrooted reclamation of a list/dictionary cycle. Update
   the ownership ledger and retain production `NoAuto`.

I4D completed on 2026-09-02. The generic list representation now exposes one
crate-private `visit_logical_parts` walk. It uses an explicit LIFO worklist for
arbitrarily shaped `Concat` shells, preserves left-to-right order, delegates
FingerTree structure to its iterator, and reports byte segments, strict shared
value slices, and deferred thunks without forcing them. Its counters record
logical node, chunk, segment, value, and thunk visits; shared physical spines
are deliberately counted once per logical occurrence.

The compatibility adapters report every strict list value and lazy/promise
thunk, every RPDS mapped value in key order, and no byte or key edge. An
exhaustive iterative `Key` walk records recursively nested key work and makes
the current value-free key policy explicit. Persistent nodes and immutable
arrays acquire no mutation gateway. Focused fixtures cover every current list
shape, nested keys, persistent map versions, duplicated shared list spines,
non-forcing lazy sentinels, and rooted survival followed by reclamation of a
closed list-to-dictionary cycle. Production remains `NoAuto`; I7 replaces and
re-audits these compatibility adapters with the final managed value form.

### Phase I4E — Net Value Adapter

- Define the exact non-reducing visitor boundary for `NetValue`, function-stage
  net handles, and the core net payload types selected before I8.
- Do not yet migrate synchronized runtime-net ownership or value-replacing net
  mutations; I8 owns that choice and its gateway inventory.

Verification: `net_value_adapter_traces_without_reduction_or_materialization`
and `net_value_adapter_cycle_marks_exactly`. Production remains `NoAuto`.

Implement this phase in two bounded checkpoints:

1. **I4E.1 — Logical payload and identity adapters.** Add one generic,
   read-only `RuntimeNet` payload walk which reports data, operators, retained
   source-net identities, and specialization-owned stuck reasons. It must not
   claim work, reduce an active pair, follow a source frontier, materialize a
   cursor, publish a topology revision, or register/perform a semantic wait.
   Translate that walk through bounded `CoreRuntimeNetAccess`; report the
   direct values held by every
   `CoreOperator` variant and the external net identities held by `NetValue`,
   `FunctionCode`, `FunctionValue`, unresolved net/function-call lazies,
   function-capture operators, copy state, and cursor dependencies. Classify
   retained `EvaluationHalt` payloads exhaustively while treating wait tokens
   as leaves.
2. **I4E.2 — Closed verification and reconciliation.** Cover every current
   core-operator variant, direct function/net value arms, function stages,
   unresolved net/function-call sources, a retained specialization failure,
   and an untouched remote-copy layer. Latch unchanged topology/disturbance
   revisions and a no-forcing sentinel. A closed managed fixture parameterizes
   the same generic runtime-net walk over managed data and proves rooted
   survival followed by reclamation of an unrooted net self-cycle. Update the
   ownership ledger and retain production `NoAuto`.

I4E completed on 2026-09-02. `RuntimeNet::visit_logical_payloads` now walks
only authoritative representation state under its caller-provided read
boundary. It reports every data/operator node, every source identity retained
by copy or blocked cursor state, and every specialization stuck reason; ports,
fan identities, wait tokens, structural nodes, and generic no-rule failures
remain leaves. Counts describe owning occurrences, so duplicated source
handles are not deduplicated.

The core adapter qualifies source handles with their owner's value domain and
then reports exact direct `Value` and `CoreRuntimeNet` edges. Operator matching
is compile-exhaustive, function-code operators expose their stage net, and
halt matching distinguishes structured failure, blocked wait, and unassigned
promise without forcing any payload. Focused tests preserve net revisions,
retain a stuck pair, leave a remote cursor unmaterialized, and prove that a
semantic thunk is not invoked. A separate closed specialization stores a
managed self-edge as runtime-net data and demonstrates exact survival and
reclamation. This phase does not migrate the synchronized owner or authorize
production collection; I8 still owns the managed outer cell, lock protocol,
and value-installing mutation gateways. I5's lazy-cell migration must combine
value and net categories from one stable source/result snapshot rather than
calling the two compatibility adapters across a publication race.

### Phase I4F.1 — Durable Root-Surface Conversion Gate

Before the production managed-root switch, complete a source-backed inventory
of every value which can outlive its constructing mutator region and migrate
its *storage surface* to a root-shaped type. This phase is organized by
ownership behavior rather than the spelling of the current wrapper.

For every source result, choose exactly one disposition:

1. durable external storage uses `RuntimeValueRoot` or the selected
   registered-root equivalent;
2. the value is behind an exact managed edge whose owner and visitor are
   introduced in the same checkpoint; or
3. the value is a bounded mutator-local which cannot be parked, erased,
   returned, published, or captured.

The field migrations in I4F.1 deliberately use the compatibility
`RuntimeValueRoot` implementation while no production value contains `Gc`.
They establish stable owner types, constructors, and retirement points without
claiming that this temporary wrapper registers a collector root. I4F.2 changes
the wrapper and managed value representation atomically. There is no permitted
buildable state combining a `Gc`-bearing production value with the old
non-registering wrapper.

The former phase text required each I4F.1 owner fixture to force collection.
That was chronologically impossible: the compatibility wrapper being installed
here is not yet a collector root. I4F.1 instead registers an exhaustive table
of real owner fixtures and proves ordinary publish/retain/retire behavior. The
same table becomes the post-switch collection matrix in I4F.2e. No I4F.1 test
may imply that a compatibility root survived collection.

Implement the gate through the following independently buildable checkpoints.
Each checkpoint updates the executable owner inventory and the ownership ledger
in the same commit as any field or constructor change.

#### Phase I4F.1a — Inventory Schema and Baseline

- Add one owner-level inventory whose rows name the source module, concrete
  type and field/container, lifetime boundary, construction/publication site,
  retirement site, chosen disposition, and owning migration checkpoint.
- Seed it from the existing public compatibility-access, evaluator, compiler,
  closure/opaque, and ownership-ledger inventories, but do not infer safety
  from a token count. Mechanically latched counts detect source drift; reviewed
  rows explain the semantic disposition.
- Scan both the library and binary crate. `src/bin` cannot use private core
  projections, but it can retain public values, diagnostics, configuration,
  logger state, and callback captures across mutator regions.
- Classify direct and container-held `core::Value`, public `Value`,
  `RuntimeValueRoot`, `EvaluatedValue`, `EvaluationFailure`, `Gc`, synchronized
  net handles, and every `Any`, closure, generic attachment, or callback which
  can hide one. A bounded-local row must identify the existing I3 scope proof;
  an unexplained or merely presumed local fails the inventory.
- Define the common fixture protocol which later rows use to name a real owner
  constructor, publication boundary, retained-value observation, and
  retirement path. Each owner checkpoint registers its own fixtures; I4F.1a
  does not implement all of them up front.

Verification: first latch an intentional mismatch, then add
`durable_value_owner_inventory_is_complete`. At this baseline it rejects a new
source owner, a missing current/target disposition, an unassigned migration
checkpoint, an unknown type-erased family, or a bounded-local claim without
its enforcing scope. Rows which still require conversion are explicitly
`open(checkpoint)`; I4F.1g, not I4F.1a, requires zero open rows and zero durable
bare values/managed pointers. Production remains unchanged and `NoAuto`.

I4F.1a completed 2026-09-02. The executable inventory now separates reviewed
owner semantics from mechanical source drift. Its owner rows record current
storage, target disposition, construction/publication and retirement
boundaries, the responsible I4F.1 checkpoint, and either the common durable
fixture contract, an existing bounded-scope proof, an exact managed-edge
proof, or an edge-free proof. Open conversion rows remain explicit; this
checkpoint does not claim that the compatibility `RuntimeValueRoot` is already
a collector root.

The source side parses production Rust declarations with `syn` rather than
counting substrings. It currently latches 135 declarations whose stored types
mention core/public values, runtime roots, evaluated values, failure graphs,
synchronized nets, managed pointers, type erasure, or callbacks. The aggregate
signal counts plus a deterministic declaration fingerprint detect balanced as
well as simple additions/removals, and every discovered declaration maps to a
named semantic owner row. Test-only support and the inventory/closed-fixture
modules are excluded. The baseline was first run empty and produced the
intended mismatch before the reviewed count, fingerprint, and assignments were
installed. Production representation, behavior, and `NoAuto` policy are
unchanged.

#### Phase I4F.1b — Canonical Cache and Attachment Roots

Implement this owner family in two checkpoints:

1. **I4F.1b.1 — Canonical bundle and initialization seam.** Convert
   `CoreValues` and the runtime canonical cache to stable root-shaped fields.
   Resolve initialization without a heap-retaining backedge. If construction
   must become two-stage, publish one complete root bundle; never expose a
   partially initialized canonical set. Verify canonical accessor behavior,
   one-time complete publication, construction-scope exit, and
   last-domain-owner release without collection.
2. **I4F.1b.2 — Type-erased attachments and compiler caches.** Give the
   runtime extension cache a private admitted-family boundary. A type-erased
   attachment is accepted only when its family record proves that every
   retained Glam value is a same-runtime root (or that the payload is
   value-free); arbitrary `Any` is not grandfathered by existing cache use.
   Convert `GCompilerValues`, cached diagnostic/compiler helpers, and every
   type-indexed runtime or compilation attachment. Verify harmless duplicate
   construction/races, complete-bundle publication, same-runtime rejection,
   owner retirement, and a source latch covering every admitted family.

Neither checkpoint places a strong value-domain capability inside a cached
payload or performs collection.

I4F.1b.1 completed 2026-09-02. All seven `CoreValues` fields now use the
compatibility `RuntimeValueRoot` surface. Construction receives only the
already allocated runtime ID, builds every semantic value and root in a local
complete `CoreValues` value, and installs that value inline while constructing
`RuntimeValueDomain`; there is no partially initialized cache, `OnceLock`,
factory capture, or heap/domain backedge. Existing accessors temporarily clone
the bare core projection while the I4F.2 compatibility bridge remains active.

`canonical_cache_publishes_one_complete_root_bundle` verifies all fields have
the exact runtime provenance, preserve canonical accessor behavior, and are
shared by scoped factories. `canonical_cache_releases_with_the_last_value_domain_owner`
proves the complete bundle remains live through a second factory owner and its
metadata payload is released with the final domain owner, without collection.
The executable durable-owner inventory rejected the seven-field
`Value`-to-`RuntimeValueRoot` change before its source baseline and `CoreValues`
row were updated from open to closed. Production remains `NoAuto`; extension
cache admission remains the separate I4F.1b.2 checkpoint.

I4F.1b.2 completed 2026-09-02. `RuntimeValueCache` and compilation-local
cache tiers now retain `Arc<RuntimeCacheEntry>` rather than accepting arbitrary
`Box<dyn Any + Send + Sync>`. The private unsafe `RuntimeCacheFamily` boundary
requires a stable family/source record and a complete visitor over every
retained `RuntimeValueRoot`. Admission runs after candidate construction but
before publication, rejects a root whose runtime differs from the cache
domain, and stores the reviewed record beside the type-erased payload. Cache
lookup rechecks the `TypeId` and family record. Construction, root validation,
and losing-candidate destruction remain outside the cache mutex, preserving
harmless duplicate construction and one complete installed winner.

`GCompilerValues` and `CachedDiagnosticFormatter` are the only production
families. Their compile-exhaustive visitors report all fixed roots; the
compiler effect-value map reports its synchronized roots and records the owning
runtime so its mutation/access gateway rejects a mismatched compiler factory.
Value-free and rooted fixtures exercise both admission policies. Focused tests
prove forced racing construction, pre-publication cross-runtime rejection,
and retirement with the final value-domain owner. A syntax-backed source latch
names every `RuntimeCacheFamily` implementation by file and concrete type, so
a new type-indexed attachment cannot silently inherit admission. The durable
owner and compiler/public compatibility inventories were intentionally tripped
before accepting the new boundary and counts. Production remains `NoAuto`; the
compatibility roots become registered collector roots only at I4F.2c.

#### Phase I4F.1c — Evaluation, Coordinator, and Readiness Roots

Migrate the evaluation family through four bounded checkpoints. Shared failure
identity remains an implementation property throughout this compatibility
stage; I6C later replaces the failure shell and its recursive interiors.

1. **I4F.1c.1 — Failure-root boundary.** Introduce one runtime-provenanced,
   non-forcing `RuntimeFailureRoot` compatibility wrapper. It retains the
   existing shared `Arc<EvaluationFailure>` identity plus a
   `RuntimeValueRoot` for every direct emission/context value reported by the
   compile-exhaustive compatibility visitor. Construction must neither inspect
   nor recursively walk those values. Prove exact edge coverage, duplicate
   occurrence preservation, shared failure identity, and same-runtime roots.
2. **I4F.1c.2 — Poll, task, wait, and coordinator records.** Reconcile machine
   poll failures, parked task blocks, wait terminals/polls, task statuses,
   promise-terminal publication, and the per-session failure ledgers. A bare
   failure may exist only inside a bounded evaluator operation; it becomes a
   failure root before poll return or coordinator publication. Destructure
   every affected enum so new terminal states cannot acquire an unclassified
   value field.
3. **I4F.1c.3 — Client demand and remaining session owners.** Reconcile
   client-demand operations/results/work, pending activations, and the
   remaining session-lifecycle owners. I4F.1c.2 necessarily migrated session
   failure reports and unfinished-task errors together with the shared
   coordinator ledger; this checkpoint audits those report projections and
   closes their lifetime fixtures rather than changing their storage again.
   Preserve existing failure identity while testing forced park/resume,
   terminal publication, acknowledgement, handle abandonment, and
   owner-session retirement. `SparkDemand` is already root-shaped and receives
   an explicit audit rather than another wrapper.
4. **I4F.1c.4 — Settlement and readiness closure.** Reconcile exit/kill
   settlement state and host-visible quiescence, deadlock, and unfinished-work
   snapshots. Close the evaluation-family durable-owner rows and source
   baseline only after retained reports survive the originating work/session
   retirement without collecting their values.

At every checkpoint, IDs, subscriptions, work tokens, and proven edge-free
coordination companions remain unrooted. Verification extends the I3
poll-boundary and producer-role latches and uses forced event order rather than
repeated scheduling as race evidence.

I4F.1c.1 completed 2026-09-02. `RuntimeFailureRoot` now preserves the existing
shared `Arc<EvaluationFailure>` identity while retaining one shallow
`RuntimeValueRoot` per direct emission/context occurrence. Its one edge walk is
owned by `EvaluationFailure::visit_direct_values`; the pre-managed trace
adapter delegates to that same compile-exhaustive match, so failure-root and
future managed tracing cannot silently disagree. Construction from a factory
or an already-proven runtime publication seam does not evaluate, format,
compare, or recursively traverse any value. Focused tests preserve duplicate
occurrences, prove same-runtime root provenance and `Arc` identity, and use a
panic-on-force lazy value to latch the non-forcing contract. The durable-owner
source baseline now classifies the new compatibility root explicitly. Durable
poll/coordinator owners remain unchanged until I4F.1c.2.

I4F.1c.2 completed 2026-09-03. `RuntimeFailureRoot` is now one shared pointer
to the failure identity and its shallow value-root bundle, keeping recursive
poll and status enums pointer-sized. Evaluator and reflection poll boundaries
must construct that root before returning a permanent failure. Machine polls,
parked task-block errors, wait terminals and polls, task statuses, task-owned
promise terminal publication, and producer-indexed failure ledgers now carry
the root without reconstructing it at each publication surface. The session
failure report and unfinished-task error fields necessarily use the same type
because they snapshot that ledger and parked task state; I4F.1c.3 retains the
separate responsibility for their lifetime and retirement fixtures.

A forced terminal-publication test proves that the task wait, status
publisher, and failure-ledger snapshot retain the same underlying
`Arc<EvaluationFailure>`, not merely equivalent messages. The compile-
exhaustive poll inventory now classifies every failure-bearing variant as a
root, and the durable-owner source latch was deliberately observed failing
before its reviewed count, fingerprint, and task-owner row were updated.
Temporary projections back to `Arc<EvaluationFailure>` remain only at
inventoried downstream client-demand, settlement/readiness, and reflection
owners assigned to I4F.1c.3, I4F.1c.4, and I4F.1d; raw promise cells remain the
separate recursive-core migration owned by I6C. Production remains `NoAuto`.

I4F.1c.3 completed 2026-09-03. `ClientDemandPoll` and
`ClientDemandResult` now carry `RuntimeFailureRoot`; a permanent
`EvaluationHalt` is converted while the `EvaluatorStepContext` is still
active, before the poll crosses into coordinator state. The result cell,
retirement record, and retained handle therefore preserve a rooted failure
without requiring the coordinator or originating demand session to remain
live. Forced settlement also roots the client kill result before publication;
its broader settlement and readiness payloads remain assigned to I4F.1c.4.

The compile-exhaustive client-demand latch classifies successful, failed,
blocked, abandoned, and killed boundaries; a companion report latch
destructures every unfinished-work field and roots its optional error. A
structured-failure fixture forces
a promised demand to park, publishes the external failure, resumes the exact
subscriber, retires its work record, drops the handle, session owner, executor,
and coordinator, and then verifies the retained root's runtime, direct edges,
and shared failure identity. The
persistent failure-ledger fixture now likewise retains a structured report
across owner closure, acknowledgement, context release, and ledger mutation.
Existing forced-order activation/cancellation fixtures cover pending
reflection activations, while the spark audit confirms that queued, claimed,
blocked, and retirement records move the existing `RuntimeValueRoot` without
another wrapper. The durable-owner source latch was observed failing before
its baseline and the client-demand/session rows were closed. Production
remains `NoAuto`.

I4F.1c.4 completed 2026-09-03. Coordinator deadlock snapshots and the
host-visible unfinished-work records now retain their retryable blocked
failure as `RuntimeFailureRoot`. Readiness remains observational: its
immediate diagnostic projection borrows the rooted failure without forcing
it, while the explicit post-settlement projection may evaluate through the
caller's matching runtime as before.

Forced settlement now constructs one root for its killed-work failure and one
root for the unfulfilled-promise exit failure, then clones those roots through
client results, task statuses, wait terminals, and settlement selections.
The still-unmigrated `PromiseCell` receives a bounded projection back to the
shared `Arc<EvaluationFailure>`; that recursive core interior remains assigned
to I6C. Compile-exhaustive latches cover both the coordinator settlement
records and the private host-readiness failure field. A structured-failure
fixture forces block, snapshot, kill, settlement, work retirement, owner-
session release, and runtime-handle release in that order, then proves the
retained report still owns the same failure root and both direct semantic
values. The existing task/client forced-kill fixture additionally proves that
the two terminal surfaces share the same root allocation. The durable-owner
source latch was observed failing before its baseline and readiness row were
closed. This closes the evaluation/coordinator/readiness root-surface slice;
production remains `NoAuto`.

#### Phase I4F.1d — Reflection Store, Protocol, and Machine Roots

Reflection owns the largest compatibility-access cluster, so migrate it in
three checkpoints:

1. **I4F.1d.1 — Store and query state.** Reconcile protected volumes,
   persistent store states/snapshots, journals, queries, transactions,
   rewrites, and commit records. Reflection environments belong to the host
   and protocol lifecycle in I4F.1d.2 rather than the store. Preserve snapshot
   isolation and verify snapshot/query retention, conflict, commit,
   retirement, and scope exit without collection.
2. **I4F.1d.2 — Lifecycle, protocol, and search.** Complete this boundary in
   three independently verifiable checkpoints:
   - **I4F.1d.2a — Task failures and effect lifecycle.** Preserve
     `RuntimeFailureRoot` when coordinator terminal states become `TaskHalt`
     or `EffectLifecycleStatus`; root any bounded compatibility failure before
     it leaves a direct `EffectRun`. Reconcile the effect root, optional
     failure context, reservation, activation, synchronous result, scheduled
     status, and terminal retirement paths.
   - **I4F.1d.2b — Protocol and host snapshots.** Reconcile request results,
     specialization-owned snapshots and journals, `HostSnapshot`,
     `TaskCommit`, `Transaction`, and host environments. Generic
     specialization state remains an explicit external root-owner contract;
     core protocol fields may not silently inherit that exemption.
   - **I4F.1d.2c — Isolated search.** Reconcile the isolated host environment,
     retained root branch, alternative stack, completed branches, blocked
     failure, and returned result collection. Verify restart/discard,
     successful and failed branch retention, blocking/resumption,
     cancellation, and final retirement.

   All three preserve the existing evaluator/interpreter phase boundary. Use
   compile-exhaustive ownership latches for concrete records and deterministic
   lifetime tests; no checkpoint may rely on repeated scheduling as evidence.
3. **I4F.1d.3 — Machine frames and requests.** Complete this larger graph in
   five ordered checkpoints:
   - **I4F.1d.3a — Reusable request state.** Reconcile `ReflectionRequest`,
     `ReflectionJournal`, deferred updates, `QueryRead`, tagged task states,
     query-writer handoff, and opaque task handles. Public values and
     diagnostics are roots; pending tasks and query handles delegate to their
     already inventoried lifecycle owners; borrowed mutation/query contexts
     remain bounded.
   - **I4F.1d.3b — Outer machine shell and failures.** Reconcile the effect API,
     contextual wrapper, task terminal, blocked evaluation error, exit state,
     and public poll projection. Root errors when they enter parked blocked or
     terminal state rather than relying on every eventual poll consumer.
   - **I4F.1d.3c — Branch and execution state.** Complete this graph in two
     checkpoints:
     - **I4F.1d.3c.1 — Branch roots.** Convert branch effect/state and their
       retry clones to root-shaped storage. Root fused effect/state updates at
       the assignment gateway rather than retaining a parallel phase-root
       bundle; pending raw continuation values remain bounded by that bundle
       until I4F.1d.3d.
     - **I4F.1d.3c.2 — Work and cut roots.** Convert deliver/apply/outcome
       work, cut operations/alternatives, and retry work to root-shaped
       storage, then close the branch/execution inventory. Raw values may
       exist only within one bounded evaluator/interpreter step.
   - **I4F.1d.3d — Captured control and fixpoints.** Complete this graph in two
     checkpoints:
     - **I4F.1d.3d.1 — Captured control.** Convert continuation values,
       delimiter reset stacks, reset frames, and captured-continuation
       containers to root-shaped storage.
     - **I4F.1d.3d.2 — Fixpoints and closure.** Convert fix roots/restarts and
       close the captured-control graph. Promise handles retain their
       separately inventoried lifecycle contract through I5.
   - **I4F.1d.3e — Decoded-request handoff and closure.** Reconcile the rooted
     `Request` form, specialization request state, and phase-root bundle;
     classify `Request<_, Value>`, fusion records, parser temporaries, and
     `MachineStep` as bounded. Add a compile-exhaustive inventory over the
     complete machine graph, close both owner rows, and rerun park/resume plus
     terminal-retirement fixtures.

   A claimed machine may move raw values only within one bounded poll;
   anything placed back into a coordinator, store, query, or continuation is
   root-shaped first. Treat task handles, wait tokens, and reflection
   capabilities as edge-free only when their concrete fields prove it. Every
   checkpoint uses compile-exhaustive field/variant latches and deterministic
   lifetime or park/resume fixtures; repetition is not concurrency evidence.

Each checkpoint decreases the reflection compatibility-access inventory or
records an explicit bounded projection disposition; an occurrence may not
silently move between modules.

I4F.1d.1 completed 2026-09-03 as a root-surface closure audit rather than a
storage rewrite. The reflection store already retained every heap,
runtime-query, and protected-volume value as the public `Value` compatibility
root: `StoreSnapshot`, `StoreJournal` views and edits, `ReflectionStore`, query
polls, and decoded query states contain no durable raw core value or
`EvaluationFailure`. Query IDs/domains, revision metadata, conflict indexes,
and commit outcomes are edge-free companions. A compile-exhaustive latch now
destructures all of those records and variants so a new field cannot silently
inherit either classification.

The remaining `.as_core`/`.into_core` calls in `store.rs` are bounded pure
projections used to construct lazy path accesses, rebased updates, and query
state. Every value crossing back into a snapshot, journal, store, or query
result is wrapped with `PublicValue::from_core` first; their later retirement
with the facade compatibility API remains assigned to I4F.2a. New lifetime
fixtures retain unforced lazy sentinels through heap roots, protected volumes,
journal edits, snapshots, and query-result publication, then verify exact
release after the last owning journal or result is dropped. Existing tests
continue to cover snapshot isolation, conflicts, commits, volume revocation,
and query retirement. The store owner row is therefore closed as
`PublicRoot`; production remains `NoAuto`.

I4F.1d.2a completed 2026-09-03. `TaskHalt` now records whether a permanent
failure is the narrowly permitted edge-free/bounded compatibility form or a
`RuntimeFailureRoot`. Text-only `TaskHalt::new` remains edge-free, structured
public `Error` conversion establishes a root from the diagnostic's runtime,
and adding a public context roots the complete expanded failure. The existing
bounded `EvaluationHalt` conversion remains only for the evaluator/machine
handoff assigned to I4F.1d.3.

Effect lifecycle publication now moves coordinator `RuntimeFailureRoot`
values directly into failed and killed `TaskHalt` states instead of stripping
them back to raw `Arc<EvaluationFailure>`. Scheduled result polling preserves
the same rule, while synchronous `EffectRun` roots any bounded failure before
returning it to a Rust caller. A composed caller failure may deliberately get
a distinct same-runtime root after child-report context is appended; the
unmodified lifecycle status retains the coordinator root identity. Compile-
exhaustive latches cover the complete lifecycle and `TaskHalt` representations,
and deterministic tests cover coordinator-root identity, structured public
error conversion, public-context rooting, direct-run publication, and
scheduled failure publication. The lifecycle owner row is closed as a
compatibility root, and the durable declaration latch was observed failing
before its reviewed fingerprint was updated; protocol host state and isolated
search remain assigned to I4F.1d.2b/c. Production remains `NoAuto`.

I4F.1d.2b completed 2026-09-03 as a protocol-boundary closure. Concrete
request results, task outcomes, host snapshots, commits, and transactions
already retain semantic values through the public `Value`, `StoreSnapshot`,
and `StoreJournal` root surfaces; their borrowed request/transaction contexts
remain bounded to one interpreter callback. Compile-exhaustive latches now
cover every field and variant so a new protocol field cannot silently inherit
either classification.

Generic `TaskSpecialization::Request`, `Snapshot`, and `Journal` state remains
an explicit implementor-owned root contract rather than an exemption: any
semantic value retained across dispatch, transaction, or search boundaries
must use a public/runtime root. Deterministic lifetime fixtures use such a
specialization to prove request results, outcomes, snapshots, commits, and
transactions retain their public roots until the owning carrier retires. The
durable-owner inventory now closes the protocol row independently and assigns
isolated search its own open I4F.1d.2c row. Production remains `NoAuto`.

I4F.1d.2c completed 2026-09-03. Isolated-search construction, retryable block
publication, and terminal failure publication now root every bounded
`TaskHalt` in the search runtime before returning it to a host. The search host
environment, successful values, specialization snapshots/journals, and
returned result collection already use the protocol's public-root contract.
The nested `EffectTask` remains the separately inventoried I4F.1d.3 owner; the
search wrapper does not duplicate its interior ownership classification.

Compile-exhaustive latches cover the host, search policy, result branch,
blocked state, poll variants, and wrapper. Deterministic lifetime fixtures
verify host, successful and failed branch, snapshot/journal, restart/discard,
completed-result, and blocked-error retention and exact retirement. Existing
fixtures cover state and lazy blocking/resumption, branch order, complete
search restart, and cancellation; the blocked and terminal evaluation-error
fixtures now also require an explicit runtime failure root. The isolated-
search owner row is closed as a compatibility root. Production remains
`NoAuto`.

I4F.1d.3a completed 2026-09-03 as another closure audit. Reusable request
journals already store `Diagnostic` public roots, and decoded task states plus
query reads store public `Value` roots. Deferred launch, cancel, and
acknowledgement updates delegate to the already inventoried pending-task and
task-handle lifecycle; opaque task handles contain scalar runtime provenance
plus task/query capabilities and no hidden semantic value. Query mutation
authority remains borrowed for one guarded writer callback.

Compile-exhaustive latches cover the reusable request enum, journal updates,
journal fields, task-handle cell, decoded task-state variants, query read, and
borrowed mutation token. Deterministic lifetime fixtures verify that journal
diagnostics, decoded complete/failed task states, and query results retain
their public roots exactly until retirement. Existing fixtures cover pending
task policy ordering, task-handle query retirement, host non-retention, and
foreign-runtime rejection. The request owner row is closed as `PublicRoot`;
machine interiors remain assigned to I4F.1d.3b-e. Production remains `NoAuto`.

I4F.1d.3b completed 2026-09-03. The effect API and optional diagnostic
context now enter the long-lived machine shell as explicit
`RuntimeValueRoot`s. Retryable evaluation errors are rooted when parked, and
terminal failures are rooted by the single `finish` gateway. Blocked and
terminal poll projections preserve those exact failure roots rather than
extracting an `Arc<EvaluationFailure>` and depending on each consumer to root
it again. Exit errors already retain `RuntimeValueRoot` through `ExitIntent`;
successful completion continues to retain its public root.

Compile-exhaustive latches cover the outer task shell, scheduled wrappers,
blocked/error state, exit state, poll variants, and terminal variants.
Deterministic fixtures verify exact retirement of contextual roots and verify
that blocked and terminal failure roots survive retirement of their source
owner until the projected poll root is itself dropped. The compatibility-
access latch deliberately observed the two new root constructions and two
bounded projections before accepting the new machine baseline. The durable-
owner latch separately observed both raw-to-root field conversions and the
replacement of a directly stored `EvaluationFailure` with its inventoried
failure-root shell before accepting its new declaration fingerprint. Branch,
execution, and captured-control interiors remain assigned to I4F.1d.3c-e.
Production remains `NoAuto`.

I4F.1d.3c.1 completed 2026-09-03. `Branch` now retains its current effect and
local state as explicit `RuntimeValueRoot`s, so cloning a retry candidate also
clones stable roots. All branch replacement flows pass through same-runtime
root setters or directly retain a root produced by the current evaluator
step. Fused effect and state changes therefore no longer need parallel roots
in `PreparedDrive`; only raw continuation values awaiting I4F.1d.3d remain in
that bounded phase bundle.

The branch record and retry checkpoint have compile-exhaustive ownership
latches. A deterministic effect-token fixture verifies that effect and state
payloads remain live exactly through branch retirement. Work, branch-outcome,
and cut payloads remain assigned to I4F.1d.3c.2. Production remains `NoAuto`.

I4F.1d.3c.2 completed 2026-09-03. Deliver and application work now retain
their value, function, and argument payloads as exact `RuntimeValueRoot`s;
successful branch outcomes retain the same root shape through isolated-search
or terminal publication. A cut roots its operation on entry, and both its
alternatives and retry work delegate to the now-rooted branch/work graph. Raw
values are projected only at the beginning of a bounded evaluator step and
are rooted again at the single work/outcome constructors before parking.

Compile-exhaustive latches cover task execution, every work and outcome
variant, cut fields, and wake actions. A deterministic effect-token fixture
verifies exact retention and retirement for delivery, application arguments,
successful outcomes, and cut operations. Both source inventories were first
allowed to report the expected mismatch, then relatched: the five durable raw
fields became five runtime roots, while the compatibility-access row records
only their bounded projections and construction gateways. Captured-control
and fixpoint payloads remain assigned to I4F.1d.3d. Production remains
`NoAuto`.

I4F.1d.3d.1 completed 2026-09-03. Glam, assertion, scoped-close, and
scoped-result continuations now retain exact runtime roots. Restore delimiters
root their encoded reset stacks, reset frames root captured continuation
functions, and `CapturedContinuation` therefore composes an entirely
root-shaped control graph. Parsing an encoded reset stack roots each decoded
continuation before the bounded evaluator scope ends; serialization projects
it only while constructing the replacement semantic value.

A compile-exhaustive latch covers `Control`, every continuation and delimiter
variant, captured continuations, reset frames, and the temporary captured-layer
ordering enum. A deterministic effect-token fixture verifies that every
value-bearing control variant retains its payload exactly until the complete
captured graph is retired. Both source inventories were deliberately tripped
and relatched: six durable raw fields became six runtime roots. Fix roots and
the redundant fused phase-root bundle remain assigned to I4F.1d.3d.2 and
I4F.1d.3e respectively. Production remains `NoAuto`.

I4F.1d.3d.2 completed 2026-09-03. A shared `FixRoot` now retains its function
as a `RuntimeValueRoot`, and both initial and restarted fixpoint application
move that exact root into machine work instead of projecting and rerooting it.
Active and deferred restart frames continue to share the `Arc<FixRoot>`;
their promise handle is deliberately unchanged because promise assignment and
producer lifecycle remain an I5-managed contract.

The fixpoint records and choice enum have a compile-exhaustive latch. A
deterministic fixture drops the originating root, then the active frame, then
the restart frame and verifies that the function payload retires only with the
last shared fix root. The source inventories were tripped before accepting the
one raw-to-root field conversion and removal of the now-unnecessary owned
projection. This closes the captured-control/fixpoint graph; the decoded-
request phase bundle remains assigned to I4F.1d.3e. Production remains
`NoAuto`.

I4F.1d.3e completed 2026-09-03. The rooted `Request` is now the sole handoff
from bounded request decoding into the interpreter. Rooted sequence,
alternative, cut, reset, shift, scoped-operation, and fixpoint values transfer
directly into their destination owners rather than projecting and rerooting.
The fusion-only raw `Request` and `FusedRequestAction` remain confined to one
`EvaluatorStepContext`; once a request crosses that scope, every value is
root-shaped.

Rooting continuation payloads at their assignment gateway made the old
parallel `FusionState::pending_sequence_values` and
`PreparedDrive::_phase_roots` bundle redundant. Both have been removed;
`PreparedDrive` now contains only a rooted request or a value-free continue
signal. A compile-exhaustive latch covers both request representations, every
fused action and prepared result, specialized request descriptors, volume
request identities, and every `MachineStep` variant. Existing deterministic
park/resume, callback-boundary, cut/fix/reset/shift, isolated-search, and
terminal-retirement fixtures pass without a phase-root crutch.

The two source inventories were deliberately tripped before accepting the
removal of two obsolete durable declarations, two redundant root fields, one
bounded raw field, and seven owned compatibility projections. This closes the
reflection machine and the complete I4F.1d store/protocol/machine owner row as
compatibility roots plus explicitly bounded raw evaluator values. I4F.2 will
register and replace those compatibility roots. Production remains `NoAuto`.

#### Phase I4F.1e — Diagnostic, Event, and Delivery Roots

- Reconcile diagnostics and context frames, diagnostic buses/ingress,
  admitted runtime inputs, output intents, queued/running deliveries, retained
  delivery failures, event snapshots/journals, and callback-owned payloads.
- Keep authoritative runtime buffers root-shaped. Input conversion occurs
  before admission; output decoding and user callbacks receive a retained
  public root and execute after locks, as established by the work-boundary
  plan.
- Preserve weak endpoint/ingress back-references and exact delivery ownership;
  root conversion must not introduce a bus/runtime or callback/runtime cycle.

Verification: input admission/consumption, output claim/callback/terminalization,
delivery failure retention, diagnostic subscription, and fallback-rendering
fixtures, including forced callback/retirement orderings. Construct, publish,
and retire each owner without collection.

I4F.1e completed 2026-09-03 as a closure audit of the root conversion already
established by the runtime-event and diagnostic-bus work. Authoritative input
records, output intents, queued/running delivery records, prepared input, and
delivery tickets retain `RuntimeValueRoot`. Diagnostic emissions and origins
retain the public root facade; events share their diagnostic envelope through
`Arc`. Failure snapshots retain `Error`/failure roots without reconstructing a
value. Input conversion occurs before admission, while output decode and
adapter callbacks receive a retained public value after locks and mutation
admission have been released. Endpoint, ingress, and bus back-references stay
weak, so the audit introduced no runtime cycle.

The existing forced-order fixtures cover snapshot persistence, queued and
running delivery retention, callback terminalization, decode/adapter/panic
failure publication, diagnostic fallback transfer, logger close, and runtime
retirement. Two deterministic additions close the semantic-value lifetime
edge directly: a diagnostic event retains both emission and origin tokens
until the event retires, and a consumed input retains its token through commit
until the returned public value is dropped. The durable-owner and
compatibility-access inventories do not change because the production fields
were already root-shaped. Production remains `NoAuto`.

#### Phase I4F.1f — Assembly, Compiler, Macro, Configuration, and Logger Roots

Split source-tooling and binary-host ownership so neither is hidden behind a
single compiler checkpoint:

1. **I4F.1f.1 — Assembly, compiler, module, and import.** Reconcile
   assembler/module state, compile setup/results, origins, import demands,
   recursive definitions, loader handoff, and published modules. Verify normal
   build/import paths, content-stability, callback-without-mutator behavior,
   construction-scope exit, and retirement.
2. **I4F.1f.2 — Macro and parser rewrite state.** Reconcile macro
   inputs/outputs/failures, embedded source data, staged parser journals, and
   rewrite continuations which cross macro evaluation. Verify successful,
   failed, nested, and layout expansion plus retirement. Prefer complete
   rooted bundles for reusable macro/compiler objects.
3. **I4F.1f.3 — Binary configuration and logger.** Extend the inventory beyond
   the library crate in three independently checked closures:
   - **I4F.1f.3a — Command line.** Reconcile configured-search snapshots and
     journals, token-parser runs, completion evidence/results, case
     explanations, and CLI errors. Verify parse/completion behavior and exact
     retirement of retained evidence values.
   - **I4F.1f.3b — Configuration.** Reconcile prepared assemblies, loaded
     configuration/environment values, resolver-owned argument promises, and
     preparation failures. Verify configured execution, resolve/fail paths,
     construction-scope exit, and owner retirement.
   - **I4F.1f.3c — Logger.** Reconcile logger task hosts, supervision and
     installation state, settled-report selection, default-renderer state,
     callback-owned diagnostic/binary records, and `LoggerRun`. Verify logger
     quiescence/close, fallback diagnostics, callbacks outside managed access,
     forced lifecycle orderings, and owner retirement.

All three preserve I3's distinction between bounded compiler locals and
values parked across an import, macro demand, diagnostic callback, module
publication, or logging lifecycle. Only the latter become durable roots, and
none retains a factory/value-domain capability merely to reconstruct a root
later.

I4F.1f.1 completed 2026-09-03 as a closure audit. Assembly reflection
environments, compile setup, prior/final recursive definitions, opaque source
origins, deferred module-import arguments, loader results, declaration
lowering state, and completed module results were already retained through
`RuntimeValueRoot` or the public `Value` root facade. Source provenance is
edge-free. Diagnostic-emitter arguments remain bounded raw values during one
callback, and module/binary loader callbacks continue to execute without an
inherited evaluator mutator before returning a same-runtime root.

A compile-exhaustive compiler-owner latch covers every `BinaryLoadArgs`,
`ModuleLoadArgs`, and `CompileContext` field, including the exact three rooted
context surfaces. Deterministic effect-token fixtures verify that recursive
definition roots survive a deferred loader handoff exactly until its argument
record retires, and that a normally built module retains its published value
after all construction scopes exit exactly until the `BuiltModule` is
dropped. Existing import provenance, content-stability, callback-boundary,
normal module build/import, and recursive publication suites remain the
behavioral coverage. The durable-owner rows close without changing production
storage. Both lexical access inventories were deliberately tripped and
relatched for the test-only root constructors and projections used by the new
retirement fixture; their production surface is unchanged. Production remains
`NoAuto`.

I4F.1f.2 completed 2026-09-03 as a closure audit of the public-root macro
protocol. Embedded macro input data, evolving declaration-rewrite journals,
successful output data, retained case explanations, diagnostics, and failure
context already use the public `Value` facade. Raw semantic values occur only
after a bounded projection for lookup, result forcing, source reconstruction,
or diagnostic enrichment; no rewrite continuation retains them across a macro
demand or parser stage. Macro host/search state is separately closed by the
reflection-owner checkpoints.

Compile-exhaustive latches cover every `MacroRun`, `MacroFailure`,
`MacroInvocation`, `OriginalMacroInvocation`, and `DeclarationMacroWork`
field. Deterministic effect-token fixtures verify exact retirement for macro
inputs, rewrite-journal embedded data, successful output and visited-case
values, and failed-branch case values. The existing macro suite supplies
successful, failed, nested, right-to-left, embedded-data, and hanging/nested
layout behavior coverage. Both durable-owner rows close; lexical inventories
are deliberately relatched only for test-local root use. Production remains
`NoAuto`.

I4F.1f.3a completed 2026-09-03 as a closure audit of the configured
command-line pipeline. Search journals, token-parser successes, completion
evidence and case explanations, successful branches, CLI errors, and final
completion/search results retain semantic data only through the public
`Value` and `Diagnostic` facades. Token/path capabilities are admitted opaque
families whose payload maps contain no Glam values. Host arguments, cursor
positions, replacement text, and command edits are edge-free.

Compile-exhaustive latches cover configured-search snapshots and journals,
token runs, completion candidates/results/expectations, and explanation
evidence. Deterministic effect-token fixtures verify exact retirement of
active and visited cases, successful token values, and public case
explanations. The existing configured parse, ambiguity, completion,
transaction rollback, and token-reader suites remain the behavioral matrix.
The command-line durable-owner row closes without a production representation
change. Production remains `NoAuto`.

I4F.1f.3b completed 2026-09-03 as a closure audit of batch configuration
ownership. `PreparedAssembly` owns the runtime, assembler, logger host, loaded
configuration, and unresolved process/reflection argument resolvers as one
lifecycle bundle. `LoadedConfiguration` retains its module and selected
environment through public roots. Resolver-owned promises remain the existing
external promise lifecycle rather than hiding a value in configuration state;
`resolve_environment` and `fail_environment` consume each resolver once.
Preparation failures retain their assembler/logger context and structured
`Error` until the batch host renders them.

A compile-exhaustive latch covers every prepared, failed-preparation, and
loaded-configuration field. A deterministic effect-token fixture constructs a
complete prepared owner and verifies that its loaded module/environment values
survive construction-scope exit and retire exactly with that owner. Existing
configured batch, environment resolve/fail, and assembly execution tests remain
the behavioral coverage. The configuration durable-owner row closes without a
production representation change. Production remains `NoAuto`.

I4F.1f.3c completed 2026-09-03 as a closure audit of logger and rendering
ownership, completing I4F.1f. Logger task hosts retain their reflection
environment through the public root facade; event journals, diagnostic
transports, output deliveries, failure reports, and effect lifecycles delegate
semantic retention to their already-inventoried runtime/reflection owners.
`DefaultLogger` retains its formatter as a public root alongside its assembler.
Supervisor callbacks receive rooted diagnostics or decoded bytes only after
runtime locks and managed-access regions have ended. All bus/ingress and
endpoint backedges remain weak, preserving the established acyclic teardown.

Compile-exhaustive latches cover the logger task host/journal, log host,
supervisor state, report selection, installation, `LoggerRun`, and default
renderer. Deterministic effect-token fixtures verify exact retirement of the
logger reflection environment and formatter. Existing forced-order logger
close, rearm, output isolation, fallback transfer/failure, settlement,
callback-boundary, rendering, and teardown fixtures remain the behavioral
matrix. The logger durable-owner row closes without a production
representation change. With command-line and configuration owners already
closed, I4F.1f is complete.

The full checkpoint suite also exposed a pre-existing test-ordering gap in
`runtime_input_admission_wakes_after_releasing_mutation_admission`. An observer
which begins late may see the already-advanced authoritative epoch before the
publisher releases mutation admission; that is a valid recheck, not the
parked-wakeup ordering the test intended to prove. Test-only observation-wait
instrumentation now latches entry into the condition-variable wait while its
epoch mutex is held. The fixture admits input only after that latch, forcing
the notification path and proving it runs after admission release without
depending on scheduler luck. No runtime semantics changed. Production remains
`NoAuto`.

#### Phase I4F.1g — Net, Hidden-Owner, and Gate Closure

Partition this final gate into two independently reviewable checkpoints:

1. **I4F.1g.1 — Synchronized-net ownership.** Compile-latch the durable core
   net facade, source observations, contention handoffs, function/operator
   payloads, and the evaluator's bounded driver descriptors. Verify that a
   shared net retains semantic payloads until its last owner retires. Driver
   work and live claims remain stack-bounded and reconstructible; only the
   shared net contains authoritative parked state.
2. **I4F.1g.2 — Hidden-owner and gate closure.** Re-run the repository-wide
   closure, opaque, callback, and type-erasure audits; close every previously
   completed inventory row; require the inventory to contain no open migration
   before freezing the I4F.2 baseline.

- Reconcile synchronized core-net/work records and every durable net handle
  held outside an already inventoried value root. A transient active-pair or
  cursor claim remains bounded; a parked driver, function stage, source-net
  identity, or host work record needs a stable root-shaped anchor which I4F.2c
  will register and whose I4E logical visitor reaches its retained value/net
  payloads until I8 replaces the outer representation.
- Re-run the closure, callback, opaque, and type-erasure searches after the
  preceding conversions. Unknown `Any`/closure/generic payloads are rejected;
  root-bearing external families require a named stable record, while managed
  backedges hidden behind public roots remain forbidden.
- Close every row in the owner inventory and freeze the I4F.2 input baseline.
  No residual owner may be deferred to I9 merely because production collection
  is disabled.

Verification: net/work park-and-retire fixtures, hidden-owner negative
fixtures, the complete `durable_value_owner_inventory_is_complete` baseline,
and one table-driven non-collecting publish/retain/retire pass over every
registered owner fixture. I4F.1 completes only when the inventory has no open
row.

I4F.1g.1 completed 2026-09-03. Compile-exhaustive latches now cover the
durable core-net facade, prepared copy source, contention and frontier
handles, every `CoreOperator` payload variant, and all evaluator driver/work
descriptors. The audit confirmed that `NetDriver`, its worklist, and call and
operator claims are bounded evaluator state: they are consumed or rebuilt
within one drive operation and never become a durable parked owner.
Authoritative blocked calls, cursor dependencies, stuck reasons, copied source
nets, and semantic node payloads remain in the synchronized runtime net.
A deterministic lifetime fixture proves that this shared owner retains a
function/operator payload after construction scope and releases it with the
last net owner. Existing forced park, retry, unwind, contention, and
normalization-close fixtures remain the behavioral verification. No
production representation or semantics changed.

I4F.1g.2 completed 2026-09-03, closing I4F.1. The final source-backed audit
reverified the complete declaration baseline together with the dedicated
closure/opaque-constructor, opaque-family, runtime-cache-family, managed-family,
and bounded-access inventories. These gates reject an unassigned durable
declaration, an unreviewed `Any` or cache family, an unrestricted closure
capture, a hidden managed pointer, or an authority-bearing net escape. The
four rows whose implementation checkpoints had completed but whose migration
labels remained stale (reflection machine, diagnostics, events, and core net)
are now closed. The migration-state variant and open-row macro were removed,
making a newly open owner an explicit inventory design change rather than a
string annotation. Existing subsystem-specific construction, publication,
retention, and retirement fixtures remain the non-collecting owner matrix;
I4F.2e will promote those same real owners to isolated collection fixtures
after the production root switch. Production remains `NoAuto`.

### Phase I4F.2 — Public-Root Production Switch

After I4F.1 closes every durable owner, enact I2's selected opaque
inline-or-managed public handle, heap-derived provenance, registered
`RuntimeValueRoot`, and I4's managed value shell. The original ordering tried
to admit the current compatibility `core::Value` as a passive managed family
before I5-I10. That assertion is false: a value may transitively own a lazy
reflection reservation, an arbitrary host-call closure environment, or an
external opaque payload whose destruction performs lifecycle work. Collection
would then run runtime-affecting destruction from the GC finalizer path,
contradicting I4.0's safety contract.

I4F.2 therefore first retires authority-free compatibility access, then pulls
the lifecycle-only portions of I6D/I10 forward into an explicit active-owner
extraction gate. This is dependency reordering, not early representation
migration: recursive lazy, promise, collection, function, metadata, failure,
and net identities still move in I5-I8. Once the complete compatibility value
has passive transitive destruction, private shell preparation and the atomic
root switch can proceed honestly. No checkpoint weakens `ManagedFamily` or
permits a runtime callback from GC finalization.

#### Phase I4F.2a — Compatibility Access Retirement

Retire operations which would become unsafe or architecturally false before
the atomic switch. This work uses the current representation and therefore can
be partitioned by subsystem:

1. **I4F.2a.1 — Public facade, assembly, diagnostics, and errors.** Partition
   this surface into three buildable slices:

   - **I4F.2a.1a — Handle traits and rendering.** Remove
   handle-derived equality, ordering, hashing, kind, runtime identity, and
   content rendering from public `Value`; make optional `Debug` content-free.
   Crate-private compatibility authority/projections may remain until the
   subsystem slices which consume them, but no public trait may expose
   representation-derived semantics.
   - **I4F.2a.1b — Assembly, diagnostics, and errors.**
   Route comparison, observation, WHNF extraction, and owned host conversion
   through matching runtime authority. Keep `EvaluatedValue` as the same root
   plus its weak observer and static WHNF witness.
   - **I4F.2a.1c — Public facade closure.** Update compile-time and public API
     fixtures to prove the removed traits/observers stay unavailable while
     evaluator and reflection capabilities retain the intended observations.
     Retire the test-only equality bridge and migrate equality fixtures to
     explicit runtime-authorized representation inspection (I4F.2a.1c.1),
     then retire test-only public-handle observers and constructors in favor
     of evaluator, reflection, or scoped internal construction
     (I4F.2a.1c.2). This partition is verification-only; neither substep adds
     production behavior.
2. **I4F.2a.2 — Core evaluation and net construction.** Replace unrestricted
   borrowed `as_core` projections with lifetime-bounded access and replace
   ownership-taking `into_core` with either a root-preserving move or an
   explicit clone performed inside matching runtime access. Execute this as
   four independently verified slices:

   - **I4F.2a.2a — Public evaluator and inspector.** Route WHNF demand,
     representation inspection, metadata inspection, dictionary iteration,
     and atom-key extraction through the matching `Values` scope. No public
     evaluator helper obtains a bare core value through a compatibility method
     on the public handle; the existing evaluator entry may still accept an
     owned scoped clone until the production root switch.
   - **I4F.2a.2b — Evaluation driver and completion projection.** Move rooted
     wait, demand, spark, reflection-activation, and effect-entry projections
     through `EvaluatorStepContext` or another already admitted poll scope.
     The root remains owned across orchestration boundaries; only one scoped
     clone enters the evaluator step.
   - **I4F.2a.2c — Net and builtin adapters.** Replace public/core conversion
     in interaction-net construction and builtin request helpers with bounded
     construction or evaluator access. Preserve all callback and parking
     boundaries established by I3.
   - **I4F.2a.2d — Subsystem closure.** Relatch the evaluator/net inventories,
     remove superseded compatibility adapters, and prove no owned or borrowed
     bare core value crosses the covered orchestration boundaries.
3. **I4F.2a.3 — Reflection, compiler, macro, and host integration.** Apply the
   same rule to reflection machines/requests/stores, compiler/module lowering,
   macro replay, configuration, logging, and runtime event adapters. A callback
   receives a root or owned host data, never a borrowed core representation.
   Execute this broad integration surface as eight source-inventoried slices:

   - **I4F.2a.3a — Reflection protocol, store, and search.** Replace public
   handle projections and constructors in the common transaction, volume,
   and isolated-search substrate with matching `Values`, evaluator-step, or
   retained-root operations.
   - **I4F.2a.3b — Reflection requests.** Migrate standard request decoding and
     response construction without broadening the public request API.
   - **I4F.2a.3c — Reflection machine.** Migrate the central interpreter and
     its parked state. Callback entry must retain the I3 mutator-free boundary.
     Partition the large machine module into branch/request-step access
     (I4F.2a.3c.1), transition/terminal publication (I4F.2a.3c.2), and public
     wrappers plus in-file fixtures and final source closure (I4F.2a.3c.3).
   - **I4F.2a.3d — Compiler and module lowering.** Migrate definition promises,
     origins, import handoff, and declaration lowering through the compiler's
     existing bounded access regions.
   - **I4F.2a.3e — Compiler caches.** Migrate runtime-cached compiler helpers
     and the diagnostic formatter without adding cache-local authority.
   - **I4F.2a.3f — Macro effects and runner.** Migrate rooted macro inputs,
     outputs, comparisons, and diagnostic values. Expansion callbacks retain
     public roots or owned source data, never a borrowed semantic value.
   - **I4F.2a.3g — Parser replay and source.** Migrate embedded macro data,
     source diagnostics, and conditional-definition projections through the
     parser/compiler's selected runtime authority.
   - **I4F.2a.3h — Lifecycle and host closure.** Retire remaining reflection
     lifecycle ownership transfers and audit configuration, logging, and
     event adapters for hidden compatibility access. Relatch both public-value
     and compiler-boundary inventories.
4. **I4F.2a.4 — Access closure.** Relatch the production access inventory with
   only the private managed-root gateway and explicitly lifetime-bounded core
   adapters remaining. Add negative source/compile-time fixtures for an owned
   core escape, unscoped borrowed projection, direct public semantic trait, or
   handle-derived provenance query.

Each slice must preserve the corresponding semantic operation through its
runtime-authorized replacement and update compatibility-oracle tests when an
intentionally removed public trait or observer no longer compiles. The work is
call-site migration, not permission to publish a managed value.

I4F.2a.3c.1 completed 2026-09-03. Reflection request interpretation now
projects effect arguments, state, paths, reset keys, continuation state, and
exit messages only inside the already admitted evaluator step. Transactional
heap and volume snapshots cross the host/store boundary as public roots and
are cloned through the matching internal `Values` service after callbacks and
locks have returned. No managed access is retained across host snapshots,
commits, waits, or specialization callbacks. The reflection-machine inventory
fell from 26 direct borrowed projections and 16 ownership transfers to 10 and
10; the remaining sites are explicitly partitioned between transition and
terminal handling (I4F.2a.3c.2) and wrapper/helper/fixture closure
(I4F.2a.3c.3).

I4F.2a.3c.2 completed 2026-09-03. Delivery, continuation, reset, cut, isolated
search, and terminal transitions now retain `RuntimeValueRoot` throughout the
machine state and project only within a bounded `EvaluationPollContext`
evaluation step. Specialized alternatives clone their public roots through
the matching `Values` authority after the specialization callback returns;
close-scope and reset continuations move roots directly rather than
round-tripping through bare core values. This removed every direct ownership
transfer from the reflection machine and retired the isolated-search unit
constructor compatibility path. The remaining eight borrowed projections and
one public constructor are local wrapper/helper or fixture work assigned to
I4F.2a.3c.3.

I4F.2a.3c.3 and I4F.2a.3c completed 2026-09-03. Effect dispatch now projects
its rooted API through the active evaluator step; contextual and unit task
wrappers inspect completions only through the matching poll authority while
publishing the original root unchanged. Branch helpers now require either an
evaluator step for projection or the matching core factory for root creation,
so they cannot infer authority from a handle's representation. Reset-frame
encoding follows the same scoped rule, and volume-effect construction uses
the internal `Values` service. The root-preservation source fixture was
updated to latch the consolidated unit-result path. The reflection-machine
inventory is now zero for direct borrowed projection, ownership transfer,
public construction, and provenance-only root construction; its seven explicit
`RuntimeValueRoot::new` sites are the intended private root-publication
gateway for this transitional representation.

I4F.2a.3d completed 2026-09-03. `CompileContext` no longer lends borrowed core
references from its prior/final-definition or origin roots; it returns an
owned clone made inside matching runtime access. Module lowering clones its
definition and reflection roots through the access region it already opens
for each declaration, and roots the completed definition set only after that
region closes. Import handoff continues to move durable roots unchanged.
Compiler and module-lowering compatibility projections and ownership
transfers are now both zero in the source inventory.

I4F.2a.3e completed 2026-09-03. The runtime-owned g compiler cache and default
diagnostic-formatter cache now project their retained roots through the
`RuntimeValueAccess` already admitted by their cache read helpers. Neither
cache creates independent authority or borrows through a root compatibility
accessor, and both compiler-boundary inventories now record zero direct
projections.

I4F.2a.3f completed 2026-09-03. Macro protocol constructors now publish
through `RequestContext::values`, and evaluated text is extracted through the
`EvaluatedValue` observer instead of borrowing its representation. The macro
runner wraps compiler-produced inputs through its runtime `Values`, clones
successful and diagnostic-case roots through that same authority, and never
borrows a branch root across search or evaluation. The last temporary
`Value::same_core_with` compiler hook was removed; case deduplication compares
owned scoped clones. Macro effect and runner compatibility counts, plus the
compiler-boundary projection count for the runner, are now zero.

I4F.2a.3g completed 2026-09-03. Logical macro replay now rehydrates embedded
data with the parser runtime's `Values` service, and source diagnostics clone
public emissions, cases, and expanded embedded data through the same bounded
service. Number, text, and embedded macro inputs are constructed through
`Values::wrap`. This removes the parser's ad hoc outer access regions along
with every direct public/core conversion; its retained `PublicValue` fields
remain the explicit durable boundary between macro expansion and source
replay.

I4F.2a.3h and I4F.2a.3 completed 2026-09-03. Direct and scheduled effect
lifecycle entry clone the selected effect and optional failure context through
the runtime `Values` service at the bounded construction point; deferred task
activation retains public roots rather than a bare core value. Source
compilation now returns its `RuntimeValueRoot` directly to assembly/module
handoff instead of immediately extracting and recreating it. Configuration,
logger, and event-adapter sources contain no compatibility conversions. The
public-value and compiler-boundary inventories are relatched; the sole direct
reflection-protocol projection remains the previously documented structured
`ApiError` conversion owned by I6C's managed failure shell, rather than hidden
host-integration debt.

I4F.2a.1a completed 2026-09-03. Production `Value` and `EvaluatedValue` no
longer implement semantic equality, and `Value::runtime_id` is no longer a
public observation. Both handles now render as opaque names rather than
revealing the current outer representation. Diagnostic, readiness,
reflection-lifecycle, and query wrappers stopped deriving equality merely
because they contain a value. The binary crate replaced its canonical
atom/unit and diagnostic de-duplication comparisons with matching-runtime
`ReflectionInspector::same_representation` or the observer carried by an
`EvaluatedValue`; this operation is explicitly representation inspection, not
Glam logical equality. A crate-test-only compatibility equality implementation
keeps the remaining subsystem fixtures buildable while I4F.2a.1b/a.2/a.3
migrate their assertions; I4F.2a.1c removes that oracle and proves the public
negative trait boundary.

I4F.2a.1b completed 2026-09-03. `Diagnostic::from_emission` and
`with_source_location` now require the matching `Values` authority and reject
a mismatched runtime rather than deriving authority from the handle. All
diagnostic enrichment, context, transport, and evaluator-error conversion use
scoped clones/wrapping through that authority; the diagnostic and error
modules retain no unrestricted core projection. Assembly reflection-
environment rewriting, promises, formatter construction, and net construction
likewise use a `Values` construction/access region. The only remaining
assembly projections belong to the already-rooted internal module-sealing
handoff assigned to I4F.2a.2, not the public facade.

I4F.2a.1c is intentionally verified after I4F.2a.2 and I4F.2a.3. The
crate-test-only equality oracle retained by I4F.2a.1a is solely a migration
bridge for internal assertions in those subsystems; it is absent from
production and does not define public semantics. Once the internal callers
have matching authority APIs, I4F.2a.1c removes the oracle and adds the final
negative public-facade fixtures.

I4F.2a.1c.1 completed 2026-09-03. `Value`, `EvaluatedValue`, and test-only
containers which transitively held them no longer gain equality during crate
tests. Assertions now state the actual contract: empty queues are empty,
lifecycle states have the expected variant, readiness records retain the same
work identities, and value representations compare only through matching
runtime authority. Compile-time negative trait fixtures latch the absence of
equality, ordering, and hashing on both public value handles.

I4F.2a.1c.2, I4F.2a.1c, and I4F.2a.1 completed 2026-09-03. The public handle's
crate-test-only scalar, binary, kind, undefined, and bare-core construction
shortcuts are gone. Public facade fixtures now demand through
`ValueEvaluator`, inspect through `ReflectionInspector`, or construct through
the matching `Values`; lower-level subsystem fixtures use the same scoped
internal service. Output-decoder tests now capture their runtime `Values`
authority explicitly instead of relying on a standalone `Value` observer.
The production inventory consequently records no `Value::from_core`,
`Value::from_runtime`, or direct `RuntimeValueRoot::from_runtime` path in the
facade.

I4F.2a.4 and I4F.2a completed 2026-09-03. The remaining generic owned-core
escape on `Value` was renamed and made private to affine promise resolution,
where I5 replaces it with the managed promise assignment. The borrowed
compatibility projection remains crate-private only for the bounded `Values`
adapter and the single source-latched structured-`ApiError` conversion assigned
to I6C. Exact production counts prevent either exception from spreading, while
a source fixture rejects public core/provenance/scalar observers and the
compile-time facade fixtures reject representation-derived semantic traits.

I4F.2a.2a completed 2026-09-03. `ValueEvaluator` and
`ReflectionInspector` no longer call compatibility projection or construction
methods on public values. Representation-kind and equality inspection borrow
through one matching `Values` scope; WHNF operations clone the input under
that authority, release it before evaluation, and wrap owned results through
the same runtime service. This preserves I3's rule against carrying managed
access through waits or callbacks. The compatibility-access inventory now
omits `src/api/evaluator.rs` entirely.

I4F.2a.2b completed 2026-09-03. Client demand and spark polls now project
their retained `RuntimeValueRoot` through the active `EvaluatorStepContext`
before entering the evaluator. Reflection activation projects its rooted
effect through a separate matching access region and releases that region
before invoking the launcher callback. `RuntimeValueRoot::clone_core_with`
requires admitted same-domain authority, making the remaining owned
compatibility clone explicit while preserving the root across every wait and
orchestration boundary. Production files under `src/evaluation/` now contain
no direct compatibility `as_core` or `into_core` use.

I4F.2a.2c completed 2026-09-03. Interaction-net effect construction now
wraps and clones public values through `Values`, while evaluated copy counts
and construction-port tokens are inspected through the weak observer carried
by `EvaluatedValue`. No net-specific projection API was introduced. Journaled
data remains an owned core clone only for the duration of the compatibility
representation; callbacks continue to demand arguments through
`RequestContext`, and the completed construction result retains its admitted
`EvaluatorStepContext`. The compatibility-access inventory now omits the net
construction module.

I4F.2a.2d completed 2026-09-03. The module-sealing handoff and runtime's
canonical core-value cache now clone retained roots only through matching
`RuntimeValueAccess`; production evaluator, poll, session, assembly handoff,
and net-construction sources retain no direct compatibility projection or
ownership-transfer calls. The two residual core-family exceptions remain
explicitly latched rather than hidden: test-only canonical-root assertions are
removed by I4F.2a.1c, while promise assignment ownership recovery and its
compatibility edge visitor are replaced by I5's managed promise representation.
The public compatibility-access and managed-admission inventories pass with
those assignments recorded.

I4F.2a.3a completed 2026-09-03. Effect constructors, request-context demand,
path evaluation, isolated-search entry, persistent-store query roots, and
transactional updates now clone or wrap public values through their matching
`Values` service. `TaskHalt::with_context` likewise requires that authority;
the CLI logger and internal callers supply it explicitly. Reflection store and
search sources retain no compatibility projection or construction call. The
one protocol exception is the structured `ApiError` to `TaskHalt` conversion:
the current diagnostic owns a public root but not a reusable value service, so
its emission projection remains source-latched to I6C's managed
diagnostic/failure boundary rather than gaining an authority-free workaround.

I4F.2a.3b completed 2026-09-03. Standard reflection request handlers now use
`RequestContext::values` for construction and owned cloning, and inspect
evaluated dictionaries, metadata carriers, task handles, messages, and
severity values through `EvaluatedValue`'s weak observer. Task-status encoding
clones coordinator result roots through the matching `Values` authority; query
decoding accepts the same authority. The in-module status and lifecycle tests
were migrated with the production code, leaving `src/reflection/requests.rs`
with no compatibility projection, transfer, or construction calls and no
inventory entry.

#### Phase I4F.2b — Active-Owner Extraction Gate

Make transitive destruction of the complete current `core::Value` passive
before placing it in a managed allocation:

1. **I4F.2b.0 — Destruction inventory.** Add a source-backed,
   compile-exhaustive inventory of every field reachable from `Value` whose
   destruction can invoke or retain a runtime, scheduler, host callback,
   external retirement action, or opaque destructor. Latch the negative
   `ManagedFamily` proof for compatibility `Value` until the inventory closes.
2. **I4F.2b.1 — External host calls.** Move callback objects and their rooted
   capture environments into a runtime-domain external-owner registry. A lazy
   source retains only a typed, edge-free handle. Dropping the last handle may
   enqueue retirement, but callback destruction and rooted-owner release occur
   only when the external registry is drained outside GC finalization. Preserve
   exactly-once invocation and current result/failure publication.
3. **I4F.2b.2 — Reflection reservations.** Move lazy reflection computation
   reservation state, including its active cancellation fallback, into the
   same external-owner boundary or a dedicated typed registry. The managed-
   reachable source retains only an edge-free handle; task reservation,
   activation, cancellation, and rooted effect/target values remain externally
   owned and keep their current ordering.
4. **I4F.2b.3 — Opaque payloads.** Separate the passive opaque value token from
   its type-erased payload owner. Payload lookup requires matching runtime
   authority. Last-token retirement is queued passively; arbitrary payload
   destruction and external capability retirement happen outside GC
   finalization. Edge-free payloads may use the same path for one explainable
   rule rather than a second destructor exception.
5. **I4F.2b.4 — Passive closure.** Re-run the complete transitive destruction,
   callback, opaque, and durable-owner inventories. Admit a closed test wrapper
   around every real `Value` variant as `ManagedFamily`, force rooted survival
   and unrooted reclamation, and prove that finalization invokes no runtime,
   scheduler, host callback, diagnostic/event service, or active external
   retirement.

I4F.2b.0 completed 2026-09-03. A compile-exhaustive direct `Value`/`LazySource`
classification and source-backed exceptional-field ledger identify exactly
three active destruction frontiers: deferred host-call closure environments,
unactivated reflection reservations whose drop cancels coordinator work, and
type-erased opaque payloads (including their transitive retirement handles).
Recursive value/failure storage, promises, and synchronized nets remain
semantic-edge work for I5-I8 but have passive destruction once these three
frontiers are extracted. Compatibility `core::Value` is now negatively
latched against `ManagedFamily` admission until I4F.2b.4 closes the inventory.

I4F.2b.1 completed 2026-09-03. Each deferred host call now registers its
callback owner in the matching runtime value domain and retains only a scalar
registry identity plus an ordinary lease token beneath `LazySource`. Cloning a
lazy source shares that lease. A known-safe host-call orchestration point first
detaches owners whose last lease has disappeared, then releases the lock before
destroying callbacks; lookup clones the current owner before invocation, so
neither destruction nor execution occurs under the registry lock or collector
finalization. Generic insertion and lookup deliberately do not retire unrelated
owners because they may later be called from a bounded evaluator scope.
Existing host-call result/failure publication is unchanged. A callback may be
retained until a safe registry drain or runtime-domain teardown, as permitted
by the gate's conservative-retention rule.

I4F.2b.2 completed 2026-09-03. A reflection lazy now retains only its external
owner handle and a passive completion discriminator. Its effect, optional gate
target, once-only reservation cell, task handle, and cancellation fallback are
held together in the runtime registry; effect and target are explicit
same-runtime roots. Reservation observes that owner through the matching value
domain, briefly projects the effect under managed access, then releases access
before coordinator reservation. Completion projects the rooted target through
the active evaluator step. Dropping the lazy lease alone cannot cancel work;
the latched ordering test proves that an unactivated reservation remains
registered until a known-safe external-owner drain and is discarded there,
preserving the prior uncommitted-reservation semantics.

I4F.2b.3 completed 2026-09-03. `OpaqueValue` now retains the same passive
external-owner handle as the other extracted families; its admitted
`Arc<dyn Any>` payload lives only in the runtime registry. Downcast requires a
matching `CoreValueFactory`, rejects another runtime before consulting its
local owner IDs, and clones the typed owner under the registry lock before any
client observation. This single rule covers edge-free provenance/construction
tokens as well as revocable effect tokens and reflection task handles whose
transitive drops retire external state. A dedicated fixture proves that last
token drop retains the payload inertly, cross-runtime lookup fails, and the
payload destructor runs only after a safe registry drain.

Conservative retention until the next ordinary registry drain is permitted;
retention until runtime-domain teardown is the fallback if no later operation
occurs. This can delay Rust resource destruction but must not change Glam
evaluation results, task terminal publication, or callback invocation count.
I5C, I6D, and I10 retain the later representation- and lifecycle-specific
audits, updated to recognize that this common external ownership boundary was
established here.

I4F.2b.4 and I4F.2b completed 2026-09-03. A test-only
`ClosedCompatibilityValue` is the first collector-admitted wrapper around the
real compatibility `Value`; its zero-edge trace is valid because all current
recursive ownership remains ordinary Rust ownership and the three active
destructors now live behind passive external-owner handles. A compile-
exhaustive thirteen-variant fixture proves rooted survival and unrooted
reclamation for every real value shell. Host-callback and opaque-payload drop
counters remain untouched during managed finalization and change only during
the subsequent safe registry drain. The reflection reservation fixture now
also places an unactivated reflection value in this managed wrapper and proves
that collection does not cancel the task; the external drain retains sole
responsibility for that active retirement. The complete containment, active-
owner, and durable-owner inventories pass at this gate. Compatibility `Value`
itself remains deliberately unadmitted; I4F.2c introduces the private
production node rather than making the old enum a collector allocation.

#### Phase I4F.2c — Private Production Root and Shell Preparation

Prepare the cutover without publishing a managed production value:

1. **I4F.2c.1 — Production node and family contract.** Introduce the private
   production-shaped managed node containing the now-passive compatibility
   value, its layout request, passive destruction record, and allocation
   gateway. It remains unreachable from normal production factories. Verify
   family admission, layout, drop behavior, and a source latch proving that
   only closed fixtures can construct it.
2. **I4F.2c.2 — Exact shell visitor composition.** Replace representative
   `ShellEdge` policy with compile-exhaustive real value-variant dispatch.
   Current compatibility payloads own no `Gc` edge, so their collector visitor
   reports zero managed edges while the I4B-I4E adapters continue to audit
   their logical Rust ownership. Each I5-I8 migration replaces the affected
   zero-edge arm with its exact managed visitor in the same checkpoint.
3. **I4F.2c.3 — Inline-or-root representation and provenance.** Turn I2's
   prototype into the private production root representation. Preserve an
   allocation-free inline arm where the I2 contract permits one; the managed
   arm reuses the collector root cell and derives ownership from the heap, not
   a parallel runtime-ID assertion. Rerun the prototype transport, authority,
   nested-access, cross-runtime, and dead-domain suite.

Production representation and behavior remain unchanged through all three
checkpoints.

I4F.2c.1 completed 2026-09-03. The private production-shaped
`ManagedValueNode` now contains one compatibility `Value`, requests the shared
Glam managed-slot layout, and carries the stable passive drop record admitted
by I4F.2b. Its `RuntimeValueAccess` gateway allocates and roots atomically from
the caller's perspective, so no bare pointer can escape its bounded mutator
region. The gateway remains absent from every production factory call site;
only the node-local closed fixture reaches it before I4F.2d. That fixture
proves matching-domain observation, rooted survival, unrooted reclamation, and
the expected family/layout contract. The durable-owner and managed-entry
inventories now name this prepared root surface explicitly.

I4F.2c.2 completed 2026-09-03. `ManagedValueNode::trace` now delegates to a
wildcard-free match over every real `Value` variant rather than inheriting the
representative `ShellEdge` fixture policy. Each current compatibility payload
owns only ordinary Rust edges, so every arm correctly reports zero collector
edges; I5-I8 must replace the affected arm at the exact checkpoint where a
payload first stores `Gc`. A shared thirteen-variant fixture allocates the
private production node for every shell, verifies matching-domain projection,
and proves collection marks exactly the registered roots with no hidden edge
or active external retirement. The older representative shell remains only as
I4A historical scaffolding and is deleted with the other compatibility
fixtures in I4F.2f.

I4F.2c.3 and I4F.2c completed 2026-09-03. The private
`PreparedRuntimeValueRoot` now applies I2's selected representation to the
real production managed node without changing `RuntimeValueRoot` or any
factory call site. Integral `Number` values representable as `i64` use the
allocation-free inline arm and carry only a weak value-domain observer; all
other current values use one registered `Root<ManagedValueNode>`. This is a
conservative inline policy, not the eventual tagged-word decision: atoms,
binaries, builtins, large integers, and ratios remain managed until the value-
representation plan reviews them.

Managed access validates the collector root's heap identity in all builds;
the managed arm carries no parallel runtime ID or second root cell. Inline
access validates the weak domain witness, and neither arm retains the value
domain. The promoted private suite proves allocation-free inline construction,
one root registration shared across clones and threads, same-runtime nested
projection, rejection by another runtime, inert handles after domain teardown,
opaque debug output, and absence of representation-derived equality,
ordering, or hash traits. Production remains `NoAuto` and retains its
compatibility root until the atomic I4F.2d switch.

#### Phase I4F.2d — Atomic Production Switch

Before the atomic representation change:

- **I4F.2d.0 — Post-domain canonical initialization.** Replace the canonical
  cache bundle's structurally early construction with a private `OnceLock`
  installed immediately after the value domain and its managed heap exist.
  Build the complete bundle through the finished factory and keep every
  observer behind an accessor which requires successful initialization. This
  is a behavior-neutral construction-order repair: compatibility roots and
  `NoAuto` collection remain in effect, and concurrent clients can never see a
  partial bundle.
- **I4F.2d.1 — Weak terminal publication authority.** Give promise cells and
  the runtime work coordinator the value domain's existing weak observer,
  replacing numeric runtime identity as the only information available where
  terminal values and failures are rooted. The observer must neither retain
  the domain nor authorize direct observation; it may only be upgraded at the
  later bounded publication operation. Preserve numeric IDs as diagnostics.
- **I4F.2d.2 — Atomic representation switch.** Perform the previously named
  non-partitionable `RuntimeValueRoot` cutover after both construction-order
  prerequisites are complete.

I4F.2d.0 completed 2026-09-03. `RuntimeValueCache` now holds its canonical
bundle in a private `OnceLock`. `CoreValueFactory::new` first constructs the
complete value domain and managed heap, then builds and installs all seven
canonical roots through that finished factory before returning it. Every
canonical observer goes through `core_values()`, which rejects an impossible
uninitialized domain rather than exposing the cell. Tests retain the prior
one-bundle, scoped-factory sharing, and last-domain-owner release behavior.
The roots themselves remain compatibility roots and collection remains
`NoAuto`; this checkpoint changes construction order only.

I4F.2d.1 completed 2026-09-03. Promise cells and the runtime work coordinator
now retain the value domain's existing `RuntimeValueObserver` rather than
depending on a numeric runtime ID as their only future publication context.
The observer remains weak and non-authoritative for projection: it can be
upgraded only while the domain still exists, and I4F.2d.2 will use the
resulting bounded factory/access region to register terminal values and direct
failure values. Runtime IDs remain available through the observer for
diagnostics and coordination identity. Dedicated lifetime fixtures keep a
promise or coordinator alive after dropping the last factory and prove that
neither weak publication route retains or revives the value domain.

- In I4F.2d.2's one buildable checkpoint, change `RuntimeValueRoot` to the selected
  inline-or-registered-root representation, enable the production managed
  value node, and route every core/public factory and root-preserving
  transition through that representation.
- Switch canonical/cache initialization and all I4F.1 durable owner
  constructors at the same boundary. A clone shares the existing root cell;
  neither an inline witness nor a managed root retains its value domain.
- Make matching-runtime scoped access the sole typed projection gateway and
  update every affected stable family/root record in this same checkpoint.

This is the one intentionally non-partitionable edit. No commit or accepted
test configuration may combine a production value containing `Gc` with the
old non-registering root, manufacture a new root from an unproven bare value,
or let a managed projection outlive its access scope. If this checkpoint is
still operationally large after I4F.2a/b, stop and repartition its preparatory
work rather than creating an unsafe intermediate representation.

Verification: promote the I2 prototype fixtures to production
`public_value_*` tests, including transport, weak-domain retirement,
cross-runtime rejection, owned extraction, nested access, and compile-time
no-equality/no-hash. Run the public value/factory and I4 exact-dispatch suites.
Production remains `NoAuto`, and no whole production graph is collected.

I4F.2d.2 and I4F.2d completed 2026-09-03. `RuntimeValueRoot` now contains the
private production inline-or-root representation: integral values representable
as `i64` remain allocation-free, while every other current value occupies one
registered `ManagedValueNode`. Cloning a runtime or public value shares that
root cell. Numeric runtime IDs remain diagnostic and coordination provenance;
projection requires the live weak value-domain observer and, for managed
values, collector ownership by the exact heap.

All promise, coordinator, failure, diagnostic, evaluator, compiler, reflection,
and public-value publication paths now construct roots through a live
`CoreValueFactory`; the old borrowed `as_core` and consuming `into_core`
surfaces have no production or test call sites. Public values retain no
representation equality, ordering, or hashing. Internal rooted status records
compare only inline scalar or shared root-cell identity where their containing
state requires `Eq`; this is not Glam value equality.

Existing ownership fixtures now explicitly collect after retiring their last
root. A test-only settle helper repeats collection and external-owner draining
because compatibility payloads can unregister a nested root when an outer
shell is finalized. I5-I8 replace those Rust-owned nested relationships with
exact managed edges; the exhaustive owner-by-owner survival/reclamation matrix
remains I4F.2e. Production collection policy remains `NoAuto`, and this phase
does not authorize collection of an arbitrary production graph.

#### Phase I4F.2e — Closed Durable-Owner Collection Matrix

Reuse the exhaustive real-owner fixture table registered by I4F.1. For each
independently retiring owner class, construct and publish an owner carrying
only an I4-certified closed value graph, leave its construction scope, request
full collection, and prove the retained value survives. Retire/drop that
owner, collect again, and prove its root registration and otherwise
unreachable value are reclaimed. Runtime-domain anchors are the deliberate
exception: canonical values and runtime-wide extension/compiler caches live
for the whole value domain, so their fixtures prove survival across collection
and release during terminal heap/domain teardown rather than inventing a
mid-domain removal API. A scoped factory's extension map is only a lookup
mirror of the runtime cache and is not a second owner-retirement boundary.
Use actual production owner types and constructor/retirement paths; a generic
holder is not a substitute for a difficult owner class.

Partition the matrix along the I4F.1 ownership boundaries:

1. **I4F.2e.1 — Runtime owners.** Cover these in three buildable checkpoints:
   **I4F.2e.1a** covers the production/public root, canonical bundle,
   runtime-wide extension and compiler caches, and external active-owner
   registry; **I4F.2e.1b.1** covers coordinator task, wait, and blocked-failure
   records; **I4F.2e.1b.2** covers client-demand operation and result cells;
   **I4F.2e.1b.3** covers queued/claimed sparks and pending session-owned
   reflection activations; together they complete **I4F.2e.1b**. Session and
   notification companions which carry only IDs, weak routes, or value-domain
   factories add no independent managed root. **I4F.2e.1c** covers failure
   ledgers plus
   quiescence, deadlock, unfinished-task, and settled readiness reports.
2. **I4F.2e.2 — Reflection owners.** Cover store/snapshot/query state,
   lifecycle/search/protocol records, and parked machine frames.
3. **I4F.2e.3 — Transport owners.** Cover diagnostics, ingress/subscriptions,
   event inputs, output intents, running deliveries, and retained failures.
4. **I4F.2e.4 — Tooling and hidden owners.** Cover assembly/compiler/import,
   macro/parser rewrite, binary configuration/logger state, synchronized
   net/work anchors, and every admitted type-erased/callback family.

I4F.2e.1a completed 2026-09-03. Production public values now have a focused
collection fixture proving that an inline integer adds no root, a managed
dictionary adds exactly one root shared by all clones, the last-clone drop
permits reclamation, and the runtime remains `NoAuto`. The canonical bundle
likewise accounts for its seven permanent roots and remains projectable after
collection. A real type-erased runtime cache entry retains a managed binary
root across collection and after its caller drops the returned `Arc`, then the
entry and its payload retire with terminal value-domain teardown; the plan no
longer pretends a scoped lookup mirror is an independent retirement boundary.

The real compiler-helper and diagnostic-formatter caches are constructed,
collected, and re-observed through their production accessors. The previously
established external active-owner fixture continues to prove explicit owner
retirement, managed-shell reclamation, and separation from collector
finalization. Together with the I4F.2d weak-domain teardown fixtures, these
cover all domain-anchor/cache rows assigned to I4F.2e.1a without creating an
artificial cache-removal API.

I4F.2e.1b.1 completed 2026-09-03. A terminal task now returns a managed binary
through the production poll, coordinator-publication, and wait-token path;
collection after machine retirement proves that the terminal wait retains the
single shared root, while dropping the task handle permits its shell to be
reclaimed. A separate task publishes a structured blocked failure containing a
managed binary. Collection proves that the coordinator's parked task record
retains that root, and explicit cancellation retires the record and permits
reclamation while leaving the value-free cancelled wait observable. These
fixtures cover task, wait, and blocked-failure records without involving the
failure ledger assigned to I4F.2e.1c.

I4F.2e.1b.2 completed 2026-09-03. One production client demand is collected
while still queued, proving that its coordinator-owned operation retains the
single managed input root; explicit handle abandonment retires the work and
reclaims the shell. A second demand is driven through evaluation and work
retirement, then collected while only its external result cell/handle remains.
The terminal result stays projectable and owns the same single managed root
until the handle is dropped, after which collection reclaims it. The fixture
therefore distinguishes the operation and result-cell lifetimes instead of
allowing one to mask a missing root in the other.

I4F.2e.1b.3 completed 2026-09-03. A production spark carries one existing
metadata value through queued, detached-claim, and blocked coordinator states;
collection at every state observes the same single additional root, and
quiescent abandonment releases it. The blocked-state fixture uses a separate
value-free task wait so the dependency cannot accidentally provide another
root. Pending `.task.new` and annotation-reflection reservations are exercised
separately with managed binary effects: each survives collection before
activation and is reclaimed when dropping the reservation cancels the real
coordinator work. `EvaluationSession`, `EvaluationDemandState`, IDs, and weak
notification routes carry no additional semantic value of their own; their
factory/domain ownership was already covered by I4F.2e.1a. This completes the
runtime-transient I4F.2e.1b matrix without activating reflection work assigned
to I4F.2e.2.

Keep every fixture in a fresh isolated collector-ready runtime/heap. Do not
collect a shared production graph containing I5-I10 unclassified families.
The common table-driven fixture is
`durable_root_surfaces_survive_collection_after_construction_scope`; focused
failures identify the owner row which retained too little or failed to release.

#### Phase I4F.2f — Compatibility Deletion and I4 Reconciliation

- Delete the obsolete I2 prototype, representative I4 shell scaffolding, old
  non-registering root constructors, unrestricted `as_core`/`into_core`
  methods, and compatibility traits/adapters which are no longer required by a
  named I5-I8 transition.
- Convert the compatibility access inventory into a forbidden-escape
  inventory. Reconcile the ownership ledger, public-value inventory, family
  records, and every I4 completion statement with the implemented production
  types.
- Add `public_value_switch_inventory_has_no_compatibility_escape` and rerun the
  complete repository checks. Then perform the mandatory post-I4 review before
  beginning I5.

Production remains `NoAuto`; the switch and closed owner fixtures do not
authorize whole-graph collection while I5-I10 families remain unclassified.

From I4F.2 onward, every representation change in I5-I10 updates its exact
visitor or root classification in the same checkpoint. No later phase may
perform the first root conversion for a durable owner which could already
contain a managed edge. Collection being disabled is never permission to carry
an incomplete unsafe `Trace` implementation or extend the lifetime of an
unrooted pointer.

### I5-I10 Verification Boundary

- The complete production runtime remains `CollectionPolicy::NoAuto`, and no
  explicit full collection runs over it during I5-I10. Production tests in
  these phases verify semantic behavior, exact visitor/root construction,
  mutation-gateway placement, owner retirement, and ordinary drop behavior
  without reclaiming the whole graph.
- Every value surviving a mutator region is already a registered root or an
  exact managed edge after I4F.2. A later I5-I10 checkpoint may change a
  payload's internal representation only if that same change supplies its
  exact visitor or root disposition. If a later source audit finds a durable
  bare value, the responsible earlier phase is reopened; I9 is not a safe
  holding area for delayed root conversion.
- Before any isolated fixture collects a managed family, that family completes
  I4.0's direct/transitive destruction record. Managed destruction is passive;
  an active cleanup path must remain in a separately inventoried external root
  owner and may not be reachable from the managed graph.
- A phase may force collection only in a fresh isolated collector-ready fixture
  whose complete reachable graph is closed over the family under test and
  already certified prerequisite families. The fixture must not borrow a
  production runtime, production cache, global root registry, or another
  still-unclassified graph surface.
- Each isolated fixture first proves rooted survival, then removes the final
  root and proves reclamation. This is family-level trace evidence, not
  certification that the complete runtime graph is collectible.
- I11A repeats every isolated fixture while reconciling Gate G2. I11B owns the
  first forced full collections over an actual production runtime and repeats
  the I5-I10 reclamation cases through that boundary.

## Phase I5 — Lazies and Promises

### Phase I5A — Lazy-Cell Migration

- Replace `Arc<LazyCell>` with a managed identity cell and trace every lazy
  source, terminal evaluated value, and permanent failure.
- Preserve source replacement and terminal publication order. Route source
  replacement and result installation through representation-local safe
  managed-edge gateways; clear/release the source after terminal publication
  as today.
- Complete the `LazyCell` direct/transitive destruction record before its
  first isolated collection. Any notification, task identity, or active
  cleanup state remains in an edge-free external companion rather than its
  managed fields.
- Update I4's compatibility visitor in the same checkpoint; collection being
  disabled permits no trace placeholder.

Verification: preserve the lazy publication/wait suites and reclaim a direct,
two-node, and many-node lazy cycle in a closed fixture. Add
`managed_lazy_drop_is_passive_and_releases_rust_resources`. Production remains
`NoAuto` and does not collect.

### Phase I5B — Promise-Cell Migration

- Replace `Arc<PromiseCell>` with a managed identity cell. Store successful
  assignment and other logical managed edges inside the traceable cell rather
  than hiding them behind registered roots.
- Keep task/waiter IDs, subscriptions, notifications, and producer
  coordination in ordinary Rust companions only where they contain no `Gc`,
  `Root`, public `Value`, or equivalent managed ownership.
- Route one-write assignment through a representation-local safe gateway and
  preserve publication-before-notification ordering.
- Complete the `PromiseCell` direct/transitive passive-destruction record
  before its first isolated collection.

Verification: preserve resolver/task publication races and reclaim a resolved
promise whose result contains that promise. Add
`managed_promise_drop_is_passive_and_producer_companion_is_edge_free`.
Production remains `NoAuto` and does not collect.

### Phase I5C — External Promise and Producer Lifecycle

- Keep `PromiseResolver` and any producer/task owner which performs failure,
  cancellation, abandonment, notification, or wakeup as an external/rooted
  owner with the exact authorized runtime capability it needs. It is not a
  managed finalizer and must not be reachable from `PromiseCell` or another
  managed allocation.
- Express cleanup as an explicit idempotent retirement operation. Preserve the
  existing `Drop` fallback where dropping the final unresolved resolver or
  producer currently establishes a permanent failure or terminal state.
- Preserve lock/callback ordering: terminal state is published under the
  reviewed coordinator/component protocol; wakes, callbacks, and destruction
  occur after locks are released.
- Prove retirement releases its registered roots and runtime capability and
  that duplicate explicit/`Drop` retirement is harmless.

Verification: retain unresolved-resolver failure, producer cancellation,
owner-session closure, and cross-session observation tests after promise values
become managed. Add `promise_resolver_drop_invokes_idempotent_retire_once` and
`external_promise_owner_has_no_managed_backedge`. Production remains `NoAuto`.

### Phase I5D — Cross-Family Cycle Reclamation

After I5A-I5C pass, use closed collector-ready fixtures to construct and
reclaim promise-to-lazy and lazy-to-promise graphs, a deferred producer retained
by a worker, and the terminal value still reachable from another public root.
Prove the live rooted case survives, then retire the external owner/drop the
last root and prove the otherwise unreachable cycle is reclaimed. This phase
does not repeat or weaken the I4.0 family drop audits. The complete production
runtime remains `NoAuto` and does not collect.

## Phase I6 — Functions, Applications, Metadata, and Failures

### Phase I6A — Functions, Applications, and Fixpoints

- Migrate recursive function stages/wrappers, partial builtin arguments, lazy
  applications, and fixpoint computations according to the I4 granularity.
- Preserve referential equality where current semantics relies on identity.
  Immutable argument arrays receive tracing but no mutation gateway.

Verification: `managed_function_stage_cycle_reclaims`,
`managed_partial_application_cycle_reclaims`, and
`managed_fixpoint_cycle_reclaims`; existing shared-stage and recursive-function
tests retain identity behavior. Reclamation tests use closed isolated family
fixtures; production remains `NoAuto` and does not collect.

### Phase I6B — Metadata Identity

- Migrate `MetadataCarrier` identity and its exact one-edge visitor so metadata
  can participate in cycles without leaking.
- Preserve sealing, reorder/copy, `seq`, `spark`, and reflection-inspection
  semantics without exposing metadata to pure evaluation.

Verification: `metadata_and_collections_can_participate_in_a_deferred_value_cycle`,
the metadata update reorder/copy tests, and
`managed_metadata_cycle_reclaims_in_isolated_heap`. Only the named closed
fixture collects; production remains `NoAuto`.

### Phase I6C — Failures and Context Frames

- Migrate evaluation failures and context frames, visiting emission/context
  values without evaluating, formatting, or locking them.
- Preserve shared failure identity and structured diagnostic projection.

Verification: `managed_failure_context_cycle_reclaims`, the existing
structured-failure suite, and `failure_trace_invokes_no_semantic_service`.
The reclamation case uses a closed isolated family fixture; production remains
`NoAuto` and does not collect.

### Phase I6D — Reflection and Net-Construction Payloads

- Migrate reflection computations, gate targets/results, and net-construction
  payloads. Route only actual one-write/replaceable managed edges through safe
  representation-local mutation gateways.
- Reconcile these visitors with I4E without performing runtime-net migration,
  which remains I8.

Verification: `managed_reflection_gate_cycle_reclaims`,
`managed_net_construction_cycle_reclaims`, and current task-result publication
tests. Production remains `NoAuto`; only each closed family fixture may force
collection.

## Phase I7 — Persistent List and Dictionary Trace Audit

- Keep RPDS and FingerTree/`Arc` spines initially.
- Audit and extend I4's exact logical tracing of keys, values, list chunks, lazy
  list thunks, concatenation nodes, and shared slices against the concrete
  representation inventory. A missing node is a soundness defect, not work
  intentionally deferred from I4.
- In an isolated collector-ready fixture, construct the same persistent
  representation exposed through the public value facade. Verify it retains
  all contained managed objects across full collection and that dropping its
  final fixture root permits a backedge cycle to be reclaimed. Do not run this
  collection through a production runtime or its public-root registry.
- Measure duplicate trace work for heavily shared versions and record a
  threshold for revisiting collector-aware physical nodes.

Logical duplicate visits are a performance issue in a mark collector, not an
edge-counting soundness problem. This phase must not silently turn collection
updates into whole-map copies. Production remains `NoAuto`; I11 repeats these
reclamation cases only after Gate G2 closes the whole graph.

## Phase I8 — Managed Core Runtime Nets and Trace Audit

The production representation uses one managed synchronization-owning outer
cell per shared core runtime net. The cell owns the semantic mutex, revisions,
normalization state, and ordinary Rust topology containers. Individual agents,
ports, map entries, and topology allocations do not become separate GC
allocations in this phase. Generic non-core interaction-net ownership remains
collector-independent.

### Phase I8A — Ownership-Neutral Generic Net Seam

- Reconcile I0/I4's inventory of every core value stored in net templates,
  agents, active pairs, stuck pairs, cursors, logical copies, normalization
  state, and every concrete `SharedRuntimeNet<S>` reference embedded in generic
  callable/copy/frontier/contention structures.
- Refactor the generic runtime boundary so its shared-owner and cross-net
  reference operations do not require the production core representation to
  be `Arc<SharedRuntimeNetInner<_>>`. Use a statically typed owner seam; do not
  type-erase net references or add a `glam_gc` dependency to generic topology.
- Preserve I3D.3's private authority-gated `CoreRuntimeNet` facade and all
  existing interaction-net behavior while it still uses the pre-migration
  owner internally. This checkpoint changes neither GC reachability nor
  production collection policy.

Verification: retain the complete generic interaction-net and Cursor-WHNF
suite; add compile-time/privacy coverage that generic topology has no collector
dependency and core/evaluator callers cannot recover the underlying generic
owner. Latch the concrete cross-net handle inventory for I8B. Production
remains `NoAuto`.

### Phase I8B — Managed Outer Cell, Exact Trace, and Mutation Gateways

- Introduce a managed `CoreRuntimeNetCell` (final name selected during the
  checkpoint) which directly owns the semantic mutex, revisions,
  subscriber/normalization state, and `RuntimeNet<CoreSpecialization>`. Change
  the private `CoreRuntimeNet` facade to carry this managed identity. The old
  production `Arc` owner must not remain nested underneath the managed cell.
- Convert core cross-net source/copy references to exact managed edges. Preserve
  the proven hierarchical copy topology, while allowing value payloads to form
  arbitrary cycles through nets, lazies, promises, functions, and other nets.
- Keep generic/test specializations free to use their existing external owner;
  do not make each generic runtime node a managed allocation.
- Trace every managed data, operator, failure/stuck, pending-work, and cross-net
  edge from the outer cell without reducing the net or materializing a cursor.
  Trace under the semantic mutex with nonblocking `try_lock`. `WouldBlock` is an
  invariant defect because exclusive collection precludes the scoped runtime
  access required by every legitimate ordinary lock holder. Treat poisoning
  according to the collector's reviewed panic policy.
- Route every replacement of a managed edge through the representation-local
  mutation gateway. It remains a no-op for the full collector; future moving or
  concurrent collectors may extend it under their own plans.
- In the same checkpoint, root or scope every `CoreRuntimeNet` handle which can
  survive an evaluator region. No bare managed net handle may enter parked,
  type-erased, callback-owned, or coordinator state. Adapt normalization
  leases, contention observations, and weak/external ownership conventions to
  the managed identity without weakening I3D.3's non-escape rules.
- Give the managed outer cell the reviewed internal finalization needed to drop
  its mutex and ordinary Rust containers. It must fit one typed-run slot; this
  phase introduces no large-object or multi-run exception.
- Close the pre-managed edge vocabulary after installing the final net
  visitor: delete `CompatibilityValueEdges` and its `payload_edges.rs` module,
  and migrate every remaining compatibility-edge fixture to the managed family
  test which replaces it. A source-backed latch must prove that no
  compatibility implementation or call site survives I8B.

Verification: source inventory accounts for every old production
`Arc`/`Weak<SharedRuntimeNetInner<CoreSpecialization>>` owner and every durable
core-net handle. Exact-visitor tests cover every runtime-node and pending-state
variant. Mutation tests cover each value-installing rewrite. Compile-time or
privacy fixtures reject unscoped dereference and parked bare handles. Add
`managed_edge_vocabulary_has_no_compatibility_adapter` to latch final removal.
Production remains `NoAuto`; only closed subsystem fixtures may collect.

### Phase I8C — Cycle Reclamation and Final Net Audit

- Force collection in isolated collector-ready fixtures covering a direct
  net-to-value-to-the-same-net cycle, mutually linked nets through data values,
  shared function stages, cursor materialization, stuck nets, pending
  active-pair work, and hierarchical copy-source references.
- Prove a rooted net and its complete topology survive collection, then prove
  dropping the final root reclaims its outer cell and every otherwise
  unreachable managed cycle.
- Re-audit Cursor-WHNF ownership, normalization batches, contention waits,
  unwind dispositions, finalization, and lock ordering against I3D and the
  ownership ledger.

The complete production runtime remains `NoAuto` until I11 repeats the net
cases after Gate G2 closes the entire graph.

## Phase I9 — Runtime-Root Lifecycle and Retirement Audits

I4F.1 performs the structural conversion of durable owners to stable
root-shaped surfaces before managed values can escape, and I4F.2c registers
those surfaces at the managed-value switch. I9 revisits the already-rooted
representations after the I5-I8 interior migrations to verify lifecycle,
retirement, and exact ownership. Discovering a first-time root-surface
conversion here is a chronology failure: stop and repair the checkpoint which
first allowed that owner to contain a managed edge.

### Phase I9A — Runtime Canonical and Compiler Caches

- Audit the explicit external roots or exact managed cache edges whose owner
  surfaces were installed by I4F.1 and registered by I4F.2c for runtime
  canonical values and type-indexed compiler attachments.
- Close the `Any` registration boundary: each extension family has a stable
  ledger record, remains structurally rooted, and cannot retain a
  factory/domain backedge.

Verification: existing cache initialization/race tests,
`compiler_cache_does_not_form_a_value_domain_cycle`, and
`runtime_cache_roots_release_after_last_domain_owner`. Production tests observe
root/owner retirement without collecting. Any reclamation assertion uses a
closed isolated cache fixture; production remains `NoAuto`.

### Phase I9B — Coordinator and Evaluation Ownership

- Audit the registered roots and exact managed edges for client demands,
  sparks, tasks, deferred producers, waits, failure ledgers, task handles, and
  promise resolver state after the I5-I8 interior migrations.
- Do not root a value merely because an edge-free notification companion names
  its task/wait ID. Ownership follows semantic retention, not scheduler
  reachability.

Verification: `blocked_machine_context_does_not_retain_its_owner_lease`,
`task_handle_acknowledges_terminal_failure_after_owner_lease_closes`,
`task_handle_cancellation_is_harmless_after_owner_closure`, and
`coordinator_roots_release_after_terminal_record_retirement`. Use isolated
closed coordinator fixtures for reclamation; production remains `NoAuto`.

### Phase I9C — Reflection Store and Protocol Roots

- Audit the root surfaces installed by I4F.1 and registered by I4F.2c for
  reflection environments, protected volumes, store snapshots, views,
  journals, queries, rewrites, and parked protocol machines; update only
  interior edge representations introduced by their owning migration phase.
- Preserve persistent snapshot isolation and the I1B rule that an active store
  or snapshot may retain its authorized construction domain.

Verification: existing persistent-snapshot/query-retirement/volume-revoke
tests plus `reflection_store_roots_survive_owner_close` and
`reflection_snapshot_roots_release_after_snapshot_drop`. Production tests
observe root and snapshot retirement with collection disabled. Any reclamation
assertion uses a closed isolated store fixture; production remains `NoAuto`.

### Phase I9D — Diagnostics and Runtime Events

- Audit the root surfaces installed by I4F.1 and registered by I4F.2c for
  diagnostic buses/ingress, event inputs, output intents, running deliveries,
  and retained delivery failures.
- Preserve callback-after-lock, exact delivery ownership, fallback routing, and
  settlement activity semantics.

Verification: `output_payload_is_retained_through_callback_and_dropped_after_locks`,
`running_delivery_retains_shared_resources_until_terminal_publication`,
`diagnostic_bus_and_ingress_do_not_retain_the_runtime`, and
`transport_roots_release_after_delivery_and_subscription_retirement`. Only
isolated transport fixtures may collect; production remains `NoAuto`.

### Phase I9E — Assembly, Compiler, and CLI Owners

- Audit the root surfaces installed by I4F.1 and registered by I4F.2c for
  assembler/module construction state, compiler setup and origins, source/macro
  intermediates which park across evaluation, and binary-owned
  configuration/logger records.
- Keep bounded locals under I3 regions; only semantically retained values
  become roots.

Verification: existing module build/import, macro protocol, configured CLI,
and logger supervision suites plus
`assembly_compiler_and_cli_roots_release_after_last_owner`. This checkpoint
proves production owner retirement without collection; a closed subsystem
fixture may prove local reclamation where practical. Production remains
`NoAuto`.

### Phase I9F — External Active-RAII Lifecycle Audit

- Inventory every external/rooted `Drop` implementation which performs or may
  trigger cancellation, failure, abandonment, notification, logging, task
  terminalization, or another runtime action. At minimum cover
  `PromiseResolver`, `EvaluationSession`, `ClientDemandHandle`, and an
  unactivated pending reflection task, then reconcile the source search rather
  than treating that list as permanently exhaustive.
- For each owner, record its strong runtime/value-domain capability, registered
  roots, explicit idempotent retirement operation, `Drop` fallback, terminal
  status/error semantics, lock ordering, and callback/destruction boundary.
- Prove every such owner is ordinary external Rust state and is unreachable
  from the managed graph. A managed allocation may retain an edge-free **C**
  coordination companion, but never an active external owner or a capability
  which reaches it transitively.
- Preserve active RAII where scope exit is part of current semantics. Do not
  rename these paths as managed finalizers and do not make them passive merely
  because the values they control have moved into managed cells.

Verification: after the referenced values become managed, preserve
unresolved-resolver failure, session closure and owned-work terminalization,
client-demand abandonment, and unactivated-reflection-task cancellation.
Forced-order tests cover explicit retirement followed by `Drop`, `Drop` alone,
and concurrent observation, proving exactly one terminal transition and no
callback/destruction under a component lock. Add
`active_external_raii_inventory_is_reconciled` and
`managed_graph_reaches_no_active_raii_owner`. Production remains `NoAuto`.

### Phase I9G — Runtime-Root Source Inventory

- Re-run the exhaustive source search for core/public values, roots, evaluated
  values, snapshots, diagnostics, type-erased attachments, and parked machine
  fields.
- Match every result to one stable ledger family and named owner. An unmatched
  field blocks I10/Gate G2.
- Compare the result with the latched I4F.1 inventory and I9F active-RAII
  inventory. A newly discovered
  durable bare value reopens and repairs its earliest managed-edge checkpoint;
  it is not converted opportunistically in I9G.

Verification: `runtime_root_source_inventory_is_reconciled` plus controlled
owner-drop tests named by every root family. Production remains `NoAuto`; this
checkpoint proves root classification, not whole-graph reclamation.

## Phase I10 — Deferred Closures and Opaque Boundaries

### Phase I10A — Deferred Closure Containment

`Arc<dyn Fn(&EvalContext) -> ...>` cannot be traced automatically.

- Reconcile every production deferred constructor with I4B and replace
  Glam-owned value captures with explicit traceable computation state wherever
  they can participate in the managed graph. Preserve the I3E.1 operational
  classification: callback-free semantic thunks and deterministic external
  demands may use different state representations, but neither may hide an
  untraced capture.
- A remaining genuinely external Rust closure uses an explicit same-runtime
  public-root bundle. It may not smuggle a bare managed pointer or root back to
  an internally owning managed graph.
- Apply the same rule to compiler cached functions and task launchers.

Verification: `managed_deferred_state_cycle_reclaims`,
`external_closure_bundle_retains_only_declared_roots`,
`closure_bundle_rejects_internal_backedge`, and
`deferred_closure_constructor_inventory_is_reconciled`. Only isolated closure
fixtures may collect; production remains `NoAuto`.

### Phase I10B.0 — Opaque Representation Decision Review

This is a hard design gate, not an implementation checkpoint. It begins only
after I4B's constructor restrictions, I4F.1's durable-owner inventory, I9F's
external active-RAII inventory, and I10A's deferred-closure containment have
completed. Its inputs are:

- a source-backed inventory of every `OpaqueValue::new`, downcast, `Any`,
  extension registration, token/companion, and opaque payload family;
- the concrete use cases, if any, which require an opaque-managed edge to
  participate in cycle reclamation rather than remaining an external root;
- the current owning-`Arc` downcast and identity semantics relied upon by Rust
  callers;
- I3's scoped managed-access authority, I4.0's passive managed-destruction
  admission rule, and I9F's external active-RAII boundary;
- requested layout/slot constraints for every proposed managed family; and
- the conservative-retention cost of an external public-root backedge for each
  real payload family.

The review selects exactly one bootstrap policy:

1. **External-only opaque storage.** `OpaqueValue` remains an external
   type-erased Rust owner. Admitted payloads are edge-free tokens/companions or
   audited same-runtime public-root owners. No opaque payload is collector-
   managed, no scoped managed downcast is introduced, and ordinary owning
   `Arc` access may remain subject to runtime/provenance checks. Cycles hidden
   behind external roots may be conservatively retained but never reclaimed
   prematurely.
2. **External storage plus a sealed managed arm.** Arbitrary `Any` remains
   subject to the external-only rule. A distinct private arm uses a statically
   registered concrete managed cell with exact trace and passive-drop
   functions outside the `Any` payload. Each admitted managed family has a
   stable ledger record, one-slot layout proof, exact edge visitor, provenance
   rule, I4.0 destruction proof, and mutator-bound typed access. Registration
   is sealed to reviewed Glam families; it is not a general host escape hatch,
   and no owning managed reference may leave scoped access.

The decision is recorded in a dated opaque-representation review document. It
must state why actual payload use cases justify the selected complexity and
must produce all of the following plan changes before this gate passes:

- replace I10B with concrete representation, registration, provenance, and
  negative-boundary checkpoints for the selected model;
- rewrite I10C so scoped access and passive managed destruction appear only if
  the managed arm is selected, while external active RAII remains governed by
  I9F;
- update the opaque rows in the ownership ledger and the roadmap invariant;
- update the integration completion criteria and Gate G2 source inventory;
- name migration and compatibility tests for every existing constructor and
  downcast call site; and
- if the managed arm is selected, partition implementation into representation,
  family registration, scoped access, passive-drop, and negative-boundary
  checkpoints, each with an isolated closed fixture.

Verification of the review artifact:
`opaque_representation_review_inventory_is_complete` maps every inventoried
family and call site to the selected policy;
`opaque_representation_plan_has_no_undecided_family` rejects a mixed or
deferred classification; and a plan-link check proves the decision artifact,
ledger, I10B/I10C, Gate G2, and completion criteria agree.

No I10B implementation, managed opaque allocation, scoped managed downcast,
opaque-family collection fixture, or Gate G2 certification may begin while
I10B.0 is pending. The current arbitrary-`Any` prohibition remains
authoritative throughout the review.

### Phase I10B — Decision-Selected Opaque Registration and Provenance

This phase is deliberately not implementation-ready until I10B.0 rewrites it
into the concrete checkpoints selected by the dated review. The selected plan
must preserve these common invariants:

- Keep arbitrary host `Any` payloads as tracing barriers. Each such family is
  registered as an edge-free token/companion or a genuinely external owner of
  same-runtime public roots. A selected sealed managed arm is a distinct exact
  representation, not data hidden inside `Any`.
- Forbid bare `Gc<T>`, unrooted recursive core values, foreign roots, and
  equivalent region escapes. Keep opaque construction private and do not
  re-export collector pointers.
- Prefer host-owned side tables for generic embedding payloads; the Glam token
  carries only identity/provenance.

Verification: `opaque_family_inventory_is_reconciled`,
`opaque_registration_rejects_bare_managed_pointer`,
`opaque_registration_rejects_unrooted_core_value`, and
`opaque_registration_rejects_foreign_root`. Production remains `NoAuto`; no
new destructor authority is selected here. Every admitted managed family is
already subject to I4.0 before its first collection; external owners retain the
active-RAII contract audited by I9F.

### Phase I10C — Final Opaque Destruction and External-Lifecycle Audit

- Re-audit every opaque/closure representation against I4.0's already-active
  managed destruction rule. The collector-held mutator during `Finalizing` is
  collector coordination state; it is neither passed to destructors nor
  exposed through an ambient/TLS accessor. Managed direct and transitive
  destruction releases only ordinary Rust resources and performs no runtime
  work or `Gc` observation.
- Reconcile opaque external/rooted lifecycle owners with I9F. Such an owner
  performs an explicit, idempotent retirement operation while the runtime is
  live; where scope-exit semantics require it, its ordinary Rust `Drop` may
  call that same operation as an active fallback. It is never reachable from a
  managed allocation and is not a managed finalizer. Not every rooted runtime
  element needs a managed representation.
- If I10B.0 selects a managed arm, replace owning access to that arm with a
  scoped mutator-bound borrow for live access only. External-only payloads and
  companions retain the access model selected by the review, may retain
  ordinary Rust ownership and public roots, and are not finalized as managed
  graph nodes.
- Treat any future production managed destructor that appears to need runtime
  or heap authority as a new design-review gate. Do not introduce a weak-domain
  capability or TLS bridge as a local exception.

Verification: rerun `managed_drop_has_no_runtime_or_heap_capability`,
`managed_drop_releases_transitive_rust_resources_passively`,
`external_root_owner_drop_invokes_idempotent_retire`,
`managed_graph_reaches_no_active_raii_owner`,
`managed_drop_during_domain_teardown_is_passive`, and
`opaque_drop_panic_retries_untouched_suffix`. Use isolated managed-payload and
external-owner fixtures; production remains `NoAuto`.

### Phase I10D — Final Closure/Opaque Containment Audit

- Re-run the complete closure, `Any`, opaque-constructor, downcast, compiler
  cache, launcher, and managed-payload source inventory.
- Match every result to a stable trace/root/leaf/finalization record. Never use
  an unsafe scan or conservative heap walk to discover hidden pointers.
- Confirm no managed payload retains the value domain strongly and no root is
  being used to hide an internal fixpoint edge.

Verification: `final_closure_opaque_and_any_inventory_is_reconciled`,
`managed_payloads_have_no_strong_value_domain_backedge`, all I4B/I10 negative
fixtures, and the focused collector finalization suite. Production remains
`NoAuto`. An unmatched family blocks I11A and Gate G2.

## Phase I11 — Whole-Graph Forced Full Collection

### Phase I11A — Gate G2 Certification

- Reconcile the final source inventory one-to-one with complete stable ledger
  records for values, traces, roots, closures, opaque families, caches,
  persistent collections, nets, and runtime owners.
- Audit every unsafe trace/downcast/mutation gateway and the I3 region/lock
  boundaries. Preserve GCI-007's resolved exact-edge chronology, GCI-008's
  scoped locked-net trace, GCI-009's isolated-fixture chronology, and
  GCI-011/GCI-014's I4.0 passive managed-destruction admission boundary plus
  GCI-013's separate active external-RAII ownership rule.
- Repeat every isolated family reclamation fixture while production remains
  `NoAuto`.

Verification: `gate_g2_source_inventory_is_closed`, the complete stable-ledger
check, and a dated Gate G2 review with no unmatched graph-bearing field or
incomplete family record. Until it passes, no full collection may run over a
production runtime.

### Phase I11B — Controlled Production Forced Collection

- After Gate G2, expose forced collection only to explicit tests and a private
  runtime maintenance operation.
- Run it at stable serial boundaries around module compilation, reflection
  quiescence, event delivery, logger supervision, and settlement.
- Repeat every I5-I10 ownership/reclamation case against the actual production
  runtime rather than only its isolated fixture.

Verification: `production_collection_preserves_each_serial_boundary` covers
assembly results, diagnostics, transaction data, readiness, observation epochs,
and net revisions; each I5-I10 family also gains a production reclamation
fixture. The heap policy remains `NoAuto`; collection occurs only through
explicit controlled calls.

### Phase I11C — Worker and Finalizer Concurrency Schedules

- Force collection before, during, and after worker activity using deterministic
  barriers, then add the aggressive debug request-before-outer-entry mode.
- Finalize passive opaque payloads while logger supervision and workers are
  active. Prove finalization produces no diagnostics, events, tasks, managed
  allocations, or other runtime work.
- Issue collection requests from external runtime work while finalization is
  active and prove coalescing avoids recursive collection or an immediate
  second pass. Worker entry into the same heap remains governed by ordinary
  admission and does not depend on destructor callbacks.
- Exercise runtime drop before and after collection.

Verification: `collection_interleaves_with_worker_quantum_without_lost_work`,
`passive_finalization_produces_no_runtime_work`, and
`external_request_during_finalization_is_coalesced`, followed by
repeated worker stress, focused Miri, and sanitizer runs. The production heap
remains `NoAuto`; only explicit tests/maintenance collect.

### Phase I11D — Gate G3 Certification

- Run the full repository suite under ordinary execution and the aggressive
  debug-collection mode.
- Complete focused Miri, sanitizer, unsafe-site, trace-edge, mutation-gateway,
  and lock/region audits.
- Publish a dated Gate G3 review accounting for every I11 schedule and any
  intentional nondeterministic reflection behavior.

Passing G3 authorizes I12's controlled runtime maintenance and later threshold
service review. It does not switch any heap from `NoAuto` to `Automatic`.
Collection policy is immutable for one heap; I12B.0 may select a different
construction policy only for runtimes created after that decision.

Verification: the routine repository checks, `cargo test --workspace -q`, the
aggressive debug-collection suite containing every I11B/I11C named fixture,
focused Miri, supported sanitizers, and a dated Gate G3 review. Every existing
heap remains `NoAuto` for its lifetime. A later I12 policy checkpoint may
change only how new runtime heaps are constructed.

## Phase I12 — Explicit Runtime Maintenance and Threshold Collection

### Phase I12A.0 — GC Operational Activity and Readiness Review

This is a hard design gate after Gate G3 and before explicit maintenance is
enabled outside I11's stable serial test boundaries. It reviews the existing
`RuntimeMutationAdmission`, authoritative readiness snapshots and validation,
`RuntimeActivityState` parking generation, private runtime heap-entry paths,
and collector activity/finalization statistics. Collector snapshots are
observational inputs only; readiness must not infer authority by sampling them.

The review adopts the following protocol shape:

- a private runtime heap-entry/maintenance facade acquires a logical runtime
  operational-activity lease *before* invoking any entry which may elect or
  explicitly run collection;
- lease admission is published under the same shared runtime mutation gate
  which excludes readiness/settlement's exclusive validation. The gate is then
  released while collection and passive finalization run;
- authoritative readiness observes the active-lease count/revision under its
  exclusive gate. A readiness snapshot includes the corresponding revision so
  a lease admitted after observation invalidates later acceptance;
- the lease survives collection and every running finalizer and is retired
  under shared mutation admission after success or unwind. Releasing it
  advances the existing runtime activity generation and wakes parked pumps only
  after the authoritative state change;
- `glam-gc` receives no runtime callback and knows nothing about readiness;
  `Heap::activity()` and `Heap::statistics()` remain diagnostics/profiling
  snapshots rather than settlement stamps; and
- a request-only operation does not claim an active lease because it cannot
  collect. If it creates a serviceable runtime obligation, the selected
  maintenance policy must nevertheless issue the ordinary runtime wake.

The review's required source inventory assigns every current and planned heap
entry to one of three classes:

1. cannot collect under its immutable heap policy and needs no GC activity
   lease;
2. may elect collection (including every outer entry on a future `Automatic`
   runtime) and must enter through the leased facade; or
3. explicitly collects and must enter through the same leased facade.

Recursive same-heap entries, direct test/debug entry, aggressive collection,
factory/evaluator access, explicit maintenance, and every runtime constructor
must appear in the inventory. Before an automatic runtime can be selected by
I12B.0, privacy/compile-time evidence must prove production callers cannot
bypass the facade.

The review must also select a durable disposition for a finalizer panic which
leaves a pending batch. An inactive pending batch may not remain anonymous
permanent `Busy`. The decision must choose and specify either a reportable
runtime maintenance failure carried by readiness/settlement, or an explicit
retry-required maintenance state with a public/client-visible disposition and
wake protocol. It must define acknowledgement/retry, batch ownership, runtime
exit-code impact, and how successful retry clears the state. An actively
running retry remains covered by the activity lease.

The output is a dated GC-readiness integration review which rewrites I12A and,
where necessary, runtime readiness/report types, snapshot stamps, settlement
validation, I12B's automatic-entry prerequisites, and the completion criteria.
No routine concurrent maintenance or automatic runtime construction may begin
until that artifact and its plan changes land.

Required forced-order verification in the rewritten plan:

- readiness holds or has just released exclusive admission as a collecting
  entry attempts to register its lease;
- readiness observes the runtime immediately before collection election and
  later rejects the stale snapshot;
- a pump snapshots the parking generation while a finalizer is blocked, then
  sleeps or rechecks as the lease is released, proving no lost wake;
- several concurrent may-collect entries hold independent leases and readiness
  remains `Busy` until the last retires;
- collection/finalization success, trace panic, finalizer panic, and retry all
  retire or preserve exactly the selected authoritative state; and
- both `NoAuto` manual service and any future `Automatic` outer-entry election
  use the same activity protocol without giving the collector a callback.

Named review-artifact checks:
`gc_activity_entry_inventory_is_complete`,
`gc_readiness_plan_has_one_authoritative_activity_source`, and
`pending_finalizer_batch_has_durable_nonbusy_disposition`.

### Phase I12A — Explicit Maintenance for `NoAuto` Runtimes

- Expose a narrow embedding maintenance method or runtime tuning policy; do not
  expose raw heap internals.
- Preserve the collector crate's two-level control surface: a nonblocking,
  coalescing request which may be issued before a known batch boundary, and a
  synchronous full-collection operation used only outside an active mutator.
  These are Rust runtime-maintenance controls, not Glam evaluation effects.
- Collect `NoAuto` runtimes only through explicit service at reviewed
  batch/idle boundaries. Successful typed-run publication may latch a
  pressure request, but ordinary outer mutator entry does not service it.
  Runtime maintenance observes the request/statistics and deliberately calls
  synchronous collection when its boundary policy permits. Lease-word claims
  and individual slot allocations remain outside shared pressure accounting.
- Implement the operational-activity lease, readiness revision, wake, and
  pending-finalizer disposition selected by I12A.0 before enabling this path
  for routine concurrent runtime operation. Until that implementation passes,
  I12A may run only at the stable serial boundaries already certified by I11.
- Do not begin a requested collection while the heap is in `Finalizing`.
  Requests made before successful completion are heuristic hints coalesced into
  the active collection and are cleared with its pressure baseline; they do not
  queue a second writer or deny fresh mutator admission. A request serialized
  after completion remains latched for the next explicit maintenance service.
- Ensure a request cannot make a worker spin, hold settlement admission, or
  publish semantic activity merely because collection ran.
- Report metrics for debugging and profiling without making them observable to
  pure evaluation.

Verification: construct a production runtime with immutable `NoAuto`, cross
its pressure threshold, prove repeated outer mutator entries do not collect,
then explicitly service the request at each reviewed boundary. Preserve request
coalescing, finalizer panic/retry, and no-recursive-collection behavior. Add
`runtime_no_auto_pressure_requires_explicit_service` and
`runtime_manual_maintenance_never_mutates_heap_policy`.

### Phase I12B.0 — New-Runtime Collection-Policy Decision Review

This is a hard design gate after Gate G3 and stable I12A manual maintenance.
It does not inspect or mutate the policy of a live heap. Its inputs are:

- I12A correctness, latency, throughput, pause-time, pressure, survivor, and
  boundary-placement measurements under representative assemblies;
- the immutable `CollectionPolicy` collector contract and the runtime/value-
  domain construction API;
- the list of all runtime constructors, test fixtures, embedding entry points,
  and configuration/profile paths which select or assume a policy;
- the GCI-016 readiness/activity decision and forced-order evidence if routine
  or automatic concurrent collection is under consideration, concretely the
  completed I12A.0 artifact and its implementation; and
- operational reasons to prefer collector-elected outer-entry service over
  runtime-selected explicit maintenance boundaries.

The review selects exactly one policy for future runtime construction:

1. **Automatic new runtimes.** Runtimes created after the implementation
   checkpoint construct their heap with `CollectionPolicy::Automatic` (with
   any explicit manual/testing construction mode retained by the selected
   runtime API). Existing `NoAuto` runtimes remain manual forever. Successful
   pressure requests may be elected by a later idle outer mutator entry only
   for those new automatic heaps. This option is blocked until I12A.0's
   activity/wake protocol covers every entry which may elect collection.
2. **Permanently manual runtimes.** Production construction continues to use
   `CollectionPolicy::NoAuto`. Pressure requests remain latches consumed only
   by I12A's explicit maintenance service; no plan or documentation may claim
   that ordinary outer mutator entry services them.

The decision is recorded in a dated runtime-GC-policy review. It must choose
the runtime construction API and default, inventory every constructor, and
rewrite the following I12B phase, runtime documentation, test matrix, and Gate
G4 criteria. No hybrid policy inferred from current pressure or live runtime
state is permitted.

Verification of the review artifact:
`runtime_gc_policy_review_inventory_is_complete` maps every constructor to an
explicit immutable policy;
`runtime_gc_policy_plan_has_no_live_transition` rejects any `NoAuto`-to-
`Automatic` mutation; and plan-link validation proves the decision artifact,
I12B, readiness prerequisites, and completion criteria agree.

### Phase I12B — Decision-Selected Runtime Construction Policy

This phase is not implementation-ready until I12B.0 rewrites it. In either
outcome:

- store the selected policy once when constructing the runtime heap and expose
  no live policy setter;
- preserve an explicit `NoAuto` construction path in tests so manual service
  remains covered;
- test that already-created runtimes retain their original behavior after new
  runtimes are constructed under the selected policy; and
- keep policy, pressure, and collection counts outside pure Glam observation.

If `Automatic` is selected, additionally construct and exercise both manual
and automatic runtimes, prove pressure-triggered collection occurs only on the
automatic heap, and require the completed I12A.0 entry/activity protocol. If
manual service is selected, remove every remaining suggestion that mutator
entry services production pressure and exercise each explicit maintenance
boundary under `NoAuto`.

## Phase I13 — Retire Redundant Ownership and Document the Boundary

- Remove `Arc` wrappers whose only remaining role was recursive value
  lifetime. Retain intentional `Arc`s for public roots, immutable leaf buffers,
  host identities, and edge-free scheduler notification companions.
- Remove duplicated runtime provenance fields when heap identity is
  authoritative and the boundary check remains equally cheap.
- Remove temporary collection-disable gates and migration-only adapters.
- Revisit I4.0's `ManagedDropRecord` after Gate G3 and the production
  integration have made the stable ownership ledger mechanically auditable.
  Keep the private `ManagedFamily` admission boundary; decide then whether its
  textual per-family record still prevents real mistakes or should contract to
  a simpler reviewed-admission token. Do not remove it merely as incidental
  cleanup before that audit.
- Update `docs/architecture/evaluation.md`, `docs/AgentContext.md`, focused
  agent notes, and `src/README.md` with current ownership and safepoint rules.
- Document that the baseline idle-entry collector may starve under
  continuously overlapping mutator regions. This accepted progress limitation
  is owned by the post-integration
  [`ConcurrentGarbageCollection_2026-08-28.md`](ConcurrentGarbageCollection_2026-08-28.md)
  plan and does not weaken the baseline collector's safety or Gate G4.
- Pass roadmap Gate G4 and mark the roadmap and both plans complete only after
  a final invariant and trace-edge audit.

## Integration Verification Matrix

Every managed-type phase runs focused tests before the standard repository
checks:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
cargo test --workspace -q
```

The ordinary commands preserve the repository's established root-package
contract. The workspace run (plus the current `glam-gc` verification script)
ensures the default-member configuration does not accidentally omit collector
tests.

Additional required modes:

- collector disabled, to preserve a comparison baseline;
- forced full collection at selected stable points;
- aggressive full-collection requests before outer mutator entries;
- zero workers and several workers;
- public roots moved among threads and dropped in forced orders;
- every production allocation class fitting the collector's fixed-run slot
  geometry, plus explicit rejection tests for a deliberately oversized fixture;
- Miri for focused root, trace, lazy, promise, collection, and net graphs;
- Loom or deterministic forced-order tests for mutator/root coordination;
- address/thread sanitizers where supported; and
- memory/drop counters proving both retention and reclamation.

Representative sample assemblies must produce identical outputs in all
collection modes. Timing and collection counts are profiling data, not Glam
semantics.

## Integration Completion Criteria

- Every production managed edge is exact or deliberately conservative.
- Every opaque value satisfies the policy selected by I10B.0. Arbitrary `Any`
  contains no managed edge or only ordinary same-runtime public roots. If a
  sealed managed arm is selected, each concrete family is exact, statically
  registered outside `Any`, passively droppable, and accessible only through
  matching scoped runtime authority. No bare collector pointer crosses either
  boundary.
- Public `Value` is a real runtime-local external root, remains convenient to
  clone and share, and is semantically opaque without a live matching runtime
  service.
- Workers access managed pointers only within bounded mutator regions.
- Every managed representation fits one slot in the documented fixed-size
  typed-run geometry; there is no hidden large-object or multi-run fallback.
- Fixpoint, promise, metadata, collection, function, and net cycles are
  reclaimed after their last root disappears.
- Reflection, diagnostics, stores, events, and task handles retain exactly the
  values their semantics require.
- Full collection preserves assembly results and runtime coordination.
- Every entry which can actually collect is represented as authoritative
  runtime operational activity before collection election and wakes readiness
  waiters after retirement. Collector statistics are never readiness
  authority, and an inactive pending finalizer batch has the durable disposition
  selected by I12A.0 rather than anonymous permanent `Busy`.
- Every heap's collection policy is fixed at construction. The I12B.0 decision
  governs only newly created runtimes; no live `NoAuto` heap becomes
  `Automatic`.
- No pointer-local GC locking or atomic reference count remains on internal
  managed edges.
- Any conservative retention through external opaque public roots is
  documented per family and never risks premature collection; no unreviewed
  arbitrary payload is treated as traceable managed storage.
