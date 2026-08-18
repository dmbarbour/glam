# Module Split Inventory — 2026-08-18

This is the evidence inventory for
[`ModuleSplitPlan_2026-08-18.md`](ModuleSplitPlan_2026-08-18.md).
It accounts for all 127 Rust files as they exist on 2026-08-18. Counts are
approximate production/test lines (`P/T`): a terminal inline `mod tests` is
separated mechanically, while dedicated test files are counted wholly as
tests.

Assessment codes: **C** cohesive, **F** deliberate facade/root, **M** mixed
responsibilities worth deeper review, **TH** cohesive production obscured by
large inline tests, and **TO** test-only organization.

## Embedding, runtime, and top-level modules

| File | P/T | Role | Assessment |
| --- | ---: | --- | --- |
| `src/lib.rs` | 43/0 | Crate module tree and public re-exports. | F; keep small. |
| `src/api.rs` | 6193/4449 | Values/evaluator facade, nets, diagnostics, runtime events and settlement, assembler construction, modules, promises, and public reports. | M; highest-priority dossier. |
| `src/runtime.rs` | 364/0 | Runtime identity allocation, mutation admission, rooted values, and ID domains. | C; small ownership kernel. |
| `src/compiler.rs` | 355/194 | Compile capabilities, source/binary loading callbacks, provenance, and relative paths. | C/TH; modest and internally related. |
| `src/core.rs` | 1678/516 | Deferred values, failures, runtime value factory/cache, keys, values, functions, builtins, and list thunks. | M; large but tightly coupled and likely affected by future GC. |
| `src/core/evaluation_halt.rs` | 112/0 | Structured evaluation halt wrapper and context. | C. |
| `src/core/keys.rs` | 92/0 | Lazy canonical protocol keys. | C. |
| `src/core_net.rs` | 73/0 | Core specialization of generic interaction nets. | C. |
| `src/diagnostic.rs` | 408/139 | Diagnostic severity, compilation/source provenance, context projection, and summaries. | C/TH. |
| `src/list.rs` | 824/150 | Persistent byte/value/lazy lists and traversal. | C; large single data structure. |
| `src/number.rs` | 331/101 | Exact number representation, parsing, arithmetic, and conversions. | C. |
| `src/source.rs` | 636/129 | Source systems, artifacts, digests, local consistency, and manifests. | C/TH. |
| `src/text_pattern.rs` | 341/95 | Versioned capture-free text-pattern parser and matcher. | C. |

## Evaluation

| File | P/T | Role | Assessment |
| --- | ---: | --- | --- |
| `src/eval.rs` | 60/0 | Evaluation facade and module routing. | F. |
| `src/eval/application.rs` | 144/0 | Application of builtins, functions, dictionaries, and lazy callees. | C. |
| `src/eval/operator.rs` | 288/0 | Interaction-net operators and capture/application construction. | C. |
| `src/eval/sequence.rs` | 146/0 | Semantic list/key/binary sequence extraction helpers. | C. |
| `src/eval/value.rs` | 747/0 | Evaluation error values, context frames, key conversion, and WHNF forcing. | C; broad but one value-observation layer. |
| `src/eval/net.rs` | 1280/0 | Cursor-WHNF/net driver, normalization worklist, function calls, and core specialization. | C; large algorithm, not an obvious split. |
| `src/eval/builtins.rs` | 113/0 | Builtin saturation and family dispatch. | F. |
| `src/eval/builtins/annotation.rs` | 11/0 | Annotation dispatch facade. | F. |
| `src/eval/builtins/annotation/implementation.rs` | 440/0 | Annotation recognition, metadata, reflection gates, strategies, and errors. | C; one semantic family. |
| `src/eval/builtins/assertion.rs` | 38/0 | Compiler-private assertion gates. | C. |
| `src/eval/builtins/comparison.rs` | 60/0 | Comparison builtin dispatch. | F. |
| `src/eval/builtins/comparison/implementation.rs` | 233/0 | Equality/order comparison semantics. | C. |
| `src/eval/builtins/conditional.rs` | 33/68 | Compiler-generated conditional selection policy. | C/TH. |
| `src/eval/builtins/dict.rs` | 34/0 | Dictionary builtin dispatch. | F. |
| `src/eval/builtins/dict/basic.rs` | 69/0 | Singleton, union, and update entry points. | C. |
| `src/eval/builtins/dict/merge.rs` | 196/0 | Hierarchical merge and path-update algorithms. | C. |
| `src/eval/builtins/effect.rs` | 54/0 | Effect builtin dispatch. | F. |
| `src/eval/builtins/effect/implementation.rs` | 100/0 | Fixpoint/effect-map operations. | C. |
| `src/eval/builtins/list.rs` | 60/0 | List builtin dispatch. | F. |
| `src/eval/builtins/list/implementation.rs` | 279/0 | Slice, map, concat, length, indexing, and list normalization. | C. |
| `src/eval/builtins/list_effect.rs` | 39/0 | List-effect dispatch. | F. |
| `src/eval/builtins/list_effect/implementation.rs` | 168/0 | List freer-effect sequencing, choice, cut, and fixpoint. | C. |
| `src/eval/builtins/net.rs` | 42/0 | Lambda-style opaque-net interface. | F. |
| `src/eval/builtins/net/construction.rs` | 507/20 | `interaction_net` effect journal and checked replay. | C. |
| `src/eval/builtins/numeric.rs` | 39/0 | Numeric dispatch. | F. |
| `src/eval/builtins/numeric/implementation.rs` | 46/0 | Arithmetic/division/floor/mod operations. | C. |
| `src/eval/builtins/object.rs` | 60/0 | Object builtin dispatch. | F. |
| `src/eval/builtins/object/implementation.rs` | 445/0 | Object specs, instantiation, extension, and linearization. | C; one semantic family. |
| `src/eval/builtins/pattern.rs` | 391/0 | Compiler-private list/dict/tag pattern observations. | C. |
| `src/eval/builtins/provenance.rs` | 15/0 | Opaque-origin projection. | C. |
| `src/eval/builtins/strategy.rs` | 48/0 | `seq`, `spark`, and demand strategy. | C. |
| `src/eval/test_support.rs` | 0/249 | Test expression fixtures and lowering. | TO. |
| `src/eval/tests.rs` | 0/5524 | Evaluator semantic and regression tests. | TO; split only for navigation. |

## Evaluation work ownership

| File | P/T | Role | Assessment |
| --- | ---: | --- | --- |
| `src/evaluation.rs` | 3013/5052 | Waits, promise obligations, task handles/status, client demand, reflection profiles, sessions, contexts, and task driving. | M/TH; strong candidate after dependency mapping. |
| `src/evaluation/coordinator.rs` | 5457/2090 | Completion subscriptions, work records, producer obligations, queues, claims, cancellation, readiness, deadlocks, and settlement. | M; second-largest production hotspot. |
| `src/evaluation/executor.rs` | 191/26 | Worker ownership and fair selection from coordinator work. | C. |

## Reflection

| File | P/T | Role | Assessment |
| --- | ---: | --- | --- |
| `src/reflection.rs` | 4202/4341 | Effect specialization API, host transactions, lifecycle launchers, freer-effect machine, continuations/cut/fix, and request decoding. | M/TH; strong seams already visible. |
| `src/reflection/requests.rs` | 850/243 | Standard reflection request vocabulary, journals, commits, task/query/volume operations. | M; request model and commit policy may separate. |
| `src/reflection/search.rs` | 292/27 | Isolated reflection search host and diagnostic collection. | C. |
| `src/reflection/store.rs` | 1051/497 | Reflection volumes, queries, conflict strategies, snapshots, journals, and commit. | M/TH; coherent store but separable query/conflict components. |

## Interaction nets

| File | P/T | Role | Assessment |
| --- | ---: | --- | --- |
| `src/interaction_net.rs` | 25/0 | Generic interaction-net facade. | F. |
| `src/interaction_net/builder.rs` | 408/0 | Checked reusable net-template construction. | C. |
| `src/interaction_net/model.rs` | 278/0 | Generic topology, ports, agents, and specialization traits. | C. |
| `src/interaction_net/runtime.rs` | 1859/0 | Runtime net protocol/types, shared state, normalization batches, claims, copying, calls, and mutation. | M; already partially split, recent Cursor-WHNF work makes boundaries reviewable. |
| `src/interaction_net/runtime/cursor.rs` | 580/0 | Cursor frontier inspection and lazy-copy materialization. | C. |
| `src/interaction_net/runtime/graph.rs` | 181/0 | Locked graph mutation primitives. | C. |
| `src/interaction_net/runtime/rewrite.rs` | 168/0 | Active-pair rewrite rules. | C. |
| `src/interaction_net/runtime/tests.rs` | 0/2891 | Generic runtime/cursor/rewrite concurrency tests. | TO; feature partition may aid navigation. |

## Built-in G front end: roots and lowering

| File | P/T | Role | Assessment |
| --- | ---: | --- | --- |
| `src/g_source.rs` | 160/31 | Narrow public `.g` source-inspection facade. | C. |
| `src/g_syntax.rs` | 113/0 | Front-end orchestration and module routing. | F. |
| `src/g_syntax/ast.rs` | 695/0 | Parsed syntax and public inspection structures. | C; many types but one representation layer. |
| `src/g_syntax/resolved.rs` | 184/70 | Resolved semantic-expression representation. | C. |
| `src/g_syntax/analysis.rs` | 1004/0 | Unused-local/object-body/source-expression analyses. | C; large traversal family. |
| `src/g_syntax/name_analysis.rs` | 722/30 | File-wide namespace/global/local shadow analysis. | C. |
| `src/g_syntax/keywords.rs` | 185/83 | Versioned keyword table and roles. | C. |
| `src/g_syntax/compiler_values.rs` | 802/203 | Runtime cache of closed compiler helpers and effect APIs. | M; related values, but cache families could become children. |
| `src/g_syntax/diagnostic_formatter.rs` | 119/25 | Cached Glam default-diagnostic formatter. | C. |
| `src/g_syntax/net_lowering.rs` | 291/0 | Resolved-expression to closed-net lowering. | C. |
| `src/g_syntax/recursive_do.rs` | 283/196 | Recursive-do dependency regions and plans. | C/TH. |
| `src/g_syntax/tests.rs` | 0/6540 | Broad compiler/source behavior tests. | TO; architecture-neutral navigation candidate. |
| `src/g_syntax/module_lowering.rs` | 159/0 | Module-lowering facade and shared context. | F. |
| `src/g_syntax/module_lowering/definitions.rs` | 463/0 | Definition/update lowering and assertion/context annotations. | C. |
| `src/g_syntax/module_lowering/imports.rs` | 218/0 | Builtin/local/source/binary import lowering. | C. |
| `src/g_syntax/module_lowering/objects.rs` | 495/0 | Object declaration/spec/instance lowering. | C. |

## Built-in G front end: parser

| File | P/T | Role | Assessment |
| --- | ---: | --- | --- |
| `src/g_syntax/parser.rs` | 21/0 | Parser facade. | F. |
| `src/g_syntax/parser/lexical.rs` | 1054/0 | Single-pass lexing, validation, groups, layout tokens, and embedded data. | C; large coherent machine. |
| `src/g_syntax/parser/lexical/tests.rs` | 0/550 | Lexer tests. | TO. |
| `src/g_syntax/parser/logical.rs` | 1016/131 | Logical source storage, macro invocation work, generated text, and expansion replay. | M/TH; source and generated halves merit dossier. |
| `src/g_syntax/parser/input.rs` | 679/0 | Token ranges/views and Chumsky input integration. | C. |
| `src/g_syntax/parser/input/tests.rs` | 0/347 | Token-view/input tests. | TO. |
| `src/g_syntax/parser/layout.rs` | 389/0 | Physical-line and inferred-layout structure. | C. |
| `src/g_syntax/parser/expression_context.rs` | 282/16 | Expression floors, extents, and ownership context. | C. |
| `src/g_syntax/parser/expression.rs` | 1139/0 | Ordinary expression atoms, application, paths, and precedence input. | C; large grammar role. |
| `src/g_syntax/parser/expression/infix.rs` | 298/0 | Infix precedence/associativity resolution. | C. |
| `src/g_syntax/parser/expression/tests.rs` | 0/331 | Expression tests. | TO. |
| `src/g_syntax/parser/conditional.rs` | 785/588 | `if`, `try`, `match`, and guarded choice grammar. | C/TH. |
| `src/g_syntax/parser/structural.rs` | 1487/0 | `let`, `where`, `using`, object, and `with` structural grammar. | M; related layout machinery but several grammar families. |
| `src/g_syntax/parser/structural/tests.rs` | 0/438 | Structural-expression tests. | TO. |
| `src/g_syntax/parser/declaration.rs` | 730/0 | Source/nested/object/extend declaration grammar. | C; simple forms already extracted. |
| `src/g_syntax/parser/declaration/simple.rs` | 220/0 | Non-expression top-level declarations. | C. |
| `src/g_syntax/parser/do_expr.rs` | 549/0 | `do` statement and block grammar. | C. |
| `src/g_syntax/parser/do_expr/tests.rs` | 0/693 | Do-notation tests. | TO. |
| `src/g_syntax/parser/pattern.rs` | 1020/0 | Pattern, guard, path, dict, list, and view grammar. | C; large semantic grammar family. |
| `src/g_syntax/parser/source.rs` | 727/0 | Staged source parsing, macro rounds, declarations, and inspection. | M; orchestration plus staging state. |
| `src/g_syntax/parser/floor_tests.rs` | 0/648 | Cross-grammar floor/layout regressions. | TO; intentionally cross-cutting. |

## Built-in G front end: macros and resolution

| File | P/T | Role | Assessment |
| --- | ---: | --- | --- |
| `src/g_syntax/macro_expansion.rs` | 19/0 | Macro-expansion facade. | F. |
| `src/g_syntax/macro_expansion/effects.rs` | 555/0 | Macro freer-effect requests and handler. | C. |
| `src/g_syntax/macro_expansion/host.rs` | 127/0 | Macro snapshot/journal and diagnostics. | C. |
| `src/g_syntax/macro_expansion/io.rs` | 344/0 | Structured macro input/output elements and layouts. | C. |
| `src/g_syntax/macro_expansion/runner.rs` | 305/0 | Macro search execution and failure projection. | C. |
| `src/g_syntax/macro_expansion/tests.rs` | 0/1206 | Macro protocol and expansion tests. | TO. |
| `src/g_syntax/resolve.rs` | 12/0 | Resolution facade. | F. |
| `src/g_syntax/resolve/scope.rs` | 514/0 | Resolver context, local scopes, bindings, and globals. | C. |
| `src/g_syntax/resolve/expression.rs` | 1075/0 | General expression/name/path/dict resolution and lowering. | M; central fallback plus several extractable expression families. |
| `src/g_syntax/resolve/pattern.rs` | 402/0 | Pattern expansion into effect steps. | C. |
| `src/g_syntax/resolve/effect_steps.rs` | 91/0 | Syntax-independent resolved effect sequencing. | C. |
| `src/g_syntax/resolve/do_expr.rs` | 408/346 | Do-block, recursive bindings, and effect-step emission. | C/TH. |
| `src/g_syntax/resolve/conditional.rs` | 407/588 | Conditional choice/guard resolution and lowering. | C/TH. |

## Command-line library and executable

| File | P/T | Role | Assessment |
| --- | ---: | --- | --- |
| `src/cli.rs` | 36/0 | CLI library facade. | F. |
| `src/cli/model.rs` | 407/0 | Command, error, completion, and assembly-plan data model. | C. |
| `src/cli/bootstrap.rs` | 375/0 | Bootstrap option parsing and top-level dispatch. | C. |
| `src/cli/basic.rs` | 341/0 | Built-in completion routes and candidates. | C. |
| `src/cli/completion.rs` | 294/0 | Completion request/result protocol. | C. |
| `src/cli/configured.rs` | 62/0 | Configured CLI expansion/completion entry points. | F. |
| `src/cli/search.rs` | 446/0 | Alternative search, result selection, and completion frontier. | C. |
| `src/cli/effects.rs` | 715/0 | Configured CLI freer-effect vocabulary and handler. | C; large single protocol. |
| `src/cli/host.rs` | 82/0 | CLI task-local snapshot/journal host state. | C. |
| `src/cli/token.rs` | 157/0 | Restricted token-parser state facade. | F/C. |
| `src/cli/token/effects.rs` | 264/0 | Token-parser effect vocabulary and handler. | C. |
| `src/cli/path.rs` | 140/0 | Path reader policy and completion. | C. |
| `src/cli/output.rs` | 136/0 | Human/NUL output formatting. | C. |
| `src/cli/adapters.rs` | 53/0 | Built-in shell completion scripts. | C. |
| `src/cli/tests.rs` | 0/1124 | CLI library tests. | TO. |
| `src/main.rs` | 2563/1404 | Process I/O, command execution, configuration, batch settlement, logger effect host/supervisor, and default rendering. | M/TH; strong private seams and relatively low public risk. |

## Hotspot dossiers

### 1. `api.rs`

The file combines at least five ownership domains: value/evaluator/net facade,
diagnostic bus and ingress, runtime lifecycle/readiness reports, transactional
runtime input/output state, and assembler/module construction. These are not
merely convenience methods on one type. Candidate children are `api/value`,
`api/diagnostics`, `api/runtime`, `api/events`, and `api/assembler`, with
`api.rs` retaining public re-exports. The runtime/event boundary needs a closer
dependency map before deciding whether those are siblings or one owns the
other. Public-path preservation and the large internal test module make this a
high-value but high-risk split.

### 2. `evaluation/coordinator.rs`

The coordinator owns several coherent internal protocols: exact completion
subscriptions, producer settlement obligations, per-kind work records and
claims, ready selection, and readiness/deadlock/settlement projection. Likely
children are `completion`, `work`, and `settlement`, but work-kind records are
strongly coupled to queue selection. Start by extracting passive snapshot and
settlement types or completion subscriptions, not by scattering coordinator
methods across arbitrary files.

### 3. `reflection.rs`

Four seams are strong: specialization/host transaction API, effect lifecycle
and launchers, the freer-effect task machine, and transaction/request decoding.
Existing `requests`, `search`, and `store` children already establish the
direction. A plausible shape is `reflection/{effects,lifecycle,machine}` while
the root re-exports the host-facing protocol. Continuation/cut/fix structures
must remain together. This split should reduce the current need for unrelated
machine and public-host items to share one namespace.

### 4. `evaluation.rs`

The production half owns waits/promises, task handles and status publication,
client demand, reflection task profiles, and session/context driving. Candidate
children are `wait`, `promise`, `task`, `client_demand`, and `session`.
Coordinator types currently cross several of these seams, so its dependency
map should be settled first or reviewed together. The 5,052-line inline test
module exaggerates production size but also contains clear feature clusters to
move with extracted responsibilities.

### 5. `main.rs`

The executable combines command dispatch/configuration, assembly and batch
settlement, configured logger effect handling, logger supervision, and default
diagnostic rendering. These are private binary concerns with strong seams and
little public-API risk. Candidate children under `main/` are `batch`, `config`,
`logger`, and `render`. This is probably the safest first implementation target
once call-direction and test placement are recorded.

### 6. `core.rs`

Deferred-value cells/failures, the runtime factory/cache, atoms/keys, general
values, functions/builtins, and list thunks are distinct concepts but highly
recursive. Future garbage collection and value-representation work may replace
their ownership boundary. A split could improve navigation, but should be
deferred unless it can preserve one-way dependencies without wrapper modules
or broad visibility.

### 7. Secondary candidates

- `reflection/store.rs`: query handles and conflict-analysis strategies could
  move out of the core snapshot/journal/store implementation.
- `interaction_net/runtime.rs`: protocol/snapshot types and mutable runtime/copy
  state could separate, but imminent performance work may reshape the seam.
- `g_syntax/parser/logical.rs`: original logical source/indexing and generated
  expansion replay form two plausible halves.
- `g_syntax/parser/structural.rs`: object/with parsing and let/where/using
  parsing are candidates only if shared floor/body machinery retains one owner.
- `g_syntax/parser/source.rs`: staged macro orchestration could separate from
  ordinary parsed-source assembly.
- `g_syntax/resolve/expression.rs`: dict/path resolution may separate from the
  general expression dispatcher.
- `g_syntax/compiler_values.rs`: cached helper families can become children,
  but only if the runtime cache remains centrally owned.

## Provisional ranking

1. **`main.rs`** — strongest private seams and lowest compatibility risk.
2. **`reflection.rs`** — strong conceptual seams and substantial navigation
   benefit, with moderate interpreter risk.
3. **`api.rs`** — greatest payoff, but requires the most careful public and
   ownership staging.
4. **`evaluation/coordinator.rs` plus `evaluation.rs`** — should be planned as
   one dependency review even if implemented in separate phases.
5. **Secondary parser/store/runtime candidates** — pursue only where a dossier
   confirms visibility or coupling improvement.
6. **`core.rs`** — defer until the GC/value-representation direction is clearer.

Large cohesive evaluator, lexer, parser, list, and test files are not split
merely to satisfy a line target. Dedicated test files may later be partitioned
for navigation, but that is a separate maintenance task rather than evidence
of production ownership failure.
