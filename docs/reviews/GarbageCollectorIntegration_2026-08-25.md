# Garbage Collector Integration Review — 2026-08-25

Baseline: `bb205d9`. This is a review-only audit of the production-integration
roadmap after the isolated `glam-gc` collector passed Gate G1. No production or
collector implementation changed as part of this review.

Status: complete. Integration is the recommended next workstream, but the
current Phase I1 should not begin verbatim. The findings below need to be
reflected in the plan or explicitly resolved before their affected checkpoints
begin. Gate G1 remains passed; this review does not authorize production
collection.

## Scope

The review compares:

- [`GarbageCollectorIntegration_2026-08-19.md`](../plans/GarbageCollectorIntegration_2026-08-19.md);
- [`GarbageCollectorOwnershipLedger_2026-08-20.md`](../plans/GarbageCollectorOwnershipLedger_2026-08-20.md);
- [`GarbageCollectionRoadmap_2026-08-19.md`](../plans/GarbageCollectionRoadmap_2026-08-19.md);
- the Gate G1 collector API and behavior; and
- the current runtime, value, evaluation, reflection, persistent-collection,
  and interaction-net ownership boundaries.

Drift is not automatically a defect. The review distinguishes a genuine
semantic or safety conflict from a plan which merely needs smaller checkpoints
now that the collector and runtime boundaries are concrete.

## Summary

The integration direction and major gate order remain sound:

1. establish one managed value domain per `EvaluationRuntime`;
2. introduce bounded mutator regions before managed pointer access;
3. close every exact trace, closure, opaque, collection, net, and external-root
   boundary before Gate G2;
4. force whole-production-graph collection only after G2; and
5. enable controlled and then automatic runtime collection only after G3.

The main corrections are:

1. provide a real manual/non-automatic collector policy before production
   allocations begin;
2. preserve the resolved boundary between opaque public value handles and
   runtime-mediated observation;
3. establish an explicit `RuntimeValueDomain` owner matrix;
4. add one narrow, fallible heap-provenance check for roots;
5. stop treating private, discovery-order collector identifiers as durable
   integration-ledger data; and
6. design the scoped evaluation-quantum authority before attempting broad I3
   mutator plumbing.

Several later phases also need smaller checkpoints, exact synchronization
wording, and verification deferred to the first legal whole-graph collection.

## Findings

### GCI-001 — Collection-disabled integration has no enforceable collector mode

**Classification:** gate violation and missing prerequisite  
**Priority:** critical  
**Confidence:** high  
**Status:** resolved 2026-08-25

The integration plan says production automatic and explicit collection remain
disabled until the later graph-closure gates, and I1 calls for runtime-local
tuning with collection disabled by default
([integration plan](../plans/GarbageCollectorIntegration_2026-08-19.md#phase-i1--runtime-heap-ownership-collection-disabled)).

The current collector has only `Heap::new()`. Allocation pressure sets the
same collection-request flag used by explicit requests. A later outer
`Heap::with_mutator` entry may elect and run a full collection when that flag
is set ([`heap.rs`](../../crates/glam-gc/src/heap.rs#L182),
[`heap.rs`](../../crates/glam-gc/src/heap.rs#L2029)). Consequently, merely
omitting calls to `collect_full` does not keep a partially migrated production
graph safe.

**Recommended resolution:** add an I1A collector prerequisite with an explicit
manual/non-automatic scheduling policy. Pressure may still be recorded and
`collect_full` may remain available to isolated tests, but mutator admission
must not service pressure or queued requests automatically while the
production runtime is in migration mode. Do not emulate this solely in a Glam
wrapper around an otherwise self-collecting heap.

**Required verification:** cross the pressure threshold in manual mode, enter
the heap again, and prove no collection occurred; separately prove an explicit
test-only full collection still works. The production integration must use the
manual mode until the phase which deliberately enables automatic collection.

**Resolution:** `glam_gc::CollectionPolicy` is immutable per heap and provides
`Automatic` and `NoAuto`. Under `NoAuto`, pressure and explicit requests remain
latched and visible, but outer mutator entry cannot elect collection;
`Heap::collect_full` remains the deliberate synchronous path. Public
`HeapStatistics` reports assigned runs, the current high-water mark/headroom,
the request latch, finalization-batch run count, and queued/running finalizers
without scanning heap allocations. Focused tests cover both pressure and
explicit requests across repeated entries, explicit acknowledgement, and
queued/running finalization snapshots.

### GCI-002 — Weak inert roots conflict with public `Value` equality and observation

**Classification:** public semantic contract
**Priority:** high  
**Confidence:** high  
**Status:** resolved 2026-08-26

The selected collector root is weak with respect to the heap. It remains
cloneable and droppable after heap teardown but cannot be read
([`root.rs`](../../crates/glam-gc/src/root.rs#L7)). The integration plan adopts
that property for escaped public values while also requiring current public
equality and debug semantics to remain intact
([integration plan](../plans/GarbageCollectorIntegration_2026-08-19.md#phase-i2--external-root-and-public-value-prototype)).

Today `api::Value` derives structural `PartialEq` and `Eq`, and its non-forcing
`Debug` implementation calls `kind()` on the retained core value
([`value.rs`](../../src/api/value.rs#L22),
[`value.rs`](../../src/api/value.rs#L750)). After the last authorized value-
domain owner disappears, a weak root cannot reconstruct structural equality or
outer kind. Pointer identity is not equivalent to current `core::Value`
equality, and `PartialEq::eq` cannot report that the domain is gone.

The same lifecycle question applies to every infallible observer and to
borrowed extractors on `EvaluatedValue`, not just formatting.

**Recommended resolution:** keep a Glam-owned public wrapper rather than
exposing `glam_gc::Root` directly, but make that wrapper an opaque transport
handle. Resolve I2 around the following contract:

- roots remain weak and do not preserve the value domain;
- bare `Value` and `EvaluatedValue` support transport operations such as
  cloning, dropping, and same-runtime thread transfer, but expose no semantic
  equality, ordering, hashing, kind, contents, or runtime identity;
- consequently public values do not implement `PartialEq`, `Eq`, `PartialOrd`,
  `Ord`, or `Hash`, and clients cannot use the handles themselves as semantic
  lookup keys;
- any retained `Debug` implementation is deliberately opaque and reveals no
  kind, contents, provenance, or accessibility state;
- structural comparison, kind inspection, extraction, and value rendering are
  fallible operations authorized by a live matching runtime service rather
  than observations a handle can perform by itself. The Rust API may pass the
  value to that service, pass a scoped service/mutator to a value method, or
  offer both forms when useful;
- runtime provenance validation remains private boundary enforcement, not a
  public observation; and
- runtime-mediated extraction returns owned host data which may outlive the
  value domain. Managed borrows do not escape their matching access region.

Retaining the heap from every public `Value` would preserve current equality,
but contradicts the chosen value-domain teardown model and is not recommended.

**Required verification:** use compile-time API checks to establish that
public values have no equality, ordering, or hash contract; exercise
runtime-mediated comparison of live equal, unequal, cloned, and separately
constructed structurally equal values; verify that opaque debug cannot reveal
value state; cover matching-runtime and foreign/inaccessible-domain
observation failures; and prove owned extraction results outlive the final
authorized value-domain owner.

**Resolution:** public values are opaque handles outside a runtime service.
They remain convenient to clone, drop, and transfer, but cannot themselves be
observed or used as lookup keys. `EvaluatedValue` remains the WHNF witness, not
an observation capability: semantic equality, kind, scalar/binary extraction,
and rendering all require live matching runtime authority, regardless of which
side owns the ergonomic Rust method. Internal root identity is not exposed as
a substitute equality relation. The existing compatibility representation
remains unchanged until the isolated I2 prototype fixes the new facade and I4F
performs the production switch; those checkpoints now own removal of the
unauthorized direct observation traits and methods.

### GCI-003 — The authorized value-domain owner matrix is not concrete

**Classification:** ownership architecture gap  
**Priority:** high  
**Confidence:** high  
**Status:** resolved 2026-08-25

I1 correctly says that only explicitly authorized non-root capabilities retain
the value domain, but it does not name those capabilities. Current ownership
is already distributed: `EvaluationRuntime` owns state and the immutable
profile as sibling roots, `RuntimeSharedResources` owns a value factory, and
`CoreValueFactory` is cloned into evaluation sessions, stores, compilers, and
caches ([`runtime.rs`](../../src/api/runtime.rs#L41),
[`core.rs`](../../src/core.rs#L271)). Public `Values` is also independently
cloneable and can construct values after the runtime facade has been dropped.

Without a fixed matrix, adding `Heap` to each convenient holder risks either:

- making `Value` unexpectedly preserve the complete runtime;
- making `Values` or an evaluation context become inert despite current use;
- retaining the scheduler/profile along with the value domain; or
- forming `heap -> managed closure -> factory/domain -> heap`.

**Recommended resolution:** introduce one internal
`Arc<RuntimeValueDomain>` containing the heap and value-domain facilities.
Explicitly authorize at least the public construction service, core factory,
runtime shared resources, and active evaluation/compiler owners as strong
domain leases where current behavior requires it. Public value roots and
managed nodes remain weak/non-owning with respect to the domain. Scheduler and
profile ownership stays outside the domain except for reviewed weak routes.

Every managed closure, opaque node, or other heap resident must be forbidden
from retaining a factory/domain strongly. I4B and I10 should audit that exact
backedge.

**Required verification:** latch facade drop with retained `Values`, shared
resources, evaluation context, profile, compiler cache, and bare public values
in separate tests. Each test must state whether the domain remains usable and
prove scheduler/profile cycles are not introduced.

**Resolution:** `CoreValueFactory` now has one strong
`Arc<RuntimeValueDomain>` rather than four independently cloned runtime-value
facilities. The domain owns the no-auto collector heap, runtime-local IDs,
canonical/extension cache, and a weak coordinator binding. Public construction
services, runtime shared resources, evaluation contexts, reflection
stores/snapshots, and active compiler views retain it through their factory.
Public values do not. Managed payloads and cache entries are explicitly barred
from retaining the domain strongly.

The owner matrix and conditional service-profile route are recorded in Phase
I1B. Focused lifecycle tests cover retained shared resources, public `Values`,
a bare public value, a retained service profile, an evaluation context, and a
populated compiler cache. The tests also prove that keeping the domain does not
retain runtime state, the coordinator, executor, or default profile, and that
the compiler cache does not create an internal ownership cycle. Production
values are not heap-managed yet, and collection remains disabled by policy.

### GCI-004 — Fallible same-heap root provenance is missing

**Classification:** API integration prerequisite  
**Priority:** high  
**Confidence:** high  
**Status:** resolved 2026-08-26

`Root::get` performs a release-build heap comparison and panics on mismatch.
That is an appropriate last safety check before the private typed-pointer
gateway, but it is not sufficient for Glam's public error contract. Composite
constructors and runtime stores currently reject foreign-runtime values with a
normal `Result` before evaluation.

The collector exposes no non-panicking `Root::belongs_to(&Heap)` or
`Heap::owns(&Root)` operation. Its root cell's weak heap association is private
([`root.rs`](../../crates/glam-gc/src/root.rs#L20)).

**Recommended resolution:** add one narrow constant-time ownership predicate
which compares the root's recorded weak heap identity with a live heap without
exposing an address or forgeable token. Keep `Root::get`'s assertion as the
unsafe-boundary backstop. Glam's wrapper uses the predicate to produce ordinary
cross-runtime errors before access.

**Required verification:** same-heap acceptance, foreign-heap rejection, an
inert root after heap teardown, concurrent clone/drop, and preservation of the
existing `Root::get` mismatch panic as a collector invariant check.

**Resolution:** `Heap::owns(&Root<T>)` is the single public collector
predicate. It compares the root's recorded weak heap provenance with the live
heap in constant time, without locking, entering a mutator, upgrading the weak
reference, inspecting the value, or exposing an identity token. A retained
`Weak` keeps its former control-block address from being recycled, so an inert
root cannot accidentally match a later heap. `Root::get` reuses the same
private comparison and retains its all-build mismatch assertion as the final
typed-pointer gateway backstop. Focused tests cover an owner and its clone,
another heap, a dropped owner, concurrent root clone/drop, and the existing
mismatched-access panic.

### GCI-005 — The ownership ledger requires private and unstable collector identities

**Classification:** documentation and verification-model drift  
**Priority:** medium  
**Confidence:** high  
**Status:** resolved 2026-08-25

The ownership ledger requires every managed row to record its canonical
`ObjectMetadata` pointer and dense heap-local class ID, among other geometry
([ownership ledger](../plans/GarbageCollectorOwnershipLedger_2026-08-20.md#gate-g2-blockers-and-reconciliation)).
Those identifiers are intentionally private to `glam-gc`. A metadata pointer
is process-instance data, while a dense class ID depends on discovery order
within one heap. Neither is durable type documentation or a useful production
integration contract.

**Recommended resolution:** record stable integration facts instead:

- Rust type and source owner;
- `Trace` reviewer and exact outgoing-edge policy;
- requested slot extent, Rust size/alignment, and drop/finalization policy;
- whether allocator discovery accepts the layout; and
- the mutation gateway and external-root classification.

Keep final stride, slots per run, and dense class identity in collector tests
or an explicitly test-only diagnostic surface. Do not widen the production
collector API merely to copy its internal topology into a document.

**Resolution:** the ownership ledger and I0/I1D integration contract now use a
stable representation-family reconciliation record. Each managed family is
identified by its concrete Rust type and source owner and must record its exact
trace review, requested extent plus Rust layout, allocator-discovery result,
drop/finalization policy, mutation gateways, external-root classification, and
authorizing verification. Metadata addresses, `TypeId`, dense class IDs,
frontiers, final stride, and slots-per-run geometry are explicitly excluded
from the production ledger.

Derived topology remains covered by `glam-gc` class/layout tests and may later
be exposed only through test or profiling diagnostics. Gate G2 instead checks
that every source-inventoried graph-bearing representation maps to one complete
stable family record and that every requested layout has passed collector
verification. No collector API change was needed.

### GCI-006 — I3 lacks a concrete scoped evaluation-quantum carrier

**Classification:** oversized phase and authority-design gap  
**Priority:** high  
**Confidence:** high  
**Status:** open

I3 asks mutator authority to travel through construction, evaluation,
workers, reflection, nets, compiler/macros, events, and diagnostics. The
collector's `Mutator<'heap>` is deliberately lifetime-bound and neither
`Send` nor `Sync`. Current `EvalContext` is cloneable and may be parked, and
`EvaluationTaskMachine::poll` receives only a step budget
([`session.rs`](../../src/evaluation/session.rs#L296),
[`task.rs`](../../src/evaluation/coordinator/task.rs#L132)). A mutator therefore
cannot simply become another stored context field.

Attempting the complete I3 as one phase would mix a new authority protocol,
worker scheduling boundaries, semantic lock audits, and mechanical call-site
migration.

**Recommended resolution:** begin with I3A, defining an explicit scoped
carrier such as `EvaluationQuantum<'mutator>` or `ManagedAccess<'mutator>`.
Machine polling and other internal boundaries receive or derive that carrier
only for the active quantum. A checked TLS convenience may support recursive
same-heap entry, but may not become the safety basis for dereference.

Then partition migration into:

1. public construction and synchronous evaluator/assembler access;
2. coordinator machine polls and worker quanta;
3. reflection, stores, events, diagnostics, and host conversion;
4. compiler, macro, and closed-value construction;
5. interaction-net construction and reduction; and
6. forced-order checks for sleeping, blocking, nested runtimes, and lock
   release.

Compile-fail tests should prove that mutators, allocators, managed borrows, and
quantum authority cannot escape their regions.

#### GCI-006 Plan Update

Treat this as a plan refinement before beginning I3 implementation. The
updated integration plan should preserve these decisions:

- durable task/evaluation context remains owned, `Send`, and parkable;
- scoped managed access contains a real borrow of the matching
  `glam_gc::Mutator`, so its lifetime and non-`Send`/non-`Sync` behavior are
  structural rather than an ambient convention;
- one bounded machine poll is an orchestration/scheduling quantum, not one
  continuously active mutator region. A scheduler-created ephemeral poll
  context may open several callback-free evaluator scopes, with rooted values
  between them and no active mutator during interpreter callbacks;
- the scheduler or synchronous driver owns admission. Individual machines do
  not silently enter heaps on their own;
- claimed work temporarily obtains its value domain from the matching demand
  session. The coordinator does not gain a strong value-domain backedge;
- every poll result which crosses the quantum boundary is converted to its
  owned/rooted boundary representation before the mutator is released; and
- scoped authority prevents mutators, allocators, and managed borrows from
  escaping, but does not replace the separate source inventory which forbids
  bare `Gc<T>` in parked or type-erased state; and
- subsystems which inspect managed values derive scoped capabilities from
  `RuntimeValueAccess`. The core interaction-net specialization uses a private
  authority-gated facade rather than teaching the generic interaction-net
  implementation about `glam_gc`.

Update I3 with the following checkpoints. A foundational
`RuntimeValueAccess` plus a thin evaluation-specific view carries actual
mutator authority. An ephemeral poll context is the scheduler-owned capability
which may construct those scopes; it carries no managed access between them.
These remain two layers of one authority model rather than independently
creatable capabilities.

##### I3A.1 — Authority Types and Non-Escape Contract

- Prototype the lifetime-bound managed-access carrier and its evaluation view.
  Pair it with the durable context without making the durable context itself
  lifetime-bound.
- Prototype the non-storable scheduler poll context and a closure/HRTB entry
  method which opens one evaluator scope, roots its escaping result, and drops
  the mutator before returning to orchestration.
- Derive heap provenance from the admitted mutator/domain and validate the
  durable context once when constructing the scoped view.
- Keep constructors private so neither TLS nor a runtime ID can manufacture
  authority.

Verification: compile-fail fixtures reject returning or storing the access
carrier, evaluation view, mutator, allocator, and managed borrow; trait checks
prove the scoped types cannot cross threads while the durable context remains
`Send`. No production call site changes yet, and production remains `NoAuto`.

##### I3A.2 — Claimed-Work Domain Routing

- Make reflection, deferred, client-demand, and spark claims carry or obtain a
  temporary strong demand-session reference while claimed.
- Use that session's value domain as the sole heap-admission source. Validate
  runtime agreement at claim/poll boundaries without storing a new strong
  domain owner in the coordinator or durable work record.
- Preserve current claim, release, cancellation, and owner-session shutdown
  behavior before adding mutator entry.

Verification: forced owner-close and cross-session schedules prove a claimed
poll keeps its existing session resources alive, an unclaimed record does not,
and a mismatched demand session is rejected before machine execution.

##### I3A.3 — Scheduler-Owned Poll Orchestration

- Change `EvaluationTaskMachine::poll` to receive the ephemeral poll context,
  then mechanically migrate production machines and test fixtures. Pure
  machines normally open one evaluator scope; effect machines may alternate
  evaluator scopes and interpreter work.
- Construct the poll context only after the coordinator has detached a claim
  from its locks. End each evaluator scope before claim release, terminal
  publication, host callbacks, cancellation/drop hooks, or coordinator waits.
- Route cooperative pumping, executor workers, client demand, and sparks
  through the same admission helper. Nested same-heap pumping reuses collector
  admission rather than opening an unrelated capability.

Verification: deterministic barriers observe an active mutator during pure
evaluator substeps and no active mutator during an interpreter callback placed
between two substeps, release, terminal publication, sleep, or machine
destruction. Existing task-order and shutdown suites remain semantic
regressions.

##### I3A.4 — Poll Outcome Ownership Boundary

- Inventory every `EvaluationMachinePoll`, task block, exit, and failure field
  which crosses the scoped region. Convert completed values to
  `RuntimeValueRoot` or the selected equivalent before leaving the evaluator
  scope which produced them.
- For payload families whose managed representation arrives only in I5-I10,
  record the exact later checkpoint which must update this boundary in the same
  change that introduces its first managed edge. Such a deferral remains a
  Gate G2 blocker, not permission to leave an unrooted edge once collection is
  enabled.
- Ensure outcome conversion performs no callback and that coordinator
  publication consumes only owned/rooted data.

Verification: force another thread to request collection at poll return and
prove every already-migrated outcome remains live through release and
publication. A source-backed boundary inventory covers every poll variant.

##### I3B.1 — Scoped Core Evaluator Migration

- Introduce the scoped evaluator view selected in I3A.1 and migrate the
  strongly connected evaluator call graph rooted at `eval_value`. Keep the
  persistent context only in machines and other parked state.
- Partition the mechanical migration by call-graph seams rather than one
  repository-wide signature rewrite: evaluator/application/sequence first,
  then ordinary builtins, leaving reflection and interaction-net entry points
  to their dedicated I3 checkpoints.
- Rework context-taking deferred closure signatures only where the new scoped
  contract requires it; retain I4B/I10 ownership classification for captured
  values.

Verification: a source inventory accounts for every evaluator function which
can allocate or inspect managed data, and focused tests prove recursive helper
calls reuse one outer admission. Existing evaluator and builtin suites remain
behavioral regressions.

##### I3B.2 — Poll/Wait Driver Separation

- Refactor synchronous and patient evaluation so a driver alternates bounded
  enter/poll/root/exit steps with waits outside managed access. Do not wrap an
  entire `eval_value` call in one mutator when it may reach
  `wait_for_claimed_task` or another blocking coordinator operation.
- Keep scheduled-machine paths nonblocking: dependencies return `Blocked`, the
  machine parks after the quantum ends, and another worker may resume it later.
- Ensure budget exhaustion and nested pumping cannot accidentally extend an
  outer mutator across a wait.

Verification: injected barriers force busy producers, promises, budget
exhaustion, and patient waits, asserting zero active mutators while sleeping
and successful resumption in a later quantum, including on another worker.

##### I3C-I3F — Subsystem and Final Audits

Retain the existing cooperative/worker, reflection/net,
compiler/event/diagnostic, and multi-runtime phases, but make them consumers of
the established carrier rather than opportunities to invent new admission
paths. Their verification must cover callbacks after mutator release, semantic
lock ordering, compiler suspension with roots only, opposite-runtime nesting,
and cache quiescence before workers sleep.

The call-site inventory below was completed before applying the second update.
Its scale—many machine fixtures and roughly two hundred `&EvalContext`
uses—justifies keeping the resulting checkpoints separate even though many
edits will be mechanical.

**Plan-update progress (2026-08-26):** both plan updates are applied. The first
separated authority construction, claimed-work routing, scheduler polling,
rooted outcomes, scoped core evaluation, and poll/wait driving. The second
corrected polling to be orchestration containing bounded evaluator scopes and
partitioned I3C-I3F around reflection activation, effect interpretation,
interaction-net claims, deterministic import demands, compiler/diagnostic
callbacks, and multi-runtime exit. GCI-006 remains open until implementation
and verification are complete.

#### GCI-006 Call-Site Inventory

This inventory covers production execution seams rather than listing every
transitive evaluator helper. A navigation scan excluding test modules finds
176 current `&EvalContext` type occurrences across 36 files. Most belong to the
strongly connected evaluator/builtin migration already assigned to I3B.1.
The smaller inventory below identifies every distinct outer admission, wait,
callback, or subsystem boundary which I3C-I3F must reconcile.

There are five production `EvaluationTaskMachine` adapters:

- `LazyTaskMachine` and `PromiseFollower` in
  [`eval/value.rs`](../../src/eval/value.rs);
- `ValueEffectTask`, `ContextualValueEffectTask`, and `UnitEffectTask` in
  [`reflection/machine.rs`](../../src/reflection/machine.rs).

The three reflection adapters share `EffectTask::poll`; tests contain many
additional mechanical implementations which must receive the new poll
signature but do not create another production authority path.

##### I3C: cooperative, runtime-pump, and worker dispatch

| Current seam | Work performed | Boundary implication | Intended owner |
| --- | --- | --- | --- |
| `ClaimedTask::{poll, release}` and `ReleasedTaskMachine::finish` in [`evaluation/pump.rs`](../../src/evaluation/pump.rs) | Type-erases reflection/deferred polling, publishes release state, then cancels or drops detached machines. | This is the narrow common task seam. Managed polling must end before release; cancellation, destruction, retirement, and terminal publication stay outside it. | I3A.3/I3A.4 establish it; I3C audits every caller. |
| `EvaluationDemandState::run_until_quiescent` and `pump_demand` in [`evaluation/pump.rs`](../../src/evaluation/pump.rs) | Serially claim, poll, release, and sometimes wait for another claimant. | Both already put `wait_for_change` outside `claimed.poll`, but each currently spells the poll/release sequence independently. | I3B.2 owns waiting; I3C routes both through the common admitted quantum. |
| `EvaluationWorkCoordinator::{poll_runtime_work, poll_claimed_task, poll_claimed_client_demand}` in [`evaluation/pump.rs`](../../src/evaluation/pump.rs) | Runtime-wide host pumping and shared adapters used by workers and patient demand. | These should become the only coordinator-facing admitted-poll functions. `poll_runtime_work` itself must not retain authority across selection or release. | I3A.3 implementation seam; I3C completeness check. |
| `evaluation_worker` in [`evaluation/executor.rs`](../../src/evaluation/executor.rs) | Claims ordinary tasks, client demand, and sparks; sleeps on the coordinator condition variable. | Ordinary tasks/client demand can use the shared adapter. Spark demand is a separate direct evaluator call and needs the same scoped entry. No authority may survive `release_spark` or `wait_for_change`. | I3C. |
| `ClientDemandOperation::poll` in [`evaluation/pump.rs`](../../src/evaluation/pump.rs) | Demands one rooted client value and immediately produces a rooted completion or dependency/failure. | Its operation is already carried by a claim with demand state, but the scoped evaluator view must be supplied by the caller instead of reconstructed by the operation. | I3A.2/I3A.3; I3C verifies worker and host-pump parity. |
| `EvalContext::drive_client_demand` in [`evaluation/session.rs`](../../src/evaluation/session.rs) | Alternates direct client-demand polls, dependency assistance, runtime pumping, spark abandonment, and condition-variable waits. | It is the principal patient driver. Every wait and stability snapshot must occur between quanta. | I3B.2; I3C regression audit. |
| `EvaluationRuntime::pump_until_stable` in [`api/runtime.rs`](../../src/api/runtime.rs) | Pumps runtime work, abandons quiescent sparks, takes settlement snapshots, and waits for worker/delivery activity. | It must remain an authority-free orchestration loop. A claimed quantum is wholly delegated to the coordinator adapter. | I3C. |
| `ScheduledEffectRun::run`, `run_composed_effect_task`, and direct `EffectTask::run` in [`reflection/lifecycle.rs`](../../src/reflection/lifecycle.rs) and [`reflection/machine.rs`](../../src/reflection/machine.rs) | Patient reflection execution, child draining, task polling, and host-generation waits. | These are synchronous drivers, not new heap-entry policies. They must use I3B.2 step driving and wait with no scoped access. | I3B.2 plus the I3D reflection audit. |

The release path currently converts `EvaluationMachinePoll::Complete(Value)`
to `RuntimeValueRoot` in `release_reflection_task` and
`release_deferred_task`, after `machine.poll` has returned. That conversion
must move inside the admitted outcome boundary from I3A.4; I3C must not retain
the current ordering merely because release is centralized.

##### I3D: reflection and interaction nets

| Current seam | Managed or semantic work | Wait/lock/callback concern |
| --- | --- | --- |
| `ValueEffectTask`, `ContextualValueEffectTask`, and `UnitEffectTask` in [`reflection/machine.rs`](../../src/reflection/machine.rs) | Adapt `EffectTaskPoll` to coordinator polls and currently unwrap a `PublicValue` back to bare core `Value` on completion. | Completion should preserve/root the already rooted public value before leaving scoped access. Failure contexts become managed later in I6. |
| `EffectTask::{poll, step, effect_request}` in [`reflection/machine.rs`](../../src/reflection/machine.rs) | Evaluates applications and requests, mutates branch/control state, snapshots and commits hosts, and dispatches specialization requests. | `TaskHost::{snapshot, commit}` and `TaskSpecialization::handle_request` are public Rust callbacks invoked inside `EffectTask::poll`. A whole poll therefore is not presently a callback-free mutator region. |
| `RequestContext::evaluate` and reusable handlers in [`reflection/protocol.rs`](../../src/reflection/protocol.rs) and [`reflection/requests.rs`](../../src/reflection/requests.rs) | Demand request arguments, construct return values, emit diagnostics, launch children, and inspect/update reflection state. | Managed access is needed while evaluating/constructing values, but immediate host emission and custom request code cannot inherit an ambient mutator accidentally. |
| `IsolatedEffectSearch::poll` in [`reflection/search.rs`](../../src/reflection/search.rs) | Runs the same `EffectTask` engine for macros, CLI search, token parsing, and net construction, retaining rooted branches and journals. | It must consume the same scoped reflection-step primitive as scheduled tasks; callers own the outer yield/wait loop. |
| `NetConstructionMachine::poll` in [`eval/builtins/net/construction.rs`](../../src/eval/builtins/net/construction.rs) | Advances an isolated effect search and converts successful construction-port tokens into a net. | It is nested inside `LazyTaskMachine::poll`; its search state is parkable, but any scoped access passed into it must remain non-storable. |
| `drive_net_work`, `drive_active_pair_step`, `progress_exact_core_call`, and `progress_core_operator_call` in [`eval/net.rs`](../../src/eval/net.rs) | Poll cursor frontiers, step active pairs, close normalization batches before evaluator calls, and install success/block/failure results under net mutation. | The existing batch discipline already avoids retaining one net batch while evaluating a callable or operator. That lock separation should be preserved when managed values are added. |
| `drive_net_interface` in [`eval/net.rs`](../../src/eval/net.rs) | Repeats cursor-WHNF work until a terminal interface result. | On `NetContention` it directly calls `wait_for_disturbance`. This is a narrow synchronization-handoff wait, not a semantic dependency: it may retain same-runtime mutator admission only after batch/claim containment proves another active evaluator must publish progress without requiring collection. |

The reflection callback row is not merely a verification concern. Public
specializations and hosts are allowed to execute arbitrary Rust code. The
second plan update selects a machine poll as an orchestration quantum
containing smaller evaluator scopes. `EffectTask` evaluates and roots one
monadic request, leaves scoped access, invokes its interpreter, and later
re-enters evaluation to deliver the result. The scheduler remains independent
of effect-specific request vocabulary.

Callback-free standard requests may fuse across that reference boundary when
a pure runner could implement them as transformations of branch-local state
and control. This includes task-local state but excludes shared heap/volume,
tasks, logging, reflection, and specialized host requests.

##### I3E: compiler, macros, diagnostics, events, and executable policy

| Current seam | Current behavior | Boundary classification |
| --- | --- | --- |
| `GCompilerValues::build`/`evaluate_closed` and the cached diagnostic formatter in [`g_syntax/compiler_values.rs`](../../src/g_syntax/compiler_values.rs) and [`g_syntax/diagnostic_formatter.rs`](../../src/g_syntax/diagnostic_formatter.rs) | Build complete runtime-local compiler bundles outside the cache lock, using private closed evaluation for helper functions. | Bounded construction/evaluation. Use I3B.1 scoped construction and publish only complete rooted cache bundles; no host callback is required. |
| `CompilationExecution` in [`api/assembly.rs`](../../src/api/assembly.rs) | Stores durable lookup and macro `EvalContext`s plus the macro owner session and diagnostic subscription. | The contexts remain parkable durable state. Individual lookup/macro polls receive scoped access; the subscription callback must not. |
| `run_macro`/`force_result` in [`g_syntax/macro_expansion/runner.rs`](../../src/g_syntax/macro_expansion/runner.rs) and macro lookup in [`g_syntax/parser/source.rs`](../../src/g_syntax/parser/source.rs) | Poll isolated searches and alternate evaluation with `pump_wait`. | These are explicit outer drivers. Their waits already occur after an evaluator return and should use I3B.2 rather than holding authority for the complete macro invocation. |
| `DeferredComputation` dispatch in `produce_lazy_source` in [`eval/value.rs`](../../src/eval/value.rs) | Invokes an arbitrary `Fn(&EvalContext)` from inside `LazyTaskMachine::poll`. | Split callback-free semantic thunks from external-demand producers. No arbitrary host callback remains hidden inside an evaluator scope. |
| `CompileContext::{import_module, import_binary}` in [`compiler.rs`](../../src/compiler.rs) | Implements imports as `DeferredComputation` closures which call `ModuleLoader` or `BinaryFileLoader`. The module loader may perform source I/O, recursively compile a module, emit diagnostics, and evaluate its sealed result. | Imports remain semantically reproducible after content-address/stable-hash validation, but their host loaders use a reflection-gate-like reserve/activate lifecycle outside the mutator. They remain distinct from reflection in policy and provenance. |
| `Assembler::build_module_inner`, `load_local_module`, `load_local_binary`, and `compile_diagnostic_emitter` in [`api/assembly.rs`](../../src/api/assembly.rs) | Own source loading, recursive compilation, final module demand, and diagnostic publication. | Source I/O and diagnostic callbacks are host work. Semantic lowering/construction receives bounded access; publication and recursive loader invocation do not inherit it. |
| `diagnostic_object`, `apply_updates`, `prepend_contexts_with`, and `conventional_summary_with` in [`diagnostic.rs`](../../src/diagnostic.rs) | Create isolated contexts and demand diagnostic objects/fields. | Convert to ordinary scoped evaluator calls. Each successful projection returned beyond the region must be an owned/rooted value or owned host scalar/bytes. |
| `Diagnostic::{enrich, enrich_with, apply_updates, with_context, transport_value}` in [`api/diagnostics.rs`](../../src/api/diagnostics.rs) | Validate public roots, perform semantic diagnostic composition, and retain public transport values. | Public wrappers enter scoped semantic helpers; bus publication remains outside managed access and retains roots. |
| runtime input conversion/admission and output delivery in [`api/runtime/events.rs`](../../src/api/runtime/events.rs) | Input converters run before mutation admission and return a public root. Output decode/adapter callbacks run after a delivery record and rooted payload are detached from locks. | The current callback/lock ordering is already suitable. Converters/decoders may explicitly call runtime evaluator/value services, but receive no inherited mutator. Runtime journals continue to store roots. |
| diagnostic bus callbacks and executable logger/rendering under [`api/diagnostics.rs`](../../src/api/diagnostics.rs) and [`bin/glam`](../../src/bin/glam) | Transport rooted diagnostics through buses, then evaluate/enrich/render through assembler services and write terminal output. | Bus callbacks and terminal writes remain mutator-free. Each semantic formatter/evaluator call opens its own bounded region and returns rooted data or owned bytes. |
| configured CLI/token searches under [`bin/glam/command_line/configured`](../../src/bin/glam/command_line/configured) | Use `IsolatedEffectSearch`, preserve branch journals, then construct command policy outside the library scheduler. | Consume the common isolated-search scoped step. Filesystem/path callbacks and output construction do not inherit access. |

##### I3F: multi-runtime admission and thread-cache exit

| Current seam | Inventory result | Required audit |
| --- | --- | --- |
| `RuntimeValueDomain::heap` in [`core.rs`](../../src/core.rs) | Exactly one `NoAuto` collector heap belongs to each runtime value domain. Production currently has no `Heap::with_mutator` call site. | I3A.1 must expose one private domain admission method; no caller selects a heap by runtime ID alone. |
| collector TLS in [`glam-gc/thread_cache.rs`](../../crates/glam-gc/src/thread_cache.rs) | TLS is keyed by heap identity, same-heap entry is recursive, and different heaps have independent cursor/depth records. | Glam should consume this behavior rather than add another runtime-level TLS authority system. Checked TLS lookup may optimize nesting but cannot authorize access. |
| `evaluation_worker` in [`evaluation/executor.rs`](../../src/evaluation/executor.rs) | One OS worker can process many sessions of one runtime and may eventually process nested work which enters another runtime through host code. | Drop every scoped entry before sleeping. Release that worker's inactive collector caches when the worker terminates; ordinary quantum exit need not clear them. |
| public host threads and synchronous drivers | May interleave or nest operations from multiple runtimes on one thread. | Opposite A-then-B/B-then-A entry must remain legal. No blocking wait or explicit `collect_full` may occur while either heap has active mutator authority. |
| runtime/domain teardown | Public roots become inert when their weak heap/domain route can no longer be upgraded; TLS records are weak and stale cursors are epoch-invalidated. | Verify teardown does not need to enumerate other threads' TLS and that escaped inactive caches do not retain a runtime heap. |

The collector already has focused tests for same-heap recursion, separate-heap
TLS records, opposite nesting with pending collection, cache release, and
unwind-balanced depth. I3F should reuse those as collector prerequisites and
add Glam-level forced-order tests around worker sleep, worker termination,
patient waits, and two runtime services nested on one thread.

##### Values crossing a quantum

| Boundary type | Current payload | Migration consequence |
| --- | --- | --- |
| `EvaluationMachinePoll::Complete` | bare core `Value` | Change to `RuntimeValueRoot` (or the selected owned equivalent) in I3A.4. Do not root later in release. |
| `EffectTaskPoll::Complete` / `TaskTerminal::Complete` | public rooted `Value` | Preserve the root through the coordinator adapter rather than calling `into_core` and recreating it. |
| `ClientDemandPoll::Complete` | `RuntimeValueRoot` already | No representation change; prove construction occurs under matching scoped access. |
| `ExitIntent::Error`, `EvaluationTaskStatus::Complete`, and `EvaluationWaitTerminal::Complete` | `RuntimeValueRoot` already | Already suitable parked/terminal owners. |
| `EvaluationWaitPoll::Complete` | clones a bare core `Value` from the terminal root | Restrict this projection to scoped evaluation or return an owned root at non-evaluator boundaries. It must not become a general authority-free managed value escape. |
| `EvaluationTaskBlock::error`, failed poll/status/wait variants | `Arc<EvaluationFailure>` containing diagnostic values | Keep the shared failure identity, but assign its exact managed/rooted representation to I6 before collection. I3A.4 records this deliberate deferral. |
| isolated-search branches and reflection store/event/diagnostic records | public values or `RuntimeValueRoot` | Retain their existing rooted boundary. Internal temporary `.into_core()` projections become scoped borrows/conversions. |
| net driver work and shared runtime nets | synchronized net handles containing eventual managed data/operator edges | Temporary driver descriptors remain quantum-local; exact net graph tracing and mutation barriers remain I8. |

##### Consequences and decisions for the second plan update

The inventory supports narrowing I3C to routing and forced-order verification:
I3A.3 and I3B.2 own the reusable orchestration and wait machinery. I3F is
mostly an audit plus worker-cache retirement because `glam-gc` already owns
multi-heap recursive admission.

The second plan update records three meanings of purity:

- semantic purity/reproducibility, including hash-validated imports;
- evaluator purity, including deterministic demand and suspension; and
- operational callback-freedom, which alone permits a continuous mutator
  region.

It resolves the remaining inventory decisions as follows:

1. a machine poll is orchestration containing smaller callback-free evaluator
   scopes, not one poll-wide mutator;
2. reflection task reservation occurs in pure evaluation, while launcher
   activation occurs outside scoped access;
3. effect tasks alternate pure request evaluation, callback-bearing
   interpretation, and pure continuation delivery, with a reference unfused
   path and safe pure-runner fusion;
4. callable and cursor claims become bracketed or lifetime-bound and must
   publish a durable disposition before semantic parking;
5. net contention remains a narrowly justified synchronization handoff rather
   than becoming a semantic wait; and
6. import loaders become deterministic external demands using analogous gate
   mechanics without being classified as reflection.

I3C.1-I3E.3 in the integration plan assign each source seam and its
forced-order verification. Production remains `NoAuto` throughout I3.

### GCI-007 — Exact trace responsibility across representation phases

**Classification:** trace-soundness sequencing clarification
**Priority:** low
**Confidence:** high
**Status:** resolved 2026-08-26

I4 says every then-current `core::Value` variant and transitive structure
receives an exact trace, while I7 and I8 later “audit and extend” persistent
collection and net tracing. This is safe only if later phases never introduce
the first missing adapter after a managed edge could already occupy that
structure.

The original finding overstated traversal risk for the current persistent
libraries. RPDS red-black trees are balanced and their iterator maintains an
explicit `Vec` navigation stack. FingerTree's iterator maintains an explicit
`VecDeque` of traversal frames. A logical adapter using those public iterators
does not recursively walk either external spine on the Rust call stack.

Glam's own `ListNode::Concat` is a narrower exception: repeated concatenation
can construct an unbalanced tree and current list operations recurse through
it. Gate G0 already records a pre-GC evaluator stack-overflow observation and
the broader stack-control problem remains separate runtime work. The GC trace
adapter can avoid adding another exposure by traversing `Concat` through a
small explicit local worklist and reporting lazy thunk edges without forcing
them.

**Resolution:** I4D now uses the supplied RPDS/FingerTree iterators and an
explicit worklist only for Glam's concat shell. It does not introduce generic
non-recursive structural adapters or redesign lists. I7 retains duplicate
shared-spine measurement and the possible later migration to direct managed
persistent nodes. I8 retains concrete net storage and mutation gateways.

The one safety rule preserved from the original finding is phase chronology:
every representation migration updates its exact trace adapter in the same
checkpoint which first introduces the managed edge. No later audit permits a
known placeholder in an active `Trace` implementation.

### GCI-008 — The interaction-net trace lock rule is incorrect

**Classification:** synchronization and trace-safety defect in plan  
**Priority:** high  
**Confidence:** high  
**Status:** resolved in plan 2026-08-26

The reviewed I8 previously said that acquiring a net lock while tracing should
be unnecessary because collection waits for all mutators
([integration plan](../plans/GarbageCollectorIntegration_2026-08-19.md#phase-i8--interaction-net-migration-and-trace-audit)).
Stop-the-world exclusion can prove that no legitimate mutator contends for the
lock; it does not grant safe Rust access to data still stored behind a
`Mutex`.

**Resolution:** I3D.3 now replaces the core-net type alias with a private
authority-gated newtype or equivalent facade. Every ordinary operation which
locks and inspects or mutates core semantic net state requires matching
`RuntimeValueAccess`; claims and lock-taking normalization leases are bound to
that scope. Generic interaction-net topology remains independent of
`glam_gc`. I3D.4 audits the facade and establishes the lock-exclusion invariant
before managed net migration.

I8 then traces the exact state under its semantic mutex with nonblocking
`try_lock`. Exclusive collection prevents any legitimate scoped ordinary lock
holder from existing, so `WouldBlock` is an invariant defect rather than
normal collector contention. Locking remains necessary for safe Rust access;
the stopped world never licenses unsynchronized inspection. No edge may be
omitted and tracing may neither reduce the net nor materialize a cursor.

The representation decision is now closed: I8 introduces one managed outer
cell for each production core runtime net. The cell owns the mutex and ordinary
Rust topology containers; individual agents and map entries do not become GC
allocations. I8A first abstracts the generic owner seam so non-core topology
remains independent of `glam_gc`, then I8B switches the core owner and its
cross-net references atomically with exact tracing and root discipline.

### GCI-009 — Several pre-G2 checks require production collection too early

**Classification:** verification chronology defect  
**Priority:** high  
**Confidence:** high  
**Status:** resolved in plan 2026-08-26

Gate G2 does not pass until closure, opaque, cache, collection, net, and
runtime-root inventories are closed through I10. Nevertheless, I7 asks a
public persistent collection to survive a full collection and permit a
backedge cycle to be reclaimed, while I9 asks for eventual reclamation after
dropping owners. Running a real full collection over the production runtime at
either point could traverse still-unclassified graph families.

I5 already uses the correct distinction: collector-ready isolated fixtures may
force reclamation, while the complete production graph does not collect.

**Resolution:** the integration plan now establishes one authoritative I5-I10
verification boundary. Each family may force collection only in a fresh,
closed collector-ready fixture containing that family and already certified
prerequisites. Production tests latch semantics, visitor/root construction,
mutation gateways, owner retirement, and ordinary drop behavior while the
production heap remains `NoAuto`; they do not explicitly collect it.

I7 now distinguishes a public-shape persistent representation in an isolated
heap from an actual production public-root graph. I9 owner-release checks
observe retirement without production reclamation and use isolated subsystem
fixtures only where local collection adds evidence. I11A repeats all isolated
cases while certifying Gate G2, and I11B owns the first forced collections over
an actual complete production runtime.

### GCI-010 — Later integration phases are too large for safe verification

**Classification:** implementation-risk partitioning  
**Priority:** medium  
**Confidence:** high  
**Status:** resolved 2026-08-25

I1, I3, I4, I6, I9, I10, and I11 each cross several independently risky
ownership or synchronization boundaries. This conflicts with the roadmap's
policy to divide a checkpoint before implementation when it spans several
unsafe or scheduler boundaries.

**Recommended resolution:** partition at least:

- **I1:** collection policy/dependency; value-domain topology; factory and
  scoped allocation; layout/ledger reconciliation; lifecycle regression;
- **I2:** public wrapper/provenance; opaque prototype surface;
  runtime-authorized observation prototype; production-switch inventory;
- **I4:** value shell/leaves; argument/failure structures; persistent
  adapters; net adapter; public-root switch;
- **I6:** functions/applications/fixpoints; metadata; failures/reflection/net
  construction;
- **I9:** runtime caches; coordinator/evaluation; reflection store;
  diagnostics/events; assembly/compiler/CLI; final source inventory;
- **I10:** deferred closures; opaque registration; scoped opaque access and
  finalization; final containment audit; and
- **I11:** Gate G2 audit; controlled forced collection; concurrency/finalizer
  schedules; Gate G3 certification.

Each checkpoint should name the representation migrated, exact tests latched,
and collection mode permitted at its end.

**Resolution:** the integration plan now partitions every identified oversized
phase. I1 separates policy, domain topology, scoped allocation, layout/ledger,
and lifecycle work; I2 separates provenance, the opaque prototype surface,
runtime-authorized observation, and the production-switch inventory. The first
GCI-006 update divides I3A into four authority/poll checkpoints and I3B into
two evaluator/driver checkpoints. The completed second update divides I3C into
poll routing and outcome release; I3D into reflection activation, effect
phases, net claims, and a subsystem audit; and I3E into deterministic external
demands, compiler/macro construction, and event/diagnostic callbacks. I3F
retains the final multi-runtime/TLS audit.
I4 separates shell/leaves, closure containment, argument/failure structures,
persistent adapters, net adapters, and the public-root switch; and I6 separates
functions, metadata, failures, and reflection/net-construction payloads.

Runtime integration is divided into cache, coordinator/evaluation, reflection
store, diagnostics/events, assembly/compiler/CLI, and final inventory
checkpoints in I9. I10 separates deferred closures, opaque registration,
scoped finalizable access, and the final containment audit. I11 now begins with
Gate G2 certification, then controlled production collection, deterministic
worker/finalizer schedules, and Gate G3 certification.

Each checkpoint identifies its representation or authority boundary,
verification fixtures, and permitted collection mode. Production remains
`NoAuto` through I11A; I11B-I11D permit only explicit controlled collection.
Passing G3 authorizes later I12 maintenance but does not silently enable
automatic collection. No implementation change was required for this finding.

### GCI-011 — Finalizer access to Glam runtime services is unspecified

**Classification:** lifecycle and ownership authority gap  
**Priority:** high  
**Confidence:** medium-high  
**Status:** open

I10 permits managed opaque destructors to allocate, evaluate, schedule work,
and emit diagnostics while the collector has installed its finalizer mutator.
The collector intentionally does not expose a globally discoverable “current
mutator,” and arbitrary `Drop` receives no Glam runtime context. Giving a
managed opaque allocation a strong factory or runtime-domain owner would form
the heap ownership cycle which I1 is intended to prevent.

**Recommended resolution:** add an I10 design checkpoint. Select either:

- a weak `RuntimeValueDomain`/`RuntimeSharedResources` capability stored in the
  managed payload and upgraded only during ordinary finalization; or
- a narrowly scoped Glam TLS bridge installed alongside the collector's
  finalizer mutator.

The capability must fail harmlessly during last-owner terminal teardown, must
not make a managed allocation own its heap, and must not permit rooting or
observing the allocation whose `Drop` is already running. Tests must cover
ordinary finalization, domain teardown, diagnostics/tasks emitted by a
destructor, and a destructor panic with untouched work retried later.

## Recommended Resolution Order

This order contains only work which resolves or preserves an integration-review
finding. Ordinary numerical phase dependencies remain in the integration plan
and are not repeated here as if they were additional deficiencies.

Before implementing the remaining I1 ownership checkpoints:

1. **Finding GCI-001:** use the completed no-auto collection mode for the
   production heap;
2. **Finding GCI-003:** preserve the completed value-domain topology and
   authorized owner matrix;
3. **Finding GCI-005:** preserve the completed stable ownership-ledger
   reconciliation contract; and
4. **Finding GCI-010:** preserve the completed checkpoint partition and
   collection-mode boundaries.

Before the public-root prototype or production switch:

5. **Finding GCI-002:** preserve the completed opaque-value and
   runtime-observation contract;
6. **Finding GCI-004:** preserve the completed fallible provenance operation.

Before managed recursive nodes:

7. **Finding GCI-006:** implement the I3A authority-carrier spike;
8. **Finding GCI-007, resolved:** preserve exact trace updates in the same
   checkpoint which introduces each managed edge.

Before Gate G2 and production forced collection:

9. **Finding GCI-008, resolved:** preserve the scoped core-net authority and
   exact locked-trace protocol;
10. **Finding GCI-009, resolved:** preserve the I5-I10 isolated-fixture and
    production-`NoAuto` verification boundary;
11. **Finding GCI-011:** resolve finalizer runtime authority.

## Review Decision

Shift work from isolated collector development to integration. Gate G1's
collector is sufficient for that transition, and C7/C8 stress and tuning can
continue later in response to production use.

Do not begin the current I1 as one checkpoint. First revise the integration
plan and ownership ledger to resolve or schedule the findings above. The first
implementation checkpoint should be the collector's manual/non-automatic
collection policy, followed by the runtime value-domain topology and its
latched lifetime matrix.
