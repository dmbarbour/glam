# Concurrent Garbage Collection Plan — 2026-08-28

Status: preliminary and deferred until the initial collector integration has
passed roadmap Gate G4. This is a successor to, not a continuation or renaming
of, the completed C0-C6 stop-the-world collector implementation. Its phase
names use `CG` so historical collector checkpoints remain unambiguous.

## Purpose

Replace heap-idle collection election with a non-moving collector which can
begin marking, determine semantic death, and make reclamation progress while
mutators remain active. It must compose with threads which hold mutator
authority for several heaps in arbitrary hierarchical orders.

The initial collector elects only when one outer entry observes zero active
mutators. This is safe and avoids cross-heap admission deadlock, but
continuously overlapping regions can starve collection. Blocking new entry
after a stop request is not a general repair: a thread may hold an outer heap
while entering an inner heap, and another hierarchy may acquire the same heaps
in the opposite order. Allowing those entries to bypass the stop avoids
deadlock but restores starvation.

This plan removes collection progress from heap-wide mutator exclusivity. It
separates:

1. **semantic liveness** — whether the traced Glam graph reaches an allocation;
2. **allocator quiescence** — whether an active allocation cursor may still
   publish into a run;
3. **access quiescence** — whether an older mutator may still hold a reference
   into retired storage; and
4. **physical reclamation** — destruction and reuse after the preceding proofs.

The first implementation favors delayed and conservative reclamation over
waiting for pins or eagerly recovering one newly empty run.

## Relationship to Existing Plans

Entry requires:

- completion of
  [`GarbageCollectorImplementation_2026-08-19.md`](GarbageCollectorImplementation_2026-08-19.md),
  including its final safety ledger;
- completion of
  [`GarbageCollectorIntegration_2026-08-19.md`](GarbageCollectorIntegration_2026-08-19.md)
  through I13 and Gate G4, including exact production roots, mutation gateways,
  and destruction inventories; and
- a dated post-integration review confirming that no migration adapter or
  conservative root accidentally became part of the permanent graph model.

Compact values from
[`ValueRepresentationRefinement_2026-08-19.md`](ValueRepresentationRefinement_2026-08-19.md)
are not a prerequisite. If that transition begins first, CG0 must inventory its
actual pointer decoder and managed layouts rather than assuming the older
representation.

The existing non-moving full collector remains the reference implementation
and reachability oracle throughout this plan. This plan does not rewrite its
historical C phases or weaken Gate G1-G4 claims.

## Initial Scope

The first concurrent collector provides:

- nonblocking collection initiation while mutators are active;
- concurrent exact marking with bounded mutator handshakes;
- concurrent logical sweep and run sealing;
- delayed confirmation of empty runs in a later collection epoch;
- heap-local epoch/grace-period protection for outstanding managed references;
- run-local allocation-cursor pins and generation-checked cached cursors;
- deferred finalization and physical run recycling after all proofs hold; and
- identical Glam results under the reference and concurrent collectors.

It remains non-moving and uses the existing fixed-size homogeneous typed runs.
The initial implementation need not trace in parallel: one collector worker may
mark concurrently with many mutators. Parallel mark workers are a later tuning
choice.

## Non-goals

- moving, copying, compacting, or pointer rewriting;
- generations, nursery collection, promotion, remembered sets, or cards;
- eager recycling in the same epoch which first observes an empty run;
- blocking heap admission until a collector becomes exclusive;
- a lock-free collector or lock-free mark queue as a prerequisite;
- stack-map or JIT integration;
- cross-runtime edges, heap migration, or shared runs between heaps;
- large-object spans, heterogeneous runs, or variable run sizes; and
- user-visible GC scheduling or collection-count semantics.

## Terminology

- **Collection epoch:** one concurrent reachability attempt from initiation
  through final remark and logical sweep.
- **Reclamation epoch:** heap-local progress used to prove that no mutator from
  an earlier access generation can still dereference retired storage.
- **Run generation:** an ABA-resistant identity generation incremented before
  a recycled run address can be republished.
- **Active cursor pin:** a run-local obligation held only while an active
  mutator may allocate through a cursor into that run.
- **Sealed run:** a run removed from allocation frontiers and closed to new
  reservations, but not yet destroyed or recycled.
- **Retired run:** a semantically empty sealed run awaiting finalization and/or
  an access grace period.

Advancing a collection epoch does not by itself establish reclamation safety.
A later epoch is the earliest confirmation point, not a substitute for the
mutator grace-period proof.

## Target Coordination Model

### Heap collection state

The exact representation is deferred, but the logical phases are:

```text
Idle
  -> Starting(epoch)
  -> ConcurrentMark(epoch)
  -> FinalRemark(epoch)
  -> LogicalSweep(epoch)
  -> Idle
```

Initiation under the coordinator lock publishes `Starting` even when active
mutators exist. It does not wait, deny an otherwise valid mutator entry, or
invoke collection while holding managed-data locks. A new mutator observes the
active epoch and joins its barrier protocol before accessing managed data.

Each active thread/heap participant records at least:

- recursive mutator depth;
- observed collection and reclamation epochs;
- acknowledgement state for start/final-remark handshakes;
- its pending mutation/root barrier buffer; and
- the set or counted table of run cursor pins acquired during that outer
  mutator quantum.

Different heaps have independent participant records. No transition waits
while holding coordinator or managed-data locks belonging to another heap.

### Run lifecycle

The provisional lifecycle is:

```text
Open(generation)
  -> Sealing(generation, candidate_epoch)
  -> Sealed(generation, candidate_epoch)
  -> Retired(generation, reclamation_epoch)
  -> Free(generation + 1)
```

Logical sweep may select a completed-mark empty run as a candidate and
atomically remove it from its allocation-class frontier. `Sealing` prevents
new reservation claims. Existing cursor pins may finish; the collector records
their post-snapshot publications as live for that cycle and never waits for a
pin while holding a collector lock.

The run is not recycled in the candidate epoch. A later collection may confirm
it empty only after new claims have remained closed. A confirmed empty run is
retired, finalized if necessary, and reused only after the reclamation epoch
proves that no older mutator reference remains. A run containing live values
may remain sealed or be reopened after rebuilding exact reservation state; CG4
selects and verifies that policy.

### Allocation cursors and reservations

The existing one-allocation-word lease remains the fast allocation unit.
Concurrent recycling adds:

- run generation to every cached cursor;
- an atomic run state checked before a new lease claim;
- one active cursor pin acquired lazily on first use during a mutator quantum;
- a state/generation recheck after pin acquisition;
- release of active pins at outer mutator exit; and
- cursor discard when the cached generation no longer matches.

Inactive TLS cache entries do not pin runs. They may retain stale cursor bits
indefinitely because generation validation makes those bits inert. Once a run
is sealed and its active cursor-pin count reaches zero, the collector may clear
its lease bitmap without walking TLS. It still may not destroy or reuse the run
until access quiescence is established.

### General managed access

Allocator pins do not protect arbitrary `Gc<T>` reads. The initial direction
is heap-local epoch reclamation: a mutator admission is also an access epoch
guard, and physical destruction/reuse waits for every pre-retirement guard to
leave.

This preserves the current API property that a managed reference may remain
valid for its mutator borrow without adding a pointer-local lock or per-read
run pin. It does not prove semantic reachability of an otherwise unregistered
local `Gc<T>`; CG0-CG1 must close that root problem separately.

### Concurrent marking

The provisional baseline is a conservative SATB-style protocol:

- the existing managed-edge replacement gateway records the overwritten edge
  and may initially shade both old and new edges;
- external-root removal records the removed root before publication;
- root insertion during an active epoch shades the inserted value;
- one-write publications into managed cells participate in the same gateway;
- allocations published after epoch initiation are retained for that cycle;
- mark bitmaps and work queues support atomic/concurrent discovery; and
- final remark waits for every pre-remark participant to acknowledge and for
  all barrier buffers to drain, without depending on heap acquisition order.

Tracing remains observational. Immutable objects need no object-local
synchronization. Each mutable managed family must supply a reviewed coherent
snapshot protocol, normally by briefly taking its semantic lock, copying or
submitting exact edges, and releasing it without calling arbitrary code.

The exact policy for post-snapshot allocation—black allocation bits, birth
epochs, or segregated allocation runs—is a CG2 design gate. It must make
publication and marking one unambiguous linearized transition.

### Finalization and recycling

Logical death does not authorize immediate `Drop`. Finalization begins only
after the relevant access grace period, uses the existing passive Glam
destructor contract, and runs outside collector locks. A panic retains durable
pending-finalization state as in the reference collector and does not expose
the run for reuse.

Physical recycle requires all of:

1. confirmed semantic emptiness in a later collection epoch;
2. zero active allocation-cursor pins;
3. completed finalization for every drop-bearing allocation; and
4. completion of the run's reclamation grace period.

Run generation advances before the address is published as another typed or
free run. Cached cursors and any debug lookup which still records the older
generation must reject it.

## Semantic and Safety Invariants

1. Collection initiation never waits for heap-wide mutator count to reach
   zero and never makes nested heap order part of progress.
2. A mutator may enter several heaps in any order. Each heap's epochs, barriers,
   cursor pins, and acknowledgements remain independent.
3. A run closed to reservation cannot receive a new allocation-word lease.
   A claimant racing closure either pins the old open generation or observes
   closure and retries elsewhere.
4. A cached inactive cursor owns no reclamation right. Address plus generation
   must match before it becomes active again.
5. No allocation is swept merely because it was absent from the epoch-start
   snapshot. Post-snapshot publication is explicitly retained.
6. No edge or root can disappear from the snapshot without entering an SATB
   barrier buffer first.
7. No physical slot or run is destroyed, cleared, retyped, or reused while an
   older access-epoch guard may observe it.
8. Collection, reclamation, and run generations are nonzero and do not silently
   wrap into a valid older identity.
9. Finalizers receive no Glam runtime or heap capability and run outside
   collector locks after access quiescence.
10. Collector scheduling, epochs, barriers, delayed reclamation, and run
    selection are operationally invisible to pure Glam evaluation.
11. The reference stop-the-world collector and concurrent collector agree on
    the live graph at every comparison checkpoint. Conservative extra
    retention is permitted only where named by this plan.
12. A panic before irreversible physical mutation leaves a retryable attempt;
    a panic after an irreversible boundary follows a separately reviewed poison
    or durable-finalization protocol.

## Transition Phases

Every completed `CG` phase ends with a dated implementation-versus-plan review
before the next phase begins. Drift is classified as intentional, corrective,
or accidental under the same policy as the integration plan.

### CG0 — Post-Integration Baseline and Root-Lifetime Decision

- Re-run the final ownership, trace, mutation, and destruction inventories
  against the post-G4 code.
- Add deterministic starvation fixtures with continuously overlapping
  mutators, including an outer heap which is always entered before an
  allocation-heavy inner heap and the opposite order on another thread.
- Inventory every way a bare managed pointer or managed reference can remain
  local across concurrent epoch initiation.
- Select the transient-root protocol: explicit local-root handles, registered
  mutator root frames, or another exact construction. Do not assume that the
  Rust stack can be scanned.
- Record baseline pause, throughput, run pressure, lease retention, and memory
  behavior under the reference collector.

Hard gate: concurrent marking may not begin until every pre-existing local
managed reference is either discoverable or protected by a proven SATB origin
and access-epoch rule.

### CG1 — Participant Epochs and Nonblocking Initiation

- Introduce heap-local mutator participant records and collection/reclamation
  epoch publication without changing mark or sweep behavior.
- Permit collection initiation while participants are active; initially end
  the synthetic attempt without reclaiming anything.
- Add start and final-remark acknowledgement handshakes at bounded mutator
  boundaries.
- Prove that initiation never blocks new hierarchical heap entry and never
  waits while holding another heap's lock.
- Integrate the CG0 transient-root protocol.

Verification forces every two-heap acquisition ordering, recursive same-heap
entry, participant exit during initiation, thread exit, panic, and a participant
which delays acknowledgement while unrelated heaps continue.

### CG2 — Concurrent Mark State and Barriers

- Make mark discovery and the object work queue safe for concurrent producer
  barriers and collector consumption.
- Activate edge-replacement, root, and one-write-publication barriers.
- Select and implement the post-snapshot allocation policy.
- Audit every mutable managed visitor for a coherent concurrent snapshot.
- Run concurrent marking without sweeping; retain all allocations.

Verification compares the completed mark set with an immediately following
reference stop-the-world mark over deterministic edge/root replacement races,
new allocation publication, lazy/promise assignment, interaction-net mutation,
and root insertion/removal.

### CG3 — Concurrent Mark Completion and Termination

- Implement barrier-buffer draining, final-remark acknowledgement, and exact
  termination detection.
- Ensure continuously entering post-epoch mutators cannot prevent a bounded
  pre-epoch participant set from completing remark.
- Publish a complete immutable mark result for logical sweep.
- Preserve panic/retry behavior without exposing a partial mark as complete.

Hard gate: no run state may change until the concurrent mark result repeatedly
matches the reference collector under forced schedules.

### CG4 — Run Generations, Sealing, and Cursor Pins

- Add the run lifecycle and nonzero generation.
- Remove sealing runs from allocation frontiers before blocking new claims.
- Add generation-checked cached cursors and lazily acquired active cursor pins.
- Make outer mutator exit release every active pin without walking unrelated
  inactive cursors.
- Select the reopen policy for a sealed run later found to contain survivors.
- Do not recycle any run in this phase.

Verification covers claim-versus-seal, pin-versus-seal, exit/panic with pins,
inactive stale cursors, address/generation ABA, several cursors in one run, and
allocation publication by a cursor which won immediately before closure.

### CG5 — Delayed Logical Sweep

- Classify completed-mark empty runs as sealing candidates in epoch N.
- Confirm emptiness no earlier than a later collection epoch after reservation
  closure.
- Keep post-snapshot allocations and any discovered survivor exact.
- Move confirmed empty runs into durable retired state, but do not finalize or
  recycle them yet.
- Maintain pressure statistics which distinguish open, sealed, and retired
  capacity without making those counts semantic.

Verification demonstrates that epoch N never eagerly recycles a candidate,
that an old cursor publication prevents false emptiness, and that a sealed live
run follows the selected reopen/retention policy.

### CG6 — Access Grace Period, Finalization, and Physical Recycling

- Compute the oldest active reclamation epoch without scanning managed data.
- Release a retired no-drop run only after its grace period.
- Run drop-bearing finalization after the same grace proof and outside locks.
- Advance generation and rebuild run/class reservation state before reuse.
- Preserve durable retry after a finalizer panic.

Verification holds managed references across retirement, releases them in
forced orders, races run reuse with stale cached cursors, and proves every
payload is dropped exactly once or remains durably pending after panic.

### CG7 — Runtime Integration and Policy

- Add concurrent collection as an immutable runtime construction policy; do
  not mutate live heaps between collector modes.
- Integrate collection activity with runtime readiness and settlement without
  exposing epoch details to Glam evaluation.
- Retain explicit reference-collector and `NoAuto` modes for verification.
- Exercise workers, reflection, interaction nets, diagnostics, macros, imports,
  and assembly output under forced concurrent schedules.
- Decide only after measurements whether concurrent collection becomes the
  production default.

### CG8 — Final Audit and Tuning

- Audit every unsafe pointer, barrier, mark, run-state, generation, pin,
  finalizer, and reclamation transition.
- Reconcile architecture and agent-context documentation.
- Run Miri on focused state machines, deterministic forced-order suites, Loom
  where its model is tractable, and supported address/thread sanitizers.
- Compare memory, throughput, pause, and fragmentation with the reference
  collector. Performance observations guide tuning but do not alter Glam
  results.
- Consider eager same-epoch recycling or parallel tracing only as new reviewed
  checkpoints after the conservative implementation is certified.

## Verification Matrix

At every implementation checkpoint:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
cargo test --workspace -q
```

The concurrency proof relies on controlled schedules, not stress alone. The
minimum forced cases include:

- perpetual overlap which starves the reference idle-entry collector;
- opposing two-heap nesting orders with only the inner heap allocating;
- collection initiation during every mutator admission/exit boundary;
- edge and root deletion immediately around snapshot publication;
- allocation immediately around mark initiation and run sealing;
- reservation claim immediately around frontier removal;
- active and inactive cursor reuse across run-generation change;
- semantic emptiness in N followed by publication before closure drains;
- confirmed emptiness in N+1 with an older access guard still active;
- finalizer success and panic after grace-period completion; and
- concurrent and reference collectors producing byte-identical sample output.

Use model/state-machine tests for run lifecycle and participant epochs. Every
newly discovered ordering defect receives a deterministic regression before
repair.

## Open Design Gates

These are deliberately unresolved until CG0 supplies the post-integration
inventory:

1. The exact transient-root representation for bare/local managed pointers.
2. SATB-only barriers versus initially shading both old and new edges.
3. Black allocation, birth epochs, or segregated runs for post-snapshot
   allocation.
4. Atomic packing and synchronization of run state, generation, and active
   cursor pins.
5. Per-thread participant slots versus another heap-local reclamation-epoch
   registry.
6. Whether sealed survivor runs reopen automatically or remain sealed until a
   later compaction/performance plan.
7. Whether final remark needs a brief scheduler-coordinated pause or can remain
   a fully asynchronous acknowledgement protocol.
8. Whether production should ever discard the reference stop-the-world
   collector rather than retaining it as an oracle and maintenance mode.

Each gate requires a dated decision, forced-order verification plan, and
review of effects on later phases before its implementation begins.

## Completion Criteria

- Collection can begin and finish despite continuously overlapping bounded
  mutators and arbitrary multi-heap nesting order.
- No collection phase requires heap-wide mutator exclusivity.
- A sealed run receives no new reservation, and a recycled run has passed
  semantic, allocator, finalization, and access-quiescence proofs.
- Cached cursor ABA is rejected by run generation without scanning TLS.
- General managed reads acquire no pointer-local lock.
- Concurrent and reference collectors agree on live graphs and Glam outputs.
- Finalization remains passive, outside locks, retryable under the documented
  panic protocol, and exactly once on success.
- The implementation has dated reviews after every major `CG` phase and a
  final unsafe/concurrency audit with no unresolved soundness finding.

