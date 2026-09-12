# Garbage Collector Persistent Edge Trait Migration Plan — 2026-09-12

Status: P0 complete; P1-P5 planned. This is the nested implementation plan
for the managed-edge part of GCI11R-002D.2a-D.2b in
[`GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md`](GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md).
It must coordinate with D.2c-D.2g before its final trait-removal cutover. It is
not an independent prerequisite which may silently enlarge or bypass those
raw-value migration checkpoints.

## Purpose

Make every persistent managed-edge duplication and identity observation an
explicit mutator-qualified operation.

The current `Gc<T>` is a pointer-sized, non-rooting handle which implements
`Copy`, `Clone`, `PartialEq`, `Eq`, and `Debug`. Those traits make it easy to
copy one managed pointer into arbitrary Rust state without recording whether
the new location is:

- a traced persistent edge;
- a temporary working value protected by the current mutator;
- a registered-root projection;
- collector-private address bookkeeping; or
- an accidental unrooted escape.

This did not make pointer copying or comparison intrinsically unsafe for the
initial non-moving stop-the-world collector. It does, however, conceal the
ownership handoff which GCI11R-002D must make explicit, and it gives future
moving collection an accidental permanent-address surface.

The target of this plan is a move-only persistent `Gc<T>` with explicit
mutator-qualified duplication and identity comparison. Cheap collector-private
address copying remains available through `ErasedGc`. Registered roots and
public rooted Glam values remain ordinarily clonable because they copy a
durable liveness cell, not an unrooted graph edge.

## Relationship to Other Plans

- [Aggressive GC verification remediation](GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md)
  owns the complete raw `core::Value` access migration. This plan supplies its
  managed-edge foundation and final trait cutover.
- [Scoped pointer safety](GarbageCollectorScopedPointerSafety_2026-09-09.md)
  remains a later lifetime-branding experiment. This plan deliberately does
  not introduce `ScopedGc<'mutator, T>`.
- [Value representation refinement](ValueRepresentationRefinement_2026-08-19.md)
  may later replace today's compatibility carrier and persistent containers.
  This plan establishes an ownership operation, not a representation layout,
  so that later transition should preserve the boundary rather than repeat the
  trait-policy decision.
- [Concurrent garbage collection](ConcurrentGarbageCollection_2026-08-28.md)
  may strengthen explicit persistence into a destination-aware mutation
  gateway. Mutator qualification here does not itself implement SATB,
  relocation, or a concurrent write barrier.
- [Collector integration](GarbageCollectorIntegration_2026-08-19.md) Gate G3
  remains closed until both this plan and the parent D.2 closure prove that no
  raw value or edge crosses a regional boundary without an exact owner.

## Selected Contract

### Persistent managed edges

At completion, `Gc<T>` remains pointer-sized, non-rooting, and movable, but it
implements none of:

```text
Copy Clone PartialEq Eq Debug Hash
```

Moving a `Gc<T>` transfers that particular Rust handle. It does not change
managed reachability. Constructing another persistent handle requires an
explicit operation resembling:

```rust
edge.duplicate_in(mutator) -> Gc<T>
edge.same_allocation_in(other, mutator) -> bool
```

The final spelling may live on `Mutator` if the additive prototype shows that
authority-first call sites are clearer. The semantic requirements do not
depend on method placement:

- both operations require one currently admitted matching heap;
- debug/test builds validate both supplied edges against that heap;
- optimized non-moving builds reduce to pointer copy or pointer comparison;
- duplication neither roots the allocation nor registers another root; and
- the duplicate must be installed below a traced owner or registered as a
  root before its independent liveness proof ends.

The mutator proves that the source is currently usable. It does not prove that
the returned unbranded `Gc<T>` is installed correctly. The source inventory,
Glam's `RuntimeValueAccess` boundary, and exact trace/root ownership remain the
near-term enforcement for that handoff.

### Tracing and collector-private identities

`Trace` borrows persistent edges:

```rust
visitor.visit(&self.child);
```

The visitor may immediately copy the address into collector-private
`ErasedGc`. `ErasedGc` remains `Copy`, `Clone`, `Eq`, `Hash`, and `Debug`
because it is an internal worklist/metadata identity used only while the
collector owns the relevant liveness and phase proof. This exception must not
be re-exported as an ordinary managed-value handle.

Mutation gateways similarly borrow owner, leaving, and entering edges rather
than consuming or implicitly copying them. Future policies may copy erased
identities into attempt-local barrier worklists.

### Roots and public values

`Mutator::root` continues to consume one persistent `Gc<T>`. A caller that
also needs an edge may project it from the resulting root under the same
mutator, or explicitly duplicate the edge before transferring the original.

`Root<T>::Clone` remains unqualified. It clones the registered `RootCell`,
preserving an existing durable liveness claim without reading the allocation
or registering another collector root. `Root::as_gc` remains mutator-qualified
and produces one current non-rooting edge from that durable cell.

`RuntimeValueRoot`, public `api::Value`, and `EvaluatedValue` likewise remain
clonable: managed values share the same registered root cell, while inline
immediates copy their self-contained representation. They must not recover
`PartialEq`, `Eq`, `Ord`, or `Hash`; representation observation remains an
explicit runtime/reflection operation.

### Glam managed-edge facades

`ManagedLazyEdge`, `ManagedPromiseEdge`, and `ManagedCoreNetEdge` become
move-only persistent edges. They do not reintroduce the removed traits through
manual pointer copying. Their working operations receive `RuntimeValueAccess`
or `EvaluationValueAccess` and expose named access-qualified duplication or
identity methods only where a real caller requires them.

IDs such as `LazyId`, `PromiseId`, task IDs, and interaction-net node IDs may
remain freely comparable. A scheduler should compare such stable identities
rather than managed addresses when the address itself is not the intended
contract.

### Raw Glam values

This plan does not independently migrate all raw `core::Value` operations.
The parent D.2 plan removes unqualified `Clone`, `PartialEq`, `Eq`, and
recursive `Debug` from that carrier and replaces them with regional operations
such as:

```rust
access.clone_value(&value)
access.same_representation(&left, &right)
access.format_value(&value, policy)
```

The exact operations should stay as narrow as their callers. Glam semantic
equality remains evaluator logic; reflection representation comparison remains
an explicit privileged capability; dictionary-key equality remains `Key`.

## Non-Goals

- Implementing a lifetime-branded `ScopedGc` or `ScopedValue`.
- Proving through Rust's type system that every duplicated edge reaches a
  traced destination before mutator exit.
- Implementing moving, concurrent, generational, or incremental collection.
- Adding stable address identity, hashing, serialization, or IPC for managed
  allocations.
- Removing equality from scalar data, `Key`, stable IDs, state enums, or other
  types whose relation is total and unambiguous.
- Removing `Clone` from registered roots or opaque public rooted handles.
- Treating a repeatedly passing concurrent test as ordering evidence.

## Semantic and Safety Invariants

1. No ordinary trait can duplicate or compare a persistent `Gc<T>`.
2. Every persistent-edge duplicate is created under one matching mutator or a
   stronger destination-aware mutation gateway.
3. Every persistent duplicate is installed beneath an exact traced owner or
   registered root before its independent liveness proof ends.
4. Tracing borrows stored edges and may copy only erased collector-private
   identities into collector work state.
5. Registered-root cloning shares one root cell and creates no additional root
   registration.
6. Root projection and managed-edge identity observation require a matching
   mutator/value-access region.
7. A mutator, `RuntimeValueAccess`, or managed edge never crosses a wait,
   callback, scheduler handoff, reflection gate, or safepoint.
8. Managed `Drop` continues to treat its stored `Gc<T>` fields as spoiled once
   destruction begins; move-only handles do not authorize finalizer access.
9. Release-mode duplication and identity comparison add no allocation,
   locking, reference counting, or root registration.
10. The transition does not claim moving-GC readiness: persistent stored slots
    are not yet mutable relocation records, and unbranded duplicates can still
    be mishandled after their mutator closes.

## Implementation Strategy

The old traits cannot be removed at the beginning. `LazyValue`,
`PromisedValue`, `Value`, persistent lists and dictionaries, function/net
shells, failures, and several machine records currently derive or call
`Clone` transitively through managed edges. Removing `Gc<T>: Clone` first
would turn the whole repository into one unreviewable compiler-error batch.

The transition is therefore additive before it is subtractive:

1. inventory every implicit use;
2. add borrowed tracing and explicit edge operations;
3. migrate direct collector and Glam edge consumers while the old traits still
   exist temporarily;
4. let parent D.2b-D.2g remove transitive raw-value trait dependencies; and
5. remove the traits in one small, compiler-enforced cutover.

Temporary availability is not permission for new uses. A source-backed latch
must freeze the old surface and count down its remaining occurrences.

## Phase P0 — Baseline, Inventory, and Decision Latches

### P0A — Exact occurrence inventory

Build a source-backed manifest covering:

- every production and test `Gc<T>` field;
- every `Copy`, `Clone`, `PartialEq`, `Eq`, or `Debug` implementation or derive
  whose behavior reaches `Gc<T>` transitively;
- every implicit move-after-use which presently compiles only because `Gc<T>`
  is `Copy`;
- every `visitor.visit(edge)`, `edge.erase()`, and `edge.ptr_eq(other)` call;
- every root creation and root projection;
- every mutation-gateway owner, leaving edge, and entering edge;
- every worklist, vector, channel, closure capture, or test fixture carrying a
  typed managed edge; and
- every collector-private `ErasedGc` use which is intentionally exempt.

Classify each occurrence as fresh unpublished edge, traced persistent edge,
registered-root projection, mutator-local working duplicate, mutation input,
collector-private erased identity, or defect.

Verification: an exact count and fingerprint fail on unclassified additions;
the manifest distinguishes production from tests and ordinary `Gc<T>` from
the `ErasedGc` exception.

Completed 2026-09-12. The syntax-backed
`persistent_edge_trait_inventory` initially recorded 568 classified occurrences:
146 production typed-edge occurrences, 35 production erased-identity
occurrences, 373 test typed-edge occurrences, and 14 test erased-identity
occurrences. The manifest covers direct stored fields, explicitly typed
carriers, fresh allocation and projection surfaces, trace/erase/identity
calls, root and mutation boundaries, traits on the direct and same-source
semantic carrier closure, and currently visible implicit-copy expressions.
It scans selected operations inside macro token bodies because `syn` does not
otherwise descend into `assert!` and similar invocations. The exact count,
normalized occurrence fingerprint, partition counts, disposition closure,
and collector-private source boundary are executable drift latches. The P4
compiler-enforced trait cutover remains the exhaustive backstop for implicit
move-after-use cases which cannot be inferred reliably from untyped Rust
syntax; cross-module raw-value dependencies remain jointly owned by the
parent D.2 occurrence and durable-owner inventories rather than guessed from
ambiguous unqualified type names.

P0B's deterministic baseline fixture intentionally raised the current
manifest to 573 occurrences, all five additions being test-only typed-edge
evidence. The current partition is therefore 146 production typed, 35
production erased, 378 test typed, and 14 test erased occurrences; subsequent
checkpoint notes update this countdown whenever their reviewed source changes
it.

### P0B — Baseline behavior and cost

Record before-transition evidence for:

- `size_of::<Gc<T>>() == size_of::<*const T>()`;
- allocation, root projection, trace, mutation, and cycle-reclamation tests;
- registered-root counts before and after root-handle cloning;
- a release-mode pointer-copy/identity microbenchmark or assembly inspection;
  and
- representative evaluator and compiler throughput sufficient to catch an
  accidental root or lock on every edge duplication.

This is a comparison baseline, not a new performance gate. Do not optimize
unmeasured evaluator structure during the migration.

Completed 2026-09-12. The comparison baseline is deliberately a mix of
compile-time layout assertions, deterministic operation counters, focused
behavior tests, and informational command timings rather than a wall-clock
pass/fail threshold:

- `pointer.rs` statically proves `Gc<u64>` has one pointer's size;
- `pointer_copy_and_identity_register_no_roots` proves today's pointer copy
  and identity operations do not register roots;
- `root_registry_publishes_once_per_cell_and_not_per_clone` and
  `root_projection_preserves_identity_without_registering_another_root` prove
  root-handle cloning and projection add no root registry entry;
- `manual_struct_and_recursive_enum_traces_match_expected_edges`,
  `visitor_panic_leaves_the_value_traceable_from_the_beginning`,
  `successful_collection_report_counts_roots_traces_and_distinct_marks`,
  `checked_nonrecursive_marking_handles_cycles_diamonds_and_duplicate_edges`,
  and `c5d_random_graph_marks_match_an_independent_reachability_oracle` cover
  exact tracing, retry, repeated edges, cycles, and reclamation;
- `replacement_gateway_executes_the_reported_edge_update_once`,
  `synthetic_observer_selects_distinct_empty_singleton_and_multi_edge_sets`,
  and the deterministic-hook transition tests cover mutation; and
- the release comparison command is
  `cargo test --release -q -p glam-gc pointer_copy_and_identity_register_no_roots`.
  Its source/codegen boundary is latched by the absence of root construction,
  locking, allocation, or reference-count operations in `Gc::ptr_eq` and its
  eventual explicit replacements. P5B will compare the final implementation
  at the same boundary.

Representative whole-front-end and evaluator smoke baselines use
`cargo test -q --test sample_sources` and
`cargo test -q --test hello_assemblies`. Their elapsed times are recorded by
the executing environment when useful, but are not committed as portable
performance promises. The deterministic root-registration latch is the
primary regression detector for the costly failure mode this migration could
accidentally introduce.

### P0C — Contract freeze

Update collector and Glam ownership documentation with the selected contract
above. Add compile-time/source assertions for the traits which must eventually
be absent, initially marked as pending so they cannot be mistaken for a passed
cutover.

Exit: every occurrence is classified, the exempt erased identity is exact,
and later phases can reduce a known manifest rather than discover the scope
through compiler errors.

Completed 2026-09-12. `glam-gc/SAFETY.md` now distinguishes the selected
move-only persistent-edge contract from the five standard traits which remain
temporarily implemented. It records exact traced/root ownership, the private
`ErasedGc` exception, and the required release cost. Glam's evaluation
architecture records the same boundary for its managed facades. The
`persistent_edge_standard_trait_cutover_is_explicitly_pending` source latch
names each forbidden final trait and proves it remains one deliberate P4
obligation; P4 replaces that positive pending latch with compile-time negative
contracts. P0 closes with the 573-occurrence manifest recorded by P0B.

## Phase P1 — Additive Collector API

### P1A — Borrowed trace reporting

- Change `Visitor::visit` to borrow `&Gc<T>`.
- Change `Gc<T>::erase` and collector-private conversion helpers to borrow
  rather than consume an implicitly copied edge.
- Update `Trace for Gc<T>`, options, tuples, arrays, graph fixtures, and paged
  worklist helpers without removing the old standard traits yet.
- Verify trace panic/retry, repeated-edge reporting, cycles, roots, and deep
  non-recursive worklists retain the same completed mark bitmap.

### P1B — Explicit duplication and identity

- Add mutator-qualified persistent-edge duplication and same-allocation
  comparison.
- Validate heap ownership and canonical type metadata in debug/test builds.
- Preserve one-pointer layout and zero release-mode bookkeeping.
- Add focused tests for same allocation, distinct allocation with equal Rust
  payload, wrong-heap debug rejection, duplication followed by rooting, and
  duplication followed by installation under a traced owner.

### P1C — Borrowed rooting and mutation surfaces

- Review whether root construction should consume the edge or borrow it. Keep
  the selected default of consuming it unless real call sites demonstrate
  unavoidable duplicate/project churn.
- Change mutation gateways to borrow owner and optional leaving/entering edges.
- Preserve the exact transition visitors and deterministic observation probes.
- Prove root projection creates no registration, root cloning creates no
  registration, and a mutation transition reports each selected side exactly
  once under forced orderings.

Exit: every required operation has an explicit API and can be adopted without
yet breaking downstream trait-derived code.

## Phase P2 — Direct Consumer Migration

### P2A — `glam-gc` internals and fixtures

Migrate collector implementation and test code to borrowed tracing, explicit
duplication, explicit identity, and move-aware ownership. Internal worklists
may continue copying `ErasedGc`; no ordinary typed edge may use that exception.

Run focused collector tests after each family: roots, mutation, graph tracing,
finalization, thread caches, deterministic hooks, and Miri fixtures.

### P2B — Glam managed identity families

Add equivalent narrow gateways to `RuntimeValueAccess` and migrate:

- `ManagedLazyEdge` and `ManagedLazyAccess`;
- `ManagedPromiseEdge` and `ManagedPromiseAccess`;
- `ManagedCoreNetEdge` and `ManagedCoreNetAccess`;
- their registered-root holders and first-owner construction paths; and
- exact trace, cache, promise-assignment, and net-mutation transitions.

The edge facades may temporarily retain trait implementations only when a
parent D.2 carrier still requires them. Every implementation is recorded as a
counted transitional occurrence, never an allowlisted final surface.

### P2C — Net, mutation, and worker-local consumers

Migrate core-net normalization, cursor/frontier work, edge transition owners,
and any worker-local managed pointer worklists. Prefer stable semantic IDs for
scheduler comparisons. A raw managed edge must remain inside its value-access
quantum and cannot become scheduler state.

### P2D — Additive closure review

Audit the remaining manifest. Every surviving implicit trait use must be a
transitive dependency assigned to one parent D.2b-D.2g occurrence. Any direct
collector or managed-edge use left over reopens P2A-P2C.

Exit: collector and direct managed-edge code no longer needs `Gc<T>` standard
traits; only inventoried compatibility carriers prevent final removal.

## Phase P3 — Parent Raw-Value Interlock

This phase is completed through the parent remediation rather than duplicated
here:

- D.2b removes raw carrier and persistent-container trait dependencies;
- D.2c removes evaluator and builtin dependencies;
- D.2d removes orchestration dependencies;
- D.2e removes front-end/compiler dependencies;
- D.2f removes reflection machine/store dependencies; and
- D.2g removes public API, compiler, and diagnostic dependencies.

Each parent checkpoint updates this plan's trait-dependency manifest. An
access-qualified `core::Value` clone may internally duplicate a managed edge
only through the P1/P2 gateway while the matching access remains live. A
durable public/root clone instead shares the registered root cell.

Exit: no production `Clone`, equality, or formatting implementation requires
an implicit `Gc<T>` copy or comparison, and no test-only dependency blocks the
small final cutover.

## Phase P4 — Trait Removal Cutover

### P4A — Remove implicit duplication

- Remove `Copy` and `Clone` from `Gc<T>`.
- Remove corresponding implicit traits from all managed-edge facades.
- Repair any newly exposed move-after-use through explicit ownership transfer,
  root projection, or mutator-qualified duplication; never by reconstructing
  a pointer or adding an unqualified helper.

### P4B — Remove implicit observation

- Remove `PartialEq`, `Eq`, and `Debug` from `Gc<T>`.
- Remove unqualified `ptr_eq` and replace every real identity operation with
  the P1 access-qualified form.
- Remove corresponding managed-edge/value-shell equality and formatting
  traits selected by parent D.2a.
- Retain only opaque root/public-handle formatting and explicit diagnostic or
  reflection operations.

### P4C — Negative trait and source gates

Add compile-time negative contracts proving `Gc<T>` and the three Glam managed
edge facades implement none of the forbidden traits. Close the source manifest
at zero transitional occurrences. Ensure `ErasedGc` remains crate-private and
is the sole copyable managed-address identity.

Exit: the workspace compiles only through explicit edge ownership operations;
no compatibility trait remains to conceal a missed occurrence.

## Phase P5 — Verification and Closure Review

### P5A — Focused dynamic verification

Run:

- ordinary and aggressive collector graph/root/mutation suites;
- lazy, promise, fixpoint, metadata, function, list/dict, and core-net cycle
  reclamation tests;
- worker, reflection, macro/compiler, diagnostic, and runtime-retirement
  integration tests;
- focused Miri tests for allocation, duplication, root projection, tracing,
  mutation, and destruction; and
- Loom only for synchronization changed by this transition. The trait cutover
  alone is not a reason to invent a concurrency model test.

Every disputed ordering uses barriers, channels, deterministic hooks, or a
model checker. Repetition is stress evidence only.

### P5B — Cost and layout verification

Recheck one-pointer `Gc<T>` layout, registered-root counts, allocation counts,
and release-mode code generation or microbenchmarks. Investigate any new lock,
root registration, allocation, or reference-count operation on an ordinary
edge duplicate before closure.

### P5C — Parent reconciliation and review

Reconcile this manifest with D.2h's raw-value API, durable-owner,
managed-edge, machine-state, capture, and external-owner inventories. Publish
a dated review which accounts for intentional drift, all unsafe/API changes,
remaining deferred lifetime branding, and moving-GC limitations.

Run the repository routine checks:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Also run the parent plan's complete workspace aggressive-verification command.
P5 completion contributes to Gate G3 but cannot pass it without D.2h and the
remaining GCI11R remediation.

## Verification Matrix

| Contract | Static evidence | Dynamic evidence |
| --- | --- | --- |
| `Gc<T>` is move-only | negative trait checks; zero source inventory | move/duplicate/root fixtures |
| duplication is mutator-qualified | exact API/call-site manifest | same/wrong-heap and aggressive collection tests |
| identity is explicit | no `PartialEq`/`Eq`; no raw `ptr_eq` | same/distinct allocation tests |
| tracing does not consume edges | borrowed visitor signature | cycles, repeated edges, panic/retry, deep graph tests |
| root clones stay cheap | root/public trait and layout checks | root-registration counter remains unchanged |
| no edge crosses orchestration | raw API and owner inventories | forced collection around waits/callbacks/handoffs |
| mutation remains exact | borrowed transition signatures | deterministic leaving/adding observation fixtures |
| release duplicate stays cheap | no root/lock/allocation in call graph | codegen/microbenchmark comparison |
| deferred moving safety is honest | docs and API audit | no claim beyond current non-moving tests |

## Risks and Containment

### Repository-wide compile blast radius

Removing `Clone` too early would produce thousands of secondary errors with
little ownership information. P1-P3 deliberately migrate and inventory first;
P4 is allowed only after its readiness gate reports no transitive dependency.

### Accidental root traffic

Replacing every duplicate with a temporary root would be correct but far too
expensive. Ordinary regional duplication stays a pointer operation. Roots are
created only at genuine durable external boundaries, and root-registration
counters latch that distinction.

### Ceremonial mutator parameters

A mutator-qualified method does not by itself keep its returned `Gc<T>` from
escaping. This transition gains searchable handoffs and current-heap
validation, not complete lifetime proof. Do not overstate it, and do not add a
second word or runtime allocation merely to suggest stronger enforcement.

### Duplicate migration before compact values

The current compatibility `Value` will later change. Keep explicit edge
operations centralized in `RuntimeValueAccess` so compact representation can
reuse the policy without preserving today's large enum/container mechanics.

## Deferred Follow-Up

- Prototype `ScopedGc<'mutator, T>` only after this transition and value
  representation refinement show whether lifetime branding rejects real
  residual mistakes without excessive API friction.
- Couple persistent-edge installation to a destination-aware mutation writer
  when concurrent SATB or generational barriers require it.
- Select mutable edge-slot discovery or stable indirection before claiming
  moving collection support.
- Revisit whether `Gc<T>` should remain `Send`/`Sync` once scoped working views
  and actual cross-thread persistent owners are inventoried. This plan does not
  change that contract incidentally.
