# Quiescence — 2026-10-25

A review of where `api-settlement` in the [runtime API plan](RuntimeApi_2026-10-17.md)
should end, asked for by the maintainer before deciding the transition:
settlement as a staging step, which publishes exit votes and kills a
deadlock, belongs to the exit-vote design that is retiring. What is left
is an analysis of a halted runtime and the options it gives a host.
Drafted 2026-10-25 for the maintainer's review. It reframes
[Settlement Clearing](SettlementClearing_2026-10-25.md).

## Where we want to be

1. **No phases.** The runtime runs machines, driven by turns and tasks.
   Quiescence is a state the runtime can be in, not a step it takes.
2. **Quiescent means halted.** No work progresses without action from
   outside: every turn and task waits, on input, on observed state, for
   queued tasks or on a promise, and no evaluation work remains. As
   `reflection-tasks-are-machine-tasks` classifies the waits, waiting on
   input, on observed state or for queued tasks is idle; a task waiting
   on a heap value or on its input queue stays as it is, unflagged.
3. **An analysis says what is owed.** At quiescence a host asks: are we
   halted with obligations? Unresolved is:
   - a promise a machine accepted, still pending
     (`obligations-are-accepted-promises`), with its diagnostic and the
     acceptor's state;
   - a machine in error, with its failure, an annotation task's
     anonymous root among them;
   - an evaluation awaiting a promise no one will resolve, naming who
     holds the resolver, or that no one does, or that the waiter resolves
     it itself.

   Idle waits are listed too, for inspection, but owe nothing.
4. **The host chooses what follows**, with ordinary operations it may
   also use at any other time:
   - *finish*: a batch ends, failing where anything is unresolved;
   - *act*: a turn writes a port, sets heap state, resolves or breaks a
     promise, or recovers a machine, so work resumes and a later
     quiescence is analyzed afresh;
   - *interrupt*: stop chosen work deliberately. Whatever an interrupt
     halts early is flagged (maintainer, 2026-10-25): the machine of each
     run it stops, and the children of the calls that run was inside.
     Idle work is never stopped by itself;
   - *clear*: drop a machine in error with its subtree, abandoning what
     it accepted.

   Anonymous machines are reachable only through the analysis, whose
   entries carry what these operations need.
5. **Events are diagnostics.** A failure that happens, a delivery's or a
   maintenance failure, is a diagnostic when it happens. A task's failure
   is its machine's flag, state the analysis reads, so nothing waits for
   a settlement to be reported once.
6. **The CLI.** A batch builds, waits for quiescence, and has its logger
   render the analysis, which may start work; when an analysis finds
   nothing new it exits, failing where anything is unresolved. It kills
   nothing. The interactive mode and an IDE show the same analysis and
   let the user choose.

## Where we are

- **Settlement stages.** `QuiescenceSnapshot::settle` validates and
  publishes exit votes (`ExitIntent`, `.exit.*`, dispositions) and kills,
  then reports, and keeps "pending" lists so that each failure is
  reported once across settlements.
- **Waiting is deadlock.** Readiness is `Ready` or `Deadlocked`. A
  reflection task that waits without an exit vote is deadlocked, on
  input or the heap as much as on a promise, and the CLI kills it. A
  kill flags the machine one settlement later, as "the task halted before
  it finished", rather than with its reason.
- **Obligations are not built** (`api-refl-obligations`), nor exit votes'
  retirement (`api-refl-retirements`).
- **Nothing reaches a failed anonymous machine.** Reclamation keeps
  failures whole, and no host operation can name them.
- **Two runtimes.** Settlement lives on `EvaluationRuntime`; turns, which
  quiescence must count, on `Runtime`, where `rt.settle()` was planned.

## The transition, tentatively

1. `quiescence-classifies-waits`: the coordinator tells idle waits from
   unresolved ones. Readiness becomes busy or quiescent, with an
   analysis; nothing is killed by default.
2. `api-refl-obligations`: accepted promises, which the analysis reports.
3. `quiescence-options`: interrupt and clear through the analysis's
   entries, interrupts flagging what they halt early, with the call's
   callees. Recovery and acting go through turns as they do now.
4. CLI: the batch on the analysis; exit votes, settlement's publication
   and its pending lists retire (`api-refl-retirements`).

Settlement Clearing's slices fold into step 3: its kill flags become the
interrupt's, and its clearing the clear option.

## Questions for the maintainer

1. **What fails a batch.** Every unresolved item: a pending obligation, a
   machine in error, an evaluation awaiting a promise no one will
   resolve? And never idle waits alone?
2. **No kills.** A batch exits without interrupting anything, and only a
   host's deliberate interrupt stops work. Is anything lost, such as
   abort hooks a Rust turn's cut registered, which an exit never runs?
3. **The analysis's reach.** Taken only at quiescence, or at any time,
   for an IDE, with "unresolved" meaningful only at quiescence?
4. **Reporting once.** The analysis reflects state, so a host sees the
   same machine in error each time until it acts; the CLI renders only
   what is new. Is that the right owner for repeat suppression?
5. **The name.** `api-settlement` becomes `api-quiescence`, and
   "settlement" leaves the vocabulary?
