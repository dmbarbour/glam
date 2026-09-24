# Resumable WHNF Evaluation Plan — 2026-09-12

Status: W0-W5 and their mandatory reviews plus W6A-W6F and the mandatory
post-W6F review are complete by 2026-09-18. W6G.1-W6G.5 and the mandatory
post-W6G review plus W7A are complete by 2026-09-24. W7B-W8 remain planned,
followed by the explicit W6G4R-003 performance follow-up in W9. This is the
focused implementation plan selected by
GCI11R-002D.2c.1d in
[`GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md`](GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md).
Client demand, lazy and promise following, external-source owners, and
reflection request work now use the crate-private resumable protocol. The
remaining builtin evaluator families and compatibility retirement belong to
W6-W8.

## Purpose

Replace the recursive evaluator's implicit Rust-stack continuation with one
crate-private, resumable weak-head-normal-form computation.

Today `eval_value_in` and its callees may discover a lazy, promise, reflection
gate, net dependency, or host boundary after performing part of a larger pure
calculation. A retryable `EvaluationHalt` retains the dependency, but not the
calculation state which must consume its result. The enclosing scheduled
machine either keeps that state accidentally on the Rust call stack while it
cooperatively pumps the dependency or retries from a coarser machine state.
The latter can repeat completed prefixes, construct fresh semantic lazies, and
fail to make progress.

The target is a fine-grained trampoline for **evaluation to outer WHNF**. It is
not another effect interpreter. Reflection and other freer-effect handlers
retain their existing monadic machines and host one WHNF subcomputation when
they need a value demanded.

This transition has two inseparable motives:

1. suspension must resume after the completed prefix rather than restart from
   an initial value or enclosing effect branch; and
2. ordinary pure evaluation must eventually consume an explicit work budget
   without growing the Rust call stack with semantic depth.

## Current Failure Shape

Reflection request decoding currently performs this sequence in one Rust call
chain:

```text
drive reflection branch
  -> evaluate effect object
  -> evaluate `eff`
  -> apply `eff` to the reflection API
       -> saturated Glam application constructs lazy L
  -> evaluate L to obtain the request
  -> parse and dispatch the request
```

Function saturation ordinarily produces `LazySource::FunctionCall`, including
for a trivial effect such as `.r ()`. The reflection machine therefore
immediately demands a semantic lazy which its own request-decoding step just
constructed. `EvaluatorStepContext` gives that lazy an exact temporary owner,
and coordinator admission gives its producer durable ownership. Liveness and
producer ownership are not the defect.

The defect is the missing owner for **what request decoding must do after L
completes**. `reflection::MachineWork` retains only the coarser branch work.
When the recursive evaluator cannot finish L while its Rust callers remain
live, a resumed branch can apply `eff` again and construct L2 rather than
consume L's result. An uncommitted D.2c prototype forced this ordering and
produced an unbounded sequence of fresh lazy IDs.

The same representational gap applies below other recursive evaluator callers;
reflection merely supplied a small deterministic witness.

## Terminology

- **WHNF computation** — one request to follow a value's outer lazy and
  promised shells until it produces a non-deferred outer value, permanent
  failure, dependency, or budget yield.
- **regional quantum** — callback-free reduction performed while one matching
  `EvaluationValueAccess` is active.
- **durable checkpoint** — rooted evaluator state which can survive access
  closure, a scheduler handoff, another worker, cancellation, or collection.
- **delegation** — an immediate control transition saying that the current
  result is the result of another piece of WHNF work. It creates no wait,
  semantic lazy, cache write, or machine-specific continuation.
- **dependency** — a scheduler-visible condition which currently prevents
  progress, such as a wait token or unassigned promise.
- **resumption state** — the exact computation state after the completed
  prefix. This is mathematically a defunctionalized continuation, but the
  implementation need not use closures or a general frame vector.
- **result disposition** — what the outer owner does with completed WHNF: return
  it to a client, cache it in a lazy, parse it as a reflection request, inspect
  it for a spark, or feed it into another existing machine.

## Selected Architecture

### One reusable WHNF submachine

Introduce a crate-private stateful computation with a shape resembling:

```rust
struct WhnfComputation {
    checkpoint: DurableWhnfState,
}

enum WhnfPoll {
    Ready(RuntimeValueRoot),
    Pending(WhnfDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}
```

The exact spellings and whether failure is rooted by this layer remain
implementation details. The required property is structural: after `Pending`
or `Yielded`, the same `WhnfComputation` contains the exact state to poll next.
The dependency alone is never treated as a sufficient resumption record.

An outer machine owns the `WhnfComputation`. Polling temporarily projects its
durable state beneath one access region, performs bounded callback-free work,
then either completes or publishes a replacement durable checkpoint before
that access ends. Scheduler admission, waits, callbacks, reflection
activation, and claim release occur only after the access region has closed.

### Pure progress and orchestration remain distinct

The regional reducer reports transitions resembling:

```rust
enum RegionalWhnfStep {
    Delegate(RegionalWhnfWork),
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Failed(Arc<EvaluationFailure>),
}
```

`Delegate` replaces the current work and continues within the same result
destination. `Boundary` first packages the current work into a durable
checkpoint. A mutator-free driver then translates the request into existing
lazy, promise, reflection, net, or host coordination.

This division describes pure **semantics**, not an assertion that WHNF never
depends on external work. A reflection annotation or host-backed source may
interrupt a pure demand. The reducer stops at that boundary; it does not
interpret the external operation while retaining managed access.

### `LazySource` remains a recipe, not a program counter

`LazySource` continues to describe the immutable semantic recipe owned by one
lazy and discarded only after terminal cache publication. The initial
implementation must not replace that source with partially executed variants
or construct a new semantic lazy merely to hold evaluator control state.

Resumable source work retains the source's stable lazy owner plus separate
progress. W0B found only two true tail demands among 305 relevant control-flow
occurrences, versus 157 demand-then-inspect sites and 95 more application,
collection, key, and path shapes. Source-specific phases alone would therefore
duplicate the same child-resumption protocol across most evaluator modules.

The selected representation is a shared explicit work stack:

```rust
struct WhnfState {
    focus: Value,
    frames: Vec<WhnfContinuation>,
    followed: BTreeSet<DeferredValueId>,
    source_owner: Option<LazyId>,
    cycle_promise: Option<PromisedValue>,
}

struct RegionalWhnfWork(WhnfState);

enum RegionalWhnfStep {
    Delegate(Value),
    Continue(RegionalWhnfWork),
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Failed(Arc<EvaluationFailure>),
}
```

Focused callable-spill checkpoint NC2.0 introduced the canonical
`WhnfState`/`WhnfContinuation` vocabulary and made regional and net-owned forms
zero-walk ownership wrappers around it. A role change consumes and rewraps the
same state without iterating frames, duplicating values, registering roots, or
allocating replacement containers. The older parallel regional/net frame
declarations and borrowed projection scaffold are gone.

`Delegate` remains a direct focus replacement and does not push a frame.
Repeated nested work uses shared frames for demand-and-inspect, ordered
operands, collection walks, application, key conversion, access paths, and
diagnostic context. Orchestration handoff is a boundary result rather than an
evaluator frame. A source or operation may still own a small phase enum when
it has genuinely unique state, but it resumes child WHNF through the common
stack rather than reproducing another evaluator.

Regional frames contain raw values only while one managed-access region is
active. Their durable counterparts contain matching-runtime roots only for
the focus and frame fields which cross a yield, suspension, or callback. W1
may use a plain `Vec` initially; inline capacity and root-frame compression are
profiling work, not correctness prerequisites.

### Durable state is rooted only at real boundaries

Do not register a root for every delegation, recursive call, operand, or
intermediate value. During a regional quantum, the active mutator protects raw
working values. Roots are constructed only for state which must outlive that
region: suspension, budget yield, callback handoff, or publication.

The current fine-grained-root checkpoint replacement follows this order:

1. enter with the prior durable checkpoint still live;
2. project it beneath matching access;
3. perform local work using raw/scoped values;
4. root every live value in the replacement checkpoint before access closes;
5. install the replacement checkpoint; and
6. only then retire superseded roots and perform external coordination.

W6G.3 replaces steps 4-5 with one in-place, aggregate edge-state transition on
the rooted managed cell. It preserves the same publication and unwind
properties without constructing another set of roots or walking the state once
per internal transition.

Panic/unwind handling must never leave a machine with an empty checkpoint.
Keeping the prior checkpoint until replacement publication is acceptable even
if unwind ultimately terminalizes the owning machine.

The implemented correctness scaffold uses one registered root per value live
across a real suspension. W6G.3 replaces it with one
`Root<ManagedLazyCheckpointCell>` whose mutex protects the complete canonical
`WhnfState`. One callback-free quantum mutates the state beneath matching
access and reports its complete before/after edge sets through the existing
managed transition gateway; waits, callbacks, and orchestration remain outside
the lock and access region. A future root-frame facility may replace this
managed cell if concurrent-GC profiling justifies trace-immediate
machine-adjacent state, but it is not a prerequisite for aggregate roots.

### Result destinations stay outside pure evaluation

`WhnfComputation` returns WHNF; it does not decide where that result belongs.

- `ClientDemandOperation` publishes it to the client-demand sink.
- `LazyTaskMachine` verifies non-deferred WHNF and installs the terminal lazy
  cache.
- `PromiseFollower` publishes/follows the assigned value according to its
  existing coordinator role.
- the spark driver discards the result after fulfilling best-effort demand.
- the reflection machine interprets it according to a small
  `ReflectionWhnfPurpose` or equivalent phase.
- net construction and cursor-WHNF retain their existing pollable machines and
  topology-owned progress.

In particular, delegation does not immediately write a lazy result. Only the
outer `LazyTaskMachine` which owns that cache may publish it.

### Reflection hosts rather than duplicates the trampoline

The reflection effect machine remains responsible for `.alt`, `.cut`,
transactions, request dispatch, and effect continuations. Its monolithic
request-decoding helpers must be split only enough to retain a WHNF
subcomputation and the purpose of its result:

```rust
MachineWork::EvaluateWhnf {
    computation: WhnfComputation,
    purpose: ReflectionWhnfPurpose,
    branch: Branch<_>,
    scope_depth: usize,
}
```

The exact representation may instead be a dedicated request-decoding state.
The semantic requirement is that resuming request evaluation consumes the
same application lazy and continues at request parsing; it must not replay the
preceding branch application.

### Budgeting is part of the target contract

Every iterative transition consumes a finite work unit. Collection walks and
other loops which may scale with user data consume additional units rather
than hiding unbounded work inside one nominal step. Budget exhaustion packages
the exact state and produces `Yielded`, not a fabricated dependency.

Existing task machines already understand yielding. Client demand and spark
adapters must gain an equivalent requeue disposition if their WHNF work can
exhaust a quantum. Direct synchronous adapters loop over yields through the
ordinary runtime driver rather than recursively evaluating.

The initial accounting need only be deterministic and monotonic; precise cost
weighting and adaptive scheduling are performance work.

## Preliminary Current-State Inventory

Phase W0 makes this inventory exact and source-backed. The preliminary survey
identifies these distinct owners and computation shapes.

### Outer owners

| Owner | Current state | Target relation |
| --- | --- | --- |
| `ClientDemandOperation` | Retains only the initial `RuntimeValueRoot`; each poll calls `eval_value_in` again. | Own one `WhnfComputation`; publish only `Ready`/`Failed`. |
| `LazyTaskMachine` | `Produce`, `Follow`, `HostCall`, or `NetConstruction`; ordinary source evaluation is recursive. | Host source computation; retain cache publication as the outer disposition. |
| `PromiseFollower` | Retains promise root plus assignment/follow phase. | Reuse or host WHNF assignment demand without losing canonical dependency behavior. |
| spark polling | Reprojects the original value and retries the strategy operation. | Retain resumable best-effort WHNF work; yield or park without fabricating completion. |
| reflection `MachineWork` | Persistent effect phases, but calls recursive `evaluate_in` inside individual phases. | Host a WHNF submachine plus a reflection-specific completion purpose. |
| interaction-net normalization | Already owns explicit `NetDriverWorklist`; operator evaluation can surface semantic dependencies. | Preserve the net worklist and bridge only its evaluator suboperations. |
| net construction | Already owns `NetConstructionMachine`. | Preserve it as an orchestration boundary; do not translate it into evaluator frames. |
| direct/test evaluator adapters | Keep recursive Rust calls alive while cooperatively pumping. | Drive the same WHNF submachine synchronously; remain compatibility-only where already planned. |

### Lazy sources

| Source | Principal resumption concern |
| --- | --- |
| `Error` | Permanent invariant failure; no progress state. |
| `ComputedFixpoint` | Application followed by demand, or object construction; retain the marker and post-application phase. |
| `SemanticComputation` | A function pointer can currently hide arbitrary recursive evaluator work; production uses must be converted to explicit inspectable work or proven nonsuspending. |
| test `SemanticThunk` | Opaque closure cannot package arbitrary locals; remove from suspension verification or constrain it to a classified boundary fixture. |
| `HostCall` | Already invoked outside evaluator access; preserve explicit callback handoff and feed its rooted result into WHNF work. |
| `ReflectionTask` | Reservation, activation, polling, acknowledgement, context attachment, and target projection straddle the access boundary. |
| `Access` | Iterates static/dynamic path parts and demands intermediate dictionaries; retain path/dynamic indices and current value. |
| `Application` | May demand a deferred function, apply multiple arguments, and demand a resulting lazy; retain argument position and current function. |
| `Builtin` | Dispatches to the recurring unary, multi-operand, collection-fold, application, and boundary shapes below. |
| `NetConstruction` | Already has a pollable effect machine; retain as an outer lazy-task mode. |
| `NetComputation` | Cursor-WHNF has explicit work state; preserve its special contention proof and result context. |
| `FunctionCall` | Net attachment plus cursor-WHNF extraction; preserve attached runtime and exposed interface after construction. |

### Recurring pure call shapes

1. tail demand or delegation through lazy/promised aliases;
2. demand one child, inspect its WHNF kind, then construct a result;
3. demand fixed operands in order, then combine them;
4. demand elements of a list/dictionary fold while preserving an index and
   accumulator;
5. demand a function, apply one or more arguments, and optionally demand the
   result;
6. recursively convert lists/dictionaries into `Key` values;
7. walk access paths while demanding each intermediate dictionary;
8. attach structured diagnostic context if a nested demand fails; and
9. hand a fully rooted request to reflection, host, net, or scheduler work and
   later consume its response.

The exact frequency and nesting of these shapes determine the W0C progress
representation.

## Semantic and Safety Invariants

1. `Pending` and `Yielded` retain the exact computation state after every
   completed prefix; callers never reconstruct it from the initial value.
2. A resumed operation observes the same semantic lazy, promise, application,
   path position, and already-computed operands it observed before suspension.
3. A dependency is scheduler state, not an evaluation failure. Only permanent
   failures may enter lazy caches, promise failures, wait terminals, or
   failure ledgers.
4. Delegation changes evaluator control only. It creates no semantic lazy,
   promise, wait, task, cache entry, or externally observable identity.
5. `LazySource` remains the immutable semantic recipe. Evaluator progress is
   separate and machine-local.
6. Only the owner of a result destination may publish it. The pure reducer
   cannot cache into an arbitrary lazy or complete an outer task.
7. No `EvaluationValueAccess`, `RuntimeValueAccess`, mutator, raw `Value`, or
   unrooted `Gc<T>` survives a regional quantum.
8. Every value retained across a boundary has one exact matching-runtime root
   or another already-reviewed durable owner.
9. Ordinary non-suspending delegation adds no root registration, lock,
   reference-count update, scheduler admission, or semantic allocation.
10. No wait, coordinator transition, arbitrary Rust callback, reflection
    launcher, task poll, or worker sleep occurs under managed access.
11. Work sharing remains canonical. Resumption does not evaluate a fresh copy
    merely because another worker owned the original dependency.
12. Lazy cache publication continues to reject a deferred outer shell;
    assigned promises may retain one and WHNF follows it through ordinary
    demand.
13. Existing pure-lazy cycle detection and promise-inclusive retry semantics
    remain coordinator properties; the trampoline neither poisons temporary
    promise cycles nor creates a second dependency graph.
14. Reflection reservation and activation remain once-only and
    observer-independent. Resumption cannot launch or acknowledge the same
    task twice.
15. Failure context accumulated before and after a suspension has the same
    order and shape as uninterrupted evaluation.
16. Raw `Value::Net` remains already in WHNF. Only explicit net computation,
    function application, or interaction-net operations initiate net work.
17. Cursor-WHNF's bracketed contention wait remains its narrow structural
    exception; it does not authorize general evaluator waits under a mutator.
18. Every unbounded semantic recursion or collection loop beneath WHNF is
    either converted into iterative work or recorded as a named deferred
    exception before this plan closes.
19. A poll budget yields deterministically from an exact checkpoint. It never
    changes semantic results or becomes an observable source of failure.
20. Concurrent-ordering fixes are verified with controlled probes, barriers,
    channels, or model checking. Repetition is stress evidence only.

## Non-Goals

- Replacing the reflection freer-effect machine or changing `.alt`, `.cut`,
  transaction, or task semantics.
- Turning evaluator progress into a Glam value or public API.
- Making `LazySource` mutable, serializable, or observable through reflection.
- Using reference counts to infer exclusive ownership of a freshly constructed
  lazy.
- Constructing a semantic lazy or promise solely to hold evaluator control
  state.
- Introducing a root frame, moving collection, concurrent collection, or a
  new GC barrier.
- Optimizing work-unit weights, frame layouts, small-vector capacity, or JIT
  execution before profiling.
- Controlling recursion outside semantic WHNF evaluation, such as parser or
  diagnostic-renderer recursion.
- Replacing the existing interaction-net and net-construction worklists with a
  generic evaluator stack.

## Transition Phases

Execute and verify one named checkpoint at a time. Mark its status and add its
completion record to this plan in the same commit as the implementation it
describes. Repartition a checkpoint before implementation if its inventory or
diff is materially larger than anticipated; checkpoint names are coordination
tools, not a reason to land an oversized change.

### Phase W0 — Baseline, Exact Inventory, and Representation Gate

#### W0A — Deterministic replay witness

Status: complete on 2026-09-12.

Add a test-only boundary probe which forces request decoding to yield after
the freshly constructed effect-application lazy has acquired durable producer
ownership but before that producer completes. Complete the producer, resume
the parent, and observe the current replay from coarse `MachineWork` before
landing the fix.

W0A records the current inverse identity/count assertion. When W5D repairs
the same fixture, it must assert:

- the same lazy identity is consumed after resumption;
- the effect application is constructed once;
- the reflection task is reserved and activated once;
- request parsing occurs after the forced resumption; and
- the result of `anno { refl:.r () } "ready"` remains `"ready"`.

Do not replace the forced ordering with repeated worker runs.

Completion record: a one-shot test boundary now arms only after reflection
request decoding constructs its effect-application lazy. The test receives
that exact wait through a channel, gives the parent one poll, independently
completes the producer, and then resumes the parent. It characterizes the
current defect as two distinct application-lazy identities while proving the
nested `anno {refl:(.r ())} "ready"` task launches once and the final value is
still `"ready"`. This is intentionally a passing baseline of the broken
coarse-replay behavior; W5D reverses its identity/count assertions when the
production trampoline is installed.

#### W0B — Exact suspension and recursion census

Status: complete on 2026-09-12.

Build a source-backed manifest of every production call beneath `src/eval`
and every reflection/protocol adapter which can:

- call `eval_value_in`, `eval_lazy_in`, `eval_promised_in`, `apply_value_in`,
  `apply_values_in`, `produce_lazy_source_in`, `evaluate_in`, or an equivalent
  recursive evaluator helper;
- construct `EvaluationHalt::blocked` or
  `EvaluationHalt::unassigned_root`;
- translate a retryable halt into `WorkDependency`;
- call a scheduler/coordinator operation, callback, reflection launcher, net
  wait, or task poll; or
- recurse structurally over user-sized list, dictionary, path, operand, or
  application data.

Classify each occurrence by outer owner, result disposition, stable semantic
owner, dependency kind, remaining work, context/error behavior, and one of the
nine preliminary call shapes. Reconcile rather than duplicate the parent
D.2c raw-value and mutator-introduction manifests.

Verification: exact declaration fingerprints and per-family counts fail on an
unclassified addition or move. Deliberately misclassify one tail demand and
one nested post-demand operation, prove failure, then restore them.

Completion record: [`src/eval/whnf_inventory.rs`](../../src/eval/whnf_inventory.rs)
parses the production evaluator plus the narrow reflection and coordinator
adapters with `syn`. Every matching call or loop records its declaration,
ordinal, signal, outer owner, result disposition, stable semantic owner,
dependency kind, remaining-work shape, and context behavior. The exact
baseline is 305 occurrences with fingerprint
`0xbfae81b963836039` (`13_812_119_740_430_377_017`). The signal totals are:

| Signal family | Count |
| --- | ---: |
| value/lazy/promise demand | 110 |
| value application and source production | 29 |
| reflection-local evaluation | 17 |
| retryable halt and dependency translation | 16 |
| coordinator/reflection/host/net boundary | 47 |
| structural recursion | 24 |
| conservatively inventoried loops | 62 |

The reviewed resumption shapes are:

| Remaining-work shape | Count |
| --- | ---: |
| tail demand | 2 |
| demand then inspect | 157 |
| ordered operands | 4 |
| collection walk | 45 |
| application | 11 |
| key conversion | 23 |
| access path | 12 |
| diagnostic context | 1 |
| orchestration handoff | 50 |

The census deliberately includes loops conservatively: W6/W7 may prove a
particular balanced or statically bounded loop exempt, but new unreviewed work
cannot silently disappear from the inventory. Its classification validator is
also tested with forced tail-to-nested and nested-to-tail substitutions.

#### W0C — Progress representation decision

Status: complete on 2026-09-12.

Use the census to select the smallest complete representation:

- prefer source/operation-specific phases when they contain the entire
  remaining computation without duplication;
- introduce a shared compact frame only for a repeated nested resumption shape;
- use a frame vector or linked frame representation only if arbitrary nesting
  cannot otherwise be expressed without replay or a proliferation of
  equivalent phase enums; and
- preserve an iterative `Delegate` transition in every option.

Record the selected work variants, their durable roots, work-unit accounting,
size observations, and unwind behavior in this plan before production
conversion. Include a compile-exhaustive fixture over the selected variants.

Decision record: use one shared regional work stack with a separate durable
checkpoint form. Its selected vocabulary is `Delegate`, demand-and-inspect,
ordered operands, collection walk, application, key conversion, access path,
diagnostic context, and orchestration handoff. The test inventory contains an
exhaustive match over those nine variants so additions require an explicit
decision update.

The durable root policy is exact rather than uniform: the current focus has
one root; a frame roots only captured `Value` fields needed after its child
returns; indexes, enum tags, keys, counts, and immutable operation descriptors
remain immediate. Application and collection frames initially root each
retained semantic value independently. A later root-frame facility may pack
those roots without changing the work algebra.

Revision, 2026-09-16: the work algebra remains selected, but the storage
boundary is now more concrete. Focused callable-spill NC2.0 unifies regional
and net-owned raw work as one canonical `WhnfState` moved through zero-walk
role wrappers. W6G.3 then aggregates the durable form into one rooted managed
cell rather than waiting for `RootFrame`: the cell uses the same state and
edge visitor, and its complete pre/post edge sets pass through the existing
managed transition gateway. `RootFrame` remains a possible concurrent-GC
replacement, not the next correctness step.

Every focus transition, frame push/pop, child result delivery, and collection
element consumes at least one deterministic work unit. A `Delegate` consumes
one unit but performs no allocation or root registration. Boundary checkpoint
packing is charged to the transition which discovers it; W7 may tune weights,
but no user-sized loop may hide behind one unit.

The x86-64 bootstrap currently observes `Value = 64` bytes,
`RuntimeValueRoot = 32` bytes, and `Vec<Value> = 24` bytes. These are planning
observations, not ABI or regression latches. They favor keeping regional raw
values out of roots during uninterrupted work and deferring inline-frame or
packed-root optimization until profiling.

Unwind retains the prior durable checkpoint until a replacement is fully
rooted and installed. A panic during regional work discards only transient raw
state while the owner still has its prior checkpoint; the owning machine then
terminalizes or poisons according to its existing boundary policy. No empty
or half-published checkpoint is observable.

Exit: the failing ordering is reproducible, every relevant caller has one
owner, and the state shape is selected from evidence rather than assumed.

### Phase W1 — Additive WHNF Submachine

#### W1A — Protocol and module boundary

Status: complete on 2026-09-12.

Add the crate-private `WhnfComputation`, regional work/step, durable checkpoint,
dependency/boundary, and poll result types without changing production entry
points. Place semantic reduction with `eval`; keep coordinator translation in
the existing evaluation boundary. Do not make `WhnfComputation` a `Value`,
`LazySource`, public type, or `EvaluationTaskMachine`.

Compile-time/source checks must reject raw values and active access in durable
state. Size checks should observe rather than freeze representation unless a
specific regression threshold is justified.

Completion record: [`src/eval/whnf.rs`](../../src/eval/whnf.rs) now owns the
additive crate-private protocol. `WhnfComputation` retains a durable rooted
checkpoint; `RegionalWhnfWork` and `RegionalWhnfStep` describe callback-free
work beneath managed access; `WhnfPoll` exposes completion, dependency,
budget-yield, and rooted-failure outcomes. The seven caller-frame kinds are
kept distinct from tail `Delegate` and orchestration `Boundary` transitions,
so neither transition requires a synthetic stack frame.

[`src/evaluation/whnf.rs`](../../src/evaluation/whnf.rs) is the sole W1A
adapter from semantic `WhnfDependency` to coordinator `WorkDependency`.
Source-backed checks keep the protocol crate-private, absent from `Value`,
`LazySource`, `EvaluationTaskMachine`, and current production entry points,
and reject raw `Value` or active-access fields in durable state. The W0B
census scope now includes this evaluation-boundary module.

The x86-64 bootstrap observes `WhnfComputation = 56` bytes,
`DurableWhnfState = 56`, `DurableWhnfFrame = 40`,
`RegionalWhnfWork = 88`, `RegionalWhnfFrame = 40`,
`WhnfDependency = 40`, and `WhnfPoll = 48`. These are recorded observations,
not ABI assertions or regression thresholds. W1A intentionally implements no
polling or regional reduction; W1B first exercises that protocol with its
synthetic algebra before W1C adds checkpoint projection.

#### W1B — Synthetic delegation and resumption

Status: complete on 2026-09-13.

Implement a minimal test work algebra covering immediate completion, tail
delegation, nested post-demand work, permanent failure, dependency suspension,
and budget yield. Prove that:

- delegation is iterative and allocation-free;
- a forced dependency resumes at the recorded phase;
- a yield requires no dependency;
- no completed prefix is executed twice; and
- failure context before/after suspension remains ordered.

This fixture tests the state-machine protocol before real lazy scheduling can
obscure a protocol defect.

Completion record: `drive_regional` is an iterative, callback-free driver
which requires an active `EvaluationValueAccess` and consumes one deterministic
budget unit before each transition. Its `Delegate` arm only replaces the
current focus; it neither pushes a frame nor registers a root. Boundary and
budget outcomes return the exact regional work to their in-region caller, with
W1C still solely responsible for publishing a rooted checkpoint before real
managed access closes.

The synthetic fixture in
[`src/eval/whnf/tests/w1b.rs`](../../src/eval/whnf/tests/w1b.rs) proves immediate
completion, 100,000 iterative tail delegations without frame growth or root
registration, dependency-free budget yield and resumption, and an explicitly
ordered wait suspension. The latter stops with its dependency instruction
and two caller frames intact, changes only dependency readiness, and then
observes one post-demand continuation, no replay of the completed prefix, and
outer-to-inner failure contexts introduced before and after suspension.

The W0B source census now ignores test-directory sources and explicitly exempts
the target driver's own loop: W1B proves that loop budget-bounded, so it is not
a user-sized work source awaiting migration. The durable-owner baseline gained
the bounded `RegionalWhnfDrive` result. The suspension fixture uses a root-free
wait token so it cannot disturb shared-heap root-registration probes in
parallel tests. The verification pass also generalized the evaluator-surface
inventory to exclude nested test sources and moved an older root-neutrality
probe onto its own heap, eliminating its pre-existing dependence on unrelated
parallel tests using the shared fixture heap.

#### W1C — Regional-to-durable checkpoint publication

Status: complete on 2026-09-13 after partitioning into W1C.1-W1C.2.

##### W1C.1 — Projection and atomic replacement

Status: complete on 2026-09-13.

Implement the access-scoped projection and replacement protocol. Add probes
which verify root construction precedes access closure and every scheduler or
callback action follows it. Keep the prior durable checkpoint installed until
the complete replacement has been rooted, then install the replacement before
retiring the prior roots. Publish ready values and failures beneath the same
access region; return dependency, external-boundary, and yield dispositions
for interpretation only after access closes.

##### W1C.2 — Lifecycle and collection verification

Status: complete on 2026-09-13.

Exercise cancellation and panic/unwind with a nonempty prior checkpoint.

Under `aggressive-gc-verification`, collect between two polls and prove that
every live checkpoint value survives while superseded state becomes
collectible after retirement.

Exit: a scheduler-independent WHNF submachine can delegate, yield, suspend,
resume, complete, and fail without Rust-stack continuation state.

Completion record: `WhnfComputation::poll_in` projects a durable checkpoint
to regional work only beneath matching `EvaluationValueAccess`, drives one
bounded callback-free quantum, and roots every replacement, successful result,
or structured failure before that access closes. Dependency and external
boundaries remain explicit poll dispositions for the future owner to interpret
afterward. Checkpoint publication constructs the entire replacement while the
prior roots remain installed, atomically replaces the durable state, and only
then retires the prior roots.

The W1C fixtures force external suspension and exact resumption, permanent
failure with rooted context values, panic/unwind after mutating a regional
projection, and cancellation by dropping a suspended computation. Under
`aggressive-gc-verification`, an explicit collection between polls proves that
the installed focus, frames, and retained managed values survive while the
superseded checkpoint is reclaimed. The evaluator and value-access source
inventories record the three production publication sites and keep fixture
construction distinct. Production evaluator cutover remains intentionally
deferred to W2 and later phases.

### Phase W2 — Deferred Shell Demand and Client Ownership

Status: complete on 2026-09-13 after W2R-001. Implementation order was W2A.1, W2B.1, W2A.2,
W2C.1, W2B.2, W2D, then W2E.1-W2E.2: semantic shell inspection landed
before either scheduler coordination or owner cutover.

#### W2A — Regional lazy inspection

##### W2A.1 — Lazy shell inspection

Status: complete on 2026-09-13.

Add the callback-free cached-success, cached-failure, and uncached-lazy
transitions to the semantic WHNF reducer. The uncached transition carries the
exact rooted lazy identity out of regional access without admitting work.

Completion record: `poll_semantic_in` now recognizes outer lazy shells. A
cached success delegates iteratively, a cached failure becomes the rooted
terminal failure, and an uncached lazy publishes `WhnfDeferredRequest::Lazy`
with the exact `ManagedLazyRoot`. Focused tests prove that inspection admits no
deferred producer and that completing the requested root is observed by the
same checkpoint on resumption. The recursive-identity and root-publication
inventories classify the new durable request explicitly.

##### W2A.2 — Post-region lazy admission

Status: complete on 2026-09-13.

Split lazy demand into callback-free cache/source inspection and mutator-free
producer coordination. An uncached lazy returns its exact `ManagedLazyRoot` as
a boundary request; it does not reserve, pump, wait, or wake under access.
After access closes, the driver reuses or admits the canonical producer and
records the returned dependency in the same `WhnfComputation`.

On wake, reproject the same lazy and inspect its cache. Do not snapshot and
reconstruct a replacement `LazyValue` owner.

Completion record: `evaluation::whnf::poll_computation` now closes the
callback-free `RuntimeValueAccess` region before interpreting a deferred shell
request. An uncached lazy is admitted through its exact `ManagedLazyRoot`,
while unassigned promises remain direct coordinator dependencies and
task-owned self-observation preserves the prior diagnostic. Focused tests
prove post-region admission, exact promise identity, and zero promise-follower
admission; the access, root-publication, and durable-owner inventories record
the new boundary.

#### W2B — Promise inspection and following

##### W2B.1 — Promise shell inspection

Status: complete on 2026-09-13.

Add callback-free assigned-success, assigned-failure, and unassigned-promise
transitions. An unassigned promise leaves regional access as its exact
`ManagedPromiseRoot`; producer provenance and self-observation remain outer
owner policy.

Completion record: the regional reducer now reads a promise assignment once
under access. Success delegates directly to the assigned value, failure keeps
the original structured failure, and an unassigned promise leaves as its exact
managed root without constructing `PromiseFollower` work. Focused tests prove
all three paths, including assignment after suspension and exact-identity
resumption with zero deferred-task admission.

##### W2B.2 — Promise-follower ownership

Status: complete on 2026-09-13.

Split promise assignment inspection from promise-follower admission. Preserve:

- direct retryable observation of an unassigned resolver promise;
- the task-owned self-observation diagnostic;
- canonical follower work for assigned deferred values;
- same-runtime cross-session observation; and
- permanent producer failure/abandonment rules.

Whichever parts of `PromiseFollower` remain separate must host or delegate to
the same WHNF submachine rather than restart assignment evaluation.

Completion record: `PromiseFollower` now owns one `WhnfComputation` rooted at
the canonical promise allocation instead of a promise root plus an
`AwaitAssignment`/`FollowAssignment` phase marker. Its task poll delegates to
the same post-region WHNF driver as client demand, translates an unassigned
resolver promise directly to the exact coordinator dependency, preserves
task-owned self-observation policy, and resumes assigned deferred shells from
the installed checkpoint. A forced one-step fixture proves unassigned block,
budget yield after assignment, and terminal resumption without restarting the
assignment phase. The old follower `eval_value_in` occurrence and direct
promise-root owner were removed from the WHNF and recursive-identity ledgers.

#### W2C — Client-demand cutover

##### W2C.1 — Durable owner and budget yield

Status: complete on 2026-09-13.

Replace `ClientDemandOperation(RuntimeValueRoot)` with an operation owning one
initialized `WhnfComputation`. Add an explicit yielded client-demand
disposition and requeue it without dependency subscription. Blocked demand
retains both its exact subscription and unchanged computation checkpoint.

Retirement publishes only terminal WHNF/failure and drops the computation
outside coordinator locks.

Completion record: `ClientDemandOperation` now owns one `WhnfComputation` and
polls it through the post-region WHNF driver with the runtime task quantum.
Budget exhaustion produces `ClientDemandPoll::Yielded`; release removes any
obsolete exact subscription, restores the unchanged computation, and queues
it without installing a new dependency. A forced one-step poll after promise
assignment proves the formerly blocked demand yields, becomes queued with no
subscription, then completes from the same checkpoint. The WHNF census and
durable-owner ledger record removal of the old per-poll `eval_value_in` restart
and `EvaluationHalt` dependency translation.

#### W2D — Direct driver compatibility

##### W2D.1 — One client/WHNF driver

Status: complete on 2026-09-13.

Make synchronous assembler/test demand drive the same client/WHNF path.
Remove recursive cooperative pumping from the selected entry rather than
building a second trampoline. A synchronous caller may wait for claimed work
only through the existing mutator-free client-demand driver.

Completion record: synchronous `evaluate_root_whnf` and builtin evaluation
continue through `demand_whnf` and `drive_client_demand`, while the claimed
client operation now advances only its retained `WhnfComputation`. The outer
driver may iteratively claim producer work with no managed access held; the
operation itself neither recursively pumps nor calls `eval_value_in`. A source
latch ties those three layers together, and the existing synchronous,
compiler, and generic-client fixtures exercise the shared path behaviorally.

#### W2E — Deferred-demand verification

##### W2E.1 — Forced subscription orderings

Status: complete on 2026-09-13.

Force producer completion both before and after exact client subscription;
verify the canonical producer identity and the absence of lost or duplicate
wakes without relying on repeated scheduling.

Completion record: two scheduler-controlled fixtures now force both sides of
the exact-subscription race. The producer-before-subscription case keeps the
client operation claimed, explicitly promotes and completes its canonical
lazy producer, then releases the stale blocked poll and verifies immediate
requeue. The subscription-before-producer case first observes one installed
exact subscription, then runs the producer and observes one wake and cleanup.
Both paths admit one deferred producer, invoke its source once, retire the
subscription, and complete the same client demand without repeated runs.

##### W2E.2 — Lifecycle, cycle, and collection matrix

Status: complete on 2026-09-13.

Force both producer-before-subscription and subscription-before-producer
completion orderings. Cover cached/uncached lazy, assigned/unassigned promise,
cross-session producer, cancellation, abandonment, pure lazy cycle, and
promise-inclusive retryable cycle. Record root registration and producer
admission counts across each boundary.

Completion record: W2E.1 supplies both forced producer/subscription orderings;
the W2A/W2B shell fixtures retain the cached/uncached and
assigned/unassigned cases. Existing client-owner fixtures cover cross-session
production, cancellation, and abandonment. New deterministic fixtures prove
that a pure lazy cycle publishes one canonical cached failure while a
lazy/promise cycle remains retryable and unpoisoned. Under
`aggressive-gc-verification`, an explicit collection between the blocked
client checkpoint and promise assignment proves that the checkpoint's root
survives without reconstructing its focus. The root-publication and mutator
admission ledgers classify the added test boundary explicitly.

Mandatory post-W2 review: audit correctness and later-phase drift before
converting lazy-source production.

#### Post-W2 review — 2026-09-13

Status: complete after resolving W2R-001 and rerunning the ordinary workspace
plus the focused aggressive-GC ownership matrix.

The two production owners selected for W2 each retain exactly one
`WhnfComputation`: client demand owns the request from admission through
terminal publication, and `PromiseFollower` owns the canonical promise
projection used by the remaining deferred-task adapter. Both use the same
post-region driver. Dependency subscription, producer admission, task
self-observation policy, and client result publication remain in their outer
owners; no callback or coordinator operation moved beneath managed access.

The review found one stale negative source latch from W1A: it still prohibited
`WhnfComputation` in the two files deliberately cut over by W2. The test was
first observed failing, then changed to require those named owners while
continuing to reject premature reflection ownership. Broad module-level
dead-code allowances also carried obsolete “production inactive” rationale;
they are now limited to the frame and boundary variants deliberately staged
for W3-W4.

The actual `LazyTaskMachine` remains aligned with W3: ordinary `Produce` and
`Follow` work still use the recursive source path, while `HostCall` and
`NetConstruction` remain explicit outer modes. W3B and W3C each cover several
independent source families and should receive a checkpoint partitioning pass
immediately before implementation, once W3A reveals the concrete source
machine handoff. W4-W8 require no semantic revision from the W2 cutover.

W2R-001: `CoreValueFactory::cached` deliberately runs an arbitrary closed
cache-family builder beneath one outer `RuntimeValueAccess`. This protected
unrooted intermediate managed edges during construction, but compiler cache
builders also call `evaluate_closed`, which now reaches client demand and its
post-region scheduler orchestration. The W2 boundary assertion correctly
observes that the nested WHNF region has closed while the enclosing cache
mutator remains active. Removing the assertion would permit coordinator work
and callbacks beneath managed access, contrary to the I3 and W2 boundary.

Resolution requires an ownership decision before W3. The likely direction is
to split callback-free cache construction from orchestrated cache building,
then make the compiler and diagnostic cache builders retain every managed
intermediate explicitly while evaluation runs outside access. A blanket
removal of the outer cache region is unsafe until those intermediate raw
`Value`/resolved-expression edges have been inventoried. A special permission
to retain one mutator across cache evaluation is smaller but would preserve
the callback and future collector-starvation defect.

##### W2R-001 remediation — Rooted candidate construction

Decision: a cache miss is not currently discovered beneath an existing
managed-access region. The large access scope is introduced by
`CoreValueFactory::cached` itself as a conservative legacy ownership blanket.
Do not add a cache-pending WHNF disposition or install an in-progress entry.
Racing callers continue to build independent complete candidates and race
only the final `RuntimeCacheEntry` insertion.

###### W2R-001A — Remove implicit access

Status: complete on 2026-09-13.

First add a fixture which fails because a cache builder inherits managed
access. Then invoke the candidate builder with no implicit access and retain
the existing complete-winner race. Document that a family builder may open
its own short callback-free access regions, but cannot assume hidden access
across orchestration or waiting.

Completion record: a focused cache-family fixture first failed by observing
the hidden managed access supplied by `CoreValueFactory::cached`. Candidate
construction now runs with no implicit access; builders may open only their
own bounded callback-free regions. Complete-candidate admission, runtime-root
validation, harmless duplicate construction, and one installed winner retain
their prior behavior.

###### W2R-001B — Production family root audit

Status: complete on 2026-09-13.

Audit `GCompilerValues` and `CachedDiagnosticFormatter` one construction step
at a time. Every managed raw `Value` embedded in a later closed expression
must remain backed by a live `RuntimeValueRoot` until that expression is
lowered into its own rooted input. Add an explicit root only for an observed
gap; do not introduce a general prepared-expression wrapper preemptively.

For each closed helper evaluation, construct and root the input during one
short access region, close the region, and then use the normal resumable
client-demand path. Retain every completed helper root in the local candidate
until the complete family is admitted.

Completion record: the field-by-field audit found no missing owner.
`GCompilerValues` retains each evaluated helper as a `RuntimeValueRoot`; raw
projections used to construct `std` remain backed by the live `not` and
`could` roots, and effect values remain backed by roots in the construction
map. Construction now names every final field before bundle assembly and
offers a test checkpoint after all thirteen rooted steps. A deterministic
fixture collects at every checkpoint and again after the complete candidate
returns but before cache admission. `CachedDiagnosticFormatter` already
consisted of one rooted function; a companion fixture collects that
unpublished candidate. Both pass in ordinary and aggressive-GC modes without
a prepared-expression wrapper or an added root.

###### W2R-001C — Collection and race verification

Status: complete on 2026-09-13.

Force collection between representative compiler-family construction steps
and before final publication. Preserve the existing proof that racing misses
may execute multiple builders but all callers receive one installed complete
family. Verify that a losing candidate retires normally and that reopening an
installed family registers no replacement roots.

Completion record: the compiler construction fixture forces collection after
every rooted helper and before publication, while the formatter fixture
collects its unpublished candidate. The existing barrier-controlled race
continues to prove that two builders may run and return one installed winner.
A new two-candidate barrier fixture gives each candidate an independent drop
signal: exactly the loser retires after atomic installation, and the winner
retires with the value domain. Existing root-registration assertions prove
that reopening either installed production family creates no replacement
roots.

###### W2R-001D — W2 closure

Status: complete on 2026-09-13.

Run the focused cache, compiler, formatter, ownership-inventory, and WHNF
suites in ordinary and aggressive-GC modes, followed by the ordinary
workspace. Mark W2 and its post-review complete only after no cache builder
reaches WHNF orchestration beneath managed access. Complete-repository
aggressive-GC certification remains owned by GCI11R-002D.2c-H and the final W8
gate; this intermediate W2 checkpoint does not duplicate that open migration.

Completion record: formatting and clippy pass, the ordinary workspace passes
all 1,534 active library tests and every integration/doc-test group, and the
focused aggressive-GC matrix passes the runtime-cache, compiler-family,
diagnostic-formatter, declaration cache-miss, WHNF, assigned-promise-cycle,
mixed promise/lazy-cycle, and collection-between-polls fixtures. Exact source
ledgers record the added declaration publication/access region, root-free
deferred-ID control state, and second promise-root boundary. The attempted
complete aggressive run still reaches the independently planned
GCI11R-002D.2c ownership failures; its first isolated failure is unchanged in
the public array/deque annotation path rather than this cache or W2 boundary.

###### W2R-001D.1 — Declaration-resolution access boundary

Status: complete on 2026-09-13.

The first ordinary-suite closure run found one caller-side continuation of the
same defect: `ModuleLowerer::lower_declaration` projected its durable module
roots, resolved arbitrary syntax, and lowered the resulting semantic
expression beneath one declaration-wide access region. A previously unseen
effect path may populate its compiler subcache during resolution, so that
region indirectly encloses the cache builder's closed WHNF evaluation even
after W2R-001A removed the builder's own implicit region.

Split definition, object, and extension declarations into three boundaries:

1. briefly project the durable current-definitions and reflection roots;
2. resolve syntax to the front end's affine semantic expression with no
   managed access held; and
3. lower and root the completed semantic expression in a fresh bounded access
   region.

The durable roots remain live throughout step 2, so every projected managed
edge used by the resolved expression retains its owner. Keep import and
`unique` construction on their existing bounded paths: they construct lazy
host-call or immediate dictionary state without driving evaluation. Add a
fresh effect-path regression so cache order in parallel tests cannot mask the
boundary violation, then rerun the original reflection fixture before the
full W2R-001D matrix.

Completion record: module definition, object, and extension lowering now
projects the current definitions and reflection boundary in one short region,
resolves the complete syntax expression after that region closes, then lowers
and roots the affine semantic expression in a new short region. The two source
roots stay live across resolution. A fresh, runtime-local effect path forces a
deterministic compiler subcache miss and passes without inheriting managed
access; the original reflection-branch and diagnostic-callback fixtures pass
as well. The compiler access inventory records the new explicit projection
boundary.

###### W2R-001D.2 — Assigned-promise recursion handoff

Status: complete on 2026-09-13.

The ordinary-suite run then exposed a deterministic hang in the existing
`P := P` compatibility fixture. W2's shell reducer directly delegates through
successful promise assignments; an assigned promise cycle therefore exhausts
each finite quantum and resumes the same cycle forever. The former evaluator
admitted the canonical promise follower for a deferred assigned value, whose
self-dependency let the coordinator expose a stable retryable block without
poisoning the promise.

Retain deferred identities crossed by one `WhnfComputation` as root-free
control state across yields. Insert an identity only when following a completed
shell, not when first suspending on an unresolved shell. On a repeated promise,
leave regional access with an explicit promise-follow request. After access
closes, admit or reuse the canonical promise follower and block on its wait.
This preserves direct delegation for ordinary acyclic assignments and restores
the retryable-cycle behavior for self and multi-promise cycles. Add a bounded
regional regression as well as the existing end-to-end compatibility fixture.

Completion record: durable and regional WHNF state now retain the root-free
set of deferred identities crossed through completed shells. Unresolved shells
do not enter the set. Encountering an assigned promise twice emits a distinct
`PromiseFollow` request; the outer driver admits or reuses the canonical
promise follower only after access closes. A bounded two-step regional fixture
proves the handoff without admitting work beneath access. The existing
end-to-end self-promise and mixed promise/lazy fixtures both return their
retryable blocked result without poisoning either cell, while the ordinary
assigned-success fixture continues to delegate without a follower.

### Phase W3 — Lazy Producers and Source Progress

#### W3A — Lazy task result disposition

Make `LazyTaskMachine` own one source-oriented `WhnfComputation` for ordinary
semantic sources. Keep terminal cache publication in `LazyTaskMachine` and
prove that `Ready` contains no deferred outer shell. Replace the specialized
`Follow` loop with general delegation only when that preserves the same cache
owner and dependency graph.

`HostCall` and `NetConstruction` remain explicit outer modes until W4.

##### W3A.0 — Source-family drift and checkpoint partition

Status: complete on 2026-09-13.

The post-W2 implementation still has the intended outer split: host callbacks
and net construction are explicit `LazyTaskMachine` modes, while every other
source passes through `Produce` and then a specialized rooted `Follow` value.
The generic `{kind, cursor, retained}` frame placeholder is not, however, a
safe final encoding for source progress. Application, access, key conversion,
and list walks need typed phase state so an invalid cursor/retained-value
combination cannot be manufactured accidentally.

Use one `WhnfComputation` enum with a source-entry checkpoint and the existing
rooted value-demand checkpoint. W3A first transfers an ordinary lazy owner
into that computation, records the source result as a rooted source
checkpoint, and removes `LazyTaskWork::Follow`. Until a source family is
converted, its checkpoint retains the former direct compatibility demand:
immediately switching that result to bounded general delegation caused deep
legacy sources to exhaust their nested pump budget and replay completed
prefixes indefinitely. W3B-W3D replace that compatibility step and each
recursive source entry with typed durable/regional states. No source family is
considered migrated until its typed phase state prevents replay.

Partition the rest of W3 as follows. The W3B.3 review confirmed that source
families cannot retain one attached function-call net without the pollable
normalization owner originally scheduled for W4C. The implementation order is
therefore dependency-based rather than phase-number-based:

1. W3A.1 installs the source-entry/value-demand computation shape and cuts
   over `LazyTaskMachine` without moving host or net-construction modes.
2. W3A.2 latches cache publication, deferred-result delegation, yield, and
   failure behavior at the new owner boundary.
3. W3B.1 converts ordinary application and W3B.2a converts function fixpoints.
4. W4C.1 extracts the existing net driver into a reusable pollable owner;
   W3B.3 then retains one attached function-call net in that owner. This
   deliberately absorbs the common normalization part of W4C early rather
   than adding a temporary nested lazy or a second driver.
5. W3C.1 converts access-path progress; W3C.2 converts recursive key/path
   conversion; W3C.3 converts lazy list chunks and list-backed projections;
   W3C.4 closes their structured-error and forced-yield matrix.
6. W3B.2b converts object fixpoints after those child operations have typed
   resumable owners.
7. W3D.1 inventories the remaining production `SemanticComputation`
   operations; W3D.2 replaces the list-effect operations with explicit typed
   work; W3D.3 classifies or removes the test-only opaque thunk and performs
   the ordinary-source closure audit.

After W3D, perform an extra-thorough post-W3 audit before completing the
remaining W4 boundary sources. The audit must account for every ordinary
`LazySource`, every compatibility call left in `produce_lazy_source_in`, and
every typed checkpoint's exact roots. Drift from the original phase order is
acceptable only where this dependency order records it explicitly.

##### W3A.1 — Source-entry ownership and lazy-task cutover

Status: complete on 2026-09-13.

`WhnfComputation` now has an explicit durable source-entry checkpoint in
addition to its rooted value-demand checkpoint. `LazyTaskMachine` classifies
the two W4 outer modes first, then transfers every ordinary source and its
exact lazy owner into that checkpoint. A successful source result is rooted
and installed into the same computation before it is demanded. The
specialized outer `Follow(RuntimeValueRoot)` mode is gone; the source
checkpoint temporarily owns its compatibility demand until W3B-W3D replace
it family by family. Runtime provenance is retained explicitly by the
source-entry checkpoint and checked at the one-way source-result handoff.

##### W3A.2 — Lazy cache and delegation verification

Status: complete on 2026-09-13.

A bounded source fixture now returns an unresolved promise, observes the
source-entry handoff, polls the retained promise dependency more than once,
assigns it, and proves that the source ran exactly once throughout. The final
task result and lazy cache both contain the assigned number rather than the
deferred promise, and terminal cache publication removes the source. A second
fixture proves that a permanent source failure is cached and repeated task
polls do not replay its source.

The first general-delegation cutover passed the bounded fixtures but made
several deep compiler and diagnostic evaluations fail to finish: a legacy
access source could exhaust its finite nested pump after creating child work,
then restart from its initial path on the next poll. The source-owned
compatibility demand restores the pre-W3 scheduling behavior without
reintroducing a separate machine mode. This is transitional, not evidence
that the family is resumable; the W3 closure audit must remove it after every
ordinary source has exact phase state.

#### W3B — Application and computed fixpoint sources

Convert `LazySource::Application`, `FunctionCall`, and `ComputedFixpoint` into
phase-aware work. Preserve partial application, exact argument order,
function-stage sharing, fixpoint marker identity, object fixpoint behavior,
and the rule that only saturation creates memoized function work.

Add a forced suspension after every phase and assert no function application,
net attachment, or fixpoint marker is rebuilt after resumption.

##### W3B.1 — Ordinary application

Status: complete on 2026-09-13 through W3B.1a-W3B.1c.

###### W3B.1a — Typed application frame and direct callable families

Status: complete on 2026-09-13.

Replace the generic application placeholder with typed durable/regional
argument state. Demand the current function once, then advance builtin,
partial-builtin, and function application without re-entering the original
source. Saturated builtins and functions produce their existing memoized lazy
work; extra arguments remain in the frame and apply only after that result is
demanded. Preserve a narrow explicit compatibility disposition for dictionary
application until W3B.1b rather than hiding its semantic-undefined recursion
inside the regional reducer.

The application frame now owns one rooted argument vector and an exact next
argument index. Builtin and partial-builtin saturation, function-stage
attachment, and saturated function-call allocation occur under the active
regional access. Each transition publishes only its resulting semantic value;
immediate arguments do not acquire artificial managed roots. Extra arguments
remain behind the same frame and cannot run before a saturated lazy result is
demanded. Dictionary application alone returns the named
`LegacyApplication` disposition, and the WHNF census records that one remaining
`apply_values_in` compatibility call until W3B.1b removes it.

###### W3B.1b — Dictionary applicability

Status: complete on 2026-09-13.

Represent `eff` tagged-payload recognition, recursive semantic-undefined
checks, and `apply` member demand as typed application phases. Remove the
dictionary compatibility disposition after equal success, mismatch, failure,
and retryable-wait behavior is established.

Dictionary application now retains a typed tag-recognition frame and a
bounded recursive semantic-undefined walk. Each nested dictionary member is
demanded through the shared deferred-shell protocol, and one ancestor is
unwound per regional step. The original `eff` payload remains distinct from
its demanded classification value, preserving effect construction semantics.
The fallback `apply` member still uses the prior deliberately shallow test for
literal `{}` before demanding the member. The compatibility disposition and
its outer `apply_values_in` call are removed.

###### W3B.1c — Application phase verification

Status: complete on 2026-09-13.

Force a yield or dependency after function demand, partial application,
saturation, and each extra argument. Assert exact argument order, one stage
attachment or saturated-call allocation per completed prefix, and unchanged
structured non-callable failures.

The focused matrix now forces the callable promise, partial-builtin and
partial-function boundaries, dictionary `apply` member demand, saturation,
and the first extra argument in exact order. A saturated result is explicitly
published before the extra argument can be attempted, and that attempt retains
the established structured non-callable failure. Together with the existing
source-backed application matrix, this latches success, mismatch, and
dependency behavior without relying on repeated scheduling.

##### W3B.2 — Function and object fixpoints

Status: complete on 2026-09-13 through W3B.2a-W3B.2b.

###### W3B.2a — Function fixpoints

Status: complete on 2026-09-13.

Convert `FixpointComputation::Function` directly into the typed application
checkpoint with the exact producer lazy as its marker argument. The source is
selected once; subsequent suspension belongs to ordinary application and
outer-shell demand.

Function fixpoints now enter the typed application checkpoint directly from
the lazy source selector. The exact managed producer is projected as the knot
marker once, so strict cycles retain their prior identity and suspended bodies
resume through ordinary WHNF dependencies. The compatibility helper no longer
contains either the function application or its following recursive demand;
the source census latches both removals.

###### W3B.2b — Object fixpoints

Status: complete on 2026-09-13 through W3B.2b.1-W3B.2b.4 after W3C.3.

Object construction consumes the same access, key, and lazy-list child
operations converted by W3C. Building a parallel compatibility trampoline
before those child frames exist would preserve two representations and make
the later removal harder. After W3C.3, represent C3 traversal and the ordered
definition-mixin fold as one explicit object-construction computation. This is
a dependency reorder within W3, not a relaxation of the W3 closure gate.

####### W3B.2b.1 — Reusable resumable logical-list front

Status: complete on 2026-09-13.

Wrap W3C.3a's non-forcing decomposition in one durable owner which can demand
a deferred list-or-binary chunk, prepend it to the exact suffix, and resume.
Return one rooted value plus rooted tail or exhaustion; do not force a strict
value leaf. Object dependency traversal and W3D list-effect projection share
this owner.

`ListFrontMachine` owns the current list, an optional WHNF computation for
the exact deferred chunk, and the chunk's exact suffix as runtime roots. A
resolved list or binary chunk is prepended to that suffix; strict leaves are
returned without demand.

####### W3B.2b.2 — Explicit object C3 traversal

Status: complete on 2026-09-13.

Replace recursive `object_c3_linearization` with an explicit DFS frame stack.
Each frame retains its exact spec, name, dependency cursor, completed child
linearizations, and direct-dependency sequence. Reuse the recursive key owner
for names and the logical-list-front owner for dependencies. Preserve
anonymous-before-named ordering and referential spec-identity validation.

`ObjectLinearizationMachine` now uses explicit DFS frames for spec demand,
name conversion, dependency-list demand and traversal, and child return.
Named specs retain referential dictionary equality while anonymous specs
receive occurrence-local identities exactly as before.

####### W3B.2b.3 — Explicit object mixin fold

Status: complete on 2026-09-13.

Retain the reversed C3 result, current base, self marker, spec cursor, and two
application phases. Each definitions mixin is demanded and applied to base
then self exactly once through ordinary WHNF application work. Validate the
dictionary result before advancing and install the original spec only after
the final mixin.

`ObjectMixMachine` retains the reversed linearization, exact base and self
roots, current spec index, and base/self application phase. Each application
is an ordinary typed WHNF checkpoint; only a dictionary result advances the
fold. A definitions value is first demanded to WHNF. An
`ObjectComposedDefs` partial is treated as an inspectable composition recipe,
flattened prior-before-extension, and each resulting mixin is applied through
the same resumable phases. This is necessary for object-source closure: the
generic saturated-builtin source would otherwise restart the composition
after a mixin lambda returned deferred function-call work.

####### W3B.2b.4 — Object source closure

Status: complete on 2026-09-13.

Force suspension in spec demand, name conversion, dependency chunks, nested
dependency specs, and both mixin applications. Preserve the existing C3,
identity, anonymous-ordering, and result fixtures. Remove object fixpoints
from `produce_lazy_source_in` and the recursive construction export after its
last source caller is gone.

Forced-order fixtures cover the original spec, computed name, deferred
dependency chunk, nested dependency spec, and each of the two mixin
applications. The recursive compatibility implementation and its export are
removed. The full-suite ordering exposed one adjacent scheduler defect: an
exactly claimed deferred producer could become dormant after a cooperative
yield before its parent published the dependency. Exact live-demand claims
now preserve queued demand across yields, with a direct coordinator fixture
for that ordering. A composed-mixin fixture also latches the case where the
final extension call produces a lazy function-call result; it must resume
inside the object owner rather than recreate child work.

##### W3B.3 — Saturated function-call and net bridge

Status: complete on 2026-09-13 with W4C.1c.

Select a `FunctionCall` source once, attach its argument vector once, and move
the resulting managed net into the reusable normalization owner from W4C.1.
The function source does not allocate a nested lazy merely to retain the net.
Blocked semantic calls retain the same net and normalization checkpoint.

`LazyTaskMachine` now selects a `FunctionCall` once and installs a boxed
`NetWhnfMachine`. Stage duplication, argument attachment, managed-net
construction, and request rooting happen only during that transition. A data
payload is handed to the same ordinary WHNF demand path rather than cached
prematurely, so lazy function results retain their established semantics. The
former source-time `evaluate_function_call` path is removed.

#### W3C — Access and key/list source work

Convert dynamic path evaluation, intermediate dictionary demand, recursive
key conversion, lazy list chunks, and sequence projections. Retain explicit
path/collection indices and accumulators. Missing dictionary members remain
`{}`; type mismatches and index errors retain their current structured
contexts.

##### W3C.1 — Access-path progress

Status: complete on 2026-09-13 for static path parts; dynamic part conversion
is W3C.2.

Static access sources now select a typed path checkpoint containing the exact
key vector and next index. Each demanded dictionary advances one key and
delegates its selected member through ordinary WHNF; a missing member remains
`{}` and a non-dictionary retains the established structured failure. A
forced intermediate promise fixture proves resumption from the retained path
index. W3C.2 subsequently removed the compatibility access helper from
ordinary lazy-source production entirely.

##### W3C.2 — Recursive key and computed-path conversion

Status: complete on 2026-09-13 through W3C.2a-W3C.2b.

###### W3C.2a — Scalar computed access and recursive key owner

Status: complete on 2026-09-13 with W3C.3b.

Introduce one typed source owner for dynamic access. Retain the base, path
part index, dynamic-argument index, and any in-progress recursive key
conversion explicitly. A scalar `Index` evaluates and converts exactly once;
dictionary key conversion retains an explicit member cursor and accumulator.

`AccessMachine` now retains rooted source arguments, current selection,
dynamic-argument and path cursors, pending keys, and a typed recursive key
converter. Scalar dynamic keys and dictionary-valued keys resume after their
exact child promise without re-reading completed members. Dynamic keys are
still evaluated before the corresponding dictionary base, preserving the
legacy source evaluation order.

###### W3C.2b — Computed path lists

Status: complete on 2026-09-13 with W3C.3b.

Use the same key converter and resumable logical-list walk for `PathIndex`.
Append each completed list item key to the current access path without
restarting either the source list or a previously selected dictionary.

`PathIndex` uses the same `KeyListMachine` as list-valued key conversion but
returns its completed items as sequential access keys. A forced deferred
middle-chunk fixture resumes at that chunk and preserves the completed prefix.

##### W3C.3 — Lazy list chunks and list-backed projections

Status: complete on 2026-09-13 through W3C.3a-W3C.3c. Its substrate was pulled
before W3C.2 because recursive key conversion and `PathIndex` are themselves
list clients.

###### W3C.3a — Non-forcing logical front decomposition

Status: complete on 2026-09-13.

Add a stack-bounded representation-level list operation which returns one
strict item, one deferred chunk plus its exact logical suffix, or exhaustion.
It never invokes evaluation and therefore cannot suspend or retain a managed
access region. This is shared infrastructure, not a second evaluator.

`List::pop_front_step_by` now walks arbitrary `Concat` depth with an explicit
local worklist and reports `Item`, `Deferred { deferred, suffix }`, or
`Empty`. It duplicates only the selected leaf or thunk and preserves the
remaining persistent structure. Fixtures cover a deferred middle segment and
its exact byte suffix as well as a 20,000-node compatibility concat spine.

###### W3C.3b — Resumable list/key walk

Status: complete on 2026-09-13.

Build the typed list walk used by recursive key conversion. A deferred chunk
is evaluated once, checked as list-or-binary, then prepended to the retained
suffix. Strict value leaves delegate through the ordinary WHNF/key converter;
byte leaves become numeric keys without demand.

`KeyListMachine` owns only runtime roots, item/chunk state, and completed
`Key`s across polls. It expands a forced list or binary chunk before its exact
retained suffix, delegates strict values to `KeyConversionMachine`, and maps
compact bytes directly to numeric keys. The original path operand remains
list-only while deferred chunks preserve the established list-or-binary rule.
No projected raw `Value` or `List` crosses its access regions.

###### W3C.3c — Remaining list-backed source projections

Status: complete on 2026-09-13 with W3D.1.

Inventory source-time sequence projections still reachable from ordinary
lazy production and either reuse the list walk or give them an explicit typed
owner. Generic saturated `Builtin` sources are accounted separately by W6 and
are not silently declared migrated here.

The source census found no independent sequence-projection family. All four
production `SemanticComputation` operations are lazy list-effect projections
and therefore move together in W3D.2. The remaining constructor and operation
uses are containment and edge fixtures.

##### W3C.4 — Source-family suspension and diagnostics closure

Status: complete on 2026-09-13 for access and recursive key/list source work;
W3C.3c retains the broader list-projection inventory.

Fixtures now cover scalar and recursive-dictionary promise suspension,
deferred computed-path chunks, path/chunk kind diagnostics, missing/static
members, non-dictionary bases, and the pre-existing computed-path integration
case. Source-owned WHNF computations retain the owning lazy identity and the
last assigned promise reached within the same demand. If that promise leads
back to the source lazy, orchestration resumes through the canonical promise
follower rather than incorrectly poisoning the mixed retryable cycle. Pure
lazy cycles retain their permanent-failure behavior.

#### W3D — Semantic computation representation

Inventory the production `SemanticComputation` function-pointer uses. Replace
every suspendable use with an explicit inspectable operation/work variant.
Keep a function pointer only when the operation is proved regional,
nonsuspending, and bounded, or retire the representation entirely.

The test-only opaque `SemanticThunk` may not serve as evidence for resumable
production work. Either constrain it to nonsuspending fixtures or replace it
with explicit synthetic work.

Exit: object-fixpoint and semantic-computation lazy production no longer
depends on a Rust-stack continuation across deferred children. External and
net sources close in W4. Saturated generic `Builtin` sources remain an
explicit compatibility exception until their operation families move in W6;
W3 does not falsely certify their recursive bodies.

##### W3D.1 — Production operation inventory

Status: complete on 2026-09-13 with W3C.3c.

There is one production constructor site, in the list-effect handler, and
four function-pointer operations: run one effect, sequence one list head,
cut to one head, and publish one fixpoint head. W3D.2 replaces this closed
production family with an inspectable list-effect recipe and pollable owner.

##### W3D.2 — Explicit list-effect source work

Status: complete on 2026-09-13 through W3D.2a-W3D.2c.

###### W3D.2a — Inspectable list-effect recipe

Status: complete on 2026-09-13.

Replace the function pointer plus capture array with a closed core recipe for
run, sequence, cut, and fix. Each variant exposes its exact managed edges to
the existing compatibility tracer without an opaque callback.

`ListEffectComputation` is a closed core recipe with those four variants.
Its compatibility-edge implementation reports operation, result-list,
continuation, and fix-handle values directly.

###### W3D.2b — Pollable list-effect source owner

Status: complete on 2026-09-13.

Interpret the recipe with explicit phases, rooted operands, ordinary WHNF
subcomputations, application state, and W3B.2b.1 list-front work. A yielded or
blocked run resumes after the exact completed effect/application/list prefix.
Promise publication for fix remains outside a retained managed-access region
and occurs once.

`ListEffectSourceMachine` owns run phases and reuses `ListFrontMachine` for
sequence, cut, and fix. Sequence preserves the lazy child application and
recursive tail as two inspectable recipe-backed thunks; observing the next
logical result still demands them in source order. Fix publication occurs
only after its front result is available and after managed access closes.

###### W3D.2c — List-effect source verification and retirement

Status: complete on 2026-09-13.

Force dependencies at every recipe boundary, preserve lazy sequence/cut/fix
behavior and failures, then remove production `SemanticComputation`. Test-only
synthetic work is addressed separately by W3D.3.

Forced dependencies cover run input, sequence continuation, cut input, and
fix input. Existing syntax/integration fixtures retain sequence, alternative,
cut, fix, mismatch, and failure behavior. Production construction and source
dispatch no longer use `SemanticComputation`.

##### W3D.3 — Opaque fixture policy and W3 closure audit

Status: complete on 2026-09-13.

`SemanticComputation`, its operation pointer, constructors, dispatch arm, and
edge adapter are all test-only. `SemanticThunk` likewise remains a test-only
compatibility fixture. Some scheduler and ownership tests deliberately use
these opaque fixtures to inject a precise halt or callback, but neither type
is accepted as evidence that production source work is resumable. Replacing
the broad synthetic test harness would add a parallel test-only machine
vocabulary without closing a production boundary, so it is deferred rather
than misreported as production work.

The production closure census leaves the generic saturated `Builtin` source
as the declared W6 compatibility family and reflection/host work as W4A/W4B.
Net computation, function calls, and the reusable net driver have already
moved through the reordered W4C prerequisites. No production object or
list-effect source retains an opaque Rust continuation.

W3 closeout also exposed two scheduler handoff defects which the smaller
machines made easier to reach. Exact demand published while a deferred
producer was running could be lost when that producer yielded; the producer
now latches such demand through release. Separately, the temporary
one-ordinary-machine-per-demand-session admission rule could make a target
with a broad observation look stably blocked while another machine in the
same runtime owned relevant progress. That state is now `Busy`, and patient
evaluation waits for the owning quantum. A genuinely quiescent reflection gate still
returns a retryable blocked result. Forced-order tests cover both boundaries,
including publication between `NoProgress` and its immediate recheck;
repetition is not used as evidence.

The per-session admission rule is compatibility containment, not a semantic
serialization guarantee. Revisit and preferably remove it after W6 retires
generic recursive builtin source evaluation. Exact nested demands already
bypass it, and sparks remain independently schedulable.

### Phase W4 — External and Existing Pollable Boundaries

Status: complete on 2026-09-13.

#### W4A — Reflection lazy sources

Status: complete on 2026-09-13 through W4A.0-W4A.2.

Split reflection-source recognition/target projection from reservation,
activation, polling, acknowledgement, and failure propagation. Retain the
once-only reservation observation and first-observer activation permit.

The WHNF checkpoint must identify whether it awaits gate completion or a
returned value. A completed gate delegates to its existing target; a returned
value delegates to the task result. Neither path creates a replacement
reflection computation.

##### W4A.0 — Reflection source and reservation inventory

Status: complete on 2026-09-13.

Reconcile gate and returned-value forms with `ReflectionTaskReservation`, the
first-observer permit, acknowledgement, and failure paths. Record the exact
roots and scalar identities which survive each returned poll before changing
representation.

The enclosing `ManagedLazyRoot` is the durable semantic root while reflection
work is pending: its managed source traces the effect and optional gate target.
`ReflectionSourceMachine` retains the source computation and exactly one
`ReflectionTaskReservation`; the reservation contributes only the stable task
observation/handle and the first observer's shared one-use activation permit.
Activation temporarily roots the effect, then the coordinator-owned reflection
machine owns that root. Returned polls therefore retain the managed lazy root,
the reservation's scalar task identity, and (after completion) the
coordinator-owned result root. No poll-spanning raw value is introduced by the
source owner.

##### W4A.1 — Explicit reflection source owner

Status: complete on 2026-09-13.

Move recognition, target projection, reservation, activation, and result
delegation into a typed pollable source owner. Preserve the distinction
between gate completion and a returned value, and never reserve or activate
one reflection computation twice.

`ReflectionSourceMachine` now separates first reservation from subsequent
polls. Reservation records the exact task once and defers its activation until
the evaluator region closes; the first later poll observes either its stable
wait or terminal result. Gate completion delegates to the traced target and
return-value completion delegates to the coordinator's rooted result. The old
reflection arm in `produce_lazy_source_in` is unreachable.

##### W4A.2 — Reflection ordering verification and retirement

Status: complete on 2026-09-13.

Force reservation-before-activation, completion-before-subscription,
activation races, returned lazy values, acknowledgement, cancellation, and
structured failure. Remove reflection from source-time compatibility
evaluation only after those orderings are latched.

The focused source-owner fixture forces source recognition, reservation,
activation handoff, wait publication, completion, and result delegation onto
separate polls, collecting between every returned boundary. Existing latched
fixtures continue to cover first-observer/first-activator races, cancellation
before and during activation, structured launcher/task failures,
acknowledgement, cross-session completion, returned lazy values, and external
owner retirement.

#### W4B — Host-call sources

Status: complete on 2026-09-13 through W4B.0-W4B.2.

Preserve `HostCallRootBundle` as the callback handoff. Package source progress,
close managed access, invoke exactly once, validate the returned runtime, and
feed the rooted result back into the same WHNF computation. A callback is
never replayed merely because its result is lazy or because evaluation yields.

Opaque host callbacks remain nonsuspendable Rust calls; making them
asynchronous is deferred work.

##### W4B.0 — Host-call ownership and callback inventory

Status: complete on 2026-09-13.

Reconcile `HostCallProducer`, `HostCallRootBundle`, external-owner retirement,
runtime validation, and callback failure. Identify the existing exactly-once
state rather than adding a parallel callback lifecycle.

The managed lazy remains the durable owner of the traced source and any
declared semantic captures. `HostCallProducer` retains the scalar external
owner handle and source-backed capture policy; immediately before invocation
it converts the declared captures to `HostCallRootBundle`. The callback returns
one same-runtime `RuntimeValueRoot`, which is the only semantic result retained
across the after-call boundary. External-owner retirement remains tied to the
managed source rather than to a second callback registry.

##### W4B.1 — Explicit host-call source owner

Status: complete on 2026-09-13.

Give the lazy source a typed before-call/after-call state. Package and root the
handoff, close managed access, invoke once, validate provenance, and install
the rooted result into ordinary WHNF work before demanding it.

`HostCallSourceMachine` now has explicit `Before`, `Invoking`, `After`, and
`Consumed` states. It moves irreversibly to `Invoking` before calling Rust code
outside evaluator access, stores the rooted result, yields, and consumes that
result inside a later evaluator region. Runtime provenance is validated before
projection, after which the same lazy task continues as ordinary WHNF work.
Neither a lazy return nor a failure can replay the callback.

##### W4B.2 — Host-call ordering verification and retirement

Status: complete on 2026-09-13.

Force yield immediately before and after callback invocation, a lazy returned
value, callback failure, runtime mismatch, cancellation, and external-owner
retirement. Prove one callback invocation under every forced schedule before
removing the host source compatibility mode.

Focused fixtures force a scheduling boundary immediately before and after the
callback, collect at each handoff, and prove one invocation for success,
failure, repeated terminal observation, and a lazy returned value. Existing
fixtures retain coverage for callback/access separation, contention,
same-runtime validation, and external-owner retirement. The host source was
already excluded from `produce_lazy_source_in`; its former single-state outer
mode is replaced by the typed lifecycle above.

#### W4C — Net construction and net computation

Host the existing `NetConstructionMachine`, `NormalizationRequest`, and
`NetDriverWorklist` rather than translating their internal state into WHNF
frames. Bridge their terminal value/failure/dependency back into the enclosing
WHNF computation.

Preserve cursor-WHNF's local claim containment, disturbance wait, fallback
restoration, and no-materialization observation rules. A blocked core operator
retains both the net-owned call state and the evaluator checkpoint needed to
finish that operator.

##### W4C.1 — Reusable pollable net normalization owner

Status: complete on 2026-09-13 through W4C.1a-W4C.1c; pulled before W3B.3 by
the W3 dependency review.

###### W4C.1a — Persistent driver polling

Status: complete on 2026-09-13.

Refactor `NetDriver` so its request root, worklist, and progress disposition
survive a returned yield, semantic dependency, or contention handoff. A
blocked active pair is requeued before the poll returns. Managed net access and
all call claims remain regional; the durable driver contains identities and
roots only.

`drive_net_driver_work_in` now advances an existing driver rather than
constructing one implicitly. A semantic dependency requeues its exact active
pair before returning, after the claim and normalization scope have closed.
The former one-shot entry remains a compatibility wrapper for callers not yet
migrated to the reusable owner.

###### W4C.1b — Driver ordering verification

Status: complete on 2026-09-13.

Force progress/yield, blocked-call/resume, source-frontier traversal, and
contention handoff orderings. Assert that no poll retains a normalization
scope or active claim and that resumption does not restart from a different
root or duplicate a completed semantic call.

The existing forced frontier and normalization-batch fixtures are joined by
persistent-driver schedules for both semantic parking and pairless-cursor
contention. Contended cursor/active-pair work is requeued before handoff; the
same driver observes the forced publication afterward. The semantic fixture
publishes `Blocked` first, retains the exact request root and active pair, and
resumes only after the exact wait is completed. Both assert that no claim or
normalization scope crosses the returned boundary. W3 closeout added the
missing equivalent for contention while *admitting* a normalization batch:
the uninspected work item is restored before returning the contention token,
and a forced leader/follower schedule resumes that same persistent driver.

###### W4C.1c — Net-WHNF source owner

Status: complete on 2026-09-13.

Introduce one owner around a managed runtime net, exposed interface,
operation label, and persistent driver. It returns data, structured bind or
normal-form failure, an exact semantic dependency, or a cooperative yield.
W3B.3 and W4C.2 connect `FunctionCall` and `NetComputation` to this owner; net
construction keeps its existing effect machine and hands its terminal net to
the same owner only where WHNF extraction is required.

`NetWhnfMachine` now owns the exact request root, exposed interface, operation
label, and persistent driver. Each poll returns one data payload, a
cooperative yield after progress or contention handoff, or the existing
structured/dependency failure. Extracting data deliberately does not force a
lazy payload: the enclosing WHNF computation retains responsibility for that
ordinary outer-shell demand.

##### W4C.2 — Remaining net source integration

Status: complete on 2026-09-13 after W3B.3.

Move `NetComputation` to the shared owner and reconcile
`NetConstructionMachine` handoff behavior. Remove direct source-time calls to
`extract_net_data` and `evaluate_function_call` once their final callers are
gone.

`NetComputation` now selects `NetWhnfMachine` once with the exact managed net
and exposed interface. Progress, contention handoff, and semantic suspension
resume in that owner; terminal failures retain `eval:{op:'net_computation}`.
The direct `extract_net_data` compatibility entry is removed. Net construction
continues to own its effect interpreter and does not invent a WHNF extraction
when its terminal result is already the lazy result.

#### W4D — Boundary verification

Status: complete on 2026-09-13 through W4D.1-W4D.2.

Force callback-before/after-yield, reflection activation races, net operator
dependency, and net-construction suspension. Verify exactly-once callback and
reservation counts, stable net work identities, and collection between every
handoff.

Mandatory post-W4 review: verify that the pure submachine has not absorbed
effect-handler or interaction-net lifecycle policy.

##### W4D.1 — Combined external-boundary closure

Status: complete on 2026-09-13.

Run the W4A/W4B forced-order matrices beside the completed W4C driver matrix.
Re-run the source census and exact-root inventories, and prove that
`produce_lazy_source_in` retains only the declared W6 builtin compatibility
family plus test-only fixtures.

The combined host/reflection/net matrices pass with collection between every
new W4 handoff. The exact WHNF census records removal of one recursive
application call and one retryable-halt construction from the reflection
path. Durable-owner, runtime-root, persistent-edge, raw-value, and evaluator
access inventories are reconciled. Production execution through
`produce_lazy_source_in` is now limited to saturated builtin compatibility;
`Error` is an invariant check, all migrated families are unreachable arms,
and the only other executable arms are the two `cfg(test)` fixtures.

##### W4D.2 — Post-W4 implementation and drift review

Status: complete on 2026-09-13.

Audit callback/reservation ownership, interaction-net lifecycle containment,
scheduler handoffs, future W5-W8 assumptions, and verification cost. Record
intentional phase-order drift and every remaining compatibility boundary
before beginning reflection-machine integration.

See
[`ResumableWhnfW4_2026-09-13.md`](../reviews/ResumableWhnfW4_2026-09-13.md).
The review initially found no blocking correctness defect or unresolved
semantic decision. Subsequent whole-program timing exposed the blocking
performance defect assigned to W4E below. W5 still owns
reflection-effect-machine integration; W4 moved only the lazy reflection
source boundary. W6 retains the sole production source compatibility family
and the temporary per-session admission policy. W7-W8 remain correctly
ordered after those migrations, subject to the W4E reconciliation gate.

#### W4E — Direct-assembly net-WHNF performance remediation

**Priority:** blocking before W5.

**Status:** pending.

The source-shaped duplicate-symbol executable fixture completed in about 8.1
to 8.5 seconds at every sampled revision from the pre-W0 baseline through
`7fed99e`. Its immediate successor, `e496248` (W4C.1c and W3B.3), did not
complete within 45 seconds; the completed W3 revision did not complete within
75 seconds, and current head did not complete within the isolated limits used
during investigation. The four affected executable fixtures remained
CPU-bound together after more than twelve minutes.

The first-bad transition moves function-call normalization from the direct
evaluator path into ordinary scheduled `NetWhnfMachine` dependencies. Initial
instrumentation showed continued growth in function-call owners, net
reductions, and driver work after the old path had already terminated. It also
exposed two potentially superlinear scheduler paths:

- `prioritized_task_for` walks the complete producer/dependency chain and
  constructs a `Vec` and `HashSet` on each selection; and
- `claim_ready_task` may call `session_has_running_machine` while scanning
  ready candidates, and that query scans the session's complete work set.

The observation is not yet sufficient to classify the defect as pure
scheduler amplification, replayed semantic work, or both. In particular, the
increased net-reduction count must not be dismissed as constant-factor
scheduling overhead. W4E determines that distinction before selecting a
repair.

##### W4E.0 — Switchable interaction-net accounting

**Status:** complete on 2026-09-13.

Build a reusable reduction-accounting tool before adding fixture-specific
counters. For a fixed closed net, demanded interface, and sequence of external
semantic results, reaching the same result must have the same committed
interaction-net reduction signature regardless of reduction order. Use that
property as the primary replay oracle.

Define a stable `NetReductionCounts`-like snapshot with one count for each
committed rule family currently represented by `ReductionKind`:

- bind/bind join;
- same-identity fan join and different-identity fan commute;
- fan/data, fan/bind, and fan/operator;
- erase;
- bind/data call;
- operator/data call; and
- committed remote-cursor materialization or join.

Keep terminal stuck-pair observations and remote-cursor blocking separate
from successful reductions. A recognized `Call`, `OperatorCall`, or
`RemoteCursor` claim is not sufficient evidence of a committed reduction: it
may block, be released, or be retried. Increment the semantic counters only at
the authoritative runtime-net transition which makes the rewrite durable.

Maintain a second `NetDriverCounts`-like snapshot for order-sensitive
orchestration activity, including interface polls, cursor steps and
dependencies, claim attempts, blocked retries, contention, disturbance,
request-root restarts, and driver work items. These counts explain scheduling
cost but are not expected to be reduction-order invariant. Do not combine them
with the semantic reduction signature.

The accounting sink is shared by all core nets in one `EvaluationRuntime` so
function-stage copies and nested nets contribute to one whole-evaluation
snapshot. Enable the tool statically with a dedicated Cargo feature such as
`interaction-net-profiling`, analogous to compiling a profiling build. In
that build every runtime carries counters; this is not a dynamic
`EvaluationRuntime` profile option and cannot be configured from evaluated
Glam code.

Compile the fields and update hooks out entirely when the feature is absent:
the ordinary reduction path has no observer pointer, absent-sink branch,
atomic operation, allocation, formatting, callback, or value inspection. In a
profiling build, fixed atomic counters are acceptable for the initial tool.
Do not attach an opaque callback to every reduction or force data carried by a
`Data` node merely to classify its rule.

Expose the typed snapshots through a profiling-only Rust API. The bootstrap
binary built with the feature may render one compact summary after the runtime
becomes stable; no additional runtime switch is necessary. Tests inspect the
typed snapshot directly rather than parsing that text. Keep presentation at
the binary boundary so the interaction-net and evaluator layers contain only
structured counts.

Verification for the tool itself must establish:

- exact per-rule counts on one small fixture for every rule family;
- calls and cursors count only at commit, not recognition, block, release, or
  retry;
- two deterministically forced valid reduction orders produce equal semantic
  signatures but may produce different driver signatures;
- counts aggregate across nested/copied core nets in one runtime and remain
  isolated between runtimes; and
- ordinary builds contain no accounting state or calls, while profiling
  builds leave the result, structured failures, and scheduling decisions
  unchanged.

Implemented as the static `interaction-net-profiling` Cargo feature. The
runtime-owned snapshot separates committed rule-family counts from cursor-WHNF
driver activity. Core-net facade commit points count ordinary rewrites,
completed calls, completed operator calls, and completed cursor transitions;
claim, release, block, retry, and stuck observations remain uncounted. Focused
tests latch complete rule classification, call/operator commit timing,
runtime isolation, and two forced ready-pair orders. W4E.1 owns the
source-shaped comparison and its larger driver/scheduler measurement surface.

For the W4E comparison, apply the same accounting patch to `7fed99e` and
current head. Capture the last-known-good exact signature for the successful
and duplicate-symbol fixtures. If current head exceeds any completed baseline
rule count before reaching the same public-operation prefix, the computation
has performed duplicated semantic work; there is no need to wait for it to
terminate. An intentional future topology or lowering change may update a
latched signature only with an explicit explanation of the changed rules.

##### W4E.1 — Deterministic reproducer and measurement surface

**Status:** complete on 2026-09-14.

First reproduce the mismatch with a source-shaped in-process fixture which
uses the same `direct_assembly.g` configuration and duplicate-symbol program
as `direct_assembly_rejects_duplicate_symbol_publication`. Do not replace it
with a small synthetic net unless that net independently exhibits the same
growth signature. Add zero-publication, one-publication, and duplicate-
publication prefixes so the last completed public assembly operation locates
where growth begins.

Add test-only, read-only counters at the authoritative owners for:

- unique deferred producers and unique `FunctionCall` source selections;
- lazy-task polls and their yielded, blocked, resumed, and terminal
  dispositions;
- calls to `prioritized_task_for`, dependency edges visited, and maximum chain
  depth;
- ready-queue candidates examined and session-work records visited by running
  admission checks;
- the W4E.0 semantic reduction and driver-accounting snapshots;
- committed public direct-assembly operation dispatches in exact order; and
- terminal lazy caches and the final duplicate-publication diagnostic.

Counters must not use semantic values as identities, force lazy operands, or
alter queue selection. Scope them to an explicit test probe or fixture-owned
observer so ordinary builds pay nothing. W4E.0 counters exist only in an
`interaction-net-profiling` build. Record stable producer, task, and net
identities only where the runtime already exposes such an identity.

Drive the fixture with a deterministic limit on scheduler polls and net work,
not a wall-clock timeout. A scheduler-only limit is insufficient: the current
driver may perform an unbounded amount of work before returning one poll.
Enforce the net-work limit immediately after a normalization batch or semantic
step has closed all managed scopes and claims, and report exhaustion through a
test-only out-of-band probe result rather than a Glam evaluation failure. The
probe must leave the runtime safely droppable and must not publish a terminal
lazy cache for the interrupted computation.

Before repairing the implementation, latch a test which reaches that limit
and reports the counter snapshot plus the last committed public assembly
operation. The limit should be comfortably above the work performed by the
last-known-good revision while still failing in seconds under the regression.
Preserve that failing snapshot in the checkpoint record, then change the
assertion to the intended bounded completion result as part of the repair.

Run the exact executable test serially against `7fed99e`, `e496248`, and
current head with compilation excluded. Record wall time and CPU time as
corroborating benchmark evidence only; elapsed time is not a correctness gate
and repeated success is not evidence about scheduling order.

The source-shaped failure reduced to a deterministic local expression: a
one-argument wrapper returns another one-argument function and the caller
supplies both arguments in one application spine. At `7fed99e` the fixture
terminates with 71 driver work items and the exact semantic signature now
latched by
`wrapper_returning_function_then_accepts_remaining_application`. Before the
repair, the same fixture alternated fresh wrapper and returned-function lazy
calls without completing. This established semantic replay rather than a
source-loader, module-fixpoint, or dependency-chain problem. The largest
observed `prioritized_task_for` dependency depth in the executable fixture was
four; that path was not material to the regression.

The repaired duplicate-symbol executable fixture completes in roughly 12.6
seconds on the same machine, versus roughly 8.1 to 8.5 seconds at `7fed99e`.
Its driver work is comparable (159,322 versus 159,994 work items), while its
semantic signature contains the small intentional topology increase described
by W4E.2. The remaining cost is therefore scheduler/machine overhead rather
than continuing interaction-net work.

Exit: one bounded fixture distinguishes at least these cases:

1. a source or completed semantic prefix is selected more than once;
2. logical function-call and net work remains comparable but scheduler visits
   grow superlinearly; or
3. new net work continues without advancing the public assembly-operation
   prefix; or
4. both semantic work and scheduler work grow.

##### W4E.2 — Semantic replay repair

**Status:** complete on 2026-09-14.

If W4E.1 finds replay, identify the first duplicated stable identity or
completed prefix and add the smallest forced-order fixture at that boundary.
Repair ownership or resumption there. A `FunctionCall` source is selected
once, stage attachment occurs once, one successful active-pair transition is
not reconstructed as fresh work, and a completed lazy cache is never replaced
by a new producer. Preserve the persistent `NetWhnfMachine` and exact blocked
active-pair restoration guarantees; do not restore user-controlled recursion
to the Rust stack as a performance workaround.

If semantic counts remain bounded, mark this checkpoint not applicable with
the W4E.1 evidence rather than manufacturing a semantic change.

`CoreOperator::ApplyArity` previously executed a saturated application while
the operator active pair was claimed. Over-application could force an
intermediate lazy function and yield from the nested WHNF pump; restoring the
operator pair then discarded that intermediate application state and replayed
the same call from its beginning. The operator now commits one ordinary lazy
application. Its `WhnfComputation` durably owns the argument cursor and
resumption state, so yielding does not reconstruct completed work.

This deliberately exposes the application as ordinary net work. Compared with
the pre-W4 baseline, the full executable therefore has a small, bounded change
in call/operator/cursor topology even though its total driver work remains
comparable. The minimized fixture's exact semantic signature is the regression
contract; changing it requires an explicit lowering/topology explanation.

The new application lazy also establishes the correct diagnostic boundary.
The surrounding net computation has completed successfully once it exposes
that lazy; a later application failure therefore does not inherit an
`eval:{op:'net_computation}` frame. Existing reflection fixtures which had
latched the former synchronous implementation boundary now expect only the
explicit task or log-demand contexts. Net normalization failures themselves
continue to receive the net-computation frame.

##### W4E.3 — Scheduler amplification repair

**Status:** determined not applicable on 2026-09-14.

Remove the measured superlinear selection behavior without weakening exact
demand, cycle detection, or the temporary one-ordinary-machine-per-session
rule by accident. Candidate changes include an authoritative O(1)
per-session running-machine count and explicit propagation/indexing of the
next claimable producer instead of rescanning a complete dependency chain.
Choose from evidence; do not optimize both paths merely because both are
visible in source.

If the correct repair is to retire the temporary per-session admission rule,
pull forward only the minimum W6 proof required first: nested same-session
pure work must begin after the caller's managed region and net claim have
closed, and the caller must remain a durable resumable owner. Record the
resulting W6 scope reduction explicitly.

Make `NetWhnfMachine` consume a declared portion of the task's step budget
only if W4E.1 shows that its existing cooperative-yield boundary contributes
materially. A temporary experiment batching 64 progress outcomes did not
resolve the regression, so a larger quantum alone is not an accepted repair.

The first proposed repair—an authoritative O(1) per-session running-machine
count—was implemented and measured, then reverted. The duplicate-symbol
fixture remained at roughly 13 seconds. Retired work is removed from
`work_by_session`, so the existing admission query does not scan the thousands
of historical lazy tasks created over the whole assembly; the premise for the
index was wrong for this workload. Do not reintroduce that index without new
counter or profile evidence.

The severe unbounded behavior is resolved by W4E.2. The residual regression is
a bounded constant-factor cost: approximately the same net-driver work now
passes through several thousand scheduled `NetWhnfMachine` polls. This is not
the superlinear scheduler amplification W4E.3 was intended to repair. W4E
therefore accepts the transitional cost; W6 owns its explicit measurement and
investigation while removing the compatibility/admission boundary.

##### W4E.4 — Verification and plan reconciliation

**Status:** complete on 2026-09-14.

The provisional repair matrix asked a source-shaped fixture to
deterministically:

- terminate within the latched poll and net-work budgets;
- reproduce the last-known-good per-rule reduction signature;
- select each function-call source and publish each terminal cache once;
- emit exactly one duplicate-symbol diagnostic with unchanged structured
  context;
- leave no claimed net call, normalization scope, or running task behind; and
- keep dependency-edge and ready-candidate visits within the fixture's stated
  linear or otherwise justified bound.

Add a successful direct-assembly fixture to the same bounded harness so the
repair cannot specialize failure. Exercise zero workers and one worker with
forced scheduling barriers where ordering matters. Re-run the W3B.3 and W4C
identity/restoration suites, followed by the four affected executable tests
serially. Only after those pass, run the ordinary full suite.

Run the normal Rust gates for the completed repair:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
scripts/check-interaction-net-profiling.sh
```

Update W6's per-session-policy checkpoint, W7C's quantitative work
verification, and W8A's compatibility retirement description with whichever
responsibility W4E actually removes. Record a new dated review finding rather
than silently rewriting the original W4 outcome.

Completed verification:

- `wrapper_returning_function_then_accepts_remaining_application` terminates
  through a bounded client-demand harness and latches 17 coordinator polls,
  the last-known-good per-rule reduction signature, and the complete
  net-driver signature including exactly 71 work items and six dependency
  visits under `interaction-net-profiling`;
- a test-only static profiling fuse stops that same computation after exactly
  16 work items. The inverse fixture proves the demand remains pending, the
  original source remains installed, no terminal cache has been published,
  and explicit abandonment retires the one client-demand record;
- both the interrupted and completed schedules retain no in-flight net claim
  or normalization lease. Completion removes the source, publishes the
  terminal cache, and retires the client-demand record;
- the source-shaped duplicate-symbol executable terminates, fails once with
  the expected diagnostic, and completed in 12.88 seconds in the final run;
- the affected diagnostic-context fixtures, W3B.3/W4C coverage, and all four
  executable fixtures pass through the ordinary full suite;
- the raw-value, evaluator-access, mutator-introduction, and WHNF inventories
  were reconciled and pass;
- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D
  warnings`, `cargo test -q`, and the focused
  `scripts/check-interaction-net-profiling.sh` gate pass;
- W6G owns the measured bounded overhead, W7C distinguishes its production
  task budget from W4E's test-only fuse, and W8A preserves the durable
  application boundary while retiring recursive-halt compatibility; and
- the dated `ResumableWhnfW4E_2026-09-14.md` review resolves the original W4
  blocker and records the remaining performance debt without rewriting the
  pre-remediation W4 review.

The broader provisional matrix above was narrowed after the W4E.1-W4E.3
evidence. Exact reduction and driver signatures detect semantic replay without
adding an intrusive per-source identity journal; lazy-cell synchronization
already makes terminal publication single-assignment. The fixture now observes
the stable source net directly for the lifecycle assertions that are not
encoded in those signatures.

Ready-candidate accounting belonged to the disproven W4E.3 scheduler-index
hypothesis. A second source-level success harness would duplicate the minimized
successful semantic case without isolating the repaired boundary. Forced
zero/one-worker schedules are likewise not appropriate here: W4E.2 repaired a
deterministic single-owner replay rather than a disputed cross-thread ordering.
The existing worker, restoration, and executable suites remain the coverage
for those independent policies. This checkpoint therefore does not add those
three mechanisms merely to satisfy the earlier speculative matrix.

### Phase W5 — Reflection Machine Integration

**Entry gate:** W4E is complete, the bounded semantic replay fixtures pass,
and the source-shaped direct-assembly diagnostic fixture terminates with its
expected result.

**Status: complete (2026-09-15).**

#### W5A — WHNF submachine work state

**Status: complete (2026-09-14).**

Add a reflection work variant or dedicated decoding substate which owns
`WhnfComputation` plus one explicit completion purpose. It must compose with
branch cloning, cuts, retries, exits, cancellation, and transaction scopes
without cloning a live WHNF computation into multiple committed owners.

If an `.alt` branch needs independent evaluation, it receives an independently
rooted checkpoint by the existing branch construction policy; do not make
`WhnfComputation: Clone` as a shortcut.

Completion record: `TaskExecution` now has one dedicated, non-cloneable
`EffectDecodeWork` slot containing the exact `WhnfComputation`, its explicit
effect-object completion purpose, branch, and scope depth. Retry discovery
reads whichever of ordinary work or decoder work is active; cut/search restart,
exit disposal, cancellation, and terminalization all retire decoder state
before installing replacement work. Blocking now
retains the exact `WorkDependency`, including a promise completion source,
while the synchronous and isolated-search compatibility surfaces adapt promise
dependencies to their existing wait-token APIs. Compile-exhaustive inventories
and the private-WHNF owner inventory latch the new durable owner without making
the computation cloneable. W5B activates this owner for effect decoding.

#### W5B — Effect request decoding phases

**Status: complete (2026-09-14).**

Split effect-object, function, application-result, and request parsing into
explicit reflection phases. Preserve the existing structured
`effect_dispatch` contexts for `function`, `application`, and `request`.

Applying `eff` may produce a lazy, but the application occurs once. Request
parsing begins only after that same lazy produces WHNF.

Completion record: reflection effect dispatch now advances one non-cloneable
decoder through explicit `EffectObject`, `Function`, and `ApplicationResult`
WHNF purposes. The successful function phase applies `eff` exactly once,
roots that exact application result, and transfers it to the request-purpose
computation; only its completed WHNF is parsed and handed to rooted
`MachineWork::Interpret`. A suspension therefore retains the original
application lazy instead of reconstructing it. Structured failures retain the
`effect_dispatch` function, application, and request frames, with dedicated
tests for all three boundaries.

The scheduled-machine surface publishes exact lazy or promise dependencies to
the coordinator. The older direct-poll and isolated-search conveniences pump
locally runnable wait tokens without hiding a genuinely unavailable
dependency. Standard-request fusion now begins from the rooted parsed request
and retains result equivalence. W5C.1 gives fused and generic dispatch the same
single rooted request-argument handoff, eliminating the older redundant
unfused handoff rather than retaining a misleading relative-root probe. The
deferred
[Pure Effect Access Fusion plan](PureEffectAccessFusion_2026-09-23.md) owns
any later elimination of per-step argument roots across one uninterrupted
regional effect quantum, after Value Representation Refinement.

The forced-suspension regression now asserts that the first application-lazy
identity occurs exactly once across resumption. A cancellation fixture now
uses a deliberately pending child rather than racing cancellation against an
already-returning child; W5's additional cooperative boundaries correctly do
not promise which runnable task wins that unrelated race.

#### W5C — Remaining reflection demand sites

W5C is partitioned because the remaining recursive calls do not share one
completion disposition. Request decoding resumes into dispatch, continuation
demands resume into control delivery, paths resume before transactional host
work, reset stacks retain a collection walk, and specialization callbacks
cannot preserve an arbitrary Rust call stack across suspension. Do not hide
those differences behind one increasingly general reflection-purpose enum.

##### W5C.0 — Remaining-demand census and work vocabulary

**Status: complete (2026-09-14).**

Latch every production reflection demand which can reach lazy, promise, net,
or reflection work. The W5B baseline includes the remaining local
`machine::evaluate_in` calls, the direct loops in `RequestContext`, and helper
families such as request-list, key-path, value-path, and reset-stack traversal
which can conceal more than one child demand.

For each occurrence, record its durable owner, exact result disposition,
whether any semantic or host action precedes the demand, and the smallest
resumable work form which can represent its completed prefix. Select separate
completion purposes for the families below and extend the compile-exhaustive
machine inventory before migrating behavior.

Completion record: the exact W0B source fingerprint already latches the
production call sites, while this checkpoint assigns their reflection
completion roles. `reflection::machine` retains fourteen calls to its local
recursive `evaluate_in`: two in request payload/ID decoding, six in
continuation or terminal-value delivery, four below key/path/state helpers,
and two in reset-stack traversal. Eight direct key-path evaluations and the
request-list and reset-list projections add composite demand beneath those
entry points. `RequestContext` separately retains its recursive WHNF and
value-path loops plus key-path delegation; those run inside arbitrary
`TaskSpecialization::handle_request` callbacks and therefore belong to W5C.5,
not to the machine-local conversions.

The selected state families are request decoding, scalar WHNF completion,
path/access work, reset-stack collection work, and specialization-request
preparation. All are owned by the reflection task record but have distinct
result dispositions. W5C may share the common `WhnfComputation`, key
conversion, access, and list-front machinery; it must not merge the durable
reflection continuations merely because those child evaluators share a poll
type. Host snapshots, observations, edits, commits, and callbacks remain after
the corresponding demand owner completes.

##### W5C.1 — Request payload and identity decoding

**Status: complete (2026-09-14).**

Make request payload-list WHNF, list-spine extraction, and `.resume` task and
continuation ID conversion explicit resumable decode phases. Select the
request tag before suspending when possible, retain the exact selected payload,
and do not rescan or reconstruct an already completed request prefix merely
because a later payload or ID blocks.

Preserve malformed request, unknown tag, wrong arity, non-list payload, and
invalid ID diagnostics. Force suspension independently at payload WHNF, list
extraction, task ID, and continuation ID, and assert one application result,
one selected request, and no duplicate nested reflection activation.

Completion record: `RequestDecodeWork` now owns the selected request, exact
payload computation, resumable logical-list front, completed rooted arguments,
and the two ordered `.resume` ID computations. Local decoder transitions use a
non-scheduling `Continue` result; only an exhausted child WHNF/list computation
yields the reflection machine. This distinction preserves the existing task
quantum instead of spending one scheduler claim per request argument.

Forced-order fixtures independently suspend a lazy payload, a lazy list chunk,
and both `.resume` IDs. Each source is evaluated exactly once across resumption,
and the broad reflection-machine suite covers the existing malformed request,
arity, non-list, unknown-tag, and invalid-ID diagnostics. The request handoff
now roots each argument once for both generic and fused interpretation; the
deferred
[Pure Effect Access Fusion plan](PureEffectAccessFusion_2026-09-23.md) owns any
later regional elimination of those per-step roots after Value Representation
Refinement.

##### W5C.2 — Continuation and terminal-value demands

###### W5C.2a — Glam continuations and exit errors

**Status: complete (2026-09-14).**

Replace recursive function WHNF in `Continuation::Glam` with owned WHNF work
which resumes into one continuation application. Treat `.exit.error` message
WHNF as a separate terminal disposition. Neither path may pop its continuation
or publish its exit intent until the demanded value is ready.

Completion record: `ScalarDemandWork` now retains an exact WHNF computation,
branch, scope, and completion disposition beside effect decoding. Both fused
and generic Glam delivery enter that owner before inspecting the continuation
function; successful completion alone pops the continuation, and fused
application failure retains the completed function plus the original control
stack. `.exit.error` similarly publishes no exit intent until its message WHNF
completes. Forced reflection-gate fixtures cover both continuation paths and
the exit-message path, asserting one nested activation across resumption.

The parallel gate exposed an older scheduler race at the newly more frequent
cooperative boundary: a yielded record becomes claimable before its prior
release finishes publishing advisory lifecycle status, so the later quantum
may terminally retire the record first. Nonterminal release tails now observe
the latest live record when one remains and otherwise defer to the terminal
publication. Coordinator-assigned status order prevents an older callback
from overwriting a later terminal status. A channel-latched regression forces
the second claim through terminal retirement before allowing the first release
tail to continue; repetition is not used as concurrency evidence.

###### W5C.2b — Unit assertions and scoped close

**Status: complete (2026-09-14).**

Migrate `RequireUnit`, `AssertUnit`, and `RestoreScopedValue`. Construct an
assertion computation once, preserve its diagnostic context across
suspension, and mutate the control stack only after success. Verify success,
structured permanent failure, lazy and promise suspension, cancellation, and
scope-close restoration without replay.

Completion record: the shared scalar owner now distinguishes generic unit
checking, provider-contextual `AssertUnit`, and scoped-close restoration.
`AssertUnit` constructs its assertion lazy once before handing it to WHNF
work; all three paths retain their control entry until successful completion.
A test-only scoped request exercises close activity followed by a suspended
unit result, proving that the close is committed once and the operation's
saved result is restored only afterward. Separate counted-gate fixtures cover
both generic and contextual endpoint policies.

##### W5C.3 — Keys, paths, and state operations

###### W5C.3a — Reusable key-path and value-path work

**Status: complete (2026-09-14).**

Introduce or reuse resumable work for key conversion, path-list traversal,
and intermediate dictionary WHNF. Preserve the distinction between an absent
member, which produces the language's undefined value, and a non-dictionary
intermediate, which is an evaluation failure. The final selected value remains
lazy unless the caller explicitly demands it.

Completion record: the evaluator's existing resumable key and path-list
conversion now accepts either a containing lazy owner or an explicitly
unowned reflection computation. `ValuePathMachine` retains the current rooted
member, exact next key, and intermediate WHNF computation. It demands only
values which must be dictionaries to continue traversal; an empty path and
the final selected member are returned without demand. Missing members still
select `{}`, while a demanded non-dictionary intermediate retains the existing
structured failure.

###### W5C.3b — Task-local `.get` and `.set`

**Status: complete (2026-09-14).**

Move task-local state lookup, update, and dictionary validation onto the path
work. Retain the original branch and state until the complete replacement
state is ready, then publish it once. Cover empty paths, missing members,
lazy intermediates, lazy keys, and a suspending dictionary update.

Completion record: fused and generic task-local requests now transfer their
branch into `StatePathWork`. Key conversion completes before either operation,
and `.set` retains the prior branch state until its replacement dictionary or
lazy `dict_update` result reaches WHNF. Empty-path replacement retains its
dictionary requirement; nonempty lookup preserves lazy final members. Counted
reflection-gate fixtures suspend the path, an intermediate dictionary, and an
empty-path replacement in both dispatch modes, proving one activation per
source. Separate semantic coverage latches empty paths, absent members, and
non-dictionary intermediates. The exact WHNF and durable-owner inventories
were updated at this checkpoint.

###### W5C.3c — Heap and volume transaction boundaries

**Status: complete (2026-09-14).**

Use the same resumable path work for `.heap.*` and volume operations. Complete
all path demand before observing a host snapshot, recording a transaction
read, appending a journal edit, or attempting a commit. Forced suspension must
prove that snapshots, observations, commits, and returned lazy path values are
not duplicated. Host operations remain outside managed access.

Implementation has reached the store-operation boundary and its direct
ordering fixtures pass. Full-suite validation exposed a pre-existing retry
interaction that must be resolved before this checkpoint can close: an exact
dependency reached after a heap read currently races the read's coarse host
generation retry. Unrelated task commits can therefore discard the resumable
continuation and replay the read indefinitely.

**Retry-semantics decision (2026-09-14).** A broad host generation is a wake
signal, not evidence that an optimistic transaction conflicted. The store's
configured exact, fingerprint, or coarse observation index remains the
authority for reflection-heap conflicts; specialization-owned resources use
their corresponding transaction validation. A broad wake may schedule a
blocked machine to revalidate those records, but must not itself discard its
retained work.

A successful read outside an explicit transaction has different semantics.
Its atomic snapshot is its commit point: `.heap.get` and protected-volume
`get` return a lazy projection of that stable snapshot, and no later update,
including an overlapping update, may replay the read or its continuation.
These standalone reads therefore record no retry checkpoint.

**Cut-wide observation ownership (2026-09-14).** The optimistic read set
belongs to the enclosing `CutFrame`, not to an individual `.alt` branch. It
accumulates every external read performed during the current cut attempt up
to suspension or commit, including reads in alternatives which have already
failed and reads in the currently active alternative. A later branch can win
only because every earlier branch failed, so those earlier observations are
part of the winning result's serializability proof. Entering or advancing an
`.alt` must therefore never reset or replace the cut's observation set.

Branch-local speculative edits, input cursors, buffered outputs, local state,
and continuation state still rewind when an alternative fails; the existing
rollback behavior is intentional and independently tested. The current code
clones the whole `Transaction` when it forks a branch, which accidentally
clones the exact observation index along with the speculative edit state.
Before exact conflict analysis this defect was masked by the cut-wide
`observed_failure` bit and coarse generation retry. W5C.3c must separate the
cut-wide conflict evidence from branch-local speculative state. Introducing
fine-grained read-set checkpoints at individual `.alt` boundaries would need
an explicit alternative-frame design and is deferred; it is not part of this
repair.

Retryable absence is a common instance of transactional divergence, not a
separate conflict regime. Users can express this via a pattern such as
`(.cut (.alt HappyPath DivergeOnFailure))`, using errors for the divergence.
Some system-provided APIs, such as reading from a queue, may use this
construct implicitly to enable useful optimizations such as more-precise
read-write conflict analysis for queues. This must be clearly documented in
the API specification.

Repairs required before closing this checkpoint:

1. Remove `branch.observe` from the non-transactional heap and volume read
   paths. Retain their snapshotted roots through the lazy path projection.
2. When an exact dependency is suspended inside an explicit optimistic
   transaction, use a broad generation change only to request read-only
   revalidation of the retained store and specialization observations. A real
   conflict restores the appropriate cut/search checkpoint. A still-current
   transaction updates its wake baseline and re-registers the unchanged exact
   dependency.
3. Preserve retryable divergence as the composition of an optimistic
   observation and divergence which does not advance `.alt`. A specialized
   API may install the same observation and divergence state directly, but a
   successful host read or returned lazy value does not imply it by itself.
4. Perform revalidation outside managed value access and through the same
   authoritative host state and conflict rules used by commit. Preserve the
   existing observation-epoch subscribe-and-recheck protocol so a mutation
   racing validation or re-registration cannot be lost.

Implementation checkpoints:

- **W5C.3c.1 — Complete (2026-09-14): resumable store boundary and committed
  standalone reads.** Heap and volume get/set/rewrite operations now begin
  only after resumable key conversion finishes. Standalone reads retain their
  atomic snapshot without a retry checkpoint. Forced lazy suspension followed
  by both disjoint and overlapping publication proves that heap and volume
  reads return the original value without replay; the child-task regression
  which exposed the coarse wake loop now completes.
- **W5C.3c.2 — Complete (2026-09-14): cut-wide observations and precise
  validation of suspended transactions.** First latch and repair the loss of exact read
  observations across `.alt` branches while preserving branch-local rollback.
  Then add the read-only host validation boundary and make a broad wake
  restart only a genuinely conflicting cut attempt.
  - **W5C.3c.2a — Complete (2026-09-14): reflection-store observation
    ownership.** `StoreJournal` clones now share one monotone conflict-index
    allocation while retaining independent persistent views and edit vectors.
    A regression first demonstrated that an overlapping publication could
    commit between a failed reader alternative and its winning sibling; it
    now conflicts, while the failed sibling's speculative edit remains absent
    from the winner.
  - **W5C.3c.2b — Complete (2026-09-14): specialization observations and
    read-only host validation.** `TaskSpecialization::begin_journal` now creates the one
    journal shared by an optimistic attempt before alternatives fork. Runtime
    FIFO journals share a monotone observation map while retaining branch-local
    cursors and output intents; the test specialization applies the same split
    to diagnostic reads. `TaskHost::validate` checks retained store and
    specialization evidence without applying edits. A retry capsule now keeps
    that transaction, and broad wakes validate before choosing restart or an
    updated wake baseline. Isolated macro, CLI, token, and net-construction
    searches have immutable snapshots and no mutable host-validation path.
- **W5C.3c.3 — Complete (2026-09-14): matrix and documentation closeout.**
  Exercise exact, fingerprint, coarse, specialization, retryable-divergence, and
  validation/re-registration orderings before marking W5C.3c complete.

The primary forced-order verification matrix is:

| Attempt state | Concurrent event | Required outcome |
| --- | --- | --- |
| Standalone heap or volume read has returned | Any later change, whether disjoint or overlapping | The original lazy snapshot and continuation complete once; no replay |
| Explicit transaction is suspended on an exact dependency | No publication | The dependency remains registered and resumes the retained continuation when completed |
| Explicit transaction is suspended on an exact dependency | Non-conflicting publication | A broad wake may revalidate, but the dependency and continuation are retained and re-registered |
| Explicit transaction is suspended on an exact dependency | Conflicting publication | Validation abandons the attempt and restores the transaction checkpoint |
| Explicit transaction has reached retryable divergence | No publication | The attempt remains divergent and does not advance `.alt` |
| Explicit transaction has reached retryable divergence | Non-conflicting publication | Revalidation retains the same divergence without replay or `.alt` advancement |
| Explicit transaction has reached retryable divergence | Conflicting publication | Validation restores the transaction checkpoint and retries the optimistic attempt |

Exercise those states under the configured validation policies:

| Policy | Required evidence |
| --- | --- |
| Exact reflection-store analysis | Disjoint paths and protected volumes do not conflict; exact, ancestor, and descendant overlaps do |
| Fingerprint reflection-store analysis | Every overlap conflicts; conservative collision retries remain permitted |
| Coarse reflection-store analysis | Any reflection-store write after an observed read conflicts |
| Specialization-owned analysis | The specialization's retained journal or cursor, rather than the broad wake generation, decides conflict |

Each row involving publication must force that publication after the read and
before either the exact dependency completes or retryable divergence is
reported. Counters must latch snapshot/read, continuation, validation, and
restart counts rather than relying on repeated scheduling. Also verify that a
mutation between successful validation and blocked-work registration is
observed by the existing epoch protocol. Cover retryable divergence both as an
unresolved exact dependency and as an error after an optimistic observation;
the latter must never select another `.alt` branch merely because it is an
error.

Completion record: forced-order machine fixtures distinguish validation from
snapshot/restart calls. Exact and fingerprint policies retain a suspended
dependency across a disjoint write, coarse policy restarts it, and exact
same-path, ancestor, and descendant writes restart precisely once. Separate
fixtures prove that an unrelated store write preserves a specialization-owned
empty-queue observation, a diagnostic append conflicts with it, and an
evaluation error after a transactional read remains on the same alternative
until a conflicting write restarts the cut. Runtime-event coverage proves
forked journals share empty-tail and prefix observations without sharing input
claims. Finally, a single-use validation hook publishes between successful
validation and blocked-work registration; the coordinator epoch recheck forces
a second validation, latching the formerly vulnerable ordering directly.

Standalone heap reads and the macro-drain/metadata fixtures were updated to
make retry scope explicit: ordinary reads commit their snapshot immediately,
while intended retry loops now contain the read and divergence in `.cut`.

##### W5C5-001 — Nested specialization demand deadlock

**Status: complete (2026-09-14). Execution was pulled ahead of W5C.4; the
remaining callback families stay assigned to W5C.5.**

The parallel library suite exposed a hang in
`coordinator_terminal_policy_preserves_a_descendant_failure_before_root_return`.
The zero-worker case remains reliable, while the four-worker case can park
indefinitely after a diagnostic is admitted and the scheduled logger root is
run. Parallel stress makes the ordering easier to reach, but repetition is
reproduction evidence only.

Temporary phase probes localized the hang to `ScheduledEffectRun::run`. A
debugger snapshot then showed the scheduled runner and three evaluator workers
parked on one coordinator generation while the remaining worker was inside
`TaskSpecialization::handle_request`: `read_test_log` was enriching the
diagnostic, `Diagnostic::enrich_with_factory` had entered a synchronous nested
WHNF client demand, and `drive_client_demand` was waiting for executor work.
Temporary coordinator-state inspection found a queued `ClientDemand` whose
demand session also contained a `Deferred` record in `Terminalizing`.
`claim_ready_client_demand` rejects that client demand while any machine in its
session is running or terminalizing. The runtime selector can initially churn
its broad work generation because an ineligible client demand remains in the
ready queue, then every participant can park without a useful transition.

This is provisionally a specialization-callback boundary defect, not a reason
to weaken the one-ordinary-machine-per-demand-session rule or make recursive
client demand an implicit worker behavior. The exact missing transition still
needs a forced-order proof: a normal terminal publication-to-retirement window
is expected to be finite, so the repair must distinguish an over-restrictive
eligibility rule from a stranded retirement or wake handoff. Likewise, moving
diagnostic enrichment to ingress would violate the structured-diagnostic rule
that rendering and observer-specific enrichment remain late.

Remediation checkpoints:

1. **W5C5-001A — Complete (2026-09-14): finding and evidence.** Record the
   failing surface, captured stacks and coordinator state, provisional
   ownership cycle, and the explicit non-solutions above.
2. **W5C5-001B — Complete (2026-09-14): forced-order characterization and
   semantic-admission repair.** Coordinator-level fixtures now drive the
   relevant lifecycle transitions directly: one admits a same-session client
   while its producer is `Running`, proves exclusion, publishes the producer's
   terminal result while retaining its detached machine and retirement tail,
   then proves the client becomes selectable before retirement. A paired
   fixture retires first and proves the other ordering. The first assertion
   failed finitely before the repair.

   The missing transition was the admission predicate treating
   `Terminalizing` as an active semantic machine. Terminal publication has
   already detached the machine and made its result authoritative; destruction
   and record retirement are non-semantic cleanup. Same-session admission now
   excludes only `Running` ordinary machines. Lifecycle readiness continues to
   count terminalizing records until cleanup completes. This removes one
   proven edge from the captured ownership cycle without weakening exclusion
   between two active semantic polls, relying on timing, or making worker-side
   recursive demand a scheduler feature. The original four-worker fixture can
   still expose the independently prohibited callback-to-client-demand edge;
   W5C5-001C/D own that remaining repair.
3. **W5C5-001C — Complete (2026-09-14): minimal callback protocol decision.**
   A specialization callback which can express its semantic post-processing
   as the returned value needs no new request-result variant:
   `RequestResult::Return(PublicValue)` is already the rooted, machine-owned
   handoff. The callback first performs its one atomic host observation or
   commit, then returns a lazy semantic graph without demanding it. The
   reflection machine delivers that graph to the continuation; ordinary WHNF
   work owns any later suspension, and an unused result performs no semantic
   post-processing at all.

   This is intentionally narrower than making synchronous nested evaluation
   safe inside arbitrary callbacks. Semantic argument preparation needed
   *before* a host action, and post-callback work whose disposition is not a
   returned value, still require the general W5C.5a protocol. No worker-side
   recursive-demand fallback was added.
4. **W5C5-001D — Complete (2026-09-14): diagnostic/test migration.** Diagnostic
   retrieval in both the executable logger and `TestEffects` now returns a
   lazily composed enrichment value. A compiler-private `DiagnosticObject`
   builtin preserves the existing plain-dictionary versus object
   normalization; ordinary `ObjectWithDefs` and `ObjectOverrideDefs` apply the
   metadata afterward. This staging gives each generated object fixpoint a
   stable lazy owner rather than constructing and immediately demanding it
   inside the host callback.

   FIFO observation and commit remain in the callback, while assembler and
   origin metadata are captured for late semantic enrichment. A direct API
   fixture proves prepared and eager enrichment agree after demand. A
   reflection fixture reads `message.msg.text` and latches exactly one
   specialization callback and one commit. The original zero-worker and
   four-worker descendant-failure fixture now completes with the same retained
   failure report.
5. **W5C5-001E — Complete (2026-09-14): focused audit and handback.** No
   production `TaskSpecialization::handle_request` path now calls
   `Diagnostic::{enrich,enrich_with_factory}`. The remaining synchronous
   specialization demands are the already inventoried reusable reflection
   requests, `TestEffects::Evaluate`, stderr byte extraction, configured CLI
   and token requests, macro requests, and interaction-net construction.
   W5C.5b owns the reusable reflection family; W5C.5c owns executable, macro,
   net, stderr, and test-specialization migration. None is needed by the
   diagnostic handoff mechanism, so W5C5-001 closes and W5C.4 may resume.

##### W5C5-002 — Terminal-policy parallel regression re-audit

**Status: complete (2026-09-16).**

NC2.0 routine verification on 2026-09-16 exposed another indefinite park in
`coordinator_terminal_policy_preserves_a_descendant_failure_before_root_return`
during the ordinary parallel `cargo test -q` run. The harness reported the
test running for more than 60 seconds and it remained parked until the run was
terminated. The same test completed in isolation, and the complete
single-threaded suite passed. Those passes show that the deterministic path is
intact; they are not evidence that the disputed parallel ordering is safe.

Treat this as a possible regression or incomplete characterization of
W5C5-001, not as an NC2 callable-state failure. Before closing it:

1. determine whether the parked state reproduces W5C5-001's former
   callback-to-client-demand cycle or names a distinct coordinator transition;
2. add barriers or coordinator hooks which force both sides of the discovered
   ordering and latch the exact machine, terminal-publication, retirement,
   diagnostic-admission, and wake transitions;
3. produce a finite failing fixture before changing the scheduler or
   admission rule; and
4. preserve the isolated and full single-threaded cases as semantic baselines,
   while treating uncontrolled parallel repetition as stress evidence only.

The investigation required either a code repair or proof that another named
owner was responsible; NC6C and the post-W6 review could not report the
ordinary parallel suite as clean without a forced-order disposition.

**Resolution.** This was distinct from W5C5-001. LLDB inspection of a parked
parallel run found every executor worker asleep and the settling thread inside
diagnostic normalization's client-demand wait. A bounded diagnostic probe then
captured the decisive state: the demanded work record and both ready queues
were empty while the result cell was still unpublished.

Client-demand retirement intentionally removes its coordinator record under
the runtime mutation guard, releases all runtime locks, and only then publishes
the result cell. The zero-worker driver already closed this tiny handoff by
waiting directly on the result cell when the record was absent. The
worker-backed driver instead sampled the post-retirement generation and waited
for a *future* coordinator transition; result publication does not create one,
so that ordering parked permanently.

A forced-order fixture now latches all relevant edges: worker detachment,
absence of the coordinator record, the client driver's wait decision, result
publication, and completion. It failed on the old code while identifying the
incorrect coordinator wait; a test-only generation disturbance then rescued
the thread so the fixture remained finite. The worker-backed path now adopts
the existing retirement handoff rule and rechecks the coordinator generation
before every ordinary wait. The fixture selects the result-cell handoff and
completes without rescue, and the original zero-/four-worker terminal-policy
test passes.

##### W5C.4 — Reset, shift, and continuation-stack traversal

**Status: complete (2026-09-14).**

**Review record (2026-09-14).** The semantic scope remains correct, but the
original two-checkpoint split predates the concrete W3--W5 machine shapes and
is too coarse. Reset-stack decoding is still concentrated in the synchronous
`reset_stack_value_in`, `reset_frames_in`, and
`reset_frames_from_value_in` helpers. One call can demand the stack shell,
traverse lazy list chunks, demand every frame shell, traverse each frame list,
convert its key, and validate both numeric fields. If a later step blocks, the
completed prefix exists only on the Rust stack and is replayed.

The established implementation no longer favors adding a generic
`CollectionWalk` continuation to `WhnfComputation`. `RequestDecodeWork`,
`KeyListMachine`, `ListFrontMachine`, and `StatePathWork` instead demonstrate
the smaller and more inspectable boundary: one domain-specific durable owner
composes the shared WHNF, logical-list-front, and key-conversion machines. Use
that shape for a `ResetStackMachine`; do not add reset/control policy to the
pure WHNF reducer merely because the work contains lists.

The old integration list also omitted two material call sites. Ordinary value
delivery decodes the active reset stack before choosing between a reset frame,
a Rust-side delimiter, and task completion. `Delimiter::Restore` validates a
saved stack before replacing the current stack and restoring its outer
control. Both are part of W5C.4. Conversely, `.resume` task and continuation
ID decoding is already complete in W5C.1 and must not be reopened here.

The reset stack is stored in task-local state and can be replaced by user code,
so its decoder continues to treat every serialized field as untrusted even
though compiler-created frames are strict. The canonical resumable key
converter replaces the legacy outer-WHNF-only `value_key_in` path. Scope and
order fields receive ordinary WHNF demand before integer/range validation;
this makes deferred numeric fields obey the language's normal lazy semantics
instead of being rejected merely because they are not already in WHNF. The
continuation field is deliberately retained without demand and is forced only
when control later applies it.

###### W5C.4a — Standalone resumable stack decoder

####### W5C.4a.0 — Decoder contract and source latch

**Status: complete (2026-09-14).**

Inventory every production read, validation, encoding, and replacement of
`continuation_state`. Record for each caller whether it needs the original
serialized stack, decoded `ResetFrame`s, or a newly encoded stack. Add a
compile-exhaustive fixture for the selected decoder states and latch the
remaining legacy helper call sites before behavior changes.

The decoder owns one source stack root, the currently active list-front and
field computation, completed `ResetFrame`s, and any completed fields of the
current frame. Keys, indexes, field tags, scopes, and orders are immediate;
only the source/list cursors, the current field, and completed continuation
values need runtime roots. It returns decoded frames without observing or
mutating `Control`, `next_control_order`, `next_continuation`, the continuation
table, or branch state.

Completion record: a source-backed fixture now latches the seven synchronous
helper families and their remaining definition/call counts. The selected
standalone representation is one `ResetStackMachine` containing the original
serialized root, an explicit state enum, completed decoded frames, and at most
one active shared WHNF/list/key submachine. Its terminal result returns both
the original stack root and decoded frames so fixpoint restore need not
re-encode or discard the exact saved value. W5C.4a.1--a.3 introduce the
compile-exhaustive state vocabulary together because splitting an enum from
its owning transitions would create deliberately nonfunctional intermediate
states; their semantic and verification checkpoints remain separate.

####### W5C.4a.1 — Stack shell and logical frame-list traversal

**Status: complete (2026-09-14).**

Add `ResetStackMachine` with an explicit source-WHNF phase. Require a list
after that demand, then consume its logical front through `ListFrontMachine`
so a lazy chunk suspends with the exact remaining suffix rather than replaying
earlier frames. Empty stacks complete directly. Preserve the existing
non-list stack diagnostic and give every local transition bounded work-unit
accounting.

At this checkpoint, extracted frame values may remain pending roots; frame
interpretation belongs to the next checkpoint. Verify strict, empty,
source-lazy, source-promise, and lazy-list-chunk cases with a one-step budget
and counted producers.

Completion record: `ResetStackMachine` now owns the exact serialized stack,
demands its shell through `WhnfComputation`, and advances logical list fronts
through `ListFrontMachine`. A one-step driver verifies strict and deferred
stack shells and list chunks without replay. Resolver-promise suspension is
also retained as an explicit `WorkDependency` rather than being converted to
a synchronous evaluator wait.

####### W5C.4a.2 — Frame shell, logical field traversal, and arity

**Status: complete (2026-09-14).**

For each extracted frame, demand its outer shell, require a list, and consume
exactly four logical fields through a nested list-front owner. Detect an
undersized frame at end-of-list and an oversized frame upon observing the
fifth field; do not traverse an arbitrary surplus merely to report the fixed
arity error. Preserve completed earlier frames while the frame shell or any
lazy field-list chunk suspends.

Keep the existing non-list-frame and wrong-size diagnostics. Add fixtures for
zero through five fields, a deferred frame shell, a deferred chunk before each
field boundary, and a deferred tail needed only to establish exact arity.

Completion record: each extracted frame has a nested durable owner for shell
demand and logical field traversal. The decoder retains completed fields,
rejects a non-list frame, rejects fewer than four fields at the observed end,
and rejects an observed fifth field without walking the remaining surplus.
Deferred frame shells and field chunks are counted and evaluated once.

####### W5C.4a.3 — Ordered field conversion

**Status: complete (2026-09-14).**

Decode fields in their serialized order:

1. convert the key through the canonical `KeyConversionMachine`, including
   recursive list/dictionary key values;
2. retain the continuation value as a root without demanding it;
3. demand `scope_depth` to WHNF and require a nonnegative `usize` integer; and
4. demand `order` to WHNF and apply the same validation.

Make the minimal visibility adjustment needed to reuse key conversion; do not
duplicate `Key::from_value` traversal in reflection. A suspension or failure
at a later field retains the prior converted key and continuation root. Only
after all four fields succeed may the completed `ResetFrame` enter the output
vector.

Add counted lazy and promise fixtures at key, scope, and order, including a
recursive deferred composite key. Verify that a deferred continuation remains
undemanded during decoding and is demanded exactly once only by later control
application. Preserve the existing invalid-key, invalid-scope, and
invalid-order diagnostics after WHNF is reached.

Completion record: ordered conversion now reuses `KeyConversionMachine`,
retains the continuation as an undemanded runtime root, and demands the scope
and order through separate resumable WHNF computations. The invalid-key text
is deliberately normalized to the canonical key-conversion diagnostic rather
than preserving the reset helper's private wording. Counted recursive key and
numeric fixtures demonstrate exact resumption, while a lazy continuation
remains untouched by stack decoding.

####### W5C.4a.4 — Standalone decoder closure

**Status: complete (2026-09-14).**

Compare decoded strict stacks with the legacy decoder before migrating callers.
Exercise nested frames, budget exhaustion at every phase, permanent child
failure, and cancellation/owner retirement without replaying a completed
prefix. Inspect the durable shape after each forced boundary and require that
it contains no raw `Value` and no roots unrelated to future work.

Do not remove the legacy helpers yet: W5C.4b migrates their control
dispositions one at a time, and W5C.6 owns the final recursive-helper census.

Completion record: strict nested stacks are compared field-for-field with the
legacy decoder. Dedicated fixtures cover every zero-through-five field arity,
non-list stacks and frames, invalid key/scope/order fields, deferred permanent
failure, one-step yields through every structural and conversion phase, and a
blocked decoder dropped before resolver-promise publication. The exhaustive
shape fixture confirms that durable state consists solely of roots, decoded
immediates, and the shared resumable submachines. Task-level cancellation and
branch retry remain integration properties of W5C.4b.8 rather than being
simulated by this task-independent decoder.

###### W5C.4b — Control integration

####### W5C.4b.0 — Durable control-work owner

**Status: complete (2026-09-14).**

Add a `ControlWork<S>` family beside effect decoding, scalar demand, and state
path work. It owns the original `Branch`, scope, reset-stack decoder, and the
operation-specific roots needed to finish one control transition. Integrate it
with task polling, blocking, yielding, retry/error handling, cancellation,
active-branch projection, and the compile-exhaustive root inventory before
migrating a request.

Follow the current low-risk `TaskExecution` ownership pattern for this phase;
do not combine all active work slots into a new general enum while control
semantics are moving. W5C.6 may perform that mechanical consolidation after
specialization work in W5C.5 establishes the final set of owners.

Completion record: `TaskExecution` now has a distinct `ControlWork` slot ahead
of its pre-existing demand/path/decode owners. It participates in active
branch and scope projection, cooperative polling, dependency blocking,
failure/retry handling, cancellation by ordinary task retirement, and the
compile-exhaustive root/state inventory. The placeholder `MachineWork` is
made inert while this slot owns the branch, matching the established
single-active-owner convention without consolidating the work families.

####### W5C.4b.1 — `.reset` request entry

**Status: complete (2026-09-14).**

Decode the request key, preserving it while the current stack is decoded.
Only after both complete may `.reset` allocate control order and continuation
identity, capture the current sequence, append the new reset frame, publish
the replacement stack, and begin the requested operation. All fallible
semantic validation precedes that commit section; the post-validation frame
encoder performs no demand.

Force suspension independently in the request key, stack shell, frame list,
stored key, scope, and order. Latch that branch state, control sequence,
continuation table, and allocation counters remain unchanged until the final
validation succeeds, then change exactly once.

Completion record: `.reset` now selects the current serialized stack without
demand, then transfers the branch, request key, operation, and stack decoder
to `ControlWork`. Canonical key conversion completes before stack decoding;
only their joint completion allocates the order and continuation, encodes the
new frame, publishes state, and starts the operation. A resolver-promise key
fixture forces the blocked ordering and latches that the state, continuation
table, and both allocation counters remain unchanged before publication; the
existing reset semantics and fused/unfused control suites cover successful
completion.

####### W5C.4b.2 — `.shift` request entry and capture

**Status: complete (2026-09-14).**

Decode the shift key followed by the current stack, then locate the innermost
matching frame without further demand. Preserve the unmatched-key diagnostic.
Only after a match is established may `.shift` split inner reset frames and
ordered delimiters, capture the current continuation, publish the shortened
stack, and apply the shift function.

Forced fixtures must show that suspension and permanent failure leave both
the reset and delimiter stacks intact. On success, the captured continuation
contains exactly the reset frames and delimiters inside the selected prompt,
and the target continuation remains on the outer control sequence in the same
order as uninterrupted execution.

Completion record: keyed control work now shares one resumable key/stack
frontier and carries an explicit reset-versus-shift disposition. `.shift`
does not search, split either stack, or capture a continuation until both
inputs validate; only the terminal transition performs those mutations and
applies the shift function. A resolver-promise key fixture forces suspension,
then an unmatched-key failure, while latching unchanged order/continuation
counters and delimiter state. Existing nested-reset, cut, task-locality, and
continuation-reuse cases verify the successful capture shape.

####### W5C.4b.3 — Captured-continuation installation

**Status: complete (2026-09-14).**

Migrate `install_captured_control`. Decode the caller's current reset stack
before rebasing or publishing any captured layer. Retain the captured
continuation and caller sequence while blocked. After validation, merge reset
frames and delimiters by their existing total `order`, allocate the replacement
order range once, publish the encoded stack, append the rebased delimiters,
and install the captured sequence as one control transition.

Keep cross-task and unknown-continuation rejection ahead of this work. Exercise
multiple invocation of one captured continuation, nesting beneath an existing
reset and resume delimiter, suspension in the caller stack, and failure before
publication. Completed `.resume` ID decoding remains owned by W5C.1.

Completion record: accepted `.resume` IDs now transfer the caller stack,
captured continuation, delivered value, branch, and scope to control work.
Stack validation precedes one checked reservation of the resume delimiter and
all rebased captured-layer orders. The terminal transition then publishes the
resume delimiter, encoded reset frames, rebased delimiters, captured sequence,
and delivered value without further demand. This also removes the old partial
publication in which the resume delimiter was pushed before stack validation.
A promised caller-stack fixture forces the ordering and observes no delimiter
or order allocation until resumption; the continuation suites cover nested
and repeated successful installation.

####### W5C.4b.4 — Initial fixpoint setup

**Status: complete (2026-09-14).**

Move the initial `.fix` path onto control work. Decode and preserve the
original reset stack, then encode the hidden empty stack before allocating the
control order, fixpoint promise, marker, active-fix entry, `Continuation::Fix`,
and `Delimiter::Restore`. A blocked or invalid stack must create none of those
externally meaningful control records.

Verify that a reset outside a fixpoint is hidden while the fixpoint body runs,
that the exact original serialized stack remains owned by the restore
delimiter, and that completing the body restores it before subsequent
`.shift` dispatch.

Completion record: initial `.fix` now preserves its `FixRoot`, original
serialized stack, entry branch, and empty choice history in control work. It
validates the complete stack before hiding it, allocating an order or managed
fixpoint promise, or installing active-fix/continuation/restore records. The
terminal transition retains the exact serialized stack in `Delimiter::Restore`
and encodes the hidden empty stack without another demand. A promised-stack
fixture latches the empty control/fixpoint state and unchanged order counter
before publication, then verifies normal restoration and completion after the
promise resolves.

####### W5C.4b.5 — Fixpoint restart paths

**Status: complete (2026-09-14).**

Adapt `restart_fixpoint_at_scope` and every outcome path which may select it to
return durable control work rather than synchronously calling fixpoint setup.
Preserve the selected `FixRoot`, choice history, and inherited restart stack
across suspension. Do not pop a restart or publish a new active fix until the
same validation boundary used by initial setup has completed.

Cover failed, retried, and completed alternatives, including a suspended reset
stack during replay. Count promise creation, choice selection, and reset-stack
publication so a resumed restart cannot allocate or publish either twice.

Completion record: `restart_fixpoint_at_scope` now selects and transfers a
matching root, choice history, and inherited restart stack into the same
control-work path as initial setup. Cut, ordinary top-level, and retained
search outcomes propagate that owner as `MachineStep::Control`; none can
synchronously rebuild the fixpoint. The obsolete synchronous start helper and
its nested stack decoder are removed. A promised entry-stack fixture forces a
selected restart to block, verifies that its exact `FixRoot` and unchanged
order counter remain owned, then resumes through one promise/order allocation
and the original result. Existing alternative tests retain the nonempty choice
history/replay matrix.

####### W5C.4b.6 — Delivery-time reset selection

**Status: complete (2026-09-14).**

When the ordinary continuation sequence is empty, retain the delivered value
and decode the current reset stack before comparing its innermost applicable
frame with the innermost Rust-side delimiter. If a reset wins, encode the
remaining frames and publish that stack before applying its continuation. If
no reset or delimiter applies, complete the branch without another demand.

Exercise suspension with both possible orderings of reset frame and delimiter,
at nested scope depths, and with no applicable local layer. The comparison and
selected transition must match uninterrupted execution and must not pop or
rebase either side before decoding succeeds.

Completion record: an empty Glam continuation sequence now transfers the
delivered root and branch to a delivery control operation. It decodes the
current stack before comparing the innermost applicable reset and delimiter,
then either publishes the shortened reset stack, advances the selected
delimiter, or completes. A promised-stack fixture forces both reset-wins and
delimiter-wins orderings, observes the untouched delimiter before resolution,
and inspects the exact `Apply` versus `Deliver` successor work. The migration
retires the now-unused `reset_stack_value_in` and `reset_frames_in` helpers.
While exercising the matrix, the standalone decoder-retirement GC fixture was
also moved off the process-global test heap so parallel tests cannot collect
one another's deliberately raw managed fixtures.

####### W5C.4b.7 — Delimiter restoration and stack replacement

**Status: complete (2026-09-14).**

For `Delimiter::Restore`, decode the saved serialized stack and validate the
current state dictionary before popping the delimiter or replacing the outer
control. Publish the restored stack through the non-demanding encoder, then
restore `Control` and redeliver the retained value. `Delimiter::Resume` has no
semantic stack demand and remains a direct transition.

Split the legacy helper's two roles explicitly: the decoder validates and
returns frames; the encoder writes already validated frames or a retained
validated serialized stack. No function named as a replacement or encoder may
silently demand values. Test malformed saved stacks, suspension at every saved
field, nested restore/resume delimiters, and exactly-once outer-control
restoration.

Completion record: delivery now turns a selected `Delimiter::Restore` into a
second control phase which retains the outer control, delivered value, branch,
and a decoder for the saved serialized stack. It does not pop the delimiter or
replace state until that decoder succeeds and the current state is confirmed
to remain a dictionary. Publication reinstalls the exact validated serialized
stack (whose deferred shell is then cached), restores the outer control, and
redelivers without another semantic demand. `Delimiter::Resume` remains a
direct transition. Promise and malformed-stack fixtures respectively force
successful resumption and failure-before-pop, and inspect the replacement
before redelivery. The last production synchronous reset-stack helpers are
removed; only the test-only legacy comparator remains until closure.

####### W5C.4b.8 — Control integration closure

**Status: complete (2026-09-14).**

Run the existing reset/shift, task-locality, root-state replacement, fixpoint,
cut, and continuation-reuse suites alongside a forced-boundary matrix. For
each applicable control disposition cover uninterrupted execution, one-step
budget yield, lazy and promise suspension, permanent failure, cancellation,
and branch retry. Count decoder entry, completed frame prefixes, continuation
capture, control-order allocation, fixpoint promise creation, and state
publication rather than relying on repeated schedules.

Require identical final control order and result for strict versus suspended
execution. Update the source/root inventories after each migration checkpoint;
retire `value_key_in` and the reset-specific synchronous helpers as their last
W5C.4 callers move. Leave the shared `evaluate_in` retirement and the final
no-unowned-demand census to W5C.6 after W5C.5 closes.

Completion record: all reset-stack semantic reads now pass through
`ResetStackMachine`; the old recursive key/stack/frame helper family is absent
from production source, while encoding is explicitly named and performs no
demand. Forced promise/lazy fixtures cover request key, structural stack and
frame phases, recursive key/numeric fields, captured installation, initial and
restarted fixpoints, both delivery orderings, and restore success/failure.
Existing task-locality, state replacement, cut, fixpoint, and continuation
suites provide the uninterrupted reference behavior. The final lifecycle
audit found and fixed stale `ControlWork` surviving retry wakes and terminal
task cleanup; a deterministic fixture forces both disposal paths. Task-level
work inventories remain exhaustive, and strict versus one-step polling reaches
the same control outcomes without replaying completed decoder prefixes. Every
W5C.4 fixture with durable managed state owns a private value domain; a
parallel reflection-suite run previously reproduced process-global test-heap
collection reusing one retained pointer as a different managed family.

##### W5C.5 — Specialization callback boundary

**Status: complete (2026-09-14).**

W5C5-001 pulls the protocol decision and the diagnostic/test specialization
slice ahead of W5C.4 because the known deadlock compromises routine suite
verification. Its completion does not imply that the full callback inventory
or the remaining reusable and executable-specific specializations below have
been migrated.

###### W5C.5a — Protocol decision gate

**Status: complete (2026-09-14).**

Inventory every `TaskSpecialization::handle_request` implementation and every
`RequestContext::{evaluate,evaluate_key_path,evaluate_path}` call. Determine
whether declarative argument preparation on `EffectRequestSpec`, a pollable
specialization-request machine, or a smaller combination gives the callback a
completed input without retaining managed access or an arbitrary Rust stack.

The selected protocol must structurally prevent replay of a callback after it
has observed or changed host or transaction state. Merely documenting that
callbacks should demand their arguments before side effects is not sufficient.
Record migration and compatibility consequences before changing the public
trait.

Decision record: use one pollable, specialization-owned request-work machine.
Do not add declarative argument modes to `EffectRequestSpec`. Such modes cover
the common raw-versus-WHNF distinction, but not a value discovered only after
a host or transaction read. Supporting those cases would require a second
continuation protocol beside the declarative one.

The source inventory now contains eleven `TaskSpecialization` implementations.
Seven are production specializations: `StandardEffects`, `ReflectionEffects`,
the executable logger and configured-CLI specializations, the nested token
parser, the macro runner, and interaction-net construction. The other four are
the protocol/search inventory fixtures, the full reflection test
specialization, and the public effect-embedding contract fixture. At scaffold
landing there are twenty-one direct `RequestContext` demand calls: twenty in
production and one in `TestEffects`. The logger's stderr request also enters
`Assembler::evaluator().eval` directly and is therefore a twenty-first
production nested-demand boundary even though it does not call a
`RequestContext` demand method.

The request families divide as follows:

- Reusable reflection requests demand key paths, dictionaries, metadata,
  `.eval` operands, log severity/message structure, and task handles. `.eval`
  uniquely converts a permanent demand failure to `{err:Diagnostic}` instead
  of propagating it.
- Environment lookup first demands the requested key path, then obtains the
  environment from the host or macro transaction, and only then can traverse
  lazy intermediate dictionaries. This is a genuine demand-after-observation
  shape.
- Task status/value/error first demand a task handle, then read the protected
  query view, whose path accessor is lazy, and only then can decode the query
  state. Task join similarly demands the handle before polling a shared
  completion source and may return an explicit dependency.
- Logger stderr extraction, configured-CLI text/atom/path/count decoding,
  token text/regex decoding, macro text/regex decoding, and net copy/port
  decoding are pre-effect demands. Several are sequential, so replay currently
  repeats already-completed decoding even when no host edit has occurred.
- Case scopes, task creation, macro data/layout operations, raw net data,
  diagnostic FIFO retrieval after W5C5-001, and most zero-argument readers and
  writers need no semantic demand. Configured `.read.token` runs a nested
  isolated effect search, but it does not suspend the outer request: an
  unavailable dependency is currently converted to a token-parser error. A
  future cooperative nested-search conversion can use the same owned-work
  protocol without changing it.

The selected protocol has these requirements; exact Rust names may be tuned
when the scaffold lands:

1. `TaskSpecialization` constructs a non-cloneable `RequestWork` from the
   decoded request and its rooted arguments. `EffectTask` owns that work in a
   dedicated `specializing` slot, parallel to its scalar, path, and control
   work owners. It must not leave the work in cloneable `MachineWork` while a
   specialization phase runs.
2. Advancing request work is a short, mutator-free callback. It may complete,
   take one cooperative step, request a semantic demand, or register an
   explicit shared-completion dependency. A requested demand transfers an
   already-advanced request state to the generic reflection machine before
   evaluation begins.
3. The generic owner polls the appropriate `WhnfComputation`, key-path, or
   value-path machine. Yield and lazy/promise/reflection suspension retain that
   owner and do not re-enter the specialization callback. A completed value or
   permanent failure is delivered to the advanced request state; this lets
   `.eval` capture failure while ordinary requests propagate it.
4. `RequestContext` loses `poll_context`, `evaluate`, `evaluate_key_path`, and
   `evaluate_path`. It retains value construction, host/transaction access,
   observation and commit recording, and nested-search construction. A
   callback cannot manufacture a blocked `TaskHalt`; task join and any future
   host completion wait use an explicit request-work transition.
5. Activity accumulated by one callback step is applied before a requested
   demand or wait is installed. Consequently a committed immediate effect
   clears retry eligibility once, and a host observation remains attached to
   the branch while its subsequent semantic demand is suspended.
6. Retrying an optimistic cut deliberately reconstructs request work from the
   branch checkpoint. Waking a semantic dependency resumes the existing work.
   The former is transaction replay; the latter must never replay a completed
   host/request phase.

This is a source-breaking change to the public, pre-release
`TaskSpecialization` trait. A compatibility implementation of synchronous
`handle_request` cannot provide the structural guarantee, so it must not
remain as a public fallback. Migration may use a crate-private, source-latched
adapter briefly to keep intermediate commits buildable, but W5C.5c removes it
and the old context demand methods. `EffectRequestSpec`, `TaskHost`, commit and
validation, and terminal `RequestResult` semantics remain unchanged.

The alternative designs were rejected for concrete reasons:

- Per-argument `Raw`/`Whnf` flags duplicate request-shape knowledge in the
  request spec and cannot express host-derived query/environment values,
  sequential composite message preparation, or `.eval`'s failure policy.
- Declarative preparation plus a new deferred-result continuation would create
  two suspension protocols and still need explicit wait ownership for task
  join.
- Keeping synchronous evaluation in `RequestContext` preserves an arbitrary
  Rust stack and makes dependency wake re-enter the original callback. An
  ordering convention cannot prevent a future callback from observing or
  changing host state before that demand.

###### W5C.5b — Reflection `.eval` and reusable requests

Partition this work as follows:

1. **W5C.5b.0 — Complete (2026-09-14): request-work scaffold.** Add the
   non-cloneable specialization owner, demand/result transitions, activity
   handoff, explicit wait handoff, and compile-exhaustive lifecycle inventory.
   Convert all implementations to the new trait shape with the smallest
   buildable internal adapter; do not expose the adapter as compatibility API.
2. **W5C.5b.1 — Complete (2026-09-14): Reflection `.eval`.** Move `.eval` first, preserving its
   deliberate conversion of permanent evaluation failure into
   `{err:Diagnostic}` while lazy and promise dependencies suspend the enclosing
   request. Force both completion dispositions and prove one request phase.
3. **W5C.5b.2 — Pure reusable preparation.** Preserve completed prefixes
   across every sequential demand, split into:
   - **W5C.5b.2a — Complete (2026-09-14): Dictionary and metadata inspection.** Migrate the two
     single-demand projection requests first.
   - **W5C.5b.2b — Complete (2026-09-14): Logging preparation.** Migrate message, optional message
     interface, and severity demand in established order without repeating a
     completed prefix or host emission.
   - **W5C.5b.2c — Complete (2026-09-14): Environment traversal.** Migrate key-path conversion and
     value-path selection as resumable recursive preparation.
4. **W5C.5b.3 — Task request family.** Split into:
   - **W5C.5b.3a — Complete (2026-09-14): Creation and control.** Migrate non-demanding task creation
     plus resumable task-handle preparation for acknowledgement and
     cancellation.
   - **W5C.5b.3b — Complete (2026-09-14): Query requests.** Migrate query lookup and state decoding.
     Query observation must precede its owned value demand and must not repeat
     after suspension.
   - **W5C.5b.3c — Complete (2026-09-14): Join.** Migrate task join using an explicit shared-completion
     transition rather than a blocked callback error.
5. **W5C.5b.4 — Complete (2026-09-14): Reusable closure.** Relatch request activity, retry, structured
   failure, and runtime-root inventories. Remove the reusable family's access
   to the transitional adapter.

W5C.5b.0 introduced `SpecializationRequestWork` as the only public callback
surface. `EffectTask` now owns its boxed, non-cloneable request state beside the
other durable machine owners and preserves it across WHNF demand, explicit
shared waits, yields, and wakeup without inflating every inactive task's stack
footprint. Every callback's observation and commit activity is applied before
interpreting its transition. Ten in-tree implementations use a library- or
executable-private synchronous bridge while they migrate; that bridge retains
its request inputs across the legacy callback's blocked result so intermediate
checkpoints preserve established behavior. The public embedding fixture
instead advances an explicit two-phase request, blocks on a forced promise
argument, and proves with counters that wakeup resumes rather than reconstructs
the owner. Source and compile-exhaustive inventories latch all eleven
implementations, the private bridge boundary, the new task slot, and every
transition/result variant. The old synchronous callback is absent from
`TaskSpecialization` rather than surviving as a public fallback.

###### W5C.5c — Remaining specializations

Partition the remaining migration as follows:

1. **W5C.5c.1 — Net and token preparation.** Split into:
   - **W5C.5c.1a — Complete (2026-09-14): Net construction.** Migrate copy counts and sequential
     construction ports. Raw construction data remains non-demanding.
   - **W5C.5c.1b — Complete (2026-09-14): Token parsing.** Migrate token text/regex inputs while
     zero-argument token operations remain non-demanding.
2. **W5C.5c.2 — Macro preparation.** Split into:
   - **W5C.5c.2a — Complete (2026-09-14): Environment and diagnostics.** Migrate environment
     traversal and severity/message handling in their established order.
   - **W5C.5c.2b — Complete (2026-09-14): Text operations and closure.** Migrate
     text/regex inputs, retain exact journal and scoped-layout behavior, and
     remove the macro specialization's synchronous adapter.
3. **W5C.5c.3 — Complete (2026-09-14): Configured CLI.** Migrate text, atom,
   path-handle, script, and worker-count preparation. Keep parser/effect
   operands raw. The nested token search remains behaviorally unchanged; only
   its explanatory text is demanded by the outer request owner.
4. **W5C.5c.4 — Logger, tests, and compatibility closure.** Split into:
   - **W5C.5c.4a — Complete (2026-09-14): Logger request work.** Migrate
     `.write_stderr` binary preparation and keep `.read_log` as an immediate
     transactional FIFO operation. Preserve abandoned-choice output behavior.
   - **W5C.5c.4b — Standard and test request work.** Split into:
     - **W5C.5c.4b.1 — Complete (2026-09-14): Uninhabited request work.** Use
       `Infallible` itself for the standard specialization and the two
       root-inventory fixtures rather than manufacturing unreachable adapters.
     - **W5C.5c.4b.2 — Complete (2026-09-14): Test specialization.** Migrate
       the in-tree test specialization's evaluate/stderr demands without
       changing its raw alternatives/scoped operands.
   - **W5C.5c.4c — Complete (2026-09-14): Resumption and FIFO conformance.** A
     hostile test request performs counted host activity before suspending on
     a forced promise and proves that resumption does not re-enter that phase.
     A paired machine fixture compares optimized empty `.read_log` with an
     explicitly authored retryable cut. Runtime FIFO fixtures separately force
     tail append, unrelated-endpoint publication, and competing-consumer
     commit order.
     - **W5C.5c.4c.1 — Complete (2026-09-14): Post-resumption subscription
       repair.** A forced logger schedule suspends output preparation, admits a
       diagnostic, resumes into an abandoned alternative, and only then reads
       the diagnostic FIFO. It reproduced a stale blocked transaction whose
       new read was acquired after the publication that resumed it. Every
       machine block is now installed and its complete retry read set validated
       before the blocked state becomes observable; a conflict restarts the
       machine immediately rather than waiting for another publication.
     - **W5C.5c.4c.2 — Complete (2026-09-14): Validation-latch closure.** The
       neighboring malformed-control fixture now starts from the host's
       actual generation, so it continues testing restore validation rather
       than deliberately triggering the new stale-subscription restart. The
       exact WHNF source census is refreshed for the centralized block path.
   - **W5C.5c.4d — Complete (2026-09-14): Compatibility closure.** Migrate the
     remaining uninhabited test fixtures, remove both synchronous adapters and
     every `RequestContext` demand method, and latch the closed source
     inventory.

Close the compatibility surface rather than leaving two different suspension
contracts under the same trait.

The logger migration owns the optimized-API conformance fixture. Existing
tests already cover an empty `.read_log` suspension, retry after diagnostic
arrival, clearing the retry checkpoint after a committed read, and an
observed error which does not advance `.alt`. Add a paired fixture showing
that the specialized empty-FIFO request has the same observable
retry/divergence behavior as an explicitly authored
`.cut (.alt HappyPath DivergeOnFailure)`. Force an unrelated-endpoint
publication, an append at the observed empty boundary, and a competing
consumer: the FIFO cursor must decide which events conflict while the broad
generation merely schedules revalidation. Retain an end-to-end configured
logger case beside the machine-level control fixture.

Completion record: all eleven specialization implementations now construct
owned request work directly. `RequestContext` no longer carries a poll context
or offers nested semantic evaluation, and neither the library nor executable
contains a synchronous specialization adapter. The updated WHNF census records
the removal of those evaluator and orchestration boundaries; durable-owner and
raw-value inventories record the explicit request states and their regional
evaluated-value access. Behavioral fixtures cover ordered alternatives,
macros, configured CLI parsing, logger output, request resumption, and FIFO
retry/conflict order.

##### W5C.6 — Recursive helper retirement and focused verification

**Status: complete (2026-09-15).**

W5C.5c.4d removed the local recursive `machine::evaluate_in`, `evaluate_root`,
and equivalent production loops in `RequestContext` when their final callers
moved. Close that surface in three checkpoints rather than recreating deletion
work here:

1. **W5C.6a — Complete (2026-09-15): Source and contract closure.** Re-run the
   exact W0B census and require no unowned recursive WHNF demand under
   `src/reflection`. Expand the
   source-backed callback inventory across every production specialization,
   remove migration-era comments, and update current architecture notes to
   describe owned request work rather than nested `RequestContext` evaluation.
   Any helper retained as a proven already-WHNF projection must assert or
   encode that precondition instead of silently evaluating.
2. **W5C.6b — Complete (2026-09-15): Lifecycle matrix closure.** Audit the
   family fixtures below, then add only missing forced cases:
   - **W5C.6b.1 — Complete (2026-09-15): Cooperative and failed demand.**
     Force one-step polling through a complete specialization request and
     terminate a separately suspended request through its exact promise
     failure. Count callback entry
     so neither yield nor terminal resumption can replay preparation.
   - **W5C.6b.2 — Complete (2026-09-15): Cancellation.** Cancel a
     coordinator-owned task while its specialization request owns an
     unresolved demand, then resolve that discarded dependency and prove the
     callback remains retired.
   Do not treat repeated schedules as evidence.
3. **W5C.6c — Complete (2026-09-15): Final verification.** Run the focused
   reflection, embedding, executable-specialization, macro, and profiling
   suites before the complete repository gates. Relatch inventories only for
   reviewed source movement.

Run each family with uninterrupted, budget-yielded, lazy-suspended,
promise-suspended, permanently failed, cancelled, and retryable branch forms
as applicable. Update root/publication inventories at each checkpoint rather
than relatching the aggregate only after all of W5C.

Completion record: every production specialization callback is source-latched
against nested evaluator entry, and the exact W0B census separately requires
that reflection contain no unowned recursive WHNF demand. The private request
selector now names its already-WHNF input contract. Current architecture and
agent notes describe specialization-owned work rather than the deleted
`RequestContext` evaluator service. Existing family fixtures cover strict and
lazy demand, ordered alternatives, retryable host observations, task joins,
macros, token/CLI parsing, logger input/output, and structured failures. New
forced fixtures add one-step cooperative progress, promise failure after
suspension, and cancellation followed by late dependency resolution, with
callback counts proving that no path replays completed request preparation.
Focused subsystem suites, exact source/root inventories, the complete routine
suite, and interaction-net profiling all pass.

#### W5D — Replay and branch verification

Close W5 in three bounded checkpoints:

1. **W5D.1 — Complete (2026-09-15): W0A replay closure.** Keep the forced W0A ordering and strengthen
   the repaired fixture to count every outer effect-application lazy and
   decoded request. The original application lazy must occur once, the one
   nested reflection task must activate once, and all task records and
   diagnostics must retire without replay.
2. **W5D.2 — Complete (2026-09-15): Failure and branch matrix.** Force the same application boundary
   before a request whose argument launches a nested reflection task and then
   fails with structured context. Separately block a nested reflection task in
   the first arm of a cut, observe the exact parent/child reservation set, and
   resume it before the arm fails. The fallback must run once, the discarded
   diagnostic must remain uncommitted, and parsed/dispatch/application counts
   must match the authored effect structure exactly.
3. **W5D.3 — Complete (2026-09-15): Verification and closure.** Run the focused replay, failure,
   branch, reflection, and inventory suites, followed by the routine repository
   gates and interaction-net profiling script. Record any future-phase drift
   found while closing W5; do not infer concurrency correctness from repeated
   schedules.

W5D.1 retains the exact one-shot deferred-producer boundary from W0A. Its
passing assertion now accounts for the three authored outer request
applications (`seq`, `.eval`, and `.r`), their three parse/dispatch entries,
the single nested reflection activation, empty diagnostic output, and complete
task-record retirement. The original application lazy appears exactly once;
later requests are counted separately rather than mistaken for replay.

W5D.2 adds two deterministic schedules. The failure fixture pauses before its
only outer request parses, then resumes through exactly one nested reflection
activation and retains the ordered `log_message` and authored argument
contexts without publishing a diagnostic. The branch fixture blocks a nested
reflection task on an empty diagnostic FIFO. At that point the coordinator
contains exactly the parent and child records and the outer task has parsed
and dispatched four requests. Admitting one diagnostic resumes the child; the
first arm then fails, its staged warning is discarded, and the fallback
finishes at exactly eight parsed, dispatched, and application-lazy entries.
Only the fallback's one information diagnostic remains. No fixture relies on
schedule repetition.

W5D.3 passes the 171-test reflection-machine suite, the focused WHNF/access/
durable-owner/raw-value inventories, all three new schedules under aggressive
collection, the ordinary 1,628-test library suite and every integration and
executable fixture, format, all-target/all-feature Clippy, and the focused
interaction-net profiling script. The complete aggressive-GC suite remains
red at its pre-existing public-API compatibility matrix and is not claimed as
W5 evidence; the W5D ownership boundaries themselves pass with forced
collection. The mandatory post-W5 review records implementation and
future-phase drift separately.

Exit: reflection may suspend at any WHNF request boundary without replaying
the enclosing effect phase.

### Phase W6 — Complete Pure Evaluator Conversion

These checkpoints are the control-flow counterpart of the parent D.2c raw
value/access migration. Perform each family once: a D.2c checkpoint which
needs resumable demand adopts the WHNF work form here rather than retaining a
temporary recursive wrapper for a later pass.

#### W6.0 — Inventory reconciliation and low-risk partitioning

**Status: complete (2026-09-15).**

Before editing production behavior, reconcile the exact W0B WHNF census with
the parent D.2c raw-value manifest and partition W6A-W6F into independently
verifiable checkpoints. At post-W5 review the D.2c manifest contains 176
operations: 17 value-demand, 12 application/sequence, 18 operator/net, 25
dispatch/scalar/strategy, 43 collection/pattern, 36 annotation/effect, 17
object, and 8 net-builtin operations.

The 17 `ValueDemand` operations require an explicit disposition. W1-W3 own
their resumable control flow, but the live raw-value inventory still classifies
them as D.2c violations. Assign each to an early W6 closure checkpoint or
prove that it is a downstream compatibility declaration with a different
named owner; do not silently treat the completed control-flow work as raw-API
closure.

For each family, record the exact declarations, existing regional/durable
shape, required semantic conversions, forced suspension fixture, ordinary and
aggressive verification subset, and inventory delta before implementation.
Reconcile the parent plan's dated prose counts at the same time. W6A-W6F are
scope headings, not single implementation spikes.

Completion record: the source-backed D.2c inventory assigns all 176 live
declarations to 42 exact checkpoint owners and latches each group with its own
signature fingerprint. Forty-one W6 checkpoints own 169 violations; the seven
legacy value-demand compatibility declarations are named individually and
remain assigned to W8. Current signature shape is 132 step-context, 7 durable-
context, and 37 context-free operations. The tables below record target shape,
and inventory delta for every implementation group; each family record adds
its forced fixtures and focused ordinary/aggressive verification. The parent
D.2c current-phase counts and W1-W3 versus W6/W8 responsibility are
reconciled. No production behavior changes in W6.0.

Verification record: the nine-test raw-value inventory and five-test WHNF
inventory pass, the checkpoint manifest passes with aggressive collection,
and format, all-target/all-feature Clippy, the complete routine suite, and the
focused interaction-net profiling script pass. The routine library partition
reports 1,629 passed and two ignored tests, followed by every integration and
executable partition.

#### W6A — Application and sequences

Complete the independent foundational value signatures before converting
application and sequence callers. The recursive key/tag/undefined and lazy-list
helpers are not independent foundations: their synchronous signatures can
disappear only after their cross-family consumers own resumable work. Their
established checkpoint names remain in the exact inventory, but they are
closure checkpoints executed after the final consumer migration rather than
prerequisites for W6A.1. `S`, `D`, and `F` below mean an existing
`EvaluatorStepContext`, durable `EvalContext`, or context-free signature. The
delta is the required reduction in the parent D.2c violation count.

| Checkpoint | Live declarations and current shape | Target and delta |
|---|---:|---|
| **W6A.0a — Complete (2026-09-15): Lazy-owner handoff** | 3 S | Make lazy completion/following consume evaluated or rooted handoffs; make cached-error inspection an access-qualified leaf. `-3`. |
| **W6A.0b — Complete (2026-09-15): Numeric projection** | 2 S | Split resumable operand demand from immediate `Number`/index validation. `-2`. |
| **W6A.0c — Complete (2026-09-17): Cross-family key/tag/undefined closure** | 2 S | W6E.5 removed the final synchronous key converter; W6F.4d.4 moved the remaining semantic coverage onto the durable tagged-payload owner and retired the compatibility pair. `-2`. |
| **W6A.0d — Complete (2026-09-17): Cross-family lazy-list closure (after W6F.2)** | 2 S | W6E.6 removed effect-map's final direct thunk-forcing use; W6F.2 moved object local-name parts to `ListFrontMachine`, so the production thunk-forcing and list-to-value compatibility pair is now retired. `-2`. |
| **W8 value compatibility** | 6 S, 1 D | Retain exactly `eval_value`, `eval_value_in`, `eval_lazy_in`, `eval_promised_in`, `await_deferred_task`, `deferred_wait_result`, and `produce_lazy_source_in` until their W6 callers disappear; W6 delta `0`, W8 delta `-7`. |
| **W6A.1a — Complete (2026-09-15): Application-local leaves** | 2 F | Access-qualify effect-function extension and non-callable diagnostics without introducing suspension. `-2`. |
| **W6A.1b — Complete (2026-09-17): Cross-family effect-value closure** | 1 F | Family checkpoints moved every context-free caller; W6E.6 now requires access on the shared constructor. `-1` at closure. |
| **W6A.2 — Complete (2026-09-17): Cross-family application closure** | 5 S | Migrate callers to the existing resumable WHNF application owner as their family checkpoints execute; introduce shared tagged-payload work at the first dictionary consumer, then delete the five synchronous application helpers after the last consumer moves. `-5` at closure. |
| **W6A.3 — Complete (2026-09-15): Sequence leaves** | 2 F | Access-qualify append validation/construction. `-2`. |
| **W6A.4 — Complete (2026-09-17): Cross-family key-sequence closure** | 2 S | Key paths and list keys now use owned traversal throughout; W6E.5 removed the final list-to-key helper and its direct key-conversion dependency. `-2`. |

Preserve currying, applicative dictionary behavior, list order, binary/list
streaming boundaries, and non-forcing constructors. The consumer checkpoints
which contribute to W6A.0c and W6A.0d each need a forced lazy tail or nested
deferred value; W6A.2 needs partial,
saturated, and over-application suspension; W6A.4 needs a forced lazy list
chunk. Run the focused value, application, and sequence suites ordinarily and
with `aggressive-gc-verification`. Relatch both checkpoint and family
fingerprints after each delta.

W6A.0a completion record: lazy completion now accepts only `EvaluatedValue`,
rooted machine completions project through one explicit checked adapter, and a
host callback's existing runtime root moves directly into resumable WHNF work
instead of being projected and immediately re-rooted. Cached-error inspection
is a callback-free access-qualified leaf. The host-call fixture now caches a
managed lazy result first, then forces collection after every callback/WHNF
handoff; this deterministically verifies root transfer without confusing the
returned lazy's separate coordinator-owned production with the handoff under
test. The D.2c manifest falls from 176 to 173 declarations and `ValueDemand`
from 17 to 14.

W6A.0b completion record: number and non-negative-index validation now consume
`EvaluatedValue` without owning demand or evaluator coordination. Numeric,
list, and net-arity callers perform their existing ordered demand first and
then pass the proven WHNF shell to these immediate validators; list-index and
net-arity failures retain their existing evaluation-context frames. The D.2c
manifest falls from 173 to 171 declarations and `ValueDemand` from 14 to 12.

##### W6A.0c/W6A.0d — Cross-family ownership and closure order

Do not implement either closure by placing a synchronous polling loop behind
the old helper signature. A helper which accepts `EvaluatorStepContext` and
returns an immediate `Result` cannot surface suspension without recreating the
recursive compatibility evaluator this plan is removing.

Migrate **key conversion** through the existing `KeyConversionMachine`,
generalizing its owner only as each real consumer requires. W6A.4 owns the
sequence consumers, W6B.4 the runtime-net consumer, W6D.1 the singleton
dictionary consumer, and W6E.5 the effect-call consumer. Tests which call the
old convenience wrapper directly must drive the same resumable owner or move
to the public behavior they intend to verify. After W6E.5 moves the last
consumer, remove `value_to_key_in` and its direct compatibility wrapper and
record W6A.0c's key-conversion share of the delta.

Migrate **singleton-tag inspection** by introducing one owned tagged-payload
work form at its first real consumer. W6C.3 became that first consumer, while
the remaining synchronous dictionary-application compatibility path still
needs conversion in W6F.4. The owned path performs
the recursive semantic-undefined walk,
retains its dictionary cursor and candidate values durably, and exposes normal
pending/yielded/ready/failed polls. W6C.3 reuses it for tuple comparison.
After the W6F.4 consumer moves, remove `tagged_payload_in` and
`is_semantically_undefined_in`; do not retain a second recursive undefined
walker merely for comparison.

Migrate **lazy-list forcing/front extraction** into existing `ListFrontMachine`
or the family-specific owned collection work selected by W6A.4, W6C.2,
W6C.3, W6D.3-W6D.5, W6E.1, W6E.5-W6E.6, and W6F.2. No collection callback may
call the old helper while holding regional access. W6E.5 removed the direct
front wrapper and final key-list use, W6E.6 moved effect-map's direct thunk
forcing into `ListFrontMachine`, and W6F.2 moved the final object-parts
consumer. The list-to-value walk and its thunk-forcing dependency are now
test-only, recording W6A.0d's complete delta.

The closure fixtures must force suspension at every ownership handoff rather
than rely on thread repetition: nested deferred dictionary members for tag and
key traversal, and lazy chunks before and after the requested list frontier.
Until the final consumer moves, the exact D.2c manifest deliberately continues
to assign the compatibility declarations to W6A.0c/W6A.0d; introducing their
replacement machines alone is not the checkpoint delta.

W6A.1 follows the same consumer-owned rule for `effect_value`. Requiring
regional access on that shared constructor immediately would also migrate the
context-free comparison, pattern, and effect-map leaves assigned to W6C.3,
W6D.5c, and W6E.6. W6A.1a therefore closes only
`apply_effect_function_value` and `non_callable_error`. Those later family
checkpoints thread their own regional access through effect construction; once
W6E.6 moves the final caller, W6A.1b access-qualifies `effect_value` and records
its remaining `-1` delta. Do not manufacture a context-free access token or
open nested access merely to preserve the old call shape.

W6A.1a completion record: effect-function extension and non-callable
diagnostic construction now require the caller's active regional access.
Application and runtime-net compatibility callers open bounded access only for
the immediate leaf; regional WHNF application reuses its existing access.
Neither operation can demand, block, or coordinate. The D.2c manifest falls
from 171 to 169 declarations and `ApplicationAndSequence` from 12 to 10;
`effect_value` remains the single W6A.1b closure declaration.

W6A.3 completion record: append validation and list construction now require
the caller's active regional access while preserving deferred lazy and promise
segments without demand. List dispatch and list concatenation open bounded
access only around this immediate conversion, and the direct promise fixture
uses one matching runtime throughout. The D.2c manifest falls from 169 to 167
declarations and `ApplicationAndSequence` from 10 to 8.

##### W6A.2/W6A.4 — Cross-family consumer order

W3B already provides the authoritative resumable application implementation
in `WhnfComputation`. W6A.2 must not wrap that owner in a synchronous polling
loop merely to retain `apply_value_in` or `apply_values_in`. Migrate consumers
in this order: W6B.2 owns operator application and function instantiation;
W6E.5-W6E.8 own effect API, effect-map, and list-effect application; and W6F.4
owns object composition/instantiation application. Each of these consumers
stores or delegates to the same child WHNF computation and surfaces its
pending/yielded/ready/failed result through its own machine poll.

W6D.4 is deliberately not an application consumer in this sequence. Builtin
list map is a non-forcing structural transform: it constructs ordinary lazy
application values for visible strict items and recursively wraps lazy list
holes, but it does not observe the callable or any mapped result. W6D.4
therefore retires its compatibility application call by constructing those
managed lazy values under regional access. Ordinary application machinery is
entered only if a consumer later demands a mapped item; W6D.4 neither retains
a child `WhnfComputation` nor contributes an application owner to W6A.2.

The reflection machine's `apply_in` compatibility bridge is not a separate
application semantics. Before W6A.2 closes, migrate that already-resumable
reflection owner to retain a child `WhnfComputation`; do not pull unrelated
D.2f reflection value projections into W6. After W6F.4 and this reflection
bridge move the final consumers, remove `apply_value_in`, `apply_values_in`,
`apply_function_values_in`, `apply_dict_value_in`, and `instantiate_function`.
Direct evaluator tests must drive the production application owner rather than
preserve those helpers solely as fixtures.

W6A.4 follows the key-conversion/list-front ownership established for
W6A.0c/W6A.0d. W6B.4 owns runtime-net path-index traversal, W6D.1 owns
dictionary-update paths, and W6D.5a owns pattern paths. The remaining
list-to-key consumer disappears only when W6E.5 completes the shared
W6A.0c key-conversion migration. Then remove `eval_key_path_list_in` and
`list_to_key_items_in` together and record W6A.4's `-2` delta. Forced fixtures
must suspend within a path element and within a lazy list segment without
restarting earlier converted keys.

W6A.4 completion record, 2026-09-17: effect-call name and argument preparation
now reuse the shared key-conversion and list-front owners. The obsolete
recursive `value_to_key_in` and `list_to_key_items_in` pair is removed, and
test key conversion goes through the production dictionary-singleton owner.
The remaining list-to-value compatibility walk is explicitly reclassified as
W6A.0d work rather than retained under the closed key-sequence checkpoint.

#### W6B — Operators and runtime nets

##### W6B.0 — Complete (2026-09-16): Borrowed poll-budget foundation

Complete this checkpoint before W6B.4b.2 begins. The current implementation
has two incompatible layers:

- regional WHNF already uses a mutable `WhnfStepBudget` and consumes one unit
  per callback-free semantic transition; but
- `EvaluationTaskMachine::poll` and most nested machine polls receive a copied
  `usize`, `poll_computation` constructs a fresh WHNF budget, and
  `NetWhnfMachine::poll` currently receives no production budget.

That shape permits a nested owner to reuse or recreate the same nominal
allowance and cannot report how much declared work one poll actually spent.
Replace it with one simple mutable token created for a claimed task quantum
and borrowed through every budget-aware nested poll:

```rust
struct PollBudget {
    granted: usize,
    remaining: usize,
}

impl PollBudget {
    fn try_consume(&mut self) -> bool;
    fn granted(&self) -> usize;
    fn remaining(&self) -> usize;
    fn spent(&self) -> usize;
}
```

The exact name may reuse/generalize `WhnfStepBudget`. There is one
authoritative `remaining` counter per outer poll, and a child receives
`&mut PollBudget`, never another integer initialized from the parent's
remaining value. Do not introduce synchronization or interior mutability: the
budget is stack-owned orchestration state used by one polling thread.

Completed migration:

1. **W6B.0a — Token and outer boundary.** Add `granted`, `remaining`, exact
   `spent`, and focused zero/one/many tests. Change
   `EvaluationTaskMachine::poll`, claimed-task forwarding, reflection and
   deferred task adapters, and their fixture implementations to borrow one
   token. The demand pump may continue discarding the unused part of a granted
   quantum initially, preserving current fairness and termination behavior;
   it records actual spend separately rather than pretending the whole grant
   was consumed.
2. **W6B.0b — Nested propagation.** Convert existing budget-aware reflection,
   search, access, object, list, net-construction, and WHNF polls to receive the
   same mutable borrow. Remove `step_budget.max(1)` and copied-budget
   forwarding. A phase-only yield may leave budget unused, but a yield caused
   by budget exhaustion must observe zero remaining units. Terminal or blocked
   observation may legitimately spend zero.
3. **W6B.0c — Semantic accounting latch.** Inventory every production
   `step_budget: usize`, budget constructor, and nested poll forwarding site.
   Any survivor must be an explicitly separate policy such as a host search
   limit, not a disguised evaluator allowance. Force nested calls which would
   previously each receive the full allowance and assert their combined spend
   never exceeds the original grant.
4. **W6B.0d — Translation-ready statistics.** Expose crate-private
   observations sufficient for deterministic tests and optional profiling;
   do not make work budgets part of Glam semantics or the public embedding
   API. Record that future conversion to a distinct net-reduction budget must
   reserve parent units and translate unused child units back explicitly.
   Do not design ratios, rounding, or refunds until a real second budget type
   exists.

Implementation result: `EvaluationStepBudget` is a two-word stack token with
exact grant, remaining, and spent observations. The claimed-task boundary,
client demand, resumable WHNF, reflection phases, isolated net construction,
and nested access/list/object machines all borrow that one token. Direct
session pumping and the public isolated-search convenience remain explicit
outer integer policies which construct the token; no internal `max(1)` or
copied nested allowance remains. Reflection delegates let child work consume
first, then charge an administrative transition only if the child consumed
nothing. This preserves one-unit progress without renewing fuel. The demand
pump still reserves and discards one whole task quantum, preserving scheduler
fairness while actual per-poll spend is independently observable.

This checkpoint measures declared semantic work, not wall time or CPU
instructions. It does not require every successful poll to spend a unit, and
the scheduler must not infer progress solely from `spent()`. Its purpose is to
make bounds compositional, prevent accidental budget renewal, and preserve a
usable accounting seam for profiling and later budget translation.

Verification: compile-exhaustive poll signature migration; exact spent/
remaining fixtures across direct and nested WHNF, reflection, search, and
lazy-task polls; one phase-only zero-spend yield; one exact exhausted yield;
and the existing demand-pump fairness/termination schedules. Run routine and
aggressive-GC suites because the signature reaches every task family, though
the token itself owns no values and opens no access.

| Checkpoint | Live declarations and current shape | Target and delta |
|---|---:|---|
| **W6B.1 — Complete (2026-09-15): Operator descriptors** | 9 F | Build descriptors beneath matching access or narrow them to immediate keys/IDs; never create an unrooted durable descriptor. `-9`. |
| **W6B.2 — Complete (2026-09-15): Operator execution** | 2 S | Convert one active-pair reduction and constant-effect construction without holding access across driver coordination; move operator application/function instantiation to the W6A.2 owner. `-2`. |
| **W6B.3 — Complete (2026-09-15): Net claim projection** | 2 F | Require the active claim/access capability when projecting callable or operator payloads. `-2`. |
| **W6B.4a — Complete (2026-09-15): Net attachment** | 3 S | Access-qualify function-stage attachment, argument attachment, and function-call machine construction without changing demand semantics. `-3`. |
| **W6B.4b — Complete (2026-09-16): Suspendable net application** | 2 S | Convert access resolution through its existing machine owner, then implement inline-first, spill-on-suspension callable WHNF through the focused callable-spill plan. `-2`. |

Keep topology and claim state in their existing net owners. Force suspension
after callable lowering and after the first attached argument, and retain the
cursor-claim restoration/contention fixtures. Run operator, runtime-net,
function-binding, and interaction-net profiling subsets ordinarily and under
aggressive collection; committed rule signatures must not change merely
because access ownership changes.

W6B.1 completion record: every operator constructor which carries captured or
supplied semantic values now requires the caller's active
`RuntimeValueAccess`. Front-end lowering reuses its existing access region;
the reflection request builder reuses its request-construction region; and
evaluator/test callers open only bounded, callback-free regions. The closure
also found that `constant_effect_template`, a temporary net descriptor which
embeds its request value, belonged to this checkpoint despite being omitted
from the prose enumeration. The exact manifest still confirms the planned
`-9` delta: `OperatorAndNet` falls from 18 to 9 and the complete D.2c manifest
from 167 to 158 declarations. No durable root or net-topology change was
introduced.

W6B.2 completion record: one claimed operator pair is now projected, reduced,
and terminalized beneath one matching `EvaluationValueAccess`; no driver or
scheduler coordination occurs before that region closes. Saturated applicable
operators emit a lazy application into the net instead of synchronously
demanding their function while the pair is claimed. The existing WHNF owner
therefore retains and resumes the exact promise, reflection, or intermediate
application state without restoring and replaying the pair. Function capture
instantiation and constant-effect construction reuse the same regional access.
The generic blocked-operator protocol remains test-only for exact restoration
coverage, while production operator waits now belong to emitted WHNF work.
The complete D.2c manifest falls from 158 to 155 declarations:
`OperatorAndNet` falls from 9 to 7 and `ApplicationAndSequence` from 8 to 7,
closing W6B.2 and moving `instantiate_function` out of W6A.2. Forced promise
and reflection-gate fixtures verify both the emitted lazy boundary and exact
resumption ordinarily and under aggressive collection.

W6B.3 completion record: callable roots and cloned operator/data payloads can
now be projected from their stack-bound claims only while a matching
`EvaluationValueAccess` is present. Callable projection closes its short
region before suspendable callable lowering; operator projection reuses the
single reduction/terminalization region established by W6B.2. Claims continue
to own only exact restoration state, while their originating rooted runtime
net remains the semantic owner of operator payload edges. The D.2c manifest
falls from 155 to 153 declarations and `OperatorAndNet` from 7 to 5, leaving
only W6B.4's runtime-net application seams.

W6B.4a completion record: function-stage and multi-argument attachment now
build their runtime net beneath matching access. A compatibility partial
application publishes that completed runtime through its evaluator-step owner;
the production function-call source constructs, roots, and installs its
`NetWhnfMachine` before the access region closes. `NetWhnfMachine` and
`NormalizationRequest` consequently have regional constructors rather than
reopening nested access. The D.2c manifest falls from 153 to 150 declarations
and `OperatorAndNet` from 5 to 2. The remaining two declarations are genuinely
suspendable callable/path operations assigned to W6B.4b, not attachment
leaves.

##### W6B.4b — Partition and callable-spill subplan

W6B.4b retains one inventory umbrella and its planned `-2` delta, but executes
as two independently verified checkpoints:

1. **W6B.4b.1 — Complete (2026-09-16): Runtime-net access resolution (`-1`).** Replace
   `resolve_core_access_in` with state retained by the existing resumable
   access owner. Preserve the W6A.0c/W6A.4 key/path conversion assignments and
   force suspension after at least one completed path element.
2. **W6B.4b.2 — Complete (2026-09-16): Callable WHNF spill (`-1`).** Execute NC0-NC6 in
   [`InteractionNetCallableWhnfSpill_2026-09-16.md`](InteractionNetCallableWhnfSpill_2026-09-16.md).
   Evaluate a deferred callable as far as the current bounded quantum permits
   and replace its data with one
   `CallableCheckpoint(NetWhnfState)` only on budget yield or a real dependency
   boundary. Suspended progress and liveness belong to the managed net; no
   call or checkpoint claim becomes durable.

The separate plan is required because W6B.4b.2 adds core topology, semantic
budget sharing, a zero-walk net-owned role wrapper around canonical regional
WHNF state, exact
same-pair checkpoint mutation, production blocked-checkpoint resumption, and
forced stale-admission/lost-wakeup verification. The parent plan remains
authoritative for the raw-value inventory and records the combined W6B.4b
closure only after both subcheckpoints pass.

W6B.4b.2 must not add `Clone`, `Debug`, `PartialEq`, or `Eq` to
`NetWhnfState`, `CallableCheckpoint`, or their retained values. The checkpoint
requires only `Send + 'static`: workers may own it at different times, but the
runtime-net mutex and exact pair claim make `Sync` unnecessary. It has only
the `Bind >< CallableCheckpoint` reducing rule; fan, erase, and every other
principal partner are stuck, while cursor copying waits for the source active
pair to produce semantic topology. The outer `ManagedCoreNetCell` trace must
visit every nested checkpoint edge; a box is traced storage, not a root.
Focused NC0D inventories the current
`NetSpecialization` bounds, `RuntimeNode` derives, whole-node clones, and
test-only equality/formatting assumptions before the variant lands. NC2 owns
their checkpoint-path repair, and NC6 must reconcile the result with both the
D.2c raw-value manifest and persistent-edge P3/P4 cutover.

NC0 completion record, 2026-09-16: the source-backed inventory assigns 24
interlocks before the variant lands: twelve ordinary payload-compatibility
operations remain parent D.2c/P3 work, ten runtime-node/checkpoint-path
operations belong to focused NC2, and two topology observations belong to
focused NC2 fixtures. The current D.2c count does not change merely for
recording future topology. NC2 must update this assignment atomically with the
new associated type and node variant; NC6 then owns the planned
`OperatorAndNet -1` closure rather than treating any temporary carrier trait
as progress.

NC1 completion record, 2026-09-16: the complete raw-edge `NetWhnfState` now
round-trips every regional frame and liveness field through matching access,
and its edge visitor survives a forced collection without per-value roots.
Net-owned and ordinary durable work share `drive_regional`. `NetWhnfMachine`
also borrows the one outer semantic budget; zero-budget discovery restores an
already claimed call/operator pair before requeueing it. The source-backed
reachability inventory keeps ordinary callable demand frame-free and leaves
producer-only frames, source ownership, and promise breadcrumbs intact for
NC5D rather than optimizing them prematurely.

NC6A completion record, 2026-09-16: W6B.4b.2 retires the last synchronous
deferred-callable test seam and closes its exact `OperatorAndNet -1` inventory
delta. The remaining W6B.4 net declaration is `resolve_core_access_in`, owned
solely by W6B.4b.1. The checkpoint needs only `Send + 'static`; source-backed
P3/P4 latches reject copying or equality traits on `NetWhnfState`.

NC6 closure record, 2026-09-16: routine, profiling, forced-order, focused
aggressive-GC, exact-inventory, and direct-assembly baseline verification all
pass. The focused post-NC review is recorded in
[`InteractionNetCallableWhnfSpill_2026-09-16.md`](../reviews/InteractionNetCallableWhnfSpill_2026-09-16.md)
and finds no open checkpoint defect or W6C-W8 drift. W6B.4b remains open only
for the independent W6B.4b.1 access-resolution declaration.

W6B.4b.1 completion record, 2026-09-16: the general synchronous
`resolve_core_access_in` helper is removed. `EffectCall` and the effect-map API
handoff now construct one semantic lazy composition whose inner `Access`
source feeds an outer `Application` source. Construction does not demand the
API member; ordinary WHNF/access owners retain the exact path and application
state when demand later blocks. The outer lazy is temporarily rooted by the
evaluator step, while its inner access lazy remains a traced edge allocated
under the same managed region rather than acquiring another root.

A focused fixture leaves the selected API method as an unassigned promise,
proves operation construction succeeds without observation, then demands,
assigns, and resumes to the expected result. The existing two-key static
access fixture forces suspension after its first completed path element and
proves exact-prefix resumption. Both pass under aggressive collection. The
D.2c manifest falls from 149 to 148 declarations, `OperatorAndNet` from one
to zero, and the scoped `eval/net.rs` context inventory from 22 to 21. Dynamic
key/path conversion remains with W6A.0c/W6A.4, and the surviving effect
dispatch/map work remains assigned to W6E.5/W6E.6. Together with NC6,
W6B.4b closes its planned `-2` delta.

Representation revision: NC1's parallel net/regional definitions and borrowed
projection are correctness scaffolding only. Focused NC2.0 replaces them with
one `WhnfState`/`WhnfContinuation` vocabulary before the runtime node becomes
live. `RegionalWhnfWork` and `NetWhnfState` then consume and rewrap the same
state without walking continuations, duplicating values, registering roots, or
allocating containers. NC5D records which fields production callable demand
actually exercises, but does not pare this rare transitory checkpoint merely
to save a few words.

NC2.0 completion record, 2026-09-16: the parallel definitions and projection
walk have been removed. Repeated regional/net role handoffs preserve every
tested outer and nested allocation identity and create no roots, while the
single canonical edge visitor passes the forced-collection liveness fixture.
The fine-grained rooted durable representation remains intentionally separate
until W6G.3 aggregates it.

#### W6C — Dispatch, scalars, comparisons, and strategies

| Checkpoint | Live declarations and current shape | Target and delta |
|---|---:|---|
| **W6C.1a — Complete (2026-09-16): Generic arity extraction** | 1 F | Remove raw-value ownership from exact-arity extraction by making it a generic container operation. `-1`. |
| **W6C.1b — Complete (2026-09-18): Final builtin dispatcher closure** | 1 S | After every semantic family owns suspendable work, thread the caller's regional leaf through the now callback-free dispatcher. `-1` at closure. |
| **W6C.2a — Complete (2026-09-16): Builtin assertions and conditionals** | 2 S | Separate builtin operand demand from unit/kind validation, preserve structured assertion context, and move conditional list-front demand into owned work contributing to W6A.0d. `-2`. |
| **W6C.2b — Complete (2026-09-17): Annotation assertion closure (with W6E.1)** | 1 S | Remove the remaining synchronous `assert_unit_in` compatibility helper when annotation dispatch moves into its owned machine. `-1` at closure. |
| **W6C.3a — Complete (2026-09-16): Shared tagged payload** | prerequisite | Introduce the first shared tagged-payload owner and its iterative semantic-undefined stack; retain the compatibility helpers until W6A.0c closes. |
| **W6C.3b — Complete (2026-09-16): Comparison** | 6 S, 3 F | Convert ordered/equality operand work, reuse W6C.3a tagged-payload work, and move list-front demand toward W6A.0d closure; keep condition/effect constructors immediate. `-9`. |
| **W6C.4 — Complete (2026-09-16): Numeric** | 5 S | Convert numeric operand sequencing, leaving arithmetic on immediate `Number` data. `-5`. |
| **W6C.5 — Complete (2026-09-16): Provenance** | 1 D | Replace the durable evaluator facade with an explicit provenance owner and bounded opaque-origin inspection. `-1`. |
| **W6C.6 — Complete (2026-09-16): Strategy** | 1 S, 4 D | Convert `seq` demand and `spark` admission so scheduler work begins only after regional access closes. `-5`. |

Operations which require only immediate data become immediate-data helpers,
not artificial WHNF frames. Force suspension on the second comparison/numeric
operand, inside assertion context construction, and immediately before
strategy scheduling. Run builtin dispatch, assertion, conditional, numeric,
comparison, provenance, `seq`, and `spark` suites in both GC modes. W6C.5 and
W6C.6 must retain the source-backed no-callback/no-scheduler-under-access
checks.

W6C.1 is deliberately split around the family migration. Passing regional
access through `apply_builtin_in` before W6C.2-W6F.7 would place the existing
synchronous operand demands and scheduler handoffs beneath that access and
would only make the signature inventory look complete. W6C.1a is independent:
`exact` inspects only `Vec` length/ownership, so it becomes generic rather than
accepting raw `Value`. W6C.1b closes the dispatcher only after its final
family branch is callback-free.

W6C.1a completion record, 2026-09-16: exact-arity extraction is generic over
the owned element type and therefore neither observes nor owns a semantic
value. Existing callers retain identical arity diagnostics and array
conversion behavior. The D.2c manifest falls from 148 to 147 declarations;
`DispatchScalarAndStrategy` falls from 25 to 24 and W6C.1 retains only the
final dispatcher declaration.

W6C.1b completion record, 2026-09-18: saturated builtin dispatch now accepts
the caller's bounded `EvaluationValueAccess` rather than opening access from
an evaluator-step context. Demand-capable families continue to install their
durable builtin machines; the remaining immediate append and list-effect
constructors execute regionally, and the lazy-source owner publishes their
result as a runtime root before closing that region. The test-only durable
wrapper remains explicit until W8, but production dispatch contains no nested
access, callback, wait, scheduler handoff, or unrooted regional result. The
raw-value inventory reclassifies the final dispatcher from violation to
regional access: violations fall from 263 to 262, the D.2c
`DispatchScalarAndStrategy` family and W6C.1 checkpoint reach zero, and exact
context/root-publication inventories record the immediate-result handoff.

W6C.2 is split at the annotation boundary. Saturated builtin assertion and
conditional sources can own their demand immediately, while annotation
assertion still calls the shared synchronous helper from the W6E.1 dispatcher.
Deleting that helper before W6E.1 would either duplicate assertion semantics
or conceal its demand beneath the compatibility evaluator.

W6C.2a completion record, 2026-09-16: builtin `assert_unit`, `if_result`, and
`match_result` sources now install durable builtin machines. Assertion demand
retains the asserted value and target once; a failing assertion yields before
demanding its structured diagnostic context and resumes without replaying the
value. Conditional demand validates the result list and traverses its first
logical item through the shared list-front machine, preserving the existing
empty-result diagnostics. A selected target/result remains lazy and is handed
back to ordinary WHNF demand rather than being mistaken for a completed WHNF.
The old conditional implementation and builtin assertion entry point are
removed. The D.2c manifest falls from 142 to 140 declarations; W6C.2 retains
only the annotation compatibility helper assigned to W6C.2b/W6E.1.

W6C.4 completion record, 2026-09-16: saturated numeric sources now install one
durable builtin machine. It roots each operand once, polls them in source order
through the existing WHNF owner and retains only immediate `Number` results
between polls. Arithmetic and number-kind validation remain callback-free;
dependency admission occurs only after WHNF access closes. The forced fixture
suspends on operand two after evaluating an instrumented first operand, then
resumes without replay. Division-by-zero and wrong-kind diagnostics remain
unchanged, and the fixtures pass with aggressive collection. The old numeric
dispatcher and synchronous implementation are removed. The D.2c manifest
falls from 147 to 142 declarations and the W6C.4 group reaches zero.

W6C.5 completion record, 2026-09-16: origin inspection now installs a durable
builtin machine rather than downgrading the dispatcher to `EvalContext`. The
machine owns and resumes ordinary WHNF demand, adds the established
`compilation_origin` frame to permanent demand failures, and performs the
edge-free opaque provenance downcast only after demand reaches WHNF. The
forced fixture suspends on an unassigned origin operand, then propagates a
deferred failure with its context intact; public round-trip and wrong-family
fixtures pass in both GC modes. The old provenance module is removed, the
dispatcher retains only strategy's deliberate durable-context downgrade, and
the D.2c manifest falls from 140 to 139 declarations with W6C.5 at zero.

W6C.3 completion record, 2026-09-16: saturated comparisons now install a
durable stack machine. Ordered operands, recursive lists, dictionary members,
and tuple payloads retain exact rooted progress across pending and yielded
polls; the forced fixtures suspend on operand two and on a lazy tail without
replaying an already-observed prefix. The shared tagged-payload owner uses an
explicit iterative semantic-undefined stack, so nested ignored members can
suspend without Rust recursion or re-reading prior members. Conditional
effect construction remains immediate, selected results remain lazy, and
comparison failures are terminally cached through the containing lazy just as
other builtin failures are. That last rule repairs a deterministic restart
loop exposed by nested function-comparison failure. The old comparison module
and obsolete list-like compatibility leaf are removed. The D.2c manifest
falls from 139 to 129 declarations: W6C.3 reaches zero and W6D.4 falls from
five declarations to four. Focused comparison, tagged-payload, inventory, and
aggressive-collection fixtures pass.

W6C.6 completion record, 2026-09-16: saturated `seq` and `spark` sources now
install one durable strategy owner. `seq` retains resumable progress through
ordinary demand and the value's hidden metadata demand before handing its
target back to ordinary WHNF evaluation. `spark` publishes a rooted scheduling
request from the builtin poll, and the task pump admits that best-effort work
only after evaluator access has closed. Spark workers retain the same strategy
machine across yields and dependency wakes instead of restarting demand. The
old synchronous strategy implementation is removed, the source/access and
poll-boundary inventories latch the post-access handoff, and the W6C.6 raw
declaration group reaches zero.

#### W6D — Dictionaries, lists, and patterns

| Checkpoint | Live declarations and current shape | Target and delta |
|---|---:|---|
| **W6D.1 — Complete (2026-09-17): Basic dictionaries** | 4 S | Convert dispatch, singleton, union, and update entry points, reusing W6A.0c key-conversion and W6A.4 key-path work. `-4`. |
| **W6D.2 — Complete (2026-09-17): Dictionary merge** | 6 S, 2 F | Convert recursive merge/update and duplicate handling; access-qualify key/path value leaves. `-8`. |
| **W6D.3 — Complete (2026-09-17): List observation** | 7 S | Convert at/head/len/split/tail/slice work with no access spanning lazy-tail demand, contributing its consumers to W6A.0d closure. `-7`. |
| **W6D.4a — Complete (2026-09-17): Structural lazy map** | 1 S | Replace eager list-map traversal with the non-forcing structural transform specified below. It may demand the subject enough to establish the outer list, but preserves internal list holes and does not demand or validate the callable. `-1`. |
| **W6D.4b — Complete (2026-09-17): Structural lazy list concatenation** | 1 S | Convert concatenation to one-node structural flattening, balancing the segments contributed by a reached strict leaf without observing deferred outer children. `-1`. |
| **W6D.4c — Complete (2026-09-17): Text lines** | 1 S | Convert binary/text extraction to durable source, list-front, and item demand without retaining access across suspension. `-1`. |
| **W6D.4d — Complete (2026-09-17): Family dispatch** | 1 S | Inline the immediate append leaf after every suspendable list transformation owns a machine and remove the obsolete family module. `-1`. |
| **W6D.5a.1 — Complete (2026-09-17): Pattern path equality** | 3 S | Reuse key/path conversion through a mismatch-preserving internal poll, retaining invalid expected paths as errors and invalid subject paths as pattern failure. `-3`. |
| **W6D.5a.2 — Complete (2026-09-17): Pattern dictionary shape and emptiness** | 2 S | Convert dictionary kind and logical emptiness through ordinary WHNF plus the shared iterative semantic-undefined owner. `-2`. |
| **W6D.5a.3 — Complete (2026-09-17): Pattern dictionary extraction** | 4 S | Convert required and optional recursive path extraction to an iterative owner which rebuilds persistent remainders without Rust recursion and shares semantic-undefined traversal. `-4`. |
| **W6D.5a.4 — Complete (2026-09-17): Pattern literal equality** | 1 S | Convert directional literal comparison and binary/list equality. `-1`. |
| **W6D.5b.1 — Complete (2026-09-17): Pattern-list structure** | 4 S | Convert list shape, empty, uncons, and unsnoc to shared resumable front/back traversal. `-4`. |
| **W6D.5b.2 — Complete (2026-09-17): Pattern-list item leaf** | 1 F | Retire the remaining context-free list-item conversion after binary/list literal equality moves to the pattern owner. `-1`. |
| **W6D.5c — Complete (2026-09-17): Pattern effects and dispatch** | 1 S, 3 F | Convert the dispatcher and access-qualify success/failure/effect constructors. `-4`. |

W6D.1-W6D.2 completion record, 2026-09-17: basic dictionary dispatch and
recursive merge could not form useful independent checkpoints because union
and update immediately enter the recursive transformation leaves. They were
therefore completed as one dictionary-owner checkpoint. Saturated singleton,
union, update, and duplicate-merge calls now install a durable machine which
reuses the shared key and key-list owners and retains each completed WHNF
operand across later suspension. Merge and nested update inspect and transform
already-observed persistent dictionaries only under regional value access;
deferred duplicate members and deferred nested paths remain lazy builtin
applications. Forced promise fixtures cover ordered union and duplicate-merge
resumption, including no replay of a completed left operand. The obsolete
dictionary modules are removed, `ObjectDictDefs` shares the regional merge
leaf pending W6F, and the exact raw-value manifest removes all twelve W6D.1
and W6D.2 violations: `CollectionsAndPatterns` falls from 42 to 30 and D.2c
from 124 to 112.

W6D.3 completion record, 2026-09-17: saturated at, head, length, split,
split-from-end, tail, and slice calls now install one durable list-observation
owner. Index operands are demanded before subjects as before, and logical-list
work advances through reusable front/back machines without holding regional
access across a lazy or promised chunk. Front observations retain completed
prefix progress; split-from-end walks from the back so a known strict suffix
does not force an unrelated lazy prefix. Result lists and split dictionaries
are rooted inside their producing access region. Deterministic promise
fixtures force suspension on both traversal directions, verify that a
completed prefix is not replayed, and retain the established lazy-prefix
contract. The obsolete synchronous observation helpers are removed;
`CollectionsAndPatterns` falls from 30 to 23 and the D.2c manifest from 112
to 105.

##### W6D.4a — Structural lazy map semantics

Builtin `map` is a homomorphism over the observable list structure rather than
an eager traversal of the logical list:

```text
map f (A ++ B) = defer (map f A) ++ defer (map f B)
map f (defer X) = defer (map f X)
```

`map` inspects only the root representation node reached by one invocation. A
`Concat` preserves that node and turns both children into lazy recursive maps,
regardless of whether either child is already strict. A source thunk remains
one thunk containing a lazy recursive map. Empty remains empty. A reached
`Bytes`, `Values`, or `Finger` representation is treated as one indivisible
strict leaf for this transition: construct the ordinary lazy application
`f value` for each contained item, but neither split the representation into
new `Concat` nodes nor evaluate an application. Byte items become their
corresponding numbers before application.

This one-node unfolding means mapping a `Concat` performs constant structural
work and allocates no applications for either unused child. A strict suffix
remains observable from the back without forcing or translating an unrelated
prefix, and mapping does not sacrifice laziness according to which end a
consumer chooses.

The callable is shared but not evaluated or validated by `map`. A malformed
callable fails only when a mapped item is demanded, through the ordinary
application semantics and lazy cache. The initial implementation allocates one
lazy recursive map per reached `Concat` child and one lazy application per item
in a reached strict leaf. It does not walk below a `Concat`. A specialized
mapped-list node belongs to later list-representation/performance work. The
one-node transformation is callback-free while regional access is held and
does not need a child WHNF application machine because it cannot suspend on
the applications it constructs.

Test strict value and binary fragments, a lazy middle or prefix with an
observable strict suffix, callable laziness and sharing, and delayed malformed
callable failure. In particular, the fixture for the lazy prefix must latch
that constructing the mapped `Concat` visits neither child and that observing
the mapped suffix does not force or translate the prefix. A later builtin
`concatMap` should follow the same structural law: each strict item contributes
a deferred list segment and every `Concat` child or list hole recursively wraps
`concatMap`. That future operation is a design direction, not part of W6D.4.

Ordinary thunks do not promise a logical length, so `len` still opens every
mapped structural thunk. Do not introduce special length metadata in W6.
Exact-length nodes such as `Take n xs`, including their error-filling
semantics, mapped-list nodes, and finer subdivision of large strict leaves are
deferred to the value-representation list review.

W6D.4a completion record, 2026-09-17: saturated `map` calls now install a
durable source-demand owner while leaving the callable entirely unobserved.
The persistent-list module owns one-node transformation: `Concat` keeps its
shape and gains two lazy recursive maps, a source thunk gains one lazy
recursive map, and a reached strict leaf constructs ordinary lazy item
applications without evaluating them or inventing new chunk boundaries.
Deterministic fixtures prove that constructing the mapped concatenation visits
neither child, right-to-left observation avoids an erroring prefix, callable
evaluation is delayed and shared, malformed callable failure is delayed until
item demand, and a promised source resumes through the same owner. The old
synchronous map implementation is removed; `CollectionsAndPatterns` falls
from 23 to 22 declarations, W6D.4 retains three, and the D.2c manifest falls
from 105 to 104.

##### W6D.4b — Structural lazy list-concat semantics

Builtin `list.concat` follows the same one-node structural boundary as `map`
where flattening permits it:

```text
concat (A ++ B) = defer (concat A) ++ defer (concat B)
concat (defer X) = defer (concat X)
```

A reached strict outer leaf contributes one inner list segment per item. The
implementation recognizes already-visible list, binary, lazy, and promised
segments without demanding any of them, then joins the resulting segments in
pairwise rounds. Thus a leaf containing `n` inner segments has `O(log n)`
concatenation depth instead of the old left-deep fold. The inner segments are
not inspected or rebalanced, so their own representation and deferred holes
remain intact. Invalid visible items become deferred failing list holes rather
than failing construction of the entire flattened list; observing a valid
suffix therefore need not observe an invalid prefix.

This is deliberately not an eager logical-list walk. A source `Concat` visits
neither child, a source thunk is not forced, and a promised outer source
suspends only the durable source-demand owner. Byte storage in the *outer*
list denotes numeric items rather than inner lists, so each such item becomes
the same deferred mismatch as another visible non-list item.

W6D.4b completion record, 2026-09-17: saturated `list.concat` calls now install
a durable source-demand owner and use the persistent-list module's one-node
flat-map primitive. Deterministic representation fixtures prove that outer
concat children are untouched and that 257 strict singleton segments retain
order with at most `ceil(log2(257))` concat levels. Evaluator fixtures prove
right-to-left suffix observation across an erroring deferred prefix, delayed
failure for an invalid strict prefix item, and exact resumption after a
promised outer source resolves. The old synchronous concat implementation is
removed; `CollectionsAndPatterns` falls from 22 to 21 declarations, W6D.4
retains two, and the D.2c manifest falls from 104 to 103.

W6D.4c-W6D.4d completion record, 2026-09-17: saturated `text.lines` calls now
install a durable owner which distinguishes compact binary input from logical
list input after one source demand. Logical lists advance through the shared
front machine, and each exposed item has its own retained WHNF computation;
neither list-chunk nor item suspension replays completed prefix bytes. Only
the final byte buffer and produced line values are observed under regional
access. Deterministic promised-item and promised-tail fixtures cover both
suspension sites, including a demand counter proving exact prefix reuse.
Compact binary slicing continues to preserve empty and trailing lines. With
all suspendable list leaves moved, immediate append construction is inlined
into the parent dispatcher and the obsolete list-family module is removed.
`CollectionsAndPatterns` falls from 21 to 19 declarations, W6D.4 reaches zero,
and the D.2c manifest falls from 103 to 101.

W6D.5b.1 completion record, 2026-09-17: list shape, emptiness, uncons, and
unsnoc pattern builtins now install one durable pattern-list owner after
saturation. The owner demands only the outer source, handles compact binaries
directly, and delegates logical lists to the shared front/back machines.
Result effects and decomposition dictionaries are constructed and rooted
within one regional access; no raw list item crosses a poll boundary. Existing
pass/fail, compact-remainder, empty-list, forcing-failure, and right-to-left
prefix-avoidance fixtures continue to pass. A new forced promised-suffix
fixture proves unsnoc resumes at the exact tail without observing an erroring
prefix. The four synchronous structural helpers are removed; the one
context-free item conversion used by literal binary/list equality remains
explicitly assigned to W6D.5b.2/W6D.5a. `CollectionsAndPatterns` falls from 19
to 15 declarations, W6D.5b retains one, and D.2c falls from 101 to 97.

W6D.5a.1 completion record, 2026-09-17: directional path equality now owns
the complete expected-path conversion before demanding its subject. The
shared key and key-list machines expose one internal optional result: ordinary
dictionary/path clients translate an unkeyable value into their established
evaluation error, while pattern subjects translate it into `.fail`. Nested
lists and dictionaries therefore retain one recursive converter rather than
gaining a pattern-only walk. Binary subjects still compare as numeric-key
paths, and a non-list subject mismatches without masking forcing failure. A
deterministic promised-item fixture proves that suspension in the subject path
does not replay the already-completed expected path. The old four synchronous
path/key helpers are removed. `CollectionsAndPatterns` falls from 15 to 12,
W6D.5a from ten declarations to seven, and retiring the last production
`pop_list_front_in` caller also moves W6A.0d from two declarations to one.

W6D.5a.2 completion record, 2026-09-17: dictionary kind and logical-emptiness
patterns now install one durable predicate owner. Kind checking demands only
the outer value. Emptiness delegates the entire value to the comparison
family's existing iterative semantic-undefined machine, so nested dictionary
members retain exact resumable progress without a second recursion policy.
An ordered two-member fixture suspends on the second promised value and proves
the first completed member is not replayed. The synchronous kind and emptiness
entry points are removed; `CollectionsAndPatterns` falls from 12 to 10 and
W6D.5a from seven declarations to five.

W6D.5a.3 completion record, 2026-09-17: required and optional dictionary
extraction now share one iterative owner. It converts the compiler path before
demanding the subject, retains every traversed parent as a runtime root, and
delegates leaf absence to the shared semantic-undefined machine. A successful
leaf rebuilds the persistent remainder from rooted parents inside one regional
access; no raw dictionary frame crosses a poll boundary. Missing paths and
undefined leaves retain required-versus-optional behavior, while a wrong-kind
intermediate remains `.fail` in both modes. Deterministic promise coverage
proves a completed lazy prefix is not replayed, and the empty-path diagnostic
is latched explicitly. The new durable pattern owner and its one regional root
publication are inventoried. The four recursive synchronous helpers are
removed, taking W6D.5a from five declarations to one; retiring the final
`eval_key_path_list_in` caller also moves W6A.4 from two declarations to one.

W6D.5a.4-W6D.5c completion record, 2026-09-17: directional atom, number,
binary, and binary-versus-list equality now share one durable pattern owner.
It demands the compiler literal once, then the subject, and advances logical
list items through the existing resumable front owner. Each exposed item has
its own retained WHNF computation, so suspension cannot replay the literal or
an accepted byte; an extra list item mismatches without demanding that item,
preserving the old directional forcing boundary. Unsupported compiler
literals are still diagnosed only after subject demand. Result effects are
published beneath matching regional access. The final context-free item and
effect constructors, synchronous dispatcher, and obsolete pattern module are
removed together. `CollectionsAndPatterns` and all W6D checkpoint groups
reach zero, while the D.2c manifest falls from 86 to 80 declarations. The
WHNF census records the explicit equality collection loop and the removal of
four obsolete key-conversion sites.

Preserve `.fail` mismatch semantics separately from permanent evaluation
failure and preserve optional-dictionary-key behavior. Force lazy dictionary
members, lazy list chunks on both sides of a split, refutable remainder
patterns, and view/predicate application. Run focused dict/list/pattern suites
and syntax-backed pattern samples ordinarily and aggressively. Each
collection checkpoint must retain sharing/root-registration observations and
show its exact checkpoint delta before proceeding.

#### W6E — Annotations and effect values

| Checkpoint | Live declarations and current shape | Target and delta |
|---|---:|---|
| **W6E.1-W6E.4 — Complete (2026-09-17): Durable annotations** | 15 S, 3 F | Convert recognition, assertions, array/deque/binary traversal, pure and reflection metadata, diagnostic contexts, and reflection deferral as one durable owner. The annotation dispatcher and recognition could not be separated without losing recognition progress. `-18`, plus W6C.2's final `-1` assertion leaf. |
| **W6E.5 — Complete (2026-09-17): Effect dispatch and fixpoint** | 4 S | Effect application, call preparation, and fixpoint construction now share one durable owner. The effect-map dispatcher remains assigned to W6E.6; the checkpoint removes two effect declarations plus the final synchronous key converter, and closes W6A.4. |
| **W6E.6 — Complete (2026-09-17): Effect map** | 3 S, 2 F | Convert map construction, the suspendable run step, continuation, and API-call construction; then close W6A.1b by access-qualifying the shared effect constructor. `-5`, plus closure delta `-1`. |
| **W6E.7 — Complete (2026-09-17): List-effect API** | 1 F | Access-qualify the cached list-effect API construction. `-1`. |
| **W6E.8 — Complete (2026-09-17): List-effect control** | 6 S, 1 S closure | Convert alt/cut/seq/flat-map recipe construction without changing branch order, remove the now-redundant run-list wrapper, and keep callback application in the W3/W6A.2 owner. `-7`. |
| **W6E.9 — Complete (2026-09-17): List-effect source** | 3 S | Convert family dispatch and fix/lazy-source handoffs to the existing W3 owner. `-3`. |

The W3 lazy list-effect recipes and source owner are already complete and are
not reimplemented here. Preserve sealed metadata, pure versus reflection
metadata updates, and abandoned-branch behavior. Force suspension during
container extraction, metadata input and output traversal, effect-map
continuation, and list-effect fix/flat-map. Run annotation, metadata, effect,
list-effect, reflection-reservation, and backtracking suites in both GC modes.

W6E.1-W6E.4 completion record, 2026-09-17: saturated annotations now install
one durable owner because selecting the annotation and interpreting its branch
are one resumable computation. The owner retains assertion payload/name/value
progress, walks array/deque/binary and metadata input lists through the shared
front machine, and gives each demanded item its own WHNF computation. Pure and
reflection metadata updates retain sealed-carrier arity and lazy projection;
reflection gates and metadata reflection tasks are only packaged beneath
regional access and still activate outside it. Error-message and annotation
selection failures retain their existing evaluator context frames, while a
successful context annotation still leaves its context undemanded.
Deterministic promised-byte and promised-carrier fixtures prove completed
prefixes are not replayed. The obsolete annotation and assertion modules are
removed, taking W6E.1-W6E.4 from eighteen declarations to zero and closing
W6C.2's last assertion compatibility leaf. `AnnotationsAndEffects` falls from
36 to 18 declarations and the complete D.2c manifest from 80 to 61. The root
publication and durable-owner inventories explicitly account for the new
annotation machine.

W6E.5 completion record, 2026-09-17: saturated effect application, effect API
calls, and function fixpoints now install one durable owner. Effect calls
retain converted names and completed argument prefixes, traverse lazy argument
spines through `ListFrontMachine`, and do not inspect the API until the spine
is complete. Applications delegate to the ordinary WHNF application owner;
fixpoints demand and validate their function exactly once before publishing
the computed knot. Deterministic promise fixtures cover all three operations,
including a promised list tail which proves API lookup remains deferred. The
old fixpoint and effect-call/effect-apply paths are removed, while effect-map
dispatch is reassigned intact to W6E.6. W6A.4 closes with the recursive key
helpers removed; W6A.0c retains only its tagged-value pair and W6A.0d retains
the list-to-value/thunk pair for W6E.6 and W6F.2. `AnnotationsAndEffects`
falls from eighteen to sixteen declarations, `ValueDemand` from eleven to ten,
and the complete D.2c manifest from sixty-one to fifty-eight. The durable
owner, root-publication, access, and WHNF inventories account for the new
effect machine. The type-erased effect-task pump's deterministic completion
allowance rises from 4,096 to 8,192 scheduler units because the new owner
surfaces its phase handoffs instead of recursively consuming them.

W6E.6 completion record, 2026-09-17: effect-map construction, one-step run,
and continuation now share the durable effect owner. Run demand retains the
exact items and results WHNF computations, then delegates its logical list
front to `ListFrontMachine`; no recursive evaluator or direct thunk-forcing
callback remains. A non-empty front constructs the mapped lazy application,
continuation, and `.seq` API call as one regionally rooted graph, avoiding
intermediate runtime roots. The empty case similarly defers `.r` lookup until
the front is known. A promised-tail fixture deterministically proves that API
lookup remains untouched while the list front is blocked and resumes exactly
after assignment; the source-backed effect-map order fixture continues to
prove left-to-right results. The obsolete effect-family module is removed.
Every shared `effect_value` caller now supplies regional access, closing
W6A.1b. `AnnotationsAndEffects` falls from sixteen declarations to eleven,
`ApplicationAndSequence` from six to five, and the complete D.2c manifest from
fifty-eight to fifty-two. The WHNF census removes the old two direct demands
and synchronous callable application, while root, owner, access, and regional
constructor inventories account for the durable map phases and their bounded
publications.

W6E.7 completion record, 2026-09-17: the list-effect API dictionary is now an
access-qualified immediate leaf. Its source owner constructs and roots the API
within one existing evaluator access region before installing the application
checkpoint; no context-free semantic value constructor remains. The D.2c
manifest falls from fifty-two to fifty-one declarations and
`AnnotationsAndEffects` from eleven to ten.

W6E.8 completion record, 2026-09-17: list-effect run, sequence, alternative,
and cut construction now create only explicit W3 source recipes. The family
dispatcher preserves an immediate outer list shell, alternatives retain
left-before-right `List::concat` order, and sequence retains its continuation
application inside the source owner's ordinary lazy application path. The six
synchronous control helpers and redundant run-list wrapper are removed, so the
D.2c manifest falls from fifty-one to forty-four declarations and
`AnnotationsAndEffects` from ten to three. Existing promised sequence/cut
fixtures continue to force the actual W3 traversal boundaries.

W6E.9 completion record, 2026-09-17: saturated list-effect construction is
now part of the generic builtin dispatcher and the obsolete family module is
removed. Fix construction emits a traced `FixFunction` source recipe without
demanding its function. The W3 source owner then retains one resumable WHNF
computation, constructs the promise and ordinary lazy application only after
the function reaches WHNF, and transitions into its existing fix-result
frontier. This deliberately moves non-callable failure to observation of the
lazy result list, matching the other list-effect recipes' demand boundary.
A forced promised-function fixture proves construction is non-demanding,
observation suspends at the function, and resumption does not replay the
completed lazy prefix. The remaining three synchronous declarations disappear,
so the D.2c manifest falls from forty-four to forty-one declarations and the
`AnnotationsAndEffects` family reaches zero. The exact root, durable-owner,
raw-value, access, and WHNF inventories account for the new source phase.

#### W6F — Objects and interaction-net builtins

| Checkpoint | Live declarations and current shape | Target and delta |
|---|---:|---|
| **W6F.1 — Complete (2026-09-17): Object leaves** | 4 F | Access-qualify default definitions and specification constructors/projections. `-4`. |
| **W6F.2 — Complete (2026-09-17): Object specification** | 4 S | Convert diagnostics, local-name selection, spec selection, and spec dictionary validation. `-4`; then close W6A.0d for another `-2`. |
| **W6F.3a — Complete (2026-09-17): Object composition applications** | 2 S | Convert composed and extended definitions through one durable application/WHNF phase owner. `-2`. |
| **W6F.3b — Complete (2026-09-17): Recursive object override** | 2 S | Convert overriding definitions and recursive dictionary override through a rooted persistent-dictionary frame stack. `-2`. |
| **W6F.4a — Complete (2026-09-17): Object instance construction** | 2 S | Move specification and parts-based instance construction into the durable object builtin owner without demanding specification fields. `-2`. |
| **W6F.4b — Complete (2026-09-17): Object definition adapters** | Preparatory | Convert default/dictionary definitions with explicit ordered demand; the shared synchronous dispatcher remains until its final `object_from_dict` branch moves. No standalone declaration delta. |
| **W6F.4c — Complete (2026-09-17): Plain-dictionary conversion, dispatch, and spec projection** | 3 S | Convert `object_from_dict`, retire the synchronous object family dispatcher, and access-qualify the object-fixpoint specification-member projection. `-3`. |
| **W6F.4d.1 — Complete (2026-09-17): Reflection application bridge** | Preparatory | Move initial effect, continuation, and fused-continuation application onto child `WhnfComputation` state owned by the non-cloned decode lane; preserve application/request diagnostic stages and forced no-replay coverage. |
| **W6F.4d.2 — Complete (2026-09-17): Regional function construction** | 1 S | Inline the access-qualified function-capture construction leaf into the interaction-net operator and remove the synchronous helper. `-1`. |
| **W6F.4d.3 — Complete (2026-09-17): Application compatibility retirement** | 4 S closure | Migrate direct evaluator tests onto ordinary lazy application demand and delete the four remaining synchronous application helpers. `-4`. |
| **W6F.4d.4 — Complete (2026-09-17): Tagged-dictionary closure** | 2 S closure | Close W6A.0c by moving the remaining test coverage onto the durable tagged-payload/semantic-undefined machines and deleting the recursive synchronous compatibility pair. `-2`. |
| **W6F.5 — Complete (2026-09-17): Net dispatch** | 2 S | Convert interaction-net dispatch and `net_arity`. `-2`. |
| **W6F.6 — Complete (2026-09-18): Net-construction lifecycle** | 2 S, 1 D | Convert machine construction, polling, and replay while retaining its durable journal owner. `-3`. |
| **W6F.7 — Complete (2026-09-18): Net-construction values** | 1 S, 2 F | Convert port lookup and access-qualify port/context value construction. `-3`. |

Object-fixpoint C3 traversal, referential identity validation, the mixin fold,
and the net-construction journal owner are already complete and are not
reimplemented. Force suspension during spec dependency/member demand and
during net-construction replay. Run object/C3/override and direct-style net
construction/function-binding suites in both GC modes, retaining identity,
port-family, malformed-request, and callback re-entry fixtures.

W6F.1 completion record, 2026-09-17: default definitions, dictionary-object
specification construction, parts-based specification construction, and name
projection now require the caller's regional value access. Callers open only
bounded callback-free regions around these leaves; object demand and fixpoint
construction remain outside them. The D.2c manifest falls from forty-one to
thirty-seven declarations and `Objects` from seventeen to thirteen.

W6F.2 completion record, 2026-09-17: object specification, diagnostic-object
normalization, and local-name construction now share one durable saturated-
builtin owner. Specification fields, names, and parts retain rooted progress
across ordinary WHNF suspension; local-name parts traverse their logical list
through `ListFrontMachine` rather than a synchronous whole-list walk. A forced
promised-tail fixture proves the already-demanded object name is not replayed
when traversal resumes. Existing error text and the distinction between a
missing/undefined specification, a non-dictionary specification, and a
non-object input remain unchanged. The final production consumers of
`list_to_value_items_in` and `force_list_thunk_in` disappear, closing W6A.0d;
the generic segment walker is retained only for compatibility tests. The D.2c
manifest falls from thirty-seven to thirty-one declarations: `Objects` from
thirteen to nine, `ValueDemand` from ten to nine, and
`ApplicationAndSequence` from five to four. Root-publication, durable-owner,
access, raw-value, and WHNF inventories account for the new owner and its
bounded publications. Aggressive collection also exposed that the shared
reflection-module test fixture returned a raw module graph across later
evaluation steps. The fixture now retains one explicit test-only runtime root
for the graph's lifetime; the previously failing object-reflection cases pass
under forced collection instead of relying on the old synchronous path's lack
of an intervening collection point.

W6F.3a completion record, 2026-09-17: ordinary object extension and composed
definition application now share one durable saturated-builtin owner. Object,
specification, definition, self, and intermediate callable roots survive
suspension without leaving regional access open. Composed definitions preserve
the old source order: demand `prior_defs base`, retain the lazy
`prior_stage self` result, then demand `extension_defs prior_result` before
returning the final lazy self application. The prior result therefore remains
lazy when an extension ignores it, while callable failures still surface in
their previous order. Forced promised-specification and promised-extension
fixtures prove completed object/prior-definition work is not replayed after
resumption. The D.2c manifest falls from thirty-one to twenty-nine declarations
and `Objects` from nine to seven; exact root-publication, durable-owner, access,
raw-value, and WHNF inventories account for the new owner.

W6F.3b completion record, 2026-09-17: overriding definitions now demand their
top-level updates and base in the former source order, then traverse nested
right-biased overrides with an explicit rooted persistent-dictionary frame
stack. Each frame retains its current result, update dictionary, ordered keys,
and optional prior-value WHNF computation. A child frame returns its completed
dictionary through the parent key, so arbitrary nesting uses neither Rust
recursion nor a regional value across polls. A forced promised nested-prior
fixture proves the completed earlier key and both top-level operand demands are
preserved across suspension without replay. The D.2c manifest falls from
twenty-nine to twenty-seven declarations and `Objects` from seven to five;
the exact inventories account for the enlarged composition owner and its
bounded result publications.

W6F.4a completion record, 2026-09-17: specification-based and parts-based
object instance construction now retain their inputs in the durable object
builtin owner, then construct the ordinary computed-fixpoint lazy value within
one bounded access. Construction copies but does not demand name, dependency,
definition, or complete specification fields; the existing object-fixpoint
machine remains the sole owner of their later evaluation. The D.2c manifest
falls from twenty-seven to twenty-five declarations and `Objects` from five to
three. Exact publication and durable-owner inventories account for the added
instance fields and one shared rooted result constructor.

W6F.4b completion record, 2026-09-17: default definitions and dictionary
definitions now use the durable object builtin owner. Default definitions
demand and return only their base. Dictionary definitions preserve the old
base-before-dictionary order, retain the completed base root while the
dictionary suspends, and publish the merged persistent dictionary within one
bounded access. A forced promised-dictionary fixture proves the earlier base
demand is not replayed. This preparatory checkpoint removes no separately
inventoried declaration; the exact publication, access, durable-owner, and
WHNF inventories record the migrated inline dispatcher work. The synchronous
family dispatcher remains solely for `object_from_dict` and retires in W6F.4c.

W6F.4c completion record, 2026-09-17: plain-dictionary conversion now uses
the durable object builtin owner. It demands the input dictionary first,
retains that completed root while an optional `spec` reaches WHNF, and accepts
only a missing or semantically undefined specification before constructing the
ordinary object fixpoint. A forced promised-specification fixture proves that
resumption neither replays the input dictionary nor loses its payload. The
former error text for non-dictionaries and already-object dictionaries remains
unchanged. The synchronous object dispatcher and its implementation module are
removed. Object-fixpoint member projection now accepts a closed default-policy
enum, publishes either a value root or a failure root, and no longer exposes a
raw `Value` callback in its signature. The complete D.2c manifest falls from
twenty-five to twenty-two declarations and its `Objects` family from three to
zero. Exact root-publication, durable-owner, access, raw-value, and WHNF
inventories account for the new phases and the retired synchronous demands.

W6F.4d.1 completion record, 2026-09-17: reflection's initial effect-function,
ordinary continuation, and fused-continuation applications now retain one
child `WhnfComputation` in the existing non-cloned decode lane. Retryable
branch snapshots continue to contain only roots; no evaluator continuation is
made cloneable. Forced application-boundary fixtures count checkpoint starts
instead of incidental lazy identities and prove that suspension/resumption
does not reconstruct the first application or nested reflection task. To
preserve structured diagnostics without matching error text, terminal WHNF
failure now publishes its final regional checkpoint before retirement: an
unconsumed application frame identifies the `application` stage, while a
consumed frame identifies failure while forcing the produced `request`. The
reflection-only synchronous application wrapper is removed, reducing the
reviewed D.2f raw-value manifest from twenty-six to twenty-five declarations.
The four application and two tagged-dictionary compatibility helpers are
temporarily annotated with their explicit W6F.4d.3/W6F.4d.4 retirement
checkpoints so intermediate library targets remain warning-free. The exact
forced suspension and diagnostic-stage fixtures pass under
`aggressive-gc-verification`. The broader aggressive reflection suite remains
blocked by an independently reproduced baseline fixture defect: several old
fixtures retain unrooted raw promised values across mutator regions. That
pre-existing fixture-liveness debt is not part of this reflection application
change.

W6F.4d.2 completion record, 2026-09-17: saturated function-capture operators
now validate their final capture arity and construct the function stage within
their existing `EvaluationValueAccess` region. The redundant regional
`instantiate_function` helper is removed. This changes no suspendable D.2c
coordinator: it reduces the complete raw-value API inventory from 455 to 454
declarations and its regional-access class from 140 to 139. Focused captured-
function tests pass ordinarily and under `aggressive-gc-verification`, and the
exact raw-value inventories pass.

W6F.4d.3 completion record, 2026-09-17: direct evaluator fixtures now create
the same ordinary `LazySource::Application` consumed by the durable WHNF
application owner. Tests explicitly demand that value when they need an
application result or failure; application construction itself no longer
pretends to report non-callability, arity-demand failure, or another semantic
result. The four recursive synchronous helpers (`apply_value_in`,
`apply_values_in`, `apply_function_values_in`, and `apply_dict_value_in`) are
removed, closing W6A.2. The D.2c `ApplicationAndSequence` violation family and
its W6A.2 checkpoint reach zero. The complete raw-value inventory falls from
454 to 450 declarations and from 277 to 273 violations; the WHNF census falls
from 204 to 193 occurrences and no longer contains `ApplyValue`, `ApplyValues`,
or the compatibility `Application` work shape. Focused evaluator and reflection
suites pass, as do exact inventories and representative application fixtures
under `aggressive-gc-verification`.

W6F.4d.4 completion record, 2026-09-17: singleton-tag semantics are now tested
directly through `TaggedPayloadMachine`, including recursively undefined
ignored fields, a defined extra field, an undefined payload, and the existing
forced nested-promise resumption. The test-only `TaggedDictExt` facade and the
recursive `tagged_payload_in`/`is_semantically_undefined_in` pair are removed,
closing W6A.0c. The D.2c `ValueDemand` family falls from nine to seven and its
W6A.0c checkpoint reaches zero. The complete raw-value inventory falls from
450 to 448 declarations and from 273 to 271 violations; the WHNF census falls
from 193 to 189 occurrences. Focused tag-recognition tests pass ordinarily and
under `aggressive-gc-verification`, with exact inventories updated to the
durable owner.

W6F.5 completion record, 2026-09-17: `interaction_net` and `net_arity` now
enter the ordinary durable saturated-builtin owner instead of evaluating in
the synchronous dispatcher. Interaction-net dispatch publishes the existing
net-construction lazy source without interpreting its effect; W6F.6 retains
exclusive ownership of that callback-driven lifecycle. Net arity preserves
its former source order and diagnostic boundary: it demands and validates the
arity with `eval:{op:'net_arity}` context, retains the completed index while
the net reaches WHNF, and adds no such context to a failure from the net
operand. Forced promise fixtures prove the net is not demanded before the
arity and that suspension at the net does not replay the completed arity.
The two synchronous net-dispatch declarations are removed, so `NetBuiltins`
falls from eight to six declarations, the complete raw-value inventory from
448 to 446 declarations and from 271 to 269 violations, and the WHNF census
from 189 to 188 occurrences. Exact root-publication, durable-owner, access,
raw-value, and WHNF inventories account for the new owner and bounded result
publication; focused tests pass in ordinary and aggressive-GC modes.

W6F.6 completion record, 2026-09-18: net construction now accepts a rooted
effect handoff, reports ready, dependency, yield, and rooted-failure outcomes
explicitly, and publishes its replayed net as a runtime root. Construction
data operations retain public rooted values in the persistent journal rather
than raw core values; replay projects them only inside one admitted value
region, constructs the managed net there, and publishes the containing net
before that region closes. Retryable waits surfaced through either the
isolated search or exposed-port demand remain scheduler dependencies rather
than becoming cached construction failures. The three lifecycle declarations
leave the raw-value inventory, reducing `NetBuiltins` from six declarations to
three, the complete raw-value inventory from 446 to 443 declarations and from
269 to 266 violations, and the retryable-WHNF census by the retired blocked-
halt signature. Existing forced dependency, valid replay, backtracking, local
effect-state, memoization, malformed-result, and port-family fixtures pass in
ordinary and aggressive-GC modes; exact root-publication, durable-owner,
access, raw-value, and WHNF inventories account for the rooted handoffs.

W6F.7 completion record, 2026-09-18: a successful isolated construction
search now transitions into an explicit `Exposed` phase containing the
selected persistent journal and one rooted `WhnfComputation`. Exposed-port
demand can therefore yield or suspend without replaying the search or losing
the selected branch; only the ready rooted result is projected inside bounded
evaluator access for opaque-port validation and replay. Callback-side public
port values and evaluator-side rooted values share scalar brand/identity
validation without sharing a raw-value function boundary. The diagnostic
context frame is also constructed and published inside admitted access. The
last three `NetBuiltins` declarations leave the raw-value inventory, reducing
the complete inventory from 443 to 440 declarations and from 266 to 263
violations. The D.2c net family and W6F.7 checkpoint reach zero; the WHNF
census replaces the synchronous `eval_value_in` port demand with the explicit
user-sized search/WHNF phase. Exact opaque-family, root-publication, access,
raw-value, and WHNF inventories account for the new boundary.

#### Post-W6F review — Complete (2026-09-18)

The mandatory converted-call-graph review is recorded in
[`ResumableWhnfW6F_2026-09-18.md`](../reviews/ResumableWhnfW6F_2026-09-18.md).
W6A-W6F close without an open correctness defect. Exact raw-value, access,
WHNF, durable-owner, and root-publication inventories account for the final
dispatcher and net-construction boundaries. Current architecture docs now
describe the durable builtin owner rather than the superseded I3 dispatcher.

The temporary one-ordinary-machine-per-demand-session admission rule remains
an implementation policy, not a Glam semantic. Its replacement by explicit
pump-role policy belongs to W6G.1; measurement of the remaining performance
gap belongs separately to W6G.4. W6G is therefore treated as its own major
phase rather than as unfinished correctness work beneath the W6A-W6F
conversion.

### Phase W6G — Pump Ownership and Residual Resumable-Machine Overhead

The interim **as-built** audit after completed W6G.1 family migrations
through W6G.1f.3h and completed W6G.3 is recorded in
[`ResumableWhnfW6GInterim_2026-09-21.md`](../reviews/ResumableWhnfW6GInterim_2026-09-21.md).
It does not close W6G.1 or replace the integrated W6G.5 review; its known
transitional risks and verification gaps retain their existing checkpoint
owners.

The checkpoint numbers group related work; they do not impose implementation
order. W6G.3 completed first because its aggregate demand-checkpoint transition
was independent of scheduling and effect-driver policy, and W6G.1 then
established pump ownership. The former W6G.2 regional effect-fusion
investigation has been extracted to
[`PureEffectAccessFusion_2026-09-23.md`](PureEffectAccessFusion_2026-09-23.md)
and deferred until after Value Representation Refinement; current W5 fusion is
the reference path for the rest of this plan. W6G.4 measures residual overhead
of the implemented resumable-WHNF boundary, and W6G.5 closes the phase.
Performance measurements may not quietly decide which thread role is
authorized to claim a machine.

#### W6G.1 — Role-specific pumping and causal work ownership

The mid-phase design audit after W6G.1f.3a.1 is recorded in
[`ResumableWhnfW6G1Design_2026-09-19.md`](../reviews/ResumableWhnfW6G1Design_2026-09-19.md).
It corrects the original reflection-checkpoint model and reviews the remaining
W6G.1 ownership, retention, selection, drain, and verification boundaries.

Replace the current shared work-stealing policy with an explicit distinction
between demand roots, shared completion-source producers, and temporary poll
authority. The runtime coordinator remains the common coordination boundary,
but not every coordinated record is globally executor-visible:

- a client-evaluation record owns one foreground continuation and its wake
  state;
- a spark or reflection record owns one background continuation;
- the managed lazy identity owns demand-driven partial producer checkpoints
  shared by every observer of that lazy identity;
- an autonomous reflection task owns its continuation after launch and
  fulfills an exact managed completion source rather than moving that
  continuation into the lazy;
- a canonical coordinator route owns only transient claim, block, subscriber,
  and wake state while at least one observer actively demands that lazy; and
- completion subscriptions connect those role-specific roots to canonical
  producers without transferring either continuation into the lazy.

A thread owns only one detached poll claim for one bounded quantum. Thread
role decides which root work that thread may locate. A canonical producer is
not permanently affinitized to whichever role first discovered it: if a
foreground and background root both demand the same producer, either
authorized causal traversal may win its exclusive claim and both benefit from
the same partial source evaluation and terminal cache.

Do not share every `WhnfComputation` merely because two roots currently focus
on the same `RuntimeValueRoot`. The outer continuation, source-owner state,
and failure context belong to the observing root, and an ordinary value is not
a scheduler-visible completion identity. Shared work begins at an explicit
semantic identity: a lazy producer, a promise/wait completion source, or
shared interaction-net state. Code which wants arbitrary pure computation to
be memoized must introduce such a boundary rather than relying on accidental
root or pointer identity.

The target pump policy is:

| Pump role | Claimable root work |
| --- | --- |
| foreground library-client thread | its exact client-evaluation record and the transitive exact dependency chain required to complete it |
| executor worker | ready sparks, ready reflection tasks, and the transitive exact dependency chains required by those background roots |
| explicit session/runtime background drain | reflection tasks in its selected scope and their transitive exact dependency chains, but never sparks |
| observational readiness/quiescence query | no work merely by observing |

`Deferred` is not itself a background root. A worker reaches a deferred lazy
or promise producer only by beginning at a spark or reflection task and
following the coordinator's existing exact dependency edges. Likewise, a
foreground caller reaches it from its exact client demand. If foreground and
background roots demand the same canonical producer, either role may win its
exclusive claim and the other waits on the same result. Do not add a sticky
foreground/background field to the deferred record merely to implement this
policy. Begin with causal traversal from the selected root; optimize root or
dependency indexes later only with evidence, while preserving the same
meaning.

The current deferred-value index already prevents two simultaneous producer
machines for one `DeferredValueId`, but the resulting coordinator record owns
the machine and inherits the first discoverer's demand session, task context,
and lifecycle. Preserve canonical admission while removing both that
accidental first-discoverer ownership and the coordinator's durable ownership
of semantic producer progress.
Canonical lazy production should use a runtime-owned pure-evaluation context
and the runtime's selected default reflection profile, not the profile of the
client, spark, or reflection task which happened to inspect the lazy first.
A promise with an explicit producer task remains owned by that task; its
observers share the completion subscription and assigned value, not ownership
of the producer task. Split the present combined deferred representation where
those lifecycle rules cannot be stated honestly through one record type.

Small callback-free builtins execute within the quantum which already owns
their enclosing computation. They are not globally selectable scheduler
roots. If a later builtin genuinely needs independent worker eligibility,
introduce an explicit root-work category with its own reviewed contract rather
than inspecting opaque machine types or treating every deferred producer as
background work.

This family owns `WHNFW3R-004`. The present global selectors admit client
demands, reflection tasks, deferred producers, and sparks to executor workers;
the synchronous client driver delegates all work whenever workers exist; and
foreground fallback may claim unrelated same-session tasks. The temporary
one-ordinary-machine-per-demand-session scan constrains that shared stealing,
but does not establish ownership. Removing only the scan would therefore move
away from the target policy. Replace the selectors first, then remove the scan
and its compensating waits. `work_by_session` remains a lifecycle/reporting
index, not an execution lane.

##### W6G.1a — Baseline ownership and sharing matrix

**Inventory complete (2026-09-18).** The selector, owner, provenance, and
producer-family census is recorded in
[`ResumableWhnfW6G1Baseline_2026-09-18.md`](../reviews/ResumableWhnfW6G1Baseline_2026-09-18.md).
The existing forced-order tests cover the current publication and subscription
protocols; target-policy fixtures remain latched to the mechanism checkpoints
below so they change one behavior at a time.

Inventory every production selector and caller: executor selection,
synchronous client demand, exact dependency pumping, session draining, runtime
draining, spark polling, deferred producer admission, completion subscription,
and quiescence observation. Record which work kind it can claim today, which
record owns the resumable machine, which demand session or task profile it
inherits, and which target pump role it implements.

Distinguish consumer-local partial work from a completion source's shared
partial work. For an uncached demand-driven lazy, demonstrate that the client,
spark, and reflection continuations remain distinct while all three resolve to
one canonical lazy-owned producer checkpoint. For an autonomous-reflection
source, demonstrate instead that all observers follow one managed completion
promise and that exactly one task launches. For an ordinary rooted value with
no lazy, promise, wait, or net identity, preserve independent WHNF requests
rather than inventing scheduler memoization.

Add deterministic fixtures before changing policy which force, at minimum:

- a queued foreground client demand while an executor worker is available;
- an unclaimed foreground-only deferred dependency while a worker searches;
- a reflection task blocked through one or more exact deferred producers;
- a spark blocked through the same kind of producer chain;
- one canonical lazy demanded concurrently by a client, reflection task, and
  spark, with each possible claimant winning the producer claim;
- the first discovering client session closing while a background root still
  depends on the canonical producer;
- a client root being released while another foreground client still depends
  on the same producer;
- the last subscriber disappearing before producer completion;
- explicit background draining with both client demand and spark work present;
- terminal publication racing an exact claimant; and
- independent work in one demand session while another machine is running.

The fixtures must latch both disputed orders with barriers or channels.
Repeated parallel runs are stress evidence only. Preserve a red or explicitly
current-behavior assertion for each mismatch so the transition demonstrates
which policy changed.

##### W6G.1 execution order and migration audits

The checkpoint labels group target contracts, not source order. As of the
2026-09-21 [interim implementation review](../reviews/ResumableWhnfW6GInterim_2026-09-21.md),
W6G.1b, W6G.1e.1, W6G.1f.1, W6G.1f.2a-b, W6G.1f.3a-i, and W6G.1f.4 are
complete; W6G.3 is also complete. **W6G.1c is a cross-cutting acceptance
target, not one additional implementation step:** its lazy-owned state,
session-neutral route, and causal claim policy are now covered by steps 1-4. The
[remaining-work review](../reviews/ResumableWhnfW6GRemainingPlan_2026-09-21.md)
records the source-backed drift and checkpoint-size judgments. The migration
dependency order, including completed prerequisites, is:

1. **W6G.1f.3i.0-.4 — complete:** closed the state-bearing lazy-route `Whnf`
   producer-family inventory.
2. **W6G.1f.2b — complete:** split lazy routes from task-owned promise
   producers and removed first-observer session affinity and final-subscriber
   route retention.
3. **W6G.1f.4 — complete:** verified retention, collection, and mixed-observer
   sharing on the new route lifecycle.
4. **W6G.1e.2 and e.3b complete:** background selection begins at registered
   roots and follows exact descendants. Foreground demand claims exact work
   first, then published launch-parent descendants; unrelated same-session
   work is no longer a fallback. The forced child-launch and wait-publication
   orderings are covered by e.3b.2 fixtures.
5. **W6G.1d, W6G.1e.3a, and W6G.1g complete:** the private blocking
   foreground driver claims only exact producers and launched causal children;
   stable absence returns a retryable halt rather than helping unrelated
   background work. The drain-authority evidence gate now identifies the
   configured-logger ingress as an external input, not an exact task edge;
   session drain now begins only at owned reflection roots and follows exact
   producer chains across sessions. The public bounded runtime-background
   pump reports runnable, contested, or stable work without waiting; the
   `pump_until_stable` convenience composes it with activity waits and spark
   abandonment. The
   private timed generation wait is available for later host composition, but
   no public incremental handle or productive-wait policy was introduced.
6. **W6G.1h (including W6G.1e.3c) complete:** removed the temporary
   session-wide serialization scan and only its compensating admission,
   busy/wait, drain, and readiness checks after causal ownership became
   authoritative. Exact claims, causal waits, terminal publication, and
   lifecycle indexes remain. The forced two-worker schedule proves that two
   independent machines in one demand session may now execute concurrently.

W6G.1 and W6G.3 are complete. W6G.2 has been extracted and deliberately
deferred; W6G.4 measurement is the next actionable checkpoint, followed by
W6G.5 closure.

W6G.1c is complete because steps 1-4 established its session-neutral and
causal-claim contract. This ordering is a dependency guide, not permission to
mark a broad checkpoint complete merely because its earlier representation
work has landed. Revisit this execution-order summary after each W6G.1
checkpoint; completed substeps, changed prerequisites, and the next actionable
checkpoint belong here rather than only in the detailed section below.

Maintain two source-backed audits during the transition:

- a **cross-registry wake audit** enumerating every completion publication,
  subscription, cancellation, session close, settlement, and readiness path
  which currently assumes all work IDs resolve through one `work` map; and
- an **implicit reflection-child audit** enumerating each task launch which is
  currently made reachable only by the foreground/session same-session
  fallback rather than by an exact dependency or explicit background root.

The first audit must fail closed while `ClientDemand` is removed from
`WorkKind`. The second may remain an inventory until the managed producer
transition is complete, but it must be closed before the fallback is retired.
Do not reintroduce causal-first producer selection between these steps: the
current first-observer-owned producer record makes that ordering unsound.

##### W6G.1b — Separate demand-root and producer registries

**Complete (2026-09-19).** Foreground evaluations now live in a dedicated
`client_demands` registry and session index under the existing coordinator
mutex. `WorkKind` contains only executor-visible background and producer
records. Completion wake routing resolves the runtime-global work ID against
the foreground registry before the background registry; session closure,
readiness/deadlock snapshots, forced settlement, generation accounting, and
debug inventory all deliberately combine the two where lifecycle semantics
require it. The client ready queue is private to exact client claims (plus a
test-only compatibility driver) and is never an executor selection surface.

`coordinator::registry_inventory` is the fail-closed cross-registry audit. It
locks the `WorkKind` boundary and the wake, close, settlement, and readiness
sites which must account for the foreground map. The temporary shared
`EvaluationWorkId` domain avoids tagging completion registrations while IDs
remain runtime-global and unique.

The first complete-suite run after the split also reproduced the already
planned first-observer ownership defect in
`configured_bare_cli_rewrites_and_executes_in_the_prepared_session`: both
`conf.log` and `asm.result` remained blocked and settlement killed their
deferred producer. The exact test passes in isolation, so repetition is not
accepted as a repair. W6G.1f.2 must add a forced first-session-close schedule
for this path, and W6G.1f/W6G.1c must remove the producer's session affinity
before this phase can claim a green complete-suite boundary.

Refactor the coordinator topology so role-specific root records and shared
producer records are distinct even if they initially remain beneath one state
mutex and one mutation-admission boundary. The target logical shape is:

```text
client evaluations
  ClientEvaluationId -> foreground continuation + wake state

background roots
  SparkId / ReflectionTaskId -> background continuation

managed lazy identities
  LazyId -> source | partial producer checkpoint | terminal result

active producer routes
  LazyId -> exclusive claim + block/subscriber/wake state

subscriptions
  completion source -> waiting root record + subscription epoch
```

Retire `WorkKind::ClientDemand` from globally selectable work. A client-facing
handle retains only the client-evaluation identity, matching-runtime
capability or lease, and a condition-variable/future wake endpoint. The client
registry owns the `WhnfComputation` between polls so a wait, handle move, or
temporary loss of the calling thread does not lose progress. A spark and a
reflection task continue to own their own outer machines in their respective
background records. None of these roots or coordinator routes embeds or copies
the canonical lazy producer's partial checkpoint.

Keep wake registration weak or otherwise acyclic. A published completion must
address either a client-evaluation record or a background work record together
with the subscription epoch, make that root ready, and signal its appropriate
waker without placing a client record on an executor queue.

##### W6G.1c — Session-neutral canonical lazy production

Move canonical lazy-producer lifecycle out of the first observer's demand
session. The managed lazy cell, not its first coordinator record, becomes the
authoritative owner of its original source, partial producer checkpoint, or
terminal result. Producer-route admission remains atomic by
`DeferredValueId`: racing observers may construct transient route candidates,
but exactly one active route becomes authoritative and every loser subscribes
to it. Yields, dependency waits, and removal of the final active route must
leave the partial checkpoint installed in the lazy.

Claim eligibility is causal rather than stored on the producer. A foreground
client may claim the producer only after reaching it through its exact
dependency chain. A worker may claim the same producer only after reaching it
from a spark or reflection root. If one role already owns the claim, the other
parks without rebuilding or restarting the producer. A claim temporarily
retains the exact managed lazy root supplied by causal demand, polls the
checkpoint in place, and returns that root before the route can retire. Once
the producer caches its lazy result, wake every subscribed root; each root
resumes its own outer continuation.

Audit context construction at admission. Pure lazy production must not inherit
the first observer's close policy or reflection profile. Promise observation
must retain the declared promise-producer lifecycle, and a client must not
become the owner of a task-owned promise merely because it encountered that
promise first.

Do not implement this ownership by placing the existing
`WhnfComputation`/`ManagedWhnfRoot` inside the managed lazy. A managed object
must contain traced `Gc` edges or inline traced state, never a registered root.
Introduce an edge-owned producer-checkpoint form over the existing managed
WHNF state, and retain the rooted form only for durable owners outside the
managed graph. The lazy's trace must report every edge in whichever source,
checkpoint, or terminal-result state is currently authoritative.

##### W6G.1d — Foreground client evaluation lifecycle

As built after W6G.1b/e.1, the private `ClientDemandHandle` and blocking
driver already claim the exact foreground record. W6G.1e.3b.2 removed the
unrelated same-session fallback from `pump_demand`; the blocked client driver
still helps via runtime-wide work. That remaining help is a transitional
liveness mechanism, not the target exact-only client policy. Complete this
section in two checkpoints:

- **W6G.1d.1 — ownership/drop audit.** Inventory handle retirement,
  cancellation, parked subscriptions, exact wake, the retired same-session
  fallback, and the remaining runtime-wide help against current tests. Keep
  the public `Evaluation`/`try_advance` handle
  deferred; an internal representation must not imply a public drop contract.
- **W6G.1d.2 — exact-only blocking driver.** After W6G.1e.2/e.3b supply causal
  descendants and explicit child work, remove unrelated runtime help from
  ordinary client demand and confirm no same-session fallback remains. Force
  zero-/one-/many-worker schedules for exact dependency progress, plus
  order-forced owner close, cancellation, subscription-before/after-publication,
  and stable blocked reporting. This is not a worker-count cross product for
  every independent lifecycle fixture.

  **Productive-wait decision gate resolved (2026-09-22).** The blocking
  evaluator does not help unrelated runtime-background reflection work. This
  is host orchestration policy, not pure-evaluation policy. Preserve an
  internal bounded foreground advance and an opaque observed-generation wait
  boundary so a later client may compose `try_advance(budget)`, a bounded
  background-reflection pump, and a timed wait without spinning or missing a
  publication. A timeout applies only while awaiting another owner's work;
  it does not interrupt an active reflection callback. W6G.1d owns the
  foreground handle and its lost-wakeup-safe private wait, while W6G.1g owns
  the runtime-background pump/drain surface. Do not expose a public
  incremental handle or select its drop policy in this checkpoint.

**W6G.1d.1 complete (2026-09-22).** The current internal ownership and wake
audit is:

| Boundary | Current owner and transition | Required d.2 treatment |
| --- | --- | --- |
| Foreground root | `ClientDemandHandle` owns one weak coordinator route and a result cell; dropping it explicitly abandons its coordinator record. The record owns the operation and its subscription until release or retirement. | Keep the handle private; do not turn abandonment into background eligibility or choose a public drop contract. |
| Parked exact dependency | `release_client_demand` installs `(work ID, subscription epoch)` under mutation admission before notifying waiters. `abandon_blocked_client_demand` rechecks terminal state, subscription epoch, and causal progress before retirement. | Retain this atomic subscription/recheck protocol; remove its runtime-wide readiness veto. |
| Terminal handoff | Retirement detaches the coordinator record before publishing the result cell, outside locks. `ClientDemandHandle::wait` closes only this narrow publication gap. | Continue to wait on the cell when a snapshot disappears; never treat detachment alone as completion. |
| Foreground execution | `drive_client_demand` claims its exact client record, then an exact producer. Its blocked path still calls `poll_runtime_work`, abandons sparks, and treats any runtime progress as a reason to wait. | Replace that broad path with the already-indexed causal child/producer probe, then return a retryable halt when no causal route can progress. |
| Change wait | `coordinator.wait_for_change` uses a generation and condition variable, with no timeout. Work and completion publication advance the generation and notify. | Preserve generation-before-wait recheck; add a private timed form for later host composition, without putting productive-wait policy in the evaluator. |

Existing forced fixtures cover result-cell publication after unlock, the
detach/publication gap, parked subscription and promise wake, owner close and
kill, and another client continuing to demand a shared producer. The
former `synchronous_client_demand_waits_for_worker_owned_runtime_progress`
fixture encoded broad runtime help: its worker-owned reflection task assigned
a host promise without an exact producer edge. At d.2 it was revised to
distinguish an exact claimed producer from unrelated runtime progress, with a
red unrelated-background fixture before changing the driver. The same-session
fallback was retired by W6G.1e.3b.2; session-wide draining remains separately
owned by W6G.1g.

**W6G.1d.2 complete (2026-09-22).** The blocking driver now claims the
foreground record, its exact producer chain, or published causal children;
it does not pump runtime-wide work, abandon unrelated sparks, or wait for an
unrelated worker. Stable-block retirement rechecks these same causal routes
under mutation admission, rather than vetoing retirement on broad runtime
activity. A private generation-predicate wait supports an optional idle
timeout, with the predicate checked under the publication mutex; it is not a
timeout for a running callback. This checkpoint leaves the eventual public
bounded `try_advance` and background-pump composition to their designated
phases, rather than implying that the blocking loop itself is a bounded API.
The order-forced verification now distinguishes: unrelated queued work and
worker-owned work that must not hold the client; a claimed exact lazy producer
with no workers; task-owned promise producers with one and four workers;
publication before the timed wait, publication after its locked predicate,
and timeout without change. Existing fixtures continue to cover owner close,
kill, cancellation, and both lazy-subscription publication orders.

The client-only registry is in place. The deferred
[public resumable-evaluation plan](PublicResumableEvaluation_2026-09-23.md)
owns the incremental `Evaluation` facade: it will split bounded
`try_advance` from this private blocking driver without moving the computation
back into the caller.
`ValueEvaluator::eval` currently remains the blocking convenience over the
same foreground record, not a second evaluator path.

The later bounded advance should claim and poll only the matching
client-evaluation record and its transitive exact dependencies. It should
return a terminal value or failure, budget exhaustion, or an exact parked
dependency; the last case registers a wake before reporting that no local
progress is available. The client record remains affine with respect to
polling even if its handle moves between threads.

Select the public-handle drop contract before exposing the incremental API.
At minimum, preserve distinct internal operations for:

- canceling the client root and removing its subscriptions;
- retaining an inactive handle/record for later client resumption; and
- explicitly transferring the root continuation to background ownership when
  callers want evaluation to continue without the client.

Ordinary handle destruction must not silently make arbitrary client work
worker-eligible. Regardless of the selected default, releasing one client root
removes only that root's subscriptions. A canonical producer which is still
demanded by a spark, reflection task, or another client remains live and keeps
its partial progress.

##### W6G.1e — Role-specific root selection

Partition this transition so foreground ownership changes independently from
deferred causal traversal:

- **W6G.1e.1 — foreground selector exclusion — Complete (2026-09-18).**
  Executor workers and the runtime background pump no longer select
  `ClientDemand`. The blocking client driver always claims its exact record,
  including when workers exist. A forced coordinator fixture proves that both
  background selectors reject a queued foreground record while the exact
  client claim remains live. The temporary same-session admission guard and
  the combined client registry remain transitional.
- **W6G.1e.2 — causal background traversal — Complete (2026-09-22).**
  Implementation probes established W6G.1c/W6G.1f as prerequisites: earlier
  selector experiments stranded a canonical lazy in the four-worker executable
  while producers inherited their first session. After lazy checkpoint
  ownership and session-neutral routes were installed, the selectors retired
  global deferred-ready eligibility together. The forced matrix below now
  covers foreground-only, spark-rooted, and reflection-rooted paths.
  - **W6G.1e.2a — route inventory and dry-run traversal — Complete
    (2026-09-22).** After W6G.1f.2b
    and before the e.2b selector cutover, enumerate background-root entry
    points, exact dependency edges, yield/requeue
    behavior, and every current global-ready fallback. Build the traversal
    helper and forced fixtures without changing production selection yet.
  - **W6G.1e.2b — selector cutover — Complete (2026-09-22).** Switch worker
    and runtime background selectors together to spark/reflection roots and
    their exact descendants; retire global deferred-ready eligibility in the
    same coherent change.
    A busy descendant remains a wait, and a yielded one stays rediscoverable
    from its root on a later pass. Preserve root fairness without making a
    descendant an independent background root.
  - **W6G.1e.2c — forced policy matrix — Complete (2026-09-22).** Latch
    foreground-only deferred work against worker search, spark- and
    reflection-rooted chains, contested claims, zero-/one-/many-worker
    execution, and a runtime pump which excludes sparks. Count claims to
    distinguish causal traversal from accidental global selection.

  **e.2a source-backed route inventory.** `ReflectionWork` becomes an
  autonomous root only at activation; `.task.new` records a launch parent but
  remains independently runnable. `SparkWork` is admitted as a best-effort
  worker root when workers exist. A queued root runs before following its
  prior block; a blocked root follows `WorkDependency::producer_wait` through
  `promise_by_wait`, deferred `by_wait`, or reflection `by_wait`. A resolver
  promise, missing producer, running producer, terminalizing producer, or
  repeated work ID ends that traversal without another claim. A dormant
  deferred/lazy-route producer is claimable only through such an exact path.
  Reflection yield requeues its root; spark yield requeues its root; deferred
  and lazy-route yield either requeue under live exact demand or become dormant
  until a later exact claim. The new read-only traversal helper is exercised
  against an unrooted promoted producer, a spark-rooted producer, and a
  cross-session reflection-rooted producer while the old selector is still
  authoritative.

  Before e.2b, the production global-ready fallback was `claim_ready_task` in
  both `select_worker` and `select_runtime_pump`; the latter excluded sparks.
  The test compatibility and session-drain selectors still call
  `claim_ready_task` but are outside this cutover. Previously,
  `runtime_pump_snapshot_locked` also reported every queued non-spark task as
  `background_ready`; e.2b narrowed that view alongside selection so a
  foreground-only deferred producer cannot spin the pump. The `ready_tasks`
  queue remains useful to cooperative and exact selectors; retiring its
  *global eligibility* does not mean deleting every queue update.

  **e.2b implementation boundary.** The coordinator keeps one ID-only
  round-robin registry of activated reflection and admitted spark roots.
  Both selectors walk the same exact producer-chain helper under mutation
  admission and the coordinator lock. Workers alternate root kinds when both
  can advance; the runtime pump considers reflection roots only. Selection
  rotates the chosen root, including when it claims a deferred/lazy-route
  descendant, so yielded descendants are rediscovered from their still-live
  root. A running descendant cannot be claimed twice. The global ready-task
  queue remains for explicit/session-cooperative claims, but no longer makes
  arbitrary deferred work worker- or runtime-pump eligible. Runtime-pump
  `background_ready` now uses the same reflection-rooted causal criterion,
  preventing a queued foreground-only deferred producer from causing a pump
  spin. The source inventory fingerprint changed only because two test
  declarations were renamed to describe the new scheduler policy; typed-edge
  counts gain one test-only promise root used to latch its lifetime across a
  forced collection.

  **e.2c verification boundary.** A forced coordinator matrix runs with zero,
  one, and four configured workers. For each eligible claimant order it holds
  a reflection-rooted deferred claim while the other selector probes, then
  requeues the unpolled claim and counts exactly one subsequent claim. An
  unrelated promoted deferred producer remains invisible to both background
  selectors throughout and remains exactly claimable by its foreground owner.
  The spark-rooted fixture separately checks that the runtime pump cannot
  claim a spark descendant. The executable worker-parity fixture exercises
  actual zero/one/four-worker execution; the coordinator matrix pins the
  selector ordering without relying on scheduler luck.
- **W6G.1e.3 — drain and admission cleanup.** Partitioned because the
  drain and serialization policies cannot be narrowed before the registry
  topology which replaces them exists:
  - **W6G.1e.3a — drain authority.** Retain the current session selector until
    W6G.1b-W6G.1f distinguish roots, shared producers, and causal child work.
    An attempted early restriction to session-owned reflection roots exposed a
    schedule where configured `conf.log` depends on reflection work hosted by
    another session. Do not choose between “claim that cross-session root” and
    “represent a different exact dependency” through queue filtering. After
    the topology migration, inventory the actual drain authority and force a
    logger-shaped external-input schedule alongside client/spark work. W6G.1g
    owns the subsequent drain cutover and production-ingress ordering test;
    e.3a is its evidence gate, not a second session-selector implementation.

    **W6G.1e.3a complete (2026-09-22).** Source-backed drain authority:

    | Path | Pre-W6G.1g authority | Cutover constraint |
    | --- | --- | --- |
    | `EffectRun::schedule_diagnostic_consumer` | Creates a coordinator-rooted logger effect in a fresh session and attaches the diagnostic ingress atomically with activation. | The logger session is an authorized root; the diagnostic input queue is not an exact wait on whichever independent task may next publish a message. |
    | `EvaluationDemandState::run_until_quiescent` | First claimed broadly ready work in its session, then an exact cross-session lazy route. It reported a live cross-session reflection-task wait as `Quiescent` without claiming that task. | W6G.1g.1 narrows selection to owned reflection roots while retaining exact cross-session producer traversal. |
    | `EvaluationRuntime::pump_until_stable` | Selects runtime-visible reflection roots and their causal descendants across sessions; it excludes foreground roots and executes no spark. | W6G.1g.2 must retain this independent-producer route through its bounded background pump and drain composition. |

    The zero-worker `logger_shaped_session_drain_leaves_independent_producer_client_and_spark_for_runtime`
    fixture forces the logger-shaped host-input wait before runtime background
    selection: session drain leaves the independent producer, foreground
    client, and spark untouched; one runtime background poll advances only the
    producer; admitting the input then lets the consumer finish. The existing
    former `live_cross_session_dependencies_are_reported_as_quiescent`
    fixture supplied the red pre-cutover behavior. W6G.1g renamed it to
    `session_drain_follows_an_exact_cross_session_dependency` and now asserts
    that the observer drain completes the exact producer without first
    draining its owner. `TestHost`'s separate `.read_log` snapshot protocol
    remains only the topology baseline, not a substitute for the production
    ingress tests recorded below.
  - **W6G.1e.3b — implicit-child audit and fallback retirement.** Exact demand
    pumping originally had a same-session reflection fallback. Direct removal
    exposed reflection/effect flows which launch causally related
    child work before publishing an exact dependency edge. Inventory those
    routes, make causal child ownership explicit, then remove the fallback;
    do not misclassify those children as arbitrary unrelated work merely to
    preserve the current scheduler heuristic.
    Split this into **e.3b.0** (source-backed launch/edge census and forced
    current-behavior fixtures), **e.3b.1** (explicit child ownership or exact
    dependency publication, with task lifecycle tests), and **e.3b.2**
    (remove same-session fallback and force both sides of child-launch and
    subscription publication). The audit may precede e.2; the removal must
    follow the reviewed child-edge mechanism.

    **Partial closure (2026-09-21; e.3b remains open).** The `refl` and
    `meta_refl` annotation family is no longer an implicit child of its first
    observer: source handoff reserves one task-owned completion promise,
    installs the lazy's exact WHNF dependency, then activates the autonomous
    runtime-background task. W6G.1f.2b.1 also restored selection of the
    runtime-default reflection profile for those annotations; `.task.new`
    still inherits its caller's profile. Profile selection is not causal
    scheduling. A committed `.task.new` launch is still a distinct route:
    `create_task` journals `ReflectionUpdate::Launch`, while a parent which
    later joins publishes its wait only when `.task.join` is polled. The
    same-session claim in `pump_demand` and its transitional coordinator
    selector remain. Do not revisit annotation profile ownership as part of
    that remaining work.

    **W6G.1e.3b.0 complete (2026-09-21).** The production launch/edge census is:

    | Route | Launch and authority | Edge before execution / remaining gap |
    | --- | --- | --- |
    | `EffectRun::schedule` (including configured `conf.log`) | `lifecycle.rs::schedule_with_capabilities` prepares a coordinator-owned reflection root in a fresh session. | Explicit background root; no parent edge required. |
    | `EffectRun::run` | `lifecycle.rs::run` drives a direct effect task in its own session. | Client-driven root rather than a coordinator record; its `.task.new` children still need lifecycle/causal accounting. |
    | `refl`/`meta_refl` annotations | `eval/value.rs::LazyTaskWork::ReflectionTask` reserves and activates through `session.rs::reserve_reflection_completion_activation`. | Managed completion promise is registered before the lazy's WHNF checkpoint publishes its exact dependency, and activation follows publication. This family is closed for e.3b. |
    | `.task.new` in an effect task | `requests.rs::create_task` reserves a task and journals `ReflectionUpdate::Launch`; `ReflectionJournal::commit_updates` activates only after commit. | The returned handle is plain effect data. A pending `.task.join` later publishes an exact wait; launch alone supplies no parent dependency. A child can be queued while its parent blocks on something else or has not reached the join. |

    Test-only `EvalContext::schedule_task` and direct coordinator reservations
    exercise these mechanisms but are not additional production launch forms.
    `reflection_task_launch_is_buffered_until_cut_commit` and
    `failed_transaction_discards_its_reflection_task_launch_and_cancellation`
    cover the transactional boundary; `reflection_task_returns_a_joinable_result`
    and `configured_logger_can_join_a_restricted_reflection_task` cover the
    eventual join. Two new zero-worker, single-quantum fixtures force the
    intervening scheduler states without relying on thread races:
    `causal_child_runs_before_unrelated_same_session_task_without_exact_wait`
    originally modeled the pre-join window and exposed the old fallback by
    running an unrelated task before a later queued child; e.3b.2 revised it
    to assert causal selection instead, while
    `published_child_wait_claims_exact_cross_session_child_before_unrelated_work`
    records that a published wait advances the exact child ahead of unrelated
    same-session work. The original fallback result was a red assertion, not a
    permanent semantic contract.

    **W6G.1e.3b.1 complete (2026-09-22).** A `.task.new` reservation captures
    its caller's stable task identity, including for directly driven
    `EffectRun::run` tasks with no coordinator record. Successful activation
    publishes that `launch_parent` in the child's coordinator record together
    with its queued state, after transaction commit. Discarded and initially
    cancelled reservations publish no parent. This is causal provenance, not
    a wait: parent completion does not join the child, and the parent identity
    remains on a live child even after its parent record retires. Deterministic
    fixtures cover direct and scheduled parents, return, failure, active and
    pre-launch cancellation, and session abandonment.

    **W6G.1e.3b.2 complete (2026-09-22).** The coordinator indexes activated
    children by launch parent and probes claimable, claimed, or absent work
    along their exact producer chains and descendants. `pump_demand` claims
    exact work first, then causal child work; it no longer uses the same-session
    fallback. The claimed/busy result participates in the foreground wait and
    generation-wake path, so `Busy` cannot become a lost wake. A claimed or
    reserved causal child reports `Busy`; an unrelated same-session task does
    not become causal work. Forced fixtures cover child activation before and
    after parent wait publication, a cross-session child claimed by another
    thread, a blocked child with a
    grandchild, a retired intermediate child with a live grandchild, and an
    external promise that remains `NoProgress` after side work completes.
    Retiring an intermediate child promotes its live descendants into the
    nearest still-indexed ancestor route; retiring a root removes only its
    own helping route. Neither promotion implies an implicit join or extends
    child ownership to its parent. Optional productive-wait policy remains
    deferred to the W6G.1d.2 decision gate.
  - **W6G.1e.3c — serialization retirement.** After W6G.1b-W6G.1f express
    root affinity and producer ownership directly, remove
    `session_has_running_machine` and its compensating busy waits. This is a
    prerequisite audit and handoff to W6G.1h, **not a separate removal**;
    W6G.1h owns the implementation and final forced matrix. Do not perform it
    as an isolated scheduler relaxation.

    **Complete (2026-09-22).** W6G.1h removed the scan and its compensating
    checks as one policy change after the causal selectors, foreground driver,
    and explicit drain authorities were in place.

Replace the generic executor `select` path with role-specific selectors. A
worker locates a ready spark or reflection root, or rediscovers the deepest
claimable exact producer beneath a blocked spark/reflection root. It must not
select `ClientDemand` or an unrelated entry from the generic deferred-ready
queue. The explicit background drain uses the same causal traversal beginning
only from reflection roots and excludes sparks. Preserve fairness between
background roots without converting dependency descendants into permanent
background records.

Keep exact claim and release authoritative. A dependency already owned by
another thread yields `Busy`/wait rather than a second evaluation. Budget
yield from a causal descendant must remain discoverable from its blocked
background root on a later selector pass; do not depend on a stack-local chain
surviving worker return. If the straightforward traversal is too expensive,
add a coordinator index of background *roots or causal routes*, not a semantic
eligibility bit on each descendant.

Do not leave worker count as an execution-policy switch for client calls.
Zero-, one-, and many-worker configurations must differ only in available
background progress and performance, not in which thread role is responsible
for the foreground root. Remove the unrelated same-session fallback from
ordinary foreground demand pumping; explicit background draining is the
opt-in path for helping work outside the client's causal chain.

##### W6G.1f — Producer retention and last-subscriber policy

Make producer retention independent of client-handle retention. Removing a
subscription cannot cancel or abandon a producer while another subscribed
root can still reach it. Conversely, an inactive coordinator route must not
keep an otherwise unreachable lazy graph rooted indefinitely merely to retain
partial progress. Preserve that progress in the managed lazy itself:

```text
LazyProducerState
  Source(LazySource)
  Checkpoint(ManagedLazyCheckpoint)

ManagedLazyCell
  producer: Mutex<Option<LazyProducerState>>
  result: OnceLock<LazyResult>
```

The exact representation may keep `Source` and `Checkpoint` as variants of the
current source field, and a separately allocated managed checkpoint should be
preferred where embedding the complete producer state would inflate every
lazy cell. Terminal publication keeps the current result-before-producer-
release ordering and lock-free terminal read opportunity.

Do not add a coordinator-calling lazy finalizer as the primary liveness
mechanism. A coordinator record which retains `ManagedLazyRoot` prevents the
lazy from becoming unreachable, so the finalizer needed to remove that record
would never run. Making such cleanup sound would first require a managed weak
reference, an ephemeron-like registry, or an externally stored trace-immediate
sidecar. Managed lazy destruction remains passive; no collector finalizer may
acquire coordinator locks, run evaluator policy, or invoke host callbacks.

The active producer route may retain one `ManagedLazyRoot` while it has at
least one subscriber or an in-flight claim. When its final subscriber leaves,
retire the route immediately if it is unclaimed. If a quantum is in flight,
latch retirement, let that claim publish its checkpoint or terminal result
back into the lazy, then remove the route and release the temporary root. A
later demand for a still-reachable lazy creates a new route around the stored
checkpoint rather than reconstructing the original computation.

This transition covers the complete `LazyTaskWork`, not only its `Whnf`
variant, but it must not force every family into the same ownership shape.
Classify each family before migration:

- access, builtin, object, list-effect, net-WHNF, net-construction, and other
  demand-driven partial work belong in traced lazy-owned checkpoints;
- a synchronous one-shot host callback needs a traced before/after checkpoint
  because no independent runtime root continues it after the route vanishes;
- a started reflection task is already an autonomous background root and must
  run to a terminal disposition even after its observing route vanishes. It
  fulfills a managed completion source owned by that task; it is not itself a
  lazy checkpoint; and
- fire-and-forget orchestration such as best-effort spark admission remains
  outside managed access. The durable builtin checkpoint records only enough
  scalar phase state to avoid accidental replay.

Inventory which fields are traced semantic state, edge-free scalar or task
identity, active task obligation, and external owner state. A managed
checkpoint must not contain a `RuntimeValueRoot`, `ManagedLazyRoot`, or other
registered root. Active autonomous work may temporarily own registered roots
for its inputs, completion mapping, or managed promise, but those roots live
in the task obligation, never inside the value graph, and must be released on
every terminal disposition. Convert rooted callback results back to managed
`Value` edges before publishing a durable checkpoint, and represent passive
external owners through reviewed edge-free handles whose semantic captures
remain visible to tracing.

In particular, never replay a host callback after its producer crossed the
`Before -> Invoking` boundary, and never create a second reflection-task
reservation merely because active demand temporarily fell to zero. A started
reflection task continues as background root work and terminally assigns its
one managed completion source. Although restarting abandoned pure work cannot
change a Glam result, those one-shot boundaries, reflection traces,
diagnostics, and host observations make ownership loss operationally
observable.

Implement this as the following checkpoints. Reorder W6G.1b-W6G.1f where a
narrower migration sequence avoids maintaining two authoritative producer
states.

###### W6G.1f.0 — Producer-state and external-boundary inventory

**Complete (2026-09-18).** The baseline review inventories all ten
`LazyTaskWork` families, registered roots, external boundaries, and replay
constraints. `lazy_producer_inventory` makes the family and one-shot-boundary
census source-backed. The inventory confirms that W6G.1f must migrate the
complete producer family rather than installing a `Whnf`-only checkpoint.

**Design correction (2026-09-19).** The baseline correctly identified
reflection reservation and activation as a one-shot boundary, but incorrectly
classified the started reflection task as lazy-checkpoint state. Reflection
tasks are autonomous background roots. W6G.1f.3b replaces the route-owned
reservation with a task-owned managed completion promise and an ordinary WHNF
handoff. Update `lazy_producer_inventory` when that implementation lands so
the source-backed classification distinguishes demand checkpoints from
autonomous completion sources.

Enumerate every `LazyTaskWork` variant, all roots and semantic edges it retains,
its callback or task activation boundaries, and whether its current state can
be reconstructed without replay. Identify the minimum concrete traced
checkpoint family and the edge-free orchestration tokens which must remain
outside managed memory. Record source-backed counts so a later producer family
cannot silently remain coordinator-owned.

###### W6G.1f.1 — Managed lazy checkpoint state

**Implementation design checkpoint (settled 2026-09-19).** `ManagedLazyCell`
is a core-managed identity, while `WhnfState` and the remaining producer-family
states are evaluator-owned. Use one evaluator-owned concrete
`ManagedLazyCheckpointCell` and expose only its narrow, field-opaque
`ManagedLazyCheckpointEdge` to `core`. The lazy stores that typed `Gc` edge;
its trace and mutation visitors report the edge, then the collector dispatches
the checkpoint's evaluator-owned trace through the allocation run metadata.

Rust needs no C-style forward declaration within one crate: the nominal module
references may be cyclic because the representation cycle is broken by the
pointer-sized `Gc`. `core` may report, duplicate under matching access,
install, and remove the opaque edge, but it must not inspect or match on the
checkpoint state. `eval` owns allocation, typed state access, mutation, and the
compile-exhaustive visitor. This incurs one managed allocation only while a
lazy has partial work; it adds no `Box`, vtable, second trace contract, or
general public erased-GC API. Revisit representation fusion only after the
complete producer-family migration and profiling.

Generalize the lazy's source/result protocol to source/checkpoint/result.
Introduce an edge-owned managed WHNF checkpoint over the existing canonical
`WhnfState`; keep `ManagedWhnfRoot` for external owners but prohibit it inside
managed cells. Route state transitions through the collector edge-transition
gateway, and make the lazy trace compile-exhaustive over source, every
checkpoint variant, and terminal success or failure.

Add focused tests proving that a checkpoint which points back to its owning
lazy is collected when the complete graph is unreachable, remains intact when
the lazy is reachable without active demand, and resumes without redoing
completed WHNF transitions.

**Complete (2026-09-19).** The managed lazy producer slot now distinguishes
its original source from one field-opaque `ManagedLazyCheckpointEdge`. The
evaluator-owned `ManagedLazyCheckpointCell` reuses the canonical `WhnfState`
visitor and transition gateway; `core` can only retain, trace, install, or
duplicate that typed edge beneath matching runtime access. The representation
adds one managed allocation only after partial work is published and adds no
box, vtable, erased-GC API, or registered root inside the managed graph.

Focused collection fixtures install a self-referential lazy/checkpoint graph.
They prove that the complete unreachable cycle is reclaimed, a rooted lazy
retains both cells across collection, and mutation of the installed canonical
state remains visible after a later collection without reconstructing the
checkpoint. Source-backed durable-owner and persistent-edge inventories now
classify the new exact traced edge. Production demand still uses the source
route; narrowly scoped dead-code allowances identify the staged methods and
are removed by W6G.1f.2 when that route begins polling the installed state.

###### W6G.1f.2 — Demand-backed producer routes

Implementation is deliberately reordered around the representation boundary:

1. **W6G.1f.2a** moves the ordinary WHNF producer family from its
   coordinator-owned `WhnfComputation` into the lazy's exact managed
   checkpoint. The coordinator retains a stateless route adapter temporarily;
   no second WHNF state may remain in that adapter.
2. **W6G.1f.3** migrates the remaining producer families. Demand-driven
   families receive managed checkpoint representations; reflection receives
   a task-owned managed completion source instead. This must precede removal
   of the lazy-producer coordinator machine because those ownership forms are
   not yet available.
3. **W6G.1f.2b** performs the coordinator cutover below once every route can
   poll state owned by its lazy. It removes the temporary adapters and owns
   the final subscriber/claim race matrix.

This is staging, not a weakened target: after W6G.1f.2a the WHNF family has
one lazy-owned authoritative state, and after W6G.1f.2b no *lazy-producer
route* has a durable machine in its coordinator record. Autonomous reflection
tasks remain ordinary background task records until they terminate; they no
longer borrow the lifetime or machine of the lazy route which launched them.

The first full-suite run after W6G.1f.3a.0 reproduced the same open route
ownership defect through `command_line_workers_override_glam_workers`:
`asm.result = seq 1 (spark 2 "ok")` remained blocked on the first
session-owned deferred wait and settlement killed that route. The checkpoint
carrier change is representation-only and the focused test passes, so neither
fact is accepted as race evidence. Keep this concrete schedule as W6G.1f.2b
verification: lazy-owned state alone does not make the coordinator route
session-neutral, and the complete suite is not a green gate until the forced
subscriber/close ordering and this configured CLI path both pass.

The full parallel verification after W6G.1f.3a.1 produced a second concrete
schedule through
`reflection::machine::tests::coordinator_terminal_policy_preserves_a_descendant_failure_before_root_return`:
the effect fixture's compilation remained blocked on its deferred wait instead
of reaching the descendant failure. Focused and forced host-checkpoint tests
pass, and this reflection fixture does not traverse a host-call checkpoint;
classify the result as another W6G.1f.2b route/session-affinity reproduction,
not as host-checkpoint evidence. The final cutover must force this ordering or
an equivalent latch-controlled schedule before either historical full-suite
failure may be considered repaired.

For W6G.1f.2a, reuse the already-managed cell owned by `WhnfComputation`
rather than walking or reallocating its state. Under one matching access,
project its typed edge while the registered root remains live, install that
edge into the lazy, then retire the superseded root. Later polls project the
same cell through the lazy. Force budget yield and dependency suspension on
both sides of this handoff, and prove terminal cache publication removes the
checkpoint while retaining the existing result-before-producer-release rule.

**W6G.1f.2a complete (2026-09-19).** Ordinary WHNF production now promotes
its seed once, projects the exact existing managed-state allocation while its
registered root remains live, publishes that typed edge into the owning lazy,
and drops the superseded root only after publication. The coordinator retains
only a state-free `WhnfCheckpoint` route marker for this family. Every later
poll reaches the same cell through the lazy; terminal success or failure uses
the existing cache-first publication path, which removes the producer
checkpoint before the route reports completion.

Forced budget-yield and promise-dependency fixtures prove that collection
between polls retains the exact focus and continuation state. A forced
cross-session fixture closes the first producer session, observes its old
wait as abandoned, then resumes the lazy-owned checkpoint from a later
same-runtime session without rerunning the source callback. Source-backed
root-publication, managed-access, persistent-edge, producer-family, and WHNF
checkpoint inventories classify the new handoff. The remaining nine source
families still retain route-owned state or orchestration and therefore remain
assigned to W6G.1f.3 before the route-machine cutover in W6G.1f.2b. One of
those families—reflection—will leave the route through an autonomous task and
managed completion source rather than adding a checkpoint variant.

Reduce the coordinator's lazy-producer record to transient claim, blocker,
subscriber, generation, and wake state. The record owns no durable producer
machine. Admission receives a temporary root from causal demand, atomically
installs or finds the route, and polls the lazy-owned checkpoint. A route with
no subscribers and no claim retires without waiting for GC or a lazy
finalizer.

Force the final subscriber to disappear before claim, during claim, after
checkpoint publication, and concurrently with a new subscriber. Verify that
exactly one route and claim remain authoritative, no registration epoch is
reused, and a background subscriber outlives closure of the first discovering
client session.

W6G.1f.2b is too large to treat as one unreviewed edit. Use these checkpoints;
repartition the mechanism after 2b.0 if the current coordinator shape makes
an intermediate state unsafe:

- **W6G.1f.2b.0 — lifecycle and representation gate.** Inventory the current
  `DeferredWork` admission, indexes, claim, release, settlement, cancellation,
  subscription, session-close, and pure-lazy-cycle paths. Decide how one
  machine-free lazy route is distinguished from a task-owned promise producer
  which must keep its task machine. Specify which task and wait IDs remain
  necessary for exact dependencies and cycle reporting. Record the complete
  route/root/claim invariant and the smallest coherent cutover boundary;
  do not silently turn every deferred producer into a lazy route.

  **Lifecycle inventory complete (2026-09-21).**
  `EvalContext::{lazy_root_task,promise_root_task}` currently join at
  `deferred_root_task`: both allocate a task ID and wait token, construct a
  boxed machine using the first demand session and its task profile, and
  reserve one `DeferredWork` keyed by producer value. The reservation race
  discards a losing candidate machine after releasing coordinator locks.
  `WorkRecord` requires a `demand_session`, and `DeferredWork` holds the task,
  wait, producer root, machine slot, block, and running-demand latch. Its
  indexes are `by_value`, `by_wait`, and `by_task`, plus the coordinator's
  `work_by_session`; ready queues and task-directed pumping also find it
  through those indexes. The lazy's managed checkpoint is now the semantic
  computation state, so the lazy's boxed machine is an orchestration shell;
  the promise follower still needs its task machine.

  Claim upgrades the first session's weak registration, refuses a closed
  session, detaches the machine, and marks the record running. Release
  reattaches the machine and either queues, dormants, blocks with an exact
  dependency/subscription epoch, or terminalizes. Terminal settlement consumes
  the deferred producer obligation once, publishes the wait terminal and
  settles any task-owned promises; retirement then removes all deferred
  indexes. An unclaimed deferred wait can also be abandoned and settled.
  Session close traverses `work_by_session` and abandons both lazy and promise
  records (or latches closure for a running claim). Explicit task cancellation
  is a reflection-task path, not a separate lazy-route cancellation API.

  Blocked deferred dependencies are traversed through wait-token and promise
  producer edges. A cycle is poisoned only when every member is a blocked
  lazy and no promise edge occurs; the current cycle path extracts a machine
  from each member before terminal settlement. `EvaluationWaitToken` itself
  records a producer task ID and owner session ID. The task ID currently
  supports task-directed claim/status/dependency lookup and reports, including
  `reported_dependency` and targeted pumping; it is not intrinsically needed
  to identify a lazy's exact completion source. Work ID identifies the
  scheduler record and cycle member, while a distinct wait token supplies
  exact dependency identity, terminal publication, and wakes. Task-owned
  promise producers still require a task ID for their machine and dynamically
  registered promise obligations.

  **Subscriber-count gap.** `CompletionSubscriptions` stores epoch-tagged
  registrations and filters stale ones at wake. Client demand explicitly
  unsubscribes, but other registrations can remain stale until source
  terminalization. Consequently `has_exact_subscriptions()`/`len()` is not an
  authoritative live-subscriber count, and neither wait-token clones nor the
  first session's lifetime can decide last-subscriber retirement. Checkpoint
  2b.3 needs an explicit active-demand/registration protocol and its own
  forced race tests.

  **Representation decision (2026-09-21).** Give the machine-free lazy route
  its own `WorkKind::LazyRoute`; retain a distinct task-owned promise-producer
  kind and its machine/settlement obligations. Reuse the coordinator work map,
  exact completion protocol, and mutation/settlement gate. A tagged arm inside
  the old `DeferredWork` would preserve accidental task/session paths; a
  separate scheduler would duplicate too much machinery. The runtime-owned
  lazy route is not a synthetic background-session task.

  **ID-use inventory (2026-09-21).**

  | Identity | Current use | Semantic disposition for lazy routes |
  | --- | --- | --- |
  | `LazyId` / `DeferredValueId` | `by_value` deduplicates admission | Keep `LazyId` as the stable semantic cell identity; re-admission of a reachable lazy may make a new route. Keep `PromiseId` for the separate promise producer. |
  | `EvaluationWorkId` | Work map, ready queue, wake epoch, cycle member, settlement | Keep one fresh work ID per admitted route. It is scheduler identity, not a task or source identity. |
  | Wait ID / `EvaluationWaitToken` | Exact dependency, terminal cell, subscriptions, `by_wait`, cross-route wake | Keep one fresh wait ID per route admission; an old terminal token stays observable after retirement. The token does **not** need a task ID to identify completion. |
  | `EvaluationTaskId` | `by_task`, direct claim, task-owned promise registration, task reports | Keep for reflection and promise-producing tasks. Lazy routes need no semantic task ID; task-directed pumping and reporting are migration clients. |
  | `EvaluationSessionId` | `WorkRecord.demand_session`, `work_by_session`, wait owner, closure, reports | Keep for actual session-owned work and task-owned promises. A lazy route has no first-observer owner session. |

  **Concrete migration clients.** `producer_for_wait`, `task_dependency`,
  `task_is_claimable/busy`, `task_observed_epoch`, and `claim_task` currently
  turn a wait into a task and back into work. `prioritized_task_for`,
  `pump_demand`, the client-demand pump, yielded-exact continuation,
  `target_has_running_producer`, and runtime-observation checks follow that
  task chain. The existing `work_for_wait_locked` lookup and work IDs can
  instead carry lazy-route causal progress; real tasks may continue to use
  `by_task`. `EvalContext::poll_wait` only needs to know whether an active
  producer exists, but currently tests that through `producer_for_wait`.

  `EvaluationWaitToken` presently stores `producer: EvaluationTaskId` and
  `owner_id: EvaluationSessionId`. Both are consumed by `reported_dependency`,
  `wait_for_claimed_task`, `abandon_deferred_producer`, runtime dependency
  snapshots, and task-handle/promise-obligation checks. The latter must keep
  task provenance, but already have their own task/owner fields or receive
  those separately. The chosen model makes the generic wait token only a
  runtime-local completion identity; the active coordinator record supplies
  producer kind, while task handles and promise obligations retain their
  actual task/session IDs. No task/session/lazy origin tag or dummy IDs belong
  on the token: after producer retirement its terminal result is sufficient.
  The `abandon_deferred_producer` owner-session stop rule (also reached when
  spark dependencies are abandoned) and `reported_dependency` cross-session
  classification particularly require route-aware replacement, not just
  field optionality.

  This reaches the public readiness API: `RuntimeDependency::TaskWait`,
  `RuntimeTaskWait`, `RuntimeDeadlockWork::session_id`, and killed
  `RuntimeDisposition::session_id` assume every producer is session-owned;
  the default logger projects those fields. A runtime-owned lazy route needs
  an explicitly distinct wait/deadlock origin and no invented session or
  task ID. The session report path for actual reflection tasks retains task
  IDs while following a lazy dependency. The source-structure registry test
  fixes the current `WorkKind` variant list, and about fourteen coordinator
  fixtures construct
  lazy deferred records directly; these are migration fixtures, not evidence
  that lazy work needs task identity. Four WHNF fixtures construct wait tokens
  directly and will need adjustment if the token constructor changes.

  `EvaluationUnfinishedTask` also carries optional dependency task and
  session IDs plus a wait ID; a reflection task blocked on a lazy can keep
  its own task ID and report the lazy wait without inventing a dependency
  task/session. Its `Quiescent` versus `Deadlocked` classification must follow
  actual runtime-owned progress or an eventual task owner, not the lazy's
  first observer. `wait_for_claimed_task` currently returns early for a
  different wait owner session and likewise needs route-aware progress checks.

  **Scope assessment.** This is a medium-sized, cross-layer cutover rather
  than a local rename: coordinator indexes/selectors and claim/release;
  wait-token simplification and exact-chain pumping; session closure/abandonment;
  readiness/deadlock projection and logger; then direct coordinator fixtures.
  There are about 44 `allocate_wait_token` occurrences across production and
  tests, but most are mechanical constructor call sites rather than distinct
  ID semantics; four tests call `EvaluationWaitToken::new` directly.
  It does **not** require changing the runtime's global ID allocators,
  promise producer identity, failure-ledger keys, or the exact-subscription
  protocol. The smallest coherent boundary keeps work/wait IDs and converts
  lazy chains to wait/work-directed lookup while splitting the producer
  kinds; a temporary synthetic task ID would save some edits but leave the
  misleading public and lifecycle assumptions in place.

  **ID decision (2026-09-21).** Keep `EvaluationWaitToken` minimal: unique wait
  ID, runtime/value-domain observer needed for terminal publication, terminal
  cell, and exact subscriptions. Resolve an active wait's producer through
  the coordinator's wait-to-work index; keep task/session IDs in actual
  task contexts, handles, promise obligations, and session-owned work records.
  Readiness projection must consult work state. Recursive abandonment must
  consult the producer kind and its live demand instead of a token
  owner-session rule. This also avoids giving completed, retained wait tokens
  stale producer provenance. Synthetic lazy task/session IDs and a tagged
  wait origin are not part of the target model.

  **Interim report decision (2026-09-21).** The current public deadlock report
  is an ad hoc bootstrap view, not a semantic contract for private scheduler
  identities or a promise of stable numeric IDs. Distinguish `LazyRoute` from
  promise-producing task work in the reported kind. A lazy route reports its
  available `LazyId`; existing work/wait IDs may remain as incidental debug
  fields, not its primary description. Its task and session fields are absent;
  a wait on that route is not a `TaskWait`.
  Make the relevant session fields optional for runtime-owned work (including
  killed dispositions), and give lazy-wait dependencies a separate reported
  shape carrying the wait and lazy IDs. Task-owned promise/reflection reports
  keep their genuine task/session provenance. Project this information from
  the active coordinator record, never by restoring provenance fields to
  `EvaluationWaitToken`. Update the default logger and tests for the new
  distinction without freezing an incidental text or numeric-ID schema.

  A future reporting/reflection API should inspect and describe the
  computation itself instead of using these private IDs as its primary
  explanation. That is deferred and must not complicate this lifecycle
  cutover.

  **Route/root/claim invariant.** An active route retains the lazy root needed
  while subscribed or claimed; a detached claim temporarily retains that
  exact root. The route never owns a second semantic checkpoint or durable
  task machine. It publishes checkpoint/terminal and exact block state before
  releasing its exclusive claim. A terminal wait remains observable after
  route retirement; a still-reachable lazy can later admit a fresh route over
  its checkpoint.

  **W6G.1f.2b.0 complete (2026-09-21).** The lazy/promise work-kind split,
  minimal wait-token identity, interim report distinction, and coherent
  route/root/claim boundary are selected. Implementation begins at 2b.1.
- **W6G.1f.2b.1 — typed route and context groundwork.** Introduce the
  reviewed lazy-route representation and runtime-owned pure-production
  context without creating a second authoritative producer machine. Keep
  task-owned promise records and their terminal obligations intact. Source
  selection must find the lazy's installed checkpoint or original source
  through one matching access, not retain another computation in the route.

  **Profile correction (2026-09-21; repair during this checkpoint).**
  `refl` and `meta_refl` annotations must use the runtime's immutable default
  reflection profile regardless of the first observer. Only `.task.new`
  inherits its caller's profile. I3D.1 originally implemented this rule, and
  W6G.1f.3b.2 explicitly retained it for autonomous reflection handoff.
  However, the W6G.1f.3b implementation changed completion reservation to
  copy `EvalContext::task_profile`, and `for_runtime_background` likewise
  preserves the observer's profile. A macro demand can therefore select the
  macro profile for an annotation task. This is implementation drift, not an
  open profile-policy question. Restore runtime-default selection for
  annotation completion before admitting machine-free lazy routes; add
  forced first-observer tests for both annotation forms and keep a separate
  test proving `.task.new` inherits its parent profile. Revisit the macro
  reflection diagnostic-count expectations changed by W6G.1f.3b, since the
  selected profile also controls the diagnostic destination. Do not use the
  observer's profile as a fallback when the default is unsealed.

  **Profile correction complete (2026-09-21).** Background annotation contexts
  select their demand domain's default profile, which is the one immutable
  runtime profile in production; isolated evaluator fixtures may provide a
  private default. Annotation completion reservation uses that selected
  profile. Custom effect-run profiles now belong to their `EvalContext`, not
  to a replacement session default, so ordinary `.task.new` still inherits
  the caller's profile. A macro-first result and source-level `refl`/`meta_refl`
  regressions exercise the boundary, and macro diagnostic destinations again
  match the runtime default. The existing `.task.new` inheritance test remains
  in force.
- **W6G.1f.2b.2 — coherent admission/poll/release cutover.** Atomically select
  the machine-free path for lazy producers: first-observer close cannot own
  their lifetime or profile, a claimed route temporarily retains the exact
  lazy root, one bounded quantum polls the installed checkpoint, and release
  publishes terminal cache or exact block/yield state before dropping claim
  authority. Preserve task-owned promise machine behavior. If admission and
  release cannot be made independently testable, merge their implementation
  commit rather than exposing a half-migrated route.
- **W6G.1f.2b.3 — last-subscriber retirement.** Retire an unclaimed route
  immediately at zero subscribers; latch retirement of an in-flight claim
  until its checkpoint or terminal result is published. A concurrent new
  subscriber must attach to exactly one authoritative route, never reuse a
  retired subscription epoch, and never lose a completion wake. A reachable
  lazy may later admit a fresh route over the exact retained checkpoint.
- **W6G.1f.2b.4 — forced regression gate.** Latch both orders of final
  subscriber/claim, publication/new subscriber, and first-session
  close/background subscriber. Reproduce the two historical configured CLI
  and reflection-descendant schedules above or a narrower deterministic
  equivalent. Check task-owned promise cancellation and pure lazy cycles,
  then run ordinary and aggressive-collection focused suites and routine
  repository gates before W6G.1f.4b-d claims the new lifecycle.

  **W6G.1f.2b.2-.4 complete (2026-09-21).** Lazy producers now admit one
  machine-free `LazyRoute` in the runtime background demand domain. The route
  retains only the lazy root, exact wait/block state, and a running-demand
  latch; each poll takes a transient machine shell over the lazy-owned WHNF
  checkpoint. Task-owned promise producers retain their distinct task machine
  and settlement obligations. A leased wait handle counts one logical route
  demand across clones. Its last drop retires an unclaimed route or latches
  retirement until a claimed poll publishes its checkpoint/block/terminal;
  a later observer may admit a fresh route over that retained checkpoint.
  Runtime-owned routes have no fabricated task or session identity in
  readiness reports.

  Session-wide drains may execute an exact *runtime-owned lazy route* needed
  by their blocked tasks, but do not claim another session's reflection task;
  that remains resumable cross-session quiescence. Exact client-demand pumping
  may follow the broader producer chain. The coordinator detects pure lazy
  cycles as before. It also follows reflection-task edges when a lazy route
  observes a promise owned by that same blocked task: it poisons only the lazy
  members, waking the reflection task through its normal subscription. A
  mixed task/lazy wait without a task-owned promise edge remains quiescent,
  and a separate promise-producing deferred task remains retryable. Forced
  tests cover both orders of the task/route block publication, subscriber and
  claim retirement, first-observer closure, and route re-admission. Focused
  reflection, evaluator, inventory, and complete library suites pass. The
  aggressive-GC gate found a test-fixture lifetime hole: constructing a raw
  managed lazy and rooting it in a later access allowed collection between
  the two. Route fixtures now construct and root in one access; focused route
  and mixed-cycle tests also pass under aggressive collection.

###### W6G.1f.3 — Complete producer-family ownership migration

Move every remaining `LazyTaskWork` family behind its correct durable
ownership boundary. Demand-driven work uses the lazy-owned checkpoint
protocol. A started reflection task uses a task-owned managed completion
source and then rejoins the ordinary WHNF checkpoint path. Preserve one-shot
host invocation, reflection reservation/activation, and other orchestration
across zero-demand intervals. Ensure callback execution, task admission, and
other orchestration still occur only after managed access closes; the value
graph stores traced semantic state and managed completion identities, never an
active callback, task continuation, registered root, or held mutator.

If a family requires an external sidecar, the managed lazy must own only an
edge-free lease/identity and its trace must still account for every semantic
edge. The sidecar must not retain a root back to the owning lazy or another
rooted checkpoint which reaches it. Treat any exception as a separate design
review rather than hiding it behind the external-owner registry.

Execute demand-driven checkpoint migration as an exhaustive typed sum, not
one erased checkpoint payload. Keep the autonomous reflection handoff
compile-exhaustive beside that sum rather than manufacturing a checkpoint arm
for it:

1. **W6G.1f.3a.0 — typed checkpoint carrier.** Move the field-opaque edge into
   an evaluator-owned discriminated carrier whose variants are concrete typed
   `Gc` edges. Begin with the existing WHNF variant and preserve its exact
   behavior. `core` may trace and retain the carrier but cannot inspect a
   variant. This representation-only checkpoint must add no allocation,
   vtable, box, or second trace contract.

   **Complete (2026-09-19).** `ManagedLazyCheckpointEdge` is now a
   field-opaque wrapper around the private evaluator-owned
   `ManagedLazyCheckpointKind` sum. Its initial `Whnf` arm carries the same
   typed `Gc<ManagedLazyCheckpointCell>` as before. Core sees only trace and
   access-qualified duplication; WHNF code receives the concrete edge through
   narrow evaluator-only projection methods. The change adds no runtime
   allocation or indirection. Durable-owner and persistent-edge inventories
   classify both the stored carrier and mutator-local concrete projection.
2. **W6G.1f.3a.1 — host-call checkpoint.** Publish `Invoking` before entering
   the opaque callback, retain callback captures through the lazy's traced
   source until invocation completes, and publish the rooted callback outcome
   back as traced checkpoint edges before yielding. A panic must leave a
   non-replayable checkpoint, never the original callable source state.

   **Complete (2026-09-19).** The concrete typed checkpoint sum now includes
   `Gc<ManagedHostCallCheckpointCell>`. Its mutex-protected state is either
   `Invoking(HostCallProducer)`—which continues to trace every declared
   semantic capture—or `After(Result<Value, EvaluationFailure>)`. The route
   which wins the source-to-checkpoint transition alone receives a transient
   state-free `HostCallInvoke` permit. It retires that permit before entering
   arbitrary Rust, so unwind, route loss, or later demand can observe
   `Invoking` but can never replay the callback.

   Callback execution remains outside managed access. The rooted callback
   result is validated against the runtime, projected back into the managed
   checkpoint under one short access, and then handed directly to a newly
   allocated WHNF checkpoint through an exact checkpoint-to-checkpoint edge
   transition. Failure publication uses the existing cache-first lazy
   terminal transition. No callback result root or state-bearing host machine
   remains in coordinator work.

   Forced fixtures count callback invocation across successful route loss,
   collection, and later resumption, and catch an injected callback unwind
   before dropping the original route and demanding the lazy again. Both
   schedules invoke exactly once. The explicit semantic-capture fixture now
   roots its intermediate lazy before any later mutator entry; this repairs an
   older raw-fixture lifetime gap rather than treating repetition as race
   evidence. Focused host-call tests also pass with aggressive collection.
3. **W6G.1f.3b — autonomous reflection handoff.** Do not add a reflection
   checkpoint. A reflection task which has started is autonomous background
   work and runs to a terminal disposition after the observing lazy route
   disappears. Represent its eventual result through a managed promise edge,
   not through a task handle or registered result root stored in the value
   graph.

   Partition this transition:

   - **W6G.1f.3b.0 — completion-source representation and terminal matrix.**
     Allocate one initially unassigned ordinary managed promise in the same
     value-access region which constructs the reflection computation, and
     trace it with the existing effect and optional gate target. Do not open a
     nested value-access region merely to prepare the promise. Generalize the
     task-owned promise settlement obligation so a reviewed terminal mapper
     may assign success as well as propagate failure. Return-value success
     assigns the task result; gate success assigns the target after the
     existing unit-result policy passes; failure, cancellation, abandonment,
     exit, and killing assign their existing structured failure and reflection
     context. Preserve the current distinction between assignment and
     propagation: an autonomous task failure remains unacknowledged if nobody
     ever observes the promised result, but the generic failed-promise path
     acknowledges it when an evaluator actually propagates that failure.
     Extend the promise's existing edge-free producer obligation with the
     task/session acknowledgement authority needed for that timing-independent
     operation; do not put a task handle or registered root in the managed
     cell. An unobserved reflection source owns no registered root.
   - **W6G.1f.3b.1 — one-shot reservation and activation handoff.** The first
     observer reserves exactly one reflection task and registers the managed
     completion promise with it. Retire
     `ReflectionComputationOwner`, its external-owner handle, and the cached
     weak task observation; they existed only to preserve a reservation in the
     old route-owned machine. The managed source promise, the lazy's exclusive
     source transition, and the transient activation permit are the complete
     linearization mechanism.

     Perform the handoff in this order: project and root the effect, target,
     and promise while the route owns the lazy claim; close managed access;
     reserve the task in the runtime-owned background context and register its
     terminal mapper; reopen matching access and replace the reflection source
     with the ordinary WHNF checkpoint focused on that exact promise; close
     access again; then activate. No task code may run before the producer and
     checkpoint publications are authoritative. The activation permit
     temporarily roots the effect, and the terminal-mapping obligation may
     temporarily root the gate target; these are active task obligations
     outside the managed graph. Dropping an unconsumed permit must terminalize
     the promise. If unwind happens before the source transition, later demand
     observes that terminal promise through the still-present source and
     performs the ordinary source-to-WHNF handoff; it never reserves a second
     task.
   - **W6G.1f.3b.2 — session-neutral background ownership.** Admit the task
     through one runtime-owned background-production session and the selected
     default reflection profile, not the client, spark, or reflection session
     which happened to discover the lazy first. This session lives until
     runtime teardown and supplies lifecycle/accounting identity without
     capturing an observer's close policy. Preserve the source's ordinary
     evaluation context frames in the mapped result; session neutrality must
     not erase semantic diagnostic provenance. The task remains a normal
     executor-visible reflection root; the lazy route merely depends on its
     promise and may retire independently. Registering the promise producer
     before activation also makes the task the exact causal dependency of the
     blocked lazy route, eliminating any need for same-session fallback. A
     client or session-local driver may poll this runtime-scoped task only by
     following such an exact dependency. Once no demanding route remains,
     workers or the runtime-wide background drain—not an arbitrary session
     drain—remain responsible for autonomous completion.
   - **W6G.1f.3b.3 — forced lifecycle and collection verification.** Force
     route loss before activation, while the task is queued/running/blocked,
     and after terminal assignment but before a later observer resumes. Count
     one reservation, activation, and terminal promise assignment across two
     sessions. Cover every terminal disposition, a result and gate target
     which point back to the owning lazy, first-session closure, collection
     while the task owns its promise/inputs, and reclamation after the task
     drops its final registered roots. Force ordinary failure propagation and
     prove it creates one promised failure, remains in the task ledger while
     unobserved, and is acknowledged without a duplicate diagnostic when a
     later evaluator propagates it. With zero workers, prove an exact client
     dependency may drive the task, then separately drop all demanding routes
     and prove a session-local drain does not adopt it while the runtime-wide
     drain completes it. Force a reflection task which depends back on its own
     result lazy so exact dependency/cycle reporting does not regress when the
     promise replaces the route-owned reservation.

   **Complete (2026-09-19).** `ReflectionComputation` now traces one ordinary
   managed completion promise instead of retaining a reflection-task sidecar.
   The source winner roots its inputs, reserves the task in the runtime-owned
   background demand, registers a compile-exhaustive terminal mapper,
   publishes the ordinary WHNF checkpoint focused on that promise, and only
   then activates. The selected reflection profile is the runtime default
   (restored in W6G.1f.2b.1), independent of the observer; lifecycle ownership
   is also session-neutral. Dropping an unactivated permit terminalizes the
   promise, while closing the first observing session retires only that route;
   a later session resumes the same autonomous task through a new exact wait.

   The terminal matrix covers return-value and gate success plus failure,
   cancellation, abandonment, killing, and exit. Ordinary promised-failure
   propagation now acknowledges the producing task only when an evaluator
   actually propagates the failure, so an unobserved autonomous failure
   remains reportable without producing a duplicate after observation. Result
   and gate backedges remain collectible, task-root retirement is counted
   across pending and terminal activation, and runtime teardown releases the
   background demand rather than retaining the value domain. Reflection-task
   edges also participate in direct compatibility tracing; this closed a
   collection hole exposed by descendant-failure fixtures.

   Transaction tests no longer manufacture an unsealed reflection wait: they
   use the public resolver-promise suspension boundary. Batch compiler drain
   alternates the macro session and runtime background demand so detached
   reflection work can complete without assigning ownership to the discovering
   session. The finer distinction between session-local and runtime-wide
   public drain policy remains the dedicated W6G.1g boundary; its tests should
   reuse this managed completion source rather than adding another
   reflection-specific lifecycle.
4. **W6G.1f.3c — net-WHNF checkpoint.** Replace registered net roots in the
   normalization request/worklist with traced managed-net edges and retain
   scalar ports, frontier observations, and driver state in one concrete
   checkpoint.

   Partition this transition:

   - **W6G.1f.3c.0 — edge-owned driver representation.** Give the request,
     worklist items, cursor dependencies, and frontier observations one
     compile-exhaustive managed-edge visitor. Remove registered net roots from
     that durable state while preserving transient roots at post-access
     semantic handoffs.
   - **W6G.1f.3c.1 — concrete checkpoint cell.** Add one typed
     `ManagedLazyCheckpointEdge` arm whose representation mutex contains the
     complete net driver, request interface, operation label, and pending
     semantic handoff. Trace through that state and mutate it only through one
     collector edge transition.
   - **W6G.1f.3c.2 — access and orchestration split.** Drive cursor/frontier
     work beneath the checkpoint's existing `EvaluationValueAccess`, publish
     any semantic active-pair handoff back into the checkpoint, close access,
     and only then perform semantic evaluation or wait for net contention.
     Re-enter one short transition to record the resulting retry/progress;
     never hold the checkpoint mutex or a mutator while waiting.
   - **W6G.1f.3c.3 — source cutover and forced schedules.** Construct and
     install function-call and lazy-net checkpoints in the source-claim region,
     replace `LazyTaskWork::NetWhnf` with a state-free marker, and force budget
     yield, semantic blockage, route loss, later resumption, and collection.
     Count exact net progress and prove the durable checkpoint owns no
     registered root.

   **Complete (2026-09-19).** The normalization request, driver worklist,
   cursor dependencies, and frontier observations now retain
   `CoreRuntimeNet` edges and expose one compile-exhaustive trace path.
   `ManagedNetWhnfCheckpointCell` stores the complete `NetWhnfMachine` behind
   the typed checkpoint carrier; `LazyTaskWork` retains only a
   `NetWhnfCheckpoint` route marker. Function-call and lazy-net sources install
   that checkpoint while their source claim and value access are still open.

   Cursor and frontier transitions run beneath the checkpoint's existing
   access. A semantic active pair is republished as the exact next retry before
   the checkpoint releases access, then temporarily rooted and executed only
   after both access and the checkpoint mutex close. Net contention likewise
   waits outside both scopes. The checkpoint records one scalar in-flight
   handoff while that semantic step runs, so another route yields rather than
   driving the temporarily claimed pair. The winning route clears the marker
   under a short re-entry and immediately reconciles the stored retry with the
   changed topology. This leaves no callback, wait, or registered net root in
   durable checkpoint state while preserving the pair which must be retried
   after a blocked semantic step.

   Checkpoint-family replacement now names the exact managed edge a route
   observed. A stale route may adopt the current family or terminal cache, but
   cannot overwrite a newer checkpoint merely because the lazy remains
   unresolved. This applies equally to host-to-WHNF and net-to-WHNF handoffs;
   ordinary WHNF polling treats a concurrent terminal publication as a normal
   route retirement rather than asserting that its stale checkpoint remains.

   Focused fixtures force budget yield, semantic dependency suspension,
   contention, route loss, collection, later route recreation, and successful
   completion. The route-loss fixture observes retirement of the route's net
   root before collecting with only the lazy/checkpoint and exact scheduler
   subscription alive. Source-backed inventories establish that the durable
   checkpoint owns no registered net root. Seven temporary `Clone`/`Debug`
   dependencies introduced by the edge-owned driver representation are
   explicitly assigned to the parent-carrier trait cutover rather than being
   accepted as permanent value-edge contracts. A deterministic stale-route
   fixture latches exact replacement, while the existing forced compiler-cache
   concurrency fixture latches in-flight exclusion. Direct driver-only tests
   use an isolated value heap and explicit fixture roots so an unrelated
   parallel collector cannot lend accidental lifetime to a raw test edge.
5. **W6G.1f.3d — access checkpoint.** Convert path arguments, current values,
   pending key/list conversion state, and child WHNF owners to traced edges.
   Preserve exact conversion position and source-owner diagnostics.

   The implementation review after W6G.1f.3c found that this is also the
   ownership bridge for shared key conversion. `KeyConversionMachine` is used
   by access, dictionary, object, and effect machines; `KeyListMachine` is used
   by access, dictionary, and pattern machines. Moving their present rooted
   fields directly into the access checkpoint would either duplicate the
   conversion reducer or force W6G.1f.3e-W6G.1f.3g into this checkpoint. Do
   neither implicitly.

   Partition the transition:

   - **W6G.1f.3d.0 — shared-converter ownership decision and census.**
     **Complete.** Record
     every key/list converter constructor, durable parent, registered root,
     child WHNF owner, and result/failure boundary. Choose explicitly between
     the recommended regional-state bridge below and a deliberately broader
     cross-family cutover. Reject an access-private copy of the conversion
     algorithm: it would create two resumable semantics which later phases
     must prove equivalent and remove.

     The source census selects the regional-state bridge. Scalar key
     conversion is constructed by access, dictionary insertion, object
     linearization, and named-effect dispatch. Key-list conversion is
     constructed by access, dictionary path update, and the path/list pattern
     machines. The latter parents remain rooted machine families assigned to
     W6G.1f.3e/.3g/.3i; migrating them here would be a broad cross-family
     cutover rather than access-checkpoint work.

     The pre-transition ownership and boundary ledger is:

     | State | Durable parents | Registered ownership | Nested resumable work | Boundary translation |
     | --- | --- | --- | --- | --- |
     | `AccessMachine` | `LazyTaskWork::Access` | rooted arguments/current value and rooted child machines | selected-value WHNF plus scalar/path conversion | `AccessMachinePoll` roots ready/failure and exposes scheduler dependencies |
     | `KeyConversionMachine` | access, dictionary, object, effect | rooted seed/member/list values | WHNF, recursive dict member, or `KeyListMachine` | `ConversionPoll` roots failures and exposes scheduler dependencies |
     | `KeyListMachine` | access, dictionary, pattern | rooted source/list/suffix values | source/chunk WHNF plus recursive scalar conversion | `ConversionPoll` roots failures and exposes scheduler dependencies |

     The canonical reducer will therefore use raw `Value` edges,
     `RegionalWhnfWork`, and semantic boundary/failure results. Only the
     temporary durable wrappers and, later, the access checkpoint translate
     those results into registered roots and scheduler dependencies after the
     managed access closes.
   - **W6G.1f.3d.1 — canonical regional key/list state.** **Complete.** Under the recommended
     bridge, split the existing converters into traced regional state holding
     ordinary `Value` edges plus `RegionalWhnfWork`, and a temporary durable
     wrapper for parents not yet migrated. That wrapper owns one managed cell
     and registered root, promotes any seed root only under matching access,
     and delegates to the same regional reducer. It is transitional ownership,
     not a second converter implementation; W6G.1f.3e/.3g/.3i must remove its
     remaining parent uses.

     Completion record: scalar and list conversion now share one regional
     reducer made from raw `Value` edges and `RegionalWhnfWork`. The existing
     dictionary, object, effect, and pattern parents delegate through a
     temporary managed cell carrying exactly one registered root, while all
     recursive dictionary/list children remain traced inside that cell. A
     forced one-step fixture collects between every strict nested conversion
     transition and latches the single-root ownership bound. Failure values
     are rooted before managed access closes; deferred-shell admission and
     dependency translation occur afterward through the canonical WHNF
     boundary adapter.
   - **W6G.1f.3d.2 — edge-owned access representation.** **Complete.** Convert arguments,
     current selection, pending dictionary members, list stacks/suffixes, and
     all nested converter/WHNF state to the regional forms. Add one exhaustive
     visitor over the complete access state. Preserve dynamic-key-before-base
     ordering, exact path/item indices, accumulated keys, missing-member `{}`
     behavior, and source-owner cycle diagnostics.
     Completion record: `AccessMachine` now contains only duplicated raw
     `Value` edges, regional key/list converters, and `RegionalWhnfWork`.
     Its compile-exhaustive visitor traces arguments, current selection,
     nested demand, and either converter variant. Pending keys remain semantic
     `Key` data and therefore introduce no hidden managed edge.
   - **W6G.1f.3d.3 — managed checkpoint and source cutover.** **Complete.** Add the typed
     access arm to `ManagedLazyCheckpointEdge`, install it directly from the
     claimed `LazySource::Access`, and replace `LazyTaskWork::Access` with a
     state-free marker. Poll and mutate only through one collector transition.
     Translate dependency, host, and failure boundaries after managed access
     closes; use exact checkpoint replacement and family adoption on stale
     routes as established by W6G.1f.3c.

     Completion record: dynamic access installs one typed
     `ManagedAccessCheckpointCell` directly beneath its lazy and retains only
     a state-free route marker. Each poll performs one traced state transition;
     semantic boundary interpretation and terminal publication occur outside
     that transition. Once traversal has selected its final value, the access
     cell atomically hands off to the canonical managed WHNF checkpoint rather
     than embedding a duplicate final-demand owner. A stale route adopts the
     exact winning checkpoint family or terminal cache.
   - **W6G.1f.3d.4 — forced schedules and bridge ledger.** **Complete.** Force suspension at
     scalar-key demand, dictionary-member recursion, path-list source, lazy
     middle chunk, selected dictionary base, and final result demand. For each
     shape, lose the active route, collect, resume from another route, and
     count completed prefixes so no member or chunk replays. Force concurrent
     routes across access-to-WHNF replacement. Close the access-family root
     inventory while recording every temporary durable converter wrapper
     still owned by W6G.1f.3e/.3g/.3i.

     Verification forces scalar-promise and recursive dictionary-member key
     suspension, lazy path-prefix and promised middle-chunk suspension,
     selected-base suspension, and final-result suspension. Routes are lost
     and managed collection runs between those stages; the completed prefix
     thunk is counted once, dynamic keys remain ordered before base demand,
     and terminal re-observation uses the cache. A deterministic two-route
     fixture leaves one route carrying the stale access-family marker while
     the other replaces the checkpoint with WHNF, then proves that the stale
     route adopts that exact family and completes after collection.

     The temporary bridge ledger is now closed: dictionary key/path parents
     and effect key parents remain assigned to W6G.1f.3g; object-name
     conversion remains assigned to W6G.1f.3e; pattern path/list conversion
     remains assigned to W6G.1f.3i. The access family itself constructs no
     temporary converter root. Root-publication, durable-owner, raw-value,
     persistent-edge, evaluator-access, WHNF-boundary, and WHNF-work-shape
     inventories latch this disposition.
6. **W6G.1f.3e — object-fixpoint checkpoint.** Convert linearization and mix
   stacks after their shared key/list/WHNF child forms have an edge-owned
   representation. Do not duplicate object traversal state in a sidecar.

   The post-access review found one previously implicit prerequisite:
   `ObjectLinearizationMachine` owns a `ListFrontMachine` while reading lazy
   dependency lists. That shared list projection still stores registered roots
   and a rooted `WhnfComputation`; it cannot be nested in a traced object cell.
   An object-private traversal would duplicate logical-list semantics. Use the
   same regional-state bridge established for key conversion instead.

   Partition the transition:

   - **W6G.1f.3e.0 — object and list-front ownership census.** **Complete.**
     Record every object field, child owner, result/failure boundary, and
     `ListFrontMachine` parent. The complete parent set is object
     linearization, three list-effect states, and the builtin task family's
     annotation, comparison, effect, list-observation, list-transform,
     object-builtin, pattern, and direct builtin states. Object work also owns
     the temporary key-conversion wrapper and ordinary rooted WHNF
     computations. Select one canonical raw-edge list-front reducer with a
     temporary one-root adapter for all unmigrated parents. Object consumes
     the regional form in W6G.1f.3e.2, list effect removes its adapter in
     W6G.1f.3f, and the compile-exhaustive builtin task sum removes every
     remaining adapter in W6G.1f.3g.
   - **W6G.1f.3e.1 — canonical regional list-front state.** **Complete
     (2026-09-20).** Split
     `ListFrontMachine` into raw current/chunk/suffix state using
     `RegionalWhnfWork`, plus a temporary managed-cell/root adapter for current
     parents. Preserve byte projection, deferred binary/list chunk semantics,
     exact suffix ownership, source-owner diagnostics, and all failure text.
     Force collection between deferred-chunk transitions and latch the
     one-root bound. Do not regionalize back projection before a checkpoint
     needs it.

     `RegionalListFront` is now the canonical callback-free reducer and owns
     only traced raw edges. `ListFrontMachine` promotes its rooted seed once
     into one `ManagedListFrontCell`; every existing parent keeps the old poll
     contract while object migration may embed the regional state directly.
     A one-step deferred-binary fixture proves the wrapper survives collection
     before and after its exact promise wait, preserves the suffix, and adds
     exactly one managed checkpoint root at promotion. Full library tests
     retain the existing non-list failure wording and the list/binary and byte
     projection contracts. Back projection remains rooted because no current
     managed checkpoint embeds it.
   - **W6G.1f.3e.2 — regional object representation.** This checkpoint has two
     compile-safe parts because raw state may not cross a returned poll without
     a traced owner:

    - **W6G.1f.3e.2a — regional child APIs.** **Complete (2026-09-20).** Expose the already canonical
       regional key-conversion and structured-application constructors within
       `crate::eval`; do not introduce a second converter or WHNF frame shape.
    - **W6G.1f.3e.2b — regional object state and temporary adapter.** **Complete
      (2026-09-20).** Convert
       original spec, self marker, linearization frames, seen specs,
       dependency sequences, definitions stack, accumulated base, and
       application state to raw traced edges. Use regional WHNF, key
       conversion, and list-front children. Add one exhaustive visitor over
       the complete state, then place it behind one temporary managed
       cell/root so the current parent remains valid until W6G.1f.3e.3. Preserve
       C3 ordering, anonymous-name identity, duplicate-name identity checks,
       and mix order. Do not retain a rooted and regional object traversal in
       parallel.

      `RegionalObjectFixpoint` now owns the complete raw object traversal:
      original spec, self marker, C3 frames and seen set, dependency-list
      progress, mix state, and regional WHNF/key/list-front children. Its
      exhaustive visitor is the single collector contract for those edges.
      `ObjectFixpointMachine` retains only a seed-or-managed compatibility
      shell and promotes the seed once into one managed cell, so no rooted and
      regional traversal coexist. All semantic duplication and identity tests
      occur under caller-supplied access; boundary translation and durable
      result publication remain outside it. The exact inventories record the
      temporary carrier and the reduction from fourteen scoped acquisitions
      and three durable contexts to one scoped acquisition and two durable
      contexts. The temporary root is deliberately assigned to the direct
      lazy checkpoint cutover in W6G.1f.3e.3.
   - **W6G.1f.3e.3 — managed checkpoint and source cutover.** **Complete
     (2026-09-20).** Add the typed
     object-fixpoint arm beneath `ManagedLazyCheckpointEdge`, install it from
     `FixpointComputation::ObjectInstance`, and replace
     `LazyTaskWork::ObjectFixpoint` with a state-free marker. Keep boundary
     translation and terminal root publication outside the managed transition;
     adopt the exact winning family or cache on stale routes.

     `ManagedLazyCheckpointEdge` now has a concrete object-fixpoint arm whose
     cell owns the exact `RegionalObjectFixpoint` introduced by .3e.2. Source
     admission constructs the self marker and regional state under one access,
     installs that typed edge directly beneath the lazy, and leaves only a
     state-free `ObjectFixpointCheckpoint` route marker. Polling locks and
     mutates the checkpoint through the collector transition API, then
     translates dependency/deferred boundaries or publishes the terminal
     value/failure only after the managed transition closes. The former
     temporary root/cell and rooted object machine are removed. Exact owner,
     root-publication, persistent-edge, evaluator-access, and lazy-family
     inventories latch the cutover.
   - **W6G.1f.3e.4 — forced schedules and bridge ledger.** **Complete
     (2026-09-20).** Force route loss and
     collection at spec demand, name conversion, deferred dependency chunks,
     nested dependency return, definitions demand, base application, self
     application, and terminal object publication. Count mixin applications
     and completed dependency prefixes. Assign the remaining temporary
     list-front adapters to W6G.1f.3f and W6G.1f.3g and the remaining
     key-conversion adapters to W6G.1f.3g, then close the object-family
     inventories.

     Two deterministic fixtures cover the boundary without relying on repeat
     runs. The linearization fixture destroys the active route and collects at
     spec demand, name conversion, deferred dependency-chunk demand, nested
     dependency return, and every yielded transition through terminal
     publication. Its counted first dependency proves a completed prefix is
     not replayed. The mix fixture uses separate public client demands with a
     full collection between definitions, base-result, and self-result
     boundaries; a counted definitions thunk proves demand is not replayed,
     while the profiling-only regression records exactly two committed call
     rewrites for the base and self mixin applications. Future values captured
     only by the fixture are explicitly rooted before the forced collections.

     The root-publication, mutator-entry, persistent-edge, owner, lazy-family,
     evaluator-access, WHNF-boundary, and work-shape inventories are green.
     The remaining list-front adapters stay assigned to W6G.1f.3f/.3g and the
     remaining key-conversion adapters to W6G.1f.3g; no object-private bridge
     remains.
7. **W6G.1f.3f — list-effect checkpoint.** This migration is split so the
   representation cutover and the adversarial schedule proof remain separate
   reviewable changes:

   - **W6G.1f.3f.1 — regional state and source cutover.** **Complete
     (2026-09-20).** Replace the registered-root recipe machine with one
     compile-exhaustive regional reducer beneath a concrete typed lazy
     checkpoint. Preserve run, sequence, cut, and fix phases; reuse regional
     WHNF and list-front children; and replace the fix promise root with one
     traced promise edge. Promise assignment commits beneath matching value
     access, while completion wakes and terminal lazy publication stay
     outside the managed transition. The route retains only a state-free
     `ListEffectCheckpoint` marker.

     This removes eleven list-effect root-publication sites, the temporary
     list-front adapter from this family, two durable WHNF constructors, and
     two seed-owner modifiers. The common promise publication implementation
     now admits either a registered root or an already-traced interior edge;
     both reach the same cell transition and preserve post-access wake
     delivery. Exact root-publication, durable-owner, recursive-identity,
     persistent-edge, evaluator-access, raw-value, producer-family, and WHNF
     inventories latch the new boundary.
   - **W6G.1f.3f.2 — forced schedules and family closure.** **Complete
     (2026-09-20).** Force route loss and collection during effect demand,
     handler application, deferred sequence/cut list chunks, fix-function
     demand, fix-operation demand, and terminal publication. Count
     recipe/function evaluation, fix promise construction, and terminal
     assignment so a later route demonstrably resumes rather than
     manufacturing another promise or repeating an assignment. Close the
     list-effect source, checkpoint, collection, and no-replay ledgers before
     proceeding to the builtin family.

     Three deterministic fixtures destroy the active route and collect after
     every yielded transition and at every exposed dependency boundary. The
     run fixture counts both effect and handler demand, then re-enters the
     completed lazy through a fresh route. The sequence/cut fixture retains
     deferred promise-backed chunks across collection and resumes the exact
     typed checkpoint after assignment. The fix fixture counts function and
     operation demand and snapshots runtime-local test instrumentation around
     managed promise allocation and successful publication: exactly one of
     each occurs, and a later route changes neither count.

     Fix-promise assignment and terminal lazy caching intentionally occur in
     one bounded value-access transition. There is therefore no schedulable
     route-loss point between them; notifications are delivered only after
     that access closes. The adversarial fixture forces the transition from
     both sides rather than inventing a test-only semantic gap. Durable-owner
     and persistent-edge inventories now name the forced collection fixtures,
     including the two test-only promise roots needed to keep deferred inputs
     valid while their routes are destroyed.
8. **W6G.1f.3g — builtin checkpoint.** Migrate the compile-exhaustive builtin
   task sum after its shared access, object, list, and WHNF child forms are
   available. Retain spark publication as post-access orchestration rather
   than managed checkpoint state. A best-effort spark is an autonomous
   background root after admission but is not a completion dependency of the
   enclosing builtin. Preserve only an at-most-once scalar admission phase in
   the checkpoint; loss before admission is allowed by best-effort semantics,
   while route replay must not multiply an admission already returned to the
   coordinator. Force the current release-before-submit ordering explicitly.

   The implementation census found that this is not one safe spike. The
   current 21-variant `BuiltinTaskMachine` delegates across eleven modules and
   roughly 7,100 lines; every family still retains `RuntimeValueRoot`, rooted
   `WhnfComputation`, or rooted list/conversion children. Partition it as
   follows:

   - **W6G.1f.3g.0 — census and cutover partition.** **Complete
     (2026-09-20).** Record the complete variant/module surface and separate
     raw semantic state from rooted terminal transport. The direct variants
     are annotation, assertion, conditional, comparison, dictionary, effect,
     list observation/map/concat/text, net, numeric, object, object
     composition, five pattern families, provenance, and strategy. Common
     blockers are rooted WHNF state, front/back list projection, key/path
     conversion, and result/failure roots. Spark is the only fire-and-forget
     boundary; all other variants are demand-driven regional computation.
   - **W6G.1f.3g.1 — shared regional prerequisites.** Add callback-free raw
     edge forms for back-list projection and key-list/path conversion, reusing
     `RegionalWhnfWork`, `RegionalListFront`, and `RegionalKeyConversion`.
     Define one regional builtin poll vocabulary carrying raw values/failures,
     exact scheduler boundaries, yields, and a raw spark intent. This pulls
     only the pattern list/path *child support* formerly left vaguely in
     W6G.1f.3i into the checkpoint that requires it; W6G.1f.3i retains the
     route/family closure audit.
     - **W6G.1f.3g.1a — Complete (2026-09-20): regional back-list
       projection.** Add `RegionalListBack` with raw current, prefix, and WHNF
       child edges, then reduce the durable compatibility machine to one
       temporary managed root. A forced promise-backed suffix fixture loses
       the active poll route, collects before and after the exact dependency,
       and resumes the same checkpoint without replay. The durable-owner,
       root-publication, and persistent-edge inventories record the reviewed
       exchange of three registered-root fields for one traced cell.
     - **W6G.1f.3g.1b — Complete (2026-09-20): regional key-list and path
       conversion.** Expose `RegionalKeyList` as the one callback-free raw
       child form for recursive key lists and pattern paths, preserving
       source-owner diagnostics and exact dependency boundaries. Required and
       optional conversion now share that regional implementation. Durable
       compatibility callers remain behind the existing single managed cell;
       a forced deferred-chunk fixture collects after promotion, at the exact
       promise wait, and after every resumed yield without rebuilding a vector
       of roots.
     - **W6G.1f.3g.1c — Complete (2026-09-20): regional builtin poll
       vocabulary.** Define the common raw ready/failure,
       scheduler-boundary, and yield result shape with the first production
       numeric consumer. The spark-intent arm remains deliberately absent
       until .3g.2d consumes it; this keeps the vocabulary compile-exhaustive
       without staging a dead variant.
   - **W6G.1f.3g.2 — scalar and direct families.** Regionalize numeric,
     assertion, conditional, provenance, net, and strategy state. The original
     wording placed the typed cell in .3g.5 but left no safe poll-spanning home
     for raw state during .3g.2-.4. Introduce the concrete builtin checkpoint
     incrementally here: migrated families install it beneath the lazy while
     unmigrated families continue using the durable compatibility sum. Each
     later family group extends the compile-exhaustive regional sum; .3g.5
     closes the source cutover and deletes the compatibility sum rather than
     introducing a second cell. Strategy records `SparkRequested` beneath the
     lazy before returning a spark intent; the outer route roots and submits
     it only after managed access closes.
     - **W6G.1f.3g.2a — Complete (2026-09-20): numeric foundation.** Introduce
       the common regional builtin poll vocabulary and typed lazy checkpoint
       with the numeric family as its first immediately exercised arm.
       Numeric sources now install the checkpoint directly from raw arguments;
       completed operands and the exact WHNF child remain exhaustively traced.
       A deterministic two-operand fixture destroys the first route, collects
       before and after the second-operand promise boundary, and proves the
       completed first operand is demanded once.
     - **W6G.1f.3g.2b — Complete (2026-09-20): assertion, conditional, and
       provenance.** Move the remaining direct value-demand families, reusing
       regional list-front state for conditional result selection and
       preserving diagnostic context. Forced assertion and origin fixtures
       collect on both sides of their exact promise dependency; the
       conditional fixture additionally collects while a deferred list-front
       chunk is suspended and proves the result-list source runs once.
       - **W6G.1f.3g.2b.1 — Complete (2026-09-20): assertion and provenance.**
         Move the direct scalar demand families first, retaining assertion
         phase/context and origin failure enrichment beneath the typed
         checkpoint.
       - **W6G.1f.3g.2b.2 — Complete (2026-09-20): conditional.** Reuse
         `RegionalListFront` after the result-list shell is demanded,
         preserving deferred-chunk progress and exhaustion diagnostics
         without a compatibility root projection.
     - **W6G.1f.3g.2c — Complete (2026-09-20): net operations.** Separate
       immediate construction from arity/net demand while retaining
       net/application semantics and failure context beneath the typed
       checkpoint. The existing source-order fixture now roots the owning
       lazy, collects before and after the exact net dependency, and proves a
       completed arity is not replayed. The collection fixture uses a private
       value heap; collecting the shared test factory would invalidate raw
       values held concurrently by unrelated fixtures.
     - **W6G.1f.3g.2d — Complete (2026-09-20): strategy and spark boundary.**
       Regionalize `seq` plus its one associated-metadata demand beneath the
       typed checkpoint. Spark performs no inline demand: it records
       `SparkRequested` before returning a raw intent, and the lazy route roots
       and submits that intent only after managed access closes. The detached
       worker retains the durable strategy machine solely to drive admitted
       sparks; because detached spark failures are intentionally discarded,
       that worker poll no longer constructs an unused failure root. Exact
       access, owner, checkpoint, and WHNF inventories latch the two removed
       durable child demands and the one deliberate post-access publication.
   - **W6G.1f.3g.3 — structural families.** Regionalize comparison,
     dictionary, list observation/transformation, pattern, and annotation
     state using the shared child reducers. Preserve exact traversal prefixes,
     diagnostic context, ordering, and failure text. This is six independent
     durable owners totaling several thousand lines, so migrate them through
     compile-exhaustive checkpoint additions rather than one cutover:
     - **W6G.1f.3g.3a — Complete (2026-09-20): dictionary.** Reuse
       `RegionalKeyConversion` and `RegionalKeyList` for singleton/update keys,
       replace sequential rooted demands with one regional operand queue, and
       retain completed dictionaries/name operands without replay. The queue
       removes each source as its exact child is installed, so the checkpoint
       traces remaining inputs, at most one active demand, and completed WHNF
       operands without retaining duplicate source edges. A private-heap
       fixture forces collection while the second union operand is blocked and
       again after assignment, proving the completed first operand runs once.
       The full parallel suite again reached the already-latched
       `command_line_workers_override_glam_workers` route/session-affinity
       schedule assigned to W6G.1f.2b; the complete library suite, structural
       inventories, and profiling regression set otherwise pass.
     - **W6G.1f.3g.3b — Complete (2026-09-20): comparison and tagged payload.** Move ordered scalar,
       tuple, list, dictionary, tagged-payload, and semantic-undefined frames
       together because comparison owns both tagged helper families. Preserve
       short-circuit ordering and the exact nested diagnostic context.
       - **W6G.1f.3g.3b.1 — Complete (2026-09-20): regional tagged primitives.** Introduce raw
         tagged-payload recognition and semantic-undefined traversal using an
         exact regional child demand and exhaustive visitors. Keep the durable
         adapters only until comparison switches representations in `.3b.2`.
       - **W6G.1f.3g.3b.2 — Complete (2026-09-20): comparison frame cutover.** Convert the explicit
         comparison frame stack as one unit: value classification owns its
         exact child demand; list/dictionary frames retain raw prefixes; tuple
         ordering embeds the regional tagged primitives and payload demand.
         Remove the superseded durable comparison/tagged owners and root
         publications only after the frame stack is fully traced.
       Completion record: scalar, list, dictionary, tuple, tag recognition,
       and recursive undefined progress now live as one traced raw frame stack
       beneath `ManagedBuiltinCheckpointCell`. Exact source ownership follows
       each regional WHNF child; no comparison result or recursive member is
       published through a registered root. Scalar and lazy-list route-loss
       fixtures collect before and after their exact promise assignments and
       prove completed operands/prefixes are not replayed. The legacy
       semantic-undefined owner remains only for pattern families and is
       explicitly assigned to `.3e`; its superseded tagged-payload companion
       is removed. Root-publication, durable-owner, raw-access, checkpoint,
       persistent-edge, and WHNF inventories record the cutover.
       The parallel gate also exposed a compiler import fixture constructing
       unrooted callback state on the process-global test heap while unrelated
       tests could collect it. The fixture now owns one private value domain,
       matching the existing loader/driver isolation policy; its focused
       failure is preserved as the regression rather than accepting a
       repetition-only pass.
     - **W6G.1f.3g.3c — Complete (2026-09-20): list observation.** Reuse regional logical-list
       front/back reducers for slice, length, split, index, head, and tail;
       retain strict-prefix progress and the selected suffix without roots.
       - **W6G.1f.3g.3c.1 — Complete (2026-09-20): regional owner and dispatch.** Move ordered index
         demand, subject classification, binary leaves, and front/back list
         walks beneath the typed builtin checkpoint. Keep result construction
         raw through the containing access region and preserve index failure
         contexts exactly.
       - **W6G.1f.3g.3c.2 — Complete (2026-09-20): forced traversal closure.** Force collection and
         route loss while a later list chunk is blocked, covering both front
         and back traversal and proving accepted prefixes/suffixes are not
         replayed. Remove the durable owner/publications and reconcile the
         access, root, checkpoint, edge, and WHNF inventories.
       Completion record: index operands, the subject, front/back logical-list
       reducers, accepted items, and binary/list results now remain raw and
       traced beneath `ManagedBuiltinCheckpointCell`; the durable
       list-observation owner and its four result-root publications are gone.
       Private-heap fixtures suspend after accepting the front prefix and the
       back suffix, collect before and after assignment, re-enter through the
       owning lazy, and prove those completed chunks run once. Existing
       slice, split, index, and contextual-error suites preserve the public
       behavior, while exact access, owner, checkpoint, edge, and WHNF
       inventories record the boundary change. The full parallel gate again
       reached the already-latched
       `configured_bare_cli_rewrites_and_executes_in_the_prepared_session`
       route/session-affinity schedule assigned to W6G.1f.2b; its exact test,
       complete library suite, clippy gate, and interaction-net profiling
       regressions pass. This repetition remains evidence for the pending
       coordinator cutover rather than evidence against the list-observation
       representation.
     - **W6G.1f.3g.3d — Complete (2026-09-20): list transformation.** Move structure-preserving map,
       balanced concat, and resumable text-lines extraction. Preserve lazy
       fragment boundaries and completed transform prefixes.
       - **W6G.1f.3g.3d.1 — Complete (2026-09-20): structural map and concat.** Move the callable,
         source demand, and one-node map/flatten transform beneath the typed
         checkpoint. Preserve every deferred outer child and delayed invalid
         concat leaf; neither transform gains an item-by-item eager walk.
       - **W6G.1f.3g.3d.2 — Complete (2026-09-20): text-lines traversal.** Move source demand,
         regional list-front work, exact item demand, and accepted bytes as
         one traced frame. Preserve `binary_extraction` failure context and
         prove a collected, route-lost prefix is not replayed.
       - **W6G.1f.3g.3d.3 — Complete (2026-09-20): family closure.** Remove the three durable owners
         and their result publications, then reconcile access, owner,
         checkpoint, edge, raw-value, and WHNF inventories. Retain the
         structural laziness fixtures as semantic gates in addition to the
         forced collection schedule.
       Completion record: map, concat, and text-lines are compile-exhaustive
       regional builtin variants. Map and concat demand only the outer source,
       preserve deferred children as lazy structural transforms, and return
       their raw one-node result without registered-root publication.
       Text-lines retains its exact source/list-front/item child plus accepted
       bytes beneath the typed checkpoint and preserves the established
       `binary_extraction` context. A private-heap schedule accepts one byte,
       blocks on the next, collects and re-enters before and after assignment,
       and proves the prefix callback runs once. The pre-existing structural
       laziness, delayed-invalid-leaf, back-suffix, binary, and error fixtures
       remain green; exact owner, access, root-publication, checkpoint, edge,
       raw-value, and WHNF inventories record removal of all three durable
       owners. The parallel gate also exposed the lazy-list error fixture
       carrying its managed result unrooted on the process-global test heap;
       that regression now constructs, roots, and observes the result in one
       private value domain, preserving the original delayed-error assertion.
     - **W6G.1f.3g.3e — patterns.** Move list, path, dictionary predicate/take,
       and equality patterns after their comparison and list/key dependencies
       are regional. Preserve fallthrough versus permanent failure exactly.
       - **W6G.1f.3g.3e.1 — Complete (2026-09-20): list structure.** Move is-list, empty, uncons, and
         unsnoc through the regional source/front/back reducers. Produce raw
         pass/fail effects and preserve compact binary remainders.
         Completion record: all four list-pattern operations now run as one
         compile-exhaustive regional builtin variant. Source WHNF, front/back
         traversal, and raw pass/fail effect construction remain beneath the
         owning lazy's typed checkpoint; compact binary slicing remains an
         allocation-free `Bytes` view. The final durable list-back adapter and
         its registered root are gone. A deterministic unsnoc schedule first
         traverses an empty lazy suffix, blocks on the promised prefix,
         collects and re-enters before and after assignment, and proves that
         the completed suffix is never replayed. Existing list-pattern shape,
         decomposition, delayed-failure, prefix-laziness, and promised-suffix
         suites remain green, while exact root-publication, durable-owner,
         raw-value, persistent-edge, checkpoint, and WHNF ledgers record the
         boundary change.
         The full parallel gate also exposed a separate stale net-WHNF route
         losing its checkpoint after another route published the terminal
         cache. A direct ordering fixture now terminalizes that stale route
         through the winning cache instead of panicking; this is deterministic
         coverage of the observed race rather than repetition-based evidence.
       - **W6G.1f.3g.3e.2 — path and literal equality.** Reuse regional key-list
         conversion and list-front/item work, preserving directional invalid
         path policy, unsupported-literal diagnostics, and binary/list byte
         comparison without replay.
         - **W6G.1f.3g.3e.2a — Complete (2026-09-20): path equality.** Move expected and actual path
           conversion, binary byte keys, and wrong-kind fallthrough beneath
           the builtin checkpoint. Preserve the asymmetry whereby an invalid
           compiler-provided expected path is a permanent failure while an
           unkeyable actual path is a pattern mismatch.
           Completion record: expected and actual path demand, key-list
           conversion, and binary-byte projection now remain beneath the
           owning lazy's typed checkpoint. The actual operand still uses the
           optional conversion path, so a wrong-kind or invalid user value
           falls through while an invalid compiler path fails permanently.
           The obsolete optional durable key-list adapter and rooted path
           owner are gone. A deterministic promised-actual schedule roots the
           owning application, collects and re-enters before and after
           assignment, and proves the compiler path thunk is evaluated once;
           exact owner, access, root-publication, checkpoint, edge,
           raw-value, and WHNF inventories record the cutover.
         - **W6G.1f.3g.3e.2b — Complete (2026-09-20): literal equality.** Move expected/actual WHNF,
           literal classification, and binary-versus-logical-list traversal
           beneath the checkpoint. Preserve unsupported-literal diagnostics,
           exact item failures, and accepted byte prefixes across collection.
           Completion record: expected and actual demand, literal
           classification, logical-list traversal, and the current byte item
           now remain raw traced state beneath the owning lazy's typed builtin
           checkpoint. Unsupported compiler literals are still diagnosed only
           after the subject is demanded, while scalar/binary mismatches and
           wrong list bytes remain ordinary pattern fallthrough. A
           deterministic promised-item schedule roots the application,
           collects and re-enters before and after assignment, and proves the
           expected literal thunk is evaluated once. The rooted literal
           equality owner is gone; the general durable list-front bridge stays
           until its reflection, effect, object, and annotation clients move.
           Exact owner, access, root-publication, checkpoint, edge, raw-value,
           and WHNF inventories record the cutover.
       - **W6G.1f.3g.3e.3 — dictionary predicates and extraction.** Move
         is-dict, logical emptiness, required/optional recursive take, parent
         frames, selected values, and remainder rebuilding as raw traced
         state. Reuse regional semantic-undefined traversal and key lists.
         - **W6G.1f.3g.3e.3a — Complete (2026-09-20): dictionary predicates.** Move is-dict source
           demand and logical-emptiness traversal beneath the typed builtin
           checkpoint. Force collection while emptiness is blocked after a
           completed member and prove that member is not replayed.
           Completion record: dictionary-kind source demand and recursive
           semantic-undefined traversal now remain raw traced state beneath
           the owning lazy's typed checkpoint. A deterministic two-member
           emptiness schedule roots the application, completes the first
           member, blocks on the second, collects and re-enters before and
           after assignment, and proves the completed member is evaluated
           once. Exact owner, access, root-publication, checkpoint, edge,
           raw-value, and WHNF inventories record removal of the rooted
           predicate owner. The full parallel gate also exposed an older
           evaluator fixture embedding an unrooted lazy in a process-global
           test heap before lowering; that fixture now constructs, lowers, and
           evaluates in one private value domain, making parallel forced
           collection structurally irrelevant. The gate then reached the
           separately tracked CLI prepared-session deadlock assigned to
           W6G.1f.2b.
         - **W6G.1f.3g.3e.3b — Complete (2026-09-20): dictionary extraction.** Move required and
           optional path conversion, source/member demand, parent frames,
           selected leaf, semantic-undefined traversal, and remainder
           rebuilding beneath the checkpoint. Force collection after a
           completed path prefix and preserve exact required/optional
           fallthrough, failure, and empty-compiler-path behavior.
           Completion record: required/optional key conversion, source and
           selected-member demand, parent frames, selected leaf,
           semantic-undefined traversal, and persistent remainder rebuilding
           now remain raw traced state beneath the typed builtin checkpoint.
           A promised-leaf schedule roots the application, completes a nested
           prefix, collects and re-enters before and after assignment, and
           proves the prefix thunk is evaluated once. Existing required,
           optional, wrong-kind, missing, undefined, forcing-failure, empty
           compiler-path, and remainder-shape fixtures remain green.
       - **W6G.1f.3g.3e.4 — Complete (2026-09-20): effect and family closure.** Construct raw pattern
         success/failure effects under caller access, remove the five durable
         pattern owners plus the now-unused durable semantic-undefined owner,
         force collection on the existing promised path/dictionary/list
         schedules, and reconcile every exact inventory.
         Completion record: all pattern success and fallthrough effects are
         constructed as raw values under caller access. List, path, literal,
         dictionary-predicate, and dictionary-extraction owners have all moved
         beneath the one typed builtin checkpoint, and the final durable
         semantic-undefined traversal and its registered publications are
         removed. Forced path, list, literal, emptiness, and extraction
         schedules plus exact root-publication, owner, access, checkpoint,
         edge, raw-value, and WHNF ledgers close the family.
     - **W6G.1f.3g.3f — annotations.** Annotation kind is known only after
       demanding the annotation operand, so this family cannot safely cut over
       one spelling at a time without installing a rooted fallback beneath the
       managed checkpoint. Build and review the regional family in the
       following precursor checkpoints, then switch dispatch once the family
       is compile-exhaustive:
       - **W6G.1f.3g.3f.1 — Complete (2026-09-20): regional vocabulary and recognition.** Introduce
         raw annotation arguments, recognized payloads, phase state, exact
         tracing, and callback-free demand helpers. Keep the regional machine
         undispatched until every recognized form has a regional continuation.
       - **W6G.1f.3g.3f.2 — Complete (2026-09-20): pure, assertion, and diagnostic phases.** Move
         assertion payload/name/value demand, `assert_unit`, error emission,
         diagnostic-context enrichment, unknown annotations, and `seq`/`spark`
         construction beneath the checkpoint. Preserve context attachment only
         for nested demand failures.
       - **W6G.1f.3g.3f.3 — Complete (2026-09-20): collection phases.** Move array, deque, and binary
         target/item traversal beneath the checkpoint using the regional list
         front. Preserve strict byte validation, compact-binary fast paths, and
         completed-prefix progress across yield and dependency suspension.
       - **W6G.1f.3g.3f.4 — Complete (2026-09-20): metadata and reflection construction.** Move
         metadata target/carrier traversal and projection construction beneath
         the checkpoint. Construct reflection gates and `meta_refl` result
         lazies as ordinary traced graph values; starting or resuming their
         autonomous reflection tasks remains a later post-access boundary of
         those lazy sources, never annotation-checkpoint state.
       - **W6G.1f.3g.3f.5 — Complete (2026-09-20): dispatcher cutover and closure.** Add the annotation
         variant to the compile-exhaustive regional family, remove the durable
         annotation owner and its registered roots, and reconcile the exact
         inventories. Force collection across assertion, collection, metadata,
         reflection-gate, and diagnostic-context suspension schedules, with
         no replay of completed prefixes or reflection reservation.
         Completion record: annotation recognition, assertion payload/name/
         value demand, collection traversal, metadata extraction, diagnostic
         enrichment, and result construction now remain raw traced state in
         the managed builtin checkpoint. Reflection annotations and
         `meta_refl` construct only ordinary traced reflection-source lazies;
         reservation and activation remain the source owner's post-access
         responsibility, so no reflection checkpoint or registered root was
         added to the value graph. Forced schedules collect a rooted
         annotation while waiting on a diagnostic context, binary item,
         metadata carrier, or reflection completion and prove completed
         semantic prefixes and reflection work are not replayed. The durable
         annotation owner, twelve root-publication sites, and fourteen rooted
         child-WHNF seeds are removed, and the ownership, raw-value, access,
         checkpoint, and WHNF inventories are reconciled. The library suite,
         clippy gate, and interaction-net profiling regressions pass; the full
         gate again reached the separately tracked CLI prepared-session
         deadlock assigned to W6G.1f.2b.
   - **W6G.1f.3g.4 — effect and object families.** Regionalize effect,
     object-builtin, and object-composition state. Reuse application
     checkpoints and regional child forms rather than embedding a registered
     root or reconstructing a completed prefix. Partition this deliberately:
     - **W6G.1f.3g.4a — Complete (2026-09-20): effect construction.** Move effect application/call,
       fixpoint construction, and effect-map traversal beneath the typed
       checkpoint. Effect values are evaluator-pure recipes here: this phase
       constructs lazy applications and selections but does not interpret a
       user or host callback. Completion record: function, key, argument-list,
       result-list, and fixpoint demand plus completed argument prefixes now
       remain raw traced state in one managed builtin checkpoint. Six
       production result-root publications and the durable effect owner are
       removed; five rooted WHNF seeds become three source-owned regional
       seeds. Forced effect-call and effect-map schedules collect the owning
       lazy while suspended, resume through a later route, and prove completed
       argument prefixes and delayed API selection are not replayed. The
       collection fixture also roots a function captured by a host semantic
       thunk rather than retaining an untraced managed value.
     - **W6G.1f.3g.4b — Complete (2026-09-20): object inspection and construction.** Move object
       specification, diagnostic-object, local-name, instance, default/dict
       definitions, and object-from-dictionary progress beneath the typed
       checkpoint. Preserve list-front and nested specification demand
       without rooted intermediate members. Completion record: the eight
       non-composition object builtins now use one regional object phase
       machine stored beneath the managed builtin checkpoint. Object, spec,
       name, parts, definition, base, and dictionary values plus nested WHNF
       and list-front work are traced in place; thirteen rooted WHNF seeds,
       seven result-root publications, and the durable object-builtin owner
       are removed. Object-producing leaf helpers retain explicit regional
       access, and the constructor inventory classifies their managed values
       as in-region forwarders rather than premature containing roots. Forced
       local-name, dictionary-definition, and plain-dictionary schedules keep
       only the owning lazy rooted, collect before and after dependency
       assignment, re-enter through a later route, and prove completed
       object/dictionary prefixes are not replayed. The exact inventories,
       clippy gate, full workspace suite, and interaction-net profiling
       regressions pass.
     - **W6G.1f.3g.4c — Complete (2026-09-20): object composition.** Move ordinary extension,
       composed definitions, and the recursive override stack beneath the
       checkpoint. Keep each completed prior/update prefix as traced raw state
       and use regional application demand for callable stages. Partition the
       cutover so each intermediate tree retains one complete owner:
       - **W6G.1f.3g.4c.1 — Complete (2026-09-20): ordinary and composed definitions.** Move `with`
         object/spec demand and the two-stage prior/extension callable chain
         beneath the managed checkpoint. Leave recursive override as the only
         legacy durable builtin family.
       - **W6G.1f.3g.4c.2 — Complete (2026-09-20): recursive override.** Move the iterative override
         stack, completed dictionary prefixes, pending update/prior demand,
         and nested return keys beneath the same managed checkpoint.
       Completion record: ordinary extension, composed-definition callable
       staging, and recursive override now share one compile-exhaustive
       regional composition variant beneath the managed builtin checkpoint.
       Object/specification/definition values, staged applications, completed
       dictionary prefixes, pending prior demand, nested frames, and return
       keys are traced in place. Eight result-root publications, six rooted
       WHNF seeds, one rooted application checkpoint, and both legacy
       composition machines are gone. Deterministic private-heap schedules
       retain only the owning lazy, collect before and after exact promise
       dependencies, re-enter through a later route, and prove ordinary
       object demand, prior-definition demand, and completed override prefixes
       are not replayed. The dispatcher audit also caught and fixed an omitted
       override admission arm: family and outer source admission are now both
       compile-exhaustive over all three operations.
     - **W6G.1f.3g.4d — Complete (2026-09-20): family closure.** Confirm removal of the three
       durable family owners, reconcile exact inventories, and force
       collection plus route loss during representative effect-call/map,
       object-member/list-front, and recursive override suspensions. Count
       completed traversal and callable prefixes so terminal equality cannot
       hide replay.
       Completion record: effect, object-builtin, and object-composition
       progress all live beneath the same typed checkpoint. Their former
       durable owners and registered result publications are absent from the
       ownership and root-publication ledgers. Forced effect call/map, object
       list/dictionary, ordinary/composed extension, and recursive-override
       schedules cover yield, exact dependency suspension, route loss,
       collection, and no-replay prefixes.
   - **W6G.1f.3g.5 — Complete (2026-09-20): typed checkpoint and source cutover closure.** Complete
     the incrementally introduced compile-exhaustive
     `ManagedBuiltinCheckpointCell`, allocate every selected regional family
     directly from the source's raw arguments, and remove
     `LazyTaskWork::Builtin(Box<_>)` once no legacy family remains. Translate
     raw ready/failure/spark results into temporary roots only after the
     managed transition closes. Remove the old rooted family sum and its
     temporary list/conversion adapters.
     Completion record: every demand-capable saturated builtin selected by
     `RegionalBuiltinMachine::supports` is allocated directly from raw source
     arguments into `ManagedBuiltinCheckpointCell`. `LazyTaskWork` retains
     only the state-free `BuiltinCheckpoint` route marker; the rooted
     `BuiltinTaskMachine` sum and its family-specific root adapters no longer
     exist. Immediate callback-free operations remain on the direct fallback
     and publish their result through the source owner before access closes.
   - **W6G.1f.3g.6 — Complete (2026-09-20): forced schedules and family closure.** Force yield,
     dependency suspension, route loss, and collection in every family group.
     Count completed traversal prefixes and spark admission. For spark, latch
     both permitted loss before admission and at-most-once submission after
     the poll returns, including the release-before-submit ordering. Close the
     source, checkpoint, root-publication, persistent-edge, and no-replay
     inventories before proceeding.
     Completion record: each scalar, structural, annotation, effect, object,
     and composition family has a deterministic private-heap schedule that
     crosses its representative exact dependency or yielded transition,
     destroys the active route, collects, and resumes through a later route.
     Completed operand/traversal/callable prefixes are counted rather than
     inferred from terminal equality. Strategy records spark intent beneath
     the checkpoint before returning it, and the outer route performs the one
     registered publication and submission only after managed access closes.
     The source-backed producer, root-publication, durable-owner,
     persistent-edge, raw-value, checkpoint, and WHNF inventories pass along
     with the full workspace, clippy, and interaction-net profiling gates.
9. **W6G.1f.3h — pure net construction over `ListEffect` — Complete
   (2026-09-21).** Follow
   [`PureInteractionNetConstruction_2026-09-20.md`](PureInteractionNetConstruction_2026-09-20.md).
   Do not migrate `IsolatedEffectSearch` or its roots beneath the lazy
   checkpoint. Interpret the construction program as ordinary pure builder
   state over the existing `ListEffect` return/sequence/alternative/cut/fix
   machinery, preserving the documented task-local state and delimited-control
   operations. Select exactly one strict semantic netlist, then pass it to one
   hidden callback-free `interaction_net_from_netlist` primitive for validation
   and `NetBuilder` replay. Remove the dedicated net-construction producer
   route after parity and forced route-loss verification.
   Completion record: PNC0–PNC7 are closed. Source, strict replay, private
   builder, budget-one route-loss, collection, and no-capability fixtures
   pass. A source audit confirms one exclusive deferred claim and an atomic
   replay-to-managed-WHNF-checkpoint transition under one value access;
   forced route loss followed by repeated demand observes the same cached
   net identity. Separate completed-construction and suspended-checkpoint
   backedge fixtures reclaim their cycles after external roots drop. The
   full format, Clippy, test, and interaction-net profiling gates pass.
10. **W6G.1f.3i — family closure.** Finish the remaining route-owned WHNF
    cases in small checkpoints before the coordinator cutover in W6G.1f.2b:

    - **W6G.1f.3i.0 — source and route census.** Enumerate every production
      and test-only construction of `LazyTaskWork::Whnf`, including application,
      function fixpoint, static access, immediate builtin results, and
      semantic callback sources. Record which cases require a source owner,
      cycle context, or continuation, and which can install a checkpoint
      directly. Classify `Produce` as source admission and `HostCallInvoke` as
      a transient one-shot permit, not durable producer state. Check the
      source-backed producer and route inventories against these actual
      payloads rather than accepting an exhaustive list of variant names as
      proof that every variant is state-free.
    - **W6G.1f.3i.1 — direct WHNF source installation.** Construct and install
      the existing regional WHNF checkpoint beneath the owning lazy, under
      one matching value access, for application, function fixpoint, and
      static-access sources. Preserve each case's current source-owner and
      cycle context, continuation state, cache-first terminal publication,
      and rejected-installation handling. Split the three sources into
      separate code/test checkpoints if their handoffs differ. Force budget
      yield, exact dependency suspension, route loss, collection, and later
      resumption without repeating completed transitions.
    - **W6G.1f.3i.2 — remaining source results.** Keep the production builtin
      result and test-only callback boundary separate:
      - **W6G.1f.3i.2a — immediate builtin result.** Install the callback-free
        builtin result as a WHNF checkpoint without retaining an intermediate
        rooted computation in the route. Preserve existing failure and cache
        behavior; force route loss after result production.
      - **W6G.1f.3i.2b — test-only semantic callback.** Keep the callback
        outside managed access, but hand its returned value or failure into
        the lazy-owned checkpoint or cache during the same poll. No callback
        result may remain as durable route state. Count executions across
        route loss without introducing a production callback policy merely
        to accommodate test sources.
    - **W6G.1f.3i.3 — route payload removal.** Delete the state-bearing
      `LazyTaskWork::Whnf` variant and its publish/fallback path. Make source
      selection compile-exhaustive over state-free route markers, the typed
      lazy-owned checkpoint carrier, and the reflection-to-promise handoff.
      Do not remove `WhnfComputation` where it serves other evaluation roles,
      or remove the coordinator's temporary lazy-producer machine here;
      W6G.1f.2b owns that machine cutover.
    - **W6G.1f.3i.4 — closure evidence.** Recheck the source-backed producer,
      registered-root, durable-owner, persistent-edge,
      autonomous-obligation, and no-replay inventories against the resulting
      code, including payload shape rather than only enum names. Update
      present-tense architecture text that still calls migrated producer
      families coordinator-owned, and the source module map's stale W6G.3
      "pending aggregation" wording. Run focused forced schedules and the normal
      verification gates before starting W6G.1f.2b.

**W6G.1f.3i.0 complete (2026-09-21).** The remaining `Whnf` constructors and
their current route payloads are:

| Source | Current route-owned state | Handoff requirement |
| --- | --- | --- |
| Application | Managed WHNF root with application continuation; no `source_owner` | Install its exact regional state under the lazy while one matching value access is open. |
| Function fixpoint | Same continuation, with the lazy marker as argument; no `source_owner` | Preserve the marker backedge through the managed checkpoint. |
| Static access | Managed WHNF root with key-path continuation and `source_owner = lazy.id()` | Preserve owner-aware cycle detection and partial path progress. |
| Immediate builtin result | Rooted result in a `WhnfComputation::Seed`, without a continuation | Install the result as lazy-owned WHNF focus without retaining a second route root. |
| Test-only semantic computation/thunk | `WhnfComputation::Source` holds a lazy root until the callback returns; then a rooted result | Invoke outside managed access and publish the result/failure before ending that poll. |

`Produce` is a source-admission marker; `HostCallInvoke` is an ephemeral
one-shot invocation permit. All other `LazyTaskWork` variants are fieldless
checkpoint markers. The source-backed inventory now checks those payload
shapes, not just variant names. The still-open W6G.1f.3i.1-.4 checkpoints
replace these five constructions and eventually require every route variant
to be fieldless; this census does not certify their lifecycle yet.

**W6G.1f.3i.1 complete (2026-09-21).** Application, function-fixpoint, and
static-access sources now allocate `RegionalWhnfWork` directly under the
owning lazy in one value access, without an intermediate registered WHNF
root. The function fixpoint still embeds its lazy marker, and static access
still supplies the owner's `LazyId` for cycle handling. Installation races
adopt the winning checkpoint or terminal cache. Focused route-loss fixtures
force budget-one yield, collection, unresolved promises, and later completion
for all three families; the function-fixpoint fixture hands its resumed work
to ordinary coordinator demand after dropping the first manual route.
Existing strict fixpoint-cycle, shared-session result, and regional static
access tests also pass. Source-backed mutator, raw-value, persistent-edge,
and WHNF API inventories were rebaselined against the reviewed direct
handoff; all inventory tests pass. The temporary coordinator route machine
remains unchanged until W6G.1f.2b.

**W6G.1f.3i.2a-.2b and .3 complete (2026-09-21).** Saturated immediate
builtin results now install a regional WHNF focus directly under the lazy in
the same access as builtin application. Test-only semantic callbacks execute
outside managed access and publish their result or failure within that poll;
their temporary handoff root is dropped before returning. The route now has
only unit markers, and its former `WhnfComputation` payload and publish path
are gone. The unused `WhnfComputation::Source` representation and its
source-entry fixture were retired: the new lazy-source route-loss fixtures
exercise the production handoff, while `WhnfComputation` remains for rooted
external demand and reflection work. A zero-budget builtin fixture forces a
route-loss window after installation; counted semantic callback fixtures show
that later demand does not rerun them.

**W6G.1f.3i.4 complete (2026-09-21).** Source-backed producer,
registered-root, durable-owner, recursive-identity, persistent-edge,
raw-value, mutator, and WHNF inventories were reconciled with the removed
route/source payloads. The architecture map now describes the still-temporary
coordinator producer machine separately from the checkpoint ownership it
routes. All six focused W6G.1f.3i route-loss fixtures pass.
`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
`cargo test -q`, the 116-test source-backed inventory suite, and
`scripts/check-interaction-net-profiling.sh` pass. W6G.1f.2b remains the next
coordinator cutover.

Each demand-driven family checkpoint must force budget yield or exact
dependency suspension, loss of its active route, collection while the lazy
remains reachable, and resumption from a later authorized route. Host,
list-fix, and net-construction checkpoints additionally count their one-shot
actions. Reflection instead forces route loss while its autonomous task and
managed completion promise continue, and counts reservation, activation, and
terminal assignment. Do not defer a family merely because terminal equality
hides replay.

###### W6G.1f.4 — Retention and collection verification

The W6G.1f.4a evidence census may begin before W6G.1f.2b. Run the behavioral
verification in W6G.1f.4b-d only after W6G.1f.3i closes every state-bearing
route variant **and W6G.1f.2b removes the coordinator-owned lazy-producer
machine**. Otherwise those schedules would certify transitional ownership
rather than the target lifecycle. Partition the work as follows:

- **W6G.1f.4a — evidence matrix and retention baseline.** Map the existing
  family route-loss/collection fixtures, net-construction cycle fixtures,
  client-demand and spark lifecycle fixtures, and reflection task/ledger
  fixtures to the remaining gaps. Identify which roots are expected while a
  terminal result handle, detached report, or unacknowledged failure ledger
  still retains a value. Distinguish loss of the final active demand or route
  from loss of semantic reachability; neither alone proves collection.
- **W6G.1f.4b — demand-backed checkpoint schedules.** With no active route
  or subscriber, retain only a semantic path to the lazy and force collection;
  the lazy-owned checkpoint must survive and later authorized demand must
  resume it without replay. Then release that path and force collection of
  representative source/checkpoint cycles. Count completed semantic
  transitions and one-shot host actions rather than accepting terminal
  equality as no-replay evidence. Reuse each family's prior forced schedules
  rather than multiplying every family by every observer role.
- **W6G.1f.4c — mixed-observer and last-subscriber schedules.** Force
  representative client/spark and client/reflection observer pairings to
  share one authoritative source, checkpoint, or managed completion source;
  add a three-way case only if the source permits all three roles. Cover
  route/session close before and after checkpoint publication, loss of the
  final subscriber, and arrival of a new subscriber with latch-controlled
  orderings. A best-effort spark may be lost before admission, so assert
  at-most-once admission once admitted, not guaranteed execution. A started
  reflection task remains an
  autonomous background root rather than inheriting the lazy route's
  lifetime.
- **W6G.1f.4d — reflection and final root lifecycle.** Force reservation,
  activation, terminal promise assignment, route loss, and first-session
  close in both relevant orderings, including result/gate backedges and
  abnormal task dispositions. Count each reservation, activation, and
  assignment. Drop ordinary observers while the started task retains exactly
  its reviewed effect, target, and promise obligations. Assert the exact
  expected roots while result handles, detached reports, or failure ledgers
  remain; require return to baseline only after those named owners are
  released or acknowledged. Finish with ordinary and aggressive collection
  and a registered-root, persistent-edge, autonomous-obligation, and
  no-replay audit so inactive routes and completed autonomous work do not
  become permanent root classes.

**W6G.1f.4a evidence census (2026-09-21).** The existing route-loss fixtures
in `eval/value/tests/w4.rs` cover direct WHNF, host calls, net WHNF, object
linearization, list effects/fix, and pure net construction; they force
collection between handoffs and count the relevant one-shot callbacks or
promise allocations. The pure-construction backedge fixture additionally
reclaims an unrooted lazy/checkpoint cycle. The same-runtime evaluation suite
already forces last-subscriber retirement and in-flight reattachment, shared
cross-session client demand, spark queued/claimed/blocked roots, pending
reflection activation, terminal waits, detached reports, and failure-ledger
retention. These are evidence for their *named* owners, not for collection
merely because a route or subscriber disappeared.

The remaining focused gaps are: (b) collect after the final lazy route retires
while only a semantic value root remains, then resume without replay and
release that root; (c) force client/spark and client/reflection sharing with
explicit observer-loss orders; (d) tie reflection reservation/activation and
terminal assignment to route/session loss, including abnormal dispositions,
then audit root retirement. A terminal client handle, detached report, or
unacknowledged failure ledger intentionally retains its result/failure root;
the collection baseline is expected only after that precise owner is dropped
or acknowledged. Tests must not infer collection from `Gc` facade drops alone.

**W6G.1f.4b-d complete (2026-09-21).** The route-retirement fixture now
collects with no active route and only the lazy's semantic root plus its
assignable promise owner, resumes without replay, then releases both owners
and returns to the root baseline. The existing pure-construction backedge
fixture supplies the unrooted source/checkpoint-cycle reclamation half of 4b.
The new same-runtime client/spark fixture forces admission, blockage, client
abandonment, spark abandonment, collection, and later fresh demand in that
order. The client/background-reflection fixture forces shared lazy progress,
first-session close, collection, and autonomous task completion. Existing
claimed-route reattachment covers the pre-publication subscriber order; the
last-subscriber fixture covers post-publication route retirement. No
uncontrolled repetition is treated as race evidence.

For 4d, the new reflection-completion fixture forces both session-close vs.
activation orders and checks one launcher build, one managed completion
promise allocation and assignment, and return to the root baseline after the
last observer drops. Existing reflection-result route-loss, dropped-permit,
failure/cancellation, result/gate backedge, terminal-wait, detached-report,
and failure-ledger fixtures cover the other named owners and abnormal
dispositions. The registered-root and persistent-edge inventories remain
authoritative for the complete graph; this phase adds behavioral evidence,
not another permanent root class or a duplicate structural census.

After this boundary is stable, investigate whether the lazy should also own
its exclusive claim flag, current blocker, or completion subscriptions. That
may eliminate the transient producer route, but queues, root-role policy,
fairness, task lifecycle, and cross-source cycle analysis remain coordinator
responsibilities unless a separate review proves otherwise.

##### W6G.1g — Drain and quiescence separation

Partition this after W6G.1e.2/e.3b and the e.3a evidence gate:

- **W6G.1g.1 — session drain authority.** Select session-owned reflection
  roots and their exact descendants, including a causally required
  cross-session child. Do not claim unrelated work merely because it shares
  a session. Before cutover, force both publication orders between a real
  configured `conf.log` consumer and a diagnostic produced by another
  session through the production `DiagnosticIngress`; retain e.3a's
  zero-worker logger-shaped/client/spark selector fixture as the topology
  baseline.
- **W6G.1g.2a — bounded runtime background pump.** Add a host-callable
  `EvaluationRuntime` operation (provisionally `pump_background(budget)`), not
  just a private selector. One call spends at most its supplied evaluation-step
  budget on runtime-visible reflection roots across sessions and their exact
  and published causal descendants, including autonomous lazy-launched
  reflection after route loss. Zero budget polls nothing; each claimed machine
  gets a bounded quantum before another is selected. Exclude sparks and
  foreground client roots, and never select a task merely because it shares a
  session with selected work. Report spent budget and distinguish remaining
  runnable work, a worker-owned/contested causal claim, and stable absence so
  the host can choose whether to pump again or wait; do not wait or classify
  readiness inside this operation. As elsewhere, the budget cannot preempt a
  host callback already in progress.
- **W6G.1g.2b — runtime background drain composition.** Build the existing
  `pump_until_stable` convenience behavior over the bounded pump, explicit
  worker-owned-progress waits, and spark-abandonment passes. Preserve its
  useful-work/observation sequence, but do not hide an unbounded poll loop
  inside `pump_background`. Keep the change-wait generation compatible with
  W6G.1d's lost-wakeup-safe timed wait so a later host can compose foreground
  `try_advance`, bounded background pumping, and timed waiting without a
  built-in productive-wait policy.
- **W6G.1g.3 — observational readiness and final scope matrix.** Separate
  polling from snapshot construction, specify parked external client-demand
  activity without calling it background work, and force quiescence,
  spark-abandonment, cross-session, and route-loss orderings. Include
  zero/one/many-budget tests, a machine that yields after one bounded quantum,
  and both publication orders around a contested-worker wait;
  verify that the bounded pump neither claims a client/spark nor spins when
  only worker-owned progress remains.

**W6G.1g complete (2026-09-22).** Session drains now begin only at reflection
roots owned by the session and follow exact producer chains, including an
exact producer in another session. The old mixed ready-queue selector remains
only as an explicitly named coordinator-mechanics test hook. The renamed
`session_drain_follows_an_exact_cross_session_dependency` fixture latches the
changed behavior, while
`logger_shaped_session_drain_leaves_independent_producer_client_and_spark_for_runtime`
continues to prove that an unrelated diagnostic producer is not an implicit
logger child. The production `conf.log` fixtures now generate their
diagnostics from independently scheduled Glam `.log` effects and force both
consumer-before-producer and producer-before-first-consumer-read orders
through the real `DiagnosticIngress`.

`EvaluationRuntime::pump_background` is the bounded host surface. It spends
no more than the supplied evaluation-step budget, gives each selected machine
at most the normal quantum, excludes clients and sparks, and returns exact
spent work plus `Runnable`, `Busy`, or `Stable`. `pump_until_stable` composes
bounded passes with spark abandonment and activity-generation waits; it still
waits for a worker-owned spark so that it cannot abandon an active claim.
Scoped pump snapshots distinguish a claimable background chain from a chain
owned by another poller instead of treating every running client or unrelated
machine as background activity.

The forced matrix covers zero/one/many budgets, yield-before-completion,
contested causal work before and after release, exact cross-session progress,
route-loss/autonomous reflection behavior, spark abandonment, and explicit
client/spark exclusion. A retained foreground client record is now external
activity: readiness reports `Busy` until its owner completes, cancels, or
drops it, and runtime deadlock settlement no longer reports or kills that
client as scheduler work. Snapshot construction remains observational; the
bounded pump itself neither waits, abandons, nor settles.

Narrow session and runtime drains to their documented background scopes.
Runtime-wide draining may traverse newly created reflection roots across
sessions and wait for worker-owned progress, but it must not steal another
thread's client demand. It never polls a spark; stable quiescence may abandon
unclaimed best-effort sparks under the existing lifecycle policy. Readiness
and snapshot APIs remain observational.

Runtime-owned reflection tasks launched from canonical lazy sources belong to
the runtime-wide background scope, not whichever session first demanded the
source. A session-local driver may reach one only as an exact dependency of a
root it is authorized to poll. If that root disappears, the autonomous task
remains runtime-visible and is completed by workers or the runtime-wide drain.
Do not recreate first-observer affinity as a separate drain-scope tag.

Separate “help execute background reflection work” from “classify stable
runtime state” in names and tests even if the public `pump_until_stable`
convenience operation performs both in sequence. The bounded background pump
is a deliverable of W6G.1g, not merely a future optimization of that
convenience operation.

Client-evaluation records are external demand, not executor work. Define
whether an outstanding parked client handle contributes an external-demand
activity count, but do not classify it as ready background work or a runtime
deadlock merely because no client is currently polling it. Shared producers
remain visible to readiness only through the live roots which demand them.
Autonomous reflection tasks are themselves background roots after launch and
remain visible independently of the lazy route which awaits their managed
completion promise.

##### W6G.1h — Serialization retirement and verification

Keep the scan removal distinct from the final integrated test gate:

- **W6G.1h.1 — scan and compensating-wait census.** Identify every admission
  and busy/wait check whose sole purpose is the temporary one-ordinary-
  machine-per-session restriction. Exclude exact claim ownership, terminal
  publication, causal dependency waits, and task-owned promise lifecycle.
- **W6G.1h.2 — atomic policy retirement.** Remove the temporary scan and only
  its compensating checks after W6G.1e/g make role-specific discovery
  authoritative. Preserve any same-session observation required for
  reporting rather than conflating it with execution serialization.
- **W6G.1h.3 — forced closure and documentation.** Run the W6G.1a matrix and
  the cross-root, cancellation, owner-close, lost-wakeup, readiness, and
  terminal-publication schedules below; update current architecture only
  after those policies are observed in the implementation.

**Complete (2026-09-22).** The census classified the temporary policy as:

- the `session_has_running_machine` scan itself;
- eligibility filters in the worker/session test selectors and the private
  client-demand selector;
- broad same-session or runtime-running `Busy` fallbacks in foreground pumping
  and its generation wait; and
- session-drain and runtime-readiness conversions which changed an otherwise
  claimable causal candidate into `Busy` solely because another machine in
  that candidate's demand session was running.

Those paths were removed together. The exact-claim state,
`target_has_running_producer`, causal-child `Busy` detection, checked
subscriptions, terminal publication, task-owned promises, and the
`work_by_session` lifecycle/reporting index were retained. The session-ready
probe likewise remains a drain-discovery fact; it neither grants a claim nor
excludes another machine.

The former serialization fixtures now assert the target policy: two ready
same-session coordinator records can be claimed together, an independent
client demand can run while a same-session deferred producer is claimed, and
an unrelated running runtime machine does not turn a broad unresolved target
into causal progress. A latched two-worker fixture holds the first machine
inside its poll and requires the second worker to enter a second independent
same-session machine before either is released. Exact-claim exclusion,
causal-child helping, owner close, cancellation, last-subscriber retirement,
cross-root lazy sharing, lost-wakeup, terminal-publication, and readiness
fixtures remain authoritative for the mechanisms deliberately preserved.
Current architecture and invariant documents now state explicitly that demand
sessions are lifecycle/reporting groups rather than execution lanes and that
same-session FIFO or one-machine ordering is not a contract.

After role-specific root discovery is authoritative, delete
`session_has_running_machine` from global admission and remove only those
`Busy`/wait checks which existed to compensate for that temporary scan. Exact
machine claims, lazy-owned producer checkpoints, demand-backed routes,
subscriptions, and terminal publication remain the concurrency authority. Do
not retain same-session FIFO or one-machine ordering as accidental semantics.

Run the forced W6G.1a matrix plus zero-/one-/many-worker client demand,
reflection, spark, cancellation, owner-close, lost-wakeup, no-false-
quiescence, last-subscriber retirement, cross-root producer sharing, and
terminal-publication suites. The sharing fixtures must count source polls and
prove that all observers see one canonical result without copying a
lazy-owned producer checkpoint or launching a second autonomous task. Update
current architecture only after implementation so it states that workers
select background roots and causal descendants, clients retain their own
registry records, demand-driven shared producers are session-neutral and
lazy-owned, autonomous reflection work fulfills managed completion sources,
and explicit drains have distinct claim authority. Performance attribution
remains W6G.4; W6G.1 records only gross regressions which would make the
ownership mechanism nonviable.

#### W6G.2 — Regional standard-effect fusion (extracted and deferred)

**Extracted 2026-09-23.** This optimization now belongs to the independent
[Pure Effect Access Fusion plan](PureEffectAccessFusion_2026-09-23.md),
deliberately sequenced after
[Value Representation Refinement](ValueRepresentationRefinement_2026-08-19.md).
The representation transition will change the cost and shape of intermediate
value copies, managed edges, and root publication, so implementing this
regional effect checkpoint first would optimize and then migrate a temporary
boundary.

The current bounded W5 fusion remains the production and semantic reference
path. This extraction does not block W6G.4-W8, GC integration, or Value
Representation Refinement. The independent plan retains the measurement
gate, callback-free eligibility rules, differential unfused interpreter,
budget/suspension matrix, and requirement that managed access close before all
scheduler, reflection, transaction, and host coordination.

#### W6G.3 — Aggregate durable WHNF state

Replace the current root-per-retained-value **demand checkpoint** with one
rooted managed state cell. NC2.0 has already established the canonical
`WhnfState`/`WhnfContinuation` vocabulary:

```rust
struct ManagedLazyCheckpointCell {
    state: Mutex<WhnfState>,
}

struct ManagedWhnfRoot {
    runtime: EvaluationRuntimeId,
    root: Root<ManagedLazyCheckpointCell>,
}
```

This target deliberately does **not** replace the source-entry checkpoint.
`DurableWhnfCheckpoint::Source { lazy, runtime, result }` has a distinct role:
it owns a lazy source while its producer runs and may later receive a rooted
result outside evaluator access. It remains unchanged until its result is
actually demanded. Only then, beneath matching access, does that result become
canonical managed demand state.

**Later supersession (W6G.1f.3i, 2026-09-21):** this source-entry target and
its fixture were retired when all lazy producers began installing their
progress directly beneath the owning lazy. The seed/managed-demand boundary
below still applies to external and reflection demand.

Likewise, the common `WhnfComputation::from_root` path must not open hidden or
nested access merely to allocate the cell. Retain a minimal initial demand
seed containing its one `RuntimeValueRoot` and scalar entry metadata. Promote
that seed to `ManagedWhnfRoot` on the first poll, when the caller already
provides `EvaluationValueAccess`. Constructors which already receive matching
access may allocate the managed cell directly. The steady-state target is one
demand root regardless of frame count; the source checkpoint and the brief,
publication-safe overlap between a seed root and its replacement cell are not
violations of that target.

Do not preserve access-free inspection by reaching back through a weak domain
observer. Store scalar runtime provenance next to the managed root. Replace
the diagnostic-only `application_frame_pending` deep inspection with a small
edge-free poll observation recorded while state is already borrowed. Fold
`source_owner` into seed or structured construction, or mutate it only through
an explicitly access-qualified operation.

##### W6G.3a — Baseline and boundary inventory

Before changing representation, measure root registration/removal, durable
checkpoint reconstruction, value duplication, continuation traversal, and
managed-access traffic across repeated yields and dependency boundaries with
both small and large frame stacks. If the existing test interface cannot count
root traffic exactly, add a test-only observation rather than inferring it
from timing.

Record the exact constructor, observer, and modifier inventory. Classify each
entry as source-entry, unstructured demand seed, or access-qualified
structured demand. In particular, account for `from_root`, source-result
installation, application/static-access construction, runtime provenance,
`with_source_owner`, and diagnostic application-stage inspection. Latch the
current results, exact work-budget consumption, and root/conversion counts
before the representation changes.

W6G.3a completion record, 2026-09-18: a source-backed AST inventory now latches
118 production boundary occurrences and their exact declaration fingerprint.
The current boundary comprises 91 direct `from_root` seed calls (including the
internal promise-root delegation), two promise-root adapters, six
access-qualified application constructors, one access-qualified static-access
constructor, twelve `with_source_owner` modifiers, one scalar runtime
observer, and one diagnostic deep observer. The separate source-entry path has
one constructor, pending-source probe, result installation, and result
projection. Its result still returns to the synchronous source evaluator; it
does not participate in durable demand conversion today. W6G.3d.1 owns that
deliberate transition rather than W6G.3b or W6G.3c acquiring it accidentally.

Test-only computation accounting measures the representation work that the
collector's existing monotonic root-registration counter cannot see. For a
checkpoint with `F` two-value frames and `V = 1 + 2F` rooted value positions,
an initial dependency publication, zero-budget yield, and unchanged dependency
publication perform three reconstructions and register `3V` replacement roots.
A following terminal poll projects the state but publishes no fourth
checkpoint. The one-frame fixture therefore visits ten projected and nine
rooted value positions; the 32-frame fixture visits 196 projected and 195
rooted positions. Both visit each retained continuation three times in each
direction, consume exact budgets, preserve the exact wait and result, open one
managed-access region per direct poll, and return to the original live-root
count after the computation is dropped and collection runs. The source-entry
fixture confirms that source ownership and result installation leave all
demand-conversion counters untouched. No production representation or
semantics changed in this checkpoint.

##### W6G.3b — Borrowed canonical-state driver

Refactor the regional driver so its production loop can operate on the
canonical `WhnfState` through an opaque, region-authorized mutable view. Keep
owned `RegionalWhnfWork` for net claims and other true ownership transfers,
but do not require a by-value state handoff merely to poll a managed cell.
Retire the production need for whole-state `RegionalWhnfStep::Continue` if the
inventory confirms that every production transition already mutates or
delegates in place; retain test coverage for the intended transition forms.

Prove before introducing the cell that the borrowed and owned entry paths
produce identical results, boundary identities, and exact budget use. A
forced multi-frame yield must preserve the same state containers and must not
perform a durable projection or root walk.

W6G.3b completion record, 2026-09-18: `RegionalWhnfState` is now the opaque,
access-lifetime-bound mutable view of canonical `WhnfState`. The regional
driver borrows that view while `RegionalWhnfWork` remains the ownership wrapper
used by net claim/publication paths. The driver no longer accepts an optional
owner slot, and the whole-state `RegionalWhnfStep::Continue` transition has
been removed; reducers mutate retained containers in place and return only
focus, boundary, or terminal dispositions. Forced multi-frame yield preserves
every container address and consumes the exact budget without registering a
root or touching durable projection/reconstruction accounting. Paired owned
and borrowed fixtures preserve the same exact wait identity and budget split.
Production semantics and durable ownership remain unchanged until W6G.3d.

##### W6G.3c — Managed cell, root, and access foundation

Introduce `ManagedLazyCheckpointCell { state: Mutex<WhnfState> }` and a narrow
`ManagedWhnfRoot`/access wrapper. The wrapper owns scalar runtime provenance,
projects the root only beneath matching access, centralizes same-runtime
checks, and keeps raw collector operations out of evaluator code. The managed
family has passive destruction. Its `Trace` implementation locks only in the
collector's quiescent tracing context and delegates to the same exhaustive
canonical visitor used by `NetWhnfState`; do not introduce another frame enum
or edge walk.

The mutex is held for the complete bounded evaluator quantum described by
W6G.3e, not merely while loading and storing a detached state. That is safe for
the current reference collector because collection obtains heap-wide mutator
exclusion before invoking `Trace`: a tracer can never wait for a suspended
mutator which owns this mutex. This is deliberately a stop-the-world
assumption, not a claim that the same `Trace` implementation is ready for
concurrent marking. The concurrent-GC plan owns an explicit lock-bearing
managed-state gate and the likely migration of machine-adjacent WHNF state to
`RootFrame<WhnfState>` before concurrent marking is enabled.

Define unwind behavior rather than inheriting mutex poisoning accidentally.
The state is always structurally installed, so tracing after an evaluator
unwind must still be able to visit it. Ordinary repolling may report the
existing internal-failure policy, but collection must not lose edges merely
because the mutex carries a poison marker. Add focused construction,
same-runtime rejection, edge-transition-probe, forced-collection, and
poison/unwind traceability tests before changing production ownership.

W6G.3c completion record, 2026-09-18: `ManagedLazyCheckpointCell` now owns one canonical
`WhnfState` behind its representation mutex, while `ManagedWhnfRoot` carries
only the registered root and scalar runtime provenance. Projection requires
matching `EvaluationValueAccess`, checks both the scalar runtime and collector
root provenance, and returns a thread-bound access wrapper. Its only mutation
surface holds the mutex and routes the complete pre/post canonical edge walks
through `with_managed_edge_state_transition`; no raw collector operation or
mutex escapes evaluator code. The family has an explicit passive-destruction
record and uses the shared managed-slot policy. The collector admission trait,
drop record, and slot policy are now crate-visible—not public—so this
evaluator-owned managed family does not have to reverse the `core`/`eval`
dependency.

The reference-collector `Trace` uses the canonical edge visitor, rejects an
unexpected busy mutex under stop-the-world quiescence, and recovers a poisoned
guard solely for tracing. Ordinary state inspection or repolling reports that
poison instead of silently resuming partially unwound evaluator work. Focused
tests cover construction, foreign-runtime rejection, exact leaving/adding
edge probes, forced collection and retirement, and evaluator unwind followed
by successful poisoned-state tracing. The managed root remains foundation-only
under a named dead-code allowance until W6G.3d selects production promotion and
construction sites.

##### W6G.3d — Construction and source promotion

Migrate durable demand construction in two separately committed checkpoints.

###### W6G.3d.1 — Seed and source promotion

Keep the common access-free `from_root` path as one minimal seed and promote
it exactly once on first poll. Keep source-entry ownership unchanged and
promote an installed source result only when its demand is first polled.
Publication installs the managed cell before retiring the seed root.

W6G.3d.1 completion record, 2026-09-18: access-free `from_root` now retains
one `RuntimeValueRoot` plus optional scalar source ownership in a dedicated
seed checkpoint. Its first poll allocates and publishes one
`ManagedWhnfRoot` while the seed remains rooted, then retires the seed only
after replacement publication. Repeated yield and dependency polls retain
that same managed root without projection, reconstruction, or new root
registration; one-frame and 32-frame fixtures both retain exactly one live
demand root.

The source-entry checkpoint still owns its original lazy and separately
installed result. Installation itself performs no conversion. Its first
demand poll promotes the result into canonical state with the lazy identity
as `source_owner`, after which the source result is no longer projected
through the synchronous evaluator. The managed cell supplies the minimal
in-place poll path needed to make promotion usable; W6G.3e still owns the
complete boundary, scheduler, cancellation, and unwind matrix. A reducer
unwind poisons ordinary repolling but leaves the structurally installed state
traceable, matching the policy established in W6G.3c.

###### W6G.3d.2 — Access-qualified structured construction

Allocate the cell directly from application, static-access, and other
constructors which already hold matching access. Carry entry metadata into
canonical state at construction. Replace access-free deep observations with
the scalar poll observation selected by W6G.3a.

The checkpoint must prove that no constructor opens hidden/nested value
access, no source result is prematurely converted, no steady-state demand
retains both representations, and every promotion remains safe under forced
collection. Update the constructor inventory after each step rather than
combining the whole migration into one unreviewable edit.

W6G.3d.2 completion record, 2026-09-18: application and static-access
constructors now build raw canonical focus/frame edges beneath their caller's
existing `EvaluationValueAccess`, allocate exactly one `ManagedWhnfRoot`, and
carry `source_owner` into that cell at construction. The two producer helpers
which previously appended ownership through `with_source_owner` now supply it
as constructor metadata; that modifier remains confined to minimal
access-free seeds.

`application_frame_pending` now reads only the scalar observation stored
beside the managed root. Each managed poll refreshes that observation from
the canonical state before releasing the cell lock, so reflection
contextualization no longer walks durable application frames without access.
The constructor checkpoint proves access depth remains one, one root is
published per structured demand, legacy conversion counters remain zero, and
raw frame edges survive forced collection after their former owner is
dropped. Ordinary and `aggressive-gc-verification` focused suites cover both
constructors. The source-entry fixture continues to prove that merely
installing a result performs no conversion; first demand alone performs its
one-time promotion.

##### W6G.3e — Aggregate poll transition

Polling projects the single managed root beneath matching access, locks its
cell, and performs one bounded callback-free quantum directly against the
canonical state. Wrap that quantum in one
`with_managed_edge_state_transition`: its leaving visitor sees the complete
pre-quantum state and its adding visitor sees the complete post-quantum state.
Focus replacements and frame pushes/pops inside the quantum must not each
trigger a full-state walk. This is compatible with the current stop-the-world
collector and preserves the future SATB edge boundary without designing the
concurrent snapshot synchronization here. In particular, the aggregate
before/after visitors do not by themselves authorize a concurrent marker to
wait on the state mutex.

Release the state lock and value access before dependency admission, waiting,
callbacks, scheduler coordination, reflection activation, or host work.
Deterministic fixtures must force ready, yield, dependency, external boundary,
failure, cancellation, and unwind exits; cross-worker resumption; and
collection at publication boundaries. Every nonterminal exit retains one
complete traceable state, exact dependency identity, and exact remaining work
budget. No exit may expose an unlocked empty cell or replay completed work.

W6G.3e completion record, 2026-09-18: every managed computation poll now
projects its one root, holds the cell mutex across one bounded callback-free
driver quantum, and publishes the complete before/after edge sets through one
`with_managed_edge_state_transition`. A deterministic three-step fixture
performs multiple focus and frame edits before returning an exact wait; it
observes one aggregate transition record, exact edge counts, three spent
steps, and one unspent step. No internal focus or frame edit invokes another
collector transition.

The W1C exit fixtures force ready, yield, external, permanent-failure,
owner-retirement/cancellation, and unwind paths; the seed fixture forces an
exact dependency. A new worker-migration fixture suspends one canonical cell,
forces collection, moves the computation to another thread, and resumes its
exact focus/frame state and budget. Post-region orchestration assertions in
`evaluation::whnf` remain the sole dependency-admission boundary. The cell is
structurally `Mutex<WhnfState>`, never an optional take/replace slot, so no
exit can expose an unlocked empty representation. Focused ordinary and
aggressive-GC suites cover the added transition and migration fixtures.

##### W6G.3f — Compatibility retirement and accounting

Delete `DurableWhnfState`, its durable continuation/frame mirrors, regional
projection, and root-per-field reconstruction once all demand paths use the
cell. Update the root-publication, durable-owner, managed-access, raw-value,
and WHNF inventories wherever the representation changed. Run focused suites
in ordinary and `aggressive-gc-verification` modes, including forced
suspension after a representative child demand.

Record before/after root and conversion cost for both small and large frame
fixtures. The completion evidence must distinguish the expected temporary
seed-to-cell publication overlap from steady-state ownership and demonstrate
one steady-state demand root independent of retained frame count. The
concurrent-GC plan continues to own comparison with a trace-immediate
`RootFrame`; W6G.3 neither requires nor approximates that later facility.

W6G.3f completion record, 2026-09-18: the unreachable `LegacyDemand` variant,
`DurableWhnfState`, durable frame/continuation mirrors, regional projection,
root-per-field reconstruction, and their test-only conversion counters are
deleted. `WhnfComputation::poll_in` now accepts only a promoted
`ManagedDemand` and mutates that one canonical cell. A source-backed
regression rejects reintroduction of any retired compatibility declaration or
publication function.

The one-frame fixture retains two frame values, so the former representation
required three steady roots (focus plus two frame positions). The 32-frame
fixture retained 64 frame values and therefore required 65 steady roots. Each
nonterminal repoll also visited every semantic position during both projection
and reconstruction: 6 position visits and 2 continuation visits for one frame,
or 130 position visits and 64 continuation visits for 32 frames. Both fixtures
now retain exactly one `ManagedWhnfRoot`; repolls perform zero root-per-field
projection, reconstruction, or container copying and publish one aggregate
edge transition. The first seed promotion may briefly overlap its input root
with the newly constructed managed root until replacement drops the seed, but
all steady nonterminal states own one root independent of frame count.

The root-publication, durable-owner, recursive-identity, managed-access,
raw-value, persistent-edge, and WHNF inventories were rerun and reconciled.
Focused ordinary and `aggressive-gc-verification` suites cover small/large
frames, source promotion, structured child frames, exact suspension,
collection, unwind poisoning, and cross-worker resumption. Comparison with a
trace-immediate `RootFrame` remains assigned to the concurrent-GC plan.

#### W6G.4 — Measured residual compatibility and scheduling overhead

**Status:** complete on 2026-09-23. The investigation, W6G4R-001A-G route
repair, and W6G4R-002 private-ID hasher experiment are complete. The remaining
poll-time generation-accounting question is extracted as W6G4R-003 into
explicit Phase W9 below and does not hold W6G.5 open.

Investigate the bounded performance regression accepted by W4E after W6G.1
has established role-specific pumping and completed W6G.3 has removed
root-per-field WHNF checkpoints. The current bounded W5 standard-effect path
is the fixed baseline for this measurement; regional effect fusion has been
extracted and must not be pulled back in merely to explain or repair this gap.
The duplicate-symbol direct-assembly fixture takes approximately 12.6 to 12.9
seconds after W4E, versus approximately 8.1 to 8.5 seconds at `7fed99e`, even
though net-driver work is comparable (159,322 versus 159,994 work items).

Measure the cost after each relevant compatibility or admission boundary is
removed. Attribute the W6G.1 selector transition separately from the temporary
same-session scan it retires; do not restore shared worker/client work stealing
merely because one timing is favorable. If the gap remains, add static
profiling for machine selection, causal dependency traversal, claim, poll,
release, managed-access entry, requeue, root publication, and request dispatch
before changing implementation policy. Determine whether the overhead comes
from scheduling several thousand `NetWhnfMachine` polls, managed-access
traffic, causal-chain rediscovery, or another measured source.

Do not reintroduce the rejected per-session running-machine index: retired work
does not remain in `work_by_session`, and the measured experiment produced no
improvement. Close W6G.4 by restoring comparable fixture cost or by recording
a measured, justified residual with ownership assigned to a later performance
phase.

NC6 measurement, 2026-09-16: callable-WHNF spill did not enlarge this debt.
Against its NC0 revision `08f7c09`, the exact duplicate-symbol fixture was
13.11s before and 13.16s after; the successful repeated-split ELF fixture was
53.01s before and 52.12s after. The minimized semantic/driver signature also
remains unchanged. Treat the timings only as corroboration, but do not assign
the preexisting `7fed99e` gap to callable checkpoints.

The W6G.4 investigation localizes the remaining cost to repeated exact
producer-chain rediscovery. The current duplicate-symbol fixture invokes
`prioritized_task_for` about 18,800 times, visits roughly 2.8 million edges,
reaches depth 573, and allocates about 148 MB across 220,000 temporary
`Vec`/`HashSet` blocks. Callgrind attributes about one quarter of all current
release instructions to randomized hashing of private IDs. W4E already reached
depth 572, correcting its old statement that this fixture's maximum depth was
four; W6 enlarged rather than introduced the pathology. Interaction-net work
remains comparable to the pre-W4 baseline.

The first repair candidate is one state-aware exact-target traversal and claim
under the existing mutation admission and coordinator-state lock, analogous to
`causal_background_probe_locked`. It must deliberately resolve the current
foreground/deepest versus background/queued-ancestor ordering asymmetry and
latch forced tests before changing production behavior. Container or hasher
micro-optimizations are not an adequate primary repair. Full evidence and the
verification matrix are recorded in
[`ResumableWhnfW6G4_2026-09-23.md`](../reviews/ResumableWhnfW6G4_2026-09-23.md).
That review defines W6G4R-001A-G: first make complete discovery one guarded
cold-path operation, then retain a validated foreground route zipper so common
yield, block, completion, and contention transitions do not restart discovery
at the client root. Full rediscovery remains the authoritative fallback.

The closing measurement records 19,499 fast route handoffs against 9,356
fallbacks, reducing Callgrind instructions by 21.10% and allocated bytes by
35.35% relative to the cold-path repair without changing the exact
interaction-net signature. Remaining complete searches are entirely assigned
to the broad contention class rather than background-root absence. The review
therefore owns two narrower follow-ups: W6G4R-002 trials a deterministic
hasher only for trusted private-ID loop sets, and W6G4R-003 separates
poll-local generation movement from genuine release interference before any
further route-policy change.

W6G4R-002 retains that private hasher after reducing the fixture from
3,554,190,609 to 3,339,481,894 Callgrind instructions (6.04%) while preserving
the exact semantic/driver signature and diagnostic. Its private alias covers
only retained exact-route membership and the background exact-producer
probe's temporary cycle set; persistent scheduler indexes and user-keyed
collections remain randomized. W6G4R-003 remains explicitly deferred to the
post-W6G Phase W9 checkpoints below.

#### W6G.5 — Phase closure and post-W6G review

##### W6G.5a — Integrated verification and accounting

**Status:** complete on 2026-09-23. This is a closure and evidence pass, not another
representation transition. W6G.1-W6G.4 were implemented and tested along
their individual paths; W6G.5a proves that their final boundaries compose and
produces one current accounting baseline for the mandatory review in W6G.5b.

The four inputs have deliberately different owners:

| Input | Contract to preserve | Authoritative evidence | Work excluded from W6G.5a |
|---|---|---|---|
| **W6G.1** | Foreground clients poll only their demand and its exact causal producers. Workers and runtime background pumps start only from autonomous reflection roots or sparks and follow their exact dependencies. Lazy checkpoints remain owned by the lazy rather than by a route or session. | Coordinator/evaluation forced-order fixtures, the W0B census, and the lazy-producer/managed-edge inventories. | A public resumable `Evaluation` facade and new work-stealing policy. |
| **W6G.2** | The bounded W5 standard-effect driver remains the reference implementation. | Existing reflection request resumption/replay fixtures and the explicit extraction record. | Regional pure-effect fusion, now owned by [`PureEffectAccessFusion_2026-09-23.md`](PureEffectAccessFusion_2026-09-23.md). |
| **W6G.3** | One managed checkpoint owns the complete pure-WHNF state, independent of frame count; polls exchange that state in place rather than publishing one root per frame. | The WHNF-checkpoint boundary inventory and forced small/large, collection, unwind, and cross-thread fixtures. | `RootFrame` or concurrent-marking refinements. |
| **W6G.4** | Exact-route retention and the private-ID hasher are performance mechanisms only; full rediscovery remains the authoritative fallback and semantic output remains unchanged. | The exact interaction-net signature, duplicate-symbol diagnostic, route counters, and the closing Callgrind baseline recorded above. | Poll-local generation accounting, now owned by Phase W9. |

Historical completion records remain records of their checkpoints. Do not
rewrite an older W0B or W6F baseline merely because W6G moved it later. Append
a later-state reconciliation, and treat every unexplained count or fingerprint
change as drift until its source change and owner are identified.

Partition closure as follows.

###### W6G.5a.0 — Closure ledger and exact-manifest reconciliation

1. Snapshot the implemented boundaries before changing tests or baselines.
   The current exact anchors are:
   - W0B: 207 occurrences with fingerprint
     `17_311_633_654_805_980_353`, including 21 coordinator boundaries and 51
     orchestration handoffs;
   - parent D.2c: no W6 declarations and exactly six declarations assigned to
     `W8ValueCompatibility`;
   - one compile-exhaustive managed WHNF-checkpoint API boundary, currently
     covering 73 exact call sites; and
   - the exact lazy-task producer families and external-source boundaries.
2. Run and reconcile the source-backed W0B, raw-value/D.2c,
   WHNF-checkpoint, lazy-producer, root-publication, durable-owner,
   recursive-identity, persistent-edge, active-owner, and evaluator-access
   inventories. A baseline changes only with a named source disposition; it
   is never relatching by inspection alone.
3. Append a W6G.5a completion ledger which says which W6G section owns every
   changed declaration. Where an older review reports a now-stale aggregate
   (for example, the post-W6F count before W6G), add a later-disposition note
   or link rather than altering the historical finding.

At minimum, the focused exact-manifest set includes
`whnf_suspension_and_recursion_census_is_exact`,
`reflection_has_no_unowned_recursive_whnf_demand`,
`d2c_w6_checkpoint_manifest_is_exact`,
`durable_whnf_checkpoint_boundary_is_exact`,
`checkpoint_boundary_roles_are_compile_exhaustive`, and
`lazy_task_work_families_are_exact`. The broader inventory tests above remain
part of the ordinary full repository gate.

###### W6G.5a.1 — Forced scheduler ownership and lifecycle matrix

Exercise the following cases with barriers, latches, or explicit claim/release
steps. Repetition under the thread scheduler is not evidence for either
ordering.

1. **Selection roles.** A foreground client may claim only its root or an
   exact causal producer, including one owned by another session. A worker or
   runtime background pump must reject foreground client demand. A reflection
   root and a spark may each reach their exact descendant, while independent
   same-session machines remain concurrently claimable.
2. **Route continuity.** Force child publication before subscription and
   subscription before publication; dependency completion before the parent
   polls and parent reblocking before dependency completion; another poller
   holding the anticipated child; parent wake/reblock at a new epoch; and
   contention invalidating a retained route. Each case must either retain the
   exact route or fall back to authoritative rediscovery without losing a
   wake or claiming unrelated work.
3. **Observer loss.** Close the first observer session while another client,
   reflection task, or spark still demands the lazy. Separately drop the last
   route, collect, then attach a new subscriber. The lazy-owned checkpoint
   must survive; the retired route must not keep a session alive; and
   completed prefixes must not replay.
4. **Pump and drain scope.** A blocking client wait must not pump unrelated
   reflection work. Session drain may follow the session's exact
   cross-session dependency but leaves independent roots for runtime drain.
   Bounded background pumping may advance reflection work, reports contested
   causal work without waiting, and excludes foreground clients and sparks.
   Runtime quiescence and zero-worker behavior retain their documented
   meanings.

Prefer the existing coordinator and evaluation fixtures which force these
orders. Add a fixture only where the matrix exposes a missing order; do not
replace a deterministic fixture with a repeated stress run.

###### W6G.5a.2 — Checkpoint, effect-path, and performance integration

1. Re-run W6G.3's exact checkpoint inventory and forced small/large-frame,
   source-promotion, structured-child, collection, unwind-poisoning, and
   cross-worker-resumption cases in ordinary and
   `aggressive-gc-verification` modes. Confirm that every steady nonterminal
   state still owns one managed checkpoint regardless of continuation depth.
2. Force suspension immediately after representative **pure** child demands
   (application, access/path conversion, and one structural builtin child)
   and representative **standard-effect** child demands (request decoding,
   specialization-owned argument preparation, and list-effect
   sequence/cut/fix). Count the completed prefix or host callback. Resumption
   must neither replay it nor convert the bounded W5 effect path into W6G.3's
   pure regional owner.
3. Run `scripts/check-interaction-net-profiling.sh` and compare the exact
   semantic/driver signature and duplicate-symbol diagnostic with the W6G.4
   record. Preserve the closing performance evidence—3,339,481,894 Callgrind
   instructions, 19,499 fast route handoffs, and 9,356 contention
   invalidations—as an attribution baseline, not as a timing gate. A fresh
   Callgrind run is required only if W6G.5a changes production scheduling or
   route code.
4. Confirm by source and plan links that pure-effect fusion remains in its
   independent deferred plan and generation-accounting work remains in W9.
   W6G.5a must not implement either in the name of closing verification.

###### W6G.5a.3 — Integrated gates and handoff to review

Run affected focused suites first in ordinary and
`aggressive-gc-verification` modes, then the interaction-net profiling script
and the routine repository gates:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Record exact commands and results in the completion ledger. Update current
architecture/agent notes only for discovered drift; broad compatibility-doc
retirement remains W8C. W6G.5a is complete only when:

- every exact inventory delta has a named owner and reviewed baseline;
- every disputed scheduler ordering is forced, not inferred from repetition;
- W6G.1, deferred W6G.2, W6G.3, and W6G.4 retain the separate ownership in
  the table above;
- W7, W8, and W9 have explicit ownership of every accepted residual; and
- the evidence is sufficient to begin W6G.5b without reconstructing the
  month-long W6G implementation history.

W6G.5a completion ledger, 2026-09-23:

| Checkpoint | Result |
|---|---|
| **5a.0 inventories** | All 116 source-backed inventory tests pass. W0B remains 207 occurrences at fingerprint `17_311_633_654_805_980_353`; D.2c remains closed for W6 with six `W8ValueCompatibility` declarations; the managed checkpoint boundary remains 73 exact call sites. One intentional test-only access delta removes the delayed promise-root region from `synchronous_client_demand_does_not_wait_for_unrelated_worker_progress`, reducing the `src/evaluation/tests.rs` mutator-introduction count from 16 to 15. |
| **5a.1 scheduler matrix** | Existing deterministic fixtures cover all listed selection, route, observer-loss, pump, and drain orders. The ordinary and aggressive-GC focused runs pass. No scheduler policy or production coordinator code changed. |
| **5a.2 state/effect/performance integration** | Nineteen focused pure-WHNF and bounded standard-effect filters pass across the ordinary and `aggressive-gc-verification` runs; eighteen execute in both modes, while `collection_between_polls_preserves_only_the_installed_checkpoint` is intentionally aggressive-only. The profiling script passes its 21 named regressions without semantic/driver signature drift. Production route code did not change, so no new Callgrind run was warranted. |
| **5a.3 repository gates** | `cargo fmt --check`, all-target/all-feature Clippy with warnings denied, and the complete workspace test suite pass. The main library suite reports 1,839 passed and two ignored tests; every remaining workspace and integration suite also passes. |

The aggressive run exposed three related test-fixture ownership defects which
ordinary collection timing had hidden:

1. the specialization replay fixture returned a raw application lazy from one
   mutator region and rooted it only during later effect-task admission;
2. the counted client-lazy fixture split lazy allocation from root publication
   and four callers supplied unrooted promises; and
3. synchronous-client and spark fixtures likewise opened a later region to
   root already-unprotected managed values.

The repairs are deliberately test-side. Fixtures now construct public rooted
applications, use the existing atomic rooted-lazy/rooted-promise helpers, and
admit sparks through `spark_root`. The access inventory latches removal of the
one redundant delayed root introduction. These changes preserve all forced
scheduler orderings while making the fixtures obey the same regional handoff
rule as production code.

The exact verification entry points were:

- Scheduler filters: `foreground_route`,
  `lazy_producer_completion_before_client_subscription_requeues_exactly_once`,
  `client_subscription_before_lazy_producer_receives_one_exact_wake`,
  `reflection_root_claims_only_its_exact_descendant_under_contested_orders`,
  `spark_root_claims_exact_deferred_dependency_after_block`,
  `ready_selection_allows_independent_same_session_machine_claims`,
  `running_deferred_machine_does_not_serialize_same_session_client_admission`,
  `worker_and_runtime_pump_selectors_reject_foreground_client_demand`,
  `closing_first_observer_preserves_another_sessions_lazy_route_demand`,
  `last_lazy_route_demand_retires_without_losing_its_checkpoint`,
  `claimed_lazy_route_survives_last_demand_and_accepts_new_subscriber`,
  `client_and_spark_share_a_lazy_checkpoint_after_client_route_loss`,
  `client_and_background_reflection_share_lazy_progress_after_first_session_closes`,
  `reflection_completion_activation_and_first_session_close_have_both_orders`,
  `task_owned_promise_lazy_cycle_fails_in_both_publication_orders`,
  `synchronous_client_demand_does_not_pump_unrelated_reflection_work`,
  `session_drain_follows_an_exact_cross_session_dependency`,
  `logger_shaped_session_drain_leaves_independent_producer_client_and_spark_for_runtime`,
  `bounded_background_pump`,
  `queued_foreground_lazy_route_is_not_background_pump_work`, and
  `zero_worker_executor_drops_sparks_without_forcing_them`.
- Checkpoint/effect filters:
  `small_and_large_seed_promotions_use_one_managed_root`,
  `structured_constructors_publish_canonical_state_under_existing_access`,
  `borrowed_multi_frame_yield_preserves_containers_without_projection_or_roots`,
  `forced_collection_traces_every_edge_owned_by_the_managed_state`,
  `poisoned_state_remains_traceable_but_repolling_observes_poison`,
  `one_poll_aggregates_every_focus_and_frame_edit_into_one_edge_transition`,
  `managed_checkpoint_resumes_on_another_worker_after_collection`,
  `external_boundary_publishes_the_complete_checkpoint_before_access_closes`,
  `unwind_poison_is_reported_without_reentering_the_reducer`,
  `collection_between_polls_preserves_only_the_installed_checkpoint`,
  `w6g1f3i_application_checkpoint_survives_promise_and_route_loss`,
  `w6g1f3i_static_access_checkpoint_survives_promise_and_route_loss`,
  `object_checkpoint_preserves_linearization_prefixes_across_route_loss_and_collection`,
  `list_effect_sequence_and_cut_checkpoints_survive_deferred_chunks_and_route_loss`,
  `list_effect_fix_checkpoint_constructs_and_assigns_one_promise`,
  `request_decode_resumes_the_exact_lazy_payload_without_replay`,
  `specialization_host_activity_is_not_reentered_after_owned_demand_suspends`,
  `fused_standard_chains_resume_at_the_cooperative_budget`, and
  `resumable_reflection_decode_consumes_one_application_checkpoint_after_resumption`.

```sh
cargo test -q --lib inventory
cargo test -q --lib <each focused filter above>
cargo test -q --features aggressive-gc-verification --lib <the same fixtures>
scripts/check-interaction-net-profiling.sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

W6G.5a leaves the ownership boundaries unchanged: pump-role policy is W6G.1,
regional pure-effect fusion remains in its independent deferred plan,
aggregate pure-WHNF state remains W6G.3, and W6G4R-003 remains Phase W9 work.
The phase is ready for W6G.5b.

##### W6G.5b — Mandatory post-W6G review

**Status:** complete on 2026-09-24. See the dated
[W6G review](../reviews/ResumableWhnfW6G_2026-09-24.md).

Audit the implemented phase before W7. Account for measured scheduler,
managed-access, root-publication, checkpoint-conversion, and request-dispatch
traffic; the retained bounded standard-effect driver and explicit deferral of
regional fusion; aggregate checkpoint ownership and tracing; and the final
disposition of the temporary same-session admission rule. Review W7-W8 against
the resulting implementation for drift, checkpoint size, and newly obsolete
compatibility work. Record any accepted residual performance gap with a
concrete later owner rather than leaving it implicit in W6G. W6G4R-003 is
already owned by Phase W9; verify that W6G.5 neither duplicates nor silently
widens that work.

The review found no new production correctness defect. It corrected one stale
architecture paragraph, distinguished dynamically measured scheduler traffic
from static access/root/checkpoint inventories, assigned broader post-value-
representation traffic measurement to the independent pure-effect-fusion
plan, and partitioned W7-W8 around the exact W0B and six-item W8 compatibility
manifests. The temporary same-session admission rule is fully retired: demand
sessions are lifecycle/reporting groups, while exact work claims and causal
dependencies supply concurrency authority.

### Phase W7 — Stack and Budget Closure

#### W7A — Recursive-call elimination audit

##### W7A.0 — Disposition and call-graph gate

**Status:** complete on 2026-09-24.

Begin from the exact 207-occurrence W0B baseline and its fingerprint. Extend
the audit so every occurrence has one reviewed W7 disposition:

- budgeted resumable semantic work;
- scheduler/orchestration loop with a bounded poll or explicit wait;
- statically bounded representation plumbing;
- balanced persistent-container traversal with a documented logarithmic
  depth bound;
- a separately owned worklist/trampoline such as cursor-WHNF; or
- one of the exact six `W8ValueCompatibility` declarations.

The existing syntax census detects direct self-calls and loops but does not by
itself prove the absence of mutual recursion. Add an exact call-edge ledger or
a targeted manual/AST census for every semantic evaluator family reachable
from production demand. Do not classify a call as bounded merely because its
current fixture is shallow.

Completion record: W0B's former direct-recursion predicate compared only the
called function's final name. It therefore mistook calls such as `Foo::new()`
inside another `new()` and prelude `drop(value)` inside `Drop::drop` for
recursion. The corrected predicate recognizes only unqualified free-function,
same-implementation associated-function, and `self` method re-entry. This
reduces the exact W0B baseline from 207 to 156 occurrences at fingerprint
`6_487_120_116_923_049_700`; four calls, not 55, are genuinely recursive.

Every occurrence now has one independently fingerprinted W7 disposition: 84
explicit iterative traversals, 49 orchestration/wait boundaries, 19
occurrences inside the six W8 compatibility declarations, and four
unapproved recursive calls. A second AST ledger resolves 1,152 exact
same-module free-function and same-implementation call sites at fingerprint
`6_358_179_092_932_255_754`, computes transitive cycles iteratively, and
agrees on the three recursive functions. Calls through a typed receiver are
not guessed from method names; their semantic demand boundaries remain
covered by W0B's exact evaluator-signal census and compile-exhaustive regional
owners. W7A.1 owns the exact remaining set: `key_value` twice,
`update_dict_path_in`, and `insert_effect_api_path`.

##### W7A.1 — Production semantic stack closure

**Status:** complete on 2026-09-24.

Reduce the W0B manifest to zero unapproved production semantic recursion
outside the exact W8 compatibility set. Any remaining Rust recursion must be
one of:

- statically bounded representation plumbing;
- balanced persistent-container traversal with a documented logarithmic
  depth bound; or
- a separately owned worklist/trampoline such as cursor-WHNF.

No user-controlled semantic recursion may remain implicit on the Rust stack.

Completion record: the four W7A.0 calls belonged to three functions.
Dictionary-key reification now uses an explicit list/dictionary frame stack;
nested dictionary update records its parent dictionaries and reconstructs
them in reverse; and reflection effect-API path insertion uses the same
iterative descend/rebuild shape. Their existing eager semantics are unchanged,
and W7A does not pretend those operations consume the W7C scheduler budget.

The replacements expose six ordinary source loops in place of four recursive
call sites. The exact W0B baseline is therefore 158 occurrences at fingerprint
`3_270_388_107_992_880_254`: 90 explicit iterative traversals, 49
orchestration boundaries, and 19 occurrences wholly inside W8's six
compatibility declarations. There are zero unapproved recursion dispositions.
The exact locally resolved call graph now contains 1,148 edges at fingerprint
`15_426_600_492_285_587_613` and no transitive cycle. W7B owns deliberate
small-stack depth fixtures; this checkpoint closes the production source
shape rather than using an accidental process stack overflow as its test.

##### W7A.2 — Exact W8 handoff

**Status:** complete on 2026-09-24.

Latch the remaining compatibility declarations and their fingerprint as the
only temporary stack-closure exception. Record why none is reachable from the
ordinary resumable production driver. W8A-B must remove that set rather than
relabelling it, and W8D must rerun W0B and the small-stack fixtures before the
whole plan closes.

Completion record: the W7 source gate independently names the same six
`src/eval/value.rs` declarations as D.2c's `W8ValueCompatibility` manifest:
`eval_value`, `eval_value_in`, `eval_lazy_in`, `eval_promised_in`,
`await_deferred_task`, and `deferred_wait_result`. D.2c continues to latch
their declaration/signature fingerprint at `1_485_448_540_290_006_633`; W7
additionally latches the 19 W0B calls, loops, waits, and coordinator boundaries
inside that family at fingerprint `12_624_383_794_632_597_732`.

Every remaining direct `eval_value_in`/`eval_lazy_in`/`eval_promised_in`
signal originates inside those six declarations. No ordinary production owner
enters them: client demand, lazy routes, reflection tasks, sparks, and nets all
enter their resumable owner instead. The exception is therefore test/direct-
compatibility transport rather than a hidden production stack path. W8 must
delete the exact declarations and both manifests; moving a replacement into a
new function is not closure.

#### W7B — Small-stack verification

##### W7B.0 — Harness and depth baseline

**Status:** complete on 2026-09-24.

Introduce one deterministic small-stack thread harness with an explicit stack
size and a source-shaped depth large enough to fail an equivalent recursive
fixture. Keep scale-only variants out of the routine path if necessary, but
retain one bounded routine witness. Repetition is not evidence.

Completion record: W7B uses one 512 KiB named-thread harness and a semantic
depth of 4,096. A subprocess-isolated recursive control keeps an observable
1 KiB frame live across each recursive call and proves that the chosen depth
overflows that stack without risking the main test process. At the same depth,
the callback-free regional WHNF driver completes 4,096 delegations through its
explicit work state. Both are routine bounded witnesses; no repeated run or
uncontrolled scheduler timing is accepted as stack-safety evidence.

##### W7B.1 — Alias, application, and fixpoint chains

**Status:** complete on 2026-09-24.

Run deep lazy-alias, promise-alias, application, and fixpoint fixtures in both
uninterrupted and forced-suspension forms. Force the suspension point and the
resume owner; do not infer it from worker timing.

Completion record: four production-shaped families now run beneath the W7B
small-stack harness. Promise aliases use the full 4,096-node control depth;
producer-heavy lazy-alias, function-application, and constant-function
fixpoint chains use a bounded 512-node routine depth so the fixture measures
stack behavior without adding tens of seconds of linear scheduler setup to
every suite run. Each family has an uninterrupted result witness and an
unassigned terminal-promise witness. The latter is pumped to stable
quiescence, latches the exact one promise subscription, assigns the promise,
and moves the retained client-demand computation to a newly spawned poller
whose thread identity must differ from the suspending poller. No test relies
on a worker race to choose the boundary or resumption owner.

##### W7B.2 — Structural paths, collections, and failures

**Status:** complete on 2026-09-24.

Run deep access-path, key-conversion, collection, and structured-failure
fixtures. Distinguish explicit evaluator worklists from balanced persistent-
container recursion whose logarithmic bound was approved in W7A.

Completion record: 4,096-element routine witnesses now cover static access
paths, nested dictionary-key conversion, strict list conversion, and
structured failure propagation through promise aliases. The static path and
nested-key fixtures place each dictionary layer behind an already-assigned
managed promise. This preserves the semantic depth while making compatibility
edge tracing stop at its intended managed-identity boundary rather than
conflating evaluator stack safety with a raw transitional `Value::Dict`
representation walk. The strict collection uses the balanced list
representation and verifies all 4,096 ordered results, while the failure case
preserves one exact emission and context frame.

The first nested-key witness found a real omission in W7A's source analysis:
`RegionalKeyConversion` and `RegionalKeyList` called boxed children through
typed receivers, a mutual recursion shape the deliberately conservative local
call graph did not guess. The old fixture deterministically overflowed the
512 KiB stack. Key conversion now owns one focus and an explicit vector of
dictionary/list parent frames; each poll still performs the same single
transition, and managed tracing visits the focus and frames iteratively. The
same 4,096-level witness now completes on the selected stack.

##### W7B.3 — Scheduler-owner matrix

Exercise representative client-demand, lazy-route, reflection-hosted, and
spark/deferred owners where the same semantic state may resume on another
poller. Use zero-, one-, and multiple-worker configurations only where they
force a distinct owner boundary; all semantic outcomes and exact checkpoints
must be schedule independent.

#### W7C — Budget and fairness verification

##### W7C.0 — Budget vocabulary decision

Distinguish the inner `EvaluationStepBudget::spent()` count from the
foreground pump's current reservation of a complete task quantum before a
poll. Decide whether the pump allowance is a reservation or an exact consumed-
transition budget, and document any refund/translation rule. All later tests
must use that one vocabulary; do not claim exact spend from a merely reserved
quantum.

##### W7C.1 — Exact checkpoint budget

Prove that a deep computation yields at the selected exact boundary, retains
the same checkpoint, reports the selected spent/reserved counts, and
eventually completes. A zero budget performs no semantic transition or work
claim.

##### W7C.2 — Owner-specific yield and requeue

Ensure client demand, lazy/deferred work, reflection, and sparks requeue an
ordinary budget yield without installing a dependency subscription. Force the
yield/reclaim order and distinguish it from a real lazy, promise, reflection,
or observation dependency.

##### W7C.3 — Fair ready-work scheduling

Force at least two independently ready records around repeated budget yields
and prove that each role's selected queue makes progress without granting
workers foreground authority or granting foreground pumps unrelated work.
Use latches or explicit poll gates, not repeated parallel runs.

##### W7C.4 — Cost accounting

Observe root registrations, allocations, managed-access entries, checkpoint
publications, and poll counts. Treat them as regression diagnostics initially,
except enforce that uninterrupted tail delegation performs no per-step root
registration and that a retained aggregate checkpoint does not republish
roots per frame or per repoll.

W4E supplies the static interaction-net counters and a test-only work-item
fuse for stopping a net at a known pre-completion boundary. Reuse those as
measurement and inverse-test oracles, but do not mistake the fuse for the
production evaluation budget: W7C must connect the real task/quantum budget to
the resumable owner, prove fair requeue among ready work, and show that the
same checkpoint eventually completes after one or more budget yields. Keep
the focused profiling fixtures in
`scripts/check-interaction-net-profiling.sh` rather than rerunning the complete
ordinary suite under instrumentation.

### Phase W8 — Compatibility Retirement and Documentation

#### W8A — Retire retryable recursive-halt transport

##### W8A.0 — Exact compatibility census and halt decision inputs

Begin from the D.2c `W8ValueCompatibility` manifest: six `ValueDemand`
declarations at fingerprint `1_485_448_540_290_006_633`. Inventory their real
production and test callers before deletion. Use that caller census—not the
current union representation—to decide whether `EvaluationHalt` becomes a
permanent-failure carrier, aliases `EvaluationFailure`, or remains a narrow
compatibility name.

##### W8A.1 — Remove recursive wait transport

Remove `await_deferred_task`, the recursive blocked/unassigned-promise paths,
and adapters which translate a bare `EvaluationHalt` after losing evaluator
state. Apply the `EvaluationHalt` disposition selected from W8A.0's remaining
callers. Do not preserve the union solely for obsolete call sites.

W4E removed the synchronous nested-evaluation path from a claimed
`ApplyArity` operator pair. Compatibility retirement must not reconstruct that
path: over-application continues through `LazySource::Application` or an
equivalent durable WHNF child owner whose argument cursor survives a yield.
Retain the W4E exact-signature and pre-completion inverse fixtures; replace
their test-only work fuse only when W7's general budget mechanism can force the
same boundary deterministically.

#### W8B — Retire direct evaluator compatibility

##### W8B.1 — Migrate direct-wrapper callers

Move remaining tests and internal helpers to the shared synchronous driver or
an explicitly bounded submachine fixture. Do not keep a recursive evaluator
solely because a test wants a concise assertion.

##### W8B.2 — Delete the gate and wrappers

Remove the direct evaluator constructor, `eval_value`/`eval_value_in` family,
and wrappers made unnecessary by the shared synchronous driver. Reconcile
`EvaluationMachinePoll`, client demand, spark, and reflection result
boundaries with the final yielded/pending protocol.

##### W8B.3 — Close exact inventories

Require the D.2c W8 compatibility manifest to be empty, remove its obsolete
checkpoint enum/fingerprint, and rerun W0B. No new declaration may inherit the
compatibility disposition as a replacement.

#### W8C — Inventory and documentation closure

Update:

- `docs/architecture/evaluation.md` with the current WHNF submachine flow;
- `docs/architecture/reflection.md` with hosted WHNF request decoding;
- `docs/agent_context/evaluation.md` with resumption and budgeting invariants;
- `src/README.md` with final module ownership;
- the parent D.2c completion records and raw-value manifests;
- the persistent-edge migration and Gate G3 prerequisites; and
- the GC ownership ledger for every new durable checkpoint root.

Remove chronological implementation detail from current architecture docs;
retain it in this plan's completion records and later review.

#### W8D — Full verification and review

Run:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
cargo test -q --features aggressive-gc-verification
```

Run the selected Miri/model-checking subsets if the implementation adds new
unsafe or synchronization code. Ordinary WHNF state should require neither.

Then perform a dated review against every invariant and acceptance criterion
in this plan, including future-phase drift in D.2d-D.2g, P3-P5, and Gate G3.
Repeat the W7 small-stack and budget fixtures after compatibility deletion so
W7A.2's temporary exception is actually closed.

Review Phase W9 against the then-current coordinator before beginning it. Its
release-accounting repair is deliberately sequenced after semantic stack,
budget, and compatibility closure so this performance residual does not
distort those transitions.

### Phase W9 — Exact-Route Poll and Release Accounting

**Status:** planned. This phase owns W6G4R-003 in full. It is a measured
post-W6G performance repair, not unfinished W6G semantics.

The W6G4R-002 baseline is 3,339,481,894 Callgrind instructions for the exact
source-shaped duplicate-symbol fixture. It records 19,499 fast route handoffs,
9,356 `Contention` invalidations, and the exact semantic/driver signature in
the W6G.4 review. Preserve that fixture, signature, diagnostic, and route
profile as the comparison point.

The current route stores the coordinator generation produced by exact claim.
The claimed machine then polls without holding coordinator locks or mutation
admission, as required. `ExactRouteReleaseTracker` is constructed only after
that poll, when the work kind begins its guarded release. Consequently one
`Contention` result currently conflates three distinct conditions:

1. the caller's local route no longer names the released work;
2. the global work generation moved while the claimed machine was polling,
   whether from causally owned work or a concurrent participant; and
3. an unaccounted coordinator mutation interleaved with the guarded release.

No checkpoint may make the caller-local route authoritative, hold a
coordinator lock or mutation guard across machine polling, or accept a global
generation mismatch without proving that the remembered exact path remains
valid. The guarded cold traversal remains the final fallback.

#### W9A — Classification-only baseline

Split the current broad profile reason without changing route decisions:

- current-work mismatch or missing exact release;
- generation movement between exact claim and guarded release start;
- mutation not accounted for by `ExactRouteReleaseTracker` while release is
  guarded; and
- a retained route rejected by its ordinary frame/dependency validation.

Carry only the claim/release observations needed to classify these cases.
Keep counters test-only or under the existing static profiling boundary; do
not add permanent per-work scheduler metadata. Re-run the exact fixture and
record non-overlapping counts whose sum reconciles with the 9,356 baseline
invalidations. If the baseline no longer reproduces after W7-W8, stop and
re-profile before designing a repair.

#### W9B — Forced ordering and relevance matrix

Latch each ordering with barriers rather than repeated runs:

1. unchanged claimed work with no coordinator mutation during poll;
2. a poll which causally publishes or advances coordinator work itself;
3. an unrelated participant mutating the coordinator while the poll is
   active;
4. a remembered parent waking or reblocking onto a new producer during the
   poll;
5. current work retiring, being replaced, or otherwise no longer matching the
   caller's route before reconciliation;
6. an interleaving admitted after guarded release starts but before its final
   route observation; and
7. yield, block, terminal return, lazy/promise cycle settlement, and missing
   route-release dispositions for each work family that can be claimed
   exactly.

The tests must prove both classification and current fallback behavior before
implementation changes. Preserve the existing queued-parent, busy-leaf,
cross-session, owner-closure, child-completion, and interleaved-release tests.

#### W9C — Reconciliation design gate

Use W9A-B evidence to select the smallest safe proof boundary. Evaluate at
least these choices:

- reconcile the caller-local route under the release's existing mutation
  admission and coordinator-state lock, after the work disposition is known;
- retain the local disposition but require an exact frame/dependency
  validation on the next claim; or
- keep cold rebuilding for a class whose relevant interference cannot be
  distinguished cheaply.

Prefer guarded local reconciliation when the remembered parent frames,
subscription epochs, dependency keys, current work identity, and final
disposition jointly prove that the exact path is unchanged. Unrelated global
generation movement alone is not a reason to rebuild once that local proof is
available. A parent wake/reblock, route-tip mismatch, retired work, branched
dependency, or unexplained guarded-release mutation must still validate or
fall back authoritatively.

Do not solve this with a global descendant index, a lock spanning poll, a
durable claim on the net or task, or special scheduler authority for the
caller-owned route. If safe reconciliation would require one of those, retain
the conservative fallback and record the measured residual instead.

#### W9D — Implement the selected proof boundary

##### W9D.1 — Claim/release representation

Separate claim generation, guarded-release start/end generations, current
work identity, accounted release mutations, and final disposition in the
internal exact-release representation. Delete ambiguous `Contention`
classification where a more precise cause is available. Keep the
representation private and bounded; it must not become public task status or
semantic state.

##### W9D.2 — Guarded reconciliation

Implement the W9C choice consistently for reflection, deferred, and lazy-route
release. Any path accepted without a cold rebuild must either reconcile while
the coordinator state is protected or deliberately remain pending validation
before its next claim. It must never stamp an unvalidated caller route with the
latest global generation.

Preserve notification and destruction boundaries: callbacks, wake delivery,
machine destruction, and other user-observable work remain outside runtime
locks and mutation admission. Do not duplicate release policy independently
across the three work families when one shared helper can express the proof.

##### W9D.3 — Transitional accounting cleanup

Remove temporary probes and classification scaffolding which no longer serves
tests or ongoing profiling. Retain named fallback counters only when they
describe stable implementation distinctions useful for future diagnosis.
Update coordinator comments so the generations are described as validation
inputs rather than route authority.

#### W9E — Measurement, verification, and disposition

Re-run the source-shaped duplicate-symbol fixture with the same static route
profile, Callgrind method, native debug/release timing, and exact
interaction-net profile used by W6G4R-001G and W6G4R-002. Report:

- handoffs and fallbacks by the new non-overlapping reasons;
- complete searches, visited records, maximum route depth, and retained route
  storage;
- Callgrind instructions against the 3,339,481,894 W6G4R-002 baseline;
- native timings as corroboration only; and
- the exact semantic/driver signature and structured duplicate-symbol
  diagnostic.

Run the forced W9B matrix, affected coordinator/evaluation suites,
`scripts/check-interaction-net-profiling.sh`, formatting, full-feature Clippy
with warnings denied, and the complete ordinary test suite. Run aggressive-GC
verification if W9 changes any managed ownership boundary; it should not need
to do so.

Keep the repair only if it preserves every forced ordering and materially
reduces cold rediscovery or deterministic instruction work. Otherwise revert
the optimization, keep the precise classification if it remains cheap and
useful, and record the justified residual. Close W9 with a dated review which
accounts for any drift introduced by W7-W8 and updates the W6G.4 review with
the final disposition.

## Verification Matrix

| Concern | Required evidence |
| --- | --- |
| reflection replay | Forced W0A ordering; same application lazy and one request dispatch after resumption. |
| lazy ownership | Source or partial checkpoint owned by the managed lazy; a temporary route root only while subscribed or claimed; source/checkpoint cycles collectible once semantic reachability ends. |
| promise following | Assigned/unassigned, resolver/task-owned, cross-session, abandonment, and promise-cycle cases. |
| exact resumption | Phase/counter probes showing no completed prefix executes twice. |
| delegation | Deep tail chain on a small Rust stack with no per-step semantic allocation or root registration. |
| budget yield | Exact deterministic yield count, no dependency subscription, fair requeue. |
| failure contexts | Equal structured context ordering with and without forced suspension. |
| callbacks | Host and reflection callbacks occur outside access and exactly once. |
| GC safety | Forced collection between every durable poll under `aggressive-gc-verification`. |
| work sharing | Two observers share one lazy-owned producer checkpoint; the loser resumes through the same cached result. |
| cycle behavior | Pure lazy cycles fail canonically; promise-inclusive cycles remain retryable. |
| effects | Existing `.alt`, `.cut`, transaction, exit, and task semantics unchanged. |
| nets | Raw nets remain WHNF; cursor/net-construction worklists preserve identity and restoration. |
| stack control | User-controlled semantic depth completes on a deliberately small stack. |
| bounded whole-program work | Source-shaped direct assembly completes within deterministic scheduler/net budgets; semantic selections and terminal caches are not replayed. |
| exact-route accounting | Forced poll/release orderings classify relevant interference precisely; any retained handoff has a guarded local proof or falls back to authoritative traversal. |
| closure | Source-backed manifests show no unclassified recursive/suspendable WHNF entry. |

## Risks and Review Triggers

- If source-specific phases repeatedly encode the same nested return path,
  stop and adopt a compact shared frame rather than proliferating ad hoc
  continuations.
- If a general frame stack requires rooting values on every ordinary step,
  stop and repair the regional/durable split before proceeding.
- If preserving a computation across `.alt` appears to require `Clone`, review
  branch ownership rather than cloning active machine state.
- If durable lazy-producer progress appears in a coordinator record, registered
  root, or external sidecar, review why it cannot be represented by the
  lazy-owned checkpoint and how source/checkpoint cycles remain collectible.
- If an opaque production `SemanticComputation` or callback can suspend after
  W3D, halt for a representation decision; arbitrary Rust locals cannot be
  reconstructed safely.
- If a wait or callback appears to require retaining managed access, treat it
  as a boundary defect. Cursor-WHNF's separately proved structural wait is not
  a general precedent.
- If root registration scales with uninterrupted semantic depth, treat it as
  an architectural regression even if correctness tests pass.
- If source-shaped work exceeds a deterministic scheduler or net-work budget,
  classify replay versus scheduler amplification before increasing the budget
  or relying on a later phase to hide the regression.
- If W9 can avoid cold rediscovery only by treating a global generation match
  as proof, holding coordinator synchronization across poll, or making the
  caller-local route authoritative, retain the conservative fallback instead.
- If stack closure would require converting unrelated balanced persistent
  data structures, record their actual depth bound rather than enlarging this
  plan without evidence.

## Completion Criteria

This plan is complete only when:

1. every WHNF suspension or yield retains an exact durable resumption state;
2. reflection request evaluation consumes the same intermediate lazy after a
   forced suspension and never replays the preceding application;
3. pure evaluation uses an iterative budgeted driver for all user-controlled
   semantic depth;
4. no mutator or raw managed value crosses scheduling, waiting, callbacks, or
   reflection activation;
5. uninterrupted delegation performs no per-step root registration;
6. outer machines alone own cache, task, branch, and client result
   dispositions;
7. every current retryable `EvaluationHalt` caller has moved to the stateful
   protocol or has a documented bounded exception;
8. deterministic suspension, cycle, callback, collection, and small-stack
   verification passes;
9. source-shaped direct assembly has deterministic bounded-work evidence and
   no replayed function-call source or terminal cache; and
10. a post-implementation review accounts for every deliberate or accidental
    departure from this plan; and
11. W9 either implements and verifies precise exact-route poll/release
    reconciliation or records a measured reason for retaining the conservative
    fallback.
