# Glam GC Integration Plan — 2026-08-19

Status: in progress; Phase I0 is complete, while managed ownership changes are
blocked on collector Gate G1.

This plan integrates the collector defined by
[`GarbageCollectorImplementation_2026-08-19.md`](GarbageCollectorImplementation_2026-08-19.md)
into the runtime, public value facade, evaluator, workers, reflection, and
interaction nets. Cross-plan invariants and enablement gates live in
[`GarbageCollectionRoadmap_2026-08-19.md`](GarbageCollectionRoadmap_2026-08-19.md).

## Phase Status

| Phase | Status | Outcome |
| --- | --- | --- |
| I0 | complete | complete ownership and mutation ledger |
| I1 | pending | runtime-owned heap, collection disabled |
| I2 | pending | public value and external-root prototype |
| I3 | pending | bounded evaluator/worker mutator regions |
| I4 | pending | core trace vocabulary and leaf policy |
| I4B | pending | closure and opaque managed-edge containment |
| I5 | pending | managed lazies and promises |
| I6 | pending | functions, applications, metadata, failures |
| I7 | pending | persistent list and dictionary tracing |
| I8 | pending | interaction-net tracing and mutation gateways |
| I9 | pending | runtime-owned root surfaces |
| I10 | pending | deferred closures and opaque boundaries |
| I11 | pending | whole-production-graph forced collection |
| I12 | pending | runtime maintenance and threshold collection |
| I13 | pending | redundant ownership removal and documentation |

## Current Boundary

The collector now supplies C4's checked direct root and weak registry, but it
does not yet mark, reclaim, finalize, or pass Gate G1. Integration may use that
completed boundary only in isolated prototypes; no production ownership
migration begins before G1.

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
    ├── optional Glam-owned inline scalar representation
    └── glam_gc::Root<core::Value>
```

Phase I2 decides whether `RuntimeValueRoot` is simply the collector's direct
managed root or a Glam-owned wrapper which keeps eligible scalars inline and
uses that same direct root for managed values. The collector does not accept an
alternate inline root cell or generalized registry payload merely to optimize
the public representation. A wrapper may retain a runtime ID for diagnostics
or API compatibility, but live heap identity is the authoritative provenance
for managed access.

This provisional shape is not the compact tagged-word transition. That work is
isolated in
[`ValueRepresentationRefinement_2026-08-19.md`](ValueRepresentationRefinement_2026-08-19.md)
and may begin only after the initial collector boundary is sound.

Every representation migrated in this plan must fit one slot in the
collector's fixed-size homogeneous typed-run geometry. The integration does
not request a large-object fallback, multi-run span, heterogeneous run, or
managed DST. Existing large/variable leaf storage remains audited external
ownership until a later representation plan chooses to decompose it.

## Phase I0 — Ownership and Mutation Ledger

Completed in
[`GarbageCollectorOwnershipLedger_2026-08-20.md`](GarbageCollectorOwnershipLedger_2026-08-20.md).
The ledger records the pre-GC graph, current layout baseline, synchronization
and mutation policy, semantic regression matrix, and the boundary defects that
must remain Gate G2 blockers. Its collector metadata/class geometry is
deliberately provisional until C2B and I1 exist.

Create a dated graph inventory beside this plan. For every graph-bearing type,
record:

- owner and source path;
- outgoing managed-value edges;
- whether it is immutable, replaceable, one-write, or freely mutable;
- current synchronization and lock order;
- whether it can live outside an evaluator call or worker quantum;
- whether it can cross threads;
- Rust `TypeId`, canonical object-metadata pointer, managed size/alignment, and
  selected heap-local allocation class, including its derived slot stride and
  slots-per-fixed-run geometry;
- visitor-based outgoing edge enumeration;
- required trace strategy;
- managed-edge mutation gateway, if the edge is replaceable;
- whether its homogeneous run metadata contains `Drop`;
- confirmation that it fits the collector's documented managed-layout limit;
  and
- migration phase.

The inventory must explicitly cover every `core::Value` variant, every type
containing a core or public `Value`, every `Arc<dyn Fn...>` which can capture a
value, every `OpaqueValue` payload family owned by Glam, and every interaction-
net specialization carrying core data.

For each opaque payload family, the inventory records either `no managed edge`
or the exact same-runtime public root wrapper it may retain. Discovering a bare
`Gc<T>`, unrooted recursive `core::Value`, foreign-runtime root, or equivalent
internal managed pointer is a boundary defect, not a conservative-tracing
classification.

I0 can begin before collector class discovery exists. At that point, record
Rust type/layout and projected trace/drop policy; mark metadata addresses,
dense class IDs, and derived fixed-run geometry as provisional. Reconcile and
latch those fields after C2B and I1, before Gate G2. This sequencing lets the
layout inventory inform C2A without pretending that heap-local classes already
exist.

Add tests which latch current semantics before changing representation:

- public value clone, equality, and cross-runtime rejection;
- fulfilled and unfulfilled lazy/promise behavior;
- lazy, promise, metadata, function, collection, and net cycles;
- value transfer between workers in one runtime;
- runtime drop with escaped public values, including their current supporting
  resource lifetime and access behavior;
- reflection store snapshots and task status after owner-session closure; and
- settlement and diagnostic/event retention.

This is an audit phase. Missing edges block later collection rather than being
classified optimistically.

## Phase I1 — Runtime Heap Ownership, Collection Disabled

- Add one `RuntimeHeap` to `EvaluationRuntime` ownership and the internal
  `RuntimeSharedResources` view.
- Keep the heap inside the runtime value domain. Only explicitly authorized
  runtime/value-domain owners retain it; escaped public roots do not. A root
  may still be cloned or dropped after value-domain teardown, but managed
  access through it is unavailable. I1 must identify which existing non-root
  capabilities are authorized owners instead of accidentally preserving the
  domain through every value-facing handle.
- Do not create a `heap -> runtime state -> heap` ownership cycle.
- Give the value factory a narrow allocation/rooting handle, not raw collector
  internals.
- Let runtime/value-factory construction discover and retain reusable
  `AllocationClass<T>` handles for common managed representations. Rare classes
  may use first-use discovery, but ordinary value allocation must not hash
  or otherwise look up `TypeId` on every object. Once a class is retained,
  allocation uses its dense ID and canonical metadata pointer directly.
- Add runtime-local tuning with collection disabled by default.
- Centralize Glam's node-size policy when constructing canonical object
  metadata. That policy may request a slot size larger than the Rust payload;
  allocation-class creation then applies it independently for each typed run.
  Type alignment remains expressed by the Rust node or a common aligned
  wrapper, not by runtime heap configuration. A shared managed-node wrapper or
  declaration macro is Glam's central alignment-policy point; the collector
  does not provide a mutable or per-heap alignment setting.
- Verify the earlier sibling allocation of runtime state and immutable profile
  remains internal to the runtime's ownership graph and acyclic once both can
  retain rooted values. Collector roots are not an ownership escape hatch for
  either sibling.

At this phase no core object is reclaimed by the new heap. Existing behavior
and tests must remain unchanged.

## Phase I2 — External Root and Public `Value` Prototype

- Prototype C4's direct managed root against scalars and one recursive test
  node. Separately prototype a Glam-owned public wrapper whose managed arm uses
  that root and whose optional inline arm contains only values which require no
  managed trace. Do not add another collector registry-entry or root-cell
  representation for the wrapper.
- Select between exposing the direct root and using the Glam wrapper based on
  public scalar construction cost and clarity. In either case, one public
  `Value` clone must preserve its root cell while the value domain lives, but
  must not preserve the heap after the domain's authorized owners are dropped.
- Specify and prototype how `RuntimeValueRoot` obtains authoritative managed
  provenance from the root's heap identity instead of an independently
  forgeable duplicated runtime ID. An optional runtime ID may remain only for
  diagnostics or compatibility. If the inline wrapper arm is selected, give
  the wrapper an equally non-forgeable association with the live value domain.
  Do not place the production `core::Value` in a collector root until I4
  supplies its exact trace; I2 uses collector-local fixtures or a
  non-collecting compatibility envelope.
- Ensure every safe root/public-value access validates or establishes the
  matching heap in release builds before invoking the collector's private
  unsafe `Gc<T>` gateway. Debug-only owner assertions are diagnostics for
  internal invariants, not the safety basis of the public API.
- Keep `api::Value` freely cloneable and `Send + Sync` when its contents are.
- Preserve the current public equality and debug semantics deliberately.
  Root-pointer identity is not a substitute for `core::Value` equality, and
  debug formatting must remain non-forcing. If these operations enter a
  mutator region internally, test their recursion and collection-request
  behavior explicitly.
- Prototype scoped core access under the correct mutator authority and record
  the production `as_core`/`into_core` call sites which I4 must migrate.
- Review public extractors which currently return references borrowed from a
  `Value` or `EvaluatedValue`. Managed borrows must be tied to a live matching
  mutator/access scope; owned extraction results may outlive that scope. Do not
  let a weak root manufacture a hidden heap lease merely to preserve an old
  borrowed-return signature.
- Preserve the public `EvaluatedValue` WHNF witness without making it a second
  root model.

Verification includes prototype-facade tests moving roots between threads,
dropping a facade while another authorized owner preserves access, dropping
the last authorized owner before the last value and observing an inert root,
rejecting roots from another heap, and nesting construction/evaluation entries
recursively. Existing public `Value` tests remain unchanged until the
production switch in I4.

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
current same-runtime region and heap-specific bitmap-range cursor cache. The
outer entry validates that cache once against the heap's allocation-lease
epoch. Do not enter/exit or touch shared allocation state for every pointer
access or ordinary allocation. A thread entering another `EvaluationRuntime`
activates another heap-qualified TLS entry; it does not replace or reuse the
first runtime's mutator or allocator cache.

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
- recursive entry does not create an independent class cache, while outer exit
  retains local bitmap-range cursors and makes its thread cache quiescent before
  the worker becomes eligible to sleep or service another runtime; TLS exit or
  eviction only forgets cursors, leaving full collection to recover ranges;
- one thread can nest work in two runtimes while retaining separate mutator
  tokens, recursive depths, epochs, and caches, and cannot construct a managed
  edge between them;
- opposite A-then-B and B-then-A nesting does not deadlock when collection is
  requested on both heaps, because an uncommitted request does not block
  entry, while entry waits safely if the target collector is already exclusive;
- allocation payload and allocation-bit initialization completes before the
  managed pointer is returned, rather than at the outer-region boundary; and
- collection requests can stop every worker at a bounded quantum boundary.

## Phase I4 — Core Trace Vocabulary and Leaf Policy

- Implement exact tracing for every then-current `core::Value` variant and all
  transitively owned structures which may contain a managed edge, including
  keys, lists, dictionaries, argument arrays, function/lazy/promise/net
  payloads, evaluation failures, and context frames. A recursive subgraph may
  remain `Arc`-owned, but its trace adapter must still visit any managed values
  it contains; collection being disabled does not permit an incomplete unsafe
  `Trace` implementation.
- Implement those traces as representation-aware edge visitors. Do not encode
  fixed field-offset tables: list nodes, immediate-like shells, and persistent
  collection adapters report only the managed edges they actually contain.
- Treat `Bytes`, numbers, atoms, static builtins, IDs, and similar data as
  leaves unless they actually contain managed edges.
- Trace RPDS/FingerTree contents logically. Document that duplicate traversal
  of shared spines is correct but may be expensive.
- Add trace-count instrumentation to quantify repeated logical traversal.
- Do not fork or replace persistent collections in this plan.
- Once the current production value graph has an exact trace and I3's region
  boundaries are in place, enact I2's selected `RuntimeValueRoot`
  representation and heap-derived runtime provenance. Keep collection disabled.
- Replace direct `as_core` escape paths with scoped access under the correct
  mutator authority. Eliminate ownership-taking `into_core` paths which could
  let an unrooted managed pointer escape its region; internal consumers borrow
  or clone the core shell inside an enclosing mutator scope.
- Repeat I2's root movement, value-domain-owner drop, foreign-runtime
  rejection, equality, debug, borrowed-access, and scalar-cost tests against
  the real public `Value`. If an authorized non-root owner remains after the
  `EvaluationRuntime` facade drops, access may continue through that owner;
  once the value domain itself is gone, escaped roots remain inert.

This phase establishes traversal definitions but does not enable collection
while recursive identity cells remain `Arc`-owned. From I4 onward, every
representation change in I5–I10 updates its exact visitor or root classification
in the same checkpoint; later collection gates are not permission to carry a
knowingly incomplete unsafe `Trace` implementation.

## Phase I4B — Closure and Opaque Managed-Edge Containment

Close non-traceable storage before the first recursive identity becomes a bare
managed pointer:

- use I0's constructor inventory to find every deferred Rust closure and opaque
  payload which can retain an internal `core::Value`;
- replace Glam-owned closure captures with explicit traceable computation state
  where practical;
- otherwise attach an explicit bundle of same-runtime public roots, accepting
  that a backedge through such a bundle is conservative retention rather than
  exact cycle collection;
- narrow opaque construction so an arbitrary payload may contain no managed
  edge, while audited families may contain same-runtime public roots but never
  a bare `Gc`, unrooted recursive `core::Value`, or foreign-runtime root; and
- add construction-time and compile-time tests proving each admitted closure
  and opaque family cannot smuggle a bare managed pointer after I5 changes the
  representation of captured values.

This checkpoint need not move opaque storage into collector-owned finalizable
cells. It establishes edge safety; I10 later completes ownership, scoped
downcast, destruction, and conservative-retention policy. I5 is blocked until
I4B passes.

## Phase I5 — Lazies and Promises

Migrate the principal cyclic identities first:

- replace `Arc<LazyCell>` and `Arc<PromiseCell>` with managed identity cells;
- retain scheduler/completion sidecars as ordinary `Arc` only where they carry
  no recursive managed value ownership;
- trace lazy sources, terminal evaluated values, permanent failures, promise
  assignments, and producer-owned data;
- clear/release lazy sources after terminal publication as today;
- route source replacement and terminal assignment through the managed-edge
  mutation gateway; its collector action is empty in full stop-the-world mode
  while preserving an auditable site for separately planned collectors;
- discharge the raw gateway's unsafe same-heap, current-old-edge, and exact-
  replacement obligations inside representation-local safe methods rather
  than exposing raw `Gc` mutation to evaluator callers;
- update the already-exact I4 visitors in the same checkpoint as each edge
  changes from external/`Arc` ownership to `Gc`; no phase may leave a trace
  placeholder merely because collection is disabled; and
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
- Route only actually mutable managed edges through the mutation gateway;
  immutable argument arrays need tracing but no gateway.

Verify cycles through each family and confirm that tracing does not evaluate,
force, lock, or format a value.

## Phase I7 — Persistent List and Dictionary Trace Audit

- Keep RPDS and FingerTree/`Arc` spines initially.
- Audit and extend I4's exact logical tracing of keys, values, list chunks, lazy
  list thunks, concatenation nodes, and shared slices against the concrete
  representation inventory. A missing node is a soundness defect, not work
  intentionally deferred from I4.
- Verify a public persistent collection retains all contained managed objects
  across full collection.
- Verify dropping the final external collection permits a backedge cycle to be
  reclaimed.
- Measure duplicate trace work for heavily shared versions and record a
  threshold for revisiting collector-aware physical nodes.

Logical duplicate visits are a performance issue in a mark collector, not an
edge-counting soundness problem. This phase must not silently turn collection
updates into whole-map copies.

## Phase I8 — Interaction-Net Migration and Trace Audit

- Reconcile I0/I4's inventory of every core value stored in net templates,
  agents, active pairs, stuck pairs, cursors, logical copies, and normalization
  state against the concrete migration.
- Make `NetValue`, function stages, and `SharedRuntimeNet` expose those edges
  without reducing the net or materializing a cursor.
- Decide whether the shared runtime net remains an external synchronized
  allocation traced under a stopped mutator world or becomes a managed outer
  node. Do not rewrite generic topology merely for GC aesthetics.
- Require all net mutation which can replace a managed value edge to use the
  mutation gateway. It is a no-op for the full collector; future moving or
  concurrent collectors may extend it under their own plans.
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

- Reconcile every production deferred closure constructor with I4B's
  containment ledger and close any remaining conservative escape hatch which
  can be represented exactly.
- Prefer replacing captured value state with an explicit traceable computation
  object.
- Where a Rust closure remains valuable, retain I4B's explicit external-root
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
- payloads may hold public/runtime-rooted `Value`s from the same runtime, which
  are safe but can conservatively retain a cycle;
- payloads must never hold bare `Gc<T>`, an unrooted recursive `core::Value`,
  a foreign-runtime root, or any equivalent managed pointer which could escape
  its mutator region;
- preserve that rule structurally: keep opaque construction private, do not
  re-export collector pointers, and require an audited sealed/unsafe marker or
  an external sidecar for each allowed payload family rather than retaining
  the current unconstrained `Any + Send + Sync` constructor;
- generic embedding payloads should remain in host-owned side tables whose
  opaque Glam token contains only identity/provenance. Same-runtime Glam values
  in the host payload use public roots; cross-runtime associations remain host
  state outside either runtime's value graph;
- unreachable opaque payloads still receive ordinary Rust destruction. A
  client-defined `Drop` may release host resources or otherwise be visible to
  the embedding client, but its timing and order are outside Glam evaluation
  semantics. It runs in the collector's `Finalizing` phase with a mutator
  available, outside collector locks. It may allocate, evaluate or schedule
  work, emit diagnostics, and publish a fresh equivalent of itself, but it
  cannot root or otherwise resurrect the completed dead allocation;
- place every collector-owned opaque payload in a typed run whose metadata has
  one erased `Drop` operation. All such destructors run with the finalizer
  mutator installed; the collector does not distinguish passive from
  mutator-capable Rust destruction. An explicitly external sidecar remains
  outside this guarantee and must document that fact;
- replace or narrow the current
  `OpaqueValue::downcast<T>() -> Option<Arc<T>>` boundary for
  mutator-finalized payloads. Cloning the `Arc` can move the last `Drop` outside
  collector finalization. Prefer a collector-owned payload with a scoped
  downcast/borrow valid only in a mutator region, or an explicitly classified
  sidecar whose destruction does not require the finalization guarantee;
- ensure a finalizable opaque payload cannot obtain a managed pointer or root
  to its containing allocation. Public roots to other values remain valid and
  keep those values alive independently. Constructing a fresh equivalent
  during `Drop` produces a new allocation identity rather than resurrection;
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
- Add a debug mode which requests collection before nearly every outer mutator
  entry.
- Prove that collection does not alter task readiness, diagnostic counts,
  observation epochs, transaction conflicts, net revisions, or assembly
  results.
- Force opaque finalization while logger supervision and workers are active.
  Diagnostics and tasks produced by `Drop` must be pumped normally before the
  runtime reports quiescence, and a fresh value published by a quining
  destructor must survive independently of the reclaimed identity.
- Request another collection from finalizer-driven work and prove it is
  coalesced into the collection being finalized rather than deadlocking,
  recursively collecting, or forcing an immediate second pass.
- Force a finalizer to wait for work on another runtime worker while a
  collection is requested concurrently, and prove the heuristic request does
  not prevent that worker from entering during finalization.
- Exercise runtime drop both before and after collection.

Gate G3 requires the full test suite under ordinary execution plus the
aggressive debug collection mode, Miri for focused graphs, sanitizers, and an
unsafe/trace audit.

## Phase I12 — Explicit Runtime Maintenance and Threshold Collection

- Expose a narrow embedding maintenance method or runtime tuning policy; do not
  expose raw heap internals.
- Preserve the collector crate's two-level control surface: a nonblocking,
  coalescing request which may be issued before a known batch boundary, and a
  synchronous full-collection operation used only outside an active mutator.
  These are Rust runtime-maintenance controls, not Glam evaluation effects.
- Initially collect at controlled batch/idle boundaries.
- Add allocation-pressure requests from successful typed-run publication which
  are serviced when a later outer mutator entry finds the heap idle. Lease-word
  claims and individual slot allocations remain outside shared pressure
  accounting.
- Count queued and running finalizers as runtime operational activity. A
  readiness probe must pump consequences of finalizer diagnostics, event
  output, and newly launched tasks before returning a stable report.
- Do not begin a requested collection while the heap is in `Finalizing`.
  Requests made before successful completion are heuristic hints coalesced into
  the active collection and are cleared with its pressure baseline; they do not
  queue a second writer or deny fresh mutator admission. A request serialized
  after completion remains latched for a later idle outer entry.
- Ensure a request cannot make a worker spin, hold settlement admission, or
  publish semantic activity merely because collection ran.
- Report metrics for debugging and profiling without making them observable to
  pure evaluation.

Automatic full collection is enabled only after controlled-boundary operation
is stable.

## Phase I13 — Retire Redundant Ownership and Document the Boundary

- Remove `Arc` wrappers whose only remaining role was recursive value
  lifetime. Retain intentional `Arc`s for public roots, immutable leaf buffers,
  host identities, and scheduler notification sidecars.
- Remove duplicated runtime provenance fields when heap identity is
  authoritative and the boundary check remains equally cheap.
- Remove temporary collection-disable gates and migration-only adapters.
- Update `docs/architecture/evaluation.md`, `docs/AgentContext.md`, focused
  agent notes, and `src/README.md` with current ownership and safepoint rules.
- Pass roadmap Gate G4 and mark the roadmap and both plans complete only after
  a final invariant and trace-edge audit.

## Integration Verification Matrix

Every managed-type phase runs focused tests before the standard repository
checks:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
cargo test --workspace -q
```

The ordinary commands preserve the repository's established root-package
contract. The workspace run (plus the current `glam-gc` verification script)
ensures the default-member configuration does not accidentally omit collector
tests.

Additional required modes:

- collector disabled, to preserve a comparison baseline;
- forced full collection at selected stable points;
- aggressive full-collection requests before outer mutator entries;
- zero workers and several workers;
- public roots moved among threads and dropped in forced orders;
- every production allocation class fitting the collector's fixed-run slot
  geometry, plus explicit rejection tests for a deliberately oversized fixture;
- Miri for focused root, trace, lazy, promise, collection, and net graphs;
- Loom or deterministic forced-order tests for mutator/root coordination;
- address/thread sanitizers where supported; and
- memory/drop counters proving both retention and reclamation.

Representative sample assemblies must produce identical outputs in all
collection modes. Timing and collection counts are profiling data, not Glam
semantics.

## Integration Completion Criteria

- Every production managed edge is exact or deliberately conservative.
- Every opaque payload is audited to contain no managed edge or only ordinary
  runtime/public roots; no bare collector pointer crosses that boundary.
- Public `Value` is a real runtime-local external root and remains convenient
  to clone and share.
- Workers access managed pointers only within bounded mutator regions.
- Every managed representation fits one slot in the documented fixed-size
  typed-run geometry; there is no hidden large-object or multi-run fallback.
- Fixpoint, promise, metadata, collection, function, and net cycles are
  reclaimed after their last root disappears.
- Reflection, diagnostics, stores, events, and task handles retain exactly the
  values their semantics require.
- Full collection preserves assembly results and runtime coordination.
- No pointer-local GC locking or atomic reference count remains on internal
  managed edges.
- Remaining leaks through arbitrary opaque payloads are documented,
  conservative, and never risk premature collection.
