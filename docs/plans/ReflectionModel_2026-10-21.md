# Reflection Model — 2026-10-21

`api-reflection-model`, step 14 of the [runtime API plan](RuntimeApi_2026-10-17.md):
how the reflection API, as `refl.*`, `anno refl:` and their relatives
present it, maps to machines. A design step without code, ahead of the
settlement it shapes. Drafted and decided with the maintainer on
2026-10-21; the decision is `reflection-tasks-are-machine-tasks`. Its
implementation is `api-reflection-machines`, after `api-effect-providers`.

## What the machine model already decided

Most of the mapping follows from decisions already made:

- **Machines** (`runtime-api-turns-on-machines`, `machines-form-a-tree`):
  named by paths of keys, each with local state and children, and no
  lifecycle: a machine starts empty, and `clear` returns it there. A turn
  reaches its descendants; nothing waits for a machine.
- **Cuts** (`turn-cuts-bound-transactions-and-search`): one boundary for
  a transaction and a backtracking search, as reflection's `.cut` already
  is. Failure (`Backtrack`) is explicit, and pushback (`Retry`) waits for a
  change in what the cut observed.
- **Errors** (`machine-errors-mark-and-recover`): an error that ends a turn
  flags its machine until a host or parent recovers it; observing the
  error never clears the flag.
- **Ports** (`machines-communicate-through-wired-ports`): named, wired
  from outside by the parent, buffered in transactional state;
  diagnostics gather at the root by default wiring.
- **Environment** (`machines-inherit-an-environment`): each machine binds
  a share, and its environment is its ancestors' shares composed;
  reflection tasks are logical children of a `refl` machine under the
  root.
- **Requests and capabilities** (`requests-travel-on-ports`,
  `promise-resolution-commits-with-its-cut`, `api-volume-cells`): requests
  travel on ports with resolvers; volumes and resolvers are the
  first-class capabilities.
- **Quiescence** (`quiescence-reports-unresolved-state`): a machine waiting
  on input or a heap change is idle; settlement reports what remains
  unresolved, and machines outlive it.
- **Providers** (`effect-providers-answer-within-turns`): Rust code
  answers effects only within turns, on their threads; background work
  reaches Rust only through ports that machines serve.

## The model

**A reflection task is a task on a machine:** an effect value that the
runtime runs on its workers, where a turn is Rust code that a thread runs.
Both drivers act on the same machine through the same effectful
capabilities: local state, children, ports and wiring, the environment
share, obligations, and the error flag. A cut means the same thing to
both. The rest of a Rust machine, its data stack with its push and pop
helpers, `give`, `take` and `run`, serves Rust's integration with
evaluation (the stack keeps values between turns and saves allocations),
and has no glam counterpart (maintainer).

### Tasks

- **Tasking a child.** `.child.task Key Op` queues `Op` on the child
  `Key` and returns, at once, a promise that `Op`'s result fulfills or its
  failure breaks. A caller may discard the promise, or await it to
  synchronize, even when it promises only unit. A child appears on first
  use, as machines do.
- **The queue is a port.** Tasking writes `{op, reply}` to the child's
  task input, a port named by an abstract global path, so tasks queue as
  any input does, in commit order, and the child runs them one after
  another. Several tasks compose a pipeline. Within a cut, tasking is a
  write like any other: it reaches the child when the cut commits, so a
  cut awaiting the promise of a task it queued can only wait.
- **Status.** `.child.status Key` reads `'busy` while a task is queued or
  running, `'idle` otherwise, and `'error` while the child is flagged; a
  child nothing has touched reads `'idle`. A child's status changes only
  by its own tasks finishing, from `'busy` to `'idle` or `'error`, until
  its parent acts on it again. To await a status, a parent reads it within
  a cut and fails until it reads what it wants; the cut wakes when it
  changes.
- **Failure.** A task that fails breaks its promise and flags its machine,
  and the queue stops. `.child.recover Key` clears the flag and the queue,
  whose remaining tasks' promises break; the queue never resumes, and what
  happens next is the parent's to decide (maintainer). The machine's local
  state is as the failure left it, for the parent's next tasks to inspect
  and repair.
- **Why a promise broke.** A promise that recovery, clearing or an
  interrupt breaks says so, better than a lost resolver's "resolver lost"
  (maintainer): that its machine was recovered from an error, or cleared,
  before the task ran, or the interrupt's reason for one it stopped.
- **Interrupts.** `.child.interrupt Key Reason` stops the running task,
  breaks its promise with `Reason`, and flags the machine, whose state it
  may have left half-done. Settlement's kills are interrupts: each machine
  a kill stops is flagged, and none is cleared, so each stays recoverable
  (maintainer).
- **Clearing.** `.child.clear Key` empties a child and its descendants,
  breaking the promises of the tasks it drops. Nothing else clears a
  machine, and the representation of a machine with nothing in it, absent,
  is an economy, not a status (maintainer): such a child reads `'idle`.
- **Calls.** `.child.call Key Op` is the one synchronous operation, a
  transactional remote procedure call (maintainer): it waits until the
  child has finished what was committed to it before the caller's
  transaction began, which is pushback while the caller's snapshot shows
  the child busy, then runs `Op` on the child within the caller's
  transaction and returns its result. The child's changes commit or abort
  with the caller; a task committed to the child meanwhile conflicts with
  it. Outside a transaction, a call is a plain remote procedure call
  (maintainer): the caller waits while the child runs `Op` after what was
  queued before it, step by step as any task runs, and the call returns
  `Op`'s result or fails with its failure. Within a cut, it is the
  program-side counterpart of a Rust synchronous turn, which already joins
  the cut; it is explicit rather than inferred from a caller that awaits a
  status, queues a task and awaits its result.
- **One driver at a time.** A machine runs its queued tasks or takes
  Rust turns, never both. A Rust turn, `give`, `take` or `fork` on a busy
  machine waits for it as a call does, pushback within a cut; on a flagged
  machine it fails, as today.
- **Absence.** A machine with nothing in it is absent, so a machine whose
  tasks left no local state, no children and no buffered input vanishes
  without a lifecycle.

### Annotation tasks

`anno refl:` and `anno meta_refl:` tasks are anonymous from the
assembler's point of view. Each runs on a machine under `refl`, keyed by
an abstract number, a `NonZeroU64` (maintainer). Nothing can task that
machine again, so when its task ends its local state is cleared, as if the
task ended with a clear; children it left keep running.

Provenance is the front end's: a compiler that wants a task to know where
it comes from adds it to the task's environment, which the task reads with
`.env`. A reflection boundary, its claim of `[name, 'meta, 'refl]` in the
heap, and the scanner it launches are the behavior of the tasks the front
end writes, not machine names (maintainer). The scanner runs as the
boundary task's child, and each `refl.*` entry as the scanner's.

### The effects, mapped

| Today | As tasks on machines |
|---|---|
| `.r`, `.seq`, `.alt`, `.fail`, `.cut`, `.fix`, `.reset`, `.shift` | Unchanged; `.cut` is the machine's cut, `.fail` its `Backtrack`, pushback its `Retry`. |
| `.get`, `.set` | The machine's local state, which persists across its tasks. |
| `.heap.*`, `.vol.*`, `.promise.*` | Unchanged: shared-state operations. |
| `.env Path` | The machine's composed environment, observed. New: `.env.set`, `.env.mine`, `.env.clear` for its own share, as `set_env_mixin`, `get_env_mixin` and `clear_env_mixin`. |
| `.log Sev Msg` | A convenience that writes an enriched diagnostic to the machine's `diagnostics` output. |
| `.read_log` (logger) | `.port.read 'diagnostics N`, the logger machine's input. |
| `.write_stderr Bin` (logger) | `.port.write 'stderr [Bin]`, on a port the CLI wires to a machine that writes stderr. |
| — | Ports: `.port.read Port N` (a list of `N` values; pushback while fewer are buffered), `.port.write Port List`, `.port.available Port` (an observed count), `.port.pending` (inputs holding values), `.port.ask Port Request` (writes `{request, reply}`, returns the reply's promise). |
| — | Wiring: `.wire.add Source Destination`, `.wire.del Source Destination`, `.wire.list`, `.wire.defaults Bool`, over the machine's own ports and its children's; endpoints `in:Port`, `out:Port`, `child:{key:Key, port:Port}`. |
| `.task.new Eff` | `.child.task Key Op`, returning the result's promise. |
| `.task.join H`, `.task.value H` | Awaiting the promise. |
| `.task.status H`, `.task.error H` | `.child.status Key`; the failure is the broken promise's. |
| `.task.ack_error H` | `.child.recover Key`. |
| `.task.cancel H` | `.child.interrupt Key Reason`. |
| — | `.child.clear Key`; `.child.call Key Op`. |
| `.exit.success`, `.exit.error` | Retire: a machine waiting on input or for tasks is idle. |
| — | Obligations: `.todo.add Key`, `.todo.del Key`, `.todo.list`. |
| `.eval`, `.meta.inspect`, `.dict_items`, `.env 'glam.origin.inspect` | Unchanged: evaluation and inspection. |

The CLI's own families (`.read.*`, `.write.*`, `.case`, `.token.*`) and
the macro runner's are providers' effects within turns
(`api-effect-providers`), not tasks, and no reflection: `conf.cli` and
macros are pure evaluation, ideally handled by pure functions
(maintainer).

### Rust's counterparts

Rust gets no task operations (maintainer): a host that wants a machine to
run glam work models it, as a turn reading requests from a port and
interpreting them into effects (`api-effect-providers`), and interrupts as
shared state its turns observe. `m.fork` remains concurrency for Rust code,
as a task is for glam code; each drives a machine, one at a time.
`m.recover`, `m.machines` and `m.failed` see tasks' machines as any
others, with `'busy` among the statuses, and a turn has `m.todo_add`,
`m.todo_del` and `m.todos` for its own obligations.

### Obligations

`.exit` also said that a machine, locally, had nothing outstanding. That
role passes to an explicit checklist, as the maintainer proposed on
2026-10-19: a set of keys in the machine's state. `.todo.add Key` adds
one, `.todo.del Key` removes it, and `.todo.list` reads the set. The set
is transactional machine state, the machine's alone; a key describes
itself. Settlement reports the machines whose sets are not empty.
Reflection over other machines' sets waits for reflection over the
process network.

### Which work is reflection

From an inventory of the sessions and effect runs (2026-10-21):

- **Reflection:** annotation tasks (`anno refl:`, `anno meta_refl:`)
  with their descendants, which include the front end's boundaries,
  scanners and `refl.*` tasks, all under `refl`; and the configured
  logger, whose fork's turn runs `conf.log` as an effect on the CLI's
  `logger` machine (`api-effect-providers`), with the same effects, its
  reads and its stderr as ports, as `conf.ide` will on an `ide` machine.
- **Pure evaluation:** the assembler's demand session, `conf.env`,
  completion scripts and `asm.result`; and macro expansion, `conf.cli` and
  token parsing, which run isolated searches whose handlers never commit,
  and which become turns with providers. Macro sessions belong to the
  pure front end, not to `refl` (maintainer): the `'macro` reasoning role,
  host, profile and bus retire.
- **Annotations within pure work.** Every annotation task already runs
  with the runtime's default profile, whichever session demanded it, as
  `refl`'s children would. An annotation demanded inside a pure search
  runs as reflection, outside the search's branch journal: reflection
  observes, and backtracking a pure search does not undo an observer, as
  it does not undo evaluation's caches.

### What counts as unresolved

At quiescence every machine waits. A wait is *idle* when nothing is owed:

- reading an input that holds too few values;
- an outermost cut awaiting a change in what it observed: the heap, a
  volume, the environment, a status, its pending inputs;
- a machine with no task, or with tasks only queued behind one of these;
- a fork waiting in an outermost cut, which a batch ends cleanly.

It is *unresolved* when something is owed that no one will provide, and
settlement reports it:

- an evaluation waiting on a promise, directly or through the lazies that
  wait on it, with the promise and, where known, the machine holding its
  resolver, including a machine waiting on a promise its own cut
  resolves, or on a task it queued in that cut;
- a machine in error, with its failure;
- a non-empty obligation set, with its keys;
- the work settlement kills, a deadlocked cycle among them.

A request no one answered is unresolved only when something waits on its
reply. A build or batch waits on reflection because reflection can report
errors, but it may end with tasks still waiting on ports or state once
nothing is unresolved; a deadlock is itself an error (maintainer).

### What retires

Task handles and their status queries; `.task.*`; the
failure-acknowledgement ledger (`Assembler::acknowledge_reasoning_failure`);
exit votes (`ExitIntent`, `.exit.*`); `MainEffects` and the logger's host;
the `'macro` reasoning role; and abandonment as owner-session loss, which
becomes clearing a machine with its descendants.

## Notes for `api-reflection-machines`

- `.child.call` needs a child's machine state within the transactional
  store, as a Rust synchronous turn's is within its parent's state; tasks'
  machines must therefore keep their state where a caller's transaction
  can stage it.
- A build today drains the runtime's whole background reflection domain
  and fails on any unfinished task, so a `refl.*` server that never
  returns would fail every later build; it should wait only until nothing
  is unresolved.
- The CLI's and macro runner's families install `.promise.*` even where
  the heap is hidden, and `conf.cli`'s reads the reflection environment
  with `.env`. Acceptable as bridge behavior, but those families should
  carry no reflection effects and no promises, which they have only from
  implementation hiccups (maintainer); `Design.md` now types `conf.cli`
  without `Refl`. The cleanup belongs to `api-effect-providers`, where
  these families become providers, ideally pure functions.
- Documentation drift found by the inventory: `.exit` is exposed to every
  coordinator-launched task, not only the logger; a comment in
  `api/assembly.rs` says annotations run in the macro session, and one in
  `reflection/search.rs` lists interaction-net construction among isolated
  searches. These go with the code they describe.
