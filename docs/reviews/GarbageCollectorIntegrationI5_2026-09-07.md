# Glam GC Integration Phase I5 Review — 2026-09-07

Baseline: `37186c1`, including completed implementation checkpoints I5A-I5F.4.

Status: review complete; GCI5R-001 through GCI5R-004 and GCI5R-008 are closed.
The implemented I5
graph is sound under the current `CollectionPolicy::NoAuto` boundary and the
closed isolated collection fixtures provide strong evidence for recursive
cycle reclamation. Regional construction establishes an exact traced or rooted
owner before managed access ends, and every current mutable managed family now
enters an owner-qualified collector transition. Production still does not
collect; I8 retains the explicit performance work of replacing the core net's
whole-state correctness bridge with exact high-volume edit deltas.

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
  id
  assignment: OnceLock<Result<Value, EvaluationFailure>>
  edge-free completion registrations
  root-free immutable producer route

ManagedCoreNetCell
  synchronized RuntimeNetCell<CoreSpecialization>
```

`LazyValue` and `PromisedValue` now carry only private `Gc` edges. Their durable
parked or external owners carry `ManagedLazyRoot` or `ManagedPromiseRoot`, and
the public `PromiseResolver` is the sole promise-specific weak runtime owner.
`CoreRuntimeNet` still carries its private edge plus weak value-domain
observation because stored source identities and facade construction currently
share that qualification path. Durable core-net owners carry
`ManagedCoreNetRoot`; bounded evaluation and net operations derive
non-escaping access views from one matching `RuntimeValueAccess`. GCI5R-003G
assigns removal of the remaining net observer to I8A.0.

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

The focused `cargo test -q core::managed` run passed 72 tests at the original
review baseline. The live review
also passes `cargo fmt --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, and
`cargo test -q` (1,374 library tests plus every integration suite). No new
schedule-sensitive claim in this review relies on repeated execution.

GCI5R-001's closure adds both an exact classified constructor-bearing source
inventory and deterministic root-before-region-exit chronology. GCI5R-002's
closure replaces the writer-count latch with deterministic collector-policy
probes over the actual lazy, promise, and net writer boundaries.

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
   update this review's summary while leaving GCI5R-002 independently open at
   that checkpoint (GCI5R-002 was subsequently closed on 2026-09-09);
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
**Status:** closed 2026-09-09 by GCI5R-002A-E; current STW execution remains
`NoAuto`, and I8 retains the net-delta performance conversion required before
activating a concurrent or incremental policy

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

**Status:** completed 2026-09-09.

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

**Status:** completed 2026-09-09.

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

**Status:** completed 2026-09-09.

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

##### GCI5R-002D — Core-net synchronized correctness bridge

**Status:** completed 2026-09-09; exact high-volume deltas remain an explicit
I8 audit/conversion rather than a prerequisite for closing the missing
structural boundary.

- Keep core-net transition reporting under the existing `RuntimeNetCell`
  synchronization boundary. No collector callback may acquire that mutex or
  reconstruct state after the write.
- Inventory the concrete topology and payload edits and route all of their
  synchronization paths through one gate. Use a lazily selected whole-net
  before/after visitor as the measured correctness bridge; I8 then reports the
  affected leaving/adding deltas before the gate becomes a live high-volume
  concurrent barrier.
- Align the final representation with the expectation that future managed net
  nodes usually mutate singular edges. Preserve current revision,
  materialization, disturbance, and active-pair publication behavior.

GCI5R-002D installs one generic `RuntimeNetMutationGateway` beneath every
managed net mutation while retaining the existing net mutex and revision/
disturbance chronology. Its collector-selected borrowed-state visitor sees the
whole net immediately before and after a coupled write without cloning the
semantic graph or reacquiring the mutex. The current STW policy visits neither
side. I8 must replace this bridge with exact per-edit deltas before a concurrent
policy activates it on this high-volume path.

##### GCI5R-002E — Closure and verification

**Status:** completed 2026-09-09.

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

Resolution inventory:

| Managed owner | Authoritative writers | Collector transition | Behavioral evidence |
| --- | --- | --- | --- |
| `ManagedLazyCell` | `ManagedLazyAccess::cache` | One owner-qualified transition visits the source graph as leaving and the terminal success/failure graph as adding; the existing closure still publishes result before taking source. | Success, failure, source release, and non-forcing visitors. |
| `ManagedPromiseCell` | `publish_detached` and `publish_guarded`, both through `publish_assignment` | Empty leaving set and proposed assignment adding set. One-write losers preserve the winner even when an adding-policy probe already observed their valid proposal. | Detached/guarded success and failure, forced winner/loser order, assignment-before-wake callback. |
| `ManagedCoreNetCell` | Managed `CoreRuntimeNetAccess` direct/conditional edits; active-pair and cursor steps; cursor finish/drop restoration | `ManagedCoreNetAccess` implements `RuntimeNetMutationGateway`; every path enters a borrowed-state pre/post transition while the one semantic mutex is held. Generic non-core tests use a separate direct gateway. | Exact pre/post payload replacement, real erase reduction, existing cursor/active-pair/runtime-net suites. |

The net state visitor is deliberately conservative: it reports the complete
valid pre- and post-write graphs, including unchanged edges. That is correct
barrier input and avoids semantic graph snapshots, but it is not the permanent
performance shape. I8 owns the source-backed mutation inventory again after
I6/I7 payload changes and replaces this bridge with exact edit deltas before
concurrent collection. Allocation during concurrent marking remains a separate
CG2 epoch/birth rule; GCI5R-002 does not silently mark newly allocated objects
or enable collection.

No new mutable managed family may be declared complete after 002A without
using this gateway. Production remains `CollectionPolicy::NoAuto`; completing
this remediation supplies barrier structure but does not enable collection.

Closure verification on 2026-09-09 covered the collector gateway directly
(`6` focused mutation tests), all managed recursive-cell paths (`39` tests),
the core-net facade (`17` tests), and the generic interaction-net runtime
(`71` tests). The complete `glam-gc` check passed `193` unit tests, `7` Loom
tests, `8` documentation tests, Clippy, formatting, and the unsafe-site audit.
Strict-provenance Miri passed the six collector mutation tests and the targeted
lazy, promise-publication, and core-net state-transition fixtures. The final
repository checks passed formatting, all-target/all-feature Clippy with
warnings denied, and the complete test suite. Repetition was not used as race
evidence: the promise winner/loser and callback ordering cases use explicit
deterministic probes, while the collector's concurrent protocols retain their
Loom coverage.

### GCI5R-003 — Lazy and promise façades duplicate cell identity data

**Classification:** undocumented representation drift  
**Priority:** medium  
**Confidence:** high  
**Status:** closed 2026-09-09

The I5.0 disposition said IDs and labels remain cell-resident and are copied
only into diagnostics or explicit indexes. The implementation stored both
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

#### Accepted target

`LazyValue` and `PromisedValue` become edge-only semantic façades. The managed
cell is the canonical owner of identity metadata needed by semantic
evaluation. The lazy cell retains its ID and diagnostic label; promise
evaluation needs only the cell's ID after the public resolver takes ownership
of its host-facing label. An ID or label is copied only into a concrete
scheduler index, diagnostic record, or external capability which demonstrably
needs to use it without managed access.

A weak runtime observer is likewise not a property of an interior semantic
edge. Managed reads require an explicit `RuntimeValueAccess` (or the narrower
evaluator-step access derived from it), and managed writes use the durable root
owned by the producer or task. This restores the rule that an unrooted
`Gc<T>` is usable only within a matching mutator region instead of allowing
each façade to reopen that region for itself.

`PromiseResolver` is the deliberate exception. It is a public, affine host
capability which may outlive the `EvaluationRuntime` façade, must not retain
the runtime, and must still resolve, fail, or poison its promise from `Drop`.
Its target representation is approximately:

```rust
struct PromiseResolver {
    observer: RuntimeValueObserver,
    label: Arc<str>,
    promise: Option<ManagedPromiseRoot>,
}
```

The observer supplies runtime re-entry and runtime identity, so a second
copied runtime field should not be necessary. The `Option` remains the affine
disarm state required because Rust still invokes `Drop` after an explicitly
consuming method. `EvaluatedValue` is another legitimate public weak-observer
owner and is outside this finding.

The present observer on `ManagedPromiseCell` is transitional rather than
canonical. Promise terminal publication already proceeds through a durable
promise/wait root, so the cell should not retain a weak observer merely to
repeat the same-domain check or expose a test-only runtime ID.

#### Remediation checkpoints

##### GCI5R-003A — Inventory and proof baseline

**Completed:** 2026-09-09

- Build compile-exhaustive construction and use inventories for
  `ManagedLazyCell`, `ManagedPromiseCell`, `LazyValue`, `PromisedValue`, their
  registered roots, and `PromiseResolver`.
- Classify every copied ID, label, edge, and observer as canonical cell
  metadata, semantic edge, scheduler index, diagnostic record, or runtime
  re-entry authority. An unclassified copy blocks removal.
- Record the current façade layouts and the access-free façade operations
  which upgrade an observer and enter a mutator internally. On the current
  64-bit target both façades are 48 bytes; the managed edge itself is one
  pointer.
- Identify every coordinator, wait, callback, and wake-delivery boundary near
  those operations, together with every unsafe dereference of the bare managed
  edge. This is the baseline for proving that no mutator escapes into host
  orchestration.
- Latch the inventory and current observable behavior with source/shape tests
  where useful. Repeated concurrent execution is not race evidence.

The completed inventory found the following retained roles:

| Location | Retained state | Disposition |
| --- | --- | --- |
| `ManagedLazyCell` | canonical lazy ID and label, source, terminal result | Keep; this is the managed identity. |
| `LazyValue` | managed edge | Final after 003F. |
| `ManagedLazyRoot` | lazy ID, cycle-diagnostic label, registered root | Final: scheduler/cycle reporting consumes the copies without managed access; GCI5R-008 removed its cached edge and 003F removed its observer. |
| `ManagedPromiseCell` | promise ID, assignment, completion state, producer route | Final: 003C removed the observer and 003F removed the otherwise unread label. |
| `PromisedValue` | managed edge | Final after 003F. |
| `ManagedPromiseRoot` | promise ID, registered root, terminal/completion/producer sidecars | Final: the ID indexes work and the edge-free sidecars are inspected under coordinator locks; no label, observer, or cached edge remains. |
| `PromiseResolver` | weak observer, host diagnostic label, affine promise root | Final exception after 003F. |

The access-free façade methods are lazy `root`, `source_snapshot`, `cached`,
and `cache`, plus promise `root`, `task`, assignment/subscription inspection,
and terminal publication. They all reopen a mutator through the façade's weak
observer. The evaluator already has `EvaluatorStepContext::with_value_access`
and managed access types, while registered roots already provide liveness;
003D can therefore migrate observations without inventing another authority.
Mutation remains access-free until 003E by design.

The current 64-bit 48-byte façade layouts are now compile-time latches beside
their declarations. Existing regional-construction, recursive-identity,
completion-order, and mutation-gateway inventories cover construction and
publication boundaries. The migration must continue to root values before
coordinator registration and release access before coordinator locks, waits,
wake delivery, host callbacks, or task activation.

##### GCI5R-003B — Remove duplicated façade metadata

**Completed:** 2026-09-09

- Remove `id` and `label` from `LazyValue` and `PromisedValue`, temporarily
  retaining the edge and weak observer so this checkpoint changes identity
  storage without also changing access authority.
- Define internal façade equality by managed-edge identity. Same-runtime
  provenance is a construction invariant; foreign-runtime values continue to
  be rejected at the public runtime/value boundary.
- Make Rust `Debug` output intentionally opaque. Diagnostics obtain IDs and
  labels through managed access or from durable scheduler/diagnostic records.
- Preserve the existing user-visible lazy-cycle and promise diagnostics,
  including their labels and stable IDs.
- Verify same-allocation equality, distinct-allocation inequality,
  cross-runtime rejection, cycle diagnostics, resolver diagnostics, and the
  expected 64-bit façade reduction from 48 to 24 bytes at this intermediate
  checkpoint.

The implementation now stores only the managed edge and transitional weak
observer in each façade. Equality follows exact managed-edge identity, Rust
`Debug` is opaque, and IDs and labels are read from the canonical cell under
the still-temporary self-opened access region. The target-specific compile-time
layout latches record the intermediate 24-byte representations. Existing root
records continue to preserve the scheduler and diagnostic fields needed by
callers which already own durable roots.

##### GCI5R-003C — Remove the promise cell's observer

**Completed:** 2026-09-09

- Remove `ManagedPromiseCell`'s `RuntimeValueObserver`. Completion, waiting,
  and terminal publication must use the already durable promise/wait root or
  an explicit access region.
- Remove the observer-backed, test-only runtime accessor. Tests which need to
  prove provenance should do so through the runtime access boundary.
- During the transition, validate an edge before dereference through the
  façade or root authority already in scope; GCI5R-003D and GCI5R-003F replace
  that temporary proof with the final structural one.
- Refresh the managed-cell layout ledger from the compiler rather than
  freezing an estimated size, and verify terminal publication, failed-runtime
  behavior, and promise cleanup unchanged.

`ManagedPromiseCell` no longer retains a `RuntimeValueObserver`. Its bounded
access borrows provenance from the façade or registered root during this
transition, while the cell's own access object reports the runtime of its
explicit authority. A source-structure latch prevents the observer from
returning unnoticed. The measured 64-bit cell layout fell from 192 to 176
bytes and the ownership ledger now records that result.

##### GCI5R-003D — Require scoped authority for observation

**Completed:** 2026-09-09

- Change lazy and promise reads to accept `ManagedLazyAccess` or
  `ManagedPromiseAccess` obtained from `RuntimeValueAccess` or the existing
  evaluator-step access. Remove self-upgrading read helpers from semantic
  façades.
- Reuse an already-open evaluator access region for batches of reads rather
  than nesting a mutator entry for each field observation.
- Enable the rooted lazy-access path in production. Prepare any root required
  by task registration or `EvaluationHalt` before entering coordinator
  orchestration.
- Audit the resulting source and lifetime inventories. No managed access may
  remain live across a wait, user callback, coordinator operation, wake
  delivery, or other host work.
- Verify ordinary evaluation, lazy and promise cycles, access-region reuse,
  and dead-runtime behavior with deterministic fixtures.

Production lazy and promise observation now occurs through
`EvaluationValueAccess::{lazy,lazy_root,promise,promise_root}` or an equivalent
bounded `RuntimeValueAccess`. The evaluator reuses its existing step region,
and `EvalContext::{lazy_task,promise_task}` roots the producer before entering
coordinator admission and hands a clone of that root directly to the new
machine. `EvaluationHalt` exposes its already-retained promise root to spark
and client-demand blocking paths instead of rerooting the semantic façade.

Coordinator dependency traversal revealed a stricter case: promise producer
and completion bookkeeping can be inspected while the coordinator state is
locked, where opening a managed region would invert the intended boundary.
Those edge-free components are therefore shared by the cell and its registered
roots: an `Arc<OnceLock<Arc<PromiseProducerObligation>>>`, an
`Arc<CompletionSubscriptions>`, and an acquire/release terminal bit. Semantic
assignment data remains solely in `ManagedPromiseCell` and is read only under
managed access. Subscription still performs subscribe-and-recheck against the
terminal bit, while assignment publication stores the semantic result before
publishing that bit. The resulting 64-bit cell layout is 120 bytes.

Access-free production read methods have been removed from both semantic
façades. Their remaining self-opened operations are mutation and publication
paths assigned to 003E. `cfg(test)` compatibility probes remain while the
façade observer itself exists; the bounded gateway tests exercise the intended
production access shape, and 003F removes those probes with the observer.

The migration also exposed five spark fixtures which constructed lazies in
the process-global test domain and evaluated them in another runtime. They now
allocate through the tested context's value domain, making the existing
same-runtime contract explicit rather than preserving an invalid fixture.

##### GCI5R-003E — Move mutation authority to durable roots

**Completed:** 2026-09-09

- Publish lazy cache results through the task-held `ManagedLazyRoot`, and
  promise results through the producer- or resolver-held
  `ManagedPromiseRoot`. Remove mutation APIs from `LazyValue` and
  `PromisedValue`.
- Make `PromiseResolver` upgrade its weak observer, enter one bounded access
  region, publish through its root, and release managed access before wakes or
  callbacks are delivered.
- Preserve the GCI5R-002 mutation-gateway contract and existing chronology:
  lazy result publication precedes source release; promise assignment is
  visible before wake delivery and root retirement; exactly one competing
  publisher wins.
- Force both relevant orderings with barriers or deterministic probes. Do not
  accept a test merely because it passes under repetition.

Lazy and promise mutation is now owned by registered roots. Evaluator tasks,
the coordinator, direct-runner owners, reflection continuations, list-effect
fixpoints, and module sealing retain or construct the intended
`ManagedLazyRoot` or `ManagedPromiseRoot` before publication. The semantic
`LazyValue` and `PromisedValue` façades no longer expose cache, assignment, or
failure operations. Their managed access objects retain private transition
primitives only as the representation-local implementation behind the public
crate root gateways.

`PromiseResolver` publishes through its affine promise root. It first upgrades
the resolver's weak value-domain observer so retirement still reports the
specific host-facing completion diagnostic, then lets the root open one
bounded mutation-access region. Assignment becomes visible inside that
region; access and any coordinator mutation admission end before completion
wakes or producer notifications run. Resolver drop follows the same gateway.

The GCI5R-002 transition protocol remains unchanged below those root
gateways: lazy publication installs the terminal result before releasing its
source, promise assignment precedes terminal publication and wake delivery,
and the one-write cells select exactly one winner. Deterministic completion
fixtures force publication before, during, and after subscription; the
competing-publisher fixture forces winner-before-loser order; and the
assignment-before-callback fixture latches the visibility boundary. No claim
in this checkpoint relies on repeated scheduling.

Test-only construction helpers now create the matching registered root
explicitly rather than restoring façade mutation authority. Source and access
inventories latch that production publishers use root gateways and that the
façades cannot silently regain mutation methods. The weak façade observers and
test-only observation/rooting compatibility helpers deliberately remain for
GCI5R-003F.

##### GCI5R-003F — Remove semantic-edge observers and audit durable copies

**Completed:** 2026-09-09

**Entry condition:** close GCI5R-008 first. A registered root must be able to
project its own managed edge so this checkpoint does not replace façade
observers while preserving a redundant cached pointer elsewhere.

- Remove the remaining weak observers and access-free `with_access`,
  `with_runtime_access`, and rooting helpers from the semantic façades. Their
  final representation is the managed edge alone.
- Adopt the `PromiseResolver` exception described above. Keep its label for
  host-side diagnostics and its `Option<ManagedPromiseRoot>` for affine
  disarming; use its observer as the sole weak runtime route.
- Audit registered roots and coordinator records. Keep copied IDs only where
  they are actual scheduler/index keys, keep a lazy label only where it is a
  concrete cycle-diagnostic record, and keep a promise label or observer only
  on the resolver or another proven external record.
- State the final unsafe-access proof explicitly: private constructors install
  interior edges into the matching runtime graph; a caller may dereference
  such an edge only under access derived from that graph's live root/runtime;
  public APIs reject foreign-runtime values; debug builds retain cheap
  provenance assertions.
- Verify the final 64-bit `LazyValue` and `PromisedValue` layouts are each one
  pointer, cloning them performs no `Arc`/`Weak` atomic operation, and resolver
  behavior remains correct after runtime retirement.

The semantic lazy and promise façades now contain exactly one managed edge.
Their target-specific layout latches record an 8-byte size and no drop glue on
x86-64; source latches forbid observers, roots, `Arc`, `Weak`, IDs, and labels
from returning to either façade. Because the remaining managed edge is
`Copy`, derived façade cloning only copies that pointer and performs no
reference-count atomic operation. Test inspection helpers now require an
explicit matching `CoreValueFactory`; production code continues to use the
already-open `RuntimeValueAccess` or evaluator access region.

The durable-copy audit retained lazy ID and label on `ManagedLazyRoot` for
scheduler indexing and concrete cycle diagnostics. `ManagedPromiseRoot`
retains its ID plus the registered root and edge-free terminal, completion,
and producer sidecars required while coordinator state is locked. It no longer
retains a label, observer, or cached edge. The promise cell's label also had no
remaining production reader, so it was removed; `PromiseResolver` now owns the
only host-facing promise label together with its weak runtime observer and
`Option<ManagedPromiseRoot>` affine disarm state.

The unsafe access proof is structural. Private runtime-scoped constructors
create each edge in one managed heap and publish it below either an exact
registered root or a traced same-graph value before access ends. A bare edge is
dereferenced only while traversing that live owner under its matching
`RuntimeValueAccess`. Public value boundaries reject cross-runtime
composition, while debug collector access checks validate heap and canonical
type metadata. No façade can independently reopen a runtime or extend the
mutator region across orchestration.

Focused managed-cell, evaluator, coordinator, cycle, and public-resolver tests
cover the migrated access shapes. Existing public API fixtures verify resolver
drop and completion after runtime retirement through the resolver's deliberate
weak-observer route.

##### GCI5R-003G — Closure and adjacent-façade audit

**Completed:** 2026-09-09

- Reconcile the I5.0 disposition, ownership ledger, evaluator architecture,
  and GC integration plan with the implemented policy.
- Audit `CoreRuntimeNet`, which currently has the analogous managed edge plus
  weak-observer shape. Either apply the same policy now or assign its removal
  to a concrete I8 checkpoint; do not leave it as an undocumented exception.
- Consume closed finding GCI5R-008. Registered roots and the reviewed durable
  handoffs must project rather than cache the managed edge for the allocation
  they root.
- Run focused managed-cell, evaluator, coordinator, cycle, and public-resolver
  tests; the complete repository checks; and targeted strict-provenance Miri
  because this work changes the authority surrounding unsafe managed-edge
  dereferences.
- Close GCI5R-003 only when the production inventory finds no duplicate
  ID, label, or weak observer in the two semantic façades and every retained
  copy has a named durable role.

The reconciliation confirms that the two semantic facades named by this
finding are final: `LazyValue` and `PromisedValue` each contain exactly one
managed edge, and their source-backed shape latches reject IDs, labels,
observers, roots, `Arc`, and `Weak`. `ManagedLazyRoot` retains its ID and label
for scheduler indexing and concrete cycle diagnostics. `ManagedPromiseRoot`
retains its ID and edge-free terminal, completion, and producer companions for
coordinator-locked work. `PromiseResolver` remains the documented public,
affine exception with one weak runtime observer, a host-facing label, and its
optional promise root.

Closed GCI5R-008 is fully consumed: all three `Managed*Root` records project
their managed edge from `Root<T>` under matching access, and normalization,
copy-source, frontier, and retryable-halt handoffs retain roots without caching
a facade or same-target edge.

The adjacent `CoreRuntimeNet` audit did not apply the lazy/promise edit in
place. Its observer currently qualifies source facades embedded throughout
generic core topology and supports the remaining self-rooting/test surfaces;
removing it requires coordinated source, root, and access changes. The GC
integration plan now contains I8A.0, which makes both `CoreRuntimeNet` and
`ManagedCoreNetRoot` observer-free before the final net payload/mutation audit.
The source comment and ownership ledger name this temporary exception, so it
is no longer undocumented drift.

Closure verification passed 44 managed recursive-cell tests, 17 core-net
tests, 46 coordinator tests, and 3 public resolver tests. The managed
edge/root identity fixture also passed targeted Miri with strict provenance.
Formatting, all-target/all-feature Clippy with warnings denied, and the full
repository test suite passed. The focused source inventories, rather than
schedule repetition, latch the final facade and durable-copy shapes.

The order is intentional: first remove passive metadata duplication, then
move reads and writes behind explicit authority, and only then remove the weak
observer which currently makes those access-free operations possible. That
keeps each checkpoint behaviorally reviewable while converging on the final
one-pointer façades.

### GCI5R-004 — I6 treats traced immutable paths as mandatory new identities

**Classification:** future-phase role and scope drift  
**Priority:** medium  
**Confidence:** high  
**Status:** closed 2026-09-09

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

The integration plan now treats compatibility-traced immutable shells as
audit targets rather than presumed new managed identities. I6A is partitioned
into function/net shells, partial builtins/applications, and fixpoint payloads.
I6B separates metadata identity/trace review from an optional conversion.
I6C separately audits the semantic failure graph and durable
`RuntimeFailureRoot`/diagnostic ownership before any optional conversion.
I6D.2 likewise defaults to auditing its exact immutable net-construction
effect edge.

Every audit may close without representation work when the existing visitor,
passive destruction, and external-owner boundary remain exact. Any selected
managed conversion must name an independent benefit and apply GCI5R-001's
regional allocation/first-owner rule. Reflection computation remains the
deliberate exception: I6D.1 must resolve its effect/target registered-root
backedge under GCI5R-005 rather than using audit-only completion.

Closure verification passed the failure-boundary, public-facade,
recursive-identity, and durable-owner source inventories. Formatting,
all-target/all-feature Clippy with warnings denied, and the complete repository
test suite also passed. This finding changes no production representation and
does not authorize collection outside the existing closed fixtures.

### GCI5R-005 — Reflection closure is split inconsistently between I6D.1 and I10A

**Classification:** future ownership chronology conflict  
**Priority:** high  
**Confidence:** high  
**Status:** open; A-D completed 2026-09-10; E-F remain; owns I6D.1 and blocks Gate G2

At the review baseline, `ReflectionComputation` was the one compatibility
adapter which deliberately reported no semantic value edge. Its runtime
external owner retained:

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

The phase must prove that a managed-reachable external owner contains neither
a semantic value root nor a strong route back to its own value domain. The
managed value may still retain an edge-free handle to externally destroyed
reservation authority; that is the purpose of the registry extraction. No
effect/target root may remain merely to conceal an internal cycle.

#### Construction investigation

This is a production-representable ownership cycle, not merely a broad Rust
type or test-constructor concern. `ReflectionComputation` is created at exactly
two evaluator boundaries:

- `anno refl:Effect Target` constructs a gate computation whose task must
  finish before `Target` is returned; and
- `anno meta_refl:Update Carriers` constructs a result computation which stays
  latent until reflection inspects the corresponding carrier metadata.

The direct `Value::reflection_gate` and `Value::reflection_task_result`
wrappers are test-only, but both delegate to the same production `_in`
constructors used by annotation evaluation. Ordinary `.g` recursion can close
the effect or target over the containing definition. A direct gate such as

```g
x = anno refl:(.r ()) x
```

already reaches the ordinary lazy-dependency-cycle diagnostic. That demanded
case proves source-level constructibility, but eventual cycle failure may also
release its lazy source and is therefore not the strongest reclamation witness.

The passive case is `meta_refl`:

```g
x = anno meta_refl:(\_ -> .r x) [anno 'meta_init ()]
asm.result = anno seq:x "OK"
```

Forcing `x` to WHNF constructs the carrier list while leaving its hidden
reflection result undemanded. The carrier reaches the reflection-result lazy;
its registered owner roots the update effect; and the update closure reaches
`x` through the module's recursive final-definitions promise. The program can
return `"OK"` while leaving this passive cycle in a live runtime:

```text
cached x
  -> carrier metadata projection
  -> reflection-result lazy
  -> ReflectionComputation handle
  -> ReflectionComputationOwner.effect root
  -> recursive final definitions
  -> cached x
```

The source compiler also wraps ordinary non-`refl`/`meta`/`spec` definitions
in an automatic reflection boundary whose effect closes over final module
definitions. That increases the number of ways a reflection computation may
participate in recursive runtime state; it does not make static rejection a
viable solution.

The baseline representation had three distinct backedge hazards:

1. `ReflectionComputationOwner::{effect,target}` are registered roots as soon
   as the reflection lazy is constructed.
2. After first observation, its `task` cell retains a cloned
   `ReflectionTaskReservation`. The reservation owns another rooted effect,
   and `activate()` currently borrows rather than consumes that activation
   payload, so the duplicate root survives activation.
3. The reservation retains an `EvalContext`, whose demand state strongly owns
   the `CoreValueFactory`. Because the owner registry belongs to that value
   domain, leaving this context in a persistent registry entry can also close
   an ordinary Rust ownership cycle through the domain itself.

The installed reservation error is currently produced only from an
`Arc<str>` admission failure and therefore contains no recursive managed edge
in practice. Its storage type is nevertheless the general
`Arc<EvaluationFailure>`, whose emission and context values are allowed to
contain managed identities. The remediation must either narrow this state to
its actual scalar failure vocabulary or place it on an exactly traced/rooted
boundary; it may not rely on the current constructor accidentally remaining
edge-free.

Parser rejection cannot address indirect closure through functions, objects,
imports, fixpoints, or metadata and would prohibit intended recursive values.
Evaluator dependency-cycle detection covers only demanded waits, not the
passive `meta_refl` graph. Treating the registry roots as weak would instead
make legitimate effects and targets collectible during the evaluator-to-task
handoff. The fix therefore belongs to the representation and lifecycle split
in I6D.1.

#### GCI5R-005 remediation plan

The target representation has three ownership classes:

- immutable effect and target `Value` edges belong to the managed-reachable
  reflection computation and participate in its exact compatibility trace;
- a persistent external reservation record contains only an edge-free weak
  task/wait observation, cancellation state, and weak coordinator authority;
  the ordinary strong task handle remains transient because its terminal cell
  may contain a value or failure root; and
- a first-observer activation permit temporarily roots the effect while it
  crosses from a completed evaluator step into coordinator-owned task state.
  It is consumed by activation or dropped on unwind and is never cached in the
  external owner.

Once activated, the coordinator's task machine is the intentional durable
owner of the effect and result. The reflection computation retains only the
stable weak task observation needed to reacquire a transient polling handle.
Collection of an unobserved
reflection lazy drops no active Rust lifecycle object; collection after
reservation retires the external record at the existing known-safe drain,
which cancels only a still-unactivated task.

##### GCI5R-005A — Production mismatch and owner inventory

1. Add an isolated source-level fixture based on the latent `meta_refl`
   example. Force `x` only to WHNF, drop every module/client root, and force a
   full collection while retaining the runtime long enough to inspect managed
   slots and external owners.
2. Before changing the representation, latch that the registered effect root
   retains the closed graph. Record the mismatch in this review, then update
   the committed test to require exact reclamation after the fix; do not leave
   a permanently failing or ignored test.
3. Add a smaller core fixture using the production `_in` constructors so the
   exact reflection lazy, owner entry, and semantic backedge counts are
   independently observable without substituting a test-only representation.
4. Destructure `ReflectionComputation`, `ReflectionComputationOwner`,
   `ReflectionTaskReservationInner`, `ReflectionTaskActivation`, and the
   relevant coordinator task records. Record every `Value`,
   `RuntimeValueRoot`, `RuntimeFailureRoot`, `CoreValueFactory`, `EvalContext`,
   task handle, and active `Drop` path.
5. Add a source-backed inventory which fails if a managed-reachable reflection
   owner gains another semantic root or strong value-domain route during the
   remediation.

Completed 2026-09-10. Before the repair, the direct `_in` fixture retained one
additional registered root after both semantic facades were dropped (`8`
roots versus a baseline of `7`) rather than finalizing its promise/reflection-
lazy pair. The first source witness likewise left two reflection owners after
its module was dropped. The committed source fixture places the latent binding
under `meta` to avoid conflating its hidden `meta_refl` result with automatic
ordinary-definition reflection tasks, warms compiler-cache roots before its
baseline, forces the carrier only to WHNF, and now requires exact root, slot,
and external-owner reclamation. A separate production-constructor cycle
requires exact two-cell finalization. Compile-time field destructuring and
source latches cover the managed computation, external owner, stable
observation, activation payload, task handle/weak observer, and cancellation
route.

##### GCI5R-005B — Restore exact semantic edges

1. Move the immutable effect and optional gate target out of
   `ReflectionComputationOwner` and into the managed-reachable
   `ReflectionComputation` payload. Do not introduce another managed identity
   unless an independent representation benefit is demonstrated; the existing
   lazy identity and compatibility visitor are sufficient to close this cycle.
2. Make `CompatibilityValueEdges for ReflectionComputation` report effect and
   target in stable semantic order without evaluating, formatting, comparing,
   or acquiring coordinator/registry locks.
3. Route construction through the GCI5R-001 regional allocation rule: the new
   reflection lazy and its direct edges must acquire the evaluator-step
   publication root or another exact traced owner before managed access ends.
4. Remove the immediate effect and target `RuntimeValueRoot`s from the
   external owner. The never-observed passive `meta_refl` fixture must then
   reclaim its complete graph and retire the empty external entry.

Completed 2026-09-10. `ReflectionComputation` now directly owns effect then
optional target in that stable trace order. Its external owner contains only
the task-publication cell, and construction continues through the existing
evaluator-step publication nursery. Both passive reclamation fixtures pass;
the exact-edge and durable/root-publication inventories now classify the two
former registered roots as ordinary managed semantic edges.

##### GCI5R-005C — Split stable reservation from activation ownership

1. Replace the externally cached `ReflectionTaskReservation` with an edge-free
   stable observation record. It may retain a weak task/wait identity, atomic
   activation or cancellation disposition, and weak coordinator route, but no
   `Value`, runtime value/failure root, `EvalContext`, `CoreValueFactory`, or
   strong demand-session/value-domain lease.
2. Make the first observer produce a distinct activation permit containing the
   temporary `RuntimeValueRoot(effect)`, selected task profile, result policy,
   and whatever short-lived context task construction requires. Store that
   permit only in `EvaluatorStepContext::pending_reflection_activations`.
3. Activation consumes the permit and transfers effect ownership into the
   coordinator task machine before releasing the temporary root. Dropping the
   permit because of unwind or abandoned evaluator-step publication releases
   its root and leaves the stable reservation eligible for external
   cancellation/retirement.
4. Subsequent observers share the published task handle without allocating a
   second effect root or activation permit. If implementation uses speculative
   reservation, every losing reservation must be deterministically cancelled;
   prefer a one-winner publication protocol which never calls coordinator code
   while holding a registry or managed-value lock.
5. Give the edge-free task observer the small cancellation operation currently
   reached through `EvalContext::cancel_reserved_task`, so external-owner drop
   does not need to retain the value domain merely to discard reserved work.

Completed 2026-09-10. The cached record is now a
`ReflectionTaskObservation`: an atomic disposition plus an
`EvaluationTaskObserver` containing scalar identity and weak coordinator/wait
routes. A strong `EvaluationTaskHandle` is reacquired only for the active
observation because the wait terminal may own a value or failure root. The
first `OnceLock` initializer alone publishes a shared one-use activation
permit; cloned copies share one mutex-protected payload, while later observers
receive no permit. Activation consumes the temporary effect root after the
launcher transfers ownership into coordinator task state. Dropping an
unconsumed permit deterministically discards its still-reserved work through
the weak task observer. The externally cached error was narrowed early to its
actual `Arc<str>` admission vocabulary; GCI5R-005D still owns the complete
failure/terminal/retirement audit.

##### GCI5R-005D — Failure, completion, and retirement ownership

1. Audit the only current reservation-error producer. Prefer storing its
   actual scalar admission failure and constructing `EvaluationFailure` at the
   evaluator boundary. If general structured failure identity must be
   preserved, store it on an exactly traced managed field or an independently
   retiring `RuntimeFailureRoot` which cannot lead back through the external
   owner.
2. Prove that successful gate targets are projected from the direct traced
   field only under matching evaluator access. A completed return-value task
   obtains its value from coordinator-owned terminal state; it is not copied
   into the external owner.
3. Specify retirement for never observed, reserved but unactivated, activated,
   completed, failed, cancelled, abandoned, exited, and killed tasks. The
   external owner may lag until the next known-safe drain, but it must not keep
   either the managed graph or value domain alive while waiting.
4. Preserve first-observer task identity and current result-policy semantics.
   No cleanup path may run a launcher, callback, value destruction, or
   coordinator mutation under a collector, registry, or managed-value lock.

Completed 2026-09-10. The reservation admission surface returns only
`Arc<str>` from closed-demand, expired-coordinator, identity-allocation,
wait-allocation, profile, and coordinator-reservation checks. The external
owner caches that scalar result and constructs a fresh `EvaluationFailure` at
the evaluator boundary; it never stores a general failure or failure root. A
closed-demand regression observes the same cached message twice, proves that
no partial task exists, then collects exactly the reflection lazy and drains
its edge-free external owner.

The terminal ownership audit records the following final split. "Observation"
below always means the cached weak task/wait record, never a strong wait cell:

| Disposition | Intentional durable owner | Retirement path |
| --- | --- | --- |
| Never observed | The managed computation traces effect and optional target; its external owner has an empty task cell. | Managed collection drops the scalar owner lease; a later safe registry drain drops the empty owner. |
| Admission rejected | The managed computation still owns its semantic fields; the owner caches only `Arc<str>`. | Collection and registry drain are identical to the never-observed case; no coordinator work was published. |
| Reserved, unactivated | The caller/evaluator step owns the strong task handle and one shared activation permit; the permit temporarily roots the effect. | Consuming the permit transfers ownership to a task machine. Dropping it first releases its payload, then discards reserved work through weak coordinator authority. |
| Activated, queued, running, blocked, or retryably exit-waiting | The coordinator owns the task machine and its semantic closure. Active pollers may transiently own a strong wait handle; the external owner remains weak. | Normal release, cancellation, demand-session closure, or settlement owns terminalization and machine destruction. Owner retirement cannot cancel activated work. |
| Complete | A live strong wait holds the coordinator-published result root. Gate success instead projects the direct traced target; return-value success projects this terminal root. | Evaluation installs the resulting value in the managed lazy cache, releases the source/owner lease, and leaves no result copy in the external owner. The terminal root lasts only as long as its real wait/status observers. |
| Failed | The wait and, until acknowledgement, the task failure ledger own the failure root. | The lazy caches the propagated semantic failure and releases its source. Propagation acknowledges the independent ledger entry; external-owner retirement owns neither copy. |
| Cancelled or abandoned | The terminal wait contains only the scalar disposition. An observer constructs the contextual semantic failure at the evaluator boundary. | Task retirement destroys or cancels the machine outside coordinator locking; lazy failure caching releases the reflection source. |
| Exited | The terminal wait contains only the scalar disposition; quiescence settlement has already disposed of the task machine. | An observer constructs the no-result semantic failure, then normal lazy-source retirement applies. |
| Killed | The terminal wait owns the settlement failure root; it is not copied into the external owner. | The observer propagates that failure into the lazy cache, after which ordinary source and wait retirement apply. |

`ReflectionComputation::target` reopens matching evaluator access and projects
only its direct traced field. Return-value completion continues to project the
coordinator terminal root. Existing gate-target laziness, arbitrary returned
value, failure-context, cancellation, shared-task, and result-policy tests
cover those semantic distinctions; GCI5R-005E owns forced collection and
interleaving at every row of this table.

Cleanup ordering is structural. Registry lookup returns an independent owner
`Arc` before first-observer admission begins, and registry draining detaches
entries under its mutex but destroys them afterward. Activation extracts its
payload before launcher construction. Permit unwind drops the effect root,
profile, and `EvalContext` before requesting coordinator discard. Terminal
machine destruction/cancellation likewise occurs after coordinator mutation
locks are released. Thus no reflection cleanup path nests semantic destruction
or user callbacks under collector, external-owner-registry, or coordinator
locks.

##### GCI5R-005E — Forced-order verification

1. Force both first-observer orderings across two sessions in one runtime and
   prove exactly one task reservation and one activation permit are produced.
2. Force collection at the construction/publication boundary, after stable
   reservation but before activation, during activation handoff, while the
   task is blocked, and after each terminal disposition. Each point must name
   the intentional root retaining the effect and target.
3. Force source retirement and external-owner drain while the unactivated
   one-use permit remains. Prove that the drain neither cancels nor retains the
   reservation, activation can still transfer ownership, and dropping the
   permit is the action that cancels unactivated work. Force activation first
   and prove later owner retirement does not cancel committed work.
4. Exercise an effect backedge, a gate-target backedge, and the latent
   source-level `meta_refl` backedge. Include a runtime-drop probe which proves
   no registry-owner-to-value-domain `Arc` cycle remains.
5. Use barriers, latches, and explicit collection points for concurrency
   claims. Passing under repetition is not evidence for either publication or
   cancellation ordering.

Completed 2026-09-10. The same-runtime two-session fixture forces all four
combinations of first observer and activation session. Every combination
publishes exactly one task in the first observer's registry, gives only that
observation the shared one-use permit, overlaps the other attempted activation
with launcher construction behind a barrier, and constructs the launcher
exactly once.

The collection fixture names each ownership transfer explicitly. A client
publication root retains its compatibility shell, reflection lazy, and
promise; the reserved activation root retains only its compatibility shell
and promise effect; that same root remains authoritative during the forced
launcher handoff; and the installed queued/blocked task machine owns the root
after handoff. Cancellation retires the final root and reclaims the remaining
shell/promise pair. Source and external-owner retirement are forced before
activation while the permit remains: draining the edge-free owner neither
cancels nor retains the reservation, and activation still succeeds. The
existing permit-abandonment fixture separately proves that dropping the
unconsumed permit cancels reserved work before a later owner drain.

A terminal-wait ownership matrix collects after each disposition. `Complete`,
`Failed`, and `Killed` retain exactly their documented value/failure root;
`Cancelled`, `Abandoned`, and `Exited` retain no semantic root. Dropping each
wait reclaims only the root-bearing cases. The production effect-backedge,
new gate-target-backedge, and source-level latent `meta_refl` fixtures all
require exact cycle reclamation. Finally, an undrained completed external
owner is retained across runtime wrapper teardown while a weak value-domain
probe proves that the edge-free registry record does not keep that domain
alive. All concurrency claims use explicit barriers or ordered calls; no
repetition-based evidence was added.

##### GCI5R-005F — Reconcile plans and inventories

1. Update the active-owner inventory so reflection semantic closure is owned
   by I6D.1/GCI5R-005, while arbitrary host callback environments remain
   deferred to I10A. Keep reflection reservation destruction classified as an
   external active action, but assert that its payload is semantic-edge-free
   and value-domain-lease-free.
2. Update the durable-owner and compatibility-edge inventories, source
   comments, ownership ledger, and evaluator/reflection architecture docs to
   name the final three-way split.
3. Re-run the reflection annotation, metadata, task lifecycle, cross-session,
   recursive-cycle, runtime-retirement, and managed-closure suites. Then run
   formatting, all-target/all-feature Clippy with warnings denied, the full
   repository tests, and a targeted strict-provenance Miri probe over the new
   trace and activation handoff.
4. Close GCI5R-005 only when the source-level passive cycle reclaims exactly,
   no external reflection owner contains a semantic root or strong value-domain
   route, and Gate G2 can treat reflection computation as closed.

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
- **I8A:** GCI5R-002D has installed the high-volume net's conservative
  whole-state gateway. Split payload/visitor reconciliation from exact
  per-edit delta conversion and from lock/lifecycle revalidation.
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

### GCI5R-008 — Registered roots cannot project their managed allocation

**Classification:** collector API omission and duplicate identity state

**Priority:** medium

**Confidence:** high

**Status:** closed 2026-09-09

#### Defect

`glam_gc::Root<T>` already contains the allocation address, erased inside its
private `RootCell`. `Root::get` reconstructs the corresponding `Gc<T>` before
dereferencing it, but the public collector API cannot return that inert typed
pointer. Glam therefore stores both a `Root<T>` and a separately copied
`Gc<T>` whenever a durable root must later produce a semantic edge.

This costs one pointer in every such record and, more importantly, represents
one allocation identity twice. Private constructors currently ensure the two
values agree, but the type itself permits a mismatched root and edge. In that
invalid state a root access could borrow one cell while reporting another cell
as the owner to the mutation barrier. The duplicated field is therefore more
than a layout opportunity: it is avoidable state whose consistency can only
be maintained by convention.

A safe `Root::as_gc(&self, mutator: &Mutator<'_>) -> Gc<T>` is sufficient for
the current collector. The mutator validates that the root belongs to the
entered heap and keeps that heap in a non-collecting safepoint state while the
projection is intended to be used. The result grants no dereference or heap
retention: `Gc<T>` remains an inert, non-rooting pointer, and `Root<T>`'s
private construction already establishes its type and allocation identity.
`Root::get` should use the same projection so typed reconstruction has one
implementation boundary.

The current `Gc<T>` is not lifetime-branded, so Rust cannot prevent a caller
from copying the projected pointer beyond the mutator borrow. That copy has no
valid use after its liveness or safepoint proof ends; every dereference still
requires a matching mutator and an independent liveness proof. Glam's internal
root projections should therefore be consumed within the bounded
`RuntimeValueAccess` which requested them rather than cached again.

This form leaves a direct path to moving collection: root cells can be updated
while mutators are stopped, and the next admitted projection observes the new
address. A future collector may additionally lifetime-brand projected edges
or make `Gc<T>` a stable handle. Caching the pointer beside the root would be
strictly worse because that copy could neither participate in safepoint
coordination nor follow a relocated root.

#### Current duplicate inventory

The source and recursive-identity inventories found the following same-target
pairs:

| Record | Duplicate representation | Planned disposition |
| --- | --- | --- |
| `ManagedLazyRoot` | `Root<ManagedLazyCell>` plus `ManagedLazyEdge` | Resolved: the edge field was removed and is projected from the root under matching access. |
| `ManagedPromiseRoot` | `Root<ManagedPromiseCell>` plus `ManagedPromiseEdge` | Resolved: the edge field was removed and is projected from the root under matching access. |
| `ManagedCoreNetRoot` | `Root<ManagedCoreNetCell>` plus `ManagedCoreNetEdge` | Resolved: the edge field was removed and is projected from the root under matching access. |
| `EvaluationHaltKind::UnassignedPromise` | `ManagedPromiseRoot` plus `PromisedValue` for that root | Resolved: retryable halts retain only the registered root; compatibility tracing relies on that root. |
| `NormalizationRequest` | `ManagedCoreNetRoot` plus `CoreRuntimeNet` for that root | Resolved: the request and its work descriptors retain roots and construct temporary semantic views under access. |
| `CorePreparedCopySource` | `ManagedCoreNetRoot` plus a `CoreRuntimeNet` hidden in `PreparedCopySource` | Resolved: the handoff retains its root and remote port and constructs the generic source at consumption. |
| `CoreFrontierObservation` | `ManagedCoreNetRoot` plus a `CoreRuntimeNet` hidden in `FrontierObservation` | Resolved: the observation retains its root, topology revision, and endpoint and projects the source for an attempted step. |

`RootCell`'s own `ErasedGc` is the root representation, not a duplicate.
`Managed*Access` values contain a bounded owner edge and borrowed cell but no
root; mutation barriers require that owner identity. `PreparedRuntimeValueRoot`
and ordinary scheduler records retain roots without duplicate semantic edges.
Test fixtures containing unrelated roots and edges are outside this finding.

#### Remediation plan

##### GCI5R-008A — Add and verify root projection

**Completed:** 2026-09-09

- Add `Root::as_gc(&self, mutator: &Mutator<'_>) -> Gc<T>` as an
  allocation-free, non-rooting projection and implement `Root::get` through
  it. Reject a mutator from another heap before reconstructing the typed
  pointer.
- Document the liveness and future-moving-collector limits above; do not add
  dereference, heap retention, hashing, or value equality to either handle.
- Verify projection preserves the original allocation identity, cloned roots
  project the same allocation, distinct roots remain distinct, and projecting
  does not change root registration or heap-retention behavior. Verify a
  foreign-heap mutator is rejected.
- Add targeted strict-provenance Miri coverage for this private erased-to-typed
  reconstruction boundary.

`Root::as_gc` now validates one matching admitted mutator before reconstructing
the root cell's typed non-rooting edge, and `Root::get` delegates to that
projection. The root remains one word and the implementation adds no unsafe
site. Native and strict-provenance Miri tests prove original/cloned-root
identity, distinct-allocation inequality, unchanged root-registry cardinality,
and all-build rejection of a foreign-heap mutator.

##### GCI5R-008B — Remove direct family-root duplicates

**Completed:** 2026-09-09

- Remove `edge` from all three `Managed*Root` records. Derive each
  `Managed*Edge` from `root.as_gc(mutator)` only in the
  representation-local bounded-access paths. Access-free `edge()` and
  `from_root()` helpers must either accept matching runtime access or be
  removed.
- Make root construction accept one allocation identity, register it, and
  retain only the resulting root. Eliminate the representable root/edge
  mismatch rather than adding another equality assertion.
- Update family layout assertions, the ownership ledger, and the recursive
  identity inventory. Add a source latch that no registered family root stores
  a `Gc`, `Managed*Edge`, or semantic façade.

All three registered family roots now retain one `Root<T>` as their canonical
allocation identity and project the corresponding private edge only through a
matching `RuntimeValueAccess`. Root construction cannot represent a mismatched
root/edge pair. Target-specific layout latches record the resulting 48-byte
lazy root, 72-byte promise root, and 24-byte core-net root on x86-64; these are
implementation measurements, not ABI.

##### GCI5R-008C — Remove enclosing same-target duplicates

**Completed:** 2026-09-09

- Make retryable promise halts root-only. Derive owned semantic views for
  evaluator consumers inside their existing bounded access. The compatibility
  visitor must recognize that the registered root is already the durable
  liveness authority; it must not gain a mutator-free edge projection merely
  to preserve the old façade trace. Do not restore an independently cached
  promise façade.
- Make `NormalizationRequest` root-only apart from its interface and mode.
  Project temporary `CoreRuntimeNet` views inside evaluator access when
  constructing work.
- Replace the source-bearing generic state inside `CorePreparedCopySource` and
  `CoreFrontierObservation` with their edge-free remote-port or
  topology/endpoint snapshots plus the root. Reconstruct the generic operation
  input only at its bounded, mutator-admitted consumption point.
- Preserve promise-halt equality, cursor-WHNF disturbance/retry behavior,
  prepared-copy source identity, frontier version checks, and all existing
  callback/mutator boundaries.

Retryable promise halts now carry only the boxed promise root, and scheduler
consumers construct promise followers directly from that root. Normalization
requests and work descriptors retain core-net roots rather than semantic net
facades. Prepared copies retain a root plus the remote port; frontier
observations retain a root plus topology revision and endpoint. Each generic
source facade is reconstructed at its bounded consumption point. Semantic
call and operator work still leaves the normalization scope before evaluator
logic runs.

##### GCI5R-008D — Close the inventory

**Completed:** 2026-09-09

- Re-run the compile-exhaustive recursive-identity inventory and a repository
  scan for direct or wrapper-hidden `Root<T>`/`Gc<T>` pairs. Every retained
  pair must identify different allocations or have a documented independent
  role.
- Record the measured layout changes without treating them as stable ABI.
- Run focused collector root, managed recursive-cell, evaluator halt,
  interaction-net copy/frontier, and cursor-WHNF suites, followed by the
  routine repository checks.
- Close this finding before beginning GCI5R-003F; that phase may then remove
  façade observers against one canonical root-to-edge projection.

The compile-exhaustive recursive-identity, durable-owner, evaluator-context,
and managed-entry inventories were first observed failing on the changed
representations, then updated to the reviewed root-only shapes. Source latches
reject direct family-root edge caches and wrapper-hidden semantic duplicates.
The ownership ledger records x86-64 measurements of 32 bytes for prepared
sources, 48 for frontier observations, and 32 for normalization requests, in
addition to the family-root measurements above. Focused root, recursive-cell,
promise-halt, copy/frontier, and cursor-WHNF suites pass; the routine format,
Clippy, and full repository test gates pass; and strict-provenance Miri passes
the managed root/edge identity projection fixture.

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
2. Tests described as mutation-gateway evidence originally established only
   single-writer lexical structure. GCI5R-002 replaced that evidence with
   deterministic policy probes at the production writer boundaries.
3. Phase status, ownership-ledger introduction, and pre-I8 source comments did
   not follow the atomic net cutover.

## Future-Phase Disposition

| Phase | Current disposition after I5 | Required adjustment before execution |
| --- | --- | --- |
| I6A-C | Exact compatibility tracing already closes cycles through these immutable shells. | Resolved by GCI5R-004: representation-specific audits default to retaining exact compatibility paths; conversion is optional and requires an independent benefit plus the regional allocation proof. |
| I6D.1 | Required to eliminate reflection effect/target root backedges. | Split semantic edges from active reservation lifecycle and take ownership away from I10A. |
| I6D.2 | Net-construction `Arc<Value>` is immutable and already traced. | Treat conversion as optional representation cleanup unless another identity/lifecycle need is found. |
| I7 | Visitor and most cycle evidence already exist. | Narrow to delta audit, missing thunk/backedge shape, and duplicate-work measurement. |
| I8 | Managed core-net owner and a conservative synchronized transition gateway already exist. | Split final payload audit, exact per-edit delta conversion, delta cycle states, and adapter/comment retirement. |
| I9 | I5 already changed and audited several root/RAII surfaces. | Start from I5 inventories and test only I6-I8 deltas plus final mandatory source audits. |
| I10 | Host callbacks and opaque storage remain real deferred boundaries. | Remove reflection after I6D.1; preserve host-capture and opaque decision gates. |
| I11 | Stable-boundary forced collection remains viable in principle. | Gate worker-concurrent collection on root-before-exit chronology and complete barrier/source audits. |
| I12 | Automatic entry can collect at the most dangerous handoff boundary. | Consume GCI5R-001's closed evidence as a hard policy-review prerequisite and inventory second-entry constructors. |
| I13 | Redundant compatibility/provenance cleanup remains appropriate. | Do not defer liveness or mutation safety here; add any accepted ID/label cache to its cleanup ledger. |

## Recommended Resolution Order

1. **Completed:** GCI5R-001's regional construction and first-publication
   protocol now closes the only finding which could make a currently valid
   `Gc` stale before later access once production collection is enabled.
2. **Completed:** GCI5R-002 selected the owner/set and borrowed-state gateway
   shapes, routed lazy and promise writers through them, and installed the
   synchronized whole-net correctness bridge. Exact net deltas remain I8
   performance work.
3. **Completed:** GCI5R-003A-G removed lazy/promise façade metadata and weak
   observers, moved observation/publication behind explicit authority,
   consumed GCI5R-008's root projection, and assigned the distinct net-wide
   observer retirement to I8A.0.
4. **Completed:** GCI5R-004 rewrote I6A-C and I6D.2 around the immutable-shell
   result, with audit-only completion and independently justified optional
   conversions.
5. **Planned:** resolve GCI5R-005 through the A-F semantic-edge,
   activation-lifecycle, retirement, forced-order, and reconciliation
   checkpoints now assigned to I6D.1.
6. Narrow I7, repartition I8, and update the I9-I12 entry conditions described
   above.
7. Re-run the focused and routine checks, update phase status and stale source
   comments, and close I5 before implementation proceeds into I6.

Production remains `CollectionPolicy::NoAuto`. This review authorizes no full
collection over a production runtime and does not advance Gate G2.
