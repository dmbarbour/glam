# Glam-Owned Garbage Collection Roadmap — 2026-08-19

Status: in progress; collector Phases C0 through C3D, the C2C.6 verification
follow-up, and integration Phase I0 are complete. Gate G0 is established, and
the mandatory post-C1 and post-C2C reviews are complete. Collector Phase C4
is next.

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

Scope decision, 2026-08-21: this roadmap ends with a non-moving,
stop-the-world full collector integrated across Glam. Its purpose is controlled
ownership and reclamation of recursive graphs such as fixpoints, not a complete
performance-oriented GC hierarchy. Moving and generational collection return
only under a later plan, after higher-priority performance work and profiling.

Compact tagged values, their pointer-alignment policy, and representation-
specific node-size targets belong to the deferred
[`ValueRepresentationRefinement_2026-08-19.md`](ValueRepresentationRefinement_2026-08-19.md)
plan. The collector honors each type's Rust alignment and an optional larger
slot size recorded in canonical object metadata, while preserving run-owner
lookup independently of either. It does not choose Glam's tag budget, type
alignment, or node-size policy. Compact representation is not a collector gate.

## Purpose

Replace recursive `Arc` ownership which can leak fixpoint, promise, metadata,
function, collection, or interaction-net cycles with one exact tracing heap per
`EvaluationRuntime`.

The collector is specialized for Glam:

- values never cross evaluation runtimes;
- managed pointers are initially non-moving, pointer-sized, cheap to copy, and
  shareable between runtime worker threads;
- copying an ordinary managed pointer performs no locking, reference-counting,
  rooting, or collector bookkeeping;
- ordinary allocation uses worker-local cursors over exclusively leased ranges
  of allocation-bitmap words in homogeneous typed runs; shared synchronization
  is reserved for allocation-class discovery, claiming another range,
  obtaining/recycling runs, or acquiring another arena chunk;
- managed layouts are deliberately bounded: every allocation fits one slot in
  a fixed-size typed run, with no initial large-object or multi-run fallback;
- collection coordination occurs at explicit mutator-region boundaries;
- recursive entry into the same runtime is cheap and supported;
- the initial collector stops the whole runtime while tracing and sweeping;
- the initial storage model serves only exact full collection; it does not pay
  for generations, remembered sets, or promotion before a moving nursery is a
  justified performance project;
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
    ├── arena chunks
    │   └── fixed-size aligned power-of-two typed runs
    │       ├── one allocation class and static trace/drop metadata
    │       ├── homogeneous aligned slots
    │       └── allocation and mark side metadata
    ├── process-wide TypeId -> canonical object-metadata interning
    ├── per-heap metadata pointer -> allocation-class discovery
    ├── typed-run pools
    ├── explicit external-root registry
    ├── active-mutator and safepoint coordinator
    ├── mark work and sparse quarantine state
    └── collection metrics and tuning
```

`RuntimeHeap` is internal to the runtime. A public `Value` is an external root
in exactly that heap. An internal `Gc<T>` is not a root and is usable only while
the current thread has entered that heap as a mutator. A root or mutator guard
retains the heap allocation; a bare `Gc<T>` never does.

The primary supported Glam access shape is token-based:

```rust
runtime.with_mutator(|mutator| {
    let value = root.get(mutator);
    let child = managed_node.get(mutator);
});
```

Here `root` and `managed_node` are checked runtime-owned wrappers, not a promise
that bare `Gc<T>` has a safe dereference operation. The collector's raw
`Gc<T>` access is a private unsafe gateway whose caller proves heap, liveness,
and representation; supported Glam wrappers discharge that proof from their
root/provenance invariants. A thread-local current-mutator assertion may
support internal ergonomics, but it must not become the only safety story or
permit dereference outside a region.

## Cross-Plan Semantic and Safety Invariants

1. **One heap per runtime.** Every managed allocation, internal pointer, and
   root belongs to exactly one `EvaluationRuntime` heap.
2. **No cross-runtime managed edge.** Public boundaries reject a foreign
   `Value` before exposing its core representation. Internal construction
   obtains both the target heap and mutator authority from one runtime. A
   thread may concurrently hold mutator authority for several runtime heaps,
   but each authority, recursive depth, and allocation cache remains a separate
   heap-qualified TLS entry and grants no cross-runtime edge.
3. **No collection during mutation.** The baseline collector reclaims only
   after every active mutator region has exited. Nested same-runtime regions
   count as one active mutator. Allocation also requires mutator authority, so
   no allocation can race a stopped-world trace or sweep. Every successful
   allocation is fully initialized and marked allocated before its managed
   pointer is returned; sharing that pointer does not wait for mutator exit.
   Before outermost exit makes the mutator inactive, it leaves worker-local
   bitmap-range cursors retained and makes its heap-specific thread cache
   quiescent. TLS destruction or eviction may forget cursors but never returns
   their ranges or touches the heap. The heap owns and enumerates every run and
   full collection clears all range leases without walking those caches. Each
   cache captures one heap-wide allocation-lease epoch; after a full collection,
   one epoch comparison discards its entire cursor map rather than validating
   cursors individually. Cursors carry no separate active/parked state: thread-
   local recursive depth governs use, and exclusive collector admission proves
   every cache is quiescent.
   Once collection is committed, new ordinary entrants wait, while a thread
   already active in another heap may make a dependent entry before the target
   collector becomes exclusive. This prevents opposite cross-heap nesting
   orders from deadlocking on pending collections. An exclusive collector never
   enters another heap or invokes callbacks; cross-heap entry waits until that
   exclusive phase ends.
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
   part of Glam evaluation semantics. This describes collection while a value-
   domain owner lease remains live. C6 must separately settle how the last
   external heap owner initiates terminal destruction without pretending a
   fresh public root can retain an owner which has already been dropped.
7. **No collector lock in callbacks or destructors.** Destruction, wakes,
   diagnostics, host callbacks, and scheduler callbacks occur only in phases
   whose lock and re-entry rules are explicit. No arbitrary callback or Rust
   destructor runs while heap allocator or coordinator locks are held. The
   `Finalizing` phase reopens shared mutator admission while the collector
   retains one mutator lease; recursive same-heap entry reuses that region.
   Uncommitted collection pressure arising before finalization completes is
   coalesced as a possible follow-up request on the active collector
   coordinator; it is not retroactively satisfied by the already completed
   mark. An explicit request or heuristic commitment may commit the next
   collection while finalization is still active. The collector-held mutator
   delays exclusive acquisition, while the commitment intentionally makes new
   ordinary mutators wait: stop-the-world collection has become the runtime's
   next priority.
8. **Stable addresses are an implementation phase, not a permanent API
   promise.** The initial full collector does not move allocations. Trace
   implementations enumerate outgoing edges through a visitor rather than
   publishing offset tables, so a later moving collector can add edge
   rewriting without making object layouts part of the public contract.
9. **Mutation gateways are structural; barrier work is phase-specific.** Every
   mutation capable of replacing a managed edge passes through a small,
   auditable mutation gateway. For the initial full stop-the-world collector,
   its collector action is empty. A future moving nursery, incremental marker,
   or concurrent marker may extend the gateway according to its own relocation
   or Dijkstra/SATB-style invariant. Ordinary pointer reads and copies remain
   barrier-free in the initial collector.
10. **Opaque values contain roots, never bare managed pointers.** Type-erased
    host data is not inspected by the collector. Construction must therefore
    ensure an opaque payload contains either no managed value edge or only an
    ordinary runtime/public root from the same value domain. It must not
    contain `Gc<T>`, an unrooted recursive `core::Value`, a foreign-runtime
    root, or another internal pointer which could escape a mutator region.
    Same-runtime roots retained inside an opaque payload appear independently
    in the heap root registry; a backedge through one may conservatively leak,
    but can never be reclaimed prematurely. Cross-runtime host associations
    stay outside the value payload and communicate through validated Rust-layer
    data/effect boundaries.
11. **Failed collection attempts are recoverable until reclamation commits.**
    Marking and tracing reclaim nothing: an unwind guard abandons the partial
    worklist/epoch, restores the heap phase, and permits a later collection to
    retry from all roots. Sweep and destruction use run side metadata plus
    sparse exceptional state so a panicking destructor is never invoked twice;
    its original allocation is quarantined while fresh allocations and
    already-published effects from that destructor remain valid. The
    finalization queue and heap phase must still reach a documented consistent
    state before unwinding.
    Heap-wide poison or abort is reserved for detected allocator/metadata
    corruption or an unsafe contract violation whose effects cannot be
    bounded.
12. **Collection is operational only.** Scheduling and collection timing may
    vary, but successful assembly values remain governed by Glam semantics.
13. **Unsupported layouts remain unsupported.** Class creation rejects
    zero-sized types and any type whose size, alignment, or representation
    cannot fit one slot in the fixed typed-run geometry. The bootstrap does not
    silently add a large-object path, multi-run span, DST allocator, or
    heterogeneous object header to accommodate it.

## Shared Terminology

- **Heap** — one runtime-local arena and collector state.
- **Arena chunk** — a large heap-owned reservation divided into typed runs; it
  is an allocation source, not an object-size exception mechanism.
- **Typed run** — one fixed-size, aligned power-of-two allocation unit. All
  initial runs have the same byte size, while the owning allocation class
  derives a homogeneous slot stride and slot count from its object metadata.
- **Object metadata** — one process-interned immutable descriptor for a Rust
  managed type. Its stable address is the operational type identity; `TypeId`
  is only the cold-path key used to intern it.
- **Allocation class** — a per-heap dense identity discovered from canonical
  object metadata, retaining pools of typed runs for that type. Its slot
  alignment is the Rust type alignment; its stride is the requested metadata
  size, if larger than the Rust size, rounded up to that alignment.
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
- **Full collection** — traces the complete managed heap from explicit roots.
- **Collector-ready graph** — the whole production root graph has passed the
  traceability inventory and forced-collection verification.

## Sequencing and Admission Gates

### Gate G0 — requirements latched

Established on 2026-08-20 in
[`GarbageCollectionGateG0Baseline_2026-08-20.md`](GarbageCollectionGateG0Baseline_2026-08-20.md).
The record names the preserved semantic regressions, captures repeatable
release timing and peak-RSS observations, and records a schedule-sensitive
pre-GC worker stack overflow without treating it as intended semantics.

Before unsafe managed-pointer or allocator code, preserve tests for current
runtime provenance, cross-runtime rejection, lazy and promise cycles,
concurrent worker use, interaction-net sharing, runtime settlement, and release
of fulfilled lazy sources. Record current memory and representative assembly
timings as comparison data, not pass/fail performance contracts. The safe C0
crate scaffold may precede this record; C1A may not.

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

### Gate G4 — legacy ownership retired

Cycle-bearing `Arc` scaffolding made redundant by the collector is removed;
remaining `Arc`s have a deliberate role such as immutable bytes, external
roots, host identities, or non-value scheduler sidecars. Architecture and
agent-context documentation describe the implemented collector rather than
this transition.

## Work Which May Proceed in Parallel

Integration Phase I0 is a read-only ownership/layout inventory and may proceed
before G1; its measurements should inform the fixed-run geometry chosen in C2A.
Managed ownership changes remain blocked on G1. After G1, integration API
adaptation may proceed while the GC subcrate adds full-collection stress and
metrics. These streams may not jointly enable collection until their shared
gate passes.

The following must remain sequential:

- the collector trace contract precedes production `Trace` implementations;
- mutator-region integration precedes managed-pointer dereference;
- full-graph tracing precedes any production sweep;
- full collection correctness precedes concurrent marking.

## Remaining Phase Checkpoints

These choices are intentionally unresolved rather than accidental drift:

- I2 chooses whether a public root points directly at a managed value or at a
  registered root cell containing an inline value;
- C6 selects a last-owner terminal teardown protocol which does not manufacture
  an already-dropped heap owner and does not run mutator-capable destructors
  without their promised context; and
- I8 decides whether `SharedRuntimeNet` remains synchronized external storage
  with an exact visitor or becomes a managed outer node.

Each choice must be resolved and latched by the named phase. None permits
weakening runtime locality, exact tracing, or the collection-admission gates.

## Explicitly Deferred

- concurrent marking, concurrent sweeping, or parallel tracing;
- moving, copying, or compacting collection;
- generational storage, minor collection, promotion, remembered sets, and card
  tables; these should return with a moving-nursery plan rather than precede
  one;
- variable run sizes and pointer-encoded run-size classes;
- large-object allocation, multi-run object spans, heterogeneous runs, and
  arbitrary dynamically sized managed objects;
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

Each completed checkpoint is marked in its owning plan with the verification
run and any semantic decision. Completion of every major parent phase triggers
a mandatory review of all later phases before the next parent phase begins;
that review reconciles implementation drift, dependencies, checkpoint size,
and newly settled semantics. If a checkpoint grows to touch several
independent unsafe or scheduler boundaries, divide it before implementation
rather than waiting for the following review.

For the current collector plan, the mandatory review points are after C1C,
C2C, C3D, C4, C5, C6D, C7, and C8. Review and partition the integration phases
again when Gate G1 permits that work; their present breakdown is not frozen by
the collector implementation plan.

Completion of both child plans requires a final audit against every invariant
above. Passing tests alone does not establish an unexamined trace edge.
