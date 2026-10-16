# Runtime Lifecycle and Capabilities Review — 2026-10-16

Status: review complete. No code changed. This records findings and a
recommended remediation; it does not authorize a plan.

Revision reviewed: `776bec6d` (clean `main`).

## Purpose and Scope

A flaky collection test led to runtime retirement joining its workers
(`runtime-retirement-joins-workers`). Tracing what still reached the
workers then found code they run holding runtime owners, which we removed
one leak at a time. The maintainer asked instead how to model ownership
structurally, with these principles (2026-10-16):

- The client is the runtime's only owner; workers are not owners equal to
  it.
- Retirement waits for the workers' quanta in flight, which are bounded. A
  quantum that never ends is a bug.
- Code the runtime calls borrows the capabilities it needs as a lease for
  the call, instead of holding owners.
- The top-level client owner may become affine.

Coverage: the public API (`src/api`, the crate's re-exports), the runtime's
internals (session, coordinator, executor, value domain, reflection store
and events), every surface where the runtime stores code and calls it later,
and the CLI and tests. Four read-only surveys gathered the facts, and the
claims the findings rest on were checked against the code. Line references
are to `776bec6d`.

## Current Model

### Owners

Only `EvaluationRuntime` holds `Arc<RuntimeState>` (`api/runtime.rs:90`),
and only `RuntimeState` holds the executor (`:116`). So an *owner*, which
keeps the executor and its workers alive, is anything that holds an
`EvaluationRuntime`:

- `EvaluationRuntime`, `Assembler` (through `Arc<ReasoningSession>`,
  `api/assembly.rs:373,621`) and `AssemblerBuilder`;
- the readiness results: `RuntimeReadiness`, `RuntimeMaintenanceSnapshot`,
  `QuiescenceSnapshot`, `DeadlockSnapshot` and `QuiescenceReport`
  (`api/runtime/readiness.rs:105,364,439,670`);
- `EffectRun` until it runs (`reflection/lifecycle.rs:330`);
- transiently, the `Assembler` a module or binary loader upgrades
  `DeferredAssembler` to (`api/assembly.rs:637,1386`) and the one the compile
  diagnostic emitter clones (`:1615`).

Narrower holders keep the value domain alive but not `RuntimeState`:
`Values`, `RuntimeTaskCapability`, `ReasoningVolume`, `DiagnosticIngress`,
`StoreSnapshot` and what contains it. Sessions (`EvaluationSession`,
`ScheduledEffectRun`, `IsolatedEffectSearch`, `CompilationExecution`) also
keep the coordinator alive. `Value`, `EvaluatedValue`, the endpoint handles
and `DiagnosticSubscription` own nothing; `DiagnosticBus` owns only its
subscribers.

### The internal graph is acyclic by design

No purely internal strong path leads from the coordinator or the shared
resources back to `RuntimeState`. The weak links are deliberate and
documented: shared resources, the executor, demand state and the value
domain reach the coordinator weakly (`api/runtime.rs:171-176`,
`evaluation/executor.rs:15`, `evaluation/session.rs:258-262`,
`core.rs:312-318`); values and roots do not retain the heap; endpoint
handles and ingresses keep weak links to the runtime; and `DeferredAssembler`
keeps module loaders from closing a domain cycle (`api/assembly.rs:625-634`).

Every cycle enters through code or objects the client supplies, which
runtime-owned state holds strongly:

| Entry | Held by | Called on |
| --- | --- | --- |
| task specialization and host | task machines, launchers and profiles (`reflection/machine.rs:110-111`, `lifecycle.rs:670`) | the polling thread, a worker or the client, mutator suspended |
| diagnostic subscribers | `DiagnosticBus`, reached through hosts (`api/assembly.rs:41-46`) | the publishing thread: client calls, request polls, host commits, compile emitters, output adapters |
| output decoders and adapters | the client's `RuntimeOutputDelivery`, unless placed in a host | whoever calls `deliver_next`, possibly a worker in a host commit |
| status-publisher and terminal closures | work records (`evaluation/coordinator/task.rs:278-291`) | the settling thread |
| conflict analysis strategy | `ReflectionStore` (`reflection/store.rs:504`) | under the transaction lock |
| host-call owners (module and binary loaders) | the value domain's external-owner registry (`core.rs:909-915`) | the thread that forces the import, possibly a worker |

### Leases that already exist

Heap access is a higher-ranked lease (`with_runtime_value_mutator`,
`RuntimeValueAccess<'scope>`). Request polls receive `RequestContext<'_, S>`,
which offers `values()`, `eval_context()`, `host()`, the transaction and
`isolated_search`, and evaluates by returning `Demand` and resuming.
`ReflectionEnvironmentBuilder<'_>`, `NetBuilder<'net>`, `ValueEvaluator<'a>`
and `ReflectionInspector<'a>` are borrowed for their call. The gap is
everything stored and called later.

## Findings

Severity: **high** can fail or leak in ordinary use; **medium** is a latent
race or a missing guarantee; **low** is cleanup or information.

**L1 (high, design).** Retirement follows the last `Arc<RuntimeState>`, so
whoever drops the last owner decides where and when the runtime retires: a
worker, a subscriber's thread, a terminal wake. Each later finding is an
instance. Weak routes prevent cycles but not transient ownership: a weak
handle upgraded on a worker is an owner until it drops.

**L2 (high, CLI).** The CLI's `DefaultLogger` holds an `Assembler` and
evaluates with it (`bin/glam/rendering.rs:8-63`), and code the workers run
reaches it two ways:
- it subscribes to the logger bus that `LoggerTaskHost` holds
  (`bin/glam/configuration/logger/mod.rs:24-36`), and the host publishes
  diagnostics during task commits and writes, on the polling thread;
- the fallback output adapter captures it, and the logger terminal's
  after-hook, held by a work record, delivers through that adapter
  (`bin/glam/configuration/logger/supervisor.rs:100-107`).

With workers active (`batch.rs:129`), the logger renders on workers, and a
worker can drop the last owner, which fails the retirement assertion inside
the bus's `catch_unwind` around subscribers, so the panic is swallowed and
retirement is left half done. It also cycles through the work record until
the logger task settles.

**L3 (medium, latent race).** Workers get transient owners in three
places: module and binary loaders upgrade `DeferredAssembler` to an
`Assembler` on the thread that forces an import (`api/assembly.rs:1386`); the
compile emitter clones that `Assembler` (`:1615`); and publication clones the
subscribers' `Arc`s before calling them (`api/diagnostics.rs:733-738`). If
the client drops its last owner meanwhile, retirement lands on the worker.
No test forces it.

**L4 (medium, leak).** A subscriber attached through
`AssemblerBuilder::diagnostic_subscriber` that holds an `Assembler` clone
leaks the runtime: its subscription lives in every clone's attachment list
(`api/assembly.rs:820-837`), so the clone keeps its own subscriber
subscribed, and the bus is reached from the coordinator's background profile
(`api/runtime.rs:695-701`, `api/assembly.rs:888-903`). `RuntimeState::drop`
never runs. The test subscriber `DiagnosticMessages`
(`g_syntax/macro_expansion/tests.rs:20`) has this shape.

**L5 (medium, API).** Readiness results hold owners: every
`RuntimeReadiness` variant but `Busy` and `Poisoned`, the maintenance,
quiescence and deadlock snapshots, and `QuiescenceReport`, which uses its
runtime only for `runtime_id()` (`api/runtime/readiness.rs:473`). A result
stored by a callback closes a cycle, and each result hands out an owner.

**L6 (medium, API).** Owners are handed out freely: `Clone` on
`EvaluationRuntime` and `Assembler`, `Assembler::evaluation_runtime()`,
`readiness()`, `drain_reasoning()`, `settle()`, `kill()` and
`EffectRun::new`. The CLI's `PreparedAssembly` holds three owners of one
runtime (`bin/glam/configuration/mod.rs:17-19,66-73`), and `Arc<LogHost>`
shares one with the logger thread.

**L7 (medium, guarantee).** Nothing structural stops captures. Every
callback trait already requires `Send + Sync` (`TaskSpecialization`,
`TaskHost`, `DiagnosticSubscriber`, the endpoint closures,
`ConflictAnalysisStrategy`, `SourceSystem`, `ReflectionServices`), but
owners are `Send + Sync` too. `SpecializationRequestWork` and the internal
`EvaluationTaskMachine` require only `Send`. The managed-graph audit already
bans `EvaluationRuntime` from managed declarations
(`core/managed/managed_boundary_audit.rs:401`); nothing similar covers
callbacks.

**L8 (medium, design).** Client code runs on workers. Subscribers and
output adapters run on whatever thread publishes or delivers, including a
worker inside a request poll or a host commit, and rendering evaluates
there. Evaluating inside a quantum this way is re-entrant and blocks the
worker on demand. Publication is data; rendering need not happen where it
is published.

**L9 (low, inherent).** Module loaders compile and run macro searches
synchronously inside a host-call quantum (`g_syntax/macro_expansion/runner.rs:107-160`),
reviving a dropped `CompilationExecution` and draining it. That re-entrant
evaluation is inherent to lazy imports; a lease can remove the owner but not
the nesting.

**L10 (medium, teardown).** Sessions and work can outlive retirement, and
close on workers. A lifecycle publisher holds its session through a cell
(`reflection/lifecycle.rs:164-208`), released only by `ScheduledEffectRun`'s
drop or the terminal wake, so a leaked `ScheduledEffectRun` keeps the
coordinator and its machines alive after the runtime retires. The terminal
wake drops the session on the settling thread, which can be a worker, and
closing a session drops its machines there.

**L11 (low, to check).** Retired entries of the external-owner registry
are drained only when a host call runs (`core.rs:1741`); otherwise they wait
for the domain to drop.

**L12 (low, information).** Mixing runtimes is checked at run time: about
27 `require_runtime` calls, 18 explicit errors and 85 assertions comparing
runtime IDs. A type-level brand could make these compile errors, at the cost
of a lifetime on `Value` and everything near it.

## Options

**O1. Leases per call.** What the runtime calls receives the capabilities it
needs for that call. Hosts receive the runtime's parts (transaction
snapshot and commit, environment, query writer) through `RequestContext`;
specializations use `RequestContext::values()`; decoders, converters and
adapters receive `&Values` or a scoped access; subscribers receive their
event and, if they render, a lease. This removes the reasons to hold owners,
but alone guarantees nothing: a captured owner still compiles (L7). An
evaluation lease on a worker is the hard part (L8).

**O2. Owners `Send` but not `Sync`.** The compiler would reject capturing an
owner in any `Sync` callback. Gaps: `Mutex<Owner>` is `Sync`; `Send`-only
work types still admit owners; and `Assembler`, which holds its owner inside
an `Arc`, would stop being `Send` until restructured.

**O3. An affine owner that retires explicitly.** One non-`Clone` owner,
whose `Drop` retires the runtime: it stops and joins the workers, closes
sessions and work records (dropping their hosts, specializations and
closures), releases the background demand, retires the endpoints and the
value domain. Everything else holds a cloneable handle that can reach the
runtime while it is live and fails afterwards. Retirement then happens at a
known point, on the owner's thread, however many references remain, and
memory is reclaimed when the last reference goes, wherever that is: an
`Arc` dropped on a worker frees inert state and retires nothing. This
resolves L1, L3, L5 and L6, and makes L4 and L10 teardown deterministic. It
is the largest change: about 20 files and 200 edits, mostly mechanical, plus
the handle API. It changes one semantic rule: the value domain retires when
its owner drops, not when its last holder does.

A thread-affine owner (`!Send`) would go further: it could not be moved into
any callback, all of which are `Send`, and the runtime would always retire on
the thread that created it. Clients that move the owner between threads
would hold handles instead.

**O4. A scoped runtime** (`Runtime::scope(|rt| …)`, like
`std::thread::scope`). The strongest guarantee, but lifetimes spread through
the public API and long-lived embedders need an awkward outer scope. Not
recommended now.

**O5. Runtime brands in the types** (GhostCell-style). Turns L12's checks
into compile errors, at a cost across the whole value API. Not recommended.

## Recommendation

O3 with O1. O3 makes retirement structural: the owner's drop retires, and
nothing else can, so stray references, upgrades and captures stop
mattering for lifecycle and matter only for memory. O1 then removes the
reasons callbacks hold anything, and O3's handles give the rest a
non-owning route. O2 becomes optional, since a captured handle is harmless;
a thread-affine owner is the stronger and simpler form of it.

Suggested steps, each a plan step once a plan exists:
1. `lifecycle-cli-logger`: stop running the CLI's rendering on workers
   (L2, L8). Tasks publish data; the client delivers and renders on its own
   threads. Small, and independent of the rest.
2. `lifecycle-owner`: the affine owner and its handles; explicit retirement
   that closes sessions and records (L1, L5, L6, L10); `Assembler`, the
   readiness results and `EffectRun` hold or borrow handles; the CLI keeps
   one owner in `main`.
3. `lifecycle-leases`: capabilities per call for hosts, specializations,
   decoders, converters, adapters and subscribers (L7, L8), retiring stored
   owners such as `LoggerTaskHost`'s `RuntimeTaskCapability` where the
   context can supply them.
4. `lifecycle-host-calls`: module and binary loaders receive a non-owning
   session factory and a diagnostic sink with the host call, instead of
   upgrading `DeferredAssembler` (L3, L9).
5. `lifecycle-audits`: a source audit like the managed-boundary audit, or
   compile-fail tests, showing callbacks cannot name or capture an owner;
   ordering tests for retirement during publication and import (L3, L4).
6. Check L11.

## Maintainer Decisions Needed

1. **The owner (O3):** affine with explicit retirement? Movable (`Send`) or
   thread-affine (`!Send`)?
2. **Value-domain retirement:** at the owner's drop, making values inert
   then, rather than at the last holder's drop.
3. **Where client callbacks run:** subscribers and adapters only on client
   threads, with the client pumping delivery, or anywhere, with leases.
4. **Evaluation in callbacks:** if subscribers may render on workers, what
   an evaluation lease means inside a quantum; or forbid it and render on
   the client.
5. **Readiness results:** borrow the owner (`RuntimeReadiness<'rt>`) or carry
   handles.
6. **Timing:** step 1 now, and the rest before or after the grammar review
   resumes.
