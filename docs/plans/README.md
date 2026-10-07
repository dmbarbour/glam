# Plans and Reviews

This directory and [`../reviews/`](../reviews/) hold history: implementation
plans and the reviews that checked them. They explain how the implementation
moved between designs. Current behavior is documented in the standing docs
(`docs/architecture/`, `docs/agent_context/`, `docs/AgentContext.md`, the
design docs, and the collector's `SAFETY.md` and `VERIFY.md`), and those win
when they disagree with a plan. The reasons behind significant decisions live
in [`../Decisions.md`](../Decisions.md).

**Retention rule.** A plan or review is deleted, with git as the archive, once:
1. its durable decisions live in standing docs or `Decisions.md`;
2. no code, test, or current doc references it; and
3. its last commit is listed under [Retired History](#retired-history).

Recover a retired doc with `git show <commit>:<path>`.

## Step names

Every plan step has a **step name**: kebab-case, in backticks, prefixed by
its plan's topic, and unique across all docs. Examples:
`perf-evaluation-recursion` and `parser-keyword-frames`.
- **Refer to steps by name**, in plans, reviews, docs, code comments and
  commits. Never use a number such as "track 4" or "slice 2": numbers order
  a list, and they collide across plans.
- **Commits** that advance a step end with a `Plan-Step: <name>` trailer,
  one per step, before any attribution lines. Find a step's history with
  `git log --grep 'Plan-Step: perf-evaluation-recursion'`.
- **A new step gets its name when it is written down.** Lists may stay
  numbered for order; older commits that cite a number map to that list.
- **Review finding IDs** (such as X3, N8 or F1) stay as they are: they are
  stable references into one review. Qualify them with the review where
  ambiguity is likely.

## Active Plans

- [`PerformanceRoadmap_2026-10-05.md`](PerformanceRoadmap_2026-10-05.md):
  the umbrella for performance work, covering shared measurement, how
  budgets treat batching, and the order of its `perf-` steps.
- [`ParserBacktrackingPerformance_2026-10-04.md`](ParserBacktrackingPerformance_2026-10-04.md):
  `perf-parser`. The prefix-shared term parser parses every ordinary
  expression in production. `parser-patterns`, `parser-keyword-frames` and
  `parser-grammar-retirement` remain.
- [`EvaluationRecursionPerformance_2026-10-04.md`](EvaluationRecursionPerformance_2026-10-04.md):
  `perf-evaluation-recursion`. Evaluation time grows roughly quadratically
  with recursion depth.
- [`NetPolarityChecker_2026-10-05.md`](NetPolarityChecker_2026-10-05.md)
  requires every interaction net to be polarized, with a `+` exposed port,
  and checks it before N8 and net fuzzing.
- [`UserInputPanicSafety_2026-10-04.md`](UserInputPanicSafety_2026-10-04.md)
  keeps code that observes user input from panicking, and keeps a caught panic
  from poisoning the runtime. Interaction-net inspection remains.

## Active Reviews

- [`HolisticArchitecturePrePerformance_2026-10-03.md`](../reviews/HolisticArchitecturePrePerformance_2026-10-03.md)
  is the pre-performance backlog: its P1 and P2 sequences and open maintainer
  decisions.

## Deferred Plans

- [`ConcurrentGarbageCollection_2026-08-28.md`](ConcurrentGarbageCollection_2026-08-28.md):
  concurrent marking, delayed logical sweep, and epoch-safe run recycling.
- [`ValueRepresentationRefinement_2026-08-19.md`](ValueRepresentationRefinement_2026-08-19.md):
  compact tagged values and representation splitting.
- [`GarbageCollectorScopedPointerSafety_2026-09-09.md`](GarbageCollectorScopedPointerSafety_2026-09-09.md):
  the lifetime-branded `ScopedGc` experiment, deferred until a defect demands
  it.
- [`PureEffectAccessFusion_2026-09-23.md`](PureEffectAccessFusion_2026-09-23.md):
  regional fusion for pure standard-effect chains, after value-representation
  refinement.
- [`PublicResumableEvaluation_2026-09-23.md`](PublicResumableEvaluation_2026-09-23.md):
  a library API to retain, advance, and resume one foreground evaluation.

## Open Items Without a Plan

Each line moves into a plan as soon as one owns it.

- **Public diagnostic projection.** Review the transitional
  `Diagnostic::from_parts` policy, which caches shallow `message` and `line`
  views beside the structured emission, and decide which conveniences stay.
- **`.read.token` dependencies.** It turns an unavailable dependency into a
  parser error, and nested search does not suspend.
- **Net callable performance.** Small-set followed identity, batched pure pair
  steps, annotated normalization, and a measured specialized checkpoint. Owner
  once it exists: a net performance plan (holistic N3).
- **Net construction performance.** Budgeted or incremental replay of large
  netlists, batching, split counters, and moving reusable pure handler
  definitions into Glam source. Indexed list-fix reevaluation is potentially
  quadratic and unmeasured: each alternative re-runs from the start to get
  its own future. Columnar descriptors belong to value-representation
  refinement.
- **Comment review** (maintainer, 2026-10-05). Find comments that are too
  large and belong in a standing doc or `Decisions.md`, and comments that are
  mostly redundant because the code already says it. Replace the step IDs
  that remain in test-module comments along the way.
- **History-coupling guard scope.** `tests/source_doc_coupling.rs` scans
  `src/` only; extend it to `tests/` and `crates/`.

## Completed, Awaiting Extraction

- [`ResumableWhnfHolistic_2026-09-28.md`](../reviews/ResumableWhnfHolistic_2026-09-28.md)
  and [`ResumableWhnfW6G4_2026-09-23.md`](../reviews/ResumableWhnfW6G4_2026-09-23.md)
  hold measurement series and the Callgrind/DHAT recipe. They stay until the
  holistic review's performance harness (P1-5) gives those numbers a home.

## Retired History

| Doc | Last commit | Step-ID family |
| --- | --- | --- |
| `plans/GarbageCollectorPublicValueAccessInventory_2026-08-28.md` | `1a84ca39` | GC integration (`I…`) |
| `plans/ResumableWhnfHolisticReviewPlan_2026-09-28.md` | `5b8a27f3` | resumable WHNF (`W…`, `WHNFHR-…`) |
| `reviews/GarbageCollectorGateG1_2026-08-25.md` | `bb205d9b` | collector gates (`G0`–`G4`), collector crate (`C…`) |
| `reviews/GarbageCollectorGateG2_2026-09-11.md` | `585cfec3` | collector gates |
| `reviews/GarbageCollectorIntegrationI1_2026-08-28.md` | `2c54459c` | GC integration (`I1…`, `GCI1R-…`) |
| `reviews/GarbageCollectorIntegrationI2_2026-08-28.md` | `45635ead` | GC integration (`I2…`) |
| `reviews/GarbageCollectorIntegrationI3_2026-09-02.md` | `5165f2bc` | GC integration (`I3…`) |
| `reviews/GarbageCollectorIntegrationI4_2026-09-03.md` | `6c9581ef` | GC integration (`I4…`) |
| `reviews/GarbageCollectorIntegrationI9_2026-09-11.md` | `26a422d9` | GC integration (`I9…`) |
| `reviews/GarbageCollectorIntegrationI11_2026-09-11.md` | `26c86f71` | GC integration (`I11…`, `GCI11R-…`) |
| `reviews/GarbageCollectorProductionCollectionI11B_2026-09-11.md` | `03aec8ca` | GC integration (`I11B…`) |
| `reviews/GarbageCollectorWorkerFinalizationI11C_2026-09-11.md` | `56968706` | GC integration (`I11C…`) |
| `reviews/GarbageCollectorGCI11R002Holistic_2026-10-01.md` | `26c86f71` | aggressive remediation (`GCI11R-002…`, `GCI2HR-…`) |
| `reviews/GarbageCollectorI11D2PersistentEdgeCost_2026-10-02.md` | `26c86f71` | persistent edges (`I11D.2…`, `P…`) |
| `reviews/GarbageCollectorI11D3StaticClosure_2026-10-02.md` | `26c86f71` | GC integration (`I11D.3…`) |
| `reviews/ResumableWhnfW3_2026-09-13.md` | `37775f5d` | resumable WHNF (`W3…`) |
| `reviews/ResumableWhnfW4_2026-09-13.md` | `66c2a3c4` | resumable WHNF (`W4…`) |
| `reviews/ResumableWhnfW4E_2026-09-14.md` | `be4bedf4` | resumable WHNF (`W4E…`) |
| `reviews/ResumableWhnfW5_2026-09-15.md` | `d8d44e0c` | resumable WHNF (`W5…`) |
| `reviews/ResumableWhnfW6F_2026-09-18.md` | `05b7d368` | resumable WHNF (`W6F…`) |
| `reviews/ResumableWhnfW6G1Baseline_2026-09-18.md` | `c6288431` | resumable WHNF (`W6G.1…`) |
| `reviews/ResumableWhnfW6GInterim_2026-09-21.md` | `d8d44e0c` | resumable WHNF (`W6G…`) |
| `reviews/ResumableWhnfW6GRemainingPlan_2026-09-21.md` | `d8d44e0c` | resumable WHNF (`W6G…`) |
| `reviews/ResumableWhnfW6G_2026-09-24.md` | `cd7773e9` | resumable WHNF (`W6G…`) |
| `reviews/ResumableWhnfW8_2026-09-27.md` | `672845d5` | resumable WHNF (`W8…`) |
| `reviews/ResumableWhnfW9_2026-09-28.md` | `8c611ae0` | resumable WHNF (`W9…`) |
| `reviews/InteractionNetCallableWhnfSpill_2026-09-16.md` | `fd574a05` | net callable spill (`NC…`) |
| `plans/InteractionNetCallableWhnfSpill_2026-09-16.md` | `fd574a05` | net callable spill (`NC…`) |
| `plans/PureInteractionNetConstruction_2026-09-20.md` | `d492b966` | pure net construction (`PNC…`) |
| `reviews/PureInteractionNetConstructionPNC3_2026-09-20.md` | `8a2c8a2f` | pure net construction (`PNC3…`) |
| `reviews/PureInteractionNetConstructionPNC4_2026-09-20.md` | `87c4a3fa` | pure net construction (`PNC4…`) |
| `reviews/PureInteractionNetConstructionPNC5_2026-09-21.md` | `3bfc7def` | pure net construction (`PNC5…`) |
| `plans/GarbageCollectionRoadmap_2026-08-19.md` | `9e8d86fe` | collector gates (`G0`–`G4`) |
| `plans/GarbageCollectionGateG0Baseline_2026-08-20.md` | `807a91c1` | collector gates (`G0`) |
| `reviews/GarbageCollectorC2C_2026-08-22.md` | `2d6eb2e4` | collector crate (`C2C…`, `GC2C-…`) |
| `reviews/GarbageCollectorC6_2026-08-24.md` | `bb205d9b` | collector crate (`C6…`, `GC6-…`) |
| `reviews/GarbageCollectorC8_2026-10-03.md` | `9e8d86fe` | collector crate (`C8…`) |
| `plans/GarbageCollectorIntegration_2026-08-19.md` | `9e8d86fe` | GC integration (`I…`, `GCI…R-…`) |
| `plans/GarbageCollectorImplementation_2026-08-19.md` | `9e8d86fe` | collector crate (`C…`, `GC…-…`) |
| `plans/GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md` | `26c86f71` | aggressive remediation (`D.2…`, `GCI11R-002…`) |
| `plans/GarbageCollectorAggressiveVerificationRegression_2026-10-03.md` | `94ce5952` | aggressive regression (decisions `D1`–`D11`) |
| `plans/GarbageCollectorPersistentEdgeTraits_2026-09-12.md` | `26c86f71` | persistent edges (`P0`–`P5`) |
| `plans/ResumableWhnfEvaluation_2026-09-12.md` | `8b1ace22` | resumable WHNF (`W…`) |
| `reviews/GarbageCollectorIntegration_2026-08-25.md` | `6aaaaeec` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorGateG3_2026-10-02.md` | `26c86f71` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorGateG4_2026-10-02.md` | `8ad63790` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorReadinessIntegration_2026-10-02.md` | `6aaaaeec` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorRuntimePolicy_2026-10-02.md` | `cbfa4fa6` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorExplicitMaintenance_2026-10-02.md` | `cbfa4fa6` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorI13CleanupInventory_2026-10-02.md` | `06602491` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorIntegrationI5I10_2026-09-03.md` | `0840f7be` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorIntegrationI5_2026-09-07.md` | `d75adf0c` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorIntegrationI6I7_2026-09-10.md` | `9d17b2bc` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorIntegrationI8_2026-09-10.md` | `335e37d6` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorIntegrationI10_2026-09-11.md` | `4da38497` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorIntegrationI12_2026-10-02.md` | `242d47f1` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorOpaqueRepresentation_2026-09-11.md` | `c0015f1a` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorRawValueApiAudit_2026-09-11.md` | `1b35d44c` | GC integration (`I…`, `GCI…R-…`) |
| `reviews/GarbageCollectorAggressiveD2h_2026-10-01.md` | `0a60ed34` | aggressive verification (`D.2…`, `I11D…`) |
| `reviews/GarbageCollectorAggressiveVerificationClosure_2026-10-01.md` | `26c86f71` | aggressive verification (`D.2…`, `I11D…`) |
| `reviews/GarbageCollectorI11D2DynamicToolMatrix_2026-10-01.md` | `c01e1437` | aggressive verification (`D.2…`, `I11D…`) |
| `reviews/ResumableWhnfW6G1Design_2026-09-19.md` | `d959cfd1` | resumable WHNF (`W…`) |
| `reviews/ResumableWhnfW7_2026-09-27.md` | `ca8548ca` | resumable WHNF (`W…`) |
| `plans/GarbageCollectorOwnershipLedger_2026-08-20.md` | `8ad63790` | GC integration (`I…`) |
| `reviews/ArchitectureAndVerification_2026-10-03.md` | `8b1ace22` | parallel review (`AR-…`, `RF-…`) |
| `plans/DocumentationDisposition_2026-10-05.md` | `f53e9b51` | documentation cleanup (its maintainer answers are in `Decisions.md`) |
