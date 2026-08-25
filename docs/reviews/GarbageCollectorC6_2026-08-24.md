# Glam GC C6 Implementation Review — 2026-08-24

Baseline: `7b30876`. This is the mandatory post-C6 review of the isolated
`glam-gc` collector against the GC roadmap, implementation plan, public unsafe
contract, safety ledger, and verification surface.

Status: complete. The C6 representation, ordinary and terminal failure
boundaries, current safety contract, and downstream stress/metric plan are
reconciled. Every review finding is resolved; C6D.3 can now certify the
resulting collector without absorbing new design work.

## Outcome

C6 completes a credible eager full-collection pipeline. A successful mark is
consumed without copying the bitmap; destructor-free dead allocations are
removed eagerly; completely empty reusable runs leave their classes only after
frontier withdrawal and return through one untyped free-run pool; and
drop-bearing identities remain initialized, allocated, non-rootable, and
reserved until erased destruction reaches a terminal boundary. The durable
run/word finalization index gives one authority for retry, activity, root
rejection, and terminal discovery.

The ordinary destructor-panic path is notably sound. The collector catches the
payload panic, records the attempted identity in the run-local attempt, clears
the allocation bit before removing the pending bit, commits the successful
prefix plus the panicking identity, leaves the untouched suffix pending, drops
the finalizer mutator, restores ordinary admission, relatches collection, and
only then resumes the original panic. No destructor order is exposed as
semantics.

The review found no confirmed overlap, duplicate-destruction, or missing-slot
defect in the intended success, caught-destructor-panic, retry, or passive
terminal paths. It did find a distinct panic-safety hole around *collector
invariant panics after destructive work*. The public `Trace` contract gap found
alongside it has since been resolved with one capability/liveness rule: every
managed representation is safely droppable without a matching-heap mutator,
while optional mutator availability never revives edges from the dying
allocation. The finalized-word publication and mixed terminal-topology claims
also deserve forced verification before certification rather than being
deferred into general C7 stress.

Current stable verification passes: `crates/glam-gc/scripts/check.sh` reports
185 passing unit tests, two explicit ignored scale fixtures, seven Loom models,
and eight compile-fail doctests. The exact unsafe inventory passes. This review
does not substitute that stable run for C6D.3's required complete Miri and
sanitizer audit.

## Reviewed State and Ownership

| State | Authoritative representation | Allocation/lease state | Owner and transition |
| --- | --- | --- | --- |
| Ordinary attached allocation | class run record and allocation bitmap | allocation bit set; word leased or available normally | traced from roots/managed edges, or classified by C6 |
| Dead no-drop slot in retained run | attempt-local dead-set until eager sweep | allocation bit cleared by `allocated &= marked` | immediately reusable after swept lease/frontier publication |
| Wholly dead no-drop run | `retired_no_drop_runs` only during the exclusive transition | selectors withdrawn, then header and side state reset | numeric location enters `free_runs`; old class loses all authority |
| Attached pending finalizer | `finalization_batch[run][word]` plus ordinary class run | allocation and pending bits set; exact word reserved | remains class-owned until run attempt commits |
| Detached pending finalizer | finalization batch owns the stable boxed run record | allocation and pending bits set; whole run unavailable | absent from class topology; recycled only when the record empties |
| Dispatched but not committed finalizer | run-local `RunFinalizationAttempt`; durable map still unchanged | allocation and pending bits still set | hazardous transient state; the destructor has run even though durable topology still says pending |
| Terminally retired finalizer | allocation bit cleared before pending bit | completed attached word released, or completed detached run reset | never traced, rooted, finalized, or terminally dropped again |

The “dispatched but not committed” row is intentionally short-lived, but it is
the key boundary behind GC6-002. Normal destructor unwind reaches the commit;
an unrelated assertion or panic in the collector between dispatch and commit
does not currently have the same guarantee.

## Publication Review

The successful publication sequence is internally consistent:

1. under `Exclusive`, all planning and capacity reservation precede selector
   withdrawal;
2. no-drop run retirement, finalization-batch installation, eager sweep, exact
   lease rebuilding, and frontier selection complete under managed data;
3. the allocation-lease epoch publishes the complete allocator view last;
4. `Exclusive` converts directly into one collector-owned mutator and
   `Finalizing`, with no authority gap;
5. each selected finalization run commits allocation retirement and pending
   removal once under managed data;
6. final pressure is recomputed from class topology plus detached runs; and
7. only then do the scalar report and completion epoch publish together under
   the coordinator.

An explicit or pressure request serialized before final pressure publication
is deliberately coalesced into the new survivor baseline. A request published
after that data-side acknowledgement remains latched. A destructor panic
publishes neither report nor completion epoch, but preserves the already swept
allocator view, terminally retired prefix, final pressure baseline, and a new
request. These choices match the recorded C6 semantics.

## Findings

### GC6-001 — The public managed-`Drop` contract omits terminal teardown

**Priority:** high  
**Confidence:** high  
**Classification:** unsafe public-contract gap  
**Gate G1:** resolved on 2026-08-24

The public [`Trace` safety contract](../../crates/glam-gc/src/trace.rs#L24)
says a destructor may allocate through the installed finalizer mutator. That is
true during ordinary collection. The same `Drop` implementation may also be
invoked by last-owner [`HeapInner::drop`](../../crates/glam-gc/src/heap.rs#L3145),
where the selected C6D contract deliberately provides no mutator, evaluation,
diagnostics, runtime reentry, or recovery after panic.

`SAFETY.md` and the implementation plan describe this distinction, but an
implementer of the public unsafe trait must not need a private transition plan
to discover an additional safety obligation. As written, a destructor which
correctly follows the public paragraph by requiring the finalizer mutator may
still violate the terminal contract.

Resolved by replacing the phase-oriented description with one public
capability/liveness contract. Every managed representation must remain safely
droppable when no matching-heap mutator can be obtained. A destructor which
independently obtains one may use independently live roots and allocate fresh
values, but cannot inspect or preserve bare `Gc` fields from the dying
allocation; those edges are spoiled regardless of capability availability.

The implementation's ordinary-finalization and terminal-teardown contexts now
remain private proof evidence for optional capability availability rather than
modes which a destructor must detect. The existing
`ordinary_finalization_has_a_mutator_but_terminal_teardown_does_not` fixture
latches both sides of that implementation claim, while the public `Trace`
documentation and safety ledger state the single caller obligation.

### GC6-002 — Attempt recovery does not distinguish a retryable panic from an irreversible commit panic

**Priority:** high  
**Confidence:** high  
**Classification:** panic-safety/proof defect  
**Gate G1:** blocker

[`CollectionAttempt`](../../crates/glam-gc/src/heap.rs#L2971) records only
whether the allocator view was published and whether the whole attempt
completed. Its `Drop` implementation restores ordinary admission and relatches
collection for every other unwind.

That policy is correct for trace, worklist, classification, and synthetic
pre-finalizer panics. It is also reached safely for a payload destructor panic
because [`run_finalization_batch`](../../crates/glam-gc/src/heap.rs#L2267)
catches that panic and commits the attempted identity before resuming it.

It is not a complete policy for an invariant panic after destructive work:

- the exclusive mutation window changes run ownership, allocation bits,
  leases, frontiers, and the free-run pool before
  `allocator_view_published` becomes true; and
- after `drop_in_place` returns or unwinds, the run-local attempt knows the
  destructor ran while the durable allocation and pending bits still describe
  an unattempted value until `complete_finalization_run` commits.

Today those paths are intended to be allocation-free and infallible after
prevalidation. They still contain all-build assertions, checked arithmetic,
hash/index operations, raw topology operations, and mutex reacquisition. An
internal invariant panic can therefore unwind through the generic recovery
guard. At the dangerous finalizer boundary, a later retry could invoke `Drop`
on an already destroyed or partially destroyed value if the durable commit did
not complete. During the exclusive topology transition, reopening ordinary
admission can expose a partially published allocator view.

Make the irreversibility boundary explicit. Viable policies include an attempt
state machine with a permanently poisoned terminal state, or aborting on an
invariant panic once destructive publication begins. For finalization, use a
small RAII commit guard so an observed destructor return/panic either retires
that exact identity or makes the heap permanently unavailable; never hand the
same identity back to ordinary retry merely because collector bookkeeping
panicked. The existing caught payload panic remains recoverable and should keep
its current semantics.

Add injected-panic schedules at two points:

1. after the first destructive allocator-topology mutation but before final
   lease-epoch publication; and
2. after erased `Drop` dispatch but before durable run-attempt commit.

The expected result is a deliberately unusable/aborted heap, not ordinary
retry. This also reconciles roadmap invariant 11, which reserves heap poison or
abort for allocator corruption or unsafe-contract failure whose effects cannot
be bounded.

#### GC6-002 resolution plan

Adopt permanent heap poisoning rather than process abort. Poison is an
internal collector-safety state: it preserves the original panic, admits no
new mutator or collection, wakes coordinator waiters so they cannot deadlock,
and makes terminal heap teardown skip managed destructors. Poison does not
revoke references in the middle of an already admitted Rust borrow, so such a
scope may passively unwind or exit; operations which need authoritative heap
state may themselves reject the poisoned heap. Remaining managed Rust
resources may leak when the poisoned value domain is finally released, which
is preferable to reopening uncertain topology or dispatching a destructor
twice.

Execute the repair as three independently verified checkpoints:

1. **GC6-002A — topology irreversibility and heap poison (completed
   2026-08-25).** Replace
   `CollectionAttempt`'s two booleans with an explicit state such as
   `Reversible`, `TopologyMutation`, `AllocatorViewPublished`,
   `FinalizerCommitPending`, and `Completed`. Enter `TopologyMutation`
   immediately before the first selector withdrawal, then publish
   `AllocatorViewPublished` only after the complete swept allocator view and
   allocation-lease epoch are authoritative. Preserve the current retry path
   for `Reversible` and `AllocatorViewPublished`; an unwind from either
   irreversible state permanently poisons the heap instead. Add the poison
   admission/waiter/terminal-drop gates and a deterministic panic hook between
   selector withdrawal and final lease-epoch publication. First latch that the
   current implementation reopens admission after this panic, then require
   later entry and collection to reject the poisoned heap without dispatching
   terminal destructors.
2. **GC6-002B — finalizer dispatch-to-commit guard (completed
   2026-08-25).** Keep the collection in
   `FinalizerCommitPending` from immediately before the first erased destructor
   in a run is called until `complete_finalization_run` has durably retired the
   complete attempted prefix. A run with no dispatched destructor never enters
   this state. Add a deterministic hook after `drop_in_place` and local
   terminal recording but before durable run commit. The forced panic must
   poison the heap, prevent later admission and retry, and prove that final
   heap release does not redispatch the already destroyed identity. Retain the
   existing payload-`Drop` behavior: catch its panic, commit that exact
   identity, restore `AllocatorViewPublished`, and only then resume the
   original payload.
3. **GC6-002C — boundary audit and closeout (completed 2026-08-25).** Audit
   every attempt-state transition and every heap entry, synchronous waiter,
   finalizer-activity, and terminal-release path against the permanent-poison
   rule. Ensure the poison path performs no allocation and cannot itself
   depend on damaged managed data. Update `SAFETY.md`, `VERIFY.md`, the
   implementation plan, and this resolution ledger. Run focused
   ordinary-payload-panic and both injected-invariant-panic fixtures under
   Miri, then run the complete `glam-gc` verification script and repository
   routine checks.

This repair does not change tracing, marking, allocation geometry, bitmap
layout, the ordinary allocation fast path, or the public managed-value
representation. Any public presentation of a poisoned heap should remain a
small API decision at GC6-002A; internal admission must be correct regardless
of whether `collect_full` eventually returns a dedicated error while infallible
entry helpers panic.

##### GC6-002A completion

The pre-fix forced regression reached selector withdrawal, injected a panic,
and failed because the next mutator entry completed a retry under reopened
`Ordinary` admission. The repair replaces the two attempt booleans with
`Reversible`, `TopologyMutation`, `AllocatorViewPublished`, and `Completed`
states; GC6-002B adds `FinalizerCommitPending` at its narrower boundary.
Selector withdrawal begins the irreversible state, while only publication of
the complete swept view and allocation-lease epoch restores recoverability.

An unwind from `TopologyMutation` now publishes permanent poison under the
coordinator mutex without consulting managed data. New mutator admission and
collection requests panic, `collect_full` returns `CollectionError::Poisoned`,
blocked mutator and synchronous-collection waiters wake into those outcomes,
and terminal heap release skips managed destructors. Existing admitted Rust
borrows are not revoked, although no further heap operation is promised. A
second forced fixture holds both waiter families behind `Exclusive` and proves
the poison notification releases them.

Both forced fixtures pass focused Miri. The complete collector check passes
with 184 unit tests (182 passing plus two ignored scale fixtures), six Loom
models, eight compile-fail/doc tests, and the exact unsafe audit. The change
adds no production unsafe site; its one edge-free test `Trace` declaration is
recorded in the inventory. It does not alter allocator geometry, mark or
allocation bitmaps, tracing, or the allocation fast path. GC6-002 remains a
Gate G1 blocker at this historical checkpoint; the GC6-002C closeout below
resolves it.

##### GC6-002B completion

`CollectionAttempt` now enters `FinalizerCommitPending` immediately before the
first erased destructor dispatched for a run. It remains irreversible through
local terminal recording and `complete_finalization_run`, including allocation
bit retirement, durable batch update, word release, run recycling, and
finalizer-activity retirement. A run with no dispatched destructor does not
enter this state. Successful durable publication restores
`AllocatorViewPublished`.

The one-shot forced fixture panics after `drop_in_place` and local terminal
recording but before durable commit. The original destructor runs once, the
attempt permanently poisons without consulting managed data, later entry and
collection reject the heap, and terminal release does not redispatch the
uncertain identity. The existing payload-panic fixture proves the recoverable
counterpart: catch the payload, commit that exact identity, restore the
published allocator state, then resume the original payload while leaving the
untouched suffix pending.

Both fixtures pass focused Miri. The stable collector library now contains 185
tests (183 passing plus two ignored scale fixtures). GC6-002B adds no
production unsafe site or test `Trace` declaration. The GC6-002C closeout
follows below.

##### GC6-002C completion

The closeout audit accounts for all six attempt states and their unwind
dispositions, every public heap operation, synchronous waiter wakeup,
finalizer activity, collector-mutator release, and poisoned terminal teardown.
Poison publication uses only the coordinator mutex, atomics, scalar state, and
the existing condition variable; it neither allocates nor reads managed data.

The audit corrected two residual boundaries. `FinalizerCommitGuard` now owns
the destructor dispatch-to-commit interval and poisons during
`run_finalization_batch` unwinding, before collector-mutator admission is
released. `Heap::activity` now rejects calls begun after poison before locking
potentially damaged managed data. Tests latch both details alongside request,
entry, synchronous collection, waiter, and terminal-destructor behavior.

The topology invariant panic, finalizer-commit invariant panic, and ordinary
caught payload panic all pass focused Miri. The full collector gate contains
185 unit tests (183 passing plus two ignored scale fixtures), six Loom models,
eight compile-fail/doc tests, and the exact unsafe inventory. GC6-002 adds no
production unsafe site and is resolved. The post-C6 review remains open for
its other Gate G1 blockers.

### GC6-003 — Finalized-word publication lacks a direct concurrent model

**Priority:** medium  
**Confidence:** high  
**Classification:** concurrency verification gap  
**Gate G1:** blocker

An attached finalization run clears terminal allocation bits, then
[`release_finalized_allocation_word`](../../crates/glam-gc/src/arena.rs#L397)
uses `fetch_and` on one lease bit while ordinary mutators may concurrently
claim neighboring bits. It subsequently republishes or moves the class
frontier. The Release/Acquire proof is reasonable and the native tests prove
that words remain reserved while a destructor is paused and become reusable
after commit.

The six current Loom models cover unique lease claims and coordinator
admission/handoff, but not the C6 transition which clears a reserved bit while
another claimant mutates the same lease word. No production forced schedule
holds a claimer precisely across allocation retirement, lease release, and
frontier publication.

Before G1, add a small Loom model for neighboring-bit preservation and exactly
one claim of the released bit. Pair it with a native barrier fixture around the
production release/frontier boundary which verifies that the claimant observes
the retired allocation bitmap before initializing a replacement. This is a C6
proof obligation and should not wait for general C7 worker stress.

#### GC6-003 completion

Completed 2026-08-25. The new Loom model races the collector's Release
`fetch_and` with a neighboring-bit read-modify-write and a claim of the
released bit. It proves that the neighbor survives, the released bit has one
winner, and a winner overlapping the release observes allocation retirement.

The production fixture pauses an attached finalization run after lease release
and before frontier publication. A prepared allocator claims the released word
while the collector still holds managed data, observes the cleared allocation
bitmap through the lease synchronization edge, and initializes the exact
retired slot. It then verifies the finalization report, destructor counts, and
later collection. The fixture passes focused Miri; all seven Loom models pass.
No production unsafe site was added. GC6-003 is resolved.

### GC6-004 — Terminal topology is tested by halves, not as one mixed heap

**Priority:** medium  
**Confidence:** medium  
**Classification:** terminal verification gap  
**Gate G1:** blocker

C6D.2's topology is simple and appears correct: terminal teardown visits
detached finalization records first, then all remaining drop-bearing class
runs. Separate fixtures cover one retained detached batch and one retained
attached batch. They do not construct a single heap containing both kinds at
terminal teardown.

Add one deterministic mixed fixture with:

- a detached pending run retained after an ordinary destructor panic;
- an attached pending run in another class or run;
- an ordinary still-allocated drop-bearing value; and
- at least one already terminally retired panic identity.

Dropping the last heap facade should attempt every untouched allocation once,
never retry the retired identity, and prove that detached and class traversal
are disjoint in the same concrete topology. This is inexpensive evidence for
the exact C6D claim which G1 will certify.

#### GC6-004 completion

Completed 2026-08-25. The new deterministic fixture fills one wholly dead run
and one partially live run in the same allocation class. A panic-once
destructor retires the first dispatched identity regardless of finalization-map
order. Direct state inspection then proves that terminal teardown begins with
one detached pending run, one attached pending run, one ordinary rooted value,
and exactly one retired identity.

After the root is released, dropping the last heap facade brings the total
destructor-attempt count to exactly the number of original allocations. The
detached and class traversals therefore neither omit nor duplicate an identity
in the combined topology. The new fixture and the existing terminal-panic
fixture pass focused Miri. No production or unsafe code was added. GC6-004 is
resolved.

### GC6-005 — Current public and safety documentation still describes pre-C6 liveness

**Priority:** medium  
**Confidence:** high  
**Classification:** documentation/proof drift  
**Gate G1:** blocker

Several authoritative or public surfaces still describe historical phases as
current behavior:

- [`Gc<T>`](../../crates/glam-gc/src/pointer.rs#L6) says allocations remain live
  until heap teardown “before collection is enabled,” although the public heap
  now performs reclamation.
- [`Root::get`](../../crates/glam-gc/src/root.rs#L41) justifies its unsafe cast
  partly with “C4 reclaims nothing.” Its current proof is instead that a live
  registered root participates in every exclusive root walk and the matching
  mutator excludes collection for the returned borrow.
- [`SAFETY.md`](../../crates/glam-gc/SAFETY.md) ends its implemented-phase
  narrative at C6C.1b, retains pre-sweep `Gc` proof language, and still calls
  terminal metadata dispatch provisional in an earlier unsafe-site section.
- The [roadmap status](../plans/GarbageCollectionRoadmap_2026-08-19.md) stops at
  C6A.2b, while the [integration plan's current boundary](../plans/GarbageCollectorIntegration_2026-08-19.md#current-boundary)
  says the collector cannot yet mark, reclaim, or finalize.
- The module unsafe expectation in
  [`lib.rs`](../../crates/glam-gc/src/lib.rs#L32) still labels `heap` only as the
  C2C terminal-destruction boundary rather than the C5/C6 trace, reclamation,
  and finalization dispatch owner.

These are not cosmetic around an unsafe API: they are part of the proof a
caller uses to decide whether a bare `Gc` is live and what `Trace`/`Drop` must
support. Reconcile them before G1. Keep historical phase records where useful,
but finish each section with one unambiguous current rule.

#### GC6-005 completion

Completed 2026-08-25. `Gc<T>` now says directly that it carries no liveness and
may become stale while its heap remains live. `Root::get` and the safety ledger
use the current proof: registry publication precedes observation, every live
root cell is an exclusive mark seed, and matching mutator authority excludes
collection for the returned borrow. The stale pre-sweep and provisional
terminal-dispatch claims were removed.

The safety phase history now reaches C6D and ends with one current non-moving
full-collector rule. The roadmap status, resolved terminal checkpoint, runtime
integration boundary, and `heap` unsafe-module expectation now agree that the
isolated collector marks, reclaims, and finalizes but has not passed Gate G1.
Doctests, Clippy with warnings denied, and the exact unsafe inventory pass.
GC6-005 changes no runtime semantics or unsafe operation and is resolved.

### GC6-006 — Phase-local lint reasons have outlived their boundary

**Priority:** low  
**Confidence:** high  
**Classification:** cleanup

Several `dead_code` allowances and comments still say an operation “becomes”
collector input in a later C2/C5/C6 phase. Some fields remain useful only to
tests or reports; others may now be removable. After the contract fixes, audit
the allowances rather than mechanically renaming every phase:

- remove state which has no production or verification consumer;
- gate verification-only summaries honestly; and
- describe enduring architectural ownership instead of the checkpoint which
  originally introduced a field.

This does not block G1 unless removing an allowance exposes an actually stale
second representation.

#### GC6-006 completion

Completed 2026-08-25. The audit removed every `dead_code` allowance from the
collector crate. Production tracing, topology, sweep, and class-index state
now compile without exemptions. Synchronized allocation helpers, resolved-run
enumeration, arena inspection conveniences, and detailed classification
summaries are explicitly test-only; debug builds retain only the slot totals
needed by the classification consistency assertion.

Removing the blanket module allowances exposed no stale second production
representation. A release library build is warning-free. The audit also
replaced phase-numbered sweep comments with enduring ownership/publication
rules and corrected a cross-thread pointer test to keep a registered root live
instead of relying on the obsolete pre-reclamation lifetime assumption.
GC6-006 changes no collector semantics and is resolved.

### GC6-007 — C7/C8 need minor reconciliation after the C6 representation settled

**Priority:** low  
**Confidence:** high  
**Classification:** downstream plan drift

C7 remains useful, but two details should change when the Gate blockers close:

- move the exact finalized-word release race from broad C7B stress into C6D.3
  per GC6-003, leaving C7B to scale and compose already proved transitions;
- define what “terminal metrics” means before C7C.3. Last-owner teardown has no
  surviving heap client to observe a per-heap report, so terminal consistency
  should initially be test instrumentation rather than a promised runtime
  metric surface.

C7A and C7B are each broad enough to review for smaller forced-order
checkpoints immediately before implementation. C8's measurement-first policy,
including paged array tracing and finalization-index measurements, remains
appropriate.

#### GC6-007 completion

Completed 2026-08-25. C7A now separates deterministic root handoff,
immutable-reader admission, and scale composition. C7B separately forces
single-heap allocation/admission, cross-heap and external-blocking behavior,
then allocator/coordinator scale. Repetition and randomized stress are
supplementary; a novel ordering must become a bounded forced fixture.

C6D.3 now explicitly owns certification of GC6-003's Loom and production
finalized-word release schedules. C7 may scale and compose that transition but
does not carry its missing correctness proof. C7C.3 treats last-owner teardown
counts as test-only external observations: with no surviving heap client,
terminal teardown publishes no collection report or promised runtime metric.
C8A likewise stabilizes only observable non-terminal collection reporting.
The remaining C8 measurement and paged-array exploration text needs no change.
GC6-007 is resolved.

## Intentional Drift and Accepted Boundaries

- **Finalization is run-at-a-time.** A transient run-local vector and word
  snapshot avoid holding managed data across `Drop`; the durable nested maps
  remain the authority. This is a deliberate correctness/lock boundary, not a
  second persistent representation.
- **Hash-map iteration assigns no destructor order.** Dispatch snapshots exist
  only to bound lock hold time. Tests must remain order-independent unless they
  explicitly control a single run.
- **Successful marks remain stale scratch.** Eager sweep and finalizer masks
  consume every semantic use; the next collection clears marks before tracing.
- **Failed attempts may reclaim without a report.** No-drop sweep and the
  panicking destructor identity are durable even though no completion epoch is
  published. A later report counts only work completed by that later successful
  attempt.
- **Finalization admits ordinary mutation.** The collector-held mutator blocks
  another collection but does not serialize unrelated mutators. Fresh effects
  and allocations survive a destructor panic.
- **Terminal teardown is deliberately passive.** It is not an evaluation stage,
  supplies no mutator, and stops at the first destructor panic. Untouched Rust
  resources may leak while raw arena storage is released.
- **Roots remain weak with respect to heap ownership.** A live root keeps a
  value marked only while some authorized heap owner keeps the value domain
  alive; an escaped root cannot postpone or revive terminal teardown.

## Verification Mapping

| Claim | Existing evidence | Review result |
| --- | --- | --- |
| Exact no-drop dead-set and eager sweep | word/run boundary, repeated all/one/zero, finalizer-panic retry, focused Miri | adequate |
| Whole-run retirement and cross-class reuse | mixed topology, empty drop-class run, retry/free-pool tests, focused Miri | adequate |
| Stale-cursor invalidation and swept allocator publication | persistent-worker epoch fixture, exact lease/frontier fixture | adequate |
| Pending identity root rejection | detached concurrent entrant plus attached paused-run root rejection | adequate |
| Successful and panicking finalization | run commit, attached/detached release, repeated panic, invariant-panic poison, merge-on-retry, focused Miri | adequate; GC6-002 resolved |
| Finalizer activity, pressure, and reports | paused running batch, successful/panicking allocation, report/epoch tests | adequate |
| Finalized-word concurrent release | Loom neighbor/unique-claim model plus production release-to-frontier forced schedule and focused Miri | adequate; GC6-003 resolved |
| Terminal detached and attached traversal | mixed detached/attached/ordinary/retired fixture plus first-panic fixture and focused Miri | adequate; GC6-004 resolved |
| Public managed-`Drop` and pointer-liveness contract | current capability/liveness Rustdoc, safety ledger through C6D, and optional-mutator fixture | adequate; GC6-001 and GC6-005 resolved |
| Unsafe inventory | exact checked-in inventory and passing audit | defer final certification to C6D.3 |

## Resolution Ledger

| Finding | Required disposition | Owner |
| --- | --- | --- |
| GC6-001 | Resolved with one capability/liveness contract and optional-mutator implementation evidence. | completed 2026-08-24 |
| GC6-002 | Resolved with explicit topology and finalizer-commit irreversibility, permanent poison, and complete boundary verification. | completed 2026-08-25 |
| GC6-003 | Resolved with Loom neighboring-bit, visibility, and unique-claim proof plus a production forced-order fixture. | completed 2026-08-25 |
| GC6-004 | Resolved with one mixed detached, attached, ordinary, and retired terminal-topology fixture. | completed 2026-08-25 |
| GC6-005 | Resolved across public API, safety ledger, roadmap, integration boundary, and unsafe-module descriptions. | completed 2026-08-25 |
| GC6-006 | Resolved by removing all collector `dead_code` allowances, gating test/debug-only state, and replacing stale phase-local comments. | completed 2026-08-25 |
| GC6-007 | Resolved with deterministic C7A/C7B checkpoints, C6D.3 ownership of the exact finalized-word proof, and test-only terminal metrics. | completed 2026-08-25 |

## Review Status

The post-C6 review and all seven findings are complete. The focused C6D.3
certification is next and must audit this corrected design rather than absorb
new semantic decisions into a verification checklist.
