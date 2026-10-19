# Runtime API Plan — 2026-10-17

Status: `api-runtime-turns` built 2026-10-17; `api-cli-bridge`,
`api-machine-data`, `api-cuts`, `api-virtual-resources`,
`api-machine-tree` and `api-forks-in-cuts` 2026-10-18, and
`api-machine-errors` 2026-10-19, with `api-cli-prelude` kept and
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
     the top `n` values there, and `stash_pop(n)` moves them back, so the
     pair leaves them as they were. The stash is the stack's other side, as
     in a zipper (maintainer, 2026-10-19): `stash(n)` is `stash(1)` `n`
     times, and `stash_pop(n)` likewise, so values move one at a time and
     `stash(n)` then `stash_pop(1)` `n` times also leaves them as they were.
     `flip(n)` reverses the top `n` values, generalizing `swap`.
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
     moves with ports (`api-ports`).
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
   - Then `api-forks-in-cuts`: a fork within a cut joins the cut, on a
     branch of its journal that merges at the join, with siblings
     serializable: a child whose reads overlap what the cut wrote after
     its fork reruns the outermost cut (maintainer, 2026-10-18).
   - Then `api-machine-errors` (maintainer, 2026-10-19;
     `machine-errors-mark-and-recover`): an error or panic that ends a
     turn sets its machine's error flag, which only a recovering entry
     clears; forks return nothing to Rust, and their joins return their
     outcome; hooks handle their own errors. Diagnostics, first planned as machine state, go
     instead to communication (below).
   - Deferred, `api-fine-grained-reruns` (maintainer, 2026-10-18; moved
     after, or alongside, `api-alternatives`, 2026-10-19): today
     every inconsistency reruns the outermost cut, whether a commit
     conflict, a fork's branch that read what the cut wrote after the
     fork, or a conflict inside a nested cut that then failed. A finer
     rerun would redo only the part whose reads went stale, such as a fork
     rerun at its join on the cut's current state, or a nested cut rerun
     in place, keeping the work before it. It belongs with backtracking
     finer than a whole cut, so it follows the alternative operation.
6. `api-ports` — communication through ports wired from outside, as in
   dataflow and Kahn process networks (maintainer, 2026-10-19), renamed
   from `api-heap-communication`, whose state at heap paths stays for
   state rather than streams. It amends rather than reverses
   `runtime-api-turns-on-machines`'s rejection of point-to-point channels:
   the buffers are shared state, and the topology belongs to the wiring.
   - A machine reads and writes named ports. `m.write(port, n)` pops `n`
     values, in order, and sends them; values rather than lists, so that
     nothing is forced. `m.read(port, n)` pushes the next `n`. Stack
     operations keep a static arity, with `n` a static parameter, as
     `dup(4)` stands for a Forth-like `dup4` (maintainer).
   - Too few to read, or a full buffer, is pushback, a divergence, not a
     failure that seeks alternatives (maintainer): `Backtrack` is for an
     alternative operation, and a new kind, `Retry`, is the divergence a
     cut recognizes, waiting for a change in what it observed and running
     again; `m.retry()` raises it (maintainer). Within a cut, a port
     operation that cannot proceed returns `Retry`. Outside one, `read`
     and `write` block until they can, as a Kahn network's do, and
     `try_read` returns `Retry` instead (maintainer); `try_write` comes
     with bounded buffers, deferred (maintainer, 2026-10-19). The
     outermost cut still turns a `Backtrack` from its last alternative
     into such a wait. A cut variant that returns `Retry` instead of
     waiting belongs with `api-alternatives`.
   - A port name has two independent roles, input and output
     (maintainer). Wiring is directional and external:
     `m.wire(source, destination)` and `m.unwire`, on the machine's own
     ports and its direct children's, by the Law of Demeter (maintainer).
     An `Endpoint` is `input(port)`, `output(port)` or `child(child,
     port)`, a child's port being its output as a source and its input as
     a destination (maintainer). The machine's own ports serve in both
     roles: its input to its output is a switchboard, its output to a
     child's input feeds the child, and a child's output to its input
     collects from the child (maintainer). Wiring is transactional state,
     so rewiring within a cut is atomic. `m.input_connected(port)` and
     `m.output_connected(port)` tell whether a role has a wire, and
     `m.pending()` which inputs hold values (maintainer).
   - A write to an unconnected output is a plain error (maintainer): it
     is structure, not flow, and waiting would not help.
     `m.available(port)` counts what an input holds, transactionally
     (maintainer), so a reader drains a port with a static `n`.
   - Buffers belong to receiving ports: several wires from one port
     duplicate, several into one merge in commit order. They are unbounded
     (maintainer: for reflection's purposes, at the risk any evaluation
     already runs). Bounded buffers, with a capacity that blocks the
     writer, are deferred, to keep this simple (maintainer, 2026-10-19);
     dropping with a report, or breaking the connection, later still.
   - Buffers and wiring are transactional state, serializable within cuts,
     and a short read waits through the existing wakes. Their cost is
     measured before anything specializes them (`perf-port-routing`).
   - Unwired ports do not default to their parent's (maintainer). The
     diagnostics wiring is part of the runtime's default configuration,
     with an option for none (maintainer): each child's diagnostics output
     to its parent's diagnostics input, and each machine's diagnostics
     input, the root's aside, through to its own output, so diagnostics
     gather at the root's input. Any machine intervenes by unwiring a
     default wire it owns, such as its pass-through, and reading its input.
     A turn that ends in error might emit its error on its machine's
     diagnostics output by default, so that an unobserved failure, such as
     a fork's left to its turn's end, still reaches a person.
   - The host reads the root's diagnostics input in a turn, or wires it to
     a logger. Until reflection tasks are machines, the runtime feeds their
     diagnostics (`.log`) and its own reports into the root's diagnostics
     input, a bridge that goes with the reflection API's alignment.
   - Ports live beside the runtime's existing transactional event state,
     whose FIFO buffers, cursor validation and combined commit with the
     store they extend: buffers by receiving port, wiring by the machine
     that owns it, all in Rust structures that force nothing, with glam
     values as payloads. A read sees committed state only, never its own
     transaction's writes, as glam's log reads do.
   - Deferred: hierarchical subchannels; closing a channel and the end of
     a stream; built-in mergers, duplicators and filters, which machines
     users write stand in for meanwhile; further buffer policies.
   - CLI slice, diagnostics and loggers, in three stages (maintainer,
     2026-10-19):
     1. The runtime routes its assembler's diagnostic bus into the root's
        diagnostics input, so what the bus's publishers report, the
        assembler's and the CLI's alike, reaches the port unchanged.
     2. The CLI's batch turn runs on the root and forks `logger`, a direct
        child of the root, wired from the root's diagnostics input. The
        default logger is the logger's own child, `logger/fallback`, which
        renders what reaches it as `DefaultLogger` does. A configured
        logger reads the logger's input through `read_log`; when it ends,
        the logger passes what remains to `logger/fallback` and wires its
        input there in one cut, an internal rewiring (maintainer). Without
        a configured logger, the input is wired to `logger/fallback` from
        the start. `LogHost`'s buffer, the bus's route switching and the
        supervisor's fallback endpoint retire; the bus stays as the
        publishers' entry, and its counts decide the exit code.
     3. With `api-effect-providers`, the configured logger becomes a turn
        whose stderr output goes to a port that a CLI machine drains, so
        `MainEffects` and the logger's `EffectRun` retire.
     Later, `logger/fallback` applies the glam formatter, into which the
     Rust frame summaries of `rendering.rs` move.
7. `api-machine-environment` — machines inherit an environment
   (`machines-inherit-an-environment`, maintainer, 2026-10-19), in place of
   the reflection environment an assembler's builder seals, which `.env`
   reads today.
   - A machine sets one object, a mixin, as its share of the environment,
     only in its own turns and transactionally, and may get it back to
     extend, or clear it (`set_env_mixin`, `get_env_mixin`,
     `clear_env_mixin`). Its environment is its ancestors' mixins composed
     from the root down: `.env []` is one object, built lazily, and a read
     of it, whole or at a path, is observed, so a cut that read it runs
     again when a mixin above it changes. Shares live in a store of the runtime, keyed by machine path,
     so reflection tasks read them through their hosts until they are
     machines.
   - What is not known yet is a promised value within a share, waited on
     alone: a small machine API allocates a promise and fulfills or breaks
     it, its resolver an opaque glam value whose second resolution fails,
     which `api-requests` builds on.
   - Reflection tasks are logical children of a `refl` machine under the
     root: it supplies their environment, and the runtime's own
     diagnostics enter its diagnostics input, as if they were its
     children's, passing through to the root's by default, so a host may
     intervene there. Each role's machine binds its `glam` entry.
   - The composed environment's cost is measured before anything caches
     it, within a transaction or across them by the shares' revisions.
   - CLI slice: the CLI's root binds `process` in its share, with
     `process.args` and `process.refl_args` promised values that the CLI
     fulfills after `conf.cli`'s expansion, in place of the builder's
     promises, which retire with its environment closure.
8. `api-requests` — absorbs `lifecycle-queues`, renamed from
   `api-request-queues` (maintainer, 2026-10-19): requests travel on ports,
   their answers through promises, and no queue of its own is a primitive
   (`requests-travel-on-ports`).
   - A request is a value written to an output port, carrying in a fixed
     place, `{request:…, reply:…}`, a resolver where an answer is wanted;
     wiring routes it to a server machine's input, and the server resolves
     the reply. A write commits with its cut, so a failed cut sends
     nothing; a channel's order is its commit order, and channels promise
     no order among themselves. The requester holds the promised answer,
     or waits for it in a cut, since evaluating it is pushback until it
     resolves. `m.ask(port)` posts one.
   - Duplicate wires duplicate a request, and its servers race: the first
     resolution wins, and a later one fails (maintainer). A server may read
     a promise's status to avoid duplicate work.
   - A promise's status, pending, fulfilled or broken, read without
     forcing it, by a machine operation and a reflection effect. A cut
     that reads it waits as one whose evaluation waited does.
   - Volumes as first-class capabilities (maintainer): an opaque volume
     handle and generic effects, `.vol.new`, `.vol.get`, `.vol.set` and
     `.vol.rewrite`, with `.heap.vol` the shared heap's handle, so `.heap.*`
     is shorthand; and machine operations to match, `m.volume_new`,
     `m.volume_get`, `m.volume_set` and `m.volume_rewrite`, beside
     `m.heap_*`. Allocation is transactional: its ID is reserved at once and
     its root installed at commit, so an aborted transaction burns the ID.
     A volume lives until the runtime retires, for now
     (`api-opaque-finalization`). Users model queues and other first-class
     capabilities over volumes; ports offer no first-class channel
     capabilities (maintainer). The capability dict of `get`, `set` and
     `rewrite` that `Assembler::create_volume` returns retires with the
     handle API.
   - Effect channels from reflection: the runtime's route into `refl`,
     diagnostics' today, generalizes to an output per effect channel as each
     channel's consumer moves: stderr with the logger's third stage
     (`api-effect-providers`), imports with `api-hosts-as-machines`.
   - Tests: a failed cut sends no request; a channel's order matches commit
     order; a server answering a request a requester awaits; duplicate
     servers racing, the second resolution failing; a promise's status as
     it resolves, and a cut waiting on it; a volume allocated, written and
     read, a failed cut's allocation leaving nothing, and its handle passed
     between machines.
9. `api-opaque-finalization` — a representation review, then collection of
   what opaque values stand for (maintainer, 2026-10-19): a resolver
   collected unresolved breaks its promise, and a volume whose handles are
   all unreachable is reclaimed. Opaque payloads hold no managed edge, and
   the runtime's external-owner registry retires them, never the
   collector's finalization, so the review decides how a payload learns
   that it is unreachable.
10. `api-reflection-model` — a design step, without code, ahead of the
   settlement it shapes (maintainer, 2026-10-19): how the reflection API, as
   `refl.*` and `anno refl:(…)` present it, maps to machines. Spawned tasks
   (`.task.new` and its relatives) become named machines in the tree that
   outlive quiescence; `.exit` retires, since a machine waiting on input is
   idle; `.log` and `.read_log` become ports; observing or acknowledging a
   task's failure becomes recovering its machine; and the step settles
   which waits count as unresolved (`quiescence-reports-unresolved-state`).
   It ends in an ADR. Its implementation is a later step, after
   `api-effect-providers`.
   - `.exit` also said that a machine, locally, had no outstanding
     obligations, and something replaces that role (maintainer,
     2026-10-19). Proposed: a machine's outstanding obligations as explicit
     state, a set of keys in its local state beside its stack, a checklist;
     settlement reports the machines whose sets are not empty, or at least
     their count. A key describes itself; projecting machine states for a
     report comes later. Semantically the set is the machine's alone:
     reflection may see or change it once reflection over the process
     network is designed. Reflection's operations come when it is time,
     such as `.todo.add`, `.todo.del` and `.todo.list` (maintainer, the same
     day).
11. `api-settlement` — settlement as `quiescence-reports-unresolved-state`
   decides (maintainer, 2026-10-19), in place of exit votes. The runtime is
   quiescent when every machine waits on input or a heap change, retrying
   its current atomic step, and no evaluation work remains; turns in
   progress are known, so settlement counts them, which the handle API
   could not. `rt.settle()` waits for quiescence, services maintenance,
   kills a deadlock, and has a machine whose diagnostics output it checks
   first build and emit a report of what remains unresolved: obligations
   no one will meet, machines still in error, task failures no one
   observed, and the work it killed. Failures known when they happen, a
   delivery's or a maintenance failure, are diagnostics then. Settlement
   leaves the logger running; a batch ends when a quiescence finds nothing
   new, ending each fork that then waits in an outermost cut with its
   machine as it was.
   - CLI slice: the CLI's two settlement loops become one, and the bridge
     behavior below that settlement retires goes.
12. `api-hosts-as-machines` — absorbs `lifecycle-host-messages` and
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
13. `api-effect-providers` — absorbs `lifecycle-leases`. A turn runs a
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
14. `api-handle-retirement` — the public handle types go: `Assembler`,
    `ModuleBuilder`, readiness results, `EffectRun`, endpoints and buses,
    and `Values` as a separate handle. The bridge goes with them, and the
    public tests move to turns (see
    [Parity](#parity-for-handle-retirement)). Standing docs describe the new
    API.
15. `api-retirement` — absorbs `lifecycle-owner`'s retirement protocol (L1,
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
precede the CLI's first slice. Cuts precede ports,
whose buffers request queues extend; the machine tree precedes the loggers' move;
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
| Preparation | `FileSourceSystem`, `EvaluationRuntime`, `DiagnosticBus`, and an `Assembler` whose reflection environment holds two promises | an owner, then the environment and sources in their target form | 2, 6, 7, 12 |
| Configuration and assembly | compiles `configuration` and `assembly` over initial definitions; access, `defined_or`, `context:` annotations, evaluation to bytes | data operations | 3 |
| Logging | `DefaultLogger` renders as a subscriber; a configured logger is an `EffectRun` on its own thread with `MainEffects` (`read_log`, `write_stderr`), supervised, with a fallback endpoint | forks, ports, then turns that run effects | 5, 6, 13 |
| Settlement | two loops over readiness, maintenance, deadlock kills and quiescence reports | the reflection model, then `rt.settle()` and a report that a machine emits | 10, 11 |
| Configured CLI | `conf.cli` runs as an isolated effect search over argv: `CliEffects`, with nested `TokenEffects` searches, per-branch journals, and a search again for each completion candidate | effect providers | 13 |

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

### Bridge behavior

Some of what the CLI and the runtime do now is scaffolding for the
transition, not design (maintainer, 2026-10-19). Each goes with the step
named:
- The CLI's settlement loops, the configured logger's exit vote, and the
  settled reports the CLI renders itself, or publishes on the bus where no
  logger runs: `api-settlement`, where a machine reports what remains
  unresolved (`quiescence-reports-unresolved-state`).
- The configured logger as an `EffectRun` with `MainEffects`, reading the
  logger's input through `RuntimeEventJournal::read_port`, with a lifecycle
  that closes its session when its root ends
  (`EffectLifecycle::new_closing_session`): `api-effect-providers` and the
  reflection model's implementation.
- The runtime's own diagnostics, routed from the assembler's bus into the
  root's diagnostics input as if a child had sent them, and one whose
  routing fails waiting where it entered: `api-machine-environment` moves
  the route to the `refl` machine's diagnostics input, and the reflection
  model's implementation retires it, when reflection tasks are machines
  whose writes fail as any machine's do.
- Reflection's hosts reading the `refl` machine's environment as the
  shares stand, outside any transaction, over the role's sealed
  environment, so a change to a share wakes no task that read it, and the
  CLI's logger host taking its environment once, through
  `glam::transition::reflection_environment`: the reflection model's
  implementation.
- The process environment's variables, which the CLI builds with the
  handle API and pushes onto the root's stack
  (`glam::transition::push_value`), since a machine's dicts are keyed by
  name and these by byte strings: `api-handle-retirement`. A direction
  for it (maintainer, 2026-10-19): users may take temporary handles to
  values, branded by a scope that `m.scope()` isolates, so building a
  value outside a machine's operations needs no bridge.

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
  API (7, 8, 12);
- the diagnostic formatter and diagnostic updates, by the logger machine
  (6);
- reasoning reports, retryable blocked errors and draining, by settlement
  (11);
- sources, imports, invocations and provenance, by hosts as machines (12);
- effect specialization, by effect providers (13);
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
| `lifecycle-queues` | `api-requests` |
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
  logger machine (`api-ports`) ends what remains.
- **L3, L9** (workers upgrading `DeferredAssembler` for imports):
  `api-hosts-as-machines`.
- **L4** (a subscriber leaking its runtime): the diagnostics port
  (`api-ports`) retires subscribers.
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
  `depth`, `dup`, `swap`, `flip` (added 2026-10-19), `discard` (a method
  named `drop` would read as `Drop`), `stash`, `stash_pop`, `stash_depth`; `access` (a dotted path),
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
  values and register a `'static` closure over the machine, `Send` since
  `api-forks-in-cuts`,
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
  the cut, and runs there. Forks and joins within a cut failed until
  `api-forks-in-cuts`, the same day.
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

Affine runtime (2026-10-18, maintainer, under `api-machine-tree`): the
runtime is `Send` but not `Sync`, by a `Cell` marker in place of the mutex
guard's, so a thread may hand it to another, and doc tests check both. A
test moves a runtime to a new thread, which takes a turn and retires it.
Thread-local state was checked first: beside debug and test bookkeeping,
only the collector's per-thread allocation caches, which a forked thread
now releases as it ends, as a worker does.

`api-forks-in-cuts` (2026-10-18), in `src/api/turn.rs` and the reflection
store:
- A fork within a cut takes a branch of the cut's transaction, a clone of
  its journal that shares the cut's observations, so the fork's reads join
  the cut's validation and its wait after a failure. The branch also
  records its own reads, computed over its own edits alone: a read that a
  write of the cut's before the fork covers is internal to the cut, but
  the branch depends on it.
- At the join, the branch's edits replay onto the cut's journal, and its
  hooks and whether it observed anything join the cut's. Should a read of
  the branch overlap an edit the cut made after the fork, by the parent or
  a sibling joined first, nothing merges but the hooks, the join fails,
  and the outermost cut runs again whatever its body returns. So a cut
  behaves as if each fork ran at its join, in join order. A branch merged
  into another passes up its reads, less those its parent branch's own
  earlier writes cover. A fork whose turn fails still merges, as a turn is
  not atomic; a nested cut within the fork makes its work atomic.
- Levels, first: a machine counted the cuts it was within, and a fork
  joined only at its level, a rule checked as the program ran. Branded
  forks replaced it the same day (below). A cut's end, nested or
  outermost, joins the forks it made, merging them, before the cut
  commits, undoes or runs again.
- A fork's hooks return to its parent's thread, so hooks are now `Send`;
  the tests' hook logs moved from `Rc` to `Arc`.
- A thread that does not start leaves the forked machine to return to its
  place, which `api-machine-tree` lost.
- Tests: `tests/runtime_forks_in_cuts.rs` covers forks joining the cut's
  transaction, with a fork's fork, a failed cut undoing its forks, write
  skew between siblings forced through channels and run again into join
  order, a fork conflicting with the cut's write after it even where the
  cut's earlier write covered the read, joins at the wrong level, a fork's
  hooks on its machine, and a failed cut waiting on what its fork read. A
  store unit test covers merges, a refused merge, and a fork's fork whose
  read its parent branch covered.

Branded forks (2026-10-18, maintainer, under `api-forks-in-cuts`): a
fork's lifetime is the thread scope it was made in, as a scoped thread's
join handle's is, so the compiler rejects a join on another machine or in
another cut or scope, and a fork leaving its scope.
- Each turn (the runtime's, a descendant's or a fork's), each attempt of a
  cut and each `m.scope` opens a `std::thread::scope`, and hands its body a
  machine of a fresh `'scope`: the same machine's state, cut and offset,
  moved in and back, with its own forks, which join at the scope's end.
  `Fork<'scope, R>` is invariant in `'scope`, so no fork passes for one of
  a narrower scope.
- The cut-level count, its check, and the check that a fork belongs to the
  machine joining it are gone. Three `compile_fail` doc tests on `Fork`
  stand in for the tests that checked them; their errors read as borrowed
  data escaping a closure.
- `m.cut`, `m.scope` and `m.run` take a fresh lifetime `'b` for what their
  forks may borrow, bounded by the machine's scope, so a fork within a cut
  may borrow what the turn declared before the cut; a test shows it.
- Each such scope costs an allocation of the standard library's scope
  data, beside a cut's snapshot and journal; the CLI's batch, with no
  cuts or descendants' turns, does not pay it.

`api-machine-errors` (2026-10-19), in `src/api/turn.rs`, after a discussion
with the maintainer that day:
- A machine's state holds an error flag. An error or panic that ends a
  turn sets it, whether the turn was the runtime's, a descendant's or a
  fork's; a panic that unwinds through several turns flags each machine it
  leaves. Nothing else sets it, and observing the error, as a turn's
  result or a join's, never clears it. A flagged machine is not pruned.
- Entry: `rt.run` and `m.run` fail on a flagged machine without entering.
  `rt.recover` and `m.recover` clear the flag and enter the machine as the
  failed turn left it. `give`, `take` and `fork` fail on a flagged
  machine. Only the target's own flag bars it: a flagged machine between
  bars nothing below it (maintainer). Forking a flagged machine is
  refused; the parent recovers it first.
- `rt.failed(path)` and `m.failed(path)` read the flag without clearing
  it; an absent machine is in no error, and the machine's version fails
  where a machine on the path is forked.
- Forks return nothing to Rust: `m.fork`'s closure returns `Result<(),
  Error>`, a fork leaves what it computed on its machine's stack, for
  `take` after the join, and `Fork<'scope>` carries no result type. `m.join`
  returns the fork's outcome, success, a failure (`Backtrack`) or another
  error, and the forked machine stays flagged.
- Implicit joins at the end of a cut attempt or `m.scope` join in the
  order the forks were made, and the outcome is the body's own error, or
  else the first fork's: an operation diverges if a join does, and fails
  if a join fails, in that deterministic order. `m.scope`'s body returns
  a `Result`.
- Hooks are `FnOnce(&mut Machine) + Send + 'static` and return nothing:
  they handle their own failures, and a cut's outcome is only its own. A
  hook runs on its machine even if a later error flagged it, since it sees
  only the values it captured.
- The end of a turn does not observe the forks it joins (maintainer, the
  same day): once the body has returned, a fork's error stays in its own
  machine, and its panic resumes without flagging the turn's machine. The
  end of an `m.scope` or a cut still observes them, as a join does.
  `Fork` is `#[must_use]`, so a fork left unjoined shows in a warning; the
  tests that leave one do so with `let _ =`.
- Discovery: `rt.machines(path)` and `m.machines(path)` return the
  machines below a path, depth first in key order, each with its path and
  a `MachineStatus` (idle, failed, forked), collected as they stand. The
  walk does not descend into a forked machine, and the machine's version
  fails where the path is forked.
- First built the same day with a list of the errors' diagnostics in place
  of the flag, pushed by recovery, and `m.register_error`; the maintainer
  reduced it to the flag, since recovery reads the machine's state, not
  messages. Diagnostics are left to communication, at `api-ports`.
- Tests: `tests/runtime_api.rs` covers a turn's error flagging its machine,
  the queries, recovery finding the machine as the turn left it and
  entering an unflagged one, a panic flagging, and a descendant in error
  barring its parent's operations. `tests/runtime_forks.rs` covers a join
  returning a fork's outcome with its machine left flagged, a failure
  staying `Backtrack`, implicit joins in join order after the body, and a
  panic flagging each machine it unwinds, a fork left to a turn's end
  flagging only itself, and the walk from the runtime and from a machine.
  The forked tests elsewhere leave
  their counts on their machines' stacks; a cut's forked reader now fails
  until its value is ready, and the cut waits on what it read.

`api-ports`, first slice (2026-10-19), in `src/api/runtime/ports.rs`,
`src/api/runtime/events.rs` and `src/api/turn.rs`:
- `PortState` sits in the runtime's event state beside the runtime
  inputs: buffers by receiving port, a machine's path and a port name, and
  wires by the machine that owns them, with a revision that each wiring
  change advances. A cut's transaction gains an event journal beside its
  store journal; they snapshot, validate and commit together, through the
  existing `transaction_snapshot`, `validate_transaction` and
  `try_commit_transaction`.
- `PortJournal` claims from buffers through FIFO cursors, with
  observations that every alternative of a transaction shares, as runtime
  inputs do; appends at commit; and edits a view of the wiring that its
  routing follows. Validation fails when a buffer a journal read was
  consumed, a buffer it found short gained an item, or the wiring it
  followed changed (coarsely, by revision).
- Machines know their paths from the root. `write(port, n)` routes from
  the machine's output through the wires its ancestors own and the
  pass-throughs of machines on the way, appending each value to every
  buffer reached; an output with no wire on the way is a plain error, and
  a loop is an error. `try_write`, the same as `write` while buffers are
  unbounded, was removed when bounded buffers were deferred. `read(port, n)` and `try_read` claim `n` values or meet
  pushback, `Retry`: within a cut both return it, and outside one `read`
  waits, as a failed cut does, while `try_read` returns it. `available`,
  `input_connected` and `output_connected` observe; `wire` and `unwire`
  edit the machine's own wiring. Each acts within the machine's cut, or,
  outside one, commits at once.
- `ErrorKind::Retry` and `m.retry()`: the outermost cut waits out `Retry`
  as it does a `Backtrack` from its last alternative.
- A fork within a cut branches the cut's port journal too. The merge
  refuses a branch, and the cut runs again, when the cut claimed from a
  buffer the branch read, or changed wiring the branch followed, after
  the fork; the store's and the ports' merges happen both or neither.
- First built with a machine's own ports in one role each, which could
  not feed a child: revised the same day (below).
- Tests: `tests/runtime_ports.rs` covers ordered delivery between wired
  siblings, an unconnected output, duplication and merging, a switchboard,
  the connection queries, a wiring loop, pushback from a short read, a
  read waiting outside a cut, a cut waiting out pushback (forced by an
  abort hook), a failed cut sending nothing, reads seeing committed values
  only, rewiring undone with a cut, a fork whose routing its cut changed
  running again (forced through channels), and `retry`.
- Not yet: bounded buffers, the runtime feeding its diagnostics to the
  root, and the CLI slices.

`api-ports`, second slice (2026-10-19), after the maintainer's review:
- A wire's ends are `WireEnd::In`, `Out` or `Child`, publicly
  `Endpoint::input`, `output` and `child`, an abstract type with three
  constructors (maintainer). A machine wires only its own ports and its
  direct children's. Routing from an output follows its parent's wires
  outward and its own inward; an input passes through where the machine
  wired it on, and otherwise buffers.
- Default diagnostics wiring, `DiagnosticsWiring::ToRoot` unless the
  runtime's configuration says `None`: rules rather than stored wires, a
  child's diagnostics output to its parent's input and a non-root
  machine's diagnostics input to its own output. `unwire` of a default
  wire records a suppression in the owner's wiring, and `wire` lifts it.
  `input_connected` counts the default collection for the diagnostics
  input.
- `m.pending()` returns a machine's inputs holding values, with counts, in
  port order. It records a survey of the machine's buffers, which
  validation compares whole, so a cut that found nothing and retries
  wakes when any input receives, even one that never held a value.
- Tests added to `tests/runtime_ports.rs`: a machine feeding a child
  through its own output and collecting from one through its own input,
  `pending` in port order, a cut woken through `pending` by a new port
  (forced by an abort hook), and the default diagnostics wiring: gathered
  at the root, a machine intervening by unwiring its pass-through, and a
  runtime configured with none.

`api-ports`, the runtime's diagnostics at the root (2026-10-19), the CLI
slice's first stage:
- A diagnostic bus may route its single runtime ingress to a machine's
  input port (`DiagnosticBus::route_to_port`) in place of the input FIFO
  and the fallback output, whatever the route's mode. Each diagnostic's
  transport value arrives outside any transaction, in the bus's order, as
  runtime inputs are admitted, and the change is published, so a waiting
  read wakes. It arrives as a child's write would, passing through where
  the machine wired the input on; where that routing fails, by a loop or
  an unconnected output, it waits in the input itself, since the runtime
  has no writer to fail and nothing should be lost. Both are bridge
  behavior (see [Bridge behavior](#bridge-behavior)).
- `Runtime::open` routes its assembler's bus to the root's diagnostics
  input under `DiagnosticsWiring::ToRoot`; under `None`, the bus keeps
  them. The CLI's runtime, built around its assembler by the bridge, is
  left for the second stage.
- Unread, they gather at the root without bound, the buffers' one policy
  for now.
- Tests, in `src/api/turn/tests.rs`, since only the handle API publishes
  on the bus: the diagnostics arrive in the bus's order and decode, a
  runtime configured with none keeps them on the bus, a read waits for one
  published from another thread, and they follow the root's wiring, or
  wait at the root where it loops.

`api-ports`, `clear` and presence (2026-10-19, maintainer's agreement):
- `clear` returns the machine's ports and its descendants' to their
  initial state too: it claims what their inputs hold as of the
  transaction's snapshot, so later arrivals stay, drops what the
  transaction was to send them, and returns the wiring they own to the
  defaults (`WiringEdit::Reset`). The parent's wires to and from the
  machine stay. Within a cut it is journaled, so a failed cut restores it.
- A machine that holds port state, values waiting in an input or wiring
  it owns, is present: `machines` walks include it, as idle, though its
  machine state is empty and pruned. Pruning leaves port state alone. The
  walk's port part is discovery, which a cut does not validate.
- Tests added to `tests/runtime_ports.rs`: `clear` resetting a machine's
  ports and its descendants', a failed cut restoring what it cleared, and
  the walk counting port state.

`api-ports`, the CLI's logger on machines (2026-10-19), the CLI slice's
second stage:
- The CLI's runtime routes its diagnostics to the root as `Runtime::open`'s
  does: the bridge's `runtime_around` takes the default diagnostics wiring,
  through the constructor both share.
- Before configuration loads, the CLI wires the root's diagnostics input to
  `logger`, a direct child of the root, and the logger's `unread` output to
  its child `logger/fallback`. Both keep what reaches them, unwiring their
  default pass-through; the fallback's would loop back through the logger.
- The batch's turn runs on the root: it starts the logger, assembles on
  `cli`, settles, joins the logger, and renders what remains.
- A configured logger runs on the logger's fork and reads the logger's input
  through `RuntimeEventJournal::read_port`, hidden, for the CLI until
  loggers are turns. When it ends, the fork hands over: one cut wires the
  logger's input on to `fallback` and sends what it holds through `unread`.
  Without a configured logger, the logger hands over at once.
- The CLI renders `logger/fallback` with `DefaultLogger`: between settlement
  pumps while no fork holds the logger, after the join, and where the CLI
  ends without a batch. Settled reports render directly, as before, and a
  failure to render them ends settlement as failed rather than looping.
- A configured logger's lifecycle closes its demand session when its root
  ends (`EffectLifecycle::new_closing_session`), abandoning the tasks it left
  unjoined, as the ingress's lifecycle terminal had done.
- `LogHost`, `LoggerSupervisor`, its fallback endpoint and the route
  activation leave the CLI. The bus stays as the publishers' entry, and its
  counts, with the logger's bus's, decide the exit code. The configured
  logger's reads, its lifecycle, and the CLI's settlement are bridge
  behavior (see [Bridge behavior](#bridge-behavior)).
- The bridge's uses in the CLI rise from seven to nine: the fallback pops
  the transport values it renders, and the CLI's ends without a batch find
  their assembler for the renderer. The logger's test support adds two.
- Found on the way: a read of none panicked at an input that never held a
  value; it claims nothing.
- Tests: in the CLI, the logger keeping diagnostics and its hand-over
  keeping their order, and bus counts surviving reads; the configured
  logger's exit vote, resumed choice and input queued before its first poll,
  against the logger's input; settled reports, and settlement rendering what
  its rendering reports. Retired with their mechanisms: the fallback
  endpoint's delivery failures, the ingress's rearm and its race, and the
  supervisor's teardown. `tests/runtime_ports.rs` adds reading none.
- Not yet: the library's route switching (`DiagnosticIngress::activate`,
  `fallback`, `set_fallback_output`, `logger_terminal`, and the consumer
  activation) has no caller left in the CLI, and retires next.

`api-ports`, the route switching retired (2026-10-19):
- A diagnostic bus's single ingress now always routes to a machine's input
  port: `DiagnosticBus::route_to_port` installs it and returns the ingress,
  for its failure, in place of `diagnostic_ingress`, whose input FIFO and
  fallback output retire with the route modes, `DiagnosticIngress`'s
  `set_fallback_output`, `activate`, `fallback` and `logger_terminal`, the
  settlement-atomic consumer activation and `schedule_diagnostic_consumer`.
- `EffectLifecycleTerminal` and `EffectLifecycle::new_with_terminal` retire
  with `logger_terminal`, their one producer; closing the root's session,
  which a terminal had implied, is `new_closing_session`'s alone.
- Tests: the ingress's own (roots retained through the port and a committed
  read, bound to one runtime and installed once, a failed transport still
  counted, sequence order, the runtime keeping the route, subscribers after
  admission, no runtime retained) read the port; the session-closing tests
  move to `new_closing_session`. Retired with their mechanisms: the consumer
  activation's hidden intermediate state, the fallback's transfer, its
  stale FIFO claim and its publication race, and the terminal's view of
  each root disposition. In their place, a hand-over cut that a diagnostic
  disturbs runs again and keeps their order (forced within the cut's first
  attempt).

`api-ports`, discovering and switching a machine's wiring (2026-10-19,
maintainer's agreement):
- `m.wiring()` reports the wiring a machine owns as a `MachineWiring`: its
  own wires, the default rule that applies to it (`DiagnosticsWiring`), and
  the default wires it removed. Wires its parent owns to and from it are
  the parent's. Rules such as one endpoint for every child are not wires;
  the default rule is reported beside the listing instead (maintainer).
- `m.set_default_wiring(enabled)` turns the default rule for the wires a
  machine owns on or off, transactionally; off, a wire of the rule's shape
  it adds is its own, and a default wire it also owns delivers once when
  the rule is on again. `clear` turns the rule back on. An operation that
  removed every wire at once was rejected as combining too much
  (maintainer).
- Tests added to `tests/runtime_ports.rs`: a machine reporting its own
  wiring with the default rule, and turning its default wiring off and on.

`api-machine-environment`, shares and their composition (2026-10-19):
- The reflection store gains a third fixed volume, the machines'
  environment volume, beside the user heap and the runtime's own. A
  machine's share lies under `children` at each key of its path, then at
  `share`, so one write unbinds a subtree.
- `m.bind_env()` pops an object and binds it as the machine's share, and
  `m.env()` pushes the machine's environment, composed lazily
  (`g_syntax::composed_environment_root`): an anonymous object extending
  each share, the machine's own first, as `object _ extends` would, so a
  share overrides those above it. Each share is wrapped in an anonymous
  object of its own, since glam rejects a named dependency before an
  anonymous one, and an unbound share, `{}`, stands for an empty object
  through `default`. Whether a share may redefine an inherited name
  without `:=` is glam's object semantics; a named one may.
- Within a cut, reading the shares is observed in the cut's store journal,
  and binding is a store write, so forks in cuts, validation and waits work
  as the heap's do. Outside a cut, `env` reads the committed shares.
- `clear` unbinds the shares of the machine and its descendants. Shares do
  not count as presence in the `machines` walk: finding them would read the
  volume's structure, which may be lazy.
- Tests in `tests/runtime_environment.rs`: the composition from the root
  down, a share overriding its ancestors'; an unbound environment as an
  empty object; a binding undone by a cut that ends in error and unbound by
  `clear`; a cut waiting on its environment woken when an ancestor binds
  (forced through a channel).

`api-machine-environment`, promises (2026-10-19):
- `m.promise()` pushes a promised value and its resolver; `m.fulfill()`
  pops a value and a resolver, and `m.break_promise()` a resolver. A
  resolver is an opaque glam value, which may hold no managed root, so it
  carries its promise's identity, and the runtime's turn-side state holds
  each unresolved promise's root until it resolves. Resolution evaluates
  the resolver, never the value; it happens once, a second failing; and it
  is an error within a cut, which cannot undo it, so a cut resolves in a
  commit hook. It advances the observation epoch, so a cut waiting on a
  change wakes. Breaking a promise whose resolver is collected stays with
  `api-request-queues`, now `api-opaque-finalization`.
- An evaluation that waits on what is not known yet, an unresolved promise
  or other blocked work that a client demand abandons, is pushback: its
  error is `Retry`, where it was a plain failure. A machine's evaluating
  operations now pop nothing when evaluation fails, as `write` does, so a
  value that waits stays. Within a cut, such an evaluation counts as an
  observation, and the outermost cut runs again after any change of the
  observation epoch (`perf-fine-grained-wakes` records the coarse wake).
- A bound share is stored as a list of one: `default`, which tells an
  unbound share, `{}`, from a bound one, decides whether a dict is
  logically `{}` by evaluating its entries, and so forced a promised value
  within the share. A list is never `{}`, so the rest of the environment
  reads while a promised value in it waits alone.
- Tests in `tests/runtime_promises.rs`: a fulfilled promise read; a cut
  waiting until a promise is fulfilled, and a promised value within a share
  waiting alone (each forced through a channel after the first attempt
  waits); outside a cut, an unresolved promise as pushback that pops
  nothing; single resolution, the refusal within a cut, a broken promise's
  readers failing, and a value that is no resolver.

`api-machine-environment`, reflection's environment (2026-10-19):
- Once a `Runtime` holds its runtime, machines supply reflection's
  environment: an assembler's reflection host, the assembly's and each
  compilation's macro host alike, reads the `refl` machine's environment,
  composed from the shares as they stand, over the environment it was
  sealed with, which lies beneath them all (`ObjectFromDict`). An
  assembler without a `Runtime` reads its sealed environment as before.
  Roles still derive from the sealed environment, a plain dictionary, so
  each role's `glam` entry stays there for now.
- The reads are outside any transaction, a bridge until reflection tasks
  are machines: a promised value in a share wakes its waiters, as a promise
  wakes any work, but rebinding a share wakes no task that read it.
- The runtime's own diagnostics enter the `refl` machine's diagnostics
  input, as if they were its children's, and pass through to the root's by
  default; one whose routing fails waits at `refl`.
- `glam::transition::reflection_environment(machine, role)` gives the CLI
  a role's environment as a machine sees it, for the configured logger's
  host, which the CLI creates within a turn.
- Tests in `src/api/turn/tests.rs`: reflection reading the `refl`
  machine's environment over its sealed one, and the runtime's diagnostics
  following the root's wiring through `refl`.

`api-machine-environment`, the CLI slice (2026-10-19):
- The CLI's assembler no longer seals a reflection environment of its
  own. Before its configuration loads, the CLI's root binds its share,
  `process`: the user's arguments at `cli.args`, the variables at `env`,
  and promised values at `args` and `refl_args`, whose resolvers the root
  keeps in its local state. The CLI fulfills them after `conf.cli`'s
  expansion, at once where the arguments are known from the start, and
  breaks them where the expansion fails; a broken promise carries no
  message of its own, where the builder's failed with "configured CLI
  expansion failed".
- The prelude gains `process_share`, which makes the share an object.
  `load` compiles the prelude on `cli`, binds the share on the root, then
  compiles the configuration, so the configuration's reflection reads
  `process` as before.
- The configured logger's host takes its environment from the logger
  machine (`glam::transition::reflection_environment`), once, as it
  starts. `PromiseResolver` and the builder's environment closure leave the
  CLI.
- The bridge's uses in the CLI rise from nine to eleven: the process
  variables pushed onto the root's stack, and the logger host's
  environment.
- Tests: `tests/cli.rs` (54), whose configured loggers read
  `process.refl_args` and compare `process.args` with `process.cli.args`,
  and whose configured CLIs and assemblies read the arguments, all pass
  unchanged.
- Profiles (`scripts/profile.sh`; `perf` cannot count instructions in
  this container, so the glam-prof counters, which are exact), from
  `d4ea2d78`, before this session's CLI slices, to `bafbfb92`: `hello_elf`
  78,846 to 78,865 reductions, 39,081 to 39,116 roots and 70,620 to 70,677
  allocations; `hello_do` and `minimal` by the same constants, 19
  reductions, 13 accesses, 35 roots and 57 allocations a run, the cost of
  binding and composing the environment and wiring the logger, under half a
  percent of `minimal`'s.

`api-machine-environment`, mixins, presence and atomic operations
(2026-10-19, after the maintainer's review):
- `m.bind_env()` becomes `m.set_env_mixin()`, beside `m.get_env_mixin()`,
  which pushes the machine's own mixin, or an empty object where it has
  none, to extend and set again, and `m.clear_env_mixin()` (maintainer).
- A record of which machines have a mixin, in the runtime's transactional
  event state beside the ports (`ShareRecord`, `ShareJournal`), changes
  with the mixin in one transaction. Composing an environment takes from
  it the mixins there are, so nothing tests a place in the volume against
  `{}`, and mixins are stored as themselves: the list of one, which only
  kept a set place from reading as `{}` while `default` tested it, goes
  (maintainer). A reader still observes every place in the chain, set or
  not, so setting a mixin later conflicts with it or wakes it. A machine
  with a mixin is present in walks (maintainer).
- Every machine operation is atomic: it succeeds per its contract, or
  fails having done nothing, reflection aside (maintainer). Review found
  `make_list`, `make_dict`, `access`, `anno`, `apply`, `compile_over`,
  `heap_set`, `heap_rewrite`, the evaluating pops (`pop_int`,
  `pop_bytes`, `pop_text`, `pop_atom_name`, `unpack_list`) and `clear`
  changing something before they could fail; they now build first and
  change the stack, or commit, last (`replace_top`, `pop_evaluated`, and
  `transact`, which commits store and events as one transaction). A
  failed `pop_atom_name` now leaves its atom, which one test relied on.
- `break_promise` takes `fulfill`'s shape: it pops a value, a message or
  a diagnostic, then a resolver, and breaks the promise with the value as
  its failure's emission (maintainer). The CLI's broken argument promises
  carry "configured CLI expansion failed" again.
- Tests: a machine extending its own mixin and clearing it, and a machine
  holding only a mixin present in walks (`tests/runtime_environment.rs`);
  an operation that fails doing nothing (`tests/runtime_api.rs`); a
  broken promise's readers failing with its message
  (`src/api/turn/tests.rs`).

## Verification

Runtime changes run `scripts/check.sh full` before commit. Concurrency
fixes force the bad interleaving in a test; repeated runs are not proof
(`docs/AgentContext.md`, "Working Rules"). Public behavior is tested through
the public API, in `tests/`.

## Open Questions (2026-10-18)

- A promise resolves only outside a cut, since a resolution cannot be
  undone: a known limitation (maintainer, 2026-10-19). Revisit promises
  whose resolution a failed cut undoes once the heaps are fast enough.

- Names, per step (above).
- Hook errors, settled at `api-machine-errors` (maintainer, 2026-10-19):
  hooks return no error and handle their own failures, so a cut's outcome
  is only its own.
- Aligning the reflection API, as `refl.*` and `anno refl:(…)` present it,
  with this model (maintainer, 2026-10-19), perhaps making spawned tasks
  (`.task.new` and its relatives) persistent machines in the tree: its
  design is now a step ahead of settlement, `api-reflection-model`, and its
  implementation a later step, after `api-effect-providers`.
- Communication's open details, at `api-ports`: how far the API
  specializes buffers, by measurement.
- The ownership plan's open details, each settled when its step starts:
  retired operations' error (`api-retirement`); the queue capability's
  representation, and a request's status, possibly with a received state
  (`api-requests`); which host state moves into the store
  (`api-hosts-as-machines`).
