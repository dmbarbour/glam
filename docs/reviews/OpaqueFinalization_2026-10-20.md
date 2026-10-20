# Opaque Finalization Review — 2026-10-20

Status: findings and a recommendation for the design step
`api-opaque-finalization` (runtime API plan). No code changed. The
maintainer asked what weak references glam-gc could feasibly offer, with
each mechanism's pros and cons and a recommendation (2026-10-20), then
answered its questions the same day (see
[Maintainer Decisions](#maintainer-decisions-2026-10-20)). The step's ADR
records what is settled.

Revision reviewed: `49ba7006` (clean `main`).

## Purpose and Scope

What an opaque value stands for should be reclaimed once nothing reaches
it: a promise whose resolver is collected unresolved is broken, and a
volume whose handles are all unreachable is reclaimed. Of the runtime's
opaque and capability values (inventoried in the plan's
`api-opaque-finalization`), only two stand for resources that need
reclaiming once the bridge is gone: volume handles and resolvers. Task
handles, effect tokens, protected volumes and the Rust `PromiseResolver`
go with the bridge; compilation origins and construction brands hold no
resource.

The maintainer suggested modelling resolvers and volume handles as special
values in the managed heap, so the collector sees them. This review asks
what each needs, what glam-gc would have to offer, and at what cost.

**Terms.** "Heap" names several things. Here the *managed heap* is
glam-gc's: the allocations that hold glam values, which the collector
traces. The *shared heap* is the store's volume that `.heap.*` reads and
writes, one volume among others.

## Current Model

- **Opaque payloads live outside the managed heap.** A `Value::Opaque` holds an
  `ExternalOwnerHandle`: a runtime ID, a registry ID and a lease, an
  `Arc<()>` (`src/core/managed/external_owners.rs:17`). The payload stays in
  the runtime's external-owner registry. Every copy of the value clones the
  lease, in a managed node's shell or a Rust root. Once the last copy is
  gone (its node swept and finalized, or its root dropped), the lease is
  dead, and the registry's `drain_retired` destroys the payload outside the
  collector (`external_owners.rs:130`). The leases are thus reference
  counting layered over tracing: a payload learns that it is unreachable
  when its `Drop` runs.
- **Draining is incidental.** The only production call of `drain_retired` is
  a host call's invocation (`src/core.rs:1754`), so a dead payload waits for
  some later host call, or for the domain's drop.
- **The store holds volumes as roots.** Each volume's committed value is a
  registered root in the store's persistent map, shared by its snapshots,
  until the volume is revoked or the runtime retires. Opaque volume handles
  (`src/reflection/volume_handle.rs:29`) are minted afresh by each
  `volume_handle_in`, carry the ID alone, and have no `Drop`.
- **The record holds pending promises as roots.** `Promises` keeps each
  pending promise's `ManagedPromiseRoot` until it resolves, then a
  `Resolved` marker forever (`src/reflection/promises.rs`). The resolver,
  `PromiseCapability`, carries the ID alone and has no `Drop`. A pending
  promise cell traces only its assignment, so while pending it reaches
  nothing (`src/core/managed/recursive_cells.rs:1588`).
- **The collector's stance.** glam-gc is an exact, non-moving,
  stop-the-world, full collector. It defers "weak pointers, ephemerons, and
  user-visible weak-key semantics", "first-class Glam finalizers and
  resurrection", "GC-aware persistent containers" and "precise reclamation
  through opaque host payloads" (`crates/glam-gc/SAFETY.md:1742`). Managed
  `Drop` is passive: it may not enter the managed heap, call the runtime, or treat a
  managed edge as live. Active cleanup belongs in the external-owner
  registry. An opaque payload that is part of a managed cycle may not hold
  a root as a stand-in for tracing (`SAFETY.md:1227`).
- **Where collection happens.** The runtime collects at its maintenance
  points, outside every mutator, through `collect_under_lease`
  (`src/runtime.rs:499`), which takes a hook that runs after the collection.
  Inside the collector, marking seeds the roots, drains a worklist, and then
  classifies the dead set under exclusive authority before it sweeps and
  finalizes (`crates/glam-gc/src/heap.rs`, `collect_full`).

## Findings

- **F1. Hidden roots defeat cycles.** The store's volume roots and the
  record's pending roots are roots the collector cannot see through. A
  handle or resolver kept within a volume stays reachable through the
  store, so a volume holding its own handle, or a ring of volumes, never
  dies, and a resolver within such a volume never breaks its promise. Task
  handles already show this: one within its own task's result is never
  freed, and a test confirms it (`src/reflection/requests.rs:2172`).
- **F2. Draining is incidental** (above). Even an acyclic dead payload is
  noticed late or never.
- **F3. A finalizer needs a safe point.** Breaking a promise publishes into
  a managed cell, which takes value access, and writes the promise's place
  in the store, which takes the runtime's transaction lock. A payload's
  `Drop` runs during a drain, which may hold neither safely, so it can only
  queue a notice, as a query handle queues its retirement for the next
  commit (`src/reflection/store.rs:973`).
- **F4. Identity per resource.** Nothing counts a volume's handles: each
  minted handle is a payload of its own. A volume's identity matters to
  conflict analysis, which keys on volume IDs, not to users, who do not
  compare volumes (maintainer, 2026-10-20). Volume IDs are never reused, so
  a reclaimed volume's conflict entries can be pruned without aliasing.
- **F5. Collection timing becomes visible to settlement.** Glam promises
  that collection's timing never changes results. Breaking a promise when
  its resolver is collected keeps that promise: an unreachable resolver can
  never resolve, so the promise's outcome is already decided, and only when
  waiters learn it depends on collection. But a quiescence report taken
  before a collection would list waits on promises already lost. Settlement
  should collect, and process the notices, before it reports.
- **F6. A transaction may outlive its handle.** Decoding a handle yields a
  volume ID, and the handle value may then be garbage while the transaction
  still reads or writes the volume. Reclamation must not turn that into the
  revoked-volume error at commit: the transaction should keep the volume
  alive until it ends.

## What Each Resource Needs

**Resolvers need no place in the managed heap.** A resolver can stay an
opaque payload: its liveness is already exact under the leases, except
through hidden roots. A resolver may keep its promise alive, but never the
reverse (maintainer, 2026-10-20), and the record keeps that shape: it roots
each pending promise while its resolver lives, and a pending promise
reaches nothing, so it cannot keep its resolver alive. Machines' stacks,
state and port buffers have explicit lifetimes and drop their roots. The
shared heap's volume is always live, so a resolver kept there is correctly
alive. That leaves cycles through collectable volumes, which are
the volumes' problem (F1). Given prompt draining and a safe point, the
resolver's payload `Drop` is a sound death notice: it queues the promise's
ID, and the runtime breaks the promise and drops its record. The status
record then needs no `Resolved` marker forever: once the resolver is gone,
nothing can ask.

**Volumes need the collector.** A volume's value can reach its own
handles, and the store must hold the value somewhere, in the managed heap
or beside it. Unless that holding
is visible to the collector as conditional on the handles, cycles through
volumes never die. This is the elephant: no amount of reference counting
over external owners collects it.

## Mechanisms

### M1. Death notices over the existing leases (no collector change)

A payload's `Drop` queues a notice, and the runtime drains the registry
after each collection and processes notices at a safe point.
- Pros: no collector change; glam's rules already allow active cleanup in
  external owners; resolvers are finished by it; acyclic volumes are
  reclaimed once each volume has one payload, shared by its handles.
- Cons: cycles through volumes leak until the runtime retires (F1);
  timeliness depends on collection frequency.

### M2. Weak references with death notices

A weak slot names an allocation without tracing it. After marking, a slot
whose target is unmarked is cleared, and its notice token is published for
the runtime to drain, as Java's reference queues and `Cleaner` do.
- Pros: simple in a stop-the-world collector: one pass over the weak slots
  between draining the worklist and classifying the dead set, where the
  exclusive post-mark authority already exists; no resurrection; the
  notice carries host data, never the dead object.
- Cons: weak references alone do not collect the store's cycles: the store
  must hold each volume's value through something the collector traces
  from the handle, which means moving committed values into managed volume
  cells (M6). A future concurrent collector needs a keep-alive barrier on
  weak reads, a known technique.

### M3. Ephemerons

An ephemeron pairs a key with a value: the value is traced only while the
key is reachable by other paths. After ordinary marking, the collector
repeats: for each ephemeron whose key is marked and value unmarked, mark
the value and drain; until nothing changes. Ephemerons with unmarked keys
are then cleared and their notices published. A weak reference is an
ephemeron whose key and value coincide, so M3 subsumes M2.
- Pros: exactly the store's shape, a runtime-owned table whose entries
  should live as long as their keys. The store keeps its persistent map and
  snapshots; each committed volume value is held by an ephemeron keyed on
  the volume's identity in the managed heap, so a volume holding its own handle is
  ordinary garbage.
- Cons: the fixpoint costs a pass per round over the ephemerons, bounded by
  the number of volumes and usually a round or two. A new root kind
  touches the collector's reviewed unsafe root code and its verification
  gates (Loom, Miri, sanitizers, scale proofs). The key must be a managed
  allocation: a volume's identity becomes a value kind with an edge to a
  small managed cell, as `Value::Promised` has an edge to its promise cell,
  rather than an opaque payload. Concurrent marking handles ephemerons with
  more care, as V8 does.

### M4. Guardians

Objects registered with a guardian that die are retained for one more cycle
and handed back to the runtime, which then finalizes them with their state
intact.
- Pros: the finalizer sees the dead object's edges, such as a resolver's
  promise.
- Cons: resurrection, which glam-gc's ledger excludes, and nothing here
  needs it: a notice can carry what the finalizer needs (a promise's ID or
  root) without reaching the dead object.

### M5. Active managed finalizers

Managed `Drop` that enters the managed heap or calls the runtime.
- Pros: none here that M1–M3 lack.
- Cons: overturns the passive-`Drop` rule and the finalization guarantees
  the collector's gates certified.

### M6. Volume cells owning committed values

The maintainer's first vision (2026-10-20): a volume is a cell in the
managed heap that traces its committed value; handles are edges to it, and
a transaction records a volume's snapshot when it first reads the volume.
- Pros: ordinary tracing collects volumes, cycles included, with no new
  collector mechanism; the store holds no volume values, and a reclaimed
  volume's conflict entries could be pruned by a watermark, the oldest
  live transaction's revision, rather than by notices.
- Cons: isolation. A transaction that first reads a volume after another
  commits to it would see a later state than its other reads. Keeping reads
  consistent means one of two things. It could conflict at the first read
  whenever the volume changed since the transaction began (versioned reads,
  as TL2 does), which is coarser than today's conflict analysis by path. Or
  each cell could keep the versions that live transactions may still read
  (multiversioning), which the store's persistent maps already give its
  snapshots for free.

## Recommendation

In both M3 and M6 a volume becomes a cell in the managed heap: its
identity, to which handles are edges. They differ in where its committed
values live. M6 moves them into the cell and rebuilds isolation around
first reads; M3 leaves them in the store's persistent map, held through
ephemerons keyed on the cell, so snapshots and conflict analysis by path
stay as they are. Ephemerons also serve more than volumes: the design's
scope-unique atoms, `anno 'scope_unique`, as dictionary keys, model a heap
within a pure computation whose entries are collected with their keys
(`Design.md`, "Ephemerons").

Stage the work, resolvers first:

1. **Death notices (M1), now.** One payload per volume, which the store
   caches weakly and handles share; a `Drop` for volume handles and
   resolvers that queues a notice; draining the registry after each
   collection, inside `collect_under_lease`; processing notices at that
   safe point: breaking a lost resolver's promise ("its resolver was lost")
   and dropping its record, and retiring a dead volume's root and conflict
   entries. A transaction keeps the volumes it touches alive (F6);
   settlement collects before it reports (F5). Resolvers are then done,
   and acyclic volumes are reclaimed. Cycles through volumes leak until
   the runtime retires, documented as a known limitation.
2. **Ephemerons in glam-gc (M3), then volumes as cells in the managed
   heap.** A new root kind, with weak references as its degenerate case,
   and a volume's identity as a managed cell, a value kind whose committed
   values the store holds through ephemerons keyed on it. This closes F1
   for volumes without reworking the store's snapshots, which M6 would.

Prefer M3 over M6: both need collector work and a managed volume identity,
but M3 keeps the store's transactions as they are, and its extra collector
cost, the fixpoint, is bounded by the number of volumes. Reject M4 and M5:
notices carry what finalizers need, and the collector's rules stay intact.

Planned steps: `api-death-notices` (stage 1), then `api-ephemerons` and
`api-volume-cells` (stage 2).

## Maintainer Decisions (2026-10-20)

1. Stage 1's leak of cycles through volumes is acceptable while temporary;
   the plan returns to it in stage 2.
2. A lost resolver breaks its promise with a plain "resolver lost".
3. Settlement always collects before it reports, rather than only when
   promises are pending, so later weak references, ephemerons or
   finalizers that make noise add no special cases.

Open: stage 2's mechanism, M3 (recommended) or M6.
