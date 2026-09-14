# Resumable WHNF Evaluation Plan — 2026-09-12

Status: W0-W2 and the post-W2 remediation are complete by 2026-09-13;
W3-W8 planned. This is the focused implementation plan selected by
GCI11R-002D.2c.1d in
[`GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md`](GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md).
Client demand and promise following now own the crate-private resumable
protocol; lazy-source production and deeper evaluator callers remain on the
legacy recursive path until W3 and later checkpoints.

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
struct RegionalWhnfWork {
    focus: Value,
    frames: Vec<RegionalWhnfFrame>,
}

enum RegionalWhnfStep {
    Delegate(Value),
    Continue(RegionalWhnfWork),
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Failed(Arc<EvaluationFailure>),
}
```

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

Checkpoint replacement follows this order:

1. enter with the prior durable checkpoint still live;
2. project it beneath matching access;
3. perform local work using raw/scoped values;
4. root every live value in the replacement checkpoint before access closes;
5. install the replacement checkpoint; and
6. only then retire superseded roots and perform external coordination.

Panic/unwind handling must never leave a machine with an empty checkpoint.
Keeping the prior checkpoint until replacement publication is acceptable even
if unwind ultimately terminalizes the owning machine.

The first correct implementation may use one registered root per value live
across a real suspension. A future root-frame facility may compress
machine-adjacent roots; this plan must not invent that concurrent-GC mechanism
or add root traffic to the ordinary non-suspending path in anticipation of it.

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

##### W4E.3 — Scheduler amplification repair

**Status:** reassessment required as of 2026-09-14.

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
passes through several thousand scheduled `NetWhnfMachine` polls. Before
choosing another repair, decide whether W4E should accept that transitional
cost until W6 removes the compatibility/admission boundary, or whether to add
a narrower profile of machine claim, poll, release, and managed-access costs.

##### W4E.4 — Verification and plan reconciliation

The repaired source-shaped fixture must deterministically:

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
```

Update W6's per-session-policy checkpoint, W7C's quantitative work
verification, and W8A's compatibility retirement description with whichever
responsibility W4E actually removes. Record a new dated review finding rather
than silently rewriting the original W4 outcome.

### Phase W5 — Reflection Machine Integration

**Entry gate:** W4E is complete and the bounded direct-assembly fixtures pass.

#### W5A — WHNF submachine work state

Add a reflection work variant or dedicated decoding substate which owns
`WhnfComputation` plus one explicit completion purpose. It must compose with
branch cloning, cuts, retries, exits, cancellation, and transaction scopes
without cloning a live WHNF computation into multiple committed owners.

If an `.alt` branch needs independent evaluation, it receives an independently
rooted checkpoint by the existing branch construction policy; do not make
`WhnfComputation: Clone` as a shortcut.

#### W5B — Effect request decoding phases

Split effect-object, function, application-result, and request parsing into
explicit reflection phases. Preserve the existing structured
`effect_dispatch` contexts for `function`, `application`, and `request`.

Applying `eff` may produce a lazy, but the application occurs once. Request
parsing begins only after that same lazy produces WHNF.

#### W5C — Remaining reflection demand sites

Migrate task assertions, continuations, request payloads, keys, paths, stacks,
and protocol `.eval` requests currently using local `evaluate_in` loops.
Classify each as WHNF submachine work or a proven already-WHNF projection.
Remove the local recursive `evaluate_in` helper after its final caller moves.

#### W5D — Replay and branch verification

Land the W0A regression as a passing test. Add forced suspension to a nested
reflection request, a failing request with contexts, and alternative branches.
Assert exact lazy construction, task reservation, request dispatch, and
diagnostic counts.

Exit: reflection may suspend at any WHNF request boundary without replaying
the enclosing effect phase.

### Phase W6 — Complete Pure Evaluator Conversion

These checkpoints are the control-flow counterpart of the parent D.2c raw
value/access migration. Perform each family once: a D.2c checkpoint which
needs resumable demand adopts the WHNF work form here rather than retaining a
temporary recursive wrapper for a later pass.

#### W6A — Application and sequences

Convert the complete application and sequence families. Preserve currying,
applicative dictionary behavior, list order, binary/list streaming boundaries,
and non-forcing constructors.

#### W6B — Operators and runtime nets

Convert operator descriptors/execution and the remaining runtime-net evaluator
bridges. Keep topology and claim state in their existing net owners.

TBD: revisit and resolve WHNFW3R-004

#### W6C — Dispatch, scalars, comparisons, and strategies

Convert builtin dispatch, arity/assertion/conditional operations, numeric and
comparison operands, `seq`, `spark`, and provenance boundaries. Operations
which require only immediate data should become immediate-data helpers rather
than artificial WHNF frames.

#### W6D — Dictionaries, lists, and patterns

Convert collection and pattern families, including collection folds and view
or predicate application. Preserve `.fail` mismatch semantics separately from
permanent evaluation failure.

#### W6E — Annotations and effect values

Convert pure annotations, reflection/strategy annotation recognition, general
effect construction, and the remaining list-effect builtin dispatch. The W3
lazy list-effect recipes and source owner are already complete and are not
reimplemented here. Preserve sealed metadata and the distinction between pure
metadata updates and reflection boundaries.

#### W6F — Objects and interaction-net builtins

Convert the remaining object builtins and interaction-net builtin request
construction. Object-fixpoint C3 traversal, referential identity validation,
and the mixin fold are already complete in W3B.2b and are not reimplemented
here. Preserve referential-spec validation and source-owned net journals.

W6 closure also revisits the temporary one-ordinary-machine-per-demand-session
admission rule introduced to contain recursive compatibility evaluation.
Remove it once converted builtin work no longer needs that containment, or
record a narrower surviving owner and forced justification.

Each W6 checkpoint updates the exact W0B and parent D.2c manifests, runs its
focused suites in ordinary and `aggressive-gc-verification` modes, and adds a
forced suspension after at least one representative child demand.

Mandatory post-W6 review: audit the complete converted call graph before
retiring compatibility entry points.

### Phase W7 — Stack and Budget Closure

#### W7A — Recursive-call elimination audit

Reduce the W0B manifest to zero unapproved recursive WHNF calls. Any remaining
Rust recursion must be one of:

- statically bounded representation plumbing;
- balanced persistent-container traversal with a documented logarithmic
  depth bound; or
- a separately owned worklist/trampoline such as cursor-WHNF.

No user-controlled semantic recursion may remain implicit on the Rust stack.

#### W7B — Small-stack verification

Run deterministic deep lazy-alias, promise-alias, application, access-path,
key-conversion, collection, fixpoint, and structured-failure fixtures in a
thread with a deliberately small Rust stack. Include both uninterrupted and
forced-suspension forms. Success must not depend on worker count or repeated
runs.

#### W7C — Budget and fairness verification

Prove that a deep computation yields after its exact budget, resumes from the
same checkpoint, and eventually completes. Ensure client demand, deferred
work, reflection, and sparks requeue yields without installing dependency
subscriptions or starving other ready work.

Observe root registrations, allocations, and poll counts. Treat them as
regression diagnostics initially, except enforce that uninterrupted tail
delegation performs no per-step root registration.

### Phase W8 — Compatibility Retirement and Documentation

#### W8A — Retire retryable recursive-halt transport

Remove `await_deferred_task`, the recursive blocked/unassigned-promise paths,
and adapters which translate a bare `EvaluationHalt` after losing evaluator
state. Decide from remaining callers whether `EvaluationHalt` becomes only a
permanent-failure carrier, aliases `EvaluationFailure`, or remains as a narrow
compatibility name. Do not preserve the union solely for obsolete call sites.

#### W8B — Retire direct evaluator compatibility

Remove the direct evaluator constructor and test wrappers made unnecessary by
the shared synchronous driver. Reconcile `EvaluationMachinePoll`, client
demand, spark, and reflection result boundaries with the final yielded/pending
protocol.

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

## Verification Matrix

| Concern | Required evidence |
| --- | --- |
| reflection replay | Forced W0A ordering; same application lazy and one request dispatch after resumption. |
| lazy ownership | Exact temporary owner before admission, canonical producer root after admission, collectible state after completion. |
| promise following | Assigned/unassigned, resolver/task-owned, cross-session, abandonment, and promise-cycle cases. |
| exact resumption | Phase/counter probes showing no completed prefix executes twice. |
| delegation | Deep tail chain on a small Rust stack with no per-step semantic allocation or root registration. |
| budget yield | Exact deterministic yield count, no dependency subscription, fair requeue. |
| failure contexts | Equal structured context ordering with and without forced suspension. |
| callbacks | Host and reflection callbacks occur outside access and exactly once. |
| GC safety | Forced collection between every durable poll under `aggressive-gc-verification`. |
| work sharing | Two observers share one producer; the loser resumes through the same cached result. |
| cycle behavior | Pure lazy cycles fail canonically; promise-inclusive cycles remain retryable. |
| effects | Existing `.alt`, `.cut`, transaction, exit, and task semantics unchanged. |
| nets | Raw nets remain WHNF; cursor/net-construction worklists preserve identity and restoration. |
| stack control | User-controlled semantic depth completes on a deliberately small stack. |
| bounded whole-program work | Source-shaped direct assembly completes within deterministic scheduler/net budgets; semantic selections and terminal caches are not replayed. |
| closure | Source-backed manifests show no unclassified recursive/suspendable WHNF entry. |

## Risks and Review Triggers

- If source-specific phases repeatedly encode the same nested return path,
  stop and adopt a compact shared frame rather than proliferating ad hoc
  continuations.
- If a general frame stack requires rooting values on every ordinary step,
  stop and repair the regional/durable split before proceeding.
- If preserving a computation across `.alt` appears to require `Clone`, review
  branch ownership rather than cloning active machine state.
- If a `LazySource` must be mutated to record progress, review whether the
  proposed state actually belongs to the lazy producer's machine.
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
   departure from this plan.
