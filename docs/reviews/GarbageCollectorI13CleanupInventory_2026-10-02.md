# Garbage Collector I13 Cleanup Inventory — 2026-10-02

Related plan:
[`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md),
Phase I13A.

Status: **complete; I13A passes.** The inventory found no remaining `Arc`
owner for a recursive production semantic identity and no duplicated
provenance field which can be removed without adding a mutator entry, losing
dead-runtime identity, or retaining a runtime. I13B is therefore a narrow
scaffolding/terminology cleanup rather than another ownership migration.

## Evidence Reused

I13A deliberately reuses the existing authoritative inventories instead of
creating another repository-wide scanner:

- `recursive_identity_source_inventory_is_complete` owns the lazy, promise,
  and core-net identity census;
- `semantic_recursive_facades_are_managed_edges_only` forbids roots,
  observers, IDs, labels, `Arc`, and `Weak` from semantic façades;
- `durable_recursive_owner_copies_have_proven_roles` owns the copied lazy and
  promise scheduler/diagnostic fields;
- `durable_value_owner_inventory_is_complete` and
  `runtime_root_source_inventory_is_reconciled` own durable roots;
- `managed_graph_reaches_no_active_raii_owner` and
  `managed_payloads_have_no_strong_value_domain_backedge` own active
  destruction and runtime-retention closure;
- `persistent_edge_trait_occurrence_inventory_is_complete` owns the
  persistent `Gc` edge surface;
- `raw_core_value_api_inventory_is_complete` owns unrooted compatibility
  values and access qualification; and
- the containment and opaque-family inventories own callback/type-erasure
  boundaries.

These latches are independently supported by cycle reclamation, passive
finalization, aggressive-entry, and whole-production-graph collection tests.

## Remaining Ownership Classification

| Representation | Stable role | I13 disposition |
| --- | --- | --- |
| `LazyValue`, `PromisedValue`, `CoreRuntimeNet` | one exact `Gc` semantic edge | Final; no reference-counted recursive owner remains. |
| `ManagedLazyRoot`, `ManagedPromiseRoot`, `ManagedCoreNetRoot`, public/runtime value roots | registered external collector roots | Retain. They cross orchestration or embedding boundaries where a bare `Gc` is invalid. |
| lazy-source payloads, failures, labels, captured argument/key arrays, persistent list/dict structure | immutable structural sharing | Retain. The `Arc` supplies constant-time shell duplication, not recursive identity lifetime; the compatibility walker traces through it to exact managed edges. Value Representation Refinement may later replace these shells. |
| promise terminal bit, completion subscriptions, producer publication cell | edge-free scheduler/notification sidecars shared by cell and durable roots | Retain. Coordinator-locked operations must inspect them without opening a mutator, and they cannot reach semantic values or registered roots. |
| runtime-net disturbance, work coordinator, demand state, task profiles and activation permits | edge-free scheduling/notification ownership | Retain. They coordinate host threads and wakeups rather than keep semantic graph nodes alive. |
| host-call operation, opaque external owner registry, source/loader callbacks | host identity/resource ownership | Retain outside the managed graph. Semantic captures remain explicit traced values or registered roots. |
| generic `SharedRuntimeNet<S>` | non-core generic external-owner API | Retain. Core specialization embeds the owner-neutral `RuntimeNetCell` directly in `ManagedCoreNetCell`; it does not use this `Arc` owner. |

No item remains in the prohibited category “`Arc` whose only purpose is to
keep a recursive Glam value alive.” Removing any retained item above would
either copy immutable structures eagerly, move scheduler coordination under a
mutator, or erase a real external owner.

## Provenance and Identity Copies

| Copy | Why heap identity alone is insufficient at that boundary | Disposition |
| --- | --- | --- |
| `RuntimeValueObserver::{runtime, domain: Weak<_>}` | the scalar ID remains available after domain retirement; the weak route enables ergonomic bounded re-entry without retaining the heap; inline values have no collector root | Retain both fields. |
| `PreparedRuntimeValueRoot` observer beside an inline value or managed root | runtime-owned/public wrappers need access-free runtime validation and later bounded projection; moving the observer only into the inline arm would break observer projection for managed values | Retain. |
| `ManagedWhnfRoot::runtime` | the scheduler routes and rejects work before opening evaluator access; root heap identity is checked again during projection | Retain the cheap scalar precheck. |
| lazy root ID/label | work indexing and dependency-cycle diagnostics occur without managed access | Retain only on the durable root, as already source-latched. |
| promise root ID and sidecars | task/completion indexing and subscribe/recheck occur under coordinator locks without managed access | Retain only the already-latched fields. |
| `HostCallRootBundle::runtime` | an empty capture bundle still needs same-runtime callback validation and must not carry a value factory/mutator | Retain. |
| coordinator, wait, task, event and endpoint runtime IDs | cross-session routing and foreign-runtime rejection happen outside managed access | Retain as routing keys, not ownership. |

There is consequently no provenance deletion whose replacement is equally
cheap and equally available. Pointer/heap identity remains authoritative once
managed access is open; scalar IDs remain routing and diagnostic data outside
that region.

## Gates and Adapters

- `CollectionPolicy::NoAuto` is production policy, not a temporary disable
  gate. Stable pump/service owns collection placement.
- `gc_activity_for_entries` and the pre-entry collection hook are
  test/feature-only aggressive verification. They must remain so ordinary and
  collecting entries exercise the same managed graph.
- `CompatibilityValueEdges` and the central compatibility trace walk are the
  exact bridge from current immutable aggregate shells to managed identity
  leaves. They remain until a later value-representation transition replaces
  those aggregate shells with direct managed nodes.
- The external-owner registry is required because callbacks and opaque host
  resources may have active retirement; managed destructors remain passive.
- Test-only direct collection and passive compatibility-value fixtures remain
  independent verification surfaces. Their phase-specific names and comments
  may be made stable in I13B, but their evidence must not be removed.

No production collection-disable gate or migration-only owner adapter remains
eligible for deletion in I13B.

## `ManagedDropRecord` Decision

Retain the complete textual `ManagedDropRecord`, not merely an empty token.
`Trace` proves edge enumeration but says nothing about destructor behavior.
The private `ManagedFamily` bound plus its mandatory record forces every new
allocation family to name:

1. the stable representation family;
2. its owning source;
3. direct destruction behavior; and
4. transitive destruction behavior.

The record materially supports source-backed audits and makes active runtime,
callback, or `Gc` observation from `Drop` a reviewed exception rather than an
accidental implementation detail. I13B will remove stale phase language from
the constructor/comments and fixture records while preserving this contract.

## I13B Authorized Changes

I13B may:

- remove obsolete `allow(dead_code)` attributes from now-live production
  gateways;
- replace phase-history reasons on authority/lifetime fields with stable
  invariant explanations;
- rename test-only compatibility/finalization fixtures and record labels to
  describe their continuing verification role; and
- refresh managed-cell/value-node comments to current ownership rather than
  migration chronology.

I13B may not remove or change a semantic owner, provenance check, pressure or
aggressive-verification gate, compatibility trace edge, external owner, or
drop-admission record without reopening I13A and the earlier owning phase.

