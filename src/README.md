
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
  exposed interface, and allocate fan sites locally; an active pair is keyed by
  its lower node ID, with ready work in an ordered set and suspended/stuck work
  in exact keyed maps; claimed cursor and host work can release the runtime
  mutex without surrendering pair ownership; layered cursors expose a precise
  local cursor, source cursor, or source pair dependency instead of scanning or
  sweeping scheduler collections; nodes materialize only through principal
  frontiers, active pairs never cross cursor boundaries, and source-frontier
  inspection never nests target/source locks; per-copy frontier cursors are the
  only port provenance, embedded data is cloned without transformation, and
  fan sites are translated per logical copy
- `list.rs` provides compact byte leaves, generic value leaves, finger-tree
  ropes, and opaque lazy holes; `core::List` supplies `Value` and `Thunk`
- `eval.rs` keeps compatibility closures on semantic evaluation, turns target-
  local blocked bind-data pairs into stable call frames, and executes generic unary `HostFn` requests
  outside runtime locks; HostFn failures become permanently stuck pairs rather
  than an underspecified retry state; net-lowered builtins curry by returning
  another bind-wrapped HostFn and retain saturated work as memoized semantic thunks;
  contiguous application spines targeting `Value::Net` share one evaluator-
  owned caller runtime and one generic bind spine; List and Access lower through
  HostFn chains, with embedded lazy list values stored as values rather than
  exported runtime-backed holes; the core HostFn boundary rejects returned
  lists containing structural holes; closed net values attach their exposed
  ports through logical-copy cursors and may normalize to either data or a non-
  data net frontier
- `main` expects binary `asm.result`, writes to `stdout`

At the moment, even this simple case is not fully implemented. Thus, it remains the focus for now.

The current interaction-net slice establishes the lambda-to-shared-net boundary
without exposing syntax. Templates use local fan sites and each runtime graph
gets one fresh namespace. The current oracle records dynamic duplication paths
directly; it provides reference semantics for replacing those histories with
Lamping-style bracket/croissant control interactions. Builtin currying and
closed list construction now cross the net runtime boundary. General
application bodies remain on compatibility evaluation until logical copies
retain an erased frontier outcome long enough for a later cursor at the other
end of the source wire to converge. That state and general construction effects
still belong before adding the `interaction_net` keyword.
