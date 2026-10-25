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

1. **Busy is pushback.** Where the child is busy as the caller's snapshot
   shows it, an unflagged queue holding a task, the call fails having
   observed it, as recovery and clearing do, so the cut waits for the
   child's queue to change.
2. **A call frame.** The call enters a frame in the caller's effect task,
   much as a cut does: it keeps the caller's branch state and machine, and
   runs `Op` in a branch whose state is the child's, read from the
   journal, and whose machine is the child. On success the frame writes
   the state `Op` left as the child's, restores the caller's, and answers
   `Op`'s value; a failure restores the caller's state with the frame,
   which a cut's alternatives or `.try` then handle, and stages nothing
   for the child.
3. **The machine per branch.** The branch carries the machine it runs on,
   replacing the task-level `EvalContext::machine` where requests read it,
   so `.child.*` within `Op` names the child's children and a call nests
   in a call. The task's own machine stays where its run's end and its
   guard need it.
4. **What `Op` sees.** Local state: the child's. Children: the child's.
   The heap, volumes, promises and `.log`: as the caller sees them, all
   transactional with the caller. `.env`: the caller's for now, since
   tasks bind no shares yet; the child's composed environment once
   `api-refl-ports` gives tasks shares.

`Op`'s own cuts nest in the caller's transaction, and its `.child.task`
queues on the child's children, reaching them when the caller's
transaction commits, as any queueing in a cut does.

### Outside a transaction

`.child.call Key Op` with no transaction around it queues `Op` on the
child, as `.child.task` does, and waits for its reply: the child runs it
after what was queued before it, and the call answers its result, or
fails with its failure (see Q1 on what that does to the child).

### Slices

Each with tests, the full gate, and a commit.

1. `child-state-transactional` — child state moves into the event state's
   record with its journal; runs take and write it through their commits,
   clears and reclamation drop it. Tests: the existing child task tests
   (state persists between tasks, clears drop it, reclamation drops it),
   and a conflict between a run's end and a transaction that read the
   child's state.
2. `child-call-frame` — the per-branch machine and the call frame within
   a transaction: busy as pushback, the child's state in and out, nesting,
   and conflicts with a task committed meanwhile. Tests: a call reads and
   changes the child's state, which commits with the caller's cut and
   aborts with a failed one; a call to a busy child waits until its task
   ends; nested calls; a task queued on the child after the caller's
   snapshot conflicts, and the cut runs again.
3. `child-call-plain` — the call outside a transaction, queued and
   awaited. Tests: order after earlier tasks, the result, and the
   failure.

## Questions for the maintainer

1. **A plain call's failure.** Outside a transaction, a call runs as a
   queued task. Its failure fails the caller; should it also flag the
   child, as any queued task's failure does, or not, since the caller
   observes it as it happens? Within a transaction a failure never flags
   the child: it aborts with the caller.
2. **A flagged child.** May a call run on a child in error? A Rust turn on
   a flagged machine fails, and only `recover` enters one. A call that
   failed at once would leave recovery as the only way in; one that ran
   would let a parent inspect and repair a failed child within a
   transaction, much as `m.recover` lets a turn.
3. **Ordering against the parent's own queue.** Nothing orders a call
   against tasks the caller queued on the same child within the same
   transaction: those reach the child at commit, after the call ran. This
   seems right, since the call's view is the caller's snapshot, but it is
   a visible ordering.
