# Resumable WHNF W6G.4 Investigation — 2026-09-23

Baseline: `7fed99e` immediately before W4C.1c; current implementation
`d8d44e0` after W6G.1, extracted W6G.2, and W6G.3.

Status: investigation complete; repair decision and implementation remain
pending. No production fix was applied during this investigation.

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

**Status:** remediation planned

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

### W6G4R-001B — Make complete discovery one guarded operation

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

### W6G4R-001C — Measure the cold-path repair

Re-run the exact fixture and capture:

- cold searches, visited edges, maximum depth, and allocations;
- Callgrind instruction attribution and DHAT allocation attribution;
- debug and release timing as corroboration; and
- unchanged semantic and interaction-net driver signatures.

This checkpoint determines how much cost remains asymptotic rather than
constant-factor. It does not close the finding merely because the fixture
becomes faster.

### W6G4R-001D — Inventory incremental route handoffs

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

### W6G4R-001E — Implement incremental descent and return

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

### W6G4R-001F — Exercise invalidation and fallback

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
