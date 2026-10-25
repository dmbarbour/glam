# Child Calls — 2026-10-25

A design note for `api-refl-call`, a slice of `api-reflection-machines`
in the [runtime API plan](RuntimeApi_2026-10-17.md): `.child.call Key Op`,
the one synchronous operation on a child machine, as the
[Reflection Model](ReflectionModel_2026-10-21.md) decides
(`reflection-tasks-are-machine-tasks`). It builds on
[Child Tasks](ChildTasks_2026-10-24.md). Drafted 2026-10-25 for the
maintainer's review before building.

## The model

From the Reflection Model (maintainer):

- `.child.call Key Op` is a transactional remote procedure call. Within a
  transaction it waits until the child has finished what was committed to
  it before the caller's transaction began, which is pushback while the
  caller's snapshot shows the child busy, then runs `Op` on the child
  within the caller's transaction and returns its result. The child's
  changes commit or abort with the caller; a task committed to the child
  meanwhile conflicts with it.
- Outside a transaction, a call is a plain remote procedure call: the
  caller waits while the child runs `Op` after what was queued before it,
  step by step as any task runs, and the call returns `Op`'s result or
  fails with its failure.
- Within a cut, a call is the program-side counterpart of a Rust
  synchronous turn, which joins the cut; it is explicit rather than
  inferred from a caller that awaits a status, queues a task and awaits
  its result.
- One driver at a time: a machine runs its queued tasks or takes a call,
  never both.

## Today (survey, 2026-10-25)

- **Child state lives outside the transactional store.** Each child's
  local state is a cell in the runtime's `MachineCells`
  (`api/runtime/machines.rs`): a task takes it as it starts and writes it
  back as it ends, through `MachineHost::finish`, beside the transaction
  that claims its queue item. Child Tasks' first default recorded this
  as provisional: "`.child.call` stages it later."
- **A task's machine is per task.** A request finds the machine it acts
  on through `EvalContext::machine`, set once when a child's queue
  launches the task (`child_machine` in `reflection/requests.rs`). A call
  runs `Op` inside the caller's task, so the machine must vary within one
  task.
- **Local state is per branch.** The core effects `.get`, `.set` and
  `.try_get` act on `Branch::state`; a cut keeps its outer branch, which
  restores the state on failure (`CutFrame::outer`). A request reaches it
  through `RequestContext::state`.
- **Pushback exists for requests** as a failure after an observation
  (`ChildBusy` in `transact_on_machines`): a cut waits for what it
  observed to change, unless an alternative succeeds, and a task outside a
  cut waits.
- **Scoped requests exist**: `RequestResult::Scoped { operation, close }`
  runs an operation in the request's place, then a closing effect, which a
  failed operation skips.

## Proposed design

### Child state in the transactional event state

A child's local state moves from its cell to a record in the runtime's
event state, beside the flags (`api/runtime/flags.rs`): a persistent map
from machine path to state, whose journal records the states a
transaction read, for validation, and those it wrote, for its commit. Each
writer is one transaction:

- a task's run takes the committed state as it starts, and its end
  writes the state it leaves in the same commit that claims its item and
  resolves its reply, as `finish` does now;
- a call reads the state in the caller's transaction, observed, and
  writes the state `Op` leaves there, so the two commit or abort together;
- a clear drops the states below it, in its own journal; a run still
  winding down when a clear commits leaves no state, as the cell's
  `discard` ensures today, which stays with the run;
- reclamation drops the states of the subtree it reclaims.

A task committing on the child after the caller's snapshot, a queueing or
a run's end, conflicts with a call that read the child, as the model asks.
The cells keep only the runs.

### Within a transaction

`.child.call Key Op`, inside a cut or a `.try`:

1. **What the call may enter.** A child in error the call cannot enter: it
   fails with an error, and recovery is the only way in (maintainer,
   2026-10-25), as `run`, `give`, `take` and `fork` fail on a machine in
   error (`machine-errors-mark-and-recover`): the flag records an early
   exit that may have left the child's state half-done, so no work runs on
   it until its parent recovers it, acknowledging that. Nor can a call
   wait out tasks the caller's transaction queued on the child: it waits
   for the child to finish its task queue, and the child starts those
   tasks only once the transaction commits, so the wait could never end,
   and the call reports it as an error. This is no special rule, only
   reporting an error where it is noticed (maintainer, 2026-10-25: the
   error, over running the pending tasks within the caller's transaction,
   which would surprise the caller). Both reads are observed.
2. **Busy is pushback.** Where the child is busy as the caller's snapshot
   shows it, an unflagged queue holding a task, the call fails having
   observed it, as recovery and clearing do, so the cut waits for the
   child's queue to change.
3. **A call frame.** The call enters a frame in the caller's effect task,
   much as a cut does: it keeps the caller's branch state and machine, and
   runs `Op` in a branch whose state is the child's, read from the
   journal, and whose machine is the child. On success the frame writes
   the state `Op` left as the child's, restores the caller's, and answers
   `Op`'s value. An explicit failure, `Op`'s `.fail`, restores the
   caller's state with the frame, which a cut's alternatives or `.try`
   then handle, and stages nothing for the child. An error that ends `Op`
   is undone with the transaction: `.try` answers it, undoing what it
   tried, the child's state with it, and a cut, having observed the child,
   waits for a change that might change the error
   (`dict-empty-is-a-value`). The error flag means a machine's state is
   bad, and an undone error leaves the child's as it was, so the call
   flags nothing; `.try` is how a caller bounds the wait (maintainer,
   2026-10-25, at `child-call-frame`, clarifying decision 1, which this
   note first read as staging a flag that `.try` would commit). A call
   blocked for good within a cut is for settlement to find, which flags
   both the child and the caller (`settlement-clears-machines`).
4. **The machine per branch.** The branch carries the machine it runs on,
   replacing the task-level `EvalContext::machine` where requests read it,
   so `.child.*` within `Op` names the child's children and a call nests
   in a call. The task's own machine stays where its run's end and its
   guard need it.
5. **What `Op` sees.** Local state: the child's. Children: the child's.
   The heap, volumes, promises and `.log`: as the caller sees them, all
   transactional with the caller. `.env`: the caller's for now, since
   tasks bind no shares yet; the child's composed environment once
   `api-refl-ports` gives tasks shares. Control: the reset stack is part
   of a branch's local state, so a `.shift` within `Op` reaches only the
   `.reset`s within `Op`, as a Rust descendant's turn is a closure of its
   own.

`Op`'s own cuts nest in the caller's transaction, and its `.child.task`
queues on the child's children, reaching them when the caller's
transaction commits, as any queueing in a cut does.

### Outside a transaction

`.child.call Key Op` with no transaction around it queues `Op` on the
child, as `.child.task` does, and waits for its reply: the child runs it
after what was queued before it, and the call answers its result, or
fails with its failure, which flags the child as any task's failure does
(maintainer, 2026-10-25). A child in error it cannot enter: it fails at
once, as within a transaction, rather than queue behind a stopped queue.

### Slices

Each with tests, the full gate, and a commit.

1. `child-state-transactional` — child state moves into the event state's
   record with its journal; runs take and write it through their commits,
   clears and reclamation drop it. Tests: the existing child task tests
   (state persists between tasks, clears drop it, reclamation drops it),
   and a conflict between a run's end and a transaction that read the
   child's state.
   Built 2026-10-25: `StateRecord` and `StateJournal`
   (`api/runtime/states.rs`), versioned per machine so a read validates; a
   run's end writes its state in the commit that claims its item, reading
   it so a clear committed meanwhile conflicts, and the run's `discard`
   still drops a state a clear came after; the cells keep only runs.
   Mutation-checked: read validation, the run's write, the clear's and
   reclamation's drops.
2. `child-call-frame` — the per-branch machine and the call frame within
   a transaction: the errors for a child in error and for tasks the
   transaction queued on the child, busy as pushback, the child's state in
   and out, an error's flag staged with the caller, nesting, and conflicts
   with a task committed meanwhile. Tests: a call reads and changes the
   child's state, which commits with the caller's cut and aborts with a
   failed one; a call to a busy child waits until its task ends; a call
   to a flagged child fails until a recovery; a call after the
   transaction queued on the child fails; an error in `Op` that `.try`
   answers leaves the child as it was, unflagged; nested calls; a task
   queued on the child after the caller's snapshot conflicts, and the
   cut runs again.
   Built 2026-10-25: `.child.call` answers a scoped request, whose
   operation is `Op` and whose close, a hidden request, writes the state
   `Op` left as the child's, in the caller's transaction; each branch
   keeps a stack of the calls it runs within (`ChildCall`: the child and
   the caller's state), whose innermost child the `.child.*` requests act
   on, so a failure or a `.try` restores the caller's machine and state
   with its branch. A call on an anonymous root's child installs the
   root's guard, as a task does, so reclamation drops the state it
   leaves. Outside a transaction it fails with an error until
   `child-call-plain`. A task committed to the child meanwhile cannot
   come from Glam today: only the parent's tasks, one at a time, queue on
   a child, and a busy child turns a call away; it guards Rust turns and
   settlement as they come to act on children. So a test-only effect,
   `.TEST_ONLY.queue_now`, commits a queueing at once within the caller's
   cut, as such a writer would, and the cut runs again (first tested on
   the journals by hand, retired for it, at the maintainer's suggestion).
   Mutation-checked: each entry condition, the state's swap in and out,
   the per-branch machine, the root's guard, and the reads' observation.
3. `child-call-plain` — the call outside a transaction, queued and
   awaited. Tests: order after earlier tasks, the result, the failure and
   the child's flag, and a flagged child.
   Built 2026-10-25: the call queues as `.child.task` does, through one
   `queue_on_child`, which admits the child in the queueing's own journal,
   so a flag set before the commit conflicts and the queueing runs again
   rather than queue behind a stopped queue; it then answers a new
   `RequestResult::Await` on the reply, by which a request answers a
   value once evaluated, as `seq value (.r value)` does, so the caller
   waits for the child and fails with the task's failure, the very
   diagnostic. Mutation-checked: the wait, the child's admission, and the
   anonymous root's guard. The conflict between the admission and a flag
   committed meanwhile is a guard Glam cannot force in order, as the flag
   comes only from the child's own run ending in between.

Turns get the same rule for what the transaction sent a child, in the
runtime API plan's `turn-pending-input-conflicts`.

## Decisions (maintainer, 2026-10-25)

1. An error that ends a call's `Op` flags the child, plain or within a
   transaction, as an error that ends a turn flags its machine; within a
   transaction the flag stages with the caller. Observation never clears
   it, only recovery. (Clarified by the maintainer the same day: the flag
   means a machine's state is bad. Within a transaction the error is
   undone, `.try` answering it or a cut waiting for a change, so the child
   is left as it was and unflagged; only a plain call's error flags it.
   A call blocked for good flags child and caller at settlement.)
2. A call cannot enter a child in error: recovery is the only way in, for
   the reason machines have one (`machine-errors-mark-and-recover`).
3. A call cannot ignore tasks its own transaction queued on the child; it
   fails with a conflict error, rather than run them within the caller's
   transaction. Machines get the same rule. (Clarified by the maintainer
   the same day: no special rule. The call waits for the child to finish
   its queue, which the child starts only once the transaction commits,
   and the error reports that wait, which could never end, where it is
   noticed.)
