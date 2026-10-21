# Reflection Model — 2026-10-21 (draft)

`api-reflection-model`, step 14 of the [runtime API plan](RuntimeApi_2026-10-17.md):
how the reflection API, as `refl.*`, `anno refl:` and their relatives
present it, maps to machines. A design step without code, ahead of the
settlement it shapes; it ends in an ADR, and its implementation is a later
step, after `api-effect-providers`. This draft is for discussion: the open
questions are at the end.

## What the machine model already decided

Most of the mapping follows from decisions already made:

- **Machines** (`runtime-api-turns-on-machines`, `machines-form-a-tree`):
  named by paths of keys, each with a stack, local state and children,
  and no lifecycle: a machine starts empty, and `clear` returns it there.
  A turn reaches its descendants; nothing waits for a machine.
- **Cuts** (`turn-cuts-bound-transactions-and-search`): one boundary for
  a transaction and a backtracking search, as reflection's `.cut` already
  is. Failure (`Backtrack`) is explicit, and pushback (`Retry`) waits for a
  change in what the cut observed.
- **Errors** (`machine-errors-mark-and-recover`): an error that ends a turn
  flags its machine until a host or parent recovers it; observing the
  error never clears the flag.
- **Ports** (`machines-communicate-through-wired-ports`): named, wired
  from outside by the parent, buffered in transactional state; diagnostics
  gather at the root by default wiring.
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

**A reflection task is a machine that runs a program.** A program is an
effect value, which the runtime interprets on its workers, where a turn is
Rust code that a thread runs. Both drivers act on the same machine state
(stack, local state, children, ports, environment share, error flag)
through the same atomic operations, and a cut means the same thing to
both. Reflection's effects are the machine's operations, spelled as
effects; the task API becomes operations on child machines.

### Programs

- **Launching.** `.task.new Key Effect` launches `Effect` as the program
  of the child machine `Key`. A Rust turn does the same with
  `m.launch(path, n)`, the program on top of its stack, so the two APIs
  stay one model.
- **One driver at a time.** A machine runs a program or takes turns,
  never both: running joins idle, failed and forked as a status, and a
  turn, `give`, `take` or `fork` on a running machine fails, as on a
  failed one. Waiting for a program to end is an explicit join, visible
  to settlement.
- **State.** A program's local state is its machine's local state, so
  `.get` and `.set` are the machine's, and a recovered machine shows the
  state its program left. The interpreter's frames (continuation, cuts,
  alternatives) are the program's own, held while it runs.
- **Steps.** Each request outside a cut is one atomic step, as each action
  in a turn is; a cut groups requests into one transaction. A program
  that waits retries its current step, as a machine always does.
- **Ending.** A program that returns pushes its result onto its machine's
  stack and leaves the machine idle, as a fork leaves its result. One that
  ends in error, by a halt, an unhandled failure or a kill, flags its
  machine and keeps its failure for joins.
- **Absence.** A machine with nothing in it is absent. Taking a program's
  result leaves its machine absent if the program left no local state and
  no children, so finished tasks vanish without a lifecycle.

### Naming

- **`refl.*` tasks** run under `refl`, on a machine named for their
  reflection boundary, the module or declared object instance, and the
  key of their `refl.*` entry: `refl/<boundary>/<key>`.
- **Annotation tasks** (`anno refl:`, `anno meta_refl:`) are anonymous,
  so the runtime names each a fresh key under `refl/anno`. The annotated
  lazy takes the program's result, or its failure, and the runtime clears
  the machine's local state, so it vanishes unless the program left
  children running.
- **Children of tasks** are children of the task's machine, named by
  the key `.task.new` gives.
- **Handles retire.** A task names its children by key; it cannot reach
  another task's machine, as a turn cannot reach beyond its descendants.
  Communication between tasks is ports, promises and volumes.

### The effects, mapped

| Today | As machines |
|---|---|
| `.r`, `.seq`, `.alt`, `.fail`, `.cut`, `.fix`, `.reset`, `.shift` | Unchanged; `.cut` is the machine's cut, `.fail` its `Backtrack`, pushback its `Retry`. |
| `.get`, `.set` | The machine's local state. |
| `.heap.*`, `.vol.*`, `.promise.*` | Unchanged: the machine's shared-state operations. |
| `.env Path` | The machine's composed environment, observed. New: `.env.set`, `.env.mine`, `.env.clear`, its own share, as `set_env_mixin`, `get_env_mixin` and `clear_env_mixin`. |
| `.log Sev Msg` | A write to the machine's `diagnostics` output, enriched with severity and origin as today. |
| `.read_log` (logger) | A read of the machine's `diagnostics` input. |
| `.write_stderr Bin` (logger) | A write to a `stderr` output, which the CLI wires to a machine that writes stderr. |
| — | New port effects: `.read Port`, `.write Port V`, `.ask Port Request` (a write of `{request, reply}` returning the promised reply), `.pending`, `.wire Source Destination`, `.unwire Source Destination`, endpoints as values (`in:Port`, `out:Port`, `child:[Key, Port]`). |
| `.task.new Eff` | `.task.new Key Eff`: launch on the child `Key`. Transactional: within a cut, the launch commits with it. |
| `.task.join H` | `.task.join Key`: wait for the child's program to end, then take its result, or fail with its failure, with the `{task:…}` context as today. The flag stays. |
| `.task.status H` | `.task.status Key`: the child's status (`'absent`, `'idle`, `'running`, `'forked`, `failed:Diagnostic`), observed. |
| `.task.value H`, `.task.error H` | Subsumed: `.task.join` takes the value, `.task.status` shows the failure. |
| `.task.ack_error H` | `.task.recover Key`: clear the child's flag, keeping its state. |
| `.task.cancel H` | `.task.cancel Key`: stop the child's program, flagging the machine, whose state it may have left half-done. |
| — | New: `.task.clear Key`, empty a child and its descendants; `.task.give Key Values` and `.task.take Key N`, as `m.give` and `m.take`. |
| `.exit.success`, `.exit.error` | Retire: a machine waiting on input is idle. |
| — | New: `.todo.add Key`, `.todo.del Key`, `.todo.list`, the machine's obligations (below). |
| `.eval`, `.meta.inspect`, `.dict_items`, `.env 'glam.origin.inspect` | Unchanged: evaluation and inspection, independent of machines. |

The CLI's own families (`.read.*`, `.write.*`, `.case`, `.token.*`) and
the macro runner's are providers' effects within turns
(`api-effect-providers`), not programs; nothing here changes them.

### Obligations

`.exit` also said that a machine, locally, had nothing outstanding. That
role passes to an explicit checklist, as the maintainer proposed on
2026-10-19: a set of keys in the machine's state beside its stack.
`.todo.add Key` adds one, `.todo.del Key` removes it, and `.todo.list`
reads the set; a turn has `m.todo_add`, `m.todo_del` and `m.todos`. The
set is transactional machine state, the machine's alone; a key describes
itself. Settlement reports the machines whose sets are not empty.
Reflection over other machines' sets waits for reflection over the
process network.

### Launch paths

- **`anno refl:Eff Target`.** The first demand launches `Eff` under
  `refl/anno`; the annotated lazy waits on the program's end, and exposes
  `Target` once it returns unit, as today.
- **`anno meta_refl:F Carriers`.** The same, with the program's result as
  the carriers' metadata.
- **`refl.*` boundaries.** A boundary launches its scanner, `meta.refl`,
  on the boundary's machine, `refl/<boundary>`, once; the machine's
  presence replaces the heap claim that launches it once today. The
  scanner launches each `refl.*` entry on a child named by its key.
  An entry that ends with a result other than unit fails, as today; a
  `refl.*` server that never returns is natural.
- **`conf.log`.** The CLI's `logger` machine runs `conf.log` as its
  program, wired from the root's diagnostics input and to a `stderr`
  output that a CLI machine drains. With `.read_log` and `.write_stderr`
  as ports, it needs no provider, and `MainEffects` retires. Its fallback
  is `logger/fallback`, as `api-ports` planned.
- **`conf.ide`** (not built). An `ide` machine under the root that takes
  over logging by rewiring diagnostics, and whose interaction, terminal
  or sockets, reaches Rust through ports that CLI machines serve.
- **Isolated searches** (`conf.cli`, macros, tokens): turns with
  providers, as `api-effect-providers` plans; no program runs them.

### What counts as unresolved

At quiescence every machine waits. A wait is *idle* when nothing is owed:

- reading an input whose buffer is empty;
- an outermost cut awaiting a change in what it observed: the heap, a
  volume, the environment, a status, its pending inputs;
- a returned program, or a machine with none;
- a fork waiting in an outermost cut, which a batch ends cleanly.

It is *unresolved* when something is owed that no one will provide, and
settlement reports it:

- an evaluation waiting on a promise, directly or through the lazies that
  wait on it, with the promise and, where known, the machine holding its
  resolver, including a machine waiting on a promise its own cut
  resolves (the `api-settlement` candidate);
- a join waiting on a child whose program waits idle;
- a machine in error, with its failure;
- a non-empty obligation set, with its keys;
- the work settlement kills, a deadlocked cycle among them.

A request no one answered is unresolved only when something waits on its
reply; an unread request that nothing awaits owes nothing, unless its
server keeps an obligation for it.

### What retires

Task handles and their status queries; `.task.value`, `.task.error` and
`.task.ack_error`; the failure-acknowledgement ledger
(`Assembler::acknowledge_reasoning_failure`); exit votes (`ExitIntent`);
`MainEffects` and the logger's host; the heap claim of reflection
boundaries; and abandonment as owner-session loss, which becomes clearing
a machine with its children.

## Open questions

1. **Names.** `.task.*` for operations on child machines, or `.machine.*`
   / `.child.*`, since a child need not run a program? And the port
   effects' names: `.read` and `.write` collide in spirit with the CLI
   providers' `.read.*` and `.write.*`; `.port.read` and `.port.write`
   avoid that.
2. **Boundary identity.** `refl/<boundary>` needs a key per module and per
   declared object instance; `spec.name` alone collides for repeated
   instances of one object. A content identity, a counter per name, or a
   key the front end derives?
3. **Annotation machines.** A fresh runtime-chosen key under `refl/anno`,
   or nesting under the boundary or definition that demanded it? And
   should the runtime clear an annotation machine whose program left
   children, or keep it present until they vanish, as drafted?
4. **Cancel.** Stop and flag, as drafted, so the canceller recovers or
   clears next; or stop and clear at once, since the canceller decided?
5. **Join.** Should `.task.join` take the result, leaving the machine
   absent when nothing else is left (drafted), or copy it, so several
   readers can see it, with `.task.take` to remove it?
6. **Completion as a promise.** Should `.task.new` also return a promise
   of the program's result, so a machine outside the parent's reach can
   wait on it, a capability in the spirit of `requests-travel-on-ports`?
   It would replace what handles offered across tasks.
7. **Programs and turns.** A turn on a running machine fails, as drafted;
   or should a parent's turn be able to stop a program and enter its
   machine, as recovery enters a failed one?
8. **Rust's view.** `m.launch` beside `m.fork`: should a turn also join a
   program (`m.join_program(path)`), or wait on its completion promise
   (question 6) by evaluation?
9. **Port arities.** `m.read(port, n)` pushes `n` values; should `.read
   Port` take one value, or a count and return a list, which would force
   nothing but needs a list?
10. **Wiring from reflection.** May a program wire its own ports and its
    children's, as a turn may (drafted: yes), and set the default rule
    (`set_default_wiring`)?
11. **Assembler and macro sessions.** Today they run reflection with their
    own role environments (`'assembler`, `'macro`). Do their annotation
    tasks run under `refl/anno` with the `refl` machine's environment, or
    under a machine per role, `refl/<role>`, whose share binds the role's
    `glam` entry?
12. **Kills.** Settlement kills deadlocked work: should a killed program
    flag its machine like any error, so the report lists it, or clear it,
    since the report already names what it killed?
