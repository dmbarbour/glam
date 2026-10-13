# Explicit Heap Holds — 2026-10-09

Status: in progress; `gc-hold-api` landed on 2026-10-13. The maintainer
adopted the 2026-10-10
design review on 2026-10-12 (see "Decisions, 2026-10-12") and suggested
pulling this plan forward, ahead of the leftovers of
`perf-reflection-step-cost`. Since the plan was written, a per-thread
allocation-class cache landed (`perf-allocation-path`) and transient-root
avoidance landed (`perf-transient-root-avoidance`, `2484ad2a`). Steps, in
order: `gc-hold-api`, `gc-hold-boundaries`, `gc-hold-tests`,
`gc-hold-transient-roots`, then `gc-hold-thread-context`;
`gc-hold-class-cache` is narrowed to a measurement. The Steps use the
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
   entries until they take access from the context. Second part: the
   machine poll takes the context mutably, reflection polls hold a
   mutator, and their host callbacks and pumping suspend it.
4. The public API, the compile entry, the remaining waits and the
   production nesting sites.

### Tests and enforcement (`gc-hold-tests`)

Convert the test helpers, then remove the shim, so access without a
mutator panics in every build.

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

### Heap context in Glam (`gc-hold-thread-context`)

With one heap per thread, Glam's call chains could find their runtime from
the mutator instead of passing value factories down (maintainer,
2026-10-09). Investigate the trade: a thread-local lookup against passed
references, and which same-runtime validations it makes redundant.
