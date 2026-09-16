# Interaction-Net Callable WHNF Spill Plan — 2026-09-16

Status: in progress; NC0-NC2.0 completed on 2026-09-16. The plan was revised on
2026-09-16 first to use one net-owned callable checkpoint rather than an
`Operator >< Data` encoding, then to make regional and net-owned roles
zero-walk wrappers around one canonical `WhnfState`. This is the focused
topology and suspension subplan for W6B.4b.2 of
[`ResumableWhnfEvaluation_2026-09-12.md`](ResumableWhnfEvaluation_2026-09-12.md).
The parent plan remains authoritative for the D.2c inventory and its `-1`
callable-lowering delta.

## Purpose

Replace synchronous callable forcing inside an interaction-net
`Bind >< Data` reduction with an inline-first, spill-on-suspension protocol.

The common case must remain simple. When the callable reaches WHNF within the
current evaluator quantum, the original call reduction lowers it directly to
a copied raw net or a `CoreOperator`; it does not install intermediate graph
state merely because the input began as a lazy or promise. Only work which
exhausts its budget or reaches a real dependency publishes resumable state
into the net.

Suspended work has this logical topology:

```text
Bind >< CallableCheckpoint(NetWhnfState)
```

`CallableCheckpoint` is one internal, linear runtime-progress node. The
managed net is the liveness owner for every ordinary `Value` edge in the
canonical `WhnfState`; the state must not contain runtime roots merely to
survive suspension. A later worker may briefly claim the pair, move the
checkpoint's common state through an ephemeral regional-role wrapper without
walking it, use another bounded quantum, and either lower the completed call
directly or publish that state in one replacement checkpoint before yielding
again.

This is a spill protocol:

```text
Bind >< Data(callable)
        |
        +-- bounded regional callable WHNF
              |
              +-- ready  -> direct existing callable lowering
              +-- failed -> structural stuck result
              +-- yield  -> Bind >< CallableCheckpoint(NetWhnfState)
              +-- wait   -> Bind >< CallableCheckpoint(NetWhnfState)
                                  then exact dependency suspension
```

The checkpoint replaces the original `Data` node in place where practical.
There is no additional normalization operator, operand node, or terminal
reduction merely to reconstruct `Bind >< Data`.

## Why This Is a Separate Plan

The parent W6B.4b checkpoint originally grouped two raw-value declarations:
runtime-net access resolution and callable lowering. Access resolution can
reuse the existing `AccessMachine` ownership pattern. Callable lowering also
requires:

- a new specialization-owned runtime-node payload;
- one canonical raw-edge WHNF state with zero-walk regional/net role wrappers;
- bounded evaluator-budget integration in `NetWhnfMachine`;
- exact in-place checkpoint mutation with managed-edge barriers;
- production blocked-checkpoint resumption;
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

The reusable WHNF evaluator already has the required semantic information:

- `RegionalWhnfWork` owns one callback-free focus, continuation frames,
  followed identities, source-owner identity, and promise-cycle breadcrumb
  while matching value access is active;
- `WhnfStepBudget` bounds a regional quantum;
- `DurableWhnfState` is the current fine-grained-root, machine-owned copy of
  that regional work, pending the aggregate-cell migration in parent W6G.3;
- `WhnfDeferredRequest` separates dependency admission from regional value
  access.

NC1 introduced a separate `NetWhnfState`/`NetWhnfContinuation` correctness
scaffold and proved its exhaustive edge walk. NC2.0 has now replaced that
scaffold with one canonical `WhnfState`/`WhnfContinuation` representation.
Regional and net-owned forms are consuming role wrappers; changing roles no
longer duplicates values, reconstructs continuation containers, or uses the
borrowed projection oracle.

`DurableWhnfState` cannot be embedded in the managed net. Its runtime roots
are correct for an outer Rust machine, but a root stored inside the traced heap
would bypass the net's ordinary edge ownership and could keep the runtime
alive independently. The selected net checkpoint instead owns the same
canonical raw-edge state as regional execution, visited through the managed
runtime net. The later durable aggregation in parent W6G.3 will root one
managed state cell rather than every retained value independently.

## Selected Representation

### One complete canonical state

The safe representation is one complete state, not two isomorphic structures
or a hand-selected callable subset:

```rust
struct WhnfState {
    focus: Value,
    frames: Vec<WhnfContinuation>,
    followed: BTreeSet<DeferredValueId>,
    source_owner: Option<LazyId>,
    cycle_promise: Option<PromisedValue>,
}

struct RegionalWhnfWork(WhnfState);
struct NetWhnfState(WhnfState);
```

`WhnfContinuation` contains every active continuation variant with normal raw
`Value` fields. `RegionalWhnfWork` and `NetWhnfState` are zero-cost role
wrappers, not separate storage policies. They keep the ownership contracts
searchable: regional work may be inspected only beneath matching access,
while net state is dormant traced payload which may only be claimed into a
regional wrapper under that access.

Claim and publication are consuming ownership moves:

```text
NetWhnfState(WhnfState) --claim/move under access--> RegionalWhnfWork(WhnfState)
RegionalWhnfWork(WhnfState) --publish/move via barrier--> NetWhnfState(WhnfState)
```

Neither direction may iterate continuations, duplicate a `Value`, register a
root, or allocate a replacement vector. Tests retain the address and capacity
of the outer frame buffer and representative nested buffers across a complete
regional/net round trip. The claim guard owns enough exact state to restore
the same payload if the transition unwinds before publication. Every field
which can contain a `Value` participates in the canonical state's exhaustive
edge visitor and the runtime-net mutation transition.

NC5D still audits which state families callable normalization actually uses,
but field removal is no longer an expected optimization. A few unused words in
a rare boxed checkpoint are cheaper than walking and reallocating the state at
every transition. Specialization is considered only if profiling shows a
material end-to-end benefit and it preserves zero-walk ownership moves.

### Later durable aggregation

The same `WhnfState` is also the target payload for machine-owned durable
evaluation. Parent W6G.3 will compare and initially favor a rooted managed
cell resembling:

```rust
struct ManagedWhnfCell {
    state: Mutex<WhnfState>,
}

struct WhnfComputation {
    checkpoint: Root<ManagedWhnfCell>,
}
```

One rooted cell replaces the present root-per-retained-value representation.
Polling projects the root under matching access, locks the cell for one
callback-free quantum, and reports the complete pre/post `WhnfState` through
the existing managed edge-state transition gateway. The state is valid and
traced before the mutex and mutator are released; waits, callbacks, and
orchestration remain outside both.

Prefer `Mutex<WhnfState>` while the driver can operate through `&mut` state.
Use `Mutex<Option<WhnfState>>` only if a by-value handoff remains necessary,
and then retain the mutex guard plus an unwind guard until the state is
restored. Never expose an unlocked empty cell. This durable migration is not
an NC2 prerequisite; the focused net work establishes the common state and
zero-walk transition first. A future trace-immediate `RootFrame<WhnfState>` may
replace the managed cell if concurrent-GC profiling justifies avoiding
whole-state SATB transition walks.

### Runtime node boundary

The generic runtime gains a specialization-owned, runtime-only checkpoint
payload, conceptually:

```rust
trait NetSpecialization {
    type CallableCheckpoint: Send + 'static;
    // existing associated types...
}

enum RuntimeNode<S: NetSpecialization> {
    // existing variants...
    CallableCheckpoint(S::CallableCheckpoint), // boxed if the size gate says so
}
```

The exact name and boxing are implementation details. The node is never added
to the public/template `Node` vocabulary. Users cannot construct it; only a
running `Bind >< Data` callable reduction may spill one.

The payload is a one-port, linear runtime checkpoint rather than data:

- `Bind >< CallableCheckpoint` is the only reducing interaction;
- `Fan >< CallableCheckpoint` is stuck;
- `Erase >< CallableCheckpoint` is stuck; and
- every other principal interaction is stuck.

A checkpoint remains the principal partner of the original `Bind` until that
same pair yields another checkpoint or terminalizes. Cursor/source copying
which reaches the pair therefore waits for the active pair to reduce and
copies its eventual semantic result; it never materializes or duplicates the
checkpoint itself. Latch this topology rule so a later generic interaction
cannot accidentally turn checkpoint state into copyable data.

### Standard-trait interlock

`CallableCheckpoint` must not require `Clone`, `Debug`, `PartialEq`, or `Eq`.
The parent GC remediation is deliberately removing those unqualified traits
from raw `Value`, managed edges, and eventually the generic runtime carriers
which contain them. Adding the bounds here would create a new transitive
interlock immediately before that cutover.

It does require `Send`. The checkpoint is stored under the runtime-net mutex
or moved into one winning, thread-bound claim. Different workers may own that
state at different times, so the mutex-protected runtime state must be
sendable. It does not require `Sync`: no shared checkpoint reference escapes
the runtime lock or claim, and pair claiming excludes simultaneous regional
access. The concrete state may happen to auto-implement `Sync`, but the
specialization contract must not rely on it.

`Send` describes the dormant payload stored in the runtime net. Once a claim
moves that payload into regional work, the claim/state guard is lifetime-bound
to the non-`Send` value-access region. It must publish or restore the complete
state before that access closes and cannot be handed to another worker.

Boxing does not change this contract: `Box<NetWhnfState>` is `Send` whenever
the common state is `Send`, and it keeps that state uniquely owned and movable.
Do not replace it with `Gc<WhnfState>` merely for cross-thread use. `Gc<T>`
requires `T: Trace`, whose current contract includes both `Send` and `Sync`,
and a managed allocation cannot simply be moved out when a claim takes the
checkpoint into regional work. That representation would therefore add a
stronger bound, collector tracing/mutation machinery, indirection, and likely
one managed allocation per successor checkpoint. Reconsider managed
indirection only if later profiling identifies a separate sharing or layout
need which outweighs those costs.

### Nested tracing boundary

The checkpoint must nevertheless be traced. Boxing changes storage layout; it
does not make the retained values roots and does not make tracing optional.
The intended boundary extends the existing outer managed-net traversal:

```text
ManagedCoreNetCell::trace
  -> RuntimeNetCell::try_visit_logical_payloads
  -> RuntimeNetPayload::CallableCheckpoint(&NetWhnfState)
  -> WhnfState edge visitor
  -> visit every managed edge retained by the state and its frames
```

The canonical `WhnfState` therefore provides one exhaustive internal edge walk
covering at least `focus`, every value-bearing continuation field, and the
managed promise breadcrumb when present. The net wrapper delegates to that
walk; a later `ManagedWhnfCell` will delegate to the same walk. ID-only fields
contribute no edge. Adding a state field without updating this walk must fail
a source-backed exhaustive inventory or compile-exhaustive dispatch test.

This nested walk need not be an implementation of glam-gc's unsafe `Trace`
trait on the net role wrapper. `ManagedCoreNetCell` is the managed allocation and
already owns the unsafe tracing contract. It locks the runtime cell while
visiting a stable logical payload snapshot. Because `Mutex<T>` can make the
outer cell shareable when its protected state is `Send`, the nested payload
does not independently need `Sync` merely to be visited by the collector.

There is no untraced gap when a claim takes the state. The move into
regional work occurs only after matching mutator admission; collection cannot
start while that access remains active. The access-branded claim must publish
or restore the complete state into the traced net before the mutator exits.
Dormant and blocked checkpoints are always resident in the outer trace walk.

The current generic `RuntimeNode` derives and `NetSpecialization` bounds are
transition scaffolding, not the checkpoint contract. NC0D inventories every
derive, bound, formatting path, equality use, and whole-node clone which the
new variant would otherwise inherit. NC2 then makes the checkpoint path obey
these rules:

- checkpoint state moves between the claimed pair and regional work without a
  representation walk; it is never cloned;
- stale admission compares pair identity plus checkpoint generation/revision,
  never checkpoint payload equality;
- generic debug output may name the opaque node variant but cannot format its
  payload; and
- semantic tests inspect claimed regional state beneath matching value access
  instead of using ordinary equality.

If adding a non-trait-bearing variant prevents a blanket `RuntimeNode` derive,
replace that blanket operation with variant-specific operations. Do not add a
wrapper whose manual traits merely recreate raw `Value` cloning, formatting,
or equality. Record every affected declaration in the parent D.2c and
persistent-edge P3 inventories so their later cutover cannot overlook this
new carrier.

Measure `size_of::<RuntimeNode<CoreSpecialization>>()`, its managed wrapper,
and the applicable GC slot class before selecting storage. If an unboxed
checkpoint increases a hot node or allocation size class materially, store
the state behind `Box`. Checkpoints exist only after suspension, so one boxed
allocation is preferable to increasing every ordinary net-node allocation.
NC5D records actual field use and reruns the measurement, but does not pare the
canonical state merely to reconsider one transient box. Revisit boxing only if
the complete common state later fits without affecting node size or profiling
shows the indirection matters.

### Meaning of `focus`

`focus` is the exact value which the next WHNF transition must inspect. It is
not necessarily the callable originally attached to the `Bind`.

Regional transitions are atomic with respect to checkpoint publication. The
driver checks budget before a transition. A transition may inspect one cached
lazy or assigned promise and update the complete regional state, including
focus, frames, followed identities, and cycle metadata. Only then may another
budget check yield.

For example:

```text
original focus: Lazy A
cached A:       Promise B
assigned B:     Lazy C
```

may spill with `focus = Lazy C`; resumption does not return to `A` or re-read
`B`. If budget expires before a transition starts, no part of that transition
has happened and the prior complete state remains authoritative.

No callback, task admission, reflection activation, or host operation occurs
inside such a transition. Those are reported as boundaries only after the
complete `NetWhnfState` has been published.

### Producer work remains canonical

When focus is an unfulfilled lazy, the checkpoint deliberately retains that
same lazy identity. Its canonical `LazyTaskMachine` owns source evaluation,
including application frames, reflection tasks, host calls, access work, or
net work. The callable checkpoint admits or joins that producer and blocks.
After wakeup it reads the lazy's cache and continues from the cached result; it
never re-evaluates the lazy recipe or constructs a second reflection task.

Promises follow the same division. An unassigned promise remains the focus;
its producer or canonical follower owns eventual assignment. An assigned
promise transition installs its assigned value as the new focus before
publication.

NC1 and NC5 must nevertheless investigate this ownership boundary. In particular,
they must establish whether a callable checkpoint can ever inherit application
or access frames rather than assuming all frames belong to a producer. If the
proof fails, the observation becomes part of the common-state usage record;
the full `frames` vector remains part of `WhnfState` either way unless a later
measured specialization preserves zero-walk transitions.

### Inline first, spill only at a boundary

Encountering `Value::Lazy` or `Value::Promised` does not by itself install a
checkpoint. The claimed call moves the state into ephemeral regional WHNF work
and drives it with
the remaining semantic budget for the current net-machine quantum.

A cached/assigned chain which reaches an immediate callable within that budget
takes the existing direct copy/operator path. No checkpoint node, managed
root, producer, or additional net reduction survives the operation.

The work is reified only if:

1. the available callable-WHNF budget is exhausted;
2. the current lazy or promise is unresolved;
3. dependency admission must occur outside managed access; or
4. a future WHNF rule reaches another explicit external boundary.

### Resume and terminalize the same pair

Claiming `Bind >< CallableCheckpoint(state)` moves the complete common state
into its regional-role wrapper without walking it and uses the remaining
bounded quantum. Its result is:

- **Ready:** classify the final focus and directly apply the existing raw-net
  copy or callable-to-operator rewrite using the original `Bind` wiring. Do
  not first reconstruct `Bind >< Data` merely to reduce it again.
- **Failed:** retain the structured evaluation failure as the stuck reason.
- **Yielded:** replace the checkpoint payload once, make the same pair ready,
  and end the claim.
- **Boundary:** publish the complete replacement state, end regional access
  and the claim, admit the dependency, then conditionally block only that
  exact checkpoint generation.

The graph records quantum boundaries, not individual cached-shell steps.

### Budget relationship

This plan depends on parent checkpoint W6B.0's borrowed poll-budget
foundation. `NetWhnfMachine::poll` receives the same mutable budget object as
its outer task poll; it does not receive a copied integer or construct a fresh
allowance per active pair. The budget records its original grant, remaining
units, and spent units, making actual declared consumption available to tests,
profiling, and later scheduler statistics.

Initially, the net driver charges the shared semantic budget as follows:

- each call or operator semantic dispatch consumes a unit;
- each regional callable-WHNF transition consumes a unit from the same
  remaining budget; and
- existing structural interaction-net normalization batches retain their
  separate bounded-batch policy rather than suddenly charging every wire or
  cursor operation as an evaluator semantic step.

A callable must never receive a fresh full budget for every active-pair retry
within one outer poll. Budget exhaustion publishes the exact `NetWhnfState`
and returns `Yielded`.

If structural net reductions later use a distinct typed budget, translation
reserves an explicit portion of the evaluator budget, constructs the child
budget from that reservation, and returns only the representable unused
portion. The initial implementation need not add that conversion machinery,
but preserving both `remaining` and `spent` avoids a second budget-interface
rewrite when profiling or measured conversion arrives.

### Dependency admission without a durable claim

No call or checkpoint claim may enter a machine field, scheduler record, or
returned `Pending` result. A claim may span only bounded callback-free work
and its immediate atomic checkpoint publication.

Dependency admission is logically two-stage:

1. publish a ready `CallableCheckpoint` and finish the current claim;
2. admit or observe the lazy/promise dependency outside managed access;
3. conditionally block the pair only if runtime net, pair, and checkpoint
   generation/revision still match; and
4. close the subscribe/observe race so completion immediately before or after
   blocking cannot lose a wakeup.

Another worker may claim, update, or terminalize the checkpoint between steps
1 and 3. The older boundary action is then stale and must do nothing. It must
not restore an older state or block newer work. The implementation must not
solve this race by retaining a claim across a callback, scheduler wait, or
later poll.

## Semantic and Ownership Invariants

1. **Immediate and cheaply resolved callables leave no checkpoint.** Deferred
   representation alone is not a reason to add topology.
2. **Regional and net work share one complete state.** The role wrappers move
   the same `WhnfState`, including every continuation and ownership field.
3. **Role changes are zero-walk.** Claim and publication neither iterate the
   continuation graph nor duplicate values, register roots, or rebuild
   containers. Field-use proofs inform diagnostics and later profiling; they
   do not justify adding a specialized conversion path by default.
4. **Suspension does not replay a completed prefix.** The complete successor
   state is published atomically at every yield and dependency boundary.
5. **Claims are quantum-local.** No claim survives budget yield, dependency
   admission, scheduler return, callback, cancellation, or unwind.
6. **The net owns suspended progress and liveness.** The outer managed-net
   trace visits every checkpoint edge; they are not independent roots or
   machine-side shadows. Regionally claimed state exists only beneath active
   mutator admission and returns to the trace before that admission closes.
7. **Lazy producers remain canonical.** Callable normalization joins an
   existing source owner and never reconstructs its recipe or reflection work.
8. **Cycle semantics match ordinary WHNF.** Splitting at any quantum or wait
   boundary cannot change cycle recognition or its structured failure.
9. **Checkpoints are linear and uncopyable.** A cursor or logical net copy
   which reaches the active checkpoint pair waits for that source pair to
   reduce. No target net materializes a checkpoint.
10. **No semantic value pollution.** Checkpoints are internal runtime nodes
    and cannot be observed by ordinary Glam patterns, equality, or value-kind
    diagnostics.
11. **Every checkpoint edit is a traced edge transition.** Replacing a state
    reports all leaving and entering `Value` edges through the managed-net
    mutation gateway.
12. **Stale orchestration is harmless.** Delayed admission, wake, or retry
    cannot block, restore, or fail a newer checkpoint.
13. **Failure provenance is preserved.** Inline and spilled evaluation produce
    equivalent failures and context frames.
14. **Raw nets and applicable values retain existing meanings.** Final
    classification preserves raw-net copying and ordinary builtin, function,
    partial-function, and applicable-dictionary behavior.
15. **No new global allocator is introduced.** Checkpoint generations use
    runtime-owned identity allocation or an existing exact runtime revision.
16. **No standard value observation traits are reintroduced.** Checkpoint
    storage, orchestration, tests, and diagnostics do not require `Clone`,
    `Debug`, `PartialEq`, or `Eq` from checkpoint state or its retained values.
17. **Thread mobility is ownership mobility.** Checkpoint state is `Send` so
    the mutex-protected net may move work between workers; `Sync` is not a
    checkpoint requirement because the lock and exact claim prevent shared
    regional access.

## Non-Goals

- Do not move general pure evaluation into interaction-net topology.
- Do not add a public WHNF, demand, or checkpoint agent.
- Do not introduce durable active-pair claims.
- Do not add an active-pair side table for callable progress.
- Do not wrap every deferred callable in a synthetic managed lazy.
- Do not add `Value::CallableProgress` or another semantic value variant.
- Do not split or optimize fields out of the canonical `WhnfState` merely for
  the transient callable checkpoint.
- Do not redesign cursor-WHNF normalization, generic task scheduling, or the
  entire interaction-net work budget in this subplan.
- Do not migrate the separate W6B.4b access-path operation here.

## Implementation Phases

### NC0 — Latch the seam, state, and size baseline

Status: complete on 2026-09-16.

NC0 exposed one staging error in the original checklist. A test cannot install
or observe `CallableCheckpoint` before NC2 introduces the runtime-only variant.
NC0 therefore latches the complete *pre-checkpoint* oracle. NC2B owns the
cursor fixture over the real variant, NC3B-NC3C own the forced budget and
dependency spill observations, and NC4 owns proof that resumption starts from
published state. No ignored, disabled, or synthetic checkpoint fixture stands
in for those later executable contracts.

Prerequisite: W6B.0 has replaced copied `usize` poll allowances with one
borrowed budget token through the task and relevant nested-machine path. NC0
must not begin by adding a callable-local compatibility budget.

#### NC0A — Current-path characterization

Record focused tests and profiling observations for:

- immediate builtin, partial builtin, function, dictionary, and raw-net calls;
- cached lazies and assigned promises resolving to each callable family;
- unresolved lazies and promises;
- permanently non-callable results; and
- existing claim release, unwind, blocked retry, and fused
  callable-to-operator topology.

Show that synchronous `lower_core_callable_in` is the only remaining W6B.4b
callable declaration and identify where its claim currently crosses retryable
evaluation. Do not change expected semantics in NC0A.

Completion record: the callable-disposition fixture now covers immediate
builtin, partial builtin, function, dictionary, and raw-net values; cached
lazy and assigned-promise shells resolving to every one of those families;
unresolved promise and lazy dependencies; and permanent non-callability. The
existing forced release, unwind, exact blocked retry, and direct operator
splice fixtures remain in place. `callable_lowering_has_one_synchronous_claim_seam`
source-latches one `lower_core_callable_in` declaration and the one call from
`progress_core_call_claim` which currently crosses synchronous WHNF demand.
Under `interaction-net-profiling`, direct operator and copy completion each
commit one `call` reduction while dependency blockage commits none until the
semantic rewrite terminalizes.

#### NC0B — State-shape and allocation baseline

Record `size_of` and applicable GC slot/run-class observations for:

- `RegionalWhnfWork` and each continuation family;
- `RuntimeNode<CoreSpecialization>`;
- the managed runtime-net node/container; and
- boxed versus unboxed prototype checkpoint payloads.

This baseline decides NC2A boxing. Prefer const assertions for architectural
size assumptions and ordinary tests for policy thresholds which may change.
Add a compile-time positive contract for checkpoint `Send`, without imposing
or attempting to prove the absence of an incidental `Sync`
auto-implementation. Separately latch that the active regional-work guard is
not `Send` and cannot outlive its matching value access.

Completion record on x86-64/64-bit targets:

| Representation | Bytes |
|---|---:|
| `Value` | 64 |
| `RegionalWhnfFrame` | 40 |
| `RegionalUndefinedDictionary` | 32 |
| `RegionalWhnfContinuation` | 160 |
| `RegionalWhnfWork` | 128 |
| `RuntimeNode<CoreSpecialization>` | 96 |
| prototype node with unboxed `RegionalWhnfWork` | 128 |
| prototype node with boxed `RegionalWhnfWork` | 96 |
| `ManagedCoreNetCell` / requested GC slot extent | 248 / 248 |

The test uses exact target-specific latches for the measurements and portable
policy assertions that boxing preserves the existing runtime-node extent
while the unboxed prototype currently enlarges it. `RegionalWhnfWork` and the
boxed prototype satisfy `Send`. The existing compile-negative contracts in
`evaluation/access.rs` prove `EvaluationValueAccess` and its underlying
`RuntimeValueAccess` are neither `Send` nor `Sync`; their lifetimes remain the
structural bound on projected work. This baseline selects boxed storage for
NC2A unless the canonical-state remeasurement changes the measured class
before insertion.

#### NC0C — Failing spill oracle

Add test-only observation sufficient to distinguish:

- direct lowering with no checkpoint installed;
- one checkpoint installed because budget was forced to expire;
- one checkpoint installed because a dependency was reached; and
- resumption from the published state rather than the original callable.

Prefer topology and counters over timing or thread repetition. Target tests
may remain expected-failing only inside the NC0 commit and become ordinary
regressions as their owning phases land.

Completion record: `CurrentCallablePath` is the test-only topology oracle for
the current four observable outcomes: direct copy, direct operator splice,
exact dependency blockage, and permanent failure. It proves the NC0 fast path
contains no progress node. Rather than retain expected-failing tests after the
checkpoint, the four checkpoint-only observations have been assigned to
NC3B, NC3C, and NC4 as described by the NC0 staging correction above.

#### NC0D — Runtime-node trait and copy inventory

Inventory the exact generic declarations which would make a new runtime-node
payload inherit `Clone`, `Debug`, `PartialEq`, or `Eq`, and separately confirm
where `Send` is required for runtime sharing, including:

- `NetSpecialization` bounds and associated-type bounds;
- `RuntimeNode` derives and whole-node clones;
- generic fan/erase rewrites;
- cursor/source materialization;
- logical-payload tracing and managed-edge inventories;
- stuck-state and diagnostic formatting; and
- tests which compare complete nodes rather than semantic observations.

Classify every occurrence as an unrelated temporary compatibility interlock,
a checkpoint-path operation to remove in NC2, or a test to migrate to an
explicit observation. Add these occurrences to the parent D.2c raw-value and
persistent-edge P3 manifests before introducing the variant. Demonstrate the
topological premise with a fixture in which a cursor reaches a source
`Bind >< CallableCheckpoint`: it must observe the active-pair dependency and
must not request a checkpoint clone.

Completion record: the exact source-backed inventory contains 24 interlocks
and assigns each to one of three owners:

- twelve existing-payload compatibility operations remain under parent D.2c/P3:
  the specialization and associated-type trait bounds, template/claimed
  payload clones, fan duplication, and ordinary data source materialization;
- ten checkpoint-path operations belong to NC2: the blanket `RuntimeNode`
  derives, whole-node source/cursor clones and their frontier carrier,
  transition/full logical-payload vocabularies, materialization/erase match
  coverage, managed trace dispatch, and opaque stuck-node formatting; and
- two observation fixtures belong to NC2: the compile-exhaustive runtime
  payload walk and the existing active-source-call cursor test.

There are no tests comparing a complete runtime node as the semantic result;
existing runtime tests use variant/topology observations. The current
`active_source_call_is_a_dependency_and_is_never_copied` fixture already
proves that a cursor stops at a claimed `Bind >< Data` source pair without
copying the callable. NC2B will extend that same fixture to the real
`Bind >< CallableCheckpoint` pair once the variant can be constructed. The
source inventory fails on count drift before a new variant can silently
inherit an old copy or observation path.

Exit: existing behavior, target topology, state size, and the no-checkpoint
fast path are executable baselines, and every trait/copy interlock has one
named migration owner.

### NC1 — Complete net-owned WHNF state and shared budget

Status: complete on 2026-09-16.

#### NC1A — Full regional/net isomorphism

Introduce `NetWhnfState` and `NetWhnfContinuation` with every field and variant
of the current regional work. Add explicit projection/publication conversions
under matching access. At this phase the state may be test-only or hosted by a
temporary focused fixture; do not add runtime topology merely to exercise it.

Force round trips with:

- empty and nonempty frame stacks;
- application and dictionary-application frames;
- static-access and semantic-undefined frames;
- followed lazy/promise identities;
- source-owner identity and promise breadcrumb; and
- values in every retained frame position.

Collection between publication and the next claim must retain exactly the state
reachable through the net-owned edge visitor, with no runtime roots inside the
state. Give each value-bearing frame position and the optional promise
breadcrumb an independently collectible sentinel so omitting any one edge
fails deterministically.

Completion record: `NetWhnfState` now mirrors all five regional continuation
families plus `focus`, `followed`, `source_owner`, and `cycle_promise`. It owns
raw semantic edges rather than registered roots. Explicit publication consumes
one complete `RegionalWhnfWork`; projection duplicates every raw value only
under matching `EvaluationValueAccess`. Its `Trace` implementation composes
the same compile-exhaustive compatibility walk as the existing managed value
families and directly reports the promise breadcrumb. A temporary managed
fixture roots only the state itself, collects, then verifies independently
allocated lazy sentinels in every value-bearing position and the promise
breadcrumb all survive before a second publication/claim round trip. The
representation and its narrow trace adapter carry a bounded `dead_code`
allowance until NC2 installs the corresponding runtime node.

#### NC1B — Shared callback-free driver

Factor the regional driver so general `WhnfComputation` and net checkpoint
work use identical transition semantics. Drive net work to `Ready`, `Failed`,
`Yielded`, or `Boundary` while retaining the entire regional successor on
yield and boundary.

Force budget splits before and after every transition in frame-free shell
chains and representative frame-bearing work. Results, failures, remaining
frames, cursors, and retained values must match uninterrupted evaluation.

Completion record: both durable and net-owned work use `drive_regional` as the
single callback-free transition loop. `NetWhnfState::drive_in` projects once,
uses that loop, and returns complete net-owned successors on yield and
boundary. Forced splits at zero and after each transition match uninterrupted
ready results and exact spent/remaining budget. Separate frame-bearing tests
retain focus, cursor, arguments, boundary state, and failure identity.

#### NC1C — Net-machine semantic budget

Pass the outer mutable poll budget into `NetWhnfMachine::poll` and charge that
same token for call/operator dispatch and WHNF transitions. Preserve the
existing structural normalization-batch policy. Add a deterministic
zero/one/many matrix, assert exact `spent` and `remaining` observations, and
prove one outer poll cannot grant a fresh full budget to every retry.

Completion record: `NetWhnfMachine::poll` now borrows the caller's
`EvaluationStepBudget`; structural cursor/frontier normalization remains
uncharged while each call, operator-call, or blocked semantic retry consumes
one shared token. Because semantic discovery has already claimed a fresh call
pair, denied budget explicitly releases that exact call/operator claim before
requeueing it. The deterministic zero/one/many fixture forces denial and
resumption, checks exact spent/remaining observations, and reuses an exhausted
budget across another poll to prove there is no nested allowance renewal.

#### NC1D — Reachability inventory, not optimization

Inventory every constructor and transition by which callable normalization
could receive or create:

- a continuation frame;
- `source_owner`; or
- `cycle_promise`.

Record the apparent proof that ordinary call-site demand begins frame-free and
that lazy-source application/reflection/access work belongs to the canonical
producer. Do not remove fields in NC1; retain the observations for NC5D's
post-topology usage audit.

Completion record: the source-backed inventory records the following current
creation paths without optimizing any field away:

- ordinary `Bind >< Data` callable lowering receives one value and contains no
  application/static-access constructor, source-owner attachment, frame push,
  or promise breadcrumb;
- application frames enter through `from_application_checkpoint_in`, while
  static-access frames enter through `from_static_access_checkpoint_in`;
- `with_source_owner` is used only by lazy producer machines (access, list,
  list-effect, object, and their application submachines), not call-site
  lowering;
- dictionary-application and semantic-undefined frames are created only while
  resuming the corresponding application, and static-access/undefined frames
  requeue only their own successor state;
- the promise breadcrumb is created only after following an assigned promise;
  and
- `LazySource::Application`, `ReflectionTask`, and `Access` remain dispatched
  by the canonical lazy producer in `value.rs`.

The focused inventory test latches the frame-free callable seam and those
producer-owned source families. NC3 deliberately replaces the synchronous
`eval_value_in` call named by that latch; NC5D remains responsible for recording
which complete fields callable normalization actually exercises, without
assuming that paring the shared representation is beneficial.

Exit: complete WHNF state can move losslessly between regional execution and
net-owned storage under one bounded semantic quantum.

### NC2 — Runtime callable-checkpoint node

#### NC2.0 — Canonical state and zero-walk role wrappers

Status: complete on 2026-09-16.

Replace the parallel regional/net state and continuation definitions with one
`WhnfState` and one `WhnfContinuation` vocabulary. Keep
`RegionalWhnfWork(WhnfState)` and `NetWhnfState(WhnfState)` as explicit
ownership-role wrappers. The conversion in either direction consumes its
source wrapper and rewraps the same state; it must not call the NC1 borrowed
`project` path, duplicate a `Value`, iterate a continuation, or collect a new
container.

Make the canonical edge visitor exhaustive once and delegate net tracing to
it. Preserve NC1's forced-collection and full-frame fixtures, but update the
round-trip oracle to record and compare the outer frame buffer plus
representative nested buffer pointers, lengths, and capacities across both
role changes. Add root-registration and explicit duplication counters if
pointer retention alone cannot observe a future accidental walk. Remove the
bounded NC1 `dead_code` allowance only when NC2A installs the runtime payload.

Record the current `DurableWhnfState` conversion as the one intentionally
remaining isomorphism. Assign its aggregation to parent W6G.3 rather than
generalizing the net wrappers back into a storage-policy abstraction.

Completion record: `WhnfState` and `WhnfContinuation` now own the complete
raw-edge vocabulary. `RegionalWhnfWork` and `NetWhnfState` consume and rewrap
that exact state; their transition bodies contain no traversal, collection,
duplication, or root construction. The role-handoff fixture preserves the
outer frame buffer and every representative nested buffer's pointer, length,
and capacity across repeated transitions, while root-registration counts stay
unchanged. The forced-collection fixture traces the same canonical state and
retains every focus, frame, followed identity, source owner, and promise
breadcrumb. The durable rooted representation remains the sole intentional
isomorphism and stays assigned to parent W6G.3.

#### NC2A — Specialization payload and boxing decision

Add the specialization-owned callable-checkpoint associated type and the
runtime-only node variant. Use NC0B measurements to select boxed or unboxed
storage. Latch the resulting `RuntimeNode` and managed wrapper size classes.
Give the associated type no ordinary duplication, formatting, or equality
bounds beyond `Send + 'static`. Remove or narrow any blanket `RuntimeNode`
derive which would impose them, using the NC0D assignment rather than
introducing a compatibility shim.

Rerun NC0B against the canonical common state before committing the boxing
choice. Extend structural variant rendering, profiling classification,
managed-drop/ownership inventories, and the core checkpoint edge visitor.
Structural rendering identifies the opaque checkpoint variant only. Exact
orchestration uses its generation/revision and pair identity, not payload
equality. Extend `RuntimeNetPayload` and the outer `ManagedCoreNetCell` trace
adapter so the checkpoint's complete internal edge walk participates in every
logical payload snapshot; do not add roots to compensate for a missing edge.

#### NC2B — Linear interaction and cursor rules

Implement the sole reducing rule for `Bind >< CallableCheckpoint`, the stuck
rules for fan, erase, and all other principal partners, and the checkpoint's
one-port shape. Add no public template constructor and no generic checkpoint
duplication operation.

Verify that logical copying or cursor materialization at this source frontier
blocks on the active pair, then copies only the semantic topology produced
after the source pair terminalizes. The target must never contain a
`CallableCheckpoint`, even transiently. Extend NC0D's
`active_source_call_is_a_dependency_and_is_never_copied` fixture with a real
source `Bind >< CallableCheckpoint`; this is the deferred NC0 cursor oracle,
not a new independent fixture.

#### NC2C — Spill and update mutations

Add separately named generic runtime mutations which:

1. replace one claimed call's `Data` node with a callable checkpoint while
   retaining the original `Bind`, node identity, auxiliaries, and pair;
2. replace one claimed checkpoint payload with its complete successor by
   moving the same common-state representation; and
3. terminalize a claimed checkpoint directly through the existing raw-net copy
   or callable-to-operator rewrites.

Every mutation validates the exact claimed pair, publishes the complete
replacement before release, and reports old/new payload edges through the
managed mutation gateway. Test stale calls and unwind before publication.

#### NC2D — Conditional exact blocking

Add the minimum exact-state operation needed to block a published checkpoint
after dependency admission. Match runtime net, pair, and checkpoint
generation/revision atomically. A stale request returns a non-error disturbed
result.

Use deterministic barriers for:

1. pending dependency through successful blocking;
2. completion immediately before blocking;
3. another worker updating the checkpoint first; and
4. another worker terminalizing it first.

Close the subscribe/observe race explicitly; no ordering may lose a wakeup.

Exit: the runtime can own, move, update, block, and terminalize a complete
callable checkpoint without an additional graph node, durable claim, payload
observation trait, checkpoint-copy path, or regional/net representation walk.

### NC3 — Inline-first original call reduction

#### NC3A — Regional fast path

Replace synchronous deferred forcing in `lower_core_callable_in` with bounded
regional WHNF work. Immediate callables bypass the driver. Deferred callables
which become ready within the remaining budget take the existing direct
copy/operator/failure paths.

Prove through topology observation that cached or assigned chains within the
budget never install a checkpoint.

#### NC3B — Spill on budget exhaustion

When regional work yields, use NC2C to replace the original `Data` node with a
checkpoint formed by moving the entire current common state into
`NetWhnfState`. End the original claim and return ordinary runnable progress.
Never retain the original callable as a separate restart point. Complete
NC0C's budget-spill oracle with an exact one-checkpoint topology observation.

#### NC3C — Spill on dependency boundary

Publish the same complete checkpoint for an unresolved lazy/promise boundary,
then use NC2D to admit and block outside access. Ensure abandoned, cancelled,
failed, and completed producers match ordinary WHNF behavior. Complete
NC0C's dependency-spill oracle with an exact one-checkpoint topology
observation.

Exit: original calls either finish inline or leave one complete net-owned
state; they never retain a durable claim or machine-side continuation.

### NC4 — Resume and terminalize checkpoints

#### NC4A — Checkpoint claim and regional handoff

Teach semantic active-pair dispatch to recognize
`Bind >< CallableCheckpoint`, claim it briefly, move the common state into its
regional-role wrapper without inspecting its frames, and use the remaining
shared semantic budget.

Force a checkpoint whose focus has already advanced through at least two
cached/assigned shells and prove resumption begins at that published focus,
not at the original callable. This closes NC0C's deferred resumption oracle.

#### NC4B — Yielded checkpoint replacement

On budget yield, publish one complete successor checkpoint through NC2C. Do
not mutate topology once per WHNF transition. Add counters proving a quantum
with many cached transitions performs one checkpoint publication.

#### NC4C — Direct readiness and failure

On readiness, classify the final focus while the checkpoint remains claimed
and terminalize directly using the original `Bind` wiring. Do not reconstruct
a `Data` node and schedule another call reduction. On failure, retain the
structured stuck reason.

Preserve raw-net copying, function partial application, and applicable
dictionary/builtin behavior.

#### NC4D — Frame-bearing resumption oracle

Even if production callable entry is believed frame-free, force at least one
checkpoint with nonempty continuation frames through yield, block, unwind,
and completion. Preserve the recorded continuation buffer identities across
every role change. This proves the common state is genuinely lossless and
zero-walk before NC5D records actual production usage.

Exit: a checkpoint can cross arbitrary budget and dependency boundaries and
terminalize without replay or temporary topology.

### NC5 — Concurrency, ownership, and state-usage audit

#### NC5A — Cycle, producer, and dependency matrix

Force:

- lazy-to-lazy, promise-to-promise, and mixed chains;
- repeated identities before and after one or more spills;
- unresolved dependencies at the first and later focuses;
- cached or assigned failures;
- cancellation, abandonment, and task failure while blocked; and
- application-, reflection-, access-, host-, and net-backed lazy producers.

Every producer-backed fixture records its canonical lazy/task identity. Budget
splits and stale retries must retain one producer and, where applicable, one
reflection task rather than reconstructing source work.

#### NC5B — Contention and stale work

Use barriers rather than repetition to force two workers toward the same
checkpoint. Verify one authoritative state, harmless stale admission, exact
blocked retries, no restored predecessor, and no claim after either worker
returns. Cover unwind before and after publication.

#### NC5C — Cursor deferral and GC ownership

Attempt to copy or materialize a closed runtime net while a checkpoint pair is
ready and while it is blocked. The target cursor must depend on source active
pair progress; after the source terminalizes, it copies only the semantic
result. Assert that no checkpoint payload is cloned and no target checkpoint
exists. Force collection at claim, publication, dependency admission,
wake, cursor deferral, source terminalization, result materialization, and
checkpoint retirement.

#### NC5D — Record actual callable-state usage

Revisit the NC1D inventory after the complete representation passes NC3-NC5C.
For `frames`, `source_owner`, and `cycle_promise`:

- identify the private constructors which establish the initial value;
- identify every transition capable of changing it;
- retain a source-backed inventory or type boundary which detects a new
  producer; and
- retain forced semantic fixtures spanning that state family.

Use test/profiling-only observations at checkpoint publication and resumption
to distinguish production occurrences from deliberately constructed
frame-bearing completeness fixtures. Record counts for nonempty `frames`,
nonempty `source_owner`, and nonempty `cycle_promise`; do not retain these
counters in the normal runtime merely to support the audit.

Record whether production callable normalization currently uses each family,
but keep the canonical `WhnfState` complete. Do not introduce an outer-shell
checkpoint, a second continuation enum, or a conversion walk merely to save a
few fields in a rare transitory box. Avoiding per-transition traversal,
allocation, and value duplication has priority over shrinking the checkpoint
payload.

Rerun NC0B size measurements with the complete common state and record the
boxing result. A future specialized representation requires measured
end-to-end benefit, a zero-walk ownership handoff, and a proof that it does not
duplicate the work algebra. It is not part of NC5 merely because some fields
are currently unused.

Exit: the complete canonical state remains authoritative, with an exact usage
record available for diagnostics and any later measured specialization.

### NC6 — W6 integration, profiling, and focused review

#### NC6A — Compatibility retirement and inventory closure

Remove synchronous deferred callable forcing from `lower_core_callable_in` or
retire the helper if classification now belongs directly to call progression.
Relatch the D.2c manifest and record W6B.4b.2's `OperatorAndNet -1` delta. Keep
the separate access-resolution declaration assigned to W6B.4b.1. Reconcile
NC0D's runtime-node trait/copy occurrences with persistent-edge P3 and prove
the checkpoint introduced no new P4 trait dependency.

#### NC6B — Profiling and performance

Extend static interaction-net profiling with, at minimum:

- inline callable-WHNF transitions;
- checkpoint installation;
- checkpoint resumption and replacement;
- exact dependency block/retry;
- stale boundary admission; and
- direct checkpoint terminalization.

Update the focused profiling script with named tests. Immediate and
within-budget cached callables must install zero checkpoints. Compare
direct-style assembly fixtures against the pre-NC baseline and feed residual
cost into W6G rather than hiding it with an unbounded budget.

#### NC6C — Routine verification and review

Run:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Also run the focused interaction-net profiling script, callable/function/net
suites, forced-schedule tests, and relevant aggressive-GC partitions. Re-run
the forced-order retirement/publication latch which closed parent finding
`W5C5-002`; an isolated or repeated pass remains insufficient evidence for
concurrency regressions. Then perform a focused post-NC review of:

- canonical-state identity across regional/net role changes;
- inline-versus-spilled results;
- claims and dependency ownership;
- mutation barriers and cursor deferral at a checkpoint pair;
- zero checkpoint trait/copy interlocks in the D.2c and P3 inventories;
- boxing and complete common-state node-size effects;
- profiling determinism;
- remaining synchronous evaluator compatibility; and
- drift in W6C-W8 caused by the new checkpoint vocabulary.

Exit: W6B.4b.2 is complete, reviewed, and ready for parent W6B.4b closure.

## Verification Matrix

| Callable situation | Large budget | Forced yield | Dependency boundary | Required topology |
|---|---|---|---|---|
| Immediate callable | direct result | direct result | n/a | no checkpoint |
| Cached lazy chain | direct result | same result | n/a | checkpoint only at forced split |
| Assigned promise chain | direct result | same result | n/a | checkpoint only at forced split |
| Unresolved lazy | eventual direct result | same result | exact lazy wait | one blocked checkpoint |
| Unassigned promise | eventual direct result | same result | exact promise wait/follower | one blocked checkpoint |
| Reflection-backed lazy | one reflection task | same task | exact lazy wait | no source replay |
| Frame-bearing fixture | same state/result | same state/result | exact dependency | every frame retained |
| Mixed deferred cycle | same failure | same failure | same cycle behavior | no replayed prefix |
| Deferred non-callable | same stuck failure | same failure | optional earlier wait | stuck once |
| Raw net result | copied source | same source | optional earlier wait | checkpoint removed directly |
| Partial function | one ordinary application | same value | optional earlier wait | checkpoint removed directly |
| Cursor reaches checkpoint pair | eventual copied result | same result | source-pair dependency | no target checkpoint |
| Fan/erase reaches checkpoint | stuck | stuck | n/a | no duplication or erasure rule |

Every forced split asserts semantic result, complete state continuity, claim
absence after return, producer identity, and expected profiling counts. Thread
repetition alone is not evidence for a concurrency row.

## Deferred Optimization

After correctness and profiling, consider:

- a compact small-set representation for short followed-identity paths;
- batching several pure semantic active-pair steps within one access region;
- incorporating callable normalization into future annotated normalization or
  JIT policies; and
- only after measurement, a specialized callable checkpoint which preserves
  zero-walk ownership transfer and does not duplicate the WHNF work algebra.

None may reintroduce durable claims, unrooted machine state, or dependence on
a particular worker's Rust stack.
