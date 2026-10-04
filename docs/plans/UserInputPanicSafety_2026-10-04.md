# User-Input Panic Safety Plan — 2026-10-04

Status: open. W1 has begun; F1 is fixed.

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

## W1 findings (2026-10-04)

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
  deferred within W1. The likely remedies are a nesting-limit diagnostic for
  nested syntax, plus iterative handling of infix chains and deep-tree drop.
- **The lexer withstands hostile text.** Twenty-two probes covered non-ASCII
  names, text, comments, and operators; a BOM; CRLF and lone CR; tabs;
  zero-width and non-breaking spaces; invalid and truncated UTF-8; NUL bytes;
  and unterminated literals. Every probe produced a diagnostic or was
  accepted; none panicked or aborted.
- **Duplicated parser helpers have not drifted further.** The copies of
  `view_between`, `split_top_level`, and `error_at_view` are byte-identical.
  F1 shows how such copies diverge, so deduplicating them remains worthwhile.

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

- **W1 — Parser and lexer.** F1 is fixed. Inspect `g_syntax` for slicing,
  indexing, and `expect` on token views and spans. Also inspect byte-offset
  arithmetic on source text (UTF-8 boundaries) and recursion depth on nested
  input, which can overflow the stack and abort without unwinding. Each
  finding becomes a deterministic regression, preferably an invalid sample.
- **W2 — Lowering, builtins, and operators.** Find panics reachable from
  program-supplied values, such as arity, shape, and type mismatches, and
  convert them to `EvaluationFailure`.
- **W3 — Interaction nets.** Covers builder validation and reduction-time
  panics. N8's random closed-net generator follows the planned net polarity
  change, so the generator exercises the final `bind >< bind` semantics.
  Users can build nets with subnets disconnected from the public port. Such
  garbage must never fail evaluation unless demand reaches it.
- **W4 — Poisoning.** Inventory the mutexes and `RwLock`s whose poisoning
  makes a runtime unusable, especially `lock().expect(..)` in `Drop` impls
  and guards, and `try_lock().expect(..)` on collector paths. For each lock
  class, decide between recovering via `PoisonError::into_inner` (when the
  protected state stays consistent across the panic) and a documented bug
  boundary. Verify with forced panics from client callbacks: after the
  client catches the unwind, the same runtime still evaluates.

Order: W1, W2, W4, then W3. Any converted site that W4's ordering would
delay can be fixed immediately when found.

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
   Expected answer: per-class recovery, decided in W4.

## Acceptance

- Every class U site found by W1–W3 reports a diagnostic or
  `EvaluationFailure` and has a regression.
- W4's forced-panic tests show a runtime remains usable after a client
  catches a callback panic.
- Remaining panics are class I, with invariant-stating messages.
