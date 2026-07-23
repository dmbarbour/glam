# Bootstrap Implementation Map

This is the navigation and dataflow map for the current Rust bootstrap. It does
not define future language semantics or collect subsystem invariants.

- Current control flow:
  [`assembly`](../docs/architecture/assembly.md),
  [`evaluation`](../docs/architecture/evaluation.md), and
  [`reflection`](../docs/architecture/reflection.md).
- Regression-sensitive rules: [`AgentContext.md`](../docs/AgentContext.md) and
  its focused notes.
- Target design: [`DistilledDesign.md`](../docs/DistilledDesign.md).

## Module Ownership

| Path | Responsibility |
| --- | --- |
| `main.rs` | Executes typed top-level commands, chooses configuration/assembly roots, owns logger policy, process I/O, and exit status |
| `cli.rs`, `cli/model.rs` | Public CLI facade plus validated bootstrap/configured command and argument models |
| `cli/bootstrap.rs`, `cli/output.rs` | OS-string bootstrap dispatch, standalone-option validation, help text, and inspection/completion formatting |
| `cli/configured.rs`, `cli/search.rs` | `conf.cli` lookup, isolated all-results execution, branch validation, and semantic-plan selection |
| `cli/effects.rs`, `cli/host.rs` | Serial CLI reader/writer effect specialization and immutable invocation host |
| `cli/completion.rs`, `cli/basic.rs` | Optional-cursor completion requests, candidates/frontiers, lexical routing, and bootstrap-option completion |
| `cli/path.rs`, `cli/adapters.rs` | Shared filesystem completion plus replaceable minimal Bash/Zsh bindings over the shell-neutral protocol |
| `cli/token.rs`, `cli/token/` | Restricted nested token-effect search plus literal, Unicode-scalar, end, and capture-free regex readers |
| `source.rs` | Immutable source artifacts, identities and digests, relative resolvers, host compatibility, and tracked local files |
| `lib.rs`, `api.rs` | Embedding facade: staged assembler construction, opaque values, internal reasoning-session ownership, modules, evaluation, diagnostics, extraction, and checked nets |
| `g_source.rs` | Narrow public inspection report for the built-in `.g` parser; no syntax tree or lowering context escapes |
| `compiler.rs` | Per-source capabilities, hidden artifact/import provenance, loaders, namespace qualification, and diagnostic emission |
| `g_syntax.rs` | Private built-in `.g` front-end facade |
| `g_syntax/parser/lexical.rs` | Authoritative single-pass source validation and spanned tokens, text values, delimiter pairs, indentation facts, and declaration ranges |
| `g_syntax/parser/input.rs` | Checked token-range views, balanced-group iteration, mapped Chumsky input, token predicates, and source-aware parser diagnostics |
| `g_syntax/parser/source.rs`, `lexical.rs`, `input.rs`, `layout.rs` | UTF-8 orchestration, the one source scan, token-range/Chumsky input, and contextual layout views |
| `g_syntax/parser/expression.rs`, `structural.rs`, `do_expr.rs` | Ordinary precedence grammar, structural `let`/`where`/object/`with` forms, and do statements |
| `g_syntax/parser/declaration.rs`, `declaration/simple.rs` | Top-level and recursive object-body declarations, including simple language/import/abstract/unique forms |
| `g_syntax/keywords.rs` | Language-version-owned `g0` reserved words and their syntactic roles |
| `g_syntax/resolve/`, `resolved.rs`, `analysis.rs`, `name_analysis.rs` | Lexical resolution, affine semantic IR, local-use warnings, and file-wide local/global shadow checks |
| `g_syntax/compiler_values.rs` | Shared closed helpers and built-in modules owned by the g compiler |
| `g_syntax/module_lowering/` | Imports, definitions, objects, and module fixpoint orchestration |
| `g_syntax/net_lowering.rs` | Resolved functions and applications to closed interaction nets |
| `g_syntax/diagnostic_formatter.rs` | Cached closed Glam default `Diagnostic -> Bytes` formatter |
| `core.rs`, `core/` | Syntax-independent values, functions, computed lazies, named promises, dictionaries, keys, and builtin IDs |
| `core_net.rs` | Core data/operator specialization of generic interaction nets |
| `interaction_net/model.rs`, `builder.rs` | Generic identities, agents, specialization protocol, and checked construction |
| `interaction_net/runtime/` | Mutable graph, active-pair rewrites, cursors, and runtime tests |
| `evaluation.rs`, `evaluation/executor.rs` | Sessions, contexts, task scheduling, workers, and sparks |
| `eval/value.rs`, `application.rs`, `operator.rs`, `net.rs` | Value forcing, application, operator staging, and net driving |
| `eval/builtins/` | Builtin implementations split by semantic family |
| `eval/builtins/net/construction.rs` | Lazy `interaction_net` effect search, branch-local construction journals, opaque port capabilities, and checked replay |
| `eval/sequence.rs` | Lazy list-to-binary observation and ranged extraction |
| `list.rs`, `number.rs` | Compact persistent list ropes and exact-number boundary |
| `diagnostic.rs`, `api.rs` diagnostic facade | Diagnostic values, enrichment metadata, session buses, subscriptions, and severity counts |
| `reflection.rs`, `reflection/requests.rs`, `reflection/search.rs` | Persistent freer-effect machine, task API, transactions, request helpers, and isolated all-results policy |
| `reflection/store.rs` | Persistent shared/private volumes, journaled ordered edit rebasing, asynchronous query state, and pluggable read-conflict analysis |

`interaction_net.rs`, `eval.rs`, and `g_syntax.rs` are facades over their
submodules rather than homes for another implementation layer.

## End-to-End Assembly

```text
main or embedding client
  -> AssemblerBuilder fixes SourceSystem + reasoning resources
  -> Assembler + ModuleBuilder
  -> SourceSystem supplies immutable SourceArtifact
  -> artifact supplies bytes + identity + digest + relative resolver
  -> CompileContext supplies source-scoped capabilities
  -> selected front end parses, resolves, and lowers
  -> closed module Value
  -> explicit evaluation/extraction
```

Imports re-enter the same assembler session through artifact-installed relative
resolvers.
For a bare command, the CLI first loads configuration with a dormant runtime,
runs `conf.cli` as an isolated all-results search, resolves the promised
canonical argument environment, and only then activates the selected worker
count. It then extracts `asm.result`, drains reflection reasoning, finalizes
local file tracking, and closes configured logging. See the
[assembly flow](../docs/architecture/assembly.md) for ordering and failure
behavior.

The same configured interpreter has a non-committing completion mode. It
preserves an optional active argument's prefix and suffix plus later arguments,
retains only evidence at the furthest argument/token frontier, and validates
complete candidates by replaying ordinary isolated parsing. Bootstrap and
configured completion share that request/result model. `--completions v0`
accepts a count-framed OS-argument request and emits only NUL-terminated whole
argument replacements. `.case` scopes lazy structured explanations around
configured branches; failed frontier evidence and completion results retain
those values without changing raw choice semantics. `.read.token` delegates
one UTF-8 argument to a second restricted all-results effect machine; its
ordinary result resumes the enclosing CLI continuation once per token
alternative.

## Front-End Dataflow

```text
raw source bytes
  -> built-in g lexical structure and source-wide diagnostics
  -> parser-owned SyntaxExpr and declarations
  -> resolver-owned BindingId locals and ResolvedExpr<Value>
  -> module lowering or net lowering
  -> closed Value / FunctionCode / NetValue
```

Syntax and sugar end in `g_syntax`. `ResolvedExpr<Value>` is moved through a
single lowering; no syntax or core expression tree survives into evaluation.
Module lowering owns declaration order and the open module fixpoint. Net
lowering emits complete bind and application spines.

## Evaluation Dataflow

```text
Assembler -> EvalContext -> Value
  -> observe existing data
  -> claim and memoize computed lazy work
  -> read and follow named promise assignments
  -> apply a builtin/function/net
  -> drive a net until its interface exposes data

EvaluationSession <-> EvaluationExecutor
  -> demanded reflection producers via the serial pump
  -> ready reflection work via shared workers
  -> optional spark work via shared workers
```

Evaluation receives closed values and an explicit session context. Reflection
effects remain external freer-monad tasks. Generic interaction-net reduction
knows topology; `core_net` and `eval` supply core semantics. See the
[evaluation](../docs/architecture/evaluation.md) and
[reflection](../docs/architecture/reflection.md) notes for the handoffs.

Source-level net construction follows a separate lazy path:

```text
interaction_net Effect
  -> isolated standard-effect search
  -> persistent write-only journal per alternative
  -> exactly one successful exposed-port result
  -> checked replay through NetBuilder
  -> one memoized shared Value::Net runtime
```

Construction never exposes raw graph identities. Branded opaque port handles
exist only while the effect runs, and failed alternatives are never replayed.

## Interaction-Net Reduction

```text
claim one principal-principal pair under the runtime mutex
  -> rewrite a topology-only rule immediately, or
  -> release the mutex for callable/operator/cursor work
  -> complete, block, or mark that same pair stuck
```

Logical copies are target-owned one-way cursors. Source active pairs reduce in
the source and never migrate into the target. Detailed fan, frontier, and
locking rules live only in the focused
[interaction-net invariants](../docs/agent_context/interaction_nets.md).

## Test Navigation

- Parser tests sit beside `g_syntax/parser/` modules.
- Cross-front-end tests live in `g_syntax/tests.rs`.
- Runtime topology and cursor tests live in
  `interaction_net/runtime/tests.rs`.
- Evaluator integration tests live in `eval/tests.rs`; fixtures are in
  `eval/test_support.rs`.
- `tests/` covers the public facade, CLI, valid samples, and invalid source
  fixtures.
