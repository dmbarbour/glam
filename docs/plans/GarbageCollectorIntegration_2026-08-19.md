# Glam GC Integration Plan — 2026-08-19

Status: planned; blocked on collector Gate G1 before managed ownership changes.

This plan integrates the collector defined by
[`GarbageCollectorImplementation_2026-08-19.md`](GarbageCollectorImplementation_2026-08-19.md)
into the runtime, public value facade, evaluator, workers, reflection, and
interaction nets. Cross-plan invariants and enablement gates live in
[`GarbageCollectionRoadmap_2026-08-19.md`](GarbageCollectionRoadmap_2026-08-19.md).

## Phase Status

| Phase | Status | Outcome |
| --- | --- | --- |
| I0 | pending | complete ownership and mutation ledger |
| I1 | pending | runtime-owned heap, collection disabled |
| I2 | pending | public value and external-root prototype |
| I3 | pending | bounded evaluator/worker mutator regions |
| I4 | pending | core trace vocabulary and leaf policy |
| I5 | pending | managed lazies and promises |
| I6 | pending | functions, applications, metadata, failures |
| I7 | pending | persistent list and dictionary tracing |
| I8 | pending | interaction-net tracing and barriers |
| I9 | pending | runtime-owned root surfaces |
| I10 | pending | deferred closures and opaque boundaries |
| I11 | pending | whole-production-graph forced collection |
| I12 | pending | runtime maintenance and threshold collection |
| I13 | pending | barrier audit and minor collection |
| I14 | pending | redundant ownership removal and documentation |

## Current Boundary

`RuntimeValueRoot` currently stores `{EvaluationRuntimeId, core::Value}`. It
protects provenance but does not register a collector root. Recursive core
ownership includes at least:

- `Arc<LazyCell>` and its source/result graph;
- `Arc<PromiseCell>` and successful assignments;
- metadata carriers;
- partial builtin arguments, lazy applications, fixpoints, reflection tasks,
  function stages, and net construction inputs;
- RPDS dictionary values and FingerTree/list chunks;
- shared mutable interaction nets containing core data;
- runtime value caches and compiler attachments;
- reflection store snapshots, volumes, queries, and transactions;
- task waits, client demands, sparks, deferred work, diagnostic values, and
  event input/output records; and
- type-erased opaque payloads and deferred Rust closures which may hide public
  roots.

No production collection may run until this graph is completely classified.

## Intended Value Shape

The provisional target keeps cheap scalar values inline and moves recursive
identity-bearing nodes behind managed pointers:

```text
core::Value
├── Atom / Builtin                 inline
├── Number / Binary                existing immutable leaf ownership initially
├── List / Dict                    persistent containers traced logically
├── Function / partial application traced recursive payloads
├── Net                            traced shared runtime-net payload
├── Lazy / Promised                Gc-managed identity cells
├── Metadata                       Gc-managed identity cell
└── Opaque                         conservative host boundary

api::Value
└── RuntimeValueRoot
    └── glam_gc::Root<core::Value or root cell containing core::Value>
```

Whether the external root points directly at a managed `Value` or at a
registered root cell containing an inline `Value` is decided during Phase I2.
The latter avoids allocating every number and atom merely because it crosses
the public API.

## Phase I0 — Ownership and Mutation Ledger

Create a dated graph inventory beside this plan. For every graph-bearing type,
record:

- owner and source path;
- outgoing managed-value edges;
- whether it is immutable, replaceable, one-write, or freely mutable;
- current synchronization and lock order;
- whether it can live outside an evaluator call or worker quantum;
- whether it can cross threads;
- required trace strategy;
- required generational barrier;
- destruction behavior; and
- migration phase.

The inventory must explicitly cover every `core::Value` variant, every type
containing a core or public `Value`, every `Arc<dyn Fn...>` which can capture a
value, every `OpaqueValue` payload family owned by Glam, and every interaction-
net specialization carrying core data.

Add tests which latch current semantics before changing representation:

- public value clone, equality, and cross-runtime rejection;
- fulfilled and unfulfilled lazy/promise behavior;
- lazy, promise, metadata, function, collection, and net cycles;
- value transfer between workers in one runtime;
- runtime drop with escaped public values;
- reflection store snapshots and task status after owner-session closure; and
- settlement and diagnostic/event retention.

This is an audit phase. Missing edges block later collection rather than being
classified optimistically.

## Phase I1 — Runtime Heap Ownership, Collection Disabled

- Add one `RuntimeHeap` to `EvaluationRuntime` ownership and the internal
  `RuntimeSharedResources` view.
- Keep the heap inside the runtime value domain. Escaped public roots may keep
  it alive after the `EvaluationRuntime` facade is dropped, just as escaped
  values currently retain their supporting value resources.
- Do not create a `heap -> runtime state -> heap` ownership cycle.
- Give the value factory a narrow allocation/rooting handle, not raw collector
  internals.
- Add runtime-local tuning with collection disabled by default.
- Verify the earlier sibling allocation of runtime state and immutable profile
  remains acyclic once both can retain rooted values.

At this phase no core object is reclaimed by the new heap. Existing behavior
and tests must remain unchanged.

## Phase I2 — External Root and Public `Value` Prototype

- Prototype both root representations against scalars and one recursive test
  node.
- Select the representation which keeps public scalar construction cheap while
  ensuring one public `Value` clone cannot lose liveness.
- Make `RuntimeValueRoot` obtain runtime provenance from its heap/root rather
  than maintaining independently forgeable duplicated provenance.
- Keep `api::Value` freely cloneable and `Send + Sync` when its contents are.
- Preserve the current public equality and debug semantics deliberately.
  Root-pointer identity is not a substitute for `core::Value` equality, and
  debug formatting must remain non-forcing. If these operations enter a
  mutator region internally, test their recursion and collection-request
  behavior explicitly.
- Replace direct `as_core` escape paths with scoped access under the correct
  mutator authority.
- Eliminate ownership-taking `into_core` paths which could let an unrooted
  managed pointer escape its region. Internal consumers borrow or clone the
  compact core shell within an enclosing mutator scope instead.
- Preserve the public `EvaluatedValue` WHNF witness without making it a second
  root model.

Verification includes public API tests moving roots between threads, dropping
the runtime facade before the last value, rejecting roots from another
runtime, and nesting construction/evaluation entries recursively.

Collection remains disabled for the production graph.

## Phase I3 — Mutator Regions Across Evaluation and Construction

Introduce region boundaries before managed pointers require them:

- public `Values` construction and composition;
- evaluator demand and extraction;
- one cooperative or worker-owned evaluation quantum;
- interaction-net call/reduction entry;
- reflection machine polling and request interpretation;
- compiler/macro closed-value construction;
- runtime event admission/delivery encoding and decoding; and
- diagnostic enrichment and rendering access.

Prefer one outer region per meaningful quantum. Nested helpers reuse the
current same-runtime region. Do not enter/exit for every pointer access.

Determine how `Mutator` authority travels through `EvalContext`, the core value
factory, reflection contexts, and net specialization without exposing it in
the public embedding API. The primary design passes a token explicitly at
internal boundaries; a checked thread-local convenience may reduce mechanical
plumbing but may not permit unguarded dereference.

Verify that:

- workers never hold a mutator region while sleeping for work;
- blocking on coordinator, store, host callback, or delivery activity does not
  indefinitely retain a region;
- semantic mutex guards do not escape the region which authorizes their value
  references;
- recursive evaluator and reflection entry does not count as another mutator;
  and
- collection requests can stop every worker at a bounded quantum boundary.

## Phase I4 — Core Trace Vocabulary and Leaf Policy

- Implement exact tracing for `core::Value`, keys which contain values, lists,
  dictionaries, argument arrays, evaluation failures, and context frames.
- Treat `Bytes`, numbers, atoms, static builtins, IDs, and similar data as
  leaves unless they actually contain managed edges.
- Trace RPDS/FingerTree contents logically. Document that duplicate traversal
  of shared spines is correct but may be expensive.
- Add trace-count instrumentation to quantify repeated logical traversal.
- Do not fork or replace persistent collections in this plan.

This phase establishes traversal definitions but does not enable collection
while recursive identity cells remain `Arc`-owned.

## Phase I5 — Lazies and Promises

Migrate the principal cyclic identities first:

- replace `Arc<LazyCell>` and `Arc<PromiseCell>` with managed identity cells;
- retain scheduler/completion sidecars as ordinary `Arc` only where they carry
  no recursive managed value ownership;
- trace lazy sources, terminal evaluated values, permanent failures, promise
  assignments, and producer-owned data;
- clear/release lazy sources after terminal publication as today;
- route source replacement and terminal assignment through the collector
  barrier API; and
- preserve exact wait, cancellation, abandonment, and resolver semantics.

Focused tests must construct and reclaim:

- a direct lazy self-cycle;
- two- and many-lazy cycles;
- promise-to-lazy and lazy-to-promise graphs;
- a resolved promise whose result contains the promise;
- a deferred producer retained by a worker; and
- a terminal value still reachable from another public root.

Use an isolated collector-ready fixture for forced reclamation. The complete
production runtime still does not collect.

## Phase I6 — Functions, Applications, Metadata, and Failures

- Migrate recursive function stages or wrappers, partial builtin arguments,
  lazy applications, fixpoint computations, reflection computations, and net-
  construction payloads as required by the selected object granularity.
- Migrate metadata identity so associated metadata can cycle without leaking.
- Ensure evaluation failures and context frames are traced without forcing
  their contained values.
- Preserve referential equality where current semantics rely on identity.
- Apply write barriers only to actually mutable fields; immutable argument
  arrays need tracing but no barrier.

Verify cycles through each family and confirm that tracing does not evaluate,
force, lock, or format a value.

## Phase I7 — Persistent Lists and Dictionaries

- Keep RPDS and FingerTree/`Arc` spines initially.
- Implement exact logical tracing of keys, values, list chunks, lazy list
  thunks, concatenation nodes, and shared slices.
- Verify a public persistent collection retains all contained managed objects
  across full collection.
- Verify dropping the final external collection permits a backedge cycle to be
  reclaimed.
- Measure duplicate trace work for heavily shared versions and record a
  threshold for revisiting collector-aware physical nodes.

Logical duplicate visits are a performance issue in a mark collector, not an
edge-counting soundness problem. This phase must not silently turn collection
updates into whole-map copies.

## Phase I8 — Interaction Nets

- Inventory every core value stored in net templates, agents, active pairs,
  stuck pairs, cursors, logical copies, and normalization state.
- Make `NetValue`, function stages, and `SharedRuntimeNet` expose those edges
  without reducing the net or materializing a cursor.
- Decide whether the shared runtime net remains an external synchronized
  allocation traced under a stopped mutator world or becomes a managed outer
  node. Do not rewrite generic topology merely for GC aesthetics.
- Require all net mutation which can publish a young value into an old net to
  use the barrier API.
- Preserve the Cursor-WHNF ownership and normalization-batch invariants.

Because collection waits for all mutators, acquiring a net lock while tracing
should be unnecessary. Treat a still-held net lock at collection as an
invariant defect rather than using a lossy `try_lock` trace.

Verify cycles through net `Data`, shared function stages, cursor
materialization, stuck nets, and values reachable only from pending active-pair
work.

## Phase I9 — Runtime-Owned Root Surfaces

Convert every long-lived Rust owner of a value to an explicit external root or
a traced managed edge, including:

- runtime canonical values and type-indexed compiler attachments;
- reflection environment and protected volume roots;
- store snapshots, views, journals, queries, and rewrites;
- coordinator client demands, sparks, tasks, deferred producers, waits, and
  failure ledgers;
- task handles and promise resolver state;
- diagnostic buses, ingress records, event inputs, output intents, and running
  deliveries; and
- assembler/module construction state.

Do not root values merely because a weak notification sidecar names their task
or wait ID. Root ownership must follow semantic retention, not scheduler
reachability by accident.

Verification drops each owning session/runtime facade in controlled orders and
checks both terminal observability and eventual reclamation.

## Phase I10 — Deferred Closures and Opaque Boundaries

`Arc<dyn Fn(&EvalContext) -> ...>` cannot be traced automatically.

- Enumerate every production deferred closure constructor.
- Prefer replacing captured value state with an explicit traceable computation
  object.
- Where a Rust closure remains valuable, attach an explicit external-root
  bundle for every captured value and verify the closure cannot smuggle a bare
  managed pointer.
- A runtime-created lazy/deferred computation which can participate in the
  managed graph must use traceable captured state. External-root bundles are a
  conservative escape hatch for externally owned computations, not the normal
  representation of evaluator fixpoints; a root back to the owning managed
  graph would intentionally leak and must be reported by the inventory.
- Audit compiler cached functions and task launchers under the same rule.

For `OpaqueValue`:

- ordinary arbitrary host payloads are tracing barriers;
- payloads may hold public rooted `Value`s, which are safe but can
  conservatively retain a cycle;
- Glam-owned opaque payload families are individually audited and may receive
  a private traceable representation instead; and
- no unsafe downcast or heap scan attempts to discover hidden pointers.

Gate G2 passes only after I0's inventory contains no unclassified closure,
opaque family, or value-bearing runtime record.

## Phase I11 — Whole-Graph Forced Full Collection

- Enable forced full collection only in explicit tests and a private runtime
  maintenance operation.
- Run it at every significant stable boundary: before/during/after worker
  activity, reflection quiescence, event delivery, logger supervision, module
  compilation, and settlement.
- Add a debug mode which collects at nearly every outer mutator exit.
- Prove that collection does not alter task readiness, diagnostic counts,
  observation epochs, transaction conflicts, net revisions, or assembly
  results.
- Exercise runtime drop both before and after collection.

Gate G3 requires the full test suite under ordinary execution plus the
aggressive debug collection mode, Miri for focused graphs, sanitizers, and an
unsafe/trace audit.

## Phase I12 — Explicit Runtime Maintenance and Threshold Collection

- Expose a narrow embedding maintenance method or runtime tuning policy; do not
  expose raw heap internals.
- Initially collect at controlled batch/idle boundaries.
- Add allocation-pressure requests which are serviced at outer mutator exits.
- Ensure a request cannot make a worker spin, hold settlement admission, or
  publish semantic activity merely because collection ran.
- Report metrics for debugging and profiling without making them observable to
  pure evaluation.

Automatic full collection is enabled only after controlled-boundary operation
is stable.

## Phase I13 — Generational Barrier Audit and Minor Collection

For every mutable graph edge in the I0 ledger:

- identify owner generation and publication operation;
- ensure the edge is dirtied before a minor collector can miss it;
- force publication versus collection ordering with deterministic barriers;
  and
- run differential full-only versus minor/full histories.

High-value surfaces include lazy result/source publication, promise
assignment, metadata updates, mutable net data, reflection store root changes,
and any managed task/deferred state.

Enable minor collection only after both collector Phase C9 and this audit pass
Gate G4.

## Phase I14 — Retire Redundant Ownership and Document the Boundary

- Remove `Arc` wrappers whose only remaining role was recursive value
  lifetime. Retain intentional `Arc`s for public roots, immutable leaf buffers,
  host identities, and scheduler notification sidecars.
- Remove duplicated runtime provenance fields when heap identity is
  authoritative and the boundary check remains equally cheap.
- Remove temporary collection-disable gates and migration-only adapters.
- Update `docs/architecture/evaluation.md`, `docs/AgentContext.md`, focused
  agent notes, and `src/README.md` with current ownership and safepoint rules.
- Mark the roadmap and both plans complete only after a final invariant and
  trace-edge audit.

## Integration Verification Matrix

Every managed-type phase runs focused tests before the standard repository
checks:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Additional required modes:

- collector disabled, to preserve a comparison baseline;
- forced full collection at selected stable points;
- aggressive full collection at outer mutator exits;
- minor/full differential histories after I13;
- zero workers and several workers;
- public roots moved among threads and dropped in forced orders;
- Miri for focused root, trace, lazy, promise, collection, and net graphs;
- Loom or deterministic barrier tests for mutator/root/barrier coordination;
- address/thread sanitizers where supported; and
- memory/drop counters proving both retention and reclamation.

Representative sample assemblies must produce identical outputs in all
collection modes. Timing and collection counts are profiling data, not Glam
semantics.

## Integration Completion Criteria

- Every production managed edge is exact or deliberately conservative.
- Public `Value` is a real runtime-local external root and remains convenient
  to clone and share.
- Workers access managed pointers only within bounded mutator regions.
- Fixpoint, promise, metadata, collection, function, and net cycles are
  reclaimed after their last root disappears.
- Reflection, diagnostics, stores, events, and task handles retain exactly the
  values their semantics require.
- Full and minor collection preserve assembly results and runtime coordination.
- No pointer-local GC locking or atomic reference count remains on internal
  managed edges.
- Remaining leaks through arbitrary opaque payloads are documented,
  conservative, and never risk premature collection.
