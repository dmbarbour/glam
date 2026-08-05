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

Every production evaluator entry receives an `EvalContext` borrowed from an
`EvaluationSession`. An `Assembler` and its clones share one internal
`ReasoningSession`, which owns that evaluation session and the assembler's
reflection host. `EvaluationSession` owns active reflection and deferred-value
task records, wait lookup, a reference to the runtime-default annotation
profile, and a persistent ledger of unacknowledged reflection failures.

The runtime-owned `EvaluationWorkCoordinator` owns session registration, the
ready-session queue and de-duplication index, worker fairness, its work
generation, the condition variable used to await work, and stable runtime-local
spark records. Each spark record retains its demand value, dependency,
demand-session index, checked subscription epoch, and a close request while
worker-owned. Parked indexes contain `(work ID, subscription epoch)` rather
than bare IDs. A wake batch is accepted only while the record remains blocked
on both that epoch and the same runtime-local dependency key; stale completion,
session teardown, and reblocking notifications are harmless. The attached
`EvaluationExecutor` owns only worker activation, shutdown, and thread handles.
Workers retain a weak coordinator attachment and claim either a ready session
or spark record from it. Sessions retain only a weak coordinator link. The
immutable reflection environment belongs to the active task host rather than
either scheduling component.

Coordinator mutations participate in the runtime settlement-admission gate
and advance the coordinator's work generation before external wakes. They do
not advance `RuntimeObservationEpoch`: ready-queue churn is not a semantic
heap/input disturbance and must not cause a task to invalidate its own prior
observations.

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

The runtime resource state and immutable default reflection profile remain
sibling roots owned by `EvaluationRuntime`. A profile launcher retains its
host, and that host may retain the resource state; placing the profile inside
that state would therefore create a direct cycle. The sibling arrangement lets
an escaped `EvalContext` keep both resources usable and releases both when the
last context disappears.

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
publication precedes removal of the shared source, so concurrent snapshots may
finish while later observers take the cached path. Terminal deferred-task
records are removed from every session index; their records and machines are
dropped after releasing the registry mutex. Blocked tasks retain both source
and machine. A worker which captured a source before another worker completed
may register one redundant producer after retirement. That producer observes
the canonical lazy cache, terminates, and retires without changing the result.

Every scheduler wait token is one shared cell containing identity, a weak
session owner, an optional terminal result, and exact weak work registrations.
Completion, permanent failure, cancellation, and abandonment publish the
result while holding the active task-registry mutex. The producer record and
indexes retire there; exact registrations detach and reach the coordinator
only after the registry is released. Polling checks the cell before and after
taking the mutex, so a waiter racing publication sees either active state or
the terminal result. A terminal token remains observable after its owner
session is dropped; a pending token does not keep the session alive and reports
`Abandoned`, not evaluation failure, if its owner disappears without
publishing a more specific terminal result. Active registries retain only
unresolved deferred producers and nonterminal reflection tasks. A blocked
spark registers its stable work ID and subscription epoch directly with this
cell. Only terminal publication of that exact wait can requeue it; unrelated
session task progress does not.
Reflection task handles own their shared terminal wait cells, so completion,
failure, and cancellation remain observable after the active record and
task-ID index are retired. An unacknowledged failure also leaves one minimal
session-ledger entry until `.task.ack_error` removes it. Rust clients receive
the corresponding opaque, session-bound `ReasoningFailure` from
`Assembler::drain_reasoning` and may remove the same entry with
`Assembler::acknowledge_reasoning_failure`. Acknowledgement through either
surface leaves the terminal result unchanged.

### Scheduler State Ownership

The session owns only live scheduling state:

| State | Owner |
| --- | --- |
| runnable, running, blocked, or unresolved producer | active session registry |
| queued, worker-owned, or dependency-blocked spark | runtime work coordinator |
| completed, failed, cancelled, or abandoned outcome | shared `EvaluationWaitToken` cell |
| unacknowledged reflection failure | persistent session reporting ledger |
| transactional `.task.status`, `.task.value`, or `.task.error` view | reasoning-store query |

Terminal publication happens while the active registry mutex is held and
precedes record removal. `poll_wait` checks the shared cell before and after
taking that mutex; after the second check, finding an active record means only
that the producer is pending. It does not reinterpret terminal state from a
retained record. A promise assignment observed in the narrow interval before
its publisher acquires the mutex is canonicalized into the same wait cell and
retired by the polling path.

Abandonment describes loss of a session-local producer, not a universal
failure of the value being awaited. Reflection-task handles retain it as a
terminal task outcome. Ordinary lazy demand and host-promise followers may
discard the abandoned producer and install fresh same-runtime work instead.
An unresolved task-owned promise is different: the closing producer session
fulfills it with a structured producer-abandoned failure because that promise
has lost its sole responsible producer. Host promises remain controlled only
by their resolver.

When a lazy or assigned-promise task blocks on another deferred producer, the
session records one strict dependency edge. The graph has at most one outgoing
edge per unresolved producer, so an edge insertion can find a cycle with a
successor walk. A pure deferred-value cycle receives one canonical structured
failure shared by all members; an edge through reflection or another external
producer is not poisoned.

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
Session-owned task followers register one deduplicated weak target; one
resolver publication wakes every such same-runtime session without retaining
it. A coordinator-owned spark does not install that broad target. When it
parks, its stable work ID and current subscription epoch enter the promise
cell's exact-subscriber component. Terminal publication therefore queues only
work still blocked on that promise; the common subscribe-and-recheck protocol
closes assignment races without nested component locks.

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

Active reflection records retain machines. Every terminal transition first
publishes the shared wait result and status-query update under the scheduler
lock, records an unacknowledged failure when needed, and then removes the
active record and task-ID index. Completion and failure destroy the detached
machine after unlocking. Cancellation similarly invokes the detached
machine's cancellation hook only after unlocking.

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
foreground dependencies and explicit batch draining.

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
is parked without occupying a worker and re-advertised after its evaluation
session changes. A session generation check prevents promise resolution racing
with parking from losing the wakeup. Broad disturbance now routes through the
same epoch-and-dependency validator intended for one-shot completion sources.
Promise cells own the source-side exact subscriber set, but sparks do not join
it yet; wait cells likewise remain broad. One relevant change may therefore
still retry other blocked sparks in the same session.
Dropping the session discards its parked sparks; any later registration is
stale and retains neither that session nor its work.
