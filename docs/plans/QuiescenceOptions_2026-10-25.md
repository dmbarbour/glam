# Quiescence Options — 2026-10-25

A design note for `quiescence-options`, step 4 of the
[Quiescence](Quiescence_2026-10-25.md) transition: the operations a host
takes after an analysis, interrupt and clear, through what the analysis's
entries name. It folds in [Settlement Clearing](SettlementClearing_2026-10-25.md),
whose slices become these options. Drafted 2026-10-25 for the maintainer's
review before building; its first slice, `reclaim-once-clear`, was decided
already and is built.

## The model

From `quiescence-is-analyzed` and the Quiescence note's end state:

- A host may *interrupt* chosen work deliberately, at quiescence or at any
  other time. Whatever an interrupt halts early is flagged: the machine of
  each run it stops, and the children of the calls that run was inside
  (maintainer, 2026-10-25). Idle work is never stopped by itself.
- A host may *clear* a machine in error with its subtree, abandoning what
  it accepted.
- Recovery and other acts go through turns, as they do now.
- Anonymous machines are reachable only through the analysis, whose
  entries carry what these operations need.

## Today (survey, 2026-10-25)

- **Children have both operations.** `.child.interrupt Key Reason` takes
  the running task of the child and of each descendant from its queue,
  flags the machine with the reason, and breaks the reply, all in the
  transaction; the commit then cancels each stopped run
  (`MachineHost::interrupt`), whose end claims nothing.
  `.child.clear Key` empties the subtree, breaking the replies of the tasks
  it drops, and waits while a machine there is busy (`ChildBusy`), so a
  parent interrupts first what it means to stop. Neither flags a callee.
- **An annotation task's root is not a child.** Its machine is its
  anonymous root, `refl/<task>`, with no queue; its failure flags the root
  in the coordinator (`FailedRoots`), not in the flag record. Cancelling
  the task ends it `Cancelled`, which flags nothing. A Rust-scheduled run,
  the configured logger's among them, has an anonymous root too, for the
  children it tasks; its host stops it (`EffectRunStop`).
- **What the analysis names.** A machine in error carries its path
  (`FailedMachine`), as do a backlog and each acceptor of an obligation. A
  wait carries its work, task and session IDs, but no machine: the
  coordinator knows a reflection task's work, not the child it runs on,
  which only the task's own `TaskMachine` holds.
- **Call frames are a branch's.** A branch's calls (`Branch::calls`) live
  in the task's reflection machine. A cut that failed or erred within a
  call waits to rerun from its start, holding no frame; only an evaluation
  blocked inside `Op` holds one, so at quiescence only a promise wait
  inside a call does. A cancelled machine is dropped with its frames.
- **Reclamation keeps what owes, and now forgets it once discharged**
  (`reclaim-once-clear`, below). Only a clear takes a flag from a machine
  nothing runs on.

## Proposed design

### What a wait names

`RuntimeWait::machine()`: the machine the waiting task runs on, a child's
path or its anonymous root, recorded in the coordinator's reflection work
as the task launches (`ReflectionLaunchPolicy` already carries both); none
for an evaluation that is not a task.

### Interrupt

`EvaluationRuntime::interrupt(&MachinePath, reason) -> Result<Vec<MachinePath>, Error>`
stops the run on the machine at the path and on each of its descendants,
as `.child.interrupt` does a child's, in one transaction, and answers the
machines it flagged:

- a child's run: taken from its queue, its machine flagged with the
  reason, its reply broken, and the run cancelled at commit, as now;
- an annotation task's root: the task cancelled, and its root flagged with
  the reason, as its failure would flag it (`FailedRoots`); a
  Rust-scheduled run is its host's to stop, so its root's path stops only
  the runs below it;
- each callee of a call the stopped run was inside: flagged with the
  reason too, since the call is work in the interrupt's direction
  (decision 3). The run's frames are read where the run waits: the
  coordinator holds a waiting task's reflection machine, whose active
  branch holds its calls, so the interrupt reads them under the
  coordinator lock and flags the callees in its own commit, where no
  later flag can land on a machine cleared meanwhile. A run caught in the
  middle of a quantum is held by its worker, not the coordinator: the
  interrupt flags its machine and stops it at the quantum's end, its
  calls unflagged, since they are that quantum's own and nothing of them
  commits.

Nothing running there changes nothing. `.child.interrupt` takes the same
callee rule.

### Clear

`EvaluationRuntime::clear(&MachinePath) -> Result<bool, Error>` empties
the machine at the path and its subtree, in error or not (see question
1), as `.child.clear` does a child:
its flags, states, ports, shares and environment; its queues, whose
tasks never ran, their replies broken; and what it accepted, abandoned.
For an anonymous root it drops the root's failure too. It answers whether
anything was there, and fails, changing nothing, where a task runs or
waits in the subtree: a host interrupts first what it means to stop, as a
parent does. Clearing below an ended root leaves the root forgotten once
nothing there owes.

### The CLI

No option yet: a batch ends at quiescence, killing nothing, and the
interactive mode or an IDE takes the choice from its user when it comes.
Embedders and tests use the API.

### Slices

1. `reclaim-once-clear` — built 2026-10-25; see the Quiescence note.
2. `quiescence-waits-name-machines` — `RuntimeWait::machine()`. Test: the
   analysis's waits name a child's path and an annotation task's root,
   and no machine for an evaluation.
   Built 2026-10-25: the coordinator's reflection work records the child
   a task runs on as it launches, beside whether its failure flags its
   root (`record_launch`, which replaces `flag_root_on_failure`); a
   reflection task without one names its anonymous root, by its task.
   Mutation-checked: the child's recording and its naming, the root's
   naming, no machine for other work, and the root flag's recording.
3. `quiescence-interrupts` — `EvaluationRuntime::interrupt`, with the
   frames on a task's block and the callee rule for both interrupts.
   Tests: interrupting a child waiting idle flags it with the reason and
   breaks its reply; an annotation root's interrupt flags the root; a
   promise wait inside a call flags caller and callee; an interrupt of
   nothing running changes nothing; a recovery then runs on a flagged
   child, through its parent.
   Built 2026-10-25: `EvaluationRuntime::interrupt(&MachinePath, Value)`
   (`api/runtime/options.rs`), answering the machines it flagged. A run's
   calls come from its task's reflection machine while the coordinator
   holds it (`EvaluationTaskMachine::calls`, the active branch's frames;
   `MachineHost::run_calls`), and `interrupt_tasks` flags them in the same
   commit, for `.child.interrupt` and the host alike. An annotation root's
   interrupt flags the root in `FailedRoots`, clearing the task's
   `flags_root` under the coordinator lock, so a failure at the end of a
   quantum running meanwhile flags nothing twice, then cancels the task
   (`cancel_reflection`, which the task handle's cancel now uses). The
   recovery test was left out: interrupting leaves a flag as any failure
   does, and recovery is tested already. Mutation-checked: the callee
   flags and each source of the calls, the root's callees, the broken
   replies, the runs' and the root task's cancellation, the root's flag,
   and the path's reach. One mutant is equivalent: listing every
   reflection task, not only annotation tasks, as a root to interrupt
   changes nothing observable, since only annotation and child tasks run
   on a host with machines, so a Rust-scheduled run never calls, and a
   child task's calls are flagged through its run. The guard on
   `flags_root` needs a forced interleaving to test, and has none.
4. `quiescence-clears` — `EvaluationRuntime::clear`. Tests: a failed
   child under an ended root clears, leaving nothing; a failed annotation
   root clears; a clear refused while a task waits there, then accepted
   after an interrupt; a cleared queue breaks its replies; an abandoned
   obligation leaves the analysis.

Then `settlement-retires`, which these options unblock.

## Decisions (maintainer, 2026-10-25)

1. **A host may clear any machine whose subtree is not busy**, in error
   or not, orphan or not.
2. **The options take a machine's path.** An anonymous root's path
   interrupts its annotation task; a Rust-scheduled run stays its host's
   to stop.
3. **An interrupt is transitive.** It halts work in the direction of the
   machine it names: a parent should not need to care how a child does
   its task, whether it tasks further children or calls them, and just
   wants what goes on in that direction to halt. Idle machines are still
   not flagged, so a cleared machine is untouched. The same rule holds
   for `.child.interrupt`.

## Questions for the maintainer, as drafted

1. **What a host may clear.** Settlement Clearing leaned toward clearing
   only orphans, failures under an ended root, leaving one under a live
   root to its parent, which may yet recover it. A host clear is an
   action from outside, which is how quiescence ends; a parent waiting on
   a cleared child sees it idle, and the replies the clear breaks. My
   lean: any machine whose subtree is not busy, as `.child.clear` takes,
   orphan or not, in error or not: the host acts for the user, a clear of
   a machine that owes is how a host drops an obligation no one will
   meet, and refusing would add a notion (orphan) to the API for little
   gain.
2. **Interrupt by path.** The options take a machine's path, which the
   analysis's entries carry, rather than an entry, so a host may also name
   a machine it knows. An anonymous root's path interrupts its annotation
   task; a Rust-scheduled run stays its host's to stop. Is that the shape?
3. **One callee rule.** `.child.interrupt` flags callees too, so one rule
   holds for every interrupt. My lean: yes.
