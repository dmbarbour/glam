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
| I5 | pending | managed lazies and promises |
| I6 | pending | functions, applications, metadata, failures |
| I7 | pending | persistent list and dictionary tracing |
| I8 | pending | interaction-net tracing and mutation gateways |
| I9 | pending | runtime-owned root surfaces |
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
the public representation. A wrapper may retain a runtime ID for diagnostics
or API compatibility, but live heap identity is the authoritative provenance
for managed access.

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

- public value clone, equality, and cross-runtime rejection;
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

- Resolve GCI-004, then prototype C4's direct managed root against scalars and
  one recursive test node. Separately prototype a Glam-owned public wrapper
  whose managed arm uses that root and whose optional inline arm contains only
  values which require no managed trace. Do not add another collector
  registry-entry or root-cell representation for the wrapper.
- Select between exposing the direct root and using the Glam wrapper based on
  public scalar construction cost and clarity. One public `Value` clone must
  preserve its root cell while the value domain lives, but must not preserve
  the heap after the domain's authorized owners are dropped.
- Derive authoritative managed provenance from root heap identity rather than
  an independently forgeable runtime ID. Every safe access performs the
  release-build same-heap check before the private typed-pointer gateway.

Verification: `prototype_root_moves_between_threads`,
`prototype_root_rejects_another_heap`, `prototype_root_becomes_inert_after_domain_drop`,
and the collector's existing mismatched-`Root::get` invariant test. Only the
isolated prototype heap may collect; production remains `NoAuto` and retains
its compatibility `RuntimeValueRoot`.

### Phase I2B — Inert Observation, Equality, and Extraction

- Resolve GCI-002's post-domain behavior for equality, debug/kind, and borrowed
  extraction. Preserve current structural equality while the domain lives;
  root-pointer identity is not semantic equality and debug remains
  non-forcing.
- Review reference-returning public extractors. Managed borrows must be tied to
  a live matching mutator/access scope, while owned extraction results may
  outlive it. Do not manufacture a hidden domain lease merely to preserve an
  old borrowed-return signature.
- Keep `api::Value` freely cloneable and `Send + Sync` when its contents are,
  and preserve `EvaluatedValue` as the sole WHNF witness rather than a second
  root model.

Verification: `prototype_values_preserve_live_structural_equality`,
`prototype_value_debug_and_kind_are_non_forcing`,
`prototype_borrowed_access_requires_live_domain`, and
`prototype_owned_extraction_outlives_domain` cover the selected live/inert
semantics. Only isolated prototype fixtures may collect; production remains
`NoAuto`.

### Phase I2C — Scoped-Access and Production-Switch Inventory

- Prototype scoped core access under the I3 authority shape and inventory every
  production `as_core`/`into_core` call site which I4 must migrate.
- Inventory constructor, composite-validation, storage, evaluator, reflection,
  diagnostic, and binary-extraction paths which assume the compatibility
  wrapper.
- Fix the selected public wrapper/provenance contract, but do not place
  production `core::Value` in a collector root until I4 supplies its exact
  trace and I4F performs the switch.

Verification: the complete existing public `Value` suite stays unchanged;
`prototype_value_access_nests_in_one_mutator` exercises recursive entry; and
`public_value_compatibility_access_inventory_is_complete` assigns every access
to a later migration checkpoint. Production collection remains `NoAuto`.

## Phase I3 — Mutator Regions Across Evaluation and Construction

### Phase I3A — Scoped Authority Carrier

- Resolve GCI-006 by defining one lifetime-bound internal evaluation-quantum
  carrier which borrows `Mutator<'heap>` and cannot be stored in a parked
  `EvalContext`, task machine, coordinator record, or public API value.
- Make the quantum carrier the explicit authority passed to allocation and
  managed access. A checked TLS convenience may locate an already active
  same-heap region, but it cannot manufacture authority or permit unguarded
  dereference.
- Define the poll/dispatch boundary which reconstructs that carrier around one
  machine quantum without changing the persistent `EvaluationTaskMachine`
  representation.

Verification: compile-time non-escape checks plus
`evaluation_quantum_reuses_recursive_same_heap_entry`,
`parked_machine_contains_no_mutator_authority`, and
`different_heap_authority_is_rejected`. Production remains `NoAuto`; no
production representation is collected.

### Phase I3B — Construction and Synchronous Evaluation Regions

- Enclose public `Values` construction/composition, evaluator demand, WHNF
  extraction, and direct isolated evaluation in one outer region per operation.
- Nested helpers reuse the current same-runtime region and heap-qualified TLS
  cursor cache rather than entering for every pointer access or allocation.
- Complete payload and allocation-bit initialization before returning a managed
  pointer, never at outer-region exit.

Verification: `recursive_construction_reuses_one_mutator`,
`composite_construction_preserves_provenance_errors`, and
`owned_extraction_survives_mutator_exit`. Production remains `NoAuto`;
isolated closed fixtures may force collection.

### Phase I3C — Cooperative and Worker Quantum Regions

- Wrap exactly one cooperative or worker-owned machine poll/reduction quantum.
- Release the region before sleeping, waiting for work, publishing host
  callbacks, or parking a machine in coordinator state.
- Make every worker collection request observable at a bounded quantum
  boundary without adding a mutator to scheduler records.

Verification: extend `workers_force_sparks_and_poll_ready_reflection_tasks`
with barriers proving a worker owns authority only while polling; add
`worker_releases_mutator_before_sleep` and
`blocked_machine_parks_without_mutator`. Production remains `NoAuto`.

### Phase I3D — Reflection and Interaction-Net Regions

- Enclose reflection-machine polling/request interpretation and each
  interaction-net call/reduction entry in the current quantum's authority.
- Prove semantic net/store locks do not escape the authorizing region and no
  callback or wait occurs while both are held.
- Keep net-lock trace policy deferred to I8/GCI-008; this checkpoint establishes
  only the mutator and lock lifetime relation.

Verification: `reflection_poll_releases_mutator_on_every_exit` and
`net_quantum_releases_mutator_on_block_or_terminal` cover scheduled
reflection, active-pair, cursor, and stuck-net paths. Production remains
`NoAuto`; subsystem-local closed fixtures may collect.

### Phase I3E — Compiler, Event, and Diagnostic Regions

- Enclose compiler/macro closed-value construction, runtime input/output
  encoding and decoding, diagnostic enrichment, and rendering access.
- Release authority before invoking user callbacks, delivery endpoints, source
  systems, or terminal writers. Parked host records retain roots, never scoped
  borrows or mutators.

Verification: `compiler_suspension_parks_only_roots`,
`event_delivery_invokes_callback_without_mutator`, and
`diagnostic_rendering_invokes_writer_without_mutator` prove retained values
survive while no mutator crosses a host boundary. Production remains `NoAuto`.

### Phase I3F — Multi-Runtime and Exit Audit

- A thread entering another runtime activates a separate heap-qualified TLS
  entry; recursive same-runtime entry reuses depth, epoch, and cache.
- Prove opposite A-then-B and B-then-A nesting cannot deadlock when both heaps
  have pending requests. An uncommitted request does not block entry; an active
  exclusive collector does.
- On outer exit, make the thread cache quiescent before the worker may sleep or
  service another runtime. TLS eviction forgets cursors only; full collection
  recovers ranges.

Verification: `opposite_runtime_nesting_with_pending_collection_does_not_deadlock`,
`runtime_tls_caches_remain_heap_qualified`, and
`all_managed_entries_have_bounded_mutator_regions`. Production remains
`NoAuto`; passing I3 authorizes managed access, not production collection.

## Phase I4 — Core Trace Vocabulary and Leaf Policy

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
  thunks, shared slices, and mapped values. Duplicate traversal of shared spines
  is correct but measured; do not fork or replace persistent collections.
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

### Phase I4F — Public-Root Production Switch

- After I3 and I4A-I4E pass, enact I2's selected `RuntimeValueRoot`
  representation and heap-derived provenance.
- Replace direct `as_core` escapes with scoped access and eliminate
  ownership-taking `into_core` paths which could let an unrooted managed
  pointer escape its region.
- Update each affected stable family/root record in the same checkpoint.

Verification: promote every `prototype_*` I2 fixture to its production
`public_value_*` counterpart and rerun the existing public value/factory suite.
`public_value_switch_inventory_has_no_compatibility_escape` closes the access
inventory. Production remains `NoAuto`; the switch does not authorize
whole-graph collection while I5-I10 families remain unclassified.

From I4 onward, every representation change in I5-I10 updates its exact visitor
or root classification in the same checkpoint. Collection being disabled is
never permission to carry an incomplete unsafe `Trace` implementation.

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
tests retain identity behavior. Production remains `NoAuto`.

### Phase I6B — Metadata Identity

- Migrate `MetadataCarrier` identity and its exact one-edge visitor so metadata
  can participate in cycles without leaking.
- Preserve sealing, reorder/copy, `seq`, `spark`, and reflection-inspection
  semantics without exposing metadata to pure evaluation.

Verification: `metadata_and_collections_can_participate_in_a_deferred_value_cycle`,
the metadata update reorder/copy tests, and
`managed_metadata_cycle_reclaims_in_isolated_heap`. Production remains
`NoAuto`.

### Phase I6C — Failures and Context Frames

- Migrate evaluation failures and context frames, visiting emission/context
  values without evaluating, formatting, or locking them.
- Preserve shared failure identity and structured diagnostic projection.

Verification: `managed_failure_context_cycle_reclaims`, the existing
structured-failure suite, and `failure_trace_invokes_no_semantic_service`.
Production remains `NoAuto`.

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
- In an isolated collector-ready fixture, verify a public persistent collection
  retains all contained managed objects across full collection and dropping its
  final external root permits a backedge cycle to be reclaimed.
- Measure duplicate trace work for heavily shared versions and record a
  threshold for revisiting collector-aware physical nodes.

Logical duplicate visits are a performance issue in a mark collector, not an
edge-counting soundness problem. This phase must not silently turn collection
updates into whole-map copies. Production remains `NoAuto`; I11 repeats these
reclamation cases only after Gate G2 closes the whole graph.

## Phase I8 — Interaction-Net Migration and Trace Audit

- Reconcile I0/I4's inventory of every core value stored in net templates,
  agents, active pairs, stuck pairs, cursors, logical copies, and normalization
  state against the concrete migration.
- Make `NetValue`, function stages, and `SharedRuntimeNet` expose those edges
  without reducing the net or materializing a cursor.
- Decide whether the shared runtime net remains an external synchronized
  allocation traced under a stopped mutator world or becomes a managed outer
  node. Do not rewrite generic topology merely for GC aesthetics.
- Require all net mutation which can replace a managed value edge to use the
  mutation gateway. It is a no-op for the full collector; future moving or
  concurrent collectors may extend it under their own plans.
- Preserve the Cursor-WHNF ownership and normalization-batch invariants.

Because collection waits for all mutators, acquiring a net lock while tracing
should be unnecessary. Treat a still-held net lock at collection as an
invariant defect rather than using a lossy `try_lock` trace.

Verify cycles through net `Data`, shared function stages, cursor
materialization, stuck nets, and values reachable only from pending active-pair
work. Forced collection is limited to an isolated collector-ready net fixture;
production remains `NoAuto` until I11.

## Phase I9 — Runtime-Owned Root Surfaces

### Phase I9A — Runtime Canonical and Compiler Caches

- Convert runtime canonical values and type-indexed compiler attachments to
  explicit external roots or managed cache edges.
- Close the `Any` registration boundary: each extension family has a stable
  ledger record and cannot retain a factory/domain backedge.

Verification: existing cache initialization/race tests,
`compiler_cache_does_not_form_a_value_domain_cycle`, and
`runtime_cache_roots_release_after_last_domain_owner`. Production remains
`NoAuto`.

### Phase I9B — Coordinator and Evaluation Ownership

- Convert client demands, sparks, tasks, deferred producers, waits, failure
  ledgers, task handles, and promise resolver state.
- Do not root a value merely because an edge-free notification companion names
  its task/wait ID. Ownership follows semantic retention, not scheduler
  reachability.

Verification: `blocked_machine_context_does_not_retain_its_owner_lease`,
`task_handle_acknowledges_terminal_failure_after_owner_lease_closes`,
`task_handle_cancellation_is_harmless_after_owner_closure`, and
`coordinator_roots_release_after_terminal_record_retirement`. Use isolated
closed coordinator fixtures for reclamation; production remains `NoAuto`.

### Phase I9C — Reflection Store and Protocol Roots

- Convert reflection environments, protected volume roots, store snapshots,
  views, journals, queries, rewrites, and parked protocol machines.
- Preserve persistent snapshot isolation and the I1B rule that an active store
  or snapshot may retain its authorized construction domain.

Verification: existing persistent-snapshot/query-retirement/volume-revoke
tests plus `reflection_store_roots_survive_owner_close` and
`reflection_snapshot_roots_release_after_snapshot_drop`. Production remains
`NoAuto`.

### Phase I9D — Diagnostics and Runtime Events

- Convert diagnostic buses/ingress, event inputs, output intents, running
  deliveries, and retained delivery failures.
- Preserve callback-after-lock, exact delivery ownership, fallback routing, and
  settlement activity semantics.

Verification: `output_payload_is_retained_through_callback_and_dropped_after_locks`,
`running_delivery_retains_shared_resources_until_terminal_publication`,
`diagnostic_bus_and_ingress_do_not_retain_the_runtime`, and
`transport_roots_release_after_delivery_and_subscription_retirement`. Only
isolated transport fixtures may collect; production remains `NoAuto`.

### Phase I9E — Assembly, Compiler, and CLI Owners

- Convert assembler/module construction state, compiler setup and origins,
  source/macro intermediates which park across evaluation, and binary-owned
  configuration/logger records.
- Keep bounded locals under I3 regions; only semantically retained values
  become roots.

Verification: existing module build/import, macro protocol, configured CLI,
and logger supervision suites plus
`assembly_compiler_and_cli_roots_release_after_last_owner`. Production remains
`NoAuto`.

### Phase I9F — Runtime-Root Source Inventory

- Re-run the exhaustive source search for core/public values, roots, evaluated
  values, snapshots, diagnostics, type-erased attachments, and parked machine
  fields.
- Match every result to one stable ledger family and named owner. An unmatched
  field blocks I10/Gate G2.

Verification: `runtime_root_source_inventory_is_reconciled` plus controlled
owner-drop tests named by every root family. Production remains `NoAuto`; this
checkpoint proves root classification, not whole-graph reclamation.

## Phase I10 — Deferred Closures and Opaque Boundaries

### Phase I10A — Deferred Closure Containment

`Arc<dyn Fn(&EvalContext) -> ...>` cannot be traced automatically.

- Reconcile every production deferred constructor with I4B and replace
  Glam-owned value captures with explicit traceable computation state wherever
  they can participate in the managed graph.
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

### Phase I10C — Scoped Opaque Access and Finalization Authority

- Resolve GCI-011 before implementation, selecting a finalizer-safe weak domain
  capability or narrowly scoped TLS bridge without a managed
  `payload -> domain -> heap` ownership cycle.
- Put collector-owned drop-bearing payloads in typed runs with erased `Drop`.
  Destruction runs outside collector locks during `Finalizing` with the
  collector-installed mutator, but cannot root or observe the allocation whose
  `Drop` is running. A fresh equivalent is a new identity, not resurrection.
- Destruction timing and ordering remain outside Glam evaluation semantics.
  The scoped authority may allocate, evaluate, schedule work, and emit
  diagnostics subject to the same no-resurrection and ownership rules.
- Replace `OpaqueValue::downcast<T>() -> Option<Arc<T>>` for collector-owned
  payloads with a scoped mutator-bound borrow. Explicitly external companions
  may retain ordinary Rust ownership only when they contain no managed edge
  and do not require collector finalization.

Verification: `opaque_drop_runs_with_scoped_runtime_authority`,
`opaque_drop_during_domain_teardown_fails_harmlessly`,
`opaque_drop_can_publish_diagnostics_tasks_and_fresh_identity`, and
`opaque_drop_panic_retries_untouched_suffix`. Use an isolated closed
runtime/opaque fixture; production remains `NoAuto`.

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
  boundaries. Resolve GCI-007 through GCI-009 and GCI-011 before certification.
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
- Finalize opaque payloads while logger supervision and workers are active.
  Pump destructor-produced diagnostics/tasks before reporting quiescence, and
  preserve a freshly published equivalent independently of the dead identity.
- Request collection from finalizer work and prove coalescing avoids recursive
  collection or an immediate second pass. Let a finalizer wait for another
  runtime worker while a request is concurrent, proving a heuristic request
  does not deny that worker entry.
- Exercise runtime drop before and after collection.

Verification: `collection_interleaves_with_worker_quantum_without_lost_work`,
`finalizer_work_coalesces_collection_request`, and
`finalizer_waits_on_other_runtime_without_request_deadlock`, followed by
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
  readiness probe must pump consequences of finalizer diagnostics, event
  output, and newly launched tasks before returning a stable report.
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
- Public `Value` is a real runtime-local external root and remains convenient
  to clone and share.
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
