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
- `force_value_shell` currently eliminates an outer `Value::Lazy` chain while
  leaving lazy dictionary fields and list elements untouched. Its
  `EvaluatedValue` wrapper records only that non-lazy structural boundary; it
  does not authorize inspecting an opaque net.
- Computed lazy work is owned by demand-driven `EvaluationSession` task
  records. Contending observers receive the task's stable wait token; they do
  not wait on a lazy-specific condition variable. Lazy tasks participate in
  exact dependency pumping but never enter the background-ready queue.
- A blocked lazy task records an edge only when its wait is produced by another
  lazy task. The resulting functional graph is checked on every edge change.
  Pure cycles are rotated to the lowest `LazyId`, poisoned with one shared
  structured failure, and cleared; mixed lazy/reflection waits remain ordinary
  quiescent scheduler state. Lazy labels and IDs belong in internal cycle
  diagnostics, never in the public value facade.
- Current `eval_value`, `PostForcePolicy`, and terminal shallow aliases are
  transition machinery, not language semantics. A WHNF demand ultimately
  follows a top-level lazy result; only lazy children inside a completed
  constructor remain undemanded. Keep the compatibility path until the
  lazy-cycle plan is complete, then remove it rather than extending it.
- A raw `Value::Net` is a valid non-lazy cached result. Reaching it does not
  inspect its exposed interface. `LazySource::NetComputation` is the internal
  arity-zero bridge, while `FunctionValue` staging supplies the positive-arity
  bridge. The provisional source spelling for both is `net_arity`.
- The current `eval_value(Value::Net)` path contradicts this boundary by
  calling `observe_net`; it is a known bootstrap mismatch scheduled for the
  post-lazy-cycle cleanup. New code must not rely on raw-net projection.
- Lazy identities are process-global nonzero IDs because a value and its result
  cell may cross evaluation sessions; each session uses them only as local
  scheduling keys.
- `Value::Function` is an independently observable curried stage. Partial
  application shares its staged runtime; saturation returns memoized work.
- Lazy lists contain opaque holes, but list code never evaluates them.
  Evaluator-owned operations force only the required pieces. Keep compact byte
  leaves compact.

## Promises and Fixpoints

- Ordinary `fix` and object-self knots use computed fixpoint cells. The first
  observing lazy task claims production. Strict recursive demand becomes an
  ordinary lazy dependency cycle; guarded self-reference beneath a completed
  constructor reaches WHNF. Other tasks wait if the producer is suspended.
- Task-owned reflection fixpoint promises retain their separate rule: direct
  observation by their owning reflection task is an error, while other tasks
  wait for the owner's assignment.
- Do not replace fixpoint ownership with a Rust stack guard: suspended
  evaluation unwinds the stack before scheduling resumes it.
- Anonymous `Promised` cells used by module assembly and deferred-list effects
  fail when observed before assignment. Do not silently reinterpret them as
  blocking joins; migrate each producer explicitly when its scheduling
  semantics are defined.
- Reflection annotations are lazy gates. Construction demands neither effect
  nor target. Demand on a gate waits for its session-owned task, requires
  canonical unit, and then transfers the same demand to the target. Waits are
  not cached as lazy failures.
- A gate's first observer owns its task. Another session may consume its final
  result but must not drive it. Wait tokens retain the owner weakly.

## Sessions and Workers

- `Assembler` clones share an `EvaluationSession`. Replacing an assembler's
  host, sink, environment, or executor creates a session consistent with the
  new configuration.
- Demand-driven lazy tasks are stored per session and keyed by stable lazy IDs;
  do not enlarge every value with mutable scheduler state.
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
