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

The coordinator remains the sole mutation authority. The pump claims and
orchestrates work through coordinator transitions; it does not own a second
queue or terminal state. Session/context code owns policy and construction,
not executable machine storage.

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
and compiler-layer caches, a no-auto collector heap, and only a weak route to
the work coordinator. Explicit construction and evaluation capabilities retain
the domain; a public `Value` does not. Retaining `Values`, a demand context, or
a runtime service can therefore keep value construction usable without also
preserving the scheduler, executor, runtime facade, or default reflection
profile. The collector heap exists at this checkpoint, but production values
remain in their compatibility representation and are not collected yet.

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

The runtime-owned `EvaluationWorkCoordinator` owns session registration, one
runtime-wide ready-task queue, worker fairness, its work generation, the
condition variable used to await work, and stable runtime-local reflection,
deferred-producer, and spark records. Reflection and deferred records own
reservation/dormancy, queued, running, blocked, control, and terminalization
state. Reflection and deferred claims take their machine from the work record
while marking it `Running`; release either restores the machine before making
the record claimable or returns it for terminal destruction. The weak session
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
Workers retain a weak coordinator attachment and claim either an exact ready
task or spark record from it. Reflection and deferred claims need only their
coordinator records; resident machine contexts may retain closed demand state,
but no route recovers the owner lease. Final owner drop can therefore close
queued and blocked work immediately while a worker safely finishes one
already-claimed quantum. The immutable reflection environment belongs to the
active task host rather than either scheduling component.

After a claim is detached from coordinator locks, `evaluation/pump.rs` derives
one `EvaluationPollContext` from that checked session and supplies it to every
type-erased task poll. Caller-driven effect runs and isolated searches derive
the same carrier from their explicitly owned demand session; they do not
manufacture a coordinator claim. Client demands and sparks use the common
coordinator-owned adapter in cooperative and executor paths, so the executor
contains no separate value-admission policy. Every carrier temporarily retains
the validated demand state but exposes neither that route nor a mutator to the
machine. Its only managed-access operation opens a lifetime-bound region for a
bounded callback-free substep and closes it before returning. Whole
`eval_value`, lazy-source, and effect operations may reach dependencies or
callbacks, so they do not open one poll-wide region; spark demand now enters
through its scoped strategy implementation. Resumable scheduler-visible
machine boundaries publish dependencies as `Blocked`; direct and patient
drivers pump and wait only while retaining the mutator-free evaluator-step
context. An opaque deferred-source Rust callback cannot yet suspend and resume,
so its temporary compatibility path may cooperatively pump a dependency, but
it also inherits no managed-access region. Claim release, terminal publication,
cancellation, destruction, coordinator waits, and worker sleeps therefore run
without inherited mutator authority.

Successful type-erased machine polls cross that release boundary as a
`RuntimeValueRoot`, never a bare `core::Value`. A currently bare evaluator
result is wrapped through the checked poll domain, while an effect result keeps
the public root it already owns. Coordinator release only publishes the root.
This is the compatibility shape before managed semantic values: I3B moves root
construction to the evaluator-step publication boundary, and I4F.2 replaces
the root's interior representation without reopening the scheduler boundary.

Within a claimed or explicitly owner-driven poll, `EvaluatorStepContext` pairs
the poll authority with the durable evaluator context without activating the
collector. It is thread-bound and may survive dependency/callback
orchestration. Only its `with_value_access` operation enters a callback-free
managed region, so the recursive evaluator can be migrated without making a
whole `eval_value` call one mutator lifetime. One direct-compatibility gate
temporarily serves the remaining builtin seams plus source-inventoried
reflection/compiler entries; I3D and I3E own its eventual removal after I3B.2
separates direct wait driving. A closure inventory accounts for every
context-bearing function below `src/eval`: scoped functions retain
`EvaluatorStepContext`, while every remaining durable `EvalContext` surface
names its I3B.2/I3C/I3D/I3E or I10 owner. Separate latches cover all external
direct calls, the single compatibility constructor, and the dispatcher
downgrade set.

The core value/application/sequence spine now consumes this step context.
Client demand and deferred lazy/promise machines derive it from their checked
poll claim; result rooting also occurs through that same carrier. Legacy
compiler, reflection, and diagnostic callers enter the identical spine through
one source-latched direct-compatibility gate. That gate opens no ambient access
region: explicit deferred callbacks, reflection, net, and builtin seams receive
only their durable evaluator context, and no `EvaluationValueAccess` crosses a
pump, wait, callback, or machine poll.
`wait_for_claimed_task` is an ordinary coordinator wait and retains only this
durable, mutator-free context. The interaction-net disturbance wait is a
separate narrow exception: its bracketed local claim and acyclic handoff prove
that another evaluator is completing the same callback-free net work, so it
does not establish a general permission to wait with managed access.

Builtin application has a matching two-level boundary. `apply_builtin_in`
retains the evaluator-step carrier for migrated callback-free families;
`apply_builtin` is the one temporary direct-compatibility wrapper. Numeric
arithmetic, comparison, dictionaries, lists, objects, patterns, assertions,
pure conditional/list-effect construction, and pure annotation work use the
scoped route. The source-latched dispatch-time downgrade set is effects,
strategies, nets, and provenance. Reflection and metadata-reflection
annotations perform scoped recognition and input validation, then cross named
durable handoffs; `seq` and `spark` cross the existing strategy seam. No
ordinary builtin can accidentally inherit an active managed-access region.

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
Interaction-net calls preserve the same distinction: both ordinary and
operator calls retain retryable evaluator waits as blocked pair state, while
only permanent failures become stuck pairs.

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
| reserved, dormant, queued, running, blocked, or terminalizing reflection/deferred work | runtime work coordinator |
| queued, worker-owned, or dependency-blocked spark | runtime work coordinator |
| opaque live reflection/deferred machines | runtime work coordinator or its exclusive claim |
| task failure acknowledgement policy | runtime work coordinator task record |
| unacknowledged task failures, partitioned by owner session | runtime work coordinator ledger |
| task wait, current published status, and optional protected-query publisher | coordinator `TaskTerminalPublisher` obligation |
| task/wait lookup and retirement indexes | runtime work coordinator |
| completed, failed, cancelled, or abandoned outcome | shared `EvaluationWaitToken` cell |
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
construction task. It runs an isolated standard-effect search, accumulates one
write-only graph journal per alternative, requires exactly one successful
exposed-port result, then replays that journal once through checked
`NetBuilder`. `net_arity 0 Net` constructs a net computation; a positive arity
constructs a `FunctionValue`. Ordinary evaluation is one WHNF demand: it
follows top-level lazy aliases, but returns a raw `Value::Net` unchanged and
does not inspect its interface.

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
Direct observation before assignment fails without filling the cell. An
enclosing lazy task instead records a scheduler-visible promise dependency and
stays uncached, so later assignment can satisfy a new demand. Assigned promises
follow lazy or promised payloads through the common deferred dependency graph.
Promise-only and mixed promise/lazy cycles remain retryable scheduler waits;
only pure lazy cycles permanently poison computed results.

A `PromisedValue` is a thin shared `PromiseCell`. Successful assignment,
explicit failure, resolver drop, and task-producer termination all publish its
one authoritative assignment under shared runtime mutation admission. The
cell then fulfills an attached task producer obligation, detaches completion
registrations, and releases notifications only after admission ends.

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
brand, so handles cannot cross construction boundaries. `.data` journals its
payload without forcing it. Failed search alternatives retain no graph; only
the selected journal is replayed, and finalization remains authoritative for
linearity and topology errors.

Logical copies use target-owned one-way cursors into stable source frontiers.
A source active pair reduces in the source and never crosses a cursor boundary.
See the focused interaction-net note for fan identity, frontier, and locking
rules.

## Shared Executor

One `EvaluationRuntime` owns its attached `EvaluationExecutor`; assembler,
logger, macro, and future IDE demand sessions share that runtime rather than
registering independent worker pools. The fixed workers claim coordinator-owned
ready reflection/deferred work or optional sparks. The serial pump remains
available for exact foreground dependencies and explicit batch draining. It
selects by demand ID through the coordinator and does not require the external
session owner lease; an ownerless spark context can therefore finish a deferred
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
