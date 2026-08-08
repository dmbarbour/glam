# Evaluation Architecture

This document follows ordinary value evaluation through sessions, lazy work,
interaction nets, and background workers. Detailed hazards live in
[`../agent_context/evaluation.md`](../agent_context/evaluation.md) and
[`../agent_context/interaction_nets.md`](../agent_context/interaction_nets.md).

In evaluation lifecycle terminology, **foreign** means another
`EvaluationRuntime`. An owner session, observer session, non-owner session, or
cross-session dependency always refers to sessions within one runtime.

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

Every production evaluator entry receives an `EvalContext` derived from an
external `EvaluationSession` owner lease. An `Assembler` and its clones share
one internal `ReasoningSession`, which retains that lease and the assembler's
reflection host. `EvalContext` retains only `Arc<EvaluationDemandState>`, its
selected task profile, and current task provenance. The demand state holds the
value factory, session policy, and explicit closed flag; its routes to both
the coordinator and task reporting store are weak. An explicit
`Arc<SessionTaskReportingStore>` sibling of the demand state owns only the
transitional task/wait indexes used by serial pumping and retirement. Opaque
reflection and deferred machines, failure-acknowledgement policy, protected
status publication, and the persistent failure ledger reside directly in
coordinator state. The ledger is a persistent map from
owner session to that owner's task/failure map, so owner closure does not erase
an unacknowledged failure and a session report cheaply clones only its bucket.
Dropping
the final owner marks their shared closed flag, then performs one guarded
coordinator closure transition across every record indexed by that demand ID.
Queued and blocked work terminalizes immediately; running work retains its
first close reason and exclusive machine claim until release. Task producer
obligations settle before dependencies retired with parked sparks are
abandoned, so one closure cannot release the same reusable claim twice. Direct
isolated evaluation uses an explicit owner/context wrapper instead of hiding
the lease in `EvalContext`.

The runtime-owned `EvaluationWorkCoordinator` owns session registration, one
runtime-wide ready-task queue, worker fairness, its work generation, the
condition variable used to await work, and stable runtime-local reflection,
deferred-producer, and spark records. Reflection and deferred records own
reservation/dormancy, queued, running, blocked, control, and terminalization
state. Reflection and deferred claims take their machine from the work record
while marking it `Running`; release either restores the machine before making
the record claimable or returns it for terminal destruction. Session
registration retains the reporting store while indexed reflection work
remains, and a reflection claim retains that store for its transitional
task/wait lookup during a poll quantum; neither route recovers or retains the
external owner lease. Blocked reflection,
deferred, and spark records retain their exact dependency and checked
subscription epoch; spark records additionally retain their demand value, an
`Arc<EvaluationDemandState>` which cannot recover the external owner lease,
their demand-session index, and a close request while worker-owned. Completion
sources retain
`(work ID, subscription epoch)` rather than bare IDs. A wake batch is accepted
only while the record remains blocked on both that epoch and the same
runtime-local dependency key; stale completion, session teardown, and
reblocking notifications are harmless. The attached
`EvaluationExecutor` owns only worker activation, shutdown, and thread handles.
Workers retain a weak coordinator attachment and claim either an exact ready
task or spark record from it. A reflection claim retains the registered
reporting store while its machine remains exclusively in the coordinator
claim; deferred claims need only the coordinator record. The store has only
a weak coordinator route and no direct demand-state route; resident machine
contexts may retain demand state, whose route back to the store is weak, but
no route reaches the owner lease. Final owner drop can therefore close queued
and blocked work immediately while a worker safely finishes one
already-claimed quantum. The immutable reflection environment belongs to the
active task host rather than either scheduling component.

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
the acyclic resource bundle plus their selected environment and diagnostic
capabilities. A retained profile can therefore keep values, transactions, and
volumes usable without keeping `RuntimeState`, the executor, or the coordinator
alive. Keeping the default profile outside `RuntimeState` avoids a direct
profile ownership cycle while the remaining demand-state transition proceeds.

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

Every scheduler wait token is one shared cell containing identity, a weak
demand-state reference, an optional terminal result, and exact weak work
registrations.
Completion, permanent failure, cancellation, and abandonment publish the
result before coordinator retirement; exact registrations detach and reach
the coordinator only after the terminal cell's lock is released. Polling
checks the cell before and after coordinator lookup, so a waiter racing
publication sees either active state or the terminal result. A terminal token
remains observable after its owner session is dropped; a pending token does
not keep the session alive and reports `Abandoned`, not evaluation failure, if
its owner disappears without publishing a more specific terminal result.
Blocked reflection tasks, deferred producers, and sparks register their stable
work ID and subscription epoch directly with this cell. Only terminal
publication of that exact wait can requeue them; unrelated session task
progress does not.
Reflection task handles own their shared terminal wait cells, so completion,
failure, and cancellation remain observable after the active record and
task-ID index are retired. An unacknowledged failure also leaves one minimal
entry in its producer owner's runtime-ledger bucket until `.task.ack_error`
removes it. Rust clients receive
the corresponding opaque, session-bound `ReasoningFailure` from
`Assembler::drain_reasoning` and may remove the same entry with
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
| current published status and optional protected-query publisher | runtime work coordinator task record |
| transitional task/wait lookup and retirement indexes | demand session reporting store |
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
  -> LazyValue work is claimed, computed, and memoized through its session task
  -> PromisedValue reads one raw assignment, then follows a deferred assignment

arity bridge
  -> arity 0: LazySource::NetComputation expects exposed Data
  -> arity n: FunctionValue attaches n arguments, then expects exposed Data

apply(function, arguments)
  -> builtin or partial-builtin staging
  -> shared FunctionValue curried stage
  -> temporary dictionary-applicability compatibility

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

Computed fixpoints are immutable lazy sources; their ordinary session lazy task
is the sole production owner and wait source. Strict recursive observation is
diagnosed by the common lazy dependency graph, while guarded recursion can
finish at a constructor. Same-session observers share a stable token if
production suspends. Task-owned reflection fixpoints retain their direct owner
check. Assignment-style `PromisedValue` cells hold a raw one-write assignment
rather than a computed result cache.
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
into the target. Blocking remains session task state rather than a cached lazy
error. If another session owns a still-pending gate task, the observer records
a cross-session dependency and polls it once per quiescence pass without
driving its owner. Both sessions belong to the same runtime; another runtime
would reject the containing value before evaluation. Reports retain the
producing session and task IDs; clients decide when to poll again. Terminal
cross-session results remain observable, while a dropped owner is a permanent
producer failure.

## Reflection Task Handles

An opaque reflection task value retains its `EvaluationTaskHandle`: the task
ID, session provenance, and wait token are one lifetime-bearing capability.
Join polls that wait directly. Cancellation first validates that the handle
belongs to the caller's evaluation session, then updates the record addressed
by the same wait token. Transactional status, value, and error observations
remain query-backed and therefore keep their existing snapshot semantics.

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
external owner lease. Current status stays beside that optional publisher in
the coordinator record, while the session reporting store supplies only the
remaining task/wait lookup tail.

Active reflection records retain machines. Every terminal transition first
publishes the shared wait result and records an unacknowledged failure when
needed. It detaches the protected-status update while changing coordinator
state, then applies that update only after scheduler state and mutation
admission have been released. Completion and failure destroy the detached
machine after unlocking. Cancellation similarly invokes the detached
machine's cancellation hook only after unlocking. Phase 8B.1c consolidates
these sequential publications before removing the transitional reporting
indexes.

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

Related assembler, logger, and future IDE sessions register with one
`EvaluationExecutor`. Its fixed worker pool alternates between ready reflection
sessions and optional spark work. The serial pump remains available for exact
foreground dependencies and explicit batch draining. It selects by demand ID
through the coordinator and does not require the external session owner lease;
an ownerless spark context can therefore finish a deferred follower within the
same demand instead of restarting from its original value.

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
