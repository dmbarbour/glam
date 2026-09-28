# Resumable WHNF Holistic Review — 2026-09-28

Implementation baseline: `8c611ae0`, after W9E closure.

Status: in progress. HR0 baseline/artifact mapping, HR1 contract accounting,
HR2 ownership/GC-safety review, HR3 resumption-equivalence review, HR4
stack/budget/scheduling/concurrency review, and HR5 complexity/performance
accounting are complete. HR2 repaired two
test-fixture publication gaps and one stale foreground-pump expectation before
accepting its forced checkpoint evidence. HR6-HR8 remain
governed by
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

After WHNFHR-001C it contains 885 occurrences: 200 production typed, 36
production erased, 635 test typed, and 14 test erased. Seventy-seven remain
classified as defects:

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

## HR1 — Original-intention accounting

HR1 is complete. It found no semantic defect in the resumable WHNF protocol.
The selected architecture was refined materially, but each refinement preserves
the original contract: one canonical state replaced isomorphic copies, one
aggregate managed root replaced fine-grained durable roots, and semantic lazy
owners now retain their own partial work. WHNFHR-001 repaired two test
publication gaps before aggressive evidence was accepted. WHNFHR-002 records
and resolves the stale plan-level status discovered by HR0.

### Evidence legend

The tables split one contract matrix by source section for readability. Every
row uses the same six columns required by the review plan.

Static evidence abbreviations:

- **S1** — `eval/whnf.rs`: canonical `WhnfState`, role wrappers,
  continuations, bounded regional driver, `WhnfComputation`, and poll algebra.
- **S2** — `eval/whnf/managed_state.rs` and `eval/lazy_checkpoint.rs`: aggregate
  managed state, traced lazy-owned checkpoints, mutex/edge-transition gateway,
  and specialized source machines.
- **S3** — `evaluation/access.rs`, `evaluation/whnf.rs`, and
  `evaluation/session.rs`: higher-ranked regional access, mutator-free poll
  context, dependency translation, and client-demand facade.
- **S4** — `evaluation/coordinator/`, `evaluation/pump.rs`, and
  `eval/value.rs`: result owners, exact producer sharing, cycles, claims,
  release, and role-specific pumping.
- **S5** — `reflection/machine.rs`, `eval/net.rs`, `core_net.rs`, and
  `eval/builtins/net/`: hosted request decoding, callable checkpoints, traced
  core-net payloads, and pure net construction.
- **S6** — WHNF/access/owner/raw-value/persistent-edge source inventories. In
  particular, D.2c is empty and the W7 recursive-call census names no
  unclassified user-sized production recursion.

Fresh dynamic evidence:

- **T1** — complete focused WHNF filters: 85 ordinary and 86 aggressive tests
  pass after WHNFHR-001.
- **T2** — complete W7 filters: 21 ordinary and 21 aggressive tests pass,
  including the small-stack and budget families.
- **T3** — all five W7C budget/scheduling tests pass in the current tree.
- **T4** — every named `scripts/check-interaction-net-profiling.sh` fixture
  passes in the current tree.
- **T5** — phase-local forced fixtures named below remain source-owned tests;
  their current existence is also latched by S6. HR3 and HR4 will inspect
  semantic and ordering sufficiency rather than merely count them.

### Purpose and selected architecture

| Contract | Current representation/path | Static evidence | Dynamic evidence | Drift | Disposition |
| --- | --- | --- | --- | --- | --- |
| Purpose 1: resume after the completed prefix | One canonical focus/frame/followed/source state is retained in `WhnfComputation`, beneath a lazy, or beneath a callable checkpoint. | S1-S5 | T1 plus exact-prefix fixtures in application, builtin, object, access, reflection, and net tests | Refined from an unspecified trampoline into shared canonical state. | Satisfied; HR3 audits each suspension family. |
| Purpose 2: explicit work budget without semantic Rust-stack depth | `EvaluationStepBudget` is borrowed through regional WHNF and specialized machines; yields retain exact state. | S1, S3, S6 | T2-T3 | Scope grew to list/net/effect work and role-specific requeue. | Satisfied. |
| Architecture 1: one reusable WHNF submachine | `WhnfComputation` owns `Seed` or one `ManagedWhnfRoot`; `WhnfState` is reused by regional and net wrappers. | S1-S2 | T1 | Aggregate managed state replaced fine-grained rooted fields. | Accepted refinement; stronger ownership boundary. |
| Architecture 2: pure progress distinct from orchestration | Regional reducers return ready/boundary/failure/yield; dependency translation and scheduling occur after access closes. | S1, S3-S4 | T1, access-scope callback probes | `Continue` became in-place mutation/delegation rather than a public regional variant. | Accepted refinement. |
| Architecture 3: `LazySource` remains recipe | Source is immutable until cache publication; partial progress is a separate traced checkpoint beneath the lazy. | S2, S4 | lazy route-loss, source-retirement, and cycle fixtures in T1 | Progress moved from machine-local expectation to semantic lazy ownership. | Accepted refinement; matches collection goal. |
| Architecture 4: root only at real durable boundaries | Regional state uses raw edges under access; ordinary durable work uses one root; lazy/net owners trace their checkpoints. | S1-S3, S6 | aggregate-root counters, cross-thread collection fixtures, T1 | One aggregate cell replaces one root per live field. | Satisfied. |
| Architecture 5: result destination outside reducer | Client, lazy, promise, spark, reflection, and net owners alone publish or consume terminal results. | S3-S5 | owner-specific completion fixtures in T1/T3 | More explicit outer adapters than initially listed. | Satisfied. |
| Architecture 6: reflection hosts the submachine | Reflection records retain a `WhnfComputation` and purpose/phase; activation and request dispatch remain reflection-machine work. | S4-S5 | forced request-resumption and once-only activation fixtures | Pure net construction moved out of reflection into its own effect machine; reflection itself remains hosted. | Satisfied; intended scope growth. |
| Architecture 7: budgeting is contractual | Every poll receives a mutable shared budget; exhaustion yields without fabricating a dependency. | S1, S3-S5 | T2-T3 | Budget vocabulary now covers scheduler, WHNF, and net units separately. | Satisfied. |

### Semantic and safety invariants

| Contract | Current representation/path | Static evidence | Dynamic evidence | Drift | Disposition |
| --- | --- | --- | --- | --- | --- |
| I1 exact state after every prefix | Canonical state is mutated in place and retained on pending/yield. | S1-S2 | exact-prefix fixtures, T1 | None semantically. | Satisfied. |
| I2 same identities and positions on resume | Focus, frame cursor, operands, `followed`, source owner, and promise breadcrumb are fields of the retained state. | S1 | application/access/object/list fixtures | Net/regional wrappers now move the same state zero-walk. | Satisfied. |
| I3 dependencies are not failures | `WhnfDependency`, deferred requests, and permanent failure remain disjoint poll cases. | S1, S3-S4 | promise/lazy/cycle fixtures | None. | Satisfied. |
| I4 delegation has no semantic identity | Delegation replaces focus; it creates no value, route, or cache entry. | S1 | 4,096-delegation small-stack fixture and root counters | None. | Satisfied. |
| I5 immutable source, separate progress | Lazy cell retains immutable source or checkpoint/cache according to phase. | S2, S4 | route-loss/source-retirement fixtures | Checkpoint is now lazy-owned, not merely machine-local. | Satisfied. |
| I6 only destination owner publishes | Reducer cannot publish arbitrary lazy/task results; outer owner paths are distinct. | S3-S5 | cache/publication/terminal fixtures | None. | Satisfied. |
| I7 no access/raw edge survives region without owner | Higher-ranked access cannot escape; durable state is rooted or traced. | S1-S3, S6 | T1 aggressive, cross-worker checkpoint tests | Current raw-value facade remains a parent-plan violation outside D.2c. | Satisfied for D.2c/WHNF; HR2 scopes the larger claim. |
| I8 exact durable ownership | Seed root, managed root, lazy edge, core-net trace, or coordinator-owned rooted machine covers every boundary. | S1-S6 | T1 aggressive and owner inventories | Specialized lazy checkpoints broadened the owner set explicitly. | Satisfied for inventoried WHNF owners. |
| I9 no ordinary-delegation overhead | Focus replacement stays regional; no root/admission/semantic allocation is performed. | S1 | W6G/W7 counters and T2 | One initial seed promotion remains intentional. | Satisfied. |
| I10 no waits/callbacks/coordinator work under access | Poll context opens bounded access only around callback-free work; boundary handling follows closure. | S3-S5 | callback lock/access probes and T1 | Cursor contention remains separate. | Satisfied. |
| I11 canonical sharing | Lazy producer, promise follower, and exact route all converge on canonical producer/checkpoint state. | S2, S4 | multi-observer/route-loss fixtures | Exact-route zipper is optimization only. | Satisfied. |
| I12 lazy cache rejects deferred shell | Lazy cache stores `EvaluatedValue`; promises may retain raw deferred assignment which WHNF follows. | S4 | forwarding/promise fixtures including repaired WHNFHR-001 | None. | Satisfied. |
| I13 cycle policy remains coordinator-owned | `followed` supplies local shell evidence; coordinator graph poisons pure lazy cycles and leaves promise-inclusive cycles retryable. | S1, S4 | pure/mixed cycle fixtures | Scheduler graph became more exact, not duplicated. | Satisfied. |
| I14 reflection activation once-only | Reservation/permit/activation live in coordinator records, not checkpoint replay. | S4-S5 | forced reflection activation/request counters | Reflection checkpoint remains outside value graph. | Satisfied. |
| I15 failure-context order | Continuation/specialized machines retain pending context and append at the same semantic boundary. | S1, specialized machines | assertion/provenance/context suspension fixtures | None. | Satisfied; HR3 rechecks equality oracles. |
| I16 raw net already WHNF | Reducer returns `Value::Net`; explicit net sources/calls own net work. | S1, S5 | raw-net and callable-checkpoint fixtures, T4 | None. | Satisfied. |
| I17 cursor wait is narrow exception | Net access holds only the proven bracketed same-net contention claim; it does not authorize general waits. | S5 | forced cursor contention tests | Future concurrent GC interaction remains documented. | Satisfied for current STW design. |
| I18 user-sized semantic recursion closed or named | W7 source/call-graph inventory names the bounded exceptions and rejects regressions. | S6 | T2 small-stack families | Balanced persistent-container recursion remains justified bounded work. | Satisfied. |
| I19 deterministic budget yield | Shared mutable budget records spend; yield preserves checkpoint and carries no dependency. | S1, S3-S5 | T2-T3 | More budget types were retained where units differ. | Satisfied. |
| I20 forced concurrency evidence | Ordering claims use barriers, channels, probes, and collector tests; stress repetition is not cited as proof. | S4-S6 | completion/subscription/release/route fixtures | W9 added exact guarded probes. | Satisfied as an evidence policy; HR4 audits each claim. |

### Non-goals and review triggers

| Contract | Current representation/path | Static evidence | Dynamic evidence | Drift | Disposition |
| --- | --- | --- | --- | --- | --- |
| Non-goal: replace reflection effect machine or semantics | Reflection retains `.alt`, `.cut`, transaction, task, and effect continuation ownership. | S5 | reflection regression suite | Pure net construction moved to a dedicated machine without replacing reflection. | Preserved. |
| Non-goal: expose progress as Glam/public API | All progress wrappers and constructors are crate-private except the opaque trait-required net payload type. | S1-S2 | source inventory | A future resumable public `Evaluation` is a separate plan. | Preserved. |
| Non-goal: mutable/observable `LazySource` | Partial work is in managed checkpoint variants, not source mutation or reflection. | S2, S4 | lazy source/checkpoint fixtures | None. | Preserved. |
| Non-goal: infer freshness from refcounts | Ownership uses explicit roots/access and coordinator records. | S1-S6 | aggressive tests | None. | Preserved. |
| Non-goal: semantic lazy/promise as control state | Managed checkpoint cells are internal traced structures, not semantic values. | S1-S2 | owner inventories | None. | Preserved. |
| Non-goal: introduce root frame/moving/concurrent GC/new barrier | Current work uses existing STW roots and edge-transition gateway. | S2 | T1 aggressive | Managed-cell mutex creates a future concurrent-GC review point, already outside this plan. | Preserved. |
| Non-goal: premature frame/JIT tuning | State uses ordinary vectors/boxes and measured route optimization only. | S1-S5 | W9 Callgrind record | Exact-route/hasher work was evidence-driven. | Preserved. |
| Non-goal: control unrelated parser/renderer recursion | W7 census is scoped to semantic WHNF. | S6 | T2 | None. | Preserved. |
| Non-goal: replace net worklists with evaluator stack | Net driver and pure-construction machines retain topology/program worklists; only callable WHNF uses canonical evaluator state. | S5 | callable/netlist fixtures, T4 | Pure net construction replaced a reflection-hosted implementation with a dedicated worklist. | Preserved. |
| Trigger: repeated source-specific child protocols | Shared continuation/specialized-machine protocol was adopted. | S1-S2 | T1 | Trigger fired and was resolved during W3-W6. | Resolved. |
| Trigger: general stack roots every step | Aggregate regional/durable split avoids it. | S1-S3 | root counters, T2 | Trigger informed W6G.3. | Resolved. |
| Trigger: active state cloned for `.alt` | Branching stays in reflection/list-effect machines; active WHNF state is moved/owned. | S1, S5 | effect fixtures | None. | Did not fire. |
| Trigger: lazy progress outside lazy owner | Partial lazy work moved beneath lazy; coordinator stores only route/scheduling state. | S2, S4 | cycle/reclamation tests | Trigger fired during W6G.1 design and was resolved. | Resolved. |
| Trigger: opaque production semantic callback can suspend | `SemanticComputation`/`SemanticThunk` are test-only; production host/reflection boundaries are explicit machines. | S2, S5-S6 | callback fixtures | Test callbacks remain constrained fixture seams. | Resolved. |
| Trigger: wait/callback retains access | Higher-ranked access and explicit boundary polls forbid it. | S3-S6 | access/callback probes | Cursor wait remains separately proved. | Did not fire in current code. |
| Trigger: root registration scales with depth | One aggregate root covers arbitrary frame depth. | S1-S2 | W6G root counters, T2 | Trigger motivated W6G.3. | Resolved. |
| Trigger: source fixture exceeds work budget | Replay was classified and repaired; W9 then reduced rediscovery without altering semantics. | S4-S5 | exact source counters and T4 | Performance work grew substantially. | Resolved. |
| Trigger: W9 needs unsafe global-generation proof | Exact guarded route plus authoritative fallback was retained. | S4 | forced route matrix, T4 | Poll-local generations are hints, not authority. | Did not fire. |
| Trigger: unrelated balanced structures required for stack closure | W7 documented logarithmic/persistent-container bounds instead. | S6 | T2 | None. | Did not fire. |

### Verification matrix and completion criteria

| Contract | Current representation/path | Static evidence | Dynamic evidence | Drift | Disposition |
| --- | --- | --- | --- | --- | --- |
| V1 reflection replay | Hosted request WHNF retains application result and purpose. | S4-S5 | forced one-dispatch/same-lazy fixtures | None. | Satisfied. |
| V2 lazy ownership | Lazy cell traces partial checkpoint; route is scheduling-only. | S2, S4, S6 | reachable/unreachable cycle and route-loss fixtures; T1 aggressive | None. | Satisfied. |
| V3 promise following | Promise roots/followers distinguish assignment, producer, abandonment, and cycle roles. | S1, S4 | promise matrix plus T1/T2 | Scope broadened to cross-session ownership. | Satisfied. |
| V4 exact resumption | Canonical cursors/frames survive boundary and yield. | S1-S2 | no-replay counters across machine families | None. | Satisfied; HR3 performs family accounting. |
| V5 delegation | Focus replacement stays in one regional state. | S1 | 4,096-depth fixture and no-root counter | None. | Satisfied. |
| V6 budget yield | Mutable budget yields/requeues same record without subscription. | S1, S3-S5 | T2-T3 | None. | Satisfied. |
| V7 failure contexts | Failure frames are retained by continuations/machines. | S1 and specialized machines | paired uninterrupted/suspended oracles | None. | Satisfied. |
| V8 callbacks | External/host/reflection boundaries publish checkpoint before invocation. | S2-S5 | exactly-once and lock-free callback probes | None. | Satisfied. |
| V9 GC safety | All durable WHNF state is rooted or traced. | S1-S2, S6 | T1 aggressive after WHNFHR-001 | Fixture gap repaired. | Satisfied for WHNF partition. |
| V10 work sharing | Canonical lazy/checkpoint and exact producer route are shared. | S2, S4 | two-observer/contested-producer fixtures | None. | Satisfied. |
| V11 cycle behavior | Pure lazy and promise-inclusive cases take distinct coordinator paths. | S4 | forced cycle tests | None. | Satisfied. |
| V12 effects | Effect state remains specialized and owns branch/transaction semantics. | S5 | `.alt`/`.cut`/retry/reset/shift/task/exit fixtures | Pure net construction became explicit rather than reflection-driven. | Satisfied. |
| V13 nets | Raw net is WHNF; callable/pure-construction checkpoints are topology/owner traced. | S1-S2, S5 | NC and PNC suites, T4 | Callable checkpoint added as necessary topology. | Satisfied. |
| V14 stack control | User-sized semantic paths are iterative. | S1, S6 | T2 | None. | Satisfied. |
| V15 bounded whole-program work | Source fixture has deterministic semantic/reduction/driver signature. | S4-S5 | W4E/W6G/W9 counters, T4 | Exact-route performance repair followed semantic closure. | Satisfied. |
| V16 exact-route accounting | Zipper is caller-local hint with guarded validation and full fallback. | S4 | forced release/poll matrix, T4 | Added after initial plan scope. | Satisfied. |
| V17 parked-thread precision | Waiter classes and notifications are explicit; useless wakes measured. | S4 | forced admission/completion/shutdown tests and W9 profiles | One shared condition variable retained by measured decision. | Satisfied. |
| V18 closure | Source inventories reject unclassified recursive/suspendable entry. | S6 | inventory suite and T2 | D.2d-D.2g remain outside the zero D.2c partition. | Satisfied for focused scope. |
| C1 every suspension/yield retains exact state | Canonical managed/lazy/net state. | S1-S2 | V1-V6 evidence | None. | Satisfied. |
| C2 reflection consumes same intermediate lazy | Hosted reflection WHNF purpose. | S4-S5 | V1 | None. | Satisfied. |
| C3 iterative budgeted driver covers semantic depth | Regional driver plus specialized machines and W7 closure. | S1-S6 | T2-T3 | None. | Satisfied. |
| C4 no managed authority crosses boundary | Higher-ranked access and rooted/traced handoffs. | S2-S3, S6 | T1 aggressive, callback probes | Parent raw-value violations remain outside D.2c. | Satisfied for WHNF scope. |
| C5 uninterrupted delegation has no per-step roots | Direct focus replacement. | S1 | delegation/root counters | One initial root is intentional. | Satisfied. |
| C6 only outer owners publish | Distinct client/lazy/promise/reflection/spark/net paths. | S3-S5 | owner fixtures | None. | Satisfied. |
| C7 retryable halt callers are stateful or bounded | W7 disposition and call-graph gates classify every current caller. | S6 | T2 | Test-only compatibility facade remains parent-owned, not a new recursive evaluator. | Satisfied. |
| C8 deterministic suspension/cycle/callback/collection/small-stack tests | Focused ordinary/aggressive and forced suites. | S6 | T1-T4 | WHNFHR-001 repaired fixture publication first. | Satisfied. |
| C9 source assembly has bounded no-replay evidence | Exact source fixture and static profile counters. | S4-S5 | T4 and W9 record | None. | Satisfied. |
| C10 final review accounts for drift | This HR0-HR8 review is the owner. | This document | Pending later review checkpoints | The implementation is done but holistic review is not. | In progress; blocks declaring the focused plan wholly closed. |
| C11 W9 resolves or measures route fallback | W9 exact guarded route implementation and review. | S4 | T4 and W9 Callgrind/counters | Precise path implemented; authoritative fallback retained. | Satisfied. |

HR1 therefore closes every implementation contract except completion criterion
10, which is intentionally this review. It does not infer Gate G3 closure from
the focused results.

## HR2 — Representation, ownership, and GC safety

HR2 is complete for the focused WHNF partition. The audit found no production
owner or trace omission. It did find the two fixture-publication gaps recorded
by WHNFHR-003 and the stale foreground-pump expectation in WHNFHR-004; all
three were repaired before the dynamic ownership evidence was accepted.

### Current ownership graph

The durable graph has five distinct roots/owners rather than one universal
machine container:

| Boundary | Authoritative owner | Traced path | Deliberately non-owning state |
| --- | --- | --- | --- |
| newly admitted ordinary demand | input `RuntimeValueRoot`, then `WhnfComputation::Seed` | seed root is promoted inside matching access to one `ManagedWhnfRoot` | work ID, route, and epoch hints |
| resumable ordinary/client/reflection/spark demand | coordinator record containing `WhnfComputation::Managed` | registered `Root<ManagedLazyCheckpointCell>` -> mutex-protected `WhnfState` | subscriber lists and exact-route zipper |
| lazy-owned producer | managed lazy source/checkpoint phase | lazy -> `ManagedLazyCheckpointEdge` -> exact specialized checkpoint cell | coordinator route and lazy ID |
| callable normalization in a net | rooted/traced managed core net | runtime-net payload -> `CallableCheckpoint` -> `NetWhnfState` | pair/generation observation handles |
| immutable promise assignment/following | managed promise plus producer/follower record | promise assignment root/edge and retained WHNF computation | promise ID and wait token |

Reflection work is intentionally not a value-graph checkpoint. Once launched,
its coordinator machine owns the hosted `WhnfComputation` through completion;
the managed reflection completion promise is the semantic edge exposed to the
value graph. Likewise, task and client records are external runtime owners,
not objects reachable from a managed lazy. This avoids manufacturing the root
cycles which the collector exists to reclaim.

### Canonical and specialized trace audit

| Representation | Exact retained edges | Audit result |
| --- | --- | --- |
| `WhnfState` | focus; every generic/application/dictionary/undefined frame value; optional cycle-promise edge | `trace_managed_edges` is exhaustive over `WhnfContinuation`; scalar cursors, IDs, key arrays, and `followed` contain no managed edge. |
| `ManagedLazyCheckpointCell` | the canonical `WhnfState` | one registered root or traced lazy edge owns it; the cell contains no registered root. |
| `ManagedHostCallCheckpointCell` | invoking producer captures or published value/failure direct values | pre/post transition visitors cover both states; callback invocation occurs outside value access. |
| `ManagedNetWhnfCheckpointCell` | request, driver worklist, frontier observations, and nested net edges | the net driver's compile-exhaustive visitor owns the state; no compatibility root is stored inside it. |
| access/object/list-effect/builtin checkpoint cells | each specialized machine's raw arguments, child WHNF work, containers, and promise/net edges | enum matches and state visitors are compile-exhaustive; cells contain passive data and no roots. |
| `NetWhnfState` | the same canonical `WhnfState` moved into a callable-checkpoint payload | its `Trace` delegates directly to the canonical visitor, so regional/net handoff neither copies nor translates the graph. |
| managed core net | callable checkpoint and every other specialization payload | core-net durable-owner and payload visitors destructure the current runtime-net state and reach nested WHNF state. |

Every managed checkpoint mutation holds the cell's sole state mutex and calls
the collector-owned edge-transition gateway with the same complete visitor for
the before and after states. A panic leaves the structurally installed state
behind a poisoned mutex; collection recovers the state only to trace it, while
subsequent evaluator access reports poison instead of silently resuming a
possibly partial transition. Direct destruction is passive for every managed
checkpoint family.

### Lifecycle and boundary audit

| Lifecycle boundary | Evidence and conclusion |
| --- | --- |
| seed promotion | Input root remains authoritative until one aggregate managed root is installed in the same access region. No raw edge crosses the promotion boundary. |
| budget/dependency suspension | Canonical state is already beneath its durable root, lazy edge, or net payload before orchestration receives the boundary poll. |
| scheduler handoff | Coordinator records carry roots or managed owner records; route state contains only scalar identities and revision observations. |
| callback/host boundary | Host-call state is published as `Invoking` before access closes; result/failure is installed through an exact edge transition after callback return. |
| cancellation and abandonment | Producer/follower/client records terminalize or release their external owner; lazy-owned partial work remains reachable from the lazy rather than being discarded with the last route. |
| terminal publication | Only the outer client/lazy/promise/reflection/spark/net owner publishes; retirement occurs after the terminal root/cache/assignment is installed. |
| unwind | Managed state remains structurally installed and traceable; callable claims restore the exact predecessor unless a successor was already published. |
| collection between polls/workers | Focused ordinary/aggressive WHNF and checkpoint matrices cover managed, lazy-owned, and net-owned states, including route loss and worker handoff. |
| unreachable cycles | Lazy/checkpoint/source and construction cycles are reclaimed once external roots drop; scalar diagnostic IDs do not retain them. |

Fresh accepted evidence is now:

```text
cargo test -q --lib whnf
    85 passed
cargo test -q --features aggressive-gc-verification --lib whnf
    86 passed
cargo test -q --lib checkpoint
    60 passed
cargo test -q --features aggressive-gc-verification --lib checkpoint
    62 passed
cargo test -q --lib inventory
    119 passed
cargo test -q --features aggressive-gc-verification --lib inventory
    119 passed
```

The checkpoint matrix includes forced collection after builder handoffs and
after callable-checkpoint installation, route loss, cross-worker resumption,
poison/unwind, cancellation/abandonment, and lazy/source cycle reclamation.
The two-worker linear-payload test uses an explicit barrier. Passing repeated
parallel runs is not cited as evidence.

### What the inventories prove—and do not prove

The checkpoint, access, root-publication, active-owner, durable-owner,
recursive-identity, containment, core-net-owner, and persistent-edge ledgers
are source-backed drift alarms. Compile-exhaustive destructuring functions and
enum visitor matches additionally force a compiler error when a represented
owner/state variant changes. Together with aggressive collection fixtures,
they make the focused proof materially stronger than source counting alone.

They are not a type-level theorem that every future raw value is owned. In
particular:

- `trace_compatibility_value_managed_edges` remains the transitional visitor
  for recursive raw `Value` fields until the parent representation migration;
- D.2c proves that the evaluator/WHNF partition has no raw-value API violation,
  but D.2d-D.2g still own 249 violation functions and three derived-trait
  violations elsewhere;
- GCI11R-002E/F still own the general test-fixture and verification-schedule
  migrations; the four pulled-forward fixture repairs do not close those
  inventories; and
- P3 still reports persistent trait dependencies in parent compatibility
  carriers, so HR2 does not authorize P4.

The current collector is stop-the-world. Its mutator gate proves that
`Managed*CheckpointCell::Trace` cannot encounter a live evaluator holding the
cell mutex; `try_lock` therefore treats `WouldBlock` as an invariant failure.
That proof must not be carried into concurrent marking. WHNFHR-005 records the
already concrete CG0/CG1 replacement gate. Subject to that future boundary,
HR2 finds the present ownership and tracing model sound.

## HR3 — Resumption and semantic equivalence

HR3 is complete. No state family rebuilds its completed semantic prefix merely
because a dependency, budget boundary, host call, or scheduler handoff is
encountered. A transaction may deliberately restart an attempt after a
previously observed location is disturbed; that is optimistic-transaction
semantics, not WHNF replay.

### Suspension-family matrix

| Family | Retained semantic position | Forced evidence | Disposition |
| --- | --- | --- | --- |
| budget yield | focus, exact continuation stack/cursors, specialized-machine phase, and mutable remaining budget | one-unit client/reflection/spark tests; deep application/fixpoint/lazy/promise small-stack cases; borrowed multi-frame yield | Exact state is requeued without a dependency or subscription. |
| lazy demand | lazy-owned checkpoint or rooted outer WHNF state plus exact lazy ID/source owner | route-loss, last-subscriber, forwarding-chain, cross-worker, and unreachable-cycle fixtures | One canonical producer continues; only the lazy owner installs cache. |
| unassigned/assigned promise | promise breadcrumb, follower computation, and immutable assignment root | exact promise resumption, deep aliases, resolver completion, task terminal matrix | Unassigned promise remains retryable; assignment resumes rather than caches absence/error. |
| reflection gate | hosted request phase, application checkpoint, reservation/activation permit, and completion promise | exact net function/call/operator fixtures, one-dispatch counters, cancellation/failure cases | Activation is not repeated; only reflection owner publishes completion. |
| metadata reflection | carrier/update phase and shared reflection task | inert-until-demand, shared-task resumption, cancellation/failure, seq/spark demand | Update task is launched once and resumed through the same managed carrier. |
| host/import callback | `Invoking` producer captures or `After` result/failure checkpoint | yield on both sides, interruption/route loss, failure publication, lazy result; compiler root-only suspension and import provenance fixtures | Callback is never reinvoked after result publication. Import loaders use this same host-call boundary. |
| callable/net computation | `NetWhnfState`, exact pair/generation, driver worklist/frontier | callable checkpoint no-replay, promise/cycle/terminal matrix, cursor deferral, worker contention, profiling signature | Raw `Net` is already WHNF; explicit callable/net work owns all subsequent progress. |
| application/dictionary application | supplied arguments, next index, effect payload/list cursor, optional member | promised callable/member, partial builtin, nested undefined, effect payload fixtures | Completed arguments and tag recognition are not repeated. |
| key/path/dictionary access | base/current value, path/key conversion submachine, precise member/chunk cursor | deferred middle path, recursive key, dictionary take/emptiness/union/merge fixtures | Prefix access and completed members remain installed. |
| list observation/transformation | front/back walk, completed chunks/items, callable/source phase | list-at, unsnoc/split-end, text-lines, map, concat, comparison, lazy-tail fixtures | Strict spine progress is retained; unrelated list side is not forced. |
| object/fixpoint/composition | linearization/mix/definition stacks and child demand phase | mixin-once counters, nested specs, composed defs, override/from-dict/with-defs cases | Completed mix/definition stages are not reconstructed. |
| annotation/assertion/failure context | selected annotation phase, payload/message/context child state | binary/assert-unit/provenance and suspended-request failure fixtures | Interrupted and uninterrupted failures retain the same ordered semantic contexts. |
| `.alt`/`.cut` and transactional retry | effect-machine branch/cut frames, whole-cut observation journal, retry checkpoint | lazy cut choice, prior-commit counter, transactional path suspension and revalidation cases | A live attempt retains prior branch reads. A disturbed failed attempt intentionally restarts under transaction policy. |
| reset/shift | hidden reset stack in user state plus decoder/control phase | lazy reset/shift keys, captured cut, decoder structural layers, whole-state restore fixture | Capture/publish happens only after the key/stack is ready; restore resumes the serialized checkpoint. |
| task/exit/fixpoint effects | reflection effect task phase, task handle/root records, exit-message child WHNF, fixpoint promise | task state path, cancellation/abandonment/failure, exit message, initial fixpoint/reset-stack cases | Monadic task machine retains control; nested WHNF child does not replace task semantics. |
| pure net construction | fixed-arity builder state, compact reverse program, hidden reset/sequence state | builder operand promises, copy/wire/path/state checkpoints, route-loss and compact replay counters | Effect program is processed once; only the selected strict netlist is replayed into topology. |

### Counter and equivalence quality

The no-replay evidence is not inferred from a stable whole-suite total. Tests
which claim exactly-once behavior inject a counted lazy/host/reflection
boundary, force the dependency or route loss, and assert the counter at the
semantic boundary. Tests for list/object/path work likewise place the counter
in the completed prefix rather than only in the final result. Callable-net and
pure-construction cases additionally assert exact checkpoint/reduction/driver
events. Failure-context tests inspect ordered structured contexts or compare
the suspended result with the uninterrupted oracle.

Fresh ordinary evidence includes:

```text
cargo test -q --lib resumes                 # 81 passed
cargo test -q --lib without_replay          # 41 passed
cargo test -q --lib route_loss              # 21 passed
cargo test -q --lib host_call               # 11 passed
cargo test -q --lib reflection_gate         # 11 passed
cargo test -q --lib metadata_reflection     # 5 passed
```

Additional exact probes passed for compiler/import suspension, failure
contexts, transaction revalidation, reset-stack decoding, lazy cut choice,
reset/shift key suspension, exit-message resumption, task-state paths, and
list-effect sequence/cut/fix boundaries. HR2's 86-test aggressive WHNF and
62-test aggressive checkpoint filters cover the managed lifetime dimension;
the known repository-wide aggressive fixture/schedule gaps remain assigned to
the parent plan rather than being mislabeled as semantic replay failures.

The only bounded/non-suspending exceptions are immediate scalar/tag/key
operations and raw-net WHNF recognition. They neither call a host nor open a
child computation. HR3 therefore accounts for every suspendable family named
by the review plan without finding a new remediation item.

## HR4 — Stack, budget, scheduling, and concurrency

HR4 is complete. Current semantic and liveness claims are backed by bounded
machine structure and forced interleavings rather than scheduler luck. The
remaining shared-notification imprecision is a measured performance choice,
not an authority or correctness mechanism.

### Stack and budget closure

The regional WHNF driver, every continuation family, list front/back walkers,
object/comparison/access submachines, callable-net driver, and reflection
request decoders retain their own explicit stack/cursor. The W7 source census
rejects a new user-sized recursive production call unless it is classified.
Retained Rust recursion is limited to logarithmic balanced persistent
containers or fixed-shape helpers; neither scales with arbitrary evaluator
delegation depth.

Budgets are mutable shared counters across nested machine polls. A transition
spends before beginning the next semantic step; exhaustion returns the same
checkpoint without fabricating a wait token. Client, reflection, and spark
yield release their claim and requeue the same FIFO record. Net-reduction and
evaluator budgets remain distinct where their units differ, with explicit
translation at their boundary.

Fresh W7 evidence remains 21 ordinary/aggressive tests, 14 W7B small-stack
tests, and five W7C one-unit scheduling tests. Those tests exercise 4,096-deep
delegation/application/fixpoint/lazy/promise paths, cross-worker checkpoint
resumption, zero/one/many budgets, and exact one-unit root/allocation
stability.

### Role and ownership audit

| Concern | Current rule | Evidence and disposition |
| --- | --- | --- |
| foreground client work | only the retaining client-side driver may claim a foreground demand | worker/runtime selectors explicitly reject foreground records; bounded background-pump tests confirm exclusion. |
| background reflection/task work | workers and optional client background pump may claim it | bounded pump reports runnable/busy/stable and never waits inside one bounded call. |
| sparks | executor-owned best-effort work; a foreground client may share its canonical lazy producer but does not claim the spark record | client/spark sharing and quiescent abandonment fixtures preserve the lazy checkpoint. |
| last subscriber | dropping the last route removes scheduling demand, not semantic partial work reachable through its lazy | route-loss, lazy checkpoint, and unreachable-cycle fixtures distinguish retention from collection. |
| exact producer | lazy producer, task-owned promise producer, or coordinator record is followed; resolver-owned promises are terminal dependencies with no invented producer | foreground-route promise ownership and cross-session route fixtures. |
| cycles | pure lazy cycles are terminalized; promise-inclusive cycles remain waits because an external resolver may still complete them | forced pure/mixed cycle tests and exact foreground-search cycle detection. |
| runtime-global work generation | wake/readiness hint only | exact subscription records decide semantic readiness; a broad condition variable may wake another waiter class but cannot authorize a claim. |
| exact route zipper | caller-local optimistic path | every frame is validated beneath mutation admission; authoritative full search handles any hazard or mismatch. |

### Atomic subscription and exact-route evidence

The subscription protocol covers completion before registration, during the
registration hook, and after registration. The “during” tests use barriers and
a spawned completer to force the contested window; task and spark families are
both covered. Guarded completion updates authoritative state while mutation
admission is held and defers the scheduler notification until after release.
Duplicate registration delivery is rejected without advancing the work
generation.

The exact foreground zipper is similarly non-authoritative. Forced probes
show that mutation admission and coordinator state remain held from validation
through claim. Channels pause release at the exact publication seam while an
unrelated mutation is committed; reconciliation then rejects the stale route
and rebuilds from the root. Other fixtures force child-first completion,
parent reblock at a new epoch, ancestor cancellation/retirement, observation
wake, busy child handoff, unrelated neutral mutations, and independent
revision wrap. Route counters distinguish validation, invalidation, retired
work, and cold fallback.

Fresh focused results are:

```text
cargo test -q --lib subscription       # 14 passed
cargo test -q --lib exact_route         # 8 passed
cargo test -q --lib background_pump     # 4 passed
cargo test -q --lib w7c                 # 5 passed
cargo test -q --lib claim_release       # 5 passed
cargo test -q --lib cursor_driver       # 2 passed
cargo test -q --lib normalization_batch # 6 passed
```

### Locks and the cursor exception

Ordinary semantic waits, callbacks, coordinator mutation, and publication all
occur after `EvaluationValueAccess` closes. Managed checkpoint mutexes cover
only one budget-bounded callback-free quantum. Coordinator state and mutation
admission are not held across callbacks, destruction, or notification.

Cursor WHNF remains the narrow exception: a thread holding the same-net
normalization lease may wait for a contending thread which currently owns the
productive active-pair/cursor work. The dependency is structural within a
closed net; drivers release each runtime before crossing to another net, and
normalization-batch release publishes one disturbance after maximal local
work. Forced release/waiter tests cover the handoff. This proof does not
authorize ordinary promise/lazy waits under access.

The reference collector's checkpoint-mutex proof and its concurrent-GC limit
are already isolated in WHNFHR-005. Shared lifecycle notification can still
wake a worker beside a parked client; W9 measured that behavior and retained
one condition variable because exact subscriptions and claim validation make
the wake harmless. HR5 assesses its cost. HR4 finds no current correctness
claim resting on uncontrolled scheduling.

## HR5 — Complexity and performance accounting

HR5 is complete. Every retained WHNF-specific cost has a current semantic,
ownership, or measurement role. The audit found no decision-only instrument or
obsolete compatibility wrapper which should be removed before parent GC work.
WHNFHR-006 records the two accepted bootstrap costs which remain material.

### Representation and access costs

| Cost | Current behavior | Evidence and disposition |
| --- | --- | --- |
| seed promotion | one input `RuntimeValueRoot` becomes one aggregate managed checkpoint root | small and 32-frame fixtures both retain exactly one steady-state root. |
| ordinary delegation | focus is replaced in regional state | 4,096-step and root-counter fixtures show no per-step root or managed allocation. |
| multi-frame yield | existing vectors/arrays are retained in place | container-identity and no-projection/root fixtures pass. |
| structured application | arguments are batched into one continuation | application fixture rejects an intermediate root per supplied argument. |
| one-unit polling | first real step allocates at most the one required checkpoint; later steps reuse it | zero/one/many budget fixture compares root-registration and managed-slot counters. |
| nested access | current D.2c gateways accept caller-supplied `EvaluationValueAccess` and use higher-ranked regions | source-backed access inventory reports no unclassified nested D.2c introduction. |
| compatibility facade | `evaluate_compatibility_whnf` projects raw input to a root and terminal root back to raw | deliberately expensive transitional parent-plan violation; D.2d-D.2g own its call sites and removal. It is not used to simplify canonical WHNF internals. |
| checkpoint transition | one mutex acquisition and complete before/after edge walk per bounded published quantum | exact SATB/reference-collector correctness boundary; CG0 must compare it with `RootFrame`, not silently add parallel roots. |
| callable checkpoint | boxed `NetWhnfState` occupies one pointer in the runtime node | x86-64 layout latch records `Value` 64 B, continuation 160 B, state 128 B; boxing avoids enlarging every runtime node. |
| specialized checkpoint | allocation occurs only when its lazy source actually crosses a resumable boundary | terminal/immediate paths install result/cache directly; source inventories latch every allocator/observer. |

The focused cost probes all pass in the current tree:

```text
small_and_large_seed_promotions_use_one_managed_root
borrowed_multi_frame_yield_preserves_containers_without_projection_or_roots
exact_one_unit_polls_retain_one_checkpoint_without_root_or_allocation_churn
function_application_batches_arguments_without_intermediate_roots
regional_whnf_and_callable_checkpoint_layout_baseline
```

### Scheduler and source-shaped work

W6G.4 localized the post-W4 regression to repeated exact-producer-chain
rediscovery. Its W6G4R-002 baseline performed 9,374 complete searches, visited
1,418,995 records, and cold-fell back 9,356 times. W9 retained a guarded
caller-local route and split broad scheduler movement from exact-route hazards.
On the identical duplicate-symbol source fixture it now performs 28,855 fast
handoffs, 18 complete one-record searches, 18 record visits, and zero cold
fallbacks. The pathologically retained route still reaches depth about 573
and roughly 42 KiB, but common reconciliation is O(1) and full traversal is
authoritative only after an actual mismatch/hazard.

W9 also reduced `notify_all` calls from 88,579 to 58,798 (33.62%) by
suppressing broadcasts for publications which no parked host class can use.
One broad condition variable remains. A worker-enabled profile could justify a
future split; the current zero-worker fixture cannot, and forced mixed-waiter
tests prove correctness meanwhile.

Callgrind fell from the W6G4R-002 3,339,481,894 instructions to
2,562,993,262 (-23.25%). Warm native release time improved from 1.11-1.14 s to
0.93-0.94 s and debug from 14.18-14.34 s to 12.93-12.98 s. Native time is only
corroboration. More importantly, the exact interaction-net signature and
duplicate-symbol diagnostic are unchanged, so the reduction is scheduler work
rather than omitted semantics.

### Retained profiling and deferred optimization

Temporary histograms, per-kind decision snapshots, poll-origin scopes, and
DHAT/Callgrind instrumentation were removed after their decisions. The code
retains only:

- the exhaustive coordinator mutation-class boundary, because it prevents a
  future transition from bypassing route-hazard accounting;
- exact route/handoff/fallback/notification counters under tests or the
  static `interaction-net-profiling` feature; and
- interaction-net reduction/driver counters under the same static feature.

Ordinary builds have no profiling observer. The named profiling script is a
small regression suite rather than a second full test run.

Pure standard-effect access fusion remains deliberately extracted to
`PureEffectAccessFusion_2026-09-23.md`, after value-representation refinement.
List-spine representation, JIT/normalization annotations, moving/concurrent GC,
and public resumable `Evaluation` ergonomics likewise remain separate plans.
None is required to make the current WHNF boundary correct.

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

### WHNFHR-001 — Resolved: two WHNF fixtures published unrooted managed identities

**Severity:** medium verification-boundary defect; no current evidence of a
production WHNF ownership defect.

The ordinary focused WHNF suite passes 85 tests, while a fresh
`cargo test -q --features aggressive-gc-verification --lib whnf` run fails in:

- `eval::tests::demanded_forwarding_chain_caches_whnf_in_every_lazy_member`,
  after constructing each member of one forwarding chain through a separate
  self-opening test factory call and retaining only raw lazy facades; and
- `evaluation::tests::abandoned_whnf_producer_resumes_from_the_lazy_owned_checkpoint`,
  after separately constructing an unrooted promise, capturing it in an opaque
  test thunk, constructing an unrooted lazy, and only then attempting to root
  that lazy in another access region.

The failures are respectively `managed pointer does not identify an allocated
value` and `managed pointer does not belong to this heap`. The second failure's
backtrace reaches `LazyValue::root` before coordinator admission or checkpoint
installation. Both therefore fail at the already-declared test-fixture
publication boundary rather than while tracing or resuming `WhnfState`.
GCI11R-002E already owns this class of `SameRuntimeFixture` and evaluator lazy/
promise migration, but leaving these exact fixtures broken would make the HR1
aggressive matrix and HR2 ownership proof unnecessarily conditional.

**Immediate remediation checkpoints:**

1. `WHNFHR-001A` constructs the forwarding chain inside one matching access
   region, publishes a durable outer root before leaving, and evaluates that
   root. The root must keep every source/cache edge observable for the rest of
   the fixture without adding one root per chain member.
2. `WHNFHR-001B` uses the existing rooted promise and rooted semantic-lazy
   fixture builders, retains the promise owner required by the opaque test
   thunk, and uses the already-published lazy root for admission. It must not
   alter production checkpoint or coordinator behavior.
3. Rerun both tests individually in ordinary and aggressive modes, then rerun
   the complete ordinary/aggressive `--lib whnf` filters. Only after all four
   gates pass may HR1 treat focused aggressive WHNF behavior as evidence.
4. `WHNFHR-001C` assigns the new bounded-access, root-publication, and
   mutator-local core-net duplication sites to the executable access and
   persistent-edge inventories. The full ordinary/aggressive inventory filter
   must reject any unclassified delta and then pass.

This is a deliberate early pull-forward of two narrow GCI11R-002E fixtures,
not closure of that parent checkpoint. The general test-fixture inventory and
migration remain in 002E.

**Resolution:** WHNFHR-001A now constructs the forwarding function and all
three lazy cells inside one access region, publishes one outer runtime root,
and evaluates that root. WHNFHR-001B now retains the existing rooted promise
and lazy fixture owners while testing route abandonment and checkpoint
resumption. WHNFHR-001C classifies the two new access entries, one runtime-root
publication, and one test-only mutator-local core-net duplicate. Both tests
pass individually in ordinary and aggressive modes; the complete focused
filters pass 85 ordinary and 86 aggressive tests. The ordinary/aggressive
inventory gates each pass 119 tests.

### WHNFHR-002 — Resolved: the focused plan status predated W9 closure

**Severity:** low documentation drift.

The file-level status still described W9C.1-E as planned even though the W9
section, dated review, current code, and profiling suite all record W9 as
complete. The W4E section likewise retained its original `pending` label below
a W4 review paragraph which records its later completion. HR1 also found one
duplicated sentence in the original architecture prose. The status now records
W0-W9 as implemented, W4E points to its dated closing review, and the plan
identifies this holistic review as the one remaining completion-criterion-10
obligation; the duplicate was removed. No semantic statement changed.

The stale parent-plan inventory counts above remain evidence until HR6-HR7
audit their ownership claims; they are not yet assigned severity or resolution.

### WHNFHR-003 — Resolved: two checkpoint fixtures retained unrooted recursive owners

**Severity:** medium verification-boundary defect; no production ownership
conclusion yet.

The first complete ordinary `--lib checkpoint` filter run failed in
`hidden_builder_whole_state_checkpoint_restores_reset_scope` while attempting
to access a managed pointer already pending finalization. The fixture currently
crosses three value-access boundaries with raw recursive values:

1. the initial builder state and reset operation leave `with_access` unrooted;
2. `run_builder_at` returns a decoded checkpoint and state as raw values after
   the compatibility evaluator has projected its result root; and
3. the restore operation embeds that raw checkpoint, then again crosses an
   access/evaluation boundary unrooted.

An isolated run can pass because it does not necessarily collect at one of
those gaps. That is not concurrency evidence and cannot justify accepting the
fixture. The failure occurs at collector root/access validation, not during
checkpoint trace or restoration, so the first hypothesis is the existing
GCI11R-002E test-publication class rather than a production checkpoint defect.

After repairing and forcing that case, the aggressive checkpoint filter also
failed deterministically in
`two_workers_contend_for_one_linear_checkpoint_payload`. Its setup constructs
a managed core net through the test-only non-rooting
`instantiate_core_net`, leaves the allocation region, and opens a second
region to discover and claim the active pair. Aggressive collection therefore
retires the net before either worker or the callable checkpoint can own it;
the backtrace fails in `CoreRuntimeNet::access` during setup. The worker
interlock itself is not reached.

**Remediation checkpoints:**

1. `WHNFHR-003A` replaces the helper path for the builder fixture with rooted input
   and result handoffs. Construction, list selection, result decoding, and
   restore construction must each happen while matching access is held; only
   `RuntimeValueRoot` values may cross between those regions.
2. The repaired fixture explicitly collects after initial publication and
   after the first checkpoint result is published, forcing both former gaps.
3. `WHNFHR-003B` constructs the callable-checkpoint net and registered root in
   one access region, retains that root for all worker/interlock observations,
   and adds a forced collection before either worker claims the payload. The
   deliberate overlapping-mutator interlock uses a private `NoAuto` value
   domain: verification-only collection-before-every-entry would otherwise
   make the second entry wait for the first mutator while the first worker is
   intentionally waiting for the second at the barrier. Production is also
   `NoAuto`; GCI11R-002F separately owns general schedule-fixture migration.
4. Run both exact fixtures ordinarily and aggressively, then the complete
   ordinary/aggressive checkpoint filters. Only those forced runs may become
   HR2 ownership evidence.
5. `WHNFHR-003C` updates the access/root-publication inventories and reruns the
   complete ordinary/aggressive inventory gates if the new helper changes
   their source-backed ledgers.

If the rooted fixture still reaches pending finalization or trace failure, stop
HR2 and reclassify this as a production owner/transition defect. The broader
fixture migration remains GCI11R-002E work.

**Resolution:** WHNFHR-003A now publishes all three builder handoffs as
`RuntimeValueRoot` values and forces collection after initial construction,
after checkpoint capture, and after restore construction. WHNFHR-003B builds
and roots the callable-checkpoint net in one access region, forces collection,
then runs the exact two-worker interlock in a private `NoAuto` domain. Both
exact fixtures pass ordinarily and aggressively; the complete checkpoint
filters pass 60 ordinary and 62 aggressive tests. WHNFHR-003C classified five
new test root-publication entries and the single new test access entry; both
119-test inventory filters pass ordinarily and aggressively.

### WHNFHR-004 — Resolved: blocked-client fixture asked the background pump to run foreground work

**Severity:** low test-policy drift; no production scheduling defect.

The aggressive checkpoint filter also fails in
`blocked_client_checkpoint_survives_collection_until_promise_assignment` after
the promise assignment. The fixture calls `EvaluationRuntime::pump_until_stable`
and then expects a private client-demand handle to contain its result. Since
W6G.1, that runtime API deliberately pumps background reflection/task/spark
lifecycle work and never claims a foreground client demand. Reaching a stable
background instant while the foreground handle remains ready is therefore the
specified result, not evidence that collection lost its checkpoint.

`WHNFHR-004A` must retain the forced collection and promise assignment, then
advance the exact foreground client demand through the existing test driver
until it publishes. The exact fixture and complete aggressive checkpoint
filter must pass. Reintroducing foreground claims into
`pump_until_stable` is explicitly out of scope because it would reverse the
reviewed client/worker ownership model.

**Resolution:** the fixture now uses the existing exact foreground test driver
after assigning the promise. It retains the forced collection and verifies the
same terminal value. The exact aggressive fixture and complete aggressive
checkpoint filter pass. No scheduler or public runtime behavior changed.

### WHNFHR-005 — Resolved by CG0/CG1: checkpoint tracing assumes stop-the-world quiescence

**Severity:** medium future collector interlock; no defect under the selected
reference collector.

Every lock-bearing managed checkpoint visitor uses `try_lock` and treats an
unpoisoned busy mutex as an invariant failure. That is correct today because a
collection holds exclusive heap admission after all mutators leave; evaluator
mutation can only occur under a mutator. Poison recovery is observational and
retains the structurally installed edges. A concurrent marker, however, could
otherwise contend with or wait behind a participant holding the cell guard,
and cannot inherit this proof.

This obligation is resolved by named future checkpoints in
[`ConcurrentGarbageCollection_2026-08-28.md`](../plans/ConcurrentGarbageCollection_2026-08-28.md):

- CG0 inventories every managed trace implementation which locks or assumes
  heap-wide quiescence, classifies WHNF ownership as external versus
  lazy-owned, and chooses either a trace-immediate `RootFrame` or a reviewed
  collector-coherent snapshot for each family;
- CG1 integrates frame admission/retirement with epoch initiation and forces
  initiation immediately before, during, and after a bounded WHNF quantum;
  and
- the hard gate forbids concurrent marking while any managed family still
  relies on the reference collector's quiescence assumption.

The selected baseline remains one callback-free, budget-bounded quantum under
the managed-cell mutex. No current code change is warranted before CG0 because
introducing a parallel root/frame now would risk retaining lazy-owner cycles.
Exit is the CG0 inventory and representation decision plus CG1's forced
handshake matrix; this finding does not block D.2d, P4 accounting, or current
STW operation, but it blocks concurrent marking.

### WHNFHR-006 — Accepted: aggregate edge walks and exact-route depth remain bootstrap costs

**Severity:** medium measured performance, none for current correctness.

The aggregate managed checkpoint deliberately walks its complete edge set
before and after each published bounded quantum, and the client-local exact
route retains O(depth) work IDs/observations. The former is the simple exact
barrier for the reference collector; the latter is a validated hint whose
common handoff is O(1) after W9 and whose cold traversal remains authoritative.
Removing either without its replacement proof would weaken ownership or
scheduling correctness.

The current dispositions are:

- CG0 measures one aggregate leaving-edge walk per WHNF quantum against a
  trace-immediate `RootFrame` or coherent managed snapshot before concurrent
  marking changes the boundary;
- value-representation and pure-effect-fusion work may later reduce state size
  and route depth, but must preserve the canonical owner and exact fallback;
  and
- a route-storage change requires a source-shaped memory/profile result, not
  only an aesthetic preference for a smaller record.

This accepted cost does not block D.2d, P4 accounting, or Gate G3 under the
reference collector. Reopen it if edge-walk or retained-route memory dominates
a post-representation profile, or before concurrent marking adopts a barrier.
