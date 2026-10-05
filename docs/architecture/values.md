# Managed Value Architecture

How the bootstrap represents, owns, traces and collects values (`core.rs`,
`core/`, `runtime.rs`, `api/value.rs`, `api/runtime.rs`). See also
[`evaluation.md`](evaluation.md) (flow; "Collector Boundary"),
[`../agent_context/evaluation.md`](../agent_context/evaluation.md) (hazards)
and [`SAFETY.md`](../../crates/glam-gc/SAFETY.md) (collector contracts).

The collector is exact, non-moving, stop-the-world and full-heap. Each runtime
owns one heap, no managed edge crosses heaps, and roots are explicit.

## Terms

| Term | Meaning |
| --- | --- |
| value domain | `RuntimeValueDomain`: one runtime's heap, IDs, cache, external owners and weak coordinator route. |
| mutator | A thread's admission to one heap. A heap cannot collect while any mutator is active. Same-heap re-entry reuses the outer admission. |
| access region | One `with_runtime_value_access` call: a mutator plus the factory view that opened it, carried as `RuntimeValueAccess<'scope>`. Callback-free and thread-bound. |
| regional | Valid only inside a region: bare `Gc<T>`, raw `core::Value`, `Managed*Access` views, evaluator `Regional*` state. It outlives the region only beneath a traced owner. |
| durable / publish | Durable means rooted or beneath a traced owner. Publishing makes a fresh allocation durable before its region ends. |
| publication nursery | `EvaluatorStepContext`'s temporary family roots, dropped by `finish` after the step publishes. Only test constructors use it; production publishes within one region. |
| owner lease | An `EvaluationSession`, the external owner of a demand session; not a GC activity lease or an external-owner lease. |
| compatibility | The Rust-owned aggregate shells inside `core::Value`; see "Compatibility Shells". |

"Purity" means three different things: *semantic* (result fixed by declared
inputs), *evaluator* (no Glam-observable effect, though it may suspend on a
promise or import), and *operational callback-freedom* (this Rust step calls
no callback and never blocks). Only the last permits one continuous region; a
machine poll may open several.

## Representation

```text
api::Value ─ RuntimeValueRoot ─ observer (runtime ID, Weak<domain>)
                               + InlineInteger(i64) | Root<ManagedValueNode>
core::Value: Atom Number Binary Builtin          leaves in the shell
             List Dict PartialBuiltin Metadata   compatibility shells
             Lazy Promised Function Net          one Gc edge each
             Opaque                              ExternalOwnerHandle
```

- Only integers fitting `i64` are inline; atoms, unit and ratios are managed.
  This is conservative, not the eventual tagged word.
- Inline provenance is the weak domain witness; managed provenance is root
  heap identity. Neither keeps the domain alive.
- Every managed node fits one slot of a 64 KiB typed run: no large-object
  path, multi-run span or DST. `managed_slot_extent::<T>()` is the one size
  policy. Core cells and facades latch their x86-64 layouts in `const` asserts.

## Public Value Contract

`api::Value` is an opaque transport handle: clone (sharing one root
registration), drop, `Send`, `Sync`. It has no equality, ordering or hash
traits (compile-time latched), and its `Debug` prints only `Value`. Kind,
contents and comparison need a live matching runtime: `ValueEvaluator::eval`,
`EvaluatedValue` extractors, `ReflectionInspector`. `same_representation` is
representation identity, not Glam equality. `EvaluatedValue` adds an
outer-WHNF witness and a weak observer; extraction returns owned host data and
fails after teardown. Hosts derive map keys from extracted data.

Why: roots are weak to the heap, so nothing could answer equality after
teardown, and keeping the heap alive per handle would defeat teardown.

## Value Domain and Ownership

`CoreValueFactory` is an `Arc<RuntimeValueDomain>` plus an optional
compilation-local cache view (`scoped()`). `CoreValueFactory::new` is the only
production heap constructor.

| Holder | Domain ownership |
| --- | --- |
| runtime shared resources, `Values`, factories, `EvalContext`, compiler contexts, `ReflectionStore` | strong |
| `Value`, `EvaluatedValue`, `RuntimeValueRoot`, `Root<T>` | weak |
| managed nodes, opaque payloads, cache entries | none (would cycle through the domain) |

The domain reaches the coordinator only weakly, so a retained factory keeps
construction usable without keeping execution alive. When the last strong
holder drops, the domain retires and its roots become inert.

## Access Regions and Publication

- `with_runtime_value_access` is the only production heap admission; a latch
  confines `Heap::with_mutator` to `core/managed.rs`. Its higher-ranked
  callback keeps the carrier, allocators and borrows from escaping.
- Inside, code allocates, roots, borrows, duplicates or compares edges, and
  changes a published owner's edges via `with_managed_edge_transition`.
- **Allocation chronology.** No collection starts while a mutator is active,
  so fresh allocations live until the region ends. Each survivor must first be
  installed beneath a traced owner or rooted. `construct_runtime_value_root`
  roots the result; its fallible form roots nothing on `Err`.
- No constructor opens its own region and returns a fresh `Gc`, facade or raw
  value. A root marks a real handoff (wait, callback, coordinator, lock),
  never adjacent statements.
- Locks, waits, callbacks, delivery, sleeps and wakes (such as
  `ManagedPromisePublication`) happen after the region closes.

## Managed Families

`ManagedFamily: Trace` is the private unsafe admission every allocator
requires. `Trace` does not constrain `Drop`, so a family also supplies a
`ManagedDropRecord` (see the admission table below). Destruction is passive:
it may release Rust resources but never reach a runtime, heap, scheduler,
service or callback, or touch a `Gc` it holds. Active cleanup belongs to an
external owner.

Ownership classes: *managed node* (traced); *external root owner* (outside the
graph, deliberately extends rooted lifetimes); *companion* (edge-free
coordination state with no `Gc`, root or public value, so it keeps nothing
alive); *transient* (bounded owner that borrows only inside a region and roots
whatever it keeps across regions or callbacks); *external leaf* (no edge).
Any other holder is a defect: remove it or make it an exact same-runtime
root. Never hold an internal lazy, promise or fixpoint edge through a root:
it hides the cycle.

| Family | Source |
| --- | --- |
| `ManagedValueNode` | `core/managed/value_node.rs` |
| lazy, promise and core-net cells | `core/managed/recursive_cells.rs` |
| `ManagedLazyCheckpointCell` (WHNF state) | `eval/whnf/managed_state.rs` |
| six typed lazy-producer checkpoints | `eval/lazy_checkpoint.rs` |
| list-front and key-conversion checkpoints | `eval/list_machine.rs`, `eval/access_machine.rs` |

Admitting a family settles each concern below. A family is identified by name,
Rust type and source path, never by `TypeId`, metadata address or class ID,
which vary by process or heap.

| Concern | Settled by |
| --- | --- |
| identity | `ManagedDropRecord` `family` and `source` |
| edges | its `unsafe impl Trace`, reporting every direct edge |
| layout | `managed_slot_extent::<T>()`; `const` latches on core cells and facades |
| destruction | the record's direct and transitive reviews (`no_drop` or `passive`) |
| mutation | immutable (`ManagedValueNode` and its shells), one-write (lazy result, promise assignment), or replaceable under the owner's mutex (lazy producer slot, checkpoint state, net topology); every change after publication is one owner-qualified transition |
| no edge hidden behind a root | the `core/managed/managed_boundary_audit.rs` source audit |
| evidence | survival across a forced collection; cycle-reclamation fixtures for the core cells |

## Recursive Identities

Lazies, promises and core nets are the only mutable shared identities, so only
they close cycles; functions do so through their net. Each semantic facade
(`LazyValue`, `PromisedValue`, `CoreRuntimeNet`) is one `Gc` edge. Durable
owners hold `Managed{Lazy,Promise,CoreNet}Root`, which copy only the IDs,
labels and edge-free promise sidecars read without access.

- A lazy cell holds a producer slot (source, checkpoint or panic report) under
  a mutex and a one-write result, published before the producer is released.
- A promise cell holds a one-write assignment; its sidecars and producer
  record hold no root and reach the coordinator only weakly.
- A net cell embeds `RuntimeNetCell`; edits report exact leaving and adding
  edges through `RuntimeNetMutationGateway` under the net mutex.
- Every post-publication edge change is an owner-qualified transition. Its
  barrier is a no-op under stop-the-world; the site awaits a concurrent
  collector.
- `ReflectionComputation` keeps effect, target and completion promise as
  edges. The collector has no weak pointers.

Handoffs and `PromiseResolver`: [`evaluation.md`](evaluation.md) "WHNF
Submachine Flow".

## Tracing

The collector marks with its own worklist; each `trace` reports direct edges
via `core/managed/payload_edges`.

- `CompatibilityValueEdges` lists a shell's direct `Value` children without
  evaluating, formatting, comparing or forcing.
- `visit_compatibility_managed_edges` composes them through nested shells and
  stops at the first `Lazy`, `Promised`, `Function` or `Net`, reporting its edge.
- Lists and dictionaries use their iterators (`Concat` via a worklist); keys
  are leaves. Nets use `try_visit_logical_payloads`, which never reduces.
- Mutex-guarded cells trace via `try_lock`: contention is an invariant
  failure, and poison is read through so no edge is lost.

Cost scales with logical references, not allocations: shared shells and
spines are revisited per occurrence, and the shell walk recurses on nesting.

## Compatibility Shells

"Compatibility" names the Rust-owned parts of `core::Value` that predate the
collector: list and dictionary structure, builtin arguments, metadata, lazy
sources, failures and contexts. It means neither deprecated nor unchecked; it
is a seam that Value Representation Refinement replaces.

- Shells are immutable, and acyclic once lazies, promises and nets are removed.
  Never add interior mutability or recursion to one.
- The central walk is their exact trace, not scaffolding.
- A raw `core::Value` is regional, with no `Clone`, `Eq` or `Debug`;
  `duplicate_value` copies it under access.

An adjacent `Arc` must be structural sharing, a registered root, an edge-free
sidecar (read under coordinator locks without a mutator), or a host resource.
One that only keeps a recursive value alive is a defect. Copied IDs, observers
and labels serve routing and diagnostics without a mutator; heap identity
governs once access is open.

Convert a shell only for a recorded benefit (a real root backedge, a measured
cost, or a boundary the refinement needs), with a full family admission.
Metadata and failures were audited and left as shells.

## Opaque Values and Host Calls

`OpaqueValue` holds an `ExternalOwnerHandle` (runtime, ID, lease); the payload
lives in the domain's `ExternalOwnerRegistry`.

- `OpaquePayloadFamily` records each family as `edge_free` (compilation
  origins, net construction tokens) or `external` (effect tokens, task
  handles). No payload directly holds a `Gc`, raw value or root.
- `downcast` checks runtime, lease and family and returns an owning `Arc<T>`,
  sound only because payloads are external. Glam equality compares owners.
- A dropped last lease retires the entry. `drain_retired` detaches retired
  entries under the lock and drops them after unlocking, catching destructor
  panics. Production drains only in `HostCallProducer::invoke`, else at
  teardown.
- Collection never retires an external owner, so a task result holding its
  own handle survives until runtime teardown (accepted).

`HostCallProducer` traces its Glam captures and keeps its closure in the
registry. Invocation roots the captures into a `HostCallRootBundle`, closes the
region, then calls the closure.

## Durable Roots and the Runtime Cache

Roots: `RuntimeValueRoot` (waits, tasks, events, store, cache, public values);
`RuntimeFailureRoot` (shared failure plus one root per direct value);
`Managed*Root`; `ManagedWhnfRoot`. A root registers a weak cell; clones
share it, and collection prunes it after the last clone drops.

The cache (`core/runtime_cache.rs`, `CoreValueFactory::cached`) holds canonical
`CoreValues` and type-indexed compiler entries (`GCompilerValues`,
`CachedDiagnosticFormatter`).

- The unsafe `RuntimeCacheFamily` requires a record (`value_free` or
  `same_runtime_roots`) and a visitor over every retained root. Admission
  rejects foreign roots; lookup rechecks the record.
- Builders run outside the cache mutex and any region; racing builders may
  both run, and one complete entry wins.
- Cache roots live as long as the domain. Statics hold only `Key`s.

## Collection Policy and Maintenance

Every heap is `NoAuto`, fixed at construction and never configurable
(`api/runtime/gc_activity_inventory.rs`). Ordinary access never collects or
publishes runtime state. Allocation latches pressure at a high-water mark
derived from the last collection's survivors; pressure is advisory, while an
explicit request is authoritative. Why: automatic election would wrap every
access in lease traffic and pause wherever a region happened to open.

1. **Promote.** At a stable instant under exclusive settlement admission,
   `pump_until_stable` turns pressure into the explicit request, then returns.
2. **Request.** `request_managed_collection` sets the same request.
3. **Observe.** `readiness` precedence: runtime `Poisoned`; `Busy` (no
   settlement admission, or a GC lease live); `MaintenanceFailed` (heap
   poisoned); `MaintenanceRequired` (request or retry); then work and events.
4. **Service.** `RuntimeMaintenanceSnapshot::service` rechecks the revision
   (else `RuntimeChanged`), publishes a `RuntimeGcActivityLease` under shared
   admission, runs `collect_full` outside runtime locks, and publishes the
   outcome.

- The revision serves only service; settlement checks for no lease, no
  request and an `Idle` disposition.
- Late pressure stays latched for the next pump: a race may add an attempt,
  never lose a request.
- Collector or finalizer panics yield `RetryRequired` plus a durable failure.
  Irreversible damage poisons the heap (`MaintenanceFailed`, terminal). A
  dropped unfinished lease marks `RetryRequired`.
- Collection is not a semantic mutation: no epoch, value, transaction,
  diagnostic or topology changes, and pure Glam cannot observe it.
- `aggressive-gc-verification` swaps only the pressure input (any allocation
  since the last stable pump) and lets the pump service its own request.
  There is no per-entry hook.
- The CLI writes its output before its first settle loop, so assembly never
  collects: peak memory equals total allocation.

## Invariants and Verification

- Root heap identity is authoritative provenance.
- Edges are exact; conservative retention runs only through external owners.
- No managed node strongly reaches its domain, runtime or an active owner.
- Allocations are published before their region ends.
- Destruction is passive; a trace does no semantic work.

Source audits reject the shapes that would break these rules: authority,
roots or active `Drop` in managed-graph declarations, and unadmitted opaque
payloads (`core/managed/managed_boundary_audit.rs`); raw core values crossing
production signatures without access (`core/managed/raw_value_api_inventory.rs`);
heaps that collect automatically (`api/runtime/gc_activity_inventory.rs`); and
unreviewed runtime cache families (`core/runtime_cache.rs`). Tests collect only
private domains.
`scripts/check.sh full` adds `aggressive-gc-verification`. Collector evidence
is in [`VERIFY.md`](../../crates/glam-gc/VERIFY.md).

## Deferred Work

- [Value Representation Refinement](../plans/ValueRepresentationRefinement_2026-08-19.md):
  tagged values; managed spines replacing shells; shared persistent spines
  revisited during logical tracing; the shell walk's recursion; a
  process-wide metadata lock plus heap lock per allocation; unmeasured
  pressure thresholds.
- [Concurrent Garbage Collection](../plans/ConcurrentGarbageCollection_2026-08-28.md):
  concurrent marking; a never-stable runtime never collects; CLI peak memory
  equals total allocation because there is no foreground GC (open decision);
  settlement's reliance on non-moving collection.
- [Scoped Pointer Safety](../plans/GarbageCollectorScopedPointerSafety_2026-09-09.md):
  branded `ScopedGc`, only after a concrete defect.
- Moving collection waits for parallel-root retirement: no bare value across a
  yield, no data beside a parallel root, no `CompatibilityValueEdges`, and
  rewritable persistent edges.
- Accepted: a task result holding its own handle lives until teardown;
  retired external owners wait for a host call. Weak references and
  generational collection are not planned.
