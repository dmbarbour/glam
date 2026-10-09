# Evaluation Invariants

This note collects regression-sensitive rules for values, lazy computation,
evaluation sessions, and background workers. See
[`../architecture/evaluation.md`](../architecture/evaluation.md) for the
control-flow overview.

## Values and Forcing

- Constructed protocol values and compiler-layer bundles belong to one
  runtime's `CoreValueFactory`/`RuntimeValueDomain`. The domain contains the
  value cache and no-auto collector heap. Factories, active evaluation or
  compiler contexts, and reviewed runtime services may retain it; public
  values, managed payloads, and cache entries must not. Its coordinator route
  stays weak so a value-domain lease cannot retain runtime execution. Keep immutable global
  `Key` descriptions if useful, but do not add a production static which
  retains a constructed `Value`. Optional compiler caches publish one complete
  type-indexed bundle rather than exposing partially initialized entries.
- Treat `Gc<T>` as a non-rooting interior edge. Code may inspect, duplicate,
  compare, or install one only under matching `RuntimeValueAccess` (or its
  evaluator-qualified view). Anything surviving that access must already be
  beneath an exact traced owner or published as a registered root. Do not put a
  root inside the managed graph to solve a liveness problem; that hides the
  cycles collection is meant to reclaim.
- Managed access is callback-free. Close it before acquiring coordinator or
  host lifecycle locks, waiting, invoking user/loader/logger callbacks,
  delivering events, or sleeping a worker. The retained allocation scope in
  `RuntimeValueAccess` is intentional mutator authority, not redundant state.
  Known exception, not a pattern to copy:
  `PromiseProducerObligation::publish_assignment_guarded` runs inside a
  promise's managed edge transition, under shared mutation admission, and
  locks the coordinator there to retire the producer and publish the wait
  terminal. There is no lock-order table yet; moving that coordinator work
  after access closes is a concurrent-GC prerequisite.
- A thread holds one runtime's managed access at a time: `glam-gc` panics
  on entering a second heap while the thread holds a mutator for another
  (`gc-one-heap-per-thread`). Close one runtime's access before entering
  another's. The rule keeps a collection waiting on one heap from depending
  on a thread blocked entering a different heap.
- Shared runtime mutation admission may be taken inside a managed-access
  region; promise publication does. This cannot deadlock because settlement
  never collects, and a pending collection blocks no mutator entry: the
  collector takes heap exclusivity only after every mutator has left, and
  holds no runtime gate while it collects. The converse is forbidden. No
  allocation or value-domain entry may take the gate, a non-reentrant
  `RwLock` that settlement holds exclusively while it constructs values.
- Glam heaps use immutable `CollectionPolicy::NoAuto`. Pressure is acted on
  in two places only:
  - after a claimed task, spark or client demand is polled and before it is
    released, through `service_collection_pressure`;
  - a stable runtime pump promotes it to `MaintenanceRequired` for explicit
    service.

  Never collect on ordinary mutator entry. A new kind of claimed work gets the
  same call between its poll and its release. Test fixtures that hold raw
  values across evaluation must root them; the shared test domains
  (`shared_test_value_factory`) are never collected.
- Collection is not a Glam semantic mutation. It never advances observation
  epochs or changes values, transactions, diagnostics, or net topology, and
  pure Glam cannot observe policy, pressure, revisions, collection counts, or
  reports. Reflection-task scheduling and the arrival order of independent
  diagnostics stay outside pure reproducibility either way.
- Managed `Drop` is passive: it may release Rust shells but cannot observe or
  preserve dying managed edges, enter the heap/runtime, invoke callbacks, or
  perform active retirement. Put active cleanup in the external-owner registry
  with explicit registered roots instead.
- Compatibility shells (lists, dictionaries, builtin arguments, metadata, lazy
  sources, failures) stay immutable, and acyclic once lazies, promises, and
  nets are removed. The compatibility walk recurses through shells and stops
  only at those three identities, so never add interior mutability or
  recursion to a shell.
- A new managed family needs a private allocator; no constructor that opens
  its own region and returns a fresh `Gc`, facade, or raw value; publication
  before access ends, with a root only at a real handoff and never to bridge
  adjacent statements; a forced collection across the former
  allocation/publication gap; an exact trace; a layout latch; passive drop
  with a `ManagedDropRecord`; and survival and reclamation tests. See
  [`values.md`](../architecture/values.md) "Managed Families".
- No user-controlled semantic recursion on the Rust stack. Permitted
  recursion is bounded representation plumbing, balanced persistent-container
  traversal with a logarithmic depth bound, or an owned worklist such as
  cursor WHNF. Deep-structure tests use explicit depths on a small stack, and
  the recursive control must fail by reporting a stack overflow, not by any
  abnormal exit. The `.g` parser is not yet covered
  (`no-semantic-recursion-on-rust-stack` in [`Decisions.md`](../Decisions.md)).
- Ordinary non-suspending WHNF delegation changes evaluator control only. It
  creates no lazy, promise, wait, task, cache entry, or root, and takes no lock
  or scheduler admission. A retained computation registers one checkpoint root
  at its first retained transition and none on later polls; root registration
  that scales with semantic depth is a regression.
- `map` and `list.concat` are structural and non-forcing: each step unfolds
  one representation node. `map f (A ++ B)` is
  `defer (map f A) ++ defer (map f B)`, a thunk gains one deferred map, and a
  strict leaf becomes lazy item applications without new chunk boundaries.
  The callable is not evaluated until an item is demanded. `list.concat`
  likewise defers both halves of a source concatenation; for a strict outer
  leaf it joins the visible segments pairwise to O(log n) depth without
  inspecting them, and an invalid item becomes a deferred failing hole.
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
- Runtime-owned client demand is the synchronous outer-WHNF entry. It follows
  every top-level lazy or promised result while leaving lazy dictionary fields
  and list elements untouched. Each durable caller owns a `WhnfComputation`;
  bounded polls resume its managed state rather than replaying the original
  demand. `EvaluatedValue` records only that non-deferred structural boundary;
  it does not authorize inspecting an opaque net.
- A `WhnfComputation` begins with one rooted seed and atomically replaces it
  with one `ManagedWhnfRoot` during bounded access. Never discard the old owner
  before the new managed owner is installed. Lazy producer checkpoints live
  beneath the lazy itself; client, promise, reflection, and spark computations
  remain owned by their durable machines.
- Preserve one canonical continuation state across `Pending` and `Yielded`.
  Do not reinitialize focus, continuation frames, followed identities, or
  source-owner/cycle state merely because an outer machine was rescheduled.
  The retired direct evaluator gate and whole-value wrappers must not return;
  regional tests may use only the bounded normal-poll test step.
- A failed demand returns `core::EvaluationHalt`: either a permanent
  `Arc<EvaluationFailure>`, a scheduler wait, or an unassigned promise.
  Pollable computations may propagate all three cases. Only the permanent
  variant may enter a lazy cache, promise failure, terminal wait cell, or
  failure ledger. Diagnostic projection belongs to `eval`, not the core halt
  representation. Diagnostic normalization and object/viewer mixins also
  retain this halt until the public `Error` boundary; they must not stringify
  a failure merely because they operate on diagnostics.
- Computed lazy work is owned by runtime-coordinator deferred-work records
  indexed by demand session. Contending observers receive the task's stable
  wait token; they do
  not wait on a lazy-specific condition variable. A pump distinguishes a
  producer claimed by another thread (`Busy`) from stable quiescence
  (`NoProgress`). Cooperative and scheduled contexts return the wait, while
  synchronous assembler contexts wait on a coordinator generation only while
  exact or launched causal work is owned elsewhere; stable causal absence
  returns a retryable halt without pumping unrelated runtime work. Deferred
  producers begin dormant. Publishing an exact dependency may
  queue the canonical producer for exact/cooperative claim, but does not make
  it an independent background root. Workers follow exact descendants of
  registered reflection or spark roots; the runtime background pump follows
  reflection roots only. Construction alone advertises no work.
- A blocked lazy or assigned-promise task records an edge only when its wait is
  produced by another deferred-value task. The resulting functional graph is
  checked on every edge change. Cycles containing only computed lazies are
  rotated to the lowest `LazyId`, poisoned with one shared structured failure,
  and cleared. Any cycle involving a promise remains retryable, quiescent
  scheduler state and may only become a session-level deadlock; poisoning a
  lazy from such a temporary dependency would be unsound. Deferred labels and
  IDs belong in internal cycle diagnostics, never in the public value facade.
- A foreground pump may help exact producers and activated `launch_parent`
  descendants, including across demand sessions, but never arbitrary
  same-session work. A claimed or reserved causal child is `Busy` until a
  generation wake; an unrelated unresolved target may still be `NoProgress`.
  Retiring an intermediate task must preserve access to its live descendants.
  Launch provenance is a helping route, not an implicit join or ownership
  transfer.
- A demand session groups lifecycle, closure, drain, and reporting state; it
  is not a serial executor. Independent same-session records may run
  concurrently, with no FIFO or one-machine ordering contract. Do not restore
  a session-wide running-machine admission scan: exact claims and causal waits
  are the concurrency authority.
- Lazy production always transfers the current WHNF demand through a
  top-level lazy or promised result. Reflection gates, `seq`, and `spark`
  therefore perform their prerequisite work and continue the same demand;
  only lazy children inside a completed constructor remain undemanded.
- A raw `Value::Net` is a valid non-lazy cached result. Reaching it does not
  inspect its exposed interface. `LazySource::NetComputation` is the internal
  arity-zero bridge, while `FunctionValue` staging supplies the positive-arity
  bridge. `import 'std` exposes `net_arity` for both forms, alongside `seq` and
  `spark`. A permanent failure while the zero-arity
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
  coordinator work record retains task/wait lookup, failure policy, and a
  `TaskTerminalPublisher` containing the wait, current status, and optional
  protected-query publisher. The latter retains a narrow runtime query writer,
  not its role host or owner lease. There is no session-side reporting store.
  A raced source snapshot may register one redundant producer; it must observe
  the canonical cache and retire harmlessly. Every
  scheduler wait token shares a lock-free terminal cell plus a weak exact-work
  subscription set routed through the runtime coordinator. It retains scalar
  runtime, owner-session, and producer identity, but no weak demand-state
  route. Terminal state is published and producer indexes retire
  through the owning coordinator/store transition; exact subscribers detach
  and notify the coordinator only after unlocking. Polling checks the cell
  around registry lookup, so a terminal result outlives its former demand
  state while a pending wait cannot recover or keep its external owner alive.
  An unregistered nonterminal wait is an invariant failure; owner closure must
  publish abandonment before retirement. Terminal
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
- Owner-session closure publishes `Abandoned`; it is never inferred from a
  failed weak-owner upgrade. Interpret abandonment according to the producer
  obligation:
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
  transport never launch it. The first demand reserves the task; copied carriers
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
  beneath ordinary `LazyValue`s. One runtime-coordinator deferred record is
  the canonical producer and wait source for a demanded lazy. Strict recursive
  demand becomes an ordinary lazy dependency cycle; guarded self-reference
  beneath a completed constructor reaches WHNF. Same-runtime observers share
  that producer. If its owning demand session closes, another session may
  reclaim the reusable lazy without poisoning its shared result cell.
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
  `PromiseResolver`. Its `PromisedValue` is one non-owning managed edge; the
  resolver and task/local producers retain registered roots which own terminal
  publication. Assignment is published once under shared runtime mutation
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
  nor target. Demand on a gate waits on its completion promise, which the
  task assigns the target only after its result is canonical unit; the same
  demand then continues to the target. Waits are not cached as lazy failures.
- `refl` and `meta_refl` select the runtime's once-sealed default task profile,
  never the profile of the session which claims the lazy annotation. A
  `.task.new` child instead inherits its parent's whole profile, including
  effect vocabulary, environment, diagnostic routing, and shared host
  resources. An annotation child therefore inherits the runtime default which
  its parent received.
- A reflection lazy traces its immutable effect, optional gate target, and one
  managed completion promise; it has no external-owner record or task
  observation. The first producing poll registers the reserved task as the
  promise's producer, installs a WHNF checkpoint on the promise, and defers a
  one-use permit that roots the effect until activation transfers ownership
  to the coordinator machine. Dropping an unconsumed permit cancels the
  still-reserved task and fails its promise. An activated task is an
  autonomous background root; retiring the lazy's route never cancels it.
- The boxed reflection lazy source has an explicit completion policy. A gate
  selects `RequireUnit`, and its terminal mapper assigns the target; the
  internal result-producing form selects `ReturnValue` and assigns the task
  result. Either reaches the consumer through the ordinary WHNF demand on the
  promise. A panicked task leaves the promise unassigned. Keep this
  distinction at the launcher boundary rather than inferring policy from
  whether a task happens to be public or joinable. `meta_refl` is the
  evaluator production use of the result-producing form; its result remains
  hidden behind metadata carriers.
- When WHNF propagates a failed completion promise, the promise's producer
  obligation acknowledges the task's failure-ledger entry through its scalar
  task/session identity and weak coordinator route, whichever same-runtime
  session's demand propagates it. If nobody propagates the failure, it remains
  unacknowledged in the runtime ledger and appears in settled reports.
- An annotation task belongs to the runtime's background demand domain and
  uses the runtime-default profile, whichever session observes the gate
  first, and it outlives that session. Same-runtime observers follow the
  lazy's exact producer route and may help pump the task without changing
  ownership or profile; terminal success transfers demand to the target.
  Another runtime rejects the value before demand. Wait tokens retain stable
  scalar runtime, owner-session, and producer IDs but no owner lease;
  coordinator registration and exhaustive closure publication make
  unavailable or dropped producers explicit without caching a retryable
  condition as a permanent lazy failure.
- Opaque reflection task values retain `EvaluationTaskHandle`, not a bare task
  ID. `.task.status`, `.task.value`, and `.task.error` may inspect the
  protected query from any session in the same runtime without changing
  ownership or reporting state. Join, cancellation, and acknowledgement
  validate runtime provenance and route to the producer's coordinator records;
  exact internal followers operate on wait tokens.
- A transaction folds modifiers for its newly reserved tasks before launch.
  In particular, same-transaction cancellation must publish `'canceled`
  without constructing a task machine or exposing runnable work to the shared
  executor, while same-transaction error acknowledgement must be installed
  before an immediate failure can be reported. Older-task modifiers remain
  ordinary committed updates.
- Never notify task-status observers, invoke task cancellation hooks, or
  destroy machines while holding a coordinator/component mutex or runtime
  mutation admission. Protected status itself is published under the guard;
  notifications and destruction happen after it is released.

## Sessions and Workers

- `Assembler` clones share one internal reasoning session selected by
  `AssemblerBuilder`. Source, runtime, environment, diagnostic, and executor
  policy is fixed before construction rather than replaced by fluent
  assembler mutation.
- Demand-driven reflection and deferred tasks live in runtime-coordinator work
  records indexed by demand owner and stable value/task identities; do not
  enlarge every value with mutable scheduler state or recreate a session-side
  registry.
- One runtime-owned `EvaluationWorkCoordinator` registers related assembler,
  logger, and future IDE sessions and owns ready-session selection, fairness,
  its work generation, worker waiting, and stable spark records.
  `EvaluationExecutor` owns worker activation and shutdown; workers retain only
  a weak coordinator attachment. Active reflection/deferred records and their
  claimable machines, reporting policy, and failure ledgers are
  coordinator-owned. Machine contexts retain only `EvaluationDemandState`;
  serial pumping and reporting use its weak coordinator route and never
  recover the external owner lease.
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
- The coordinator's condition variable serves several waiter classes
  (workers, exact clients, session drains, task observers), so never use
  `notify_one`: it may wake only a waiter that cannot take the work. A host
  wake may be suppressed only for mutation kinds that cannot enable a parked
  waiter (`notifies_waiters`); the `work_generation` advance is never
  suppressed, and a waiter rechecks it under the coordinator mutex.
- Every condvar is a `crate::counted_condvar::CountedCondvar`, so a
  notification with no waiter costs nothing. That relies on the notifier
  changing the awaited state under the waiter's mutex before notifying.
- Maps keyed by runtime-allocated ids use `crate::trusted_hash`. A key type
  must implement `TrustedKey` in `trusted_hash.rs`, with the reason a
  program cannot choose it. Keys a program can influence keep
  `RandomState`.
- Workers opportunistically poll reflection tasks and are the only consumers
  of sparks. Workers and the runtime background pump follow exact producer
  chains from permitted roots, not globally ready deferred work. An explicit
  serial demand driver still selects its chosen session by coordinator demand
  ID. It must not upgrade the external owner lease: worker and task contexts
  may need to finish exact dependencies
  after the client has released that lease.
- Keep evaluator quantum vocabulary exact. `EvaluationStepBudget::spent()` is
  the transition count consumed inside one claimed poll. The foreground exact
  demand driver's scalar is instead a conservative reservation allowance: it
  reserves a whole quantum and does not refund unused units. Its
  `BudgetExhausted` result must never be presented as an exact-spend report.
  The runtime background pump does translate exact inner spend into its public
  report. Nested WHNF and list-front machines borrow the same mutable budget;
  they never recreate an allowance from the remaining count. An outer
  administrative phase charges one transition only when its child spent none.
- Preserve inactive per-heap allocation cursors across ordinary worker
  quantums, but explicitly release all such thread-local cache records when a
  worker terminates. This exit boundary must run only after scoped mutators
  unwind; the collector release operation intentionally panics if any mutator
  depth remains active. A future concurrent collector reinterprets this as
  participant retirement without turning a poll context into participation.
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
- A blocked record whose dependency is terminal but which has not yet been
  woken is `Busy`, never `NoProgress`: the publishing thread owns its wake.
  Exact walks classify that edge through `dependency_edge_locked`. A client
  that sees `NoProgress` while progress is latent retries without waiting,
  and each retry walks the chain under the coordinator lock the publisher
  needs (`perf-worker-scaling`).
- Runtime readiness covers all demand sessions in the runtime. Runnable or
  claimed producer work is `Busy`; a stable set in which every unfinished
  record is blocked is a typed deadlock snapshot. Pure deferred-value cycles
  are diagnosed earlier by the strict dependency graph, while cycles through
  promises or reflection tasks remain ordinary deadlock candidates.

## Verification Discipline

- `test_value_factory()`, `compiler_test_runtime()` and the other shared test
  domains are per thread: libtest runs each test on its own thread, so a
  test's fixtures share one runtime and tests never contend on one heap. A
  test that spawns threads hands them its factory rather than calling these
  again there. A shared domain is never an explicit collection target: its
  fixtures can hold unrooted compatibility values. Any test that forces GC
  must use `private_test_value_factory()` or another private runtime. A
  test-only assertion at the collection gateway latches this rule.
- Representative sample outputs must be identical in every collection mode.
  Collection counts and timing are profiling data, not semantics.
- Test-only exact roots are real fixture ownership. A fixture that observes a
  lazy or promise after cancellation, abandonment, settlement, or retirement
  keeps that identity rooted from its construction region, and publishes its
  graph in one access region. Reclamation fixtures assert liveness and
  eventual collection, never which collection epoch reclaimed a slot.
- Install one-shot collector probes last, after any setup that might itself
  collect. Snapshot completed collection epochs immediately before the
  disputed collection, and keep exact `+1` assertions inside that interval.
- `EvaluationRuntime::readiness()` is an instantaneous observational probe; it
  may see `Busy` while a worker briefly holds mutation admission to park.
  Assert "no new work" with fixed scheduler inventories or terminal
  observations, not an immediate readiness result.
- For an order-dependent evaluator, coordinator, promise, reflection, or spark
  defect, place the participating operations explicitly on both sides of the
  disputed transition. Repeating an uncontrolled threaded test is only a
  stress/smoke check and must not be cited as correctness evidence. Prefer
  natural blocking callbacks first, narrow test-only hooks second, and a model
  checker for custom multi-atomic or multi-lock protocols.
