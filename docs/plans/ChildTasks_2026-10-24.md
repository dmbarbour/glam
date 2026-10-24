# Child Tasks — 2026-10-24

A design note for `api-refl-child-tasks`, a slice of
`api-reflection-machines` in the [runtime API plan](RuntimeApi_2026-10-17.md):
`.child.task`, `.child.status`, `.child.recover`, `.child.interrupt` and
`.child.clear`, and the retirement of `.task.*` and task handles, as the
[Reflection Model](ReflectionModel_2026-10-21.md) decides
(`reflection-tasks-are-machine-tasks`). Drafted 2026-10-24 for the
maintainer's review before building.

## Today (survey, 2026-10-24)

- **Turn machines** are a Rust tree. `Runtime.root: MachineState` holds a
  stack, a stash, `locals: HashMap<Key, HandleValue>`, `children` (each
  `Idle(MachineState)` or `Forked`) and `failed` (`api/turn.rs:273-303`).
  A turn moves its machine's state out by value and writes it back as it
  returns; one driver at a time follows from ownership and from the
  runtime being held by one thread (`machines-form-a-tree`). A cut
  snapshots the state, children included, and restores it on abort.
  Ports, wiring and environment shares live apart, in the transactional
  store's event state, keyed by machine path (`api/runtime/ports.rs`).
- **Reflection tasks** are coordinator tasks on the runtime's workers
  (`EffectTask`, `reflection/machine.rs`), with no machine identity: an
  `EvaluationTaskId`, local state of their own that starts at `{}` and dies
  with them (`branch.state`), commits through a `TaskHost`, and a failure
  ledger that `acknowledge_reasoning_failure` and `.task.ack_error` empty.
  `.task.new` returns an opaque handle over an `EvaluationTaskHandle` and a
  status query in the runtime volume (`reflection/requests.rs:706-796`).
- **Users of `.task.*`**, outside tests, are compiler-generated alone: the
  reflection annotator launches a boundary's scanner (or `meta.refl`) with
  `.task.new` and records its handle as `[guard, 'claim]`; the scanner
  launches each `refl.*` entry with `.task.new (op =>> .r ())` and records
  `[guard, 'tasks]` (`g_syntax/compiler_values.rs:930-1219`). Nothing
  joins them: a definition waits only for the boundary to launch, and
  failures reach the CLI through the ledger, as the quiescence report's
  `task_failures`, which fail a batch (`bin/glam/batch.rs:295-301`). No
  sample or prelude uses `.task.*`; about 200 test sites do.

## The question: where a tasked machine's state lives

A task runs on a worker while the runtime's holder thread may take turns,
so a machine driven by tasks cannot live by value inside the holder's
tree. Its state, local state persisting from task to task, its error flag,
its queue and its children tasks' machines, must be reachable by workers,
and by turns that reach it (`m.recover`, `m.failed`, `m.machines`, and a
turn waiting for a busy machine).

Options:

1. **One shared, flat machine table.** Every machine, turn-driven or
   tasked, is an entry keyed by path in the runtime's shared resources; a
   driver checks an entry out and in; children are entries below a path.
   The end state the model describes, and what `.child.call` wants, since
   its staging is per entry. It rewrites the turn tree: cut snapshots of
   descendants, forks, `prune`, `machines`.
2. **Delegated subtrees (recommended).** A machine that is ever tasked
   becomes a shared cell, as a fork's subtree leaves its parent:
   `Child::Tasked(cell)` beside `Idle` and `Forked`, with the cells also
   indexed by path for workers. A cell holds the machine's state, its error
   flag, and whether a driver holds it. Turn-only machines stay inline,
   untouched. A turn that reaches a tasked child waits while it is busy, as
   the model says, and otherwise takes its state for the turn; a task's
   children it tasks are cells too. A cut's snapshot keeps the cell, not
   its contents: the child's state is its own tasks', which run only once
   the cut that queued them commits. Option 1 can follow, entry by entry,
   when `.child.call` needs staging.
3. **A separate task-machine table**, disjoint from the turn tree, which
   turns cannot enter. The smallest step, but `m.recover`, `m.failed` and
   `m.machines` would see two trees, against the model.

## Proposed slices

Each with tests, the full gate, and a commit.

1. `child-task-machines` — the cells and machine identity: a task runs on
   a machine path, its `.get`/`.set` state is the machine's local state,
   taken when the task starts and written back when it ends, as a turn's
   is, cuts within the task still undoing their own. Annotation tasks run
   on anonymous machines `refl/<n>`, `n` a `NonZeroU64`, cleared when
   their task ends (pulled forward from `api-refl-annotations`, since
   `.child.task` needs its caller's machine). `.env` keeps today's
   committed reading.
2. `child-task-queue` — `.child.task Key Op` writes `{op, reply}` to the
   child's task input, a port named by an abstract global path, and
   returns the reply's promise. A committed write to an idle machine's
   task input starts its task on a worker; the head item stays in the
   buffer while it runs and is claimed when it ends, so `.child.status`
   (`'busy` while the buffer holds a task, `'error` while flagged,
   `'idle` otherwise) is an observed read that wakes a cut when it
   changes. A task's result fulfills its reply; a failure breaks it,
   flags the machine, and stops the queue.
3. `child-task-control` — `.child.recover` (clear the flag and the queue,
   breaking the queued replies with "recovered before it ran"),
   `.child.interrupt Key Reason` (cancel the running task, break its reply
   with `Reason`, flag the machine), `.child.clear` (empty the child and
   its descendants, breaking the replies of the tasks it drops).
4. `child-task-migration` — the compiler's annotator and scanner move to
   `.child.task`: the scanner as the boundary task's child, each `refl.*`
   entry as the scanner's child under its key, and the tests from
   `.task.*` to `.child.*`. Failures reach the CLI as flagged machines in
   place of the ledger's `task_failures`, until `api-settlement` reports
   them.
5. `child-task-retirement` — `.task.*`, `TaskHandleCell`, the status
   queries, `acknowledge_reasoning_failure` and the failure ledger retire,
   with their docs.

## Questions for the maintainer

1. Option 2, delegated subtrees, for now, with option 1 later if
   `.child.call` needs it?
2. Annotation machines (`refl/<n>`) first, pulled forward from
   `api-refl-annotations`?
3. A task's local state as the machine's, taken at the task's start and
   written back at its end, as a turn's is, rather than staged
   transactionally? (`.child.call` would stage it later.)
4. Keys for the compiler's tasks: the scanner as the boundary task's child
   `'scanner`, each `refl.*` entry as the scanner's child under its own
   key?
5. Until `api-settlement`, may flagged task machines take the ledger's
   place in the quiescence report, so the CLI fails a batch on them as it
   does on `task_failures` today?
