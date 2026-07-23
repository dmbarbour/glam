# Agent Context

This file is a short checklist of implementation boundaries that are easy to
regress. It is not an architecture guide or a development diary. Replace stale
notes instead of appending history; put subsystem details in
`docs/agent_context/` and current control flow in `docs/architecture/`.

## Where to Look

- [`src/README.md`](../src/README.md) is the compact source-module map.
- [`architecture/assembly.md`](architecture/assembly.md) follows sources,
  diagnostics, and CLI batch execution.
- [`architecture/evaluation.md`](architecture/evaluation.md) follows values,
  lazy work, nets, sessions, and workers.
- [`architecture/reflection.md`](architecture/reflection.md) explains the
  external effect machine and reflection-task lifecycle.
- [`agent_context/evaluation.md`](agent_context/evaluation.md),
  [`agent_context/reflection.md`](agent_context/reflection.md),
  [`agent_context/interaction_nets.md`](agent_context/interaction_nets.md), and
  [`agent_context/objects.md`](agent_context/objects.md) record detailed
  subsystem invariants.
- [`DistilledDesign.md`](DistilledDesign.md) describes intended language design,
  not necessarily implemented behavior.
- [`SyntaxCheatSheet.md`](SyntaxCheatSheet.md) is a target syntax reference;
  verify current acceptance against parser tests and samples.

## Working Rules

- Prefer narrow, testable slices and focused regression tests.
- Treat valid and invalid samples as executable syntax specifications.
- Prefer source spans and diagnostics to panics for user-facing failures.
- Use Chumsky for growing `.g` grammar work. Small hand-written layout or
  normalization passes are fine when clearer.
- `g_syntax/parser/lexical.rs` owns source-wide newline, whitespace, text,
  delimiter, indentation, and declaration-section recognition. Fatal lexical
  errors stop grammatical parsing. `parser/input.rs` is the only adapter from
  that one lexical result to token parsers; production parsers receive an
  existing `TokenView` and never re-lex substrings. `LayoutView` interprets
  `LineStart` tokens only at its current delimiter depth.
- Keep current implementation claims out of target-state design documents, and
  keep chronological spike notes out of this file.

## Cross-Layer Boundaries

### Front end

- `.g` syntax, lexical scope, capture discovery, and sugar belong to
  `g_syntax`. Core and evaluation have no expression, lambda, closure, or local
  environment representation.
- `ResolvedExpr<Value>` is affine front-end IR. Move it through one lowering;
  cloning it risks lowering and evaluating the same work twice.
- Definition targets retain parsed `SyntaxKeyExpr` paths through lowering.
  Never reconstruct or re-lex a target source fragment.
- A complete source function lowers to one bind spine, including leading binds
  for captures. Application spines lower together when possible.
- Front ends receive a `SourceArtifact`'s raw bytes separately from
  `CompileContext`. The built-in `.g` compiler validates UTF-8 itself. Source
  identity, digest, relative resolver, and importer provenance remain
  assembler-owned.
- `CompileContext` supplies source-scoped authority: relative loads,
  `abstract_global_path`, prior/final definitions, canonical unit, and
  diagnostic emission. Ordinary values and builtins are constructed directly
  by the front end; the context must not become an expression DSL.
- Front-end import requests and `abstract_global_path` components are relative.
  Reject absolute paths, backslashes, empty components, dot components, parent
  traversal, and other dot-prefixed components. Top-level paths supplied by
  the host CLI are a separate trust boundary.
- The built-in compiler's closed helpers and built-in modules are lowered once
  in `g_syntax/compiler_values.rs`. Per-module paths, environments, promises,
  and reflection tasks remain local.

### Diagnostics

- Severity is an argument to diagnostic emission, not something inferred by
  evaluating the message. A session bus publishes the original value plus
  hidden assembler provenance only after its transaction commits.
- The bus owns sequence numbers and coherent severity counts, never retention.
  External buffers, callbacks, `conf.log` input, and terminal rendering are
  independent subscriptions. `Assembler` drops events by default. Assembler
  and logger sessions have separate buses.
- An observer explicitly enriches that envelope with authoritative
  `msg.severity` and `msg.origin`; enrichment returns an independent object
  view. The assembler library neither renders nor prints diagnostics.
- Source origins are tagged values. Import provenance must not retain module
  values or compilation environments.
- The executable's default logger adds `viewer` context and applies the cached
  closed Glam `Diagnostic -> Bytes` formatter. Rust formatting is only an
  emergency fallback. See
  [`architecture/assembly.md`](architecture/assembly.md) for the logger
  lifecycle.

### Values and execution

- Production evaluation consumes closed `Value`s and always receives the
  caller's `EvalContext`. Deferred work must not capture the session that
  happened to construct it.
- Effects are freer-monad data interpreted by reflection tasks. Interaction-net
  reduction does not perform reflection state changes or external I/O.
- A net is closed except for one exposed port. Composition uses one-way logical
  copy cursors, never capture-like back-references.
- Only principal-principal active pairs reduce. Specialization work claims one
  exact pair and runs without holding a runtime lock; source and target net
  locks must never be nested.
- Core dictionary applicability is compatibility code. Preserve it until the
  persistent lazy dictionary design replaces it.

Use the focused evaluation, reflection, interaction-net, and object notes
before changing these subsystems; the top-level summary deliberately omits
their detailed scheduling and representation contracts.

## Public Facade and CLI

- The embedding API keeps `Value` opaque. Clients explicitly evaluate or apply;
  accessors do not silently drive arbitrary computation.
- Public number conversion exposes canonical text, finite `f64`, `i64`, and
  small ratios rather than the backing big-number crates.
- Binary extraction accepts compact binaries and byte-valued list elements. It
  must not flatten nested binary/list values such as `["A", 10, "B"]`.
- `Assembler::net` is a scoped facade over the one checked `NetBuilder`; runtime
  nodes, cursors, schedulers, and fan histories stay internal.
- `AssemblerBuilder` fixes source authority, runtime, conflict strategy, and
  reflection environment before creating one live reasoning session. Its
  environment closure may create session-bound protected volumes. Do not add
  fluent `Assembler` methods that silently replace the session.
- A completed assembler has one immutable `SourceSystem`. Relative imports use
  the resolver carried by their loaded artifact; diagnostic origin records the
  SHA-256 digest of the exact bytes given to the front end.
- `main` chooses the `configuration` and `assembly` roots. The library assigns
  neither name nor role.
- CLI worker count comes from `--workers`, then `GLAM_WORKERS`, then zero.
  Configuration and configured CLI rewriting run on a dormant zero-worker
  runtime; selected assembly activates that same runtime exactly once. Workers
  are shared by related assembler/logger sessions. A divergent spark can
  occupy one indefinitely; cancellation and reduction fuel are deferred.
- Bare arguments run `conf.cli` through the isolated all-results interpreter.
  Its API contains standard control, `.env`, CLI-local `.log`, and CLI
  readers/writers, but deliberately omits `.heap.*` and `.task.*`; it therefore
  makes no retryable state observations. Branch journals never commit.
- `.read.token Expectation Parser` runs `Parser` in a separate restricted
  all-results machine against exactly one UTF-8 argument and requires complete
  token consumption. Token requests that escape this boundary are errors;
  token alternatives resume the enclosing CLI continuation independently.
- `.case Explain Parse` is CLI-owned scoped metadata. It does not change raw
  `.alt` ordering and does not force `Explain` during successful command
  construction. Failed readers retain their active nested cases; successful
  scopes close them. Completion exposes the original explanation values, while
  parse errors render text or the conventional `usage`, `summary`, and
  `details` fields and retain the raw values under diagnostic `cli.cases`.
- `complete_configured` is shell-neutral analysis, not bootstrap dispatch. It
  retains an optional active argument's prefix/suffix and following arguments,
  keeps only the furthest candidate/expectation frontier, and never commits
  command writers. Capture-free token regexes report expectations but do not
  enumerate their languages. `--completions v0` transports this model through
  counted `OsString` fields and emits only NUL-terminated whole-argument
  replacements; do not expose lossy display text or internal candidate kinds
  through that protocol.
- `process.cli.args` is concrete while configuration loads. For bare dispatch,
  canonical `process.args` and `process.refl_args` are builder-created promises
  resolved only after one semantic command plan is selected. Bootstrap plans
  resolve them before configuration. Do not construct a second assembler or
  reparse projected arguments to cross this lifecycle seam.
- `FileSourceSystem` retains each local read's SHA-256 digest. A conflicting
  repeat read is an error; a change found only during the final recheck is a warning.
  Manifests contain the retained digests, not a later rescan. Standalone
  `--check_manifest` verifies those files without entering assembly.
- `inspect_g_source` is the narrow public Rust facade for built-in `.g` parser
  diagnostics and declaration summaries. The syntax AST, compile context, and
  lowering implementation stay private. Standalone `--parse` writes this
  report to stdout and does not enter assembly or load imports.
- Bootstrap CLI parsing lives in the library `cli` facade and consumes
  `OsString`; `main` executes its typed `TopLevelCommand` without interpreting
  individual assembly flags. Keep opaque paths and arguments out of UTF-8
  conversion until a typed operation explicitly requires text.
- `--parse_cli` and `--parse_cli.0` use the same configured expansion as bare
  execution but neither executes the plan nor activates workers. Their line
  and NUL output forms invent no escaping language.
- A complete `--parse_cli` or `--parse_cli.0` prefix delegates completion of its
  tail to `conf.cli`. A missing first completion argument remains bootstrap;
  a present empty first argument is a configured prefix. Minimal built-in
  Bash/Zsh adapters are replaceable by `conf.completion_script.[NAME]`.

## Source-Surface Regressions

- A `.g` source begins with a language declaration such as `language g0`.
- Comments begin with `#`. Top-level declarations are unindented; continuation
  lines are indented, except a closer-only line may dedent.
- `=`, `:=`, and `::=` remain distinct introduction, override, and update
  operations.
- List literals preserve every comma-separated expression as one element. Only
  explicit `++` or `list.concat` flattens structure.
- Multiline `let`/`where` bindings align under the first binding and do not
  accept `in`. Naked semicolons do not group local bindings; semicolon-separated
  bindings require braces. Braced `let`/`where` and every `with` body permit
  leading/trailing semicolons and explicit empty `{}` bodies. Keep valid and
  invalid samples synchronized with parser tests.
- Source local variables may not shadow another active local or a global that
  the same file introduces or actually selects through a visible namespace.
  The global check is file-wide and lives in `g_syntax/name_analysis.rs`, not
  parser routing. `_name` has canonical name `name`; repeated `_` binders and
  compiler-generated bindings remain exempt.
- `g_syntax/keywords.rs` is the one `g0` reserved-word table. Enforce it in
  definitions, locals, references, direct tags, and every direct path segment;
  do not grow parser-local keyword lists. `module` and `self` remain special
  references, while atoms, effect paths, and computed keys such as
  `.['where]` remain data escapes.
- Layout `do` is front-end sugar and must disappear during g-syntax
  resolution. A bare intermediate statement reuses `=>>` and therefore
  requires unit; the final expression is the continuation itself and is not
  implicitly wrapped in `.r`.
- Recursive do is never implicit. A direct `abstract Name, ...` step delimits
  one independently completable standard-effect `.fix` per name, ending at
  that name's fulfillment. Per-name intervals lower sequentially or
  hierarchically; crossing intervals promote later starts with a warning while
  withholding source visibility until the written declaration. The resolved
  value and continuation use a compiler-private payload, with no dedicated
  recursive-do representation in core or evaluation.
