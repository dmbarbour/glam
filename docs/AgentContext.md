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
  specialization's request enum. The reusable reflection family contributes
  `glam_ver`, `os_env`, `cli_args`, `dict_items`, `log`, and the task operations
  `refl_task`, `join_task`, `task_result`, `task_error`, `task_status`, and
  `cancel_task`;
  `main` adds provisional `read_log` and `write_stderr` effects. OS environment
  values and command-line arguments preserve Rust's platform encoding as binary
  values rather than forcing UTF-8. Spawned tasks receive only
  `ReflectionEffects`, even when their parent has broader host capabilities.
  `join_task` propagates terminal task errors; `task_result` and `task_error`
  symmetrically extract only their terminal state and otherwise fail.
  `task_status` returns `'pending`, `'complete`, `'error`, `'foreign`, or
  `'canceled`. Pending extractors fail observably, so an exhausted choice
  suspends on that task's exact wait token and retries on any terminal transition. Logged
  diagnostics join the current transaction or go directly
  to the host outside `cut`. Queue reads inspect only their host snapshot, never
  journaled writes, and yield failure when no input is available. That failure
  retains the queue observation, so the task waits for a host change and retries
  even outside `cut`. A
  top-level `alt` is rejected; alternatives belong to `cut`. This and
  task-local `.shift` continuations are the conservative
  standard-effect contract: general-purpose utilities must not assume broader
  behavior, although a specialized handler may explicitly provide it.
- `dict_items` is the narrow privileged dictionary-iteration boundary. It
  exposes immediate entries as key-ordered `{key,value}` records. The g
  compiler's `eff.map` helper sequences mapped effects left-to-right and
  preserves result order; batching does not require a separate reflection
  request.
- The Rust bootstrap treats the built-in g compiler like one ordinary shared
  compiler value. `g_syntax/compiler_values.rs` lowers its closed effect
  selectors, helper functions, and built-in modules once. Cached helpers are
  normalized far enough to expose stable data or function values before they
  are shared across evaluator sessions; unresolved lazy computations are not
  process-global. Module paths, environments, final-definition promises, and
  reflection tasks remain module- or session-local.
- `ReflectionEffects` is the reusable annotation specialization: standard
  effects plus `.log`, with no logger-consumer operations such as `read_log` or
  `write_stderr`. Every ordinary `Assembler` session installs a type-erased
  launcher for that specialization. Its host owns the session's current
  reflection heap and routes emitted diagnostics to the assembler's configured
  sink. Replacing an assembler's diagnostic sink creates a fresh evaluation
  session so the launcher cannot retain the prior sink.
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
- `refl_task` reserves an opaque task handle immediately but journals the
  launch inside a transaction. `cancel_task` is another commit-ordered task
  journal entry. Only the winning outer commit applies either request;
  discarded journal branches cancel unused launch reservations and discard
  cancellation requests. A cancellation that reaches a terminal task is a
  no-op. Outside a transaction, scheduling or requesting cancellation is an
  immediate retry barrier when it changes task state.
- The built-in g front end decorates ordinary module definitions with a shared
  demand trigger for the final module `refl.*`. Members of named top-level
  objects and their nested declared objects similarly trigger their final
  object-local `refl.*`; an object value itself does not trigger reflection in
  its host scope. Object scanner guards derive from final `spec.name`, so
  inherited definitions use the derived object's final reflection namespace
  and direct extensions share that object's one-shot guard. The `refl`, `meta`,
  and `spec` subtrees, computed-root definitions, and object expressions remain
  inert. One closed compiler helper implements the boundary protocol; it is
  partially applied once per module or declared object and shared by that
  boundary's definitions. The demand transaction claims the boundary by
  storing a scanner task handle. That scanner waits for the final `refl.*`,
  then atomically launches its named tasks and stores their ordered
  `{key,task}` records under the boundary's `tasks` state entry. Every named
  task is wrapped to require a unit result. Keeping the scanner `claim`
  separate remains meaningful when the resulting task list is empty and avoids
  inspecting a module's final definitions while constructing it. This
  decoration is unconditional g-front-end lowering policy; `CompileContext`
  does not select it, and direct lowering has the same semantics as normal
  compilation.
- `cut` supplies choice and transaction scope, not retryability. A failed branch
  may retry only if it observed changeable host state: `.fail` and `.cut .fail`
  are permanent failures, while an empty `read_log` is retryable. Outside a
  transaction, the task checkpoints immediately before its first observation;
  an immediate host commit clears that checkpoint so retry cannot duplicate the
  committed effect.
- Reflection choice and lazy demand are deterministic. `.alt` explores its next
  branch only after failure; blocking on a lazy value suspends the current branch
  and therefore leaves a task waiting on at most one lazy value. The same task
  may also retain multiple prior state observations. Any future racing or
  nondeterministic choice must be an explicit effect distinct from `.alt`.
- `EffectTask` is a persistent cooperative machine. Its drive, delivery,
  application, and nested-cut frames survive `poll` calls; evaluator waits are
  suspension tokens rather than textual task failures. A blocked task reports
  at most one lazy dependency plus the current coarse host generation. If both
  can wake it, a changed host generation restarts the recorded transaction or
  retry boundary before the lazy continuation is resumed. One poll step must
  not leave the machine positioned to repeat an already committed host effect.
- `EvaluationSession` stores executable tasks behind a type-erased machine
  interface, plus a FIFO ready queue and stable task/wait lookup. The serial
  pump claims a machine under the session mutex, polls it outside the mutex,
  and then restores its queued, blocked, or terminal state. It prioritizes the
  producer of a demanded wait token, follows a blocked task's one lazy
  dependency when that producer is known, and otherwise polls ready tasks in
  bounded round-robin order. A per-pump visited set stops dependency cycles and
  repeated polling of unchanged blocked tasks. Public assembler evaluation and
  extraction operations also grant a small bounded background budget after
  their foreground result, so scanners and short tasks progress without making
  long-lived tasks synchronous. Fine-grained wake indexes, worker threads, and
  evaluator reduction fuel remain deferred.
- `reflection::run` is the synchronous compatibility driver for that machine.
  It polls with bounded effect-step fuel and calls `TaskHost::wait_for_change`
  only after polling reports a state block. `run_with_reflection_host` also
  installs the restricted child-task launcher and cooperatively pumps scheduled
  lazy dependencies; a dependency with no runnable producer remains an error.
- Each reflection `fix` alternative receives its own task-owned fixpoint cell.
  Recursive observation by its producer is an error; another task observes its
  precise wait token. When a chosen result later fails, the handler restarts at
  the fixpoint boundary and replays its transactional `alt` choices rather than
  reusing an initialized cell. Task termination fails any unfulfilled cells it
  still owns.
- `main` owns the diagnostic queue, logging request dispatcher, and logging
  transaction snapshot/journal. Defined `conf.log` consumes enriched messages
  effectfully; undefined, completed, or failed custom logging falls back to the
  Rust terminal logger. If configured logging fails, the terminal logger first
  renders one synthetic error diagnostic, then drains the remaining queue in
  FIFO order. Stderr effects commit to a host buffer before bytes are written
  to the OS.

### Values and evaluation

- Production evaluation consumes only closed `Value`s. The small fixture IR in
  `eval/test_support.rs` must lower to interaction nets before evaluation; do
  not restore a second expression interpreter or thread a local environment
  through evaluator APIs.
- Production evaluator entry points receive an explicit `EvalContext` borrowed
  from their caller. `Assembler` clones share one `EvaluationSession`, while
  standalone tests and reflection tasks create their own. Deferred work must
  receive the active context when forced; do not capture whichever session
  happened to construct the lazy value.
- `Value::Net` is an explicit first-class closed net. `Value::Lazy` is a
  memoized computation whose net-backed forms must expose `Data` when forced;
  an exposed `Bind` is an error, not an implicit function conversion.
- `Value::Function` is an independently observable curried function stage.
  Partial application shares the staged runtime; saturated application returns
  a memoized computation.
- Ordinary `fix` and object-self knots are computed fixpoint cells, claimed by
  their first observing evaluation task. Recursive demand while that task is
  actively producing the cell is an error. If production instead suspends on
  other work, the same task may resume it while other tasks wait on the cell's
  stable token. Do not replace this state with a stack guard: blocked evaluation
  unwinds the Rust stack before the task is scheduled again.
- `list.rs` owns compact persistent list ropes. Keep `Bytes` as compact leaves;
  lazy holes are opaque to the list and are forced only through evaluator-owned
  operations.
- The current dictionary/access evaluator is compatibility code. Preserve its
  behavior until a first-class persistent lazy dictionary design replaces it.
- Anonymous `Promised` lazy cells used by module assembly and the deferred-list
  effect currently fail if observed before assignment. Do not silently turn
  those remaining producer-less holes into blocking joins; migrate each use to
  an explicit producer model when its scheduling semantics are defined.
- A reflection annotation is a boxed lazy gate, not reflection performed by an
  interaction-net operator. Its task handle and result are monotonic cells;
  mutable queued/running/blocked state belongs to `EvaluationSession`. Task
  waits must not be cached as lazy failures, and successful gates return their
  original target without forcing it.
- The first observer owns a reflection gate's running task. A different session
  must not drive it, though either session may consume the completed result.
  Wait tokens retain only a weak reference to the owner. The evaluator invokes
  the serial pump when it observes a pending gate. A successful annotation task
  must return the canonical unit value, because the annotation implicitly
  discards its result; any other value fails the gate. Success exposes the
  original target without forcing it. Bare standalone evaluator contexts have
  no launcher and retain dormant task records only for focused manual tests.

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
