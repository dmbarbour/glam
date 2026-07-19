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
| `compiler.rs` | Per-source compiler capabilities: hidden source provenance, module identity, prior/final definitions, import loaders, and diagnostic emission |
| `g_syntax.rs` | `.g` front-end facade and source diagnostics |
| `g_syntax/parser/` | Layout, declarations, expressions, and compound syntax parsing |
| `g_syntax/resolve/`, `resolved.rs`, `analysis.rs` | Lexical resolution, affine semantic IR, capture discovery, and front-end warnings |
| `g_syntax/compiler_values.rs` | Closed functions, effect selectors, and built-in modules owned and shared by the built-in g compiler |
| `g_syntax/module_lowering/` | Declaration, import, definition, and object orchestration into a module value |
| `g_syntax/net_lowering.rs` | One-pass lowering of resolved expressions, functions, and applications into closed interaction nets |
| `core.rs` | Syntax-independent assembly values, lazy cells, function stages, keys, dictionaries, and builtin identifiers |
| `core_net.rs` | Core `Value`/`CoreOperator` specialization of generic interaction nets |
| `interaction_net.rs` | Facade for generic topology, checked templates, and mutable runtime reduction |
| `interaction_net/model.rs`, `builder.rs` | Packed ports/node identity, net agents, specialization protocol, and checked construction |
| `interaction_net/runtime/` | Runtime graph storage, active-pair rewrites, cursor materialization, and focused tests |
| `evaluation.rs` | Shared evaluation-session ownership and the cheap context threaded through evaluator work |
| `eval.rs` | Evaluation facade |
| `eval/value.rs`, `application.rs`, `operator.rs`, `net.rs` | Value forcing, application, semantic operator staging, and interaction-net driving |
| `eval/builtins/` | Small builtin dispatcher with implementations split by semantic family |
| `eval/sequence.rs` | List-to-binary observation and range extraction |
| `list.rs` | Generic compact/lazy persistent list ropes |
| `number.rs` | Exact-rational wrapper and public conversion boundary |
| `diagnostic.rs` | Diagnostic severity plus conventional `msg` values and assembler metadata records |
| `reflection.rs` | External opaque-request effect tasks, task-local control/state, cut transactions, and host-resource boundary |

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
       -> CompileContext qualifies relative names/imports using hidden provenance
       -> g_syntax explicitly interprets raw source bytes into ParsedSource
  -> g_syntax resolves and lowers declarations into a core Value
       -> the final-definition lazy cell closes the module fixpoint
       -> eval exposes the assembled module value
  -> Assembler::binary_at(module, "asm.result")
  -> main writes valid result bytes, drains assembler reasoning, then closes and joins logging
```

Inputs are processed from last to first so earlier command-line inputs override
later ones. Local source and binary imports re-enter the same `Assembler`
session through loaders installed in `CompileContext`; their diagnostics join
the originating build session.

Each source compilation receives an assembler-local invocation ID. A hidden
immutable trace links imported compilations to their parent invocation and
relative import request. Diagnostic callbacks and retained histories receive a
raw envelope containing the emitted value, severity, and that compact trace;
front ends never receive the trace. An observer calls `Diagnostic::enrich` to
project it into `msg.origin`, and may call `enrich_with` to add an independent
viewer context. Sources and requests are tagged values, `namespace` is globally
qualified, and `import_chain` contains ordered root-to-parent
`{importer,request,extends}` edges. Rendering is client policy; the executable's
default terminal logger is not part of `Assembler`.

`main` installs a queue-backed diagnostic sink before compiling configuration,
so bootstrap diagnostics are available to the configured logger. If `conf.log`
is defined, it runs through the generic external freer-effect task machine.
That machine owns the standard effects and delegates additional private request
tags to a `TaskSpecialization`. Reusable request families map into a host
specialization's request enum; the reflection family currently contributes
`log`, while `main` adds `log_status`, `read_log`, `write_stderr`, and their
shared atomic snapshot/journal data. `log_status` is the status of the reified
diagnostic stream only: it returns `'open` until `main` seals the input and
`'closed` afterward.
Failure becomes retryable only after a request observes changeable host state;
`cut` itself merely scopes alternatives and transactions. Consequently, plain
`.fail` remains permanent, while an empty `read_log` can suspend and replay from
the checkpoint immediately before its queue observation. `main` runs the
configured logger with an outer `(=>> .r ())` continuation, so `conf.log` must
return unit when it completes.
Otherwise the Rust terminal logger drains the queue. Normal early termination
or task failure also returns remaining messages to the fallback logger. A
configured-logger failure is itself rendered as the next default diagnostic
before that queue is drained and makes the process fail. The logger's task host
separates this incoming queue from a session-local diagnostic output target.
Logger `.log` writes remain transactional, but committed output is rendered by
the Rust default logger rather than being fed back to `.read_log`; output cannot
reopen the sealed input stream. Core operators only construct requests;
reflection state and external I/O are never performed by interaction-net
reduction. The standard handler stores its active reset stack as a private
entry in ordinary user state. Whole-state replacement therefore also switches
the delimited-continuation environment, which supports cooperative threads
within one reflection task; transaction and host-resource bookkeeping remains
outside that state.

The effect interpreter itself is resumable: persistent drive, delivery,
application, and cut frames advance under an effect-step budget. State failure
returns a coarse generation block instead of waiting inside the interpreter,
and lazy evaluator demand retains its exact wait token without selecting a
sibling alternative. `reflection::run` remains a synchronous wrapper that
polls this machine and performs the legacy host wait.

`EvaluationSession` can also own these machines through a type-erased polling
interface. Its serial pump removes one machine under the session lock, polls it
without that lock, then records its new state. It first follows the producer
chain for the demanded wait token, then uses a bounded FIFO ready queue, and
coarsely rechecks blocked tasks once per pump. Public assembler observation
also supplies a small bounded background budget after its foreground result.
For batch completion, `Assembler::drain_reasoning` instead runs without a step
or time limit. It keeps polling runnable work, including newly spawned tasks,
and stops only when every task is terminal or a complete pass leaves all
unfinished tasks unchanged. The latter returns a structured deadlock report;
terminal task failures are always included and are not acknowledged or cleared
by observation. Promise records retain their producer task IDs for shallow
dependency prioritization, and the report includes known dependencies, wait
tokens, and observed host generations. Fine-grained observation indexes,
persistent waiter graphs, worker threads, timed quiescence, and evaluator
reduction fuel are intentionally deferred.

The reusable reflection API exposes `.glam_ver`, `.os_env`, and `.cli_args` as
basic host information. `.dict_items` returns immediate key-ordered dictionary
entries as `{key,value}` records. `.eval Value` forces only the value's lazy
outer shell and returns the singleton result `ok:WHNF` or `err:Text`; pending
dependencies suspend the task rather than becoming errors. Tasks can reserve
`.refl_task Effect` children behind opaque handles. The compiler-provided
`eff.map` combinator sequences mapped effects left-to-right and preserves result
order; the g front end uses it to schedule named reflection tasks without a
dictionary-aware batch request.
`.join_task` returns success or propagates the child's error;
`.task_result` and `.task_error` are read-only, symmetric state-specific
extractors, and `.task_status` provides a nonblocking status atom. Pending
extractors are failed effect choices carrying the child's exact wait token.
Transaction journals apply task launches and `.cancel_task` requests in effect
order only after the winning outer commit; abandoned journals cancel unused
reservations and discard cancellations.

An ordinary `Assembler` installs a reflection-task launcher when it creates its
session. The launcher wraps annotation effects in the reusable
`ReflectionEffects` specialization: standard effects plus `.log`. Its host
stores the session's reflection heap and sends diagnostics to the assembler's
configured sink. CLI-only logger-consumer requests remain in `main`'s separate
`MainEffects` specialization. Children spawned by that logger still receive
only `ReflectionEffects`, so they inherit session-local `.log` output without
receiving `.read_log`, `.log_status`, or raw `.write_stderr`. After the parent
terminates, the composed-task runner drains all of its scheduled children
without a time limit. A child failure or stable child deadlock fails the
configured logger.

In batch mode, `main` writes a valid `asm.result` before draining the assembler
session. It turns every task failure and a stable quiescent session into error
diagnostics, seals the log input, and waits for `conf.log`. The logger can use
`.log_status` alongside `.read_log` to define its shutdown policy. A monotonic
error count is independent of queue retention and consumption, so dropped or
rendered error diagnostics still produce a nonzero exit status.

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
Normal g compilation also wraps ordinary module definitions in one-shot demand
boundaries that scan the final `refl.*`; members of named top-level objects and
their nested declared objects use the same convention against final self.
Object guards derive from final `spec.name`, so inherited mixins scan the
derived object's overridable reflection namespace and extensions retain the
same one-shot identity. `refl`, `meta`, and `spec` remain inert, object values
do not trigger their host scope, and expression-local objects receive no
automatic boundary. This is g-syntax lowering policy, not an assembler or
evaluator interpretation of the name `refl`.

## Evaluation and Application Flow

An `Assembler` owns one `EvaluationSession`; its clones share that session.
Each evaluator entry borrows an `EvalContext` pointing to it, including work
performed later by a lazy value. The session owns reflection task identity,
queued/blocked/terminal state, and the serial cooperative executor; later
slices will add the shared heap, diagnostics, and cancellation facilities at
the same boundary.

The evaluator exposes outer semantic values on demand:

```text
Assembler -> EvalContext -> Value
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

Reflection fixpoints fulfilled by the effect interpreter are task-owned lazy
cells. Ordinary `fix` and object-self knots use computed fixpoint cells instead:
the first observer claims production, recursive demand by that active producer
fails, and a suspended producer retains ownership while other tasks receive the
cell's stable wait token. Anonymous assignment holes used by module construction
and the deferred-list effect remain `Promised` values and retain the bootstrap's
fail-fast observation rule.

An undersaturated source function produces another `FunctionValue` sharing a
curried runtime stage. Saturation produces a memoized computation. Explicit
`Value::Net` is different: application attaches a logical copy of its exposed
port and may leave a residual non-data net. A net-backed lazy computation, by
contrast, must expose `Data` when forced.

The singleton annotation `refl:Effect` constructs a boxed lazy gate. Its first
observer registers one task in that observer's session and receives a precise
wait until the task completes; it then yields the original target without
forcing it. A `Data >< Bind` demand records the same wait in the exact active
pair rather than turning suspension into a stuck error. Gate observation now
offers that wait to the serial pump. Completion exposes the original target
without forcing it, but only after verifying that the effect returned unit;
the implicitly discarded non-unit result is a task error. Bare standalone
evaluator contexts retain dormant records for low-level tests, while ordinary
assembler annotations launch executable machines immediately on demand.

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
