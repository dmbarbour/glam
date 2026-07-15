
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
  binds that the enclosing semantic expression supplies once. Core stores the
  result as `FunctionCode`, while each evaluated `FunctionValue` names a shared
  curried runtime stage. Calls lazily copy the runtime frontier through
  evaluator-only remote cursors
- `interaction_net.rs` provides generic `InteractionNet<Specialization>`
  topology. A specialization supplies cloneable `Data` and unary `Operator`
  values plus the rules for callable data and `Operator >< Data`;
  checked construction through one `NetBuilder` (including fallible
  wiring/finalization and balanced copy helpers), active-pair discovery, and
  mutable runtime reduction; builder-only one-output copy tunnels are spliced
  out before a template is produced;
  runtime nodes use monotonic IDs and hash-table storage, preserve a stable
  exposed interface, and allocate fan sites locally; an active pair is keyed by
  its lower node ID, with one ordered tree recording ready, claimed, cursor-
  blocked, and stuck states; claimed cursor and operator work can release the
  runtime mutex without surrendering pair ownership; layered cursors expose a precise
  local cursor, source cursor, or source pair dependency instead of scanning or
  sweeping scheduler collections; nodes materialize only through principal
  frontiers, active pairs never cross cursor boundaries, and source-frontier
  inspection never nests target/source locks; per-copy frontier cursors are the
  only port provenance, embedded data is cloned without transformation, and
  fan sites are translated per logical copy
- `list.rs` provides compact byte leaves, generic value leaves, finger-tree
  ropes, and opaque lazy holes; `core::List` supplies `Value` and `Thunk`
- `eval.rs` contains no lambda or closure representation. Source functions are
  ordinary, observable `Value::Function` data; partial application derives and
  shares another curried runtime stage. Saturated calls are memoized thunks.
  Source-level application lowers through a data-consuming `CoreOperator`, while raw
  `Value::Net` remains the explicit callable-data/cursor path. The evaluator
  implements generic callable-data policy for target-local blocked bind-data
  pairs and executes generic unary operator requests
  outside runtime locks; operator failures become permanently stuck pairs rather
  than an underspecified retry state; net-lowered builtins curry by returning
  another bind-wrapped operator and retain saturated work as memoized semantic thunks;
  contiguous application spines are represented by one semantic operator chain;
  function stages attach all presently available arguments together. List,
  dictionary, and Access construction lower through operator chains, with lazy
  aggregate members represented as closed value/computation thunks rather than
  exported runtime-backed holes; closed net values attach their exposed
  ports through logical-copy cursors and may normalize to either data or a non-
  data net frontier
- `main` expects binary `asm.result`, writes to `stdout`

At the moment, even this simple case is not fully implemented. Thus, it remains the focus for now.

The current interaction-net slice removes lambdas and closures from core while
keeping lambda syntax in `g_syntax`. Templates use local fan sites and each runtime graph
gets one fresh namespace. The current oracle records dynamic duplication paths
directly; it provides reference semantics for replacing those histories with
Lamping-style bracket/croissant control interactions. Builtin currying, closed
list construction, and general application bodies now cross the net runtime
boundary. Callable data is claimed and lowered immediately without touching its
argument: nets load through cursors, while builtins lower to Bind/Operator
topology. Cursor erasure uses ordinary materialization and Erase interactions;
no erased frontier state or mapped-node history is required. General
construction effects still belong before adding the `interaction_net` keyword.
