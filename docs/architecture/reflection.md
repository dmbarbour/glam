# Reflection Architecture

Reflection tasks interpret freer-monad effects outside pure value and
interaction-net semantics. This note describes the current implementation;
[`../agent_context/reflection.md`](../agent_context/reflection.md) contains the
regression-sensitive rules. Runtime work ownership lives in
[`evaluation.md`](evaluation.md), while diagnostic transport and configured
logger lifecycle live in [`diagnostics.md`](diagnostics.md).

## Specialization Layers

`reflection.rs` is the public facade over three ownership layers:

- `reflection/protocol.rs` owns specialization, host, transaction, and
  structured task-outcome contracts;
- `reflection/lifecycle.rs` owns scheduled runs, lifecycle publication, and
  type-erased task launchers; and
- `reflection/machine.rs` owns the persistent interpreter, control frames,
  retry state, and request decoding.

`TaskSpecialization` contributes an additional request enum, private request
tags, host behavior, and transactional host data. Request families remain
reusable by mapping their request type into a specialization. The existing
`requests`, `search`, and `store` children retain their focused roles rather
than becoming implementation details of one of the three layers.

The protocol owns the root shape of its concrete fields: request results,
task outcomes, and reflection environments cross phases only as public values,
while task failures use the explicit rooted-or-bounded disposition described
below. A specialization owns the corresponding contract for semantic values
inside its generic request, snapshot, and journal types. Such values must be
public/runtime roots; generic specialization state is not an exemption for
storing bare core values across a transaction or search boundary. Borrowed
request and transaction contexts remain bounded to one interpreter callback.

The reusable `ReflectionEffects` family adds environment lookup, diagnostic
emission, dictionary iteration, lazy-shell value observation, and child-task
operations. `main` defines a broader logger specialization with diagnostic
stream reads and stderr output. Tasks queued on child machines through
`.child.task` inherit their parent's complete profile, including its
additional operations, role environment, and diagnostic destination.

`refl` and `meta_refl` annotations instead use the runtime's immutable default
reflection profile. Their behavior therefore does not depend on whether an
assembler, macro, logger, or other demand session happened to claim the lazy
annotation first. Tasks an annotation queues inherit that default profile
from the annotation task.

Reusable reflection request journals retain diagnostics as public roots and
the machines' port actions as handle values. Their commit-time machine
updates (`ReflectionUpdate::Wake`, `Interrupt`, `Clear`) name a host and a
machine path, plus the launching context for a wake; they contain no hidden
semantic value.

Each annotation lazy's managed reflection computation owns and traces the
immutable effect, the optional gate target, and a managed completion promise.
The first producing poll reserves a task in the runtime's background demand
domain, registers it as that promise's producer, and leaves the lazy waiting
on the promise through an ordinary WHNF checkpoint. A one-use permit
temporarily roots the effect and selected launch policy. The evaluator-step
boundary ends before that permit invokes the type-erased launcher; successful
activation transfers ownership into the coordinator task machine. Later polls
find the registered producer and construct no competing machine or permit.
Dropping an unconsumed permit cancels the reservation and fails its promise.
A pre-activation terminal state suppresses launcher construction;
cancellation or demand closure racing an already-entered launcher keeps the
terminal state and discards the unused machine. The task's terminal outcome
is mapped onto the promise; [`evaluation.md`](evaluation.md) describes the
mapper and the anonymous root that the task's failure flags.

Core operators merely construct tagged request values. Host operations occur
when the effect task dispatches those requests.

Permanent `TaskHalt` values distinguish freshly constructed, text-only
validation failures from failures carrying semantic diagnostic values. The
latter retain a `RuntimeFailureRoot`: coordinator terminal publication moves
the existing root into the lifecycle view, structured public errors establish
one from their diagnostic value, and a direct `EffectRun` roots any bounded
evaluator failure before returning it to its Rust caller. Adding a context to
an already rooted halt rebuilds the root for the expanded failure in the same
runtime. Isolated-search publication and parked machine compatibility failures
remain assigned to their search and machine root checkpoints respectively.

## Persistent Effect Machine

An `EffectTask` retains its continuation, application, alternative, and nested
transaction frames across bounded polls:

```text
effect value
  -> drive until request or result
  -> dispatch request through TaskHost
  -> deliver success, failure, or suspension
  -> preserve frames in the coordinator-owned work record
```

Standard effects include `r`, `seq`, `alt`, `fail`, `cut`, `fix`, indexed
task-local `get`/`set`/`try_get`/`del`, shared
`heap.get`/`heap.set`/`heap.rewrite`/`heap.try_get`/`heap.del`, volume
handles' `heap.vol` and
`vol.new`/`vol.get`/`vol.set`/`vol.rewrite`/`vol.try_get`/`vol.del`,
`promise.new`/`promise.status`/`promise.fulfill`/`promise.break`, and indexed
`reset`/`shift`.
Local user state, including the reset stack, is ordinary task state. Shared
store state is staged separately in the runtime transaction; it is never
projected into local state. The ordinary heap is one runtime-owned store
volume shared by every attached reasoning host. Choice frames and journals
remain machine or host bookkeeping.

An outer `cut` provides an optimistic transaction boundary. Alternatives start
from snapshots; losing branches discard changes; a winning outer branch
validates and commits. A host observation can turn later failure into a retry
point. `cut` alone does not: unobservant failure is terminal.

`reflection/search.rs` supplies a second outer policy for clients that need all
effect results, such as configured CLI parsing. It installs one uncommitted
outer transaction, explores successful alternatives in deterministic
left-to-right depth-first order, and returns each value with its isolated
specialization journal. Nested `cut` still chooses at most its first success.
No search branch commits to the host. A retryable observation or lazy
dependency suspends the pollable search without discarding pending branches;
an observed-state change conservatively restarts the complete isolated search.
Ordinary `EffectRun` retains its explicit-cut requirement and single-result
behavior.

The isolated host environment, branch values, and returned result collection
are public roots. Specialization snapshot/journal roots follow the protocol
contract above. Retryable and terminal search failures are rooted before a
poll result crosses back to the host, including errors retained beside an
observed generation. Restart drops branch-local alternatives and results but
retains the immutable root branch; a returned `Arc` result collection owns its
branches independently of the search machine. The nested `EffectTask` remains
the separately inventoried machine owner rather than duplicating its interior
ownership in the search wrapper.

A specialization request may return an ordered collection of alternatives.
The machine resumes the current continuation once for each value using the
same ordinary choice machinery; an empty collection is effect failure. This is
used by nested configured-CLI token parsing so distinct structured token
results remain distinct outer parse branches rather than being collapsed in
host code.

Both outer policies use the same standard-effect machine. The machine separates
deterministic effect failure, current dependency waits,
and evaluation errors. A dependency becoming terminal reruns the unchanged
operation that observed it. A non-blocking evaluation error remains retryably
blocked only when an existing state observation can rewind its checkpoint; it
does not advance `.alt`. The scheduler receives only the dependency token,
coarse retry generation, and retained structured evaluation failure.

Each machine step alternates a bounded, callback-free evaluator phase with an
interpreter phase. Request payloads and evaluator results are rooted before the
evaluator phase ends; host snapshots, commits, and specialization callbacks run
only afterward. A specialization constructs non-cloneable request work, whose
short callback may advance locally, request one semantic demand, or finish.
The reflection machine owns that demand and later returns an `EvaluatedValue`
or structured failure to the already-advanced request work. `RequestContext`
exposes host/transaction state and value construction but no evaluator
authority, so suspension cannot replay prior callback activity. Poll and
evaluator authorities are thread-bound and cannot be retained by the `Send`
effect machine.

### Hosted WHNF Request Decoding

`EffectDecodeWork` owns either one scalar WHNF demand or one request decoder;
it never clones the operation in order to retry it. Request decoding first
selects the request tag inside bounded evaluator access and roots the selected
payload before leaving that region. `RequestDecodeState` then advances from
`Select` to `PayloadWhnf`, through incremental `PayloadItems` list-front work,
and, for resume requests, through `ResumeTask` and `ResumeContinuation` before
publishing the rooted request.

Every payload demand owns its `WhnfComputation`, and list-front iteration and
nested WHNF polls borrow the same `EvaluationStepBudget`. `Pending` retains the
exact dependency and `BudgetExhausted` retains the exact decoder state, so neither
outcome repeats tag selection, list traversal, or continuation construction.
Host dispatch begins only after decoding is complete and evaluator access has
closed. Scalar and specialization
demands follow the same rule; a specialization callback receives an evaluated
public value only after its owned WHNF computation completes.

There is no fused chain or fusion budget. After the shared decoder produces a
request, a dispatch shortcut handles four task-local requests directly, one
machine step each:

- `.seq` pushes its Glam continuation and decodes the operation next;
- `.r` applies a Glam continuation on top of the sequence stack directly;
- `.get` and `.set` enter task-local state-path work.

Every other request takes the general interpreter. Choice and cut,
reset/shift/fix, shared-state and task requests, logging, reflection,
specialization callbacks, and non-Glam delivery stay at explicit interpreter
boundaries because they publish control, transaction, promise, or host
obligations. Tests can force the general path, but it shares the decoder, so
it is not an independent oracle.

`reflection/store.rs` owns the volumes' committed versions, transaction
snapshots, ordered edit overlays, rebasing, and commit. Its private
`reflection/store/conflict.rs` child owns volume-qualified hierarchical conflict
paths and the pluggable observation strategy/index contract. The store retains
exact changed addresses independently of host wake state. Blind sets and
rewrites, including overlapping parent and child paths, serialize in commit
order while their target volume exists. The runtime-selected
`Arc<dyn ConflictAnalysisStrategy>` controls only how reads are summarized.
The bootstrap supplies exact, conservative fingerprint, and fully coarse
strategies; clients may implement the public strategy contract without access
to the private child module. `EvaluationRuntime` fixes the strategy at
construction, and an assembler which attaches that runtime cannot replace it.

Heap paths are ordinary lazy value operations rather than a store schema.
`.heap.set` stages a replacement without inspecting the old heap.
`.heap.rewrite Path Updater` lazily applies `Updater` to the commit-time value
at `Path`, allowing concurrent rewrites to serialize without retrying. A later
local read remains snapshot-dependent through a rewrite; an ancestor rewrite
widens a descendant read to the updater's complete input path. An earlier
covering set can still make that widened dependency entirely local.
`.heap.get` returns an unforced access value; malformed roots, updates, and
updaters therefore remain latent evaluator errors, which `.eval` can observe
as data. `.try_get Path` gives the value where `Path` is defined and fails
where not, forcing the dicts on the way, never the value; within a cut its
read is observed as `.heap.get`'s is, so a cut that fails on it waits for
the path's definition. `.del Path` removes what is there, pruning the dicts
the removal empties, a store edit that conflicts as a set does
(`dict-presence-ops`). The heap's and volumes' forms are alike.

Volumes are first-class capabilities (`requests-travel-on-ports`): a handle
is an opaque value naming one volume of one runtime, the same that machines'
`volume_*` operations take. `.vol.new Initial` allocates a volume, within a
cut's transaction or in a commit of its own, and returns its handle;
`.heap.vol` returns the shared heap's. `.vol.get`, `.vol.set` and
`.vol.rewrite` take a handle first, evaluate it, never the value, and then
read and stage as the heap effects do on that volume.

A volume is a managed cell holding its ID, and a handle is a value of its
own kind, `Volume`, one edge to the cell, which Glam sees as opaque
(`api-volume-cells`, `opaque-resources-reclaimed-when-unreachable`). The
store maps each volume to its committed version: a root on an ephemeron
keyed on the cell, whose value is a `Contents` cell holding the volume's
value, one per commit, since snapshots keep the versions they began from.
The store's root holds the ephemeron, never the cell or the value, so a
volume lives while some other path reaches its cell: a handle, the store
for its own volumes and a host's until revoked (`kept`, which snapshots
share), or a transaction that reads, writes or creates it, until it ends;
its commit keys each new version on the cell it holds. The store's users
reach a volume only through a view of its cell, `VolumeCellView`: reads
return a view of its contents, a version's, checked against the version's
key, or the journal's draft, and leave rooting to the caller; writes and
read observations take a cell view, and creation returns one. Only hosts'
protected volumes, which the store keeps, go by ID (`*_kept_volume*`). A
request names its volume by a `VolumeTarget`, the handle it decoded or a
host's volume by ID, which it resolves under access. A journal's writes
build drafts, contents cells on which its commit keys the new versions. Contents reach the cell only
through their ephemeron, so a volume reaching its own handle, or volumes
reaching each other's, are collected. A collected volume's versions clear
silently: nothing can name it again, so no transaction conflicts. The store
prunes cleared versions as it creates a volume, the only change that grows
its map, if a collection has completed since it last pruned. A read roots
its result alone, under an access region; a machine's poll, which holds
one, uses the journal's `_in` forms.

Promises are shared with machines through the store
(`reflection/promises.rs`, `promise-cells-are-weakly-resolved`). A promise's
identity is its cell's ID. `.promise.new` returns `{promise, resolver}`; the
resolver is an edge to the promise's breaker, which keeps its status, and
the store holds each pending promise's cell only weakly, by its ID, so the
promised value alone keeps the cell. `.promise.status`, `.promise.fulfill`
and `.promise.break` take the resolver, which decoding evaluates, and the
request holds its breaker until it completes. A status is `'pending`,
`'fulfilled` or `'broken`; a promised value has none, since it stands for
whatever it evaluates to. A lost resolver's breaker queues its promise's ID,
and the runtime breaks the promise, if still pending, after the
collection. Within a cut a status read observes the
promise's place in the runtime's volume, which a resolution writes once it
has published the value, so the read conflicts and wakes precisely. A
resolution is a store edit (`promise-resolution-commits-with-its-cut`),
which holds the breaker until its transaction ends: within a cut it commits
with the cut, which sees it in the promise's status
but not in its value, and outside one it commits on its own. A resolution
that another commits first conflicts, and the rerun finds the promise
resolved.

The runtime transaction lock makes reflection-store and generic event changes
atomic. A failed validation cannot partially edit the heap, consume admitted
input, or leak buffered output from an abandoned choice. The outer mutation
gate spans authoritative publication through the broad observation-epoch
advance; wakes, callbacks, decoding, and value destruction run after release.
The diagnostic architecture owns the logger-specific ingress and outbox flow.

## Protected Client Volumes

`AssemblerBuilder` selects the runtime and creates an unsealed host before
constructing the reflection environment. Its environment closure may therefore
create runtime-owned protected volumes and embed their capabilities. `build()`
then seals the environment. The first assembler attached to a dormant runtime
also seals its default annotation-task profile; later assemblers reuse that
profile and cannot replace it. Each assembler still owns its complete task
profile, which its evaluation contexts carry. Runtime resources and the
default profile are held as siblings: the retained host may keep the
resources alive, but has only a weak link back to the profile. This avoids a
runtime/profile ownership cycle while allowing cached evaluation contexts to
keep their resources usable. Selecting another runtime or conflict strategy
after runtime-bound builder state exists makes construction fail.

`Assembler::create_volume` installs an explicitly initialized volume and
returns a Rust owner handle. The handle exposes one closed Glam
`{get,set,rewrite}` capability value. Possession is authority: the functions
are not members of the ordinary reflection API, while `.heap.*` remains rooted
to the runtime's shared heap volume.

The capability value carries its evaluation-runtime provenance and embeds the
runtime-local `VolumeId` plus its operation. Possession authorizes use from any
reasoning or demand session in that runtime. Another runtime rejects the
capability value before the request enters a store journal.

The owner explicitly revokes the complete volume and recovers its final
unforced value. Volume IDs are never reused. A missing `get` returns a latent
error value; blind sets and rewrites still enter the journal but fail
permanently at commit, so they cannot recreate a revoked volume. Revocation is
serialized with commits through runtime transaction state and mutation
admission. It records a root change, causing transactions that read the old
volume to retry. Dropping the Rust owner does not revoke it.

## Reusable Reflection Requests

- `.env Path` reads the active task host's immutable reasoning environment.
  The authoritative `glam.origin.inspect` capability projects assembler-created
  opaque compilation-origin values into ordinary provenance. It rejects other
  opaque values; callers choose which context value to inspect.
- `.log Severity Message` stages a diagnostic in the current transaction and
  publishes it through the session's diagnostic bus only after commit.
- `.dict_items Dict` returns ordered `{key,value}` records.
- `.eval Value` is `.try (seq Value (.r Value))` (`try-reports-outcomes`):
  `ok:Value`, forced to weak-head normal form, `err:Diagnostic`, or
  `wait:Diagnostic` for a promise no one has resolved.
- `.try Op` runs `Op` once as a transaction and returns its outcome as
  data: `ok:Value`, the atom `'fail` for an explicit failure, `wait:Diagnostic`
  for a promise no one has resolved, `conflict:Diagnostic` for a commit
  another's interfered with, or `err:Diagnostic` for an evaluation error.
  Every outcome but `ok` undoes `Op`; within a cut it nests. What it
  observed and waited on stays with the transaction around it, so a cut
  that fails on `wait` parks until the promise progresses. Where a turn
  runs the effect, the turn decides which waits are outside the effect: a
  request it serves parks on a promise it resolves. A raw opaque `Value::Net` is already WHNF and is returned
  unchanged; only an explicit net-arity bridge observes its interface.
- `.child.task Key Op` queues `Op` on a child machine of the current
  task's machine, its task input a port named by an abstract global path,
  and answers the promise of its result; `.child.status Key` reads
  `'idle`, `'busy` or `'error`, observed within a cut. A task that a child's
  queue starts runs on that child (`EvalContext::machine`), whose local
  state persists between tasks as transactional state, written in the
  commit that ends each run; any other task is
  an anonymous root, `refl/<abstract task id>`. The finishing task claims
  its queue item, resolves its reply, flags the machine on failure and
  starts the next item itself (`MachineRunEnd`). `.child.interrupt`,
  `.child.recover` and `.child.clear` edit the queue and flags within the
  transaction; an interrupt, transitive, stops runs at commit, and a run
  is named by its item's sequence, so a stopped task's end claims nothing.
  Recovering a busy child, or clearing a subtree with a busy machine,
  fails having observed it (`ChildBusy`), so it waits as `.child.call`
  does.
- `.child.call Key Op`, within a transaction, runs `Op` on the child within
  the caller's transaction and answers its result (`child-call-frame`). It
  reads the child's flag, queue and state, observed: a child in error is
  an error; a busy child fails, so the call waits for its queue; and tasks
  the transaction itself queued on the child, which the child starts only
  once the transaction commits, make a wait that could never end, an
  error. An error in `Op` is undone with the transaction, `.try` answering
  it or a cut waiting for a change, so it flags nothing. It answers a scoped request: the
  operation is `Op`, and the close, a hidden request, writes the state
  `Op` left as the child's in the caller's journal. Each branch keeps the
  calls it runs within (`ChildCall`: the child, and the caller's state the
  close restores); the innermost call's child is the machine the `.child.*`
  requests act on, else the task's own. A failed or tried branch drops the
  call with itself. Since the reset stack is part of a branch's state, a
  `.shift` within `Op` reaches only `Op`'s own resets. Outside a
  transaction a call is plain (`child-call-plain`): it queues `Op` as
  `.child.task` does, admitting the child by its flag in the queueing's
  journal so a child in error fails it at once, and answers
  `RequestResult::Await` on the reply, which waits for it and fails with
  the task's failure.

The immutable environment conventionally contains assembler-owned `glam`
identity plus client context. `glam.reasoning.role` distinguishes assembler,
logger, and future service sessions. `main` adds process arguments,
reflection-only arguments, and binary-preserving OS environment data. This is
data installed by the client, not command-line policy embedded in the
reflection API.

## Session Scheduling

`EvaluationSession` is the external demand-owner lease; it does not store a
task registry. The runtime coordinator owns reflection work records, their
machines while not exclusively claimed, task/wait indexes, terminal
publication, dependencies, and failed anonymous roots. Claims take a machine
from one record, poll outside the coordinator lock, and restore or terminalize
it before the record becomes claimable again.

Foreground demand follows exact producer chains. Workers may opportunistically
claim ready work, and explicit runtime drain includes newly spawned tasks
without a timeout or step budget. Exact subscriptions and broad observation
epochs are coordinator mechanisms described authoritatively in
[`evaluation.md`](evaluation.md); reflection-store read journals independently
decide whether an optimistic transaction conflicts.

## Sources of Tasks

`anno refl:Effect Target` creates a lazy gate. Demand launches `Effect`; unit
success reveals `Target` without forcing it. An annotation task, `refl` or
`meta_refl`, has no machine of its own: it is an anonymous root,
`refl/<abstract task id>`, whose failure flags that root for good, since no
parent can recover it, even when a demander observed the failure through the
gate (`reflection-tasks-are-machine-tasks`). A Rust-scheduled root
(`EffectRun`, `schedule_task`) reports its failure only to its Rust caller,
and the runtime does not record it. A composed effect run drains the work it
scheduled and fails on its own result or a stable deadlock, not on a child's
failure, which is its machine's.

The built-in g front end also decorates ordinary module definitions and named
declared-object members with one-shot boundaries. Demand queues one scanner on
the boundary task's child `'scanner`, which waits for final `refl.*`, then
queues each named task, in deterministic order, on its own child keyed by its
name (`.child.task`). Guards are stored with explicit shared-heap effects under
identities derived from module paths or final object `spec.name`, holding the
tasks' promises. A failed task's machine stays flagged, and settlement reports
it (`QuiescenceReport::failed_machines`), which fails a CLI batch.

## CLI Logger Session

This is the configured logger during the runtime API's transition; its exit
vote retires with `quiescence-reports-unresolved-state`.

Configured `conf.log` is a reflection task with a broader specialization.
Main-only requests read the diagnostics waiting at the CLI's `logger`
machine, buffer stderr or diagnostic output, and coordinate
`.exit.success`/`.exit.error`. The input stream has no close request and
logger output uses a separate bus, so it cannot feed its own input. The
root's end closes its demand session.

See [`diagnostics.md`](diagnostics.md) for the logger's wiring, transactional
transport, its hand-over to the fallback, and rendering, and
[`assembly.md`](assembly.md) for settlement and process exit.
