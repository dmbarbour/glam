# Explicit Heap Holds — 2026-10-09

Status: in progress; `gc-hold-api` landed on 2026-10-13. The maintainer
adopted the 2026-10-10
design review on 2026-10-12 (see "Decisions, 2026-10-12") and suggested
pulling this plan forward, ahead of the leftovers of
`perf-reflection-step-cost`. Since the plan was written, a per-thread
allocation-class cache landed (`perf-allocation-path`) and transient-root
avoidance landed (`perf-transient-root-avoidance`, `2484ad2a`). Steps, in
order: `gc-hold-api`, `gc-hold-boundaries` through its third slice,
`gc-hold-transient-roots`, the rest of `gc-hold-boundaries`,
`gc-hold-tests`, `gc-hold-entry-cost`, `gc-hold-suspension-review`,
`gc-hold-thread-context`, then `gc-hold-values-api`; `gc-hold-class-cache`
is narrowed to a measurement. The maintainer moved transient roots ahead of
the remaining enforcement on 2026-10-13, once every poll lent its mutator,
and on 2026-10-14 chose that drivers hold the mutator across quanta
(`driver-mutator`) and added the entry-cost and `Values` steps. The Steps use the
review's vocabulary: an owned `Mutator`, not a `Hold`.

## Decisions, 2026-10-12

- **Adopt the design review** (maintainer), not the counted stopgap: an
  owned per-thread `Mutator`, `GcRef` views borrowed from it, and a
  re-admitting `suspend`; `LocalRoot` stays deferred until measured.
- **`Root::as_gc` changes in this plan** (maintainer): it returns a
  `GcRef`, as do the other detached-`Gc` sources the plan reaches.
- **Transition aids are welcome** (maintainer): hints, shims and latches
  as incremental steps need them, and suspend-generation metadata in
  `GcRef`. The check runs in debug builds and tests (agent proposal):
  safe code cannot hold a view across a suspend, since `suspend` takes
  `&mut Mutator` and a view borrows `&Mutator`; the check catches views
  made through `unsafe`, and legacy release points that end admission
  without that borrow, until those points retire.
- **Lifetimes.** `'mu` is a mutator's invariant, generative brand, which
  ties views to one mutator; `'m` is the borrow of `&'m Mutator<'mu>` a
  view lives within, which ends at a suspend.

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

Agreed with the maintainer on 2026-10-09. The design review below,
adopted on 2026-10-12, supersedes it where they differ: an owned
`Mutator` instead of a `Hold`, and access as a borrow rather than
counted.
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
- **Holds cache allocation classes.** Class discovery on each access was
  about 4% of `countdown_800` (metadata lookup, `RunGeometry::derive`,
  `discover_class_with`). Since landed per thread instead
  (`perf-allocation-path`): `Mutator::allocator` checks the thread's
  cached class first (`mutator.rs`, `thread_cache.rs`), which recovered
  the 4%.

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
  mutator. With one mutator per thread, a destructor must not require a
  mutator, as `trace.rs` already requires; glam-gc has no `try_` entry
  today.
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
| Net contention wait (`CountedCondvar`) | any machine on the inline stack with a net checkpoint (`LazyTaskMachine::poll_net_whnf_checkpoint`); the only condvar wait inside held polls | easily: already followed by `return Yielded` |
| Nested `drive_client_demand` | not reachable from held polls | already outside; assert no mutator |
| Pressure servicing | quantum boundaries | already outside |

So evaluator polls may need no suspend point at all. Machines would still
take `&Mutator`, to carry views between accesses.

Reflection polls hold no region, but pump the lazy routes they wait on
within the poll (`EffectTask::poll_pumping`, every blocked step since
`b6222f5f`; `poll_blocked` before), each pumped claim taking its own
held region. If a reflection poll's pure steps take a mutator, that
pumping must sit outside it or inside a suspend, since a second mutator
on the thread panics (audit, 2026-10-12).

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
  about 440 calls that open access themselves in non-test code (91
  `with_value_access`, 348 `with_runtime_value_access`; recounted
  2026-10-12, 100 and 344 at the review).

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
| `crates/glam-gc/SAFETY.md` "Regional Mutator Admission Invariants" | admission belongs to the owned mutator; recursive entry becomes a borrow |
| `docs/agent_context/evaluation.md` (heap access and held regions) | the same change, for agents |

### Alternative

A counted stopgap on today's held regions. Carried edges count
themselves on a thread-local counter, and a release point checks the
count is zero, always on, with creation sites tracked in debug builds.
About a third of the churn, checked at runtime instead of by the borrow
checker, and removed when `GcRef` lands.

### Open questions (resolved 2026-10-12)

- Adopt this revision, the counted stopgap, or neither for now: the
  revision.
- Whether `Root::as_gc` and the detached-`Gc` sources change with this
  plan or with the scoped-pointer plan: this plan.
- Where the generation check runs ("some modes"): debug builds and tests,
  during migration and for `unsafe` views.

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
the public value API (`Values::list`, `record` and `dictionary` run the
caller's iterator inside their own access; diagnostic emission, as
`Diagnostic::with_source_location` reopens access) and cycle poisoning
(`pump.rs`, rooting a failure through a second access). Forbidding nesting
outright broke 186 tests (at `4072eb92`).

Reflection steps (2026-10-12, `perf-reflection-step-cost`): a `.r ()`
step in a `refl.*` chain costs 518 K instructions, with 64 root
registrations, about 35 outer and 118 nested heap regions, and 80
allocated slots. Root registration and validation are 11% of it by self
cost (about 1,400 instructions per transient root with its allocation and
release), heap region entry and the thread cache another 11%; most of the
roots carry values between the accesses of lazy routes forced within one
poll.

Heap entries: each mutator entry costs about 180 instructions, 11% of a
lazy-forcing iteration of `heavy` (`perf-lazy-force`, 2026-10-11) and
about 58 K of a reflection step (35 outer and 118 nested entries). An
owned mutator replaces most of them with borrows, which is a motivation
of its own beside the roots.

## Steps

### Mutator and access API (`gc-hold-api`)

`glam-gc` gains the owned mutator: one per thread, `!Send`, taken at a
boundary through a closure that brands it with an invariant `'mu`;
access borrows it (`&Mutator`), and `suspend(&mut self, f)` releases heap
admission, runs `f` and re-admits, with the guarantees above.
`GcRef<'m, T>` views borrow it; `Root::as_gc` and the other
detached-`Gc` sources the slice reaches return views. Transition aids:
today's closure token keeps working under a temporary name
(`MutatorAccess`) for unconverted callers, a legacy release point reached
while an owned mutator is live panics or does nothing, and views carry a
suspend generation checked on use in debug builds and tests.

Landed 2026-10-13. `Heap::mutate(|mutator: &mut Mutator<'mu>| ...)` takes
the owned mutator, which dereferences to `MutatorAccess` (today's token,
renamed), so every existing operation accepts it. `Heap::with_mutator`
keeps its name and lends a `MutatorAccess`; inside a mutator it is a
recursive entry. `GcRef<'m, T>` is one pointer, `Copy`, `!Send`, with
`get`, `Deref`, `edge` (detach to `Gc`) and `same_allocation`;
`Root::as_gc` returns one, `Root::get` reads through one, and
`Gc::view_unchecked` is the `unsafe` view of a bare `Gc`. Glam's one
`as_gc` caller (`project_root`) detaches with `edge`. Tests in
`crates/glam-gc/src/heap/owned_mutator_tests.rs`; compile-fail examples
latch the suspend borrow (E0502), escape, and `!Send` for mutators and
views.

Agent decisions in this step, accepted by the maintainer on 2026-10-13:

- **`mutate` must be the thread's outer entry.** It panics inside any
  active entry, legacy ones included; inside a suspension another mutator
  may be taken. Converting boundaries outside-in therefore cannot nest a
  converted boundary inside an unconverted access (see the open question
  under `gc-hold-boundaries`).
- **A legacy release point panics under an owned mutator**, rather than
  doing nothing: doing nothing could leave a wait blocking a collection.
- **A suspended record stays registered.** The collector skips removing
  it, `Heap::release_current_thread_caches` panics, and other heaps may be
  entered inside the suspension; resumption asserts it found the same
  record.
- **Failure leaves the mutator without access.** If the suspended closure
  or re-admission unwinds, the suspension is withdrawn and the mutator
  keeps no entry; dereferencing it panics in every build (one branch per
  dereference), so catching the unwind cannot reach the heap without
  admission. Re-admission is not attempted while unwinding.
- **The generation is per thread, not per mutator**: every end of a heap
  admission on the thread advances it (debug builds only), so the view
  check also covers legacy entries.
- `Mutator::suspend` refuses while a legacy access is open inside the
  mutator, before taking anything, so the refusal keeps the mutator usable.

### Boundaries (`gc-hold-boundaries`)

Convert Glam's boundaries to take the mutator: the public API, the
compile entry, reflection polls, and directly driven polls. Reflection
polls take it for their pure steps, such as local state, and release it
or suspend around client callbacks and around the dependency pumping in
`poll_pumping` and `poll_blocked`, since each pumped claim takes its own
region. The release points inside held evaluator polls move to their
drivers instead (host calls return an invoke request, launchers return
their reservations, the net contention wait already returns `Yielded`),
so evaluator polls need no suspend. The remaining waits become suspend
scopes above the locks: the coordinator's change wait, the client demand
result wait, the runtime activity wait, the reflection lifecycle waits,
nested drivers and pressure servicing. Fix the production nesting sites:
collect a caller's iterator before taking access, and reuse an open
access in diagnostics and cycle poisoning.

Nesting during migration (maintainer, 2026-10-13): since `mutate` must be
outermost, a converted boundary reached inside an unconverted access, as
public-API boundaries are from tests, would panic. A separately named
aid, `Heap::mutate_recursive`, is `mutate` outside every entry and a
recursive mutator that cannot suspend inside one. Distinct names show
where a feature's transition has stabilized: a boundary moves from
`mutate_recursive` to `mutate` once every caller takes its own mutator,
and `gc-hold-tests` removes the aid with the legacy token.

Slices, in order (2026-10-13):

1. `gc-hold-defer-release-points` (landed): the release points inside held
   evaluator polls move out of the poll. `EvaluationPollContext` gains an
   after-region queue: work deferred with `after_region` runs once the
   poll's region ends, before the driver releases its claim, and at once
   outside a held region. Host calls (`invoke_host_call`), reflection
   activations from `EvaluatorStepContext::finish`, and the net contention
   wait defer through it, and the inline loop ends its quantum when work
   is deferred, spilling an inline lazy as for any suspension. Drivers
   catch panics outside the region, so deferred work keeps its panic
   boundary. Audit: the contention wait in `LazyTaskMachine` is the only
   production disturbance wait in held polls; `NetWhnfMachine::poll`'s is
   reached only from tests.
2. `gc-hold-quantum-mutator` (landed): quanta hold a mutator instead of a
   held region (`CoreValueFactory::hold_recursive_runtime_value_mutator`,
   not yet lent to machines), so a release point inside one panics. Only
   test fixtures reached one: test-only semantic sources that blocked on a
   condvar or nested a synchronous driver inside their poll. Blocking ones
   became host calls; those reading an input use
   `EvaluatorStepContext::promise_or_block_for_test`, keeping their
   assertions; the nested-pumping test now nests inside a host call, the
   production site for it. Production quanta reached none. Decision:
   `quantum-mutator`.
3. `gc-hold-reflection-polls`: reflection polls take the mutator for their
   pure steps and suspend around pumping and client callbacks.
   Measured 2026-10-13 at `c864db35` (`.r ()` chain of 200 against its
   compile-only baseline): 513 K instructions per step, 34.6 outer and
   117.6 recursive heap entries. By call site, the reflection poll's outer
   entries are single-access polls of its sub-machines: WHNF computations
   (`whnf::poll_computation`) 15.3 per step, `EvaluatorStepContext::
   project_root` 6.1, list fronts 5.1, application setup 3.1; pumped quanta
   are the other 5. Holding a mutator per `EvaluationPollContext::evaluate`
   phase, which is callback-free by contract, made a step 2% dearer
   (524 K): outer entries stayed at 34.6 and recursive ones rose by 14,
   since a phase averages one access. A mutator must span the step loop to
   merge them, and the loop calls client code (`TaskHost` snapshot,
   validate and commit, 18 sites) and pumps dependencies, so those sites
   must suspend it from deep inside the machine.

   Decided (maintainer, 2026-10-13): coarse-grained evaluation. The
   evaluation budget alone bounds how long a mutator is held, and several
   operations share one suspension where that costs little. Local
   reasoning about safety and refactoring, and performance, matter more
   than the Rust expression; threading the mutator through the poll
   context packages it as a parameter. Static borrows, not `RefCell`.

   `gc-hold-lend-mutator`, first part (landed): a quantum's
   `EvaluationPollContext` carries the mutator as a `RuntimeValueMutator`
   (`EvaluationPollContext::quantum`), and value access through the
   context or an evaluator step borrows it instead of entering the heap.
   Evaluate phases carry the borrowed `RuntimeValueAccess`. Against
   `373725e6`: countdown and sum -13%, append_walk and list_sum -12%,
   chain and `hello_elf` -6.5 to -7%, `.r ()` steps -5.4%, `minimal`
   -2.8%; `parse_ifs_16` +0.6%. Accesses that open their own region
   through `CoreValueFactory::with_runtime_value_access` stay recursive
   entries until they take access from the context.

   Second part (landed): the machine poll takes the context mutably, and
   every production poll holds a mutator, reflection polls and direct
   effect runs included (`quantum_for_context`); a mutator-free context
   remains only for test fixtures. Suspension sites: `TaskHost` snapshot,
   validate and commit (a snapshot, edit and commit share one), a
   specialization request (started and polled under one), host calls
   (whose result the same quantum consumes), and releasing pumped work.
   Pumped work shares the pumping quantum's mutator through
   `EvaluationPollContext::nested_quantum`, and suspends it only to run
   deferred work. A first version suspended around each pump: it livelocked
   when one pump was missed, and cost a suspension per pump; the shared
   mutator avoids both. Releasing pumped work stays suspended, since
   settling a terminal task can evaluate a diagnostic through a nested
   driver. Pump entry points that may wait are release points. Per `.r ()`
   step: 487 K to 480 K instructions, 15.4 outer and 3.0 recursive heap
   entries. Elsewhere within -0.5% to +0.4% of the first part (`chain`
   +0.4%, `hello_elf` +0.3%), after `#[inline]` on the access wrappers
   recovered a codegen regression of up to 1.9% from the `&mut` context.
   64 roots per step remain, about 18% of a step: the next target.
4. The public API, the compile entry, the remaining waits and the
   production nesting sites.

   Inventory (2026-10-14, at `18fd1696`): the public-API integration tests,
   probed for heap entries outside any mutator, reached 89 sites. The
   public API about 56 (`Values` about 40 in `api/value.rs`, `Assembler`
   7, the evaluator 5, diagnostics 2, and their helpers in `core.rs` and
   `diagnostic.rs`); the compile entry about 17 (module lowering 7, source
   parsing 5, the macro runner and effects 4, the compiler 1); and 14 in
   work that runs while a quantum is suspended (specialization requests 10,
   host calls 2, terminal publication 1, one reflection-machine helper),
   which must take a mutator of their own once access requires one. Glam
   no longer opens held regions, so `release_held_region` in Glam only
   panics under an owned mutator: an assertion in all but name.

   Measured the same day with a probe, reverted, across the profiling
   workloads: single-threaded runs never block on a condvar and collect at
   most twice; plain evaluation (`countdown`, `sum`, lists, literal loops)
   suspends nowhere, so all of a run's 193 to 196 suspensions are the
   compile pipeline's (`TaskHost` validation in macro expansion 136 to
   167, releasing pumped work 47 to 77). A `.r ()` step polls 5.1 quanta
   and suspends 5.1 times, every one releasing pumped work. With four
   workers (`chain_w4_800`), threads waited 5,109 times against 1,408
   quanta, idle each time. An outer entry costs about 476 instructions, a
   suspension and resumption 579, a legacy recursive entry 255; with
   quanta of about 232 K instructions (`countdown_800`), entries and
   suspensions are 0.1 to 0.5% of plain workloads, 1.3% of `minimal` and of
   a `.r ()` step.

   Decided (maintainer, 2026-10-14): drivers hold the mutator across quanta
   (`driver-mutator`). A driver takes the thread's mutator at its outermost
   boundary, the quanta it runs borrow it as pumped work already does
   (`EvaluationPollContext::nested_quantum`), waits suspend it before taking
   their locks, and a quantum boundary suspends it only to join a requested
   collection, so a collection still waits for at most one quantum. The
   entry cost is the lesser gain: threading the mutator lets views rather
   than roots carry values between steps, and `&` against `&mut` marks
   which code may suspend, a local capability for reasoning about
   suspension. Quanta may grow: 64 steps is fine-grained, small only while
   steps cost so many instructions. Rejected: driving without a mutator,
   with access phases taking short ones.

   First, a fix the driver analysis found (`status-diagnostic-before-gate`,
   2026-10-14): publishing a failed reflection task's `.task.status` built
   its diagnostic under the runtime mutation gate through a nested driver,
   which took the gate again and deadlocked behind a queued settlement. A
   test forcing that order hung; the diagnostic is now built before the
   gate, and debug builds panic when a thread takes the gate twice. The
   same analysis found that release between quanta runs client callbacks
   and, for failed statuses, nested drivers, so for now each quantum
   boundary is one suspension covering the quantum's deferred work, its
   release and collection pressure, as pumps already do; shrinking it
   belongs to `gc-hold-suspension-review`.

   Order: drivers (the worker loop, the client demand driver, the pump
   entry points and the coordinator's claimed polls); the compile entry,
   suspending around macro runs; the public API, one mutator per method
   (`mutate_recursive` while tests nest); the remaining waits as suspends
   above their locks, with `release_held_region` an assertion; the nesting
   sites; then the work that runs suspended.

   Drivers (`gc-hold-drivers`, 2026-10-14): the worker loop holds a mutator
   across a busy run and ends it to wait; `drive_client_demand`, session
   drains, `pump_wait_on_route`, the runtime's `pump_background` and direct
   effect runs (`EffectTask::run`) hold one across their loops. Claimed task,
   client-demand and spark polls borrow it
   (`EvaluationPollContext::quantum_under`), and the pump takes it as a
   parameter, which retired `nested_quantum`. Each quantum boundary is one
   suspension covering the deferred work, the release and the disposal;
   waits suspend before taking their locks. `glam-gc` gained
   `Mutator::readmit`: a host call panicking inside a quantum's suspension
   left the driver's mutator without admission, which the quantum's own
   mutator had hidden. A domain-lifetime test caught an idle worker keeping
   its value domain alive across its wait. Heap entries are unchanged in
   number, a resumption replacing each quantum's entry. Instructions in the
   default release build (16 codegen units): -0.7% to +1.9% across the
   profiling workloads, median +0.4%; the increases are inlining shifts
   from codegen-unit partitioning (`drive_net_batch` and the operator-call
   helpers no longer inlined into `with_net_whnf_transition_in`), plus about
   0.5% on `chain_w4`, whose idle client thread now suspends around about
   5,000 waits. `#[inline]` on `lazy_root` and `poll_lazy_checkpoint`
   recovered `countdown` and `sum`. With one codegen unit on both sides:
   -0.3% to -1.8% (`.r ()` step 390.1 K to 389.1 K). One codegen unit alone
   makes a step about 5% cheaper (410 K to 390 K); the maintainer adopted
   it on 2026-10-14 (`release-one-codegen-unit`): -2.3% instructions across
   the profiling workloads, with `chain_where` and `parse_ifs_16` the
   exceptions. The macro runner's isolated search (`IsolatedEffectSearch::poll`,
   public) still takes a mutator per poll; it moves with the compile entry.
   The `full` gate's aggressive-GC stage then failed two session drain
   tests: their probes, acting as another worker, released work under the
   drain's mutator, and releasing services collection pressure, which that
   mode always requests. The probes now run suspended (`1cacdd99`).

   Compile entry (`gc-hold-compile-entry`, 2026-10-14): each source lowers
   under one mutator (`lower_source`); parsing and lowering enter the heap
   under it. A declaration that invokes macros expands under one suspension
   of it, since expansion forces prior definitions, runs macro effects and
   delivers diagnostics to subscribers; inside, the macro machinery still
   takes mutators of its own (lookups, the isolated search, its pumps). The
   lowerer is built first, since building the compiler-values cache
   evaluates, and the compile's diagnostics reach subscribers after the
   mutator ends. A nested compile, from an import's host call, runs in that
   call's suspension and takes its own. Instructions -0.2% to 0.0% on the
   compile-heavy workloads in either build. Lending the compile's mutator to
   the macro machinery, instead of suspending around it, is left for later.

   Public API (`gc-hold-public-api`, 2026-10-14): `Values::with_access`, the
   wrapper behind most `Values` methods, and the promise resolver's
   completions and `seal_module` take access from a mutator of their own
   (`CoreValueFactory::with_boundary_access`), nesting inside another entry.
   The methods that took a caller's iterator (`list`, `record`,
   `dictionary`, `apply`, `access_path`, `access_names`,
   `abstract_global_path`) collect it first, so no client code runs under
   the mutator. Probed again, the public-API integration tests reach 25
   sites outside any mutator, down from 89: core construction helpers, the
   diagnostic helpers that evaluate (`failure_diagnostic_root_with` and its
   kin), and work that runs inside a suspension (macro expansion, host
   calls, terminal publication, a reflection-machine helper), which needs a
   mutator of its own once access requires one.

   Waits and nesting (2026-10-14): the release point is now an assertion,
   `assert_outside_mutator`, in `CountedCondvar` waits, launchers and
   pressure servicing; Glam opens no held regions, which a debug assertion
   checks. `Diagnostic::with_source_location` builds its diagnostic in the
   access it holds (`Diagnostic::from_parts_in`), and cycle poisoning roots
   each member's failure through its own access, which is now a boundary of
   its own since it runs at a suspended quantum boundary.

   Remaining boundaries (2026-10-14): the 19 other sites the public-API
   integration tests reached outside any mutator became boundaries
   (`with_boundary_access`): the core construction helpers
   (`construct_runtime_value_root`, `RuntimeFailureRoot::new`,
   `from_producer`), the diagnostic helpers that evaluate, host calls,
   terminal promise publication, the compiler's import failures, macro
   expansion's accesses and the lowerer's construction. Probed again, those
   tests open no heap entry outside a mutator; unit tests still do, through
   production functions they call directly, which `gc-hold-tests` gives
   boundaries. Instructions level (`.r ()` step 413.0 K).

### Tests and enforcement (`gc-hold-tests`)

Convert the test helpers, then remove the shim, so access without a
mutator panics in every build. Retire held regions with it: Glam opens none
since `gc-hold-quantum-mutator`, so `Heap::with_held_region`,
`Heap::release_held_region` and their thread-cache support go.

### Mutator entry cost (`gc-hold-entry-cost`)

Added by the maintainer on 2026-10-14: make taking, ending, suspending and
resuming a mutator cheaper. Measured that day in a loop benchmark: an outer
`Heap::mutate` costs about 476 instructions, a suspension and resumption
579, a legacy recursive entry 255. The outer entry's cost is spread out:
ending the thread entry 90, activating it 64, finding and preparing the
thread record 78, three layers of admission 107, merging region metrics 23,
the admission gate's exit 21. After `gc-hold-tests`, which removes the
legacy entry paths this one shares code with.

### Class cache (`gc-hold-class-cache`), narrowed

The per-thread class cache landed under `perf-allocation-path`; it lives
in the thread's cache record, which outlives a mutator. What remains per
allocator acquisition is a `RefCell` borrow, a `TypeId` lookup and an
`Arc` clone. Measure that once mutators exist; move the cache only if it
shows.

### Transient roots (`gc-hold-transient-roots`)

`countdown_800` registered more roots than it allocated objects (211,594
against 182,557), about 9% of its samples with collection's root scan;
most root a value only to carry it between two accesses of one poll.
`perf-transient-root-avoidance` (2026-10-10) removed the transient roots
that never left one access or were never read (`countdown_800`: 100,385
roots against 134,699 allocations, 8.6% fewer instructions); the
2026-10-11 snapshot has `countdown_400` at 27,930 roots against 50,592
allocations, before `perf-lazy-force` took 12.1% more off `countdown_800`.
These shares are historical; measure again before the step. Rooting
values in a frame per poll saved nothing (`perf-poll-root-frames`);
avoiding a root saves about 1,400 instructions.

What remains carries values between accesses: the inline stack's lazies,
the net semantic action, results leaving the poll, and in reflection
steps 64 roots each. A mutator proves that no safepoint lies between
those accesses unless it suspends, so such values become views, rooted
only across a suspend. First slice: the WHNF request and
`follow_forwards` into the inline stack, then the reflection machine's
demand and decode carriers.

Measured 2026-10-13 at `35ebeeb9`, with a probe build recording each root's
call site and whether it was dropped within the heap admission that made
it:

- A `.r ()` step registers 64.1 roots; 44.7 die within their admission.
  By site (made, same admission): the WHNF lazy request in
  `reduce_semantic_shell` 16.3 (11.2); reflection decode and list-front
  carriers 11.2 (8.1); WHNF results rooted by `regional_status_poll` 10.2
  (9.2); seed promotion 7.1 (5.1); net-WHNF checkpoint transitions 7.1
  (7.1); step-boundary results (`EvaluatorStepContext::root_value`) 5.1
  (0); reflection application arguments 3.1 (0); the effect's `eff` and
  the request payload selection 2.0 each (2.0).
- `countdown_800` registers 27,254 roots; 25,879 (95%) die within their
  admission, and 24,595 are WHNF lazy requests.
- A registration costs about 1,130 instructions, of which validation
  (`validate_rootable_in_state`: the pending-finalization check and slot
  topology, each resolving the slot's owner through the arena) is about
  790. Validation guards against rooting a dead `Gc`; Glam's root sites
  root edges they have just dereferenced unchecked under the same access,
  so there it proves nothing new.
- Experiment: release builds without root validation (debug builds kept
  it). Per `.r ()` step 480.5 K to 438.4 K instructions (-8.8%);
  `countdown_800` -6.8%, `sum_800` -8.8%, `list_sum_400` -6.3%,
  `chain_800` -3.7%.

Decided (maintainer, 2026-10-13): skip validation that proves nothing,
and name each root's evidence at construction (`root-evidence`). Evidence
by kind, per `.r ()` step: fresh allocations 40.8 (value nodes 25.5, WHNF
checkpoints 10.2, list fronts 5.1), proven by construction; edges held in
values 23.3 (lazies 16.3, nets 7.1), proven by provenance, a claim that
will include the tag check once `Value` holds immediates; views reached
from roots, none yet; unproven pointers, tests only.

Slices, in order:

1. `gc-hold-root-evidence` (landed): `glam-gc` roots views without
   validation in release builds (`MutatorAccess::root`), allocation returns
   views (`Allocator::alloc_view`), and the validated path for a bare `Gc`
   is renamed `root_checked`. Glam roots fresh allocations through views and
   held edges through `RuntimeValueAccess::view_edge`, whose
   `EdgeLiveness` argument names the claim (`HeldValue` is the only one so
   far); `root_checked` remains only in tests. Per `.r ()` step 480.5 K to
   434.1 K instructions (-9.7%); across the profiling workloads -8.9% to
   +0.1% (`sum` -8 to -9%, `countdown` and `append_walk` about -7%, `chain`
   and `hello_elf` about -4%, `minimal` -4.4%, `parse_ifs_16` +0.1%).
2. Measure, then design carrying values without registration for roots
   that die within their admission, starting with WHNF lazy requests and
   inline forcing, and review it before building it.

   Measured 2026-10-13 at `e2e6e2a2`: a registration and its drop cost
   about 530 instructions without validation (a loop of `root` and drop,
   with the deferred free at collection): the `Arc` cell's allocation and
   free, the registry push under the heap's data mutex, and the counter.
   Dead cells stay in the registry until a collection, or until heap
   teardown when none runs: a 200-step reflection chain spends 11 M
   instructions (6.7%) tearing down its heaps, about 3 M of it freeing dead
   root cells. Carrying the roots that die within their admission would
   save about 23 K of a 434 K `.r ()` step (5%) once every carrier
   converts, 6 K (1.3%) for WHNF lazy requests alone, and about 13 M of
   `countdown_800`'s 229 M (5.7%), nearly all WHNF lazy requests.

   Proposed design, `gc-hold-local-roots` (for review):

   - `glam-gc` gains `LocalRoot<T>`, a slot in its owned mutator's local
     table: `MutatorAccess::carry(&self, GcRef) -> LocalRoot<T>` writes the
     pointer into a free slot, with no allocation, lock or atomic.
     `LocalRoot::as_gc` views it under the same mutator;
     `LocalRoot::into_root` registers an ordinary root for state that
     outlives the quantum. Dropping one frees its slot. A recursive entry
     carries into its owned mutator's table.
   - `Mutator::suspend` publishes the table to the heap under the data
     mutex before releasing admission, and withdraws it after re-admission,
     so the collector marks its live slots; the cost scales with suspends,
     not values. A local root dropped while its mutator is suspended
     defers its release to a thread-private list applied on resume. The
     table lives in its own allocation, so the collector's reads never
     overlap a live `&mut` on the owning thread (Miri).
   - Local roots carry no lifetime. Glam's access closures are
     higher-ranked, so a carrier made inside one cannot name the mutator's
     brand without a second lifetime on every access type (about 690
     parameters). Instead a local root is `!Send`, and `as_gc` and
     `into_root` check, in every build, that it belongs to the current
     admitted mutator: a carrier kept past its quantum panics on use rather
     than dangling, and its drop does nothing.
   - Glam's first slice: WHNF lazy requests carry a local lazy root; the
     inline stack's frames and claims hold it, the base keeps the route's
     durable root, and persistent consumers (`lazy_root_wait`, spills)
     convert with `into_root`. Later slices: WHNF results rooted as value
     nodes, seed promotion, net-WHNF transitions, reflection decode
     carriers.

   Rejected: relying on each inline lazy being reachable from the frame
   below, which holds by the stack's construction but rests on invariants
   spread across forwards, cycles and failure; and a cheaper `Root`
   (pooled cells, a pruned registry), which keeps the lock and reference
   counts. Pruning dead registry entries when the registry grows would
   still bound its memory to the live roots, an independent idea.

   Shelved (maintainer, 2026-10-14) in favour of making roots as cheap as
   possible first: a local root must beat the cheapest durable root, not
   today's.

3. `gc-hold-pooled-roots` (landed): roots are counted slots in heap-owned
   chunks (`pooled-roots`). Before it, a root cost about 527 instructions:
   61% malloc and free of its `Arc` cell, 13% the locked registry push, 8%
   reference counts, 6% the collector's registry walk. After it, about 82,
   20 of them zeroing new chunks. Per `.r ()` step 434.4 K to 406.1 K
   instructions (-6.5%); across the profiling workloads -6.1% to +0.6%
   against `gc-hold-root-evidence` (`sum` -6%, `countdown` and
   `append_walk` -4 to -5%, `chain` and `hello_elf` -2%), and -14.5% to
   -0.2% against `f25c0621`. The two increases, `list_literal_4000` +0.6%
   and `dict_lookup_2000` +0.3%, are -1.2% each in whole-process counts.
   Ideas: hand out a fresh chunk's slots by bumping an index instead of
   linking them all, and allocate chunks zeroed, which avoids the 20; reuse a
   slot dropped on its own admitted thread before the next collection.
   Measure again before reconsidering local roots.

   Variants compared 2026-10-14 (maintainer's list), on experiment commits
   off `40eb2895`. Per `.r ()` step, with about 0.5% noise between runs;
   chunks at exit for `chain_r200`, `countdown_800`, `sum_800`, `chain_800`:
   - V0, landed: slots return only at a heap collection, which builds fresh
     free lists from zero-count slots, with no sentinel; the epoch discards
     threads' earlier lists. 406 to 408 K; 62, 59, 92, 98 chunks.
   - V0b: the admitted thread's free list in a thread-local `Cell` instead
     of its `RefCell` record. 404.5 to 408 K; a root costs about 66
     instructions instead of 82 in the loop benchmark.
   - V1b, V0b plus freeing on release: a root's final drop on a thread
     admitted to its heap pushes its slot onto that thread's list. 411.8 K
     (+1%), and -0.3% to +0.8% elsewhere; 62 instructions in the loop
     benchmark, which reuses every slot. The chunk-header read on every drop
     and the check on final drops cost about what reuse saves.
   - V2, V0b plus reclamation apart from heap collections: a chunk is leased
     while a thread's list holds its slots; when no chunk has a free list,
     the pool rebuilds the lists of unleased chunks, which is safe while
     mutators run, but only after allocating as many new slots as were live
     at the last scan, else it allocates a chunk. 406 K, level elsewhere;
     11, 13, 18, 17 chunks, with the pacing flaw described below.
   CPU time scattered by 10% between runs of the same code, too noisy to
   rank these; peak memory was the same within 0.3 MiB.

   Adopted (maintainer, 2026-10-14): V2, which builds on V0b
   (`root-chunk-reclamation`). Its tests found a pacing flaw in the
   experiment: it counted only new chunks as allocation since the last
   scan, so slots reused from a scan never triggered the next one and the
   pool grew by the live roots after every productive scan. Counting every
   slot handed out fixed it: 2, 2, 2 and 3 chunks at the end of the four
   runs, with instruction counts level with V0. With so few chunks
   allocated, initializing a fresh chunk costs at most about 15 K
   instructions a run, so bump allocation within new chunks is not worth
   adding (the maintainer doubted it would be). Landed as `c7d1a70e`:
   across the profiling workloads -0.6% to 0.0% against `40eb2895`.

### Suspension review (`gc-hold-suspension-review`)

Suspend only around operations with a long or unbounded wait on the world
outside the heap, and avoid those where feasible (maintainer,
2026-10-13). Review the suspension sites that `gc-hold-lend-mutator`
needed: `TaskHost` snapshot, validate and commit; specialization
requests; host calls; releasing pumped work; the reflection launches and
contention wait deferred until after admission; and the test fixtures
that wait inside polls. For each, decide whether it waits on the outside
world, and whether it could run under the mutator or return its request
to a driver instead.

Counts from 2026-10-14 (see `gc-hold-boundaries`, fourth slice): releasing
pumped work suspends once per pump, 5.1 times per `.r ()` step, the only
frequent suspension; it suspends because settling a terminal task can
evaluate a diagnostic through a nested driver, which under `driver-mutator`
could borrow the mutator instead (about 0.7% of a step). `TaskHost`
validation suspends 136 to 257 times per run during macro expansion: client
code, but brief.

### Heap context in Glam (`gc-hold-thread-context`)

With one heap per thread, Glam's call chains could find their runtime from
the mutator instead of passing value factories down (maintainer,
2026-10-09). Investigate the trade: a thread-local lookup against passed
references, and which same-runtime validations it makes redundant.

### The public value API (`gc-hold-values-api`)

Each `Values` method takes its own mutator, so a client building or
reading values call by call enters the heap once per call. The maintainer
deferred this on 2026-10-14 until heap entries elsewhere are reduced. One
idea: a public scope that holds one mutator across calls.
