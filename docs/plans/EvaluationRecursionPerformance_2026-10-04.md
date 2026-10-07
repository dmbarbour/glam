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
- *Rechecked 2026-10-05:* an old worker stack overflow (the
  `direct_assembly_elf` sample exited with status 134 under `--workers 4` and
  `--workers 1`, before the collector and resumable WHNF) did not reproduce
  in 40 release runs with 1, 2, and 4 workers.
