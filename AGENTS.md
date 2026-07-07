# Agent Notes

This repository is the Rust bootstrap for `glam`, a high-level language for
assembly-level software description. Before broad implementation work, read:

- `docs/Overview.md` for project shape.
- `docs/DesignPrinciples.md` for the control/reproducibility philosophy.
- `docs/AgentContext.md` for compact implementation constraints.

Keep implementation slices narrow and testable. Prefer preserving the language
vision in `docs/*` over choosing whatever is easiest for the current Rust code.

## Current Bootstrap

- The executable is a temporary bootstrap shell, not the final command model.
- Bare command-line arguments are reserved for configured `conf.cli` rewriting.
- Developer inspection commands should be explicit options, e.g. `glam --parse`.
- The parser in `src/source.rs` is an early source-surface parser, not the final
  `.g` grammar.

## Routine Checks

Run these after Rust edits:

```sh
cargo fmt --check
cargo test
```
