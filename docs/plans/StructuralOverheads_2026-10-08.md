# Structural Overheads — 2026-10-08

Status: active, as the `perf-structural-overheads` step of the
[performance roadmap](PerformanceRoadmap_2026-10-05.md). Done:
`perf-admission-wakeups`, `perf-idle-wakeups`, `perf-fast-id-hashing`,
`perf-net-builder-wired-ports`, `perf-scaling-workloads`,
`perf-list-front-walk`, `perf-list-leaf-walk`, `perf-access-region-cost`,
`perf-worker-scaling`, `perf-collection-growth`,
`gc-one-heap-per-thread` and `perf-quantum-region`. Next:
`gc-bounded-collection-wait`.

## Purpose

Remove the costs that are not representation: scheduler round trips, the
allocation and rooting path, lock and wake-up traffic, and algorithmic
defects. Left in place, they would mask what the representation steps
measure. The scope began as the holistic pre-performance review's P2. The
performance review after the recursion work (2026-10-08, retired at
`b2ee16c9`) added the open steps below; this plan now holds its findings.

Costs that belong to a representation go to its plan instead:
- **Net node storage.** Every port access is a hash lookup; this is
  `perf-net-representation` in the roadmap.
- **Allocation volume.** Allocation is diffuse, and most of it shrinks with
  value representation; see
  [Value Representation Refinement](ValueRepresentationRefinement_2026-08-19.md),
  "Current Pressure".

## Method

- **Instruction counts** from `perf stat -e instructions:u`, with
  `task-clock` for CPU time. Wall time is trend data only: the machine is
  shared. `scripts/profile.sh` reports both.
- **Sampling profiles** with `perf record -g` on a release build with line
  tables and frame pointers. Allocator samples need DWARF unwinding, since
  libc has no frame pointers.
- **Scaling series.** A cost that looks constant at one size can be
  quadratic. `scripts/profile.sh` runs each workload family at n, 2n and
  4n and reports each cost's growth exponent (see `perf-scaling-workloads`
  and [Scaling 2026-10-08](#scaling-2026-10-08)).

## Baseline 2026-10-08

At `84bfd7d2`, after the recursion work. Instructions per family:

| Family | n = 500 | 1,000 | 2,000 | 4,000 | Last ratio |
| --- | ---: | ---: | ---: | ---: | ---: |
| countdown | 1,537 M | 3,012 M | 5,961 M | 11,859 M | 1.99 |
| flat list literal (front end only) | 77 M | 105 M | 203 M | 566 M | 2.79 |
| `dict_lookup` | 265 M | 508 M | 1,167 M | 3,001 M | 2.57 |
| `list_map` | 489 M | 1,312 M | 4,259 M | 15,334 M | 3.60 |

A ratio near 2 per doubling is linear, near 4 quadratic. The countdown is
linear; at size 4,000 each other family has one dominant function, named in
its step below.

Where a countdown's time goes (`countdown_2000`, share of samples):

| Area | Share |
| --- | ---: |
| Collector admission per outer access region | about 20% |
| Allocator (`malloc`, `free`, `memmove`) | about 14% |
| Net reduction (`drive_net_batch`, inclusive) | 16% |
| Collection and finalization | 6% |
| Net building and polarity checks during evaluation | 2.6% |
| Forward following (`follow_forwards`, inclusive) | 3% |
| Coordinator claims, releases and admissions | under 2% |

## Open Steps, in Order

### Bounded collection wait (`gc-bounded-collection-wait`)

Collection waits for an idle heap, and since `perf-quantum-region` each
evaluation quantum holds the heap, so overlapping quanta can starve it
(accepted for that step's commit, maintainer). Design (maintainer,
2026-10-09): while a collection waits, hold back new held regions at the
quantum boundary, the three polls that call `with_held_region`, and admit
every other entry as now. The boundary holds no glam lock, which matters:
settlement holds the runtime mutation gate for writing while it constructs
values, and promise publication takes that gate for reading inside a
region, so holding settlement's entry back would deadlock. A collection
then waits at most for the quanta in flight, and drivers converge on it at
their next boundary. The gate's word can carry the request.

### Two-level mutator access (`gc-two-level-mutator-access`)

Arbitrary recursive same-heap entry was introduced for possible Glam uses
that never appeared: before held regions, Glam made about 104 recursive
entries per run (maintainer, 2026-10-09). Held regions now need exactly one
level beneath them. Restrict entry to a held region with plain accesses
inside it, and reject nesting a mutator inside a mutator, which may
simplify the collector's reasoning and tests.

### Heap context from thread-local storage (`gc-thread-local-heap-context`)

With one heap per thread, the current heap is known from thread-local
storage, so carriers need not thread heap identity through every frame
(maintainer, 2026-10-09). Investigate the trade: a thread-local lookup
against passing heap references down the stack, and which same-heap
validations it makes redundant. Pairs with
`gc-two-level-mutator-access`.

### `list_map` growth (`perf-list-map-growth`)

Split from `perf-interface-demand-walk` on 2026-10-09, since nothing yet
shows that walk is the cause. After `perf-list-front-walk`, `list_map` is
still quadratic (exponent 1.92 from 2,000 to 8,000 items) at half its
former cost; its reductions, accesses, roots and allocations all grow
linearly, so the growth is in the cost per operation. At `list_map_8000`
the largest self costs are `memset` (7%), `NetWhnfMachine::poll_in`
(5.8%), `Topology::check` (4.5%) and `RuntimeNet::wire` (4.3%). Before
`perf-list-front-walk`, at `list_map_4000`, the interface walk's node
lookups and hash-set inserts were almost 40% of samples.

The step starts by profiling two sizes to find which costs grow, then
decides whether the remedy is `perf-interface-demand-walk` or a step of
its own.

### Free bindings in lowering (`perf-lowering-free-bindings`)

`ResolvedNetLowerer::lower_code_in` calls `body.free_bindings()` for each
nested lambda or lazy body, and each call walks that body's whole subtree.
A large dictionary literal nests about as deeply as it has entries, so
lowering is quadratic: `collect_free_bindings` is 40% of
`dict_lookup_4000`. A `do` block nests each step's continuation the same
way: `collect_free_bindings` and its `BTreeSet` updates are about 45% of
`do_chain_1600`, a 1,600-step chain, whose cost per step grows with
exponent 1.6. The walk also recurses on the Rust stack to the
literal's depth, an instance of the roadmap's `perf-pre-eval-stack-depth`.

Candidate remedies: derive each body's captures from the uses the lowerer
already records, or compute every body's free set in one bottom-up pass.
The first needs care with parameters that `ApplyLambda` binds inline.

### Interface demand walk (`perf-interface-demand-walk`)

`RuntimeNet::poll_interface_demand` walks from an interface along auxiliary
to principal ports until it finds an active pair. Each poll starts again at
the interface and records every visited node in a freshly allocated hash
set, so repeated polls of a long chain are quadratic. At `list_map_4000`,
the node lookups (`RuntimeNet::reference`, 20%), the set inserts (10%) and
their rehashing (8.4%) are almost 40% of samples. (The rehash appears under
an `EvaluationTaskId` symbol only because identical generic code was
folded.)

Candidate remedies: detect cycles without allocating (Brent's algorithm),
and remember the frontier found for an interface, revalidated against the
net's topology revision, so that a poll resumes where the last one stopped.

`list_map` is no longer attributed to this walk; see
`perf-list-map-growth`.

### Module definition demand (`perf-module-definition-cost`)

Found by the `chain` workload, a chain of module definitions
`x2 = x1 + 1`, … Each demanded module definition costs about 1,500
reductions, 97 of them reflection steps, and 6.5 M instructions at 400
definitions: more than twice a countdown level, which also adds one. A
definition that nothing demands costs almost nothing. The reflection steps
probably come from the module's shared reflection boundary for final
`refl.*` (`g_syntax/module_lowering`), which every named module definition
passes through, though no `refl` task exists here; this is from reading
the code, not yet confirmed. `hello_elf` runs 7,834 reflection steps.

The cost also grows mildly with the number of definitions (exponent 1.24;
8.9 M instructions per definition at 1,600). At that size exact-route
validation (`validate_exact_route_locked`, 4%) and `work_for_wait_locked`
(2%) lead the profile, and each collection traces the whole definitions
dictionary (`visit_dict_edges`, 2%).

The step starts by confirming where the reflection steps come from and
what the boundary does per definition, then asks whether a module without
`refl` tasks can skip it.

### Allocation and rooting path (`perf-allocation-path`)

Holistic V1, listed in the value-representation plan's V-1 prework, plus
the root registrations measured since:
- **Class lookup per allocation.** Every allocator acquisition locks the
  process-wide metadata registry (`metadata_for`), derives the run
  geometry again, and locks the heap's data mutex (`discover_class`).
  These three are about 4.4% of `chain_400`'s samples.
- **Root registration.** Each registered root allocates an
  `Arc<RootCell>`, and `countdown_400` registers about 100,000.

Remedies: a per-family static or per-thread class cache keyed by metadata
address, shared by allocators and roots; register fewer transient roots
(code inside one access region can use edges); and pool `RootCell`s.

### Operator nets (`perf-runtime-net-attach`)

`attach_net_many_in` builds a fresh net for each core operator call and runs
the full template checks: validation, polarity and normalization. Net
building is 2.6% of the countdown, inclusive, and its polarity check mostly
comes from this caller; in `hello_elf`, net building and checking take about
2.7% of self time. The evaluator fixes the net's shape, not user input.
Candidate remedies: skip the polarity check for evaluator-built shapes
covered by tests, or reuse a template per arity.

### Reflection step cost (`perf-reflection-step-cost`)

Holistic R1, R2 and R6, still open: the reflection store's change log grows
without bound and each validation scans it; the reflection machine
deep-clones its active branch every step, so a `do` chain costs O(n²); and
captured continuations are never released. No profile has measured them
yet. The `do_chain` workload does not: its pure effects reduce in nets, and
its reflection-step count stays constant. A workload that runs a long chain
of reflection effects would.

### Route walks with workers (`perf-worker-route-walks`)

After `perf-worker-scaling`, `chain_w4_800` still runs 8% more
instructions than `chain_800` and 2.3 times its CPU time, growing as
n^1.27 against n^1.23. Two walks remain quadratic:
- **Workers walk from the root.** A worker finds work by walking each
  reflection root's exact producer chain from the top on every claim
  (`exact_producer_probe_locked`). The profiling counters do not see these
  walks. A worker could keep a route as the foreground does.
- **Mid-route mismatches rebuild.** When a worker changes a record in the
  middle of the foreground's route, validation falls back to a complete
  search (781 `changed_dependency_fallbacks` and 290
  `guarded_release_mutation_fallbacks` at `chain_w4_800`). The frames above
  the mismatch are proven, so the route could shrink to them, as it now
  does for a retired tip, and reach the same tip a rebuild would.

Walks are about 8% of samples and lock contention 4%. Workers are opt-in
and help only programs with parallel work, so this comes last.

## Experiments

- **Coalesced wake-ups** (`perf-coalesced-wakeups`), open. The maintainer's
  suggestion: coordinator mutations set a notify flag that is flushed once
  per quantum, trading up to a quantum of latency for fewer wakes of parked
  threads. The notifying thread must flush before it parks or blocks
  itself. It matters only with threads parked, so it starts with a profile
  that has workers enabled.
- **mimalloc** (`perf-mimalloc-allocator`), not adopted. A temporary build
  with the mimalloc global allocator ran 9–16% fewer user-space
  instructions but used more CPU, apparently in the kernel (memory
  reservation and page faults in this container):

  | Workload | glibc M instr | mimalloc | glibc CPU ms | mimalloc |
  | --- | ---: | ---: | ---: | ---: |
  | `countdown_400` | 1,190 | 1,078 | 278 | 376 |
  | `hello_elf` | 3,493 | 3,171 | 786 | 959 |
  | `list_map_1000` | 1,283 | 1,074 | 272 | 275 |
  | `minimal` | 34 | 30 | 22 | 67 |

  Allocating less is the better lever.

## Done

- **One region per quantum** (`perf-quantum-region`), 2026-10-09. Done
  before `gc-bounded-collection-wait`, accepting starvation in between
  (maintainer).
  - **Held regions.** `glam-gc` gains `Heap::with_held_region`, an outer
    entry without a mutator whose admission the thread's heap state owns,
    and `Heap::release_held_region`, which ends it early when it is the
    thread's only active entry. Entries inside are recursive.
  - **Where.** Deferred and lazy-route task polls, client demands and
    sparks each hold one region. Reflection task polls hold none, since
    they call into their host between steps.
  - **Release points.** `CountedCondvar` waits, host calls
    (`HostCallProducer::invoke`), reflection launchers, synchronous drivers
    (`drive_client_demand`) and pressure servicing release the region; a
    released region is not reacquired. The tests found each of these: a
    hang where a host-work fixture blocked inside a poll, and probes that a
    collection can run in a launcher, an interpreter callback, and nested
    pumping.
  - **Cross-runtime test values.** Two spark sentinels, run on a worker,
    built their result with the worker thread's own test runtime since
    `test-fast-tier`; they now build it in the evaluating runtime.
  - Outer regions fall about a hundredfold (`countdown_800` 606,273 to
    5,915), and instructions 2.7% (`countdown_800`, `sum_800`), 2.5%
    (`append_walk_800`), 1.8% (`hello_elf`), 1.5% (`chain_800`) and 0.7%
    (`list_map_2000`). `chain_w4_800` is unchanged within noise. The region
    merges once kept as a fallback are no longer needed.

- **One open heap per thread** (`gc-one-heap-per-thread`), 2026-10-09.
  - **Why.** A thread could hold mutators for several heaps at once, which
    is what made a draining collection request too complex
    (`idle-entry-election-and-condvar-admission`). Glam's runtimes never
    nest heaps, and the collector need serve only Glam, so the contract
    tightens: a thread holds mutators for at most one heap at a time
    (maintainer; `one-heap-per-thread` in `docs/Decisions.md`).
  - **Enforcement, in every build.** A thread-local current-heap slot names
    the heap the thread entered last. Preparing an entry for another heap
    while that one is active panics before any TLS record or admission.
    Entering the current heap again reuses its record without a registry
    lookup, and `thread_has_any_active_mutator` reads the slot; debug builds
    check it against the registry.
  - **Tests.** No production path nested heaps; only tests did. Two became
    tests of the rule, in `glam-gc` and in glam's access layer. Three that
    tested nesting itself retired, with the reciprocal Loom model. Three
    adapted: an unwind fixture now unwinds a recursive region, a scale
    fixture enters its second heap after leaving the first, and a request
    survives an unwind on one heap.
  - Instructions fall 0.8% at `countdown_800` and 0.6% at `hello_elf`.

- **Cost per collection** (`perf-collection-growth`), 2026-10-09.
  - **Found** by `append_walk` at 800 to 3,200 items: instructions grow
    with exponent 1.70 while every counter grows linearly, and collection
    took 45% of CPU time at 3,200. Two growths multiply. The number of
    collections (4, 8 and 14) is expected: a periodic full collection
    traces every live item, so a program holding a growing list pays
    O(n²); generational collection is the eventual mitigation. This step
    took the cost of each collection (43, 99 and 273 ms), which grows 2.3
    and 2.8 times per doubling.
  - **Profile** at `9e651b11`, before any collector change. Collection is
    47% of `append_walk_3200`'s samples; its cost is slot resolutions
    times the cost of each, and both grow:
    - **Resolutions repeat.** The last six collections resolve 1.5 to
      5.2 M edges into 23,000 to 43,000 distinct slots: each slot 120 to
      150 times on average, and up to 3,201 times, one per list item.
      List values are `Arc` structure with no mark bits, so marking traces
      shared list structure once per path that reaches it. Visiting list
      edges is 31% of collection self time, and slot resolution 27%. Each
      list is also walked twice per visit, once for thunk edges and once
      for value edges.
    - **Each resolution scanned its class's runs.**
      `AllocationClassEntry::contains_run` compared run records one by
      one: 9.5 per resolution in the first collection, 33 to 38 in the
      last, when the largest class held 148 runs; about 11 to 20% of
      collection time.
  - **O(1) run membership.** Each class keeps a set of its run locations
    beside its ordered run pool, updated where the pool gains or loses a
    run. A run detached for finalization keeps its header but leaves the
    set, as it left the pool. At `append_walk_3200`, over three
    interleaved runs, instructions fell 12.6% (41.2 G to 36.0 G), cycles
    and CPU time 7.7% (8.30 s to 7.71 s), and collection time 11% (4.08 s
    to 3.58 s). `countdown_800` and `hello_elf` are unchanged.
  - **Repeated traversal moves to value representation.** The maintainer
    expects it to remain until basic data types stop using `Arc`
    (2026-10-09). Deduplicating shared `Arc` structure within a collection
    is not a local change: `glam_gc::Visitor` carries no per-collection
    state, and list and dict traversals would need node identities.
    Recorded under `perf-value-representation` in the roadmap.
  - In debug builds the same resolution runs on every managed access
    (`debug_assert_access`), which locks the heap. With per-thread test
    runtimes it is about 3% of the test suite.

- **Worker scaling** (`perf-worker-scaling`), 2026-10-09.
  - **Found** while comparing builds with `GLAM_WORKERS=4`: the profiling
    workloads ran without workers, so nothing had measured it.
    `chain_800` took 108 to 283 s of CPU at four workers, against 1.25 s
    at none. Even one worker cost 4.2 times the instructions at
    `chain_300`.
  - **Cause: a client spin.** A worker finishes a record on the chain the
    foreground demands. Publication sets the record's terminal before it
    wakes the blocked dependent, and in that window the exact walks found
    no producer and reported `NoProgress`. The client's abandon check saw
    the progress was latent and declined, and the client loop retried at
    once. Each retry walked the whole chain under the coordinator mutex,
    which the worker needed to deliver the wake, so the window stretched:
    119,000 retries at `chain_300` with one worker, where the wake was
    late only 66 times once the client waited. Cost grew as chain depth
    times retries, and lock order made it vary between runs.
  - **Busy, not stuck.** `dependency_edge_locked` classifies the edge out
    of a blocked record: its registered producer, a pending wake (the
    dependency is terminal but the record not yet woken), or the end of
    the route. The exact route, the background probe and the causal-child
    probe report a pending wake as `Busy`, so the client waits on the work
    generation. Test:
    `exact_walks_report_a_blocked_record_awaiting_its_wake_as_busy`.
  - **Retired tips.** When a worker retires the route's tip, validation
    returns the route to its nearest registered parent instead of
    rebuilding it from the root; only a route with one parent recovered
    before. At `chain_w4_800`, complete searches fell from 9,197 to 1,193
    and records visited from 7.3 M to 0.96 M.
  - **Profiling.** `family NAME BASE WORKERS` in `scripts/profile.sh` runs
    a family with background workers; `chain_w4` is `chain` at four. The
    JSON report now gives exact-route fallbacks by reason.
  - Results, instructions and CPU time:

    | Workload | Before | After | No workers |
    | --- | ---: | ---: | ---: |
    | `chain_w4_200` | 3,571 M, 1.6 s | 1,239 M, 0.68 s | 1,183 M, 0.28 s |
    | `chain_w4_400` | 59,684 M, 21.9 s | 2,579 M, 1.31 s | 2,429 M, 0.56 s |
    | `chain_w4_800` | 108 to 283 s | 5,783 M, 2.72 s | 5,356 M, 1.20 s |

    Workloads without workers are unchanged (`countdown_800` 2,226 M).
    What remains is `perf-worker-route-walks`.

- **Access-region cost** (`perf-access-region-cost`), 2026-10-09.
  - **Cause.** Every outer access region entered and left the collector's
    mutator admission: about 640 instructions per region, 20% of
    `countdown_2000` and 11% of `hello_elf`'s self time. Per region it
    looked up the thread's heap state and created and dropped a `Weak` to
    compare heap identity, locked the admission mutex on entry and exit,
    and on exit upgraded a `Weak` and added four metrics with atomic
    read-modify-write loops. The evaluator opened about 3.6 regions per
    reduction.
  - **Per-thread region counters.** Each thread cache has its own
    counters, written with plain stores at an outer region's exit and
    summed when metrics are read; a released cache's counters fold into the
    heap's totals.
  - **Heap identity by pointer.** `PreparedThreadHeapEntry::prepare`
    compares addresses; the cached `Weak` keeps the heap's address from
    being reused.
  - **One region to start a `Produce` poll.** It reads cache, source,
    checkpoint and forward together. Checkpoint pollers skip the cache
    check: a lazy cached meanwhile has lost its checkpoint, which they
    already handle (`resume_moved`).
  - These three cut outer regions 16–22% in every workload, and
    instructions 1–5% (`countdown_800` 2,375 M to 2,255 M; `sum_800`
    2,665 M to 2,528 M; `hello_elf` 3,503 M to 3,385 M).
  - **Admission gate** (`admission-gate-fast-path` in
    `docs/Decisions.md`). The active outer-mutator count moved from the
    coordinator mutex into an atomic word beside it: in `Ordinary`
    admission with nobody waiting, entry is one compare-and-swap and exit
    one decrement. One module, `glam-gc/src/admission.rs`, owns the gate
    and every phase change, and the Loom models compile the gate itself;
    four planted faults were each caught. Instructions fell a further
    1.0–1.3% (`countdown_800` 2,255 M to 2,226 M, `hello_elf` 3,385 M to
    3,354 M), but cycles and CPU time did not change measurably in pinned,
    interleaved single-thread runs (`chain_800` −0.2%, `countdown_800`
    +0.8%, medians of six). The profile's 5% for the mutex overstated what
    a fast path saves: an uncontended lock is mostly its atomic operations,
    and the gate keeps two of the four. Its benefit under contention is
    unmeasured, pending `perf-worker-scaling`.
  - Region merges and a quantum-wide region moved to
    `perf-quantum-region`.

- **Leaf walks in list observers** (`perf-list-leaf-walk`), 2026-10-09.
  - **Cause.** `len`, `at`, `split`, `slice` and `split_end` advanced one
    item per builtin step, each step a full lazy-machine poll, so a loop
    that tested `len` stayed quadratic after `perf-list-front-walk`.
    `split` and `slice` copied what they took into a new value leaf.
  - **Leaf walks.** These observers now take one strict leaf (a byte
    slice, value slice or finger tree) per projection step, and every
    strict leaf in one builtin step, stopping only to force a deferred
    chunk. `head` and `tail` still take one item. The item pops are now
    built on leaf pops (`List::pop_front_leaf_step`,
    `List::pop_back_leaf_step`).
  - **Ropes for what is taken.** What `split`, `split_end` and `slice`
    take comes back as a flat slice when it lies within one leaf, sharing
    its storage, and otherwise as a finger-tree rope sharing the leaves it
    spans (`List::join_strict_pieces`). Runs of pieces shorter than 32
    items are copied into one chunk. The remainder is shaped as a pop
    leaves it.
  - **Maintainer decisions** (2026-10-09), recorded as
    `list-observers-walk-leaves` in `docs/Decisions.md`:
    - no cached lengths in `Concat` nodes, since value representation will
      compact list nodes much further (pointer-tagged singleton lists,
      concatenation pairs and other tagged shapes);
    - a program that indexes one list repeatedly asks for an indexable form
      with the `array` or `deque` annotation; `at` does not rebalance.
  - Unit tests cover leaf steps from both ends, rope joining and lookups
    across rope chunks; an evaluator test covers every position observer
    across value leaves, a lazy chunk and a byte leaf.

  Instructions (four-times sizes):

  | Workload | Before | After |
  | --- | ---: | ---: |
  | `list_sum_400` | 1,911 M | 1,690 M |
  | `list_sum_1600` | 10,958 M | 7,540 M |
  | `list_map_8000` | 35,451 M | 35,453 M |

  `list_sum`'s reductions are now linear (exponent 1.70 before, 1.00
  after; 1.86 M to 0.58 M at 1,600), and its instructions grow with
  exponent 1.11, from 1.39. `list_map` and `append_walk` use no position
  observer and are unchanged. `append_walk` at these sizes found
  `perf-collection-growth`.

- **Front of a list** (`perf-list-front-walk`), 2026-10-08.
  - **Cause.** Popping the front of a strict left-deep spine rebuilt the
    remaining spine in the same shape (`List::join_logical_suffix`), so
    every pop cost O(n). The core list operator built every literal as
    such a spine, one `Concat` per item.
  - **Pops reshape.** A front pop now returns its tail as a right-leaning
    spine, and a back pop its init as a left-leaning one, without forcing
    a lazy chunk. A walk from one end takes each `Concat` apart once:
    O(depth) on the first pop, then O(1) amortized. This is the
    maintainer's direction: lazily concatenated lists are the normal case,
    so observers shape lists rather than relying on construction. See
    `list-pop-reshapes-remainder` in `docs/Decisions.md`.
  - **Flat literals.** The list operator now builds one value leaf
    (`List::from_values`), as request payloads already did. The operator
    still copies its supplied operands at each partial application, O(n²)
    for a literal of n items, though cheaply: the flat-literal family
    stays linear to 16,000 items.
  - **A program-built family.** `append_walk` builds a list with
    `build n = build (n - 1) ++ [n]` and walks it with `head`, `tail` and
    `ys == []`. It was already linear: `++` keeps an unevaluated operand
    as a lazy chunk, and forcing chunks one at a time joined short
    suffixes. Only strict spines, such as the literals, were quadratic.
    Unit tests cover strict spines in both directions.

  Instructions (default and four-times sizes):

  | Workload | Before | After |
  | --- | ---: | ---: |
  | `list_sum_400` | 5,933 M | 1,911 M |
  | `list_sum_1600` | 272 G | 10,958 M |
  | `list_map_2000` | 4,000 M | 2,773 M |
  | `list_map_4000` | 14,438 M | 9,605 M |
  | `append_walk_800` | 6,382 M | 6,385 M |

  `list_sum`'s exponent fell from 2.95 to 1.39 (400 to 1,600 items); the
  rest is `len` counting item by item, `perf-list-leaf-walk`.
  `list_map` remains quadratic (1.92 at 2,000 to 8,000); see
  `perf-list-map-growth`.

- **Scaling workloads** (`perf-scaling-workloads`), 2026-10-08.
  `scripts/profile.sh` ran each family at one size, where a quadratic looks
  like a large constant: three of the four quadratics in the review were
  invisible to it. Families now run at n, 2n and 4n, and the summary fits
  each cost to a + b·n^k, reporting k, with fixed costs cancelled.
  `GLAM_PROFILE_SCALE` multiplies the sizes, and `GLAM_PROFILE_TIMEOUT`
  stops a runaway workload. New families: non-tail recursion (`sum`), a
  chain of module definitions, a list walked with `len`, `head` and
  `tail`, a long `do` chain, and a flat list literal for the front end.
  Findings are in [Scaling 2026-10-08](#scaling-2026-10-08).

- **Admission wake-ups** (`perf-admission-wakeups`), 2026-10-08. Every
  outer access region ended with a condvar `notify_all`, a `futex` syscall
  even with no waiter: about a third of the CPU time in the countdown. The
  collector now notifies only registered waiters, which halves the
  countdown's CPU time. See the evaluation-recursion plan, "Finding
  2026-10-08".
- **Idle wake-ups elsewhere** (`perf-idle-wakeups`), 2026-10-08. Every
  glam condvar counts its waiters (`CountedCondvar`), removing the remaining
  75,000 `futex` calls in `countdown_400`: CPU time 407 to 330 ms,
  `hello_elf` 1,129 to 827 ms.
- **Wired ports in the net builder** (`perf-net-builder-wired-ports`),
  2026-10-08. `NetBuilder::try_wire` checked that neither port was wired by
  scanning every existing wire, so building a net with W wires cost O(W²):
  60% of lowering a flat 4,000-element list. The builder now keeps a set of
  wired ports. Builders also run during evaluation, so evaluation gains
  slightly.

  | Flat list literal | n = 500 | 1,000 | 2,000 | 4,000 |
  | --- | ---: | ---: | ---: | ---: |
  | Before | 77 M | 105 M | 203 M | 566 M |
  | After | 71 M | 78 M | 92 M | 121 M |

  The same commit gave `follow_forwards`, new with tail forwarding, a fast
  path: a single forward to a lazy that does not forward returns without
  allocating. Shortening keeps nearly every chain to one forward.
- **Fast id hashing** (`perf-fast-id-hashing`), 2026-10-07 for glam itself.
  - Maps keyed by runtime-allocated ids use `crate::trusted_hash` instead
    of SipHash. That covers the net runtime's node, copy and port maps, the
    net builder, the coordinator, managed external owners, effect tokens and
    reflection continuations.
  - **Its hasher** does one widening multiply per integer, folded so that
    strided keys spread across buckets as well as counters do. It has no
    collision resistance.
  - **Keys are checked statically.** `TrustedState<K>` requires
    `K: TrustedKey`, which is implemented only in `trusted_hash.rs`, with a
    reason for each key type. A test fails if `TrustedKey` is implemented
    anywhere else, so a new key type gets reviewed.
  - **Compared with foldhash** in the same structure, it ran 0.4–0.6%
    fewer instructions, and its hasher state is zero-sized. Wall-time
    differences were within run-to-run noise. foldhash was used briefly
    (`d80bbb7c`) and dropped.
  - Countdown 100 fell from 735 M to 494 M instructions; `hello_elf` went
    from 3,029 to 2,743 ms, and `list_map_1000` from 802 to 674 ms.
  - Keys a program can influence keep `RandomState`.
  - **The collector crate** keeps its own private copy, so it gains no
    dependency and exports no hasher. It covers the thread-local heap
    cache, arena chunk lookup, the type-metadata registry, and classes by
    metadata. Its cold finalization maps keep SipHash.
  - **Both hashers end with `rotate_left(26)`.** Folding alone left
    2^20-aligned keys clustered in the low bucket bits. With the rotation,
    every tested stride fills buckets at least as evenly as uniform
    hashing.
  - Countdown 100 then ran 454 M instructions, and `hello_elf` took
    2,646 ms.
- **Scheduler round trips** (holistic S1, S5, E1–E4) were the
  `perf-evaluation-recursion` step; see
  [Evaluation Recursion Performance](EvaluationRecursionPerformance_2026-10-04.md).
  Coordinator work is now under 2% of the countdown.

## Scaling 2026-10-08

At `64ba1c00`, rustc 1.99.0, Intel Core i7-6700, release build with
`glam-prof`, zero workers. Growth exponents k (cost ≈ a + b·n^k) with
`GLAM_PROFILE_SCALE=4`, and the marginal instructions per unit of size
between the two largest sizes:

| Family | Sizes | k instructions | k reductions | K instr per unit |
| --- | --- | ---: | ---: | ---: |
| `countdown` | 400/800/1,600 | 0.98 | 1.00 | 2,925 |
| `sum` (not in tail position) | 400/800/1,600 | 1.04 | 1.00 | 3,412 |
| `chain` (module definitions) | 400/800/1,600 | 1.24 | 1.00 | 8,913 |
| `do_chain` | 400/800/1,600 | 1.62 | 1.00 | 4,556 |
| `dict_lookup` | 1,000/2,000/4,000 | 1.48 | 1.00 | 919 |
| `list_map` | 1,000/2,000/4,000 | 1.91 | 1.00 | 5,219 |
| `list_sum` | 400/800/1,600 | 2.95 | 1.70 | 294,957 |
| `list_literal` (front end) | 4,000/8,000/16,000 | 1.03 | flat | 15 |
| `parse_lists` (depth) | 400/800/1,600 | 1.48 | flat | 77 |
| `parse_parens` (depth) | 400/800/1,600 | flat | flat | 3 |
| `parse_ifs` (depth, default scale) | 4/8/16 | 7.71 | flat | 173,584 |

- **Linear:** recursion in and out of tail position, and the front end on
  large flat literals and nested parentheses.
- **Exponential:** `parse_ifs`, a keyword form inside parentheses nested in
  another keyword form, as in `f (if c then f (if …) else 0)`. Depth 16
  parses in 178 ms, and depth 100 did not finish in ten minutes. Each such
  `if` parses its branches with a fresh term parse, which covers the inner
  groups again, though the enclosing term parse has covered them already.
  Without the parentheses (else-if chains, `then if …`) parsing stays
  linear. This belongs to the parser plan's `parser-keyword-frames`.
- **Superlinear, each named in a step above:** `list_sum` and `list_map`
  (`perf-list-front-walk`, `perf-list-map-growth`), `do_chain` and
  `dict_lookup` (`perf-lowering-free-bindings`), `chain`
  (`perf-module-definition-cost`).
- **Mild:** nested brackets cost 77 K instructions per level at depth
  1,600 (k = 1.48). Not yet a step.

At the default scale, the suite takes about 10 s of CPU (42 G
instructions). Its exponents use smaller sizes, so they understate growth
(`list_map` reads 1.82, `do_chain` 1.48), and collections that land at
different sizes make the smallest families read a little above or below 1.

## Not a Step

**Collection and finalization.** Two collections take 65 ms of
`hello_elf`'s 794 ms; in the countdown, collection plus the finalization
batch are about 6%. This scales with garbage, and nothing stands out
against the
[concurrent collection](ConcurrentGarbageCollection_2026-08-28.md) plan.
