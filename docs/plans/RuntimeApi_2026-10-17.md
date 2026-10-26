# Runtime API Plan — 2026-10-17

Status: `api-runtime-turns` built 2026-10-17; `api-cli-bridge`,
`api-machine-data`, `api-cuts`, `api-virtual-resources`,
`api-machine-tree` and `api-forks-in-cuts` 2026-10-18, and
`api-machine-errors` 2026-10-19, with `api-cli-prelude` kept and
`api-scoped-threads` superseded by `api-machine-tree`, then `api-ports`,
`api-machine-environment` and `api-requests` 2026-10-19 and 2026-10-20
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
   - A promise's status, pending, fulfilled or broken, read from its
     resolver, by a machine operation and a reflection effect (maintainer):
     a promised value stands for whatever it evaluates to, so has no status
     of its own. Within a cut the read is a read of the store, which the
     resolution writes, so it conflicts and wakes precisely.
   - Reflection's promise family (maintainer): `.promise.new`,
     `.promise.status`, `.promise.fulfill` and `.promise.break`, with the
     resolvers turns use. Within a cut a resolution is an asynchronous
     send: it goes once the cut commits, as a turn's in a commit hook does
     (maintainer). (Superseded by `api-transactional-promises`: it commits
     with the cut.)
   - Volumes as first-class capabilities (maintainer): an opaque volume
     handle and generic effects, `.vol.new`, `.vol.get`, `.vol.set` and
     `.vol.rewrite`, with `.heap.vol` an effect returning the shared heap's
     handle, so `.heap.*` is shorthand; and machine operations to match, `m.volume_new`,
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
     (`api-effect-providers`), imports with `api-import-requests`.
   - Tests: a failed cut sends no request; a channel's order matches commit
     order; a server answering a request a requester awaits; duplicate
     servers racing, the second resolution failing; a promise's status as
     it resolves, and a cut waiting on it; a volume allocated, written and
     read, a failed cut's allocation leaving nothing, and its handle passed
     between machines.
9. `api-opaque-finalization` — a design step, ending in an ADR, then the
   implementation steps it names (maintainer, 2026-10-20; first planned as
   a representation review, 2026-10-19). What an opaque value stands for
   is reclaimed once nothing reaches it: a resolver collected unresolved
   breaks its promise, and a volume whose handles are all unreachable is
   reclaimed. The design investigates what weak references glam-gc could
   offer, with each mechanism's pros and cons and a recommendation
   ([review](../reviews/OpaqueFinalization_2026-10-20.md)); the maintainer
   suggests modelling resolvers and volume handles as special values in
   the managed heap, glam-gc's, which the collector then sees. ("Heap"
   names several things: the managed heap is glam-gc's allocations, the
   shared heap the store's volume that `.heap.*` reaches.)
   - The inventory (2026-10-20), by each value's fate once the bridge is
     gone:
     - Stay, reshaped: volume handles (`VolumeHandle`), never reclaimed
       today; resolvers (`PromiseCapability`), whose promise a lost
       resolver should break, and whose resolved records stay forever.
     - Stay as they are: compilation origins (`CompilationOrigin`) and
       net construction brands (`ConstructionToken`), which hold no
       resource; `glam.origin.inspect`; continuations, whose map a task
       never prunes, a leak for the task's lifetime.
     - Go: task handles (`TaskHandleCell`) and their queries, with
       `api-reflection-model`; effect tokens (`EffectToken`), with
       `api-effect-providers`; protected volumes' capability dicts and
       `ReasoningVolume`, the Rust `PromiseResolver`, and host-call
       loaders, with the handle API.
   - The problems:
     - Hidden roots: the store holds each volume's root, and the record
       each pending promise's, where the collector cannot see through
       them, so a handle or resolver kept within a volume stays reachable
       through the store: cycles never die, as a task handle within its
       own task's result does not today.
     - Incidental draining: the runtime's external-owner registry destroys
       dead payloads only when a host call drains it.
     - Safe points: breaking a promise takes heap access and a store
       write, so a finalizer queues a notice for a safe point, as a query
       handle queues its retirement for the next commit.
     - Identity: each handle minted mints a payload, so nothing counts a
       volume's handles. A volume's identity matters to conflict analysis,
       which keys on volume IDs, not to users, who do not compare volumes
       (maintainer).
   - The review recommends staging (2026-10-20): resolvers stay opaque
     payloads, a resolver keeping its promise alive but never the reverse
     (maintainer), finalized through death notices, which also reclaim
     acyclic volumes, `api-death-notices`; then ephemerons in glam-gc,
     `api-ephemerons`, and volumes as cells in the managed heap,
     `api-volume-cells`, so cycles through volumes are collected.
     Ephemerons also serve the design's scope-unique dictionary keys.
   - Maintainer decisions (2026-10-20): the first stage's leak of cycles
     through volumes is acceptable while temporary, and the plan returns
     to it; a lost resolver breaks its promise with "resolver lost";
     settlement always collects before it reports. Volumes' committed
     values stay in the store, held through ephemerons keyed on the
     volume's cell, rather than moving into the cell with a snapshot
     recorded at a transaction's first read, as the maintainer first
     envisioned; agreed the same day, which settles the design
     (`opaque-resources-reclaimed-when-unreachable`).
10. `api-death-notices` — the first stage of `api-opaque-finalization`.
   Each volume has one payload, which its handles share and the store
   caches weakly; a volume handle's or resolver's payload queues a notice
   when it dies; the runtime drains its external owners after each
   collection and processes the notices there: a lost resolver's promise
   breaks with "resolver lost" and its record goes, and a dead volume's
   root goes. A transaction that read a reclaimed volume conflicts, and
   one that only wrote it commits without those writes, and settlement
   collects before it reports. Cycles through volumes leak until
   `api-volume-cells`, a known limitation.
11. `api-transactional-promises` — promise resolution within cuts, with
   specialized attention, more than a commit hook for the public
   reflection API (maintainer, 2026-10-20). A resolution cannot be
   undone, so a turn refuses one within a cut, and resolves in a commit
   hook, while reflection stages it in the cut's transaction and sends it
   once the cut commits. A resolution that another beats between the
   staging and the sending is lost without notice. Candidates: the
   resolution as part of the commit, validated against the cut's read of
   the promise, so a lost race conflicts and the rerun, finding the
   promise resolved, fails unless it checked (resolving without knowing
   the status may diverge, maintainer); a cut seeing its own resolutions;
   evaluation within the transaction that fulfills, which alone would do
   better than an asynchronous send (maintainer, 2026-10-20). It follows
   `api-opaque-finalization`, whose design reshapes the record of
   promises.
12. `api-ephemerons` — ephemerons in glam-gc, the second stage's first
   part: managed cells whose value is traced only while their key is
   reachable by other paths, with weak references as the case where they
   coincide, cleared silently. Its marking iterates to a fixpoint
   after the ordinary worklist drains, under the collector's exclusive
   post-mark authority, and it passes glam-gc's own gates.
13. `api-volume-cells` — volumes as cells in the managed heap, closing the
   first stage's leak: a volume's identity is a managed cell, a value kind
   to which handles are edges, and the store holds its committed values
   through ephemerons keyed on it. Cycles through volumes are then
   collected. Decided with the maintainer, 2026-10-20:
   - The store's map stays a Rust persistent map, from `VolumeId` to a root
     on an ephemeron of the volume's cell and a `Contents` cell, one per
     committed version, since snapshots keep older versions and an
     ephemeron is immutable. A managed map behind one root waits for
     managed persistent maps (`perf-managed-volume-table`). The store's
     root holds the ephemeron, never the cell or the value, so it hides no
     cycle.
   - A handle is a value kind of its own, `Volume`, one edge to the cell,
     which suits the maintainer's data representation plans.
   - `Contents` holds just the committed value at first. It is the
     per-version record, which may grow a short history of reads, writes
     and rewrites for finer conflict analysis (`perf-volume-histories`).
   - Death is silent. The store prunes entries whose ephemerons a
     collection cleared when it next creates a volume, the only operation
     that grows its map, if a collection has completed since it last
     pruned.
   - The store holds the cells of its own volumes (heap, runtime,
     environment), a host's protected volume capability holds its cell,
     and a transaction holds the cell of each volume it writes or creates
     until it commits, keying the new version on it.
14. `api-reflection-model` — a design step, without code, ahead of the
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
   - *Decided (maintainer, 2026-10-21):*
     [Reflection Model](ReflectionModel_2026-10-21.md), ADR
     `reflection-tasks-are-machine-tasks`. A reflection task is a task on
     a machine, an effect value the runtime runs, with the machine's
     effectful capabilities and none of a Rust machine's conveniences for
     evaluation. `.child.task Key Op` queues a task through the child's
     task port and returns its result's promise; `.child.status`,
     `.child.interrupt`, `.child.recover` and `.child.clear` act
     asynchronously, and `.child.call` is a transactional remote
     procedure call. Annotation tasks run on anonymous machines under
     `refl`; macro expansion and `conf.cli` are pure evaluation. The
     obligation set, `.todo.*`, takes over `.exit`'s role, and the
     decision classifies which waits are unresolved. Obligations became
     promises a machine accepts, and `.todo.*` retired
     (`obligations-are-accepted-promises`, maintainer, 2026-10-21).
15. `api-import-requests` — renamed from `api-hosts-as-machines` and
   narrowed (2026-10-21), absorbing `lifecycle-host-calls` (L3, L9): a
   forced import asks for its source's binary on a port
   (`imports-are-source-requests`, maintainer, 2026-10-21). An import's
   binary is embedded data, not I/O, and the front end is conceptually a
   pure function over it.
   - The request names the importer's source, the relative path, and
     whether it is a module or a binary, with a reply resolver; the import
     awaits the reply, the binary alone, and a module import compiles it
     on the forcing thread, as today. A macro that forces an import makes
     another request.
   - Requests reach the root's `imports` output by a default wiring rule
     through the root, as diagnostics pass through; a host serves them by
     wiring that output to its server.
   - The CLI slice moved to `api-cli-sources` (2026-10-21). The CLI's
     evaluations still run on the handle API's paths, which give up on an
     evaluation stably blocked on a promise rather than wait for it, and
     its settlement kills such an evaluation as a deadlock, so a server
     forked beside them would race both. Once effect providers, reflection
     machines and settlement have moved those evaluations into turns,
     whose cuts wait, and settlement counts turns, the CLI serves its
     sources without a race.
   - The reflection hosts' retirement (`lifecycle-host-messages`, decision
     6) moved to the steps that replace what they do:
     `api-effect-providers` retires `IsolatedTaskHost` and
     `LoggerTaskHost`, whose work becomes turns with providers, and
     `api-reflection-machines` retires `AssemblerReflectionHost` and
     `TaskHost`'s `snapshot`, `commit` and `validate` hooks.
   - As built (2026-10-21):
     - A runtime that `Runtime::open` opens has no source system: its host
       hands over a top-level source as `ModuleInput::source(identity,
       bytes)`, whose label's extension selects the front end, and a file
       input there is an error. The handle API's assembler, and so the
       CLI's, keeps its source system and today's loaders until
       `api-cli-sources`.
     - A request is `{request: {source, path, kind}, reply}`. `source` is
       the top-level source's identity; `path` is the import's path from
       that source's folder, which nested imports compose, since imports
       are child-relative; `kind` is `'module` or `'binary`. An imported
       source's identity is `{import: path}`, which the compilation's
       import chain leads back to the top-level source, and its label
       joins the path to that source's folder. A script has no folder,
       and imports nothing.
     - A binary import is the reply. A module import is `seq reply
       compile`, where `compile` reads the binary the reply reached, never
       evaluating it, and compiles it on the thread that forces it. A
       broken reply fails the import with the server's reason, so the
       server words it.
     - The default wiring passes the root's `imports` input through to
       its output, under `DiagnosticsWiring::ToRoot`. Where that output
       has no wire, or under `None`, requests wait in the root's input.
     - Fixed on the way: a cut that counted a port's items without
       claiming them all never committed, since validation compared the
       buffer's end with the cut's claims rather than with the end it saw.
     - An import that a macro forces while compiling fails ("macro
       environment could not be selected: evaluation is blocked"), since
       compile-time evaluation treats a wait as an error. Decided
       (`compile-yields-an-open-mixin`, maintainer, 2026-10-21): such a
       wait is pushback, `api-compile-replay`. The same decision keys
       sources as `{package, path}`, so a request carries the key alone,
       without its kind, and `api-compile-ops` replaces the shape above.
     - Moved to `api-runtime-compile`: the test of an import that a
       worker forces (see there).
16. `api-compile-ops` — split from `api-runtime-compile` (2026-10-21):
    compilation as lazy machine operations
    (`compile-yields-an-open-mixin`, maintainer, 2026-10-21). `m.compile`
    conflated loading a file, choosing a front end, compiling with its
    macros, closing the module's fixpoint, evaluating it and returning its
    diagnostics.
    - Sources are keyed `{package, path}`: the package an opaque tagged
      value, such as `{folder: "/home/u/proj"}`, the path a list of keys
      from it, such as `["lib", "leaf.g"]`. A request carries the key
      alone, without its kind or any provenance, replacing the shape
      `api-import-requests` built.
    - `m.import_source` pops a key and pushes a lazy binary, whose forcing
      writes the request.
    - `m.compile` pops `{scope, lang, source}`, the scope being
      `{location, namespace}`, and pushes an open mixin, a function of
      base and self: a compiler-private builtin, partly applied, that
      compiles when applied, on the thread that forces it. It evaluates
      nothing, and the `.g` front end's imports lower to the same builtin.
    - Compile diagnostics go where the runtime's own go, through `refl` to
      the root; a compile that fails, fails when forced.
    - Helpers close a mixin over a base, making the fixpoint where
      bootstrapping will happen, compose inputs, earlier overriding later,
      and build the records for a script and for a source.
    - `ModuleInput`, `m.compile_over`, and the diagnostics and eager
      evaluation of today's `m.compile` leave the turn API. The CLI keeps
      the handle API's path until `api-cli-sources`.
    - Tests: those of `api-import-requests`, on the new operations; a
      mixin closed over a base; two inputs composed; a script importing
      nothing; a failed compile's diagnostics reaching the root.
    - As built (2026-10-21):
      - `m.import_source`, `m.compile`, `m.instantiate` (the fixpoint of a
        mixin over a base), `m.compose(n)` (the top mixin outermost),
        `m.script_record`, `m.source_record` (which pops a package, takes
        the path from Rust, and reads the language from its extension),
        and `m.script_object`, a script compiled and instantiated over a
        base.
      - The open mixin is `compile_source compiler record`, a partial
        application of a compiler-private builtin whose first argument is
        an opaque `SourceCompiler`, a host closure over the assembler's
        deferred route, until `api-runtime-compile` gives the runtime a
        compile service of its own. `mixin_compose outer inner` composes.
      - The builtin's machine converts the whole record to a key, which
        forces its source to a binary and lets the compile read its scope
        and language as data, then returns a host call that compiles over
        the base and self.
      - A served import composes its key from its importer's location and
        lowers to the same builtin, with a compiler per import site that
        carries the site's provenance for the trace; a served source's
        identity in the trace is its key.
      - The CLI compiles through `glam::transition::compile` and
        `compile_over`, the old eager path, until `api-cli-sources`: the
        bridge's uses in the CLI rise from 13 to 16.
      - Each compile still makes its own `CompilationExecution` and drains
        it after, so a compile that a worker forces hangs until
        `api-runtime-compile`, and one that a macro's import waits on
        fails until `api-compile-replay`.
17. `api-compile-replay` — a wait while compiling is pushback
    (`compile-yields-an-open-mixin`): an attempt that waits on a reply
    abandons, evaluation waits on the reply and applies the compile
    again. A per-compile cache keeps the lazy binaries the compile has
    received across its attempts, and goes when it completes; a
    runtime-wide table of replies is rejected, since binaries may be large
    and dependencies are mostly affine. Diagnostics are held until an
    attempt completes, and invocation IDs count compiles, not attempts.
    Replay is the bridge to resumable compilation (`compile-resumable`).
    - Tests: an import that a macro forces while compiling (from
      `api-import-requests`), and a chain of two such imports.
    - As built (2026-10-21):
      - A compile-time evaluation that halts waiting reports the pending
        reply it ultimately waits on, followed through the coordinator's
        blocked work; the front end notes the first on its compile
        context, and the attempt runs on, so that its other compile-time
        requests go out too.
      - `compile_source` takes a fifth argument, `replay`: `{}`, then
        `{invocation, sources}`, an ordinary value the collector traces,
        holding the compile's invocation ID and the lazy binaries it asked
        for, by key. An attempt that waited returns `seq reply
        (compile_source compiler record replay' base self)`, and drops
        its diagnostics.
      - A compile forced while its importer's attempt runs shares that
        attempt's cache, through a weak reference its compiler holds, so
        a chain of compile-time imports asks for each source once.
      - A revived compilation execution drains its macro session alone,
        not the whole background domain, which holds the evaluation that
        forced the import (moved from `api-runtime-compile`): the test of
        an import that background work forces, from
        `api-import-requests`, passes.
      - Tests: also that a compile which waited reports its diagnostics
        once.
18. `api-stack-ops` — a side path (maintainer, 2026-10-21): a solid stack
    semantics for the machine, whose operations were first proposals no one
    had reviewed. A machine's state may move into a `RootFrame<State>` later,
    which makes the stack's semantics, not its roots, what lasts.
    - Every operation's arity is static: it depends on the operation and
      its Rust arguments (`n`, keys, paths, patterns), never on what the
      stack holds. `unpack_list` and `unpack_dict` go.
    - Packing and unpacking are symmetric: one after its twin is the
      identity on well-typed data. Operations that neither extract data to
      Rust nor evaluate explicitly build lazy thunks.
    - Names read `type_op`, and the families mirror reflection's effects.
    - Stack: `dup(n)`, `discard(n)` (Clippy reserves `drop`), `swap`,
      `flip(n)`, `stash(n)`, `stash_pop(n)`, and `shuffle(pattern)`, a
      small language: in `"abc-cabbca"` the names left of `-` bind the top
      items, the rightmost the top, and the right side lists the outputs.
    - Scalars: `unit_push`, `int_push`/`int_pop`, `text_push`/`text_pop`,
      `bytes_push`/`bytes_pop`, `atom_push`/`atom_pop`,
      `abstract_path_push`/`abstract_path_pop`.
    - Lists: `list_pack(n)` and its twin `list_unpack(n)`, which fails
      unless the list has exactly `n` items; `list_append`, `list_split`
      and `list_split_end` (`xs i -- front back`), `list_len`.
    - Dicts: `dict_pack(keys)` and its twin `dict_unpack(keys)`, which
      fails where it would drop a key; `dict_insert_keys(keys)`
      (`i1…in d -- d'`) and `dict_extract_keys(keys)` (`d -- i1…in d'`);
      `dict_insert` (`item dict key -- dict'`) and `dict_extract`
      (`dict key -- item dict'`); `dict_union`, lazy, merging a key both hold
      as glam's dict literals do, so a conflict fails where forced; `dict_get` and path operations; `tag_pack(k)`
      and `tag_unpack(k)` for singleton tags. Keys are any `Key`.
    - Evaluation: `eval`, and `eval_budget(&mut steps)`, which spends the
      borrowed budget and returns `ErrorKind::Retry` once it runs out, so
      its caller sees the remaining budget (maintainer).
    - Local state: `local_set`, `local_get`, which pushes `{}` where the
      machine holds nothing (maintainer), and `local_take`.
    - Values: `value_push` and `value_pop`, with `Value<'scope>`, a handle
      rooted for the rest of the turn; the handle API's unscoped value
      takes a transitional name until it retires.
    - Later: `list_reverse`, which needs a builtin, and reflection's
      `dict_entries` and `dict_pairs` with their twins.
    - As built, first slice (2026-10-21): the stack, scalar, list, dict,
      tag and local-state operations, and the families renamed to mirror
      reflection's effects: `promise_new`, `promise_fulfill`,
      `promise_break`; `env_get` and `env_mixin_*`; `port_write`,
      `port_read`, `port_try_read`, `port_ask`, `port_available`,
      `port_pending`, `port_input_connected`, `port_output_connected`;
      `wire_add`, `wire_del`, `wire_list`, `wire_defaults`; and
      `source_import`. `dict_get(key)` and `dict_get_path(keys)` replace
      `access`, whose dotted strings named atoms only. Builders compose the
      evaluator's lazy builtins: `Append`, `ListSplit` (its `{left,
      right}` halves), `ListLen`, `DictUpdate` (whose `{}` removes) and
      `DictUnion`; `dict_unpack` and `list_unpack` evaluate, to check
      their shape. The CLI tells an empty list by `list_len`.
    - As built, second slice (2026-10-21): `Value<'scope>`, a handle that
      `value_pop` returns and `value_push` takes, covariant in the turn's
      scope, so a turn within may push its enclosing turn's values and none
      leaves its turn (a compile-fail doc test). The handle API's value is
      exported as `glam::HandleValue` until it retires. The CLI builds its
      environment's dict with `dict_pack` over byte-string keys, so
      `transition::push_value` goes, and the bridge's uses in the CLI fall
      from 16 to 14.
    - As built, third slice (2026-10-22): `eval_budget(&mut steps)` drives
      its client demand with a budget, charged for the reductions of the
      client's polls, its pumps and the causal children it polls, never
      for observing, walking a route or waiting on other threads
      (`reduction-costs-one-budget-unit`). Once spent, the demand is
      abandoned with a new halt, `BudgetSpent`, which the API reports as
      pushback; what it evaluated stays, so another call continues.
    - Fixed on the way: the pump charged each poll its whole reserved
      quantum of 64 steps, whatever it reduced, so every step budget that
      pumps drained about six times too fast: a sum over thirty levels
      made 1,566 reductions in its pump's 148 polls and was charged 9,472.
      It now charges the reductions and returns the rest. (The scheduler's
      fairness loop then still counted each poll it ran; the poll budget
      plan removed that, with the yields that needed it.) A sum over three hundred levels costs
      about 10.8k steps, and budgets of 16, 100 and 1,000 finish it in
      681, 108 and 11 calls; a budget of one step may not, since each call
      spends a step to resume.
      Pump tests that stepped one poll per unit of budget now step one
      reduction: their fixture machines pay a unit for each step of work,
      and blocking stays free.
    - As built, fourth slice (2026-10-22): `list_reverse` (`xs -- ys`);
      `dict_pairs` (`d -- [[k, v]…]`, in key order) and its twin
      `dict_from_pairs`; `dict_entries` (`d -- keys values`) and its twin
      `dict_from_entries`. All are lazy, on three new builtins, kept from
      glam's std since listing a dict's keys bears on abstract and
      scope-unique keys: `ListReverse`, a walk of the list observation
      machine; `DictPairs`; and `DictFromEntries` with `DictFromPairs`,
      which walks the pairs, each to exactly two items, before the shared
      key conversion. `dict_entries` maps `ListHead` and `ListAt 1` over the
      pairs, which are well-formed by construction. A twin fails where
      forced on lists of unequal length, a key given twice or a pair of
      another length, and leaves out members already `{}`.
    - Then (maintainer, 2026-10-22, `dict-keys-listed-by-reflection`): a
      dict's keys are listed only by reflection, so glam's std gains the
      constructors alone, as `'dict`'s `from_entries` and `from_pairs`,
      with `reverse` in `'list`. The reflection API may later list members
      through `DictPairs`, consistently with client machines.
19. `api-runtime-compile` — split from `api-import-requests` (2026-10-21):
    (Sketched with the maintainer on 2026-10-22: a runtime-owned compile
    service, which the compile builtin's opaque compiler operand reaches
    weakly with its import site, so no worker builds an `Assembler`; its
    front-end lookup is one place that `compile-bootstrap` later extends,
    since user-defined syntax has not started. Compilation is a pure
    function, so its diagnostics are reflection effects, never output of
    the machine that asked for the compile.)
    compilation builds its context from runtime-side parts, the value
    factory, the front ends, an invocation counter and a diagnostics
    port, so no worker reaches an `Assembler`, a `DeferredAssembler` or a
    `SourceSystem` (L3), and macro sessions no longer revive a
    `CompilationExecution`. `DeferredAssembler` retires, but for the
    loaders that read files, which go with `api-cli-sources`.
    - The test of an import that background work forces, from
      `api-import-requests`, landed with `api-compile-replay`, which
      narrowed a revived execution's drain to its macro session.
    - No compile drains (maintainer, 2026-10-21): a compile ends when its
      front end does, and sparks and reflection work its macros started
      run on in the background, with no imports or diagnostics of that
      compile. One `CompilationExecution` per runtime, made by the first
      compile, serves every compile, so none is revived, and a dropped
      per-compile macro session no longer abandons what its macros
      started. Done ahead of the rest of this step, with the drain's own
      test of detached `.task.new` failures, which retire with task
      handles.
    - As built (2026-10-22): the reasoning session is the runtime's compile
      service. It holds the value factory, the shared execution, the
      runtime's one invocation counter (moved from the `Assembler`) and
      the diagnostics bus, and its methods take the served path
      (`api/assembly/compile.rs`): the compile builtin's compiler holds the
      session weakly with its import site, so a served compile on a worker
      builds no `Assembler`; a source audit keeps the module from naming
      one. Its diagnostics go to the session's bus, which `Runtime` routes
      into `refl`'s diagnostics input, as reflection effects. The front
      end is still chosen by extension, in one place. `DeferredAssembler`
      stays only for the file loaders, which go with `api-cli-sources`.
20. `api-effect-providers` — absorbs `lifecycle-leases`. A turn runs a
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
      public API; with the logger's turn running `conf.log`, `LoggerTaskHost`
      retires too (from `api-hosts-as-machines`, 2026-10-21). They and the macro runner's family lose their reflection
      effects and promises (`.env`, `.log` where it reaches reflection,
      `.promise.*`), which they carry only as bridge behavior: `conf.cli`
      and macros are pure evaluation, ideally handled by pure functions
      (maintainer, 2026-10-21).
    - Sketched with the maintainer (2026-10-22,
      `effect-families-pass-through`): `m.effect_run(&mut provider)` runs
      `state effect -- state' result`; the pure effects, state and the
      reset stack among them, stay in the effect machine, and the host's
      requests return to the turn. The provider's flags say which host
      families pass through to the machine (the heap, promises, `.log` to
      the diagnostics output, `.env`), and it defines the rest. It answers
      `State Args -- State Response` on the stack: `Return`,
      `Alternatives(n)`, or `Scoped { close }`. Slices: `effect_run` with
      returns and failure; host families passing through by flags; state
      kept pure with the reset stack (below); search, with the turn's cuts
      checkpointing alternatives for their host effects; `TokenEffects`;
      then `CliEffects`, each a provider or a glam library over a thin one.
    - Built (2026-10-22): `effect_run` with returns and failure. The
      effect machine runs on the turn's thread; a provider's request parks
      on a promise the turn fulfills once the provider has answered on the
      machine's stack, and `m.fail()` fails the request within the effect.
    - Built (2026-10-22): families passing through by flags. The turn
      serves each family in the provider's `EffectFamilies` (all, by
      default) as the machine's own methods do: `.heap.*` and `.vol.*`;
      `.promise.*`; `.log` to the diagnostics output; `.env`. The effect
      machine converts a path argument to keys before the request parks. A
      provider may define a family it does not pass through.
    - Built (2026-10-22): state kept pure, as the maintainer chose.
      The first draft sent `.get`/`.set` to the machine's locals, parting
      state from the reset stack, which `.get []` must capture. Now the
      effect machine serves `.get`/`.set` as it does `.reset`/`.shift`,
      and `effect_run` takes and returns the state. A provider's request
      receives it below its arguments (`RequestContext::state`) and
      answers with the state to continue from, which the request demands
      to a dict before installing (`set_state`), as `.set []` does. The
      provider answers on a stack of its own, as a commit hook runs: only
      the state and arguments, an empty stash, and an enforced
      `state' result` left; the turn's stack and stash return after. A turn's effect machine
      leaves the host families to the turn
      (`TaskSpecialization::serves_host_families`) and, unable to undo
      their effects in a failed alternative, has no search until the next
      slice: until then an effect a turn runs has no `.alt`, `.fail` or
      `.cut`.
    - Decided with the maintainer (2026-10-22,
      `alt-cuts-its-first-operand`): `.alt a b` means `.alt (.cut a) b`
      for machines and reflection tasks, an effect's cuts scope its
      choices, and waiting answers only a failure nothing handles. An
      effect's `.cut` is a turn transaction: the outermost one outside
      `m.cut`, nested within it. Built first for reflection tasks: a choice
      within a cut leaves its second operand among the cut's alternatives
      and enters a cut for its first; outside every cut, the first
      operand's cut takes the second as its fallback.
    - Built (2026-10-23): search in effects a turn runs. The effect runs
      as a task whose host runs its cuts (`HostedEffectRun`), searching
      for its first complete success. It reports where its outermost cut
      begins, commits or fails, and where it resumes a branch set aside
      at an earlier version of the turn's state, counted in requests the
      turn answered; the turn takes a checkpoint, as a nested cut saves
      one, only before answering with a branch set aside since its last
      answer, so search that never reaches the turn takes none. Outside
      `m.cut`, the effect's outermost cut is the turn's transaction,
      begun, committed and retried as `m.cut`'s are through primitives
      factored out of it; within one, it nests. A provider may answer
      `Alternatives(n)`. Undo-based checkpoints are to explore
      (`perf-undo-checkpoints`).
    - Cleaned up (2026-10-23, with the maintainer): the run pauses with
      `EffectTaskPoll::Paused`, one event a poll, in place of a queue
      signalled by an empty block; a commit waits for the turn's answer
      (`resume_commit`); and a restore is raised where a set-aside branch
      resumes, a cut's next alternative or a fixpoint's restart, with a
      debug check that each step drives the turn's present version, so
      tasks the host does not run pay nothing.
21. `api-atomic` — `m.atomic`, a transaction run once (maintainer,
    2026-10-23): as `m.cut`, without its automatic retry. Built on the
    primitives `api-effect-providers` factored out of `m.cut`
    (`transaction_begin`, `_end`, `_abort`). Outermost, a success commits;
    a failure or pushback rolls back and returns, without waiting; a
    conflict at commit rolls back and returns a new
    `ErrorKind::Conflict`, so a caller tells "run again now" from "run
    again after a change". Nested within a cut, it is a nested cut. An
    effect run's own transaction may share it, keeping the retry it does
    on purpose.
    - Built (2026-10-23): `m.atomic(body)`, one pass of `m.cut`'s loop over
      the same primitives. A failure or pushback undoes the body and returns
      its error at once; a commit that conflicts, or forks that merged in no
      serial order, undo it and return `ErrorKind::Conflict`; within a cut,
      it is a nested cut. Its body runs once, so it may be `FnOnce`. Tests
      force each outcome, the conflict by a commit between the
      transaction's read and its commit.
22. `api-reflection-machines` — reflection tasks become tasks on machines
    (`reflection-tasks-are-machine-tasks`), the implementation of
    `api-reflection-model`: `.child.*`, `.port.*`, `.wire.*`, `.env.*`
    shares and accepted promises (`obligations-are-accepted-promises`) for
    tasks and turns alike,
    annotation tasks on anonymous machines under `refl`, and the
    retirement of task handles, `.task.*`, exit votes and the `'macro`
    role. Tasks commit to the runtime's store directly, so
    `AssemblerReflectionHost` and `TaskHost`'s `snapshot`, `commit` and
    `validate` hooks retire with the handles (decision 6, from
    `api-hosts-as-machines`, 2026-10-21). Its notes are in
    [Reflection Model](ReflectionModel_2026-10-21.md).
    - `.try Op` (maintainer, 2026-10-23): `Op` run once as a transaction,
      returning `ok:Value`, `'fail`, `wait:Diagnostic` (input or a promise),
      `conflict:Diagnostic` or `err:Diagnostic` (an evaluation error), the
      effectful counterpart of `.eval` and glam's counterpart of
      `m.atomic`. See
      [Trying an operation](ReflectionModel_2026-10-21.md#trying-an-operation).
      An effect of today's reflection machine, it may be built ahead of the
      rest of the step.
    - CLI slice: `conf.log`, which the logger's turn runs as an effect
      since `api-effect-providers`, reads its diagnostics input and writes
      a `stderr` port with `.port.*`, and its exit vote goes (gone
      2026-10-25, `exit-votes-retire`).
    - Slices (2026-10-23), in roughly this order, which is the
      implementer's to adjust (maintainer); checkpoints where they make
      sense, and the full gate at least once a slice:
      - `api-refl-try`: `.try`. Built (2026-10-23): a cut frame marked as
        trying, which answers rather than retries. Its commit's conflict,
        its operation's failure with no alternative, an error within it,
        and a block on a promise no one resolved are its answers, `fail`
        the bare atom `'fail` (maintainer, 2026-10-23). A task waits on a promise only after pumping what
        it waits on makes no progress; work another worker runs is waited
        for. Where a turn runs the effect, the turn decides, since the
        requests it serves park on promises it resolves:
        `HostedEffectRun::answer_try_wait`. Its commit event carries
        whether the cut retries, so the turn ends a `.try`'s transaction
        on a conflict rather than beginning another. An isolated search
        rejects `.try`.
      - Then (2026-10-23, `try-reports-outcomes`): `.eval V` became `.try
        (seq V (.r V))`: it answers `wait` where it waited, and moved from
        the reflection specialization to the machine's own requests, so
        turns' effects have it too. A `.try` that answers `wait` records a
        read of the promise's status in the transaction around it, as
        `.promise.status` does, so a cut that fails on the `wait` parks on
        what it observed and wakes on that promise's resolution, however
        many promises it waited on; a turn's cut records the same read
        (maintainer, on several promises in one cut). `wait` is for the
        store's promises, found through `awaited_promise` and the store's
        pending cells: a promise internal to evaluation is work in
        progress, waited out. `.try` runs nothing to decide: it asks when
        the task blocks and after the poll's pumping, and the scheduler
        re-polls a task once the work it waits on rests, so a promise
        behind lazy values or a reflection gate answers `wait` as one held
        directly does (tested both ways). Pumping within `.try` had spun
        on a panicked producer's wait.
      - `api-promise-cells-review` (reviewed 2026-10-23), then
        `api-promise-cells` (built 2026-10-24,
        `promise-cells-are-weakly-resolved`): one identity per promise, its
        cell's; resolvers as the `Resolver` value kind, edges to managed
        breakers that keep each promise's status and queue its ID when
        they die; the store holding each pending cell weakly, by ID, so no
        promise lives for the store, and `.try` finding the awaited cell by
        lookup, the scan gone. Resolvers no longer compare. Ahead of child
        tasks, which answer with promises. See
        [Promise Cells](PromiseCells_2026-10-23.md).
      - `api-comparables` (maintainer, 2026-10-24): only comparables
        compare, as specified: numbers, and lists or dicts of comparables.
        Ordering is for numbers and lists, never dicts; texts and binaries
        are lists of bytes, and atoms are dicts. Comparing anything else,
        for equality or order, is an evaluation error that reports it
        incomparable: `err` under `.try`, never `'fail`, never `false`.
        Resolvers already are (`promise-cells-are-weakly-resolved`). What
        differs today, in `eval/comparison_machine.rs`:
        - opaque values compare equal by identity, and volume handles by
          the volume they name; equality between one and any other value
          is `false`;
        - `tuple:[…]` dicts, which the tuple syntax `(a, b)` builds, order
          lexicographically, as `Syntax.md` has it ("two tuples"): kept,
          tuples are the special case (maintainer, 2026-10-24);
        - values of two different comparable kinds compare unequal, and
          ordering them is an error, as `Syntax.md` has it: kept;
        - an atom never equals a dict; it should equal the dict it stands
          for, `[key]:()` (maintainer, 2026-10-24). Unit is a primitive
          atom, never constructed, so it stands for no dict.
        Comparing with `{}` is no test of definedness: `{} == \x -> x` is
        incomparable, an error (maintainer, 2026-10-24). A primitive,
        `defined X`, takes that role, and the compiler's lowering (the
        reflection scanner's `== {}` guards) and tests move to it.
        Tests: each incomparable kind under `==` and `<`, directly and
        within a list and a dict, answering `err` under `.try`.
        Held (2026-10-24) behind [Dict Semantics](DictSemantics_2026-10-24.md),
        which makes `{}` the empty dict rather than undefined, union
        shallow, and adds the definedness operations its checks move to.
        Built 2026-10-24 (`only-comparables-compare`), after Dict Semantics
        made dict equality structural, atoms equal to their dicts, and
        moved the definedness checks to `dict.defines`: one check rejects
        an incomparable operand, a function, net, sealed carrier, opaque
        value, volume handle or resolver, for equality and order, with one
        message, "cannot compare Kind"; opaque values and volume handles
        no longer compare by identity. Tested for each kind under `==`,
        `<>` and `<`, directly, against a number, and within a list, a dict
        and a tuple, and, for volumes and resolvers, as `err` under `.try`.
      - `api-refl-child-tasks`: `.child.task`, `.status`, `.recover`,
        `.interrupt` and `.clear`; `.task.*` and task handles retire.
        Designed in [Child Tasks](ChildTasks_2026-10-24.md): each anonymous
        reflection task is its own root for child tasks (maintainer,
        2026-10-24). Built so far (2026-10-24): the port journal in
        reflection transactions, `.child.task` and `.child.status` with
        child machines' queues, persistent local state and flags,
        `.child.interrupt`, `.child.recover` and `.child.clear`, and the
        compiler's reflection boundaries on child tasks, failed machines
        reported at settlement, and the retirement of `.task.*`, task
        handles, status queries and the failure ledger (2026-10-25).
      - `api-refl-call`: `.child.call`, with a child's machine state where
        a caller's transaction stages it. Designed in
        [Child Calls](ChildCalls_2026-10-25.md), drafted 2026-10-25 for
        review: child state in the event state's transactional record, a
        call frame with a per-branch machine, and the plain call as a
        queued, awaited task. The maintainer decided its questions the
        same day: an error ending a call flags the child, a call cannot
        enter a child in error, and one after tasks its own transaction
        queued on the child fails with a conflict error. Built
        2026-10-25: `child-state-transactional`, `child-call-frame`, the
        call within a transaction, and `child-call-plain`, the queued,
        awaited call outside one.
      - `turn-pending-input-conflicts` (maintainer, 2026-10-25: machines
        get a call's rule), retired the same day: built for `run`,
        `recover`, `give` and `take`, it refused a turn that entered a
        child its cut had sent input to, then was reverted on review with
        the maintainer. There is no conflict: a machine has no driver of
        its own, its ports buffer input for a turn to read, and a read
        within the cut of what the cut sent is pushback, as reading one's
        own write is (`a_cut_may_enter_a_child_it_sent_input_before_the_input_arrives`).
        Only a reflection child's task queue, which the child drains by
        itself, its tasks work forked onto it, can be overtaken, and a
        call's rule covers it.
      - `api-refl-annotations`: annotation tasks on anonymous machines
        under `refl`.
      - `api-refl-ports`: `.port.*`, `.wire.*` and `.env.set`, `.env.mine`
        and `.env.clear` for tasks.
        Ports and wiring built 2026-10-26 (`refl-ports`): the effects the
        reflection model maps, on the branch's machine, with pushback as
        the ports ADR decides it, a request result and branch outcome of
        its own that no alternative or fallback takes, waited out by the
        outermost cut, answered as `wait` by a `.try`, and blocking a read
        outside every cut until what it read changes. Choices of the
        implementer: `.port.pending` answers a dict from port to count,
        `.wire.list` a record `{wires:[{from, to}], defaults, unwired}`,
        and `.wire.defaults` takes `'on` or `'off`, since glam's booleans
        are pass/fail effects. The test forces each wait by a heap gate;
        building it found two faults, a cut's pushback surviving into its
        rerun and a fallback consumed by a pushback, both fixed and
        mutation-checked.
        Decided after (maintainer, 2026-10-26), in the order built: `.retry`
        (`api-refl-retry`, no longer a candidate), on the pushback outcome,
        a permanent divergence error outside every cut or with nothing
        observed (built 2026-10-26: a core effect beside `.fail`; its test
        and each rule mutation-checked);
        the environment as `.env.lookup Path`, `.env.overlay Mixin`, whose
        overlays stack, and `.env.clear`, composed along the task's own
        machine, `.env Path` and `.env.mine` going
        (`machines-inherit-an-environment`; built 2026-10-26,
        `refl-env-overlays`: a lookup composes the shares along the
        branch's machine over the host's sealed environment within the
        transaction, observed in a cut; turns gained `env.overlay` and
        `env.clear` effects and `m.env_overlay()`. Overlays compose as
        objects, so an overlay overrides an inherited name with `:=`, as
        any mixin does. Building it found the store's reads keyed abstract
        paths raw where its writes canonicalize them, so a lookup missed
        what an overlay wrote; reads now canonicalize too.
        Each rule mutation-checked); `.port.unread Port List`, a
        putback at the head of the machine's input, for tasks and turns
        (built 2026-10-26, `refl-port-unread`: `.port.unread` and
        `m.port_unread(port, n)`, which pops `n` values as
        `m.port_write` does. Choices of the implementer: a transaction's
        own reads, counts and `.port.pending` see its putbacks, ahead of
        the buffer, unlike its appends, which no read of its own sees,
        since a putback undoes a read and the next read must find it; a
        putback reads nothing, so `.retry` after one alone diverges for
        good; its commit gives the buffer sequences beyond all it held,
        so any transaction that claimed from the old head conflicts. A
        fork's putbacks merge at its join, conflicting where its cut
        changed the same input's putbacks, a rule no turn can reach yet,
        since a fork's subtree is out of its cut's reach until the join.
        Each rule mutation-checked);
        then the logger, `api-refl-cli-log`, whose machine runs `conf.log`
        through its turn's `effect_run`, ports passing through to the
        turn's machine, so `.read_log` and `.write_stderr` become
        `.port.read 'diagnostics 1` and `.port.write 'stderr [Bin]` and
        `LoggerTaskHost` retires.
      - `api-refl-obligations`: accepted promises, for tasks and turns.
        Designed in [Obligations](Obligations_2026-10-25.md), drafted
        2026-10-25 for review, decided the same day (acceptance explicit,
        what is not clear stays visible); built the same day:
        `obligations-record`, for tasks, and `obligations-turns`.
      - `api-refl-retry` (maintainer, 2026-10-25, candidate): an explicit
        `.retry`, intentional divergence until something observable
        changes, as opposed to `.fail`'s backtracking, the counterpart of
        Rust's `m.retry()`. Pushback today reaches glam only through what
        cannot proceed, a short port read or a wait; `.retry` would let a
        task say it. The quiescence analysis counts such a wait idle,
        where an error awaiting a change is unresolved
        ([Quiescence](Quiescence_2026-10-25.md)).
      - `api-refl-retirements`: exit votes and `.exit.*` (retired
        2026-10-25 by `exit-votes-retire`, under `api-quiescence`),
        `AssemblerReflectionHost` and `TaskHost`'s hooks, and the `'macro`
        role, host, profile and bus. `macro-import-waits` is revisited here,
        where the macro session moves to the pure front end, or with
        `compile-resumable` if that fits better (maintainer).
      - `api-refl-cli-log`: the CLI slice above.
23. `api-settlement` — redirected to `api-quiescence`
    (`quiescence-is-analyzed`, maintainer, 2026-10-25): settlement as a
    staging step belonged to exit votes, which are retiring, and under the
    bridge it killed every task waiting without a vote. A halted runtime
    is analyzed instead, at any time, by whoever asks, who decides what
    follows; [Quiescence](Quiescence_2026-10-25.md) holds the end state
    and the transition, whose first step, `quiescence-analysis`, was built
    2026-10-25, and whose host options, `quiescence-options`, interrupt and
    clear, 2026-10-26, after which settlement retired
    (`settlement-retires`, 2026-10-26). The text below is the step as first planned (maintainer,
    2026-10-19), in place of exit votes. It follows
    `api-reflection-machines` (moved 2026-10-21), so it meets reflection
    tasks as machines and no exit votes, rather than building on both. The runtime is
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
    - Which waits are unresolved, and that a batch may end with tasks
     still waiting once nothing is: as `reflection-tasks-are-machine-tasks`
     classifies them (2026-10-21).
    - CLI slice: the CLI's two settlement loops become one, and the bridge
     behavior below that settlement retires goes.
    - `settlement-clears-machines` (maintainer, 2026-10-25): machines in
     error stay whole, with their state, until settlement clears them on
     a decision its host or user makes from the report, such as a CLI
     option or an IDE's command: failed child machines and failed
     anonymous roots, and alike the work blocked permanently on
     divergence, which settlement kills and an interrupt flags: a call
     blocked for good within a cut flags both its child and its caller
     (maintainer, 2026-10-25). Clearing
     drops each machine with its subtree and its failure; inspection tools
     may later clear at a finer grain. Until then they stay as long as the
     runtime does. Designed in
     [Settlement Clearing](SettlementClearing_2026-10-25.md), drafted
     2026-10-25 for review: kills flag in their own report, and a host
     clears orphaned failures through the report.
    - Candidate (maintainer, 2026-10-20): recognize a self-wait from
     outside, for the report: a cut that waits on the value of a promise
     it resolves itself, which only its commit would settle. Inspecting a
     deadlocked machine's or reflection task's state, its staged
     resolutions beside where its wait ends, can explain the divergence,
     a better diagnostic than a bare deadlock, without the cross-layer
     check that evaluation would otherwise need at every wait. Turns
     already fail such a cut when their own evaluation returns
     (`api-transactional-promises`).
24. `api-cli-sources` — the CLI slice of `api-import-requests`, moved after
    settlement (2026-10-21): a CLI machine serves the requests with
    `FileSourceSystem`, records the reads, and writes the manifest and
    checks that no file changed once settlement is done, so imports forced
    during settlement or by the logger count. The CLI opens its runtime
    with `Runtime::open` and hands its top-level files over as sources;
    the bridge's constructor goes, and with it the assembler's source
    system and the loaders that read files.
25. `api-handle-retirement` — the public handle types go: `Assembler`,
    `ModuleBuilder`, readiness results, `EffectRun`, endpoints and buses,
    and `Values` as a separate handle. The bridge goes with them, and the
    public tests move to turns (see
    [Parity](#parity-for-handle-retirement)). Standing docs describe the new
    API.
26. `api-retirement` — absorbs `lifecycle-owner`'s retirement protocol (L1,
    L5, L6, L10). With the handles gone, the `Runtime` is the only owner,
    and dropping it retires in order: admit no new work; stop, wake and
    join the workers; fail pending operations as retired; close sessions
    and work records, dropping stored providers and closures; release the
    background demand and retire the queues; retire the value domain. No
    turn is in flight, since a turn borrows the runtime mutably and joins
    its forks.
    - Tests force each ordering: retirement with work queued, a pending
      operation failing as retired, and values inert afterwards.

Later, beyond this plan (2026-10-21):
- `compile-resumable` — front-end compilers as monadic effects whose
  assembler-provided handler is pure, resumable at their import effects
  as evaluation is; it retires replay (maintainer).
- `compile-bootstrap` — user-defined front ends, `_env.lang.[lang].compile`
  from the base, and bootstrapping where a mixin is closed: closing with
  one compiler, reading the next from the result, until it stays the same.
  Which object's `env.lang` counts, where inputs disagree, needs a
  semantics pass first. Closing stays `fix (mixin base)` without it
  (maintainer, 2026-10-21). The record takes an optional `compiler`,
  used in place of one drawn from the base, so `compile_source` serves a
  bootstrap loop with small changes; the builtin then converts `scope`,
  `lang` and `source` to a key and passes `compiler`, a function, as a
  value.
- `macro-import-waits` (found 2026-10-23, `pure-handler-macro-resume`): a
  macro that forces an import while it runs never yields its wait to the
  compile. From the macro's demand session, the pending reply looks owned
  by work in progress, which is the root's own suspended turn, so the run
  waits on it; the isolated search, before it, spun on it instead. A wait
  during a macro's lookup is unaffected. Until this is fixed, keeping a
  macro's run across a compile's replays would not help.
- `control-separation` — explore separating delimited control's
  responsibilities in the standard API: the reset stack's placement in the
  user's state, a thread's checkpoint, reinstating a continuation
  (maintainer, 2026-10-23). Near future;
  [Delimited Control](DelimitedControl_2026-10-23.md).
- `net-builder-handler` — the interaction-net builder runs on the pure
  handler's core in place of its private session (maintainer, 2026-10-23).
  `eval/builtins/net/builder.rs`, about 1,600 lines, keeps its own copies
  of state, cut, fix, reset and shift, and decodes and re-encodes its whole
  continuation on every operation. Its constructors would become private
  operations over an opaque construction state, as the token parser's are.
  Best after `control-separation` settles, since it carries its own reset
  stack.
- `origin-meta-refl` — compilation origins as `meta_refl` metadata, not
  opaque payloads that `glam.origin.inspect` opens (maintainer).
- `api-dict-ops` — a review of the machine's dict operations, for building
  and taking apart dicts swiftly (maintainer).
- Candidate (maintainer): default effects served from the root's outputs,
  as imports are, such as stderr and the fallback diagnostics renderer,
  rather than callbacks.
- `api-fresh-review` — once the API steps land, a fresh holistic review,
  starting with an audit of what the 2026-10-03 pre-performance reviews
  left open. They were thought closed before performance work began, but
  E1 was found open on 2026-10-22 (maintainer, 2026-10-22; see the
  [poll budget plan](PollBudget_2026-10-22.md)).

Two steps from the ownership plan stand beside these, under their names:
- `lifecycle-audits` grows with each step: a source audit or compile-fail
  tests showing that code on workers cannot name the `Runtime` or call
  Rust code, and ordering tests for the L3 and L4 shapes until the steps
  that remove them land.
- `lifecycle-external-owners` checks L11, independently.

`api-runtime-turns` comes first, then the bridge. The data operations
precede the CLI's first slice. Cuts precede ports,
whose buffers request queues extend; the machine tree precedes the loggers' move;
settlement follows the logs it appends to. Import requests follow requests,
whose ports and promises carry them, and precede the compile operations,
which take their requests' keys, then replay, which needs a compile that
evaluation applies, and the runtime-side compile context, which removes
what workers still reach; the hosts retire with the
steps that take their roles, effect providers and reflection machines. Reflection machines follow effect providers,
whose turns run effects as tasks will, and settlement follows reflection
machines, whose waits it classifies. The CLI serves its sources after
settlement, once its evaluations wait in turns. Handle retirement precedes the
retirement protocol, which needs a sole owner.

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
  whose functions each name the module where they are used. Their uses
  in the CLI count what remains to move, and each step's "As built" records
  the count.
- **The CLI's alone:** the library's tests and the public tests never use
  it (`tests-follow-target-architecture`).
- **Retired by plan:** the constructor goes with `api-cli-sources`,
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
- Retired 2026-10-25 under `api-quiescence` (`quiescence-is-analyzed`): the
  CLI's settlement loops and the settled reports it rendered
  (`quiescence-batch`), for a batch that publishes an analysis's issues
  through its logger, and the configured logger's exit vote
  (`exit-votes-retire`).
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
- The environment taken once, outside any transaction, as the shares
  stand over the role's sealed environment, so a change to a share wakes
  nothing that read it: by `Assembler::reflection_environment`, which the
  CLI's parser search reads, and by the CLI's logger host, through
  `glam::transition::reflection_environment`. Reflection tasks look
  theirs up within their transactions (`refl-env-overlays`); the rest is
  the reflection model's implementation.
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
| `lifecycle-host-messages` | `api-effect-providers` and `api-reflection-machines` (from `api-hosts-as-machines`, 2026-10-21) |
| `lifecycle-leases` | `api-effect-providers` |
| `lifecycle-host-calls` | `api-import-requests` and `api-runtime-compile` |
| `lifecycle-audits` | itself, beside the steps |
| `lifecycle-external-owners` | itself, beside the steps |

The review's findings, by the step that resolves each:
- **L1, L5, L6, L10** (owners everywhere, readiness results holding
  owners, sessions outliving retirement): `api-handle-retirement` removes
  the handles, and `api-retirement` retires in order.
- **L2** (the CLI logger on workers): done with `lifecycle-cli-logger`; the
  logger machine (`api-ports`) ends what remains.
- **L3, L9** (workers upgrading `DeferredAssembler` for imports):
  `api-import-requests` and `api-runtime-compile`; served sources now
  compile through the reasoning session, and only the file loaders still
  upgrade `DeferredAssembler`, until `api-cli-sources`.
- **L4** (a subscriber leaking its runtime): the diagnostics port
  (`api-ports`) retires subscribers.
- **L7, L8** (callbacks that capture owners, client code on workers):
  `api-effect-providers` and `api-reflection-machines`, with
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

`api-requests`, first-class volumes in turns (2026-10-19):
- A volume's handle is an opaque glam value naming a runtime's volume
  (`reflection::volume_handle`); it holds the IDs alone, as a promise's
  resolver does. `m.volume_new()` allocates a volume whose root is the top
  value, replacing it with the handle; `m.volume_get(path)` replaces a
  handle with the value at `path`; `m.volume_set(path)` and
  `m.volume_rewrite(path)` pop a value or function, then a handle; and
  `m.heap_volume()` pushes the shared heap's handle. Each evaluates the
  handle, never the value, and fails having done nothing.
- Allocation is a store edit, `Create`, whose ID the transaction reserves
  at once from a counter the store shares with its snapshots, so a
  machine's cut and, later, a reflection task reserve one without the
  store's lock. Validation exempts a volume the journal created from the
  missing-volume check; a commit installs its root; reads of it within the
  transaction depend on nothing outside it. An aborted transaction burns
  the ID.
- Tests in `tests/runtime_volumes.rs`: a volume allocated, written and
  read; its handle passed between machines; a failed cut allocating
  nothing and a committed one keeping its volume; the heap as a volume;
  a value that names no volume as an error that pops nothing.

`api-requests`, requests and promise status in turns (2026-10-19):
- `m.ask(port)` replaces the top value with the promise of its reply and
  writes `{request:value, reply:resolver}` from the output `port`, as
  `write` writes (they share one path, `send`). A server reads the request
  and resolves its `reply`. Where the write fails, `ask` forgets the
  promise it allocated and does nothing. Within a cut that fails, nothing
  is sent, but the promise it allocated stays pending in the runtime's
  record until `api-opaque-finalization`.
- `m.promise_status()` returns a `PromiseStatus`, pending, fulfilled or
  broken, for the top value, which stays: a promised value as it stands,
  read from its cell without forcing it, or a resolver, evaluated, read
  from the runtime's record, which keeps how each promise resolved, a
  marker without its value. Within a cut the read counts as an
  observation, waking on any change, as an evaluation that waited does.
  (Superseded: status reads the resolver alone, a read of the store;
  see "status by resolver" below.)
- Tests in `tests/runtime_requests.rs`: a server answering what a
  requester asks; a requester waiting in a cut for its answer (forced
  through a channel after the first attempt waits); a failed cut sending
  no request and a channel keeping commit order; duplicate servers
  racing, the second seeing the promise fulfilled and its resolution
  failing; a promise's status as it resolves, from the promised value and
  from its resolver; a cut waiting on a status woken by the resolution
  (forced).

`api-requests`, reflection's volumes and promise status (2026-10-19):
- Reflection gains `.vol.new Initial`, `.vol.get Handle Path`,
  `.vol.set Handle Path Value` and `.vol.rewrite Handle Path Updater`,
  `.heap.vol` and `.promise.status Promised`, with the handles turns use
  (`reflection::volume_handle`). A volume effect evaluates its handle,
  never the value, and fails where it names no volume of this runtime;
  `.vol.get`, `.vol.set` and `.vol.rewrite` then act as the protected
  volumes' effects do, observing, staging and committing alike.
  `.vol.new` allocates within a cut's transaction, or alone in a commit
  of its own outside one.
- `.heap.vol` is a nullary request, as `.fail` is, not a value in the
  effect API: a dotted path is sugar for an effect,
  `eff:(\api -> api.heap.vol)`, so the handle comes from running it,
  `.heap.vol >>= (\h -> .vol.get h Path)`.
- `.promise.status` returned `'pending`, `'fulfilled` or `'broken` for a
  promised value as it stands, and a blocked retry restarted once a
  promise it read pending had resolved. (Superseded: status reads the
  resolver; see "status by resolver" below.)
- Effect channels from reflection stay with their consumers, as planned:
  stderr in `api-effect-providers`, imports in `api-hosts-as-machines`.
- Tests in `src/reflection/machine/tests.rs`: a volume allocated, written
  and read through its handle, and within a cut; the heap's handle
  reaching the shared heap; a volume effect without a handle failing; a
  promise's status as it resolves or breaks, and an integer failing; a
  cut that read a pending status retrying once the promise resolves
  (forced: the search blocks, then the promise resolves and the host
  wakes).

`api-requests`, status by resolver and reflection's promise family
(2026-10-20):
- A promise's status is read from its resolver alone (maintainer): a
  promised value stands for whatever it evaluates to, and one fulfilled
  with another promised value would have no clear referent. In a turn,
  `m.promise_status()` evaluates the top value as a resolver, so a
  pending promised value is pushback, as evaluating it is, and a
  fulfilled one evaluates to its value, which is no resolver.
- The record of promises moves from the turn `Runtime` to the store
  (`reflection::promises`), shared by its snapshots as the volume counter
  is, so turns, reflection hosts and the test host reach one record.
- A promise's status is a resource of the store: it has a place in the
  runtime's private volume, apart from users' virtual resources. Within a
  cut a status read observes that place, and a resolution, once it has
  published the promised value, commits a write there. A cut that read a
  promise pending conflicts with a resolution that commits first, and a
  failed one wakes to it, precisely, in turns and reflection alike;
  turns' rerun after any change stays for evaluations that waited. The
  write's commit advances the observation epoch, as
  `RuntimeSharedResources::publish_change` did, which goes.
- Reflection gains `.promise.new`, returning `{promise:P, resolver:R}`,
  `.promise.status R`, `.promise.fulfill R V` and `.promise.break R M`,
  the last two alike, `M` a message or diagnostic. A resolver argument is
  evaluated and identified as a volume handle is (decoding's `Handle`
  state now serves both). Outside a cut a resolution is at once; within
  one it is staged in the transaction and sent once the outermost cut
  commits, as a turn's commit hook would, an asynchronous send. A
  resolution the cut stages for a promise already resolved, or resolved
  in the cut already, is an error; where another resolution comes first
  between the cut's staging and its commit, it stands and the cut's is
  lost, as a duplicate server's would fail. A failed or abandoned
  alternative's resolutions go with it; an isolated search never commits,
  so sends none. (Superseded: `api-transactional-promises` makes the
  resolution part of the commit, and a lost race a conflict.)
- Tests: in `tests/runtime_requests.rs`, a resolver reading its promise's
  status, a pending promised value being pushback and a fulfilled one no
  resolver; a cut that read a promise pending conflicting with its
  resolution and rerunning (forced: the resolution commits between the
  cut's read and its commit). In `src/reflection/machine/tests.rs`, a
  promise allocated, fulfilled and broken through its resolver; a promise
  effect without a resolver failing, and resolving twice failing, within
  a cut too; a cut's resolution sent only once it commits, and a failed
  alternative's not at all; a cut that read a promise pending retrying
  once it resolves (forced).

`api-opaque-finalization`, the design (2026-10-20): the inventory and the
[opaque finalization review](../reviews/OpaqueFinalization_2026-10-20.md),
which compared death notices, weak references, ephemerons, guardians,
active managed finalizers and volume cells owning their values. Settled in
`opaque-resources-reclaimed-when-unreachable`: notices first, then
ephemerons and volumes as cells in the managed heap. It amends
`external-only-opaque-storage` for volume identities.

`api-death-notices` (2026-10-20), the first stage:
- The store owns a channel of death notices, shared by its snapshots.
  Each volume has one payload, made when the volume is allocated
  (`StoreSnapshot::new_volume_handle`), which every copy of its handle
  shares, and the shared heap's handle is one payload the store holds, so
  it never dies. A volume handle's and a resolver's payload send a notice
  from `Drop`; both payload families are now `external`.
- `RuntimeSharedResources::process_death_notices` drains the external-owner
  registry, then breaks each lost resolver's still pending promise with
  "resolver lost" (`Promises::lose`), drops its record either way, writes
  the broken promises' places in the store so their readers wake, and
  reclaims dead volumes (`ReflectionStore::reclaim_volume`). It runs after
  each collection: a driver's, through a new
  `RuntimeCoreState::after_collection`, and an explicit service's.
  Settlement collects first (`collect_for_settlement`), and a notice that
  changes anything fails the settlement's validation, so its caller pumps
  again.
- A transaction keeps no volume alive, unlike the review's proposal (F6):
  reclamation records a change to the volume's root, so a transaction that
  read it conflicts, while one that only wrote it commits without those
  writes, since nothing can read them. The store tells reclaimed volumes
  from revoked ones, whose late writes stay an error. Conflict entries stay
  as they are, since pruning them could hide a conflict from a transaction
  still running; `perf-change-log` (reflection performance plan) prunes
  the log as a whole.
- `drain_retired` used to run only within host calls, and two collection
  tests asserted that; they now assert that the runtime drains once the
  collection is done.
- Tests: in `src/reflection/store/tests.rs`, a reclaimed volume dropping a
  late write and conflicting its reader, a revoked one's late write staying
  an error, and the shared heap never reclaimed; in `src/api/turn/tests.rs`,
  a lost resolver breaking its promise and waking its reader (forced: the
  reader's first attempt waits, then the resolver goes and the runtime
  collects), a resolved promise's record going with its resolver, a volume
  living while any copy of its handle does, and settlement reclaiming a
  volume by itself.

`api-transactional-promises` (2026-10-20), after the maintainer asked for
more than a commit hook (`promise-resolution-commits-with-its-cut`):
- A resolution is a store edit, `StoreEdit::Resolve`, which writes the
  promise's place without changing data and carries its resolution, so a
  journal's branching, merging, replay and conflict analysis serve it as
  they serve any write. `StoreJournal::resolve` stages one, refusing a
  promise already resolved or resolved in the journal already.
  Validation conflicts where the record no longer holds the promise
  pending, and the commit settles the record under the store's lock
  (`Promises::settle`), leaving the promised value for the committer to
  publish once it has released the store (`publish_settled`).
- Turns resolve within cuts, through `transact`, instead of refusing, and
  read their own resolution in `promise_status`. A fork's join reports a
  promise resolved by both the fork and its cut since the fork as an
  error (`Merged::ResolvedTwice`), not a conflict. Reflection stages into
  its transaction's journal, or commits a resolution of its own outside a
  cut, retrying a conflict, whose rerun then finds the promise resolved;
  its post-commit send is gone. Death notices break a lost resolver's
  promise through the same commit.
- Tests: in `tests/runtime_promises.rs`, a cut's resolution committing
  with it, seen within the cut, and undone with a failed cut; a resolution
  beaten to the commit failing when its cut runs again (forced: the
  server's cut stages, then the parent resolves outside a cut, then the
  server commits); forks resolving one promise within one cut failing the
  second join. In `src/reflection/store/tests.rs`, of two journals
  resolving one promise, the second conflicting. The reflection cut test
  now expects a cut to see its own resolution.
- Publication order, after the maintainer's reviews the same day. The
  first build advanced the observation epoch before publishing the
  values, so a turn's cut waiting on a promised value could wake to the
  epoch, find the value missing, and wait again past its publication. A
  second build published under the mutation admission the committer still
  held, before the advance; a third, preferred as cheaper, advanced the
  epoch a second time after fulfillment. The maintainer then asked why a
  cut waiting on a promise waits differently from evaluation: a turn's
  cut whose evaluation waited waited on the epoch alone, which no
  finished evaluation advances, so the same cut waiting on a lazy value
  finished elsewhere, or on a promise publishing after the commit, had no
  wake. Now the commit claims the promises and advances the epoch;
  fulfillment publishes the values, waking evaluation's waiters; and a
  turn's cut subscribes as evaluation does. Its pushback keeps what its
  evaluation waited on (`Error`), the cut collects it, and on retry it
  pumps or waits on that work, a promise's follower
  (`promise_root_wait`) or a lazy route, through the work generation,
  counting itself an observer of the epoch
  (`EvaluationWorkCoordinator::observe_epoch_as_client`), whose advances
  then move the work generation too, so a change in what it read wakes
  it as well. The second epoch and the admitted publication went.
- Forced tests in `src/api/turn/tests.rs`: a cut waiting on a promised
  value, and one waiting on a lazy value that needs it, each read it
  pending, then the resolving commit wakes them before fulfillment
  (a hook holds fulfillment until they have read it pending again), and
  they must wake to the fulfillment alone. Both failed with fulfillment
  advancing nothing, before the subscription, and pass with it.
- A cut that waits on a promise it resolves (maintainer: a wait can know
  its promise). A turn's pushback keeps what its evaluation waited on, in
  `Error`: a promise, or the work producing what it needed. Where its cut
  resolves any promise, `EvaluationWorkCoordinator::awaited_promise`
  follows the blocked producers to the promise at the end, and a promise
  the cut resolves (`StoreJournal::resolves_cell`) fails the cut instead
  of waiting forever. Tests in `tests/runtime_promises.rs`: the value read
  directly, and through a lazy access, which waits on a lazy route the
  coordinator follows. Reflection does not check yet: where its effect
  blocks, `install_blocked` sees no transaction on the active branch, even
  for `.eval` within a cut, so the check needs to find where a cut's
  blocked evaluation is suspended. Until then such a cut waits, and
  settlement reports it as a deadlock.

`api-ephemerons` (2026-10-20), in glam-gc:
- An ephemeron is a managed cell of two words, the key's and value's
  addresses as atomic pointers, which lives as any allocation does, while a
  root or a traced edge reaches it (maintainer, the same day). The first
  build kept a table of counted slots outside the heap, which cost a mutex
  to create one, two atomics to copy a handle, and reuse only after a
  collection, and whose handles no managed value could hold without
  leaking. `MutatorAccess::ephemeron` and `ephemeron_checked` allocate one
  from views or from checked bare pointers; `weak` and `weak_checked`
  allocate a weak reference, a `Weak<T>` cell holding an ephemeron whose
  key is its value. It was first an alias for `Ephemeron<T, T>`; a type of
  its own (maintainer, the same day) polices the API and lets a one-word
  weak cell replace it unseen. A view of one views its key and value,
  `GcRef::entry` and `GcRef::target`, until it is cleared.
- Each `Ephemeron<K, V>` instantiation is a class of its own, with its
  own runs (maintainer, the same day): validation stays exact, where a
  stale `Gc` whose slot another instantiation reused would otherwise pass
  and view its key and value at the wrong types. Shared runs for no-drop
  types of one size are a tentative performance step,
  `perf-shared-no-drop-runs`.
- Marking: a cell's trace reports the cell, not its words, through a
  crate-private visitor channel, and marking records it. Once the worklist
  drains, rounds over the recorded cells mark the value of each whose key
  is marked, draining the worklist after each, which may record more, until
  a round marks nothing new; then each recorded cell whose key stayed
  unmarked is cleared, before the sweep. A cell nothing reaches keeps
  nothing alive and is swept. A cell whose key is already marked when
  marking reaches it keeps its value at once, without a round (maintainer,
  the same day): the visitor sends what a trace reports to a sink, and
  marking's sink tests the key as the cell reports itself, marking the value
  as an edge or recording the cell. `CollectionReport::ephemeron_rounds`
  counts the rounds that marked.
- Clearing is silent (maintainer, the same day). The first build queued a
  notice token per cleared ephemeron that a handle held, until the heap's
  owner took them, which an owner that never took them would grow without
  bound. An owner instead notices a cleared ephemeron as it uses it, or as
  it next traces what holds it, and cleans its container then.
- glam does not use them yet; `api-volume-cells` will, keying each
  volume's committed values on its cell. The crate's `SAFETY.md` records
  the unsafe sites, the cell's `Trace` impl and constructor, marking's
  reads of recorded cells, and view construction, and the invariants they
  rest on; its deferred work keeps user-visible weak keys and ephemerons
  under concurrent marking.
- Tests in `heap::tests`, listed in the crate's `VERIFY.md`, which also
  records the gates: the crate's full gate, and Miri over the ephemeron
  tests.

`api-volume-cells` (2026-10-20):
- A volume is a managed cell holding its `VolumeId`, which moved to
  `core` (still `glam::reflection::VolumeId`), and a handle is the `Volume`
  value kind, one edge to it, in place of an opaque payload
  (`core/managed/volume_cells.rs`). Glam sees a handle as opaque: its kind
  reads `Opaque`, it is no key and no function, ordering refuses it, and
  equality compares the volumes named. `Contents` holds the committed value.
- The store maps each volume to a root on an ephemeron of its cell and a
  fresh `Contents`, per commit. It keeps the cells of its own volumes and of
  hosts' volumes in `kept`, which snapshots share, so a snapshot outliving
  its store still reads them. A journal holds the cell of each volume it
  writes, taken from the snapshot's version at the first write, and a
  creation's edit holds the new cell; a commit keys each written volume's
  new version on that cell, from the journal's views when it began from the
  current revision, else from its edits rebased onto the current versions.
- Journals keep views only of what they wrote; reading another volume roots
  the snapshot version's value afresh, which opens an access region, so a
  machine's poll, which holds one, uses `view_in`, `volume_view_in` and
  `create_volume_in`. Representation identity of a committed value
  survives, since the contents keep the value's own structure.
- A volume's death is silent: its versions clear, it queues no notice, and
  the store records no change, since nothing can name it again. Before
  creating a volume, the store prunes cleared versions once a collection
  has completed since it last pruned, by the heap's collection epoch.
  `DeathNotice::Volume`, the handle payload and `reclaim_volume` went.
- Tests: in `reflection::store::tests`, a volume reaching its own handle,
  two reaching each other's, a writer keeping its volume through a
  collection until it commits, a snapshot keeping its version, the store
  keeping its own and a host's volumes until revoked, pruning only at a
  creation after a collection, and a revoked volume's late writes; in
  `api::turn::tests`, a turn's volume reaching its own handle, which fails
  at `53da9d01`. The managed boundary audit's fixtures gained the variant.
- Cost, by `scripts/profile.sh` against `53da9d01` (user-space
  instructions): `minimal` 45.7 M to 45.9 M, `hello_elf` 648.6 M to
  649.6 M, `chain_refl_800` 534.4 M to 534.7 M, with about 50 more roots
  and allocations a run, from the store's versions and fresh reads. None
  of these workloads writes volumes in quantity; one that does is
  unmeasured.
- Review the next day (maintainer, 2026-10-21), on the silent death of a
  volume. It is serializable: death changes no value and records no change,
  writes before it still conflict, and nothing can name the volume again. A
  replay starts from rooted state, so a volume can die under a transaction
  only where every path to naming it again passes through an upstream
  change, itself a conflict. A suspected gap, a reflection request
  carrying a bare `VolumeId` from decoding to execution while a collection
  ran between polls, does not occur: the branch keeps its effect, which
  holds the handle, until the request completes, to run it again. A test
  with a host collecting at its snapshot callback confirms it.
- Even so, following the review: a request carries a `VolumeTarget`,
  the handle it decoded (already rooted) or a host's protected volume by
  ID, and resolves it under access to a `VolumeCellView`, a view of the
  cell. Reads take that view, check it against the version's key, and
  return the value unrooted, so a read outside a transaction roots only
  its result, once where it rooted twice. A transaction's journal keeps
  each volume it reads, as it keeps those it writes, until it ends
  (`read_volume_in`, `read_volume` for turns). Diagnostics name a handle
  `Volume`; evaluation still sees it as opaque. Tests: the request test
  above, a reader keeping its volume through a collection, which fails
  without the journal's keep, and reads' root counts.
- Then the cell view became the store's whole external API for volumes
  (maintainer, the same day: a verifiable capability in place of a detail
  of the representation, so reasoning stays local). Writes, read
  observations and turns take a `VolumeCellView`, and creation returns
  one; reads return a `VolumeContentsView`, asserting first that a
  version's ephemeron key is the cell. A journal's writes build drafts,
  `Contents` cells, which its reads view and its commit keys the new
  versions on. The ID-based methods are the store's own; only hosts'
  protected volumes, which the store keeps until revoked, go by ID, through
  the `*_kept_volume*` methods.

## Verification

Runtime changes run `scripts/check.sh full` before commit. Concurrency
fixes force the bad interleaving in a test; repeated runs are not proof
(`docs/AgentContext.md`, "Working Rules"). Public behavior is tested through
the public API, in `tests/`.

### Tests to revisit

Behavior built without a test that pins it, each to take up when the
means to test it is ready (recorded 2026-10-26, maintainer's request):

- `interrupt-root-race` — an annotation root's interrupt
  (`EvaluationWorkCoordinator::interrupt_root`) records the root's failure,
  then requests the task's cancellation, under two acquisitions of the
  coordinator lock. It clears the task's `flags_root` with the first, so
  a task whose running quantum ends failed between the two flags nothing
  again, where the coordinator asserts that a root fails once; and a
  second interrupt of the same root answers that nothing was there. Both
  need a forced interleaving: a task held in a running quantum, the
  interrupt run, then the quantum ended failed; and two interrupts
  ordered across the lock.
- `interrupt-mid-quantum` — a run interrupted while its task runs a
  quantum, its reflection machine held by its worker rather than the
  coordinator, flags only its own machine, its calls unflagged, and stops
  at the quantum's end (`MachineHost::run_calls`,
  `EvaluationWorkCoordinator::waiting_calls`). Needs a task held mid-quantum
  within a call while the interrupt commits.
- `rust-run-callees` — a host's interrupt takes only annotation tasks as
  roots to stop (`annotation_tasks`), leaving a Rust-scheduled run to its
  host. No test tells the two apart today, since only the assembler's
  reflection host has machines, so a Rust-scheduled run never calls a
  child; revisit when a Rust host gains machines, as the logger may as a
  turn's fork (`api-hosts-as-machines`).

## Open Questions (2026-10-18)

- A promise resolves only outside a cut, since a resolution cannot be
  undone: a known limitation (maintainer, 2026-10-19). Revisit promises
  whose resolution a failed cut undoes once the heaps are fast enough. A
  cut defers a resolution to its commit: a turn's in a commit hook,
  reflection's by itself, an asynchronous send (maintainer, 2026-10-20).
  Doing better would need evaluation itself under the transaction that
  fulfills the promise. *Settled in part (2026-10-20,
  `api-transactional-promises`):* a resolution commits with its cut, and a
  failed cut resolves nothing. A cut reading the value of a promise it
  resolves fails in turns; in reflection it waits, which settlement reports
  as a deadlock, until reflection checks its waits too. Reading that value
  would need evaluation within the transaction.

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
  (`api-reflection-machines`, from `api-hosts-as-machines`).
