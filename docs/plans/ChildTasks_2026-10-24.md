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
   Built 2026-10-24.
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
   Slices 2 and 3 built together, 2026-10-24: every task without a machine
   is an anonymous root keyed by its task id (`anonymous_root`), so
   annotation and `.task.new` tasks task children without a change to
   their activation; only a task a child's queue starts carries a
   `TaskMachine`. The runtime's `MachineHost` keeps local state and a
   running bit per child; flags are a record in the event state whose
   journal validates its reads, as the share record's store values do for
   shares. A queued task starts at the commit that queued it (a
   `ReflectionUpdate::Wake`) and the finishing task starts the next, both
   outside the mutator; a failing task's state is the one before its
   outermost cut. A child's failures are acknowledged at launch, so they
   never reach the failure ledger, and reports of flagged machines wait for
   `child-task-migration`. Not yet: a child task that panics leaves its
   item queued and its machine running; interrupts and clearing come with
   `child-task-control`. Tests: queueing, state between tasks, order,
   failure flags, status through a gated task, a failed cut's task never
   running, one task at a time (driven directly), and listings.
4. `child-task-control` — `.child.recover` (clear the flag and the queue,
   breaking the queued replies with "recovered before it ran"),
   `.child.interrupt Key Reason` (cancel the running task, break its reply
   with `Reason`, flag the machine), `.child.clear` (empty the child and
   its descendants, breaking the replies of the tasks it drops).
   Built 2026-10-24. Each is a transactional edit, within a cut or
   committed at once: an interrupt takes the head of an unflagged queue,
   breaks its reply and sets the flag; a recovery clears the flag and takes
   the whole queue, observing its end, so a task queued meanwhile
   conflicts; a clear takes every queue at or below the child (the heads
   of unflagged queues "cleared while the task ran", the rest "before"),
   with the tasks its own transaction queued there, and drops port state,
   shares, environment and flags as a turn's clear does. Recovering and
   clearing wait while the child is busy, as `.child.call` does
   (maintainer, the same day): the edit fails having observed the child,
   so a cut waits for it to change, unless an alternative succeeds, and a
   task outside a cut waits; `.try` reports them `'fail`. A clear still
   stops its descendants' running tasks. At commit, `ReflectionUpdate::Interrupt` and `Clear` stop the
   runs: a run is named by its item's sequence in the queue
   (`TaskMachine::sequence`), so a stopped task's end claims nothing and
   starts nothing while the machine stays running until it ends, one task
   at a time. An interrupted task's state stays the machine's; a cleared
   one's is dropped. A stop that lands before the task's handle attaches
   cancels it on attaching. A task cancelled from teardown (a blocked
   task's cancellation) now ends its run, as a polled one does. Tests:
   interrupt then recovery (reason, flag, the waiting task's break, state
   kept), a failure's state as recovery finds it (the state before the
   outermost cut, untested until now), the no-ops, a clear across a
   grandchild, an interrupt undone by a failed cut, and the attach window
   driven directly, then recovery and clearing waiting out a busy child
   and a clear dropping its own transaction's task; each guard
   mutation-checked. Still open: a child task
   that panics, or that its launching session abandons as it closes,
   never ends its run, so its machine stays running.
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
4. `child-task-control`: an interrupt where nothing runs (an idle or a
   flagged child) changes nothing (maintainer: reasonable), as does a
   recovery of an idle child. Recovering or clearing a busy child waits
   (maintainer, 2026-10-24: synchronous, under `.child.call`'s
   conditions); only the child itself is waited for, and a clear stops
   its descendants' running tasks, since a parent has no other way to stop
   a grandchild's work before `.child.call`.
5. An interrupt breaks the reply with `Reason` itself, as the Reflection
   Model says; recovery and clearing break with a diagnostic, "the task's
   machine was recovered from an error before the task ran", "... cleared
   while the task ran" or "... cleared before the task ran".
