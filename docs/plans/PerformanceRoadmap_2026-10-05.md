# Performance Roadmap — 2026-10-05

Status: agreed with the maintainer on 2026-10-05, including the harness
questions; nothing started. This is
the umbrella for performance work. Each track gets its own plan when it
starts; the existing plans it names stay the detailed records.

## Purpose

Performance is poor enough that real assembly work cannot exercise the
system: the direct-assembly Hello World sample takes seconds even in a
release build. More is always better, but the first goal is to make ordinary
programs cheap enough to develop against.

The directions span the parser, the evaluator, interaction nets and the
runtime, so measurements must cross those boundaries. This roadmap fixes a
shared measurement design first, then orders the tracks.

## Principles

- **Measure first.** Each track starts from a recorded baseline and ends
  with the same measurement.
- **Exact counters, noisy timings.** Counters are deterministic for a fixed
  worker count and serve as regression oracles. Timings are trend data,
  recorded with machine details and compared with tolerances.
- **Budgets are scheduling heuristics, not meters.** Exact work counts come
  from profiling counters. A budget only decides when a poll yields (see
  [Budgets and Batching](#budgets-and-batching)).
- **Semantics do not move.** An optimization keeps results, failures and
  diagnostics identical. Differential checks against the unoptimized path,
  or an independent reference, guard each track.

## Measurement

### Profiling builds

- **An explicit profiling build mode,** the cargo feature `glam-prof`. It
  compiles counters and phase timers in, and ordinary builds pay nothing.
  It absorbs today's `interaction-net-profiling` feature.
- **A production-representative binary.** Test binaries carry the
  collector's deterministic hooks, so their timings do not represent
  production. Measurements use a release binary built with `glam-prof`.
- **A JSON report.** A `glam-prof` binary prints the counters and timers as
  JSON on request.

### What to count

- **Reductions by kind.** These are net rules, WHNF delegations, builtin
  steps and reflection steps: the units of
  `reduction-costs-one-budget-unit`. Counting them is a semantic measure of
  work that survives representation changes, so time per reduction becomes
  the efficiency metric.
- **Boundary crossings:**
  - access-region entries;
  - root registrations;
  - managed allocations by family;
  - coordinator transitions;
  - net lock acquisitions;
  - checkpoint publications.

  Crossings per reduction is what structural work and batching should drive
  down.
- **Phase timers:** lexing and parsing, lowering, evaluation, net reduction,
  collection, and settlement.

### Profiling suite

A suite separate from the correctness tests: workloads built to be measured
rather than to pass or fail. It lives wherever is most convenient, provided
normal builds exclude it.

- Workloads mix generated and hand-written sources during development. A
  workload worth keeping goes into a profiling-only test or a samples
  file.
- Baselines live in the track's plan or in a performance review. They are
  working data and need no long-term retention.

- Each workload records its counters exactly and its timings as trends.
- A counter change is a reviewed baseline update, never noise.
- **The proximal real workload** is direct-assembly Hello World.
- **Each track adds microbenchmarks:**
  - deep nesting for the parser;
  - the recursion countdown at several depths;
  - list map and fold, and dict build and lookup, at 10⁴–10⁶ elements;
  - net-heavy programs;
  - long effect chains.

## Budgets and Batching

Maintainer decision, 2026-10-05. Machine-state batching already happens:
the regional WHNF driver runs many delegations inside one access region. The
batching still to come is for interaction nets, which claim several pairs
matching a known pattern and reduce them in one step.

Charging a batch strictly per reduction would force a poll to tip-toe
through single steps near the end of its budget, to avoid crossing it.
Instead:
- a batch is admitted while at least one unit remains;
- it spends its reductions and may finish past the budget, and the overrun
  is forgiven;
- exact reduction counts come from the profiling counters, not from the
  budget.

`EvaluationStepBudget::consume` currently refuses to overspend. The first
batch adds a saturating charge, and `reduction-costs-one-budget-unit`
gains this consequence when it lands.

## Tracks, in Order

1. **Profiling harness.** The build mode, counters, report, suite, and a
   recorded baseline for Hello World and the first microbenchmarks.
   Everything after this measures against it.
2. **Collection during foreground work.** *Done 2026-10-05, ahead of the
   harness at the maintainer's request.* Whoever polls claimed work
   collects between the poll and the release when the collector's pressure
   latch is set (revised
   `noauto-runtime-collection-policy`). CLI memory figures now reflect live
   memory.
3. **Parser.**
   - Apply the constant-time lookahead fix from
     [Parser Backtracking Performance](ParserBacktrackingPerformance_2026-10-04.md);
     today parse time is exponential in nesting depth.
   - Replace Rust-stack recursion with an explicit-stack parse over the
     lexer's delimiter groups (maintainer preference over a nesting limit).
     This closes the parser exception in
     `no-semantic-recursion-on-rust-stack`.
   - This track is small, independent, and validates the harness.
4. **Evaluation recursion cost.** Diagnose
   [Evaluation Recursion Performance](EvaluationRecursionPerformance_2026-10-04.md)
   before any representation work: a simple countdown costs tens of
   milliseconds per call and grows roughly quadratically, which would swamp
   every other measurement.
5. **Structural overheads.** The holistic pre-performance review's P2:
   scheduler round trips, the allocation and rooting path, the reflection
   branch clone, and obvious algorithmic defects. These would otherwise mask
   representation measurements.
6. **Representations, two parallel tracks:**
   - **Values:**
     [Value Representation Refinement](ValueRepresentationRefinement_2026-08-19.md),
     with special focus on lists and dicts and on list processing:
     - contiguous chunks for strict lists;
     - ropes for concatenation;
     - small inline dicts and a persistent map for large ones;
     - shared key shapes where keys are static, as in modules and objects.
   - **Interaction nets:**
     - a slab of nodes with free-slot recycling, in place of hash maps;
     - nodes as four 32-bit words (kind and three typed links) with
       payloads in side tables;
     - the GAL adaptation, whose fixed-size levels replace growing fan
       histories. The compact node needs those levels, so GAL comes first
       or alongside. It also takes on the deferred positive-erasure
       translation.
7. **Normal forms and batching:**
   - **Normal forms at construction.** These make hashing and memoization
     possible later, and simplify bulk materialization and multi-step
     evaluation without explicit materialization. Pure nets can diverge,
     so construction-time normalization runs under fuel or only for rule
     patterns known to terminate.
   - **Cursor materialization batching.** Copy several nodes per lock
     round trip when a lightweight analysis shows a source region is inert.
   - **Net pattern batching.** Claim several pairs that match a known
     pattern and reduce them in one step, under the budget rule above.
   - **Effect chains:**
     [Pure Effect Access Fusion](PureEffectAccessFusion_2026-09-23.md).

**Deferred:** JIT compilation. There is much to gain without it.

## Settled Harness Questions

Maintainer answers, 2026-10-05:
- **Feature name:** `glam-prof`.
- **Report format:** JSON.
- **Workloads:** a mix during development. Keepers go into profiling-only
  tests or samples files.
- **Baselines:** in the track's plan or a performance review, not kept long
  term.
- **Suite location:** wherever is convenient, excluded from normal builds.
- **Collection during CLI assembly:** required, and done; see track 2.
