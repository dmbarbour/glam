# Evaluation Invariants

This note collects regression-sensitive rules for values, lazy computation,
evaluation sessions, and background workers. See
[`../architecture/evaluation.md`](../architecture/evaluation.md) for the
control-flow overview.

## Values and Forcing

- Constructed protocol values and compiler-layer bundles belong to one
  runtime's `CoreValueFactory`/`RuntimeValueCache`. Keep immutable global
  `Key` descriptions if useful, but do not add a production static which
  retains a constructed `Value`. Optional compiler caches publish one complete
  type-indexed bundle rather than exposing partially initialized entries.
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
  representation. Diagnostic normalization and object/viewer mixins also
  retain this halt until the public `Error` boundary; they must not stringify
  a failure merely because they operate on diagnostics.
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
- Lazy and promise identities are runtime-local nonzero IDs. Values may cross
  evaluation sessions belonging to the same `EvaluationRuntime`, so escaped
  identities must always be interpreted with that runtime rather than as
  process-global keys. `EvaluationRuntimeId` is the sole process-global ID
  allocator.
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
  Reflection and deferred machines reside in their coordinator work records
  while claimable. Claiming takes a machine under the coordinator lock;
  release restores it before a nonterminal record becomes claimable or returns
  it for terminal destruction. Abandonment and pure-cycle terminalization
  likewise take machines during the authoritative coordinator transition.
  Machine cancellation/destruction and captured-value release happen only
  after coordinator/component locks and mutation admission are released. The
  session reporting store retains only transitional task/wait lookup and
  retirement indexes, never executable machines, failure policy, or protected
  status publication. Current task status and the optional protected-query
  publisher reside in the coordinator work record. That publisher retains a
  narrow runtime query writer, not its role host or owner lease. A raced source
  snapshot may register one redundant producer; it must observe the canonical
  cache and retire harmlessly. Every
  scheduler wait token shares a lock-free terminal cell plus a weak exact-work
  subscription set. Terminal state is published and producer indexes retire
  through the owning coordinator/store transition; exact subscribers detach
  and notify the coordinator only after unlocking. Polling checks the cell
  around registry lookup, so a terminal result outlives the weakly held demand
  state while a pending wait does not keep its external owner alive. Terminal
  reflection records and their task-ID indexes are retired immediately. A
  wait-blocked spark subscribes its stable work ID and epoch directly to this
  source; unrelated task progress must not wake it.
  Unacknowledged reflection failures remain only in the coordinator's
  persistent runtime ledger, partitioned by producer-owner session;
  `.task.ack_error` updates the active coordinator policy or removes that
  owner's terminal entry without changing the handle's observation. Terminal
  wait publication occurs only after that ledger decision under the same
  runtime mutation admission. Task-owned promise waits follow the same
  ownership boundary: terminal assignment is copied into the shared wait cell
  before the promise record and owner index are retired.
- Final demand-owner drop is one guarded coordinator transition. It records
  the first close reason on running work and takes queued or blocked work for
  terminal settlement. Settle reflection/deferred producer obligations before
  abandoning dependencies detached from parked sparks; reversing that order
  can attempt to release one reusable deferred claim twice.
- Machine and spark contexts may retain `Arc<EvaluationDemandState>`, but never
  the external `EvaluationSession` lease. The closed flag is a fast rejection;
  coordinator reservation must repeat the authoritative open-session check
  under its state transition so closure and admission have a defined winner.
- A weak wait owner disappearing is `Abandoned`, never an inferred evaluation
  failure. Interpret abandonment according to the producer obligation:
  reflection task handles retain a terminal abandoned status; reusable lazy
  claims and host-promise followers may be replaced without poisoning their
  value; unresolved task-owned promises fail because their sole responsible
  producer is gone. Explicit cancellation wins only when it was committed
  before closure.
- Treat any terminal state found in an active reflection or deferred registry
  as an internal scheduler bug. `poll_wait` obtains terminal outcomes only from
  the shared wait cell; after checking that cell under the registry mutex, any
  registered producer is pending. Promise polling may discover a completed or
  abandoned assignment during the publication race, but must publish it into
  the same cell and retire both promise indexes before returning it.
- `Value::Function` is an independently observable curried stage. Partial
  application shares its staged runtime; saturation returns memoized work.
- `Value::Metadata` is a sealed carrier, not an observable unit value or an
  opaque host payload. Ordinary equality, key conversion, unit assertion,
  pattern matching, application, kind reporting, and debug formatting must
  not expose either its implicit unit payload or hidden Glam metadata.
  `anno 'meta_init ()` returns the cached initial carrier with `{}` metadata.
- `anno meta_pure:UpdateFn Carriers` builds one shared lazy update and one lazy
  `list.at` projection per input slot. It preserves only outer arity: do not
  eagerly require the update result to be a list or have a matching length.
  Each derived carrier holds an immutable hidden `Value`; there is no mutable
  metadata cell and no evaluator-defined merge.
- `anno meta_refl:EffectfulUpdate Carriers` uses the same validation,
  arity-preserving projections, and sealed outputs, but its one shared update
  is a result-producing reflection task. Annotation construction and ordinary
  transport never launch it. The first demand owns the task; copied carriers
  and projections share its result, waits, failure, and cancellation.
- Associated metadata has bidirectional hidden transport but one-way
  observability. Ordinary transport leaves hidden work latent; `seq` demands
  it and `spark` may demand it on a worker. Only reflection `.meta.inspect`
  and the reflection-aware Rust facade may retrieve it. A mismatch is effect
  failure, not a permanent evaluation error or transactional observation.
  Committed reflection effects are not undone if semantic code later discards
  a demanded carrier.
- Metadata records logical history carried by surviving values. Never present
  it as evidence of worker order, evaluator demand order, or discarded
  alternatives. A handler trace should retain its carrier in protected state
  and inspect only the carrier selected at the final reflection boundary.
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
  `PromiseResolver`. Its `PromisedValue` is a thin `Arc<PromiseCell>` whose
  terminal assignment is published once under shared runtime mutation
  admission. Resolving, failing, or dropping the resolver wakes every live
  same-runtime session whose task work actually observed the unresolved
  promise; those follower targets are weak and deduplicated, so sharing a
  promise retains no session. Parked spark work instead subscribes by its
  stable work ID and subscription epoch. A rejected foreign-runtime resolution
  consumes the resolver but leaves the promise unassigned and sends no wake.
- `PromiseResolver::fail` accepts a complete diagnostic-style Glam value.
  Projection preserves its ad hoc fields and existing `msg.context`, prepending
  later evaluator-owned demand frames rather than replacing client context.
- Reflection annotations are lazy gates. Construction demands neither effect
  nor target. Demand on a gate waits for its session-owned task, requires
  canonical unit, and then transfers the same demand to the target. Waits are
  not cached as lazy failures.
- `refl` and `meta_refl` select the runtime's once-sealed default task profile,
  never the profile of the session which claims the lazy annotation. A
  `.task.new` child instead inherits its parent's whole profile, including
  effect vocabulary, environment, diagnostic routing, and shared host
  resources. An annotation child therefore inherits the runtime default which
  its parent received.
- The boxed reflection lazy source has an explicit completion policy. A gate
  selects `RequireUnit` and then exposes its target; the internal
  result-producing form selects `ReturnValue` and forwards the task result
  through the ordinary WHNF demand. Keep this distinction at the launcher
  boundary rather than inferring policy from whether a task happens to be
  public or joinable. `meta_refl` is the evaluator production use of the
  result-producing form; its result remains hidden behind metadata carriers.
- When a demand-owned reflection task failure is propagated into its lazy
  consumer, the task handle acknowledges the owner's reporting ledger,
  including when demand transfers through an observer in another session of
  the same runtime. If nobody observes the failure, it remains unacknowledged
  and is reported during reasoning drain.
- A gate's first observer owns its task. Another session may poll but must not
  drive that task: pending work becomes a cross-session dependency in the
  local lazy-task record, while a terminal result transfers demand to the
  target. Both sessions are in the same runtime; another runtime rejects the
  value before demand. Wait tokens retain the owner weakly plus stable session
  and producer IDs, so a dropped owner is a terminal failure and live
  cross-session work remains visible in quiescence reports without becoming a
  cached `LazyFailure`.
- Opaque reflection task values retain `EvaluationTaskHandle`, not a bare task
  ID. Public join and cancellation validate the handle's session provenance;
  internal cross-session followers deliberately operate on wait tokens
  instead.
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
- One runtime-owned `EvaluationWorkCoordinator` registers related assembler,
  logger, and future IDE sessions and owns ready-session selection, fairness,
  its work generation, worker waiting, and stable spark records.
  `EvaluationExecutor` owns worker activation and shutdown; workers retain only
  a weak coordinator attachment. Active reflection/deferred records and their
  claimable machines are coordinator-owned; session state retains reporting
  policy only.
- Every published spark blockage advances a checked, non-wrapping subscription
  epoch. Parked indexes and one-shot subscriber cells retain only the work ID
  and that epoch. A coordinator wake must still match `Blocked`, the epoch, and
  the retained runtime-local dependency key before queueing; a stale wake does
  not advance work generation. Subscriber and coordinator mutexes are never
  nested, and notification occurs after shared runtime mutation admission is
  released. Promise cells own that exact subscriber component, and unresolved
  spark demand registers with it directly. Spark contexts do not also install
  a weak session wake, so one dependency cannot broadly wake work parked on
  another. Wait cells and promise cells both publish through the exact
  subscriber path.
- Coordinator transitions take shared runtime mutation admission and publish
  their own work generation before waking workers. They must not advance the
  semantic `RuntimeObservationEpoch`; otherwise ordinary scheduler churn can
  spuriously invalidate the state observations of the task being scheduled.
- Workers opportunistically poll reflection tasks and are the only consumers
  of sparks. A serial pump continues to select task records directly within
  its chosen session by coordinator demand ID. It must not upgrade the external
  owner lease: worker and task contexts may need to finish exact dependencies
  after the client has released that lease.
- Zero workers discard sparks without queueing. Sparks are nontransactional
  hints: rollback does not retract them, their errors are not independently
  reported, and queued work does not keep a session alive.
- A spark is best-effort background `seq` demand, not just execution of work
  directly owned by its outer value. Admit lazy values, promises, and sealed
  metadata, but not already-WHNF nets. Retryably blocked sparks park without
  occupying workers. Promise and wait-token dependencies use exact source
  subscriptions; subscribe-and-recheck closes the completion-before-parking
  race without a session-wide retry. A wait-blocked spark promotes its deferred
  producer directly, without borrowing the session's serial pump. Session
  teardown discards parked sparks.
- A divergent spark may occupy a worker indefinitely. Cooperative cancellation,
  evaluator fuel, and fine-grained wake indexes remain deliberate future work.
- Claimed interaction-net pairs are live work, not quiescence. An observer must
  wait for that runtime's generation to change before deciding the net is
  blocked or complete.
- A stable pass containing a live cross-session task dependency is quiescent,
  not a proven deadlock. The client may poll the reported session/task later.
  The bootstrap does not spin or pump the producer's session, and
  cross-session cycle diagnosis remains future work.
