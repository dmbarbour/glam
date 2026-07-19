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
| `main.rs` | CLI policy, configuration/assembly roots, diagnostic-bus subscriptions, logger host, and process output |
| `local_files.rs` | CLI local-file consistency and optional SHA-256 manifest |
| `lib.rs`, `api.rs` | Embedding facade: opaque values, source hosts, internal reasoning-session ownership, modules, evaluation, diagnostics, extraction, and checked nets |
| `compiler.rs` | Per-source capabilities, hidden provenance, loaders, namespace qualification, and diagnostic emission |
| `g_syntax.rs` | Built-in `.g` front-end facade |
| `g_syntax/parser/` | Layout, declaration, expression, and compound parsing |
| `g_syntax/resolve/`, `resolved.rs`, `analysis.rs` | Scope resolution, affine semantic IR, captures, and warnings |
| `g_syntax/compiler_values.rs` | Shared closed helpers and built-in modules owned by the g compiler |
| `g_syntax/module_lowering/` | Imports, definitions, objects, and module fixpoint orchestration |
| `g_syntax/net_lowering.rs` | Resolved functions and applications to closed interaction nets |
| `g_syntax/diagnostic_formatter.rs` | Cached closed Glam default `Diagnostic -> Bytes` formatter |
| `core.rs`, `core/` | Syntax-independent values, functions, lazy cells, dictionaries, keys, and builtin IDs |
| `core_net.rs` | Core data/operator specialization of generic interaction nets |
| `interaction_net/model.rs`, `builder.rs` | Generic identities, agents, specialization protocol, and checked construction |
| `interaction_net/runtime/` | Mutable graph, active-pair rewrites, cursors, and runtime tests |
| `evaluation.rs`, `evaluation/executor.rs` | Sessions, contexts, task scheduling, workers, and sparks |
| `eval/value.rs`, `application.rs`, `operator.rs`, `net.rs` | Value forcing, application, operator staging, and net driving |
| `eval/builtins/` | Builtin implementations split by semantic family |
| `eval/sequence.rs` | Lazy list-to-binary observation and ranged extraction |
| `list.rs`, `number.rs` | Compact persistent list ropes and exact-number boundary |
| `diagnostic.rs`, `api.rs` diagnostic facade | Diagnostic values, enrichment metadata, session buses, subscriptions, and buffers |
| `reflection.rs`, `reflection/requests.rs` | Persistent freer-effect machine, task API, transactions, and request helpers |

`interaction_net.rs`, `eval.rs`, and `g_syntax.rs` are facades over their
submodules rather than homes for another implementation layer.

## End-to-End Assembly

```text
main or embedding client
  -> Assembler + ModuleBuilder
  -> Host supplies source bytes
  -> CompileContext supplies source-scoped capabilities
  -> selected front end parses, resolves, and lowers
  -> closed module Value
  -> explicit evaluation/extraction
```

Imports re-enter the same assembler session through host-installed loaders.
The CLI then extracts `asm.result`, drains reflection reasoning, finalizes local
file tracking, and closes configured logging. See the
[assembly flow](../docs/architecture/assembly.md) for ordering and failure
behavior.

## Front-End Dataflow

```text
raw source bytes
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
  -> claim and memoize lazy work
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
