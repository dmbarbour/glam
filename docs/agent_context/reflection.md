# Reflection-Task Invariants

This note records the conservative semantics implemented by the bootstrap's
freer-effect machine. See
[`../architecture/reflection.md`](../architecture/reflection.md) for ownership
and control flow.

## Effect Boundary

- `reflection::run` interprets effects outside interaction-net reduction.
  Generic request operators construct singleton dictionaries tagged by hidden
  abstract-global atoms; they do not perform host work themselves.
- Evaluation failures crossing a specialization request use
  `TaskHalt::from(EvaluationHalt)`, and public facade failures use
  `TaskHalt::from(Error)`. Both retain diagnostic values and context stacks;
  `TaskHalt::new` is only for genuinely new validation or host errors.
- `TaskSpecialization` adds a request family, private tags, and transactional
  host data. Reusable request families map their request enum into a host
  specialization rather than depending on it directly.
- Spawned `.task.new` children inherit their parent's complete task profile,
  including specialization requests, immutable environment, diagnostic
  destination, and host resources. Annotation tasks begin with the runtime
  default profile, so their children inherit that default.
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
- The heap is an ordinary volume whose ID and root are retained by the
  runtime reflection store. Protected client volumes use the same journal and
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
- Choice and lazy demand are deterministic. A blocked branch waits on at most
  one lazy value, though it may retain several prior state observations. Any
  racing choice must be introduced as a distinct explicit effect.
- Each reflection `fix` alternative gets a task-owned cell. Recursive producer
  observation fails; other tasks receive its wait token. A failed chosen result
  restarts the fixpoint boundary and its transactional alternatives.

## Child Tasks and Evaluation

- `.task.new` reserves an opaque handle and a private transactional status
  query, but journals launch inside a transaction. Losing branches discard
  both. The query stores atoms `'launched` or `'blocked`, terminal tagged values
  `ok:Value` or `err:Error`, or the atom `'canceled` or `'abandoned`; the handle
  keeps the terminal observation alive. Abandonment is owner-session loss, not
  a failed task, and therefore creates no failure-ledger entry.
- `.task.join` waits directly on every nonterminal child state and propagates
  terminal errors. A joined dependency becoming terminal reruns the join
  operation; it does not select another `.alt` branch. An error with prior
  state observations remains blocked until those observations can retry its
  checkpoint. Propagation prepends
  `{task:{operation:'join, id:TaskId}}`. `.task.status` returns the stored
  status value unchanged.
  `.task.value` and `.task.error` project its matching terminal payload, fail
  transactionally while it is nonterminal, and fail permanently for the other
  terminal outcome; an abandoned task has neither payload. Observation through
  those operations never adds the join frame.
- `.task.cancel` is an unconditional best-effort, commit-ordered request. Any
  session in the same runtime may invoke it; a foreign-runtime handle is
  rejected at the value boundary. Late cancellation is a harmless no-op and
  losing branches discard the request.
- `.task.ack_error` is a timing-independent, commit-ordered modifier for a
  same-runtime handle. It suppresses the task's present or future failure from
  reasoning reports but never mutates `.task.status`, `.task.error`, or
  `.task.join`. Same-transaction acknowledgement is installed before launch;
  losing branches discard it. Repeated acknowledgement and acknowledgement of
  success or cancellation are harmless.
- `.eval` demands WHNF and returns `ok:WHNF` or `err:Diagnostic`. A raw
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
  `ReasoningFailure` is an opaque capability bound to its originating
  evaluation runtime:
  `Assembler::acknowledge_reasoning_failure` is idempotent, accepts any
  assembler view of that runtime, rejects other runtimes, and removes only the
  producer-owner reporting-ledger entry. It does not alter the task's terminal
  result.

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
- The CLI logger's assembler-bus input and its session-local diagnostic bus are
  separate. Logger output cannot feed its own input stream, and logger children
  inherit `read_log`, `write_stderr`, and coordinated exit operations.
- Logger input claims and heap edits share one atomic runtime commit without
  sharing conflict addresses. See
  [`diagnostics.md`](diagnostics.md) for ingress, output, and rendering
  invariants and [`assembly.md`](assembly.md) for settlement and exit policy.
