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
  existing `TokenView` and never re-lex source substrings. Source macro
  replacement is the deliberate exception: it locally classifies each
  evolving owned logical declaration before the ordinary parser sees it.
  `LayoutView` infers both parser bodies and macro child-layout scopes; do not
  add a second indentation algorithm for macros. It returns rather than
  consumes its first dedent. A leading macro anchor after a layout-introducing
  keyword uses the first hanging member's column, not its physical line
  indentation, including when that nested layout is bounded by a delimiter.
  Macro input discovery never treats `,` or `;` as a boundary; punctuation is
  ordinary macro text until the ordinary parser interprets the expanded
  source. Macro `.write.text` excludes ASCII C0 controls, SP, and DEL; only
  `.write.sep` and `.write.anchor` emit logical whitespace. `ExpressionContext`
  decides whether a postfix/infix owner may resume at a dedent.
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
- One top-level module build creates one `CompilationExecution` and propagates
  it through every input and recursive import. Macro effects use its private
  evaluation/reflection session, not assembler reasoning: heaps, tasks, and
  diagnostic counts remain distinct even though both sessions share an
  executor. Direct macro journals backtrack; committed `anno refl:` work does
  not.

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
- Comments begin with `#`. Top-level declarations are unindented and
  continuation lines are indented. A boundary-aligned line containing only
  closing delimiters may terminate a declaration, but may not carry or precede
  a later expression suffix. Nested declarations use the same relative rule.
- Layout siblings align at the first member's inferred anchor; deeper lines
  continue that member. A final structural child inherits its owner's floor.
  Do not make the inline RHS column a new floor or let a nested body collide
  with the operator indentation that introduced it.
- Delimited groups retain ordinary expression continuation internally.
  Commas/semicolons alone separate members; only post-opening member or
  separator contribution lines establish and obey the delimiter content
  anchor.
- `=`, `:=`, and `::=` remain distinct introduction, override, and update
  operations.
- List literals preserve every comma-separated expression as one element. Only
  explicit `++` or `list.concat` flattens structure.
- Dot-leading effect paths must be parenthesized in application-argument
  position: `foo (.bar)` is valid and `foo .bar` is rejected. Head-position
  `.bar`, member access `foo.bar`, and `foo <| .bar` remain valid.
- Distinct arithmetic operators have no implicit precedence, and neither do
  `and` and `or`. Preserve homogeneous chains and require parentheses for
  mixed forms such as `a + (b * c)` and `A or (B and C)`.
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
  bare definition roots, locals, and bare references; do not grow parser-local
  keyword lists. Explicit member/key positions may use keyword spellings:
  `module.where`, `where:Value`, and `.['where] = Value` are valid. `module`
  and `self` remain special bare references.
- `using Dict in Expr` lowers entirely in `g_syntax`: evaluate and share
  `Dict` in the surrounding scope, use it as the temporary final namespace and
  `self`, use `{}` as the prior namespace and `_self`, and retain the
  surrounding scope as the `^` escape target. `using Dict do Body` is only
  syntax sugar for a do-valued body; it does not create an object or runtime
  scope agent.
- Layout `do` is front-end sugar and must disappear during g-syntax
  resolution. A bare intermediate statement reuses `=>>` and therefore
  requires unit; the final expression is the continuation itself and is not
  implicitly wrapped in `.r`.
- Do patterns expand directly into resolved primitive-do steps. Multi-capture
  `P as Q` binds one internal subject and emits ordered source aliases; do not
  reconstruct a surface `DoExpr` or lower the subject more than once.
- Literal and list-pattern observations are compiler-private pass/fail effects:
  mismatches invoke `.fail`, while forcing errors still propagate. Quoted-path
  patterns with only literals remain exact fixed lists; computed components
  resolve at their source-order match step and use directional key-path
  comparison. A list pattern has at most one variable-length segment; that
  segment is an ordinary pattern over the residual middle slice and may be
  refutable.
- Fixed dictionary, tag, and tuple patterns use the same compiler-private
  dictionary decomposition. Known paths are removed persistently without
  iteration; only exact-empty remainder checks may scan deferred fields to
  honor logical undefined equivalence. Required `path:Pattern` entries mismatch
  on absent/undefined paths; explicit `path?:Pattern` entries pass `{}` to the
  payload and preserve the original remainder. A defined non-dict path prefix
  still mismatches. Computed dictionary paths are evaluated once immediately
  before their payload match and may use earlier captures. Computed
  `[KeyExpr,...]:Pattern` and `(PathExpr):Pattern` tags are the same singleton
  dictionary decomposition without braces. A final dictionary remainder is an
  ordinary pattern over the residual dictionary and may be refutable; omitting
  it instead requires the residual dictionary to be logically empty.
- Effectful patterns also expand into the same primitive-do stream. A view
  applies its expression to the subject, binds the resulting effect, then
  matches the produced value. A predicate appends the original subject to its
  expression, requires unit success, then matches that unchanged subject.
  Complete match-arm patterns may omit the usual outer parentheses around
  either form because `=>` owns their exact range; `do` and nested patterns
  retain the grouping requirement.
  Local `when` guards run after their enclosed pattern; `and`-separated effect,
  effect-bind, and value-bind clauses run left to right, and their captures
  remain visible after the complete guarded pattern succeeds. General `P as Q`
  matches one shared subject left to right.
- Prefix `if Guards then A else B` is syntax-owned pure search. Both arms lower
  through the shared conditional effect-step builder under one `.cut`; the
  second arm is unconditional. Flat `match Subject with` binds its subject
  once outside the same ordered search, then gives each full pattern and its
  optional guards an isolated sibling scope. Cached closed compiler helpers
  run pure searches and apply syntax-specific result policies: `if` has a
  required fallback, while an empty or exhausted `match` reports
  `match exhausted on line N`, where `N` is the enclosing match's source line.
  Lowering supplies that lazy terminal result before the root cut; the generic
  result selector's empty-list error is only a compiler-invariant diagnostic.
  Guard and pattern captures are progressively visible only in their result
  arm. Missing host effects and selected-result failures are evaluator errors,
  not fallback.
- `match when` is the distinct guard-only form. In either match form, an arm
  ending in `when` owns deeper layout or braced child arms. The resolved
  parent pattern/guard step stream wraps the child `.alt` search directly:
  children share progressive parent captures and work, child exhaustion may
  fall through to the next parent arm, and only the complete match owns a
  `.cut`.
- `try` and `try_match` use those same syntax and resolved choice shapes in
  host mode. They return the one root `.cut` operation to the ambient effect
  handler instead of using the cached isolated pure runner. Consequently host
  effects and transactional rollback are available; exhausted `try_match`
  remains `.fail`, while required `else` keeps `try` total.
- Starred matches are the explicit open-search forms. `match*` runs the uncut
  choice tree through the isolated list handler and returns its lazy result
  list; exhaustion is `[]`. `try_match*` returns that same uncut tree to the
  ambient handler; exhaustion is `.fail`. Keep root commitment orthogonal to
  pure-versus-host handling in syntax and lowering. In particular, never add
  the ordinary pure-match exhaustion error as a hidden `.alt` branch of
  `match*`.
- Ordinary `then`/`=>` results are staged with compiler-owned `.r`; tentative
  `then?`/`=>?` results are emitted as effect operations directly. A tentative
  `.fail` therefore falls through under the same root `.cut`. Keep this as an
  explicit result mode through syntax and resolved choice IR rather than
  inferring it from the result expression.
- Postfix `A if Guards else B` is a structural suffix over the same pure
  `IfExpr` as prefix syntax. Resolve and analyze `Guards` before `A`; captures
  are visible only in `A`, never in `B` or the surrounding expression. A
  following prefix `if ... then ... else ...` remains an application argument,
  distinguished by its owned `then`.
- Recursive do is never implicit. A direct `abstract Name, ...` step delimits
  one independently completable standard-effect `.fix` per name, ending at
  that name's fulfillment. Per-name intervals lower sequentially or
  hierarchically; crossing intervals promote later starts with a warning while
  withholding source visibility until the written declaration. Recursive
  planning scans the completed primitive-do stream and its explicit
  declaration/fulfillment provenance; it must not infer regions from surface
  patterns. The resolved value and continuation use a compiler-private
  payload, with no dedicated recursive-do representation in core or
  evaluation.
