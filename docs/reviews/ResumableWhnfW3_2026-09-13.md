# Resumable WHNF Phase W3 Review — 2026-09-13

Baseline: `a486553`, including completed W3 and the W4C work pulled forward by
its dependency review.

Status: complete. W3 now gives every ordinary production lazy-source family
except the explicitly deferred builtin, reflection, and host boundaries an
inspectable pollable owner. The review found and resolved three scheduler/net
handoff defects exposed by the new machines. No open design question blocks
W4. One temporary scheduling containment and one test-cost limitation remain
explicit follow-ups rather than hidden semantics.

## Scope

This is the mandatory extra-thorough post-W3 review required by
[`ResumableWhnfEvaluation_2026-09-12.md`](../plans/ResumableWhnfEvaluation_2026-09-12.md).
It audits:

- every `LazySource` variant and its current production dispatcher;
- every compatibility call still reachable through `produce_lazy_source_in`;
- the durable roots and scalar identities retained by each W3 owner;
- yield, dependency, completion, failure, contention, and wake handoffs;
- the intentional W4C-before-W3 reorder;
- tests which force the relevant orderings rather than relying on repetition;
  and
- drift in W4-W8 relative to the implementation now in place.

The review does not certify the remaining reflection or host-call conversion,
the general builtin conversion in W6, or final Rust-stack closure. Those are
still explicit later phases.

## Outcome

The implemented design has one durable owner per migrated source:

- ordinary application and function fixpoints use typed `WhnfComputation`
  application checkpoints;
- static access uses a typed WHNF access checkpoint, while computed access and
  recursive key/path conversion use `AccessMachine`;
- deferred list chunks use `ListFrontMachine`, which retains the exact chunk
  and suffix without forcing strict leaves;
- object fixpoints use explicit C3 DFS and mixin-fold machines;
- list effects use a closed `ListEffectComputation` recipe and
  `ListEffectSourceMachine` rather than an opaque production callback;
- function calls and net computations use one persistent `NetWhnfMachine` and
  reusable `NetDriver`; and
- net construction remains in its existing effect interpreter because its
  terminal result is already the source result, not an additional WHNF
  extraction request.

No migrated source retains a Rust callback as its continuation. A returned
`Pending` or `Yielded` disposition leaves all semantic progress in roots and
typed phase fields. Managed access, interaction-net claims, and normalization
batch ownership end before that disposition is published.

## Lazy-Source Census

| Source family | Current owner | W3 disposition |
|---|---|---|
| `Error` | terminal lazy cache path | immediate/terminal |
| `ComputedFixpoint::Function` | application checkpoint | migrated |
| `ComputedFixpoint::ObjectInstance` | `ObjectFixpointMachine` | migrated |
| `Application` | application checkpoint | migrated |
| `Access` | static checkpoint or `AccessMachine` | migrated |
| `FunctionCall` | `NetWhnfMachine` | migrated with early W4C |
| `NetComputation` | `NetWhnfMachine` | migrated with early W4C |
| `NetConstruction` | `NetConstructionMachine` | existing pollable boundary; W4C closed |
| `ListEffectComputation` | `ListEffectSourceMachine` | migrated |
| `HostCall` | explicit `LazyTaskWork::HostCall` | external W4B boundary |
| `ReflectionTask` | source-owned compatibility evaluation | pending W4A |
| `Builtin` | source-owned compatibility evaluation | declared W6 exception |
| `SemanticComputation` | test-only callback fixture | excluded from production evidence |
| `SemanticThunk` | test-only callback fixture | excluded from production evidence |

`produce_lazy_source_in` is no longer reachable for production object,
application, access, list-effect, function-call, net-computation, or
net-construction work. Its production work is reflection and generic builtin
evaluation only. The test build additionally retains the two deliberately
opaque scheduler fixtures.

## Durable Ownership Accounting

`ListFrontMachine` owns the current list, deferred chunk computation, and
exact suffix as `RuntimeValueRoot`s. Its `LazyId` is scalar cycle provenance,
not semantic liveness. A strict head and tail are rooted only for the returned
handoff.

`AccessMachine` owns source arguments, current base/key state, path cursors,
and recursive conversion state through runtime roots. Completed `Key` values
are semantic data and contain no unrooted `Value` transport.

`ObjectLinearizationMachine` owns each pending spec, completed child
linearization, dependency tail, and seen named spec as roots. Its frame stack
is ordinary Rust control state with no scoped managed borrow. Anonymous IDs,
path indices, and C3 positions are scalar. `ObjectMixMachine` owns the base,
self marker, remaining specs/definition recipes, and at most one application
checkpoint. The original spec remains rooted until final `spec` installation.

`ListEffectSourceMachine` owns the exact operation/result list/continuation or
promise handle required by its selected recipe. Fix publication occurs only
after list-front polling returns and managed access has closed. Sequence child
thunks receive inspectable recipes; they do not capture the parent machine.

`NetWhnfMachine` owns the managed net request root and persistent driver.
Active-pair, cursor, and normalization guards remain regional. On semantic
parking or contention, the driver restores the exact popped work item before
returning.

The exact-root inventories, compatibility-edge visitors, recursive identity
inventory, and raw-value/access inventories were updated with the new closed
recipe and owner shapes. The full library inventory suite passes.

## Scheduling and Handoff Accounting

The W3 machines exposed three pre-existing or adjacent handoff gaps:

1. A dependency could be published while its deferred producer's machine was
   detached and running. A later cooperative yield then restored it as
   dormant and lost that authoritative demand. Deferred work now latches
   demand received while running and consumes the latch during release.
2. `NetDriver` popped a work item before attempting normalization-batch
   admission. Contention at that admission boundary returned without restoring
   the uninspected item. It now requeues the exact item before handing the
   contention token to the scheduler.
3. The temporary one-ordinary-machine-per-session rule could prevent a ready
   task from being admitted while another machine ran. If the requested chain
   ended in a broad runtime observation, `pump_demand` could misreport the
   instant as `NoProgress`. A running same-session owner, or any running
   runtime owner capable of disturbing that broad observation, now yields
   `Busy`. Patient evaluation waits for one generation change; ordinary direct
   evaluation still exposes a genuinely quiescent reflection gate as a
   retryable block.

The third correction deliberately distinguishes an already-racing wake from a
future external event. `retry_after_no_progress` rechecks terminal, claimable,
running, and generation state without waiting. The existing synchronous
reflection runner retains its separate ability to await a broad observation.

## Forced Verification

The following tests establish order rather than merely increasing schedule
probability:

| Boundary | Forced witness |
|---|---|
| demand published while deferred work runs | `dependency_published_while_deferred_runs_survives_its_yield` |
| exact local continuation across yield | `exact_deferred_demand_remains_local_across_a_cooperative_yield` |
| serialized runtime owner versus broad observation | `running_runtime_owner_makes_a_broad_dependency_busy_not_stable` |
| disturbance between `NoProgress` and recheck | `patient_deferred_demand_retries_when_disturbance_races_no_progress` |
| net batch-admission contention | `persistent_driver_retains_work_across_batch_admission_contention` |
| object spec/name/dependency suspension | `object_fixpoint_resumes_through_spec_name_and_dependency_chunk` |
| nested object dependency | `object_fixpoint_resumes_through_a_nested_dependency_spec` |
| both object mixin applications | `object_fixpoint_resumes_each_mixin_application_once` |
| composed definitions returning function work | `object_fixpoint_resumes_through_composed_definition_function_calls` |

The broader application, access, list-effect, cursor, object, syntax, and
compiler suites supplement these latches. Their repeated or parallel success
is regression coverage, not the proof of the races above.

## Drift Assessment

### Intentional and justified

1. **W4C was pulled before W3B.3.** A saturated function-call source needs a
   durable net normalization owner. Adding a temporary nested lazy or duplicate
   driver would have created another state representation solely to remove it.
2. **Logical-list front became shared infrastructure.** Object dependencies,
   recursive key/path conversion, and list effects all need the same
   non-forcing deferred-chunk traversal. One owner avoids subtly different
   suffix semantics.
3. **List effects received a semantic recipe in `core`.** The four production
   callback operations formed a closed family. Making it inspectable closes
   tracing and resumption together; the generic callback remains test-only.
4. **Object definition composition is flattened by the object owner.** The
   generic builtin result can itself be deferred function-call work. Treating
   `ObjectComposedDefs` as an inspectable prior/extension recipe prevents
   replay without changing its order.
5. **Scheduler repairs landed with W3.** They are not new language semantics;
   they preserve authoritative demand and distinguish transient ownership from
   stable quiescence.

### Corrective new information

1. **An exact yielded continuation must stay local unless demand was globally
   published.** Globally requeueing every exact speculative yield would turn
   abandoned alternatives into eager background work. `pump_demand` therefore
   retains a bounded local continuation, while the running-demand latch handles
   authoritative publication.
2. **Broad-observation stability is runtime-wide.** Tests asserting stable
   quiescence now use private value domains; a shared fixture runtime may
   legitimately contain an in-flight machine able to disturb the observation.
3. **Batch admission is itself a work-retention boundary.** Cursor and
   active-pair contention already restored their item, but the earlier
   normalization-batch admission did not.

### Temporary convenience policy

The coordinator currently admits at most one ordinary machine per demand
session through global ready selection. Exact dependency claims bypass this,
and sparks are separate. This contained concurrent entry into the former
recursive builtin compatibility evaluator, but costs parallelism and makes
ready selection scan session work for each candidate. It is not part of Glam
semantics. The post-W6F review confirmed that the rule remains live after
generic builtin source work became resumable and assigned its measured
removal to W6G.1.

## Findings

### WHNFW3R-001 — Resolved: running deferred demand could be lost on yield

**Severity:** high correctness

**Status:** resolved in the reviewed baseline

The deferred record now carries a one-shot `demand_while_running` latch, and
release requeues only when authoritative or already-queued demand requires it.
The forced test publishes demand after claim and before yield, then requires
the exact producer to remain ready.

### WHNFW3R-002 — Resolved: net batch contention discarded uninspected work

**Severity:** high correctness

**Status:** resolved in the reviewed baseline

`drive_net_driver_work_in` retains the popped item before admission and restores
it on contention. A barrier-held normalization owner forces the former loss,
then releases it and requires the same persistent driver to finish.

### WHNFW3R-003 — Resolved: transient scheduler ownership appeared quiescent

**Severity:** high correctness

**Status:** resolved in the reviewed baseline

The initial repair overreached by allowing patient evaluation to wait for any
future broad disturbance; that hung the established macro-session reflection
gate test. The final rule waits only while an actual machine owns possible
progress and otherwise returns the retryable block. Separate barriers cover a
running cross-session owner and publication racing the post-`NoProgress`
recheck.

### WHNFW3R-004 — Temporary per-session serialization needs retirement

**Severity:** medium performance and architecture

**Status:** open, assigned to W6G.1 by the post-W6F review

The rule was conservative containment for legacy builtin evaluation. W6A-W6F
removed that evaluator but deliberately did not change scheduler policy in
the same checkpoint. W6G.1 must replace it with role-specific foreground,
worker-background, and explicit-drain selectors, using causal traversal from
spark/reflection roots rather than sticky metadata on deferred descendants.
The replacement separates those role-specific roots from session-neutral
canonical lazy producers, with partial producer progress owned by the managed
lazy rather than a first-observer coordinator record. Foreground and
background observers therefore share source progress without sharing their
outer continuations. W6G.4 owns residual measurement. The temporary rule must
not silently become a semantic ordering guarantee.

### WHNFW3R-005 — Full command verification includes a pre-existing very slow executable fixture

**Severity:** low verification throughput

**Status:** open, separate test-performance docket

`cargo test -q --lib` completes, but the complete command reaches an executable
sample whose direct assembly evaluation exceeded three minutes both on the W3
work and on a pre-W3 baseline check. This is not evidence of W3 regression,
but it prevents the complete command from serving as a fast checkpoint. The
fixture should eventually receive an explicit fast/stress policy or a smaller
default workload; correctness claims here rely on the complete library suite
and focused sample-independent tests until then.

### WHNFW3R-006 — Resolved: W4A, W4B, and W4D were too coarse after W3

**Severity:** planning risk

**Status:** resolved in the transition plan

W4A now separates inventory, explicit owner, and forced retirement checks.
W4B separates current callback-lifecycle inventory, owner conversion, and
exactly-once verification. W4D separates combined closure from the mandatory
post-W4 implementation/drift review. W4C remains complete and is not repeated.

## Future-Phase Review

W4 should proceed W4A.0-W4A.2, then W4B.0-W4B.2, followed by W4D.1-W4D.2.
W4C is already complete. Host-call work already has an explicit lazy-task mode,
so W4B must first determine whether a new enum is useful or whether the current
mode only needs typed before/after state and stronger tests.

W5 remains correctly scoped: the reflection effect machine needs hosted WHNF
substates distinct from the W4A lazy-source owner. W5 must reuse
`WhnfComputation`; it must not clone a live checkpoint across alternatives.

W6 descriptions remain broader than the work left. `W6E` now means remaining
list-effect builtin dispatch and effect construction, not the migrated lazy
list-effect source recipes. `W6F` means remaining object builtins and net
request construction, not object-fixpoint C3 or mixin traversal. W6 is also the
owner of the temporary session-serialization retirement decision.

W7's recursive-call census and small-stack tests remain necessary. The W3
machines remove important recursive source entry, but generic builtin,
reflection decoding, and collection/pattern work still contain user-sized
recursive evaluation.

W8 remains coherent. `await_deferred_task` and direct compatibility evaluation
cannot retire before W6/W7 because generic builtin and test adapters still use
the recursive halt transport.

## Verification Record

Completed on the reviewed baseline plus the review corrections:

- `cargo fmt --check`;
- `cargo clippy --all-targets --all-features -- -D warnings`;
- `cargo test -q --lib` — 1,565 passed, 2 ignored;
- `cargo test -q --lib 'g_syntax::tests::' -- --test-threads=16` — 236 passed;
- the forced scheduler, object, list-effect, access, net-driver, and WHNF
  inventory tests named above.

The complete `cargo test -q` command is recorded under WHNFW3R-005 rather than
misreported as passing.
