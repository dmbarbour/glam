# Agent Context

This file records current implementation constraints that are easy to regress.
Keep it short: replace obsolete notes instead of appending a history. Put
detailed subsystem invariants in `docs/agent_context/` and target language
design in the design documents.

## Navigation

- [`src/README.md`](../src/README.md) maps current modules and end-to-end
  control flow.
- [`agent_context/interaction_nets.md`](agent_context/interaction_nets.md) is
  the authoritative interaction-net implementation note.
- [`agent_context/objects.md`](agent_context/objects.md) records current object
  representation, scope, and linearization rules.
- [`DistilledDesign.md`](DistilledDesign.md) describes the intended language.
  It is not evidence that a feature is implemented.
- [`SyntaxCheatSheet.md`](SyntaxCheatSheet.md) is the compact target syntax
  reference. Confirm current acceptance with the parser and samples.

## Working Rules

- The executable is a bootstrap shell, not the final command model.
- Prefer small checked-in samples to ad hoc `/tmp` programs, and cover useful
  samples with tests.
- Treat valid and invalid sample files as executable syntax specifications.
- Prefer source spans and diagnostics to panics for user-facing failures.
- Add a focused regression test when a design constraint becomes executable.
- Use Chumsky for growing `.g` grammar work. Hand-written parsing is appropriate
  for source layout and other small normalization passes when it is clearer.

## Boundary Invariants

### Front end

- `.g` syntax, lexical scope, capture discovery, and syntax sugar belong to
  `g_syntax`. Do not move syntax nodes or lambda concepts into `core` or `eval`.
- `ResolvedExpr<Value>` is an affine front-end IR. Lower it once into closed
  values and interaction nets; cloning it risks lowering the same work twice.
- Lambdas and applications are front-end constructs. A complete source
  function lowers to one bind spine, with free locals supplied as leading
  capture binds. Core has no expression, lambda, or closure representation.
- Front ends receive raw source bytes separately from `CompileContext`; the
  built-in `.g` compiler explicitly validates UTF-8. Source paths remain
  assembler-owned provenance used by import and diagnostic handlers.
- `CompileContext` supplies module capabilities, values, builtins, loaders,
  and diagnostic emission. It is not an expression-building DSL and must not
  acquire lambda/application helpers.
- Front ends pass relative names to `CompileContext::abstract_global_path` and
  relative import requests to `import_module`/`import_binary`. Absolute module
  paths and importer provenance remain handler-owned and must not be exposed
  back to the front end.
- Diagnostic severity is a front-end emission-effect argument, not a field the
  assembler discovers by evaluating the message. Before dispatch, the
  assembler mixes authoritative `msg.severity` and `msg.origin` fields into
  the message and composes that mixin into the resulting object `spec`.

### Values and evaluation

- `Value::Net` is an explicit first-class closed net. `Value::Lazy` is a
  memoized computation whose net-backed forms must expose `Data` when forced;
  an exposed `Bind` is an error, not an implicit function conversion.
- `Value::Function` is an independently observable curried function stage.
  Partial application shares the staged runtime; saturated application returns
  a memoized computation.
- `list.rs` owns compact persistent list ropes. Keep `Bytes` as compact leaves;
  lazy holes are opaque to the list and are forced only through evaluator-owned
  operations.
- The current dictionary/access evaluator is compatibility code. Preserve its
  behavior until a first-class persistent lazy dictionary design replaces it.
- Pending lazy cells currently fail if observed before assignment. Parallel
  evaluation will need thunk scheduling and continuations; do not turn this
  temporary fail-fast rule into a blocking join without that design.

### Interaction nets

- `interaction_net` owns generic topology and reduction,
  `core_net` supplies core data/operator semantics, and `g_syntax` owns
  front-end net emission.
- Every net value is closed except for exactly one exposed port. Nets compose
  through one-way logical-copy cursors; captures are not remote back-references.
- Only principal-principal active pairs reduce. A source active pair never
  crosses a cursor boundary. Materialization happens only when a demanded
  cursor reaches a stable source principal frontier.
- External callable/operator/cursor work claims an exact active pair, releases
  the runtime lock, then completes or updates that pair. Do not scan scheduler
  collections or hold source and target runtime locks together for progress.
- `NetBuilder` is the one checked construction layer used by compiler lowering
  and the public `Assembler::net` facade. Do not introduce a second write-only
  construction IR unless it adds observable semantics.

See the focused [interaction-net invariants](agent_context/interaction_nets.md)
before changing fan identity, active-pair bookkeeping, cursor provenance, or
runtime locking.

### Objects

- Objects are dictionaries containing `spec:{name,deps,defs}`. Object bodies
  lower in their own final/prior self scope, never ordinary module scope.
- Ordinary object-body names resolve through final self; `_name` resolves
  through the prior definitions. An explicit alias keeps ordinary names in the
  parent scope and names the object self separately.
- Object dependency order uses C3 linearization. Named specs deduplicate by
  name; anonymous specs remain distinct and must precede named dependencies.

See [object invariants](agent_context/objects.md) for the current lowering and
bootstrap compatibility behavior.

## Public Facade and CLI

- The embedding API keeps `Value` opaque. Clients explicitly call
  `Assembler::evaluate` or `Assembler::apply`; accessors do not silently drive
  evaluation.
- Public numbers use canonical exact text, finite `f64`, `i64`, or small
  `(i64, i64)` ratios rather than exposing the backing big-number crates.
- Binary extraction accepts compact binaries and byte-valued lazy lists, and
  ranged extraction forces only the required evaluator-owned list work.
- `Assembler::net` exposes only scoped ports plus `bind`, balanced `copy`,
  `data`, checked `wire`, and exposed-port selection. Runtime nodes, cursors,
  schedulers, and fan histories remain internal.
- `main` builds configuration and assembly as ordinary caller-selected module
  roots. The library API does not assign those names or roles.
- The current CLI accepts repeated `--file`/`-f` and
  `--script.<ext>`/`-s.<ext>` assembly inputs, then optional arguments after
  `--`. It also has a temporary `--parse` inspection path. Bare arguments and
  configured `conf.cli` rewriting are not implemented.
- Configuration paths come from `GLAM_CONF` path list, or an OS-specific default

## Source-Surface Regressions

- A `.g` source starts with a language declaration such as `language g0`.
- Comments begin with `#`. Top-level declarations are unindented; continuation
  lines are indented, except a closer-only line may dedent.
- Introduction, override, and update remain distinct: `=`, `:=`, and `::=`.
- Multiline `let`/`where` bindings align under the first binding. Multiline
  forms do not accept `in`; keep the valid and invalid samples synchronized
  with parser tests.
