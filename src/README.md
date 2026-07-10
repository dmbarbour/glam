
# Architecture Sketches

## Simplest Case

        glam --file source.g

- `main.rs` parses CLI, initiates ops
- `main.rs` prepares a compile-time context for the source
  - optional source path for local-import loading
  - prior module value for future mixin-style compilation
  - abstract module path for namespace-relative identities such as `abstract_global_path`
- `g_syntax.rs` parses file.g through compile-time context, sourcing bytes and reporting diagnostics there
- `g_syntax.rs` lowers AST to a module lambda body expression, plus lowering diagnostics, through compile-time context and a core-facing interface
- `main.rs` applies one temporary top-level fixpoint to the anonymous assembly module
- `eval.rs` evaluates core terms and lazily traversed nested module dictionaries
- `main` expects binary `asm.result`, writes to `stdout`

At the moment, even this simple case is not fully implemented. Thus, it remains the focus for now.
