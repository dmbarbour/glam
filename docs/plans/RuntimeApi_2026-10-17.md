# Runtime API Plan — 2026-10-17

Status: `api-runtime-turns` built 2026-10-17 and `api-cli-bridge`
2026-10-18 (see [As built](#as-built)). The CLI's transition was re-planned the
same day, after a survey of the CLI (see [CLI Transition](#cli-transition)):
`api-cli` gave way to a bridge and a CLI slice in each later step, moved
piecemeal (maintainer, 2026-10-17). On 2026-10-18 the maintainer agreed
effect providers (option A), which tie into the queue-based effects of the
[runtime ownership plan](RuntimeOwnership_2026-10-17.md), and its remaining
steps merged here (see [From the ownership plan](#from-the-ownership-plan)).
The same day the maintainer clarified that a turn is no transaction: a cut
bounds transactions and search (`turn-cuts-bound-transactions-and-search`),
and a turn runs an effect by evaluating it. They agreed the bridge, and the
CLI prelude as an experiment. Drafted 2026-10-17 from the
[runtime API review](../reviews/RuntimeApi_2026-10-17.md), whose direction
the maintainer agreed the same day (`runtime-api-turns-on-machines`). Names
and communication's conventions are discussed as each step reaches them.

## Goal

A public API in which a Rust thread acts within the runtime: it opens a
thread-affine runtime and runs turns on machines named by user-chosen keys,
each with a stack and local state of glam values. Each action on shared
state is atomic, and a cut groups actions into a transaction that also
bounds a backtracking search. Fork-join comes from scoped threads, and tasks
and machines communicate only through the shared heap or volumes. The
handle API goes.

## Principles

- **Over the existing internals first.** The runtime already works in
  scopes: heap regions, staged transactions, persistent effect machines.
  The new API is a facade over them before it reshapes them, and internal
  unit tests may keep internal types throughout.
- **One heap region per operation,** never one across user code: waits and
  callbacks happen outside regions (`no-recursive-heap-entries`).
- **Glam values only** in a machine (maintainer, 2026-10-17).
- **Scoped threads only** (maintainer, 2026-10-17); relax later on a strong
  use case.
- **Measure before accelerating:** glam code starts with the basic heap
  operations, and specializes as profiling shows (maintainer, 2026-10-17).
- **The CLI moves piecemeal** (maintainer, 2026-10-17): each step moves
  the part of the CLI that its operations cover, and the rest stays on the
  handle API until a later step covers it.
- **Rust code runs only on the threads that take turns,** never on a
  worker (the lifecycle review's decisions 3 and 4, 2026-10-16). Workers
  run runtime code; what a background task needs from Rust crosses as data,
  through the heap and request queues, to a machine that answers it.
- **Communication is transactional operations on shared state**
  (maintainer, 2026-10-18). Queues, data buses and publish-subscribe are,
  logically, transactions on state in the heap or a specialized volume. How
  far the API specializes, abstracts and accelerates them is open, since
  each choice trades performance, reasoning and robustness. The minimal
  API, a shared heap alone, would leave optimizations on the table.

## Steps

1. `api-runtime-turns` — `Runtime`, turns, and a minimal machine, over the
   existing `EvaluationRuntime` and `Assembler`, which it holds privately.
   - `Runtime::open(config)`; the runtime is `!Send` but `Sync` (a marker
     such as `PhantomData<MutexGuard<'static, ()>>`), and dropping it
     retires the runtime through today's path.
   - `rt.run(key, |m| …)` with `key: impl Into<Key>`; machines' stacks and
     states live in the runtime by key, rooted, an empty machine storing
     nothing.
   - Machine operations: pushes and pops of Rust data (unit, numbers, text,
     bytes, atoms), stack operations (`dup`, `swap`, `drop`, `depth`),
     `access`, `apply`, `eval`, `compile`, local state (`get`, `set`), and
     `clear`.
   - A `stash` beside the stack (maintainer, 2026-10-17): `stash(n)` moves
     the top `n` values there as a block, and `stash_pop(n)` moves them
     back, so the pair leaves them as they were.
   - Tests: the Hello World sample's bytes through a turn; state across
     turns; `clear`; errors on underflow and on a pop of the wrong kind; a
     compile-fail test that `Runtime` is not `Send`.
2. `api-cli-bridge` — the CLI holds a `Runtime`, and reaches the handle API
   through a bridge for what has not moved (see
   [The bridge](#the-bridge)).
   - A hidden module, `glam::transition`, with four functions: a `Runtime`
     around an `Assembler` the caller built, that runtime's assembler, and
     a `Value` pushed onto or popped from a machine. The last two come with
     their first use, in `api-machine-data`.
   - `Runtime::activate_workers(n)`, since the configured CLI learns its
     worker count from the configuration after the runtime opens; and
     `Runtime::profile()` under `glam-prof`.
   - The CLI's main thread opens the runtime and drops it last, and
     `PreparedAssembly` holds it. Behavior does not change.
3. `api-machine-data` — operations that build and read data, and the CLI's
   first slice.
   - Building: `list(n)`, `dict(names)`, `anno`, and a push of an
     abstract global path; `compile` from several inputs, over initial
     definitions popped from the stack.
   - Reading, for consumers, which evaluate: a list's items onto the stack,
     a dictionary's entries, a value's kind, an atom's name, and equality
     of representation.
   - CLI slice: loading the configuration, the assembly, selecting
     `conf.log`, and the configured completion script run in turns on a
     `cli` machine, whose state holds the configuration.
   - Experiment, `api-cli-prelude`: the CLI's policy moves into a glam
     prelude, kept if it proves good in practice (see
     [Policy in glam](#policy-in-glam-experiment)).
4. `api-cuts` — a turn's transactions and backtracking search
   (maintainer, 2026-10-18; `turn-cuts-bound-transactions-and-search`). A
   turn is no transaction: each action on shared state is atomic by
   itself, without the setup of an isolated transaction.
   - `m.cut(|t| …)` groups actions into one atomic, isolated transaction:
     shared heap reads and writes staged in a runtime transaction, the
     machine's stack and state restored on failure, and `t.retry()` waiting
     until a read changes. Cuts nest.
   - As `.cut` does in reflection tasks, a cut also bounds a backtracking
     search: a failure undoes what the cut did, so the caller may try
     another alternative. Rust code chooses alternatives by ordinary control
     flow, or by nested cuts; it is not restricted to backtracking.
   - Rust side effects within a cut must tolerate a rerun, wait for
     commit, or be undone on abort, through hooks a cut registers
     (maintainer, 2026-10-18). Commit hooks run in the order registered
     once the outermost cut commits. Abort hooks run in reverse order as a
     failure backtracks through them to a cut, including those of nested
     cuts that had committed into it. Like their counterparts in C
     transaction libraries, they are best effort: they make compensation
     convenient, not certain.
   - Beside it, `api-virtual-resources`: a cut reads and writes *virtual
     resources*, proxies named by key for host resources the runtime does
     not see (maintainer, 2026-10-18). Conflict analysis covers them, and a
     retry that read one wakes when a later cut writes it, so Rust code
     that observes a host change writes its proxy to wake the waiters.
   - No CLI slice: the CLI's transactional code is the logger's, which
     moves with heap communication.
5. `api-scoped-threads` — `rt.scope(|s| …)`, fork-join within a turn, and
   waiting. One turn at a time per machine: a second waits or fails, or
   observes the machine's status by key and so waits transactionally
   (maintainer, 2026-10-17: perhaps both). Ordering tests force each
   interleaving.
   - CLI slice: the logger's thread becomes a scoped thread that borrows
     the runtime, so no other thread owns it.
6. `api-heap-communication` — state and logs in the heap or a volume, with
   readers' cursors in their own state; abstract global paths as
   capabilities; volumes allocated through the reflection API. Retention,
   backpressure and discovery are settled here, after a look at Kafka, DDS
   and MQTT conventions.
   - CLI slice, diagnostics: the CLI appends diagnostics to a log in turns.
     `DiagnosticBus`, `LogHost` and the supervisor's fallback endpoint
     retire.
   - CLI slice, loggers: the default logger is a machine on a scoped thread
     that reads the log and applies the glam formatter, into which the
     Rust frame summaries of `rendering.rs` move. A configured logger reads
     the log with heap operations and appends its stderr output to a log
     that a CLI machine drains, so `MainEffects` retires.
   - CLI slice, environment: `process.args` and `process.refl_args` become
     state at paths that the CLI writes after expansion, in place of
     promises.
7. `api-request-queues` — absorbs `lifecycle-queues`. A request queue is,
   logically, transactional operations on a log in the heap or a volume,
   whose elements pair a value with an optional promise, so writes and
   queries mix in one queue (the lifecycle review's decision 8). Whether
   the API specializes or accelerates it beyond heap operations is decided
   by measurement.
   - A request posted within a cut becomes visible when the cut commits,
     so a failed cut sends nothing; one posted outside a cut is visible at
     once. A queue's order is commit order; queues promise no order among
     themselves, and resolvers need not resolve in the order read.
   - The requester holds the promise. It takes the result as a promised
     value, or observes the request's status (uncommitted, sent, broken,
     fulfilled) and awaits a change with a cut's retry.
   - Resolvers, like volumes, are opaque glam values (maintainer,
     2026-10-18). Nothing affine stops a program from resolving one twice,
     so the runtime enforces single assignment: a second resolution fails.
     A resolver collected unresolved breaks its promise, through the
     collector's finalization. A resolver's holder may read the request's
     status without resolving it, and break the promise itself rather than
     wait for collection (maintainer, 2026-10-18).
   - Tests: a failed cut sends nothing; a queue's order matches commit
     order; a second resolution fails; a collected resolver, or one broken
     by hand, breaks the request; a resolver reads the status; a status
     change wakes a waiting retry.
8. `api-settlement` — `rt.settle()` runs until no work and no other turn
   remains, services maintenance, kills a deadlock, and appends the
   runtime's reports (task, delivery and maintenance failures, exit errors,
   killed work) to a log. Turns in progress are known, so settlement
   counts them, which the handle API could not.
   - CLI slice: the CLI's two settlement loops become one, and its exit
     status reads the report log.
9. `api-hosts-as-machines` — absorbs `lifecycle-host-messages` and
   `lifecycle-host-calls` (L3, L7, L8, L9). The reflection hosts
   (`AssemblerReflectionHost`, `LoggerTaskHost`, `IsolatedTaskHost`) reduce
   to queues and store state that machines serve, and `TaskHost`'s
   `snapshot`, `commit` and `validate` hooks retire (decision 6). The
   assembler host's diagnostics become appends to the diagnostic log, no
   longer published during a commit, on a worker.
   - A forced import's file load becomes a request whose promised value is
     the import (decision 7), so workers neither call a `SourceSystem` nor
     upgrade `DeferredAssembler`, which retires. L9's nesting, a macro
     search within an import, stays: it is inherent to lazy imports.
   - CLI slice: a CLI machine serves the import queue and records the
     reads, for the manifest and the final check that no file changed. The
     CLI opens its runtime with `Runtime::open`, and the bridge's
     constructor goes.
10. `api-effect-providers` — absorbs `lifecycle-leases`. A turn runs a
    monadic effect by evaluation, on its own thread (option A, maintainer,
    2026-10-18; see [Effect providers](#effect-providers)).
    - The turn supplies the effects API, evaluates the program to weak-head
      normal form, and observes either a yielded request with its
      continuation or the final result. It interprets a request, and the
      answer resumes the continuation.
    - Helpers interpret the standard effects (`.r`, `.seq`, `.alt`,
      `.fail`, `.cut`, `.fix`, `.get`, `.set`, `.reset`, `.shift`, and the
      heap's), so a user handles only the unique ones. `.alt`, `.fail` and
      `.cut` map to the turn's cuts. The helpers start by running today's
      effect machine on the turn's thread, returning to Rust only for
      unique requests (maintainer, 2026-10-18); their performance is
      `perf-reflection-effects`'s concern, as an independent task.
    - A *provider* is the Rust code that answers the unique requests. It
      receives per-call capabilities and stores none, so none holds an
      owner (L7). It answers what is local to its run, such as a cursor in
      argv or a query of the file system; a background task's requests go
      through queues, and shared state through the heap.
    - CLI slice: the configured CLI's `CliEffects` and `TokenEffects` become
      providers, their per-branch journals become machine state that cuts
      restore, and `IsolatedTaskHost` and `IsolatedEffectSearch` leave the
      public API.
11. `api-handle-retirement` — the public handle types go: `Assembler`,
    `ModuleBuilder`, readiness results, `EffectRun`, endpoints and buses,
    and `Values` as a separate handle. The bridge goes with them, and the
    public tests move to turns (see
    [Parity](#parity-for-handle-retirement)). Standing docs describe the new
    API.
12. `api-retirement` — absorbs `lifecycle-owner`'s retirement protocol (L1,
    L5, L6, L10). With the handles gone, the `Runtime` is the only owner,
    and dropping it retires in order: admit no new work; stop, wake and
    join the workers; fail pending operations as retired; close sessions
    and work records, dropping stored providers and closures; release the
    background demand and retire the queues; retire the value domain. No
    turn is in flight, since scoped threads join before the drop.
    - Tests force each ordering: retirement with work queued, a pending
      operation failing as retired, and values inert afterwards.

Two steps from the ownership plan stand beside these, under their names:
- `lifecycle-audits` grows with each step: a source audit or compile-fail
  tests showing that code on workers cannot name the `Runtime` or call
  Rust code, and ordering tests for the L3 and L4 shapes until the steps
  that remove them land.
- `lifecycle-external-owners` checks L11, independently.

`api-runtime-turns` comes first, then the bridge. The data operations
precede the CLI's first slice. Cuts precede heap communication,
whose logs request queues extend; scoped threads precede the loggers' move;
settlement follows the logs it appends to. Hosts follow queues, since their
requests are queue elements, and precede effect providers, which take what
remains of the hosts' role. Handle retirement precedes the retirement
protocol, which needs a sole owner.

## CLI Transition

A survey at `2955aee8` found that the CLI, about ten thousand lines under
`src/bin/glam`, uses the handle API for five concerns. The bootstrap
commands (help, version, `--parse`, the manifest check, basic completion)
need no runtime, and stay as they are.

| Concern | Today | Needs | Steps |
|---|---|---|---|
| Preparation | `FileSourceSystem`, `EvaluationRuntime`, `DiagnosticBus`, and an `Assembler` whose reflection environment holds two promises | an owner, then the environment and sources in their target form | 2, 6, 9 |
| Configuration and assembly | compiles `configuration` and `assembly` over initial definitions; access, `defined_or`, `context:` annotations, evaluation to bytes | data operations | 3 |
| Logging | `DefaultLogger` renders as a subscriber; a configured logger is an `EffectRun` on its own thread with `MainEffects` (`read_log`, `write_stderr`), supervised, with a fallback endpoint | scoped threads, logs | 5, 6 |
| Settlement | two loops over readiness, maintenance, deadlock kills and quiescence reports | `rt.settle()`, a report log | 8 |
| Configured CLI | `conf.cli` runs as an isolated effect search over argv: `CliEffects`, with nested `TokenEffects` searches, per-branch journals, and a search again for each completion candidate | effect providers | 10 |

### The bridge

Each slice moves while the rest still runs, and both work on one runtime,
so values cross between them: a slice built on turns hands the
configuration to the logger, which still runs as an `EffectRun`. The bridge
is that crossing, kept small and visible:
- **Hidden and named:** `glam::transition`, outside the documented API,
  whose four functions each name the module where they are used. Their uses
  in the CLI count what remains to move, and each step's "As built" records
  the count.
- **The CLI's alone:** the library's tests and the public tests never use
  it (`tests-follow-target-architecture`).
- **Retired by plan:** the constructor goes with `api-hosts-as-machines`,
  and the rest with `api-handle-retirement`, or sooner when no use remains.

The alternative, the CLI moving whole once turns cover all of it, would
leave it on the handle API until `api-handle-retirement`, with nothing
moved piecemeal.

*Agreed (maintainer, 2026-10-18):* the bridge, since holding the CLI back
until a late step would hinder validating the new API as it grows.

### Policy in glam (experiment)

The CLI builds records in Rust today: the assembly's initial definitions
and their journal records, `conf.entry` contexts, path-lookup annotations,
terminal viewers. A CLI prelude can hold them instead: a glam module in the
binary, compiled in a turn and kept in the `cli` machine's state.
`anno context:Frame Expr` and `abstract_global_path` are syntax, and the
compiler's `defined_or` helper is a match on `{}`, the undefined value,
which glam code can write. Rust then pushes argv and paths,
applies the prelude's functions, and pops bytes, so the machine's
operations stay general, as any embedder needs them, rather than shaped
by the CLI.

*Maintainer, 2026-10-18:* a good idea in theory; try it, as
`api-cli-prelude`, and see whether it is one in practice. It is judged on:
- the prelude's compile cost per run, by `hello_elf`'s instruction count;
- whether the CLI's Rust shrinks to pushes, applications and pops, and the
  prelude reads as plainly as the Rust it replaces;
- whether errors in the configuration still point to the user's source
  rather than into the prelude.

If it does not hold up, the policy stays in Rust, built with the data
operations.

### Effect providers

The configured CLI parses arguments in an isolated search. Its effects
(`.read.keyword`, `.read.path`, `.read.token`, `.write.file`, `.case`)
return alternatives, edit per-branch journals (a cursor, the plan, the
expectations), and query the file system within branches that may never
commit. A machine answers only committed requests, through the heap, so it
cannot serve them. The options were:
- **A. Providers stay.** A Rust interface, successor to
  `TaskSpecialization`, answers effects synchronously within a transaction
  or search, and a turn runs a search with one (`m.search(provider, …)`,
  pushing the branches). Machines serve committed, asynchronous effects;
  providers serve hypothetical ones.
- **B. The parser in glam.** Its primitives become glam code over argv in
  task-local state, using `.alt` and `.cut`, with text patterns as macros
  have them. File-system queries remain: glam has no file access within a
  branch.
- **C. A search in a turn,** which pushes every branch as data, over B's
  primitives.

*Agreed (maintainer, 2026-10-18):* A, since the parser's file-system
queries within hypothetical branches need Rust that answers without
committing. It ties into the queue-based effects of the ownership plan,
whose remaining steps merged here. The maintainer clarified it the same
day:
- **Running an effect is evaluation.** A turn supplies the effects API,
  evaluates the program to weak-head normal form, and observes a yielded
  request with its continuation, or the final result; it interprets a
  request and resumes. Helpers interpret the standard effects, so a user
  sees only the unique ones, which the provider answers.
- **Search is cuts.** The search's branches are cuts within the turn, so a
  branch's journal is machine state that a failed cut restores, and A's
  `m.search(provider, …)` is no separate operation.
- **Where providers run:** within a turn, on its thread. The lifecycle
  review's decisions keep Rust code off workers, so a provider never serves
  a background task.
- **What they answer:** what is local to their run. A background task's
  requests go through queues to machines, and shared state, such as the
  reflection environment's `.env`, is state at paths that any reader sees.
- **A committed effect within a cut** is a request posted there, published
  when the cut commits, so its result comes after: as a promised value, or
  as a status that a later cut awaits with retry. Nothing waits for a
  request within the cut that posts it (the lifecycle review's target
  model).

### Tests and measurement

- **`tests/cli.rs`** (54 tests, which run the binary) is the safety net: a
  slice changes no CLI behavior unless this plan says so.
- **The CLI's unit tests** construct handles: in `main.rs`, the 31
  configured-CLI tests of `command_line/tests.rs`, the 10 rendering tests,
  and the logger's. Each slice converts the tests of what it moves, and
  leaves the others.
- **Profiles:** `hello_elf`'s instruction count is compared before and
  after each slice.

### Parity for handle retirement

Before the handles go, turns cover what `tests/public_api.rs` (47 tests),
`tests/effect_embedding.rs` and `tests/macro_protocols.rs` exercise:
- values, access, application, annotations, numbers and list slices, by
  machine operations (steps 1, 3);
- evaluation errors and their structured diagnostics, by operations' errors
  and `context:` frames (3);
- reflection inspection (structure, atom identity, sealed metadata), by the
  reading operations (3);
- promises, protected volumes, reflection environments and roles, by state
  at paths, request queues, and volumes allocated through the reflection
  API (6, 7, 9);
- the diagnostic formatter and diagnostic updates, by the logger machine
  (6);
- reasoning reports, retryable blocked errors and draining, by settlement
  (8);
- sources, imports, invocations and provenance, by hosts as machines (9);
- effect specialization, by effect providers (10);
- conflict analysis and workers, by `RuntimeConfig` and `activate_workers`
  (1, 2).

The checked net builder is open: it may stay a builder whose net a machine
pushes.

## From the Ownership Plan

The [runtime ownership plan](RuntimeOwnership_2026-10-17.md)'s remaining
steps merged here on 2026-10-18. Its target model, from the
[lifecycle review](../reviews/RuntimeLifecycleCapabilities_2026-10-16.md),
holds where `runtime-api-turns-on-machines` did not replace it: workers
run only runtime code, effects cross to Rust as data, and the owner alone
retires the runtime. The owner is now the `Runtime`, and the `Send` handles
are turns.

| Ownership step | Here |
|---|---|
| `lifecycle-cli-logger` | done 2026-10-16 |
| `lifecycle-owner` | the owner: `api-runtime-turns`; the retirement protocol: `api-retirement` |
| `lifecycle-queues` | `api-request-queues` |
| `lifecycle-host-messages` | `api-hosts-as-machines` |
| `lifecycle-leases` | `api-effect-providers` |
| `lifecycle-host-calls` | `api-hosts-as-machines` |
| `lifecycle-audits` | itself, beside the steps |
| `lifecycle-external-owners` | itself, beside the steps |

The review's findings, by the step that resolves each:
- **L1, L5, L6, L10** (owners everywhere, readiness results holding
  owners, sessions outliving retirement): `api-handle-retirement` removes
  the handles, and `api-retirement` retires in order.
- **L2** (the CLI logger on workers): done with `lifecycle-cli-logger`; the
  logger machine (`api-heap-communication`) ends what remains.
- **L3, L9** (workers upgrading `DeferredAssembler` for imports):
  `api-hosts-as-machines`.
- **L4** (a subscriber leaking its runtime): the diagnostic log
  (`api-heap-communication`) retires subscribers.
- **L7, L8** (callbacks that capture owners, client code on workers):
  `api-hosts-as-machines` and `api-effect-providers`, with
  `lifecycle-audits` to keep them out.
- **L11:** `lifecycle-external-owners`. **L12** (runtime IDs checked at run
  time) is information: values within a turn come from one runtime.

Where the merge changes the ownership plan's design:
- **Queues are shared state.** Decision 8 stored queues apart from the user
  heap. *Maintainer, 2026-10-18:* queues, data buses and publish-subscribe
  are, logically, transactional operations on shared state, in the heap or
  a specialized volume; how far the API specializes them is open.
- **Resolvers are glam values.** A machine holds glam values only, so the
  resolver it reads from a queue is an opaque glam value, as a volume is,
  no longer an affine Rust handle (maintainer, 2026-10-18). Single
  assignment is easy to enforce, and proposed; breaking the promise when a
  resolver is collected is proposed too.

## Names (to discuss as each step reaches them)

For `api-runtime-turns`, a first proposal:
- **`glam::Runtime`** for the runtime: short, and distinct from today's
  `EvaluationRuntime`, which remains during the transition. Alternatives:
  `GlamRuntime`, or taking over `EvaluationRuntime`'s name.
- **`run`** for a turn (`rt.run(key, |m| …)`). Alternatives: `turn`,
  `with_machine`.
- **`Machine`** for the context of a turn, the `m`. Alternative: `Turn`,
  which names the visit rather than the thing visited.
- **`RuntimeConfig`** for what opening takes: the runtime's own settings,
  worker threads and the conflict strategy. It is apart from the CLI's
  `GLAM_CONF`, a CLI notion (maintainer, 2026-10-17); compilation's
  settings travel with what is compiled.

*Agreed (maintainer, 2026-10-17):* `Runtime` and `run`.

For `api-machine-data`, a first proposal: building operations named for
what they build (`list`, `dict`), and reading operations named for what
they leave on the stack (`unpack_list`, `unpack_dict`). *Maintainer,
2026-10-18:* `dict`, glam's name for the type, rather than `record`.

For `api-cuts`: `m.cut(|t| …)`, after glam's `.cut`, which bounds the same
transaction and search (maintainer, 2026-10-18), in place of the review's
`m.transact`.

## As built

`api-runtime-turns` (2026-10-17), in `src/api/turn.rs`:
- `Runtime::open(RuntimeConfig)` builds an evaluation runtime and an
  assembler on it, held privately; dropping the `Runtime` drops the
  machines' values, then retires both through today's path. A
  `PhantomData<MutexGuard<'static, ()>>` makes it `!Send` but `Sync`, which
  doc tests check.
- Machines live in a map from `Key` to a slot, idle with its state or in a
  turn, under a leaf lock held only to take or return a state. A second
  turn on a machine in a turn fails; a turn may take a turn on another
  machine. An empty machine leaves no entry. The `Machine` returns its state
  when it drops, so a turn's changes stay whether it succeeds, fails or
  unwinds.
- Text keys name atoms (`From<&str>` for `Key`), and `Key` is public.
- Operations: `push_unit`, `push_int`, `push_text`, `push_bytes`,
  `push_atom`; `pop_int`, `pop_bytes`, `pop_text`, which evaluate;
  `depth`, `dup`, `swap`, `discard` (a method named `drop` would read as
  `Drop`), `stash`, `stash_pop`, `stash_depth`; `access` (a dotted path),
  `apply`, `eval` (to weak-head normal form); `compile`, which pushes the
  module and returns its warnings; `get`, `set`, `clear`.
- `pop_bytes` evaluates under the `binary` annotation, so a value that is
  no binary fails as evaluation does, with its structured error.
- Tests in `tests/runtime_api.rs`, including scoped threads taking turns
  on one runtime.

`api-cli-bridge` (2026-10-18):
- `src/transition.rs` is `glam::transition`, hidden from the docs, with
  `runtime_around` and `assembler`; each function's doc names the CLI
  modules that use it. A value's push and pop wait for their first use.
- `Runtime::activate_workers` starts the workers once in a runtime opened
  with none, and `Runtime::profile` returns the `glam-prof` snapshot. A
  test activates workers, takes a turn, and checks that activation happens
  once.
- The CLI wraps its assembler in a `Runtime` as soon as it is built, so
  `PreparedAssembly` and `PreparationFailure` hold the `Runtime` and no
  separate assembler or evaluation runtime. The main thread drops it; the
  logger's thread still holds assembler handles until `api-scoped-threads`.
- Bridge uses in the CLI: `runtime_around` once and `assembler` four times.
  One of those is `PreparedAssembly::assembler`, which `main` and `batch`
  call at 34 sites.
- `hello_elf`: 599.5 M instructions before, 599.4 after, with every runtime
  counter equal.

`api-machine-data`, its operations (2026-10-18):
- Building: `push_abstract_path(parts)`; `list(n)`; `dict(names)`, whose
  deepest value takes the first name, and which rejects a repeated name
  without popping; `anno`, the annotation pushed before its target, as
  `anno Annotation Target` reads. `compile` takes its inputs in order, and
  `compile_over` pops the initial definitions first.
- Reading: `unpack_list` evaluates under the `array` annotation and pushes
  the items, the first deepest; `unpack_dict` pushes each key before its
  value, in the runtime's canonical key order, as reflection, since glam
  offers no key iteration; `pop_atom_name` takes a text-named atom's name.
  `kind` and `same_representation` observe the top values without popping
  or evaluating them.
- Not built: a dict with keys other than names, until something needs one;
  and reading an error's structured diagnostic within a turn, for which
  `context:` frames are checked through the CLI's tests for now.

## Verification

Runtime changes run `scripts/check.sh full` before commit. Concurrency
fixes force the bad interleaving in a test; repeated runs are not proof
(`docs/AgentContext.md`, "Working Rules"). Public behavior is tested through
the public API, in `tests/`.

## Open Questions (2026-10-18)

- Names, per step (above).
- Communication's conventions, at `api-heap-communication`, and how far
  the API specializes queues and the like, by measurement.
- How a cut's failure and retry appear in Rust (an error, or outcomes of
  their own), at `api-cuts`.
- `api-cli-prelude`'s verdict, judged as
  [Policy in glam](#policy-in-glam-experiment) says.
- The ownership plan's open details, each settled when its step starts:
  retired operations' error (`api-retirement`); the queue capability's
  representation, and a request's status, possibly with a received state
  (`api-request-queues`); which host state moves into the store
  (`api-hosts-as-machines`).
