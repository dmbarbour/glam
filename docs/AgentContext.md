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
| Freer effects, heap, reflection tasks | [`architecture/reflection.md`](architecture/reflection.md) | [`agent_context/reflection.md`](agent_context/reflection.md) |
| Structured failures and configured logging | [`architecture/diagnostics.md`](architecture/diagnostics.md) | [`agent_context/diagnostics.md`](agent_context/diagnostics.md) |
| Interaction nets | Evaluation handoff above | [`agent_context/interaction_nets.md`](agent_context/interaction_nets.md) |
| Objects and linearization | Front-end and evaluation notes above | [`agent_context/objects.md`](agent_context/objects.md) |

[`src/README.md`](../src/README.md) is the compact source-module map.
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
- Prefer source spans and structured diagnostics to panics for user-facing
  failures.
- Use `rg`/`rg --files` for discovery and preserve unrelated worktree changes.
- Keep implementation claims out of target-state design documents and
  chronological transition notes out of current architecture/invariant docs.
- State one authoritative owner for a lifecycle or invariant and link from
  adjacent documents instead of repeating the full rule.
- When changing concurrent publication or shutdown, force the disputed event
  ordering with barriers. A transiently passing stress test is not proof.
- When removing a check or representation, distinguish redundant work from a
  deliberate boundary projection or zero-cost invariant type.

## Verification

After Rust edits run:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Add a focused regression before a broad fix when practical, then run the full
suite. Documentation-only changes need link/path validation and
`git diff --check`; they do not require a full Rust test cycle unless Rust docs
or source changed.

Before declaring a large transition complete, audit the final implementation
against every named invariant and acceptance criterion. Passing tests are
evidence only for behavior they actually exercise.
