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

## The model, clarified (maintainer, 2026-10-24)

The draft asked where a tasked machine's state lives so that turns and
tasks share one tree, offering a shared flat table, delegated subtrees or a
separate table. That question does not arise: each anonymous reflection
task is its own root for child tasks (maintainer). Its machine is
anonymous, unnamed in listings and unrecoverable, and nothing joins it, so
its children, the only machines tasks create, are reachable only from
tasks within its subtree. No turn enters, lists, recovers or joins them,
and the turn tree is unchanged.

So the machines below an anonymous root live on the reflection side, in
the runtime's shared resources where workers reach them, keyed by path
under the root. Their ports, a task input among them, live in the
transactional port state like any machine's, under the root's anonymous
key, which turns' listings leave out.

A task's commit carries only the store journal today; ports travel in a
turn's event journal (`api/runtime.rs:455`, `commit_reflection`). The task
queue is a port, so reflection transactions first gain the port journal,
the groundwork of `api-refl-ports`, pulled forward.

## Proposed slices

Each with tests, the full gate, and a commit.

1. `task-port-journal` — reflection transactions carry the port and wiring
   journal beside the store journal, and commit both, as a turn's cut
   does; a task's port reads are observed, so a cut that read too few
   waits for arrivals. The groundwork of `api-refl-ports`, whose `.port.*`
   effects can follow on it.
2. `child-task-machines` — anonymous roots and the machines below them: a
   reflection task runs on a machine path, annotation tasks on anonymous
   roots `refl/<n>`, `n` a `NonZeroU64`, whose local state clears when
   their task ends. A child machine's local state persists from task to
   task: a task takes it when it starts and writes it back when it ends,
   as a turn's is, cuts within the task still undoing their own.
   Listings of `refl` leave anonymous roots out.
3. `child-task-queue` — `.child.task Key Op` writes `{op, reply}` to the
   child's task input, a port named by an abstract global path, and
   returns the reply's promise. A committed write to an idle machine's
   task input starts its task on a worker; the head item stays in the
   buffer while it runs and is claimed when it ends, so `.child.status`
   (`'busy` while the buffer holds a task, `'error` while flagged, `'idle`
   otherwise) is an observed read that wakes a cut when it changes. A
   task's result fulfills its reply; a failure breaks it, flags the
   machine, and stops the queue.
4. `child-task-control` — `.child.recover` (clear the flag and the queue,
   breaking the queued replies with "recovered before it ran"),
   `.child.interrupt Key Reason` (cancel the running task, break its reply
   with `Reason`, flag the machine), `.child.clear` (empty the child and
   its descendants, breaking the replies of the tasks it drops).
5. `child-task-migration` — the compiler's annotator and scanner move to
   `.child.task`: the scanner as the boundary task's child, each `refl.*`
   entry as the scanner's child under its key, and the tests from
   `.task.*` to `.child.*`. Failures reach the CLI as flagged machines in
   place of the ledger's `task_failures`, until `api-settlement` reports
   them.
6. `child-task-retirement` — `.task.*`, `TaskHandleCell`, the status
   queries, `acknowledge_reasoning_failure` and the failure ledger retire,
   with their docs.

## Defaults, open to the maintainer

Taken unless the maintainer says otherwise (2026-10-24):

1. A child machine's local state persists from task to task, as the
   Reflection Model says, taken at a task's start and written back at its
   end rather than staged transactionally; `.child.call` stages it later.
2. The compiler's keys: the scanner as the boundary task's child
   `'scanner`, each `refl.*` entry as the scanner's child under its own
   key.
3. Until `api-settlement`, flagged task machines take the failure ledger's
   place in the quiescence report, so the CLI fails a batch on them as it
   does on `task_failures` today.
