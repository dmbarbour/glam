# Agent Notes

This repository is the Rust bootstrap for `glam`, a high-level language for
assembly-level software description. 

## Reading

Required reading:

- `docs/DistilledDesign.md` - a compressed view of eventual target design
- `docs/AgentContext.md` - constraints based on tasks or misadventures

Contingent:

- When writing glam ".g" code
  - `docs/SyntaxCheatSheet.md` - a compressed view of eventual syntax

- When contemplating or reviewing overall holistic implementation:
  - `docs/Overview.md` - project shape
  - `docs/DesignPrinciples.md` - guiding principles

- When more details are needed, e.g. deep reviews or initial implementation:
  - `docs/Design.md` - details and motivations on assembler, configuration, assembly
  - `docs/Syntax.md` - details and motivations on syntax

## Project Structure

- `docs/` - design docs and some agent docs
- `samples/` - Glam code samples for testing
- `src/` - Rust source code for project
- `tests/` - Rust tests

## Approach

- assembler is written in Rust
- clarity over performance, at least for now
  - but don't lock in poor performance
- favor robust Rust packages where they fit, e.g.
  - Chumsky for parsing
  - internment for interning
- keep implementation slices narrow and testable

## Routine Checks

Run these after Rust edits:

```sh
cargo fmt --check
cargo test
```
