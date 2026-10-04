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

- **W1 — Parser and lexer.** F1 is fixed. Sweep `g_syntax` for slicing,
  indexing, and `expect` on token views and spans. Add a no-panic harness
  that mutates samples from `samples/` (truncating, deleting, and
  duplicating tokens and lines) and asserts that `inspect_g_source` returns
  diagnostics without panicking.
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

1. **No-panic harness.** `cargo-fuzz` needs a nightly toolchain, which the
   project does not pin. The alternative is a deterministic stable-toolchain
   mutation test inside `cargo test`. Recommendation: stable mutation test
   first; revisit fuzzing when a nightly is pinned for Miri and sanitizers.
2. **Poison recovery.** Recover per lock class, or keep treating poisoning as
   terminal and instead make every panic-adjacent path avoid holding locks.
   Expected answer: per-class recovery, decided in W4.

## Acceptance

- Every class U site found by W1–W3 reports a diagnostic or
  `EvaluationFailure` and has a regression.
- The no-panic harness runs in `scripts/check.sh` and passes.
- W4's forced-panic tests show a runtime remains usable after a client
  catches a callback panic.
- Remaining panics are class I, with invariant-stating messages.
