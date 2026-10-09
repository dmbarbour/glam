# Performance Roadmap — 2026-10-05

Status: agreed with the maintainer on 2026-10-05, including the harness
questions. `perf-profiling-harness` and `perf-foreground-collection` are
done, `perf-parser` is in production, and `perf-evaluation-recursion` is
done (2026-10-08). `perf-structural-overheads` is active, with its own
[plan](StructuralOverheads_2026-10-08.md). This is the umbrella for
performance work. Each step gets its own plan when it starts; the existing
plans it names stay the detailed records. Steps are referred to by name,
never by number (see the plans README, "Step names").

## Purpose

Performance is poor enough that real assembly work cannot exercise the
system: the direct-assembly Hello World sample takes seconds even in a
release build. More is always better, but the first goal is to make ordinary
programs cheap enough to develop against.

The directions span the parser, the evaluator, interaction nets and the
runtime, so measurements must cross those boundaries. This roadmap fixes a
shared measurement design first, then orders the steps.

## Principles

- **Measure first.** Each step starts from a recorded baseline and ends
  with the same measurement.
- **Exact counters, noisy timings.** Counters are deterministic for a fixed
  worker count and serve as regression oracles. Timings are trend data,
  recorded with machine details and compared with tolerances.
- **Budgets are scheduling heuristics, not meters.** Exact work counts come
  from profiling counters. A budget only decides when a poll yields (see
  [Budgets and Batching](#budgets-and-batching)).
- **Semantics do not move.** An optimization keeps results, failures and
  diagnostics identical. Differential checks against the unoptimized path,
  or an independent reference, guard each step.

## Measurement

### Profiling builds

- **An explicit profiling build mode,** the cargo feature `glam-prof`. It
  compiles counters and phase timers in, and ordinary builds pay nothing.
  It absorbs today's `glam-prof` feature.
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
- Baselines live in the step's plan or in a performance review. They are
  working data and need no long-term retention.

- Each workload records its counters exactly and its timings as trends.
- A counter change is a reviewed baseline update, never noise.
- **The proximal real workload** is direct-assembly Hello World.
- **Each step adds microbenchmarks:**
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

## Steps, in Order

1. **Profiling harness** (`perf-profiling-harness`). *Done 2026-10-06; see
   [Baseline](#baseline-2026-10-06).*
   - The `glam-prof` feature replaces `interaction-net-profiling`.
   - Per-runtime counters cover reductions by kind, net rules and driver
     events, collector allocations, roots and access regions, and phase
     timers.
   - The binary writes its JSON report to `GLAM_PROF`, and
     `scripts/profile.sh` runs the workloads.
   - Usage is in `AgentContext.md` "Profiling".
   - Not yet counted: net lock acquisitions and checkpoint publications.
     Add them when a step needs them.
2. **Collection during foreground work** (`perf-foreground-collection`).
   *Done 2026-10-05, ahead of the
   harness at the maintainer's request.* Whoever polls claimed work
   collects between the poll and the release when the collector's pressure
   latch is set (revised
   `noauto-runtime-collection-policy`). CLI memory figures now reflect live
   memory.
3. **Parser** (`perf-parser`).
   - [Parser Backtracking Performance](ParserBacktrackingPerformance_2026-10-04.md)
     replaces the backtracking grammar with a prefix-shared,
     explicit-stack parser over the lexer's delimiter groups, with a cover
     IR for patterns and expressions. Today parse time is exponential in
     nesting depth. The lookahead-guard fix was rejected.
   - *Production since 2026-10-07:*
     - the term parser parses every ordinary expression;
     - keyword forms are delegated to the structural parsers;
     - the old grammar remains only as a fallback for invalid tokens, and
       as the differential oracle.

     Nested parens and lists now parse as fast as an empty program.
   - **Remaining**, as that plan's steps:
     - `parser-patterns`, which needs the cover IR below group level;
     - `parser-keyword-frames`, keyword forms as parser frames, which
       removes the structural layer's whole-file scans. It also removes
       an exponential found on 2026-10-08: a keyword form inside
       parentheses, nested in another, doubles parse time per level;
     - `parser-grammar-retirement`.
   - This closes the parser exception in
     `no-semantic-recursion-on-rust-stack`.
   - **Stack depth before evaluation** (`perf-pre-eval-stack-depth`). A
     deep syntax tree overflows the
     Rust stack when dropped: a 100k-term flat chain, for example.
     Expected (maintainer, 2026-10-06): every stage before evaluation needs
     an audit for recursion on user-controlled depth. That covers the
     syntax tree and its drop, name analysis, resolution, and lowering.
     Known instance: lowering's `collect_free_bindings` recurses to a
     dictionary literal's depth (see `perf-lowering-free-bindings`).
4. **Evaluation recursion cost** (`perf-evaluation-recursion`). Diagnose
   [Evaluation Recursion Performance](EvaluationRecursionPerformance_2026-10-04.md)
   before any representation work: a simple countdown costs tens of
   milliseconds per call and grows roughly quadratically, which would swamp
   every other measurement. `eval-recursion-route-validation` made it
   linear, and `eval-recursion-inline-forcing` (2026-10-07) removed most
   per-lazy routes: `countdown_400` takes 788 ms and `hello_elf` 1,874 ms.
   `eval-recursion-tail-forwarding` (2026-10-08) made tail recursion run in
   constant space: depth 10,000 peaks at 46 MB instead of 175 MB. The
   review that followed opened the next steps of
   `perf-structural-overheads`.
5. **Structural overheads** (`perf-structural-overheads`).
   [Structural Overheads](StructuralOverheads_2026-10-08.md) holds the
   steps, their evidence and the results. It started as the holistic
   pre-performance review's P2 (scheduler round trips, the allocation and
   rooting path, the reflection branch clone, and obvious algorithmic
   defects), which would otherwise mask representation measurements. The
   review after the recursion work (2026-10-08) added the open steps.
   - *Done:* `perf-admission-wakeups`, `perf-idle-wakeups`,
     `perf-fast-id-hashing`, `perf-net-builder-wired-ports`,
     `perf-scaling-workloads`, `perf-list-front-walk`,
     `perf-list-leaf-walk`, `perf-access-region-cost`,
     `perf-worker-scaling`, `perf-collection-growth` and
     `gc-one-heap-per-thread`. The scheduler round trips were
     `perf-evaluation-recursion`.
   - *Open, in order:* `gc-bounded-collection-wait`,
     `perf-quantum-region`,
     `perf-list-map-growth`,
     `perf-lowering-free-bindings`,
     `perf-interface-demand-walk`, `perf-module-definition-cost`,
     `perf-allocation-path`, `perf-runtime-net-attach`,
     `perf-reflection-step-cost` and `perf-worker-route-walks`.
   - *Experiments:* `perf-coalesced-wakeups` (open) and
     `perf-mimalloc-allocator` (not adopted).

6. **Representations**, two parallel steps:
   - **Values** (`perf-value-representation`):
     [Value Representation Refinement](ValueRepresentationRefinement_2026-08-19.md),
     with special focus on lists and dicts and on list processing:
     - contiguous chunks for strict lists;
     - ropes for concatenation;
     - small inline dicts and a persistent map for large ones;
     - shared key shapes where keys are static, as in modules and objects;
     - no `Arc` in basic data types (maintainer, 2026-10-09). Marking
       traces shared `Arc` list structure once per path, so
       `append_walk_3200` resolves each live slot 120 to 150 times per
       collection (`perf-collection-growth`); managed nodes would be
       marked once.
   - **Interaction nets** (`perf-net-representation`):
     - a slab of nodes with free-slot recycling, in place of hash maps.
       `RuntimeNet::reference` resolves every port through the `nodes`
       hash map: 3–5% of samples in every workload on 2026-10-08, from
       cursor claims, wiring, disconnection and frontier inspection, apart
       from `perf-interface-demand-walk`. NodeIds are referenced outside
       the graph and never reused, so slots need generations (holistic N3);
     - nodes as four 32-bit words (kind and three typed links) with
       payloads in side tables;
     - the GAL adaptation, whose fixed-size levels replace growing fan
       histories. The compact node needs those levels, so GAL comes first
       or alongside. It also takes on the deferred positive-erasure
       translation.
7. **Normal forms and batching**, as separate steps:
   - **Normal forms at construction** (`perf-normal-forms`). These make hashing and memoization
     possible later, and simplify bulk materialization and multi-step
     evaluation without explicit materialization. Pure nets can diverge,
     so construction-time normalization runs under fuel or only for rule
     patterns known to terminate.
   - **Cursor materialization batching** (`perf-cursor-batching`). Copy several nodes per lock
     round trip when a lightweight analysis shows a source region is inert.
   - **Net pattern batching** (`perf-net-pattern-batching`). Claim several pairs that match a known
     pattern and reduce them in one step, under the budget rule above.
   - **Effect chains** (`perf-effect-chain-fusion`):
     [Pure Effect Access Fusion](PureEffectAccessFusion_2026-09-23.md).

**Deferred:** JIT compilation. There is much to gain without it.

## Baseline 2026-10-06

Commit `b89c2d78` plus the harness. rustc 1.99.0, Intel Core i7-6700
(8 threads), release build with `glam-prof`, zero workers. Times are one run
each and are trend data; the counters reproduced exactly on a second run.

| Workload | Total ms | Reductions | Access regions | Root registrations | Allocations | Collections |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `minimal` | 36.7 | 2,242 | 6,515 | 2,050 | 2,498 | 0 |
| `hello_do` | 64.2 | 4,581 | 14,248 | 4,048 | 4,866 | 0 |
| `hello_elf` | 3,448.6 | 303,303 | 846,986 | 200,259 | 222,642 | 2 |
| `parse_parens_10` | 470.9 | 2,264 | 6,612 | 2,068 | 2,536 | 0 |
| `parse_lists_16` | 708.9 | 2,264 | 6,612 | 2,068 | 2,568 | 0 |
| `countdown_100` | 568.4 | 32,443 | 112,535 | 24,200 | 28,959 | 0 |
| `countdown_200` | 1,496.3 | 61,543 | 214,836 | 45,400 | 54,259 | 1 |
| `countdown_400` | 4,653.5 | 119,743 | 419,436 | 87,800 | 104,859 | 1 |
| `list_map_1000` | 830.3 | 47,589 | 180,187 | 23,293 | 29,001 | 0 |
| `dict_lookup_1000` | 1,148.0 | 27,001 | 123,067 | 26,746 | 33,345 | 0 |

Reductions add the runtime's evaluation reductions and net rules.

**Observations**
- **Fixed cost.** An empty assembly takes 37 ms and about 2,200 reductions.
- **Parser.** The nesting workloads spend almost all their time parsing
  (435 of 471 ms, and 671 of 709 ms) with the same reduction count as
  `minimal`. This is the exponential backtracking that `perf-parser` removes.
- **Recursion.** Each countdown level costs a constant ~290 reductions:
  - 120 WHNF delegations;
  - 43 builtin steps;
  - 5 immediate builtins;
  - 138 net rules.

  Time per level still grows: about 5.7, 7.5 and 11.6 ms at depths 100,
  200 and 400. So the superlinear cost behind `perf-evaluation-recursion` lies outside reduction
  work. Each level also issues ~360 coordinator `notify_all` calls.
- **Hello World.** It runs ~300k reductions in 3.4 s, about 11 µs each.
  Each reduction makes ~2.8 access-region entries, ~0.66 root registrations
  and ~0.86 coordinator `notify_all` calls. Net rules are 61% of the
  reductions, mostly cursor materializations and joins, bind joins and
  operator calls. These are the targets of `perf-structural-overheads` and the
  representation steps.
- **Collections.** CLI assembly now collects: two collections took 71 ms in
  `hello_elf`.
- **Lists and dicts.** They cost 47 and 27 reductions per element
  (lists mapped, dicts built from a literal), plus a parse-and-lower share
  of the time that grows with the literal.

## Snapshot 2026-10-07

After `perf-parser` (`parser-term-parser-first`, `parser-keyword-delegation`)
and `eval-recursion-route-validation`, on the same machine and build
settings as the baseline. Reduction counts are unchanged.

| Workload | Baseline ms | Now ms | Mostly from |
| --- | ---: | ---: | --- |
| `minimal` | 36.7 | 36.9 | — |
| `hello_do` | 64.2 | 62.4 | — |
| `hello_elf` | 3,448.6 | 3,029.0 | both steps |
| `parse_parens_10` | 470.9 | 37.6 | the parser |
| `parse_lists_16` | 708.9 | 36.9 | the parser |
| `parse_ifs_10` (new) | — | 46.1 | the parser |
| `countdown_100` | 568.4 | 366.2 | route validation |
| `countdown_200` | 1,496.3 | 816.8 | route validation |
| `countdown_400` | 4,653.5 | 1,363.8 | route validation |
| `list_map_1000` | 830.3 | 801.5 | — |
| `dict_lookup_1000` | 1,148.0 | 416.1 | the parser, on the 1,000-entry literal |

The countdown is now linear, at about 3.2 ms and 290 reductions per level,
or about 11 µs per reduction. That per-reduction cost is the next target
everywhere, including Hello World's 300k reductions.

## Settled Harness Questions

Maintainer answers, 2026-10-05:
- **Feature name:** `glam-prof`.
- **Report format:** JSON.
- **Workloads:** a mix during development. Keepers go into profiling-only
  tests or samples files.
- **Baselines:** in the step's plan or a performance review, not kept long
  term.
- **Suite location:** wherever is convenient, excluded from normal builds.
- **Collection during CLI assembly:** required, and done; see `perf-foreground-collection`.
