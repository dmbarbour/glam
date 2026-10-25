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
   Built 2026-10-24, revised 2026-10-25. Each is a transactional edit,
   within a cut or committed at once. An interrupt is transitive: at the
   child and each descendant it takes the head of an unflagged queue,
   breaks its reply and sets the flag. A recovery clears the flag and takes
   the whole queue, observing its end, so a task queued meanwhile
   conflicts. A clear takes every queue at or below the child, which only
   flagged machines hold, with the tasks its own transaction queued there,
   and drops port state, shares, environment and flags as a turn's clear
   does. Interrupting is the only way work stops (maintainer, 2026-10-25),
   so recovering waits while the child is busy, and clearing while the
   child or a descendant is, as `.child.call` does (maintainer,
   2026-10-24): the edit fails having observed the busy machine, so a cut
   waits for it to change, unless an alternative succeeds, and a task
   outside a cut waits; `.try` reports them `'fail`. At commit, `ReflectionUpdate::Interrupt` and `Clear` stop the
   runs: a run is named by its item's sequence in the queue
   (`TaskMachine::sequence`), so a stopped task's end claims nothing and
   starts nothing while the machine stays running until it ends, one task
   at a time. An interrupted task's state stays the machine's, unless a
   clear comes while it winds down, which drops it. A stop that lands before the task's handle attaches
   cancels it on attaching. A task cancelled from teardown (a blocked
   task's cancellation) now ends its run, as a polled one does. Tests:
   interrupt then recovery (reason, flag, the waiting task's break, state
   kept), a failure's state as recovery finds it (the state before the
   outermost cut, untested until now), the no-ops, a clear across a
   grandchild, an interrupt undone by a failed cut, and the attach window
   driven directly, then recovery and clearing waiting out a busy child,
   a clear waiting out a busy grandchild until a transitive interrupt, a
   clear dropping its own transaction's task, and a clear dropping a
   winding-down task's state (driven directly); each guard
   mutation-checked. Still open: a child task
   that panics, or that its launching session abandons as it closes,
   never ends its run, so its machine stays running.
5. `child-task-migration` — the compiler's annotator and scanner move to
   `.child.task`: the scanner as the boundary task's child, each `refl.*`
   entry as the scanner's child under its key, and the tests from
   `.task.*` to `.child.*`. Failures reach the CLI as flagged machines in
   place of the ledger's `task_failures`, until `api-settlement` reports
   them.
   Built 2026-10-25, with defaults 2 and 3. The annotator queues the
   boundary's operation on `'scanner` and records its promise as
   `[guard, 'claim]`; the scanner queues each `refl.*` entry under its key,
   recording `{key, task}` with the task's promise. A flag keeps the
   diagnostic of what raised it, a failure's or an interrupt's reason, and
   settlement reports every standing flag as a `FailedMachine`
   (`QuiescenceReport::failed_machines`), each once among the pending
   reports, which the CLI fails a batch on and renders with the machine's
   path (`Values::key` reifies its keys). The compiler's probe tests await
   the recorded promises. The other `.task.*` uses, the tests of `.task.*`
   itself and the CLI logger's children, wait for
   `child-task-retirement`, each migrated or retired by its purpose. Noted:
   the machines below anonymous roots stayed, a cell and empty queues per
   machine, until `anonymous-root-reclamation`.
6. `child-task-retirement` — `.task.*`, `TaskHandleCell`, the status
   queries, `acknowledge_reasoning_failure` and the failure ledger retire,
   with their docs, and the remaining `.task.*` tests and the CLI logger's
   `.task.new` children move to `.child.*` or retire with what they test.
   Built 2026-10-25. Retired: the seven `.task.*` requests, task handles,
   the reflection store's protected query domain and its writer, pending
   launch policies, the request-level wait poll (only joins used it), the
   acknowledgement ledger with `ReasoningFailure`,
   `QuiescenceReport::task_failures` and propagated acknowledgement, and
   the CLI logger's children (maintainer, the same day: they return as
   `.child.task` once the logger is a turn). An annotation task's failure
   flags its anonymous root, for good, even where a demander saw it
   (maintainer: "flag its root"): the coordinator keeps these failed roots,
   which settlement reports beside flagged children as failed machines. A
   Rust-scheduled root's failure (`EffectRun`, `schedule_task`) goes only
   to its caller, so the CLI now reports a failed configured logger itself,
   and a composed run fails on its own result or a deadlock, not on a
   child's failure. The coordinator's status publication stays for
   host lifecycles, its former query slot a test-only observer. Tests of
   `.task.*` itself and of acknowledgement retired; the macro test's child
   moved to `.child.task`; the CLI rendering tests fail through an
   annotation; tests that read the session ledger read the task's wait;
   new: an annotation's failure flags its root though observed, its policy
   mutation-checked. Noted: `EffectLifecycle`'s descendant tracking now
   serves only child tasks a hosted root queues, on hosts with machines; its
   tests of `.task.new` descendants and abandonment retired with them.
7. `child-task-halts` — every run on a child machine ends: a run whose
   task ends abnormally, by a panic above all, halts with the error flag
   (maintainer, 2026-10-25), its reply broken with what happened and its
   machine freed, so the parent reads `'error` and may recover it. Today a
   panicked child's item stays at its queue's head, its machine `'busy`,
   and its reply pending, all for good. A guard on each run posts the
   abnormal end to the host, which halts the run where it is safe to.
   Session closure cannot abandon a child today, since only annotation
   tasks, whose work lives in the runtime's background domain, have
   children, and no Rust-scheduled root's host has machines; the guard
   would cover it too. Once sessions go away, a turn-hosted root's
   children are its machine's, with no session to choose (maintainer:
   child tasks join the parent machine).
   Built 2026-10-25. Each run's `TaskMachine` holds a halt action, from
   `launch_machine_tasks`, which ending the run takes, so a run ends once;
   the effect task holds a `MachineRunGuard`, whose drop halts a run not
   yet ended, and a task that fails to build or panics while building
   halts it too. A halt fails the run as a failure does, breaking the reply
   with "the task halted before it finished", and starts the next task
   where the machine goes on. Where the thread holds a mutator, or unwinds,
   the host keeps the action for its next safe point, death-notice
   processing among them. Tests: a child forcing a panicking thunk halts,
   flagged, and recovers to run its next task; a halt under a mutator waits
   for the safe point, and halts once; the guard and the deferral are
   mutation-checked. The halt of a task that fails to build has no test:
   glam code cannot make a launcher fail. A task settlement retires on its
   exit vote ends its run as exited, its reply broken but its machine
   unflagged, which goes with exit votes (`api-refl-retirements`); the
   existing exit settlement test covers it. `launch_machine_tasks` is the
   first entry on the recursion audit's `BOUNDED_RECURSION`: a halt
   relaunches through it only where a clear came between an interrupt and
   the halt, and a failed relaunch flags the machine.
8. `anonymous-root-reclamation` — once an anonymous root's task has ended
   and no task runs or waits in its subtree, nothing can reach the subtree,
   whose keys name a task id never reused: the runtime drops its machines'
   cells and port state. Failed machines stay whole, flag and state, for
   settlement to decide when to clear them, as later inspection tools
   might at a finer grain (maintainer, 2026-10-25).
   Built 2026-10-25, at whichever ends last: an anonymous root's task that
   tasks a child holds an `AnonymousRootGuard` (the request reports the
   host through `RequestActivity`), whose drop reports the root's end; a
   run's end in an ended root's subtree checks again. Where no run is
   active and no unflagged queue holds a task, the host drops the
   subtree's cells and port buffers, flagged machines' aside; settlement
   needs no sweep, so successful work leaves nothing behind (maintainer:
   reclaiming only at settlement would be unbounded garbage). Test: roots
   that end after their children, a child that ends after its root (as the
   compiler's scanner does), and a failed child that stays whole; each
   trigger, and the kept failure, mutation-checked. Wiring and shares below
   anonymous roots, which tasks cannot own yet, wait for `api-refl-ports`.

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
4. `child-task-control`: an interrupt where nothing runs changes nothing
   (maintainer: reasonable), as does a recovery of an idle child.
   Recovering a busy child, or clearing a subtree with a busy machine,
   waits (maintainer, 2026-10-24 and 2026-10-25: synchronous, under
   `.child.call`'s conditions; interruption is explicit, and transitive).
5. An interrupt breaks each reply with `Reason` itself, as the Reflection
   Model says; recovery and clearing break with a diagnostic, "the task's
   machine was recovered from an error before the task ran" or "... was
   cleared before the task ran".
6. `child-task-retirement`: only annotation tasks flag anonymous roots; a
   root Rust schedules reports its failure to its caller, so settlement
   never reports it twice.
