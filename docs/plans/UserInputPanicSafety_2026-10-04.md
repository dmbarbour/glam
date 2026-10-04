# User-Input Panic Safety Plan — 2026-10-04

Status: open. The parser and evaluation inspections are done: F1 is fixed,
and no further panic was found. The poisoning audit is done, and its
containment design is decided. Implementation is in progress.

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
   API reports the panicked kind, and workers survive.
3. **The lazy `Panicked` evaluation state**, including torn checkpoints,
   interrupted host calls (which replaces "refusing to replay"), and poisoned
   per-value cells.
4. **Runtime-core fault handling.**
5. **Call-site containment of client callbacks outside polls.**

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
  panics. N8's random closed-net generator follows the planned net polarity
  change, so the generator exercises the final `bind >< bind` semantics.
  Users can build nets with subnets disconnected from the public port. Such
  garbage must never fail evaluation unless demand reaches it.
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
