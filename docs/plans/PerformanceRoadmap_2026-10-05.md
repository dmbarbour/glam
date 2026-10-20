# Performance Roadmap — 2026-10-05

Status: agreed with the maintainer on 2026-10-05, including the harness
questions. `perf-profiling-harness` and `perf-foreground-collection` are
done, and `perf-evaluation-recursion` is done (2026-10-08). `perf-structural-overheads` closed on 2026-10-15
(maintainer); its reflection steps continue as `perf-reflection-effects`
([plan](ReflectionPerformance_2026-10-15.md)), and the rest became
[standalone steps](#standalone-steps). `perf-parser` closed on 2026-10-17
(maintainer). Next, in the maintainer's order (2026-10-15, 2026-10-17): the
representations, then `perf-net-lowering`, then `perf-reflection-effects`,
since representation costs now bury other signals. The latest measurements are in
[Snapshot 2026-10-15](#snapshot-2026-10-15). This is the umbrella for
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
3. **Parser** (`perf-parser`). *Done 2026-10-17* (maintainer);
   [Parser Backtracking Performance](ParserBacktrackingPerformance_2026-10-04.md)
   holds the record.
   - A prefix-shared term parser with an explicit stack of frames reads
     every expression and pattern, parsing each group once; Chumsky reads
     only declaration heads. The exponentials in nesting are gone: every
     parse family is linear.
   - Local analysis is one walk with a scope stack, and file `let` names
     are read through the resolver (`parser-scope-analysis`):
     `chain_where` grows as n^1.06, and file `let`s, once cubic, as
     n^1.17.
   - Parsing is about 1% of `hello_elf` (2026-10-17), so its constants were
     not pursued: allocation parsing nested forms, expression mode's term
     wrapping, and the lexer and number parsing. Net lowering's are
     `perf-net-lowering`. Open findings moved to the standalone steps
     `perf-nested-scopes` and `perf-pre-eval-stack-depth`.
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
   *Closed 2026-10-15* (maintainer).
   - *Done:* `perf-admission-wakeups`, `perf-idle-wakeups`,
     `perf-fast-id-hashing`, `perf-net-builder-wired-ports`,
     `perf-scaling-workloads`, `perf-list-front-walk`,
     `perf-list-leaf-walk`, `perf-access-region-cost`,
     `perf-worker-scaling`, `perf-collection-growth`,
     `gc-one-heap-per-thread`, `perf-quantum-region`,
     `gc-bounded-collection-wait`, `perf-allocation-path`,
     `perf-root-frames`, `perf-transient-root-avoidance`,
     `perf-list-map-growth`, `perf-list-literal-composition`,
     `perf-lowering-free-bindings`, `perf-interface-demand-walk`,
     `perf-interface-route-ownership`, `perf-reduced-source-copy`,
     `perf-driver-path-inlining`, `perf-module-definition-cost`,
     `perf-runtime-net-attach`, `perf-application-values` and
     `perf-definition-assertions` (2026-10-12). The scheduler round trips
     were `perf-evaluation-recursion`.
   - *Moved on closing:* `perf-reflection-step-cost` and
     `perf-reflection-scanner` to `perf-reflection-effects` (below);
     `perf-debug-annotations`, `perf-worker-route-walks`,
     `perf-root-check-kinds` and the open experiment
     `perf-coalesced-wakeups` to [standalone steps](#standalone-steps);
     `chain_where`'s name analysis to `parser-scope-analysis`.
   - *Experiments:* `perf-mimalloc-allocator` and `perf-poll-root-frames`
     (not adopted).
   - *Planned separately:* [Explicit Heap Holds](ExplicitHeapHolds_2026-10-09.md)
     separates holding a heap from accessing it. Pulled ahead of the
     leftovers of `perf-reflection-step-cost`; done 2026-10-15.

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
       the interface routes evaluations keep rely on this
       (`interface-routes-belong-to-evaluation`);
     - nodes as four 32-bit words (kind and three typed links) with
       payloads in side tables;
     - the GAL adaptation, whose fixed-size levels replace growing fan
       histories. The compact node needs those levels, so GAL comes first
       or alongside. It also takes on the deferred positive-erasure
       translation.
7. **Net lowering** (`perf-net-lowering`), after the representations and
   before the evaluation steps that follow (maintainer, 2026-10-17, as
   intuition). Lowering resolved expressions to interaction nets is 30.5%
   of `hello_elf` (183 M of 599 M instructions, 2026-10-17): finishing
   nets (`NetBuilder::try_finish`) 118 M, of which the polarity check
   (`Topology::check`) 52 M, and list trees 74 M. Builder and finish costs
   depend on the net representation, hence the order. Whether the polarity
   check runs on every net is the maintainer's call: it guards an
   invariant (the [polarity plan](NetPolarityChecker_2026-10-05.md)).
8. **Reflection effects** (`perf-reflection-effects`).
   [Reflection Performance](ReflectionPerformance_2026-10-15.md), split from
   the structural overheads plan on 2026-10-15: `perf-reflection-step-cost`
   (a `.r ()` step is 376 K instructions, mostly net reduction, allocation
   and lazy routes) and `perf-reflection-scanner` (on the order of 9 M of
   `minimal`'s 24 M). It follows the representations (maintainer,
   2026-10-15): much of a step's cost is now representation, which buries
   any other signal.
9. **Normal forms and batching**, as separate steps:
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

### Standalone steps

Small steps without a plan of their own, moved here when the structural
overheads plan closed (2026-10-15) and the parser plan closed (2026-10-17).
Their sections hold their earlier evidence.

- **Stack depth before evaluation** (`perf-pre-eval-stack-depth`). A
  deep syntax tree overflows the Rust stack when dropped: a 100k-term flat
  chain, for example. Expected (maintainer, 2026-10-06): every stage
  before evaluation needs an audit for recursion on user-controlled depth.
  That covers the syntax tree and its drop, name analysis, local analysis,
  resolution, and lowering. Known instance: lowering's
  `collect_free_bindings` recurses to a dictionary literal's depth (see
  `perf-lowering-free-bindings`). It owns the deep-nesting remedy that
  `no-semantic-recursion-on-rust-stack` first gave the panic-safety plan
  (maintainer, 2026-10-15). The term parser itself is iterative.
- **Nested scopes in the front end** (`perf-nested-scopes`), not scheduled
  (maintainer, 2026-10-17: know the behavior; not a priority). Objects
  nested n deep grow as n^1.31 over 50/100/200, an `^x` at each level as
  n^1.34, and one deep `^…^x` as n^1.20, with reductions linear: the
  resolver copies the scope chain for each body and each escape step
  (`NameScope::duplicate_in`), and the parser reads nested `with` bodies
  again at each level. The resolver also scans the visible locals per
  name lookup and per binding group, O(L) each; nested `let`s grow as
  n^1.17. The parser plan's "Scope analysis 2026-10-17" has the evidence.

- **Root checks by kind** (`perf-root-check-kinds`), after the
  representation steps (maintainer, 2026-10-15). A root made from a view
  registers without validation (`root-evidence`), about 82 instructions in
  a pooled slot (`pooled-roots`); a bare `Gc` is validated. Inner roots,
  made by the runtime from values it holds and never crossing runtimes,
  might skip more, even registration. Root registration was 3% of a
  `.r ()` step on 2026-10-15, but its share should rise as the
  representation steps shrink the rest; measure then.
- **Shared runs for no-drop types** (`perf-shared-no-drop-runs`), a
  tentative idea, not planned (maintainer, 2026-10-20). Each managed Rust
  type is an allocation class of its own, with its own 64 KiB runs, so
  each type in use holds at least one partly filled run on every thread
  that allocates it, and costs a class discovery and one of the 64 hot
  class slots in each thread's cache. Ephemerons raised it: each
  `Ephemeron<K, V>` instantiation is a class (`api-ephemerons`). No-drop
  types of one slot size could share runs, with a type per slot, in a word
  or a side table, for tracing's dispatch and for validation, which must
  stay exact: checked roots and debug access compare a slot's type with
  the type asked for, and safe code may keep a stale `Gc` whose slot
  another type reuses. Measure first: glam allocates about fifteen managed
  types, and fragmentation has shown in no profile yet.
- **The volume table as a managed map** (`perf-managed-volume-table`), to
  revisit after the representation steps (maintainer, 2026-10-20). The
  reflection store maps each volume to a root on an ephemeron per
  committed version (`api-volume-cells`), in a Rust persistent map. Once
  `perf-value-representation` provides managed persistent maps, the table
  could be one, behind one root per snapshot, or the store's state a
  value. Not hot: one entry per volume, and a commit touches few.
- **A module's fixed cost** (`perf-module-fixed-cost`), after the
  representation steps (maintainer, 2026-10-18), and not ahead of the
  runtime API plan unless it gets in the way. Compiling a module costs
  about 10 M instructions beyond its text: the CLI's prelude
  (`api-cli-prelude`) added 16.7 M per run, `hello_elf` going from 599.5
  to 616.2 M and a run without configuration from 23.8 to 40.4 M, while
  the same text compiled within a user's script costs about 6 M. The rest
  is another session, its tasks and their net work. Every import pays it.
- **Waking a failed cut** (`perf-fine-grained-wakes`), not measured. A cut
  that fails after observing shared state awaits a change in what it
  observed (`turn-cuts-bound-transactions-and-search`). As `api-cuts`
  built it, any commit in the runtime wakes every such waiter, which
  validates its reads and sleeps again if they stand, so each commit costs
  a validation per waiter. A wake indexed by what each waiter observed
  keeps the contract. Measure once machines wait in numbers, from
  `api-requests`. A cut whose evaluation waited on what was not known
  yet, such as an unresolved promise, wakes on any change of the
  observation epoch and evaluates again (`api-machine-environment`); a wake
  on the evaluation's own dependency keeps that contract too.
- **Port routing** (`perf-port-routing`), not measured, after the
  representations, as the runtime API plan goes ahead of performance
  (maintainer, 2026-10-19). As `api-ports` built them, each write walks
  the wiring from the writer's output, through its parent's and its own
  wires and the pass-throughs on the way, every time; any wiring change
  conflicts with every transaction that routed (one revision); `pending`
  scans all buffers; and buffers are never removed. Routes could be
  cached by wiring revision and collapsed through switchboards, wiring
  observed per owner, and buffers indexed by machine. Measure once ports
  carry the CLI's diagnostics.
- **Debug annotations** (`perf-debug-annotations`). Each module definition
  carries `anno {context: {g: {origin, line, definition}}} value` for
  diagnostics; building without it saved 8 to 10% of the `chain` family
  (2026-10-10). Debugging annotations should be far cheaper (maintainer,
  2026-10-10, open to investigation). Starting points: what evaluating one
  costs, and whether a context known at compile time, as an origin and
  line are, can travel with its definition without an evaluation step.
- **Route walks with workers** (`perf-worker-route-walks`). With four
  workers, `chain_800` runs within 1% of the foreground's instructions but
  about 1.6 times its CPU (Snapshot 2026-10-15; 2.3 times on 2026-10-09).
  A worker finds work by walking each reflection root's exact producer
  chain from the top on every claim (`exact_producer_probe_locked`), which
  the counters do not see; it could keep a route as the foreground does.
  A worker's change in the middle of the foreground's route makes
  validation fall back to a complete search, where the route could shrink
  to its proven frames. Workers are opt-in, so this comes last.
- **Coalesced wake-ups** (`perf-coalesced-wakeups`), an open experiment
  (maintainer's suggestion). Coordinator mutations set a notify flag
  flushed once per quantum, trading up to a quantum of latency for fewer
  wakes of parked threads; a notifying thread flushes before it parks or
  blocks. It matters only with threads parked, so it starts from a profile
  with workers.

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

## Snapshot 2026-10-11

At `e6e07f2d`, after `perf-parser`, `perf-evaluation-recursion` and most of
`perf-structural-overheads`, through `perf-compiled-nets`. Same machine and
toolchain as the baseline: Intel Core i7-6700 (8 threads), rustc 1.99.0,
release builds with `glam-prof`, zero workers. The baseline (`7962639e`,
the harness on `b89c2d78`) and the structural overheads baseline
(`84bfd7d2`, 2026-10-08) were rebuilt and rerun the same day on the
baseline's own workload sources, so the columns compare directly; the
samples have not changed since. Instructions are user space and, with CPU
time, the mean of three runs; counters are exact.

| Workload | M instr 10-06 | 10-08 | Now | Change | CPU ms 10-06 | 10-08 | Now |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `minimal` | 63.5 | 34.4 | 23.6 | −63% | 42.9 | 21.6 | 18.8 |
| `hello_do` | 119.8 | 60.6 | 33.8 | −72% | 68.0 | 27.1 | 20.9 |
| `hello_elf` | 9,645 | 3,493 | 1,305 | −86% | 3,499 | 736 | 293 |
| `parse_parens_10` | 3,565 | 35.0 | 24.1 | −99.3% | 456 | 21.5 | 19.0 |
| `parse_lists_16` | 5,194 | 35.5 | 24.2 | −99.5% | 668 | 21.5 | 24.0 |
| `countdown_100` | 1,685 | 323 | 114 | −93% | 553 | 86.7 | 40.6 |
| `countdown_200` | 5,127 | 633 | 198 | −96% | 1,440 | 154 | 61.8 |
| `countdown_400` | 17,374 | 1,190 | 392 | −97.7% | 4,475 | 265 | 102 |
| `list_map_1000` | 3,043 | 1,283 | 94.1 | −96.9% | 815 | 260 | 33.6 |
| `dict_lookup_1000` | 4,848 | 480 | 47.2 | −99.0% | 1,124 | 104 | 22.9 |

Counters, baseline to now:

| Workload | Reductions | Access regions | Root registrations | Allocations | Collections |
| --- | ---: | ---: | ---: | ---: | ---: |
| `minimal` | 2,242 → 1,596 | 6,515 → 479 | 2,052 → 1,132 | 2,500 → 1,662 | 0 → 0 |
| `hello_elf` | 303,303 → 137,796 | 846,986 → 5,883 | 200,261 → 61,428 | 222,644 → 92,605 | 2 → 1 |
| `countdown_400` | 119,743 → 58,244 | 419,436 → 1,695 | 87,802 → 27,930 | 104,861 → 50,592 | 1 → 1 |
| `list_map_1000` | 47,589 → 15,038 | 180,187 → 825 | 23,295 → 4,415 | 29,003 → 9,039 | 0 → 0 |
| `dict_lookup_1000` | 27,001 → 1,865 | 123,067 → 486 | 26,748 → 1,291 | 33,347 → 1,897 | 0 → 0 |

Growth at the sizes of the structural overheads plan's
[Scaling 2026-10-08](StructuralOverheads_2026-10-08.md#scaling-2026-10-08)
(at `64ba1c00`): the exponent k of instructions, with cost ≈ a + b·n^k,
and the marginal instructions per unit of size between the two largest
sizes.

| Family | Sizes | k then | k now | K instr per unit then | Now |
| --- | --- | ---: | ---: | ---: | ---: |
| `countdown` | 400/800/1,600 | 0.98 | 1.06 | 2,925 | 960 |
| `sum` | 400/800/1,600 | 1.04 | 0.96 | 3,412 | 1,012 |
| `chain` (module definitions) | 400/800/1,600 | 1.24 | 0.94 | 8,913 | 1,795 |
| `do_chain` | 400/800/1,600 | 1.62 | 1.00 | 4,556 | 994 |
| `dict_lookup` | 1,000/2,000/4,000 | 1.48 | 1.07 | 919 | 24 |
| `list_map` | 1,000/2,000/4,000 | 1.91 | 1.23 | 5,219 | 78 |
| `list_sum` | 400/800/1,600 then, 200/400/800 now | 2.95 | 1.01 | 294,957 | 1,144 |
| `parse_lists` (depth) | 400/800/1,600 then, 200/400/800 now | 1.48 | 0.91 | 77 | 6 |
| `parse_ifs` (depth) | 4/8/16 | 7.71 | 7.78 | 173,584 | 172,670 |

The default suite now has 60 workloads and costs 25.1 G instructions and
6.2 s of CPU; `parse_ifs_16` alone is 1.4 G.

**Observations**
- **Hello World** takes 12 times less CPU than at the baseline (3.5 s to
  0.29 s). A reduction costs about 9,500 instructions and 2.1 µs, from
  31,800 and 11.5 µs, and there are 55% fewer of them: whole copies,
  applications built directly, lazy locals passed as they are, and
  compiled nets replace net rules.
- **Boundary crossings** nearly vanished. Access regions fell 99% (one
  heap region per evaluation quantum), root registrations 69%
  and allocations 58% in `hello_elf`, which now collects once.
- **Fixed cost.** An empty assembly takes 23.6 M instructions and 19 ms of
  CPU, from 63.5 M and 43 ms.
- **Parser.** Nesting parentheses or brackets costs 3 to 6 K
  instructions per level beyond `minimal`. `parse_ifs`, a keyword form in parentheses inside another, is
  still exponential (`parser-keyword-frames`).
- **Recursion** is linear and costs about 0.96 M instructions and 140
  reductions per level, from 61 M (and growing) and 290.
- **Lists and dicts.** A dictionary literal resolves during lowering, so
  lookups no longer reduce at run time. Mapping a list costs 78 K
  instructions per item, from 5.2 M (at 10-08, sizes 2,000 to 4,000).
  Folding one from the front is linear, 1.1 M per item between 400 and 800
  items; at 10-08 it grew with k = 2.95, 295 M per item between 800 and
  1,600.
- **Still superlinear at small sizes:** `chain` and `chain_where` at 200
  to 800 definitions read k = 1.26 and 1.44, though every counter is
  linear and `chain` reads 0.94 at twice the sizes. `dict_computed`'s
  access regions and root registrations grow with k = 1.78 and 1.45. With
  four workers, `chain_w4_800` takes 42% more CPU than `chain_800` for the
  same instructions (`perf-worker-route-walks`).

## Snapshot 2026-10-15

At `ce7d3657`, after the rest of `perf-application-values`,
`perf-definition-assertions`, explicit heap holds, lazy diagnostics and
the allocator class cache. Same machine and toolchain as the earlier
snapshots (Intel Core i7-6700, rustc 1.99.0), the same workload sources,
zero workers, release builds with `glam-prof`, and the mean of three runs.
The 2026-10-11 binary (`e6e07f2d`) was rerun the same day: its
instruction counts match that snapshot exactly, and its CPU times differ
by up to 18%, mostly higher, so the CPU columns compare against that
rerun.

| Workload | M instr 10-06 | 10-11 | Now | Since 10-11 | Since 10-06 | CPU ms 10-11 (rerun) | Now |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `minimal` | 63.5 | 23.6 | 24.2 | +2.5% | −62% | 22.1 | 22.3 |
| `hello_do` | 119.8 | 33.8 | 30.9 | −8.6% | −74% | 21.9 | 21.2 |
| `hello_elf` | 9,645 | 1,305 | 597 | −54% | −93.8% | 322 | 156 |
| `parse_parens_10` | 3,565 | 24.2 | 24.5 | +1.2% | −99.3% | 20.4 | 20.2 |
| `parse_lists_16` | 5,194 | 24.2 | 24.6 | +1.7% | −99.5% | 20.5 | 20.7 |
| `countdown_100` | 1,685 | 113.6 | 46.0 | −60% | −97.3% | 42.8 | 25.2 |
| `countdown_200` | 5,127 | 198.0 | 65.1 | −67% | −98.7% | 65.4 | 31.5 |
| `countdown_400` | 17,374 | 391.7 | 103.5 | −74% | −99.4% | 108.5 | 39.5 |
| `list_map_1000` | 3,043 | 94.2 | 71.3 | −24% | −97.7% | 33.8 | 29.5 |
| `dict_lookup_1000` | 4,848 | 47.2 | 42.8 | −9.3% | −99.1% | 25.5 | 23.3 |

Counters, 10-11 to now (outer heap regions replace access regions):

| Workload | Reductions | Heap regions | Root registrations | Allocations | Collections |
| --- | ---: | ---: | ---: | ---: | ---: |
| `minimal` | 1,596 → 2,213 | 479 → 423 | 1,134 → 1,640 | 1,664 → 2,417 | 0 → 0 |
| `hello_elf` | 137,796 → 76,608 | 5,883 → 1,001 | 61,430 → 37,639 | 92,607 → 68,666 | 1 → 1 |
| `countdown_400` | 58,244 → 24,108 | 1,695 → 423 | 27,932 → 14,000 | 50,594 → 29,547 | 1 → 0 |
| `list_map_1000` | 15,038 → 15,508 | 825 → 424 | 4,417 → 6,313 | 9,041 → 9,670 | 0 → 0 |
| `dict_lookup_1000` | 1,865 → 2,349 | 486 → 423 | 1,293 → 1,729 | 1,899 → 2,550 | 0 → 0 |

Growth at twice the default sizes (`GLAM_PROFILE_SCALE=2`), as in the
2026-10-11 snapshot, with `parse_ifs` and `list_sum` at their default
sizes:

| Family | Sizes | k 10-11 | k now | K instr per unit 10-11 | Now |
| --- | --- | ---: | ---: | ---: | ---: |
| `countdown` | 400/800/1,600 | 1.06 | 1.06 | 960 | 255 |
| `sum` | 400/800/1,600 | 0.96 | 1.31 | 1,012 | 312 |
| `chain` (module definitions) | 400/800/1,600 | 0.94 | 0.84 | 1,795 | 578 |
| `do_chain` | 400/800/1,600 | 1.00 | 0.81 | 994 | 503 |
| `dict_lookup` | 1,000/2,000/4,000 | 1.07 | 1.08 | 24 | 20 |
| `list_map` | 1,000/2,000/4,000 | 1.23 | 1.01 | 78 | 45 |
| `list_sum` | 200/400/800 | 1.01 | 0.55 | 1,144 | 296 |
| `parse_lists` (depth) | 200/400/800 | 0.91 | 0.92 | 6 | 6 |
| `parse_ifs` (depth) | 4/8/16 | 7.78 | 7.77 | 172,670 | 175,701 |
| `append_walk` | 400/800/1,600 | | 1.89 | | 1,626 |
| `chain_where` | 400/800/1,600 | 1.44 at 200–800 | 1.76 | | 1,193 |

The default suite's 60 workloads cost 13.07 G instructions and 3.43 s of
CPU, from 25.09 G and 6.16 s; `parse_ifs_16` alone is 1.44 G (11%).

**Observations**
- **Hello World** takes 597 M instructions and 156 ms of CPU: half of
  10-11, and a sixteenth of the 10-06 instructions (22 times less CPU).
- **Recursion** costs about 255 K instructions and 54 reductions per
  level, from 960 K and 140.
- **Heap regions** are about 423 a run in every small workload, whatever
  its size: drivers hold one mutator across their quanta
  (`driver-mutator`), so entries no longer scale with work. About 175 of
  `minimal`'s entries are public `Values` calls, most of them the CLI's
  environment (`gc-hold-values-api`, closed).
- **Fixed cost** grew 2.5%: `minimal` makes more reductions, roots and
  allocations than at 10-11, from the reflection-state check and journals
  (`defs-refl-state`, `defs-journal-flat`). The scanner and that check are
  on the order of 9 M of its 24.2 M (`perf-reflection-scanner`).
- **A reflection step** (`.r ()`) costs 376 K instructions, from 519 K on
  2026-10-12, now mostly net reduction, allocation and lazy routes (see
  `perf-reflection-step-cost`).
- **Superlinear:**
  - `parse_ifs` is still exponential (`parser-keyword-frames`).
  - `append_walk` (k = 1.89): collections double with size (1, 2, 4 at
    400 to 1,600), each tracing live list structure, while every counter
    is linear. Marking shared `Arc` spines once per path is
    `perf-value-representation`'s ("no `Arc` in basic data types").
  - `chain_where` (k = 1.76, no collections): the front end. At 1,600,
    about 520 M of 1,411 M instructions exceed linear, in the structural
    parser's top-level token scans (245 M; `parser-keyword-frames`), name
    analysis's `mark_used_locals` and `NestedLocals::bind` over n nested
    `where` scopes (160 M), and name comparisons (71 M). No step owns the
    name-analysis part.
  - `sum` (k = 1.31): its collections (1, 1, 2), not its counters.
- **Workers:** `chain_w4` runs within 1% of `chain`'s instructions (8%
  more on 2026-10-09), but about 1.6 times its CPU at 800 and 1,600
  (single runs; `perf-worker-route-walks`).

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
