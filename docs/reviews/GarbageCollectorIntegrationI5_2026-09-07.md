# Glam GC Integration Phase I5 Review — 2026-09-07

Baseline: `37186c1`, including completed implementation checkpoints I5A-I5F.4.

Status: review complete; GCI5R-001 is closed and corrective work remains open
for the independent mutation-gateway finding. The implemented I5 graph is
sound under the current `CollectionPolicy::NoAuto` boundary and the closed
isolated collection fixtures provide strong evidence for recursive cycle
reclamation. Regional construction now establishes an exact traced or rooted
owner before managed access ends. Production managed-edge writers still do
not pass through the collector's structural mutation gateway, which blocks
claiming that the current code is ready for a future incremental or
generational barrier. Production still does not collect.

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

GCI5R-001's closure adds both an exact classified constructor-bearing source
inventory and deterministic root-before-region-exit chronology. The remaining
important verification gap is GCI5R-002: current writer latches count writer
functions but do not prove a call into the collector mutation API.

## Findings

### GCI5R-001 — Fresh managed edges can escape before acquiring liveness

**Classification:** ownership chronology and future collection safety  
**Priority:** high  
**Confidence:** high  
**Status:** closed 2026-09-08 by GCI5R-001B-G; the regional-allocation rule
remains mandatory for every later managed family

The collector contract is explicit: `Gc<T>` is a non-rooting pointer which may
become stale after it leaves a mutator region. Glam's own
`CoreValueAllocationScope` documentation therefore requires every pointer
leaving the region to be installed as an exactly traced edge or published as a
root.

At the review baseline, three production constructor families crossed that
boundary with only an interior edge and a weak observer:

- `LazyValue::with_source` allocates `ManagedLazyCell`, then returns
  `LazyValue { edge, values, ... }` after the allocation region closes;
- `PromisedValue::with_cell` does the same for `ManagedPromiseCell`; and
- `CoreValueFactory::instantiate_core_net` returns `CoreRuntimeNet` containing
  a `ManagedCoreNetEdge` after its allocation region closes.

Subsequent root publication originally opened another mutator region and
treated the edge as live. That chronology was safe only while production
remained `NoAuto`: a collection between allocation-region exit and later
publication could reclaim the cell because the weak value-domain observer kept
neither the allocation nor its heap root alive.

GCI5R-001B-F closed those gaps. Containing-value construction now publishes in
the allocating access region; evaluator operations retain intended family
roots in a step-local publication nursery; and genuine promise or core-net
orchestration handoffs carry explicit family roots. `ScopedValues::wrap`
publishes through its already-active access rather than nesting another entry.
G.2 inventories and classifies the resulting constructor-bearing surface.

Recommended resolution before introducing another managed identity:

1. inventory every allocation-to-first-owner handoff for the three families;
2. distinguish construction which installs an edge within an existing access
   region from construction which returns to external Rust;
3. make raw family allocation private and keep the complete intermediate graph
   within its caller-owned managed-access region;
4. end that region only after installing the graph in an exact traced owner or
   publishing the root which the receiving owner actually requires; use an
   already-intended root across a genuine access/orchestration boundary; and
5. add forced-order tests which stop immediately after allocation, attempt
   collection at the former escape boundary, and prove the selected traced
   owner or intentional root—not disabled collection—preserves liveness.

The original finding did not demonstrate a production use-after-free because
production collection was disabled. Its deterministic mismatch instead
identified a constructor boundary which had to be repaired before the later
collection modes described by the plan could be certified.

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

The construction unit is the admitted region, not each individual allocation.
Several new cells may safely remain unrooted while one mutator protects them;
the completed graph must acquire its exact owner before that region ends. A
generic `NewGc<'scope, T>` rejects direct escape from an allocator, but cannot
prove that an eventual `Value` containing the converted edge was rooted or
installed. An unrestricted conversion to `Gc<T>` would therefore move rather
than close the safety boundary, while recursively branding `Value` and all of
its containers would be a disproportionate migration.

The corrective design consequently keeps `Allocator::alloc` unchanged and
narrows Glam's integration surface instead. Raw managed-family allocators are
private to the regional construction/publication layer. External operations
either build and publish the completed root under one `RuntimeValueAccess`, or
install the completed edge into an already traced owner under that access. A
real intended root crosses an unavoidable wait, callback, coordinator, or lock
boundary. A private fresh-allocation wrapper remains an optional local
implementation aid only if a prototype can consume every instance directly
into one of those owner-producing operations without exposing an unrestricted
escape. It is not a collector-wide contract or an entry condition for this
repair.

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

##### GCI5R-001A — Construction and factory-use inventory (complete)

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

###### Direct allocation and constructor inventory

All three allocation primitives are methods on `RuntimeValueAccess`, but each
currently returns an ordinary copyable edge. Their production facade
constructors open and close access internally:

| Family | Direct allocator | Production facade constructor | Work after allocation | Current first durable owner |
| --- | --- | --- | --- | --- |
| lazy | private `allocate_managed_lazy` | `LazyValue::with_source` or `failure`, delegating to regional gateways | reads the ID before returning; `failure`/`error` install the initial terminal cache before the gateway returns | a later containing `RuntimeValueRoot`, managed net payload, or machine result |
| promise | private `allocate_managed_promise` | `PromisedValue::with_cell`, reached by `new` and `fixpoint`, delegating to a regional gateway | `fixpoint` later registers and installs its producer | a later `ManagedPromiseRoot` in a resolver/coordinator/local owner, or a containing value root |
| core net | private `allocate_managed_core_net` | `CoreValueFactory::instantiate_core_net`, also reached by `instantiate_related`, delegating to a regional gateway | none before returning the facade | a later `ManagedCoreNetRoot`, containing value root, or traced net payload |

The `construct_rooted_managed_*` gateways in
[`recursive_cells.rs`](../../src/core/managed/recursive_cells.rs) now allocate
and register the intended family root under one access. The complementary
`construct_managed_*` gateways return an owner-neutral facade for immediate
installation below a traced owner in the same region.
`CorePreparedCopySource`, `CoreFrontierObservation`, and
`NormalizationRequest` similarly root an *existing* net before their genuine
handoff. These are publication precedents; the D-F cutovers still have to
carry each production construction through its actual first owner.

Every nonterminal production lazy constructor reaches the one `with_source`
boundary:

- `computed_fixpoint`, `semantic_computation`, and `external_host_call` select
  their corresponding source records;
- `from_access`, `from_application`, `from_builtin`,
  `from_net_construction`, `from_function_call`, `from_net_computation`, and
  `from_reflection_gate` package semantic work; and
- `error`/`failure` are the exceptional terminal path and now use
  `construct_failed_managed_lazy` to install the cached failure before the
  facade leaves its allocating access.

The test-only `semantic_thunk` and `host_call` wrappers use the same production
boundary. `Value::{failure,error,external_host_call,reflection_gate,
reflection_task_result,builtin_call}` add no ownership; they only select or
wrap the same lazy constructors. The external host-call registry owns the
callback producer, not the managed lazy cell.

###### Allocation-to-owner chains

The following table follows production construction through its first exact
owner. “Gap” means collection can run after the listed access closes but before
that owner is published. The liveness witness in every gap is currently only
`CollectionPolicy::NoAuto`.

| Construction zone | Active authority and crossed boundary | First authoritative owner | Disposition and retirement | Finding |
| --- | --- | --- | --- | --- |
| public `ScopedValues::{access,apply}` and saturated builtin wrapping | an outer `Values::with_access` remains active across nested construction and `ScopedValues::wrap`; no callback/wait | containing `RuntimeValueRoot` | publish directly from the existing access; retire when public `Value` drops | safe chronology today, but needlessly reenters access |
| `Assembler::net` | `instantiate_core_net` closes access before `Values::wrap` opens another | public containing value root | scoped net construction followed by same-access value-root publication; retire with public `Value` | gap |
| `Assembler::promise` and `ReflectionEnvironmentBuilder::promise` | promise allocation closes before both `Values::wrap` and `promise.root` | containing public value root plus resolver's `ManagedPromiseRoot` | publish both intended owners under construction access; resolver root retires on resolve/fail/drop, value root with public `Value` | gap |
| `CompileContext::new` final-definitions promise | `PromisedValue::new` closes before outer `RuntimeValueRoot::new` | `CompileContext::final_defs` value root | install directly in that containing root; retire with compile context | gap |
| per-declaration source lowering | `ModuleLowerer::lower_declaration` holds broad access while lowering, returns a bare definitions value from it, then roots afterward | replacement `ModuleLowerer::definitions` root | publish the replacement root before the access ends; prior root retires on replacement | gap |
| completed source lowering | `ModuleLowerer::finish` projects its definitions root to a bare `LoweredSource`, consumes the old owner, and `compile_source` roots later | compiled definitions root | transfer/retain a root through `LoweredSource`, or publish the final root before return; retire with compiled module | unproven root-to-facade transfer and gap |
| evaluator application, operator, and builtin result construction | callback-free semantic functions create lazies outside `EvaluationValueAccess`; results cross an operator-yield or machine-step boundary before `root_value` or net installation | evaluator result root, or exact traced payload installed by `claim.finish` | construct under a bounded access and consume into the immediate result owner; do not span waits, host callbacks, or contention parks | gap |
| list-effect fixpoint and deferred lists | promise/lazy constructors run while building a bare list result; the promise is later reached through that graph rather than registered separately | eventual evaluator result root and traced list graph | construct and assemble graph under one callback-free access; retire with result graph | gap |
| reflection-store rewrite | lazy access/application is built before `apply_value_at_path` calls public wrapping | replacement public store root | construct and publish the replacement under one access; prior root retires on committed replacement | gap |
| task-owned `PromisedValue::fixpoint` | allocation closes before `register_promise`, which may enter coordinator state and therefore cannot run under managed access | coordinator/local-owner `ManagedPromiseRoot` | publish the intended root before registration, carry it across orchestration, and retire on assignment/cancellation/abandonment | gap; requires rooted handoff |
| reflection fixpoint | task-owned registration is followed by a marker value root and an `ActiveFix` promise root | coordinator promise root, marker value root, and active-fix root | same rooted registration handoff; active roots retire when continuation completes | gap before registration |

The evaluator row includes the concrete producers in
[`operator.rs`](../../src/eval/operator.rs),
[`application.rs`](../../src/eval/application.rs), and `eval/builtins/*`.
`progress_core_operator_claim` confirms the chronology: semantic application
returns an `OperatorYield` first, then `claim.finish` reenters net access to
install it. The source-lowering row includes nested net construction in
[`net_lowering.rs`](../../src/g_syntax/net_lowering.rs), not just the outer
definitions dictionary.

###### Root projection audit

Projecting a facade from a durable root is valid only while that root, or an
already-traced containing owner, remains live:

| Projection sites | Liveness witness | Result |
| --- | --- | --- |
| `LazyTaskMachine::lazy` | the machine retains its `ManagedLazyRoot` field | proven |
| deferred lazy-cycle completion in `evaluation/pump.rs` | the cycle member retains its root through cache publication and terminalization | proven |
| `WorkDependency` coordinator probes/subscriptions | the dependency retains `ManagedPromiseRoot` for each immediate facade call | proven |
| local-owner `fail_all` | each obligation retains its root through immediate failure publication | proven |
| `PromiseResolver::{resolve,fail,drop}` | the affine resolver retains/takes its root through immediate publication | proven |
| reflection `Continuation::Fix` | `ActiveFix` and the continuation retain cloned roots through immediate assignment | proven |
| `client_demand_halt` | consumes `WorkDependency::Promise`, projects a facade, then drops the only root visible in the function before returning the halt | **unproven**; retain the root in the halt/dependency representation or prove a separate containing owner |

No `from_root` call should be changed mechanically: the proven sites are the
intended bounded projection API. The final site is an owner-transfer defect to
resolve with the promise cutover.

###### `CoreValueFactory` role classification

The production uses divide cleanly enough to guide GCI5R-001C without turning
this repair into a general API rewrite:

| Role | Representative responsibilities | Disposition |
| --- | --- | --- |
| domain/control | runtime and managed IDs, heap/domain lifetime, coordinator binding, external-owner registry, canonical/runtime caches | remain on `CoreValueFactory` |
| access entry | `with_runtime_value_access` and weak domain observers | remain on the factory; access should borrow the entering factory |
| scoped value work | managed allocation, observation, edge mutation, root publication, cached-value cloning, and constructor helpers | move or delegate only the operations needed by this repair to `RuntimeValueAccess` |
| orchestration boundary | `EvalContext`, compiler/source lowering, reflection store/machine, sessions, coordinator, and public `Values` retain a long-lived domain handle across waits/callbacks/locks | retain the factory, open a fresh narrow access on each safe side of the boundary |

The inventory therefore does **not** recommend a literal factory/access merge.
It recommends that `RuntimeValueAccess` borrow the factory so already-admitted
code has the complete operational value API without passing two capabilities
or reopening the heap.

###### Latched mismatch evidence

At the review baseline,
`fresh_managed_facades_survive_until_first_publication` ended the old
allocation region for one failed lazy, one promise, and one core net before
publishing an owner. It then collected and observed the deterministic mismatch:

```text
left: 3 finalized slots
right: 0 finalized slots
```

That historical result was the mismatch latch, not accepted behavior. The
current, non-ignored fixture constructs the three facades within one
`construct_runtime_value_root` operation, attempts collection immediately
after allocation and observes `CollectionError::ActiveMutator`, then collects
after return and proves that the containing root retains all three identities.
Dropping that root makes the graph collectible. The exact current evidence
command is:

```sh
cargo test -q fresh_managed_facades_survive_until_first_publication
```

The distinct orchestration case is covered by
`evaluator_step_guards_fresh_managed_results_until_containing_root_publication`:
the evaluator publication nursery keeps the intended family roots alive across
a successful collection between access regions. G.4 records the final focused
and repository-wide results.

##### GCI5R-001B — Operational regional-access foundation (complete)

This checkpoint absorbs the former operational-access checkpoint because its
authority is a prerequisite for expressing the regional publication boundary;
the owner-producing API should not first be designed around a bare domain and
then immediately rebuilt around the factory.

1. **B.1 — Access authority (complete).** Keep
   `glam_gc::Allocator::alloc` returning
   `Gc<T>`, and document that mutator admission is the liveness witness for all
   unpublished intermediate allocations in one construction region. Make
   `RuntimeValueAccess` borrow the entering `CoreValueFactory` while retaining
   its existing allocation scope and I3 lifetime/thread guarantees. Move or
   delegate only the operations needed by this repair; allocation and
   observation must not redundantly require a separately supplied factory.

   Completed on 2026-09-07. `RuntimeValueAccess` now retains the exact entering
   factory view, including compilation-local extensions, and derives managed
   lazy, promise, and core-net construction state from that authority. The
   allocators no longer accept a redundant factory argument. A focused test
   distinguishes retention of the exact scoped factory view from ordinary
   same-domain provenance checks. Collector and Glam access documentation now
   state that active mutator admission protects an unpublished intermediate
   graph only until the region ends; later survival still requires a root or
   installation under an exactly traced owner. Root publication remains B.2.
2. **B.2 — Root publication (complete).** Add an access-owned containing-value root
   publisher over `RuntimeValueRoot::new_from_access`, plus a factory entry
   which runs a callback-free construction closure and publishes its returned
   graph before managed access ends. Convert `ScopedValues::wrap` to that
   existing access instead of nesting another entry. Traced-owner installation
   remains family/owner specific rather than pretending an arbitrary `Value`
   destination is statically known to the collector.
3. **B.3 — Boundary verification and documentation (complete).** Keep runtime/coordinator
   ownership, external-owner operations, and access entry on the factory. Code
   crossing waits, callbacks, coordinator calls, or locks retains the factory
   and opens a later access; do not let `Deref` blur that boundary. Add focused
   tests for multi-allocation graph construction followed by same-region root
   publication, early-return garbage, and absence of nested access in
   `ScopedValues::wrap`. Reconcile the regional liveness documentation. This
   foundation does not yet make the GCI5R-001A mismatch fixture pass because
   the family cutovers remain in D-F.

Completed on 2026-09-07. `RuntimeValueAccess::root_runtime_value` is the one
access-owned containing-value publisher over the now-private
`RuntimeValueRoot::new_from_access`. `CoreValueFactory` provides infallible and
fallible synchronous construction entries; both keep partial allocations in
one admitted region, the successful path roots its complete returned graph
before leaving, and the error path publishes nothing. Existing admitted
core-net claim paths now use the same publisher. `ScopedValues::wrap` no longer
re-enters the heap.

`regional_value_publication_retains_only_the_returned_managed_graph` now
constructs all three recursive facades plus one omitted cell without temporary
family roots, then proves one outer root traces the three returned identities
while the omitted allocation is reclaimed.
`early_regional_return_leaves_partial_managed_graph_collectible` covers the
fallible exit. `scoped_wrap_publishes_without_nested_managed_access` uses a
thread-local high-water latch to force the public wrapper's maximum access
depth to one. The gateway and root-publication inventories were extended rather
than weakened. The ownership ledger and current evaluation architecture now
record regional liveness and orchestration boundaries. As planned, the ignored
GCI5R-001A mismatch remains until the C-F family cutovers.

##### GCI5R-001C — Regional managed-family constructor gateways (complete except deferred C.5)

1. Add access-taking, non-self-opening construction gateways for managed
   lazies, promises, and core nets. A gateway may return its facade to code
   already inside the admitted region, but must not open access itself or
   imply that the facade is an owner.
2. Add family-specific owner-producing variants for an immediately required
   managed root and establish the owner-specific traced-edge installation
   shape used by later cutovers. Initialize mandatory post-allocation state
   before either publication, including the failed lazy's terminal cache.
3. Reduce the three raw `allocate_managed_*` operations to the smallest module
   visibility which supports these gateways. If migration temporarily requires
   broader visibility, enumerate those exact callers and make final privacy a
   hard G checkpoint rather than claiming the boundary is already sealed.
4. Add a fail-closed source inventory which rejects new self-opening managed
   constructors while allowing only the explicitly enumerated legacy wrappers
   awaiting D-F. Do not mistake this lexical latch for the forced-order
   behavioral proof.
5. **Deferred until after D-F.** Reconsider a private family-specific or
   generic fresh-allocation wrapper only after the lazy, promise, and core-net
   cutovers reveal whether an owner-neutral construction seam remains and is
   genuinely difficult to audit. Do not add an intermediate representation
   during C: anything more than a pointer-sized, zero-overhead carrier needs a
   demonstrated safety benefit. If revisited, retain it only when every
   conversion is coupled to actual root publication or traced-edge
   installation; an unrestricted `into_gc`, `Deref`, or equivalent escape
   fails the experiment.
6. Verify direct regional construction, intentional rooted handoff,
   traced-owner installation, discard/reclamation, and representation privacy.
   Compile-fail evidence is useful only if the selected private API establishes
   a meaningful lifetime property.

Completed on 2026-09-07, with C.5 deliberately deferred. The three raw
`allocate_managed_*` methods are now private to `recursive_cells.rs`.
`RuntimeValueAccess` exposes non-self-opening facade gateways for lazy,
promise, and core-net construction, registered-owner variants for explicit
handoff, and a failed-lazy gateway which installs the terminal cache before
returning the facade. The existing production wrappers delegate to these
gateways but remain explicitly inventoried self-opening migration shims for
D-F.

The source inventory rejects raw allocator use outside the representation
module and parses every Rust function to latch the exact current set of
self-opening wrappers. Focused collector tests cover all three facades under
one traced containing-value root, all three explicit family-root handoffs,
terminal failed-lazy initialization, and reclamation of discarded partial
graphs. No intermediate fresh-allocation wrapper was introduced; the concrete
boundary therefore provides the evidence for the separate C.5 discussion
rather than prejudging it.

##### GCI5R-001D — Lazy construction cutover

This family has enough call-site breadth to remain partitioned:

1. **D.1 — Core lazy gateway (complete).** Introduce the access-taking replacements for
   every `with_source` wrapper and initialize already-terminal lazies,
   including failure values, before first publication under the same access.
   Keep the enumerated self-opening wrappers only as temporary migration
   shims.
2. **D.2 — Already-admitted and public-value paths (complete).** Convert `ScopedValues`,
   access/application helpers, and other paths which already own an outer
   access. Publish the containing root through B rather than reopening nested
   access.
3. **D.3 — Evaluator paths (complete).** Convert application, operator, annotation,
   object/effect/list/dictionary/net builtin, and machine-result construction.
   Open or extend one callback-free `with_value_access` region through result
   rooting or exact net installation; split any path which reaches a callback,
   wait, reflection activation, or contention park.
4. **D.4 — Compiler and reflection paths (complete).** Convert source/net lowering,
   per-declaration and final-definition publication, reflection-store edits,
   reflection tasks, and external-host-call construction. Do not hold managed
   access while invoking a host callback or coordinator operation.
5. **D.5 — Lazy closure (complete).** Delete the self-opening shims and update the source
   inventory. Add forced-order survival, post-publication root-retirement,
   early-return reclamation, and representative evaluator, compiler,
   reflection, and public-value tests.

D.1 completed on 2026-09-07. Every lazy source vocabulary member now has an
access-taking constructor, including semantic computations, application and
access thunks, builtins, net construction/computation, host calls, reflection
gates/results, computed fixpoints, and already-terminal failures. The failed
path initializes its cache in the allocating region. Factory-taking entry
points remain temporary, explicitly inventoried shims for D.2-D.5; the unused
generic `with_source` shim was removed.

D.2 completed on 2026-09-07. Saturated builtin, access, application, and
annotation construction through public `Values` now consumes the
`RuntimeValueAccess` already held by `ScopedValues` and publishes its outer
root before that region closes. The public `Assembler::net` callback still
runs without managed access; after it returns, net allocation and containing
value-root publication share one access region. The scoped-construction test
now latches a maximum access depth of one across nested public construction.

D.3 completed on 2026-09-07. Evaluator operations now retain only newly
constructed lazy, promise, and core-net family identities in a private
step-local publication nursery until the operation installs its result in a
cache, net, or runtime root. Directly rootable reflection-machine results use
same-region containing roots instead. The nursery crosses neither managed
access nor host callbacks, waits, coordinator operations, nor delayed
reflection activation; it is a narrow ownership handoff rather than a second
value representation. Forced collection between construction and containing
root publication covers all three managed families.

D.4 completed on 2026-09-07. Source and net lowering now receive the active
`RuntimeValueAccess` from their declaration operation, so function nets and
their lazy computations are constructed before the declaration root is
published. Deferred imports allocate their host-call lazy inside that same
compiler region but invoke the loader only after managed access has ended.
Compiler fallback values and final definitions are rooted at construction,
and reflection request/result construction uses either the evaluator nursery
or a same-region containing root.

D.5 completed on 2026-09-07. Every factory-taking lazy constructor is now
test-only or deleted; the fail-closed source inventory accepts no production
self-opening constructor. The former ignored publication-gap fixture now
forces collection after lazy, promise, and core-net construction beneath one
containing root, then proves retirement after that root drops. Existing
compiler, evaluator, reflection, and public-value suites remain the
representative behavior matrix.

##### GCI5R-001E — Promise construction cutover

This family crosses coordinator ownership and therefore remains separate from
the otherwise similar lazy conversion:

1. **E.1 — Rooted host/compiler promises.** Convert the two public resolver
   constructors and the compiler final-definitions promise. Publish the public
   containing value root and resolver/owner root before allocation access
   ends; these are intended owners, not temporary construction roots.
2. **E.2 — Producer registration.** Change task/local/coordinator registration
   to accept the already-intended `ManagedPromiseRoot`, carry it across the
   orchestration boundary, and project the facade only while that root remains
   owned. Do not hold managed access across coordinator calls or waits. Repair
   `client_demand_halt` so an unassigned-promise halt cannot outlive the root
   projected from its consumed dependency.
3. **E.3 — Semantic promise graphs (complete).** Convert list-effect fixpoints,
   reflection fixpoints, and other evaluator graphs which embed promises as
   traced values rather than producer-owned handles. Establish the containing
   value root or traced owner before their construction region ends.
4. **E.4 — Promise closure (complete).** Delete self-opening `with_cell`/`new` paths which
   can escape and update the source inventory. Force collection between
   construction and registration/publication, and preserve cancellation,
   abandonment, assignment, resolver drop, and root-retirement coverage.

E.3 completed on 2026-09-07. List-effect and computed-fixpoint promises are
now admitted through the evaluator-step publication nursery and installed in
their containing semantic graph before those guards retire. Producer-owned
promises remain on the explicit rooted registration path established by E.2.

E.4 completed on 2026-09-07. The self-opening promise helpers are test-only;
production construction either publishes a resolver/consumer root directly
or remains in an evaluator/compiler construction region until a traced owner
exists. Forced collection covers public resolver assignment and retirement,
the containing-graph path, and evaluator-step publication. An unassigned
promise halt retains its exact root indirectly: boxing this uncommon payload
keeps ordinary recursive evaluator result frames at 64 bytes and repairs the
deterministic stack regression exposed by the dictionary-pattern suite.

##### GCI5R-001F — Core-net construction cutover

1. **F.1 — Local and containing-value construction (complete).** Convert standalone,
   related, source-lowered, function-stage, and public `Assembler::net`
   instantiation to the C gateway. Install the fresh net directly into its
   containing traced value/net owner when construction is local.
2. **F.2 — Genuine net handoffs (complete).** Use an intentional managed-net or
   containing value root only where normalization, copy-source, callable, or
   frontier state must leave access. Generalize the ownership pattern already
   demonstrated by `CorePreparedCopySource` without making prepared roots the
   default constructor result.
3. **F.3 — Net closure (complete).** Delete factory methods which open an allocation
   region and return a bare `CoreRuntimeNet`; update the source inventory and
   cover standalone/related instantiation, function/net wrapping, copy-source
   handoff, and collection immediately after permanent publication.

F.1-F.3 completed on 2026-09-07. Public net construction, function stages,
source lowering, evaluator attachments, and reflection request functions now
allocate through an already-active access and install the net in their
containing value before it ends. The remaining durable handoffs are explicit:
normalization requests, frontier observations, and prepared copy sources hold
`ManagedCoreNetRoot`; ordinary net values do not. The self-opening factory
constructor is test-only, the unused related-net wrapper is gone, and a
forced-collection fixture proves that a prepared copy source alone retains
and then retires its source net.

##### GCI5R-001G — Closure audit and review reconciliation

G remains partitioned so the lexical closure latch, implementation surface,
documentation, and behavioral evidence cannot be mistaken for one another:

| Checkpoint | Status | Purpose |
| --- | --- | --- |
| G.1 | complete | decide the deferred C.5 fresh-typestate question from the completed D-F call sites |
| G.2a | complete | replace the boolean source heuristic with an exact regional-constructor caller inventory and classifier tests |
| G.2b | complete | delete or narrow obsolete constructor seams and seal the final production visibility surface |
| G.3 | complete | reconcile current regional-liveness and owner-handoff documentation |
| G.4a | complete | run focused publication, reclamation, evaluator, promise, and net evidence |
| G.4b | complete | run the repository routine checks and any change-triggered unsafe-boundary tools |
| G.5 | complete | close the finding and update downstream integration gates only after G.4 passes |

###### G.1 — Post-cutover C.5 assessment and recommendation

The completed D-F call sites do not reveal a useful local role for the
previously considered fresh-allocation typestate. They divide into four
concrete ownership shapes:

1. A callback-free construction region builds an ordinary semantic graph and
   `construct_runtime_value_root` or `ScopedValues::wrap` roots that complete
   graph before managed access ends.
2. Evaluator construction acquires a family root before leaving its small
   access region, retains that root in the step-local publication nursery, and
   retires it only after the result has entered its cache, net, or runtime
   root.
3. A genuine orchestration handoff carries `ManagedPromiseRoot` or
   `ManagedCoreNetRoot`; the receiving coordinator, resolver, normalization,
   frontier, or copy-source record is the intended durable owner.
4. Same-region construction passes an owner-neutral facade through ordinary
   `Value`, list, dictionary, function, and net builders before the complete
   unpublished graph is installed beneath its eventual traced owner.

A `Fresh<'scope, ManagedEdge>`-like carrier would constrain only the first
step of case 4. Those builders need the normal semantic facade before its
outer owner exists. An unrestricted facade projection would merely recreate
the allocation-to-owner gap one line later; preventing that projection would
require propagating the fresh lifetime through `Value` and every recursive
container or introducing parallel fresh-aware builders. That is a broad
semantic-representation transition, not a pointer-sized local guard. A
lifetime tag on managed read pointers is also redundant: `RuntimeValueAccess`
and the three `Managed*Access` types already carry the access lifetime and are
thread-bound.

**Recommendation:** do not introduce a fresh-allocation wrapper for the three
I5 families. Keep the access-taking owner-neutral gateways as explicitly
regional construction primitives, reduce their visibility where G.2 can do so
without replacing them with equivalent forwarding APIs, and rely on three
complementary closure checks:

- raw allocation and cell/edge representations remain private to
  `recursive_cells`;
- the source inventory rejects production constructors which open their own
  managed region and return a fresh facade or containing structure, while
  separately enumerating the small regional gateway surface; and
- forced collection at the former allocation/publication boundary proves that
  each production handoff establishes its intended root or traced owner before
  collection can intervene.

Reconsider private fresh typestate for a later family only if that family
introduces a necessary generic handoff whose intended owner cannot be kept in
one regional operation. It remains acceptable only if every consuming
operation simultaneously publishes a real root or installs the edge beneath
a traced owner and there is no unrestricted facade/`Gc` conversion. The
[I6+ Regional Allocation Migration Rule](../plans/GarbageCollectorIntegration_2026-08-19.md#i6-regional-allocation-migration-rule)
continues to make that decision explicit at each new family.

###### G.2 — Constructor-surface closure

G.2 is source and API closure, not another representation migration.

**G.2a — Exact regional caller inventory.** Replace
`ConstructorCallInventory`'s current two booleans with a source-backed record of
the exact production functions which call an owner-neutral constructor, call
an in-region forwarding constructor, or open a managed access region around
either operation. The access-entry vocabulary must include
`with_runtime_value_access`, public-facade `Values::with_access`, and
evaluator-step `with_value_access`; the constructor vocabulary must remain an
explicit fail-closed set rather than a suffix match. Classify every admitted
caller as one of:

- an in-region forwarding helper which itself accepts `RuntimeValueAccess`;
- a containing-root region which publishes through
  `construct_runtime_value_root`, `try_construct_runtime_value_root`, or
  `ScopedValues::wrap`;
- an evaluator publication-nursery handoff which acquires its family root
  before the small access region ends; or
- an explicit family-root constructor for a genuine orchestration handoff.

The inventory is deliberately an exact change detector, not an attempted Rust
dataflow proof. Add synthetic parser/visitor fixtures which demonstrate that a
new self-opening bare-facade return and a containing-structure return are
reported before updating the production allowlist. A new caller must therefore
fail the test and receive an ownership classification during review.

**G.2b — Visibility and forwarding closure.** Starting from the G.2a record:

- keep `allocate_managed_{lazy,promise,core_net}` and the concrete cell/edge
  representations private to `recursive_cells`;
- delete an obsolete self-opening or owner-neutral forwarding helper where it
  has no remaining caller;
- otherwise narrow visibility only when doing so does not replace the helper
  with an equivalent forwarding API in another module;
- retain test-only self-opening fixtures only when they exercise a distinct
  lifecycle contract, and keep them excluded explicitly rather than by an
  accidental parser blind spot; and
- do not add fresh typestate, temporary roots, or a generic edge-install API as
  incidental closure work.

If the inventory proves that the present access-taking gateways are already
the narrowest useful production seam, G.2b may close with no representation
change. Its deliverable is the sealed surface and fail-closed regression latch,
not a quota of deleted methods.

Installing into an already published traced owner remains governed by the
structural mutation gateway selected in GCI5R-002. Installation into a new,
unpublished parent is construction and must not be routed through a fake
post-publication mutation solely to make the inventories look uniform.

Completed on 2026-09-08. The source-backed inventory now records 55 exact
production constructor-bearing functions and classifies them as 14 in-region
forwarders, 16 containing-root regions, 22 evaluator publication-nursery
handoffs, and 3 explicit family-root constructors. Synthetic fixtures prove
that both a new self-opening bare-facade return and a containing-structure
return are reported before the reviewed list is changed. The inventory is a
lexical change detector over the explicit access and constructor vocabulary,
not a substitute for Rust dataflow analysis or the behavioral chronology
tests.

Raw allocation remains private. Lazy owner-neutral construction and rooting
are now visible only within `crate::core`, and the otherwise unused rooted-lazy
constructor is test-only. Promise and core-net construction/root handoffs keep
their crate-wide visibility because current compiler, evaluator, reflection,
API, and net orchestration callers genuinely cross those module boundaries;
moving equivalent forwarding methods would not narrow the authority.

###### G.3 — Ownership-contract reconciliation

After G.2 fixes the final surface, reconcile one authoritative description of
regional liveness and link the adjacent layers to it:

- update `CoreValueAllocationScope`, `RuntimeValueAccess`, family gateway, and
  evaluator publication-nursery comments to distinguish access-region
  liveness from durable ownership;
- keep the collector's `Allocator::alloc` contract general: allocation returns
  a non-rooting pointer protected by the current mutator, while Glam supplies
  its stricter graph-publication policy above that boundary;
- update `docs/architecture/evaluation.md` with current control flow, and the
  ownership ledger with the authoritative owner classes; and
- update this review's GCI5R-001A mismatch-fixture account to distinguish its
  historical failing chronology from the post-cutover same-region test;
- remove completed migration wording from the integration plan without
  copying review chronology into current architecture documentation.

`docs/AgentContext.md` and `src/README.md` need changes only if G.2 changes a
cross-layer rule or module responsibility. Do not churn them merely to repeat
the ownership ledger.

Completed on 2026-09-08. `CoreValueAllocationScope` and
`RuntimeValueAccess` now state the distinction between temporary admission
liveness and durable ownership; the family gateways state whether they return
an owner-neutral facade or an explicit registered owner; and the evaluator
publication nursery documents its exact temporary-owner role. The current
evaluation architecture and ownership ledger record the four handoff forms.
The collector's `Allocator::alloc` contract remains general and unchanged.
G.2 did not alter a cross-layer module responsibility, so `AgentContext.md` and
`src/README.md` were intentionally left alone.

###### G.4 — Verification

**G.4a — Focused evidence.** Run the constructor-privacy and exact-caller
inventories together with the regional containing-root, early-return,
former-gap replacement, evaluator publication-nursery, promise
registration/retirement, and prepared-net handoff fixtures. Audit the tests
before adding another: each of the four G.1 ownership shapes needs one
deterministic example, but duplicate permutations add no proof.

The historical gap now has two distinct replacement proofs:

- When construction and containing-root publication share one access region,
  attempt collection after the fresh family allocations but before the closure
  returns and assert that active mutator admission excludes it. Then collect
  after the region closes and prove that the newly published containing root
  retains exactly the returned graph.
  `fresh_managed_facades_survive_until_first_publication` now covers both
  halves explicitly.
- When an orchestration boundary genuinely requires separate access regions,
  retain the already-intended family root and force a successful collection in
  that interval. The evaluator publication-nursery, promise registration, and
  prepared-net tests cover representative forms; add a fixture only if their
  forced chronology does not match the final call site.

A passing test which merely inherits `CollectionPolicy::NoAuto` is not
evidence. Same-thread `CollectionError::ActiveMutator` plus post-publication
collection is sufficient for the regional case because the collector's
mutator/exclusive-admission protocol is verified independently. Any claim
about a concurrent handoff must instead use deterministic barriers or an
existing forced-order fixture; no schedule-sensitive conclusion may rely on
uncontrolled repetition.

**G.4b — Broad and unsafe-boundary evidence.** Run the repository routine
checks after the focused suite. Run targeted Miri only if G.2 changes collector
unsafe code, root construction, managed dereference, or access-lifetime
machinery. Visibility, source-inventory, and documentation-only closure does
not justify an unrelated Miri run; the integration verification matrix and
Gate G3 retain the complete focused Miri/sanitizer obligation.

Completed on 2026-09-08. The focused commands for
`regional_constructor_`, `recursive_cell_gateways_are_private_and_complete`,
`fresh_managed_facades_survive_until_first_publication`,
`regional_value_publication_retains_only_the_returned_managed_graph`,
`early_regional_return_leaves_partial_managed_graph_collectible`,
`evaluator_step_guards_fresh_managed_results_until_containing_root_publication`,
`promise_settlement_releases_task_and_local_owner_roots`,
`prepared_copy_source_is_an_exact_temporary_net_owner`, and
`promise_resolver_drop_invokes_idempotent_retire_once` all pass. The first
broad run found that the older raw-text access inventory counted the two Rust
snippets embedded in the new synthetic parser fixture. The fixture now
assembles the access identifier at runtime, both inventories pass without
changing the production count, and the final runs pass:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
cargo test --workspace -q
```

The final library run passes 1,383 tests with none ignored; the workspace run
also passes the `glam-gc` suites (189 passing and 2 explicitly ignored in its
main library group). G.2 changed no unsafe collector code, root implementation,
managed dereference, or access-lifetime mechanism, so it did not trigger an
ad-hoc Miri run. Gate G3 retains that broader obligation.

###### G.5 — Finding and roadmap closure

Only after G.4 passes:

1. record exact commands and results here, change GCI5R-001 to closed, and
   update this review's summary while leaving GCI5R-002 independently open;
2. reconcile the integration plan's I6+ regional-allocation entry condition
   with the completed I5 precedent, without weakening the rule for new
   families;
3. update Gate G2/I11 certification language to consume the closed temporal
   publication evidence; and
4. remove the temporary GCI5R-001 block from I11C and I12 while preserving
   their independent Gate G2, GCI5R-002, readiness, and policy prerequisites.

G.5 is administrative reconciliation, not permission to enable automatic
collection. Production remains `NoAuto` until the later integration gates
explicitly select another policy for newly constructed runtimes.

Completed on 2026-09-08. GCI5R-001 is closed. The integration plan now treats
the I5 repair and exact temporal evidence as the precedent consumed by I6+
family migrations and Gate G2 rather than as an outstanding I11C/I12 blocker.
GCI5R-002, whole-graph closure, readiness, and collection-policy gates remain
independently open; this closeout does not enable production collection.

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

#### GCI5R-002 remediation plan

This remediation establishes a structural mutation boundary without selecting
the eventual concurrent collector. Its target invariant is:

> Every post-publication change to a managed object's outgoing graph passes
> through an owner-qualified edge-transition gateway. The call site supplies
> complete, lazily traversed leaving and adding edge sets, and the active
> collector policy decides which sets it must observe before the write becomes
> visible.

Initialization of a new unpublished object remains governed by GCI5R-001's
regional construction rule and is not a post-publication mutation. The
transition API must accept distinct synchronous edge visitors for the two sets
and must not eagerly walk either one merely because the gateway was called.
Ordinary `Trace` values can supply those visitors directly; the temporary
compatibility representations can adapt their existing exact borrowed walks
without cloning a semantic graph or pretending those borrowed views are
managed allocations. The current stop-the-world collector observes neither
set. A future snapshot-at-the-beginning collector may mark allocations born
during concurrent marking and observe only leaving edges; another incremental
or generational policy may also use the owner and adding edges.
Moving collection may later require a rewrite-capable visitor, but routing all
writes through this gateway is its prerequisite rather than a claim that the
initial trace-only API completes moving support.

The transition describes the graph conservatively. A policy may inspect a
proposed adding set before a one-write publication discovers that it lost; the
extra marking or remembered-set work is harmless. Leaving edges need be
reported only for a mutation which can actually remove them. Representation
gateways should retain their existing winner, lock, and publication ordering
rather than moving synchronization into the collector.

##### GCI5R-002A — Collector edge-transition contract

- Generalize the single-edge replacement operation to an owner-qualified
  transition over arbitrary synchronous leaving and adding edge visitors. The
  visitors may borrow representation state for the duration of the call and
  must report exact managed edges without evaluation or retention. Keep a
  `Trace` adapter and singleton-edge convenience operation only if both
  delegate to the same contract.
- Couple the representation-specific write to the transition operation; a
  detached notification before or after an unrelated write is not sufficient.
- Make edge traversal policy-selected and lazy, so the STW implementation pays
  only the gateway call cost and SATB need not traverse adding values.
- Validate owner provenance at the gateway. Edge provenance remains part of
  the visitor contract and is validated when a selected policy actually walks
  that side; the STW no-op must not traverse a set merely to validate it. Add a
  test-only observer which records which transition sides a selected policy
  requests. Do not expose collector phase state to Glam code.

Verification: focused collector tests cover empty, singleton, and multi-edge
sets; distinct leaving/adding adapter types; no traversal under the current STW
policy; leaving-only traversal under a synthetic SATB-like observer; and
conservative observation of a losing proposed addition.

##### GCI5R-002B — Runtime value-access gateway

- Add the stable Glam-facing transition operation to `RuntimeValueAccess`.
  Production representations supply their managed owner and semantic edge-set
  adapters through this operation without receiving the raw collector mutator.
- Reuse the exact compatibility edge visitors while `Value`, failures, lists,
  dictionaries, and net payload shells remain Rust-owned. Do not force,
  evaluate, format, or clone a complete semantic graph merely to describe a
  transition.
- Preserve a direct singleton path for later managed list/dictionary/net nodes,
  where most edits should become one or a few `Gc` edge changes after the
  compatibility representations are retired.

Verification: a scoped-access fixture proves that a transition rejects a
foreign owner, cannot outlive its `RuntimeValueAccess`, and invokes no semantic
service while visiting compatibility edges.

##### GCI5R-002C — Lazy and promise production writers

- Route `ManagedLazyAccess::cache` through the transition gateway. Its logical
  transition adds the terminal result graph and removes the deferred source
  graph while preserving terminal publication before source removal. Under
  SATB only the source removal requires traversal.
- Route `ManagedPromiseAccess::{publish_detached,publish_guarded}` through the
  same gateway. Promise settlement has an empty leaving set and adds the
  assignment graph; losing publishers must not alter semantic state, although
  a future policy may conservatively inspect their proposed addition.
- Keep notification, retired-root destruction, and other external lifecycle
  work after the existing representation/coordinator locks and runtime mutation
  admission. The collector gateway changes edge accounting, not wake or
  retirement order.
- Replace the current lexical writer-count claim with behavioral instrumentation
  proving that every winning production path enters the owner-qualified
  gateway and supplies the expected edge-set direction.

Verification: cover lazy success and failure, source release, detached and
guarded promise success/failure, a forced winning/losing publisher ordering,
and unchanged terminal-before-source/root-retirement chronology. Re-run the
existing lazy/promise reclamation and coordinator publication suites.

##### GCI5R-002D — Core-net delta integration in I8

- Keep core-net transition reporting under the existing `RuntimeNetCell`
  synchronization boundary. No collector callback may acquire that mutex or
  reconstruct state after the write.
- Inventory the concrete topology and payload edits, then report the affected
  leaving/adding semantic edges. A whole-net before/after trace is acceptable
  only as a measured correctness bridge; it must not silently become the
  permanent high-volume path.
- Align the final representation with the expectation that future managed net
  nodes usually mutate singular edges. Preserve current revision,
  materialization, disturbance, and active-pair publication behavior.

This checkpoint is implemented with the I8 post-cutover net audit. GCI5R-002
remains open after A-C and continues to block Gate G2, I11 production
collection, and concurrent/incremental/generational/moving claims until D is
complete.

##### GCI5R-002E — Closure and verification

- Replace source-count evidence with an exact writer/gateway inventory plus
  behavioral transition probes for lazy, promise, and net mutation.
- Reconcile the ownership/mutation ledger, collector contract, evaluation
  architecture, I8 plan, and concurrent-collector plan. Record explicitly that
  allocations born marked during concurrent marking belong to the later SATB
  phase/epoch protocol, not to this STW remediation.
- Run focused mutation, reclamation, forced-order publication, net, and
  collector-policy tests, followed by the repository routine checks. Add Miri
  coverage if implementation changes unsafe tracing, managed dereference, or
  mutation/access lifetimes.
- Close GCI5R-002 only after the production inventory has no unmatched writer
  and all three managed families demonstrably enter the transition gateway.

No new mutable managed family may be declared complete after 002A without
using this gateway. Production remains `CollectionPolicy::NoAuto`; completing
this remediation supplies barrier structure but does not enable collection.

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
5. apply GCI5R-001's closed regional-allocation and first-owner protocol to
   every new construction handoff.

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
- **I11/Gate G2:** consume GCI5R-001's exact temporal root-publication
  inventory and forced-order evidence. A declaration-only root inventory does
  not prove that an allocation remains live between allocation, handoff, and
  first publication; every I6+ family must add the same evidence before Gate
  G2 can close.

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
| I12 | Automatic entry can collect at the most dangerous handoff boundary. | Consume GCI5R-001's closed evidence as a hard policy-review prerequisite and inventory second-entry constructors. |
| I13 | Redundant compatibility/provenance cleanup remains appropriate. | Do not defer liveness or mutation safety here; add any accepted ID/label cache to its cleanup ledger. |

## Recommended Resolution Order

1. **Completed:** GCI5R-001's regional construction and first-publication
   protocol now closes the only finding which could make a currently valid
   `Gc` stale before later access once production collection is enabled.
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
