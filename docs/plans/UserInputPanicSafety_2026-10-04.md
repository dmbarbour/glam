# User-Input Panic Safety Plan — 2026-10-04

Status: open. The parser and evaluation inspections are done: F1 is fixed,
and no further panic was found. The poisoning audit is done, and its
containment design is decided, and steps 1-5 are implemented. The net
polarity change it waited on has landed: `Bind >< Bind` now joins crossed.
What remains is the interaction-net inspection.

This plan responds to the holistic pre-performance review, X4 and Maintainer
Decision 1 ([review](../reviews/HolisticArchitecturePrePerformance_2026-10-03.md)).
Two user inputs crashed the process there: an interaction net (N1) and a
malformed braced `do` (F1). The review recommended an overall inspection of
panic sites that observe user input.

## Policy

Maintainer decision, 2026-10-04. The durable rule lives in
[`AgentContext.md`](../AgentContext.md) working rules.

- **A panic is a bug.** Neither an abort nor a caught unwind makes it
  acceptable.
- **Detect invalid input first.** Code that observes user input reports
  problems instead of panicking:
  - evaluation reports an `EvaluationFailure`;
  - parsing backtracks while preserving closest-match diagnostics.
- **Do not poison state unnecessarily.** If a client catches a panic, the
  runtime must remain usable. Existing unwind handling around user-provided
  callbacks serves this goal.

## Known instances

- **N1, interaction-net rewrites.** Fixed in `0d5c54df`. It also shows the
  poisoning hazard. The rewrite panicked while holding the shared net mutex.
  Then `NormalizationBatchGuard::drop` called `lock().expect(..)` during the
  unwind, and that second panic in a destructor aborted the process. Collector
  tracing also reaches the net through `try_lock().expect(..)`, so a poisoned
  net breaks collection.
- **F1, braced `do` with an empty statement spanning a line break.** Fixed
  with this plan. `do_expr.rs` held a private copy of `trim_layout` that had
  missed the shared helper's fix for layout-only views. Slicing `len..0` made
  its `expect` fire. The duplicate and its twin `is_layout_empty` are deleted
  in favor of the shared `structural` helpers. The new invalid sample
  `braced_empty_member` specifies empty-member diagnostics across line breaks
  for `do`, `let`, `where`, `with`, and `match`.

## Parser findings (2026-10-04)

All timings use a release build unless noted.

- **Exponential parse time on nested parentheses and lists.** This is a hang
  rather than a crash, so it belongs to performance work. The causes, the
  measurements, and a constant-time guard fix are in the deferred
  [parser backtracking performance plan](ParserBacktrackingPerformance_2026-10-04.md).
- **Stack overflow aborts on deep nesting.** An overflow cannot be unwound
  or caught. The release-build thresholds, from parsing alone (`--parse`),
  are:
  - nested `if`: about 10,000;
  - infix chains (`1 + 1 + …`): about 30,000 terms;
  - nested dictionaries: about 30,000.

  A debug build overflows sooner; nested `if` overflows at 1,000. Recursive
  drop of deep syntax trees may contribute, alongside recursive descent.
  Hand-written source does not reach these depths, so this abort class is
  deferred within the parser workstream. The likely remedies are a nesting-limit diagnostic for
  nested syntax, plus iterative handling of infix chains and deep-tree drop.
- **The lexer withstands hostile text.** Twenty-two probes covered non-ASCII
  names, text, comments, and operators; a BOM; CRLF and lone CR; tabs;
  zero-width and non-breaking spaces; invalid and truncated UTF-8; NUL bytes;
  and unterminated literals. Every probe produced a diagnostic or was
  accepted; none panicked or aborted.
- **Duplicated parser helpers have not drifted further.** The copies of
  `view_between`, `split_top_level`, and `error_at_view` are byte-identical.
  F1 shows how such copies diverge, so deduplicating them remains worthwhile.

## Evaluation findings (2026-10-04)

- **A deterministic sweep found no panics.** It took 11,720 variants of the
  88 samples: line prefixes, single-line deletions, mid-line cuts, and
  dropped closing delimiters. Running them through `--parse` produced no
  panics, aborts, or timeouts. Running them through full compilation and
  evaluation (`--file`, minimal configuration) also produced no panics or
  aborts. Four variants timed out, and all four were genuine program
  divergence: a combinator in `samples/contracts/macros/rewrite_rules.g`
  that recursed without consuming input once a line was deleted.
- **Builtins report structured errors.** Targeted probes covered `list`
  `head`, `tail`, `at`, `slice`, `split`, `split_end`, `len`, `map`, and
  `concat`; `math` `floor` and `mod`; arithmetic and comparisons;
  `object_from_dict`; and dictionary paths. Inputs included empty and
  out-of-range lists, negative, huge, and fractional indices, division and
  modulus by zero, and type mismatches. Every probe produced a diagnostic.
  One wording issue: a huge positive index reports "requires non-negative
  integer indices".
- **Interaction-net construction validates malformed nets.** Probes covered
  a port wired twice, a port wired to itself, unwired auxiliaries, a missing
  or duplicated exposed port, copy counts that were zero, negative, huge, or
  fractional, non-port wiring, foreign ports, and stuck or garbage pairs.
  Each produced an error or ran correctly. Some messages print internal
  `Port { node: NodeId(..) }` structures, which is a diagnostic-quality
  issue only.
- **Deep recursion is slow, not unsafe.** Evaluation time grows roughly
  quadratically with recursion depth, but nothing crashes. The finding is
  recorded in the deferred
  [evaluation recursion performance plan](EvaluationRecursionPerformance_2026-10-04.md).

## Poisoning findings (2026-10-04)

Three read-only audits covered every production lock class (44), every
client-callback site, and the scheduler's behavior when a claimed poll
unwinds. Today a client that catches a panic generally does **not** keep a
usable runtime. Lock poisoning is only one of three mechanisms:

- **Stranded claims.** No unwind boundary exists between a claimed poll and
  the coordinator. The only production `catch_unwind` calls are the two GC
  maintenance entries and output delivery. Every claim type lacks a
  destructor: `ClaimedTask`, `ClaimedClientDemand`, `ClaimedSparkWork`,
  `ClaimedReflectionWork`, `ClaimedDeferredWork`, and `ClaimedLazyRoute`.
  A panic during a poll leaves the record `Running` forever:
  - its waiters never wake;
  - `readiness()` reports `Busy` forever, so settlement is impossible;
  - `pump_until_stable` can park forever;
  - on a worker the thread dies silently, and `worker_count` never drops.

  Host callbacks such as import resolution run inside these polls, so a
  client's own code reaches this path. Not only internal bugs do.
- **Poison cascades and aborts.** About 278 production lock sites use
  `.lock().expect(..)`. Sixteen destructors reach such a site, directly or
  transitively. Once their lock is poisoned, any such handle dropped during
  an unwind aborts the process, which is how N1 aborted. They include:
  - the net guards: `NormalizationBatchGuard`, `CursorClaimGuard`, and the
    `Core{Call,Checkpoint,Operator}Claim` drops;
  - the coordinator handles: `ClientDemandHandle`, `LazyRouteDemandLease`,
    `EvaluationSession`, `SessionClosureWork`, and `PromiseResolver`;
  - the admission guards: `RuntimeMutationGuard` and
    `RuntimeGcActivityLease`;
  - `DiagnosticSubscriptionInner` and `EffectToken`.

  Two collector traces panic on a poisoned lock:
  `RuntimeNetCell::try_visit_logical_payloads` and `ManagedLazyCell::trace`.
  glam-gc treats a mark-phase panic as recoverable, so the heap survives.
  But every later collection panics again while the value stays reachable,
  so the runtime is stuck in `RetryRequired`. Skipping the poisoned value's
  edges instead would be unsound, because the `Trace` contract forbids
  omitting an edge. Tracing must read through poison, as the nine checkpoint
  traces already do.
- **Client code inside runtime-wide critical sections.**
  - **Conflict analysis.** `ConflictObservationIndex::may_conflict` runs
    under the runtime transaction lock, and `begin` runs there through
    `update_query`, which can sit under the settlement write guard. A panic
    in either poisons the transaction state; `pump_until_stable` then
    poisons the gate.
  - **Client values dropped under glam mutexes.** A diagnostic subscriber's
    `Arc` and an effect-token payload are both dropped while a glam mutex is
    held.
  - **A commit split by a client callback.** `AssemblerReflectionHost::commit`
    publishes diagnostics between committing the store and activating the
    children it launched.

**Lock classes by scope and critical-section risk.** "Leaf" means the
critical sections perform only trivial data operations. "Complex" means
they run evaluation, rewrite, or state-machine logic, or assertions:

| Scope | Leaf | Complex |
| --- | ---: | ---: |
| Runtime-wide | 10 | 6 |
| Session, transaction, task, or client object | 10 | 4 |
| Per value | 2 | 11 |
| CLI | 1 | 0 |

For leaf classes, a panic cannot tear the protected state. Recovery via
`PoisonError::into_inner` is therefore sound for all of them. Recovery is
unsound for:
- the coordinator state, whose obligations are taken before assertions;
- the transaction state, where a commit can be torn;
- a settlement write;
- the net cell, where a rewrite can be half-applied.

Per-value machine cells already show the right per-value disposition. Six
of them map poison to `Failed`/`Poisoned`; the net cell, the access and
net-WHNF checkpoints, the lazy producer, and the host-call checkpoint still
`expect`.

## Containment design (decided 2026-10-04)

### Panics are faults, not semantics

Maintainer direction: a panic is never part of glam semantics. An
`EvaluationFailure` is a normal semantic outcome, such as taking the head of
an empty list. A panic means one of two things: a runtime implementation
error, or a client that violated a contract.

Consequences:
- No panic is ever converted into an `EvaluationFailure` or cached as a lazy
  result.
- Backtracking (`.fail`, `.alt`) never sees a panic.
- Reflection `.eval` never produces `ok:` or `err:` for one.
- A panic never appears as a program diagnostic.

Because a panic is not semantics, the runtime is free to handle it however is
most robust. The one constraint is negligible overhead when no panic occurs.

A panic is modelled as an *interruption*, akin to halting on an unfulfilled
promise. It is task-layer state, never a lazy result. The scheduler already
draws the matching distinction:
- `EvaluationHalt::Blocked` and `UnassignedPromise` can be retried and are
  never cached.
- The wait-token terminals `Abandoned`, `Cancelled`, and `Killed` describe
  the loss of a producer, not a failure of the awaited value.

### Where the panic is recorded

Three placements were compared:
- **(1) Task terminal only:** a `Panicked` wait-token terminal; the lazy is
  untouched.
- **(2) Lazy evaluation state:** `Panicked(report)` beside `Source` and
  `Checkpoint` in the lazy's producer slot; the result slot stays empty.
- **(3) Lazy result:** a panicked case cached in the lazy's result
  `OnceLock`.

| | (1) Task terminal | (2) Lazy evaluation state | (3) Lazy result |
| --- | --- | --- | --- |
| Kept out of semantics | yes | yes: never in the result slot, so `.eval`'s demand halts | only if every result consumer special-cases it |
| Current observers | waiters, through the wait token | every observer, through the producer slot read on the slow path | every observer, through the lock-free fast path |
| Late observers | reinstall work and re-run the bug | halt with the original report and origin | same as (2) |
| Stickiness | none (like `OnceLock`) | per lazy (like `LazyLock`) | permanent |
| Torn or non-replayable progress | needs a second, per-value mechanism | the same `Panicked` state absorbs it | becomes results too |
| A consumer forgets the case | fails loudly (hang or invariant panic) | fails safely: not mistakable for a value | fails silently into semantics through a generic `Err(failure)` arm |
| Cost when unused | zero | zero | zero |

(3) is the most convenient: propagation, wakeups, and result-before-retire
ordering come for free. But it fails silently in the wrong direction, and
closing that gap means auditing every consumer of
`LazyResult = Result<_, Arc<EvaluationFailure>>`.

**Decision: (2) for lazies, (1) for tasks.**
- **Lazies.** A lazy whose own evaluation panicked, or whose progress a
  panic tore, gets the `Panicked` evaluation state. This includes a host call
  interrupted mid-invocation, which must not be replayed, and a net torn
  mid-rewrite. Every later observer halts with the original report.
- **Tasks.** Reflection tasks, client demands, sparks, and promise producers
  end with a `Panicked` wait terminal.
- **Waiters.** A waiter on a panicked producer halts and does not reinstall
  work. Unlike `Abandoned`, reinstalling would just re-run the bug.
- **Dependents.** A dependent of a panicked lazy halts at the task layer and
  stays retryable. A retry halts again at the same lazy with the same report,
  so stickiness never spreads past where the panic happened.
- **Promises.** A task-owned promise whose producer panicked records the
  panic on its producer obligation (task layer), not as an assignment.
  Abandonment without a panic keeps its existing producer-abandoned failure.
- **Clients.** The client API returns `Err` with a panicked kind. Reflection
  semantics are implementation-dependent, so `.task.status` may report
  `panicked`.

The panic hook still prints every panic, so each one remains a visible bug.

### Remaining dispositions

- **Runtime core.** A panic inside a coordinator, transaction, or settlement
  critical section tears runtime-wide state. Recovery there is unsound, so
  the runtime faults as a whole, and the fault is permanent. Requirements:
  - no abort and no hang;
  - destructors become no-ops;
  - later operations fail loudly instead of reporting `Busy` forever;
  - the client can drop the runtime and build another.

  Making these critical sections panic-free or transactional is deferred.
- **Client callbacks outside polls.** These are caught at the call site,
  under the lock, so the panic never crosses a glam guard. The operation that
  invoked the callback is what gets interrupted:
  - a panicking conflict index interrupts that validation, and the commit
    returns `Err`;
  - a panicking subscriber is skipped for that one event.
- **Lock classes.** For leaf classes, poison carries no information, so they
  recover via `into_inner`. A poisoned per-value cell means its progress is
  torn: its lazy enters the `Panicked` state. The six cells that today map
  poison to `Failed` change accordingly.

### Implementation sequence

1. **No-regret changes**, which every alternative needs. Done on
   2026-10-04, except for destructors that reach runtime-core locks:
   - **Collector traces read through poison.** This covers the net cell and
     the lazy producer.
   - **Leaf lock classes recover.** Twenty-eight classes recover via
     `into_inner` at every site. Each field's documentation now states the
     invariant that makes recovery sound, so a future complex critical
     section is a visible contract change.
   - **Net cleanup tolerates poison.** Releasing or restoring a claim on a
     poisoned net is skipped through `with_cleanup_mut_via` and its edge
     form: restoring on a torn net is moot, and re-running a transition there
     could panic during the unwind. Batch close reads through poison instead,
     because its bookkeeping is independent of the topology and contenders
     still need waking.
   - **Client values are dropped after glam locks are released.** This
     covers diagnostic subscribers and effect-token payloads. It also keeps
     a client destructor that re-enters the bus from deadlocking.
   - **Destructors that reach runtime-core locks are moved to step 4.** These
     are the coordinator, the settlement gate, and the transaction state.
     Their destructors run long call chains shared with normal operation, so
     a no-op disposition needs the runtime-wide fault signal that step 4
     introduces.

   Regressions:
   - A panic under the net lock with an open batch, which reproduces N1's
     abort on the old code; collection then traces the rooted poisoned net.
   - A cursor claim unwinding through a poisoned net.
   - Collection through a poisoned lazy producer.
   - A subscriber whose destructor panics.

   Each regression fails with its fix reverted, except the subscriber test:
   leaf recovery alone already keeps the bus usable.
2. **Poll-boundary containment.** `catch_unwind` at `ClaimedTask::poll`,
   `poll_claimed_client_demand`, and `poll_claimed_spark` ends the claim as
   `Panicked` through the existing terminal path. Waiters halt, the client
   API reports the panicked kind, and workers survive. Done on 2026-10-04.

   Implementation rules:
   - **Only the poll boundary creates a panic value.** An evaluator machine
     needs no panic vocabulary: it treats a panicked wait as still blocked,
     and the boundary halts it. The boundary checks twice. Before polling,
     it checks the dependency the claim last blocked on. After polling, it
     checks whether a `Blocked` result names a panicked dependency. The
     pre-poll check is required: a WHNF demand re-reads its uncached lazy
     and would reinstall the panicked route, so panic, wake, and reinstall
     would loop forever.
   - **Converting a panic into a failure re-raises it.** These sites call
     `EvaluationPanic::resume` instead of producing a failure:
     - `EvaluationHalt::into_permanent_failure`, whose catch-all arm
       previously formatted any non-failure halt into a failure;
     - `TaskHalt::into_failure` and `into_failure_root`;
     - `From<api::Error> for TaskHalt`;
     - the macro, lookup, and compiler-helper sites that format a halt into
       a message.

     Inside a poll, the boundary catches the re-raised panic again and keeps
     the original report.
   - **A machine's own panic outranks cancellation and session closure.**
     Such a machine is torn: it is dropped without `cancel()`, under an
     unwind boundary. A panic propagated from a dependency yields to those
     dispositions, because that machine is intact.
   - **Promise producers.** `settle_panicked_work` publishes the `Panicked`
     wait and task status and leaves owned promises unassigned. It records
     the panic on the producer obligation inside the promise's completion
     publication. The subscribe predicate and `WorkDependency::is_terminal`
     both count a panicked producer, so observers neither park nor lose a
     wake. They halt with the report.
   - **Sparks are speculative**, and nobody waits on one. A panicking spark,
     or one blocked on panicked work, retires as complete without a report.
     The value's real demand reports any panic to its own waiters.
   - **Direct effect runs** have no poll boundary. A panicked dependency
     ends the run with a panicked `TaskHalt`, and its local promises stay
     unassigned.
   - **Diagnostic-rendering fallbacks are unchanged.** These are the context
     and enrichment helpers that substitute a fallback rendering on any
     halt. They never create a semantic failure.

   Public surface:
   - `ErrorKind::{Failure, Panic}`, `Error::kind`, and `Error::panic_message`.
   - `TaskHalt::panic_message`.
   - `EffectLifecycleStatus::Panicked`.
   - The reflection status `panicked`. Its `.task.value` and `.task.error`
     fail, as for `abandoned`. A `.task.join` on a panicked task halts the
     joiner instead of producing `err:`.

   Regressions:
   - A client-thread panic returns `ErrorKind::Panic` with no diagnostic. It
     runs the panicking thunk exactly once, and the same runtime then
     evaluates, settles, and collects.
   - A worker survives a panic in the lazy route that a spark installs.
   - A panicked promise producer leaves its promise unassigned, and its
     observer halts with the report.
   - The `panicked` status round-trips.

   Both boundary regressions fail with their boundary disabled. The
   collector inventories record the moved panic-payload helper and six new
   acyclic call edges.

   Known gaps:
   - **Step 3, now resolved.** A new demand reran a lazy whose own evaluation
     panicked, and an interrupted host call became the `EvaluationFailure`
     "refusing to replay".
   - **Release paths can still panic** on coordinator assertions; this is
     step 4.
   - **Unobserved panics** are visible only through the panic hook and
     `.task.status`. `QuiescenceReport` has no panic ledger.
   - **Local promise owners** of direct effect runs are outside the
     boundary.
3. **The lazy `Panicked` evaluation state**, including torn checkpoints,
   interrupted host calls (which replaces "refusing to replay"), and poisoned
   per-value cells. Done on 2026-10-04.
   - **Recording.** Every lazy is evaluated in its own route, confirmed by
     probing: a direct demand and a dependent's demand both catch the panic
     in the panicking lazy's route. So a route's own-panic release records
     `Panicked` on exactly the lazy whose evaluation panicked, whatever the
     scheduling.
   - **What the state does.** It releases the source or checkpoint, so the
     work is never replayed and its edges are reclaimed. A lazy that cached
     its result before panicking keeps the result, and the route settles
     with it.
   - **Detection stays off the demand hot path.** Checking for the state when
     a route is reserved would open an access region on every lazy demand. So
     a demand of a panicked lazy installs an ordinary route instead. That
     route's poll finds no source, checkpoint, or result, and re-raises the
     recorded report from `cached_poll`. The boundary then halts it with the
     original report. The extra route costs something only for lazies that
     already panicked.
   - **Torn progress is a fault.** The six cells that mapped poison to a
     semantic `Failed` now panic: object fixpoint, list effect, builtin, list
     front, key conversion, and WHNF state. The message is "poisoned by an
     earlier panic", and the boundary contains it. A torn checkpoint is
     normally released along with its panicked owner, so this path is
     defensive.
   - **Interrupted host calls are a fault.** An interrupted host invocation
     re-raises "refusing to replay" instead of caching a failure. Every
     production poll is behind the boundary, so the original panic has
     already marked the lazy.
   - **Tests adapted to the intended semantics:**
     - the host-call replay test now expects that fault;
     - the WHNF-poison test expects a fault instead of a rooted failure.
   - **New regressions:**
     - a re-demand of a panicked lazy halts with the same report and never
       reruns its thunk;
     - a panicking host callback is invoked exactly once across two demands.

     Both fail with the recording disabled.
   - **Inventories record:**
     - one cold-path outer admission and one same-region root publication,
       both in `record_lazy_panic`;
     - the changed acyclic call-edge fingerprint.
   - **Remaining limit.** A net shared by several lazies, once torn, faults
     each later observer with "shared runtime net was poisoned", not the
     original report.
4. **Runtime-core fault handling.** Done on 2026-10-04, with a narrowed
   surface decided by the maintainer. Instrumenting every core lock site
   (about 130) is unnecessary:
   - **Detection is free.** Std mutexes already record poison when a guard
     unwinds.
   - **One authority covers every core mutation.** Every coordinator and
     transaction mutation holds `RuntimeMutationGuard` or a settlement guard.
   - **Clients park in two places.** These are the coordinator's
     `wait_for_change_for` and the activity condvar.

   Mechanism:
   - **The poison flag.** `RuntimeMutationAdmission` holds a set-once
     `poisoned` flag. The coordinator and the transaction state register as
     `RuntimeCoreState`s, which report their poison and wake their parked
     threads.
   - **Marking.** Three triggers set the flag:
     - A mutation or settlement guard unwinding probes actual core poison.
       It checks poison rather than `panicking()`, because contained
       evaluation panics also unwind through guards taken inside a poll.
     - The worker loop has an outer catch. A panic that escapes the poll
       boundaries came from scheduler code, so it marks the runtime and the
       worker exits.
     - Any `is_poisoned` probe marks the runtime when it finds poison.

     Marking wakes coordinator waiters and advances activity, so parked
     threads relock and fail loudly instead of hanging.
   - **Checks:**
     - `mutation_guard()` checks the flag with one atomic load and fails
       loudly.
     - Core-reaching destructors return early: client-demand handles,
       lazy-route leases, sessions, reflection reservations, deferred-
       producer abandonment, promise resolvers, the runtime state, the
       executor (which still wakes parked workers), and GC leases. Dropping a
       poisoned runtime is safe.
     - `readiness()` returns the unit variant `RuntimeReadiness::Poisoned`.
       The panic message went to the thread that unwound.
     - `pump_until_stable` returns immediately.
     - GC request and service return `RuntimeMaintenanceErrorKind::Poisoned`,
       whose message names the runtime core.
   - **Accepted gap.** A core panic in a read-only critical section, with no
     authority held, is detected lazily: by the next mutation, worker access,
     or readiness probe. Until then, a client parked only on another thread's
     progress may stay parked.
   - **Regressions:**
     - A scheduler panic under mutation authority, while a client waits on a
       worker-held value. The client wakes and fails loudly, readiness is
       `Poisoned`, the stable pump returns, collection is refused, and the
       runtime drops cleanly. The test fails with the unwind hook disabled.
     - A read-only scheduler panic is detected by `readiness()`, and a later
       evaluation fails loudly.
     - A panic under settlement authority poisons the gate and marks the
       runtime.

     Poison wakes pass through the coordinator's profiled notification
     boundary, under a new `RuntimePoison` mutation kind. The executor-drop
     wake uses `ExecutorAvailability`. The RAII inventory classifies the
     settlement guard's new destructor.
5. **Call-site containment of client callbacks outside polls.** Done on
   2026-10-04. Callbacks inside polls were already contained by step 2: host
   calls, import resolvers, the conflict index's `begin` and `observe` in
   task journals, and custom reflection hosts. The remaining sites fall into
   two dispositions.
   - **Resume.** Commits and validations a client or task initiated call the
     client's conflict analysis under the transaction lock. That lock is
     runtime core:
     - `try_commit_transaction`, `validate_transaction`,
       `commit_reflection`, `validate_reflection`, and `update_query` catch
       the panic under the lock;
     - the client is called before the store changes, so the store stays
       consistent;
     - after releasing their guards, they resume the client's own panic.

     The initiating client gets its panic back, and inside a task the poll
     boundary marks that task `Panicked`. This refines "the commit returns
     `Err`": an `Err` inside a task would have become a semantic failure.
   - **Skip.** Runtime-owned notifications skip only the panicking callback:
     - task-status query publication, which leaves the status unwritten;
     - `TaskStatusWake` closures;
     - diagnostic subscribers, which the remaining subscribers still reach.
       Publication is never interrupted, so a commit that publishes
       diagnostics is never split.
     - destructors of retired external owners, which no longer interrupt the
       unrelated host call draining them.

     The panic hook still reports each skipped panic.
   - **Interrupt the launched task.** A panicking reflection launcher settles
     the reserved task it was launching as `Panicked` and retires it. The
     intact parent continues, and a parent that waits on the child halts by
     the waiter rule.
   - **Tests.** The external-owner drain test encoded the old propagation and
     now expects the drain to continue past the panicking owner. New
     regressions:
     - a panicking conflict index reaches the committing client, and the
       runtime keeps committing. This test fails without the containment,
       because the panic then poisons the runtime.
     - a panicking subscriber is skipped while publication continues.
   - **Not separately regressed.** The launcher and status-wake catches have
     no dedicated tests. Both are single choke points with no test fixture
     for a custom launcher or wake today.

Each step lands with forced-panic regressions. The acceptance test: after a
client catches a panic, the same runtime evaluates unrelated work, settles,
and collects.

## Method

Production modules contain about 3,900 panic-capable sites (`panic!`,
`unreachable!`, `expect`, `unwrap`, and assertions). This count includes
inline test modules, so it is an upper bound. The inspection is risk-driven
rather than line by line. Each site reachable from an entry surface is
classified:

| Class | Meaning | Action |
| --- | --- | --- |
| I — internal invariant | Unreachable from any input once earlier validation passes | Keep. Prefer an `expect` message that states the invariant. |
| U — user-reachable | Some program, source text, or API call reaches it | Convert to a diagnostic or `EvaluationFailure`. Add a regression. |
| P — poisoning hazard | A lock is held across code that can panic, or `Drop` and guard code panics on poison | Leave state usable after unwind. Recover from poison when the protected state is consistent, and never panic in `Drop` on poison. |

Entry surfaces:

- source text: the lexer, parser, and lowering in `g_syntax`;
- module loading and compilation;
- evaluator builtins and operators applied to program values;
- interaction-net construction and reduction;
- reflection effects;
- the public embedding API;
- CLI arguments and configuration.

## Workstreams

- **Parser and lexer.** F1 is fixed. Inspect `g_syntax` for slicing,
  indexing, and `expect` on token views and spans. Also inspect byte-offset
  arithmetic on source text (UTF-8 boundaries) and recursion depth on nested
  input, which can overflow the stack and abort without unwinding. Each
  finding becomes a deterministic regression, preferably an invalid sample.
- **Lowering, builtins, and operators.** Find panics reachable from
  program-supplied values, such as arity, shape, and type mismatches, and
  convert them to `EvaluationFailure`.
- **Interaction nets.** Covers builder validation and reduction-time
  panics. N8's random closed-net generator follows the net polarity change,
  so the generator exercises the final `bind >< bind` semantics. That
  change landed on 2026-10-05: binds join crossed (`B.1-C.2`, `B.2-C.1`),
  and function binds list `[result, argument]`.
  Construction rejects a net with a component disconnected from the public
  port, as a miswiring error; test builds already check this, and polarity
  slice 3 enforces it at `try_finish`. Only disconnected garbage that
  reduction leaves behind, such as closed loops and erased subnets, must never
  fail evaluation unless demand reaches it. This follows the maintainer's
  2026-10-05 decision in the [polarity plan](NetPolarityChecker_2026-10-05.md)
  and supersedes this plan's earlier wording, which let users build
  disconnected subnets.
- **Poisoning.** Inventory the mutexes and `RwLock`s whose poisoning
  makes a runtime unusable, especially `lock().expect(..)` in `Drop` impls
  and guards, and `try_lock().expect(..)` on collector paths. For each lock
  class, decide between recovering via `PoisonError::into_inner` (when the
  protected state stays consistent across the panic) and a documented bug
  boundary. Verify with forced panics from client callbacks: after the
  client catches the unwind, the same runtime still evaluates.

Order: parser, evaluation, poisoning, then interaction nets. Any converted
site that this ordering would delay can be fixed immediately when found.

## Decisions for the maintainer

1. **Discovery method — decided 2026-10-04.** Conventional inspection comes
   first, guided by reasoning about where input reaches panics. Fuzzing is a
   discovery tool, not a routine test. It is deferred until inspection stalls,
   and its tooling (`cargo-fuzz` on a nightly toolchain) is obtained only
   then. Every fuzzing finding becomes a deterministic regression; no fuzz run
   joins `scripts/check.sh`. Generated inputs stay shallow, and each run uses a
   per-input timeout, so the known exponential nesting cost does not block
   discovery. Fixing that performance issue is not a prerequisite.
2. **Poison recovery — decided 2026-10-04.** Leaf lock classes recover.
   Poisoned per-value cells put their lazy into the `Panicked` evaluation
   state. Runtime-core poison faults the whole runtime without aborting or
   hanging. See the containment design.
3. **Panic disposition — decided 2026-10-04.** A panic is a fault, never
   semantics. It is modelled as a task-layer interruption: a `Panicked` wait
   terminal for tasks and a `Panicked` evaluation state for lazies, never a
   lazy result or `EvaluationFailure`. The containment design records the
   comparison and reasoning.

## Acceptance

- Every class U site found by the inspection reports a diagnostic or
  `EvaluationFailure` and has a regression.
- The poisoning workstream's forced-panic tests show a runtime remains
  usable after a client catches a panic. The same runtime evaluates
  unrelated work, settles, and collects. No panic is observable as an
  `EvaluationFailure`.
- Remaining panics are class I, with invariant-stating messages.
