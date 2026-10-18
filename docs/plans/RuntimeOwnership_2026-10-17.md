# Runtime Ownership and Host Messaging Plan — 2026-10-17

Status: merged 2026-10-18 into the
[runtime API plan](RuntimeApi_2026-10-17.md), which maps each remaining
step to its own ("From the Ownership Plan"); `lifecycle-audits` and
`lifecycle-external-owners` continue there under their names. The
[runtime API review](../reviews/RuntimeApi_2026-10-17.md) (2026-10-17)
replaced the owner-and-handles design below with a thread-affine runtime in
which Rust threads run turns on keyed machines. The queues, host messages
and retirement protocol below hold, adapted to turns
(`effect-providers-answer-within-turns`). A survey for `lifecycle-owner` at
`fd97593e` found, against the design below: assemblers create their own
runtimes, the drop order releases the background demand before joining the
workers, and the logger thread still holds owners. The design was agreed
with the maintainer on 2026-10-16 in
the [lifecycle review](../reviews/RuntimeLifecycleCapabilities_2026-10-16.md)
("Target Model" and "Maintainer Decisions"); this plan orders the work.
`lifecycle-cli-logger` is done (2026-10-16, `988e887d`). The rest waits for
the grammar work the [grammar review](GrammarReview_2026-10-16.md)
sequences, through the tag prefix syntax plan; each step's details are
settled when it starts.

## Goal

Make the runtime's ownership structural. One thread-affine owner retires the
runtime and its value domain; workers run only runtime code; effects cross
to client threads as messages; code the runtime calls borrows capabilities
for the call instead of holding owners. The review's findings L1–L12 are the
evidence; this plan cites them by number.

## Pause State (2026-10-17)

What holds now:
- retirement joins the workers and asserts it is not running on one
  (`runtime-retirement-joins-workers`);
- the CLI's effect specialization holds `Values`, its stderr decoder does not
  evaluate, and its diagnostics render only on client threads.

Known hazards while paused, none expected in ordinary runs:
- **L3:** a worker forcing a lazy import upgrades `DeferredAssembler` to an
  owner, and the compile emitter clones it. If the client dropped its last
  owner meanwhile, retirement would land on the worker and fail the
  assertion. The CLI keeps its owner until settlement, after which no import
  runs.
- **The library's assembler reflection host** publishes journal diagnostics
  to its bus during commit, possibly on a worker, so a subscriber attached
  through `AssemblerBuilder` runs there. The CLI attaches none.
- **L4:** a builder subscriber that holds an `Assembler` leaks the runtime.
  Only a test subscriber has this shape.

## Design Summary

- **The owner** is affine and thread-affine (`!Send`, `!Sync`). Its `Drop`
  retires the runtime:
  1. mark the runtime retired, so no new work is admitted;
  2. stop and wake the workers, and join them;
  3. wait for client threads' quanta in flight;
  4. fail pending client operations as retired;
  5. close sessions and work records, dropping stored hosts,
     specializations and closures;
  6. release the background demand, and retire endpoints and queues;
  7. retire the value domain, so values become inert.
- **Handles** are `Send + Sync + Clone`. They do everything but retire:
  evaluate, pump, deliver, answer requests, feed inputs, read readiness. They
  fail once the runtime retires. `Assembler`, `EffectRun` and readiness
  results hold handles; readiness results carry data and a handle.
- **Queues** are capabilities stored apart from the user heap. Each element
  pairs a value with an optional promise handle, so writes and queries mix.
  A queue's order follows its transactions' commit order; queues promise no
  order among themselves, and resolvers need not resolve in the order read.
  A pair becomes visible on commit, so an aborted transaction sends nothing.
- **A request's status** (uncommitted, sent, broken, fulfilled) is a
  capability on shared state apart from the main heap, as spawned tasks'
  status is. A task awaits a change with the ordinary `.cut` retry, or takes
  the promised result at once.
- **Hosts are endpoints:** queues, inputs and state in the reflection store.
  `TaskHost`'s `snapshot`, `commit` and `validate` hooks retire. The store's
  interface stays open to a transactional database behind it.

## Steps

1. `lifecycle-cli-logger` — *done 2026-10-16.* The logger task's host only
   commits; the logger thread delivers between pumps and after the task
   ends (L2, L8).
2. `lifecycle-owner` — the owner and its handles, and the retirement protocol
   above (L1, L5, L6, L10).
   - `Assembler`, readiness results and `EffectRun` move to handles; the CLI
     keeps its one owner in `main`, and the logger thread holds a handle.
   - Tests force each ordering: retirement during a client thread's pump, a
     pending client operation failing as retired, values inert after
     retirement, and a compile-fail test that the owner is not `Send`.
3. `lifecycle-queues` — the request queue and status capabilities.
   - Transactional publication, a client-side receive-and-resolve API, and
     the status states with `.cut` retry observing them.
   - Tests: an aborted transaction sends nothing; a queue's order matches
     commit order; dropping a resolver breaks the request; a status change
     wakes a waiting retry.
4. `lifecycle-host-messages` — hosts reduced to endpoints over queues and
   store state; the `TaskHost` hooks retire (L7, L8).
   - Migrates `AssemblerReflectionHost`, so diagnostics become queue writes
     that a client delivers, ending its publication on workers.
   - Also migrates `LoggerTaskHost`, `IsolatedTaskHost` and
     `ReflectionServices::emit_diagnostic`.
5. `lifecycle-leases` — the remaining per-call capabilities: decoders and
   converters receive `&Values`, specializations use
   `RequestContext::values()`, and hosts drop stored capabilities the
   context supplies. Runs alongside step 4.
6. `lifecycle-host-calls` — a forced import's file load becomes a request
   whose promised value is the import (L3, L9). Module loaders take a
   non-owning session factory, and `DeferredAssembler` retires. Needs step 3.
7. `lifecycle-audits` — a source audit, like the managed-boundary audit, or
   compile-fail tests showing that code workers run cannot name an owner or
   call client code; ordering tests for the L3 and L4 shapes. Grows with
   each step.
8. `lifecycle-external-owners` — check L11: retired external-owner entries
   wait for a host call or the domain's drop. Independent.

Step 2 changes every client API, so it comes first after the pause. Step 3
precedes steps 4 and 6.

## Open Details

Each is settled when its step starts:
- **Names:** for the owner and handle types, and whether
  `EvaluationRuntime` becomes the handle.
- **Client pumping:** which client operations run quanta, and how retirement
  waits for them.
- **Retired operations:** the error a pending operation returns.
- **The queue capability's representation:** typed halves like today's
  endpoints, or one capability; how an element's promise handle relates to
  `PromiseResolver` and promised values.
- **Status:** its states, possibly including received, and how `.cut`
  observes it, as store state with a generation.
- **Host state:** which of today's host state moves into the store.

## Verification

Concurrent changes force every disputed ordering (`docs/AgentContext.md`,
"Working Rules"); repeated runs are not proof. Runtime and collector changes
run `scripts/check.sh full` before commit.
