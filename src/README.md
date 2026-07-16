# Bootstrap Implementation Map

This document is a navigation aid for the current Rust bootstrap. It describes
where data and control move today; it does not define the eventual language.
See [`docs/DistilledDesign.md`](../docs/DistilledDesign.md) for target semantics
and [`docs/AgentContext.md`](../docs/AgentContext.md) for regression-sensitive
implementation constraints.

## Entry Points and Module Ownership

| Path | Current responsibility |
| --- | --- |
| `main.rs` | CLI parsing, configuration/assembly policy, diagnostic rendering, and writing `asm.result` |
| `lib.rs`, `api.rs` | Embedding facade: opaque values, host capabilities, module assembly, diagnostics, evaluation, binary extraction, and checked net construction |
| `compiler.rs` | Per-source compiler capabilities and context: module identity, prior/final definitions, source bytes, and import loaders |
| `g_syntax.rs` | `.g` front-end facade and source diagnostics |
| `g_syntax/parser/` | Layout, declarations, expressions, and compound syntax parsing |
| `g_syntax/resolve/`, `resolved.rs`, `analysis.rs` | Lexical resolution, affine semantic IR, capture discovery, and front-end warnings |
| `g_syntax/module_lowering/` | Declaration, import, definition, and object orchestration into a module value |
| `g_syntax/net_lowering.rs` | One-pass lowering of resolved expressions, functions, and applications into closed interaction nets |
| `core.rs` | Syntax-independent assembly values, lazy cells, function stages, keys, dictionaries, and builtin identifiers |
| `core_net.rs` | Core `Value`/`CoreOperator` specialization of generic interaction nets |
| `interaction_net.rs` | Facade for generic topology, checked templates, and mutable runtime reduction |
| `interaction_net/model.rs`, `builder.rs` | Packed ports/node identity, net agents, specialization protocol, and checked construction |
| `interaction_net/runtime/` | Runtime graph storage, active-pair rewrites, cursor materialization, and focused tests |
| `eval.rs` | Evaluation facade and shared evaluator context |
| `eval/value.rs`, `application.rs`, `operator.rs`, `net.rs` | Value forcing, application, semantic operator staging, and interaction-net driving |
| `eval/builtins/` | Small builtin dispatcher with implementations split by semantic family |
| `eval/sequence.rs` | List-to-binary observation and range extraction |
| `list.rs` | Generic compact/lazy persistent list ropes |
| `number.rs` | Exact-rational wrapper and public conversion boundary |
| `diagnostic.rs` | Shared diagnostic severity |

The detailed interaction-net invariants live in
[`docs/agent_context/interaction_nets.md`](../docs/agent_context/interaction_nets.md),
not here.

## Module Assembly Flow

The ordinary CLI path uses only the embedding facade:

```text
main
  -> Assembler::module(module_path)
  -> ModuleBuilder + ModuleInput values
  -> api::Assembler::build_module_inner
       -> Host reads each source
       -> CompileContext records source/module/import context
       -> g_syntax parses SourceFile into ParsedSource
       -> g_syntax resolves and lowers declarations into a core Value
       -> the final-definition lazy cell closes the module fixpoint
       -> eval exposes the assembled module value
  -> Assembler::binary_at(module, "asm.result")
  -> main writes bytes and drains retained diagnostics
```

Inputs are processed from last to first so earlier command-line inputs override
later ones. Local source and binary imports re-enter the same `Assembler`
session through loaders installed in `CompileContext`; their diagnostics join
the originating build session.

`main` chooses the `configuration` and `assembly` module paths and constructs
their initial definitions. Those names and roles are CLI policy, not library
policy. `--parse` is the one temporary exception to the facade boundary: it
calls the front end directly for inspection until reflection provides that
view.

## Front-End Dataflow

```text
source bytes
  -> parser-owned SyntaxExpr and declaration nodes
  -> resolver-owned BindingId locals and ResolvedExpr<Value>
  -> module lowering or net lowering
  -> closed core Value / FunctionCode / NetValue
```

`SyntaxExpr` describes spelling and sugar. `ResolvedExpr<Value>` is a private,
affine semantic IR: consumers move its children rather than clone and re-lower
them. Resolution discovers lexical bindings and explicit function captures;
net lowering emits one bind spine for a whole source function and one operator
chain for a maximal application spine. No syntax or core expression tree
survives into evaluation.

Module lowering owns declaration order and the open module fixpoint. It routes
ordinary expressions through resolution/net lowering, imports through compiler
loaders, and object declarations through the object lowering helpers. The
front-end facade returns only lowered definitions plus source diagnostics.

## Evaluation and Application Flow

The evaluator exposes outer semantic values on demand:

```text
Value
  -> eval_value / force_value_shell
       -> return already-observable data
       -> force and memoize LazyValue work
       -> drive a net-backed computation until its interface exposes Data

apply(function, arguments)
  -> builtin/partial builtin staging, or
  -> shared FunctionValue curried stage, or
  -> explicit Value::Net cursor attachment, or
  -> dictionary applicability compatibility
```

An undersaturated source function produces another `FunctionValue` sharing a
curried runtime stage. Saturation produces a memoized computation. Explicit
`Value::Net` is different: application attaches a logical copy of its exposed
port and may leave a residual non-data net. A net-backed lazy computation, by
contrast, must expose `Data` when forced.

Builtins are identified in `core`, dispatched once in `eval/builtins.rs`, and
implemented by semantic family below `eval/builtins/`. Net-lowered application
uses inspectable `CoreOperator` values rather than Rust closure agents.

## Interaction-Net Control Flow

`NetBuilder` validates an immutable `InteractionNet` template. Instantiation
creates a `SharedRuntimeNet` with a stable interface around the exposed port.
The runtime records every principal-principal wire in one active-pair map.

```text
evaluator asks for one reduction
  -> runtime claims one exact active pair under its mutex
  -> topology-only rule rewrites it immediately, or
  -> evaluator performs callable/operator/cursor work without the mutex
  -> runtime finishes, blocks, or marks that same pair stuck
```

A logical copy is represented by one-way remote cursors owned by the target
runtime. Demand on a cursor may materialize a stable source node or drive one
exact dependency in the source. Source active pairs are reduced in the source;
they are never copied into the target. Source and target runtime locks are not
held together.

## Test Navigation

- Parser-only tests sit beside parser submodules under `g_syntax/parser/`.
- Cross-front-end tests live in `g_syntax/tests.rs`.
- Runtime topology and cursor tests live in
  `interaction_net/runtime/tests.rs`.
- Evaluator integration tests live in `eval/tests.rs`; shared fixtures are in
  `eval/test_support.rs`.
- `tests/` covers the public facade, CLI behavior, samples, and invalid source
  fixtures.
