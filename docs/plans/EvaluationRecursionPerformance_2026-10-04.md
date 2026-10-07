# Evaluation Recursion Performance — 2026-10-04

Status: active from 2026-10-07 as the `perf-evaluation-recursion` step of
the [performance roadmap](PerformanceRoadmap_2026-10-05.md). It was found
during the [user-input panic safety](UserInputPanicSafety_2026-10-04.md)
evaluation inspection. The root cause is not yet established.

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

