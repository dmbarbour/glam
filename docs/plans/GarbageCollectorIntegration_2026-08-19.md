# Glam GC Integration Plan — 2026-08-19

Status: in progress; Phase I0 plus I1A-I1B are complete, and collector Gate G1
passed on 2026-08-25. The remaining integration work follows the completed
owner-matrix, stable-ledger, and low-risk checkpoint corrections from the
integration review.

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
| I1 | pending | runtime-owned heap, collection disabled |
| I2 | pending | public value and external-root prototype |
| I3 | pending | bounded evaluator/worker mutator regions |
| I4 | pending | core trace vocabulary and leaf policy |
| I4B | pending | closure and opaque managed-edge containment |
| I4F.1 | pending | durable root-surface conversion gate |
| I4F.2 | pending | public managed-root production switch |
| I5 | pending | managed lazies and promises |
| I6 | pending | functions, applications, metadata, failures |
| I7 | pending | persistent list and dictionary tracing |
| I8 | pending | managed core-net outer cells, exact tracing, and mutation gateways |
| I9 | pending | runtime-root lifecycle and retirement audits |
| I10 | pending | deferred closures and opaque boundaries |
| I11 | pending | whole-production-graph forced collection |
| I12 | pending | runtime maintenance and threshold collection |
| I13 | pending | redundant ownership removal and documentation |

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

For each opaque payload family, the inventory records either `no managed edge`
or the exact same-runtime public root wrapper it may retain. Discovering a bare
`Gc<T>`, unrooted recursive `core::Value`, foreign-runtime root, or equivalent
internal managed pointer is a boundary defect, not a conservative-tracing
classification.

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
| active compiler contexts and scoped compiler factories | strong, through the factory | compilation-local construction and cache access must remain usable |
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

- Give the value factory a narrow allocation/rooting handle, not raw collector
  internals.
- Do not create a `heap -> runtime state -> heap` ownership cycle.
- Let runtime/value-factory construction pre-discover common heap-owned classes,
  but do not retain public allocator capabilities across mutator regions.
  Every actual allocation uses an `Allocator<'_, T>` borrowed from its current
  mutator. If repeated scoped lookup becomes measurable, cache canonical
  metadata, dense class IDs, and stable frontier cells only in the collector's
  existing per-thread/per-runtime-heap state. Such cache entries retain weak
  heap identity rather than owning the runtime value domain. Rare classes may
  use first-use discovery; the allocation hot path must not hash `TypeId` per
  object once its scoped allocator is obtained.

Verification: add `factory_scoped_allocation_uses_current_mutator` and
`scoped_factory_does_not_retain_allocator_or_scheduler` fixtures, while
preserving `scoped_factories_share_one_no_auto_runtime_value_domain`. At this
checkpoint production collection remains `NoAuto`; an isolated factory fixture
may call `collect_full` only over representations introduced in this
checkpoint.

### Phase I1D — Layout Policy and Ownership-Ledger Reconciliation

GCI-005 completed the ledger-schema half of this checkpoint on 2026-08-25.
The ledger now identifies stable Glam representation families and records
reviewable layout, trace, drop, mutation, and root facts. It deliberately does
not use process-local metadata addresses or discovery-order class IDs. Each
later representation-migration phase completes its own family record; Gate G2
performs the final one-to-one source reconciliation.

- Centralize Glam's node-size policy when constructing canonical object
  metadata. That policy may request a slot size larger than the Rust payload;
  allocation-class creation then applies it independently for each typed run.
  Type alignment remains expressed by the Rust node or a common aligned
  wrapper, not by runtime heap configuration. A shared managed-node wrapper or
  declaration macro is Glam's central alignment-policy point; the collector
  does not provide a mutable or per-heap alignment setting.

- As the initial managed shell is selected, complete its stable family record
  and prove allocator discovery accepts its requested extent. Keep final run
  geometry in `glam-gc` verification. Later phases repeat this for their own
  families rather than performing one discovery-order-dependent dump.

Verification: add `managed_family_requested_layout_is_accepted` for every
requested extent selected here, require the corresponding stable ledger
record, and rerun `glam-gc`'s requested-layout rejection suite. Production
collection remains `NoAuto`.

### Phase I1E — Lifecycle Reverification

- Verify the earlier sibling allocation of runtime state and immutable profile
  remains internal to the runtime's ownership graph and acyclic once both can
  retain rooted values. Collector roots are not an ownership escape hatch for
  either sibling.

Verification: rerun `public_values_retain_only_the_runtime_value_domain`,
`runtime_shared_resources_do_not_retain_runtime_lifecycle_owners`,
`retained_reflection_profile_keeps_only_shared_resources_alive`, and
`compiler_cache_does_not_form_a_value_domain_cycle` after I1C/I1D, plus
`runtime_value_domain_has_no_scheduler_or_profile_backedge`. Production
collection remains `NoAuto` and no production `core::Value` is reclaimed.

At this phase no core object is reclaimed by the new heap. Existing behavior
and tests must remain unchanged.

## Phase I2 — External Root and Public `Value` Prototype

### Phase I2A — Wrapper and Provenance Prototype

- Use GCI-004's completed `Heap::owns(&Root<T>)` provenance predicate, then
  prototype C4's direct managed root against scalars and one recursive test
  node. Separately prototype a Glam-owned public wrapper whose managed arm uses
  that root and whose optional inline arm contains only values which require no
  managed trace. Do not add another collector registry-entry or root-cell
  representation for the wrapper.
- Select between exposing the direct root and using the Glam wrapper based on
  public scalar construction cost and clarity. One public `Value` clone must
  preserve its root cell while the value domain lives, but must not preserve
  the heap after the domain's authorized owners are dropped.
- Derive authoritative managed provenance from root heap identity rather than
  an independently forgeable runtime ID. Every safe access performs the
  release-build same-heap check before the private typed-pointer gateway.

Verification: `prototype_root_moves_between_threads`,
`prototype_root_rejects_another_heap`, `prototype_root_becomes_inert_after_domain_drop`,
the collector's `heap_ownership_predicate_accepts_only_the_recorded_live_heap`
and `heap_ownership_predicate_tolerates_concurrent_root_clone_and_drop`, and
the existing mismatched-`Root::get` invariant test. Only the isolated prototype
heap may collect; production remains `NoAuto` and retains its compatibility
`RuntimeValueRoot`.

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
  evaluation scope for one callback-free semantic substep, roots its escaping
  values, and releases the mutator before returning to poll orchestration.
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

Verification: compile-fail fixtures reject returning or storing the access
carrier, evaluation view, mutator, allocator, and managed borrow. Trait checks
prove scoped authority cannot cross threads while durable context remains
`Send`; `different_heap_authority_is_rejected` covers provenance. A focused
fixture proves a poll context can open two separate evaluator scopes with a
callback between them and that no mutator remains active during that callback.
No production call site changes yet, production remains `NoAuto`, and no
production representation is collected.

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

### Phase I3A.3 — Scheduler-Owned Poll Orchestration

- Change `EvaluationTaskMachine::poll` to receive the ephemeral poll context,
  then mechanically migrate production machines and test fixtures. Pure
  evaluation machines normally open one bounded evaluator scope; effect
  machines may alternate several evaluator scopes with interpreter work.
  Test-only machines which manipulate no values may ignore the context.
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
`parked_machine_contains_no_mutator_authority`, and
`worker_releases_mutator_before_sleep`; retain existing task-order and shutdown
suites. Production remains `NoAuto`.

### Phase I3A.4 — Evaluator and Poll Outcome Ownership Boundaries

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

### Phase I3B.1 — Scoped Construction and Core Evaluator Migration

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
- Keep scheduled-machine paths nonblocking: dependencies return `Blocked`, the
  machine parks after the quantum ends, and another worker may resume it later.
- Ensure budget exhaustion and nested pumping cannot extend an outer mutator
  across a wait. Direct isolated evaluation uses the same step driver rather
  than a separate long-lived authority path.

Verification: injected barriers force busy producers, promises, budget
exhaustion, and patient waits, asserting zero active mutators while sleeping
and successful resumption in a later quantum, including on another worker. Add
`blocked_machine_parks_without_mutator` and retain direct-evaluation result and
failure regressions. Production remains `NoAuto`.

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
with forced-order barriers covering ordinary tasks, client demand, sparks, and
patient pumping. Add `worker_releases_mutator_before_sleep`,
`blocked_machine_parks_without_mutator`, and
`all_poll_routes_use_scheduler_context`. Production remains `NoAuto`.

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
`concurrent_reflection_gate_observers_activate_once` forces the first-observer
race; cancellation before and during activation remains covered. Production
remains `NoAuto`.

### Phase I3D.2 — Effect Evaluation and Interpreter Phases

- Refactor `EffectTask` so a monadic step has explicit phases: evaluate and
  parse the next request in a callback-free evaluator scope; root request data
  which must leave that scope; interpret it with no inherited mutator; then
  enter a later evaluator scope to deliver the result or apply its
  continuation.
- Permit an interpreter callback to request evaluation explicitly through the
  bounded evaluator service. Such a request opens its own scope and returns an
  owned result; the callback never receives or retains the mutator itself.
- Fuse a standard request with adjacent evaluator work only when a pure runner
  could implement it as a deterministic transformation of branch-local state
  and control. The initial candidates are `.r`, `.seq`, `.alt`, `.fail`,
  `.cut`, task-local state, and callback-free reset/shift/fix control. Shared
  heap/volume state, task operations, logging, reflection, and every
  specialized request remain interpreter boundaries.
- Make the unfused phase boundary the reference semantics. Fusion must preserve
  alternative rollback, retry observations, continuation order, and the same
  rooted values as the unfused path.

Verification: callback probes for `TaskHost::{snapshot, commit}` and
`TaskSpecialization::handle_request` observe no active mutator. Run each fused
standard family against a forced-unfused test mode and compare results,
failures, branch order, retry state, and task-local state. An admission counter
may demonstrate reduced scope churn but is not a semantic assertion.
Production remains `NoAuto`.

### Phase I3D.3 — Interaction-Net Claim and Contention Discipline

- Replace the `CoreRuntimeNet` type alias with a private newtype or equivalent
  scoped facade over `SharedRuntimeNet<CoreSpecialization>`. Do not expose the
  wrapped shared net to ordinary core/evaluator callers. Every operation which
  can lock and inspect or mutate core semantic net state must receive a
  same-runtime `RuntimeValueAccess`; identity-only operations which neither
  lock nor inspect managed contents remain outside this rule. This keeps the
  generic interaction-net topology independent of collector policy while
  making core-net access constructively mutator-bound.
- Replace manual `Claimed` bookkeeping at callable active pairs and cursor
  obligations with a bracketed or lifetime-bound claim protocol. A claim is
  confined to one callback-free evaluator scope and must be consumed into an
  exhaustive durable disposition such as resumed, blocked, stable, failed, or
  released. `Drop`/unwind fallback republishes a safe releasable state and a
  disturbance; `#[must_use]` and private constructors supplement but do not
  replace that fallback.
- Prefer a private guard plus a closure returning `CallDisposition` or
  `CursorDisposition`, so ordinary callers cannot store or forget a raw claim.
  If an internal claim token remains useful, bind it invariantly to the
  evaluator-scope lifetime and give it only consuming terminal methods.
- Preserve the existing rule that normalization batches close before Glam
  callable/operator evaluation. Bind every core normalization lease which can
  lock on close or `Drop` to the same access scope, but do not conflate a batch
  lease with an active-pair claim.
- Treat `NetContention::wait_for_disturbance` as a narrow synchronization
  handoff, not a semantic dependency or deadlock edge. It may wait while the
  same-runtime mutator is held only because another active evaluator owns the
  normalization batch or structurally acyclic claim, collection is not needed
  for progress, and that owner must publish a disposition before any semantic
  park. Do not generalize this exception to promises, reflection gates,
  imports, or coordinator waits.
- Keep worker saturation, delayed collection, and contention wake storms as
  profiling/tuning concerns. They do not weaken the no-escaped-claim rule.

Verification: compile-fail or privacy fixtures prevent claims from entering
machine state and poll outcomes, prevent ordinary core-net inspection without
`RuntimeValueAccess`, and prevent the raw shared-net implementation from
escaping its core facade. Forced schedules cover resume, explicit blocked
disposition, failure, cursor completion, unwind fallback, and a contending
evaluator wake. A claim owner forced to encounter a semantic wait must publish
`Blocked` before the machine parks. Preserve the existing pairless-cursor and
contention-order regressions. Production remains `NoAuto`; subsystem-local
closed fixtures may collect.

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

Verification: `reflection_poll_releases_mutator_on_every_callback`,
`net_claim_is_resolved_before_semantic_block`, and
`net_quantum_releases_mutator_on_terminal` cover scheduled reflection,
isolated search, active-pair, cursor, and stuck-net paths. Production remains
`NoAuto`; subsystem-local closed fixtures may collect.

### Phase I3E.1 — Semantic Thunks and Deterministic External Demands

- Split the current generic `DeferredComputation` family by operational role.
  Internal semantic thunks remain evaluator-pure and callback-free, receive
  only scoped evaluation access, and may execute within an evaluator region.
  Module and binary loaders become deterministic external-demand producers:
  forcing one reserves or discovers its demand, exits scoped access, invokes
  the host loader, validates the declared content identity or stable secure
  hash, publishes a rooted result, and resumes evaluation through the demand
  token.
- Reuse the reflection gate's reserve/activate mechanics where helpful without
  describing imports as reflection. Imports are semantically reproducible;
  reflection remains outside reproducibility. Their producer capabilities,
  cache keys, environments, and error contexts stay distinct.
- Inventory any remaining `Fn(&EvalContext)` lazy producer. Classify it as a
  callback-free semantic thunk, an explicit external demand, or a later
  traceable opaque boundary; no arbitrary host callback remains hidden in a
  pure lazy-machine evaluator scope.

Verification: `import_loader_callback_runs_without_mutator`,
`concurrent_import_demands_share_one_verified_result`, and forced local-file
replacement/hash mismatch tests cover deterministic demand. A source-backed
inventory classifies every deferred producer; existing list/conditional lazy
tests establish the semantic-thunk path. Production remains `NoAuto`.

### Phase I3E.2 — Compiler, Macro, and Closed-Value Regions

- Enclose compiler-value bundles, macro lookup/expansion, token searches,
  diagnostic formatter helpers, and recursive module-result construction in
  bounded evaluator scopes. Publish complete cache bundles and suspended
  compiler state using roots only.
- Source loading, recursive loader invocation, diagnostic publication, and
  macro/parser host policy execute outside inherited mutator access. A host
  component may explicitly call the evaluator service, obtaining another
  bounded scope.

Verification: `compiler_suspension_parks_only_roots`,
`compiler_cache_publishes_complete_rooted_bundle`, and macro/import forced
schedules prove callback and wait separation. Production remains `NoAuto`.

### Phase I3E.3 — Event, Diagnostic, and Executable Callback Regions

- Enclose runtime input/output value conversion, diagnostic enrichment,
  contextual composition, and rendering evaluation in explicit bounded
  scopes. Retain the existing ordering in which input conversion precedes
  mutation admission and output decode/adapter callbacks follow guarded
  delivery detachment.
- Release authority before delivery endpoints, diagnostic bus callbacks,
  terminal writers, source systems, and executable policy callbacks. Parked
  host records retain roots, never scoped borrows, poll contexts, or mutators.

Verification: `event_delivery_invokes_callback_without_mutator`,
`diagnostic_rendering_invokes_writer_without_mutator`, and input/output
forced-order tests prove retained values survive while no managed authority
crosses a host boundary. Production remains `NoAuto`.

### Phase I3F — Multi-Runtime and Exit Audit

- A thread entering another runtime activates a separate heap-qualified TLS
  entry; recursive same-runtime entry reuses depth, epoch, and cache.
- Prove opposite A-then-B and B-then-A nesting cannot deadlock when both heaps
  have pending requests. An uncommitted request does not block entry; an active
  exclusive collector does.
- A poll context without an active evaluator scope may orchestrate nested work
  in another runtime. It confers no heap access by itself. Opposite-runtime
  evaluator scopes remain separate and may nest only according to the
  collector's reviewed multi-heap admission protocol.
- Drop every active evaluator scope before a worker sleeps. On worker
  termination, release that thread's inactive collector caches; ordinary
  quantum exit need not discard reusable cursors. TLS eviction forgets cursors
  only; full collection recovers ranges.

Verification: `opposite_runtime_nesting_with_pending_collection_does_not_deadlock`,
`runtime_tls_caches_remain_heap_qualified`,
`poll_context_without_scope_carries_no_heap_authority`, and
`all_managed_entries_have_bounded_mutator_regions`. Glam-level schedules also
cover patient waits, worker termination, and two runtime services nested on
one host thread. Production remains `NoAuto`; passing I3 authorizes managed
access, not production collection.

## Phase I4 — Core Trace Vocabulary and Leaf Policy

I4A-I4E develop and verify the managed shell, exact visitors, and adapters in
closed fixtures. They do not publish a production `core::Value` containing a
bare managed edge. I4F.1 first converts every durable production owner to a
root-safe shape; I4F.2 then switches the root facade and managed value shell
together. This ordering avoids any buildable interval in which production can
park an unrooted `Gc` merely because collection remains disabled.

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

### Phase I4E — Net Value Adapter

- Define the exact non-reducing visitor boundary for `NetValue`, function-stage
  net handles, and the core net payload types selected before I8.
- Do not yet migrate synchronized runtime-net ownership or value-replacing net
  mutations; I8 owns that choice and its gateway inventory.

Verification: `net_value_adapter_traces_without_reduction_or_materialization`
and `net_value_adapter_cycle_marks_exactly`. Production remains `NoAuto`.

### Phase I4F.1 — Durable Root-Surface Conversion Gate

Before the production managed-root switch, complete a source-backed inventory
of every value which can outlive its constructing mutator region. The inventory
is organized by ownership behavior rather than wrapper name and includes:

- canonical values, `CoreValues`, the type-indexed runtime extension cache,
  `GCompilerValues`, and every future compiler attachment;
- parked evaluation/effect machines, task and coordinator records, waits,
  sparks, client demands, deferred producers, failures, and report snapshots;
- reflection environments, volumes, store snapshots, journals, queries,
  transactions, and protocol-machine state;
- diagnostic buses, event inputs, output intents, running deliveries, retained
  delivery failures, and callback-owned payloads;
- assembler/module state, compiler and macro intermediates, origins, import
  demands, configuration/logger state, and other host records;
- synchronized net/work records and any other external Rust owner whose
  contained value is not reached solely through an exact traced edge; and
- every `Any`, opaque, closure, or generic attachment boundary which could
  hide one of the preceding owners.

For every source result, choose exactly one disposition:

1. replace durable raw `core::Value` storage with `RuntimeValueRoot` or the
   selected registered-root equivalent;
2. place the value behind an exact managed edge whose owner and visitor are
   introduced in the same checkpoint; or
3. prove the value is a bounded mutator-local which cannot be parked, erased,
   returned, published, or captured.

Unknown type-erased attachments are not grandfathered. The extension/opaque
registration boundary must either expose an exact rooted family or reject the
payload. Existing traceable interior owners may retain their reviewed internal
shape, but an external Rust owner of a managed edge must use a registered root.

The field/type migration may temporarily use the compatibility
`RuntimeValueRoot` implementation while no production value contains `Gc`.
I4F.2 changes that wrapper and the managed value representation atomically.
There is no permitted buildable state combining a `Gc`-bearing production
value with the old non-registering wrapper.

Verification: latch the source inventory in
`durable_value_owner_inventory_is_complete` and reject every unmatched durable
bare `core::Value`, `Gc`, or type-erased value owner. Closed fixtures for each
owner class construct and publish the owner, leave its construction scope,
immediately request full collection, and prove the retained value survives;
then retire the owner and prove its root registration is released. The common
fixture is named
`durable_root_surfaces_survive_collection_after_construction_scope`.
Production remains `NoAuto`, and this checkpoint does not run collection over
the incomplete production graph.

### Phase I4F.2 — Public-Root Production Switch

- After I3, I4A-I4E, and the I4F.1 durable-owner gate pass, enact I2's selected
  `RuntimeValueRoot` representation and heap-derived provenance together with
  the managed value shell.
- Replace direct `as_core` escapes with scoped access and eliminate
  ownership-taking `into_core` paths which could let an unrooted managed
  pointer escape its region.
- Remove the compatibility facade's direct equality, ordering, hashing, kind,
  extraction, runtime-identity, and content-rendering observations. Route each
  retained semantic operation through the runtime authority selected in I2B.2,
  using whichever call direction that checkpoint found clearest;
  keep `EvaluatedValue` as an opaque WHNF witness and return only owned host
  data from extraction.
- Update each affected stable family/root record in the same checkpoint.

Verification: promote every `prototype_*` I2 fixture to its production
`public_value_*` counterpart, including the compile-time no-equality/no-hash
checks, and rerun the existing public value/factory suite after migrating it to
runtime-mediated observation.
`public_value_switch_inventory_has_no_compatibility_escape` closes the access
inventory. Production remains `NoAuto`; the switch does not authorize
whole-graph collection while I5-I10 families remain unclassified.

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

Migrate the principal cyclic identities first:

- replace `Arc<LazyCell>` and `Arc<PromiseCell>` with managed identity cells;
- retain scheduler/completion host companions as ordinary `Arc` only where
  they carry locks, notifications, task/waiter identities, or other
  coordination data and no `Gc`, `Root`, public `Value`, or equivalent managed
  ownership. Store promise assignments, lazy sources/results, and all other
  logical managed edges in traceable managed cells;
- trace lazy sources, terminal evaluated values, permanent failures, promise
  assignments, and producer-owned data;
- clear/release lazy sources after terminal publication as today;
- route source replacement and terminal assignment through the managed-edge
  mutation gateway; its collector action is empty in full stop-the-world mode
  while preserving an auditable site for separately planned collectors;
- discharge the raw gateway's unsafe same-heap, current-old-edge, and exact-
  replacement obligations inside representation-local safe methods rather
  than exposing raw `Gc` mutation to evaluator callers;
- update the already-exact I4 visitors in the same checkpoint as each edge
  changes from external/`Arc` ownership to `Gc`; no phase may leave a trace
  placeholder merely because collection is disabled; and
- preserve exact wait, cancellation, abandonment, and resolver semantics.

Focused tests must construct and reclaim:

- a direct lazy self-cycle;
- two- and many-lazy cycles;
- promise-to-lazy and lazy-to-promise graphs;
- a resolved promise whose result contains the promise;
- a deferred producer retained by a worker; and
- a terminal value still reachable from another public root.

Use an isolated collector-ready fixture for forced reclamation. The complete
production runtime still does not collect.

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

Verification: source inventory accounts for every old production
`Arc`/`Weak<SharedRuntimeNetInner<CoreSpecialization>>` owner and every durable
core-net handle. Exact-visitor tests cover every runtime-node and pending-state
variant. Mutation tests cover each value-installing rewrite. Compile-time or
privacy fixtures reject unscoped dereference and parked bare handles.
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

I4F.1 performs the structural conversion of durable owners before managed
values can escape. I9 revisits those already-rooted representations after the
I5-I8 interior migrations to verify lifecycle, retirement, and exact ownership.
Discovering a first-time root conversion here is a chronology failure: stop
and repair the checkpoint which first allowed that owner to contain a managed
edge.

### Phase I9A — Runtime Canonical and Compiler Caches

- Audit the explicit external roots or exact managed cache edges installed by
  I4F.1 for runtime canonical values and type-indexed compiler attachments.
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

- Audit the roots installed by I4F.1 for reflection environments, protected
  volumes, store snapshots, views, journals, queries, rewrites, and parked
  protocol machines; update only interior edge representations introduced by
  their owning migration phase.
- Preserve persistent snapshot isolation and the I1B rule that an active store
  or snapshot may retain its authorized construction domain.

Verification: existing persistent-snapshot/query-retirement/volume-revoke
tests plus `reflection_store_roots_survive_owner_close` and
`reflection_snapshot_roots_release_after_snapshot_drop`. Production tests
observe root and snapshot retirement with collection disabled. Any reclamation
assertion uses a closed isolated store fixture; production remains `NoAuto`.

### Phase I9D — Diagnostics and Runtime Events

- Audit the registered roots installed by I4F.1 for diagnostic buses/ingress,
  event inputs, output intents, running deliveries, and retained delivery
  failures.
- Preserve callback-after-lock, exact delivery ownership, fallback routing, and
  settlement activity semantics.

Verification: `output_payload_is_retained_through_callback_and_dropped_after_locks`,
`running_delivery_retains_shared_resources_until_terminal_publication`,
`diagnostic_bus_and_ingress_do_not_retain_the_runtime`, and
`transport_roots_release_after_delivery_and_subscription_retirement`. Only
isolated transport fixtures may collect; production remains `NoAuto`.

### Phase I9E — Assembly, Compiler, and CLI Owners

- Audit the registered roots installed by I4F.1 for assembler/module
  construction state, compiler setup and origins, source/macro intermediates
  which park across evaluation, and binary-owned configuration/logger records.
- Keep bounded locals under I3 regions; only semantically retained values
  become roots.

Verification: existing module build/import, macro protocol, configured CLI,
and logger supervision suites plus
`assembly_compiler_and_cli_roots_release_after_last_owner`. This checkpoint
proves production owner retirement without collection; a closed subsystem
fixture may prove local reclamation where practical. Production remains
`NoAuto`.

### Phase I9F — Runtime-Root Source Inventory

- Re-run the exhaustive source search for core/public values, roots, evaluated
  values, snapshots, diagnostics, type-erased attachments, and parked machine
  fields.
- Match every result to one stable ledger family and named owner. An unmatched
  field blocks I10/Gate G2.
- Compare the result with the latched I4F.1 inventory. A newly discovered
  durable bare value reopens and repairs its earliest managed-edge checkpoint;
  it is not converted opportunistically in I9F.

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

### Phase I10B — Opaque Registration and Provenance

- Keep arbitrary host payloads as tracing barriers. Each admitted family is
  registered as an edge-free token/companion, a genuinely external owner of
  same-runtime public roots, or a private traceable managed representation.
- Forbid bare `Gc<T>`, unrooted recursive core values, foreign roots, and
  equivalent region escapes. Keep opaque construction private and do not
  re-export collector pointers.
- Prefer host-owned side tables for generic embedding payloads; the Glam token
  carries only identity/provenance.

Verification: `opaque_family_inventory_is_reconciled`,
`opaque_registration_rejects_bare_managed_pointer`,
`opaque_registration_rejects_unrooted_core_value`, and
`opaque_registration_rejects_foreign_root`. Production remains `NoAuto`; no
destructor authority is selected yet.

### Phase I10C — Passive Managed Destruction and External Retirement

- Give managed `Drop` no Glam runtime, value-domain, heap, evaluator,
  scheduler, diagnostic, or event capability. The collector-held mutator
  during `Finalizing` is collector coordination state; it is neither passed to
  destructors nor exposed through an ambient/TLS accessor.
- Require every production managed payload, including the transitive `Drop`
  behavior of its fields, to release only ordinary Rust resources. A managed
  `Drop` must not evaluate, allocate managed values, schedule work, publish
  diagnostics/events, or inspect or preserve any `Gc` edge held by the dying
  representation.
- Keep resources that require active cleanup in an external/rooted lifecycle
  record. Its owner performs an explicit, idempotent retirement operation
  while the runtime is live; the record's eventual Rust `Drop` remains
  passive. Not every rooted runtime element needs a managed representation.
- Replace `OpaqueValue::downcast<T>() -> Option<Arc<T>>` for collector-owned
  payloads with a scoped mutator-bound borrow for live access only. Explicitly
  external companions may retain ordinary Rust ownership and public roots,
  but are not finalized as managed graph nodes.
- Treat any future production managed destructor that appears to need runtime
  or heap authority as a new design-review gate. Do not introduce a weak-domain
  capability or TLS bridge as a local exception.

Verification: `managed_drop_has_no_runtime_or_heap_capability`,
`managed_drop_releases_transitive_rust_resources_passively`,
`external_root_owner_retires_before_passive_drop`,
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
  GCI-011's passive managed-destruction boundary.
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
service. It does not itself switch the heap from `NoAuto` to automatic
collection.

Verification: the routine repository checks, `cargo test --workspace -q`, the
aggressive debug-collection suite containing every I11B/I11C named fixture,
focused Miri, supported sanitizers, and a dated Gate G3 review. The heap remains
`NoAuto` until a later I12 policy checkpoint deliberately changes it.

## Phase I12 — Explicit Runtime Maintenance and Threshold Collection

- Expose a narrow embedding maintenance method or runtime tuning policy; do not
  expose raw heap internals.
- Preserve the collector crate's two-level control surface: a nonblocking,
  coalescing request which may be issued before a known batch boundary, and a
  synchronous full-collection operation used only outside an active mutator.
  These are Rust runtime-maintenance controls, not Glam evaluation effects.
- Initially collect at controlled batch/idle boundaries.
- Add allocation-pressure requests from successful typed-run publication which
  are serviced when a later outer mutator entry finds the heap idle. Lease-word
  claims and individual slot allocations remain outside shared pressure
  accounting.
- Count queued and running finalizers as runtime operational activity. A
  readiness probe waits for passive finalization to complete; there is no
  destructor-produced diagnostic, event, or task work to pump.
- Do not begin a requested collection while the heap is in `Finalizing`.
  Requests made before successful completion are heuristic hints coalesced into
  the active collection and are cleared with its pressure baseline; they do not
  queue a second writer or deny fresh mutator admission. A request serialized
  after completion remains latched for a later idle outer entry.
- Ensure a request cannot make a worker spin, hold settlement admission, or
  publish semantic activity merely because collection ran.
- Report metrics for debugging and profiling without making them observable to
  pure evaluation.

Automatic full collection is enabled only after controlled-boundary operation
is stable.

## Phase I13 — Retire Redundant Ownership and Document the Boundary

- Remove `Arc` wrappers whose only remaining role was recursive value
  lifetime. Retain intentional `Arc`s for public roots, immutable leaf buffers,
  host identities, and edge-free scheduler notification companions.
- Remove duplicated runtime provenance fields when heap identity is
  authoritative and the boundary check remains equally cheap.
- Remove temporary collection-disable gates and migration-only adapters.
- Update `docs/architecture/evaluation.md`, `docs/AgentContext.md`, focused
  agent notes, and `src/README.md` with current ownership and safepoint rules.
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
- Every opaque payload is audited to contain no managed edge or only ordinary
  runtime/public roots; no bare collector pointer crosses that boundary.
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
- No pointer-local GC locking or atomic reference count remains on internal
  managed edges.
- Remaining leaks through arbitrary opaque payloads are documented,
  conservative, and never risk premature collection.
