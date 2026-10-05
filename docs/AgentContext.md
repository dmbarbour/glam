# Agent Context

This is the short required-reading checklist for implementation work. It is
not an architecture guide, syntax reference, or development diary. Current
control flow belongs in `docs/architecture/`; detailed regression hazards
belong in `docs/agent_context/`; target behavior belongs in the design docs.

## Where to Look

| Work | Current architecture | Regression-sensitive rules |
| --- | --- | --- |
| Source loading, modules, CLI, batch lifecycle | [`architecture/assembly.md`](architecture/assembly.md) | [`agent_context/assembly.md`](agent_context/assembly.md) |
| Built-in `.g` compiler and macros | [`architecture/front_end.md`](architecture/front_end.md) | [`agent_context/g_syntax.md`](agent_context/g_syntax.md) |
| Values, laziness, sessions, workers | [`architecture/evaluation.md`](architecture/evaluation.md) | [`agent_context/evaluation.md`](agent_context/evaluation.md) |
| Managed values, roots, collection policy | [`architecture/values.md`](architecture/values.md) | [`agent_context/evaluation.md`](agent_context/evaluation.md) |
| Freer effects, heap, reflection tasks | [`architecture/reflection.md`](architecture/reflection.md) | [`agent_context/reflection.md`](agent_context/reflection.md) |
| Structured failures and configured logging | [`architecture/diagnostics.md`](architecture/diagnostics.md) | [`agent_context/diagnostics.md`](agent_context/diagnostics.md) |
| Interaction nets | [`architecture/interaction_nets.md`](architecture/interaction_nets.md) | [`agent_context/interaction_nets.md`](agent_context/interaction_nets.md) |
| Objects and linearization | Front-end and evaluation notes above | [`agent_context/objects.md`](agent_context/objects.md) |

[`src/README.md`](../src/README.md) is the compact source-module map.
[`Decisions.md`](Decisions.md) records why significant decisions were made;
standing docs keep the resulting rules. The log is incomplete: it starts late
in development.
[`DistilledDesign.md`](DistilledDesign.md) describes intended design, not
necessarily implemented behavior. [`SyntaxCheatSheet.md`](SyntaxCheatSheet.md),
[`CLI.md`](CLI.md), and [`Macros.md`](Macros.md) are user-facing references;
verify current bootstrap acceptance against tests and samples.

## Cross-Layer Boundaries

- `.g` syntax, lexical scope, capture analysis, and sugar end in `g_syntax`.
  The front end lowers affine `ResolvedExpr<Value>` directly into closed
  semantic values and interaction nets. Core and evaluation have no syntax
  expression, local environment, lambda AST, or closure representation.
- Source discovery and provenance remain assembler-owned. A front end receives
  raw artifact bytes plus a narrow `CompileContext`; it cannot infer filesystem
  authority or inspect opaque origins.
- Every public `Value` is rooted in exactly one `EvaluationRuntime`. Construct
  through that runtime or assembler's `Values` factory and reject foreign roots
  at public boundaries before exposing recursive core values.
- A managed `Gc<T>` is an interior edge, never durable ownership. Observe it
  only under matching bounded value access; publish state crossing that region
  as a registered root. Managed access is callback-free, managed destruction
  is passive, and `NoAuto` collection runs only through explicit stable runtime
  maintenance.
- Evaluation is pure value demand. Reflection effects, shared heap edits,
  diagnostics, and external I/O remain outside value and interaction-net
  semantics. Reflection may inspect evaluation; evaluation cannot observably
  depend on reflection.
- `EvaluationSession` is an external demand-owner lease. Machine-visible
  `EvalContext` retains demand state, not a recoverable owner lease. Runtime
  coordinator records own scheduled machines, terminal publication, exact
  dependencies, and failure reporting.
- Permanent failures retain structured Glam diagnostic values and ordered
  context until a client explicitly projects them. Retryable waits and
  unassigned promises are scheduler states, not errors.
- The embedding facade stays narrower than runtime internals. Extend
  `Assembler::reflection` or a constructed capability when client policy needs
  privileged access; do not leak raw runtime resources or add renderer policy
  to evaluator builtins.
- Generic interaction-net modules own topology and reduction mechanics.
  `core_net`, `eval`, and `g_syntax` supply semantic specialization; do not move
  syntax or core policy into the generic graph implementation.

## Working Rules

- Prefer narrow, testable slices and focused regression tests.
- Treat valid and invalid samples as executable syntax specifications.
- A panic is a bug. Code that observes user input must detect invalid states
  before they can panic: evaluation reports an `EvaluationFailure`, and parsing
  backtracks while preserving closest-match diagnostics. A panic must still not
  poison runtime state unnecessarily: if a client catches it, the runtime
  remains usable. Unwind handling around user-provided callbacks serves that
  goal; it does not make the panic acceptable. A panic is never semantics:
  never convert it into an `EvaluationFailure` or cache it as a lazy result.
  It is a task-layer interruption. See the
  [panic-safety plan](plans/UserInputPanicSafety_2026-10-04.md).
- Use `rg`/`rg --files` for discovery and preserve unrelated worktree changes.
- Keep implementation claims out of target-state design documents and
  chronological transition notes out of current architecture/invariant docs.
- State one authoritative owner for a lifecycle or invariant and link from
  adjacent documents instead of repeating the full rule.
- When changing concurrent publication, activation, cancellation, or shutdown,
  force every disputed event ordering with barriers, channels, or a model
  checker. Repetition of an uncontrolled concurrent test is stress/smoke
  evidence only: it is never proof that a nondeterministic defect is fixed.
  If the failing ordering cannot be forced, report the verification gap rather
  than accepting a transient or repeatedly passing test.
- When removing a check or representation, distinguish redundant work from a
  deliberate boundary projection or zero-cost invariant type.
- During a major representation or ownership transition, add syntax-backed
  negative tests that show a retired type, signature shape, or call path
  cannot reappear. Key them by module and item, not by exact source text or
  counts. At the transition's closing review, retire these tests and keep only
  the negative rules that remain durable. Do not use fingerprints or census
  totals as evidence of safety. Rust tests must not read plan or review prose.
- Removing a trait or representation from a widely used type breaks every
  implicit consumer at once. Partition such a removal from the start by
  production seam, fixture seam, negative compiler contract, and dynamic
  certification.
- Plan and review step IDs (such as `W7B` or `GCI11R-002D.2e.4`) may appear in
  code and comments while their work is active. At the next cleanup, replace
  each one with a reference to its `Decisions.md` entry or with a short
  explanation, whichever is clearer. A step ID never stands alone as a
  justification. (Glam itself uses "label" for a dictionary key.)

## Verification

`rust-toolchain.toml` pins the exact Rust toolchain, and rustup selects it
automatically. The root `Cargo.toml` declares the matching `rust-version`, so
older compilers fail with an explicit version error. Do not change the pin as
part of unrelated work. Upgrade it at a plan boundary in a dedicated commit
that updates both files and fixes any new lints. Record `rustc --version`
alongside verification results in reviews. When upgrading, or when running
nightly tools, also check compiler future-compatibility warnings; nightly has
warned about deep auto-trait recursion in the root crate.

After Rust edits run `scripts/check.sh`, the workspace verification entry
point. Its levels are cumulative:

- `scripts/check.sh fast` — `cargo fmt --check`, workspace Clippy, and the
  workspace test suite at default features. The quick inner-loop gate.
- `scripts/check.sh` (default `all`) — adds the collector's own `check.sh`
  (glam-gc all-features tests, persistent-edge codegen latch, unsafe-site
  audit), the G0 semantic regressions, and the interaction-net profiling
  fixtures. The pre-commit gate. The profiling check names only the
  profiling-specific and profiling-augmented fixtures; it does not repeat the
  whole suite under instrumentation.
- `scripts/check.sh full` — adds the expensive tier: the aggressive-GC
  verification pass over the whole workspace, the cursor-stress and
  million-edge scale proofs, and Miri and the sanitizers. Run it before
  performance work and after any change to runtime or collector code, or to
  unsafe, tracing, mutation, root, admission, finalization, or scheduler
  behavior. Documentation-only changes may reuse the last result. Running it
  only periodically once let a regression stay unnoticed for weeks.

Scale-only proofs (cursor stress, million-edge) stay `#[ignore]`d in the
ordinary suite and run only at `full`. The project pins a stable toolchain, so
`full` skips Miri and the sanitizers with a notice unless a nightly with the
`miri` and `rust-src` components is installed; run
`crates/glam-gc/scripts/check-miri.sh` and
`crates/glam-gc/scripts/check-sanitizer.sh {address,thread}` directly once a
nightly is available. There is no CI yet; `scripts/check.sh` is the gate.

Dynamic tools detect defects; they are not ordering evidence. A clean
sanitizer run proves nothing about concurrency order, which still needs forced
schedules. A target too slow for Miri is recorded as a performance exclusion
with the matrix in `crates/glam-gc/VERIFY.md`, never counted as a pass.

Aggressive-GC verification collects about once per stable settlement cycle,
not at each entry or allocation, so a test that needs a collection at a
specific boundary requests it explicitly. A test whose primary purpose is
`NoAuto` behaviour does not run under the feature; if only its final
assertion depends on `NoAuto`, gate just that tail. For scale, `full` took
911 s on 2026-10-04 (rustc 1.99.0, 8 threads, no nightly tools).

Add a focused regression before a broad fix when practical, then run the full
suite. Documentation-only changes need link/path validation and
`git diff --check`; they do not require a full Rust test cycle unless Rust docs
or source changed.

Before declaring a large transition complete, audit the final implementation
against every named invariant and acceptance criterion. Passing tests are
evidence only for behavior they actually exercise.
