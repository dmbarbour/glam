
# Architecture Sketches

## Simplest Case

        glam --file source.g

- `main.rs` parses CLI
- `g_syntax.rs` parses file.g as AST
- `g_syntax.rs` lowers AST to `core.rs` terms
  - dicts, lists, numbers, functions (builtins, inets)
- `eval.rs` evaluates core terms
  - `main` still selects `asm.result` for the spike; this will move once user-defined syntax owns lowering
- main expects binary, writes evaluated result to `stdout`

At the moment, even this simple case is not fully implemented. Thus, it remains the focus for now.
