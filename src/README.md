
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
- `core_net.rs` lowers each reached core lambda body once into an immutable,
  shared interaction-net template carrying `CoreNetData`
- `interaction_net.rs` provides generic `InteractionNet<Data>` topology,
  checked construction, active-pair discovery, and mutable runtime reduction;
  runtime nodes use monotonic IDs and hash-table storage while active pairs move
  through ready, blocked-call, and stuck scheduler collections
- `list.rs` provides compact byte leaves, generic value leaves, finger-tree
  ropes, and opaque lazy holes; `core::List` supplies `Value` and `Thunk`
- `eval.rs` retains the call-by-need compatibility evaluator while core
  `bind-data` operations migrate to demand-driven net reduction
- `main` expects binary `asm.result`, writes to `stdout`

At the moment, even this simple case is not fully implemented. Thus, it remains the focus for now.

The current interaction-net slice establishes the lambda-to-shared-net boundary
without exposing syntax. Templates use local fan sites and each runtime graph
gets one fresh namespace. The current oracle records dynamic duplication paths
directly; it provides reference semantics for replacing those histories with
Lamping-style bracket/croissant control interactions. The general construction
effects and complete `bind-data` evaluator still belong before adding the
`interaction_net` keyword.
