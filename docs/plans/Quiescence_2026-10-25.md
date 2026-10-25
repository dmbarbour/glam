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

## The transition

Revised after the decisions below: the analysis comes first, as an
observation beside today's settlement, so nothing changes behavior until
the CLI moves onto it.

1. `quiescence-analysis`: `EvaluationRuntime::analysis()`, callable at any
   time, observes without changing anything. It says whether the runtime
   is quiescent, and lists each waiting work with what it waits on, and
   each issue with its family:
   - a wait on observed state (an observation epoch), on input or for
     queued tasks is idle;
   - a wait on a promise is a *promise wait*, naming the task that holds
     its resolver where the coordinator knows one;
   - a wait on other work, a task or a lazy value, takes the family of
     what that work waits on, so a chain ends at its cause;
   - a cut waiting for a change after an error, which the coordinator
     keeps with the wait, is an *erred wait*, with the error (see
     question 1);
   - each flag and failed anonymous root is a *machine in error*.

   Tests build each family and check the analysis says so, at quiescence
   and while work still runs. Settlement stays as it is.
2. `api-refl-obligations`: accepted promises, which the analysis lists as
   *pending obligations*.
3. `quiescence-batch`: the CLI waits for quiescence, takes the analysis,
   renders it, and fails where an unmasked issue remains, by default any;
   it kills nothing. The settlement loops, kills, pending lists and
   automatic rendering retire, and exit votes with them, as
   `api-refl-retirements` plans.
4. `quiescence-options`: interrupt and clear through the analysis's
   entries, interrupts flagging what they halt early, with the call's
   callees. Recovery and acting go through turns as they do now.
   Settlement Clearing's slices fold in here.
5. Later: turns counted in quiescence once settlement's runtime and the
   turn runtime meet, as `rt.analysis()`; and the analysis as a
   reflection effect, for a logger's or an IDE's status.

### Decisions on the transition (maintainer, 2026-10-25)

1. **An erred wait is unresolved**, clearly distinct from intentional
   waits: a cut that fails by `.fail`, or diverges by pushback, awaiting
   a machine's heap state or a queue, is idle. An erred wait is the
   recoverable kind of error, which a change might cure; an
   unrecoverable one ends its task, flagging the machine and emitting
   its diagnostic, so it shows as a machine in error.
2. **`.retry`.** Glam may want an explicit `.retry` for the general case:
   intentional divergence until something observable changes, as
   opposed to backtracking, the counterpart of Rust's `m.retry()`
   (pushback, which no alternative takes). The runtime API plan holds it
   as `api-refl-retry`; the analysis counts such a wait idle.
3. **Turns wait.** Settlement's runtime counts no Rust turn: one running
   Rust code is invisible to its readiness, and one waiting in a cut
   blocks its own thread. Counting turns waits until settlement's
   runtime and the turn runtime meet (step 5); until then, a batch
   accounts for its own turns, as it does now, and the analysis says
   nothing of them. Order the steps for convenient workflow (maintainer).

## Decisions (maintainer, 2026-10-25)

The questions this review asked, answered (`quiescence-is-analyzed`):

1. **What fails a batch.** By default, any unresolved item. A client may
   mask families of issues, or recognize issues the default does not: a
   batch is a quiescent state that a user decides should be one.
2. **No kills.** Nothing is lost at a batch's end: a batch is an atomic
   transition the client has chosen to recognize.
3. **The analysis's reach.** At any time, as reflection on the runtime,
   which an IDE or a logger may use to report status.
4. **Reporting once.** What to do with a report belongs to whoever asked
   for it; nothing renders one automatically.
5. **The name.** `api-settlement` is redirected to `api-quiescence`, with
   its cause.

So point 6 above reads: a batch asks for the analysis at quiescence and
renders it as it chooses, failing by default where anything is
unresolved, less what it masks.
