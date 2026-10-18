# Runtime API Review — 2026-10-17

Status: direction agreed with the maintainer on 2026-10-17, in discussion,
and the [runtime API plan](../plans/RuntimeApi_2026-10-17.md) drafted from
it. No code changed. It replaces the owner-and-handles design that
`lifecycle-owner` would have built (the
[runtime ownership plan](../plans/RuntimeOwnership_2026-10-17.md)); that plan's
remaining steps merged into the runtime API plan on 2026-10-18, with
effect providers (`effect-providers-answer-within-turns`). The same day,
transaction steps became cuts, which also bound search
(`turn-cuts-bound-transactions-and-search`). Communication's conventions,
names and the open questions below remain.

Revision reviewed: `fd97593e`.

## Purpose

The maintainer took `lifecycle-owner` as an opportunity to re-envision the
public API (2026-10-17). A public API is built from user stories, and its
users are programmers. A heap of long-lived handles, pointers through which
code asks the runtime to act, is not necessarily the best interface. The
alternative inverts it: the runtime is an environment within which a Rust
thread performs tasks.

## The API Today

A survey at `fd97593e` found about seventy public types, most of them
handles:
- **Owners everywhere.** `Assembler`, `AssemblerBuilder`, the readiness
  results, `EffectRun` and the CLI's logger each hold an `EvaluationRuntime`,
  so any of them keeps the runtime alive, and any thread may drop the last
  (findings L1, L5, L6 and L10 of the
  [lifecycle review](RuntimeLifecycleCapabilities_2026-10-16.md)).
  `Assembler::default()` creates and solely owns a runtime.
- **Retirement does not stop work.** A surviving session keeps the
  coordinator and the value domain alive, and a client may keep pumping.
  Values go inert only when the last `Values` drops.
- **No count of client quanta**, and some client waits have condvars that
  retirement never wakes.

Internally the runtime already works the other way. `Heap::mutate` takes a
closure whose borrows cannot escape, a root marks a real handoff, and
reflection tasks are persistent machines with task-local state and staged
transactions.

## User Stories

1. **Build an artifact.** Open a runtime, compile a file with a
   configuration, demand `asm.result`, take bytes out, close: the CLI, and
   most embedders.
2. **Interactive session.** One runtime on one thread across many edits
   and queries: an IDE, a REPL, the reflection-based `--interactive`.
3. **Exchange data.** Build glam values from Rust data, and read results
   back as Rust data.
4. **Provide effects.** Rust code answers what a program asks of its host:
   files, output, logging.
5. **Overlap work.** Several Rust threads and the runtime's workers make
   progress at once.
6. **Diagnose.** Receive and render diagnostics; inspect a failed or stuck
   task.

## Direction

### The runtime

`Runtime::open(config)` returns a thread-affine runtime: `!Send`, so it
stays on the thread that opened it, where dropping it retires everything;
but `Sync`, so shared borrows may go to scoped threads. A user sees it as
something created for a task and discarded when done. A mutex wrapper for
threads that are not scoped may come later.

### Machines and turns

`rt.run(key, |m| …)` is a *turn* on the machine named by `key`, run by the
calling thread. A Rust thread thereby takes part in the runtime as a peer of
its reflection tasks, rather than holding pointers into it from outside.

- **Identity is a `Key`:** user-chosen and runtime-independent, never a
  runtime-allocated ID. A string serves; an abstract global path gives a
  namespace a Rust owner can brand (see "Tentative Plans" in the plans
  README).
- **A machine has a stack and local state**, as reflection tasks have
  task-local state. It starts empty, and `m.clear()` returns it there. It
  has no lifecycle to remember: an empty machine costs nothing.
- **Within a turn:** typed pushes and pops of Rust data, stack operations,
  application, access, evaluation and compilation, and values scoped to the
  turn (`Val<'m>`). What should continue goes back onto the machine's stack
  or state.
- A turn reaches the shared heap, volumes, and the runtime's other shared
  state.

```rust
let rt = Runtime::open(Config::default())?;
let bytes = rt.run(Key::binary_from_text("cli"), |m| {
    m.push_source(Source::file("hello.g"))?;
    m.compile()?;
    m.access("asm.result")?;
    m.eval()?;
    m.pop_bytes()
})?;
```

### Transactions are steps

A turn is not atomic. A transaction is a step within one,
`m.transact(|t| …)`, and steps nest, giving hierarchical transactions, as
the effect machine's nested transaction frames do. `t.retry()` abandons the
step and waits until something it read changes: STM's `retry`, glam's
`.cut` retry. Rust side effects within a step must tolerate a rerun, or wait
for commit.

### Concurrency

Fork-join comes from Rust's scoped threads (`std::thread::scope`): a
`&Runtime` may go to threads that join before the scope ends, so several
Rust threads run turns at once, and the borrow checker ensures all have
joined before the runtime can retire. The runtime may wrap it as
`rt.scope(|s| …)`, with one turn at a time per machine, and offer
`m.fork(key, …)` within a turn. With several threads, a turn may wait.
Rayon's work-stealing pool suits blocking turns poorly.

### Communication

Tasks never send to machines, nor machines to each other: there is no
point-to-point communication, which would hinder extension. Everything is
modeled in the same heap or volume (maintainer, 2026-10-17):
- **state** at a path, whose current value a late reader sees, so startup
  order does not matter;
- **logs** at a path, appended with `.heap.rewrite` in commit order, each
  reader keeping its own cursor in its own state, so readers are many and
  independent; a reader at the end retries and wakes on the next append.

Volumes are allocated through the reflection API, and abstract global paths
serve as capabilities even within the shared heap (maintainer,
2026-10-17). The diagnostic bus becomes a log, the logger a machine that
reads it, and input and output endpoints paths that Rust machines write and
read.

Open: retention and compaction, backpressure, and discovery, for which
Kafka, DDS and MQTT conventions are worth a review; and what a log costs
as a heap value, which the representation work bears on.

### Authority

Possession is authority: of a machine's key, of a volume's capability, of
a branded abstract path. What the maintainer sketched as
`runtime.do(MyAuth, |caps| …)` is a turn whose key and capabilities decide
what it may touch.

## What Changes

- **Handles go:** `Assembler`, `ModuleBuilder`, readiness snapshots,
  `EffectRun`, endpoints and buses become operations in a turn, or state
  and logs in the heap. `Values` and `Value` become values scoped to turns.
- **Retirement** is dropping the runtime on its own thread, with scoped
  threads joined by construction: the client-quanta count and the
  retirement wakes the ownership plan needed largely disappear.
- **The ownership plan:** `lifecycle-owner`'s owner and handles are
  replaced; its queues give way to heap communication; its hosts become
  machines; its per-call capabilities are `m`. `lifecycle-audits` and
  `lifecycle-external-owners` still apply.
- **Transition:** the internals already work in scopes. The 162 runtime
  constructions are mostly internal unit tests, which may keep internal
  types; the public surface is the CLI, `tests/public_api.rs` and the docs.

## Proposed Stages

Names for a plan that would follow:
1. `api-runtime-turns`: `Runtime`, `run`, and a minimal machine over the
   existing internals, with public tests.
2. `api-cli`: the CLI on it.
3. `api-transaction-steps`: transaction steps and `retry`.
4. `api-scoped-threads`: fork-join and waiting.
5. `api-heap-communication`: state and logs; diagnostics and the logger.
6. `api-hosts-as-machines`: hosts and import loading.
7. `api-handle-retirement`: the handle API goes.

## Open Questions (2026-10-17)

Answered by the maintainer the same day, except the first two, whose
options are discussed as the work reaches them.

1. **Communication conventions:** retention, backpressure, discovery.
   *Open; discuss options as we go.*
2. **Names:** `Runtime`, machine, turn and their operations. *Open;
   discuss options as we go.*
3. **Glam's side:** whether `.heap.*` suffices for logs and cursors, or
   glam code wants helpers. *Start with the basic heap operations;
   specialize or accelerate as profiling shows.*
4. **Per-machine exclusivity:** a second turn on a busy machine waits or
   fails. *Perhaps both; or a turn observes a machine's status by key, and
   so waits transactionally.*
5. **Rust state:** whether a machine holds only glam values, or Rust data
   too. *Glam values only.* Carrying opaque Rust data from turn to turn is
   feasible, but unless it gains something significant, users can work
   around it, with a container in a shared mutex for instance.
6. **Threads that are not scoped:** the mutex wrapper, and when. *Scoped
   threads only, for now;* relax the constraint if a strong use case
   appears.
