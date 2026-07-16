
# Architecture Sketches

## Simplest Case

        glam --file source.g

- `main.rs` parses CLI arguments, delegates assembly to the public library
  facade, renders retained diagnostics, and writes output
- `api.rs` owns the public `Assembler`, opaque `Value`, module builder, host
  capabilities, and bounded diagnostic history. The facade exposes exact
  numbers through canonical text or small integer ratios, lossy finite `f64`
  conversion, lazy function application/evaluation, and ranged extraction from
  compact binaries or byte-valued lists without exposing core number or list
  representations
- `api.rs` prepares an internal compile-time context for each source
  - optional source path for local-import loading
  - prior module value for future mixin-style compilation
  - abstract module path for namespace-relative identities such as `abstract_global_path`
- `g_syntax.rs` parses file.g through compile-time context, sourcing bytes and reporting diagnostics there
- `g_syntax.rs` resolves syntax into its own affine `ResolvedExpr<Value>` IR,
  then consumes that IR directly into closed shared interaction nets; source
  lambdas remain front-end syntax, and update sugar is rewritten before net
  emission
- `api.rs` applies one temporary top-level fixpoint to a caller-named root module;
  `main.rs` chooses `configuration` and `assembly` for its two CLI modules
- `core_net.rs` defines the syntax-independent `CoreOperator` and
  `CoreSpecialization` carried by generic interaction nets. The core
  specialization embeds `Value` directly as its data type.
  The `g_syntax` emitter builds one bind chain per resolved function; free
  `BindingId`s become leading capture binds supplied once by the enclosing net.
  Core stores the result as `FunctionCode`, while each evaluated
  `FunctionValue` names a shared curried runtime stage. Calls lazily copy the
  runtime frontier through evaluator-only remote cursors
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
  ropes, and opaque lazy holes; `core::List` supplies `Value` and `LazyValue`
- `eval.rs` contains no production expression, lambda, or closure
  representation. Source functions are
  ordinary, observable `Value::Function` data; partial application derives and
  shares another curried runtime stage. Saturated calls are memoized thunks.
  Source-level application lowers through a data-consuming `CoreOperator`, while raw
  `Value::Net` remains the explicit callable-data/cursor path. The evaluator
  implements generic callable-data policy for target-local blocked bind-data
  pairs and executes generic unary operator requests
  outside runtime locks; operator failures become permanently stuck pairs rather
  than an underspecified retry state; net-lowered builtins curry by returning
  another bind-wrapped operator and retain saturated work as memoized semantic thunks;
  contiguous application spines (and direct lambda applications) are
  represented by one semantic operator chain;
  function stages attach all presently available arguments together. List,
  dictionary, and Access construction lower through operator chains, with lazy
  aggregate members represented as closed value/computation thunks rather than
  exported runtime-backed holes; closed net values attach their exposed
  ports through logical-copy cursors and may normalize to either data or a non-
  data net frontier
- the public facade extracts binary `asm.result`; `main` otherwise uses only
  this facade and writes the result to `stdout`. The temporary `--parse`
  inspection command remains the sole direct front-end API client pending a
  reflection replacement

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
no erased frontier state or mapped-node history is required. Net-backed lazy
computations and saturated ordinary function calls require an
exposed `Data` result; partial function stages explicitly require `Bind`, while
explicit `Value::Net` application may retain a residual bind-exposing net.
Construction effects still belong before adding the `interaction_net` keyword.
`CompileContext` deliberately has no expression-building compatibility DSL;
front ends own their semantic IR and return values or checked closed nets.
