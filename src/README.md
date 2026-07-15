
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
- `core_net.rs` lowers each reached core lambda body once, collapsing a maximal
  leading curried lambda spine into one bind chain, then instantiates one shared
  runtime net carrying `CoreNetData`; calls can lazily copy its normalized
  frontier through evaluator-only remote cursors; CompileContext-prepared closed
  lambda spines evaluate as `Value::Net`, while captured, nested-dependent, and
  access-bearing lambdas retain the compatibility path
- `interaction_net.rs` provides generic `InteractionNet<Data>` topology,
  checked construction through one `NetBuilder` (including fallible
  wiring/finalization and balanced copy helpers), active-pair discovery, and
  mutable runtime reduction; builder-only one-output copy tunnels are spliced
  out before a template is produced;
  runtime nodes use monotonic IDs and hash-table storage, preserve a stable
  exposed interface, and allocate fan sites locally while active pairs move
  through ready, blocked bind/host/cursor, and diagnostic-bearing stuck
  scheduler collections; layered cursors expose precise dependencies that the
  evaluator drives without nested runtime locks
- `list.rs` provides compact byte leaves, generic value leaves, finger-tree
  ropes, and opaque lazy holes; `core::List` supplies `Value` and `Thunk`
- `eval.rs` drives closure calls through runtime nets, turns blocked bind-data
  pairs into stable call frames, and executes generic unary `HostFn` requests
  outside runtime locks; net-lowered builtins curry by returning another
  bind-wrapped HostFn and retain saturated work as memoized semantic thunks;
  contiguous application spines targeting nets share
  one evaluator-owned caller runtime and one generic bind spine; dictionary-
  access closure bodies temporarily retain the call-by-need compatibility path
  pending cross-copy demand forwarding; closed net values attach their exposed
  ports through logical-copy cursors and may normalize to either data or a
  non-data net frontier
- `main` expects binary `asm.result`, writes to `stdout`

At the moment, even this simple case is not fully implemented. Thus, it remains the focus for now.

The current interaction-net slice establishes the lambda-to-shared-net boundary
without exposing syntax. Templates use local fan sites and each runtime graph
gets one fresh namespace. The current oracle records dynamic duplication paths
directly; it provides reference semantics for replacing those histories with
Lamping-style bracket/croissant control interactions. Core bind-data calls,
closure capture, builtin currying, and lazy list construction now cross the net
runtime boundary. Cross-copy access demand and general construction effects
still belong before adding the `interaction_net` keyword.
