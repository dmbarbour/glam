# Explicit Heap Holds — 2026-10-09

Status: planned, not started (maintainer, 2026-10-09: a widespread task, to
follow lower-hanging work). A design review on 2026-10-10 proposes revising
the design below; nothing from it is decided yet (see "Design review"). Steps, in order: `gc-hold-api`,
`gc-hold-boundaries`, `gc-hold-tests`, `gc-hold-class-cache`,
`gc-hold-transient-roots`, then `gc-hold-thread-context`.

## Purpose

Separate holding a heap from accessing it, so that admission, the one step
that can block on a running collection, happens only at explicit,
enumerable boundaries. Today any `with_mutator` deep in a call stack may
become an outer entry, which is what made the lock-order audit for
`gc-bounded-collection-wait` hard. This plan replaces
`gc-two-level-mutator-access` and `glam-gc`'s part of
`gc-thread-local-heap-context` from the
[structural overheads plan](StructuralOverheads_2026-10-08.md).

## Design

Agreed with the maintainer on 2026-10-09:
- **A hold is the only admission.** It is explicit and taken at
  boundaries, and a thread holds at most one; a second hold panics.
- **Access requires a hold**, and panics without one in every build. The
  access token, the phantom-scoped mutator, derives from the thread-local
  hold state. A nested access returns another scope proof; accesses are
  counted.
- **`Hold::suspend` is a scoped GC safepoint.** It is valid only with no
  access open; it releases admission and re-acquires it after. It belongs
  where the thread holds no Glam lock, so above the wait functions rather
  than inside `CountedCondvar`, which instead asserts that no hold is
  active while it waits.
- **Holds cache allocation classes.** Class discovery on each access is
  about 4% of `countdown_800` (metadata lookup, `RunGeometry::derive`,
  `discover_class_with`).

Held regions (`perf-quantum-region`) are the current approximation: an
evaluation quantum holds the heap without a token, accesses inside it are
recursive entries, and release points end the hold without re-acquiring it.

## Design review (2026-10-10)

Proposed, not decided. It came from discussing how to stop paying for
transient roots (`perf-transient-root-avoidance`), checked by three
reviews: soundness against `glam-gc`, feasibility in the evaluator, and
consistency with existing plans and decisions.

### Vocabulary

- **Mutator**: the new owned token for a thread's heap admission,
  replacing `Hold`. During migration, today's closure token needs a
  temporary name (such as `MutatorAccess`).
- **Heap admission**, not bare "admission", which also names runtime,
  spark, route and family admission.
- **Suspend point**: a scoped safepoint that re-admits after. Today's
  **release point** ends a held region and never re-admits.
- **Held region** retires once boundaries convert. Step IDs keep
  `gc-hold-*`.

### Four kinds of reference

| Kind | Valid | Kept alive by |
| --- | --- | --- |
| `Gc<T>`, stored edge | inside a heap object | tracing of its container |
| `GcRef<'m, T>`, scoped view (the scoped-pointer plan's `ScopedGc`) | while `&'m Mutator` is borrowed | no mark or sweep while the mutator is held |
| `LocalRoot<'mu, T>` | until the mutator ends, across suspends | the mutator's table, traced while suspended |
| `Root<T>` | anywhere, any thread | the root registry |

- A stored edge cannot carry a mutator lifetime: heap types are
  `'static`. A view comes from reading a field through a parent's view,
  from a root, or from allocation.
- Viewing a bare `Gc` stays `unsafe`. Safe APIs already produce detached
  `Gc` values (`Root::as_gc`, `duplicate_in`; `Gc` is `Send`). So a safe
  view of one could dangle: copy it out, drop the root, suspend and
  collect, then view it. `Root::as_gc` should return a `GcRef`. This
  meets the scoped-pointer plan's open fresh-edge question.

### Mutator semantics

- **One per thread**, owned and `!Send`, taken at boundaries through a
  closure that gives it an invariant, generative brand `'mu`. Today's
  `Mutator<'heap>` is covariant, so the brand needs its own marker.
  The thread's heap slot (`CURRENT_HEAP`) enforces one per thread, which
  soundness needs: heap admission and the thread's cache record are per
  thread.
- **`&Mutator`** grants access. **`&mut Mutator`** grants
  `suspend(&mut self, f)`: release heap admission, run `f`, re-admit.
- **A `GcRef` cannot be live across a suspend**: it borrows `&Mutator`,
  and `suspend` needs `&mut`. This is a borrow error, with no counting.
  The maintainer suggested also keeping a suspend generation in `GcRef`
  in some modes, checked on use. That would catch what the borrow cannot:
  views made through `unsafe`, and legacy release points during
  migration.
- **Code with fixed signatures** (`Drop`, `Display`/`Debug`,
  `PartialEq`/`Hash`) is already being deprecated for Glam types, and
  heaps were never to be observed without a mutator (maintainer). So
  nothing needs to derive access from a thread-local, except the
  migration shim.
- **Long-lived contexts cannot store `&Mutator`** if they span a
  suspend; they pass it alongside. `EvalContext` is durable and `Send`;
  `EvaluationPollContext` spans the host call, launcher and contention
  wait.

### The invariant, corrected

No mark or sweep while any thread holds its mutator unsuspended.
Finalization touches only objects already dead at mark. It does run
destructors and return memory to allocators while other mutators hold
heap admission, which is sound because the dead set is fixed at mark.
The allocation path never reclaims.

Concurrent marking stays feasible. Under it, threads acknowledge
handshakes only at suspend or mutator end. Local roots are marked before
a suspend resumes, as the concurrent plan does for `RootFrame`s
(maintainer). Insertions rely on the SATB barrier at the mutation
gateways, and new allocations are marked live for the cycle.

### `LocalRoot` mechanics

- **Creation is `carry(&self, GcRef)`** over a `Cell`-backed table. It is
  sound because `suspend(&mut self)` excludes every `&self`. A
  `&mut self` form and a closure form both fail to compile (E0502;
  checked with rustc), so the capability sits on suspend, not on carrying.
- **`suspend` publishes the table before releasing heap admission, and
  withdraws it after re-admission.** It uses a per-heap registry under
  the data mutex, as roots do, so a nested `mutate`/`suspend` inside `f`
  on the same thread works. Cost scales with suspends, not values.
- **A `LocalRoot` dropped during a suspend** (moved into `f`) defers its
  release to a separate, thread-private list. The owner applies it after
  resuming, and the collector may keep the object one collection longer
  (maintainer). That needs no atomics, provided published slots are never
  written while suspended. The drop tests its own mutator's suspended
  flag, not the thread's slot.
- **The table lives in its own allocation**, reached by raw pointer. The
  collector's reads must not overlap memory under a live `&mut` on the
  owning thread, such as `suspend`'s `&mut self` or a thread-state
  `RefCell` borrow. That would be undefined behaviour under the aliasing
  models even without writes. Test under Miri.
- **Deferred until measured.** Today's values that cross release points
  use `Root`. A `LocalRoot` must also avoid what sank
  `perf-poll-root-frames`: per-read cost, and catching durable roots.

### What `suspend` must guarantee

- **No Glam lock held.** Re-admitting while holding a Glam mutex that a
  finalizer needs can deadlock. That is why held regions never re-acquire
  (`quantum-held-region`). Suspend points sit above the wait functions,
  and `CountedCondvar` asserts no mutator is held.
- **Allocation cursors.** A collection during the suspend advances the
  lease epoch and re-leases memory. On resume, the thread's record must
  pass the epoch check. While suspended, the record counts as occupied;
  otherwise a `collect_full` inside `f` (`remove_inactive_thread_cache`,
  `release_current_thread_caches`) orphans it. Missing this corrupts
  memory.
- **Failure to re-admit.** Re-admission panics on a poisoned heap.
  Unwinding must not leave a usable mutator without heap admission, nor
  let the mutator's exit release one it does not hold.
- **Collection on resume.** Under the crate's default `Automatic`
  policy, re-admission may itself collect, so withdrawal follows
  re-admission. Runtime heaps are `NoAuto`.
- **Finalizers.** Destructors run under the collector thread's finalizer
  mutator. With one mutator per thread, a destructor's heap entry must be
  a non-panicking `try_`, as `trace.rs` already requires.
- **Migration.** Views do not raise the entry depth, so a legacy
  `release_held_region` reached while views are live would end heap
  admission beneath them. While a new mutator is live, legacy release
  must panic or do nothing. In effect, `gc-hold-boundaries` comes first
  for paths reachable from lazy routes.

### Release points in held evaluator polls

| Release point | Reached from | Can it move to the driver? |
| --- | --- | --- |
| Host call (`eval/value.rs`) | the inline stack's base only (`forceable_inline` excludes host-call sources) | easily: return an invoke request |
| Launcher (`session.rs`) | the base only (reflection-task sources) | moderate: return the reservations |
| Net contention wait (`CountedCondvar`) | inline children with a net checkpoint; the only condvar wait inside held polls | easily: already followed by `return Yielded` |
| Nested `drive_client_demand` | not reachable from held polls | already outside; assert no mutator |
| Pressure servicing | quantum boundaries | already outside |

So evaluator polls may need no suspend point at all. Machines would still
take `&Mutator`, to carry views between accesses.

### Migration size (estimates)

- **First slice:** the WHNF request and `follow_forwards` into the inline
  stack, 44% of transient roots. About 600–1,000 lines in about 8–10
  files in Glam, plus 500–1,000 unsafe-sensitive lines in `glam-gc` (the
  owned mutator, re-admitting suspend, `GcRef`).
- **Types gaining `'m`:**
  - `LazyTaskMachine`, split so the base keeps a `Root` and children
    hold views;
  - `InlineClaims`, `ForwardEnd`, `WhnfDeferredRequest`, `WhnfPoll`,
    `LazyCheckpointPoll`;
  - `NetSemanticAction`, `NetDriverOutcome`, `NetWhnfAccessPoll`.
- **Keep `'m` out of the shared `RegionalWhnf*` types**, or it spreads to
  20+ files. The reducer emits a view for use within one access, converted
  at the closure exit.
- **Access along the poll path ties to a named `'m`**, instead of
  `for<'scope>` closures.
- **Most code already takes access as a parameter:** about 690
  parameters (`&EvaluationValueAccess`, `&RuntimeValueAccess`), against
  about 450 calls that open access themselves (100 `with_value_access`,
  344 `with_runtime_value_access`).

### Ordering with other plans

- The scoped-pointer and value representation plans say not to migrate
  the compatibility `Value` twice.
- A narrow slice limited to the single-edge lazy and net carriers avoids
  that. They cover about 70% of the transient-root cost. Sites that
  carry `Value`s stay rooted.
- Poll results stay durable at the poll boundary.

### Decisions and documents this would revise

| Decision or document | Change |
| --- | --- |
| `scoped-gc-deferred` ("only after reproducing a concrete defect"); scoped-pointer plan SP0 | supersede on performance grounds |
| `explicit-heap-holds` | access is a borrow, not counted |
| `quantum-held-region` (never re-acquire) | suspend re-acquires, with the guarantees above |
| `publish-before-access-ends` | publish "before the mutator suspends or ends"; `Value` stays unbranded |
| `noauto-runtime-collection-policy` | a nested driver skips collection only while inside an access region; under suspend it may collect |
| concurrent-GC plan | stale lines on several heaps per thread; this is a CG0 candidate for transient roots |
| `docs/architecture/evaluation.md` | "the poll carrier exposes neither that route nor a mutator to the machine" reverses; the WHNF rule that poll results cross as `RuntimeValueRoot` stays |
| `docs/architecture/values.md` glossary | "regional" becomes "valid until suspend" |

### Alternative

A counted stopgap on today's held regions. Carried edges count
themselves on a thread-local counter, and a release point checks the
count is zero, always on, with creation sites tracked in debug builds.
About a third of the churn, checked at runtime instead of by the borrow
checker, and removed when `GcRef` lands.

### Open questions

- Adopt this revision, the counted stopgap, or neither for now.
- Whether `Root::as_gc` and the detached-`Gc` sources change with this
  plan or with the scoped-pointer plan.
- Where the generation check runs ("some modes").

## Measurements

Call sites that entered the heap with no hold, from the library tests at
`4072eb92` (the integration tests did not finish within the run's time
limit, so the counts are a floor):

| Kind | Sites | Natural hold point |
| --- | ---: | --- |
| Public host API: `Values` (about 32 methods), `Assembler`, evaluator, diagnostics | about 50 | each API method |
| Compile and setup: module lowering, source parsing, compiler values, macro runner | about 8 | the compile entry |
| Reflection polls: machine, protocol, requests, lifecycle | about 9 | the poll, suspended around client callbacks |
| Polls driven directly by a caller (`whnf`, session, list and access machines) | about 6 | their drivers |
| Tests, mostly through `eval/test_support`, lazy-checkpoint and `whnf` fixtures | about 290 | the test, through the helpers |

Nesting today: production nests an access inside an access only through
the public value API (`Values::list` and `record` run the caller's
iterator inside their own access; diagnostic emission) and cycle poisoning
(`pump.rs`, rooting a failure through a second access). Forbidding nesting
outright broke 186 tests.

## Steps

### Hold and access API (`gc-hold-api`)

`glam-gc` gains `Heap::hold`, where a nested hold panics, `Hold::suspend`,
and access that requires a hold, with nested accesses as plain scope
proofs. A temporary shim keeps today's automatic entry for unconverted
callers.

### Boundaries (`gc-hold-boundaries`)

Convert Glam's boundaries: the public API, the compile entry, reflection
polls (hold their pure steps, such as local state, and suspend around
client callbacks), and directly driven polls. Turn release points into
suspend scopes above the locks: the coordinator's change wait, the client
demand result wait, the runtime activity wait, the net disturbance wait,
the reflection lifecycle waits, host calls, launchers, nested drivers and
pressure servicing. Fix the production nesting sites: collect a caller's
iterator before taking access, and reuse an open access in diagnostics
and cycle poisoning.

### Tests and enforcement (`gc-hold-tests`)

Convert the test helpers, then remove the shim, so unheld access panics in
every build.

### Class cache (`gc-hold-class-cache`)

Cache allocation classes in the hold, so an access finds its class without
the registry, the geometry derivation or the heap's data mutex. A smaller
per-thread cache may land earlier under `perf-allocation-path`; this step
moves it into the hold.

### Transient roots (`gc-hold-transient-roots`)

`countdown_800` registers more roots than it allocates objects (211,594
against 182,557), about 9% of its samples with collection's root scan;
most root a value only to carry it between two accesses of one poll. A
hold proves that no safepoint lies between those accesses unless one is
suspended, so such values could stay unrooted edges, rooted only across a
`Hold::suspend`. Rooting them in a frame per poll instead saved nothing
(`perf-poll-root-frames`); avoiding the root saves about 1,400
instructions each.

### Heap context in Glam (`gc-hold-thread-context`)

With one heap per thread, Glam's call chains could find their runtime from
the hold instead of passing value factories down (maintainer,
2026-10-09). Investigate the trade: a thread-local lookup against passed
references, and which same-runtime validations it makes redundant.
