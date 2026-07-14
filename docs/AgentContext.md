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
  closed lambda spines with no captures, nested function values, or accesses.
  `g_syntax` constructs a multi-parameter lambda through one batched compiler
  call, so intermediate semantic lambda wrappers do not each prepare a net.
  Captured, nested-dependent, and access-containing lambdas deliberately retain
  `Value::Closure`, capture mapping, and expression evaluation.
- Lambda templates contain `Bind`, binary `Fan`, `Erase`, and `Data` nodes.
  The generic topology lives in `interaction_net.rs`; core data and expression
  lowering live in `core_net.rs`.
  `NetBuilder` is the single checked construction layer: it provides semantic
  bind/data/copy helpers plus fallible wiring/finalization diagnostics. A
  one-output copy is a builder-only tunnel normalized to a direct wire; it is
  never stored in a template or runtime net.
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
- Principal-principal connections appear in exactly one scheduler collection:
  ready, blocked call, blocked remote cursor, or stuck. Reduction results retain
  their `ActivePair`; calls also identify their bind and data nodes for later
  completion.
- Runtime instantiation wires the template's exposed port to a stable,
  evaluator-only interface anchor. A `RemoteCursor { copy, remote }` is a
  one-way suspended wire: `copy` selects target-owned shared copy state and
  `remote` identifies an interface or migrated auxiliary port in the source.
  A cursor materializes a node only when that source frontier faces its
  principal port. If it faces an auxiliary, one conservative sweep reduces
  every pair that was ready at the start of the sweep. Copying a partially
  applied net may encounter a cursor in the intermediate source; demand advances
  that intermediate cursor outward toward its own source, then retries. It does
  not reverse cursor flow or give an inner source a caller reference.
- A blocked bind-data pair can be consumed as a generic `CallFrame`; its
  argument and result survive behind independently stable interfaces. Core
  thunks may name one of those runtime/interface pairs, and memoize both values
  and errors without introducing a new language-level `Value` variant.
- The core runtime driver interprets closures and builtins outside the generic
  topology reducer. Partial builtins retain shared lazy arguments. Saturated
  builtins become memoized semantic thunks, so the conservative active-pair
  sweep does not force strict builtin work until its result is observed. An
  imported runtime that still exposes an unsupplied bind may not detach lazy
  call arguments yet; later parameters in the same bind spine are not captures.
- List applications lower to callable core data and computed list elements
  become opaque lazy holes. Access applications also have semantic thunk
  support, but closure bodies containing access currently remain on the
  compatibility evaluator: nested logical copies still need an explicit way to
  forward demand to the caller-side argument frontier.
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
