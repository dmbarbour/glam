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
  back to the front end. Local requests are portable child paths: reject
  absolute paths, backslashes, empty components, and `.`/`..` or other
  dot-prefixed components. Top-level host inputs such as `--file` and
  `GLAM_CONF` are exempt because the caller, not a front end, supplies them.
- Diagnostic severity is a front-end emission-effect argument, not a field the
  assembler discovers by evaluating the message. Diagnostic sinks receive a
  raw envelope containing the original emission, severity, and hidden
  assembler provenance. Observers explicitly call `Diagnostic::enrich` (or
  `enrich_with`) to mix authoritative `msg.severity` and `msg.origin` into an
  independent object view whose `spec` records each mixin. The assembler does
  not render diagnostics.
  `msg.origin.source` is tagged (`file:Path`, `script:Bytes`, and future source
  kinds), while `invocation` is a fresh assembler-local compilation ID.
  `namespace` is the globally qualified definition namespace. `import_chain`
  contains root-to-parent `{importer,request,extends}` edges; local requests are
  tagged `file:RelativePath`, and `extends` is relative to the importer namespace.
  Provenance must not retain module values or compilation environments.
- The executable's default logger is one diagnostic observer. It applies
  assembler enrichment, adds terminal context under `viewer`, and renders a
  compact text view. More elaborate logging and IDE policy belongs to glam
  configuration, not to the assembler library.

### Reflection effects

- `reflection::run` interprets freer effects outside interaction-net
  evaluation. Generic request operators only construct singleton dictionaries
  tagged by host-only abstract-global atoms.
- The standard task machine provides `r`, `seq`, `alt`, `fail`, `cut`, `fix`,
  `get`, `set`, `reset`, and `shift`. `TaskSpecialization` contributes an API
  fragment, private request tags, and specialization-owned transaction data.
  Reusable request families compose by mapping their requests into the host
  specialization's request enum. The first reflection family contributes
  `log Severity Message`; `main` adds provisional `read_log` and `write_stderr`
  effects. Logged diagnostics join the current transaction, including
  read-your-writes behavior, or go directly to the host outside `cut`. A
  top-level `alt` is rejected; alternatives belong to `cut`. This and
  task-local `.shift` continuations are the conservative
  standard-effect contract: general-purpose utilities must not assume broader
  behavior, although a specialized handler may explicitly provide it.
- Every `get`/`set` path is implicitly under user-owned `user_state`. The
  ordinary `heap` subtree is shared through the host transaction snapshot.
  The active reset stack lives under a private key in that same state, so a
  whole-state value carries its delimited-continuation environment between
  cooperative threads. Root replacement may therefore replace or corrupt the
  reset stack; those consequences belong to the user. Immediate sequence and
  fixpoint bookkeeping, choice search, transaction journals, and host queues
  remain task-owned. Opaque captured continuations are valid only within the
  reflection task that created them; a process-global `u64` task ID makes
  foreign invocation fail before its task-local continuation ID is inspected.
- An outer `cut` snapshots the heap and specialization-owned resources. For the
  logger those resources are the diagnostic queue and buffered stderr. Failed
  alternatives discard state, reserved reads, and buffered writes; successful
  nested cuts merge upward; an outer success validates and commits. The
  bootstrap is currently serial and uses a coarse generation, but the boundary
  is shaped for finer optimistic observations later.
- Each `fix` alternative receives its own pending future. When a chosen result
  later fails, the handler restarts at the fixpoint boundary and replays its
  transactional `alt` choices rather than reusing an initialized future.
- `main` owns the diagnostic queue, logging request dispatcher, and logging
  transaction snapshot/journal. Defined `conf.log` consumes enriched messages
  effectfully; undefined, completed, or failed custom logging falls back to the
  Rust terminal logger. Stderr effects commit to a host buffer before bytes are
  written to the OS.

### Values and evaluation

- Production evaluation consumes only closed `Value`s. The small fixture IR in
  `eval/test_support.rs` must lower to interaction nets before evaluation; do
  not restore a second expression interpreter or thread a local environment
  through evaluator APIs.
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
