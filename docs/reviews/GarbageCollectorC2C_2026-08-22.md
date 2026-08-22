# Glam GC C2C Implementation Review — 2026-08-22

Baseline: `4e99d78`. This is the mandatory post-C2C review of the isolated
`glam-gc` allocator against the implementation plan, GC roadmap, safety
ledger, and verification surface. The current working plan additionally
contains the subsequent decision to lower C3B's automatic trigger from 128 to
112 typed-run publications.

Status: closed on 2026-08-22. Accepted forward requirements remain assigned to
their owning phases; no unresolved C2C defect blocks C3A.1.

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

The review did find one forward safety boundary and one coordination boundary
which must be made explicit before collection can become real:

1. allocation-class discovery currently mutates the class registry without
   mutator admission; C3 will move discovery behind an admitted mutator so an
   exclusive collector sees stable heap-local class topology; and
2. the safe generic collector API can construct a cross-heap managed edge, so
   C5 must detect foreign traced edges in every build and panic safely before
   reclamation, rather than validate them only in debug and tests.

Neither issue makes C2C unsafe while collection remains disabled. The first is
a C3 design decision; the second is a C5 release-safety requirement. The one
material C2C verification gap found by the review has been closed by C2C.6's
forced exhausted-frontier schedules. The forward requirements are now recorded
under their owning phases, and the immediately actionable C3A work has been
partitioned around the admission transitions described below. The review also
records the recommended partitions for later C3 checkpoints.

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

### GC2C-001 — C3 must gate allocation-class discovery with a mutator

**Priority:** medium  
**Confidence:** high  
**Classification:** forward coordination precision

**Decision:** accepted; implement in C3A.1

[`Heap::allocation_class`](../../crates/glam-gc/src/heap.rs#L51) is a safe
operation which may discover and publish a new heap-local class. It requires no
mutator and reaches the class table through
[`discover_class_with`](../../crates/glam-gc/src/heap.rs#L199). That is valid in
C2C because there is no collector.

C3 currently plans to infer exclusive allocator access after the active
mutator count reaches zero, then release the phase/state mutex while tracing
or sweeping. Class discovery can still enter independently and mutate the
class vector and metadata index, so “no active mutators” does not literally
mean that every heap-local topology field is immutable.

The settled policy is to make discovery itself a mutator capability:

```rust
let class = heap.with_mutator(|mutator| {
    mutator.allocation_class::<Node>()
})?;
```

An already discovered `AllocationClass<T>` remains a reusable, cacheable
heap-provenance handle and does not borrow the mutator. Requiring admission
only for discovery gives C3 the stronger and simpler invariant that the class
table, metadata index, and run topology cannot change once all mutators drain.
It also avoids a second topology counter, collector snapshots designed around
concurrent class-vector growth, and a hidden mutator acquisition inside a
seemingly cheap `Heap` query.

Implementation should let `Mutator` borrow the heap's existing `Arc<HeapInner>`
so discovery can construct the retained typed handle without cloning an Arc
for each mutator region. Tests must prove that discovery blocks behind an
exclusive synthetic collection, recursive admission can discover a class,
and a retained handle remains usable after its discovery mutator exits.

This is a C3A API/admission prerequisite. C2C's current public method remains
historical implementation state, not the intended collector boundary.

### GC2C-002 — C5 must panic safely on a foreign traced edge

**Priority:** high  
**Confidence:** high  
**Classification:** future reclamation safety

**Decision:** accepted; enforce as a collector invariant in C5

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

The settled user contract is deliberately simpler: every managed edge stored
in an object allocated by heap H must point to another live allocation in H.
The collector trusts ordinary Glam construction and reviewed mutation wrappers
to maintain that invariant. It does not perform a second validation trace
before every payload write, and the unsafe mutation/`Trace` boundary remains
responsible for state changed outside those wrappers.

C5 nevertheless needs an all-build checked owner lookup for each edge while
performing the trace it already requires for marking. A foreign, stale,
non-slot, or otherwise invalid traced edge panics before the collector
dereferences that address or begins sweep. This is an invariant panic, not a
recoverable `CollectionError` variant.

“Panic safely” has a precise collector meaning:

1. pointer ownership and slot validity are checked before dereference;
2. marking does not reclaim or run destructors;
3. an unwind guard abandons the partial mark epoch and restores ordinary heap
   admission; and
4. every allocation in both heaps remains intact.

If the application catches the panic, a later collection may be attempted,
but it will panic again while the same invalid reachable edge remains. With
`panic = "abort"`, process termination still occurs without collector-induced
undefined behavior. Debug builds may enrich the panic with class and address
details, but release marking must perform the ownership check.

Add the safe construction above as a regression and prove the panic occurs
before dereferencing the foreign address or reclaiming from either heap. Glam
integration should still reject the edge earlier at its runtime-value
boundary; the collector panic is the final invariant boundary, not a normal
user-facing diagnostic path.

### GC2C-003 — C3 entry needs an explicit prepare/admit/activate protocol

**Priority:** medium  
**Confidence:** high  
**Classification:** plan precision and checkpoint sizing

**Decision:** accepted; use the heap-state `Mutex` plus one `Condvar`

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

The settled coordinator is an explicit active-count state machine protected by
the heap-state `Mutex`, with one sibling `Condvar` for blocked entrants and
collection waiters. An admitted mutator retains no mutex or read-lock guard:
it increments the active count, releases the mutex, and executes concurrently
with other admitted mutators. Every condition-variable wake rechecks the phase
and caller classification under the mutex.

A bare `RwLock` is deliberately rejected. Its reader/writer priority is not a
portable policy boundary, and it cannot admit only a thread which already
holds another heap's mutator while keeping an ordinary new reader behind a
queued collector. Layering that policy over an `RwLock` would duplicate the
active state and introduce a handoff race. Under the selected state machine,
the final active mutator and a dependent entrant race atomically with the
collector's transition to `Collecting` under one mutex.

Partition C3 before implementation:

- **C3A.1:** heap-state mutex/condition-variable coordinator, phase
  representation, ordinary mutator admission, and migration of
  allocation-class discovery to that admitted capability;
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

**Resolution:** completed as verification-only Phase C2C.6; no repeat review

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
confounded with an unlatched allocator race. This checkpoint changes no
semantics and does not trigger another mandatory post-C2C review.

C2C.6 now forces both successor activation and publication with eight claimers
held after the same atomic miss. The tests latch one advance attempt, seven
winner-frontier recheck hits, eight distinct words, and exact pressure behavior
under the production mutex and frontier implementation. Miri, AddressSanitizer,
ThreadSanitizer, the stable collector check, and full workspace checks pass.

### GC2C-005 — The initial pressure policy is a growth heuristic, not a reuse heuristic

**Priority:** low  
**Confidence:** high  
**Classification:** resolved forward C6 policy

**Decision:** replace publication history with recyclable-run occupancy in C6

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

The settled C6 policy is to recycle every fully cleared run into a heap-wide
free-run list. A run is no longer attached to its old allocation class after
its payloads are dead, all required destruction has completed, its frontiers
and pool membership are retired, and its header/bitmap state is safe to
reinitialize. Assigning either a virgin run or a recycled run to an allocation
class is one occupancy event; allocating another object inside an assigned run
is not.

Automatic pressure is based on assigned typed runs and a survivor-relative
high-water mark. With `S` assigned runs retained by the completed collection,
the next trigger has the shape:

```text
S + (RUNS_PER_CHUNK * 7 / 8) + ceil(S * survivor_growth_ratio)
```

The fixed term preserves the initial 112-run trigger when `S == 0`; the growth
term raises the water mark for a high-survivor collection instead of running
GC again after one additional activation. The exact ratio remains an internal
C8 tuning constant rather than public collector semantics. Sweep decrements
assigned occupancy for each run returned to the free list, and later
activation increments it again, so stable-capacity churn naturally approaches
the next absolute trigger without per-object accounting. The target may exceed
currently committed chunk capacity; the allocator grows by ordinary retained
chunks as needed. Explicit collection remains available independently.

For drop-type runs, compute the marked-survivor contribution before
finalization, then finalize the trigger when the finalization batch drains.
Finalizer allocations consume the new headroom rather than becoming part of
the survivor baseline; a quarantined run does become retained baseline. This
allows a sufficiently allocating finalizer to request one follow-up collection
without attempting collection during `Finalizing`.

The bootstrap does not return wholly empty chunks to the host. They retain
stable arena indexing and contribute free runs until heap destruction. Chunk
release or decommit is deferred because it adds address-index and free-list
retirement complexity for a case expected to be rare.

### GC2C-006 — Lease publication ordering is sound but described imprecisely

**Priority:** low  
**Confidence:** high  
**Classification:** unsafe-proof documentation

**Resolution:** initial publication corrected in C2C.6; later C3A.3/C5 proofs
remain planned

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

**Resolution:** completed; phase contracts and status surfaces reconciled

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

The implementation plan now limits C2C.5b to successful typed-run publication,
threshold latching, and failure atomicity. Explicit/coalesced requests are C3
constraints; sweep replacement, recycled-run occupancy, and finalizer pressure
are C6 constraints. The implementation-plan and roadmap status lines identify
C3A.1 as next, and the verification guide consistently describes both Loom
models.

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
| C3 | Semantics remain appropriate, but GC2C-001 and GC2C-003 require mutator-gated class discovery, ordered admission, and smaller checkpoints before implementation. |
| C4 | Root-registry ownership remains compatible with pointer-sized `Gc`; release-validating root creation is still required. |
| C5 | Visitor/worklist design remains appropriate. Upgrade foreign-edge validation to an all-build checked invariant panic with unwind-safe rollback, and specify lease-reset ordering per GC2C-002/006. |
| C6 | Existing no-drop/finalizer partition is useful. Implement GC2C-005's heap-wide free-run list and survivor-relative assigned-run high-water mark before claiming useful automatic repeat collection. |
| C7 | Stress scope remains appropriate. C2C.6 already owns the forced frontier-exhaustion schedules, so C7 need not rediscover that allocator boundary. |
| C8 | Keep tuning contingent on profiling, including run size, cache width, and repeat-collection pressure. |

## Resolution Ledger

| Finding | Review disposition | Owning checkpoint |
| --- | --- | --- |
| GC2C-001 | Accepted and specified; no collection exists in C2C, so implementation is intentionally forward work. | C3A.1 |
| GC2C-002 | Accepted as an all-build checked marking invariant with unwind-safe abandonment. | C5 |
| GC2C-003 | Accepted; coordinator primitives and prepare/admit/activate ordering are in the plan, C3A is partitioned, and later C3 partitions are recorded here for checkpoint-local review. | C3A.1–C3D |
| GC2C-004 | Closed by deterministic production-path races for prepublished activation and new-run publication. | C2C.6, completed |
| GC2C-005 | Accepted; free-run reuse and survivor-relative assigned-run pressure are specified as sweep policy. | C6, tuning in C8 |
| GC2C-006 | Initial-publication proof corrected; collector visibility and lease-reset proof remain explicitly assigned. | C2C.6 completed; C3A.3 and C5 pending |
| GC2C-007 | Closed by separating historical C2C checks from C3/C6 constraints and reconciling status/verification docs. | Documentation, completed |

## Review Closure

The mandatory post-C2C review is complete. C2C.6 passed the collector check
(74 unit tests, two Loom models, and six compile-fail doctests), full
leak-checking Miri, AddressSanitizer, ThreadSanitizer, workspace formatting,
Clippy with warnings denied, and the complete workspace test suite. The
remaining findings are deliberate forward obligations whose implementations
would be premature before their owning collector phases; they are not latent
C2C behavior.

C3A.1 is the next implementation checkpoint. It introduces the coordinator
and moves allocation-class discovery behind mutator admission without yet
adding collector election.
