# Reflection-Task Invariants

This note records the conservative semantics implemented by the bootstrap's
freer-effect machine. See
[`../architecture/reflection.md`](../architecture/reflection.md) for ownership
and control flow.

## Effect Boundary

- `reflection::run` interprets effects outside interaction-net reduction.
  Generic request operators construct singleton dictionaries tagged by hidden
  abstract-global atoms; they do not perform host work themselves.
- A specialization's owned request work reports demanded-value failures as a
  rooted `SpecializationRequestInput::Failed`; it does not reconstruct an
  evaluator failure after suspension. A `TaskHalt` is only a permanent
  failure or a panic: reflection machines report evaluator waits as
  `WorkDependency` values, never as halts, so there is no conversion from
  `EvaluationHalt`. A bounded evaluator failure enters through
  `TaskHalt::failure`, and public facade failures use
  `TaskHalt::from(Error)`. A failure is rooted before it
  enters a lifecycle or direct `EffectRun` result; converting an existing
  coordinator terminal must preserve its `RuntimeFailureRoot` rather than
  reconstructing it from the raw failure. Isolated-search publication remains
  the next explicit boundary. All paths retain diagnostic values and context
  stacks. `TaskHalt::new` is only for genuinely new text-only validation or
  host errors.
- `TaskSpecialization` adds a request family, private tags, and transactional
  host data. Reusable request families map their request enum into a host
  specialization rather than depending on it directly.
- A specialization callback advances non-cloneable request work before asking
  the reflection machine to demand a value. Dependency resumption returns to
  that advanced owner; it must not reconstruct or replay host observation or
  mutation. `RequestContext` deliberately has no semantic evaluation methods.
- Concrete protocol results, outcomes, environments, snapshots, commits, and
  transactions retain public/runtime roots across interpreter phases. A
  specialization is responsible for the same root contract inside its generic
  request, snapshot, and journal types; those types may not hide a durable bare
  core value. `RequestContext` and `TransactionContext` are borrowed,
  callback-bounded views and must not escape the dispatch that created them.
- Tasks queued on child machines with `.child.task` inherit their parent's
  complete task profile, including specialization requests, immutable
  environment, diagnostic destination, and host resources. Annotation tasks
  begin with the runtime default profile, so the tasks they queue inherit
  that default.
- `.env Path` reads the active task host's immutable reasoning environment
  using `.get` path and missing-as-`{}` conventions. There is no reflection
  write. The assembler reserves and replaces the complete `glam` subtree and
  supplies authoritative metadata (version, role, etc.) plus
  `glam.origin.inspect`. That capability only projects assembler-created
  opaque compilation origins. Nothing scans diagnostic contexts or invokes it
  automatically.
- `.dict_items` is the narrow privileged dictionary-iteration boundary. It
  returns immediate entries in key order. The compiler's `eff.map` sequences
  effects left-to-right and preserves list order.
- Reusable reflection journals store diagnostics as public roots and the
  machines' port actions as handle values. Their commit-time machine updates
  (`ReflectionUpdate::Wake`, `Interrupt`, `Clear`) name a host and a machine
  path, plus the launching context for a wake, and store no semantic value.

## State, Choice, and Control

- `.get`/`.set` access only task-local user state. The active reset stack is
  stored under a private key in that state, so replacing all local state also
  replaces or corrupts the continuation environment. That consequence belongs
  to the user. `.heap.get`/`.heap.set`/`.heap.rewrite` are the distinct
  shared-state effects; `[]` explicitly means the whole local state or whole
  shared heap according to the selected effect.
- `.vol.*` take an opaque volume handle first; `.heap.vol` is an effect
  returning the heap's handle, not a value in the API, since every dotted
  path is effect sugar. `.promise.*` take a resolver, never the promised
  value; a status read in a cut is a store read, and a resolution is a
  store edit that commits with its cut, so a lost race conflicts.
- Choice frames, journals, immediate sequence state, and host queues remain
  task-owned. An outer `.cut` snapshots shared heap and specialization data
  without inserting either into local state; failed alternatives discard
  changes, nested success merges upward, and outer success validates and
  commits.
- Shared-heap reads record their required snapshot dependency, including
  missing paths and `[]`. Sets and rewrites are unvalidated lazy edits and
  observe nothing: overlapping blind edits serialize in commit order. A prior
  local set at or above a read path masks that snapshot read. A rewrite does
  not: an ancestor rewrite widens a later descendant read to the updater's
  complete input path, though an earlier covering set can still make the
  widened dependency local. Earlier observations remain. The conflict-analysis
  strategy may only conservatively summarize reads and must never redefine
  edit semantics.
- The heap is an ordinary volume, whose cell the runtime reflection store
  keeps. Each volume's committed versions are ephemerons keyed on its cell,
  so a Glam volume lives while its handles, or a transaction reading or
  writing it, do (`api-volume-cells`). The store's users reach a volume
  through a `VolumeCellView`, and reads return a `VolumeContentsView`;
  only hosts' protected volumes go by ID (`*_kept_volume*`). Requests carry
  a `VolumeTarget`. Protected client volumes use the same journal and
  atomic commit, but `.heap.*` can never address them. Exact and fingerprint
  conflict analysis includes `VolumeId`; the coarse strategy may conflict
  across volumes.
- A protected capability value carries evaluation-runtime provenance and a
  request containing `VolumeId` plus operation. Any reasoning or demand
  session in that runtime may use a capability explicitly supplied to it;
  another runtime rejects the value before journaling. Volume IDs are
  runtime-local and never reused.
- Only explicit creation installs a volume. A missing-volume read returns a
  latent error value. Blind writes and rewrites remain blind but commit with a
  terminal missing-volume error rather than recreating storage. Explicit
  whole-volume revocation is serialized with commit and returns the final
  unforced root; dropping its Rust owner has no effect.
- `AssemblerBuilder` owns an unsealed reasoning host. Its environment closure
  may create runtime-bound volumes before any task or evaluation context
  exists. `build()` seals the environment and installs the launcher. Draft
  volume creation never wakes waiters.
- Heap effects impose no dictionary schema. Root replacement accepts any
  value, and nested updates or accesses return ordinary lazy errors when their
  eventual structure is invalid. `.eval` is the explicit way to observe such
  an error as data instead of failing the task.
- The exact strategy is the correctness reference. Fingerprint collisions may
  cause extra retries but never missed overlaps. The coarse strategy treats
  every heap write as conflicting after any heap read. Strategy selection is
  fixed by the builder before a reasoning session becomes runnable.
- `.cut` supplies choice and transaction scope, not retryability. Plain `.fail`
  and `.cut (.fail)` are permanent. A failed operation retries only when it
  observed changeable host state, such as an empty log queue.
- Top-level `.alt` is invalid for ordinary reflection tasks. The isolated
  all-results runner is an explicit host policy that supplies its own outer
  transaction and may enumerate top-level alternatives without changing that
  general rule. `.shift` continuations are task-local and carry a runtime-local
  task identity so invocation by another task fails before consulting a local
  continuation ID.
- Isolated all-results search never commits its branches. It retains successful
  values and specialization journals in deterministic left-to-right order;
  nested `.cut` remains first-success. A changed state observation restarts the
  whole isolated search conservatively, while a lazy dependency resumes its
  current branch.
- An isolated-search poll roots both retryable blocked errors and terminal
  errors before publishing them to its host. Restart/discard retires only
  branch-local alternatives and results, retaining the immutable root branch;
  a returned result collection owns its branches after the search retires.
- Choice and lazy demand are deterministic. A blocked branch waits on at most
  one lazy value, though it may retain several prior state observations. Any
  racing choice must be introduced as a distinct explicit effect.
- Each reflection `fix` alternative gets a task-owned cell. Recursive producer
  observation fails; other tasks receive its wait token. A failed chosen result
  restarts the fixpoint boundary and its transactional alternatives.

## Child Tasks and Evaluation

- `.child.task Key Op` queues `Op` on the child machine `Key` of the
  current task's machine and answers the promise its result fulfills or
  its failure breaks (`reflection-tasks-are-machine-tasks`). A task queued
  from a child's queue runs on that child; any other task is an anonymous
  root, `refl/<abstract task id>`, left out of listings. The queue is the
  child's task input, a port named by an abstract global path: the task
  reaches the child when the queueing commits, so a cut that awaits the
  promise of a task it queued, as `=>>` does by requiring unit, can only
  wait. A child runs its tasks one at a time, in order; its local state
  persists from task to task, taken when a task starts and written back
  when it ends (the state before the outermost cut, where a failure
  undoes one). A failure claims the task, breaks its reply and flags the
  machine in one commit, and the queue stops.
- `.child.status Key` reads `'error` while the child is flagged, `'busy`
  while its task input holds a task, queued or running (the head stays in
  the buffer until it finishes), and `'idle` otherwise. Within a cut the
  read is observed, so a failed cut waits for the status to change.
- `.child.interrupt Key Reason` takes the running tasks (the heads of
  unflagged queues) of the child and its descendants, breaks their replies
  with `Reason` and flags their machines; `.child.recover Key` clears a
  flagged child's flag and queue, breaking the dropped replies;
  `.child.clear Key` empties the child and its descendants as a turn's
  clear does, breaking every reply it drops. Each is a transactional
  edit. Only an interrupt stops work (`reflection-tasks-are-machine-tasks`):
  recovering waits while the child is busy, and clearing while it or a
  descendant is, as `.child.call` does (the edit fails having observed the
  busy machine, so a cut waits, and `.try` reports `'fail`). Recovery and
  clearing observe a queue's end, so a task queued meanwhile conflicts.
- The runtime's `MachineHost` (`api/runtime/machines.rs`) keeps each
  child's run, named by the sequence of the running task's item in the
  queue (`TaskMachine::sequence`); its task input, flag and local state lie
  in the transactional event state (`api/runtime/flags.rs`,
  `api/runtime/states.rs`), whose journals validate the flags and the
  versioned states they read. A run takes its state as it starts and
  writes the state it leaves in the commit that ends it, so a transaction
  that read the state, as a call will, conflicts with it. A queued task's launch is a
  commit-time `ReflectionUpdate::Wake`; a finishing task starts the next
  itself. An interrupt stops the run at commit
  (`ReflectionUpdate::Interrupt`): the stopped task's end claims nothing,
  since the item is gone, and the machine runs nothing else until it ends.
  A clear's commit (`ReflectionUpdate::Clear`) drops local state, and the
  state a task still winding down there leaves. Every way a task on a machine ends, teardown cancellation
  included, ends its run (`MachineRunEnd`), and a run ends once
  (`TaskMachine::end`). A task that stops without finishing, as a panic
  stops one, or that never launches, halts its run through the
  `MachineRunGuard` it holds: the reply breaks with "the task halted before
  it finished" and the machine is flagged. A halt where the thread holds a
  mutator, or unwinds, waits on the host for its next safe point
  (`run_deferred`, also run with the death notices).
- An anonymous root's subtree is unreachable once the root's task ends: its
  keys name a task id never reused. The root's task, once it tasks a child,
  holds an `AnonymousRootGuard` reporting its end, and the host reclaims
  the subtree, its cells and port buffers, when the last of the root and
  its runs ends with no task waiting in an unflagged queue. Failed machines
  stay whole for settlement.
- An annotation task (`anno refl:` or `anno meta_refl:`) has no machine of
  its own: it is an anonymous root, `refl/<abstract task id>`, and its
  failure flags that root for good, since no parent can recover it, even
  when a demander observed the failure through the gate
  (`reflection-tasks-are-machine-tasks`). A Rust-scheduled root's failure
  (`EffectRun`, `schedule_task`) goes only to its Rust caller and is not
  recorded. A composed effect run drains the work it scheduled and fails on
  its own result or a stable deadlock, not on a child's failure, which is
  its machine's.
- `.eval` demands WHNF and returns `ok:WHNF` or `err:Diagnostic`. A raw
  opaque `Value::Net` is already WHNF and is returned unchanged; only an
  explicit net-arity bridge observes its interface. A pending evaluator
  dependency suspends the request. `.eval` does not isolate or roll back
  reflection tasks activated by evaluation.
- Observing a failure never clears a flag; a parent recovers or clears its
  child explicitly.

## Machine and Scheduler

- `EffectTask` is persistent. Drive, delivery, application, and nested-cut
  frames survive polls; one poll must not leave the machine able to repeat an
  already committed host effect.
- A blocked task reports one current dependency, an optional retry generation,
  and an optional retained evaluation error. Dependency completion reruns the
  unchanged operation. A non-blocking error is terminal unless earlier state
  observations provide a retry checkpoint; it never becomes effect `.fail`.
  When both changed state and a dependency could resume a task, state change
  restarts the saved transaction/retry boundary first.
- The runtime coordinator owns each reflection machine while claimable. A
  claim takes the machine and marks its work record running under the
  coordinator lock; polling happens outside the lock; release restores it
  before requeueing/blocking or returns it for terminal destruction. There is
  no session-side machine registry.
- Ordinary value observation pumps only a demanded producer chain. Unrelated
  reasoning runs through workers or explicit `Assembler::drain_reasoning`.
- Runtime-wide reasoning drain has no timeout or step limit. It includes newly
  launched work from every demand session and returns a stable
  `RuntimeReadiness` snapshot. Ready exit votes require explicit settlement;
  deadlock snapshots retain known dependencies and structured retryable
  failures until the client chooses whether to kill them. A settled
  `QuiescenceReport::failed_machines()` lists every machine in error as a
  `FailedMachine` (path and diagnostic): flagged child machines and failed
  anonymous roots, in path order; `pending_failed_machine_reports()` lists
  each once for rendering.

## Front-End and Logger Integration

- `anno refl:Effect Target` launches lazily and exposes `Target` only after the
  effect returns unit.
- The g front end wraps ordinary module definitions and members of named
  declared objects with one shared demand boundary for final `refl.*`: one
  annotated unit per module or declared object instance, which each wrapped
  value forces (`seq`) before exposing itself, so the boundary effect runs
  once. The `refl`, `meta`, and `spec` subtrees, computed roots, and
  expression-local objects stay inert.
- Object scanner identity derives from final `spec.name`; inherited definitions
  therefore use the derived object's overridable reflection namespace.
- A boundary transaction first records one scanner's promise in the shared
  heap (`[guard, 'claim]`): it queues the scanner on the annotation task's
  child `'scanner` with `.child.task`, below the task's anonymous root. The
  scanner waits for final `refl.*`, queues each named task, in order, on its
  own child keyed by the task's name, requires unit from each, and stores
  ordered `{key, task}` records whose `task` is the task's promise
  (`[guard, 'tasks]`). A failing `refl.*` task flags its machine, which
  settlement reports among `failed_machines`, as the CLI renders
  (`runtime: machine failure (machine refl/<abstract>/scanner/<key>)`) and
  fails the batch on.
- The CLI logger's assembler-bus input and its session-local diagnostic bus are
  separate. Logger output cannot feed its own input stream.
- Logger input claims and heap edits share one atomic runtime commit without
  sharing conflict addresses. See
  [`diagnostics.md`](diagnostics.md) for ingress, output, and rendering
  invariants and [`assembly.md`](assembly.md) for settlement and exit policy.
