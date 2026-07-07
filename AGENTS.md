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
- The parser in `src/g_syntax.rs` is an early `.g` parser, not the final
  `.g` grammar.
- Use checked-in `samples/` files for smoke checks instead of ad hoc source
  files in `/tmp`.
- When configuration is needed, use `GLAM_CONF` with a checked-in fixture such
  as `samples/config/minimal.g`; the devcontainer default is
  `samples/config/dev.g`.

## Routine Checks

Run these after Rust edits:

```sh
cargo fmt --check
cargo test
```
