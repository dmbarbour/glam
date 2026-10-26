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
   Built 2026-10-25: `EvaluationRuntime::analysis`, with `RuntimeWait`
   (its family, the work its chain ends at, and whether another work
   awaits it) and `RuntimeIssue` by `RuntimeIssueFamily`: promise waits,
   erred waits, machines in error. A promise wait counts once, at its
   chain's head, since the lazy routes forced on the way are its links. A
   wait on work queued, running, reserved, or dormant but demanded is in
   progress, and the runtime then is not quiescent; so is a delivery to a
   Rust endpoint still pending. No cycle family: the runtime already fails
   a cycle among evaluations, as two annotation gates forcing each other
   showed, so a chain never returns to itself, and the analysis only
   guards against it. Mutation-checked: each family, chain following, the
   links' omission, quiescence by records, by waits in progress and by
   deliveries, and failed roots.
2. `api-refl-obligations`: accepted promises, which the analysis lists as
   *pending obligations*. Built 2026-10-25, for tasks and turns
   ([Obligations](Obligations_2026-10-25.md)).
3. `quiescence-batch`: the CLI waits for quiescence, takes the analysis,
   renders it, and fails where an unmasked issue remains, by default any;
   it kills nothing. The settlement loops, kills, pending lists and
   automatic rendering retire, and exit votes with them, as
   `api-refl-retirements` plans.
4. `quiescence-options`: interrupt and clear through the analysis's
   entries, interrupts flagging what they halt early, with the call's
   callees. Recovery and acting go through turns as they do now.
   Settlement Clearing's slices fold in here; the design is
   [Quiescence Options](QuiescenceOptions_2026-10-25.md).
   First, `reclaim-once-clear`, the reclamation Obligations left open.
   Built 2026-10-25: an ended anonymous root stays among the ended roots
   while a machine below it owes, and a commit that discharges an
   obligation, alone or with a cut, reclaims the ended roots again
   (`reclaim_ended_roots`), so an owing machine goes once what it owes is
   discharged, while a machine in error stays. Only discharge needs the
   rerun: under an ended root nothing but a clear, which drops what it
   clears, can take a flag or an acceptance away. Mutation-checked: the
   root's keeping and release, both commit paths, and the subtree
   test.
   Built through 2026-10-26: waits name their machines, a host interrupts
   by path, transitively through calls, and clears an idle subtree; see
   [Quiescence Options](QuiescenceOptions_2026-10-25.md).
5. Later: turns counted in quiescence once settlement's runtime and the
   turn runtime meet, as `rt.analysis()`; and the analysis as a
   reflection effect, for a logger's or an IDE's status.

### Step 3, planned (survey, 2026-10-25)

**Today.** The CLI's batch is one turn on the root: it starts the logger,
assembles, then loops settlement (`settle_batch_runtime`, and
`settle_batch_runtime_default` without a configured logger) until a
report renders nothing new, killing a deadlock on the way, and joins the
logger. The configured logger, `conf.log`, runs as an `EffectRun` whose
idiom (`CLI.md`) reads the log in a cut and, on an empty read, votes
`.exit.success`. Settlement publishing that vote is what ends it, or a
kill does; nothing else can stop the run from outside. So settlement
cannot leave the CLI until the logger ends another way.

**The batch's end, as designed** (review asked for by the maintainer,
2026-10-25: whether to halt the configured logger at all, or only report
final diagnostics through it).

- **Everything goes through the logger.** The batch publishes each new
  issue of an analysis as an error diagnostic on its bus, which routes to
  the logger's input as any diagnostic does, so a configured logger
  renders the analysis too. Nothing bypasses it with the CLI's own
  renderer, as settled reports do today.
- **An idle logger is not halted.** At quiescence a logger that has read
  all its input waits on it, idle. When an analysis finds nothing new,
  the batch ends there: an atomic transition the CLI recognizes, at which
  nothing is lost, so the logger's wait ends with the process, cleanly,
  as any idle wait does, with no diagnostic and no flag. (Its run must
  still end for the batch's turn to return; that is an implementation
  detail of the bridge, a cancellation, until the logger is a turn's
  fork.)
- **A logger with a backlog is halted.** At quiescence, unread
  diagnostics in the logger's input mean it is not reading: blocked on
  something else, or broken. The batch then halts it, a diagnostic of its
  own ("the configured logger stopped reading with N diagnostics
  unread"), and hands its input to the fallback, which renders the
  backlog and all that follows, as it does when a configured logger ends
  today. The heuristic is the maintainer's.
- **Backlogs in general.** A machine's input that holds unread items at
  quiescence is incomplete work, of interest for debugging, and so is a
  child's task queue holding tasks that have not started. The analysis
  lists each backlog, by machine and port, with its count, as an issue
  family, `Backlog` (see question 1). The logger's backlog is one of
  them, which the batch acts on as above.
- **Retained failures.** A delivery failure, output a machine sent to a
  host's Rust endpoint whose callback failed (decoding, the adapter, or a
  panic), and a maintenance failure, a collection the runtime ran for
  maintenance that failed, are kept until the host acknowledges them: the
  analysis lists them as issue families of their own (maintainer: analyze
  them).
- **The exit status** fails on any error diagnostic the batch rendered,
  issues included, less what a client masks.

**Slices, revised.**

1. `quiescence-backlogs` — the analysis lists backlogs, delivery failures
   and maintenance failures. Tests: a port holding unread input, a queue
   holding unstarted tasks, a machine's failed delivery, and their
   absence once read, run or acknowledged.
   Built 2026-10-25: `RuntimeBacklog` (machine, port or `None` for a task
   queue, count) from the event state's buffers, a task queue counting the
   tasks behind its head on a machine not in error, and all of them on one
   in error, whose queue stopped; the retained delivery failures, and the
   maintenance failures read without draining the settlement's pending
   list; each its own `RuntimeIssueFamily`. Mutation-checked: both queue
   rules, empty queues left out, ports' counts, and each family's place in
   the issues.
2. `quiescence-batch` — both batch paths pump to quiescence, take the
   analysis, and publish each issue not published before; once the
   logger has a backlog at quiescence, they halt it for the fallback,
   with its diagnostic; once an analysis finds nothing new, they end. No
   settlement, no kill. Tests: the CLI's tests, each family rendered once
   through a configured logger, a logger without an exit vote ending
   cleanly, a logger that stops reading halted with its backlog rendered
   by the fallback, and a stuck task failing the batch unkilled.
   Built 2026-10-25: `quiesce_batch` over a `BatchLogger` (the root's
   logger machine, or the bus alone without one), publishing through the
   assembler's bus, so the configured logger receives the issues;
   `PublishedIssues` keys each issue (a work, a backlog's machine and port,
   an obligation's promise and acceptor, a machine, a delivery, a
   maintenance failure) so each publishes once, with a report context:
   `promise_wait`, `erred_wait` (its retained error one nested message),
   `backlog`, `pending_obligation`, `machine_failure`, `delivery_failure`,
   `maintenance_failure`. `ScheduledEffectRun::stopper` gives the batch an
   `EffectRunStop` for the logger: the end stops an idle one, and a halt
   one with unread input, after its own diagnostic; a cancelled logger
   reports nothing. The settled-report rendering and its tests retire;
   killed work and exit errors render no more, the latter until exit
   votes retire. Mutation-checked: the halt, and a cancellation being
   clean.
3. `exit-votes-retire` — `.exit.*`, `ExitIntent`, exit dispositions and
   the exits readiness carries, with the tests and docs that vote, under
   `api-refl-retirements`; the `conf.log` idiom waits on `.read_log`.
   Built 2026-10-25: gone are `.exit.*`, `ExitIntent`, the exit wait and
   its work state, the exited terminals, statuses and polls, the exit
   dispositions and the exits readiness and settlement carried, a task's
   exit state and the profile's exit flag, and the halt's exit (`RunHalt`):
   a halt is an action. Settlement's plan carries only kills; a ready
   snapshot settles with nothing to publish. The two task launchers,
   which differed only in exposing exits, are one `task_launcher`. A
   search's discard of its progress, which only an exit used, retires;
   its test now checks that taking an alternative releases it and that
   returned results own their roots. Tests that voted retire with the
   votes, a deadlock's forced kill now meets a child that always waits,
   and the two CLI logger tests and the `conf.log` idiom wait on
   `.read_log` in a cut. The G0 latch's settlement slot, which latched
   exit votes, now latches an analysis keeping an erred wait's error and
   its machines in error (`crates/glam-gc/VERIFY.md`).
4. After `quiescence-options` gives hosts their deliberate interrupt:
   `settlement-retires` — `QuiescenceSnapshot::settle`, the deadlock
   kill, `QuiescenceReport` and its pending lists, with the tests that
   settle, each judged for whether the analysis's tests serve it.
   Built 2026-10-26: gone are `settle` and its validation, the report and
   its pending lists (a flag's reported mark, the coordinator's unreported
   roots, the pending delivery and maintenance reports), dispositions and
   the deadlock kill, the coordinator's settlement plan and publication,
   `RuntimeSettlementError`, and the `Killed` terminals of waits, task
   statuses, client demands and lifecycles, which only the kill produced,
   with the `stage_settlement` profiling count. Readiness keeps its
   `Ready` and `Deadlocked` snapshots, a stamp, the store and the
   unfinished work, as observations nothing validates later. The
   exclusive gate keeps its use, readiness probes and pressure promotion,
   under a name of its own (`RuntimeExclusiveGuard`), and the coordinator's
   `settlement` child is `readiness`. Settlement collected before it
   reported, so that a lost resolver's promise broke rather than being
   reported awaited; the batch had dropped that with `quiescence-batch`,
   and now collects once before it judges a quiescence with issues, and
   again only after it publishes (a CLI test, mutation-checked). Tests:
   the public, managed-collection, failed-machine and retention tests read
   the analysis or the snapshot instead; the validation tests, the forced
   kills, the client demand kill, and the turn test of settlement's
   collection retire with what they tested.

**Decided on the end design** (maintainer, 2026-10-25: yes to both, "this
aligns nicely with my thinking"): a backlog is unresolved by default, its
own family, and the logger is halted only when left with unread input at
quiescence. The questions, as asked:

1. **Is a backlog unresolved by default?** An input left unread at
   quiescence, or a queued task never started, is work that did not
   happen, so it would fail a batch unless masked. That includes tasks
   queued behind a task waiting idle, which the reflection model counted
   idle (`reflection-tasks-are-machine-tasks`). My lean: unresolved, its
   own family, since nothing was owed by the waiting task but the queued
   ones never ran.
2. **The logger's halt.** Halted when its input holds unread diagnostics
   at quiescence, with a diagnostic, the fallback taking over; otherwise
   never halted, its idle wait ending with the batch. Is that the rule?

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
