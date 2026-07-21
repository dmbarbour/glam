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
- `Value::Net` is an explicit first-class closed net. A net-backed
  `Value::Lazy` is a computation and must expose `Data` when forced; an exposed
  `Bind` is an error, not an implicit function conversion.
- `force_value_shell` defines outer weak-head normal form: its result cannot be
  `Value::Lazy`, but dictionaries and lists may retain lazy children. The
  `EvaluatedValue` wrapper records this boundary. Lazy result cells still
  temporarily accept forwarding values until session-owned lazy tasks replace
  that compatibility path.
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
  observing task claims production. Recursive observation by that active
  producer is an error; other tasks wait if the producer is suspended.
- Do not replace fixpoint ownership with a Rust stack guard: suspended
  evaluation unwinds the stack before scheduling resumes it.
- Anonymous `Promised` cells used by module assembly and deferred-list effects
  fail when observed before assignment. Do not silently reinterpret them as
  blocking joins; migrate each producer explicitly when its scheduling
  semantics are defined.
- Reflection annotations are lazy gates. Their queued/running/blocked state is
  session-owned, waits are not cached as lazy failures, and success returns the
  original target without forcing it. The effect must return canonical unit.
- A gate's first observer owns its task. Another session may consume its final
  result but must not drive it. Wait tokens retain the owner weakly.

## Sessions and Workers

- `Assembler` clones share an `EvaluationSession`. Replacing an assembler's
  host, sink, environment, or executor creates a session consistent with the
  new configuration.
- Lazy single-flight claims are stored per session and keyed by stable lazy IDs;
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
