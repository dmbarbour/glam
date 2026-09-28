# Resumable WHNF Holistic Review Plan — 2026-09-28

Status: active. HR0-HR2 are complete; HR3-HR8 remain. This plan governs the final implementation review of
[`ResumableWhnfEvaluation_2026-09-12.md`](../plans/ResumableWhnfEvaluation_2026-09-12.md)
and its reconciliation with the two parent workstreams which remain open:

```text
GarbageCollectorIntegration I11 / Gate G3
  -> GarbageCollectorAggressiveVerificationRemediation GCI11R-002D
       -> D.2c evaluator operations and builtins
            -> ResumableWhnfEvaluation W0-W9
       -> D.2d-D.2h and 002E-002H remain
  -> GarbageCollectorPersistentEdgeTraits P3
       -> parent D.2b-D.2g raw-value interlock
       -> P4 trait cutover and P5 certification remain
```

The WHNF plan was introduced by `42b52f2c` and closed through `8c611ae0`.
It grew from one evaluator-continuation repair into the current regional WHNF
machine, managed checkpoint ownership, role-specific scheduling, net callable
checkpoints, pure net construction, stack/budget closure, compatibility
retirement, and exact-route performance work. Passing its phase-local tests is
not by itself a holistic proof that the original intentions still match the
result or that the parent plans describe the right next work.

## Outputs

The review produces one dated integrated review:

- `docs/reviews/ResumableWhnfHolistic_2026-09-28.md`

That review has two separately closable parts:

1. **WHNF implementation audit** — original purpose, architecture, invariants,
   non-goals, verification matrix, completion criteria, current code, and
   current architecture/invariant documentation.
2. **Parent reconciliation and forward path** — the exact effect on
   GCI11R-002D.2c-D.2h, persistent-edge P3-P5, Gate G3, and the order in which
   remaining work should resume.

The review may update the three plans and current architecture documents where
the correction is obvious. A substantive code defect, semantic choice, or
large parent repartition becomes a named finding with a remediation checkpoint
rather than being silently repaired during review.

## Review rules

- Current source and executable inventories outrank completion prose.
- Drift is not automatically a defect. Classify it as intended refinement,
  justified scope growth, stale documentation, accidental compatibility, or
  unresolved implementation work.
- Distinguish **WHNF closure**, **D.2c raw-value closure**, **P3 interlock
  progress**, and **Gate G3 certification**. None implies the others.
- Do not treat ordinary-suite success as aggressive-GC proof.
- Do not treat repeated concurrent success as ordering evidence. Every race
  claim must point to a latch, barrier, deterministic probe, or model test.
- Do not preserve a historical counter, compatibility API, or wrapper merely
  because a phase once measured it. Identify its current owner and purpose.
- Do not require future D.2d-D.2g work to reproduce a WHNF abstraction when a
  current root/access/checkpoint boundary already supplies it.
- Parent-plan counts and fingerprints are historical unless rerun against the
  current tree. Label them accordingly.
- Review-only work does not enable automatic GC or authorize P4 trait removal.

## Finding format

Each finding records:

- stable ID `WHNFHR-NNN`;
- severity and affected boundary;
- current evidence with file/test references;
- original intention and actual implementation;
- disposition: resolved, accepted drift, resolved by a named future plan
  checkpoint, or open;
- exact verification or exit condition; and
- whether it blocks D.2d, P4, P5, D.2h, or Gate G3.

“Resolved by plan” closes the review obligation only when a named future
checkpoint contains concrete work and an exit condition. It does not mean the
underlying implementation or Gate G3 is complete.

## HR0 — Baseline and dependency ledger

### HR0A — Freeze review baselines

Record:

- introduction and final commits for the focused plan;
- all W0-W9 phase and review documents;
- current ordinary and profiling verification state;
- current aggressive-verification state from fresh focused probes, not only
  the W8 report; and
- the exact parent-plan status text and remaining checkpoints.

The initial documentation scan has already found one candidate drift: the
WHNF plan's file-level status still says W9C.1-E are planned even though its W9
section and dated W9 review say the phase is complete. Do not fix this isolated
line until HR1 decides whether all completion criteria are actually satisfied.

### HR0B — Build the artifact map

Inventory the current implementation and test owners for:

- `WhnfState`, continuation frames, regional/net/durable ownership wrappers,
  and the managed checkpoint cell/root;
- client demand, lazy producer, promise follower, spark, reflection, and net
  result destinations;
- callback-free regional polling and mutator-free orchestration;
- reflection request hosting and standard-effect fusion;
- net callable checkpoints and pure net-construction machines;
- budgets, stack control, dependency/cycle translation, and failure context;
- role-specific foreground/background pumping and exact-route coordination;
- source-backed raw-value, checkpoint, durable-owner, managed-edge, and
  compatibility inventories; and
- ordinary, aggressive, profiling, small-stack, and forced-ordering tests.

For each artifact record its authoritative owner, durable liveness owner,
access authority, suspension boundary, result destination, and parent-plan
assignment. This map is evidence for later findings, not another permanent
architecture document.

Exit: every implementation family named by the WHNF plan has one current code
owner and test/inventory location, or a finding records its absence.

## HR1 — Original-intention accounting

HR1 may pull forward a narrow pre-existing fixture repair only when a failed
focused aggressive test would otherwise make HR1 and HR2 repeat conditional
reasoning. Record the failure and exact GCI11R-002E ownership first, repair the
fixture without changing production semantics, then rerun the complete focused
ordinary/aggressive partition before continuing the contract matrix. This does
not close or repartition the broader GCI11R-002E work.

Audit the final implementation against, in order:

1. the two original motives in `Purpose`;
2. every statement in `Selected Architecture`;
3. all twenty semantic and safety invariants;
4. every non-goal and review trigger;
5. all eighteen verification-matrix concerns; and
6. all eleven completion criteria.

Build one matrix with these columns:

| Contract | Current representation/path | Static evidence | Dynamic evidence | Drift | Disposition |
| --- | --- | --- | --- | --- | --- |

Pay particular attention to refinements which changed the original shape:

- fine-grained durable roots became one aggregate managed checkpoint;
- lazy producer progress moved beneath the managed lazy;
- regional/net state became one canonical state rather than isomorphic copies;
- client demand and role-specific pumping grew beyond the initial trampoline;
- reflection tasks remained coordinator-owned rather than value-graph
  checkpoints;
- pure net construction replaced reflection-machine construction handling;
- exact-route reconciliation became a post-W6G performance repair; and
- W8 closed the direct evaluator/raw-value compatibility partition before W9.

Exit: every original contract has current evidence and an explicit disposition;
“phase marked complete” is not accepted as evidence.

## HR2 — Representation, ownership, and GC-safety review

Trace the current ownership graph from public/runtime roots through every WHNF
owner and managed edge. Verify:

- no raw `core::Value`, unrooted `Gc<T>`, mutator, `RuntimeValueAccess`, or
  `EvaluationValueAccess` crosses a wait, callback, scheduler handoff, or
  safepoint;
- every durable computation has one exact root, traced owner, or coordinator
  machine record;
- managed cells never contain registered roots to their own graph;
- lazy-owned checkpoints are collectible with lazy/source/fixpoint cycles;
- reflection and task records cannot become value-graph roots accidentally;
- aggregate checkpoint mutation reports complete leaving and entering edge
  sets through the current transition gateway;
- net callable checkpoints and core-net payload walks trace all nested WHNF
  state without requiring compatibility traits; and
- panic, cancellation, abandonment, terminal publication, and owner drop do
  not expose an empty or unowned checkpoint.

Reconcile this proof with the current managed-family, durable-owner,
recursive-identity, external-owner, machine-state, and raw-value inventories.
Any inventory which counts only syntax rather than the relevant ownership
property must be identified as such.

Exit: the review can state precisely what D.2c proves under aggressive
collection and what remains unproved until D.2d-D.2h and 002E-H.

## HR3 — Resumption and semantic-equivalence review

Audit behavior across every suspension source:

- budget yield;
- lazy and assigned/unassigned promise dependencies;
- reflection and metadata reflection gates;
- host-call and import boundaries;
- net computation and callable checkpoints;
- list/dict/key/path/object/control-stack traversal;
- `.alt`, `.cut`, transaction retry, reset/shift, task, exit, and fixpoint
  effects; and
- structured permanent failure with context before and after suspension.

For each family establish that completed prefixes are not replayed, the same
semantic identities are resumed, retryable dependencies are not cached as
errors, and only the owning outer machine publishes the result. Review the
current use of counters as semantic evidence: a counter must be tied to a
forced suspension or exact fixture, not merely be stable in a full run.

Exit: every suspendable WHNF family is covered by a forced resumption fixture
or a justified bounded/non-suspending proof.

## HR4 — Stack, budget, scheduling, and concurrency review

Audit:

- the absence of user-controlled Rust recursion below WHNF;
- budget propagation through nested WHNF, list, net, and effect work;
- yield/requeue fairness and absence of fabricated dependencies;
- client versus worker ownership and the bounded background pump;
- exact producer sharing, last-subscriber retention, and lazy reclamation;
- pure-lazy versus promise-inclusive cycle handling;
- work-generation waits and notification precision;
- exact-route zipper authority, hazard validation, wrap behavior, and fallback;
- cursor-WHNF's narrow structural wait exception; and
- lock/access ordering around managed checkpoint cells, coordinator claims,
  callbacks, and future concurrent marking.

Review each concurrency claim against its forced-ordering fixture. Record
future concurrent-GC interaction—especially managed checkpoint mutexes and
root-frame possibilities—in the concurrent-GC plan only when the current STW
design is sound.

Exit: no current semantic or liveness claim rests on uncontrolled scheduling,
and any performance-only residual is separated from correctness.

## HR5 — Complexity and performance accounting

Review the final design for accidental costs introduced during the transition:

- root registration and projection counts;
- nested access introductions;
- checkpoint allocation, cloning, and edge-walk traffic;
- managed-cell lock scope;
- scheduler work records, subscriptions, wakeups, and route searches;
- net checkpoint node size and payload boxing;
- whole-value versus regional construction;
- source-shaped instruction, reduction, and native timing histories; and
- static profiling code retained after its decision.

Use existing deterministic counters and Callgrind records before adding new
instrumentation. Performance findings do not block parent GC closure unless
they conceal ownership, make verification infeasible, or lock in a boundary
which prevents the planned representation/collector work.

Exit: retained complexity has a current role; obsolete compatibility and
measurement scaffolding is removed or assigned to a concrete cleanup.

## HR6 — Parent aggressive-GC reconciliation

Re-audit GCI11R-002D.2c against the current tree rather than only its
2026-09-27 W8 snapshot:

- verify the D.2c production partition is still zero after W9;
- verify the direct evaluator gate and whole-value compatibility names remain
  absent;
- determine whether `EvalContext::evaluate_compatibility_whnf` still exists,
  who calls it, and whether D.2d remains the correct owner;
- rerun D.2c ordinary and aggressive focused tests;
- reconcile the root-traffic and mutator-introduction ledgers;
- distinguish test-fixture gaps from production D.2c regressions; and
- update D.2c's completion summary only with current counts/evidence.

Then review D.2d-D.2h for drift caused by the WHNF implementation. In
particular, determine whether current orchestration already uses
`WhnfComputation`, `EvaluationPollContext`, managed checkpoints, or rooted
handoffs in ways that shrink, reorder, or repartition D.2d. Review D.2e-D.2g
for shared evaluator/value-access facilities they should reuse rather than
reimplement. Keep D.2h as the production ownership closure boundary unless
the evidence justifies a better partition.

Exit: the aggressive-remediation plan has a current, low-risk next checkpoint
with exact prerequisites, inventories, and aggressive verification.

## HR7 — Persistent-edge P3-P5 reconciliation

Rerun the persistent-edge occurrence and compatibility manifests against the
current tree. Audit every WHNF-managed representation for accidental `Clone`,
`Copy`, `Debug`, `PartialEq`, `Eq`, pointer comparison, or unqualified edge
duplication introduced after P2:

- canonical and durable WHNF state;
- managed lazy/builtin/checkpoint cells;
- callable checkpoint payloads and runtime nodes;
- client, promise, reflection, spark, and task machine records; and
- profiling/debug surfaces.

Decide whether the old 51-entry carrier interlock and its later progress notes
still accurately partition remaining declarations among D.2b and D.2d-D.2g.
Do not proceed to P4 merely because D.2c is zero. P4 requires the current
manifest to reach its actual cutover condition, including test blockers; P5
must still reconcile with D.2h and repository-wide aggressive verification.

Exit: P3 states the current dependency count and owners, removes or labels
stale historical instructions, and gives an exact readiness condition for P4.

## HR8 — Verification audit and forward-path synthesis

Build one verification ledger separating:

- ordinary semantic tests;
- focused aggressive-GC tests;
- repository-wide aggressive-GC failures;
- forced concurrency orderings;
- small-stack and budget tests;
- interaction-net profiling signatures;
- source-backed inventories and negative source gates;
- Miri/Loom evidence; and
- unverified assumptions.

Run focused tests while reviewing each lane. At closure run documentation
checks and the ordinary routine suite. Run the complete aggressive suite only
at a deliberate checkpoint: its known failures and non-settlement must be
captured by cluster so the review does not confuse expected parent-plan gaps
with a new WHNF regression.

Synthesize the forward path in dependency order. The default hypothesis to
test—not assume—is:

1. correct holistic/documentation findings from this review;
2. resume GCI11R-002D.2d orchestration and remove the last compatibility WHNF
   facade;
3. proceed through D.2e-D.2g using the established bounded-access/rooted
   handoff pattern;
4. close production ownership in D.2h;
5. migrate fixture and schedule gaps in 002E-F, then close clusters and certify
   in 002G-H;
6. rerun persistent-edge P3 after each parent partition, perform P4 only at a
   zero current manifest, and close P5 against D.2h/Gate G3; and
7. only then reconsider automatic collection and the broader Gate G3 outcome.

If current implementation shows that D.2d or P3 should be partitioned
differently, the review must provide the replacement order and explain why it
reduces risk without weakening the ownership proof.

Exit: every open finding has one owner and prerequisite; both parent plans
identify the same next boundary and neither relies on stale WHNF terminology,
counts, or compatibility assumptions.

## Final review closure checklist

- [x] HR0 baseline and artifact map complete.
- [x] HR1 all original intentions and completion criteria accounted for.
- [x] HR2 ownership and aggressive-GC boundary accounted for.
- [ ] HR3 all suspension families accounted for.
- [ ] HR4 stack/budget/scheduler/concurrency claims accounted for.
- [ ] HR5 retained complexity and performance evidence accounted for.
- [ ] HR6 aggressive-remediation parent reconciled.
- [ ] HR7 persistent-edge P3-P5 reconciled.
- [ ] HR8 verification ledger and forward path complete.
- [ ] Every finding is resolved, accepted with rationale, or owned by a named
      future checkpoint with an exit condition.
- [ ] Current architecture and agent-context docs match the reviewed result.
- [ ] The three plan statuses and cross-links agree.
- [ ] Documentation links and `git diff --check` pass.
- [ ] Required focused and routine verification is recorded in the review.
