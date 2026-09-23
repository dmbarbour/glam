# Resumable WHNF W6G.4 Investigation — 2026-09-23

Investigation baseline: `7fed99e` immediately before W4C.1c and pre-repair
`d8d44e0` after W6G.1, extracted W6G.2, and W6G.3. Cold-path repair
measurement: `fdb52907` after W6G4R-001B.

Status: investigation and W6G4R-001A-F complete; closing performance
measurement is next. Foreground exact demand retains a local validated route
across common work transitions; the complete guarded traversal remains the
authoritative cold and invalidation fallback.

## Scope

This investigation follows W6G.4 in
[`ResumableWhnfEvaluation_2026-09-12.md`](../plans/ResumableWhnfEvaluation_2026-09-12.md).
It profiles the exact source-shaped duplicate-symbol fixture from
`direct_assembly_rejects_duplicate_symbol_publication`, localizes the residual
regression by implementation phase, and identifies the first repair boundary.
It does not reconsider the semantics of resumable WHNF, role-specific pumping,
or the separately deferred pure-effect fusion work.

All source instrumentation and experimental edits were confined to detached
worktrees. The main worktree received only this investigation record and the
corresponding plan update.

## Method

The exact fixture was timed after a warm build at selected historical commits.
Elapsed and CPU time are corroborating evidence rather than a test gate.

Linux `perf` was installed from the Debian package but could not collect events
because the host exposes `kernel.perf_event_paranoid=4` and does not permit the
container to lower it. The investigation therefore used Debian's Valgrind
3.24.0 package:

- Callgrind counted release-build instructions and call paths;
- DHAT counted heap allocation volume and attributed allocation sites; and
- temporary static counters measured dependency-chain calls, edge visits, and
  maximum depth without changing the main worktree.

The comparison used the same compiler, configuration, source text, and
expected duplicate-symbol result. Callgrind and DHAT change absolute execution
time substantially, so only counts from the same tool are compared.

## Results

### Phase localization

The warmed debug fixture took approximately:

| Revision | Boundary | Seconds |
| --- | --- | ---: |
| `7fed99e` | pre-W4 | 8.63 |
| `028a215` | W4E complete | 13.00–13.24 |
| `62d7c34` | W5 complete | 13.14–13.62 |
| `a581fc3` | W6A complete | 13.38 |
| `76e3f1f` | W6B/NC complete | 13.27 |
| `716c87e` | W6C complete | 13.50 |
| `70f2675` | W6D complete | 14.37 |
| `4560cc3` | W6E annotations | 15.13 |
| `a77a2dc` | W6E effect dispatch | 17.18 |
| `0372d66` | W6F complete | 17.62–17.66 |
| `e7cf6e0` | immediately before W6G | 18.48–19.48 |
| `d8d44e0` | current | 16.43–16.79 |

The single-run intermediate points are too noisy to assign small changes, but
the boundaries establish three useful facts:

1. W4 introduced the original large residual.
2. W6D/W6E enlarged it; the effect-dispatch cutover is the largest isolated
   later step.
3. W6G.1 recovered roughly two seconds rather than causing the remaining cost.

Release timings show the same shape at a smaller scale: the pre-W4 binary took
0.63–0.77 seconds and current took 1.36–1.60 seconds in interleaved runs.

### Semantic work remains comparable

Callgrind observed closely comparable interaction-net activity. Examples are:

| Runtime-net operation | pre-W4 | current |
| --- | ---: | ---: |
| `connect` | 983,890 | 975,730 |
| `disconnect` | 345,927 | 343,548 |
| `pair_nodes` | 80,106 | 78,924 |

This agrees with the W4E semantic/driver accounting. There is no evidence that
the current gap is replayed net work.

### Exact producer-chain rediscovery dominates

The current `prioritized_task_for` starts from the requested wait token,
rebuilds a `Vec` and `HashSet`, follows the complete producer chain through
separately locking coordinator queries, and then scans the collected chain in
reverse for a claimable record.

Temporary counters on the current fixture reported:

- about 18,800 calls to `prioritized_task_for`;
- 2,759,357 dependency edges visited by the 18,000th call;
- an average of 153.3 visited edges per call at that point; and
- a maximum chain depth of 573.

The same counter applied to W4E reached depth 572 and had visited 1,320,995
edges by the 15,000th call. This corrects the old W4E statement that the
largest executable-fixture chain depth was four. Later migrations enlarged an
existing pathology but did not create it.

Callgrind measured 4,923,672,826 current release instructions versus
1,696,306,307 pre-W4. Randomized hashing of `NonZeroU64`-backed private IDs
accounted for 1,219,412,748 current instructions (24.77%) versus 365,267,714
pre-W4 (21.53%). The current profile includes about 2.83 million calls to
`work_dependency_by_id`; the overwhelming majority lie beneath exact-chain
selection.

DHAT makes the allocation component explicit:

| Allocation source | Bytes | Blocks |
| --- | ---: | ---: |
| `prioritized_task_for` temporary `HashSet` | 83,040,476 | 111,296 |
| `prioritized_task_for` temporary `Vec` | 64,574,176 | 108,360 |
| both traversal containers | 147,614,652 | 219,656 |
| current process total | 305,044,918 | 1,208,022 |
| pre-W4 process total | 138,793,591 | 881,143 |

The two temporary containers are 48.4% of current allocated bytes and explain
88.8% of the byte increase over pre-W4. They also explain 67.2% of the added
allocation count.

### Effect dispatch is an increment, not the root cause

Immediately before resumable effect dispatch (`4560cc3`), Callgrind measured
4,859,496,844 instructions and 1,183,834,548 hashing instructions. Current
measured 4,923,672,826 and 1,219,412,748 respectively. Native release time rose
from about 1.28–1.32 seconds to 1.38–1.40 seconds.

Effect dispatch adds more producer records and deeper/more frequent
rediscovery, but the W4-era selector already carries most of the cost. Pure
effect fusion may later reduce chain depth, but W6G.4 must not depend on that
deferred optimization for acceptable scheduler complexity.

## Experimental constraint

A detached-worktree experiment changed `prioritized_task_for` to return the
first claimable record while walking forward and stopped building the `Vec`.
It preserved the expected fixture result but worsened debug time to about 17.2
seconds. The experiment still entered the coordinator mutex separately for
`work_for_wait`, `work_is_claimable`, and `work_dependency_by_id` at each edge.

This rejects “remove one temporary container while preserving component
queries” as the first repair. It does not reject state-aware forward selection;
that policy must be evaluated together with one guarded coordinator traversal.

## Selected remediation direction

The first candidate repair should be a coordinator-owned exact-target selector
which, under one mutation admission and one coordinator-state lock:

1. follows the target's exact producer chain;
2. interprets each record's current state rather than following a stale prior
   block through a queued record;
3. returns `Ready`, `Busy`, or `None` using the same state vocabulary as
   `causal_background_probe_locked`; and
4. claims the selected record before releasing the lock.

This reuses an established locking and selection shape, removes the temporary
`Vec`, avoids millions of component-lock acquisitions, and leaves one bounded
cycle detector as the initial simple implementation. The implementation must
first latch ordering tests for a queued parent with an old block, a busy leaf,
an exact dependency cycle, and cross-session observation.

The direction still has one semantic point to confirm: current foreground
selection collects the whole chain and prefers the deepest claimable record,
whereas background traversal runs a queued ancestor before following its old
block. No design text found in this investigation justifies the foreground
asymmetry. The repair should deliberately unify the policies unless a forced
test demonstrates a required difference.

If one-lock traversal remains material after measurement, the next structural
option is an authoritative ready-descendant index or readiness propagation
along completion-subscription edges. That is a larger invalidation and cycle
handling change and should not precede evidence from the simpler repair.

A private-ID hasher, reusable scratch storage, or traversal stamps may reduce
constant cost, but none resolves repeated O(depth) rediscovery. They are not
recommended as the primary repair.

## W6G4R-001 — Exact demand routes are rediscovered from their root

**Severity:** high performance

**Status:** remediation in progress; W6G4R-001A-F complete

The coordinator retains every exact dependency edge needed to describe the
current demand route, but foreground pumping retains no position within that
route. After a selected producer yields, blocks, completes, or is temporarily
claimed elsewhere, the next selection normally starts again at the client
demand's original wait token. The cost therefore approaches the number of
scheduler transitions multiplied by the current route depth.

The repair has two layers:

1. make complete rediscovery a coherent, low-constant-cost cold path; then
2. retain an explicitly non-authoritative route checkpoint so the common path
   advances one scheduler edge at a time.

The checkpoint is a scheduling hint, not duplicated semantic state. Work
records, subscription epochs, and completion sources remain authoritative.
Dropping or invalidating every checkpoint must preserve behavior and merely
fall back to complete rediscovery.

### W6G4R-001A — Latch exact-selection semantics and counters

**Completed:** 2026-09-23

Before changing selection, add forced coordinator fixtures for:

- a queued ancestor which still carries its previous blocked dependency;
- a blocked chain ending in one dormant or queued producer;
- a chain ending in a running producer;
- an exact dependency cycle;
- a producer observed from another demand session in the same runtime; and
- retirement or dependency replacement between probe and claim.

The queued-ancestor fixture is the semantic decision gate. The intended rule
is the rule already used by background roots: queued work runs before its old
block is followed. First demonstrate that the old foreground reverse scan
selects the stale descendant, then change the assertion with the repair. If a
real contract requires deepest-first selection, stop and revise the following
checkpoints rather than preserving the asymmetry accidentally.

Add statically compiled profiling counters, or an equivalent test-owned probe,
for complete searches, edges visited, maximum depth, fast handoffs, checkpoint
invalidations, and fallback reasons. Ordinary builds must not acquire a
callback or dynamically configured observer on every transition.

The coordinator now has a test-only, statically compiled route profile with
those fields. Complete reverse searches publish their search count, visited
edges, and maximum depth; later checkpoints will populate the handoff and
fallback fields. Forced fixtures cover the queued ancestor mismatch, queued
and running tails, exact cycles, same-runtime cross-session demand, and a
producer which terminalizes between the old probe and separate claim. The
queued-ancestor assertion deliberately records the old stale-descendant
selection so W6G4R-001B can reverse that assertion alongside the atomic
selector repair.

### W6G4R-001B — Make complete discovery one guarded operation

**Completed:** 2026-09-23

Introduce a coordinator-private exact-target probe with a result such as
`Ready(EvaluationWorkId)`, `Busy`, or `None`. Under one runtime mutation
admission and one coordinator-state lock, it must:

1. resolve each wait to its producer record;
2. inspect that record's current `WorkState`;
3. return queued work, or a demanded dormant deferred/lazy route, before
   consulting any retained prior block;
4. follow a dependency only from an actually blocked record;
5. detect cycles without constructing a reverse result vector; and
6. claim the selected record before releasing the state lock.

Use the same state vocabulary and claim helpers as
`causal_background_probe_locked`. Replace the foreground composition of
`prioritized_task_for`, `work_is_claimable`, `claim_work`, and the duplicated
running-producer scan. Audit and remove component query helpers which become
test-only or unused.

Preserve the existing immediate re-claim of a yielded exact work item. Do not
add a per-session running-work index, change worker/client ownership, or pull
pure-effect fusion back into W6G.

`claim_exact_target` now traverses current-state dependency edges and claims
the selected task beneath one mutation admission and coordinator-state lock.
The shared traversal stops at queued ancestors, follows dependencies only from
blocked records, reports running/terminalizing producers as busy, and bounds
cycles with one forward `HashSet`; it constructs no reverse route. Foreground
demand pumping and synchronous client-demand driving use this operation, while
their wait/retry paths use one-lock status snapshots. The old
`prioritized_task_for`, `work_is_claimable`, and multi-lock running-producer
scan have been removed from production. A forced hook verifies that both
mutation admission and the state mutex remain held between selection and
claim.

### W6G4R-001C — Measure the cold-path repair

**Completed:** 2026-09-23

Re-run the exact fixture and capture:

- cold searches, visited edges, maximum depth, and allocations;
- Callgrind instruction attribution and DHAT allocation attribution;
- debug and release timing as corroboration; and
- unchanged semantic and interaction-net driver signatures.

This checkpoint determines how much cost remains asymptotic rather than
constant-factor. It does not close the finding merely because the fixture
becomes faster.

The exact duplicate-symbol fixture retained its expected diagnostic and the
interaction-net profiling regression script retained every semantic/driver
signature. Warm direct test-binary timings were 15.97–16.94 seconds in debug
(the last two runs were 15.97 and 15.99) and 1.27–1.28 seconds in release,
compared with the pre-repair 16.43–16.79 and 1.36–1.60 ranges. These timings
remain corroborating rather than gating evidence.

Callgrind measured 4,504,812,008 release instructions, down 418,860,818 or
8.51% from the 4,923,672,826 pre-repair total. `claim_exact_target` still
accounted for 2,636,835,645 inclusive instructions (58.53%), its shared
forward probe accounted for 2,605,256,370 (57.83%), and private-ID hashing
accounted for 2,010,888,838 (44.64%). Thus one-lock discovery removed material
constant work without making complete route discovery cheap.

Call counts and a temporary statically compiled maximum-depth probe recorded
19,630 complete searches over 2,811,441 visited records: an average depth of
143.22 and a maximum depth of 573. Of those searches, 18,824 attempted an
atomic claim and 806 were read-only wait/retry snapshots. The maximum is
unchanged and the edge count remains of the same order as the pre-repair
profile, confirming that the residual is still repeated O(depth) discovery.

DHAT measured 239,291,699 allocated bytes in 1,092,327 blocks, down 65,753,219
bytes (21.56%) and 115,695 blocks (9.58%) from the pre-repair totals. Allocation
beneath the remaining forward probe was 84,990,872 bytes in 125,051 blocks,
35.52% of all allocated bytes. This is essentially the bounded cycle
`HashSet`; the old 64,574,176-byte reverse `Vec` has disappeared, while the
forward-set cost remains. W6G4R-001D/E therefore remain justified: B is the
appropriate coherent fallback, not the final performance repair.

### W6G4R-001D — Inventory incremental route handoffs

**Completed:** 2026-09-23

For every foreground exact-demand driver, classify the information already
available after a poll:

- `Yielded`: the same work ID remains the preferred target;
- `Blocked`: the published dependency identifies the direct producer;
- `Terminal`: the route should return to the parent which led to this work;
- `Busy`: the contested candidate remains a useful retry point; and
- observation-only or resolver-owned waits: no producer work can be followed.

Include blocking `drive_client_demand`, bounded public advancement,
`pump_demand`, and any test-only compatibility drivers. Identify where a route
must live to survive a bounded `try_advance` return. Prefer foreground demand
or handle state; do not place roots or scheduler routes inside semantic values,
lazy cells, or runtime nets.

The target shape is a lightweight scheduler zipper, approximately:

```text
ExactDemandRoute
  current: WorkId
  parents: Vec<RouteFrame>

RouteFrame
  work: WorkId
  subscription_epoch: u64
  dependency_key: WorkDependencyKey
```

The exact representation remains an implementation checkpoint. It contains
only scheduler identities and validation tokens, not `Value`, `Gc`, or
`RuntimeValueRoot` edges.

The inventory found one common mechanism with several different-duration
owners. `EvalContext::pump_wait` and `pump_demand` are currently stateless
bounded calls: only the `yielded_exact` work ID survives within one call. A
budget return, contention return, or caller-level suspension discards it and
the next call begins at the original wait token. The implementation checkpoint
should therefore introduce a private route-aware pump state and retain the
current method as a cold compatibility wrapper, rather than placing the route
in `EvalContext` or in the coordinator's semantic work records.

The foreground-driver census is:

| Driver | Current lifetime | Route owner selected for W6G4R-001E |
| --- | --- | --- |
| Blocking `drive_client_demand` behind `ValueEvaluator::eval` | The private `ClientDemandHandle` is consumed by one blocking loop; its client record and result cell already survive thread waits. | One local route beside the handle. The client record remains authoritative for its own blocked dependency and subscription epoch; the route begins at that published dependency. |
| Future bounded public advancement | No public `Evaluation` or `try_advance` API exists. `ValueEvaluator::eval` is still blocking. | The [public resumable-evaluation plan](../plans/PublicResumableEvaluation_2026-09-23.md) owns this deferred facade. It must retain the private client handle and route together so both survive a bounded return; W6G4R does not expose the facade. |
| `EvalContext::pump_wait` / `pump_demand` | A generic bounded helper used by direct evaluation, effect machines, macro execution, and tests. It retains only one yielded work ID until the call returns. | Add a private stateful form accepting an owner-retained route. Keep the current method as a cold, one-call wrapper for compatibility and tests which do not measure resumed-route cost. |
| Direct `await_deferred_task` | A nonscheduled direct evaluator loops in the same Rust call; a scheduled machine performs one bounded assist and then publishes its own block. | The direct loop can retain a local route. A scheduled machine does not put a route in the lazy/promise: after it returns, its published parent block lets its outer foreground driver descend normally. |
| Reflection/effect `run`, bounded `poll`, and `poll_blocked` | `run` is blocking, while bounded polls retain `self.blocked` across scheduler quanta. A budget-exhausted nested pump can currently yield the outer task and forget the inner exact position. | Blocking `run` may retain a local route. Persistent effect machines retain an optional route beside the blocked scheduler state, keyed by the current wait; changing or clearing that wait clears the route. This is orchestration state, not an effect value. |
| Macro `IsolatedEffectSearch` runner | One blocking expansion loop repeatedly polls the same search and pumps its reported dependency. It does not return an incremental handle. | One local route for the currently reported dependency, reset when the dependency changes. It remains isolated from macro input/output values. |
| Test compatibility paths | `drive_client_demand_for_test` uses the production blocking driver. Direct `pump_wait` loops, `complete_wait`/`fail_wait`, and inline net fixtures use the stateless helper; `claim_ready_client_demand_for_test` is a lifecycle hook rather than an exact-route driver. | Exercise the production route through the production driver and a new stateful bounded helper. Leave deliberately cold compatibility calls stateless unless a fixture specifically verifies persistence. |

The existing wait helpers are part of the same seam. `wait_for_claimed_task`
and `retry_after_no_progress` currently call `exact_target_status` from the
root and repeat the full guarded traversal. The stateful pump should expose
the exact busy/current checkpoint needed for a lost-wakeup-safe generation
wait and route-aware recheck. The root-based helpers remain correct fallbacks;
they should not remain the common resumed path.

The poll-result census also refined the proposed handoff boundary:

| Poll/release result | Information available | Safe route action |
| --- | --- | --- |
| Yield or spark request | The claimed work ID is known, but cancellation, abandonment, or an immediate wake may override the apparent yielded state during release. | Retain the ID only when the post-release disposition says the same record remains runnable or dormant-but-exactly-demanded. |
| Block | The raw poll names a dependency, but the coordinator assigns the subscription epoch while publishing it. Subscribe-and-recheck may immediately queue the parent again. | Have release return a validated blocked handoff only if the record remains blocked after subscription and observation rechecks. That handoff supplies parent ID, epoch, dependency key, and directly resolvable producer. |
| Terminal completion or failure | The claimed ID and terminal result are known. Settlement may wake its parent; lazy-cycle settlement may terminalize several remembered frames at once. | Pop toward the remembered parent, then atomically validate/claim it. A multi-record terminalization merely invalidates frames and invokes the existing fallback. |
| Busy exact producer | The guarded traversal sees the contested `Reserved`, `Running`, or `Terminalizing` record, but `ExactTargetSelection::Busy` currently discards its ID. | Preserve the candidate ID in the selection/status disposition and keep it as `route.current` across the generation wait. |
| Resolver-owned promise, broad observation wait, or reflection exit | The block has no directly followable producer edge. | Retain the parent as the parked point, but do not push a child frame. Ordinary no-progress, observation, or exit policy remains responsible for the wait. |

Consequently, the raw `EvaluationMachinePoll` is not the route API.
`release_reflection_task`, `release_deferred_task`, and `release_lazy_route`
currently collapse their result to booleans, while `release_client_demand`
returns no disposition. W6G4R-001E should add a small post-release scheduling
disposition produced after block publication and guarded subscribe/recheck.
It need not expose the machine or semantic result. This avoids constructing a
frame with an epoch that never became authoritative or descending after an
already-completed dependency.

The selected zipper remains local and non-authoritative:

- its original target wait stays with the driver for complete guarded
  fallback; it need not be duplicated into every frame;
- `current` is the exact runnable or contested background work ID;
- each parent frame records the parent's work ID and the subscription epoch
  and dependency key which justified descent;
- the blocking client root is validated through `ClientDemandSnapshot`
  separately from background work records; and
- a causal `.task.new` child selected after the exact route is exhausted is
  not an exact zipper edge. It may receive the existing immediate yielded
  re-claim within one call, but it is discarded at a bounded return and found
  again through the causal-child probe.

Claiming from a checkpoint must validate and claim under the same mutation
admission and coordinator-state lock. A blocked parent must still have the
recorded epoch and dependency key; a current record must still exist in the
expected runnable or contested state. Failure of either check is a cache miss,
not a semantic failure, and falls back to W6G4R-001B's complete guarded
search. No route owns a work record, keeps a demand session alive, or adds a
managed edge.

### W6G4R-001E — Implement incremental descent and return

**Completed:** 2026-09-23

Extend claim/release dispositions so the foreground driver can update the
route without searching from its root:

- push the current parent when it blocks on a directly resolvable producer;
- retain the current work across an ordinary yield;
- pop to the remembered parent after terminal completion;
- retain a busy candidate while waiting for a coordinator change; and
- persist the zipper across bounded client returns.

Each frame is valid only while its parent remains blocked at the recorded
subscription epoch on the recorded dependency key. Validation and claim must
occur atomically under coordinator state. The existing completion subscription
continues to wake work; the zipper only tells the foreground driver which
record on its own route to try next.

Do not maintain an eagerly propagated root-to-leaf cache in every work record.
That would move the same O(depth) work into invalidation and introduce shared
write contention before the local route has been measured.

Foreground exact demand now carries a private `ExactDemandRoute` containing
the current work, validated parent frames, a retained-route cycle set, and the
last observed coordinator generation. The coordinator claims from this route
under the existing mutation admission and state lock. A matching generation
continues directly; a changed generation first validates the retained frames;
and failed validation invokes W6G4R-001B's complete guarded traversal. The
cycle set is local scheduler state, not an authoritative descendant index, and
prevents lazy/reflection wait cycles from making retained traversal spin.

Task release produces a post-publication route disposition. A small generation
tracker accounts for the release's own queue, subscription, promotion, and
observation transitions; any interleaved mutation makes the handoff a cache
miss. An uninterrupted block pushes its directly resolved producer, yield
retains the current work, terminal completion pops its parent, and a busy
selection preserves the contested work ID. Completion subscriptions remain
the wake authority—the zipper neither owns nor wakes work.

Blocking client demand and macro execution retain a local route, effect tasks
retain one beside their blocked orchestration state, and direct deferred waits
retain one for the lifetime of their loop. The route-aware bounded helper
accepts caller-owned state so a future public bounded evaluation facade can
retain it without placing scheduler data in values, lazies, or runtime nets.
The deliberately cold test compatibility wrapper still constructs a route for
one call.

Forced tests cover direct block descent, repeated yields across bounded
returns, a contested child becoming available, terminal return to a woken
parent, and pre-existing lazy/reflection cycles. The transition tests assert
that only the initial cold search occurs; repeated execution is not used as
evidence for the concurrent ordering.

### W6G4R-001F — Exercise invalidation and fallback

**Completed:** 2026-09-23

Force both sides of each condition which can invalidate or suspend the route:

- another thread claims the anticipated child;
- the child completes before the foreground claim;
- the parent is woken by another completion source;
- the parent publishes a different dependency at a later epoch;
- work or its demand session retires;
- lazy-cycle terminalization retires several frames;
- a task-owned promise has a producer but a resolver-owned promise does not;
- causal `.task.new` work branches away from the exact producer route; and
- a bounded public evaluation is dropped, resumed, or abandoned.

An invalid checkpoint falls back to the nearest independently valid frame when
that can be established in O(1); otherwise it invokes the complete guarded
search from the original target. It must never convert contention into stable
absence, keep retired work alive, or broaden foreground authority to unrelated
same-session work.

Fallbacks now retain one private reason until the next authoritative rebuild:
interleaved release contention, a changed dependency/epoch, retired work, or
replacement of a remembered producer branch. Test-only profiling increments
the matching reason counter alongside the aggregate invalidation and cold-
fallback counts. A generation change by itself is not an invalidation: if all
frames still validate, the route continues and a child claimed by another
poller remains a `Busy` checkpoint.

The one constant-time retirement recovery is deliberately narrow. If the
current child disappeared and its only remembered parent is the original
target root, the coordinator can discard that child frame and continue from
the independently rooted parent. Deeper retirement cannot prove that all
remaining ancestors still lead from the target in O(1), so it uses the cold
guarded traversal. A woken parent or a parent reblocked at a new subscription
epoch similarly rebuilds and selects current state rather than following the
stale child.

Forced tests cover an anticipated child claimed and released by another
poller, child completion before foreground claim, an observation waking the
parent while its child remains live, reblocking onto another producer, an
interleaved release mutation, whole demand-session retirement, task-owned
versus resolver-owned promise dependencies, lazy/promise cycle settlement in
both publication orders, and causal child work remaining outside the exact
zipper. Discarding caller-owned bounded route state causes one cold rebuild and
does not change the result; dropping the demand owner retires work even while
the route remains alive.

There is still no public resumable `Evaluation` facade, so F cannot literally
drop or resume a public bounded evaluation handle. The private stateful pump
test covers route discard/resumption, while the owner-closure fixture covers
abandonment and proves the route holds neither a work lease nor a demand-owner
lease. The [deferred public facade plan](../plans/PublicResumableEvaluation_2026-09-23.md)
must repeat those lifecycle assertions for its own handle rather than treating
this API surface as implemented.

### W6G4R-001G — Close the performance finding

Repeat the W6G4R-001C measurements after incremental routes. Report:

- fast handoffs versus complete fallback searches;
- fallback reason counts and edges per fallback;
- maximum retained route depth and storage;
- Callgrind and DHAT deltas from current and pre-W4 baselines; and
- the exact fixture's semantic/driver signature and diagnostic.

The expected complexity for an uncontended linear demand is amortized in the
number of actual route transitions, plus O(depth) only for cold entry or a
genuine invalidation. If complete searches remain frequent, use the recorded
reasons to decide whether background roots need similar route state or whether
an authoritative ready-descendant index is justified. Do not introduce either
without that evidence.

Run the routine repository gates and
`scripts/check-interaction-net-profiling.sh`, update the W6G.4 plan status, and
reconcile W6G.5 before closing the finding.

## Verification required by the repair

Before-and-after verification should include:

- the exact source-shaped duplicate-symbol fixture in debug and release mode;
- Callgrind instruction counts and DHAT allocation attribution;
- static chain-call, edge-visit, and maximum-depth counters;
- the W4E semantic and driver signatures;
- forced queued-parent, busy-leaf, cycle, and cross-session selector tests;
- `scripts/check-interaction-net-profiling.sh`; and
- the routine formatting, Clippy, and test gates.

Repeated timing alone is not evidence for scheduling correctness.
