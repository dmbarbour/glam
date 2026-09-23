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

## Repair decision gate

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
