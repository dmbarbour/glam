# Resumable WHNF W9 Review — 2026-09-28

Status: closed. W9A-E are complete, the selected proof boundary is retained,
and W6G4R-003 is resolved.

## Scope and method

W9 separated broad scheduler/readiness movement from mutations which can
invalidate a retained exact producer route. It then applied a bounded
notification disposition without changing the shared condition-variable
architecture. This review checks the implementation against the Phase W9
proof boundary, forced ordering matrix, source-shaped duplicate-symbol
fixture, and W6G4R-002 performance baseline.

The source measurement used the same no-worker direct-assembly configuration,
source text, release Callgrind method, native debug/release timing method, and
static interaction-net counters as W6G4R-002. Native time is corroborating
evidence only. Concurrency correctness comes from latched orderings, not test
repetition.

## Semantic and synchronization accounting

The implementation preserves the intended authority boundary:

- `work_generation` remains the broad scheduler/readiness revision;
- `exact_route_hazard_revision` changes only for transitions which may alter
  the retained target-to-tip projection;
- neutral movement is accepted in O(1);
- hazard movement validates retained frames while coordinator state is
  protected; and
- only a concrete target, identity, epoch, dependency, projection, state,
  retirement, branch, or guarded-release mismatch causes cold invalidation.

Neither revision is semantic state or public task status. The client-local
route remains a hint: authoritative work records, subscription epochs, and
completion sources decide every guarded validation. Machine polling still
occurs outside coordinator locks and mutation admission, and notifications and
destruction remain outside protected publication.

Forced tests cover poll-owned and externally admitted neutral movement,
successful validation after unrelated hazard movement, ancestor cancellation
and retirement, observation wake, dependency replacement, all three work
families, terminal return, guarded-release interference, independent
retirement, revision wrap, and each suppressed notification class. These tests
force the relevant orderings with barriers or latches.

## Source-fixture results

### Exact route

| Counter | W6G4R-002 | W9 |
| --- | ---: | ---: |
| fast handoffs | 19,499 | 28,855 |
| complete searches | 9,374 | 18 |
| records visited | 1,418,995 | 18 |
| cold fallbacks | 9,356 | 0 |
| moved poll windows | 9,356 | 9,356 |
| O(1) accepted releases | n/a | 28,855 |
| hazard validations | n/a | 0 |

The retained representation still reaches the prior pathological depth of
roughly 573 and occupies roughly 42 KiB at that peak, plus one private `u64`
hazard observation. W9 changes reconciliation, not the route's asymptotic
storage.

Temporary factual attribution explained every former invalidation. The 9,356
moved windows contained 9,356 fresh admissions, 50 activations, and 45
task-promise admissions made synchronously by the claimed work; there were no
external mutations. All are neutral for the retained route and now take the
O(1) path. Forced tests, rather than this benign source fixture, exercise
hazard validation and rejection.

### Notifications

| Counter | W9A baseline | W9 |
| --- | ---: | ---: |
| `notify_all` calls | 88,579 | 58,798 |
| `notify_one` calls | 0 | 0 |

The reduction is 29,781 broadcasts (33.62%): 16 demand-session registrations,
45 task-promise admissions, and 29,720 work claims. These publications still
advance `work_generation`; only their incapable host wake is suppressed. The
zero-worker batch fixture has no parked host waiter, so its waiter-outcome
counts remain zero. Latched mixed worker/client, exact-completion, lifecycle,
and suppressed-publication fixtures establish wake correctness.

### Deterministic work and native corroboration

Callgrind measured 2,562,993,262 instructions, down 776,488,632 (23.25%) from
the 3,339,481,894 W6G4R-002 baseline. Warm native runs measured 12.93–12.98
seconds in debug and 0.93–0.94 seconds in release, versus 14.18–14.34 and
1.11–1.14 seconds respectively at W6G4R-002.

The interaction-net signature is unchanged: 12,356 bind joins, 519 fan/data
reductions, 3,683 calls, 9,926 operator calls, 15,214 cursor materializations,
and 6,215 cursor joins, with every other reduction counter zero. The driver
still records 3,670 machine polls, 154,573 work items, 47,900 interface polls,
35,326 cursor steps, 49,388 active-pair steps, and 21,959 cursor dependencies;
all retry, contention, disturbance, restart, and checkpoint counters are zero.

The fixture still exits with the structured duplicate-symbol diagnostic and
the same `asm.result`, source-definition, and binary-extraction contexts.

## Drift and cleanup

W7-W8 did not invalidate the W9 boundary. Their resumable state and managed
ownership changes affect what work records contain, not coordinator route
authority or guarded release ordering. W9 introduces no managed ownership
change, so aggressive-GC verification is unnecessary for this phase.

One useful profiling refinement remains: exact-route search, handoff,
fallback, reconciliation, and notification counters are now exposed under the
static `interaction-net-profiling` feature as well as tests. They impose no
ordinary-build observer. Decision-only poll-origin scopes, per-kind mutation
snapshots, mutation histograms, depth/disposition attribution, and their public
snapshot fields were removed after this measurement. The exhaustive factual
publisher boundary remains in code because it cheaply prevents uncategorized
coordinator mutations.

## Verification and disposition

The forced W9 route and notification suites, affected coordinator/evaluation
suites, interaction-net profiling script, formatting, all-target/all-feature
Clippy with warnings denied, and complete ordinary test suite pass. No
repetition-only result is used as race evidence.

The optimization materially reduces deterministic work, eliminates the
measured cold rediscovery, preserves every forced ordering and semantic
signature, and has a small stable implementation boundary. Keep it. No open
W9 finding remains; a future condition-variable split requires evidence from
a worker-enabled workload and is not implied by this zero-worker fixture.
