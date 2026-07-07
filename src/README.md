
# Architecture Sketches

## Simplest Case

        glam --file source.g

- `main.rs` parses CLI
- `g_syntax.rs` parses file.g as AST
- `g_syntax.rs` lowers AST to `core.rs` terms
  - dicts, lists, numbers, functions (builtins, inets)
  - TODO: currently done in 'main', but should be done by syntax
- `eval.rs` evaluates `asm.result` 
  - TODO: separate concern of what term `eval.rs` is evaluating
  - (should not reference assembly or 'asm.result', leave that to 'main')
- main expects binary, writes evaluated result to `stdout`

At the moment, even this simple case is not fully implemented. Thus, it remains the focus for now.
