# Collector Verification

Run the stable collector checks from the repository root:

```sh
crates/glam-gc/scripts/check.sh
```

The script formats, lints, and tests the crate, including compile-fail doctests
and the Loom models. `audit-unsafe.sh` then compares every unsafe construct
and every module-level unsafe opt-in with the checked-in inventories before
building all crate targets and features under the crate's default
`unsafe_code` denial.

Optional toolchain checks:

```sh
crates/glam-gc/scripts/check-miri.sh
crates/glam-gc/scripts/check-sanitizer.sh address
crates/glam-gc/scripts/check-sanitizer.sh thread
```

The sanitizer scripts exercise the collector library rather than the separate
Loom scaffold. Even an empty `loom::model(|| {})` currently retains one
256-byte Loom allocation under LeakSanitizer and emits the documented ASan
stack-switch warning; that tool incompatibility is independent of Glam heap
entry and explicit TLS release. The ordinary stable check continues to run the
Loom models separately.

These scripts deliberately fail with the underlying toolchain diagnostic when
nightly, Miri, `rust-src`, or a sanitizer is unavailable. Every checkpoint
which changes the unsafe allocation or access surface records a focused Miri
pass; later routine runs remain optional when the local toolchain component is
unavailable.

C1 temporarily passed `-Zmiri-ignore-leaks` because its prototype allocator
used `Box::leak`. C2C.1b replaced that path with arena-owned allocation and
terminal payload destruction, so `check-miri.sh` now keeps leak checking
enabled alongside pointer provenance, aliasing, initialization, thread access,
and mismatch gateways.

The Loom tests retain the heap-entry tooling/API smoke model and model C2C.5's
atomic lease-bit claim transition. Raw arena-pointer integration remains under
native forced schedules, sanitizers, and Miri. C2C.5 claims whole allocation
words through atomic lease bitmaps and consults the heap mutex only when a
class frontier is exhausted; after a claim, a worker is the only writer of its
atomic allocation word. C2C.6's native barrier fixtures force eight production claimers
past the same exhausted-frontier observation and verify one synchronized
advance or publication plus seven winner-frontier rechecks. C3 adds Loom models
for mutator-exit visibility, unique idle-entry election, reciprocal nested
admission with requests latched, and the no-gap
exclusive-to-finalizer-to-entry handoff. Native forced schedules exercise
production request epochs, idle-entry and synchronous election, waiter
coalescing, direct admission transfer, collector-local cache reset, the absence
of exit-time service, the finalizer mutator, request/pressure acknowledgement,
and unwind restoration. The coordination body began as a synthetic harness;
C4 and C5 now supply production roots and exact tracing, while C6 owns
reclamation and destructor recovery. C4A
adds release-checked direct roots and makes allocation-bit publication atomic
so root validation can inspect a word while its leased writer advances other
slots. C4B publishes each cell into a weak heap registry before returning its
public root and adds exclusive, stable, in-place traversal and pruning. C4C
integrates that walk with every elected collection and forces the last-root
drop on both sides of a temporary weak upgrade. The C4 walk remains a no-op
seed receiver until C5 adds marking.
Ordinary threaded stress remains supplementary rather than proof of coordinator
ordering.

## C5 Exact-Mark Verification

The ordinary stable check compares 24 deterministic randomized managed graphs
with an independent index-based reachability oracle. It checks the successful
report, every allocation's mark bit, and each object's trace count. A separate
full-run fixture drives one assigned run through all, one, and zero live slots
across three successful collections, proving that each attempt clears the prior
bitmap before publishing new reachability without resurrecting an allocation
which an earlier eager sweep reclaimed. The terminal zero-live collection
also permits C6A.2b retirement and C6A.2c reset; stale unrooted handles are
deliberately not resurrected afterward. Empty-heap and clear-before-mark
fixtures independently cover zero as an initial state. The roughly
eight-thousand-root fixture is
native-only; Miri retains the existing focused bitmap boundary, partial-mark
recovery, and one bounded 65-node randomized-oracle case.

The native million-edge fixtures are intentionally isolated from routine unit
test latency. Run them serially, with their worklist measurement visible, via:

```sh
crates/glam-gc/scripts/check-scale.sh
```

That script exercises a one-million-node chain and a flat one-million-edge
array through the checked non-recursive production marker. On 2026-08-23 the
flat fixture reported a peak object-worklist length of 1,000,000 and capacity
of 1,048,576. These are observations of the current LIFO `Vec` worklist, not
correctness or performance thresholds. The routine native suite retains its
20,000-node chain, while Miri uses the same path with 256 nodes; stack-depth and
million-edge scale remain native responsibilities.

C5D.2 adds no unsafe operation or module opt-in. The exact unsafe inventory
must therefore remain unchanged while these fixtures and their ledger entries
are added.

ThreadSanitizer passes the complete C5D.2 native suite. The post-C5 review
isolated the prior 24-byte LeakSanitizer report to
`forgotten_scoped_allocator_does_not_retain_its_heap`, the C4D fixture which
deliberately calls `mem::forget` on one inert frontier cell to prove that an
escaped allocator does not retain its heap. It is not TLS or managed-heap
retention and reproduces from the clean pre-C5D.2 `d7977d4` worktree.
`check-sanitizer.sh address` therefore runs every other test with leak
detection enabled, then runs that exact ownership fixture with ASan enabled and
leak detection disabled. This is an explicit process-lifetime-fixture
exception, not a general LeakSanitizer suppression.

## C6 Collection-Pipeline Verification

C6A.0 is a no-semantics handoff refactor. One focused fixture roots a managed
`u64` and proves the post-mark callback receives root-entry, traced-object,
distinct-mark, and conservative-retention counts together with direct access
to its authoritative mark bit. The callback is data-side only. The following
finalizer callback successfully `try_lock`s managed data, proving that neither
the mutex guard nor its borrow crosses finalizer admission. Existing injected
post-mark panic, request-during-post-mark, failed-mark retry, successful report,
and acknowledgement schedules run unchanged. No bitmap is copied and no
classification, sweep, lease invalidation, or report field is introduced. The
new handoff fixture passes a focused Miri run in addition to the ordinary
crate and workspace suites.

C6A.1 derives one attempt-local dead-set plan from the authoritative compact
allocation and mark words. Focused fixtures prove exact live, no-drop-dead,
and drop-required-dead classification for mixed typed runs; exact nonzero dead
masks across allocation-word and run boundaries; and exclusion of invalid
suffix bits. No payload is inspected. A forced post-classification panic
snapshots class membership, frontiers, allocation identities, allocation,
lease, and mark side metadata, pressure, and the heap-wide lease epoch. Recovery
matches that snapshot exactly, publishes no collection report, relatches the
request, and completes a clean retry. Reclamation and allocator publication
remain disabled until C6A.2 and C6A.3. All three classification fixtures pass
the focused `cargo +nightly miri test --package glam-gc --lib --all-features
dead_set` run.

C6A.2a publishes the first allocator invalidation boundary. One fixture
compares post-mark and finalizer-time snapshots and proves that class/run
membership, allocation identities and words, mark words, and pressure are
unchanged while every class frontier becomes null, every valid lease bit is
set, and the heap-wide lease epoch advances exactly once. Another keeps an
inactive cursor on a persistent worker thread across collection by a different
thread; the worker's next outer entry captures the new epoch, discards its
stale cursor, and allocates from a distinct fresh run without changing the old
allocation word. Existing forced panics now latch both sides of the boundary:
post-mark failure preserves the old epoch, while finalizer failure retains the
published invalidation, relatches collection, and advances again on retry.
No old slot or run is reclaimed or reused. Both the exact transition and
cross-thread stale-cursor fixtures pass focused Miri.

C6A.2b introduced whole-run retirement before reuse. Its primary fixture
allocated alternating live and dead one-slot runs in one class, plus a
partially live no-drop run and a wholly dead drop-bearing run, and latched the
intermediate boxed-record movement and unchanged arena storage before C6A.2c.
The current successor fixtures retain the same topology and destructor
boundaries while observing the completed reset/reuse endpoint described below.

C6A.2c validates every detached arena identity before resetting the first one,
clears the exact old allocation, lease, and mark side state, installs an empty
header, and publishes the numeric location to one heap-wide free-run pool. The
mixed-topology fixture proves stable retained order, exact reset state,
exclusion of partial and destructor-bearing runs, preference over virgin arena
capacity, and retyping into another allocation class without restoring old
class authority. An empty-run fixture proves that zero allocated slots imply
no finalization obligation even for class metadata with a Rust destructor. A
forced finalizer panic proves a reset location remains published exactly once,
retry does not duplicate it, and a later allocation consumes that exact run.
These fixtures are selected for focused Miri with:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  wholly_dead_no_drop_runs_reset_and_reuse_across_allocation_classes
cargo +nightly miri test --package glam-gc --lib --all-features \
  empty_runs_have_no_finalization_obligation_and_recycle_from_any_class
cargo +nightly miri test --package glam-gc --lib --all-features \
  finalizer_panic_retains_one_free_run_and_retry_does_not_duplicate_it
```

Header reset adds one reviewed raw write to the unsafe inventory; side-state
reset reuses the existing initialization boundary after mutator drain and
selector retirement. The full suite contains 156 unit tests (154 passing and
two ignored scale fixtures), six Loom models, and eight compile-fail/doc tests.

C6A.3a prevalidates every retained partial no-drop target, then intersects its
compact atomic allocation words with the ordinary mark words under exclusive
collection. The subset assertion is debug-only and uses
`marked & allocated == marked`; the release build still masks both inputs and
publishes their intersection. No payload is enumerated or referenced, and
drop-bearing allocation words remain unchanged for later finalization.

The existing bitmap-boundary fixture now checks the exact post-sweep allocation
state around indices 63/64 and across two runs, including dense death and sparse
survival. The mixed no-drop/drop fixture checks that only the no-drop dead bit
is cleared. The all/one/zero complete-run history proves repeated reduction,
and a new forced finalizer panic proves the reduced allocation state remains
valid and nonduplicating across an unpublished attempt and clean retry. Focused
Miri runs are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  post_mark_dead_set_classifies_exact_live_and_dead_slot_masks
cargo +nightly miri test --package glam-gc --lib --all-features \
  dead_set_masks_cross_bitmap_words_and_run_boundaries
cargo +nightly miri test --package glam-gc --lib --all-features \
  finalizer_panic_after_partial_no_drop_sweep_preserves_reclaimed_state
```

The sweep adds one reviewed raw atomic allocation-word load, one ordinary
mark-word read, and one raw atomic allocation-word store to the exact unsafe
inventory. The full suite contains 157 unit tests (155 passing and two ignored
scale fixtures), six Loom models, and eight compile-fail/doc tests.

C6A.3b replaces C6A.2a's completed-path blanket revocation with exact allocator
publication. Capacity reservation and topology/dead-word validation precede
selector withdrawal. The allocation-free mutation window retires whole
no-drop runs, sweeps partial no-drop words, publishes exact lease masks,
selects the first eligible frontier per class, and advances the cache epoch
last. Successful marks remain stale private scratch until the mandatory next
attempt clear; no allocator path consults them.

The publication fixture proves that partially occupied unreserved runs receive
clear lease bits and retain an eligible frontier rather than an all-ones mask.
A barrier-backed persistent worker proves its old cursor is discarded by the
new epoch and a fresh claim safely returns to the same retained run. A direct
claim reuses an eagerly swept slot without a lazy sweep. Drop fixtures prove
that a partial run reserves exactly the words with finalization obligations,
while a wholly dead drop-bearing run publishes no frontier. Focused Miri runs
are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  successful_post_sweep_publishes_final_leases_frontiers_and_epoch
cargo +nightly miri test --package glam-gc --lib --all-features \
  next_outer_entry_discards_a_stale_cursor_before_claiming_the_rebuilt_view
cargo +nightly miri test --package glam-gc --lib --all-features \
  partial_drop_runs_reserve_only_words_with_finalization_obligations
cargo +nightly miri test --package glam-gc --lib --all-features \
  wholly_dead_drop_runs_publish_no_allocator_frontier
```

The provisional raw lease-word Release store remains one reviewed unsafe site,
but now stores the exact final unavailable mask rather than a blanket valid-bit
mask. The full suite contains 160 unit tests (158 passing and two ignored scale
fixtures), six Loom models, and eight compile-fail/doc tests.

C6A.4 replaces historical typed-run publication pressure with exact assigned-
run occupancy. Virgin and recycled activation increment once after class
publication; whole-run reset decrements once before free-list publication.
Every successful sweep recomputes occupancy from class topology, checks it
against arena capacity, and publishes the saturating target
`S + 112 + ceil(S / 2)`. The pure arithmetic fixture injects alternate ratios
to cover exact ceiling behavior and overflow independently of the provisional
one-half tuning choice.

The integrated survivor fixture retains three one-slot runs, reclaims a fourth,
and observes the corresponding fixed plus proportional target. A recycled-run
fixture lowers the private target to force the recycled activation itself
across the boundary and verifies one occupancy increment and one request. A
finalizer-panic fixture proves that the already published swept baseline is
durable while the failed attempt relatches collection. Existing forced
acknowledgement schedules distinguish pressure before and after the data-side
clear; at this checkpoint, finalizer allocations before that clear are
deliberately coalesced until C6C.2 publishes the final post-finalization
baseline. Focused Miri runs are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  completed_sweep_publishes_survivor_assigned_run_baseline
cargo +nightly miri test --package glam-gc --lib --all-features \
  recycled_run_activation_crosses_pressure_target_once
cargo +nightly miri test --package glam-gc --lib --all-features \
  finalizer_panic_retains_the_completed_sweep_pressure_baseline
```

All three focused fixtures pass Miri. The stable collector matrix contains 164
unit tests (162 passing and two ignored scale fixtures), six Loom models, and
eight compile-fail/doc tests. Workspace formatting, all-target/all-feature
Clippy with warnings denied, and the complete workspace test suite also pass.

## C6B.1 Finalization-Batch Verification

C6B.1 materializes exact drop-required identities before selector withdrawal.
Native fixtures prove word-local reservation in an attached partial run,
complete detachment of wholly dead drop-bearing runs, preservation of former
class order across multiple detachments, and assigned-pressure accounting
which treats detached runs as occupied. At the C6B.1 checkpoint, no destructor
yet ran during collection; C6B.2 migrates those same fixtures below.

A second collection retains pending identities without tracing them and does
not duplicate batch records. A staged fixture drops another root between
collections and proves the newly dead slot merges into the existing attached
run record. Exact root and debug-access validation rejects both attached and
detached pending identities. A barrier-forced worker enters while the
collector holds its finalizer mutator and proves detached root rejection while
ordinary admission is open. Terminal teardown then visits class and batch
identities disjointly and invokes every destructor exactly once.

Focused Miri runs are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  partial_drop_runs_reserve_only_words_with_finalization_obligations
cargo +nightly miri test --package glam-gc --lib --all-features \
  pending_finalization_owns_partial_and_detached_runs_across_collection
cargo +nightly miri test --package glam-gc --lib --all-features \
  later_dead_slots_merge_into_existing_partial_finalization_record
cargo +nightly miri test --package glam-gc --lib --all-features \
  multiple_whole_finalization_detachments_preserve_former_class_positions
cargo +nightly miri test --package glam-gc --lib --all-features \
  concurrent_finalizing_entrant_cannot_root_a_detached_identity
```

All five focused fixtures pass Miri. The stable collector matrix contains 168
unit tests (166 passing and two ignored scale fixtures), six Loom models, and
eight compile-fail/doc tests. The exact unsafe inventory, workspace formatting,
all-target/all-feature Clippy with warnings denied, and the complete workspace
test suite also pass.

## C6B.2 Erased-Destruction Verification

C6B.2 migrates every C6B.1 fixture from durable pending state to completed
destruction. Drop counters now advance during the collection which discovers
the dead allocation; successful payloads have their exact allocation bits
cleared and contribute no conservative retention to a later collection.
Pre-collection slot snapshots preserve bitmap verification without attempting
to resolve stale post-destruction `Gc` identities.

`managed_destructor_runs_outside_locks_with_the_recursive_finalizer_mutator`
uses a real managed Rust `Drop` implementation to reenter the same heap,
discover an allocation class, allocate `u64`, and publish a root. It observes
recursive depth two and validates the fresh value after collection. Completion
proves erased dispatch held neither managed data nor the coordinator mutex and
that the successful dying allocation bit was retired exactly once.

`managed_destructor_panic_quarantines_exactly_once_without_retracing` forces a
real `Drop` panic. It checks attempt recovery, one exact quarantine record, a
still-set allocation bit, an empty pending mask, root rejection, and propagation
of the original panic. The next collection marks one conservative slot while
running neither `Trace` nor `Drop`; terminal heap release also leaves the
destructor count at one. C6C.1 later adds draining of work after that first
panic and restores all remaining safe run capacity.

Focused Miri runs are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  managed_destructor_runs_outside_locks_with_the_recursive_finalizer_mutator
cargo +nightly miri test --package glam-gc --lib --all-features \
  managed_destructor_panic_quarantines_exactly_once_without_retracing
```

The collection-time erased `drop_in_place` call and exact allocation retirement
are reviewed unsafe sites; sparse quarantine itself is safe Rust and allocates
only on the exceptional path. The stable collector matrix contains 170 unit
tests (168 passing and two ignored scale fixtures), six Loom models, and eight
compile-fail/doc tests.

## Gate G0 Baseline

Before changing the unsafe surface in C1, recheck the focused pre-GC semantic
contracts with:

```sh
crates/glam-gc/scripts/check-g0-semantics.sh
```

The operational comparison data can be recaptured on Linux with:

```sh
crates/glam-gc/scripts/capture-g0-baseline.sh
```

That script reports release-process timing and peak RSS; it does not enforce
performance thresholds. The dated measurements, environment, methodology, and
known pre-GC worker-stack observation are recorded in
[`GarbageCollectionGateG0Baseline_2026-08-20.md`](../../docs/plans/GarbageCollectionGateG0Baseline_2026-08-20.md).
