# Resumable WHNF Holistic Review — 2026-09-28

Implementation baseline: `8c611ae0`, after W9E closure.

Status: in progress. HR0 baseline and artifact mapping are complete. HR1-HR8
remain governed by
[`ResumableWhnfHolisticReviewPlan_2026-09-28.md`](../plans/ResumableWhnfHolisticReviewPlan_2026-09-28.md).
No finding is closed merely by this initial inventory.

## Scope

This review audits the complete implementation of
[`ResumableWhnfEvaluation_2026-09-12.md`](../plans/ResumableWhnfEvaluation_2026-09-12.md)
against its original purpose, selected architecture, twenty semantic/safety
invariants, verification matrix, and eleven completion criteria. It also
reconciles the result with:

- GCI11R-002D.2c-D.2h and 002E-H in
  [`GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md`](../plans/GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md);
  and
- P3-P5 in
  [`GarbageCollectorPersistentEdgeTraits_2026-09-12.md`](../plans/GarbageCollectorPersistentEdgeTraits_2026-09-12.md).

The dependency relation is deliberately asymmetric. WHNF can be complete while
the D.2c raw evaluator partition is closed, P3 remains open on downstream
carrier traits, and Gate G3 remains blocked by D.2d-D.2h plus fixture and
schedule migration. This review must not collapse those statements into one
generic notion of “GC complete.”

## HR0 — Baseline and artifact map

### Change boundary

The focused plan was introduced by `42b52f2c` and closed through `8c611ae0`.
Its implementation expanded in reviewed increments through W0-W9, including
the nested callable-WHNF and pure-net-construction plans and mandatory reviews
after the major phases. The phase-local record is extensive; this review uses
current source and current inventories as authority, with historical reviews
as explanations rather than proof of the present tree.

One documentation mismatch is already confirmed but not yet disposed:

- the WHNF plan's file-level status still says W9C.1-E are planned;
- its Phase W9 section, W9 completion record, and dated W9 review say W9 is
  complete.

HR1 will decide whether every plan-level completion criterion is satisfied
before the top-level status is corrected.

### Current implementation ownership

| Role | Current owner | Durable liveness and boundary |
| --- | --- | --- |
| canonical semantic progress | `eval/whnf.rs::WhnfState` and `WhnfContinuation` | Raw edges exist only regionally or beneath an exact traced owner. |
| callback-free working view | `RegionalWhnfWork` / `RegionalWhnfState` | Borrowed `EvaluationValueAccess`; cannot cross the region. |
| ordinary durable demand | `WhnfComputation` with `ManagedWhnfRoot` | One registered root to one `ManagedLazyCheckpointCell`; seed promotion atomically replaces the input root. |
| lazy-owned partial demand | `ManagedLazyCheckpointEdge` in the managed lazy | Traced semantic edge below the lazy; no root is stored in the value graph. |
| net-owned callable demand | `NetWhnfState` / `CallableCheckpoint` | Complete canonical state is traced by the managed core-net payload visitor. |
| checkpoint mutation | `ManagedWhnfAccess::with_state_transition` | One mutex and one collector-owned leaving/adding edge transition per bounded regional quantum. |
| evaluator access | `EvaluationPollContext` -> `EvaluatorStepContext` -> `EvaluationValueAccess` | Access exists only inside a higher-ranked callback-free poll region. |
| client result | `ClientDemandOperation` and sink | Coordinator machine owns progress; result leaves as a runtime root. |
| lazy result | lazy route plus lazy-owned checkpoint/cache | Only the lazy producer publishes the cache; coordinator route owns scheduling rather than semantic state. |
| promise result | promise follower and managed promise | Assignment/following remains distinct from lazy cache publication. |
| reflection result | coordinator-owned reflection machine plus managed completion promise | Reflection hosts one WHNF computation; it is not a value-graph checkpoint. |
| spark result | coordinator-owned spark adapter | Best-effort demand shares canonical producers and discards terminal WHNF. |
| net construction | pure construction runner, managed builder checkpoints, strict netlist replay | Effect processing is resumable; selected strict replay is callback-free inside access. |
| foreground scheduling | private client-demand handle and exact-demand zipper | Route stores scalar work/epoch/dependency facts only and remains non-authoritative. |
| background scheduling | reflection roots and sparks | Workers never claim foreground roots; background selectors follow exact causal producers. |

The primary implementation map is spread across `eval/whnf.rs`,
`eval/whnf/managed_state.rs`, `eval/lazy_checkpoint.rs`, the specialized
`eval/*_machine.rs` files, `evaluation/whnf.rs`, `evaluation/coordinator.rs`,
`evaluation/pump.rs`, `core_net.rs`, and the pure net-construction modules.
`docs/architecture/evaluation.md`, `docs/agent_context/evaluation.md`, and
`src/README.md` currently describe this same high-level shape; HR1-HR4 will
check their detailed claims rather than accepting this map by inspection.

### Current test and inventory ownership

| Evidence | Current location and role |
| --- | --- |
| suspension/replay and protocol phases | `eval/whnf/tests/`, `eval/value/tests/w4.rs`, and specialized evaluator-machine tests |
| lazy/promise/client/reflection/spark lifecycle | `evaluation/tests.rs`, coordinator submodule tests, and `eval/value.rs` ownership tests |
| stack and budget closure | `eval/tests/w7b.rs`, W7 budget/owner fixtures, and the W0B/W7 source census |
| callable-net checkpoint behavior | `eval/net.rs`, `eval/net/tests/nc5.rs`, `core_net.rs`, and interaction-net runtime tests |
| pure net construction | `eval/builtins/net/` tests and netlist source latch |
| exact-route concurrency and profiling | `evaluation/tests.rs`, `evaluation/coordinator/tests.rs`, static interaction-net profiling, and the W9 review |
| raw-value API closure | `core/managed/raw_value_api_inventory.rs` |
| managed access and root publication | `evaluation/access_inventory.rs`, `api/value/access_inventory.rs`, and focused checkpoint counters |
| durable WHNF construction | `eval/whnf_checkpoint_inventory.rs` and `eval/whnf_inventory.rs` |
| lazy-producer ownership | `eval/lazy_producer_inventory.rs` |
| persistent-edge traits | `core/managed/persistent_edge_trait_inventory.rs` plus collector pointer tests |
| recursive/durable/external owners | `core/managed/*_inventory.rs` families and Gate G2 records |

### Fresh parent-boundary evidence

The focused D.2c boundary passes both ordinarily and with aggressive
verification:

```text
cargo test -q --lib d2c
    1 passed
cargo test -q --features aggressive-gc-verification --lib d2c
    1 passed
```

The current raw-value API inventory contains 587 declarations/operations:

| Disposition | Count |
| --- | ---: |
| regional-access function | 298 |
| collector primitive function | 30 |
| violation function | 249 |
| regional-representation alias | 7 |
| violation derived trait | 3 |

The D.2c owner partition is exactly zero. The remaining 249 violations are
currently assigned as follows:

| Parent owner | Count |
| --- | ---: |
| D.2b core compatibility | 34 |
| D.2d orchestration | 11 |
| D.2e front end/compiler values | 137 |
| D.2f reflection | 25 |
| D.2g public/compiler/diagnostics | 45 |

This differs from some prose counts in the parent plan and will be reconciled
in HR6. The temporary raw-value facade
`EvalContext::evaluate_compatibility_whnf` remains deliberately classified as
a violation. Its implementation performs a project-to-root, runtime-owned
client demand, and root-to-raw projection. Current call sites include tests and
production orchestration/compiler/diagnostic helpers; HR6 must determine the
correct parent partitions rather than repeat the D.2c summary's shorthand that
the facade simply “belongs to D.2d.”

The current persistent-edge inventory also passes its nine focused tests:

```text
cargo test -q --lib persistent_edge
    9 passed
```

It now contains 884 occurrences: 200 production typed, 36 production erased,
634 test typed, and 14 test erased. Seventy-seven remain classified as defects:

| Cutover owner | Count |
| --- | ---: |
| P4 collector traits | 5 |
| P4 managed facades after parent closure | 13 |
| parent raw-value compatibility | 59 |

The P3 prose still records older milestones of 69 total and 51 parent defects.
The executable inventory's 77/59 split is current. HR7 must attribute the
eight added parent dependencies and decide whether they are justified WHNF/net
carrier interlocks, accidental trait reintroduction, or stale plan accounting.
Passing the inventory proves the additions are exactly classified; it does not
by itself prove that retaining them until P4 is desirable.

### Current ordinary and profiling baseline

The immediately preceding W9 closure established:

- formatting and all-target/all-feature Clippy pass;
- 1,875 ordinary library tests pass with two ignored, followed by every
  integration group;
- the named interaction-net profiling suite passes;
- the exact source fixture preserves its reduction/driver signature and
  structured diagnostic; and
- exact-route cold fallback and deterministic instruction work are materially
  reduced.

Those results are the review baseline, not a substitute for the focused
semantic and aggressive evidence required by HR1-HR8. No full aggressive
workspace run has yet been performed for this holistic review; the known W8
result still contains D.2d-D.2g, fixture, schedule, and settlement failures.

## Preliminary reconciliation questions

These are questions for the later passes, not findings yet:

1. Does the current aggregate managed checkpoint fully satisfy the original
   “exact state after every completed prefix” rule across every specialized
   machine, or do any phase owners still recreate child WHNF work?
2. Are all `evaluate_compatibility_whnf` production callers correctly owned by
   D.2d/D.2g, and can D.2d remove the facade without forcing D.2e-D.2g to
   duplicate orchestration?
3. Which eight persistent-edge defects were added after the 51-entry P3
   milestone, and are their traits inherent to current net/WHNF representations
   or transitional conveniences?
4. Does every stateful machine's edge visitor cover precisely the fields it can
   retain across a poll, including pure net construction and callable
   checkpoints?
5. Does the managed checkpoint mutex remain valid for the current STW
   collector while leaving a clear path to concurrent marking/root frames?
6. Which known aggressive-workspace failures are production boundaries versus
   test-fixture lifetime errors or schedule interference?
7. Is D.2d still the lowest-risk next implementation checkpoint, or did the
   WHNF transition expose a smaller compatibility-facade removal slice that
   should precede the rest of orchestration?

## Findings

Findings will be added beginning with HR1. The confirmed stale status/count
statements above remain evidence until their underlying completion and
ownership claims are audited; they are not yet assigned severity or resolution.
