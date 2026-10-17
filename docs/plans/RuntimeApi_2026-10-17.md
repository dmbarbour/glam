# Runtime API Plan — 2026-10-17

Status: drafted 2026-10-17 from the
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
   - Tests: the Hello World sample's bytes through a turn; state across
     turns; `clear`; errors on underflow and on a pop of the wrong kind; a
     compile-fail test that `Runtime` is not `Send`.
2. `api-cli` — the CLI's assembly path on `Runtime`: configuration,
   compilation and output. The logger stays on internal types until
   `api-heap-communication`.
3. `api-transaction-steps` — `m.transact(|t| …)`, nesting: shared heap
   reads and writes staged in the runtime transaction, the machine's stack
   and state restored on abort, and `t.retry()` waiting until a read
   changes. Rust side effects belong after commit (an after-commit hook).
4. `api-scoped-threads` — `rt.scope(|s| …)`, fork-join within a turn, and
   waiting. One turn at a time per machine: a second waits or fails, or
   observes the machine's status by key and so waits transactionally
   (maintainer, 2026-10-17: perhaps both). Ordering tests force each
   interleaving.
5. `api-heap-communication` — state and logs in the heap or a volume, with
   readers' cursors in their own state; abstract global paths as
   capabilities; volumes allocated through the reflection API. The
   diagnostic bus becomes a log, and the logger a machine. Retention,
   backpressure and discovery are settled here, after a look at Kafka, DDS
   and MQTT conventions.
6. `api-hosts-as-machines` — the reflection hosts (`AssemblerReflectionHost`,
   `LoggerTaskHost`, `IsolatedTaskHost`) and import loading become machines
   and heap state; `DeferredAssembler` retires.
7. `api-handle-retirement` — the public handle types go: `Assembler`,
   `ModuleBuilder`, readiness results, `EffectRun`, endpoints and buses, and
   `Values` as a separate handle. Standing docs describe the new API.

`api-runtime-turns` comes first and `api-cli` follows it. Transactions
precede heap communication, and scoped threads precede the logger's move.

## Names (to discuss as each step reaches them)

For `api-runtime-turns`, a first proposal:
- **`glam::Runtime`** for the runtime: short, and distinct from today's
  `EvaluationRuntime`, which remains during the transition. Alternatives:
  `GlamRuntime`, or taking over `EvaluationRuntime`'s name.
- **`run`** for a turn (`rt.run(key, |m| …)`). Alternatives: `turn`,
  `with_machine`.
- **`Machine`** for the context of a turn, the `m`. Alternative: `Turn`,
  which names the visit rather than the thing visited.
- **`RuntimeConfig`** for what opening takes: worker threads, the source
  system, the conflict strategy, the reflection environment.

## Verification

Runtime changes run `scripts/check.sh full` before commit. Concurrency
fixes force the bad interleaving in a test; repeated runs are not proof
(`docs/AgentContext.md`, "Working Rules"). Public behavior is tested through
the public API, in `tests/`.

## Open Questions (2026-10-17)

- Names, per step (above).
- Communication's conventions, at `api-heap-communication`.
