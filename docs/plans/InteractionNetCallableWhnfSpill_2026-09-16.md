# Interaction-Net Callable WHNF Spill Plan — 2026-09-16

Status: planned. This is the focused topology and suspension subplan for
W6B.4b.2 of
[`ResumableWhnfEvaluation_2026-09-12.md`](ResumableWhnfEvaluation_2026-09-12.md).
The parent plan remains authoritative for the D.2c inventory and its `-1`
callable-lowering delta. This plan owns the larger semantic, interaction-net,
budget, race, and verification work needed to achieve that delta without a
durable net claim.

## Purpose

Replace synchronous callable forcing inside an interaction-net
`Bind >< Data` reduction with an inline-first, spill-on-suspension protocol.

The common case must remain simple. When the callable reaches WHNF within the
current evaluator quantum, the original call reduction lowers it directly to
a copied raw net or a `CoreOperator`; it does not install intermediate graph
state merely because the input began as a lazy or promise. Only a computation
which exhausts its budget or reaches a real dependency publishes resumable
state into the net.

The suspended representation is an internal `NormalizeCallable` operator. It
retains the completed prefix of callable-WHNF work as ordinary managed net
topology, not as a worker-local Rust continuation, a synthetic semantic
`Value`, an active-pair side table, or a durable claim. A later worker may
briefly claim that operator pair, project the checkpoint into ephemeral
regional work, use another bounded quantum, and either remove the operator on
completion or publish one replacement checkpoint before yielding again.

This is a spill protocol:

```text
Bind >< Data(callable)
        |
        +-- bounded regional callable WHNF
              |
              +-- ready  -> direct existing callable lowering
              +-- failed -> structural stuck result
              +-- yield  -> NormalizeCallable(checkpoint) >< Data(focus)
              +-- wait   -> NormalizeCallable(checkpoint) >< Data(focus)
                                then exact dependency suspension
```

## Why This Is a Separate Plan

The parent W6B.4b checkpoint originally grouped two raw-value declarations:
runtime-net access resolution and callable lowering. Access resolution can
reuse the existing `AccessMachine` ownership pattern. Callable lowering also
requires:

- a new core-net topology state;
- bounded evaluator-budget integration in `NetWhnfMachine`;
- a same-pair payload checkpoint transition with managed-edge barriers;
- production use of exact blocked operator pairs;
- stale-admission and lost-wakeup handling; and
- a split-point, contention, profiling, and aggressive-GC matrix.

Those changes are independently reviewable and substantially larger than the
remaining W6B.4b access conversion. Keeping the detailed work here makes the
parent inventory readable while preserving its execution order and closure
accounting.

## Current Implementation

`eval::net::lower_core_callable_in` currently recognizes a deferred callable
and calls synchronous `eval_value_in`. `CoreCallClaim` owns the original pair
while that operation determines whether the value is a raw net, builtin,
partial builtin, function, applicable dictionary, failure, or retryable wait.
The retryable path can block the original call pair, but the exact incremental
WHNF checkpoint is not represented in the net.

The reusable WHNF evaluator already has the required semantic concepts:

- `RegionalWhnfWork` performs callback-free work beneath one matching access;
- `WhnfStepBudget` bounds a regional quantum;
- `DurableWhnfState` retains focus, followed deferred identities, cycle state,
  and general continuation frames; and
- `WhnfDeferredRequest` separates dependency admission from regional value
  access.

Its durable form is intentionally machine-oriented. It owns runtime roots, is
not cloneable, and must not simply be embedded in cloneable `CoreOperator`
payloads. The call site initially needs only outer-shell normalization: the
canonical lazy producer owns evaluation of an unfulfilled lazy source, while
the call site follows fulfilled lazies and assigned promises and retains their
cycle history.

The generic interaction-net runtime currently supports `OperatorYield::Data`
and `OperatorYield::Operator`. The latter means that an operator application
returned another callable operator; it consumes the current operand and
installs a future `Bind`. It is not a same-pair continuation and must not be
repurposed for callable normalization.

## Selected Semantics

### Inline first, spill only at a boundary

Encountering `Value::Lazy` or `Value::Promised` does not by itself install a
normalizer. The claimed call projects an ephemeral callable-WHNF work record
and drives it with the remaining semantic budget for the current net-machine
quantum.

A chain such as:

```text
Lazy A -> Promise B -> Function F
```

which is already cached and assigned completes inline and follows the same
direct lowering path as an immediate `Function F`. No additional node,
managed root, task, or net reduction survives the operation.

The work is reified only if:

1. the available callable-WHNF budget is exhausted;
2. the current lazy or promise is unresolved;
3. dependency admission must occur outside managed access; or
4. a future outer-shell rule reaches another explicit external boundary.

### Net-owned checkpoint

The initial checkpoint shape is deliberately narrower than a general
`WhnfComputation`:

```rust
struct CallableWhnfCheckpoint {
    generation: CallableCheckpointGeneration,
    followed: BTreeSet<DeferredValueId>,
}

CoreOperator::NormalizeCallable(CallableWhnfCheckpoint)
```

The exact field spelling may change after the NC1 inventory, but its ownership
does not:

- the associated `Data` node owns the current focus;
- the operator owns only the remaining cycle/progress state;
- semantic `Value` gains no callable-progress variant;
- no `RuntimeValueRoot` is embedded merely to keep the checkpoint alive; and
- every raw `Value` retained by the operator is a managed net edge covered by
  the normal `CoreOperator` edge visitor and mutation barrier.

`generation` is an opaque runtime-local checkpoint identity, not semantic
data. Together with the runtime-net identity and active-pair key, it prevents
a delayed dependency admission from blocking a newer checkpoint which happens
to occupy the same operator and data nodes. If existing runtime revisions can
provide the same exactness without broad equality or retention, NC2 may reuse
them; the invariant is required even if the field is not.

### Meaning of `Data(focus)`

`focus` is the exact value which the next outer-shell WHNF step must inspect.
It is not necessarily the callable originally attached to the `Bind`.

Regional shell transitions are atomic with respect to budget publication. The
driver checks budget before a transition. Following a cached lazy or assigned
promise updates both the current focus and `followed` before another budget
check can yield. Consequently:

```text
original focus: Lazy A
cached A:       Promise B
assigned B:     Lazy C
```

may spill as `Data(Lazy C)` with `A` and `B` already represented in the
followed identities. Resumption begins at `C`; it does not return to `A` or
re-read `B`. If budget expires before a transition starts, the prior focus is
retained and only that not-yet-performed observation occurs after resumption.

When the focus is an unfulfilled lazy, `Data(focus)` deliberately remains that
same lazy identity. Its canonical `LazyTaskMachine` owns source evaluation,
including function application, reflection tasks, host calls, or net work.
The normalizer admits or joins that producer and blocks. After wakeup it reads
the lazy's cache once and continues from the cached result. It never evaluates
the lazy recipe itself and cannot construct a second reflection task.

Promises follow the same division. An unassigned promise remains the focus;
its producer or canonical follower owns eventual assignment. An assigned
promise transition replaces focus with its assigned value before publication.
The root-free `followed` set is sufficient for call-site cycle detection
because the current repeated promise is itself available when a canonical
promise-follow request is needed. The separate `cycle_promise` breadcrumb in
general source-oriented `WhnfComputation` exists only for a producer which
returns to its own source lazy; callable normalization has no `source_owner`
and must not retain that field until such ownership is actually introduced.

### Resume by projection

Claiming `NormalizeCallable(checkpoint) >< Data(focus)` projects those payloads
into regional callable work beneath one matching value-access region. One
claim may cover several callback-free outer-shell steps, but only within the
bounded current quantum. The result is one of:

- **Ready:** remove the normalizer and emit the final callable as `Data` at the
  original `Bind` principal. Ordinary call lowering then proceeds. A later
  measured optimization may fuse these two terminal reductions.
- **Failed:** retain the structured evaluation failure as the stuck reason.
- **Yielded:** atomically replace the operator checkpoint and data focus once,
  make the pair ready, and end the claim.
- **Boundary:** publish the replacement checkpoint once, end the regional
  access and claim, admit the dependency, then conditionally block only that
  exact checkpoint.

The implementation must not mutate graph topology once per followed shell.
The graph records quantum boundaries; the regional reducer records the cheap
steps within a quantum.

### Budget relationship

`NetWhnfMachine::poll` must receive the outer task's `step_budget`. Initially,
the net driver should use a semantic sub-budget:

- each call or operator semantic dispatch consumes a unit;
- each regional callable-WHNF transition consumes a unit from the same
  remaining budget; and
- existing structural interaction-net normalization batches retain their
  separate bounded-batch policy rather than suddenly charging every wire or
  cursor operation as an evaluator semantic step.

The precise accounting is latched in NC1 before topology changes. A callable
must never receive a fresh full budget for every active-pair retry within one
outer poll. Budget exhaustion returns `Yielded` after publishing the exact
checkpoint.

### Dependency admission without a durable claim

No call or operator claim may enter a machine field, scheduler record, or
returned `Pending` result. A claim may span only the bounded callback-free
regional computation and its immediate atomic topology publication.

Dependency admission happens after managed value access closes. The selected
protocol is logically two-stage:

1. publish a ready normalization checkpoint and finish the current claim;
2. admit or observe the lazy/promise dependency outside managed access;
3. conditionally block the pair only if the runtime net, pair, and checkpoint
   generation still match; and
4. close the subscribe/observe race so completion immediately before or after
   the conditional block cannot lose a wakeup.

Another worker may claim or even complete the normalizer between steps 1 and
3. That is ordinary contention. The older boundary action becomes stale and
must do nothing. It must not restore an older payload or block newer work.

The implementation may use an existing persistent wait state or an explicit
post-block completion recheck to close the lost-wakeup window. It must not
solve the race by retaining the original claim across a callback, scheduler
wait, or later poll.

## Semantic and Ownership Invariants

1. **Immediate and cheaply resolved callables leave no normalizer.** Deferred
   syntax or representation alone is not a reason to add topology.
2. **Suspension does not replay a completed prefix.** Focus, followed deferred
   identities, promise-cycle state, and any later required continuation state
   are published together.
3. **Claims are quantum-local.** No claim survives budget yield, dependency
   admission, scheduler return, callback, cancellation, or unwind.
4. **The net owns suspended progress.** A ready or blocked normalizer is
   complete authoritative state; no machine-side shadow checkpoint is needed
   to resume it.
5. **Lazy producers remain canonical.** An unfulfilled lazy is evaluated by
   its normal producer owner. Callable normalization waits on and follows its
   result; it does not create a second producer.
6. **Producer work is not duplicated in topology.** The checkpoint retains
   the current deferred identity, not its source recipe or producer machine.
   Reflection, host, function-call, and net-source progress remain in the
   canonical lazy task which the normalizer joins.
7. **Cycle semantics match ordinary WHNF.** Splitting at any quantum or wait
   boundary cannot change promise/lazy cycle recognition or the resulting
   structured failure.
8. **A checkpoint is copied as topology.** Copying or materializing a partially
   normalized closed net retains its focus and checkpoint coherently; no
   active-pair side table is required. Scheduler-local blocked status need not
   copy: the target pair observes the retained focus and independently admits
   the same semantic dependency.
9. **No semantic value pollution.** Callable checkpoints are internal operator
   state and cannot be observed through ordinary Glam patterns, equality, or
   value-kind diagnostics.
10. **Every checkpoint edit is a traced edge transition.** Replacing focus or
   checkpoint-owned values reports exact leaving and entering edges through
   the managed runtime-net mutation gateway.
11. **Stale orchestration is harmless.** A delayed admission, wake, or retry
    cannot block, restore, or fail a newer checkpoint.
12. **Failure provenance is preserved.** Inline and spilled evaluation produce
    equivalent evaluation failures and context frames.
13. **Raw nets and applicable values retain existing meanings.** Reaching
    `Value::Net`, builtin, partial builtin, function, or applicable dictionary
    hands off to the established callable classification without inspecting
    function stages or changing partial-application semantics.
14. **Operator continuation is explicit.** `OperatorYield::Operator` retains
    its current returned-callable meaning. Same-pair checkpoint replacement
    receives a separately named mutation/result path.
15. **No new global allocator is introduced.** Any checkpoint generation uses
    runtime-owned identity allocation or an existing exact runtime revision.

## Non-Goals

- Do not move general pure evaluation into interaction-net topology.
- Do not add a public WHNF, demand, or normalization agent.
- Do not introduce durable active-pair claims.
- Do not add an active-pair side table for callable progress.
- Do not wrap every deferred callable in a synthetic managed lazy.
- Do not add `Value::CallableProgress` or another semantic value variant.
- Do not fuse final callable normalization with ordinary call lowering before
  profiling demonstrates a worthwhile benefit.
- Do not redesign cursor-WHNF normalization, generic task scheduling, or the
  entire interaction-net work budget in this subplan.
- Do not migrate the separate W6B.4b access-path operation here.

## Implementation Phases

### NC0 — Latch the current seam and target behavior

#### NC0A — Current-path characterization

Record focused tests and profiling observations for:

- an immediate builtin, partial builtin, function, dictionary, and raw net;
- a cached lazy resolving to each callable family;
- an assigned promise resolving to a callable;
- an unresolved lazy and promise;
- a permanently non-callable result; and
- the existing callable claim release, unwind, blocked retry, and fused
  callable-to-operator topology.

The tests should show that synchronous `lower_core_callable_in` is the only
remaining W6B.4b callable declaration and identify where its claim currently
crosses retryable evaluation. Do not change expected semantics in NC0A.

#### NC0B — Failing spill or instrumentation oracle

Add test-only observation sufficient to distinguish:

- direct lowering with no normalizer ever installed;
- one checkpoint installed because a budget was forced to expire;
- one checkpoint installed because an exact dependency was reached; and
- resumption from the published focus rather than the original callable.

Prefer topology and counter observations over timing or thread repetition.
The target tests may remain expected-failing only inside the NC0 commit and
must become ordinary passing regressions as their owning phases land.

Exit: the existing behavior and the desired no-spill fast path are executable
contracts before production topology changes.

### NC1 — Shared regional callable-WHNF work and budget

#### NC1A — Factor outer-shell semantics

Extract the callback-free lazy/promise shell reducer and its deferred-identity
rules so ordinary `WhnfComputation` and callable normalization use one
implementation. Introduce a narrow regional callable work form only if the
general `RegionalWhnfWork` cannot be safely reused without manufacturing
irrelevant frames.

Prove that a call-site computation begins with no general continuation frames
and that unfulfilled lazy-source evaluation remains owned by the canonical
lazy task. If this proof fails, expand the net checkpoint deliberately rather
than silently discarding general WHNF state.

#### NC1B — Reusable bounded driver

Drive callable work to `Ready`, `Failed`, `Yielded`, or `Boundary` beneath one
matching access. Retain the exact regional work on yield and boundary. Add
forced-budget tests which split a cached lazy/promise chain after every shell
and compare it with uninterrupted WHNF. Add a reflection-backed lazy callable
whose producer is paused before and after reflection admission; every split
must retain one lazy identity and one reflection task rather than reconstruct
either source.

#### NC1C — Net-machine semantic budget

Pass the outer `step_budget` into `NetWhnfMachine::poll` and introduce shared
semantic-budget accounting for call/operator dispatch and callable shell
steps. Preserve the existing structural normalization-batch policy. Add a
deterministic zero/one/many-budget matrix and show that one outer poll cannot
grant a fresh full callable budget to each retry.

Exit: callable WHNF can be driven and split exactly without graph changes,
and the net owner has an explicit bounded quantum to lend it.

### NC2 — Managed normalization checkpoint topology

#### NC2A — Core operator state and edge coverage

Add `CoreOperator::NormalizeCallable` with the minimum checkpoint proven by
NC1A. Extend every exhaustive operator match, debug rendering, equality used
for exact internal matching, compatibility edge visitor, recursive identity
inventory, and managed-drop/ownership inventory as appropriate.

Checkpoint-owned semantic values must appear in GC edge tests. Root-free IDs
must not be turned into managed roots merely for uniformity.

#### NC2B — Same-pair checkpoint transition

Add a separately named generic runtime mutation which replaces the operator
and data payloads of one claimed `Operator >< Data` pair while preserving its
node identities and output connection. It must:

- validate the exact claimed pair;
- report old and new operator/data edges through the mutation gateway;
- publish the replacement before releasing the claim;
- make the pair ready exactly once; and
- remain distinct from `OperatorYield::Operator`.

Test the generic runtime transition independently, including stale calls,
unwind restoration before publication, and edge-set accounting.

#### NC2C — Conditional exact blocking

Add the minimum exact-state operation needed to block a published
normalization checkpoint after dependency admission. Match runtime net, pair,
and generation/revision atomically. A stale request returns a non-error
"disturbed" result and does not change pair state.

Build deterministic barriers for both admission orderings:

1. the dependency remains pending through successful conditional blocking;
2. the dependency completes before conditional blocking;
3. another worker advances the checkpoint before conditional blocking; and
4. another worker completes and removes the normalizer first.

Exit: the graph can durably publish and exactly suspend one callable
checkpoint without storing a claim.

### NC3 — Inline-first original call reduction

#### NC3A — Regional fast path

Replace `lower_core_callable_in`'s synchronous deferred forcing with bounded
regional callable work. Immediate callables bypass the reducer. Deferred
callables which become ready within the remaining budget take the existing
direct copy/operator/failure paths.

Prove through topology observation that cached or assigned chains within the
budget never install `NormalizeCallable`.

#### NC3B — Spill on budget exhaustion

When callable work yields, atomically replace the original call pair with the
normalization topology containing the exact current focus and checkpoint.
End the original claim and return ordinary runnable progress. Resume from that
focus in NC4; never restart from the callable clone retained by the old claim.

#### NC3C — Spill on dependency boundary

Publish the same topology for an unresolved lazy/promise boundary, then use
the NC2C protocol to admit and block outside access. Ensure an abandoned,
cancelled, failed, or completed producer produces the same result as ordinary
WHNF following.

Exit: original calls either finish inline or leave one complete graph-owned
checkpoint; they never retain a durable claim or machine-side continuation.

### NC4 — Resume and retire normalization operators

#### NC4A — Operator projection and regional resume

Teach core operator dispatch to recognize `NormalizeCallable`, project its
checkpoint plus operand into regional callable work, and use the remaining
shared semantic budget. This path is the first production user of blocked
operator-pair resumption retained after W6B.2.

#### NC4B — Yielded checkpoint replacement

On budget yield, use NC2B to publish exactly one updated operator/data pair.
Do not mutate topology once per followed shell. Add counters proving that a
quantum with many cached transitions performs one checkpoint publication.

#### NC4C — Ready and failed retirement

On readiness, remove the normalizer and emit the final callable `Data` at the
original `Bind`, allowing the established call reduction to classify it. On
failure, retain the structured stuck reason. Preserve raw-net copying,
function partial application, and applicable dictionary/builtin behavior.

Exit: a spilled callable can cross arbitrarily many budget and dependency
boundaries, then remove all checkpoint topology on terminal completion.

### NC5 — Concurrency, copying, and failure closure

#### NC5A — Cycle and dependency matrix

Force:

- lazy-to-lazy, promise-to-promise, and mixed chains;
- a repeated identity before and after one or more spills;
- an unresolved dependency at the first and later focuses;
- failure cached by a lazy or assigned to a promise; and
- cancellation, abandonment, and task failure while blocked.

Require identical result or structured failure for uninterrupted, every-step
budget splitting, and dependency-split execution.

#### NC5B — Contention and stale work

Use barriers rather than repetition to force two workers toward the same
normalization frontier. Verify one authoritative checkpoint, harmless stale
admission, exact blocked retries, no restored older focus, and no claim after
either worker returns. Cover panic/unwind before and after checkpoint
publication.

#### NC5C — Copy and GC ownership

Copy or materialize a closed runtime net while callable normalization is
ready and while its source pair is blocked. Each copy must retain a coherent
focus and checkpoint without an external side record, then independently
observe and block on the still-pending semantic dependency. The source pair's
scheduler-local blocked state is not copied. Run the cases with collection
forced at projection, publication, dependency admission, wake, and terminal
retirement boundaries.

Exit: topology, dependency, and ownership behavior remains correct under
forced schedules and aggressive collection.

### NC6 — W6 integration, profiling, and focused review

#### NC6A — Compatibility retirement and inventory closure

Remove synchronous deferred callable forcing from `lower_core_callable_in` or
retire the helper if classification now belongs directly to call progression.
Relatch the D.2c manifest and record W6B.4b.2's `OperatorAndNet -1` delta.
Keep the separate access-resolution declaration assigned to W6B.4b.1.

#### NC6B — Profiling and performance

Extend static interaction-net profiling with, at minimum:

- inline callable-WHNF steps;
- normalization checkpoint installation;
- normalization resumption;
- checkpoint replacement;
- exact dependency block/retry; and
- stale boundary admission.

Update the focused profiling script with named tests. Verify that immediate and
within-budget cached callables install zero normalizers. Compare direct-style
assembly fixtures against the pre-NC baseline and feed any residual scheduling
or managed-access cost into W6G rather than hiding it with an unbounded budget.

#### NC6C — Routine verification and review

Run:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Also run the focused interaction-net profiling script, callable/function/net
suites, forced-schedule tests, and relevant aggressive-GC partitions. Perform
a focused post-NC review of:

- inline-versus-spilled equivalence;
- claims and dependency ownership;
- exact mutation barriers;
- runtime-net copy semantics;
- profiling determinism;
- remaining synchronous evaluator compatibility; and
- drift in W6C-W8 caused by the new budget/checkpoint vocabulary.

Exit: W6B.4b.2 is complete, reviewed, and ready for the parent W6B.4b closure.

## Verification Matrix

| Callable situation | Large budget | Forced budget yield | Dependency boundary | Required topology |
|---|---|---|---|---|
| Immediate callable | direct result | direct result | n/a | no normalizer |
| Cached lazy chain | direct result | same result after resume | n/a | normalizer only when forced split occurs |
| Assigned promise chain | direct result | same result after resume | n/a | normalizer only when forced split occurs |
| Unresolved lazy | eventual direct result | same result | exact lazy wait | one blocked normalizer |
| Unassigned promise | eventual direct result | same result | exact promise wait/follower | one blocked normalizer |
| Reflection-backed lazy | one producer and reflection task | same producer and task | exact lazy wait | one blocked normalizer, no source replay |
| Mixed deferred cycle | same structured failure | same failure | same cycle dependency/failure | no replayed prefix |
| Deferred non-callable | same stuck failure | same failure | optional earlier wait | normalizer retired or stuck exactly once |
| Raw net result | copied source | same copied source | optional earlier wait | final normalizer removed |
| Partial function result | ordinary one-argument application | same value | optional earlier wait | final normalizer removed |

Every forced split must assert semantic result, checkpoint focus, followed-set
continuity, claim absence after return, and expected profiling counts. Thread
repetition alone is not evidence for any concurrency row.

## Deferred Optimization

After correctness and profiling, consider:

- fusing terminal `NormalizeCallable -> Data(callable) >< Bind` with the
  established callable classification rewrite;
- replacing the followed `BTreeSet` with a cheaper small-set representation
  for short chains;
- sharing normalized callable work across copied nets through a managed
  resolver only if measurements justify the added allocation;
- batching several pure semantic operator steps within one access region; and
- incorporating callable normalization into future annotated normalization or
  JIT policies.

None of these optimizations may reintroduce durable claims or make suspended
progress depend on a particular worker's Rust stack.
