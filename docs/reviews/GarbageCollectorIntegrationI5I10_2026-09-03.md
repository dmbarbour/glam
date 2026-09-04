# Glam GC Integration Phases I5-I10 Forward Review — 2026-09-03

Baseline: `0091a62`, including completed and reviewed Phases I1-I4. This is a
forward review of the pending integration plan, not a post-implementation
certification of I5-I10.

Status: complete as a review and updated after the recursive-identity closure
discussion. The integration plan now prepares lazies, promises, and production
core nets separately but changes their production representation in one
indivisible cutover. That resolves transitive managed-edge closure and core-net
chronology without a temporary rooted-net layer. The durable/scoped/weak
handle model was subsequently closed by the source-backed I5F-003 disposition
inventory below. Implementation may now enter I5A; the exact private type
names and the generic-net guard shape remain checkpoint-local engineering
choices rather than semantic blockers.

## Scope

The mandatory post-I4 review established the implemented I4 boundary and
checked the later handoff only at a high level. This review re-derives pending
Phases I5-I10 against the final I4 representation:

```text
RuntimeValueRoot
  -> inline integer
  |  Root<ManagedValueNode>
       -> compatibility-owned core::Value
```

It asks:

- when the first interior `Gc` pointer can enter each compatibility owner;
- which production trace must become exact in the same checkpoint;
- which identities may remain scoped and which require registered roots;
- whether scheduler, promise, net, and external-owner lifecycle assumptions
  still match current code;
- whether isolated collection fixtures remain closed and legal before Gate
  G2; and
- whether the remaining phase boundaries are small enough to verify without
  combining independent representation and lifecycle decisions.

The original forward-review pass did not select the I5 lazy/promise handle
representation. The 2026-09-04 I5F-003 addendum below now selects its semantic
owner roles while leaving private type layout to the implementation
checkpoints. This review still does not enable production collection,
implement a managed core net, or choose the I10 opaque representation.

## Summary

The overall direction remains sound, but the original variant-by-variant
phase order understates one transitive property: after the first recursive
identity becomes a `Gc`, that pointer may occur below every compatibility
container capable of holding a `Value`. Exact production tracing must therefore
walk through compatibility-owned containers until it reaches managed
identities; updating only the newly managed `Value` arm is insufficient.

The synchronized core-net owner is the important chronology case. A durable
`CoreRuntimeNet` currently owns ordinary `Value` payloads outside the managed
outer value node. Once any such payload may contain a `Gc`, the net needs an
exact root or managed-edge disposition immediately. The accepted resolution
therefore treats lazy cells, promise cells, and production core-net cells as
one recursive-identity closure: preparatory checkpoints may remain
representation-neutral, but no buildable production state may manage only a
subset of those three identities.

The same first-managed-identity boundary exposes handle questions which the
old `Arc` representation concealed. Evaluator machines and coordinator work
currently retain `LazyValue` and `PromisedValue` directly; task-owned promise
obligations deliberately retain only `Weak<PromiseCell>`. A managed identity
needs distinct vocabulary for interior `Gc` edges, durable registered roots,
bounded scoped access, and edge-free coordination. `glam-gc` currently has no
weak managed-pointer facility, so promise liveness must be decided explicitly.

The atomic cutover relies on one transitive compatibility walk. It recursively
crosses ordinary Rust-owned value structure, reports a `Gc` when it reaches a
managed lazy, promise, or net identity, and stops at that identity. The
collector worklist, not the compatibility walk, follows the managed edge. The
structural invariant is that removing those three managed identity families
from the semantic graph leaves an acyclic compatibility-owned graph.

Later work can be simplified. I9 should be a delta audit over ownership changed
by I5-I8 rather than a repetition of I4F's complete owner proof. I7 remains a
persistent-container coverage audit, not the first point at which nested
managed edges become traceable. Compatibility traversal performance and its
eventual replacement belong to Value Representation Refinement rather than
this integration. I6 and the post-cutover I8 audit still need bounded
checkpoints, while I10 needs its closure wording updated to the external
host-call boundary which actually remains.

## Findings

### I5F-001 — The first managed recursive identity requires transitive trace closure

**Classification:** trace chronology and soundness blocker  
**Priority:** critical  
**Confidence:** high  
**Status:** resolved in plan

`ManagedValueNode::trace_managed_edges` is correctly zero-edge for the current
I4 representation. The I5 text says to replace `Arc<LazyCell>` and update I4's
compatibility visitor, but a managed lazy or promise can be nested beneath a
list, dictionary, partial application, metadata carrier, failure, lazy source,
or net payload. The production node and any other managed family containing
such a compatibility payload must discover the new pointer in the same
checkpoint.

The plan now selects one central, wildcard-free managed-edge walk. It may reuse
the logical child enumeration proven by `CompatibilityValueEdges`, but it
ultimately calls the collector visitor for every actual `Gc` and stops at
managed lazy, promise, and net identity boundaries. It neither recursively
follows a managed identity nor forces, formats, compares, or evaluates a
compatibility payload. Later family migrations may replace individual
compatibility steps without changing that transitive rule.

Required evidence is now distributed across I5A-I5F:

- a nested matrix placing the first managed identity under every current
  compatibility owner which can hold a `Value`;
- a source latch proving the production `ManagedValueNode` uses the central
  walk rather than leaving the affected arm at zero edges; and
- rooted-survival and unrooted-reclamation fixtures which would fail if any
  enclosing compatibility step omitted the new identity.

### I5F-002 — Core-net ownership is ordered too late for the first managed payload

**Classification:** ownership chronology blocker  
**Priority:** critical  
**Confidence:** high  
**Status:** resolved in plan

`CoreRuntimeNet` remains an `Arc`-owned synchronized graph with ordinary
`Value` and `CoreOperator` payloads. Core contention, normalization, frontier,
call, and cursor descriptors can also carry cloned net identities outside one
managed-access scope. I8 currently introduces the managed outer net only after
I5-I7.

Once a net payload can contain `Value::Lazy(Gc<_>)` or another managed identity,
an external net which survives a mutator region cannot remain merely an
unregistered compatibility owner. Production `NoAuto` prevents immediate
collection but is not a trace or lifetime proof, and the integration plan
already forbids using disabled collection to excuse a durable bare pointer.

The accepted resolution is stricter than merely moving I8 earlier: I5 prepares
the net seam and managed outer cell alongside lazy and promise cells, then one
atomic production checkpoint changes all three recursive identities together.
This avoids both a temporary rooted-net payload representation and a mixed
state in which a raw `Arc` identity must be traversed recursively to discover
managed values beneath it. The later I8 phase becomes a post-cutover net and
mutation audit rather than the point where managed net identity first appears.

The I5F-003 inventory now distinguishes a durable net root from an interior
cross-net `Gc` and bounded `CoreRuntimeNetAccess`. The exact private guard
layout remains an I5C.1 implementation choice and does not change the
chronology.

### I5F-003 — Lazy and promise handle roles required an explicit disposition

**Classification:** representation and lifecycle design blocker  
**Priority:** high  
**Confidence:** high  
**Status:** resolved in plan; implementation remains I5

The current `Arc` identities serve several roles simultaneously:

- a recursive semantic edge inside `Value`;
- an identity/ID and diagnostic-label source;
- durable ownership in deferred producers and evaluator machines;
- promise assignment and lazy cache access;
- completion subscriptions and producer lookup; and
- a weak promise target retained by task/local producer obligations.

Those roles cannot all become one bare `Gc<LazyCell>` or `Gc<PromiseCell>`.
I5.0 therefore inventoried direct method and field use and selected distinct
roles for:

- the managed semantic identity stored as an exact graph edge;
- a registered durable handle for parked machines, coordinator work, resolver
  state, and any other cross-region owner;
- bounded access to source, result, assignment, and diagnostic data; and
- an edge-free coordination companion for IDs, subscriptions, waits, and
  notifications where semantic ownership is unnecessary.

Task-owned promise obligations currently use `Weak<PromiseCell>` so forgotten
promises are not retained until their producer terminates. Because `glam-gc`
has no weak managed pointer, I5.0 had to choose among a collector-aware weak
facility, deriving producer failure from the existing terminal wait without
writing an otherwise unreachable cell, or stronger rooted retention with its
costs and cycle behavior documented. The disposition inventory below selects
obligation-scoped registered roots and, critically, reverses the current
producer link so the managed promise cell has only a weak backlink to the
external obligation which owns its root.

#### I5F-003 disposition inventory — 2026-09-04

The source review covered every production occurrence of `LazyValue`,
`PromisedValue`, `PromiseCell`, and `CoreRuntimeNet`, plus every production
`Weak<_>` field which can affect their liveness. Test-only weak probes were
excluded after confirming that they do not define another production owner.
The inventory uses roles rather than committing prematurely to final private
type names:

- **M** is an exact managed semantic edge, represented by `Gc<Cell>` after the
  atomic cutover;
- **R** is a cloneable registered root which may survive a mutator region;
- **A** is a non-escaping access/view branded by the matching
  `RuntimeValueAccess` region; and
- **C** is edge-free coordination containing IDs, revisions, registrations,
  atomics, condition variables, or weak runtime routes, but no `Gc`, `Root`,
  `Value`, or active external owner.

| Current owner/use | Source evidence | Final disposition | Confidence and reason |
| --- | --- | --- | --- |
| `Value::Lazy`, `ListThunk::Lazy`, and lazy identities reached through raw compatibility `Value` structure | `src/core.rs`; `src/core/managed/payload_edges.rs` | **M**: exact `Gc<LazyCell>` stop edge | High. These are recursive semantic graph edges, never independent Rust liveness claims. |
| `Value::Promised`, `ListThunk::Promised`, and `EvaluationHalt::UnassignedPromise` | `src/core.rs`; `src/core/evaluation_halt.rs` | **M**: exact `Gc<PromiseCell>` stop edge | High. The halt may itself be retained by a net, so it cannot hide a root or become coordination-only. |
| `NetValue`, `FunctionCode`, function stages, lazy net sources, net payloads, and cross-net copy sources | `src/core.rs`; `src/core_net.rs`; `src/interaction_net/runtime.rs` | **M**: exact `Gc<CoreRuntimeNetCell>` edge | High. These links are what make net/value cycles collectible. |
| `LazyCell::source` and terminal result | `src/core.rs` | Managed cell fields with exact child traversal and one publication gateway | High. Terminal result is published before source release; neither field may contain a registered root. |
| `PromiseCell::assignment` | `src/core.rs` | Managed semantic result/failure edge, not `RuntimeValueRoot` | High. A rooted assignment inside the cell would make an ordinary promise backedge uncollectible. |
| Core net topology, values, operators, stuck reasons, and source-net links | `src/interaction_net/runtime.rs`; `src/core_net.rs` | Managed cell state with exact tracing and mutation gateways | High. The current semantic mutex/revision boundary remains authoritative. |
| `LazyTaskMachine::lazy`, `DeferredProducer::Lazy`, and `DeferredLazyCycleMember::lazy` | `src/eval/value.rs`; `src/evaluation/coordinator/deferred.rs` | **R**: registered lazy root for each durable owner | High. Each can remain parked after its evaluator access region closes and must still publish a cache result or failure. Duplicate clones of one root cell are acceptable initially. |
| `PromiseFollower::promise`, `DeferredProducer::Promise`, reflection `ActiveFix::handle`, and `Continuation::Fix` | `src/eval/value.rs`; `src/evaluation/coordinator/deferred.rs`; `src/reflection/machine.rs` | **R**: registered promise root | High. These are parked machine or coordinator state, not semantic interiors. |
| `LazyTaskWork::Follow`, `PromiseFollowerState::FollowAssignment`, and other parked machine values which may contain any recursive identity | `src/eval/value.rs`; `src/reflection/machine.rs`; `src/eval/builtins/net/construction.rs` | Existing general `RuntimeValueRoot`, not a family-specific bare `Gc` | High. I4 already established the correct general durable-value surface. |
| Public `PromiseResolver` | `src/api/value.rs`; constructors in `src/api/assembly.rs` | `RuntimeValueObserver` plus `Option<R>` for the promise | High. The weak observer reopens the matching runtime; `Option` is solely the affine `Drop`-disarm state. A live resolver intentionally retains its promise even if the public value was discarded. |
| Coordinator `TaskOwnedPromiseObligation` and direct-runner `LocalPromiseObligation` | `src/evaluation/coordinator.rs`; `src/evaluation/coordinator/task.rs` | External producer obligation owns **R** until that individual promise settles | High. Retention is bounded by unresolved producer obligations already tracked today; disappearing observers do not semantically cancel the producer. The root is removed on promise settlement, not delayed until whole-task retirement. |
| `PromiseCell::producer` | `src/core.rs`; `src/evaluation/coordinator/task.rs` | Weak backlink to an externally owned `Arc<PromiseProducerObligation>` (or an equivalent weak/scalar **C** link) | High. The current strong cell-to-obligation direction must reverse when the obligation gains a root, otherwise `cell -> obligation -> Root<cell>` is a permanent hidden cycle. Coordinator/local owner records become the strong obligation owners. |
| `WorkDependency::Promise` | `src/evaluation/coordinator.rs`; `src/evaluation/coordinator/{completion,client_demand,spark}.rs` | **R** while the dependency record/subscription is live | High. The dependency actively observes assignment, subscribes, and may project a producer wait. Sharing the already registered root is simpler than a weak managed pointer and adds no new semantic retention: the blocked machine, spark input, or client demand already owns the demanded value. |
| Lazy/promise ID and label reads, source/result/assignment inspection, cache/assignment publication | `src/core.rs`; `src/eval/value.rs`; `src/evaluation/session.rs` | **A** through matching runtime access | High. IDs and labels remain in the managed cell. Cycle diagnostics copy `LazyId` plus label only when constructing the diagnostic; no permanent duplicate label is required. |
| `CompletionSubscriptions` and promise producer lookup | `src/evaluation/coordinator/completion.rs`; `src/evaluation/coordinator/task.rs` | **C** plus bounded access to the rooted promise where assignment state is needed | High for the boundary, medium for the exact field split. Registrations and weak coordinator routing are edge-free. A managed cell must not strongly reach an `EvaluationWaitToken` capable of later holding rooted terminal data. |
| `CoreRuntimeNetAccess`, call/operator claims, `NormalizationRequest`, `NetDriverWork`, `CoreFrontierObservation`, and `CorePreparedCopySource` before installation | `src/core_net.rs`; `src/eval/net.rs` | **A**, with a source-net **M** edge installed when a prepared copy enters topology | High. These values exist only inside one callback-free evaluator quantum and can be scope-branded instead of rooted. |
| Core net construction or another handoff which must survive before installation into a managed semantic owner | `src/core_net.rs`; front-end/evaluator constructors | **R** until publication, then replace it with **M** | High. A constructor must never publish or park a bare `Gc`. |
| `CoreNetContention` | `src/core_net.rs`; `src/eval/net.rs` | **C**: disturbance signal plus observed revision, while the enclosing request/access retains net liveness | Medium-high. It needs synchronization, not semantic ownership. The precise signal placement depends on the I5C.1 generic-net seam. |
| Generic `NormalizationBatchLease<CoreSpecialization>` | `src/interaction_net/runtime.rs`; `src/core_net.rs` | Core use becomes an access-bounded guard or weak edge-free **C** lease; generic non-core `SharedRuntimeNet` may retain its current `Weak<SharedRuntimeNetInner<_>>` | Medium. The semantic disposition is fixed—no weak managed pointer and no durable root—but I5C.1 must choose the least intrusive generic API shape. |

The producer-root ownership graph is therefore:

```text
coordinator work record or LocalPromiseOwner
  -> Arc<PromiseProducerObligation>
       -> Root<PromiseCell>
       -> producer/wait IDs and weak owner/coordinator route

PromiseCell
  -> Weak<PromiseProducerObligation>
  -> completion registrations with weak coordinator route
```

Assignment temporarily upgrades the weak backlink, publishes the assignment,
removes the authoritative obligation and its root, detaches wakes, leaves all
locks/access regions, and only then notifies. Task termination instead walks
the same rooted obligations and fails each still-unresolved promise. This
preserves one terminal winner without requiring `Weak<Gc<_>>` or
`WeakRoot<_>`.

#### Existing weak edges outside the promise ownership inversion

| Weak edge | Final disposition | Confidence |
| --- | --- | --- |
| `RuntimeValueObserver -> RuntimeValueDomain` and `RuntimeValueDomain -> EvaluationWorkCoordinator` | Keep weak. Values/observers must not retain the runtime, and the value domain must not close the runtime/coordinator cycle. | High |
| Coordinator demand-session registry, spark demand, and client-demand work -> `EvaluationDemandState` | Keep weak. Work cannot manufacture or prolong the external demand-owner lease. | High |
| Executor, task handle, client-demand handle, wait/completion source -> coordinator | Keep weak. These are observation/control routes; escaped handles must not retain runtime execution. | High |
| `PromiseProducerSource::{Coordinator,Local}` -> coordinator/local owner | Keep weak inside the producer obligation. The authoritative owner already owns the obligation, so a reverse strong link would cycle. | High |
| `NormalizationBatchLease -> SharedRuntimeNetInner` | Keep for non-core generic nets only. Replace the core specialization as described above. | Medium |
| Collector root registry/TLS -> heap | Keep weak; this is the established `glam-gc` lifetime boundary and is independent of I5 semantic handles. | High |
| Diagnostic ingress/bus, runtime event endpoint, reflection query domain, opaque external-owner lease, and effect-token domain weak routes | Keep weak and unchanged. They prevent unrelated external-owner/runtime cycles and do not point at lazy, promise, or core-net cells. | High |

No other production `Weak<_>` occurrence is a candidate weak semantic pointer
for I5. In particular, task-owned and local promise-cell weakness is the only
current weak edge whose *payload liveness* changes at the cutover.

#### Remaining implementation-local choices

The following do not reopen the semantic decision, but deserve explicit I5C
checkpoints and focused tests:

1. **Net guard layout (medium confidence):** choose whether the core
   normalization guard borrows the managed cell directly or owns a weak
   edge-free signaling companion. It must be non-escaping and generic
   non-core specializations must remain collector-independent.
2. **Promise coordination split (medium confidence):** select the smallest
   cell-resident edge-free fields which preserve subscribe-and-recheck while
   ensuring the cell cannot reach a wait token after that token contains a
   rooted terminal value.
3. **Snapshot-to-machine conversion (medium-high confidence):** a lazy source
   snapshot is scoped. Any branch retained across a yield must project its
   recursive payloads to existing roots before the access region closes; the
   implementation may root the whole retained value instead of inventing
   family-specific fields.
4. **Private naming (low risk):** names such as `RuntimeLazyRoot` and
   `RuntimePromiseRoot` are provisional. The proof is the M/R/A/C role and
   source inventory, not a particular spelling.

Rejected foreign-runtime input to a consuming `PromiseResolver` remains a
separate public-API question and is explicitly outside this GC integration
plan. I5 preserves the existing behavior; its handle migration must not change
that behavior accidentally.

### I5F-004 — The I5-I10 verification boundary describes I4 ownership too strongly

**Classification:** documentation drift  
**Priority:** medium  
**Confidence:** high  
**Status:** resolved in plan

The boundary formerly said every value surviving a mutator was already a root
or managed edge. I4 deliberately also permits compatibility payload ownership
inside a registered `ManagedValueNode`, with compile-exhaustive adapters acting
as the migration oracle. The revised wording distinguishes durable boundary
values, compatibility-owned interiors, and the point at which a later
representation change first introduces a managed pointer.

The common family rule now also requires exact trace, stable layout/admission,
passive-drop record, rooted survival, and unrooted reclamation before a managed
family's isolated collection.

### I5F-005 — I6 combines independently risky representation boundaries

**Classification:** checkpoint-size and terminology drift  
**Priority:** medium  
**Confidence:** high  
**Status:** partially resolved; final subdivision follows the I5 cutover

Functions/stages, partial applications, fixpoint/capture state, metadata,
failures, reflection computation, and net-construction payloads do not share
one access or lifecycle model. In particular, `RuntimeFailureRoot` already
provides shallow rooted compatibility ownership, while reflection effect and
target values now live in the external-owner registry and net construction
still owns an `Arc<Value>`.

The plan now separates reflection from net construction and corrects the stale
I4E reference to I4C. Before I6 begins, it should further split I6A into
partial application, fixpoint/capture, and function-stage checkpoints, and I6C
should explicitly distinguish managed failure identity from durable rooted
failure reports. Exact subphases depend on the central trace and handle
vocabulary selected by I5.0.

### I5F-006 — I7 is an audit, not deferred first tracing

**Classification:** phase-role and verification clarification  
**Priority:** medium  
**Confidence:** high  
**Status:** resolved in plan

Lists and dictionaries must expose a nested I5 managed identity as soon as that
identity exists. I7 therefore audits the already-active logical traversal,
exercises the real production collection shape in a fresh isolated runtime,
and measures duplicate work. It does not introduce the first sound traversal
or require a performance threshold before evidence exists.

### I5F-007 — I8B is too large and final compatibility removal is order-dependent

**Classification:** implementation-risk partitioning  
**Priority:** high  
**Confidence:** high  
**Status:** resolved in plan

Current I8B combines the generic owner seam, managed outer cell, durable and
interior handle representations, cross-net edges, trace locking, every
mutation gateway, owner migration, passive-drop admission, and deletion of all
compatibility adapters. These are independent proof boundaries.

The revised plan separates:

1. generic owner seam;
2. managed outer cell plus durable root/interior `Gc`/scoped-access model;
3. cross-net edges and exact current trace;
4. mutation gateways and lock invariant;
5. durable-owner migration and drop/layout admission; and
6. final compatibility-adapter deletion after every I5-I7 consumer has been
   replaced.

I5 now owns items 1-5 as preparation plus one indivisible recursive-identity
cutover. Chronological I8 retains the final payload/mutation audit and retires
only obsolete net-specific compatibility adapters. The central compatibility
walk remains until Value Representation Refinement replaces the raw value
representation; deleting it is no longer an I8 completion criterion.

### I5F-008 — I9 repeats completed I4F ownership work

**Classification:** redundant planned work  
**Priority:** low  
**Confidence:** high  
**Status:** resolved in plan

I4F already installed and tested durable root surfaces across caches,
coordinator/evaluation, reflection, diagnostics/events, compiler/assembly/CLI,
and external owners. I9 formerly repeated those subsystem audits even where
I5-I8 changed no ownership or lifecycle boundary, and still described the
`Any` registration boundary as work to close.

I9 now begins with a source-backed delta inventory. Subsystem follow-ups are
created only for ownership, retirement, or representation changes introduced
by I5-I8. External active-RAII reconciliation and the final runtime-root source
inventory remain mandatory. Discovering a first durable root conversion still
reopens the checkpoint which introduced the managed edge.

### I5F-009 — I10A names a retired semantic-closure problem

**Classification:** documentation drift and pending containment decision  
**Priority:** medium  
**Confidence:** high  
**Status:** resolved in wording; implementation remains I10

Production semantic thunks no longer use `Arc<dyn Fn(&EvalContext) -> ...>`;
they use a function pointer plus explicit value captures. The remaining opaque
closure boundary is the external `HostCallOperation`, whose callback accepts no
evaluator context and returns a `RuntimeValueRoot`.

I10A now focuses on proving or replacing those external closure environments.
`HostCallRecord` is useful source classification but cannot prove what an
arbitrary Rust closure captured. Implementation must either use explicit typed
same-runtime root bundles, eliminate value-capturing arbitrary closures, or
record a deliberately conservative external-owner policy. A test cannot claim
to mechanically reject an internal backedge hidden in an unrestricted Rust
closure.

I10B.0 remains a sound hard decision gate. I10C's eventual verification matrix
must omit managed-opaque destruction cases if that review selects external-only
opaque storage.

## Recommended Resolution Order

1. Begin I5A from the completed I5.0 handle inventory recorded by I5F-003.
2. Finalize the dormant family/root/access types in I5C from that decision;
   the transitive-walk and atomic-cutover chronology is already fixed.
3. Implement I5A-I5F, then perform the normal mandatory post-I5 review before
   continuing.
4. Partition I6 using the resulting managed-edge and durable-handle vocabulary.
5. Revisit I7-I10 checkpoint sizes at their major-stage boundaries; merge or
   delete audit-only checkpoints when the phase-entry delta proves they add no
   evidence.

Production remains `CollectionPolicy::NoAuto`. This review authorizes no
forced collection over the full production runtime and does not advance Gate
G2.
