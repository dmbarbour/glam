# User-Input Panic Safety Plan — 2026-10-04

Status: open. The parser and evaluation inspections are done: F1 is fixed,
and no further panic was found. The poisoning audit is done, and its
containment design awaits maintainer decisions.

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

## Proposed containment design (2026-10-04)

**Principle:** a panic fails the smallest unit that owns the interrupted
work. The units, from smallest: value, demand or task, transaction, runtime.
Poisoning a larger unit than that is "unnecessary" in the policy's sense.

**No-regret changes.** Every option below needs these:

1. **Collector traces read through poison.** This affects the net cell and
   the lazy producer, and matches the checkpoint traces.
2. **Destructors never panic on poison.** Guard and handle drops skip their
   restore when the lock is poisoned, because the owning unit has already
   failed.
3. **Leaf lock classes recover via `into_inner` everywhere.** Their poison
   carries no information.
4. **Client values are dropped after releasing glam locks.** This covers
   subscribers and effect-token payloads.
5. **Per-value cells that still `expect` adopt the `Failed`/`Poisoned`
   disposition.** A poisoned net or checkpoint fails its value's demand and
   no longer panics again.

**Design decisions, pending the maintainer:**

- **Poll-boundary containment.** Add `catch_unwind` at the claimed-poll
  choke points: `ClaimedTask::poll`, `poll_claimed_client_demand`, and
  `poll_claimed_spark`. Release the claim through the existing failure paths.
  The recommended disposition fails the work with an internal-error
  `EvaluationFailure` that carries the panic message. The failure is cached
  on the lazy, following the host-call "refusing to replay" precedent. This
  happens on every thread, so outcomes do not depend on which thread ran the
  poll, and the worker survives. The panic hook still prints the bug.
  Alternatives: re-raise on client threads after releasing; or release
  without caching, which allows replay.
- **Runtime-core poison.** A panic inside a coordinator, transaction, or
  settlement critical section tears runtime-wide state. Recovery is unsound
  there, so this stays a runtime failure: the poisoning is necessary. The
  requirements are no abort and no hang:
  - destructors become no-ops;
  - later operations fail loudly instead of reporting `Busy` forever;
  - the client can drop the runtime and build another.

  The alternative is to make those critical sections panic-free or
  transactional so they could recover. That is larger work, deferred.
- **Callbacks inside commits.** Catch client panics at the call site, under
  the lock, so the panic never crosses a glam guard. Each callback then gets
  a defined disposition:
  - a panicking conflict index fails that validation with an error;
  - a panicking subscriber is skipped for that event, and the remaining
    subscribers still receive it.

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
2. **Poison recovery.** Recover per lock class, or keep treating poisoning as
   terminal and instead make every panic-adjacent path avoid holding locks.
   Proposed: per-class recovery for leaf locks, value failure for per-value
   cells, and runtime failure for the runtime core. See the proposed
   containment design.

## Acceptance

- Every class U site found by the inspection reports a diagnostic or
  `EvaluationFailure` and has a regression.
- The poisoning workstream's forced-panic tests show a runtime remains
  usable after a client catches a callback panic.
- Remaining panics are class I, with invariant-stating messages.
