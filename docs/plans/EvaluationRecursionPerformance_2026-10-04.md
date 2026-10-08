# Evaluation Recursion Performance — 2026-10-04

Status: active from 2026-10-07 as the `perf-evaluation-recursion` step of
the [performance roadmap](PerformanceRoadmap_2026-10-05.md). It was found
during the [user-input panic safety](UserInputPanicSafety_2026-10-04.md)
evaluation inspection. The quadratic cost was exact-route validation, fixed
by `eval-recursion-route-validation`. The large constant was one coordinator
route per forced lazy, mostly removed by `eval-recursion-inline-forcing`.
`eval-recursion-tail-forwarding` is next.

## Problem

Time to evaluate a simple recursive function grows superlinearly with
recursion depth, roughly quadratically. A release build evaluates:

```text
loop n = if n == 0 then 0 else loop (n - 1)
```

| Program | n = 200 | n = 400 | n = 800 | n = 1,000 |
| --- | ---: | ---: | ---: | ---: |
| countdown, no accumulator | 1.5 s | 4.8 s | — | — |
| countdown, lazy accumulator `acc + 1` | 1.8 s | 5.6 s | 30 s | 38 s |
| accumulator forced with `std.seq` | 2.2 s | 7.7 s | — | — |
| non-tail `1 + count (n - 1)` | — | — | — | 46 s |

Depth 10,000 does not finish within 120 s. Each call costs tens of
milliseconds at modest depth, far beyond any plausible constant overhead.

## Observations

- **Not the lazy accumulator.** Forcing it with `std.seq`, or removing it
  entirely, leaves the growth unchanged. The cost tracks recursion depth.
- **Not a crash.** No variant panicked or aborted. Very deep recursion is a
  hang rather than a stack overflow, consistent with the resumable, heap-based
  evaluator.

## Hypotheses to test first

1. A per-step scheduler operation walks the whole chain of pending lazy
   dependencies. Exact-dependency routing, cycle detection, or demand-spine
   traversal would each make every call O(depth). The cycle diagnostics
   already report full dependency chains. Prior evidence: the resumable-WHNF
   scheduler measurements found an O(depth) chain walk in exact-route
   selection, and accepted O(depth) route storage as a bootstrap cost.
2. Each call becomes coordinator work, plus repeated round trips: holistic
   review S1, E1, and E2. This would add a large constant, but not the
   growth.
3. Managed allocation and rooting cost grows with live heap size: holistic
   review V1.

## Steps

Steps are referred to by name; see the plans README, "Step names".

- **Profile the countdown** (`eval-recursion-profile`). Profile `loop 400`
  with Callgrind or `perf`, and count coordinator transitions per call. The
  `glam-prof` counters (the holistic review's X3) make this cheap.
- **Scaling with pending lazies** (`eval-recursion-lazy-scaling`). Check
  whether time per call scales with the number of pending lazies.
- **Countdown workloads** (`eval-recursion-workloads`). The countdown at a
  few depths is in `scripts/profile.sh` as `countdown_100` to
  `countdown_400`.
- **Fix exact-route validation** (`eval-recursion-route-validation`):
  *done 2026-10-07*; see "Route validation fix" below.
- **Tail-call forwarding** (`eval-recursion-tail-forwarding`): to
  investigate below, under "Findings".
- *Rechecked 2026-10-05:* an old worker stack overflow (the
  `direct_assembly_elf` sample exited with status 134 under `--workers 4` and
  `--workers 1`, before the collector and resumable WHNF) did not reproduce
  in 40 release runs with 1, 2, and 4 workers.

## Findings 2026-10-07 (`eval-recursion-profile`)

**Method.**
- `perf` is unavailable in the dev container (`perf_event_paranoid` is 4),
  so the profiler was Callgrind (Valgrind 3.24, installed with apt).
- The binary was a release `glam-prof` build with
  `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`.
- Comparing countdown depths 100 and 200 separates linear cost from
  superlinear cost. Total instructions were 1.75 G and 5.23 G.

**Cause: exact-route hazard validation (hypothesis 1, confirmed).**
- A client demand keeps an *exact route*: the chain of blocked producers
  from its target to the runnable tip. The countdown's route grows by one
  frame per recursion level.
- Any coordinator mutation that might invalidate a retained route advances
  one global `exact_route_hazard_revision`. The kinds are release, wake,
  settlement and retirement. The next probe then calls
  `validate_exact_route_locked`, which walks every frame of the route,
  with several SipHash map lookups per frame.
- Nearly all of the superlinear instructions are in that function and in
  `work_for_wait_locked`, mostly SipHash.

**Measured** with two new `glam-prof` counters:
- `exact_routes.validated_frames`;
- `coordinator_notify_all_by_kind`.

| Depth | Total ms | Frames walked by validation |
| ---: | ---: | ---: |
| 100 | 595 | 2.35 M |
| 200 | 1,514 | 8.88 M |
| 400 | 4,686 | 34.55 M |

The frames walked are about 235 × depth²: roughly 450 validations per
level, each over the whole route.

Per level, the countdown makes about:
- 60 fresh work admissions;
- 170 work releases;
- 60 dependency wakes;
- 60 work retirements.

So most hazards touch short-lived work that is not on the route at all, or
touch only its tip. Each one still costs a full-route validation.

**Proposed fix** (`eval-recursion-route-validation`, *to discuss*):
- **A bounded hazard log.** Each hazardous mutation logs the work items it
  touched, or a global marker when it cannot name them.
- **Frame indexes.** `route.members` maps each work to its frame index.
- **Validation from the lowest touched frame.** A route checks only the
  log entries since its last validation. It revalidates from its lowest
  touched frame's parent link onward, and does nothing when no member was
  touched. A route older than the log, or a global marker, falls back to
  today's full validation.

Validation then costs O(touched route frames) per probe, O(1) in the
countdown, instead of O(depth). The risk is a mutation site that fails to
name a work it changed. Unknown sites must log the global marker, and the
existing exact-route tests guard the policy.

**Also worth investigating** (`eval-recursion-tail-forwarding`): the
countdown is a tail call, yet each level leaves its caller blocked on the
callee, so the route and the live work grow linearly. Forwarding a caller's
waiters to its tail callee would keep tail recursion at constant route
depth. Non-tail recursion, such as `1 + count (n - 1)`, still needs the
validation fix.

**Constant factors.** The coordinator's id-keyed maps use SipHash, which
dominates these profiles. A faster hasher is a cross-cutting
`perf-structural-overheads` item.

## Route validation fix 2026-10-07 (`eval-recursion-route-validation`)

Approved by the maintainer as proposed.
- **The single boundary takes a hazard.** `advance_work_generation` takes
  a `RouteHazard`:
  - `None` for kinds that cannot affect a route;
  - `Works(&[…])` naming the works whose route-visible state changed (state,
    subscription epoch, dependency, or a wait-index entry mapping to them);
  - `All` when a site cannot name them.

  A debug assertion checks that kind and hazard agree.
- **The hazard log.** Each named work, or `All`, advances the hazard revision
  by one and enters a log of the last 4,096 hazards.
- **Frame depths.** A route's `members` maps each work to its frame depth.
  Depths are stable because the route grows and shrinks only at its tip.
- **Validation** keeps its O(1) checks of the tip and the target's root.
  Then it checks frames only from the parent link of the lowest touched
  member, and nothing when no member was touched. A route older than the
  log, or an `All` hazard, validates in full as before.
- **Sites naming their works**, which are the countdown's hot ones:
  - deferred, lazy-route, reflection and spark releases: the released work
    plus any lazy cycle it terminalized;
  - their retirements: the retired work;
  - dependency wakes: the woken registrations.
- **Sites left as `All`:** cancellation, session closure, terminal and
  stage settlement, task-promise index retirement, and observation wakes.
  None occurs per recursion level.

**Result**, in a release `glam-prof` build:

| Depth | Before | After | Frames validated after |
| ---: | ---: | ---: | ---: |
| 100 | 595 ms | 392 ms | 6,788 |
| 200 | 1,514 ms | 723 ms | 12,788 |
| 400 | 4,686 ms | 1,362 ms | 24,788 |

Time per level is now flat at about 3.2 ms. The remaining cost is ordinary
per-reduction overhead: about 290 reductions per level, at about 11 µs
each. That belongs to `perf-structural-overheads` and the representation
steps.

**Regression test:**
`api::tests::exact_route_validation_stays_linear_in_recursion_depth`, in
the gate's profiling fixtures. It fails, at 243 k against 859 k frames,
when validation is forced to start from the root.

## Inline-first lazy forcing: proposed mechanism 2026-10-07

Approved in principle by the maintainer as the tail-call equivalent; this
section is for review before implementation. It addresses holistic review
decision 3 and `eval-recursion-tail-forwarding`.

**Today.**
- **The cell.** A lazy cell holds a producer, which is `Source`,
  `Checkpoint` (partial progress) or `Panicked`, and a one-time result.
  Partial progress lives in the lazy (`lazy-owns-partial-progress`). The
  cell has no "being evaluated" marker.
- **Forcing.** When WHNF reaches an uncached lazy, it returns a deferred
  boundary. The forcer then reserves the lazy's coordinator route and
  blocks on its wait. Exclusion and sharing come only from that route: one
  route per lazy, claimed while it runs.
- **Cycle detection** walks blocked routes.
- **Measured per countdown level:**
  - 60 routes are created, and none is ever shared.
  - 34 are pure forwards: the forcer had no pending frames, so its result
    is the inner lazy's result.
  - 17 are lazies the WHNF reducer creates and forces at once (function
    and builtin calls), so they are private by construction.
  - 7 routes per level stay blocked, which is the growing chain.
  - About 50 polls per level also end in `Yielded` with budget remaining,
    at family handoffs (holistic review E1 and E2).

**Proposed mechanism.** "Every uncached lazy is a route" becomes "every
*suspended* lazy is a route".
1. **Inline claim.** When an evaluation meets an uncached lazy `B` that has
   no route, it claims `B` inline. It marks `B`'s producer
   `InlineForcing`, under the producer mutex `B` already has, then
   continues evaluating `B`'s source or checkpoint in its own WHNF state.
   No coordinator work is created.
2. **Tail forwarding.** If the forcer `A` had no pending frames, its value
   *is* `B`'s value. `A`'s producer becomes `Forward(B)`, an indirection,
   and evaluation continues in `B`. Each further tail call replaces the
   target, so the indirections compress to the newest one. Nothing stays
   blocked per level, and an unreferenced `A` is garbage. A later observer
   of `A` follows the indirection and caches the value in `A` then. So
   "every member caches" becomes "caches when observed".
3. **Update frames.** If `A` has pending frames, `B` is evaluated beneath an
   update frame. When `B` reaches a value, the value is cached into `B` and
   `A`'s frames continue. This is the existing frame stack plus a "cache
   into `B`" step.
4. **Spill on suspension.** If an inlined lazy must suspend, its progress is
   written back into its own checkpoint, as today. That covers both budget
   exhaustion and a real wait on a promise or route. Only then does it get
   a route, and the forcer blocks on that route as it does now. The
   suspended state is therefore exactly today's state.
5. **Contention.** A second demander that finds `B` marked
   `InlineForcing` requests `B`'s spill and waits on its route. The inline
   owner checks for the request at each step boundary.
6. **Cycles.** Meeting a lazy already marked `InlineForcing` within the same
   evaluation is a cycle. It gives the existing `DependencyCycle` failure,
   with member labels, so it does not loop.

**Never inlined:** host calls, reflection tasks and sparks. Their sources
must not run twice, so they keep routes.

**Panics:** the inline frames name every claimed lazy, so a caught panic
marks each of them `Panicked`.

**Rooting:** inline state lives in the forcer's managed WHNF state, which
collection already traces at each quantum end.

**Expected effect.** Most of the 60 routes per level disappear: the 17
private lazies and most of the 34 forwards. Each one costs an admission,
claim, release, retirement and notifications. Tail recursion stays at a
constant route depth.

**Proposed steps:**
1. **`eval-recursion-inline-private-lazies`.** Inline the lazies the WHNF
   reducer creates and forces at once. They are unshared, so this needs no
   marker. It is the smallest and safest win.
2. **`eval-recursion-tail-forwarding`.** Add `Forward` indirections with
   compression for tail-position forcing.
3. **`eval-recursion-inline-forcing`.** General inline claims, with the
   `InlineForcing` marker, spill on suspension or contention, and cycle
   detection without routes.

### Implementation plan for `eval-recursion-inline-private-lazies`

Approved with the mechanism above (maintainer, 2026-10-07).
- **Privacy.** A saturated application in the WHNF reducer
  (`apply_whnf_function`, `apply_whnf_builtin`) creates a fresh
  function-call or builtin lazy and makes it the focus. The reducer flags
  that focus as private until the focus changes. Its deferred boundary then
  carries `Lazy { root, private: true }`. Nothing else references such a
  lazy: only the forcer's own WHNF state, and later its checkpoint.
- **Interception.** `interpret_poll` maps a private lazy boundary to a new
  "inline" outcome instead of reserving the lazy's route.
- **Inline stack.** The route machine (`LazyTaskMachine`) keeps an explicit
  stack of inline lazies above the claimed one. It polls the top lazy's
  family checkpoint with the shared budget:
  - when the top lazy completes, it is cached, popped, and its parent is
    re-polled, finding the value cached;
  - a private boundary from the top pushes again.
- **Spill.** If any inline lazy suspends, the claimed lazy reserves the route
  of the first inline lazy and blocks on it. Suspension covers budget
  exhaustion, a real wait, and a non-private lazy. Every inline lazy's
  progress already lives in its own checkpoint, so the state is exactly
  today's, and later polls rebuild the chain through the normal path.
- **Panics.** A drop guard marks inline lazies `Panicked` if a poll unwinds
  through them.
- **Cycles.** A private lazy cannot be reached by anyone but its forcer, so
  it cannot close a cycle on its own. A cycle through a shared lazy spills
  and is detected by routes as today.

**Result (2026-10-07).** Inlining only private lazies left about 60 routes
per countdown level, so it gave no benefit. It was not committed; at the
maintainer's direction the general claim below replaced it.

## Inline forcing as built 2026-10-07 (`eval-recursion-inline-forcing`)

Recorded as `inline-lazy-forcing` in `Decisions.md`. The mechanism follows
the proposal above, with these differences:
- **The claim lives in the coordinator.** An inline set beside the
  lazy-to-route index replaces the `InlineForcing` cell marker. A lazy is
  claimable only with neither a route nor another inline claim, both
  decided under one lock, and a route admitted for an inline lazy cannot be
  claimed until the claim ends. Claims end with the forcer's poll, so the
  cell needs no marker, recovery or tracing.
- **Where it is decided.** A route's lazy machine offers each lazy boundary
  inline only when the route's loop drives it (`LazyTaskMachine` with an
  inline depth). Otherwise, as when tests poll a machine directly, the
  boundary keeps its old route-and-wait path. That path also covers every
  ineligible lazy: a host call or reflection task, a routed or claimed lazy,
  or one more than `INLINE_LAZY_DEPTH` (32) above the route's lazy.
- **Spill target.** A suspended inline lazy spills *itself*, the top of the
  stack, not the first inline lazy. The lazies between are then polled again
  once, when the top completes. Spilling the first one, or yielding with the
  stack, re-walked up to 32 lazies on every quantum; a measured variant that
  yielded raised access regions by 40%.
- **No spill request, no inline cycle check.** A second demander simply
  admits the lazy's route and waits, which resolves at the end of the
  forcer's poll rather than at its next step. That route is `InlineForced`
  meanwhile, a busy work state, so probes report it busy rather than
  runnable; the release makes it dormant, or queued if it was demanded. A cycle through inline lazies
  meets an already-claimed lazy, admits its route and blocks; spilling turns
  the cycle into routes, and the existing detection reports it with every
  member's label.
- **Handoff re-polls.** A poll that yields with budget left, at a family
  handoff, is polled again rather than suspended, at most 8 times in a row
  without consuming budget. This also applies to the route's own lazy, which
  used to return to the scheduler at every handoff (part of holistic review
  E1). Without it, spilled routes multiplied scheduler round trips.
- **Panics.** Inline polls run under `catch_unwind`. A panic marks the
  inline lazy that panicked, ends the claims, and resumes unwinding to the
  scheduler boundary, which marks the route's lazy. Lazies between keep
  their checkpoints and observe the panicked lazy when next forced.

**Measured** with `scripts/profile.sh`, which now counts user-space
instructions with `perf` (repeat runs agree within 0.01%). Before is
`3791da97`, after `perf-fast-id-hashing`; after is `20c5598a`:

| Workload | M instructions before | After | Route admissions before | After |
| --- | ---: | ---: | ---: | ---: |
| `minimal` | 38.9 | 34.9 | 328 | 63 |
| `countdown_100` | 420.3 | 344.2 | 6,540 | 487 |
| `countdown_400` | 1,556.0 | 1,272.4 | 24,540 | 1,687 |
| `hello_elf` | 4,133.8 | 3,656.6 | 44,092 | 5,332 |
| `list_map_1000` | 1,439.3 | 1,285.6 | 7,572 | 605 |
| `dict_lookup_1000` | 577.1 | 497.2 | 6,473 | 317 |

Reductions, allocations and access regions are within a few percent of
before. Route admissions are counted as `fresh_work_admission`
notifications.

CPU time fell further than instructions: `countdown_400` from 1,286 to
854 ms and `hello_elf` from 2,890 to 1,937 ms (`perf` task clock, three
runs each). A third of that time is in the kernel, nearly all of it
`futex` wake-ups (see the finding below), and fewer routes mean fewer
coordinator wake-ups. Wall times from `scripts/profile.sh` are trend data
only on a shared machine.

**What remains per countdown level:** about 4 route admissions. Without
tail forwarding, the chain of lazies a countdown level leaves pending is
about 58 deep, so the stack reaches its depth limit about every half level
and spills. Each spilled route also stays blocked until the recursion
returns. `eval-recursion-tail-forwarding` removes that chain for tail
calls.

**Tests:**
- `inline_forcing_admits_few_routes_per_recursion_level`, a `glam-prof`
  fixture: under 10 admissions per countdown level, against about 65
  without inlining.
- `deep_non_tail_recursion_spills_inline_lazies_to_routes`.
- `a_cycle_among_inline_forced_lazies_is_a_dependency_cycle`.
- `a_panic_in_an_inline_forced_lazy_is_recorded_in_it_and_ends_the_claims`.
- The net driver's test-only work-item probe now spends the step budget,
  and also fires before a new batch. Otherwise a route re-polling a
  handoff ran past the limit. A wrapper fixture's scheduler polls fell from
  17 to 3, with its reduction and driver counts unchanged.

**Uncached failures** (reviewed with the maintainer, 2026-10-08). An inline
lazy whose poll fails without caching ends the route's poll, uncached. A
lazy machine fails uncached only for an observer-relative refusal:
- admission refused, because the demand closed or the coordinator expired;
- a reflection promise observed by its own producer task, which a lazy
  route cannot hit, having no task identity.

Routes run in the runtime's background demand, so in practice this is
shutdown. The test
`an_uncached_failure_in_an_inline_forced_lazy_ends_the_route_uncached`
covers it, and resumes both lazies with a later route.

**Finding: refusals are cached at the WHNF boundary.** Without inlining,
the same refusal *was* cached in the route's lazy. The WHNF checkpoint
boundary passes every interpretation failure to `fail`, which caches it,
while the family boundaries pass them on uncached. `WhnfOwnerPoll::Failed`
conflates permanent evaluation failures with observer-relative refusals.
Separating them is a candidate follow-up; inlining already avoids the cache
for inline lazies.

## Poll quantum comparison 2026-10-08

Every claimed poll gets `TASK_POLL_QUANTUM` = 64 steps, and foreground
pumping reserves 4,096 steps per round. With inline forcing in place, the
workloads ran at larger quanta. The allowance was raised to match at
16,384.

| Workload | 64 | 256 | 1,024 | 4,096 | 16,384 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `hello_elf` M instructions | 3,656.6 | 3,668.7 | 3,549.6 | 3,595.9 | 3,610.4 |
| `countdown_400` M instructions | 1,272.4 | 1,281.6 | 1,274.5 | 1,266.9 | 1,267.0 |
| `list_map_1000` M instructions | 1,285.6 | 1,272.8 | 1,271.3 | 1,271.1 | 1,270.6 |
| `hello_elf` route admissions | 5,332 | 3,493 | 2,267 | 2,700 | 2,838 |
| `hello_elf` route releases | 11,646 | 6,479 | 4,480 | 4,754 | 4,876 |
| `countdown_400` route admissions | 1,687 | 1,303 | 1,250 | 1,200 | 1,200 |

- A larger quantum halves scheduler traffic or better, but changes
  instructions by 3% at most. Once inline forcing removed most routes, the
  quantum stopped being a lever.
- From 1,024 up, the countdown's admissions are its depth-limit spills,
  about 3 per level, which tail forwarding addresses.
- Reductions fall by under 0.5% at larger quanta, the work repeated after
  suspensions. Access regions vary within a few percent.
- The quantum stays at 64 for now. Revisit it in the performance review,
  together with fairness between demands.

## Finding 2026-10-08: a futex wake per access region

In `countdown_400`, `strace` counts 518,114 `futex` calls, nearly all of
the process's syscalls, with a single evaluating thread. `getrusage`
puts system time at 0.25 s of 0.85 s CPU.

The source is the collector's admission. `release_outer_mutator` in
`crates/glam-gc/src/heap.rs` calls `notify_all` on `admission_changed`
whenever the active mutator count reaches zero. Single-threaded, that is
the end of every outer access region, about 437,000 of them here. The
standard library's futex condvar makes a `FUTEX_WAKE` syscall on every
notify, waiter or not.

**Fixed** (`perf-admission-wakeups`, 2026-10-08). The coordinator counts
its waiters, each registered under the lock before it sleeps, and a
notification with none is skipped. Notification therefore requires the
locked coordinator, which every caller already held.

| `countdown_400` | Before | After |
| --- | ---: | ---: |
| `futex` calls | 518,114 | 75,019 |
| User CPU | 0.60 s | 0.34 s |
| System CPU | 0.25 s | 0.07 s |

- User time fell too: each syscall also costs the code around it.
- `hello_elf` fell from 1.93 to 1.13 s of CPU, and `list_map_1000` from
  about 445 to 310 ms.
- User-space instructions fell under 1%. Instruction counts alone would
  have hidden the change, so `scripts/profile.sh` now also reports CPU
  time.

**Remaining wake-ups** in `countdown_400`, 75,000 in all:
- about 16,000 on one condvar, most likely the runtime activity condvar
  (`RuntimeActivityState::changed`), notified by every guarded runtime
  transition;
- about 8,900 on the coordinator's shared condvar;
- about 50,000 spread over 7,500 addresses, so probably condvars in
  per-net or per-wait cells.

The same waiter count applies to each. The maintainer also suggested
coalescing: a notify flag set by mutations and flushed once per quantum,
trading latency for throughput. That suits the coordinator's condvar, where
a quantum makes bursts of mutations and parked workers can wait a quantum.
It must flush before the notifying thread parks or blocks itself.
