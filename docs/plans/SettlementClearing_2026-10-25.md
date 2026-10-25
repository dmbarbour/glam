# Settlement Clearing — 2026-10-25

A design note for `settlement-clears-machines`, a step of `api-settlement`
in the [runtime API plan](RuntimeApi_2026-10-17.md): how the reflection
machines left in error leave the runtime, and how settlement's kills flag
them first. It builds on [Child Tasks](ChildTasks_2026-10-24.md) and
[Child Calls](ChildCalls_2026-10-25.md). Drafted 2026-10-25 for the
maintainer's review before building. Reframed the same day, at the
maintainer's request, by the end-state review in
[Quiescence](Quiescence_2026-10-25.md): settlement as a staging step
retires, and this note's slices become options a host takes after an
analysis.

## The model

From the decisions (`quiescence-reports-unresolved-state`,
`reflection-tasks-are-machine-tasks`) and the plan step (maintainer):

- At quiescence, waiting on input, on observed state or for queued tasks
  is idle. Unresolved is an evaluation awaiting a promise no one will
  resolve, a machine in error, a pending obligation, and killed work. A
  batch may end with tasks still waiting once nothing is unresolved.
- Settlement's kills are interrupts, so killed machines stay recoverable.
  Clearing the machines settlement kills was rejected.
- Machines in error stay whole, with their state, until settlement clears
  them on a decision its host or user makes from the report, such as a
  CLI option or an IDE's command: failed child machines and failed
  anonymous roots. Clearing drops each machine with its subtree and its
  failure; inspection tools may later clear at a finer grain. Until then
  they stay as long as the runtime does.
- A call blocked for good within a cut flags both its child and its
  caller (maintainer, 2026-10-25; see question 1).

## Today (survey, 2026-10-25)

- **Reclamation keeps failures.** Once an anonymous root's task has ended
  and nothing runs or waits in its subtree, the runtime drops the
  subtree, keeping each flagged machine whole: its cell, flag, state and
  queue (`reclaim_if_idle`). A failed annotation task's root is kept in
  the coordinator (`FailedRoots`), which settlement reports. Nothing
  removes either.
- **The report lists them.** `QuiescenceReport::failed_machines` lists
  each flag and each failed root, by path with its diagnostic, and
  `pending_failed_machine_reports` those not yet reported, so each
  failure is reported once.
- **A kill flags a round late, and vaguely.** Probed with two child tasks
  blocked for good: settlement kills both and reports two killed works,
  but no failed machine. The killed effect tasks are dropped, and their
  run guards halt their runs afterwards, at the next safe point, so the
  next settlement reports the machines as failed, with "the task halted
  before it finished" rather than the kill's reason.
- **Settlement kills more than the model does.** A cut waiting on
  observed state, a heap read, is killed as a deadlock, where the model
  counts it idle. Classifying waits is `api-settlement`'s own work; this
  note only takes care that its tests use waits the model kills too.
- **A call's frames are gone while its cut waits.** A cut that failed or
  erred within a call waits to run again from its start, so a task
  killed there holds no call frame. Only an evaluation blocked inside
  `Op`, on a promise no one resolves, holds one when killed, which is also
  the only wait within a call that the model kills.
- **No CLI mode outlives a settlement.** A batch ends once a settlement
  finds nothing new; `--interactive` is designed (`Design.md`) but not
  built.

## Proposed design

### A kill flags in its own report

A kill is an interrupt: settlement ends each run it stops as
`.child.interrupt` does, flagging the machine with the kill's diagnostic,
before it reads the flags for its report. The report then lists the
killed work and, beside it, the machines it left in error, both in the
same round. The likely mechanism: settlement ends the detached effect
tasks' runs through their halt actions, with the kill's failure as the
reason, and reads the flags after running them, once it has released
its exclusive guard, since ending a run commits.

### Clearing what nothing can reach

A failed machine is orphaned when its anonymous root's task has ended and
no task runs or waits in the root's subtree: no task can recover it, and
reclamation has dropped all else. A failed anonymous root is orphaned at
once. Settlement offers to clear orphans only (see question 2); a failure
under a live root is its tasks' to recover or clear.

- `FailedMachine::orphaned()` tells the two apart in a report.
- `QuiescenceReport::clear(&FailedMachine)` drops an orphan with its
  subtree: the flags, states, ports and cells below it, and its queue,
  whose tasks never ran, breaking their replies as `.child.clear` does. A
  failed root's record goes too. It answers whether it cleared anything:
  a failure already cleared is left alone, and one that is not an orphan
  is refused with an error.
- Reclamation drops a root from the host's ended roots as it reclaims,
  even where it keeps failures; a root that keeps some stays among them
  instead, which is how a failure reads as orphaned, and leaves once the
  last is cleared.

The CLI gets no option for now, since a batch ends after settling; the
interactive mode, or an IDE, takes the decision from the user when it
comes (see question 3). Embedders and tests use the API.

### A call blocked for good

A kill stops the caller's task, flagging the caller's machine. Where the
task is blocked inside `Op`, its call frames name the children it was
running on, innermost last. Whether the kill flags those children too is
question 1.

### Slices

Each with tests, the full gate and a commit.

1. `settlement-kills-flag` — a kill ends each run it stops with the kill's
   diagnostic, flagging its machine in the same report; with question 1,
   the children of the call frames it stops. Tests: a child task blocked
   on a promise no one resolves (its resolver kept in the heap, so it
   never breaks) is reported killed and its machine failed, with the
   kill's reason, in one settlement; a recovery then runs on it.
2. `settlement-clears-machines` — `FailedMachine::orphaned` and
   `QuiescenceReport::clear`. Tests: a failed child under an ended root
   stays across settlements until cleared, then nothing is left; a failed
   anonymous root clears; a failure under a live root is not an orphan,
   and its clear is refused; a cleared queue breaks its replies.

## Questions for the maintainer

1. **The callee of a killed call.** Under the model, only an evaluation
   blocked inside `Op` is killed within a call; a cut waiting to rerun is
   idle. Should that kill flag the callee, beside the caller? The callee's
   changes lie in the caller's uncommitted transaction, so its state is
   as it was, and the flag means bad state; but the call was the child's
   driver when stopped, and an interrupt flags each machine it stopped
   work on. My lean: flag the caller only.
2. **Orphans only.** May settlement clear only orphaned failures, leaving
   one under a live root to its tasks? My lean: yes, as a live parent may
   yet recover it, and clearing behind it would surprise it.
3. **No CLI option yet.** Defer the CLI's choice to the interactive mode,
   since a batch ends after settling? My lean: yes.
