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

- Every `.get`/`.set` path is implicitly under user-owned `user_state`. The
  active reset stack is stored under a private key in that state, so replacing
  all user state also replaces or corrupts the continuation environment. That
  consequence belongs to the user.
- Choice frames, journals, immediate sequence state, and host queues remain
  task-owned. An outer `.cut` snapshots shared heap and specialization data;
  failed alternatives discard them, nested success merges upward, and outer
  success validates and commits.
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

- `.refl_task` reserves a handle immediately but journals launch inside a
  transaction. `.cancel_task` is an unconditional best-effort, commit-ordered
  request; late and foreign cancellation are harmless no-ops. Losing branches
  discard launches and cancellation requests.
- `.join_task` propagates terminal errors. `.task_result` and `.task_error` are
  read-only terminal-state extractors. Pending extractors fail with the child's
  exact wait token.
- Mutable task state is not read directly. `.query_task` journals an immutable
  tagged snapshot request and returns a distinct query handle; `.query_result`
  fails until that request commits, then always returns the same
  `pending`, `complete`, `error`, `canceled`, or `foreign` snapshot. An
  uncommitted query result does not install a wake dependency on itself.
- `.eval` forces only successive lazy outer shells and returns `ok:WHNF` or
  provisional `err:Text`. A pending evaluator dependency suspends it. It does
  not isolate or roll back reflection tasks activated by evaluation.
- Task failure is never implicitly acknowledged or cleared by inspection.

## Machine and Scheduler

- `EffectTask` is persistent. Drive, delivery, application, and nested-cut
  frames survive polls; one poll must not leave the machine able to repeat an
  already committed host effect.
- A blocked task reports one lazy dependency plus observed host generation.
  When both changed state and a lazy dependency could resume it, state change
  restarts the saved transaction/retry boundary first.
- `EvaluationSession` claims a machine under its mutex, polls outside the
  mutex, then restores its state. It prioritizes known producers and otherwise
  polls ready work in bounded FIFO order. A visited set prevents dependency
  cycles within one pump.
- Ordinary value observation pumps only a demanded producer chain. Unrelated
  reasoning runs through workers or explicit `Assembler::drain_reasoning`.
- Reasoning drain has no timeout or step limit. It includes newly launched
  tasks and ends only when all tasks are terminal or one stable pass proves
  deadlock. Failures and known wait dependencies remain in its report.

## Front-End and Logger Integration

- `anno refl:Effect Target` launches lazily and exposes `Target` only after the
  effect returns unit.
- The g front end wraps ordinary module definitions and members of named
  declared objects with one shared demand boundary for final `refl.*`. The
  `refl`, `meta`, and `spec` subtrees, computed roots, and expression-local
  objects stay inert.
- Object scanner identity derives from final `spec.name`; inherited definitions
  therefore use the derived object's overridable reflection namespace.
- A boundary transaction first records one scanner handle. The scanner waits
  for final `refl.*`, launches named tasks in order, requires unit from each,
  and stores ordered `{key,task}` records.
- The CLI logger's input queue and its session-local `.log` output are separate.
  Logger output cannot reopen a sealed input stream. Its children inherit
  `.log`, but not `read_log`, `log_status`, or `write_stderr`.
- Batch execution seals diagnostics only after assembler reasoning drains. A
  logger waiting on an empty input must observe `.log_status` and finish after
  closure. Logger failure falls back to the default formatter and makes the
  process fail.
