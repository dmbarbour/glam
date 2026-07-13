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
- Each `core::Lambda` owns a once-initialized interaction net. Closure creation
  reuses that template and captures only its environment; applying a closure
  must not re-lower its body. Runtime reconstruction may copy topology with new
  node IDs. Nested lambdas stay unlowered until reached.
- Lambda templates contain `Bind`, binary `Fan`, `Erase`, and `Data` nodes.
  The generic topology lives in `interaction_net.rs`; core data and expression
  lowering live in `core_net.rs`.
  Fan sites are local to a template; one process-global `InstanceId` qualifies
  the entire runtime instance. Fan pairing goes through an oracle and includes
  dynamic duplication history rather than comparing permanent global UIDs.
- `HistoryOracle` is a correctness-oriented direct-history implementation and
  the reference semantics for later Lamping bracket/croissant control nodes; it
  is not itself the final local, optimal oracle.
- The topology reducer implements bind/fan join, fan commutation, duplication,
  and erasure rules. Core
  evaluation still has a compatibility bridge while `bind-data` calls are being
  moved to demand-driven net evaluation. Complete that before exposing the
  `interaction_net` keyword.

### Configuration Fixtures

- `GLAM_CONF` defaults to `samples/config/dev.g` in workspace container
- provide test utility functions via `conf.env` within configuration file
  - configurations may import from a shared utility config
- specialized configurations for testing features like configurable logging
