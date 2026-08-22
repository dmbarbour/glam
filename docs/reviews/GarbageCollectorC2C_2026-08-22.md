# Glam GC C2C Implementation Review — 2026-08-22

Baseline: `4e99d78`. This is the mandatory post-C2C review of the isolated
`glam-gc` allocator against the implementation plan, GC roadmap, safety
ledger, and verification surface. The current working plan additionally
contains the subsequent decision to lower C3B's automatic trigger from 128 to
112 typed-run publications.

## Outcome

C2C establishes a credible non-moving allocation foundation. Chunks and runs
have one enumerable owner, allocation classes retain exact heap provenance,
payload initialization precedes allocation-bit publication, atomic lease bits
give one worker exclusive ownership of each ordinary allocation word, stable
class frontiers remove the heap mutex from ordinary cursor refill, and inert
TLS caches cannot retain or call back into a dead heap. The checked unsafe
inventory, full leak-checking Miri, AddressSanitizer, ThreadSanitizer, and the
native concurrent fixtures found no current allocation-path race or invalid
payload lifetime.

The review did find one forward safety boundary and one coordination exception
which must be made explicit before collection can become real:

1. allocation-class discovery currently mutates the class registry without
   mutator admission; it is safe to leave concurrent only if collection treats
   it as metadata-only and never retains class-table borrows outside the heap
   lock; and
2. the safe generic collector API can construct a cross-heap managed edge, so
   C5 must reject foreign traced edges in every build before reclamation,
   rather than validate them only in debug and tests.

Neither issue makes C2C unsafe while collection remains disabled. The first is
a C3 design decision; the second is a C5 release-safety requirement. One
material C2C verification gap also remains: current tests do not force several
threads through the exhausted-frontier slow-path recheck. Close that gap before
starting C3A, then partition C3 around the admission transitions described
below.

## Reviewed Representation

| Concern | Authoritative C2C representation | Review conclusion |
| --- | --- | --- |
| Heap storage | `HeapState::arena`, aligned chunks, typed run headers | Sound for stable non-moving allocation; collector-time retirement remains C6. |
| Type identity | process-lifetime `ObjectMetadata` pointer plus heap-local dense class | The static metadata leak is intentional and bounded by used Rust representations. |
| Run ownership | boxed `RunClaimTarget` records owned by one class entry | Stable frontier addresses are valid for the heap lifetime; C5/C6 must clear frontiers before retiring or repurposing records. |
| Word ownership | one atomic lease bit per ordinary allocation word | CAS ownership and non-atomic worker-local allocation words are consistent under the current no-collection phase. |
| Object publication | payload write, then allocation-bit write, both under one word owner | Correct; panic-capable preparation ends before either publication write. |
| Thread cache | heap-qualified weak TLS record, recursive depth, epoch, direct cursor cache | Correctly inert on collision, release, and thread exit; stale weak records prevent Arc-address reuse without retaining the heap. |
| Pressure | saturating successful typed-run publication count | Coherent as an initial heap-growth heuristic, but not an allocation-volume heuristic. |
| Teardown | final `HeapInner` owner enumerates allocation bits and drops payloads | Adequate only under the documented non-reentrant, non-panicking C2C restriction; C6 still owns real finalization. |

## Findings

### GC2C-001 — C3 must classify concurrent allocation-class discovery

**Priority:** medium  
**Confidence:** high  
**Classification:** forward coordination precision

[`Heap::allocation_class`](../../crates/glam-gc/src/heap.rs#L51) is a safe
operation which may discover and publish a new heap-local class. It requires no
mutator and reaches the class table through
[`discover_class_with`](../../crates/glam-gc/src/heap.rs#L199). That is valid in
C2C because there is no collector.

C3 currently plans to infer exclusive allocator access after the active
mutator count reaches zero, then release the phase/state mutex while tracing
or sweeping. Class discovery can still enter independently and mutate the
class vector and metadata index, so “no active mutators” does not literally
mean that every `HeapState` field is immutable.

Class discovery does not publish a run, allocation bit, payload, root, or
edge. Because payload allocation still requires a mutator, a class discovered
during collection is empty and cannot appear in the fixed root graph. The
cheapest policy is therefore to preserve the useful pre-discovery API and
classify class discovery as metadata-only concurrency:

1. every collector traversal snapshots the run location, geometry, and static
   metadata it needs while holding heap state;
2. no borrow or pointer into the movable class vector survives release of that
   lock;
3. collector mutation of class run pools reacquires heap state; and
4. only run/payload/root mutation, not publication of an empty class entry, is
   excluded by mutator admission.

If later implementation wants to retain class-table references across the
pause, use a short topology-admission obligation instead. Do not add that
counter preemptively. Tests should discover a new class during a synthetic
exclusive phase and prove that it remains empty, does not enter the collector
snapshot, and becomes allocatable after ordinary admission reopens.

This is a C3A/C3B plan prerequisite, not a reason to change C2C allocation
semantics.

### GC2C-002 — C5 foreign-edge validation must be release-enforced

**Priority:** high  
**Confidence:** high  
**Classification:** future reclamation safety

The roadmap correctly requires one heap per runtime and no cross-runtime
managed edge. Production Glam wrappers will reject foreign `Value` inputs at
their public boundary. The generic collector crate, however, can construct a
foreign edge using only safe calls:

```rust
let first = Heap::new();
let second = Heap::new();
let leaf = second.allocation_class::<u64>().unwrap();
let foreign = second.with_mutator(|m| m.alloc(&leaf, 1));

let edge = first.allocation_class::<Gc<u64>>().unwrap();
let _holder = first.with_mutator(|m| m.alloc(&edge, foreign));
```

This works because safe [`Mutator::alloc`](../../crates/glam-gc/src/mutator.rs#L57)
validates the allocation class but does not inspect `value`, while the crate
itself implements [`Trace for Gc<T>`](../../crates/glam-gc/src/trace.rs#L105).
That is harmless before collection: terminal destruction of a `Gc` does not
dereference it. It becomes a soundness boundary when exact marking follows the
edge.

The C5 plan currently says to validate traced-pointer ownership only in debug
and test configurations. Change this to an all-build checked owner lookup. A
foreign, stale, non-slot, or otherwise invalid traced edge must abort that
collection attempt before sweep and return a structured collection/graph
error. All allocations remain intact and the heap returns to its ordinary
phase. Debug builds may add richer representation assertions, but release
marking cannot assume integration already upheld the invariant.

Add the safe construction above as a regression and prove that collection
fails recoverably without dereferencing the foreign address or reclaiming from
either heap. Glam integration should still reject the edge earlier; the
collector check is the final safety boundary, not the desired user-facing
diagnostic path.

### GC2C-003 — C3 entry needs an explicit prepare/admit/activate protocol

**Priority:** medium  
**Confidence:** high  
**Classification:** plan precision and checkpoint sizing

Current [`ThreadHeapEntry::enter`](../../crates/glam-gc/src/thread_cache.rs#L258)
performs the direct TLS lookup, epoch refresh, and recursive-depth increment as
one operation. C3 must insert a potentially blocking coordinator admission for
an outer entry while preserving these properties:

- a recursive same-heap entry is recognized before it waits behind a pending
  collector;
- a new outer entry is counted by the coordinator before its TLS cache becomes
  active;
- a panic after coordinator admission but before TLS activation rolls the
  active count back; and
- outer exit first makes TLS/cursors quiescent and only then decrements the
  coordinator count.

Simply adding a coordinator call before or after the current `enter` creates a
window in one direction or the other. Split the operation conceptually:

1. **prepare:** find/create the heap-qualified TLS record and classify the
   request as recursive or outer without incrementing depth;
2. **admit:** for an outer entry, obtain the coordinator obligation with a
   rollback guard; recursive entry reuses the existing obligation;
3. **activate:** refresh the epoch, increment depth, and transfer rollback to a
   combined RAII entry; and
4. **drop:** decrement depth, then retire the outer coordinator obligation.

This seam also supplies C3C with the information needed to distinguish a
dependent cross-heap entry from an ordinary outer entry.

Partition C3 before implementation:

- **C3A.1:** coordinator/phase representation and nonblocking ordinary
  topology plus mutator admission;
- **C3A.2:** prepare/admit/activate TLS integration, recursion, and unwind;
- **C3A.3:** release/acquire visibility tests and the first real Loom model;
- **C3B.1:** explicit request, commitment, election, and a synthetic exclusive
  collection body;
- **C3B.2:** synchronous `collect_full`, waiters, and teardown;
- **C3B.3:** outer-exit servicing and the 111/112 automatic-pressure boundary;
- **C3C.1/C3C.2:** dependent cross-heap admission, then forced reciprocal
  schedules; and
- **C3D.1–C3D.3:** collector-to-finalizer handoff, follow-up pressure, then
  panic/waiter audit.

This follows the project rule that a checkpoint crossing several independent
unsafe or scheduler boundaries should be divided before implementation.

### GC2C-004 — Frontier exhaustion is not forced under concurrent slow-path entry

**Priority:** medium  
**Confidence:** high  
**Classification:** C2C verification gap

The implementation has the correct shape: a cursor miss first claims through
the atomic frontier, then locks heap state and rechecks the frontier before
advancing or publishing
([`claim_allocation_cursor`](../../crates/glam-gc/src/heap.rs#L310)). The tests
cover two adjacent pieces, but not their concurrent boundary:

- `concurrent_mutators_claim_disjoint_words_in_one_run` races eight claimers
  while the initial run has ample capacity
  ([test](../../crates/glam-gc/src/heap.rs#L1097)); and
- `exhausted_frontier_advances_through_prepublished_runs` advances one thread
  after exhausting a prepublished run
  ([test](../../crates/glam-gc/src/heap.rs#L1159)).

No test forces several threads to observe the same exhausted frontier before
one advances it. Add a deterministic test hook after the initial fast-path
miss and before heap-state acquisition. Pre-exhaust the current run, release
several claimers together, and assert:

- exactly one next run is published or activated;
- all returned allocation words are distinct;
- every loser rechecks and consumes the winner's frontier;
- pressure advances exactly once for a newly published run; and
- no thread rescans an exhausted prefix.

An abstract Loom model may cover the winner/recheck state, but the native
forced schedule should exercise the production frontier pointer and heap
mutex. Resolve this before beginning C3A so later coordination failures are not
confounded with an unlatched allocator race.

### GC2C-005 — The initial pressure policy is a growth heuristic, not a reuse heuristic

**Priority:** low  
**Confidence:** high  
**Classification:** accepted initial limitation requiring later review

C2C counts only successful typed-run publication
([`AllocationPressure`](../../crates/glam-gc/src/heap.rs#L101)). This is an
excellent low-traffic trigger for initial heap growth and the recently chosen
7/8 threshold leaves useful first-chunk headroom. It does not measure object
allocation into already published runs.

The current C6 plan resets the count after sweep, then allows reclaimed slots
and runs to be reused without another pressure event. A churn workload can
therefore refill all reclaimed capacity without requesting another collection;
only subsequent run publication resumes the counter. With a reset-to-zero
policy, it can then grow by roughly another trigger allowance before automatic
collection.

This does not affect correctness and need not complicate the initial collector.
Record it explicitly as a provisional heap-growth policy. Before C6 rearms
automatic collection, choose and test one of:

- bias the post-sweep counter from survivor/free-capacity information so the
  first growth beyond reclaimed capacity can trigger promptly;
- count a run's first post-sweep activation once, still avoiding per-object
  traffic; or
- retain growth-only automatic collection and require explicit/host requests
  for stable-capacity churn until C8 profiling supplies a better policy.

Do not silently describe the current counter as allocation-volume pressure.

### GC2C-006 — Lease publication ordering is sound but described imprecisely

**Priority:** low  
**Confidence:** high  
**Classification:** unsafe-proof documentation

[`RunClaimTarget::claim_allocation_word`](../../crates/glam-gc/src/arena.rs#L111)
says its Acquire load of a lease word “pairs with publication of a stable run
record.” That publication is actually the Release store to the class frontier,
paired by the Acquire frontier load in
[`AllocationClassShared::claim_frontier`](../../crates/glam-gc/src/class.rs#L248).
Claims reached under heap state instead rely on mutex synchronization.

The lease-word Acquire has a separate future role: after C5 rebuilds lease
state under collector exclusion, a Release reset can publish the rebuilt
ordinary allocation word and sweep state to the next winning claimant. Update
the inline proof and safety ledger to distinguish:

1. initial run/topology publication through the frontier Release/Acquire or
   heap mutex; and
2. post-collection word-state publication through lease reset Release and
   claim Acquire.

No ordering change is recommended for C2C. The clarification should accompany
C3/C5's exact reset protocol and a forced post-reset claim test.

### GC2C-007 — Phase/status documentation overstates completed C2C behavior

**Priority:** low  
**Confidence:** high  
**Classification:** documentation drift

The C2C.5b section lists explicit `request_collection`, successful/failed
sweep rearming, and finalizer publication behavior as C2C verification
requirements even though those operations correctly remain owned by C3 and C6
([plan](../plans/GarbageCollectorImplementation_2026-08-19.md#c2c5b-typed-run-publication-collection-pressure)).
The completion section is more accurate, but the phase contract reads as if
unimplemented tests were required before C2C.5c began.

Additionally:

- the GC roadmap status still says C2C.1a “may proceed”
  ([roadmap](../plans/GarbageCollectionRoadmap_2026-08-19.md#glam-owned-garbage-collection-roadmap--2026-08-19)); and
- `VERIFY.md` initially describes one Loom smoke model even though the stable
  check now runs an entry smoke model plus the lease-CAS model.

Move sweep, finalizer, and explicit-request bullets into clearly labeled
“constraints on C3/C6” subsections, leave only implemented threshold and
failure-atomicity checks under C2C.5b, update status after this review closes,
and use plural “Loom models” consistently.

## Intentional Drift and Accepted Boundaries

- **Stable run records are separately boxed.** The extra allocation per typed
  run is justified by an address-stable frontier without coupling atomic
  readers to `Vec` storage. Retain this for the initial collector.
- **Forgotten TLS leases are not returned.** This intentionally trades some
  temporary capacity for a callback-free, heap-independent TLS lifecycle. C5
  revokes all leases under stop-the-world exclusion.
- **The synchronized allocator remains test-only.** It is a useful correctness
  oracle but must never run concurrently with production cursors because it
  does not participate in lease ownership.
- **Terminal destruction remains provisional.** Its non-reentrant,
  non-panicking restriction is explicit and sufficient while collection is
  disabled. Do not expand it instead of implementing C6 finalization.
- **The Loom CAS model is abstract.** Native forced schedules, Miri, and
  sanitizers cover raw arena integration. C3 must add real state-machine models
  rather than treating the abstract CAS model as general collector proof.

## Forward Phase Assessment

| Phase | Assessment after C2C |
| --- | --- |
| C3 | Semantics remain appropriate, but GC2C-001 and GC2C-003 require an explicit metadata-only class-discovery policy, ordered admission, and smaller checkpoints before implementation. |
| C4 | Root-registry ownership remains compatible with pointer-sized `Gc`; release-validating root creation is still required. |
| C5 | Visitor/worklist design remains appropriate. Upgrade foreign-edge validation to an all-build recoverable collection error and specify lease-reset ordering per GC2C-002/006. |
| C6 | Existing no-drop/finalizer partition is useful. Resolve the pressure-rearm limitation in GC2C-005 before claiming useful automatic repeat collection. |
| C7 | Stress scope remains appropriate; add forced frontier exhaustion now rather than postponing it to general stress. |
| C8 | Keep tuning contingent on profiling, including run size, cache width, and repeat-collection pressure. |

## Recommended Resolution Order

1. Add GC2C-004's forced exhausted-frontier race test.
2. Update the plan for GC2C-001, GC2C-002, GC2C-003, GC2C-005, and
   GC2C-006; divide C3 before coding.
3. Correct GC2C-007's documentation/status drift.
4. Re-run the collector check and ThreadSanitizer after the frontier fixture.
5. Mark the mandatory post-C2C review complete, then begin C3A.1.
