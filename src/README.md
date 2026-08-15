# Bootstrap Implementation Map

This is the navigation and dataflow map for the current Rust bootstrap. It does
not define language semantics or collect subsystem invariants.

- Current architecture:
  [`assembly`](../docs/architecture/assembly.md),
  [`front end`](../docs/architecture/front_end.md),
  [`evaluation`](../docs/architecture/evaluation.md),
  [`reflection`](../docs/architecture/reflection.md), and
  [`diagnostics`](../docs/architecture/diagnostics.md).
- Regression-sensitive rules: [`AgentContext.md`](../docs/AgentContext.md) and
  its focused notes.
- Target design: [`DistilledDesign.md`](../docs/DistilledDesign.md).

## Module Ownership

| Path | Responsibility |
| --- | --- |
| `main.rs` | Typed command execution, logger supervision, process I/O, exit policy |
| `cli.rs`, `cli/model.rs` | Public CLI facade and validated command models |
| `cli/bootstrap.rs`, `cli/output.rs` | Bootstrap dispatch, validation, help, output formatting |
| `cli/configured.rs`, `cli/search.rs` | `conf.cli` search and semantic-plan selection |
| `cli/effects.rs`, `cli/host.rs` | Serial CLI effect specialization and invocation host |
| `cli/completion.rs`, `cli/basic.rs` | Completion model, frontiers, and bootstrap completion |
| `cli/path.rs`, `cli/adapters.rs` | Filesystem completion and minimal shell adapters |
| `cli/token.rs`, `cli/token/` | Restricted nested token-effect parsing |
| `source.rs` | Source artifacts, digests, relative resolvers, tracked local files |
| `lib.rs`, `api.rs` | Embedding facade, runtime construction, modules, diagnostics, extraction |
| `g_source.rs` | Non-evaluating public `.g` inspection summary |
| `compiler.rs` | Per-source compiler capabilities and hidden provenance |
| `g_syntax.rs` | Private built-in `.g` compiler facade |
| `g_syntax/parser/source.rs` | Staged source parsing, macro expansion, declaration orchestration |
| `g_syntax/parser/lexical.rs`, `logical.rs`, `input.rs` | Shared lexical structure and parser-token views |
| `g_syntax/parser/layout.rs`, `expression_context.rs` | Layout ownership, floors, and expression boundaries |
| `g_syntax/parser/expression.rs`, `structural.rs`, `do_expr.rs`, `conditional.rs` | Expression and structural syntax |
| `g_syntax/parser/declaration.rs`, `declaration/` | Top-level and recursive declarations |
| `g_syntax/keywords.rs` | Language-version keyword ownership |
| `g_syntax/resolve/`, `resolved.rs`, `analysis.rs`, `name_analysis.rs` | Resolution, affine IR, and source analysis |
| `g_syntax/compiler_values.rs` | Runtime-cached closed compiler helpers and modules |
| `g_syntax/macro_expansion/` | Macro effect API, journals, and isolated search |
| `g_syntax/module_lowering/` | Imports, definitions, objects, module fixpoint |
| `g_syntax/net_lowering.rs` | Resolved functions and applications to closed nets |
| `g_syntax/diagnostic_formatter.rs` | Cached Glam `Diagnostic -> Bytes` formatter |
| `text_pattern.rs` | Shared capture-free text-pattern language |
| `core.rs`, `core/` | Syntax-independent values, lazies, promises, functions, keys, builtins |
| `core_net.rs` | Core specialization of generic interaction nets |
| `interaction_net/model.rs`, `builder.rs` | Generic topology and checked construction |
| `interaction_net/runtime/` | Mutable graph, active-pair reduction, logical copies |
| `evaluation.rs`, `evaluation/coordinator.rs`, `evaluation/executor.rs` | Demand ownership, runtime work records, workers |
| `eval/value.rs`, `application.rs`, `operator.rs`, `net.rs` | Value forcing and semantic execution |
| `eval/builtins/` | Builtin implementations by semantic family |
| `eval/builtins/net/construction.rs` | Source interaction-net construction search |
| `eval/sequence.rs` | Lazy sequence and binary extraction |
| `list.rs`, `number.rs` | Persistent list ropes and exact numbers |
| `diagnostic.rs`, `api.rs` diagnostic facade | Diagnostic values, buses, ingress, enrichment |
| `reflection.rs`, `reflection/requests.rs`, `reflection/search.rs` | Persistent effect machine, requests, isolated search |
| `reflection/store.rs` | Journaled volumes, conflict analysis, query state |
| `runtime.rs` | Runtime identity, mutation admission, activity accounting |

`interaction_net.rs`, `eval.rs`, and `g_syntax.rs` are facades over their
submodules rather than additional implementation layers.

## Principal Dataflows

### Assembly

```text
main or embedding client
  -> AssemblerBuilder fixes SourceSystem + EvaluationRuntime + reasoning policy
  -> ModuleBuilder creates one CompilationExecution
  -> SourceArtifact bytes + CompileContext capabilities
  -> selected front end produces closed module Value
  -> explicit evaluation / reflection inspection / extraction
  -> runtime readiness, settlement, diagnostics, process result
```

See [`architecture/assembly.md`](../docs/architecture/assembly.md) for client
ordering and [`architecture/diagnostics.md`](../docs/architecture/diagnostics.md)
for logger and fallback flow.

### Built-in front end

```text
source bytes
  -> one lexical token/group/declaration structure
  -> staged macro expansion and declaration parsing
  -> lexical and namespace resolution
  -> affine ResolvedExpr<Value>
  -> direct semantic value / interaction-net lowering
  -> closed module definitions
```

See [`architecture/front_end.md`](../docs/architecture/front_end.md) and the
focused [`g_syntax` invariants](../docs/agent_context/g_syntax.md).

### Evaluation and reflection

```text
EvaluationSession owner -> EvalContext -> Value demand
                               |
                               v
                    EvaluationWorkCoordinator <- EvaluationExecutor
                      | reflection work
                      | deferred producers
                      | best-effort sparks
                      v
             runtime reflection store and event boundary
```

Evaluation consumes closed values. Reflection remains an external persistent
freer-effect machine. Generic interaction-net reduction owns topology; core and
eval provide semantic specialization. See
[`architecture/evaluation.md`](../docs/architecture/evaluation.md),
[`architecture/reflection.md`](../docs/architecture/reflection.md), and the
focused [interaction-net invariants](../docs/agent_context/interaction_nets.md).

## Test Navigation

- Parser tests sit beside `g_syntax/parser/` modules.
- Cross-stage front-end tests live in `g_syntax/tests.rs`.
- Macro tests live in `g_syntax/macro_expansion/tests.rs` and
  `tests/macro_protocols.rs`.
- Runtime topology and cursor tests live in
  `interaction_net/runtime/tests.rs`.
- Evaluator integration tests live in `eval/tests.rs`; fixtures are in
  `eval/test_support.rs`.
- `evaluation.rs`, `evaluation/coordinator.rs`, `reflection.rs`, and `api.rs`
  contain focused lifecycle and concurrency tests beside private machinery.
- `tests/` covers the public facade, CLI, valid samples, and invalid fixtures.
