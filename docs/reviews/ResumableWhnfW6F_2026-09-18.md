# Resumable WHNF Phases W6A-W6F Review — 2026-09-18

Baseline: W6F.7 closes at `572aeef`; final dispatcher closure and checkpoint
reconciliation close at `f49ff24` and `3559646`.

Status: review complete. W6A-W6F close the pure builtin conversion without an
open correctness defect. W6G is intentionally a separate performance and
representation phase. The temporary same-session scheduling policy remains
open under W6G.1 rather than becoming an implicit W6 semantic.

## Scope

This is the mandatory converted-call-graph review required by
[`ResumableWhnfEvaluation_2026-09-12.md`](../plans/ResumableWhnfEvaluation_2026-09-12.md).
It audits W6A-W6F while expressly excluding W6G:

- the final builtin saturation, lazy-source, and durable-machine call graph;
- raw-value, managed-access, root-publication, durable-owner, and WHNF census
  closure;
- result ownership across yield, dependency, failure, and publication;
- propagation of the one borrowed poll budget through nested work;
- forced suspension and no-replay coverage for each converted family;
- remaining direct-evaluator compatibility and its W8 owner;
- the temporary per-session scheduler rule inherited from W3; and
- drift in W6G-W8 after the completed implementation.

## Outcome

Every demand-capable saturated builtin now installs durable work. The
lazy-source owner roots its operands once, selects `BuiltinTaskMachine`, and
retains family-specific state while ordinary `WhnfComputation` children yield
or wait. Immediate append and list-effect construction remain regional and
callback-free: `apply_builtin_in` receives the caller's existing
`EvaluationValueAccess`, and the source owner roots the result before closing
that region.

The object and net-construction tails obey the same boundary. Object
composition and recursive override retain explicit application and persistent
dictionary frames. Net construction retains its isolated search journal,
then moves the selected branch into an `Exposed` phase with one rooted WHNF
owner; demanding an exposed port cannot replay search or lose branch state.
No converted family retains a raw `Value` across a poll or performs a callback,
wait, coordinator transition, or scheduler publication under managed access.

The direct evaluator has not yet been deleted. Its in-tree runtime consumers
are gone, while the public compatibility facade and test helpers remain
source-latched to W8. That is an intentional compatibility boundary, not an
unfinished W6 builtin family.

## Inventory Accounting

The source-backed inventories agree on the completed boundary:

| Inventory | Reviewed W6A-W6F result |
|---|---|
| parent D.2c families | every W6 family is zero; seven `ValueDemand` compatibility declarations remain assigned to W8 |
| complete raw-value API | 440 declarations: 140 regional access, 28 collector primitives, 262 violations, 7 regional type aliases, 3 derived-trait violations |
| WHNF census | 188 exact occurrences with only the latched W8 direct-demand entries and reviewed structural/orchestration work |
| builtin access | dispatcher accepts `EvaluationValueAccess`; no context downgrade or nested access |
| net construction | no direct evaluator callback; selected journal and exposed-port demand have explicit durable owners |

The final W6C.1b dispatcher reclassification accounts for the difference
between W6F.7's intermediate 263 violations and the reviewed total of 262.
There is no unexplained declaration delta.

**Later reconciliation (2026-09-23):** the table remains the historical
post-W6F baseline. Completed W6G work moves the current W0B census to 207
occurrences and leaves six, rather than seven, declarations assigned to
`W8ValueCompatibility`. The current exact accounting and its verification are
recorded by
[`W6G.5a`](../plans/ResumableWhnfEvaluation_2026-09-12.md#w6g5a--integrated-verification-and-accounting).

## Ownership and Budget Audit

One `EvaluationStepBudget` is created at an outer claimed task, client demand,
spark, or explicit synchronous convenience boundary. Nested WHNF, reflection,
access, list, object, strategy, and net-construction work borrows that same
mutable token. No converted child recreates the parent's remaining allowance.
The synchronous reflection/search facade remains an explicit outer integer
policy, as recorded by W6B.0; it is not a nested W6 machine budget.

All W6 durable owners retain roots only across orchestration boundaries.
Within one admitted regional step they use raw values under matching access.
Yield, dependency, failure, and cancellation publish either the complete next
checkpoint or a terminal rooted result; no empty owner state is externally
claimable. Immediate dispatcher results are rooted before access closes.

## Forced Verification Assessment

The completed checkpoints include deterministic interruption at representative
child demands for numeric/comparison operands, dictionary and list traversal,
patterns, annotations, metadata, effects, object application/override, net
normalization, isolated net search, and exposed-port demand. Search and
callback fixtures count invocation and selected-branch publication so a replay
cannot pass merely by returning the same value. Budget fixtures force exact
zero/one/many boundaries and assert spent plus remaining equals the original
grant.

Concurrency-sensitive scheduler tests place claims and terminal publication
on both sides of the disputed transition. Uncontrolled repetition is used
only as stress evidence. The retained same-session serialization test is an
exact characterization of current policy, not proof that the policy should
survive W6G.

## Drift Assessment

### Intentional and justified

1. **Immediate dispatch survived as a regional leaf.** Manufacturing durable
   machines for append and recipe construction would add ownership traffic
   without introducing a possible suspension. The caller-provided access plus
   pre-close root publication is the smaller boundary.
2. **Net construction has a search phase and an exposed-value phase.** Search
   completion does not imply that the selected port is already WHNF. Keeping
   the journal beside a rooted WHNF owner preserves both branch identity and
   later resumability.
3. **Compatibility removal remains W8 work.** W6 proved that runtime builtin
   paths no longer require direct evaluation. Deleting the public/test facade
   together with its remaining seven `ValueDemand` declarations is a distinct
   API and inventory change.

### Future phases

- W6G now stands as a separate major phase. It owns measured scheduler/access
  overhead, aggregate durable WHNF state, and the final disposition of
  same-session serialization. **Later disposition (2026-09-23):** regional
  standard-effect fusion was extracted to the deferred
  [Pure Effect Access Fusion plan](../plans/PureEffectAccessFusion_2026-09-23.md)
  after Value Representation Refinement.
- W7 remains coherent: it owns the residual recursive-call audit, small-stack
  verification, and the final whole-program budget/fairness contract against
  the retained bounded effect path and completed durable-state shape.
- W8 remains coherent: it owns retryable-halt simplification, direct evaluator
  retirement, final inventory/documentation closure, and Gate G3 handoff.

## Findings

### WHNFW6FR-001 — Resolved: current architecture described the pre-W6 dispatcher

**Severity:** documentation and auditability

**Status:** resolved during review

The evaluation architecture and source map still said `apply_builtin_in`
received an `EvaluatorStepContext`, listed demand-capable families as scoped
dispatcher work, and said builtin families still awaited W6. They now describe
the caller-provided regional access, durable builtin owner, and rooted
immediate-result handoff. The direct admission comment now names its actual W8
compatibility owner.

### WHNFW6FR-002 — Deferred: temporary same-session serialization remains live

**Severity:** medium performance and architecture

**Status:** assigned to W6G.1

Global ready-task selection and client-demand selection still scan the owning
session and reject a second ordinary machine while one is running. Exact
dependency claims bypass the rule. This is the W3 containment policy, not a
Glam semantic, and the recursive builtin evaluator which motivated it is now
gone.

W6G.1 must replace the rule with explicit foreground, worker-background, and
background-drain claim authority. Role-specific demand roots must retain
independent continuations while session-neutral canonical lazy producers
retain partial source work in the managed lazy itself, shared across client,
reflection, and spark roots. Coordinator producer routes remain transient and
demand-backed.
The repair must preserve terminal-publication, lost-wakeup,
no-false-quiescence, and exact-dependency behavior. It must not replace the
scan with the previously rejected running-session index or a sticky
execution-lane field on each deferred producer. Residual performance
measurement belongs separately to W6G.4.

### WHNFW6FR-003 — Resolved: W6G and the converted-call-graph review shared an ambiguous boundary

**Severity:** planning clarity

**Status:** resolved during review

The plan nested W6G under W6 and placed the mandatory converted-call-graph
review after all performance work, even though the review is the gate for
declaring W6A-W6F conversion complete. W6G is now a separate major phase, this
review closes W6A-W6F, and W6G has its own mandatory review before W7.

## Verification

The implementation baseline immediately preceding this review passed:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
scripts/check-interaction-net-profiling.sh
```

The final ordinary suite reported 1,696 active library tests and 2 ignored,
plus all integration and executable partitions. Focused ordinary and
`aggressive-gc-verification` fixtures covered append, list effects, object/net
families, isolated construction, and exposed-port suspension. This review
reran the exact raw-value, WHNF, builtin-access, and net-construction inventory
gates in both ordinary and `aggressive-gc-verification` modes after its
documentation corrections. The routine suite and interaction-net profiling
gate also pass on the reviewed tree.
