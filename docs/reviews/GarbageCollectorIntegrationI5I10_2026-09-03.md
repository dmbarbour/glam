# Glam GC Integration Phases I5-I10 Forward Review — 2026-09-03

Baseline: `0091a62`, including completed and reviewed Phases I1-I4. This is a
forward review of the pending integration plan, not a post-implementation
certification of I5-I10.

Status: complete as a review. The low-risk documentation and phase-shape
findings are resolved in the integration plan. Three related representation
questions remain open and block I5 implementation: transitive managed-edge
closure, the ordering of managed core-net ownership relative to the first
managed recursive value, and the durable/scoped/weak handle model for lazy and
promise identities. They require an explicit I5.0 design review rather than an
incidental choice during implementation.

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

This review does not select the I5 lazy/promise handle representation, enable
production collection, implement a managed core net, or choose the I10 opaque
representation.

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
exact root or managed-edge disposition immediately. The plan must decide
whether to pull the managed-net ownership core ahead of I5 or introduce a
temporary rooted-net payload representation. The former appears cleaner, but
this review deliberately leaves the choice for I5.0.

The same first-managed-identity boundary exposes handle questions which the
old `Arc` representation concealed. Evaluator machines and coordinator work
currently retain `LazyValue` and `PromisedValue` directly; task-owned promise
obligations deliberately retain only `Weak<PromiseCell>`. A managed identity
needs distinct vocabulary for interior `Gc` edges, durable registered roots,
bounded scoped access, and edge-free coordination. `glam-gc` currently has no
weak managed-pointer facility, so promise liveness must be decided explicitly.

Later work can be simplified. I9 should be a delta audit over ownership changed
by I5-I8 rather than a repetition of I4F's complete owner proof. I7 remains a
persistent-container coverage and performance audit, not the first point at
which nested managed edges become traceable. I6 and I8 need finer checkpoints,
while I10 needs its closure wording updated to the external host-call boundary
which actually remains.

## Findings

### I5F-001 — The first managed recursive identity requires transitive trace closure

**Classification:** trace chronology and soundness blocker  
**Priority:** critical  
**Confidence:** high  
**Status:** open; blocks I5

`ManagedValueNode::trace_managed_edges` is correctly zero-edge for the current
I4 representation. The I5 text says to replace `Arc<LazyCell>` and update I4's
compatibility visitor, but a managed lazy or promise can be nested beneath a
list, dictionary, partial application, metadata carrier, failure, lazy source,
or net payload. The production node and any other managed family containing
such a compatibility payload must discover the new pointer in the same
checkpoint.

I5.0 must select one central, wildcard-free managed-edge walk. It may reuse the
logical child enumeration proven by `CompatibilityValueEdges`, but it must
ultimately call the collector visitor for every actual `Gc` and stop at managed
identity boundaries. It must neither recursively follow a managed identity nor
force, format, compare, or evaluate a compatibility payload. Each later family
migration replaces its corresponding compatibility step without changing that
transitive rule.

Required evidence before I5A:

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
**Status:** open; blocks I5

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

I5.0 must compare two concrete transitions:

1. move I8A plus the representation, durable-handle, and exact-current-trace
   core of I8B before I5, retaining later I8 payload/cycle audits after I5-I7;
2. introduce a temporary exact rooted representation for every value-bearing
   external net surface, then remove it during I8.

The first option avoids deliberate conservative retention and duplicate
migration work and is provisionally preferred. No ordering change is committed
until the handle discussion also establishes how a durable net root differs
from an interior cross-net `Gc` and bounded `CoreRuntimeNetAccess`.

### I5F-003 — Lazy and promise handle roles are not yet explicit

**Classification:** representation and lifecycle design blocker  
**Priority:** high  
**Confidence:** high  
**Status:** open; blocks I5

The current `Arc` identities serve several roles simultaneously:

- a recursive semantic edge inside `Value`;
- an identity/ID and diagnostic-label source;
- durable ownership in deferred producers and evaluator machines;
- promise assignment and lazy cache access;
- completion subscriptions and producer lookup; and
- a weak promise target retained by task/local producer obligations.

Those roles cannot all become one bare `Gc<LazyCell>` or `Gc<PromiseCell>`.
I5.0 must inventory direct method and field use and select distinct types for:

- the managed semantic identity stored as an exact graph edge;
- a registered durable handle for parked machines, coordinator work, resolver
  state, and any other cross-region owner;
- bounded access to source, result, assignment, and diagnostic data; and
- an edge-free coordination companion for IDs, subscriptions, waits, and
  notifications where semantic ownership is unnecessary.

Task-owned promise obligations currently use `Weak<PromiseCell>` so forgotten
promises are not retained until their producer terminates. Because `glam-gc`
has no weak managed pointer, I5.0 must choose among a collector-aware weak
facility, deriving producer failure from the existing terminal wait without
writing an otherwise unreachable cell, or stronger rooted retention with its
costs and cycle behavior documented. This decision belongs before I5B; I5C
then verifies external resolver/producer retirement rather than inventing the
representation after publication paths have already migrated.

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
**Status:** partially resolved; final subdivision follows I5.0

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
**Status:** open; resolved together with I5F-002

Current I8B combines the generic owner seam, managed outer cell, durable and
interior handle representations, cross-net edges, trace locking, every
mutation gateway, owner migration, passive-drop admission, and deletion of all
compatibility adapters. These are independent proof boundaries.

The final plan must separate at least:

1. generic owner seam;
2. managed outer cell plus durable root/interior `Gc`/scoped-access model;
3. cross-net edges and exact current trace;
4. mutation gateways and lock invariant;
5. durable-owner migration and drop/layout admission; and
6. final compatibility-adapter deletion after every I5-I7 consumer has been
   replaced.

If the net ownership core moves before I5, only the final payload audit, cycle
matrix, and adapter deletion remain in chronological I8.

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

1. Complete the plan-only corrections recorded by I5F-004, I5F-006,
   I5F-008, and I5F-009.
2. Hold the I5.0 design discussion covering I5F-001 through I5F-003 and the
   representation portion of I5F-007.
3. Rewrite and partition I5 plus the affected portion of I8 from that decision.
4. Partition I6 using the resulting managed-edge and durable-handle vocabulary.
5. Implement I5, then perform the normal mandatory post-I5 review before
   continuing.
6. Revisit I7-I10 checkpoint sizes at their major-stage boundaries; merge or
   delete audit-only checkpoints when the phase-entry delta proves they add no
   evidence.

Production remains `CollectionPolicy::NoAuto`. This review authorizes no
forced collection over the full production runtime and does not advance Gate
G2.
