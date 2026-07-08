
# Architecture Sketches

## Simplest Case

        glam --file source.g

- `main.rs` parses CLI, initiates ops
- `g_syntax.rs` parses file.g as AST
- `g_syntax.rs` lowers AST to `core.rs` terms
- `eval.rs` evaluates core terms
- `main` expects binary `asm.result`, writes to `stdout`

At the moment, even this simple case is not fully implemented. Thus, it remains the focus for now.
