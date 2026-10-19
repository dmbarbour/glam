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

Swept for status and drift on 2026-10-15; each plan names its open
questions under "Open questions (2026-10-15)". A "Likely deprecated" mark
flags a step for discussion when work reaches it, not a decision.

- [`PerformanceRoadmap_2026-10-05.md`](PerformanceRoadmap_2026-10-05.md):
  the umbrella for performance work, covering shared measurement, how
  budgets treat batching, and the order of its `perf-` steps. Latest
  measurements: Snapshot 2026-10-15. `perf-parser` closed 2026-10-17;
  next, the representations, then `perf-net-lowering`, then
  `perf-reflection-effects`.
- [`GrammarReview_2026-10-16.md`](GrammarReview_2026-10-16.md): the
  grammar reviewed for clarity when read (Green's cognitive dimensions) and
  ease of parsing, and what belongs in the parser, a linter or a formatter.
  Started 2026-10-16; its current items were decided by 2026-10-17, and
  `grammar-body-declarations` (item 15: `alias` and `let` in bodies) was
  built 2026-10-17.
- [`RuntimeApi_2026-10-17.md`](RuntimeApi_2026-10-17.md): `api-` steps
  from the runtime API review: `Runtime`, turns on keyed machines, a
  bridge for the CLI, data operations, cuts, scoped threads, heap
  communication, request queues, settlement, hosts as machines, effect
  providers, the handle API's retirement, then the runtime's retirement
  protocol. The CLI moves piecemeal, a slice in each step. Drafted
  2026-10-17; `api-runtime-turns`, `api-cli-bridge`, `api-machine-data`,
  `api-cuts`, `api-virtual-resources`, `api-machine-tree`,
  `api-forks-in-cuts` and `api-machine-errors` are built,
  `api-machine-tree` superseding `api-scoped-threads`, and the `api-cli-prelude` trial was kept; the
  plan goes ahead of performance work unless a cost gets in the way
  (maintainer, 2026-10-18). On
  2026-10-18 the maintainer agreed effect providers, run by evaluation
  within turns, and the bridge; cuts bound transactions and search, with
  failure explicit as in glam; the
  ownership plan merged in; and a CLI prelude in glam is an experiment,
  `api-cli-prelude`.
- [`TagPrefixSyntax_2026-10-16.md`](TagPrefixSyntax_2026-10-16.md):
  `path:` is a prefix operator and `:name` a pun everywhere, so braces lost
  their member grammar; follow-ups retired bare tags, rejected tags as
  predicates and views, and made an application's arguments atoms. Built
  2026-10-17 except `tags-prefix-diagnostic-retirement`, which waits until
  the transitional diagnostic has served.
- [`ReflectionPerformance_2026-10-15.md`](ReflectionPerformance_2026-10-15.md):
  `perf-reflection-effects`, split from the structural overheads plan on
  2026-10-15. Reflection effect steps (`perf-reflection-step-cost`, 376 K
  instructions a `.r ()` step) and the compiler's scanner
  (`perf-reflection-scanner`). Waits for the parser work and the
  representations (maintainer, 2026-10-15).
- [`UserInputPanicSafety_2026-10-04.md`](UserInputPanicSafety_2026-10-04.md)
  keeps code that observes user input from panicking, and keeps a caught panic
  from poisoning the runtime. Interaction-net inspection
  (`panic-net-inspection`) remains; the deep-nesting stack overflow went
  to the roadmap's `perf-pre-eval-stack-depth` (maintainer, 2026-10-15).
- [`NetPolarityChecker_2026-10-05.md`](NetPolarityChecker_2026-10-05.md)
  requires every interaction net to be polarized, with a `+` exposed port.
  Done except `polarity-net-fuzzing`, which follows `panic-net-inspection`
  once lower-hanging fruit is gone (`panic-discovery-by-inspection`); the
  plan retires once that step has another owner.
- [`FastChecks_2026-10-09.md`](FastChecks_2026-10-09.md) keeps
  `scripts/check.sh fast` quick: per-thread test runtimes and an optimized
  dev profile. `test-slow-tier` waits until a test needs it; the plan could
  retire once its recipe moves into `AgentContext.md`.

## Active Reviews

- [`HolisticArchitecturePrePerformance_2026-10-03.md`](../reviews/HolisticArchitecturePrePerformance_2026-10-03.md)
  is the pre-performance backlog. P0 and P1 are done, except P1-6's
  oracles; P2 became `perf-structural-overheads`. Its "Status 2026-10-15"
  lists the open findings and their owners; maintainer decision 6
  (reflection semantics) is open. About 30 findings have no other home, so
  it cannot retire yet.
- [`RuntimeLifecycleCapabilities_2026-10-16.md`](../reviews/RuntimeLifecycleCapabilities_2026-10-16.md)
  reviews who owns the evaluation runtime and how code the runtime calls
  gets its capabilities. Accepted 2026-10-16: a thread-affine owner that
  retires the runtime explicitly, effects crossing to client threads as
  messages through request queues. Its decisions are settled, and
  [`RuntimeOwnership_2026-10-17.md`](RuntimeOwnership_2026-10-17.md) holds
  the remediation.
- [`RuntimeApi_2026-10-17.md`](../reviews/RuntimeApi_2026-10-17.md)
  re-envisions the public API from user stories (maintainer, 2026-10-17): a
  thread-affine runtime in which Rust threads run turns on keyed machines
  with a stack and local state, transactions as nested steps, fork-join
  through scoped threads, and communication only through the shared heap or
  volumes. It replaces the ownership plan's owner and handles. Direction
  agreed and questions answered, but names and communication's conventions,
  which the [runtime API plan](RuntimeApi_2026-10-17.md) settles step by
  step.
- [`SyntaxTypeErrors_2026-10-17.md`](../reviews/SyntaxTypeErrors_2026-10-17.md)
  reviews which type errors the front end could report from syntax alone:
  applying data, data as a predicate, view or effect, member access on
  data, and data operators' operands. Recommends `kinds-` steps, one
  classifier of evident kinds and one walk. Its questions are answered;
  the steps are low priority, after the performance work.

## Completed, Awaiting Retirement

Each is done; what keeps it under the retention rule is listed.

- [`RuntimeOwnership_2026-10-17.md`](RuntimeOwnership_2026-10-17.md),
  `lifecycle-` steps, merged 2026-10-18 into the runtime API plan, which
  carries its remaining steps; `lifecycle-cli-logger` was done 2026-10-16.
  Still cited by the runtime API plan and review and `Decisions.md`; it
  holds the queue and retirement designs the merged steps adapt.

- [`ParserBacktrackingPerformance_2026-10-04.md`](ParserBacktrackingPerformance_2026-10-04.md),
  `perf-parser`, closed 2026-10-17 (maintainer): the term parser, keyword
  frames, one-walk local analysis. Its open findings moved to the
  roadmap's standalone steps. Still cited by `Decisions.md`, the roadmap,
  the grammar review, the tag prefix and panic-safety plans, and a comment
  in `scripts/profile.sh`; it holds the measurements they cite.

- [`StructuralOverheads_2026-10-08.md`](StructuralOverheads_2026-10-08.md),
  `perf-structural-overheads`, closed 2026-10-15 (maintainer): costs that
  are not representation. Its open steps moved to the reflection plan, the
  roadmap's standalone steps and the parser plan. It holds the evidence of
  its done steps, which `Decisions.md` and the roadmap cite.
- [`ExplicitHeapHolds_2026-10-09.md`](ExplicitHeapHolds_2026-10-09.md),
  done 2026-10-15: `Heap::mutate` is the only entry, outermost only, and
  code takes access or a mutator from its caller. Still cited by the
  roadmap, the structural overheads plan and `crates/glam-gc/VERIFY.md`.
- [`LazyDiagnostics_2026-10-15.md`](LazyDiagnostics_2026-10-15.md), done
  2026-10-15: senders build and enrich diagnostics without evaluating
  them; only renderers evaluate. Still cited by the explicit heap holds
  plan.
- [`EvaluationRecursionPerformance_2026-10-04.md`](EvaluationRecursionPerformance_2026-10-04.md),
  done 2026-10-09: recursion is linear, and tail recursion runs in
  constant space. The route-validation hazard log has no `Decisions.md`
  entry, and two follow-ups have no owner (see its open questions).

## Deferred Plans

- [`ValueRepresentationRefinement_2026-08-19.md`](ValueRepresentationRefinement_2026-08-19.md):
  compact tagged values and representation splitting. It is the roadmap's
  `perf-value-representation`, after `perf-structural-overheads`.
- [`PureEffectAccessFusion_2026-09-23.md`](PureEffectAccessFusion_2026-09-23.md):
  regional fusion for pure standard-effect chains
  (`perf-effect-chain-fusion`). Drivers now hold one mutator across a
  poll's steps, so most of its premise is gone; likely to close at PEAF1.
- [`PublicResumableEvaluation_2026-09-23.md`](PublicResumableEvaluation_2026-09-23.md):
  a library API to retain, advance, and resume one foreground evaluation,
  after holistic S7 and A1.
- [`ConcurrentGarbageCollection_2026-08-28.md`](ConcurrentGarbageCollection_2026-08-28.md):
  concurrent marking, delayed logical sweep, and epoch-safe run recycling.
  Much lower priority (maintainer, 2026-10-15), but possibly needed after
  the `--interactive` switch (a reflection-based, user-defined IDE). Its
  nesting rationale was superseded by one heap per thread and
  `no-recursive-heap-entries`.
- [`GarbageCollectorScopedPointerSafety_2026-09-09.md`](GarbageCollectorScopedPointerSafety_2026-09-09.md):
  likely deprecated. Its branded views landed as `GcRef`
  (`owned-mutator-views`, 2026-10-12).

## Tentative Plans

Ideas the maintainer means to pursue, short of a plan.

- **Branded abstract global paths** (maintainer, 2026-10-17). An abstract
  global path would carry, besides its components, the `TypeId` of a
  private Rust marker type as its authority, and two paths would be equal
  only when both match. The bootstrap compiler would brand its paths with a
  marker of its own, so neither Glam code nor other Rust code could forge
  them; `Values::abstract_global_path` would go or take a capability, and
  Rust clients could brand domains of their own. The `TypeId` is only the
  brand: Rust privacy around the marker is the authority, so no API may
  accept a raw `TypeId`. This also gives the
  [runtime API review](../reviews/RuntimeApi_2026-10-17.md)'s machine keys
  and heap capabilities private namespaces.

## Open Items Without a Plan

Each line moves into a plan as soon as one owns it.

- **Public diagnostic summaries.** Since `diag-static-summaries`
  (2026-10-15), `Diagnostic::message`, `line` and `source` hold only the
  Rust text a diagnostic was built with; one built from a value reads
  `EvalFailure` or `EvalDiagnostic`. The code still calls them
  transitional. Decide whether they stay as documented conveniences or go.
- **`.read.token` dependencies.** It turns an unavailable dependency into a
  parser error, and nested search does not suspend.
- **Net callable performance.** Small-set followed identity and a measured
  specialized checkpoint. Batched pure pair steps are the roadmap's
  `perf-net-pattern-batching`, and annotated normalization landed as
  `anno 'normalize` (`perf-compiled-nets`, 2026-10-11). Owner once it
  exists: a plan for the roadmap's net steps (holistic N3).
- **Net construction performance.** Budgeted or incremental replay of large
  netlists, batching, split counters, and moving reusable pure handler
  definitions into Glam source. Indexed list-fix reevaluation is potentially
  quadratic and unmeasured: each alternative re-runs from the start to get
  its own future. Columnar descriptors belong to value-representation
  refinement.
- **Comment review** (maintainer, 2026-10-05). Find comments that are too
  large and belong in a standing doc or `Decisions.md`, and comments that are
  mostly redundant because the code already says it. Few step IDs remain:
  `W4` in two test strings in `src/eval/value/tests/lazy_checkpoint.rs`, and
  `C8` in glam-gc's `examples/c8_measurements.rs` and `src/deterministic.rs`.
- **History-coupling guard scope.** `tests/source_doc_coupling.rs` scans
  `src/` only; extend it to `tests/` and `crates/`. No Rust file there
  references the history directories today, so the extension passes at once.

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
| `reviews/PerformanceAfterRecursionWork_2026-10-08.md` | `b2ee16c9` | performance steps (`perf-…`); its findings are in `plans/StructuralOverheads_2026-10-08.md` |
| `reviews/ResumableWhnfHolistic_2026-09-28.md` | `d893d0a2` | resumable WHNF (`HR…`, `WHNFHR-…`); its Callgrind/DHAT recipe is in `AgentContext.md` "Profiling" |
| `reviews/ResumableWhnfW6G4_2026-09-23.md` | `d893d0a2` | resumable WHNF (`W6G.4…`, `W6G4R-…`) |
