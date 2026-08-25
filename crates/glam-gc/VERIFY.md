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
which treats detached runs as occupied. C6B.2 migrated these fixtures to real
destruction, and C6B.3 now removes every successfully completed batch record;
durable cross-collection retention is now the intentional C6C.1 recovery path
after a destructor panic rather than ordinary successful post-collection state.

Exact root and debug-access validation is still forced while batch identities
are pending. `concurrent_finalizing_entrant_cannot_root_a_detached_identity`
holds the collector before erased destruction, admits another mutator, and
proves detached root rejection while ordinary admission is open. The C6B.3
fixtures below preserve reservation, detachment, and batch-order coverage while
asserting the final successful release state.

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

Focused Miri runs are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  managed_destructor_runs_outside_locks_with_the_recursive_finalizer_mutator
```

The collection-time erased `drop_in_place` call and exact allocation retirement
are reviewed unsafe sites. The C6B.2 checkpoint matrix contained 170 unit tests
(168 passing and two ignored scale fixtures), six Loom models, and eight
compile-fail/doc tests. C6C.1 owns the superseding panic-path verification.

## C6B.3 Successful-Completion Verification

C6B.3 makes successful finalization capacity available without waiting for
another collection. `partial_drop_runs_release_completed_words_for_reuse`
proves every completed attached word has a directly claimable lease and that
the terminal batch record is gone. A replacement reuses the first swept slot
rather than skipping to a never-reserved later word.

C6C.1b deliberately moves the publication boundary from each destructor to the
selected run attempt. `finalization_run_commit_delays_word_reuse_by_a_later_destructor`
proves that a destructor in a later word cannot reuse an earlier completed word
until the whole local run batch commits. The barrier-forced
`pending_run_keeps_words_reserved_from_an_ordinary_mutator_until_commit` holds
that later destructor open, admits an unrelated mutator while the runtime
remains `Finalizing`, and proves both that the earlier word remains reserved and
that its pending identity remains non-rootable until commit.

`successful_finalization_releases_partial_and_detached_batch_ownership` proves
an attached run remains in its class, a completed detached run leaves the
batch and enters the free pool, stale identities remain non-rootable, and a
later collection inherits no successful batch obligation.
`wholly_dead_drop_runs_recycle_without_a_stale_frontier` proves immediate
same-location retyping through the free pool, while
`multiple_whole_finalization_detachments_recycle_without_dispatch_order` proves
that one ephemeral run-key snapshot dispatches and recycles multiple stable
detached records without imposing map iteration or destruction order.

Focused Miri runs are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  partial_drop_runs_release_completed_words_for_reuse
cargo +nightly miri test --package glam-gc --lib --all-features \
  finalization_run_commit_delays_word_reuse_by_a_later_destructor
cargo +nightly miri test --package glam-gc --lib --all-features \
  pending_run_keeps_words_reserved_from_an_ordinary_mutator_until_commit
cargo +nightly miri test --package glam-gc --lib --all-features \
  successful_finalization_releases_partial_and_detached_batch_ownership
```

The current C6C.1b verification result is recorded below rather than preserving
the historical C6B.3 matrix as a second authoritative total.

## C6C.1 Destructor-Panic Retirement Verification

C6C.1 removes the temporary `ErasedGc -> QuarantineRecord` map. The focused
two-item regression was first run against the old implementation and failed
because the panicking identity retained its allocation bit in sparse
quarantine. After the fix, reaching the collector's unwind boundary clears that
exact allocation bit before its pending bit and routes completed word/run
capacity through the same terminal path as a returning destructor. The
original panic resumes only after the finalizer mutator and coordinator phase
have recovered; no later destructor is dispatched in that attempt.

`managed_destructor_panic_retires_one_and_defers_the_untouched_batch` places a
panicking and a returning destructor in one wholly dead run. It proves only the
first runs before the panic reaches the caller, the attempted allocation is
retired, the untouched allocation remains in one detached non-rootable pending
record, and recovery relatches collection. The next collection marks that one
pending slot conservatively without tracing it, finalizes it, recycles the run,
and publishes a report with one conservative retention.

`managed_destructor_panic_keeps_an_attached_word_reserved_until_retry` keeps a
third value rooted so the same panic occurs in an attached partial run. The
failed slot clears immediately, but the shared allocation word remains reserved
while its untouched pending bit exists. A later successful collection releases
the batch record and the next allocation reuses the failed slot, proving that
the storage is neither permanently damaged nor prematurely exposed.

`repeated_destructor_panics_make_one_terminal_step_per_collection` gives three
pending values panicking destructors. Three caught collection attempts retire
exactly one successive allocation apiece, preserve the untouched suffix, and
relatch the same uncompleted epoch. A fourth clean collection publishes epoch
one with no remaining mark or conservative-retention count. Heap teardown does
not retry any retired destructor.

Focused Miri runs are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  managed_destructor_panic_retires_one_and_defers_the_untouched_batch
cargo +nightly miri test --package glam-gc --lib --all-features \
  managed_destructor_panic_keeps_an_attached_word_reserved_until_retry
cargo +nightly miri test --package glam-gc --lib --all-features \
  repeated_destructor_panics_make_one_terminal_step_per_collection
```

All three focused fixtures pass Miri. C6C.1 adds no unsafe site: it changes
bookkeeping after the existing reviewed erased `drop_in_place` boundary and
deletes exceptional safe-Rust state. The stable collector matrix contains 174
unit tests (172 passing and two ignored scale fixtures), six Loom models, and
eight compile-fail/doc tests.

## C6C.1b Indexed Finalization Verification

C6C.1b replaces linear durable run/word collections and the persistent cursor
with authoritative nested maps plus bounded local snapshots.
`retry_merges_new_same_run_and_same_word_finalizers_without_duplication`
recovers from a destructor panic, adds newly dead slots to both an existing
pending word and a new word in the same run, then proves exact mask union,
pending counts, and one dispatch per identity. The run-commit and ordinary-
mutator fixtures above prove that the maps remain the non-rootability and
reservation authority while the local work batch runs.

`multiple_whole_finalization_detachments_recycle_without_dispatch_order`
forces a multi-run batch and proves that the ephemeral run-key snapshot visits
every selected run exactly once without assigning semantics to `HashMap`
iteration order. The existing panic-prefix fixtures prove that a normal run
commits once, while unwind commits only its successful prefix plus failed
identity and leaves the suffix indexed for retry.

Focused Miri runs are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::retry_merges_new_same_run_and_same_word_finalizers_without_duplication \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::finalization_run_commit_delays_word_reuse_by_a_later_destructor \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::pending_run_keeps_words_reserved_from_an_ordinary_mutator_until_commit \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::multiple_whole_finalization_detachments_recycle_without_dispatch_order \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::managed_destructor_panic_retires_one_and_defers_the_untouched_batch \
  -- --exact
```

All five fixtures pass Miri. The stable collector matrix contains 175 unit
tests (173 passing and two ignored scale fixtures), six Loom models, and eight
compile-fail/doc tests. The unsafe inventory, workspace formatting,
all-target/all-feature Clippy with warnings denied, and the complete workspace
test suite also pass.

## C6C.2 Activity, Report, and Pressure Verification

C6C.2 exposes finalizer activity without weakening the run-at-a-time commit
boundary. `pending_run_keeps_words_reserved_from_an_ordinary_mutator_until_commit`
holds a selected run behind a deterministic barrier and observes every claimed
obligation as running until its single commit. The recovered-panic fixtures
then prove that running returns to zero and only an untouched suffix remains
queued; successful retry drains both counts.

Successful reports partition reclamation into all retired slots, the
drop-bearing finalized subset, and runs reset into the free pool.
`wholly_dead_no_drop_runs_reset_and_reuse_across_allocation_classes` combines
partial no-drop sweep, whole-run no-drop retirement, and a real destructor to
verify those counts against the exact allocator topology. Panic fixtures also
prove that a failed attempt publishes neither a report nor a completed epoch,
and that its already terminal identity is not attributed to the later retry.

`finalizer_run_activation_sets_the_completed_pressure_baseline` forces a
finalizer to activate a new typed run after sweep and proves successful
completion publishes the two-run survivor baseline. The corresponding
`panicking_finalizer_publishes_pressure_without_a_completion_report` fixture
panics after the allocation and proves the same post-attempt pressure becomes
durable while the collection request is relatched and the completion epoch and
report remain unchanged.

Focused Miri runs are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::pending_run_keeps_words_reserved_from_an_ordinary_mutator_until_commit \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::finalizer_run_activation_sets_the_completed_pressure_baseline \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::panicking_finalizer_publishes_pressure_without_a_completion_report \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::managed_destructor_panic_retires_one_and_defers_the_untouched_batch \
  -- --exact
```

All four fixtures pass Miri. The stable collector matrix contains 177 unit
tests (175 passing and two ignored scale fixtures), six Loom models, and eight
compile-fail/doc tests. The unsafe inventory is unchanged by C6C.2.

## C6D.1 Terminal-Contract Verification

C6D.1 selects passive, non-reentrant terminal destruction without an owner-
lease shell. `only_the_last_heap_facade_starts_terminal_teardown` proves that
one surviving public owner postpones destruction. The pre-existing
`terminal_heap_teardown_waits_for_active_owner_regions` forces the same rule
across threads and an admitted mutator region, while
`forgotten_scoped_allocator_does_not_retain_its_heap` and
`escaped_root_does_not_retain_its_heap_or_payload` cover the two deliberately
non-owning capability families.

`ordinary_finalization_has_a_mutator_but_terminal_teardown_does_not` directly
latches optional capability availability: collection-time Rust `Drop` observes
the C3E finalizer mutator, whereas last-owner destruction observes no active
mutator. The public destructor contract does not expose those as modes; the
fixture proves why every managed representation must remain safe without the
capability.

`terminal_teardown_finishes_the_batch_retained_after_finalizer_panic` proves
the terminal class and pending-batch walks remain disjoint, complete an
untouched pending identity once, and never retry the earlier panicking
identity.

`terminal_teardown_propagates_the_first_panic_without_continuing` uses an
order-independent shared panic flag and static attempt count. Exactly one of
three terminal destructors runs before the first panic propagates, the heap
identity expires, and dropping an escaped root cannot redispatch anything.
Static instrumentation is deliberate: placing `Arc` fields in the untouched
raw payloads would create precisely the permitted terminal resource leak and
would prevent Miri's leak detector from distinguishing policy from an
accidental collector leak.

Focused Miri runs are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::only_the_last_heap_facade_starts_terminal_teardown -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::ordinary_finalization_has_a_mutator_but_terminal_teardown_does_not \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::terminal_teardown_finishes_the_batch_retained_after_finalizer_panic \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::terminal_teardown_propagates_the_first_panic_without_continuing \
  -- --exact
```

All four fixtures pass Miri. The stable collector matrix contains 181 unit
tests (179 passing and two ignored scale fixtures), six Loom models, and eight
compile-fail/doc tests. C6D.1 adds no production unsafe site; its two edge-free
fixture `Trace` declarations are recorded in the exact inventory.

## C6D.2 Detached-First Terminal Traversal Verification

C6D.2 removes the per-object pending-finalizer lookup from terminal class
traversal. Detached finalization records are walked first and are absent from
class topology; the subsequent class walk handles every attached allocation,
including attached pending finalizers. Already attempted panic identities have
clear allocation bits and occur in neither source.

`terminal_class_walk_includes_attached_pending_finalizers_once` forces an
ordinary finalizer panic in a run kept attached by a live root. After dropping
that root, terminal traversal does not retry the failed identity and invokes
the deferred and formerly live values exactly once each. The existing
`terminal_teardown_finishes_the_batch_retained_after_finalizer_panic` supplies
the detached counterpart, while the ordinary 130-object terminal fixture and
the order-independent first-panic fixture cover complete and interrupted
traversal.

Focused Miri runs are:

```sh
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::terminal_class_walk_includes_attached_pending_finalizers_once \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::terminal_teardown_finishes_the_batch_retained_after_finalizer_panic \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::terminal_heap_teardown_drops_each_allocated_payload_exactly_once \
  -- --exact
cargo +nightly miri test --package glam-gc --lib --all-features \
  heap::tests::terminal_teardown_propagates_the_first_panic_without_continuing \
  -- --exact
```

All four fixtures pass Miri. The stable collector matrix contains 182 unit
tests (180 passing and two ignored scale fixtures), six Loom models, and eight
compile-fail/doc tests. C6D.2 adds no unsafe site.

## GC6-002A Irreversible-Topology Verification

`panic_after_destructive_topology_mutation_does_not_reopen_the_heap` was first
run with only the deterministic post-withdrawal panic hook and failed: the
attempt guard restored `Ordinary`, and the next outer mutator entered through a
retry. After the repair, the same schedule proves the attempt remains
`TopologyMutation` until the full swept view and allocation-lease epoch publish.
Its unwind permanently poisons the heap; later mutator entry and collection
requests panic, synchronous `collect_full` returns `CollectionError::Poisoned`,
and terminal release does not dispatch an allocated drop-bearing payload.

`irreversible_topology_panic_wakes_waiters_into_permanent_poison` pauses the
collector before selector withdrawal, blocks one outer mutator and one
synchronous collector behind `Exclusive`, then releases the collector into the
forced topology panic. Both waiters wake: the mutator observes the permanent-
poison panic and the collector receives `CollectionError::Poisoned`. The poison
publication takes the coordinator mutex but never reads managed data, including
when the injected panic has poisoned that mutex.

Focused Miri runs are:

```sh
cargo +nightly miri test -p glam-gc --lib --all-features \
  heap::tests::panic_after_destructive_topology_mutation_does_not_reopen_the_heap \
  -- --exact
cargo +nightly miri test -p glam-gc --lib --all-features \
  heap::tests::irreversible_topology_panic_wakes_waiters_into_permanent_poison \
  -- --exact
```

Both fixtures pass Miri. The complete collector check passes with 184 unit
tests (182 passing plus two ignored scale fixtures), six Loom models, eight
compile-fail/doc tests, and the exact unsafe inventory. The checkpoint adds no
production unsafe site; its one edge-free fixture `Trace` declaration is
recorded in that inventory. GC6-002B extends the attempt state across erased
destructor dispatch and durable run commit; ordinary caught payload-destructor
panic remains recoverable.

## GC6-002B Finalizer Commit Verification

`panic_before_finalizer_commit_permanently_poisons_without_redispatch` arms a
one-shot panic after erased `drop_in_place` and run-local terminal recording,
but before `complete_finalization_run` publishes allocation-bit and durable
batch retirement. It proves the destructor ran exactly once, the collection
remained `FinalizerCommitPending`, later entry and collection reject permanent
poison, and terminal heap release does not redispatch the uncertain identity.

`managed_destructor_panic_retires_one_and_defers_the_untouched_batch` remains
the complementary recoverable path. The collector catches the payload panic,
commits that exact terminal identity, restores `AllocatorViewPublished`, and
only then resumes the original panic. A later collection finalizes only the
untouched suffix.

Both boundary fixtures pass focused Miri:

```sh
cargo +nightly miri test -p glam-gc --lib --all-features \
  heap::tests::panic_before_finalizer_commit_permanently_poisons_without_redispatch \
  -- --exact
cargo +nightly miri test -p glam-gc --lib --all-features \
  heap::tests::managed_destructor_panic_retires_one_and_defers_the_untouched_batch \
  -- --exact
```

The complete collector check passes with 185 unit tests (183 passing plus two
ignored scale fixtures), six Loom models, eight compile-fail/doc tests, and the
exact unsafe inventory. The checkpoint adds no production unsafe site and no
new test `Trace` declaration. GC6-002C retains the complete poison-boundary and
documentation audit before the finding closes.

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
