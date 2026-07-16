# Agent Context

This document summarizes high-signal, task-relevant constraints for coding agents. Agent should add points when corrected on a matter, mark points for removal when they become irrelevant, and generally maintain this file.

This document should summarize salient, relevant points rather than asking future agents to read huge design docs. If the summary would be more than a paragraph, agents can maintain extended summaries in the `agent_context/` subfolder. 

## Source Layout

- `src/g_syntax.rs` - initial front-end compiler for ".g" syntax
- `src/core.rs` - assembly-time representations, independent of syntax
- `src/core_net.rs` - lowering from core expressions to core-data interaction nets
- `src/eval.rs` - efficient reduction of core terms
- `src/interaction_net.rs` - generic interaction-net topology and reduction
- `src/list.rs` - generic compact, lazy, persistent list ropes
- `src/main.rs` - CLI parsing, integration
- `src/numbers.rs` - wrapper for big-rationals
- `src/README.md` - rough sketch of architecture

## Todo

- parser
- data types
- evaluator
- imports - local, remote
- unique atoms
- CLI macros
- tab completion
- logging
- interactive mode
- accelerators (many!)

## Notes

- The executable is a temporary bootstrap shell, not the final command model.
- use `samples/` files for smoke checks (instead of `/tmp`).
  - keep samples small and purpose-specific
  - cover samples with tests 
- When configuration is needed, use `GLAM_CONF` with a checked-in fixture
- Implement in spikes where feasible, i.e. where a feature has observable output.
- Prefer spans and diagnostics over panics for source-facing behavior.
- Tests should pin design constraints as soon as they become executable.
- Use Chumsky for growing `.g` grammar work. Keep hand-written parsing limited
  to small source-normalization steps where that is clearer than grammar code.
- Object implementation notes live in `docs/agent_context/object_spike.md`.

### Command Line

- Add inspection or reflection commands as needed, e.g. `glam --parse`.
  - '-' prefix is required for built-ins, otherwise `conf.cli` rewrites
- Final assembly inputs are expected to be expressed with options such as
  `--file`, `--script`, and `--`.
- Current spike supports `glam (-f|--file) PATH` for one source file and writes
  `asm.result` to stdout by default.

### Source Surface

- A `.g` file should start with a language version declaration, e.g.
  `language g0`.
- Initial character set is printable ASCII plus whitespace; UTF-8 is an
  extension.
- Comments are line comments beginning with `#`.
- Toplevel declarations begin on unindented lines. Continuation lines are
  indented, except a final closer-only line may dedent.
- Introductions and overrides are distinct: `name = Expr`, `name := Expr`, and
  `name ::= Update`.

### Eval

- Keep syntax parsing separate from syntax-independent evaluation.
- Evaluation should consume core terms/values, not `.g` syntax nodes directly.
- Core dictionaries use explicit `Key` values. `.g` paths lower through
  interned atom keys, not text keys. 
- Core lists alias `list::List<Value, LazyValue>`. `list.rs` preserves `Bytes` as
  compact leaves and treats thunks as opaque lazy holes; evaluator code supplies
  forcing and converts individual observed bytes to core number values.
- `Value::Lazy` is the single suspension representation for deferred producers,
  externally fulfilled fixpoint cells, net/builtin/access/function-call work,
  and memoized failures. Pending cells currently fail if observed before being
  set; parallel evaluation will require thunk-level sparks and suspended
  continuations rather than a blindly blocking cell join.
- `core_net` accepts `(arity, body)` and produces `FunctionCode` containing one
  shared runtime with one bind chain. Locals outside the arity become leading
  capture binds. Evaluating the semantic `Expr::Function` supplies those
  captures once and produces an observable `Value::Function` whose current
  curried stage is another shared runtime. Logical copies materialize nodes
  lazily through remote cursors.
- `Value::Net` is a first-class closed net containing only a
  `SharedRuntimeNet<CoreSpecialization>`. Observing it may produce ordinary data or
  preserve a non-data normal-form net; applying it attaches the exposed port
  through a logical-copy cursor. `CompileContext::value_net` is the checked
  Rust construction entry point and discards the immutable template after
  instantiation.
- Lambdas exist only as syntax in `g_syntax`. `CompileContext` lowers a complete
  syntactic function, including its explicit lifted captures, without creating
  a core lambda or closure. Update-definition parameter sugar is likewise
  rewritten while still syntax. Core has no lambda/closure compatibility path.
- Lambda templates contain `Bind`, binary `Fan`, `Erase`, and `Data` nodes.
  The generic topology lives in `interaction_net.rs`; core data and expression
  lowering live in `core_net.rs`.
  `NetBuilder` is the single checked construction layer: it provides semantic
  bind/data/copy helpers plus a curried `bind_spine`, and fallible
  wiring/finalization diagnostics. A one-output copy is a builder-only tunnel
  normalized to a direct wire; it is never stored in a template or runtime net.
  Fan sites are `u64` values local to a runtime. Each logical copy translates
  source sites through a per-copy map into fresh target-local sites. Fan
  identities include dynamic duplication history; identical complete histories
  join and other fans commute.
- The direct-history fan representation is correctness-oriented. Replacing it
  with Lamping bracket/croissant control nodes requires replacing fan identity
  construction and rewrite rules together, not implementing an oracle hook.
- Runtime nets use monotonically allocated `u64` node IDs backed by a hash
  table. Ports pack a node ID and two-bit port index into one nonzero word; each
  node stores three inline links. IDs are not reused. Erase
  interactions and other rewrites remove nodes explicitly rather than relying
  on reachability collection.
- A principal-principal connection is keyed by its lower `NodeId`; the other
  endpoint is recovered from that node's principal neighbor. One ordered active-
  pair tree records every pair as ready, claimed, cursor-blocked, or stuck.
  External work claims a pair by changing its state in place while holding the
  runtime mutex; completion removes or updates that same entry. Stuck pairs
  retain either a no-rule reason or a permanent host error.
- Runtime instantiation wires the template's exposed port to a stable,
  evaluator-only interface anchor. A `RemoteCursor { copy, remote }` is a
  one-way suspended wire: `copy` selects target-owned shared copy state and
  `remote` identifies an interface or migrated auxiliary port in the source.
  A cursor materializes a node only when that source frontier faces its
  principal port. If it enters an auxiliary whose node belongs to an active
  pair, it records that exact lower-node pair key and reduces only that pair in
  the source. An inactive auxiliary frontier records the target-local cursor
  facing that node's principal and drives that cursor instead; it never copies
  the node through its auxiliary. Copying a partially applied net may instead
  encounter a cursor in the intermediate source; that exact cursor is driven
  transitively before the outer cursor retries. This does not reverse cursor
  flow or copy directly across an intermediate net. Cursor progress first claims the cursor pair,
  releases the target mutex, inspects the source frontier, then reacquires only
  the target mutex to finish. A converging cursor will not remove a peer while
  that peer is claimed. Per-copy `frontiers` are the authoritative reverse map
  from stable source ports to live local cursors; there is no historical
  source-node to target-node map. Embedded data is copied with `Clone`; only
  fan-site translation and frontier state vary per logical copy. `Erase ><
  RemoteCursor` has no shortcut: it demands and materializes through the cursor,
  after which the ordinary Erase rule handles the copied agent. When an
  auxiliary has no corresponding local principal cursor yet, dependency
  inspection follows the source principal chain to an exact active pair.
- A blocked `Data >< Bind` pair is resolved through the net's
  `NetSpecialization::callable` policy. `SharedRuntimeNet` claims its exact pair,
  releases the runtime lock, asks the specialization to produce either a shared
  net or `Operator`, then briefly
  reacquires the lock to install that topology or mark the pair stuck. Core
  implements the policy in `eval`; call and operator reductions are handled
  immediately rather than rediscovered by scanning scheduler collections. A raw
  `Value::Net` loads through a cursor without inspecting the argument. Builtins and partial
  builtins lower to an explicit unary `Bind` backed by `CoreOperator`, after which the
  ordinary bind-join rule applies. Dictionary applicables lower to the same
  shape using an applicable operator. Ordinary `Value::Function`
  application instead uses the semantic data-consuming operator described below.
  Source active pairs
  remain exact dependencies and are never copied across a cursor boundary; the
  evaluator no longer inspects an application argument through `Bind.aux1`.
- `Operator<S::Operator>` is a generic unary agent with a principal data input
  and one result auxiliary. Its active pair is claimed while
  `NetSpecialization::apply_operator` runs outside the runtime mutex. The rule
  either emits `Data`, emits another automatically bind-wrapped `Operator`, or
  leaves the pair permanently stuck with a diagnostic; there is no retryable
  blocking state. Core uses an explicit `CoreOperator` enum rather than opaque
  Rust closures. Saturated builtins still emit
  memoized semantic thunks, so unrelated exact source-pair progress does not
  force strict builtin work until its result is observed. Dynamically obtained
  builtin values also lower to the same Bind/Operator form; applicable lowering
  never detaches an argument from shared topology.
- `g_syntax` and `CompileContext::value_apply_many` preserve maximal
  left-associated application spines such as `f x y z`. Net lowering represents
  an ordinary application as a data-consuming operator chain whose operands are
  closed lazy values. When the head is a `Value::Function`, all currently
  available arguments attach to its stage together. An undersaturated call
  produces another shared `FunctionValue`; a saturated call produces a
  memoized function-call thunk. This preserves sharing when arguments trickle
  in without exposing linear binds as host functions.
- List, dictionary, and Access applications lower to operator chains rather than
  callable `Data`. Aggregate operators store lazy members as ordinary closed
  values/computation thunks; they do not turn network interfaces into list
  holes. Existing host-level structural list holes remain observable through
  the list evaluator.
- Preserve the current dictionary/access compatibility evaluator while a
  persistent lazy dictionary representation is designed separately.
- The topology reducer implements bind/fan join, fan commutation, duplication,
  and erasure rules. Core
  evaluation retains the applicable lowering bridge described above.
  Complete the remaining syntax transition before exposing the
  `interaction_net` keyword.

### Configuration Fixtures

- `GLAM_CONF` defaults to `samples/config/dev.g` in workspace container
- provide test utility functions via `conf.env` within configuration file
  - configurations may import from a shared utility config
- specialized configurations for testing features like configurable logging
