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
| `bin/glam/main.rs` | Process entry, typed command dispatch, and command-specific I/O adapters |
| `bin/glam/batch.rs` | Assembly output, runtime settlement, reports, and exit policy |
| `bin/glam/rendering.rs` | Default and fallback terminal diagnostic rendering |
| `bin/glam/configuration/` | `GLAM_CONF`, `conf.env`, configured logger lifecycle, and executable configuration policy |
| `bin/glam/command_line/` | Private bootstrap command grammar, validated command models, help, and shell completion |
| `bin/glam/command_line/configured/` | `conf.cli` effects, isolated search, path policy, and nested token parsing |
| `source.rs` | Source artifacts, digests, relative resolvers, tracked local files |
| `lib.rs`, `api.rs` | Stable embedding facade and re-exports |
| `api/value.rs`, `api/evaluator.rs`, `api/error.rs` | Runtime-rooted value construction, explicit demand/extraction, privileged inspection, and structured embedding failures |
| `api/value/prototype.rs` | Test-only Phase I2 experiment for the selected opaque inline-or-managed public-root representation; production values do not use it |
| `api/value/access_inventory.rs` | Test-only I2C source inventory that prevents unclassified bare-core compatibility access from growing before I3/I4 migration |
| `core/managed.rs`, `core/managed/` | Private managed-family and opaque-payload admission, scoped allocation/access, closed value-shell fixtures, and source-backed closure/opaque containment inventory |
| `api/diagnostics.rs` | Diagnostic values, buses, subscriptions, enrichment, and runtime ingress |
| `api/runtime.rs`, `api/runtime/` | Runtime ownership, transactional events, delivery, readiness, deadlock reports, and settlement |
| `api/assembly.rs` | Assembler/reasoning construction, protected volumes, sources, imports, and module builds |
| `g_source.rs` | Non-evaluating public `.g` inspection summary |
| `compiler.rs` | Per-source compiler capabilities and hidden provenance |
| `g_syntax.rs` | Private built-in `.g` compiler facade |
| `g_syntax/parser/source.rs` | Staged source parsing, macro expansion, declaration orchestration |
| `g_syntax/parser/lexical.rs` | Authoritative source-wide tokens, delimiter groups, declaration sections, and lexical payload arenas |
| `g_syntax/parser/logical.rs` | Declaration-scoped macro input/layout discovery, generated-output validation, embedded-value rendering, and source replay |
| `g_syntax/parser/input.rs` | Checked token views and Chumsky input over the authoritative lexical structure |
| `g_syntax/parser/layout.rs`, `expression_context.rs` | Layout ownership, floors, and expression boundaries |
| `g_syntax/parser/expression.rs`, `structural.rs`, `do_expr.rs`, `conditional.rs` | Expression and structural syntax |
| `g_syntax/parser/declaration.rs`, `declaration/` | Top-level and recursive declarations |
| `g_syntax/keywords.rs` | Language-version keyword ownership |
| `g_syntax/resolve/`, `resolved.rs`, `analysis.rs`, `name_analysis.rs` | Resolution, affine IR, and source analysis |
| `g_syntax/compiler_values.rs` | Atomically published, runtime-rooted closed compiler helpers and modules |
| `g_syntax/macro_expansion/` | Macro effect API, journals, and isolated search |
| `g_syntax/module_lowering/` | Imports, definitions, objects, module fixpoint |
| `g_syntax/net_lowering.rs` | Resolved functions and applications to closed nets |
| `g_syntax/diagnostic_formatter.rs` | Cached Glam `Diagnostic -> Bytes` formatter |
| `text_pattern.rs` | Shared capture-free text-pattern language |
| `core.rs`, `core/` | Syntax-independent values, runtime value-domain ownership, factory-scoped managed allocation/rooting, lazies, promises, functions, keys, builtins |
| `core/managed.rs` | Factory-qualified collector access, domain-qualified `RuntimeValueAccess`, Glam's centralized managed-slot policy, and private managed-family destruction admission records |
| `core/managed/value_shell.rs` | Test-only I4A exhaustive managed-shell, leaf-policy, layout, and cyclic tracing fixtures; production values remain unmigrated |
| `crates/glam-gc/` | Glam-owned typed-run tracing collector; the runtime domain currently owns a no-auto heap while production values remain unmigrated |
| `core_net.rs` | Exact-value-domain facade plus scoped observation/mutation view for core interaction nets; raw shared-net ownership remains private |
| `interaction_net/model.rs`, `builder.rs` | Generic topology and checked construction |
| `interaction_net/runtime/` | Mutable graph, active-pair reduction, logical copies |
| `evaluation.rs`, `evaluation/session.rs`, `evaluation/pump.rs` | Shared demand/profile contracts, session admission, cooperative and runtime pumping |
| `evaluation/access.rs` | I3 scoped evaluator authority, thread-bound mutator-free poll and evaluator-step contexts, claim/direct-owner poll admission, scoped wait-completion projection, post-scope reflection activation, remaining non-effect direct-evaluator compatibility, and temporary machine-completion root seam |
| `evaluation/coordinator.rs`, `evaluation/coordinator/` | Authoritative work registry/queues plus task, completion, client-demand, spark, reflection, deferred, and settlement lifecycles; completed wait observations retain roots, parked demand routing is weak, and detached claims temporarily upgrade the exact registered session/domain |
| `evaluation/observation.rs`, `evaluation/executor.rs` | Semantic observation epochs and worker lifecycle |
| `eval/value.rs`, `application.rs`, `operator.rs`, `net.rs` | Value forcing and semantic execution |
| `eval/access_inventory.rs` | Test-only I3B closure inventory for scoped evaluator functions, durable subsystem seams, external direct calls, and builtin downgrades |
| `eval/builtins/` | Builtin implementations by semantic family; I3B's scoped dispatcher keeps ordinary pure work on `EvaluatorStepContext` and source-latches durable effect, strategy, net, provenance, and reflection handoffs |
| `eval/builtins/net/construction.rs` | Source interaction-net construction search |
| `eval/sequence.rs` | Lazy sequence and binary extraction |
| `list.rs`, `number.rs` | Persistent list ropes and exact numbers |
| `diagnostic.rs`, `api/diagnostics.rs` | Semantic diagnostic shapes plus embedding buses, ingress, and enrichment |
| `reflection.rs`, `reflection/protocol.rs` | Reflection facade, specialization/host transaction protocol, and bounded callback evaluation service |
| `reflection/lifecycle.rs` | Effect lifecycle, scheduled runs, and task launchers |
| `reflection/machine.rs`, `reflection/requests.rs`, `reflection/search.rs` | Persistent phased effect machine, rooted request interpretation, bounded standard-effect fusion, and isolated search |
| `reflection/store.rs` | Journaled volume roots, edits, snapshots, commits, and query lifetime |
| `reflection/store/conflict.rs` | Conflict paths plus exact, fingerprint, coarse, and client-defined observation strategies |
| `runtime.rs` | Runtime identity, mutation admission, activity accounting |

`interaction_net.rs`, `eval.rs`, `g_syntax.rs`, and `reflection.rs` are facades
over their submodules rather than additional implementation layers.

## Principal Dataflows

### Assembly

```text
`glam` binary or embedding client
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
- `evaluation/tests.rs` and `evaluation/coordinator/tests.rs` contain the
  cross-session and cross-kind lifecycle/concurrency suites; focused
  coordinator children and reflection modules retain tests beside their
  private machinery.
- Reflection-store query, volume, journal, rewrite, and commit tests live in
  `reflection/store/tests.rs`; strategy-specific tests remain beside
  `reflection/store/conflict.rs`.
- `api/tests.rs` retains cross-facade value/assembly tests;
  `api/tests/runtime_tests.rs` and `api/tests/diagnostic_tests.rs` own runtime
  event/readiness and diagnostic transport integration tests.
- `api/value/prototype.rs` contains the isolated GC public-root representation
  fixtures; it may force collection while production collection remains
  disabled.
- `api/value/access_inventory.rs` owns the mechanically checked production
  `Value`/`RuntimeValueRoot` compatibility-access baseline.
- Binary command-line and logger unit tests live below `bin/glam/`; `tests/cli.rs`
  covers the executable process contract.
- `tests/` also covers the public library facade, valid samples, and invalid
  fixtures. `tests/effect_embedding.rs` is the external generic effect-host
  contract used by the binary-owned configured interfaces.
