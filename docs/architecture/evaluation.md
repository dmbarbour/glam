# Evaluation Architecture

This document follows ordinary value evaluation through sessions, lazy work,
interaction nets, and background workers. Detailed hazards live in
[`../agent_context/evaluation.md`](../agent_context/evaluation.md) and
[`../agent_context/interaction_nets.md`](../agent_context/interaction_nets.md).
Reflection-machine semantics live in [`reflection.md`](reflection.md);
structured failure transport and rendering live in
[`diagnostics.md`](diagnostics.md).

In evaluation lifecycle terminology, **foreign** means another
`EvaluationRuntime`. An owner session, observer session, non-owner session, or
cross-session dependency always refers to sessions within one runtime.

Public runtime construction and observational lifecycle projection are owned
outside this subsystem by `api/runtime.rs` and
`api/runtime/readiness.rs`. Evaluation owns demand sessions, waits, tasks,
promises, work records, scheduling, and settlement validation; it does not own
the embedding report shapes or transactional host-event transport.

## Module Ownership

`evaluation.rs` is the shared-contract facade. It owns the deliberately common
`EvaluationDemandState` and immutable reflection-task profile, then preserves
crate-private paths for consumers without becoming another scheduler.

- `evaluation/session.rs` owns the external session lease, session reports,
  evaluation-context construction, and task/deferred/promise admission policy.
- `evaluation/access.rs` owns the scheduler-claim or direct-owner-derived poll
  capability and the lifetime-bound managed-value view for bounded
  callback-free evaluator substeps. A poll context is not itself an active
  mutator.
- `evaluation/pump.rs` owns cooperative target pumping, claimed-machine
  dispatch and release, cross-session dependency assistance, lazy-cycle
  publication, and runtime-pump adapters.
- `evaluation/observation.rs` owns the semantic observation epoch, while
  `evaluation/executor.rs` owns worker activation and thread lifecycle.
- `evaluation/coordinator.rs` owns the common work registry, indexes, ready
  queues, dependency representation, and generation/condition variable.
  Its `completion`, `task`, `client_demand`, `spark`, `reflection`, `deferred`,
  and `settlement` children keep each lifecycle's state and transitions
  together without introducing separate registries.

The coordinator owns work records, their indexes, and ready state. It is not
the only lifecycle state: wait-token terminal cells, `LocalPromiseOwner`
obligations, and client-demand result cells keep their own locked state. The
pump claims and orchestrates work through coordinator transitions; it does not
own a second queue or terminal state. Session/context code owns policy and
construction, not executable machine storage.

## Context and Session

`EvaluationRuntime` is the allocation and construction boundary. It owns the
local ID domain used by sessions, tasks, waits, lazies and promises, reasoning
sessions, CLI invocations, and runtime event work. Numeric local IDs may repeat
in two runtimes; `EvaluationRuntimeId` supplies their eventual public
provenance. `EvaluationRuntime::values()` and `Assembler::values()` select the
construction domain before an embedding client builds a value. Every public
`Value` carries that runtime provenance; consuming APIs reject a value from
another runtime before exposing its recursive core representation or retaining
it in runtime-owned state.

The concrete value-lifetime boundary is one internal `RuntimeValueDomain`
shared by `CoreValueFactory` clones. It owns runtime-local value IDs, canonical
and compiler-layer caches, a no-auto collector heap, an external-owner
registry, and only a weak route to the work coordinator. Explicit construction
and evaluation capabilities retain the domain; a public `Value` does not.
Retaining `Values`, a demand context, or a runtime service can therefore keep
value construction usable without also preserving the scheduler, executor,
runtime facade, or default reflection profile. Production non-inline values
already use registered roots over managed outer value nodes, but collection
remains `NoAuto`. Controlled fixtures exercise serial, worker, and finalizer
schedules, while runtime maintenance exposes explicit request and service
without changing heap policy. Once `pump_until_stable` has drained useful
background work and obtained exclusive settlement admission, it promotes a
pending collector pressure latch into that same explicit maintenance protocol.
The pump does not collect: the next stable readiness observation returns
`MaintenanceRequired`, whose revision-checked service performs collection.
Drivers also service pressure themselves (decision
`noauto-runtime-collection-policy`, revised 2026-10-05):
- whoever polls a claimed task, spark or client demand reads the collector's
  lock-free pressure latch after the poll and before releasing the claim.
  That covers workers, the client-demand loop and the background pump. The
  poll's access region has closed and its result is rooted. The work is still
  claimed, so readiness stays `Busy` through a collection and a settlement
  snapshot is never invalidated by one;
- idle drivers never check, so an idle runtime's value domain still drops as
  soon as its last handle does;
- under pressure they collect through
  `RuntimeMutationAdmission::service_collection_pressure`, sharing the public
  service's lease and outcome handling.

So long foreground work, including CLI assembly, collects without reaching a
stable boundary. Ordinary mutator entry never elects collection, in any
build.

The private `aggressive-gc-verification` mode changes only the pressure input:
any allocation since the previous check counts as pressure.
- At a stable pump, the pump then services its own request, standing in for
  the embedding client, before continuing toward stability.
- At driver boundaries, drivers collect as in production.

Verification collections therefore use the same collection points and service
path as production. Readiness observed after a pump matches ordinary mode,
and heap policy remains `NoAuto`. Tests that need a collection at a
particular boundary request it explicitly.

Settlement validation rechecks the probe's work generation and exits, its
observation epoch, empty outputs, and a clean maintenance state: no active
lease, no pending request, and an `Idle` disposition. It does not compare the
maintenance revision. The collector is non-moving and preserves registered
roots, so a completed collection cannot change the settled instant; a failed
or in-flight collection surfaces through disposition or leases. The revision
remains the compare-and-swap token that admits exactly one service of a
maintenance snapshot. A settled report records the maintenance revision
current at commit. This treatment of settlement is a known hazard for
concurrent collection; the concurrent-collector plan's Open Design Gate 9 owns
it.

The `NoAuto` policy deliberately separates pressure detection from collection.
Allocation records pressure on the value domain. Two places act on that latch:
- a driver collects at its quantum boundary;
- a runtime client which has pumped to a stable readiness boundary promotes
  the latch to `MaintenanceRequired` and explicitly services it.

Either service publishes a GC
activity lease while holding *shared* runtime mutation admission only briefly
(`begin_gc_activity`); it never takes the exclusive settlement gate. It then
calls `Heap::collect_full`, whose own admission coordinator waits for every
active outer mutator to exit and holds the heap `Exclusive` for marking and
sweep, blocking new mutator entry. Finalizers then run in the heap's
`Finalizing` phase, outside collector locks, with ordinary mutator authority
reopened. The lease retires after the outcome is recorded. A driver nested
inside an access region gets `ActiveMutator` from the collector, which
records no failure, and skips. Work that runs long inside one access region,
such as lowering one large declaration, defers collection until the next
driver boundary. That is still no reason for ordinary mutator entry to elect
collection; the deferred concurrent-collector plan owns the remaining
starvation cases.

Every production evaluator entry receives an `EvalContext` derived from an
external `EvaluationSession` owner lease. An `Assembler` and its clones share
one internal `ReasoningSession`, which retains that lease and the assembler's
reflection host. `EvalContext` retains only `Arc<EvaluationDemandState>`, its
selected task profile, and current task provenance. The demand state holds the
value factory, session policy, and explicit closed flag; its coordinator route
is weak. The coordinator retains one weak demand-session registration solely
for guarded admission and removes it when the owner closes. Opaque reflection
and deferred machines, task/wait indexes, failure-acknowledgement policy,
protected status publication, and the persistent failure ledger reside
directly in coordinator state. The ledger is a persistent map from
owner session to that owner's task/failure map, so owner closure does not erase
an unacknowledged failure and a session report cheaply clones only its bucket.
Dropping the final owner marks their shared closed flag, then performs one
guarded coordinator closure transition across every record indexed by that
demand ID.
Queued and blocked work terminalizes immediately; running work retains its
first close reason and exclusive machine claim until release. Task producer
obligations settle before dependencies retired with parked sparks are
abandoned, so one closure cannot release the same reusable claim twice. Direct
isolated evaluation uses an explicit owner/context wrapper instead of hiding
the lease in `EvalContext`. Serial pumping and report construction belong to
`EvaluationDemandState`: they upgrade only its weak coordinator route, select
work by demand ID, and return the closed report if the external lease has
already ended. No machine-visible context can recover that lease.

The runtime-owned `EvaluationWorkCoordinator` owns session registration, the
runtime-wide ready-task set, worker fairness, its work generation, the
condition variable used to await work, and stable runtime-local reflection,
deferred-producer, and spark records. Production selection walks causal
producer chains from rotating background roots and exact demand routes; the
ready set's insertion order is read only by a test-only claim helper.
Reflection and deferred records own reservation/dormancy, queued, running,
blocked, control, and terminalization state. Reflection and deferred claims
take their machine from the work record while marking it `Running`; release
either restores the machine before making the record claimable or returns it
for terminal destruction. The weak session
registration validates admission but does not retain demand state or survive
owner closure. A reflection claim needs no session-owned reporting tail:
task/wait identity and terminal publication remain in its stable coordinator
record. Blocked reflection, deferred, and spark records retain their exact
dependency and checked subscription epoch; parked spark records additionally
retain their demand value, a weak demand-session route, their demand-session
index, and a close request while worker-owned. Detaching any reflection,
deferred, client-demand, or spark claim first upgrades the coordinator's weak
registry to one checked temporary `ClaimedDemandSession`. Completion sources retain
`(work ID, subscription epoch)` rather than bare IDs. A wake batch is accepted
only while the record remains blocked on both that epoch and the same
runtime-local dependency key; stale completion, session teardown, and
reblocking notifications are harmless. The attached
`EvaluationExecutor` owns only worker activation, shutdown, and thread handles.
Activation is all-or-nothing:
- every worker is spawned behind a `WorkerStartGate` before any may claim
  work;
- if a spawn fails, the gate aborts, the prepared workers exit and are
  joined, nothing is published, and activation stays retryable;
- on success, the executor publishes the handles and worker count and
  notifies the coordinator, and only then releases the gate.
Workers retain a weak coordinator attachment and claim either an exact ready
task or spark record from it. Reflection and deferred claims need only their
coordinator records; resident machine contexts may retain closed demand state,
but no route recovers the owner lease. Final owner drop can therefore close
queued and blocked work immediately while a worker safely finishes one
already-claimed quantum. The immutable reflection environment belongs to the
active task host rather than either scheduling component.

Demand-session indexes are lifecycle, closure, drain, and reporting indexes;
they are not execution lanes. Independent records in one demand session may be
claimed concurrently when workers or explicit claimants are available. The
coordinator promises neither same-session FIFO order nor one ordinary machine
at a time. Exact claim ownership and causal dependency state, rather than a
session-wide running-machine scan, exclude competing evaluation of the same
work.

Foreground demand claims its exact producer chain first. When an effect task
has launched a child before publishing a join, the coordinator's activation
index also lets that demand help the parent's still-live causal descendants,
including children in another demand session. It never substitutes arbitrary
same-session work. A claimed causal descendant is a `Busy` wait on the work
generation; an unrelated unresolved promise remains `NoProgress`. Child
retirement promotes its live descendants to the nearest surviving helping
route, without making launch an implicit join or transferring task ownership.

Foreground drivers retain a private, non-authoritative exact-demand zipper
across bounded polls. Its current work ID and parent subscription
epoch/dependency keys avoid rediscovering an unchanged producer chain; they do
not own work or keep a demand session alive. Claim and validation occur under
coordinator state. Two revisions decide whether the hint survives. The broad
`work_generation` advances for every scheduler-visible mutation; a private
route-hazard revision advances only for the mutation kinds that can change a
retained tip, ancestor, or projection (`affects_exact_route`). If the hazard
revision is unchanged and the claimed tip matches, the release hands off in
O(1) even though the generation moved. Otherwise the driver revalidates every
frame under the coordinator lock and keeps the route if validation passes; a
mismatch or an interrupted release falls back to the complete guarded
traversal from the original wait. No descendant index or
back-pointer is needed: a one-shot completion queues only registrations that
still match its exact subscription epoch and dependency key, so completing the
claimed tip can expose only its immediate parent. Causal `.task.new`
descendants are a separate helping route and never become zipper frames.

Ordinary worker quantums preserve their thread's inactive per-heap allocation
cursors for reuse. Worker-thread termination is the stronger collector
lifecycle boundary: an exit guard releases every inactive cache record after
the final scoped access has unwound. The release operation rejects an active
mutator, so termination also checks that no managed-access region escaped its
higher-ranked callback. Full collection, rather than TLS eviction, recovers
the forgotten allocation ranges.

After a claim is detached from coordinator locks, `evaluation/pump.rs` derives
one `EvaluationPollContext` from that checked session and supplies it to every
type-erased task poll. Caller-driven effect runs and isolated searches derive
the same carrier from their explicitly owned demand session; they do not
manufacture a coordinator claim. Client demands and sparks use the common
coordinator-owned adapter in cooperative and executor paths, so the executor
contains no separate value-admission policy. Every carrier temporarily retains
the validated demand state but exposes neither that route nor a mutator to the
machine. Its only managed-access operation opens a lifetime-bound region for a
bounded callback-free substep and closes it before returning. Whole-value,
lazy-source, and effect operations may reach dependencies or callbacks, so they
do not open one poll-wide region; spark demand enters through its scoped
strategy implementation. Resumable scheduler-visible machine boundaries
publish dependencies as `Blocked`; direct and patient drivers pump and wait
only while retaining the mutator-free evaluator-step context. An opaque host
callback is invoked only after regional access closes, and any later semantic
demand re-enters through an explicit rooted machine boundary. Claim release,
terminal publication, cancellation, destruction, coordinator waits, and worker
sleeps therefore run without inherited mutator authority.

Each claimed evaluator quantum also owns one stack-local
`EvaluationStepBudget`. Budget-aware nested machines borrow that same mutable
token; they never reconstruct an allowance from its remaining count. The
token records its original grant, remaining units, and exact declared spend.

A step pays one unit when it changes state, and observing is free (decision
`reduction-costs-one-budget-unit`). A step needs a unit available to start:
- **WHNF:** the regional driver charges each delegation; reporting a ready
  value, a boundary or a failure costs nothing.
- **Builtins:** applying an immediate builtin costs one unit. A builtin
  machine step pays one unit when its operand evaluation cost nothing
  (`ManagedLazyCheckpointEdge::machine_step`); a machine takes at most one
  step per poll.
- **Nets:** each rule application is charged at its claim; see
  [`interaction_nets.md`](interaction_nets.md) "The Net Driver".
- **Reflection:** delegating phases charge an administrative unit only when
  their child consumed none, and waiting pumps within the caller's budget.

So a one-unit quantum can always advance some work without hiding a renewed
sub-budget. The current demand pump deliberately
interprets its outer scalar as a reservation allowance: it reserves a whole
task quantum before polling and does not refund unused units. Its
`BudgetExhausted` result therefore describes exhausted reservation, not exact
inner spend. The runtime background pump instead translates exact inner spend
into its public report. Finer foreground accounting remains observational
until scheduler policy is revisited.

## Collector Boundary

One `RuntimeValueDomain` owns one non-moving collector heap. A `Gc<T>` is a
non-rooting interior graph edge and may be observed or duplicated only under
matching `RuntimeValueAccess`. A `Root<T>` is the registered durable owner used
when a value crosses an access region into scheduler, host, cache, or public
state. The public value facade keeps small integers inline and otherwise owns
one private registered root; it exposes neither representation identity nor a
raw core value without bounded matching access.

Lazy, promise, and core-net semantic facades each contain exactly one managed
edge. Their durable roots copy only the access-free scheduler, diagnostic, or
coordination fields justified by the ownership inventory. Immutable list,
dictionary, function, metadata, and failure shells remain ordinary structural
sharing, but their central compatibility visitor reaches every exact managed
identity beneath them. They are a deliberate boundary pending Value
Representation Refinement, not leftover recursive `Arc` ownership.

Fresh allocation is temporarily live only for its mutator region. Before that
region ends it must be installed under an exactly traced owner or published as
a registered root. Post-publication lazy, promise, and net changes go through
their collector-owned mutation gateways; the current stop-the-world policy
makes those barriers structural no-ops while preserving the exact sites a
future concurrent policy needs. No managed destructor may open the runtime,
observe or preserve a dying `Gc`, invoke a callback, or perform active
retirement. Such behavior belongs to an external owner which holds explicit
roots and is retired outside collector locks.

Managed access is a callback-free safepoint region. Coordinator locks, waits,
host callbacks, event delivery, diagnostic rendering, and worker sleeps occur
after it closes. Full collection stops new mutators, traces registered roots,
eagerly sweeps ordinary runs, and finalizes reviewed passive-drop families.
Recoverable trace/finalizer panics remain explicit maintenance state; permanent
heap damage is reported without re-entering the value domain.

Collector traversal is separate from mutator observation. Tracing receives
only a collector-created visitor and delegates to each family's crate-private
`trace_managed_edges`; rooting and observation use `RuntimeValueAccess`.
Neither API encodes "no active mutator", so a future concurrent or moving
collector can supply its own visitor. Canonical runtime values root only the
initial metadata carrier; edge-free atoms such as unit are built inside the
caller's access rather than held as permanent roots.

### Maintenance Admission

The runtime mutation-admission gate is a non-reentrant `RwLock`. Ordinary
publication takes it shared; readiness, settlement, and pressure promotion
take it exclusively. Settlement holds it exclusively while it constructs
managed values (kill failures and report roots), so no allocation or
value-domain entry may take the gate: settlement would deadlock on its own
thread. Collection obeys this. A service publishes its GC lease under shared
admission, releases the gate, collects, and publishes the outcome under shared
admission again.

Pressure is sampled at a stable pump under exclusive admission. Every
collection first publishes a lease, and the stable predicate requires zero
leases, so no collection can race that sample. Allocation can race it, since
allocation never collects: pressure arriving after the sample stays in the
collector's latch, and an older outcome clears only the request it serviced.
A race can therefore cause one redundant collection but never loses a request.

Under `aggressive-gc-verification`, pressure means "allocated since the
previous evaluation" (the heap's class-cache hits plus misses). A constant
`true` would livelock any client loop shaped like the CLI's settlement loop,
because each pump after a service would promote again. Rejected alternatives
are in `aggressive-gc-via-stable-pump` in [`Decisions.md`](../Decisions.md).

## WHNF Submachine Flow

Every durable whole-value owner carries one `WhnfComputation`. It begins with a
`RuntimeValueRoot`; its first bounded access promotes that seed atomically into
one `ManagedWhnfRoot`. The canonical managed state retains the current focus,
continuation frames, followed lazy and promise identities, and the optional
source-owner and cycle-promise state needed by the owning machine. Resuming the
same computation therefore continues from its last transition rather than
restarting demand from the original value. Two lighter designs were rejected:
adding lazy and promise cases to `EvaluationHalt`, and restarting the outer
machine once its dependency completes. Both lose the continuation that
consumes the result, so a step that builds an intermediate lazy rebuilds it and
suspends again without bound.

A bounded poll returns one of four kinds of outcome: a rooted ready value, a
deferred lazy or promise shell to settle outside access, an exhausted shared
step budget, or a rooted permanent failure. The access region closes before
the outer machine translates a deferred shell into coordinator work, waits,
invokes a callback, or publishes a result. Nested WHNF work borrows the same
mutable `EvaluationStepBudget`; it never manufactures a fresh allowance.

When lazy production must suspend, progress is installed beneath the owning
lazy instead of being retained by a scheduler-only side table. Ordinary WHNF
uses the managed WHNF checkpoint directly. Host-call, net-WHNF, access,
object-fixpoint, list-effect, and saturated-builtin families use typed managed
checkpoint cells whose state is traced through the same lazy owner. Client
demand, promise following, reflection decoding, and sparks retain their own
durable `WhnfComputation` while scheduled. No direct evaluator gate exists:
whole-value demand enters through these owned machines, while regional tests
and implementation helpers use a bounded normal poll context.

Successful type-erased machine polls cross that release boundary as a
`RuntimeValueRoot`, never a bare `core::Value`. Evaluator results are published
through the checked poll domain, while an effect result keeps the public root
it already owns. Coordinator release only publishes the root. The private root
representation is now either an inline small integer or one registered root to
a managed outer value node; projecting its compatibility core shell requires
matching bounded runtime access and never reopens the scheduler boundary.

Callback-free value construction uses the same regional rule. A
`RuntimeValueAccess` borrows the exact entering `CoreValueFactory`, including
its compilation-local view, and active mutator admission keeps an unpublished
intermediate graph live only until that access ends. The factory's infallible
and fallible construction entries publish the returned graph's containing
`RuntimeValueRoot` before leaving the region; a fallible early return publishes
nothing and leaves its partial graph collectible. `ScopedValues::wrap` uses
the access-owned publisher directly rather than recursively entering managed
access. Code which must wait, invoke a host callback, enter coordinator state,
or cross another orchestration boundary instead retains the factory and opens
a later access around its next bounded operation.

The current recursive managed families use three production owner-handoff
shapes. Pure regional builders retain owner-neutral facades only until a
containing value is rooted; genuine orchestration handoffs carry an explicit
promise or core-net family root; and nested semantic builders may pass facades
through ordinary values and containers only while the whole unpublished graph
remains inside one access region. None of those facades is a durable owner by
itself. `EvaluatorStepContext` also has a publication nursery, which holds
temporary family roots until the step publishes, but only test constructors
use it.

`LazyValue` and `PromisedValue` are one-pointer, edge-only semantic facades;
their registered roots carry only reviewed scheduler, diagnostic, and
coordination fields needed without managed access. `PromiseResolver` is the
public affine exception which retains weak runtime re-entry, a host-facing
label, and an optional promise root. `CoreRuntimeNet` is likewise one exact
edge-only semantic facade, including when stored as a cross-net source;
`ManagedCoreNetRoot` contains only its registered root. Net access and root
projection always require an explicit matching `RuntimeValueAccess` rather
than retaining weak value-domain re-entry on either representation.

The underlying collector pointers are likewise non-rooting regional edges.
The persistent-edge migration
([decision](../Decisions.md#persistent-managed-edges-are-move-only-raw-core-values-are-regional))
removed their ordinary copy, equality, and formatting traits. Persistent edge
duplication and allocation identity are explicit matching-access operations;
ordinary cloning remains only on registered roots and public rooted value
handles. Those clones share one root registration and gain no equality,
ordering, or hash. Duplicating an edge never registers a temporary root, which
would be correct but far too costly; the collector's release code-generation
check keeps duplication one pointer load. Glam's managed facades expose no unqualified pointer-copy or address-
comparison surface. Lifetime-branded temporary pointer views remain a deferred
safety enhancement rather than a claim of the current non-moving boundary.

Deferred external host calls separate traceable semantics from opaque host
behavior. `HostCallProducer` keeps every recursive Glam capture as an ordinary
managed edge; immediately before invocation those captures become a typed
same-runtime `HostCallRootBundle`, and the callback runs after managed access
has ended. The callback environment itself is an external conservative owner
held in the runtime registry. `OpaqueValue` similarly stores only a passive
`ExternalOwnerHandle`; its four admitted production families are source-
inventoried as either edge-free data or explicit external capabilities. No
managed node can reach a strong value-domain/runtime authority or a registered
root through those handles. Opaque access returns an owning `Arc<T>`, sound
only because every opaque payload is an external owner. Collection never
retires an external owner; an explicit drain destroys retired owners outside
the registry lock, catching a panicking destructor and continuing. Destructor
order across concurrent drains is not semantic. [`values.md`](values.md)
"Opaque Values and Host Calls" owns the details.

Post-publication changes to those families use the same bounded value-access
authority. The lazy cache reports its deferred source as leaving and its
terminal result as adding while preserving result-before-source-release.
Detached and coordinator-guarded promise publication share one empty-to-result
transition and retain their existing wake/retirement chronology. Managed core
nets implement the generic `RuntimeNetMutationGateway`: direct and conditional
edits, active-pair reductions, cursor claims, completion, and unwind restoration
all enter one collector transition while the semantic net mutex remains held.
Each semantic edit supplies an allocation-free exact set of leaving and adding
payload-owner addresses, resolved against the borrowed pre- or post-write net
without reacquiring the mutex. STW `NoAuto` visits neither side; a future
concurrent policy can activate those exact deltas without traversing the whole
net per edit.

Terminal wait records likewise retain `RuntimeValueRoot`. A general
`EvaluationWaitPoll::Complete` observation receives that owned root; only
`EvaluatorStepContext::project_root` may clone its semantic value back into a
bounded evaluator region. Non-evaluator consumers, including scheduled effect
lifecycle and `.task.join`, transfer the root directly into the public value
facade. `RuntimeValueRoot` now holds the compact private inline-or-managed-root
representation. The poll variant remains boxed and compile-time limited to two
machine words so recursive evaluator frames do not regress; the authoritative
terminal record remains inline.

Within a claimed or explicitly owner-driven poll, `EvaluatorStepContext` pairs
the poll authority with the durable evaluator context without activating the
collector. It is thread-bound and may survive dependency/callback
orchestration. Only its `with_value_access` operation enters a callback-free
managed region, so recursive evaluation does not make a whole-value demand one
mutator lifetime. A source-tree closure latch rejects the retired direct
evaluator gate and wrappers. A closure inventory accounts for every
context-bearing function below `src/eval`: scoped functions retain
`EvaluatorStepContext`, while every remaining durable `EvalContext` surface
names its durable owner. Separate latches cover retired direct entry names and
the dispatcher downgrade set.

The core value/application/sequence spine now consumes this step context.
Client demand and deferred lazy/promise machines derive it from their checked
poll claim; result rooting also occurs through that same carrier. Diagnostic,
compiler, and reflection clients use their scheduler-owned
demand/interpreter services. Explicit durable builtin seams enter the
evaluator spine only after an admitted poll has established their step
context. Deferred callbacks, reflection, net, and builtin seams receive only
their durable evaluator context, and no `EvaluationValueAccess` crosses a pump,
wait, callback, or machine poll.
`wait_for_claimed_task` is an ordinary coordinator wait and retains only this
durable, mutator-free context. The interaction-net disturbance wait is a
separate narrow exception: its bracketed local claim and acyclic handoff prove
that another evaluator is completing the same callback-free net work, so it
does not establish a general permission to wait with managed access.

Builtin application has a matching regional/durable boundary. Application
never runs a builtin. WHNF application (`Value::builtin_call_in`) and the core
`Builtin` operator yield a partial builtin until saturation and then always
allocate a lazy builtin source (`LazyValue::from_builtin_in`). Forcing that
lazy is the only production caller of `apply_builtin_in`, and only for
families that `RegionalBuiltinMachine::supports` does not cover. It runs with
the caller's existing `EvaluationValueAccess`, opens no nested access, and
receives no evaluator-step carrier. It either returns another lazy builtin
source or performs one immediate callback-free constructor such as append or
list-effect recipe construction, and the lazy continues from that result as
regional WHNF work. The lazy-source owner routes every demand-capable
saturated family through one `ManagedBuiltinCheckpointCell`.
Its compile-exhaustive regional state traces raw operands, completed prefixes,
and resumable child work beneath the owning lazy. Each transition runs inside
bounded caller-supplied value access; ready, failure, scheduler-boundary, and
spark outcomes cross that region only after the checkpoint transition closes.
Immediate families still return directly to the source owner, which roots
their result before regional access closes.

The direct `apply_builtin` wrapper is test-only and performs one bounded
regional operation. Whole-value tests use runtime-owned client demand; there is
no broader direct evaluator facade. Reflection and
metadata-reflection annotations perform regional recognition and input
validation before crossing their named durable handoffs; `seq` and `spark`
likewise publish scheduler work only after managed access closes.

Machine-visible admission uses demand state and a weak coordinator route, not
an upgraded owner lease. Its fast closed-flag check is advisory; reflection
and deferred reservation repeat the decisive check against registered open
demand state under the coordinator transition. If closure wins, admission
returns a closed-demand error. If reservation wins, the subsequent closure
sees and terminalizes that new record. `.task.new` descendants are ordinary
members of the same flat demand set: parent completion does not close them,
while final owner drop reaches every unfinished descendant.

Coordinator mutations participate in the runtime settlement-admission gate
and advance the coordinator's work generation before external wakes. They do
not advance `RuntimeObservationEpoch`: ready-queue churn is not a semantic
heap/input disturbance and must not cause a task to invalidate its own prior
observations.

Runtime store and admitted-input publication advance the separate typed
`RuntimeObservationEpoch`. Blocked task work records a checked
`{work ID, subscription epoch, observed epoch}` registration. Publication
queues every current older registration before releasing shared mutation
admission; block installation rechecks the epoch under that same admission.
When a block also carries an exact wait, both registrations use the same
subscription epoch: the first wake queues the task and either removes or makes
the other registration stale. The two-sided protocols prevent either a state
change or terminal dependency from being lost immediately before, during, or
after registration.

An effect handler's transaction generation remains private to that handler;
it is not itself a runtime observation epoch. A scheduled effect machine
captures the current runtime epoch before polling and uses that checkpoint
when the poll reports that it observed host state. Hosts which change such
state publish a runtime observation after their own commit. This keeps broad
wakes in one domain without requiring every handler's private counter to use
the runtime's numbering.

A synchronous effect facade which exhausts immediate exact work follows its
dependency chain before deciding that the wait is orphaned. If the chain ends
at a coordinator-indexed observation epoch, it waits for the corresponding
scheduler transition; a chain with no exact or broad wake remains quiescent.
Interaction-net calls preserve the same distinction: a callable that must
wait spills into a callable checkpoint whose pair blocks on that exact
retryable wait, core operators never wait, and only permanent failures become
stuck pairs. See [`interaction_nets.md`](interaction_nets.md) "Semantic
Handoffs".

`EvalContext` separately carries the complete profile inherited by
`.task.new`. A type-erased launcher closes over the specialization, immutable
environment, diagnostic destination, and shared host resources. This keeps
child-task inheritance distinct from annotation policy: annotations select the
runtime default, while their own children inherit that selected default.

### Runtime-owned constructed values

Each runtime owns one `RuntimeValueCache` behind its core value factory. It
contains canonical protocol values, the initial sealed metadata carrier, and
complete type-indexed bundles supplied by optional compiler layers. Static
protocol `Key`s remain process-wide immutable descriptions; production
statics do not retain constructed `Value`s.

An attachment is built completely outside the cache mutex, then installed as
one `Arc`. Concurrent first users may construct duplicate candidates, but
only the installed winner is observed. A `CompileContext` uses a scoped view
of the same factory which remembers attachment resolution for that compilation
without copying the bundle. The built-in `.g` compiler can consequently share
all lowered helpers, effect values, builtin modules, and its diagnostic
formatter across modules in one runtime while consulting the runtime
attachment map once per compilation.
The compiler bundle itself stores runtime roots rather than bare semantic
values. Candidate bundles are complete before publication, and lazily added
effect-path candidates are built outside their small publication mutex.

The core factory also carries one replaceable weak binding to the runtime's
work coordinator. At most one coordinator may be live for a runtime. An
isolated context reuses that coordinator when present, and may replace only an
expired weak binding. Consequently completion sources allocated through the
factory always deliver exact wakes to the coordinator which owns their work,
without making escaped values retain the coordinator.

The public `EvaluationRuntime` owns its lifecycle `RuntimeState` and immutable
default reflection profile as sibling roots. `RuntimeState` in turn owns one
`Arc<RuntimeSharedResources>` containing the value factory, transaction state,
observation epoch, mutation admission, and runtime-local IDs. That bundle has
only a weak route back to the work coordinator; the executor, coordinator
ownership, and diagnostic-ingress registry remain in `RuntimeState`. A profile
launcher retains its role-specific host, while runtime-backed hosts retain only
an internally composed view of the acyclic resource bundle plus their selected
environment and diagnostic capabilities. External effect hosts receive the
narrow `RuntimeTaskCapability`; the raw bundle, volume lifecycle, allocator,
and mutation admission are not public API. A retained profile can therefore
keep values, transactions, and volumes usable without keeping `RuntimeState`,
the executor, or the coordinator alive. Keeping the default profile outside
`RuntimeState` avoids a direct profile ownership cycle while the remaining
demand-state transition proceeds.

Lazy values retain computation and a stable identity, not a captured evaluator
session. The observing `EvalContext` supplies host and scheduling behavior when
the value is forced. There is no production `EvaluationSession::new` or
`EvalContext::standalone`: even isolated pure evaluation receives an explicitly
selected runtime value factory.

`core::EvaluationHalt` is the typed result of a demand that cannot currently
produce WHNF. Its permanent-failure case carries an arbitrary diagnostic value
and context frames; its wait and unassigned-promise cases remain retryable
scheduler control state. Core owns this distinction, while `eval` projects
permanent failures into diagnostic values. Terminal caches and wait cells never
store the retryable cases.

Its panicked case is neither. A panic is a runtime implementation error or a
client contract violation, never Glam semantics, so it is a task-layer
interruption:
- Only the scheduler's poll boundary creates one. `catch_unwind` around each
  claimed task, client-demand, and spark poll ends that work as `Panicked`
  through the ordinary terminal path, so its claim is released and the worker
  survives.
- Evaluator machines have no panic vocabulary. They treat a panicked wait as
  still blocked, and the boundary halts them, both before polling and when a
  poll blocks on panicked work. Re-polling would reinstall the panicked work.
- No panic is ever cached as a lazy result, assigned to a promise, or turned
  into an `EvaluationFailure`. Any such conversion re-raises the panic to the
  enclosing boundary instead.
- A lazy whose own route panicked records the panic as evaluation state,
  never as a result. Its source or checkpoint is released, so the work is
  never replayed, and every later route re-raises the original report. A
  poisoned per-value cell holds torn progress; observing it is likewise a
  fault, never a failure.
- A route forcing lazies inline catches a panic in one of them only to
  record it in that lazy, then resumes unwinding to the boundary, which
  records it in the route's own lazy.
- A panic that tears runtime-core state poisons the whole runtime. Core state
  means the scheduler, the transactions, or the settlement gate. The mutation
  and settlement authorities detect it while unwinding, and the worker loop
  catches scheduler panics that escape the poll boundaries. A poisoned
  runtime wakes its parked threads, refuses further mutation and GC leases,
  turns core-reaching destructors into no-ops, and reports
  `RuntimeReadiness::Poisoned`.
- The client API reports `ErrorKind::Panic`.

All clones of a lazy value share one source/result cell. Workers clone a source
snapshot without holding its mutex during evaluation. Terminal cache
publication precedes coordinator retirement, so later observers take the
cached path. A transient coordinator `Reserved` state bridges installation of
the coordinator-owned deferred machine; a racing claimant reuses the
canonical work and wait. Closure may win between reservation and activation;
activation then declines the already-terminalizing record rather than
asserting that it remains reserved. Blocked reflection and deferred work
retain their machines in their coordinator records. Terminal work retires from
coordinator indexes and destroys its detached machine outside runtime locks.

An `anno refl:Task` or metadata-reflection lazy has a boxed
`ReflectionComputation` source. It traces the immutable effect, the optional
gate target, and one managed completion promise allocated with the lazy as
direct semantic edges. There is no external-owner record or task observation
sidecar. The first producing poll roots those three values and, unless the
promise already has a producer or a result:

- reserves a task in the runtime's background demand domain, never the
  observer's session, with the runtime-default reflection profile;
- registers that task as the promise's producer. When the task terminates, a
  terminal mapper assigns the task result (`ReturnValue`), the gate target
  after unit validation (`RequireUnit`), or a structured failure for every
  abnormal outcome. A panic instead leaves the promise unassigned and records
  the panic on the producer obligation;
- installs a regional WHNF checkpoint focused on that promise beneath the
  lazy, so the lazy waits on the promise like any other dependency; and
- defers the one-use activation permit to its thread-bound step carrier. The
  permit roots the effect, profile, result policy, and bounded context. Poll
  orchestration drops the evaluator carrier before activating it, and
  successful activation transfers effect ownership into the coordinator task
  machine.

Other observers wait on the lazy's single producer route; a later producing
poll finds the registered producer or result and reserves nothing. An
activated task is an autonomous background root: retiring the lazy's route
after its last subscriber leaves never cancels it, and it outlives the
discovering session. A task terminal before activation skips the launcher.
Dropping an unconsumed permit cancels the reservation, which assigns a
cancellation failure to the promise. Cancellation or owner closure racing an
entered launcher retains the terminal result and makes machine installation
discard the unused machine outside coordinator locks. When WHNF propagates a
failed completion promise, the producer obligation acknowledges the task's
failure-ledger entry; an unobserved failure stays in the runtime ledger and
appears in settled reports.

Every scheduler wait token is one shared cell containing runtime-local
identity, scalar producer/owner provenance, an optional terminal result, and
exact weak work registrations routed through the runtime coordinator.
Completion, permanent failure, cancellation, and abandonment publish the
result before coordinator retirement; exact registrations detach and reach
the coordinator only after the terminal cell's lock is released. Polling
checks the cell before and after coordinator lookup, so a waiter racing
publication sees either active state or the terminal result. A terminal token
remains observable after its owner session is dropped. A pending token does
not retain or recover that session: owner closure must publish `Abandoned` or
a more specific terminal result before retiring the coordinator record, and an
unregistered nonterminal token is an invariant failure rather than inferred
lifecycle state.
Blocked reflection tasks, deferred producers, and sparks register their stable
work ID and subscription epoch directly with this cell. Only terminal
publication of that exact wait can requeue them; unrelated session task
progress does not.
Every clone of a public reflection task handle shares one opaque
`TaskHandleCell`. It owns the shared terminal wait and protected-query lease,
plus scalar runtime/task/owner identity and a weak coordinator reporting
route; it retains neither demand state nor the external owner lease.
Completion, failure, cancellation, and abandonment therefore remain
observable after the active record and task-ID index are retired. Final cell
drop queues protected-query retirement through ordinary store maintenance.
An unacknowledged failure also leaves one minimal entry in its producer
owner's runtime-ledger bucket until `.task.ack_error` removes it. Propagated
failure acknowledgement follows the handle's reporting identity directly to
that bucket instead of upgrading the former owner session. Rust clients receive
the corresponding opaque, runtime-bound `ReasoningFailure` from a settled
`QuiescenceReport` and may remove the same entry with
`Assembler::acknowledge_reasoning_failure`. Acknowledgement through either
surface leaves the terminal result unchanged.

### Scheduler State Ownership

Executable machine storage is split from terminal observation and reporting
state:

| State | Owner |
| --- | --- |
| reflection effect, optional gate target, and completion promise before terminal lazy caching | managed `ReflectionComputation` beneath its lazy cell |
| completion-promise producer obligation and terminal mapper | coordinator task record |
| unactivated effect and launch policy | first observer's shared one-use activation permit |
| reserved, dormant, queued, running, blocked, or terminalizing reflection/deferred work | runtime work coordinator |
| queued, worker-owned, or dependency-blocked spark | runtime work coordinator |
| opaque live reflection/deferred machines | runtime work coordinator or its exclusive claim |
| task failure acknowledgement policy | runtime work coordinator task record |
| unacknowledged task failures, partitioned by owner session | runtime work coordinator ledger |
| task wait, current published status, and optional protected-query publisher | coordinator `TaskTerminalPublisher` obligation |
| task/wait lookup and retirement indexes | runtime work coordinator |
| completed, failed, cancelled, abandoned, exited, killed, or panicked outcome | shared `EvaluationWaitToken` cell |
| transactional `.task.status`, `.task.value`, or `.task.error` view | reasoning-store query |

Terminal publication precedes coordinator record removal. `poll_wait` checks
the shared cell before and after coordinator lookup; after the second check,
finding an active record means only that the producer is pending. It does not
reinterpret terminal state from a retained record. A promise assignment
observed during producer installation is canonicalized into the same wait
cell and retired by the polling path.

Abandonment describes loss of a session-local producer, not a universal
failure of the value being awaited. Reflection-task handles retain it as a
terminal task outcome. Ordinary lazy demand and host-promise followers may
discard the abandoned producer and install fresh same-runtime work instead.
An unresolved task-owned promise is different: the closing producer session
fulfills it with a structured producer-abandoned failure because that promise
has lost its sole responsible producer. Host promises remain controlled only
by their resolver.

A panicked producer differs from an abandoned one: its waiters halt instead
of installing fresh work, because new work would only rerun the panic. A
task-owned promise whose producer panicked stays unassigned. The panic is
recorded on its producer obligation, and observers halt with it.

When a lazy or assigned-promise task blocks on another deferred producer, the
coordinator records one strict dependency edge. The graph has at most one
outgoing edge per unresolved producer, so an edge insertion can find a cycle
with a successor walk. A pure deferred-value cycle, including one spanning
demand sessions in the same runtime, receives one canonical structured failure
shared by all members. The coordinator takes every member machine in the same
terminalizing transition, then settlement, cache publication, destruction,
and wakes proceed without reaching into multiple session stores. An edge
through a promise or reflection task remains an ordinary wait.

## Value Observation

```text
ordinary value demand
  -> non-lazy data, FunctionValue, or Value::Net is already WHNF
  -> LazyValue work is claimed, computed, and memoized through one coordinator producer
  -> PromisedValue reads one raw assignment, then follows a deferred assignment

arity bridge
  -> arity 0: LazySource::NetComputation expects exposed Data
  -> arity n: FunctionValue attaches n arguments, then expects exposed Data

apply(function, arguments)
  -> builtin or partial-builtin staging
  -> shared FunctionValue curried stage
  -> legacy dictionary-applicability path

interaction-net call
  -> Bind >< Data(Value::Net)
  -> logical-copy cursor attached to the opaque net's exposed interface
```

An undersaturated `FunctionValue` shares a curried runtime stage; saturation
produces memoized work. A raw `Value::Net` is an opaque value already in WHNF,
not an ordinary callable. Only the interaction-net call reduction opens it by
attaching a cursor. `LazySource::NetComputation` is the internal zero-arity
bridge: forcing it must expose data, and an exposed bind or non-data normal
form is an error carrying `eval:{op:'net_computation}` demand context.
`FunctionValue` provides the corresponding positive-arity bridge. Partial
application only attaches arguments and returns another shared stage; it does
not evaluate the net to verify an intermediate bind. Saturation demands data
from the fully applied stage.

The built-in `std` module exposes `interaction_net`, `net_arity`, `seq`, and
`spark` as ordinary curried values. `interaction_net Effect` is a memoized lazy
construction computation. It interprets the `eff` program with a pure
state-over-list builder, requires exactly one successful exposed-port result,
then synchronously replays that result's strict semantic netlist through
checked `NetBuilder`. `net_arity 0 Net` constructs a net computation; a
positive arity constructs a `FunctionValue`. Ordinary evaluation is one WHNF
demand: it follows top-level lazy aliases, but returns a raw `Value::Net`
unchanged and does not inspect its interface.

Compact persistent lists live in `list.rs`. Their `ListThunk` holes distinguish
computed lazies from named promises but remain opaque to list structure; range
and binary observation in `eval/sequence.rs` forces only the pieces required by
the caller.

## Lazy Producers

Computed fixpoints are immutable lazy sources. Demand installs one canonical
runtime-coordinator producer and wait source; every same-runtime observer
shares it. Strict recursive observation is diagnosed by the common lazy
dependency graph, while guarded recursion can finish at a constructor. If the
producer's demand owner closes, another session may reclaim the reusable lazy
without poisoning its result cell. Task-owned reflection fixpoints retain
their direct owner check. Assignment-style `PromisedValue` cells hold a raw
one-write assignment rather than a computed result cache.

An ordinary partially evaluated WHNF source is retained by the managed lazy,
not by the session which first demanded it. Its producer slot changes from the
original source to one field-opaque typed edge to the evaluator-owned managed
WHNF cell. A coordinator machine for this family is only a route adapter: it
holds no duplicate focus or continuation state, and a later same-runtime
session polls the exact checkpoint through the lazy. Terminal cache
publication removes the checkpoint after installing the result. Specialized
lazy producer progress also lives beneath typed managed lazy checkpoints. The
coordinator supplies a runtime-owned route record for admission, polling,
dependency publication, and last-demand retirement. A claimed route
reconstructs one transient poll adapter, but neither record nor adapter owns a
duplicate focus or continuation: semantic progress is read from the managed
lazy checkpoint. The route is session-neutral and retires when its demand
count reaches zero unless a claim is still completing.

A claimed route forces inline the lazies its lazy needs, instead of admitting
a route for each one (`inline-lazy-forcing`). At a lazy boundary, an uncached
lazy with no route, no other inline claim, and a resumable source or
checkpoint is claimed in the coordinator and polled on an explicit stack
above the route's lazy, sharing its budget. A host call or reflection task
must run exactly once, so it always keeps its route. The claim lasts until
the lazy completes or the poll ends, and a route admitted for the lazy
meanwhile cannot be claimed until then. A completed inline lazy is cached
and popped, and its parent finds the value. An inline lazy that suspends,
because the budget runs out or it blocks, spills: its claim ends, it is given
a route, and the route's lazy blocks on that route. Progress already lives in
each lazy's checkpoint, so suspended state is what it would have been without
inlining, and cycles are found among the spilled routes as before.

Work that must survive route loss falls into three ownership classes.
Demand-driven resumable state (WHNF, access, object, list, builtin, and
net-WHNF progress) lives in traced managed checkpoints beneath the lazy.
Autonomous root work, a started reflection task, stays a coordinator
background root and publishes through a managed completion promise; its
continuation never moves into the lazy. Post-access orchestration happens
after managed access closes: a host call checkpoints before and after its
callback because nothing else would continue it, while a best-effort spark
needs only an at-most-once scalar admission phase in its enclosing checkpoint.

Direct observation before assignment fails without filling the cell. An
enclosing lazy task instead records a scheduler-visible promise dependency and
stays uncached, so later assignment can satisfy a new demand. Assigned promises
follow lazy or promised payloads through the common deferred dependency graph.
Promise-only and mixed promise/lazy cycles remain retryable scheduler waits;
only pure lazy cycles permanently poison computed results.

A `PromisedValue` is one non-owning managed edge. Successful assignment,
explicit failure, resolver drop, and task-producer termination all publish its
one authoritative assignment through a producer-held registered root under
shared runtime mutation admission. The cell then fulfills an attached task
producer obligation, detaches completion
registrations, and releases notifications only after admission ends. The cell
retains only an immutable, root-free producer route with a weak link to the
concrete wait state; task and direct-runner owners retain the registered
promise roots and strong wait handles externally. Assignment removes the
individual owner root under the publication protocol, carries it through an
external post-publication handoff, and destroys it only after locks and
mutation admission are released and its wake is delivered. A public resolver
uses the same idempotent retirement rule and becomes inert if its runtime value
domain has already retired.

A task-owned promise has an active wait record only while its assignment is
unresolved. Its producer obligation publishes the same terminal assignment
into the shared wait cell while the owner record and index are retired.
Outstanding wait handles therefore retain late terminal observation without
keeping session scheduler state. Host promises have no task-owned wait record.
Deferred followers and sparks publish the promise itself as their exact
dependency. When either parks, its stable work ID and current subscription
epoch enter the promise cell's exact-subscriber component. Terminal
publication therefore queues only work still blocked on that promise; there
is no session-wide promise wake. The common subscribe-and-recheck protocol
closes assignment races without nested component locks, and the retained
registration does not keep its demand session alive.

Reflection annotations are also lazy producers. Constructing a gate demands
neither its effect nor its target. Demand on the gate registers or resumes the
effect task; after checking that it returned unit, the same demand continues
into the target. Blocking remains coordinator task state rather than a cached
lazy error. If another session owns a still-pending gate task, the observer
records its exact same-runtime dependency and may pump that producer without
changing its owner or task profile. Another runtime rejects the containing
value before evaluation. Reports retain the producing session and task IDs;
terminal results and explicit abandonment remain observable after the active
work record retires.

## Reflection Task Handles

An opaque reflection task value retains its `EvaluationTaskHandle`: runtime,
task, producer-owner, work, and wait identity form one lifetime-bearing
capability. Join polls that wait directly. Cancellation routes to the work
record named by the handle, and acknowledgement routes to the immutable
producer-owner failure-ledger bucket. Transactional status, value, and error
observations remain query-backed and therefore keep their existing snapshot
semantics. Any session in the same runtime may observe, join, cancel, or
acknowledge the task; none of those operations changes its captured profile,
demand scope, or reporting owner. Runtime provenance is the capability
boundary.

Task creation reserves a non-runnable record. At transaction commit, all
modifiers for tasks created by that same journal are folded into one
pre-launch policy before any launcher is called. A same-transaction
cancellation publishes terminal cancellation and updates the status query
without constructing a machine, entering the ready queue, or notifying a
worker. Same-transaction error acknowledgement is installed before launch and
therefore suppresses reporting even if the child fails immediately; it does
not alter the wait result or status query. Modifiers for older tasks are
applied after pending launches have committed. Status-query callbacks run
after both the reasoning-store lock and scheduler lock have been released.

Only a committed public `.task.new` attaches a protected-query publisher to
its coordinator record. Internal reflection tasks use the same lifecycle with
no status query. The publisher retains the query handle, value factory, and a
narrow writer backed by `RuntimeSharedResources`; it does not retain the role
host, reflection environment, diagnostic bus, launcher, demand state, or
external owner lease. `TaskTerminalPublisher` keeps that optional publisher,
current status, and the shared wait together in the coordinator's settlement
inventory.

Active reflection records retain machines. Every terminal transition takes
its `TaskTerminalPublisher` exactly once. Under one runtime mutation admission,
it records any unacknowledged failure, publishes the shared wait terminal, and
updates the protected status query while acquiring coordinator, completion,
and transaction-state mutexes only in separate component steps. Exact wakes,
runtime-observation notifications, cancellation hooks, value release, and
machine destruction happen only after all component locks and mutation
admission have been released. A work record cannot retire until its terminal
publisher and producer-owned promise obligations are empty.

## Interaction-Net Handoff

`NetBuilder` validates an immutable template. Instantiation creates a shared
runtime with a stable interface. Evaluation repeatedly claims one exact
principal-principal active pair. Pure topology rules rewrite under the runtime
lock; core callable, operator, or cursor work runs after releasing it and then
updates the same pair.

The construction effect exposes `.bind`, `.copy`, `.data`, and `.wire` plus
the standard task-local effects. Its opaque ports carry an invocation-local
brand, so handles cannot cross construction boundaries. `.data` records its
payload in ordinary traceable builder state without forcing it. Failed search
alternatives retain no graph; only the selected result is replayed, and
finalization remains authoritative for linearity and topology errors.

Logical copies use target-owned one-way cursors into stable source frontiers.
A source active pair reduces in the source and never crosses a cursor boundary.
See the focused interaction-net note for fan identity, frontier, and locking
rules.

## Shared Executor

One `EvaluationRuntime` owns its attached `EvaluationExecutor`; assembler,
logger, macro, and future IDE demand sessions share that runtime rather than
registering independent worker pools. The fixed workers begin at activated
reflection tasks or admitted sparks and follow their exact deferred producer
chains; they do not claim unrelated ready deferred work. For fairness, a
worker alternates between task roots and spark roots when both can yield a
claim, and a root that yields one rotates to the back of the background-root
list. A poll that exhausts its budget releases the same work record without
publishing a dependency. The runtime background
pump follows reflection roots but excludes sparks. An exact serial demand
driver remains available for foreground dependencies and explicit batch
draining. It selects by demand ID through the coordinator and does not require
the external session owner lease; an ownerless spark context can finish a deferred
follower within the same demand instead of restarting from its original value.

Demand on `seq A B` demands `A` to weak-head normal form before transferring
that demand to `B`. Demand on `spark A B` records the same demand as
best-effort worker activity, then transfers foreground demand to `B`
immediately. If `A` reaches a sealed metadata carrier, both strategies demand
that carrier's one hidden value to weak-head normal form without recursively
unsealing a hidden carrier. Merely constructing either expression demands
neither target. Their annotation forms use the same paths.

Sparks express “this value will probably be needed soon,” not merely “run work
stored directly in this value.” Lazy values, promises, and sealed metadata
carriers are therefore admitted. A promise may expose work through its
producer or completed assignment; waiting on the promise is not itself the
goal. Nets and the remaining values are already in weak-head normal form.
Only workers consume sparks, so a zero-worker executor discards them
immediately.

Sparks are performance hints outside reflection transactions and reasoning
completion. They do not keep sessions alive or report independent failure. A
divergent spark can occupy a worker forever; the bootstrap currently provides
neither evaluator fuel nor cooperative cancellation. A retryably blocked spark
is parked without occupying a worker. Wait and promise dependencies retain its
exact work registration; terminal publication re-advertises only work still
blocked on that source. Subscribe-and-recheck prevents completion racing with
parking from losing the wakeup, while subscription epochs make late or
unrelated notifications harmless. Broad semantic disturbance uses the
independent runtime observation epoch rather than a session generation.
When a spark blocks on a newly reserved lazy producer, publishing the blockage
also promotes that producer; the spark never needs a serial-pump owner lease.
Dropping the session discards its parked sparks; any later registration is
stale and retains neither the owner lease nor its work.
