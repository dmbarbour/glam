# Glam GC Integration Phase I5 Review — 2026-09-07

Baseline: `37186c1`, including completed implementation checkpoints I5A-I5F.4.

Status: review complete; corrective work remains open. The implemented I5
graph is sound under the current `CollectionPolicy::NoAuto` boundary and the
closed isolated collection fixtures provide strong evidence for recursive
cycle reclamation. Two structural contracts are not yet as strong as the plan
claims: newly allocated interior edges can cross an allocation region before
they are rooted or installed, and production managed-edge writers do not yet
pass through the collector's structural mutation gateway. The former blocks
concurrent production collection and any automatic policy; the latter blocks
claiming that the current code is ready for a future incremental or
generational barrier. Neither issue is exercised by current production
execution because production does not collect.

The forward plan also needs revision before I6 begins. I5F.4 established that
lazies, promises, and core nets are the mutable recursive identities, while
most I6/I7 payloads are immutable construction-acyclic paths which already
participate in exact compatibility tracing. Their proposed migration is no
longer automatically a GC-correctness prerequisite. Reflection computation is
the important exception: its external owner can close a cycle through
registered effect/target roots and therefore needs a concrete ownership split,
not merely another compatibility-shell conversion.

## Scope and Method

This mandatory post-I5 review covers both the completed implementation and
the remaining integration plan. It compares current source and verification
against:

- [`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md);
- [`GarbageCollectionRoadmap_2026-08-19.md`](../plans/GarbageCollectionRoadmap_2026-08-19.md);
- [`GarbageCollectorOwnershipLedger_2026-08-20.md`](../plans/GarbageCollectorOwnershipLedger_2026-08-20.md);
- the pre-I5 forward findings in
  [`GarbageCollectorIntegrationI5I10_2026-09-03.md`](GarbageCollectorIntegrationI5I10_2026-09-03.md);
- the managed recursive cell implementation and its source-backed inventories;
  and
- the closed reclamation fixtures and existing evaluator/net/publication
  behavior suites.

The implementation pass asks whether:

- all recursive identities changed representation atomically;
- every managed trace observes one exact stable state without doing semantic
  work;
- every durable owner is a root and every managed interior relation is an
  edge rather than a hidden root;
- promise producer routing avoids a managed-to-root backedge;
- managed destruction remains passive;
- mutation and publication chronology matches the written contract; and
- tests distinguish cycle closure from whole-production-graph certification.

The forward pass re-derives I6-I13 from the code which now exists. Drift is
classified by consequence rather than by age: a changed implementation is not
wrong merely because the older plan expected something else.

## Implemented Boundary

I5 introduced three production managed identities:

```text
ManagedLazyCell
  id + label
  source: Mutex<Option<LazySource>>
  result: OnceLock<LazyResult>

ManagedPromiseCell
  id + label + weak value-domain observer
  assignment: OnceLock<Result<Value, EvaluationFailure>>
  edge-free completion registrations
  root-free immutable producer route

ManagedCoreNetCell
  synchronized RuntimeNetCell<CoreSpecialization>
```

`LazyValue`, `PromisedValue`, and `CoreRuntimeNet` carry private `Gc` edges
plus weak value-domain observation. Durable parked or external owners carry
`ManagedLazyRoot`, `ManagedPromiseRoot`, or `ManagedCoreNetRoot`; bounded
evaluation and net operations derive non-escaping access views from one
matching `RuntimeValueAccess`.

The production trace graph is:

```text
registered RuntimeValueRoot
  -> ManagedValueNode
       -> compatibility Value shells
            -> ManagedLazyCell
            -> ManagedPromiseCell
            -> ManagedCoreNetCell
                 -> compatibility payload shells
                 -> managed cross-net edges
```

The compatibility walk stops when it reports one of those exact managed
identities. The collector worklist follows the edge; the compatibility walker
does not dereference the cell. This correctly avoids recursive visitors,
semantic evaluation, formatting, cursor progression, and callback invocation.

Promise ownership preserves the intended inversion. Task/local producer
records own registered promise roots until individual settlement. The managed
promise retains a strong immutable `PromiseProducerObligation`, but that route
contains only IDs and weak coordinator/local-owner/wait-state links. A
`PromiseProducerPublication` carries retired roots past component locks,
runtime mutation admission, and wake delivery. The managed graph cannot reach
back to its own registered root.

`ManagedCoreNetCell::trace` acquires the existing net state nonblockingly and
visits the complete logical payload vocabulary. Exclusive collector admission
excludes managed mutators, so failure to acquire that mutex correctly
indicates a violated lock/region invariant rather than a retryable trace.
Disturbance and contention companions remain edge-free.

All three managed families have stable slot requests and passive destruction
records. The only direct net-cell destruction action closes its edge-free
disturbance companion; it does not perform evaluation, scheduling, logging, or
managed observation.

## Verification Accounting

The focused I5 suite provides unusually good graph evidence:

- exact self-cycles for lazy, promise, and core-net identities;
- all three pairwise cycles and one three-family ring;
- independent cycles through list, dictionary, partial builtin, metadata,
  shared list spine, and persistent dictionary version;
- cycles through function stages and failure emission/context values;
- a real remote-cursor prepared-copy source cycle;
- rooted survival followed by exact unrooted reclamation in every fixture;
- fail-closed direct-identity, compatibility-adapter, durable-owner, and
  active-owner inventories; and
- promise publication, root retirement, coordinator/local-owner, resolver,
  contention, and runtime-teardown behavior tests.

The fixtures correctly use fresh `NoAuto` value domains. They do not borrow a
process-wide test factory or claim that an arbitrary production runtime is
collectible. The earlier process-wide metadata fixture interference was fixed
by making the collecting fixture own its value domain, which is the right
test boundary rather than evidence from repetition.

The focused `cargo test -q core::managed` run passes 72 tests. The live review
also passes `cargo fmt --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, and
`cargo test -q` (1,374 library tests plus every integration suite). No new
schedule-sensitive claim in this review relies on repeated execution.

The important verification gaps are attached to GCI5R-001 and GCI5R-002:
current source latches count writer functions and managed role declarations,
but they do not prove root-before-region-exit chronology or a call into the
collector mutation API.

## Findings

### GCI5R-001 — Fresh managed edges can escape before acquiring liveness

**Classification:** ownership chronology and future collection safety  
**Priority:** high  
**Confidence:** high  
**Status:** open; blocks I11C, automatic collection, and reuse of this
constructor pattern by additional managed identities

The collector contract is explicit: `Gc<T>` is a non-rooting pointer which may
become stale after it leaves a mutator region. Glam's own
`CoreValueAllocationScope` documentation therefore requires every pointer
leaving the region to be installed as an exactly traced edge or published as a
root.

Three production constructor families currently cross that boundary with only
an interior edge and a weak observer:

- `LazyValue::with_source` allocates `ManagedLazyCell`, then returns
  `LazyValue { edge, values, ... }` after the allocation region closes;
- `PromisedValue::with_cell` does the same for `ManagedPromiseCell`; and
- `CoreValueFactory::instantiate_core_net` returns `CoreRuntimeNet` containing
  a `ManagedCoreNetEdge` after its allocation region closes.

Subsequent calls such as `LazyValue::root`, `PromisedValue::root`, and
`CoreRuntimeNet::root` open another mutator region and treat the edge as live.
That is safe today only because the production heap is `NoAuto` and explicit
test collections occur after fixture roots have been installed. A collection
between allocation-region exit and later publication could reclaim the cell;
the weak value-domain observer keeps neither the allocation nor its heap root
alive.

Most evaluator construction is less exposed than the type shape suggests:
nested runtime access keeps an outer mutator active and public `ScopedValues`
usually roots the result before the outer region closes. The constructors do
not encode that condition, however, and promise registration and stand-alone
net construction have real two-region handoffs. I11C's collection-during-
worker schedules and any I12 automatic outer-entry election cannot rely on
`NoAuto` to cover the gap.

Recommended resolution before introducing another managed identity:

1. inventory every allocation-to-first-owner handoff for the three families;
2. distinguish construction which installs an edge within an existing access
   region from construction which returns to external Rust;
3. make fresh allocation return a zero-cost scope-bound `NewGc<'scope, T>`
   state which cannot escape its mutator region as an ordinary interior edge;
4. consume that state by installing an exact traced edge or publishing the
   root which the receiving owner actually requires; use a separately rooted
   handoff only across a genuine access/orchestration boundary; and
5. add forced-order tests which stop immediately after allocation, attempt
   collection at the former escape boundary, and prove the selected traced
   owner or intentional root—not disabled collection—preserves liveness.

This does not show a current production use-after-free: production collection
is disabled. It does show that the current constructor boundary cannot be
certified for the later collection modes described by the plan.

#### GCI5R-001 remediation plan

This finding is large enough to resolve through explicit checkpoints before
I6 begins. The checkpoints live with the review because they repair I5's
construction boundary; the forward integration plan should consume the closed
finding as an entry condition rather than reproduce this corrective
chronology.

The target construction invariant is:

> A fresh managed allocation becomes an exactly traced edge or the intended
> registered root before the allocating `RuntimeValueAccess` ends. A root is
> not introduced merely to bridge two implementation statements which should
> share one access region.

The ordinary path must not register a temporary root for every allocation.
That would add root-registry synchronization, tracing work, and retirement
traffic to the value-construction hot path. A real rooted handoff remains
available only when a wait, callback, coordinator operation, lock boundary, or
other orchestration seam cannot retain managed access.

The proposed zero-cost construction state is conceptually:

```rust
#[repr(transparent)]
struct NewGc<'scope, T> {
    value: Gc<T>,
    _scope: PhantomData<&'scope Mutator<'scope>>,
}
```

This must be a wrapper rather than a type alias so the lifetime participates
in Rust's type system. It is neither `Copy` nor `Clone`, has the same runtime
representation as `Gc<T>`, may be dropped as ordinary future garbage, and
does not expose a safe freely copyable `Gc<T>`. Root publication consumes it.
Conversion to an interior `Gc<T>` is unsafe at the collector boundary because
the collector cannot prove that the caller installed it in a traced owner;
Glam's family-specific construction or mutation API provides the safe
operation which performs that installation. Local scoped callbacks remain
available for building a fresh parent from a fresh child, but evaluator
control flow does not become continuation-passing style.

`CoreValueFactory` and `RuntimeValueAccess` retain distinct ownership roles:

- `CoreValueFactory` is the cloneable, long-lived value-domain owner and the
  entry capability which opens managed access. It may also retain
  control-plane resources which do not themselves project bare values.
- `RuntimeValueAccess<'scope>` is the complete internal operational interface
  for constructing, cloning, observing, rooting, or mutating semantic values.
  It should borrow the `CoreValueFactory`, rather than retain only the bare
  domain, so it can use runtime IDs, canonical roots, caches, coordinator
  bindings, and compilation-local cache extensions without a redundant
  factory argument.
- The two types must not literally merge: code crossing a wait, host callback,
  reflection activation, scheduler handoff, or net-contention park retains the
  factory and opens a new bounded access region afterward. Do not use `Deref`
  to blur that distinction.
- The public `Values` API remains separate. It opens internal access and
  publishes a runtime root before returning a public value.

##### GCI5R-001A — Construction and factory-use inventory

1. Inventory every direct `allocate_managed_lazy`,
   `allocate_managed_promise`, and `allocate_managed_core_net` site and follow
   every high-level constructor to its first authoritative owner. Include
   initialization performed after allocation, such as the second access used
   to cache an initially failed lazy.
2. For each chain record the active access authority, first exact owner,
   access/callback/wait/lock boundary crossed, present liveness witness,
   intended scoped or rooted disposition, and exact root-retirement point.
3. Audit `from_root` projections separately. Record which retained root or
   traced containing owner supplies liveness for every projected facade.
4. Classify production `CoreValueFactory` uses as domain/control, access entry,
   value construction/observation, or orchestration boundary. Move only the
   operations needed to close this finding; preserve the inventory as input to
   a later broader API cleanup.
5. Build a deterministic mismatch fixture which ends the old allocation
   region and collects before first publication. It must observe reclamation
   without dereferencing the stale edge, fail under the desired invariant, and
   be updated with the implementation rather than retained as a contract for
   the defect.

##### GCI5R-001B — Scope-bound fresh allocation

1. Add the transparent `NewGc<'scope, T>` allocation result at the narrowest
   collector boundary which prevents an ordinary fresh `Gc<T>` from escaping.
   Prefer `Allocator::alloc` returning the new state; if a lower-level raw
   constructor must remain, keep it private and document its safety proof.
2. Provide consuming operations for intended root publication and an unsafe,
   precisely documented collector-level transition to a traced interior edge.
   Do not provide `Deref<Target = Gc<T>>`, `AsRef<Gc<T>>`, `Copy`, `Clone`, or a
   safe unrestricted `into_gc` escape.
3. Add compile-time representation checks plus focused allocation, rooting,
   discard/reclamation, and Miri coverage. Add compile-fail evidence that the
   scope-bound state cannot leave the mutator callback if that can be done
   without adopting a disproportionate test framework.

##### GCI5R-001C — Operational value-access facade

1. Make `RuntimeValueAccess` borrow the entering `CoreValueFactory` and retain
   the existing allocation scope. Preserve the lifetime and thread-boundary
   guarantees established by I3.
2. Move or delegate the value operations required by the three constructor
   families onto access. Allocation calls must no longer redundantly accept
   both `&RuntimeValueAccess` and `&CoreValueFactory`.
3. Add access-owned root/value publication helpers so `ScopedValues` and other
   already-admitted callers do not reopen nested access merely to publish the
   containing `RuntimeValueRoot`.
4. Keep runtime/coordinator ownership and access-opening operations on the
   factory. Defer unrelated mechanical factory-call migration rather than
   widening this defect repair.

##### GCI5R-001D — Lazy construction cutover

1. Replace self-opening `LazyValue::with_source` with access-taking fresh-lazy
   construction. The ordinary result remains scope-bound until consumed into
   a traced value/owner or intended root.
2. Migrate every lazy constructor from the inventory. Keep callback-free
   evaluator construction inside its existing `with_value_access` region;
   split any path which presently spans callbacks or waits.
3. Initialize already-terminal lazies, including failure values, before first
   publication under the same access rather than reopening the heap through
   the unrooted facade.
4. Add forced-order survival, post-publication root-retirement, early-return
   reclamation, and representative evaluator/public-value tests.

##### GCI5R-001E — Promise construction cutover

1. Replace self-opening `PromisedValue::with_cell` with access-taking fresh
   construction.
2. Publish the root required by the resolver, local producer, or coordinator
   owner before the allocation access ends. This is the promise's intended
   durable owner, not an intermediate construction root.
3. Do not hold managed access across coordinator callbacks or waits. Where
   registration must proceed afterward, carry the already-intended root and
   project the facade from it only while that root remains owned.
4. Force collection between construction and registration/publication, and
   preserve cancellation, abandonment, assignment, resolver drop, and root
   retirement coverage.

##### GCI5R-001F — Core-net construction cutover

1. Make core-net instantiation consume the active value access rather than
   opening an allocation region and returning a bare `CoreRuntimeNet`.
2. Install the fresh net directly into its containing traced value/net owner
   when construction is local. Use an intentional managed-net or containing
   value root only for a genuine external handoff.
3. Generalize the ownership pattern already demonstrated by
   `CorePreparedCopySource` where an existing normalization handoff truly must
   leave access, without making prepared roots the default constructor result.
4. Cover standalone instantiation, related instantiation, function/net
   wrapping, copy-source handoff, and collection immediately after permanent
   publication.

##### GCI5R-001G — Closure audit and review reconciliation

1. Delete or privatize every constructor which can open managed access and
   return a fresh bare edge or a structure containing one. Add a fail-closed
   source inventory for this temporal rule; do not mistake it for the
   forced-order behavioral proof.
2. Reconcile `CoreValueAllocationScope`, `RuntimeValueAccess`, collector, and
   ownership-ledger documentation with the final `NewGc` handoff vocabulary.
3. Run focused collector/managed/evaluator/net/publication tests, Miri for the
   affected collector boundary, and the repository routine checks.
4. Update this finding with final evidence and mark it closed only when the
   deterministic former-gap tests pass without relying on `NoAuto`. Then
   update the integration roadmap entry conditions for I11C and I12.

### GCI5R-002 — Production writers do not yet enter the collector mutation gateway

**Classification:** structural barrier contract drift  
**Priority:** high  
**Confidence:** high  
**Status:** open; current STW execution is sound, but I8 and any concurrent,
incremental, generational, or moving collector remain blocked

The roadmap and ownership ledger say every post-publication managed-edge
mutation passes through the collector-owned structural gateway even though the
current stop-the-world implementation makes that gateway a no-op.

Current production code has good representation-local writer boundaries:

- `ManagedLazyAccess::cache` is the sole terminal-result writer and source
  remover;
- `ManagedPromiseAccess::{publish_detached,publish_guarded}` are the assignment
  writers; and
- `CoreRuntimeNetAccess` funnels topology changes through the synchronized
  `RuntimeNetCell` API.

Those methods do not call `Mutator::with_edge_replacement`. All current
`with_edge_replacement` uses below `src/core/managed` are test fixture edge
installations. The test named
`recursive_edge_mutations_use_representation_gateways` checks lexical writer
counts, not collector-gateway participation.

This is not an immediate tracing race. Every production writer requires a
`RuntimeValueAccess`, while full collection excludes all mutators. The exact
trace therefore sees a stable state. The issue is architectural: the future
barrier cannot be activated at sites which have not been structurally routed
through it.

The existing one-edge `old/new` API is also too narrow to sprinkle mechanically
over these writers. A lazy result, promise assignment, or net rewrite may add
or remove several managed identities beneath a compatibility `Value` payload.
The corrective design should choose an owner-level or visitor-backed update
gateway which can conservatively report an arbitrary edge-set transition,
while retaining the precise single-edge gateway for naturally singular
fields. Lazy source deletion must leave room for an SATB-style old-edge rule;
net writes need one integration point under the existing net mutex.

Recommended staging:

- repair lazy and promise writer gateways in an I5 corrective checkpoint;
- make the owner/set gateway part of the stable `RuntimeValueAccess` rather
  than exposing the raw collector mutator;
- implement the high-volume net routing as a distinct I8 mutation checkpoint;
  and
- replace the current writer-count latch with tests which prove every
  production edge-changing path invokes the appropriate structural gateway.

### GCI5R-003 — Lazy and promise façades duplicate cell identity data

**Classification:** undocumented representation drift  
**Priority:** medium  
**Confidence:** high  
**Status:** open design choice; does not block current correctness

The I5.0 disposition says IDs and labels remain cell-resident and are copied
only into diagnostics or explicit indexes. The implementation stores both
`id` and `label` in every `LazyValue` and `PromisedValue` façade in addition to
the same fields in the managed cell. Their registered-root holders repeat the
fields again. `PartialEq`, dependency-key construction, cycle reporting, and
debugging can consequently read identity without managed access.

This is understandable convenience drift, and copying an ID into an explicit
scheduler index is consistent with the accepted model. Copying an `Arc<str>`
into every semantic edge is broader: it increases every façade and lets a
non-rooting edge continue to expose diagnostic state after the allocation has
become stale. It also weakens the intended distinction between a managed
semantic edge and a durable diagnostic/coordination record.

Before I6 establishes more façade patterns, select and document one policy:

- keep only edge plus weak domain on semantic façades and copy identity into
  the durable roots/work records which genuinely need it;
- retain a compact copied ID on façades but keep labels cell/root resident; or
- explicitly accept the duplicate identity cache as a performance tradeoff
  and add it to the ownership ledger and later Value Representation Refinement
  work.

The registered root may reasonably cache identity needed outside a mutator;
the finding is principally about every interior `LazyValue`/`PromisedValue`
occurrence.

### GCI5R-004 — I6 treats traced immutable paths as mandatory new identities

**Classification:** future-phase role and scope drift  
**Priority:** medium  
**Confidence:** high  
**Status:** open; blocks implementation of I6 as currently written

I5F.4 records an important result: after removing the three mutable recursive
identities, the compatibility-owned semantic graph is construction-acyclic.
The I5 cycle matrix already proves that partial builtins, metadata carriers,
function stages, failure emissions/contexts, lists, dictionaries, and shared
persistent spines lead back to managed identities through the central trace.

I6 still says to migrate function wrappers, partial applications, fixpoint
payloads, metadata identity, and failures as though each conversion were
needed to make cycles collectible. Current representations do not support an
independent safe-Rust cycle in those immutable shells. A cycle which passes
through one closes at a lazy, promise, or core-net identity and is already
collectible.

This does not prove those conversions are undesirable. Replacing `Arc<Value>`
and similar shells may reduce ownership duplication, prepare compact values,
or simplify eventual tracing. Those are representation motives, not current
Gate-G2 correctness prerequisites, and some belong more naturally to Value
Representation Refinement.

Revise I6 before implementation:

1. split the phase by concrete representation and owner boundary;
2. for each immutable shell, state the benefit which justifies a managed
   allocation now;
3. permit an audit-only outcome when exact compatibility tracing and passive
   destruction already satisfy the initial collector;
4. keep identity-sensitive metadata/failure behavior explicit if conversion
   is retained; and
5. do not create new construction handoffs until GCI5R-001 has selected their
   liveness protocol.

At minimum, I6A must no longer combine function stages, partial builtin
arguments, applications, and fixpoints in one checkpoint. I6B and I6C should
separate semantic identity preservation from durable external
`RuntimeFailureRoot`/diagnostic ownership.

### GCI5R-005 — Reflection closure is split inconsistently between I6D.1 and I10A

**Classification:** future ownership chronology conflict  
**Priority:** high  
**Confidence:** high  
**Status:** open; blocks I6D.1 and Gate G2 planning

`ReflectionComputation` is the one compatibility adapter which deliberately
reports no semantic value edge. Its runtime external owner currently retains:

- a rooted reflection effect;
- an optional rooted gate target; and
- one installed reservation or reservation failure.

That representation can conservatively retain a cycle:

```text
ManagedLazyCell
  -> ReflectionComputation handle
       -> external owner
            -> RuntimeValueRoot(effect or target)
                 -> the same lazy graph
```

I6D.1 correctly proposes moving actual semantic values into exact managed
state while keeping reservation activation/cancellation external. However,
I5F.4 and the active-owner inventory classify reflection reservation backedges
as I10A work alongside arbitrary host callbacks. Those dispositions cannot
both remain authoritative.

Reflection computation is structurally knowable and should be resolved in
I6D.1. Arbitrary `HostCallOperation` closure environments are not structurally
knowable and remain I10A. Update the inventory and plan accordingly.

I6D.1 also needs bounded checkpoints. The current `ReflectionComputationOwner`
combines immutable semantic roots with active one-write reservation state.
Separate:

1. managed-reachable effect/target edges and their exact trace;
2. external reservation/activation/cancellation authority;
3. installed reservation failure ownership, which may itself contain values;
4. publication and retirement order; and
5. the forced-order and closed-cycle matrix.

The phase must prove that no active external owner becomes reachable from a
managed allocation and that no effect/target root remains merely to conceal
an internal cycle.

### GCI5R-006 — I7-I11 need delta-oriented checkpoints after the I5 cutover

**Classification:** future verification and checkpoint drift  
**Priority:** medium  
**Confidence:** high  
**Status:** open plan update

The remaining direction is sound, but several phases still describe work
which I5 already completed:

- **I7:** logical list/dictionary visitors, non-forcing thunk visitation,
  shared-spine accounting, persistent-cycle reclamation, and duplicate-work
  counters already exist. Keep I7 as a final source/representation audit and
  add only missing closed shapes such as a list-thunk backedge. Do not repeat
  all I5F.3a fixtures under new names.
- **I8A:** this is now the natural home for the unresolved high-volume net
  mutation gateway from GCI5R-002. Split payload/visitor reconciliation from
  writer/gateway implementation and from lock/lifecycle revalidation.
- **I8B:** direct self/pairwise/cursor-source cycles overlap I5F.1-I5F.3c.
  Retain only delta cases—stuck reasons, pending active-pair work, newly
  retained I6 payloads, and any state not already covered—and partition those
  by topology state.
- **I8C:** remove stale net-specific adapters and comments, but preserve the
  central compatibility walk until a later representation project makes each
  replacement exact.
- **I9:** consume I5's existing M/R/A/C and active-owner inventories as the
  baseline. Promise resolver and producer-root retirement were already
  audited in I5E; I9 should test only ownership/lifecycle deltas introduced by
  I6-I8 plus the mandatory final external-RAII/source inventory.
- **I10:** retain arbitrary host-callback containment and the opaque decision
  gate. Remove reflection computation from I10A once I6D.1 owns it.
- **I11/Gate G2:** add a temporal root-publication inventory and
  forced-order evidence for GCI5R-001. A declaration-only root inventory does
  not prove that an allocation remains live between allocation, handoff, and
  first publication. I11C's worker/collection schedules may begin only after
  that contract is closed.

I12's automatic-policy review must explicitly treat every constructor which
can open a second mutator region while holding an interior edge. Even if I11's
controlled maintenance is confined to stable boundaries, an automatic
collector can elect on precisely that second entry. I13 should own only
redundant wrapper/provenance cleanup after these semantic boundaries are
stable; it must not be the first phase to discover a liveness or barrier gap.

### GCI5R-007 — Phase status and source comments lag the atomic cutover

**Classification:** documentation drift  
**Priority:** low  
**Confidence:** high  
**Status:** open; resolve with the substantive findings

The integration phase table still marks only I5F.1 complete and I5 itself
pending, despite completion records for I5F.2-I5F.4. The ownership ledger's
status introduction still describes I0-I4 as the current boundary and broadly
says recursive compatibility payloads wait for I5-I8, although its detailed
rows describe the managed cells correctly.

Several source comments likewise say I8 will migrate core-net ownership or
first use the core-net adapter in production tracing. I5D already performed
that migration; I8 is now a post-cutover audit. These comments are harmless to
execution but actively misstate the chronology used by later reviewers.

Do not mark I5 fully complete merely by updating the table. First disposition
GCI5R-001 and GCI5R-002, then reconcile the table, ledger header, stale
`before I8` comments, and the future phase text in one closeout.

## Drift Assessment

### Intentional and justified

1. **The three recursive identities moved atomically.** This was larger than
   the original variant-by-variant chronology but was required to avoid a raw
   net owner containing the first managed recursive pointer.
2. **Compatibility traversal remains central.** It is exact for the current
   representation and permits immutable Rust-owned shells to connect managed
   identities without pretending every shell is already a managed family.
3. **Promise producer provenance remains after settlement.** The strong
   root-free producer record preserves timing-independent diagnostics while
   weak routes prevent a root backedge.
4. **Core-net disturbance is separated from semantic ownership.** The
   edge-free companion can wake and close independently without retaining
   topology.
5. **Cycle fixtures use isolated value domains.** This deliberately avoids
   global test-factory roots and makes exact reclamation assertions meaningful.
6. **Duplicate persistent-spine visits remain unoptimized.** Marking deduplicates
   the managed target; logical duplicate visits are profiling evidence, not a
   correctness defect.

### Corrective new information

1. **The immutable compatibility graph is already sufficient for cycle
   closure once its three mutable identities are managed.** This weakens the
   correctness motive for much of I6/I7 and should reduce mandatory migration.
2. **Reflection computation, unlike the other immutable shells, crosses an
   external registered-root boundary.** Its cycle closure needs an ownership
   split even if other I6 structures remain compatibility-owned.
3. **A writer API is not automatically a collector mutation gateway.** The
   current single-edge gateway does not describe arbitrary compatibility
   payload replacement, so the structural barrier vocabulary needs one more
   shape.
4. **Root topology inventories do not prove temporal publication.** A bare
   interior edge can be perfectly classified by type and still become stale
   between mutator regions.

### Accidental or convenience-driven drift

1. `LazyValue` and `PromisedValue` duplicate IDs and labels outside their
   managed cells without a recorded representation decision.
2. Tests described as mutation-gateway evidence currently establish only
   single-writer lexical structure.
3. Phase status, ownership-ledger introduction, and pre-I8 source comments did
   not follow the atomic net cutover.

## Future-Phase Disposition

| Phase | Current disposition after I5 | Required adjustment before execution |
| --- | --- | --- |
| I6A-C | Exact compatibility tracing already closes cycles through these immutable shells. | Partition by representation; justify managed conversion independently or permit audit-only completion. |
| I6D.1 | Required to eliminate reflection effect/target root backedges. | Split semantic edges from active reservation lifecycle and take ownership away from I10A. |
| I6D.2 | Net-construction `Arc<Value>` is immutable and already traced. | Treat conversion as optional representation cleanup unless another identity/lifecycle need is found. |
| I7 | Visitor and most cycle evidence already exist. | Narrow to delta audit, missing thunk/backedge shape, and duplicate-work measurement. |
| I8 | Managed core-net owner already exists. | Split final payload audit, structural mutation gateway, delta cycle states, and adapter/comment retirement. |
| I9 | I5 already changed and audited several root/RAII surfaces. | Start from I5 inventories and test only I6-I8 deltas plus final mandatory source audits. |
| I10 | Host callbacks and opaque storage remain real deferred boundaries. | Remove reflection after I6D.1; preserve host-capture and opaque decision gates. |
| I11 | Stable-boundary forced collection remains viable in principle. | Gate worker-concurrent collection on root-before-exit chronology and complete barrier/source audits. |
| I12 | Automatic entry can collect at the most dangerous handoff boundary. | Make GCI5R-001 a hard policy-review prerequisite and inventory second-entry constructors. |
| I13 | Redundant compatibility/provenance cleanup remains appropriate. | Do not defer liveness or mutation safety here; add any accepted ID/label cache to its cleanup ledger. |

## Recommended Resolution Order

1. Resolve GCI5R-001's construction and first-publication protocol. This is
   the only finding which can make a currently valid `Gc` stale before later
   access once production collection is enabled.
2. Select the owner/set mutation-gateway shape, then route lazy and promise
   writers through it. Leave the bounded high-volume net implementation to a
   newly explicit I8 checkpoint.
3. Decide whether façade-cached IDs and labels are intentional, and reconcile
   the ledger either way.
4. Rewrite I6 around the immutable-shell result and partition reflection
   computation into semantic and external-lifecycle checkpoints.
5. Narrow I7, repartition I8, and update the I9-I12 entry conditions described
   above.
6. Re-run the focused and routine checks, update phase status and stale source
   comments, and close I5 before implementation proceeds into I6.

Production remains `CollectionPolicy::NoAuto`. This review authorizes no full
collection over a production runtime and does not advance Gate G2.
