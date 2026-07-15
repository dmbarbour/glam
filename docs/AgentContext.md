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
- Core lists alias `list::List<Value, Thunk>`. `list.rs` preserves `Bytes` as
  compact leaves and treats thunks as opaque lazy holes; evaluator code supplies
  forcing and converts individual observed bytes to core number values.
- Each `core::Lambda` owns a once-initialized shared runtime net. Closure
  creation reuses it and captures only its environment; applying a closure must
  not re-lower its body. Logical copies materialize nodes lazily through remote
  cursors. A maximal leading curried spine such as `\x y z -> ...` lowers to
  one net with a bind chain; nested function values inside the final body stay
  unlowered until reached.
- `Value::Net` is a first-class closed net containing only a
  `SharedRuntimeNet<CoreNetData>`. Observing it may produce ordinary data or
  preserve a non-data normal-form net; applying it attaches the exposed port
  through a logical-copy cursor. `CompileContext::value_net` is the checked
  Rust construction entry point and discards the immutable template after
  instantiation.
- As an incremental syntax-to-net transition, `CompileContext` precompiles
  closed lambda spines with no captures, nested function values, accesses, or
  general application bodies.
  `g_syntax` constructs a multi-parameter lambda through one batched compiler
  call, so intermediate semantic lambda wrappers do not each prepare a net.
  Captured, nested-dependent, and access-containing lambdas deliberately retain
  `Value::Closure`, capture mapping, and expression evaluation.
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
  endpoint is recovered from that node's principal neighbor. Ready work uses an
  ordered set and suspended/stuck states use keyed maps, so exact completion and
  cursor demand never search or remove from the middle of a queue. Calls also
  identify their participating nodes for completion. Stuck pairs retain either
  a no-rule reason or a permanent host error.
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
  that peer is claimed.
- A blocked bind-data pair can be consumed as a generic `CallFrame`; its
  argument and result survive behind independently stable interfaces. Core
  thunks may name one of those runtime/interface pairs, and memoize both values
  and errors without introducing a new language-level `Value` variant. A
  `Data >< Bind` and every other source active pair remain source dependencies;
  active pairs are never copied across a cursor boundary.
  `compatibility_call_argument_data` remains a target-local evaluator bridge
  and the sole content inspection through an ordinary auxiliary port.
- `HostFn<Data>` is a generic unary agent with a principal data input and one
  result auxiliary. Its active pair is claimed while its callback runs outside
  the runtime mutex. The callback either emits `Data`, emits another
  automatically bind-wrapped `HostFn`, or leaves the pair permanently stuck
  with a diagnostic; there is no retryable blocking state. Core builtin
  expressions lowered into nets use HostFn currying. Saturated builtins still emit
  memoized semantic thunks, so unrelated exact source-pair progress does not
  force strict builtin work until its result is observed. Direct evaluator
  builtin values retain the compatibility path. A runtime that still exposes
  an unsupplied bind may not detach lazy call arguments yet; later parameters
  in the same bind spine are not captures.
- `g_syntax` and `CompileContext::value_apply_many` preserve maximal
  left-associated application spines such as `f x y z`. The expression
  evaluator peels such a spine before evaluation and supplies all remaining
  arguments through one caller runtime only when the callable is `Value::Net`.
  Closures remain on the semantic compatibility evaluator until they can be
  represented as genuinely closed nets.
- List and Access applications lower to HostFn chains rather than callable
  `Data`. A list HostFn accepts only embedded values and stores lazy values as
  ordinary list values; it never exports a runtime/interface-backed list hole.
  The core HostFn boundary rejects any returned `Value::List` that already
  contains a structural lazy hole, leaving the call permanently stuck.
  General application bodies are temporarily excluded from automatic closed-
  net preparation because cycles need persistent per-port copy provenance
  after imported nodes reduce away.
- Preserve the current dictionary/access compatibility evaluator while a
  persistent lazy dictionary representation is designed separately.
- The topology reducer implements bind/fan join, fan commutation, duplication,
  and erasure rules. Core
  evaluation retains only the access-related compatibility bridge described
  above. Complete that before exposing the `interaction_net` keyword.

### Configuration Fixtures

- `GLAM_CONF` defaults to `samples/config/dev.g` in workspace container
- provide test utility functions via `conf.env` within configuration file
  - configurations may import from a shared utility config
- specialized configurations for testing features like configurable logging
