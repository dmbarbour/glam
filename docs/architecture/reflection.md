# Reflection Architecture

Reflection tasks interpret freer-monad effects outside pure value and
interaction-net semantics. This note describes the current implementation;
[`../agent_context/reflection.md`](../agent_context/reflection.md) contains the
regression-sensitive rules.

## Specialization Layers

`reflection.rs` implements the generic task machine and standard effects.
`TaskSpecialization` contributes an additional request enum, private request
tags, host behavior, and transactional host data. Request families remain
reusable by mapping their request type into a specialization.

The reusable `ReflectionEffects` family adds environment lookup, diagnostic
emission, dictionary iteration, lazy-shell value observation, and child-task
operations. `main` defines a broader logger specialization with diagnostic
stream reads and stderr output. Children launched through `.refl_task` receive
only the reusable reflection family.

Core operators merely construct tagged request values. Host operations occur
when the effect task dispatches those requests.

## Persistent Effect Machine

An `EffectTask` retains its continuation, application, alternative, and nested
transaction frames across bounded polls:

```text
effect value
  -> drive until request or result
  -> dispatch request through TaskHost
  -> deliver success, failure, or suspension
  -> preserve frames and return task state to EvaluationSession
```

Standard effects include `r`, `seq`, `alt`, `fail`, `cut`, `fix`, indexed
`get`/`set`, and indexed `reset`/`shift`. User state, including the reset stack,
is ordinary task state. Choice frames, journals, and host queues remain machine
or host bookkeeping.

An outer `cut` provides an optimistic transaction boundary. Alternatives start
from snapshots; losing branches discard changes; a winning outer branch
validates and commits. A host observation can turn later failure into a retry
point. `cut` alone does not: unobservant failure is terminal.

## Reusable Reflection Requests

- `.env Path` reads the active task host's immutable reasoning environment.
- `.log Severity Message` stages a diagnostic in the current transaction and
  publishes it through the session's diagnostic bus only after commit.
- `.dict_items Dict` returns ordered `{key,value}` records.
- `.eval Value` reduces lazy outer shells and returns `ok:WHNF` or `err:Text`.
- `.refl_task Effect` reserves a child handle; launch is commit-ordered inside
  a transaction.
- `.join_task`, `.task_result`, and `.task_error` observe immutable terminal
  task state. `.cancel_task` journals a best-effort cancellation request.
- `.query_task Task` journals one snapshot of mutable task state and returns a
  distinct query handle. After commit, `.query_result Query` returns tagged
  `pending`, `complete`, `error`, `canceled`, or `foreign` data. It fails while
  the query remains uncommitted, preventing a transaction from waiting on the
  request that only its own commit can submit.

The immutable environment conventionally contains assembler-owned `glam`
identity plus client context. `glam.reasoning.role` distinguishes assembler,
logger, and future service sessions. `main` adds process arguments,
reflection-only arguments, and binary-preserving OS environment data. This is
data installed by the client, not command-line policy embedded in the
reflection API.

## Session Scheduling

`EvaluationSession` stores type-erased machines and a FIFO ready queue. A pump
claims a machine under the session mutex, polls it without the mutex, then
records its queued, blocked, or terminal state. Exact lazy wait tokens allow it
to prioritize a known producer chain. Coarse host generations currently wake
state observations.

Foreground evaluation pumps only tasks needed by the lazy value it is trying
to observe. Shared workers may opportunistically poll any ready task. Explicit
reasoning drain continues without a time or step limit, includes newly spawned
tasks, and returns either terminal results or a structured stable-deadlock
report.

## Sources of Tasks

`anno refl:Effect Target` creates a lazy gate. Demand launches `Effect`; unit
success reveals `Target` without forcing it.

The built-in g front end also decorates ordinary module definitions and named
declared-object members with one-shot boundaries. Demand launches one scanner,
which waits for final `refl.*`, then launches each named task in deterministic
order. Guards are stored in reflection heap state under identities derived from
module paths or final object `spec.name`.

## CLI Logger Session

Configured `conf.log` is a reflection task in a separate session sharing the
same executor. Main-only effects expose the incoming diagnostic stream and its
open/closed state. Committed `.log` from the logger or its children goes to a
separate logger-session bus with a default-formatting subscriber rather than
back into that stream.

After assembly reasoning drains, `main` seals the input and lets the logger
finish its own task tree. A logger may use `.log_status` to stop once the queue
is empty and closed. Stable child deadlock, task failure, or a non-unit logger
result fails configured logging and activates the fallback path.
