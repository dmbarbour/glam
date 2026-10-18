# Runtime API Plan — 2026-10-17

Status: `api-runtime-turns` built 2026-10-17 (see
[As built](#as-built-2026-10-17)). The CLI's transition was re-planned the
same day, after a survey of the CLI (see [CLI Transition](#cli-transition)):
`api-cli` gave way to a bridge and a CLI slice in each later step, moved
piecemeal (maintainer, 2026-10-17). Drafted 2026-10-17 from the
[runtime API review](../reviews/RuntimeApi_2026-10-17.md), whose direction
the maintainer agreed the same day (`runtime-api-turns-on-machines`). Names
and communication's conventions are discussed as each step reaches them.
It replaces the owner-and-handles steps of the
[runtime ownership plan](RuntimeOwnership_2026-10-17.md);
`lifecycle-audits` and `lifecycle-external-owners` stay there.

## Goal

A public API in which a Rust thread acts within the runtime: it opens a
thread-affine runtime and runs turns on machines named by user-chosen keys,
each with a stack and local state of glam values. Transactions are steps
within turns, fork-join comes from scoped threads, and tasks and machines
communicate only through the shared heap or volumes. The handle API goes.

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
     a `Value` pushed onto or popped from a machine.
   - `Runtime::activate_workers(n)`, since the configured CLI learns its
     worker count from the configuration after the runtime opens; and
     `Runtime::profile()` under `glam-prof`.
   - The CLI's main thread opens the runtime and drops it last, and
     `PreparedAssembly` holds it. Behavior does not change.
3. `api-machine-data` — operations that build and read data, and the CLI's
   first slice.
   - Building: `list(n)`, `record(names)`, `anno`, and a push of an
     abstract global path; `compile` from several inputs, over initial
     definitions popped from the stack.
   - Reading, for consumers, which evaluate: a list's items onto the stack,
     a dictionary's entries, a value's kind, an atom's name, and equality
     of representation.
   - CLI slice: loading the configuration, the assembly, selecting
     `conf.log`, and the configured completion script run in turns on a
     `cli` machine, whose state holds the configuration. The CLI's policy
     moves into a glam prelude (proposed; see
     [Policy in glam](#policy-in-glam-proposed)).
4. `api-transaction-steps` — `m.transact(|t| …)`, nesting: shared heap
   reads and writes staged in the runtime transaction, the machine's stack
   and state restored on abort, and `t.retry()` waiting until a read
   changes. Rust side effects belong after commit (an after-commit hook).
   No CLI slice: the CLI's transactional code is the logger's, which moves
   with heap communication.
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
7. `api-settlement` — `rt.settle()` runs until no work and no other turn
   remains, services maintenance, kills a deadlock, and appends the
   runtime's reports (task, delivery and maintenance failures, exit errors,
   killed work) to a log. Turns in progress are known, so settlement
   counts them, which the handle API could not.
   - CLI slice: the CLI's two settlement loops become one, and its exit
     status reads the report log.
8. `api-hosts-as-machines` — the reflection hosts (`AssemblerReflectionHost`,
   `LoggerTaskHost`, `IsolatedTaskHost`) and import loading become machines
   and heap state; `DeferredAssembler` retires.
   - CLI slice: a CLI machine serves the reads of import loading and
     records them, for the manifest and the final check that no file
     changed. The CLI opens its runtime with `Runtime::open`, and the
     bridge's constructor goes.
9. `api-effect-providers` — Rust code that answers effects within a
   transaction or a search, which a machine cannot. The configured CLI's
   argument parser needs it (to discuss; see
   [Effect providers](#effect-providers-to-discuss)).
10. `api-handle-retirement` — the public handle types go: `Assembler`,
    `ModuleBuilder`, readiness results, `EffectRun`, endpoints and buses,
    and `Values` as a separate handle. The bridge goes with them, and the
    public tests move to turns (see
    [Parity](#parity-for-handle-retirement)). Standing docs describe the new
    API.

`api-runtime-turns` comes first, then the bridge. The data operations
precede the CLI's first slice. Transactions precede heap communication,
scoped threads precede the loggers' move, and settlement follows the logs it
appends to. Hosts precede effect providers, which may reuse their shape, and
retirement comes last.

## CLI Transition

A survey at `2955aee8` found that the CLI, about ten thousand lines under
`src/bin/glam`, uses the handle API for five concerns. The bootstrap
commands (help, version, `--parse`, the manifest check, basic completion)
need no runtime, and stay as they are.

| Concern | Today | Needs | Steps |
|---|---|---|---|
| Preparation | `FileSourceSystem`, `EvaluationRuntime`, `DiagnosticBus`, and an `Assembler` whose reflection environment holds two promises | an owner, then the environment and sources in their target form | 2, 6, 8 |
| Configuration and assembly | compiles `configuration` and `assembly` over initial definitions; access, `defined_or`, `context:` annotations, evaluation to bytes | data operations | 3 |
| Logging | `DefaultLogger` renders as a subscriber; a configured logger is an `EffectRun` on its own thread with `MainEffects` (`read_log`, `write_stderr`), supervised, with a fallback endpoint | scoped threads, logs | 5, 6 |
| Settlement | two loops over readiness, maintenance, deadlock kills and quiescence reports | `rt.settle()`, a report log | 7 |
| Configured CLI | `conf.cli` runs as an isolated effect search over argv: `CliEffects`, with nested `TokenEffects` searches, per-branch journals, and a search again for each completion candidate | effect providers | 9 |

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
leave it on the handle API until the last step, with nothing moved
piecemeal.

### Policy in glam (proposed)

The CLI builds records in Rust today: the assembly's initial definitions
and their journal records, `conf.entry` contexts, path-lookup annotations,
terminal viewers. A CLI prelude can hold them instead: a glam module in the
binary, compiled in a turn and kept in the `cli` machine's state.
`anno context:Frame Expr` and `abstract_global_path` are syntax, and the
compiler's `defined_or` helper is a match on `{}`, the undefined value,
which glam code can write. Rust then pushes argv and paths,
applies the prelude's functions, and pops bytes, so the machine's
operations stay general, as any embedder needs them, rather than shaped
by the CLI. The prelude's compile cost per run is measured on `hello_elf`
before it lands.

### Effect providers (to discuss)

The configured CLI parses arguments in an isolated search. Its effects
(`.read.keyword`, `.read.path`, `.read.token`, `.write.file`, `.case`)
return alternatives, edit per-branch journals (a cursor, the plan, the
expectations), and query the file system within branches that may never
commit. A machine answers only committed requests, through the heap, so it
cannot serve them. Options:
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

A is recommended: the parser's file-system queries within hypothetical
branches need Rust that answers without committing. *Low confidence* on
where the boundary falls for requests that are queries, such as `.env`,
which a provider and a machine's published state could both serve.

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
  at paths and volumes allocated through the reflection API (6, 8);
- the diagnostic formatter and diagnostic updates, by the logger machine
  (6);
- reasoning reports, retryable blocked errors and draining, by settlement
  (7);
- sources, imports, invocations and provenance, by hosts as machines (8);
- effect specialization, by effect providers (9);
- conflict analysis and workers, by `RuntimeConfig` and `activate_workers`
  (1, 2).

The checked net builder is open: it may stay a builder whose net a machine
pushes.

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
they leave on the stack (`unpack_list`, `unpack_dict`).

## As built (2026-10-17)

`api-runtime-turns`, in `src/api/turn.rs`:
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

## Verification

Runtime changes run `scripts/check.sh full` before commit. Concurrency
fixes force the bad interleaving in a test; repeated runs are not proof
(`docs/AgentContext.md`, "Working Rules"). Public behavior is tested through
the public API, in `tests/`.

## Open Questions (2026-10-17)

- Names, per step (above).
- Communication's conventions, at `api-heap-communication`.
- The bridge, the CLI prelude, and effect providers (option A), proposed
  for the maintainer's agreement.
