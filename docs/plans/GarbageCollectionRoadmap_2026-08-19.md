# Glam-Owned Garbage Collection Roadmap — 2026-08-19

Status: planned.

This roadmap keeps two large transitions aligned:

1. [`GarbageCollectorImplementation_2026-08-19.md`](GarbageCollectorImplementation_2026-08-19.md)
   builds and verifies a Glam-owned tracing collector in isolated path crates.
2. [`GarbageCollectorIntegration_2026-08-19.md`](GarbageCollectorIntegration_2026-08-19.md)
   makes one `EvaluationRuntime` one collected value domain, migrates every
   recursive ownership edge, and admits collection only after a whole-graph
   audit.

The plans are separate because collector soundness and evaluator migration are
independently difficult. This roadmap owns the requirements which neither plan
may weaken merely to simplify its local work.

## Purpose

Replace recursive `Arc` ownership which can leak fixpoint, promise, metadata,
function, collection, or interaction-net cycles with one exact tracing heap per
`EvaluationRuntime`.

The collector is specialized for Glam:

- values never cross evaluation runtimes;
- managed pointers are non-moving, pointer-sized, cheap to copy, and shareable
  between runtime worker threads;
- copying an ordinary managed pointer performs no locking, reference-counting,
  rooting, or collector bookkeeping;
- ordinary allocation bumps through a region local to the outer mutator entry;
  shared synchronization is reserved for obtaining/recycling regions, large
  objects, or acquiring another large arena page;
- collection coordination occurs at explicit mutator-region boundaries;
- recursive entry into the same runtime is cheap and supported;
- the initial collector may stop the whole runtime;
- storage anticipates young and old generations, but correct full collection
  precedes minor collection;
- persistent Rust collections may initially be traced logically even when that
  revisits shared spines; collector-aware spines remain a performance project;
- evaluation synchronization inside lazies, promises, tasks, and nets remains
  semantic synchronization, not GC pointer synchronization; and
- host IPC and persistence continue to exchange binaries, not live managed
  pointers.

The change must not alter Glam evaluation, reflection, scheduling, task,
diagnostic, or interaction-net semantics except for reclaiming objects which
were already unreachable.

## Fixed Architecture

```text
EvaluationRuntime
├── RuntimeState
├── immutable runtime profile
└── RuntimeHeap
    ├── young and old allocation pages
    ├── explicit external-root registry
    ├── active-mutator and safepoint coordinator
    ├── mark work and object metadata
    ├── remembered-set state
    └── collection metrics and tuning
```

`RuntimeHeap` is internal to the runtime. A public `Value` is an external root
in exactly that heap. An internal `Gc<T>` is not a root and is usable only while
the current thread has entered that heap as a mutator. A root or mutator guard
retains the heap allocation; a bare `Gc<T>` never does.

The primary safe access shape is token-based:

```rust
runtime.with_mutator(|mutator| {
    let value = root.get(mutator);
    let child = pointer.get(mutator);
});
```

The collector implementation plan must prototype this shape before freezing
it. A thread-local current-mutator assertion may support internal ergonomics,
but it must not become the only safety story or permit dereference outside a
region.

## Cross-Plan Semantic and Safety Invariants

1. **One heap per runtime.** Every managed allocation, internal pointer, and
   root belongs to exactly one `EvaluationRuntime` heap.
2. **No cross-runtime managed edge.** Public boundaries reject a foreign
   `Value` before exposing its core representation. Internal construction
   obtains both the target heap and mutator authority from one runtime.
3. **No collection during mutation.** The baseline collector reclaims only
   after every active mutator region has exited. Nested same-runtime regions
   count as one active mutator. Allocation also requires mutator authority, so
   no allocation can race a stopped-world trace or sweep. Before the outermost
   exit publishes that the mutator is gone, it commits completed allocations
   and returns or accounts for the unused tail of its local allocation region.
4. **No hidden stack scan.** Roots are explicit. Local unrooted pointers are
   safe because their entire lifetime lies within a mutator region.
5. **No partially traced collection.** Production reclamation remains disabled
   until every possible edge from a production root is traced or is covered by
   an explicitly documented conservative-retention rule.
6. **Finalization admits fresh mutation, never identity resurrection.** After
   reachability fixes the dead set, allocations requiring Rust destruction are
   detached into a non-rootable `Finalizing` set. Before releasing exclusive
   mutator admission, the collector atomically converts its authority into an
   ordinary mutator lease held by the collector thread. The heap then admits
   other mutator regions concurrently, while that held lease and the
   coordinator defer every new collection until the set is drained. `Drop` for
   implementation and host-owned payloads, including embedding-client types
   stored in `OpaqueValue`, runs inside the collector's mutator region. It may
   allocate fresh values, evaluate or schedule work, publish diagnostics, and
   even construct and publish a fresh equivalent of itself.
   It cannot recover a root to any allocation in the completed dead set or
   transition its original identity back to `Allocated`. This is quining, not
   resurrection. Finalizer timing and order remain operational rather than
   part of Glam evaluation semantics.
7. **No collector lock in callbacks or destructors.** Destruction, wakes,
   diagnostics, host callbacks, and scheduler callbacks occur only in phases
   whose lock and re-entry rules are explicit. No arbitrary callback or Rust
   destructor runs while heap allocator or coordinator locks are held. The
   `Finalizing` phase reopens shared mutator admission while the collector
   retains one mutator lease; recursive same-heap entry reuses that region.
   Uncommitted collection pressure arising before finalization completes is
   coalesced as a possible follow-up request on the active collector
   coordinator; it is not retroactively satisfied by the already completed
   mark. An explicit request or heuristic commitment may queue the next writer
   while finalization is still active. The collector-held mutator delays its
   acquisition, while writer preference intentionally makes new mutators wait:
   stop-the-world collection has become the runtime's next priority.
8. **Stable object addresses.** The initial full and generational collectors
   do not move allocations. Promotion changes metadata or page classification,
   not pointer identity.
9. **Mutation gateways are structural; barrier work is phase-specific.** Every
   mutation capable of replacing a managed edge passes through a small,
   auditable mutation gateway. For the initial full stop-the-world collector,
   its collector barrier is empty. Generational minor collection activates
   old-to-young remembered-set work even though collection remains
   stop-the-world. A future incremental or concurrent marker would separately
   add the insertion, deletion, or shading work required by its chosen
   Dijkstra/SATB-style invariant. Ordinary pointer reads and copies remain
   barrier-free in every mode.
10. **Opaque values contain roots, never bare managed pointers.** Type-erased
    host data is not inspected by the collector. Construction must therefore
    ensure an opaque payload contains either no managed value edge or only an
    ordinary runtime/public root. It must not contain `Gc<T>`, an unrooted
    recursive `core::Value`, or another internal pointer which could escape a
    mutator region. Roots retained inside an opaque payload appear
    independently in the heap root registry; a backedge through one may
    conservatively leak, but can never be reclaimed prematurely.
11. **Failed collection attempts are recoverable until reclamation commits.**
    Marking and tracing reclaim nothing: an unwind guard abandons the partial
    worklist/epoch, restores the heap phase, and permits a later collection to
    retry from all roots. Sweep and destruction use explicit object-local
    states so a panicking destructor is never invoked twice; its original
    allocation is quarantined while fresh allocations and already-published
    effects from that destructor remain valid. The finalization queue and heap
    phase must still reach a documented consistent state before unwinding.
    Heap-wide poison or abort is reserved for detected allocator/metadata
    corruption or an unsafe contract violation whose effects cannot be
    bounded.
12. **Collection is operational only.** Scheduling and collection timing may
    vary, but successful assembly values remain governed by Glam semantics.

## Shared Terminology

- **Heap** — one runtime-local arena and collector state.
- **Managed pointer** — a cheap, non-rooting pointer between collected values.
- **External root** — a shareable handle retaining a value from Rust code
  outside a mutator-local managed graph, including public `Value` handles and
  runtime-owned records.
- **Mutator region** — dynamic scope in which a thread may allocate,
  dereference, and mutate managed values in one heap.
- **Finalizing phase** — post-mark phase in which the completed dead set is
  non-rootable, collection is deferred by a collector-held mutator lease,
  ordinary mutation is concurrently admitted, and queued Rust destructors run
  outside collector locks.
- **Safepoint** — an outer mutator exit or explicit cooperative check at which
  a requested collection may stop progress.
- **Full collection** — traces all generations.
- **Minor collection** — traces young objects from roots and remembered
  old-to-young edges.
- **Collector-ready graph** — the whole production root graph has passed the
  traceability inventory and forced-collection verification.

## Sequencing and Admission Gates

### Gate G0 — requirements latched

Before collector code, preserve tests for current runtime provenance,
cross-runtime rejection, lazy and promise cycles, concurrent worker use,
interaction-net sharing, runtime settlement, and release of fulfilled lazy
sources. Record current memory and representative assembly timings as
comparison data, not pass/fail performance contracts.

### Gate G1 — isolated collector soundness

The implementation plan has a working non-moving, stop-the-world full
collector with explicit roots, regional mutators, deterministic race tests,
Miri coverage, and no production Glam dependency.

Only after G1 may integration replace `Arc` ownership with `Gc` in production
types. Production automatic or explicit reclamation remains disabled.

### Gate G2 — production graph closed

The integration inventory accounts for every root and recursive edge,
including deferred closures, opaque payloads, persistent collections,
interaction nets, reflection volumes, caches, event buffers, diagnostics, and
coordinator records. Every graph-bearing type either implements exact tracing
or has a reviewed conservative-retention rule.

Only after G2 may tests force a full collection over the complete production
graph.

### Gate G3 — full collection enabled

Forced full collections pass the complete semantic, concurrency, and drop
tests. Full collection is then enabled at explicit runtime maintenance points.
Automatic threshold collection remains disabled until those points are stable.

### Gate G4 — generational collection enabled

The barrier audit is complete, minor collections agree with full collections
under differential tests, and old-to-young publication races are forced.
Minor collection may then become the default threshold response.

### Gate G5 — old ownership retired

Cycle-bearing `Arc` scaffolding made redundant by the collector is removed;
remaining `Arc`s have a deliberate role such as immutable bytes, external
roots, host identities, or non-value scheduler sidecars. Architecture and
agent-context documentation describe the implemented collector rather than
this transition.

## Work Which May Proceed in Parallel

After G1, integration inventory and API adaptation may proceed while the GC
subcrate adds generational storage and metrics. These streams may not jointly
enable collection until their shared gate passes.

The following must remain sequential:

- the collector trace contract precedes production `Trace` implementations;
- mutator-region integration precedes managed-pointer dereference;
- full-graph tracing precedes any production sweep;
- barrier inventory precedes minor collection; and
- full collection correctness precedes concurrent marking.

## Explicitly Deferred

- concurrent marking, concurrent sweeping, or parallel tracing;
- moving, copying, or compacting collection;
- weak pointers, ephemerons, and user-visible weak-key semantics;
- first-class Glam finalizer declarations, resurrection of a completed dead
  allocation, or access to the dying allocation through the managed-pointer
  API;
- GC-aware replacements for RPDS and FingerTrees;
- LSM-tree dictionary design;
- precise reclamation through arbitrary opaque host payloads;
- cross-runtime values or heap migration;
- persistent serialization of live managed graphs; and
- JIT-specific stack maps.

These are follow-up projects. The baseline design leaves room for initial
pause, concurrent mark, final remark, and stop-the-world sweep, but does not
pay their synchronization cost before profiling supports them.

## Plan Maintenance

Each completed phase is marked in its owning plan with the verification run and
any semantic decision. Before beginning another major phase, review all later
phases for drift against the current implementation. If a checkpoint grows to
touch several independent unsafe or scheduler boundaries, divide it before
implementation.

Completion of both child plans requires a final audit against every invariant
above. Passing tests alone does not establish an unexamined trace edge.
