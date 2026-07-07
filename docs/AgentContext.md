# Agent Context

This file summarizes high-signal constraints for coding agents. It should point
to source docs rather than replace them. Agent may refine file for task-relevant content, keeping it focused and slim.

## Command Line

Source: `docs/Design.md`, "Configuration" and "Assembly".

- `conf.cli` may rewrite command-line arguments if and only if the first
  command-line argument does not start with `-`.
- Therefore, bare invocations such as `glam parse file.g` or `glam file.g`
  should not become built-in executable subcommands.
- Bootstrap/debug commands should use option-shaped forms, such as
  `glam --parse file.g`, so they do not occupy the future `conf.cli` space.
- Final assembly inputs are expected to be expressed with options such as
  `--file`, `--script`, and `--`.
- Current spike supports `glam (-f|--file) PATH` for one source file and writes
  `asm.result` to stdout by default.

## Source Surface

Source: `docs/Syntax.md`.

- A `.g` file should start with a language version declaration, e.g.
  `language g0`.
- Initial character set is printable ASCII plus whitespace; UTF-8 is an
  extension.
- Comments are line comments beginning with `#`.
- Toplevel declarations begin on unindented lines. Continuation lines are
  indented, except a final closer-only line may dedent.
- Introductions and overrides are distinct: `name = Expr`, `name := Expr`, and
  `name ::= Update`.

## Implementation Posture

- Keep the Rust bootstrap comprehensible and dependency-light until complexity
  justifies a crate.
- Prefer spans and diagnostics over panics for source-facing behavior.
- Tests should pin design constraints as soon as they become executable.
- Use Chumsky for growing `.g` grammar work. Keep hand-written parsing limited
  to small source-normalization steps where that is clearer than grammar code.
- Keep syntax parsing separate from syntax-independent evaluation. The current
  evaluator only supports text data expressions.
- `.g` parser AST and `.g`-specific lowering rules belong in `g_syntax`.
- Syntax-independent terms and values belong in `core`; keep module/import
  policy and introduce/override checks out of `core`.
- Evaluation should consume core terms/values, not `.g` syntax nodes directly.

## Samples

- `samples/` is the shared home for `.g` source used in testing,
  experimentation, and user education.
- Prefer checked-in samples for smoke checks instead of one-off `/tmp` files.
- Keep samples small and purpose-specific; if current bootstrap behavior depends
  on them, cover them with tests.

## Configuration Fixtures

Source: `docs/Design.md`, "Configuration".

- Real assembly is split between command-line assembly elements and user
  configuration from the environment.
- `GLAM_CONF` is the explicit way to select configuration in tests and scripts.
- `samples/config/minimal.g` is the smallest checked-in config fixture.
- `samples/config/dev.g` is the workspace/devcontainer default, currently set
  by `.devcontainer/devcontainer.json`.
