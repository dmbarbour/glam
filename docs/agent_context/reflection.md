# Reflection-Task Invariants

This note records the conservative semantics implemented by the bootstrap's
freer-effect machine. See
[`../architecture/reflection.md`](../architecture/reflection.md) for ownership
and control flow.

## Effect Boundary

- `reflection::run` interprets effects outside interaction-net reduction.
  Generic request operators construct singleton dictionaries tagged by hidden
  abstract-global atoms; they do not perform host work themselves.
- `TaskSpecialization` adds a request family, private tags, and transactional
  host data. Reusable request families map their request enum into a host
  specialization rather than depending on it directly.
- Spawned `.refl_task` children receive only `ReflectionEffects`, even when the
  parent has logger-only capabilities.
- `.env Path` reads the active task host's immutable reasoning environment
  using `.get` path and missing-as-`{}` conventions. There is no reflection
  write. The assembler reserves and replaces the complete `glam` subtree and
  supplies authoritative metadata (version, role, etc.).
- `.dict_items` is the narrow privileged dictionary-iteration boundary. It
  returns immediate entries in key order. The compiler's `eff.map` sequences
  effects left-to-right and preserves list order.

## State, Choice, and Control

- `.get`/`.set` access only task-local user state. The active reset stack is
  stored under a private key in that state, so replacing all local state also
  replaces or corrupts the continuation environment. That consequence belongs
  to the user. `.heap.get`/`.heap.set`/`.heap.rewrite` are the distinct
  shared-state effects; `[]` explicitly means the whole local state or whole
  shared heap according to the selected effect.
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
- The heap is an ordinary store volume whose ID and owner are retained by the
  reasoning host. Protected client volumes use the same journal and atomic
  commit, but `.heap.*` can never address them. Exact and fingerprint conflict
  analysis includes `VolumeId`; the coarse strategy may conflict across
  volumes.
- A protected capability request carries `ReasoningSessionId`, `VolumeId`, and
  operation. Child tasks on the same host may use it; a foreign reasoning
  session rejects it before journaling. Volume IDs are session-local and never
  reused.
- Only explicit creation installs a volume. A missing-volume read returns a
  latent error value. Blind writes and rewrites remain blind but commit with a
  terminal missing-volume error rather than recreating storage. Explicit
  whole-volume revocation is serialized with commit and returns the final
  unforced root; dropping its Rust owner has no effect.
- `AssemblerBuilder` owns an unsealed reasoning host. Its environment closure
  may create volumes tied to the future session ID, but no task or evaluation
  context exists until `build()` seals the environment and installs the
  launcher. Draft volume creation never wakes waiters.
- Heap effects impose no dictionary schema. Root replacement accepts any
  value, and nested updates or accesses return ordinary lazy errors when their
  eventual structure is invalid. `.eval` is the explicit way to observe such
  an error as data instead of failing the task.
- The exact strategy is the correctness reference. Fingerprint collisions may
  cause extra retries but never missed overlaps. The coarse strategy treats
  every heap write as conflicting after any heap read. Strategy selection is
  fixed by the builder before a reasoning session becomes runnable.
- `.cut` supplies choice and transaction scope, not retryability. Plain `.fail`
  and `.cut .fail` are permanent. A failed operation retries only when it
  observed changeable host state, such as an empty log queue.
- Top-level `.alt` is invalid. `.shift` continuations are task-local and carry
  a global task ID so foreign invocation fails before consulting a local
  continuation ID.
- Choice and lazy demand are deterministic. A blocked branch waits on at most
  one lazy value, though it may retain several prior state observations. Any
  racing choice must be introduced as a distinct explicit effect.
- Each reflection `fix` alternative gets a task-owned cell. Recursive producer
  observation fails; other tasks receive its wait token. A failed chosen result
  restarts the fixpoint boundary and its transactional alternatives.

## Child Tasks and Evaluation

- `.refl_task` reserves an opaque handle and a private transactional status
  query, but journals launch inside a transaction. Losing branches discard
  both. The query stores atoms `'launched` or `'blocked`, terminal tagged values
  `ok:Value` or `err:Error`, or the atom `'canceled`; the handle keeps it alive.
- `.join_task` waits directly on every nonterminal child state and propagates
  terminal errors. A joined dependency becoming terminal reruns the join
  operation; it does not select another `.alt` branch. An error with prior
  state observations remains blocked until those observations can retry its
  checkpoint. `.task_status` returns the stored status value unchanged.
  `.task_result` and `.task_error` project its matching terminal payload, fail
  transactionally while it is nonterminal, and fail permanently for the other
  terminal outcome.
- `.cancel_task` is an unconditional best-effort, commit-ordered request; late
  and foreign cancellation are harmless no-ops. Losing branches discard
  cancellation requests.
- `.eval` demands WHNF and returns `ok:WHNF` or provisional `err:Text`. A raw
  opaque `Value::Net` is already WHNF and is returned unchanged; only an
  explicit net-arity bridge observes its interface. A pending evaluator
  dependency suspends the request. `.eval` does not isolate or roll back
  reflection tasks activated by evaluation.
- Task failure is never implicitly acknowledged or cleared by inspection.

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
- `EvaluationSession` claims a machine under its mutex, polls outside the
  mutex, then restores its state. It prioritizes known producers and otherwise
  polls ready work in bounded FIFO order. A visited set prevents dependency
  cycles within one pump.
- Ordinary value observation pumps only a demanded producer chain. Unrelated
  reasoning runs through workers or explicit `Assembler::drain_reasoning`.
- Reasoning drain has no timeout or step limit. It includes newly launched
  tasks and ends only when all tasks are terminal or one stable pass proves
  deadlock. Failures, known wait dependencies, and retryably blocked errors
  remain in its report.

## Front-End and Logger Integration

- `anno refl:Effect Target` launches lazily and exposes `Target` only after the
  effect returns unit.
- The g front end wraps ordinary module definitions and members of named
  declared objects with one shared demand boundary for final `refl.*`. The
  `refl`, `meta`, and `spec` subtrees, computed roots, and expression-local
  objects stay inert.
- Object scanner identity derives from final `spec.name`; inherited definitions
  therefore use the derived object's overridable reflection namespace.
- A boundary transaction first records one scanner handle in the shared heap.
  The scanner waits
  for final `refl.*`, launches named tasks in order, requires unit from each,
  and stores ordered `{key,task}` records.
- The CLI logger's assembler-bus input subscription and its session-local
  diagnostic bus are separate. Logger output cannot reopen or feed a sealed
  input stream. Its children inherit `.log`, but not `read_log`, `log_status`,
  or `write_stderr`.
- Logger input has a revision distinct from heap revisions and the coarse wake
  generation. Arriving diagnostics do not invalidate heap-only transactions;
  a committed queue read still validates and consumes input atomically with
  its heap journal.
- Batch execution seals diagnostics only after assembler reasoning drains. A
  logger waiting on an empty input must observe `.log_status` and finish after
  closure. Logger failure falls back to the default formatter and makes the
  process fail.
