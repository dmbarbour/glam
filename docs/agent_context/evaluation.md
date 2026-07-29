# Evaluation Invariants

This note collects regression-sensitive rules for values, lazy computation,
evaluation sessions, and background workers. See
[`../architecture/evaluation.md`](../architecture/evaluation.md) for the
control-flow overview.

## Values and Forcing

- Production evaluation starts from closed `Value`s. The small fixture IR in
  `src/eval/test_support.rs` must lower to nets before evaluation; do not add a
  second expression interpreter or local environment.
- Entry points receive an explicit `EvalContext`. A lazy value is evaluated in
  the observing context, not the context that constructed it.
- `Value::Net` is an explicit, opaque, first-class closed net already in WHNF.
  Ordinary application does not accept it. Only `Data(Value::Net) >< Bind`
  opens it by installing a logical-copy cursor. A net-backed `Value::Lazy` is
  instead an explicit zero-arity computation and must expose `Data` when
  forced; an exposed `Bind` is an error.
- `eval_value` is the single outer-WHNF demand operation. It follows every
  top-level lazy or promised result while leaving lazy dictionary fields and
  list elements untouched. `EvaluatedValue` records only that non-deferred
  structural boundary; it does not authorize inspecting an opaque net.
- A failed demand returns `core::EvaluationHalt`: either a permanent
  `Arc<EvaluationFailure>`, a scheduler wait, or an unassigned promise.
  Pollable computations may propagate all three cases. Only the permanent
  variant may enter a lazy cache, promise failure, terminal wait cell, or
  failure ledger. Diagnostic projection belongs to `eval`, not the core halt
  representation.
- Computed lazy work is owned by demand-driven `EvaluationSession` task
  records. Contending observers receive the task's stable wait token; they do
  not wait on a lazy-specific condition variable. A pump distinguishes a
  producer claimed by another thread (`Busy`) from stable quiescence
  (`NoProgress`). Cooperative and scheduled contexts return the wait, while
  synchronous assembler contexts wait on the session condition variable and
  retry. Lazy tasks participate in exact dependency pumping but never enter
  the background-ready queue.
- A blocked lazy or assigned-promise task records an edge only when its wait is
  produced by another deferred-value task. The resulting functional graph is
  checked on every edge change. Cycles containing only computed lazies are
  rotated to the lowest `LazyId`, poisoned with one shared structured failure,
  and cleared. Any cycle involving a promise remains retryable, quiescent
  scheduler state and may only become a session-level deadlock; poisoning a
  lazy from such a temporary dependency would be unsound. Deferred labels and
  IDs belong in internal cycle diagnostics, never in the public value facade.
- Lazy production always transfers the current WHNF demand through a
  top-level lazy or promised result. Reflection gates, `seq`, and `spark`
  therefore perform their prerequisite work and continue the same demand;
  only lazy children inside a completed constructor remain undemanded.
- A raw `Value::Net` is a valid non-lazy cached result. Reaching it does not
  inspect its exposed interface. `LazySource::NetComputation` is the internal
  arity-zero bridge, while `FunctionValue` staging supplies the positive-arity
  bridge. `import 'std` exposes the provisional `net_arity` builtin for both
  forms, alongside `seq` and `spark`. A permanent failure while the zero-arity
  bridge demands data gains `eval:{op:'net_computation}`; raw net observation
  does not.
- Lazy identities are process-global nonzero IDs because a value and its result
  cell may cross evaluation sessions; each session uses them only as local
  scheduling keys.
- A computed `LazyValue` caches
  `Result<EvaluatedValue, Arc<EvaluationFailure>>`.
  Successful cache installation therefore rejects deferred outer shells at the
  type boundary, while a forwarded failure keeps one structured `Arc` through
  cycle members and upstream dependents. Raw `PromisedValue` assignments are a
  separate representation and may still contain deferred values.
- `LazyValue` clones share one cell. A terminal success or permanent failure is
  published before that cell releases its `LazySource`; active workers retain
  only their source snapshots and may finish harmlessly against the canonical
  cache. Blocking and retryable promise conditions retain the source.
  Terminal deferred-task records and all their indexes are removed while the
  session registry is locked, then their machines and captured state are
  dropped after unlocking. A raced source snapshot may register one redundant
  producer; it must observe the canonical cache and retire harmlessly. Every
  scheduler wait token shares a lock-free terminal cell. Terminal state is
  published while the session registry is locked, and polling checks it around
  registry lookup; a terminal result therefore outlives the weakly held owner
  session while a pending wait does not keep that session alive. Terminal
  reflection records and their task-ID indexes are retired immediately.
  Unacknowledged reflection failures remain only in the persistent reporting
  ledger; `.task.ack_error` removes that entry without changing the handle's
  terminal observation. Task-owned promise waits follow the same ownership
  boundary: terminal assignment is copied into the shared wait cell before the
  promise record and owner index are retired.
- Treat any terminal state found in an active reflection or deferred registry
  as an internal scheduler bug. `poll_wait` obtains terminal outcomes only from
  the shared wait cell; after checking that cell under the registry mutex, any
  registered producer is pending. Promise polling may discover a completed or
  abandoned assignment during the publication race, but must publish it into
  the same cell and retire both promise indexes before returning it.
- `Value::Function` is an independently observable curried stage. Partial
  application shares its staged runtime; saturation returns memoized work.
- Lazy lists contain opaque `ListThunk` holes for either computed lazies or
  named promises, but list code never evaluates them. Evaluator-owned
  operations force only the required pieces. Keep compact byte leaves compact.

## Promises and Fixpoints

- Ordinary `fix` and object-self knots use immutable computed-fixpoint sources
  beneath ordinary `LazyValue`s. The session lazy task is their only producer
  owner and wait source. Strict recursive demand becomes an ordinary lazy
  dependency cycle; guarded self-reference beneath a completed constructor
  reaches WHNF. Same-session observers share the lazy task, while another
  session may duplicate pure work against the shared result cell.
- Task-owned reflection fixpoint promises retain their separate rule: direct
  observation by their owning reflection task is an error, while other tasks
  wait for the owner's assignment. Assignment or explicit failure retires the
  active promise wait immediately; owner termination fails and retires every
  unresolved wait. Late observers use the wait cell rather than the registry.
- Suspended fixpoint production is ordinary scheduler state, not a Rust stack
  guard; evaluation unwinds the stack before scheduling resumes it.
- `PromisedValue` is a distinct raw one-write assignment cell, not a
  `LazySource` and not a computed-lazy result cache. Its payload may itself be
  lazy or promised. Direct empty observation fails fast without filling the
  cell. An enclosing computed-lazy task translates that typed condition into a
  demand-driven promise wait and leaves its own cache empty; explicit demand
  after assignment retries it. Anonymous promises have no producer to
  prioritize and do not keep a session alive independently.
- Assigned promises participate in the common deferred dependency graph.
  Promise-only and mixed promise/lazy cycles remain blocked without poisoning
  promise assignments or lazy result cells. Stable session quiescence may
  diagnose them as deadlocks, while retry or producer progress may first
  remove their temporary dependency edges.
- The public `Assembler::promise` pair gives clients one affine Rust
  `PromiseResolver`. Resolving, failing, or dropping it wakes only the creating
  assembler's evaluation session. A client that shares the consumer value with
  another session is responsible for pumping that session.
- `PromiseResolver::fail` accepts a complete diagnostic-style Glam value.
  Projection preserves its ad hoc fields and existing `msg.context`, prepending
  later evaluator-owned demand frames rather than replacing client context.
- Reflection annotations are lazy gates. Construction demands neither effect
  nor target. Demand on a gate waits for its session-owned task, requires
  canonical unit, and then transfers the same demand to the target. Waits are
  not cached as lazy failures.
- A gate's first observer owns its task. Another session may poll but must not
  drive that task: pending work becomes a foreign dependency in the local
  lazy-task record, while a terminal result transfers demand to the target.
  Wait tokens retain the owner weakly plus stable session and producer IDs, so
  a dropped owner is a terminal failure and live foreign work remains visible
  in quiescence reports without becoming a cached `LazyFailure`.
- Opaque reflection task values retain `EvaluationTaskHandle`, not a bare task
  ID. Public join and cancellation validate the handle's session provenance;
  internal foreign followers deliberately operate on wait tokens instead.
- A transaction folds modifiers for its newly reserved tasks before launch.
  In particular, same-transaction cancellation must publish `'canceled`
  without constructing a task machine or exposing runnable work to the shared
  executor, while same-transaction error acknowledgement must be installed
  before an immediate failure can be reported. Older-task modifiers remain
  ordinary committed updates.
- Never invoke task status sinks, task cancellation hooks, or machine
  destructors while holding the scheduler registry mutex. A terminal
  reflection transition detaches its record under the mutex, then releases or
  cancels the machine after unlocking.

## Sessions and Workers

- `Assembler` clones share an `EvaluationSession`. Replacing an assembler's
  host, sink, environment, or executor creates a session consistent with the
  new configuration.
- Demand-driven deferred tasks are stored per session and keyed by stable lazy
  or promise IDs; do not enlarge every value with mutable scheduler state.
- One `EvaluationExecutor` is shared by related assembler, logger, and future
  IDE sessions. Workers opportunistically poll reflection tasks and are the
  only consumers of sparks.
- Zero workers discard sparks without queueing. Sparks are nontransactional
  hints: rollback does not retract them, their errors are not independently
  reported, and queued work does not keep a session alive.
- A divergent spark may occupy a worker indefinitely. Cooperative cancellation,
  evaluator fuel, and fine-grained wake indexes remain deliberate future work.
- Claimed interaction-net pairs are live work, not quiescence. An observer must
  wait for that runtime's generation to change before deciding the net is
  blocked or complete.
- A stable pass containing a live foreign task dependency is quiescent, not a
  proven deadlock. The client may poll the reported session/task later. The
  bootstrap does not spin or pump the foreign session, and cross-session cycle
  diagnosis remains future work.
