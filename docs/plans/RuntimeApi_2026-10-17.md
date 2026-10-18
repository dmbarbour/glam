# Runtime API Plan — 2026-10-17

Status: `api-runtime-turns` built 2026-10-17; `api-cli-bridge`,
`api-machine-data`, `api-cuts`, `api-virtual-resources` and
`api-machine-tree` 2026-10-18, with `api-cli-prelude` kept and
`api-scoped-threads` superseded by `api-machine-tree`
(see [As built](#as-built)). The CLI's transition was re-planned the
same day, after a survey of the CLI (see [CLI Transition](#cli-transition)):
`api-cli` gave way to a bridge and a CLI slice in each later step, moved
piecemeal (maintainer, 2026-10-17). On 2026-10-18 the maintainer agreed
effect providers (option A), which tie into the queue-based effects of the
[runtime ownership plan](RuntimeOwnership_2026-10-17.md), and its remaining
steps merged here (see [From the ownership plan](#from-the-ownership-plan)).
The same day the maintainer clarified that a turn is no transaction: a cut
bounds transactions and search (`turn-cuts-bound-transactions-and-search`),
and a turn runs an effect by evaluating it. They agreed the bridge, and the
CLI prelude as an experiment. After `api-cuts` was built, they made failure
explicit and retry implicit, as in glam, and gave hooks the machine. After
`api-scoped-threads`, they chose a runtime that controls concurrency:
machines form a tree under the owner thread, and forks are scoped to turns
(`machines-form-a-tree`). Drafted 2026-10-17 from the
[runtime API review](../reviews/RuntimeApi_2026-10-17.md), whose direction
the maintainer agreed the same day (`runtime-api-turns-on-machines`). Names
and communication's conventions are discussed as each step reaches them.

## Goal

A public API in which a Rust thread acts within the runtime: it opens a
thread-affine runtime and runs turns on machines named by user-chosen keys,
each with a stack and local state of glam values. Each action on shared
state is atomic, and a cut groups actions into a transaction that also
bounds a backtracking search. Fork-join comes from forks within turns, and tasks
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
- **Concurrency is the runtime's** (maintainer, 2026-10-18; first, scoped
  threads only, 2026-10-17): only the owner thread takes turns, and Rust
  code runs concurrently as forks of machines within turns
  (`machines-form-a-tree`).
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
   - Building: `make_list(n)`, `make_dict(names)`, `anno`, and a push of
     an abstract global path; `compile` from several inputs, over initial
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
     shared heap reads and writes staged in a runtime transaction, and the
     machine's stack and state restored on failure. Cuts nest.
   - As `.cut` does in reflection tasks, a cut also bounds a backtracking
     search: a failure undoes what the cut did, so the caller may try
     another alternative. Rust code chooses alternatives by ordinary control
     flow, or by nested cuts; it is not restricted to backtracking.
   - Failure is explicit and retry implicit, as in glam (maintainer,
     2026-10-18): `t.fail()` is the intent to backtrack, and the outermost
     cut it reaches awaits a change in what it observed, then runs again.
     Other errors end the outermost cut. The API promises a change in the
     observations, not how the runtime detects one, so coarse wakes stay
     an implementation choice (`perf-fine-grained-wakes`).
   - Optional, later, `api-alternatives`: an alternative operation after
     `.alt`, which tries the next alternative on explicit failure alone and
     passes other errors on (maintainer, 2026-10-18, as the reason failure
     is explicit). Not scheduled.
   - Rust side effects within a cut must tolerate a rerun, wait for
     commit, or be undone on abort, through hooks a cut registers
     (maintainer, 2026-10-18). Commit hooks run in the order registered
     once the outermost cut commits. Abort hooks run in reverse order as a
     failure backtracks through them to a cut, including those of nested
     cuts that had committed into it. Like their counterparts in C
     transaction libraries, they are best effort: they make compensation
     convenient, not certain.
   - Beside it, `api-virtual-resources`: a cut gets, sets and rewrites
     *virtual resources*, as it does the heap, proxies named by key for
     host resources the runtime does not see (maintainer, 2026-10-18).
     Conflict analysis covers them, and a failed cut that read one runs
     again when a later cut writes it, so Rust code that observes a host
     change writes its proxy to wake the waiters.
   - No CLI slice: the CLI's transactional code is the logger's, which
     moves with heap communication.
5. `api-machine-tree` — machines form a tree under the owner thread, and
   forks are scoped to turns (maintainer, 2026-10-18;
   `machines-form-a-tree`), superseding `api-scoped-threads`, which built
   threads that borrow the runtime and take turns on machines by key.
   - `rt.run(&mut self, path, …)` on a path of keys, the empty path the
     root. A turn reaches its descendants alone: synchronous turns
     (`m.run`), and values moved to and from their stacks (`m.give`,
     `m.take`).
   - `m.fork` takes a descendant to a thread that joins at `m.join` or by
     the end of its turn, or of an `m.scope`; the turn is a thread scope,
     so forks borrow what outlives `rt.run`.
   - A synchronous turn within a cut joins the cut.
   - CLI slice: the batch is one turn on `cli`, which forks the logger.
   - Next, `api-forks-in-cuts`: a fork within a cut joins the cut, on a
     branch of its journal that merges at the join, with siblings
     serializable: a child whose reads overlap what the cut wrote after
     its fork reruns the outermost cut (maintainer, 2026-10-18).
   - Then, to discuss first, `api-machine-errors`: error isolation between
     machines, error states and their explicit clearing, diagnostics
     captured in machine state, and joining a child's diagnostics into its
     parent's queue (maintainer, 2026-10-18).
6. `api-heap-communication` — state and logs in the heap or a volume, with
   readers' cursors in their own state; abstract global paths as
   capabilities; volumes allocated through the reflection API. Retention,
   backpressure and discovery are settled here, after a look at Kafka, DDS
   and MQTT conventions.
   - CLI slice, diagnostics: the CLI appends diagnostics to a log in turns.
     `DiagnosticBus`, `LogHost` and the supervisor's fallback endpoint
     retire.
   - CLI slice, loggers: the default logger is a forked machine
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
     fulfilled) and awaits a change by failing a cut that read it.
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
     change wakes a cut that failed on it.
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
    turn is in flight, since a turn borrows the runtime mutably and joins
    its forks.
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
whose logs request queues extend; the machine tree precedes the loggers' move;
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
| Logging | `DefaultLogger` renders as a subscriber; a configured logger is an `EffectRun` on its own thread with `MainEffects` (`read_log`, `write_stderr`), supervised, with a fallback endpoint | forks, logs | 5, 6 |
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

*Kept (maintainer, 2026-10-18),* at about 16.7 M instructions per run (see
`api-machine-data`'s As built).

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
  as a status that a later cut awaits by failing. Nothing waits for a
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
what they build (`list`, `record`), and reading operations named for what
they leave on the stack (`unpack_list`, `unpack_dict`). *Maintainer,
2026-10-18:* `dict`, glam's name for the type, rather than `record`; then,
seeing them used, `make_list` and `make_dict`.

For `api-cuts`: `m.cut(|t| …)`, after glam's `.cut`, which bounds the same
transaction and search (maintainer, 2026-10-18), in place of the review's
`m.transact`. As built, a first proposal: `heap_get`, `heap_set` and
`heap_rewrite`, after `.heap.get`, `.heap.set` and `.heap.rewrite`;
`fail`, after `.fail` (first built as `retry`); `ErrorKind::Backtrack`
for its kind, since `ErrorKind::Failure` names any other error; and
`on_commit` and `on_abort` for the hooks. For `api-virtual-resources`:
`virtual_get`, `virtual_set` and `virtual_rewrite`, mirroring the heap's
(maintainer, 2026-10-18; first built as `virtual_read` and
`virtual_write`). For `api-scoped-threads`, since withdrawn: `try_run`,
`rt.scope` with `RuntimeScope`, and `ScopedTurn`; the maintainer found
the scope wrapper added nothing to `std::thread::scope`. For
`api-machine-tree`: `give` and `take` (maintainer, 2026-10-18); `run`,
`fork`, `fork_with`, `join` and `scope` on the machine, with `MachinePath`
and `Fork`, a first proposal.

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
- Building: `push_abstract_path(parts)`; `make_list(n)`;
  `make_dict(names)`, whose deepest value takes the first name, and which
  rejects a repeated name without popping; `anno`, the annotation pushed
  before its target, as `anno Annotation Target` reads. `compile` takes
  its inputs in order, and `compile_over` pops the initial definitions
  first.
- Reading: `unpack_list` evaluates under the `array` annotation and pushes
  the items, the first deepest; `unpack_dict` pushes each key before its
  value, in the runtime's canonical key order, as reflection, since glam
  offers no key iteration; `pop_atom_name` takes a text-named atom's name.
  `kind` and `same_representation` observe the top values without popping
  or evaluating them.
- Not built: a dict with keys other than names, until something needs one;
  and reading an error's structured diagnostic within a turn, for which
  `context:` frames are checked through the CLI's tests for now.

`api-machine-data`, its CLI slice with `api-cli-prelude` (2026-10-18):
- The prelude, `src/bin/glam/configuration/prelude.g`, holds the CLI's
  policy: the default environment object; the configuration's initial
  definitions; `entry`, an entry's value or its fallback, with `context:`
  frames naming the entry and the path looked up; the assembly's initial
  definitions with their journal records; the result under the `binary`
  annotation with an `asm.result` frame; and a completion script's request.
- Preparation compiles the prelude and the configuration in a turn on the
  `cli` machine, whose state holds `prelude`, `configuration` and
  `environment`. The assembly, `conf.log`'s selection and the completion
  script run in turns there too, sharing two Rust helpers, `select_entry`
  and `selects_nothing`.
- Error contexts come from `context:` annotations, no longer from
  `Error::with_context`. The CLI's Rust lost 121 lines net, the prelude
  has 36, and `LoadedConfiguration`, the journal and context builders and
  the Rust completion-script runner went.
- The bridge's `pop_value` hands the configuration to the configured CLI
  and the selected logger to its `EffectRun`. Uses: `runtime_around` once,
  `assembler` five times (one is `PreparedAssembly::assembler`, called at
  31 sites), `pop_value` twice.
- Tests: `tests/cli.rs` gains a test of the result's frame, in place of
  the unit test of the Rust context it replaces. Error output, compared by
  hand with the previous commit's for a failing result, environment, log
  and completion script, a configured script, and the built-in fallback,
  is identical.
- Cost: `hello_elf` rose from 599.5 to 616.2 M instructions (2.8%), and a
  run with no configuration from 23.8 to 40.4 M. The prelude's text
  compiled within a user's script costs about 6 M, so about 10 M is the
  fixed cost of compiling a second module: another session, its tasks and
  their net work. That cost looks general to module compilation rather
  than particular to the prelude.
- Verdict (maintainer, 2026-10-18): kept, at an acceptable cost. A
  module's fixed cost joins the performance roadmap as
  `perf-module-fixed-cost`, after the representations; this plan goes
  ahead of performance work unless a cost gets in the way.

`api-cuts` (2026-10-18), in `src/api/turn.rs`, over the reflection store's
journal and the runtime's snapshot, commit, validation and wait:
- Heap actions: `heap_get(path)` pushes the value at a path of keys
  without evaluating it, `heap_set(path)` pops a value to write, and
  `heap_rewrite(path)` pops a function that applies lazily at commit.
  Outside a cut, each is atomic by itself: a read takes the heap as it
  stands, and a write commits at once.
- `m.cut(|m| …)` runs its body against one snapshot, staging reads and
  writes in a journal; reads see the cut's own writes. The body takes the
  same `Machine`, so every operation works within a cut.
  - On `Ok`, the outermost cut commits. A conflict reruns the body at once
    on the state the cut started from; a revoked volume fails it.
  - On an error, a cut restores the machine's stack, stash and state and
    the journal, keeping its reads, and returns the error, so the caller
    may try an alternative.
  - Explicit failure is an error of its own kind, `ErrorKind::Backtrack`,
    from `m.fail()`. A nested cut returns it. The outermost cut waits until
    validation finds an observation changed, pumping the runtime's
    background work before each wait, since without workers nothing else
    may run it, and then reruns. A failure without observations ends the
    cut with an error, since nothing could wake it, and one outside any cut
    ends the turn. Other errors end the outermost cut at once.
  - First built with any error backtracking and an explicit `m.retry()`;
    revised the same day (maintainer) to the above.
- Hooks: `on_commit(items, hook)` and `on_abort(items, hook)` pop `items`
  values and register a `'static` closure over the machine,
  `FnOnce(&mut Machine) -> Result<(), Error>` (maintainer, 2026-10-18,
  in place of plain closures). A hook runs on the machine with its local
  state as it then stands, over a stack holding only its captured values
  and an empty stash; the machine's own stack and stash return after.
  - `on_commit` hooks run in order after the outermost commit, outside any
    cut, so their heap actions commit at once. `on_abort` hooks run in
    reverse as a failure backtracks through them, once the cut has undone
    what it did: within the enclosing cut for a nested one, so their
    actions join it, and outside any cut for the outermost. A conflict's
    rerun aborts the attempt, running its abort hooks. Outside a cut,
    `on_commit` runs at once and `on_abort` is dropped.
  - Best effort: each hook runs although one before it erred. A cut
    returns a hook's first error in place of its result or of an explicit
    failure, which ends an outermost cut that would have run again, but
    not in place of another error, which came first. This rule is open for
    a later discussion and review (see
    [Open Questions](#open-questions-2026-10-18)). A panic within a cut
    abandons it without running hooks.
- Tests in `tests/runtime_cuts.rs`. Two force their interleavings through
  channels between scoped threads: a failed cut blocks before the write
  that wakes it, running exactly twice; and a write commits between a cut's
  read and its commit, which reruns once and reads the new value.
- Not built: volumes other than the user heap; a heap path taken from the
  stack, as a glam list of keys; and runtime events (inputs and outputs)
  within a cut.

`api-virtual-resources` (2026-10-18), in `src/api/turn.rs` and the
reflection store. Virtual resources mirror the heap (maintainer,
2026-10-18), with the heap's conflict rules and no data:
- `virtual_get(path)` reads, within a cut, the virtual resource at a path
  of keys, a proxy for host state the runtime does not see. It pushes
  nothing. Like a heap read, it counts as an observation, so a failed cut
  that read only virtual resources waits. Outside a cut it does nothing, as
  a read there joins no transaction.
- `virtual_set(path)` and `virtual_rewrite(path)` write it: within a cut,
  when the outermost commits; outside one, at once. A set covers a later
  read of its path in the same cut, which then records nothing, since Rust
  code that sets a resource knows the state it stands for. A rewrite, for
  a change that depends on the state before it, such as an append, widens
  a later read to its own path instead, as on the heap.
- Paths are hierarchical, as the heap's are: a write conflicts with reads
  of its path and of paths above and below it, and wakes the failed cuts
  that made them. Virtual resources lie apart from the heap.
- In the store, virtual writes are the heap's `Set` and `Rewrite` edits
  without data (maintainer's suggestion, 2026-10-18): conflict analysis,
  coverage and widening treat them as their data counterparts, and only
  applying an edit skips them, so they change no volume. Addresses lie
  under a namespace of the runtime's private volume, apart from its
  queries.
  - First built with a separate `Touch` edit, which matched only a
    rewrite's conflicts and needed its own cases.
  - Writing data, such as `{}` at the path, was rejected: each write would
    add a lazy update to a volume root that nothing forces.
- As for every address, the store keeps the latest change of each address
  ever written, so distinct resource names accumulate there, as distinct
  heap paths already do.
- Tests in `tests/runtime_virtual_resources.rs` force each interleaving
  through channels. A write between a cut's read and its commit conflicts
  below or above the read path, and as a rewrite, but not for an unrelated
  resource or a heap write. A cut's own set covers its later read, and its
  own rewrite widens it. A cut's write takes effect at its commit, and a
  failed cut's or failed nested cut's write never. A failed cut that read
  a resource waits until it is written, running exactly twice, and one
  whose only read its own set covered cannot wait. A store unit test
  checks that data-less edits change no volume on either commit path.

`api-scoped-threads` (2026-10-18), superseded the same day by
`api-machine-tree`. It built `rt.run` waiting for a machine in a turn on
another thread, with a check that failed a wait closing a cycle of
threads, `try_run` declining instead, and `rt.scope`, a wrapper around
`std::thread::scope` with forked turns; the CLI's logger ran on a scoped
thread. Its code went with `api-machine-tree`; see `machines-form-a-tree`
for why.

`api-machine-tree` (2026-10-18), in `src/api/turn.rs`:
- The runtime holds the root machine's state, and each machine's state
  holds its children by key; an empty machine is pruned. Its shared part,
  the assembler, sits apart, so a turn borrows it while the tree stays
  mutable. Exclusive access comes from ownership: `rt.run(&mut self,
  path, …)` takes the machine at the path out of the tree, and returns it
  after, panic or not. The machine map, its lock and condition variable,
  waiting, `try_run` and the cycle check are gone, and a nested `rt.run`
  does not compile (a doc test).
- `MachinePath` converts from `()`, a key, a text, an array or a vector of
  keys. `m.run(path, …)` takes a synchronous turn on a descendant, created
  empty if absent. `m.give(path, n)` and `m.take(path, n)` move the top
  values, keeping their order. A machine's own path is no descendant's,
  so these fail on the empty path.
- `rt.run` opens a `std::thread::scope` for the turn, and `Machine` carries
  its lifetimes, `Machine<'scope, 'env>`. `m.fork(path, …)` and
  `m.fork_with(builder, path, …)` spawn a scoped thread that takes a turn
  on the descendant, leaving a `Forked` mark in its place, so neither it
  nor a machine below it is reachable until the join; a machine with a
  fork below it cannot fork. `m.join(fork)` returns the machine and the
  turn's result, resuming a panic. A turn joins its unjoined forks at its
  end, and `m.scope(|m| …)` joins those it made, by fork ID. A fork's
  state returns through the join, so a dropped `Fork` loses nothing; its
  result travels through a shared slot.
- Cuts: a synchronous turn within a cut takes the cut's transaction, so
  its reads and writes join it and its nested cuts nest; its state is part
  of the parent's, which the cut restores. A hook records the path of the
  machine that registered it, relative to the machine whose turn opened
  the cut, and runs there. Forks and joins within a cut fail until
  `api-forks-in-cuts`.
- `clear` fails, changing nothing, while a descendant is forked, and
  moving zero values with `give` or `take` only checks that the descendant
  is not forked, creating nothing (maintainer, 2026-10-18, in place of a
  `clear` that kept a forked descendant's place).
- CLI slice: `execute_assembly` is one turn on `cli`. `start_logger` takes
  the machine and the assembler, selects the logger on `cli` itself, and
  forks the child `logger` with its name and stack size; `assemble` works
  on the machine; the turn settles and joins the logger. The profile
  report moved after the turn, as it needs the runtime. `PreparedAssembly`
  gives mutable access for its turns. Bridge uses in the CLI:
  `runtime_around` once, `assembler` four times and `pop_value` twice;
  `start_logger` receives the assembler from `batch` rather than asking
  the bridge. `hello_elf` runs in 616.3 M instructions, against 616.2 M
  before, with every runtime counter equal.
- Tests: `tests/runtime_forks.rs` covers fork and join, a fork borrowing
  what outlives the turn, forked machines unreachable until the join,
  forks joined by a scope's and a turn's end, panics resuming with the
  machine kept, joins on another machine, forks within a cut failing, and
  a configured thread. `tests/runtime_api.rs` covers descendants' turns,
  paths from the root, and moved values; `tests/runtime_cuts.rs` covers a
  child turn joining its parent's cut and hooks running on the machine
  that registered them, and forces its interleavings with forks; so does
  `tests/runtime_virtual_resources.rs`.

## Verification

Runtime changes run `scripts/check.sh full` before commit. Concurrency
fixes force the bad interleaving in a test; repeated runs are not proof
(`docs/AgentContext.md`, "Working Rules"). Public behavior is tested through
the public API, in `tests/`.

## Open Questions (2026-10-18)

- Names, per step (above).
- Hook errors, for a later discussion and review (maintainer,
  2026-10-18, unsure of the rule built in `api-cuts`): each hook runs
  although one before it erred, and the first hook error replaces a cut's
  result or explicit failure but not another error. Alternatives include
  hooks that stop at the first error, hook errors reported apart from the
  cut's outcome, such as to a log once `api-heap-communication` lands, and
  hooks that return no error.
- Communication's conventions, at `api-heap-communication`, and how far
  the API specializes queues and the like, by measurement.
- The ownership plan's open details, each settled when its step starts:
  retired operations' error (`api-retirement`); the queue capability's
  representation, and a request's status, possibly with a received state
  (`api-request-queues`); which host state moves into the store
  (`api-hosts-as-machines`).
