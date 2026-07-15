
# Architecture Sketches

## Simplest Case

        glam --file source.g

- `main.rs` parses CLI, initiates ops
- `main.rs` prepares a compile-time context for the source
  - optional source path for local-import loading
  - prior module value for future mixin-style compilation
  - abstract module path for namespace-relative identities such as `abstract_global_path`
- `g_syntax.rs` parses file.g through compile-time context, sourcing bytes and reporting diagnostics there
- `g_syntax.rs` lowers AST through compile-time context and a core-facing
  interface; source lambdas remain syntax, and update sugar is rewritten before
  semantic lowering
- `main.rs` applies one temporary top-level fixpoint to the anonymous assembly module
- `core_net.rs` lowers an explicit `(arity, body)` directly to one bind chain
  and shared runtime carrying `CoreNetData`; free locals become leading capture
  binds that the enclosing semantic expression supplies once. Calls lazily copy
  the runtime frontier through evaluator-only remote cursors. Functions that
  can cross only data-only HostFn/dictionary boundaries retain the explicitly
  transitional core lambda/closure path
- `interaction_net.rs` provides generic `InteractionNet<Data>` topology,
  checked construction through one `NetBuilder` (including fallible
  wiring/finalization and balanced copy helpers), active-pair discovery, and
  mutable runtime reduction; builder-only one-output copy tunnels are spliced
  out before a template is produced;
  runtime nodes use monotonic IDs and hash-table storage, preserve a stable
  exposed interface, and allocate fan sites locally; an active pair is keyed by
  its lower node ID, with one ordered tree recording ready, claimed, cursor-
  blocked, and stuck states; claimed cursor and host work can release the
  runtime mutex without surrendering pair ownership; layered cursors expose a precise
  local cursor, source cursor, or source pair dependency instead of scanning or
  sweeping scheduler collections; nodes materialize only through principal
  frontiers, active pairs never cross cursor boundaries, and source-frontier
  inspection never nests target/source locks; per-copy frontier cursors are the
  only port provenance, embedded data is cloned without transformation, and
  fan sites are translated per logical copy
- `list.rs` provides compact byte leaves, generic value leaves, finger-tree
  ropes, and opaque lazy holes; `core::List` supplies `Value` and `Thunk`
- `eval.rs` no longer constructs lambda expressions for its helper functions;
  it requests function lowering from `core_net`. It still evaluates the
  compatibility closures selected at the construction boundary, implements
  generic callable-data policy for target-local blocked bind-data pairs, and
  executes generic unary `HostFn` requests
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
Lamping-style bracket/croissant control interactions. Builtin currying, closed
list construction, and general application bodies now cross the net runtime
boundary. Callable data is claimed and lowered immediately without touching its
argument: nets load through cursors, while builtins lower to Bind/HostFn
topology. Cursor erasure uses ordinary materialization and Erase interactions;
no erased frontier state or mapped-node history is required. General
construction effects still belong before adding the `interaction_net` keyword.
