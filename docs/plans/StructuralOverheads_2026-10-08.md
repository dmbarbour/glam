# Structural Overheads — 2026-10-08

Status: active, as the `perf-structural-overheads` step of the
[performance roadmap](PerformanceRoadmap_2026-10-05.md). Done:
`perf-admission-wakeups`, `perf-idle-wakeups`, `perf-fast-id-hashing`,
`perf-net-builder-wired-ports`, `perf-scaling-workloads`,
`perf-list-front-walk`, `perf-list-leaf-walk`, `perf-access-region-cost`,
`perf-worker-scaling`, `perf-collection-growth`,
`gc-one-heap-per-thread`, `perf-quantum-region`,
`gc-bounded-collection-wait`, `perf-allocation-path`,
`perf-root-frames`, `perf-transient-root-avoidance`,
`perf-list-map-growth`, `perf-list-literal-composition`,
`perf-lowering-free-bindings`, `perf-interface-demand-walk`,
`perf-interface-route-ownership`, `perf-reduced-source-copy`,
`perf-driver-path-inlining` and `perf-module-definition-cost`. Next:
`perf-runtime-net-attach`.
`gc-two-level-mutator-access` and `gc-thread-local-heap-context` moved to
[Explicit Heap Holds](ExplicitHeapHolds_2026-10-09.md).

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

### Operator nets (`perf-runtime-net-attach`)

`attach_net_many_in` builds a fresh net for each core operator call and runs
the full template checks: validation, polarity and normalization. Net
building is 2.6% of the countdown, inclusive, and its polarity check mostly
comes from this caller; in `hello_elf`, net building and checking take about
2.7% of self time. The evaluator fixes the net's shape, not user input.
Candidate remedies: skip the polarity check for evaluator-built shapes
covered by tests, or reuse a template per arity.

### Introduce and override checks (`perf-definition-assertions`)

Each introduced module definition carries an assertion that the
definitions before it do not define its name (an override, that they do).
With the reflection boundary shared, removing the assertion saves 50% of
`chain_400` and 58% of `chain_1600`; it is the chain's superlinear part
(exponent 1.25). Each assertion looks its name up in the definitions as
they stood just before it, which builds every intermediate dictionary and
keeps it alive while the chain is in progress, and each collection traces
every version in full: collection is 17.6% of `chain_1600` with the
assertion, 4% without. It is also the only duplicate check: `x = 1` then
`x = 2` passes unless `x` is demanded.

Maintainer direction (2026-10-10): compilers should check introductions
and overrides among a file's own definitions and report them at compile
time. A top-level `import` can still introduce a name already defined,
which no compile-time check sees, so a run-time check stays. Its form is
not formal semantics: compilers could record what they introduce or
override under `meta.*`, and the reflection boundary could look for
conflicts there without evaluating definitions. The step designs that
record and the boundary's check, adds the compile-time check, and
measures with `chain` and `defs_unused`.

### Reflection scanner (`perf-reflection-scanner`)

A module's first reflection boundary launches the `refl.*` scanner, which
waits for the final `refl.*` and launches its tasks. It cannot be skipped
when a module defines no `refl` tasks, since imports act as mixins
(maintainer, 2026-10-10). It costs about 22 M instructions per module:
44% of `minimal`, and 11.5% of `list_computed_2000` (building without any
boundary, before `reflection-boundary-shared`). By callgrind on
`minimal`, the boundary's effect program runs through net evaluation
(net checkpoints 20 M instructions with it, 9 M without), creating and
activating the reflection task costs about 2 M, and tearing down the
reflection store and launchers about 2.4 M. The maintainer expects the
speed and quality of the scanner, and of other compiler-generated
reflection tasks and annotations, to improve. Related:
`perf-reflection-step-cost`.

### Reflection step cost (`perf-reflection-step-cost`)

Holistic R1, R2 and R6, still open: the reflection store's change log grows
without bound and each validation scans it; the reflection machine
deep-clones its active branch every step, so a `do` chain costs O(n²); and
captured continuations are never released. No profile has measured them
yet. The `do_chain` workload does not: its pure effects reduce in nets, and
its reflection-step count stays constant. A workload that runs a long chain
of reflection effects would.

Reflection task polls hold no heap region (`perf-quantum-region`), since
they call into their host between steps. Their pure steps, such as local
state, could hold one, provided no client callback runs inside it
(maintainer, 2026-10-09).

### Debug annotations (`perf-debug-annotations`)

Each module definition carries a context annotation for diagnostics,
`anno {context: {g: {origin, line, definition}}} value`; building without
it saves 8 to 10% of the `chain` family. Debugging annotations should be
far cheaper (maintainer, 2026-10-10, open to investigation and
brainstorming). Starting points: what evaluating one costs, and whether a
context known at compile time, as an origin and line are, can travel
with the definition without an evaluation step.

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

- **Poll root frames** (`perf-poll-root-frames`), not adopted
  (2026-10-10; kept as `1f35ad1e`, reverted in `15bbb0bb`). `glam-gc`
  gained `RootFrame`, state outside the heap that the collector traces as
  one root, registered once. Glam rooted the hot transient value roots
  (WHNF ready results, checkpoint completions, list machine items) in one
  frame per evaluation poll, made poll results durable at the boundary,
  and asserted in debug builds that no framed root outlived its poll. That
  check found three escapes the call sites did not show: reflection
  handoff roots, task lifecycle construction, and a strategy machine that
  keeps a root across polls. Root registrations and managed allocations
  fell 10% each, but instructions moved -0.6% to +0.5%: a framed root's
  thread-local scope, frame mutex, handle `Arc` and per-read copy cost
  about what its registration saved. Framing every value root a poll
  creates, rather than chosen sites, framed durable roots too, in code
  unaware of frames.
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

- **Module definition demand** (`perf-module-definition-cost`),
  2026-10-10. Decisions `reflection-boundary-shared`,
  `binding-groups-lower-by-components` and
  `local-bindings-take-parameters`.
  - **Cause.** Each demanded module definition cost about 1,500
    reductions, 97 of them reflection steps, and 6.5 M instructions at
    400 definitions. Its value carries three wrappers; building without
    each in turn:

    | Wrapper removed | `chain_400` | `chain_800` | `minimal` |
    | --- | ---: | ---: | ---: |
    | reflection boundary | -70.3% | -70.7% | -43.8% |
    | introduce assertion | -17.6% | -26.0% | -4.0% |
    | context annotation | -4.2% | -4.1% | -1.4% |

  - **Reflection boundary** (`7aa7cb6d`). Every demanded definition ran
    the boundary's effect as its own reflection task, though it does
    nothing once the module's scanner is recorded. A module or declared
    object now shares one boundary value, which each definition forces
    with `seq`. `chain_800` falls from 4,273 M to 1,403 M instructions
    (-67%); its reflection steps from about 78,000 to 499.
  - **Families.** `defs_unused` (definitions never demanded, 0.6 M
    instructions each, linear), `chain_refl` (the chain with one `refl.*`
    task) and `chain_where` (the chain as nested `where` groups).
  - **Binding groups** (`cf669884`, `097c6e64`). Building the local
    comparison found that `let` and `where` groups resolved their values
    in the enclosing scope, though documented as mutually recursive; they
    now lower by dependency components, with `fixpoint` for cycles, and
    local bindings take parameters as module definitions do.
  - **Nested binders** (`25c66455`). Nested `where` suffixes and nested
    one-line `let`s built in cubic time (10,253 M instructions at 400
    levels, about 79 G at 800), while one group of the same bindings
    built linearly. The unused-local analysis, which runs while parsing,
    copied every enclosing local's name at each nested binder; it now
    copies only when one is shadowed. Nested lets at 400 fall to 317 M,
    `where` chains at 800 to 713 M against 617 M with the analysis off.
    The parser itself is linear here; `parse_ifs` stays exponential
    (`parser-keyword-frames`).
  - **Moved to their own steps:** the introduce and override checks
    (`perf-definition-assertions`), the scanner each module's first
    boundary launches (`perf-reflection-scanner`), and the context
    annotation (`perf-debug-annotations`).
  - **Left.** Lowering a definition costs about 0.6 M instructions even
    when it is never demanded, about 16% of the chain; parts could become
    lazier, though definitions must still lower to some value
    representation (maintainer). The unused-local analysis still re-walks
    each binder's body once, a quadratic remainder of about 95 M
    instructions at 800 nested levels; a single-pass analysis would
    remove it.

- **Driver hot-path inlining** (`perf-driver-path-inlining`), 2026-10-10.
  Decisions `driver-step-boundaries` and `interface-route-ring-record`.
  - **Pin** (`0657f0ef`). The core cursor and pair steps and the net's
    interface poll stay out of line, with the cell's steps inlined into
    them. Instructions fall 0.4 to 1.0% on every family. A patch to the
    walk, and a never-taken branch in the pair rewrite, now move counts
    0.06 to 0.18%, where such a patch moved them about 1% before.
  - **Family.** `literal_loop` evaluates a 32-item literal with computed
    items per iteration, so most of its interface routes are 3 to 16
    nodes deep (mean 5.3, longest 17); nested calls do not make deep
    routes, since each call evaluates in its own small net.
  - **Routes, rerun.** Against the full stack with its 16-node skip: a
    full stack from the first node costs the shallow families 0.3 to
    0.6% and saves 1.7% on `literal_loop`; the ring record from the first
    node costs up to 0.3% and saves 1.75%; the ring from depth 4 costs
    nothing anywhere and saves 1.8% on `literal_loop` and 0.2 to 0.4% on
    `list_computed`. Adopted (`interface-route-ring-record`).
  - **Also.** `a_quantum_boundary_joins_a_waiting_collection` failed about
    one run in sixteen under `aggressive-gc-verification`, which may
    collect again at a quantum boundary; fixed in `42704fe3`.

- **Reduced sources copied whole** (`perf-reduced-source-copy`),
  2026-10-10. Decision `reduced-sources-copy-whole`.
  - **Cause.** A call to a net callable copied the callable's net through
    a remote cursor, one node per cursor step, so the copy would share
    whatever evaluation the source had left. In 97 to 99% of cursor
    inspections the source had none (no active pair, copy or cursor
    obligation), and such sources averaged about 5 nodes, 20 at most.
  - **Fix** (`5e77e49a`). Preparing a copy captures such a source whole
    under its own lock, up to 64 nodes reachable from its interface; the
    target installs it in one transition with fresh fan sites. The
    maintainer sees this as the first, known-safe case of materializing
    chunks together based on partial normalization.
  - **Results**, instructions against `fc6974e8`: `countdown`, `sum`,
    `list_sum` and `append_walk` fall 19 to 22%, `list_map` 15 to 18%,
    `hello_elf` 16.2% (2,833 M to 2,375 M), `chain` about 7%, `do_chain`
    about 1%; the dict and `list_computed` families move under 0.5%.
    Cursor inspections on `countdown_800` fall from 81,041 to 1,696.
  - **Left on cursors**, measured at each family's largest size. `chain`
    keeps 15,964 inspections (of 97,595) and `do_chain` 30,356 (of
    39,292); the others about 1,700. Most of them read a source that is
    fully reduced by then but was not when its copy began (in `chain`,
    10,107 of 15,964): a copy that switched to whole once its source
    finished would cover them, the next case of chunking. The rest read
    sources still evaluating. No source reached the size limit.
  - **Cost now.** By callgrind on `countdown_200`, capturing sources is
    3.4% of instructions and installing copies 2.7%.
  - **Directions** (maintainer, for later). Rewrite normalized nets, or
    chunks, into a compact form for materialization, so each copy does
    not redo the capture's construction work; since nets change after
    construction only by evaluation, a reduced net's form can be built
    once. And a fast materialization path once evaluation is known to be
    done, such as the switch above.

- **Interface route ownership** (`perf-interface-route-ownership`),
  2026-10-10. Decision `interface-routes-belong-to-evaluation`.
  - **Reading.** The remembered route is the continuation stack of a
    weak-head reduction: a rewrite at the tip pushes the nodes it leaves
    waiting, a result reaching the tip pops it, and every resumed poll
    measured followed exactly one rewrite. The maintainer: that state
    belongs to the evaluation, not the net.
  - **Change.** The net driver keeps an `InterfaceRoute` and lends it to
    each root poll; the net cell loses its route map (200 bytes again).
  - **Alternatives measured** (nodes visited on `list_computed`, in the
    decision). Remembering only the node before the pair halves the walk
    but still grows with depth, since a result returning to that node
    loses it; logarithmic checkpoints would save little memory, as every
    stack entry is a live node. Dropping the 16-node unrecorded prefix
    costs common workloads 0.3 to 0.5%.
  - **Results.** Against `db57b647`, instructions fall 0.0 to 0.7%:
    `list_computed_1000` −0.72%, `dict_lookup_2000` −0.62%, the rest
    within 0.16%. A poll no longer looks its route up in a map.
  - **Cursor frontier walks** could keep routes at the evaluation layer
    too (maintainer). Counted on every profile family at its largest size
    and `hello_do`, none is longer than 7 nodes, so resuming them would
    save nothing yet. If one grows: the driver's worklist already stacks a
    `ResumeCursorDependency` frame under each dependency's work, which is
    where such a route would ride. Two differences from interface walks:
    the walk runs inside the cursor step under the source net's lock, so
    the route is lent through `step_cursor_within`; and its answer also
    depends on the copy's frontier map (a peer cursor waiting on any
    spine anchor), which changes apart from the source's topology, so a
    resumed walk must still check the anchors it skips or show that
    missing a new peer only changes which dependency the cursor awaits.
  - **Noticed.** About half of all cursor inspections walk a source spine
    and find it stable, with no pair: 39,647 of 81,041 on `countdown_800`,
    about 50 per iteration, each allocating its list of anchors. In 97 to
    99% of all inspections, on every family measured, the source net has
    no active pairs, copies or cursor obligations left, and such sources
    average about 5 nodes (20 at most). The maintainer suggests tracking
    whether a net is fully reduced and then materializing a copy of it
    whole, since no evaluation is left to share
    (`perf-reduced-source-copy`).
  - **Experiment `perf-route-ring-record`** (maintainer; inconclusive
    here, adopted under `perf-driver-path-inlining`).
    Record routes from the first node in a box holding the newest 8
    nodes exactly and a mark every 8 nodes below them, so a returning
    result walks each stretch between marks once more; memory is 8 nodes
    plus one byte per node of depth. Instructions rose 1.0 to 1.6% on
    every workload, and still 0.8 to 1.2% with the ring skipping the first
    4 nodes, where its code barely runs. Exact counts (callgrind, `minimal`)
    put the difference in the net driver's hot path, whose inlining
    changed: `NetWhnfMachine::poll_in` and `step_active_pair_if_current`
    were split differently, and small `RuntimeNetCell` wrappers became
    calls. A placebo (an unused boxed field) moved counts 0.15%, and the
    committed walk kept out of line 0.04%. So most of the difference is
    code generation on that path, not recording, and sub-percent
    comparisons of changes there are unreliable until its inlining is
    pinned (`perf-driver-path-inlining`). Walks average 1.3 nodes in
    most families; a record cannot save on walks of one or two nodes,
    since checking a recorded node costs the lookup the step it saves
    would.

- **Interface demand walk** (`perf-interface-demand-walk`), 2026-10-10.
  Decision `interface-walks-resume`.
  - **Two costs.** `RuntimeNet::poll_interface_demand` walked from an
    interface along principal-to-auxiliary wires to the active pair,
    recording every node in a freshly allocated hash set to notice a
    cycle; and every poll started again at the interface. Composition
    made literal walks short (`perf-list-literal-composition`), but the
    set still cost about 22% of `list_computed_4000`, and any net with a
    long chain rewritten at its far end polled quadratically (the
    maintainer: there are many ways to build such nets).
  - **Brent's algorithm** (`2b13fb75`). A shared `WalkCycle` notices a
    cycle with one saved item instead of a set. `list_computed_4000`:
    469 M to 415 M instructions.
  - **Resumed routes** (`5c34b13b`). Each interface keeps the end of the
    route its last walk took; the next walk drops consumed nodes from the
    tip and goes on from the last survivor. Sound because consumed route
    nodes are always a suffix of the route (see the decision); debug
    builds check every resumed walk against a full walk. A fresh walk
    records nothing for its first 16 nodes, so short walks allocate
    nothing. Common workloads move within 0.15% (the net cell grew 8
    bytes); `list_computed_2000` fell 4.3%. A test erodes a 40-node chain
    from its far end, checking the remembered route at each step.
  - **Other walks** (`2b32aa46`). Five read-only single-path walks moved
    from sets to `WalkCycle`: the cursor frontier walk and four
    dependency-chain walks. Kept as sets: `reported_dependency` (names
    where its walk first repeats), `follow_forwards`' slow path (needs
    the cycle's members) and the causal probe's child-work set (spans
    many walks).
  - **Left.** The cursor frontier walk restarts too; the same suffix
    argument would let it resume if it shows in profiles (measured under
    `perf-interface-route-ownership`).

- **Free bindings in lowering** (`perf-lowering-free-bindings`),
  2026-10-10. Decision `dict-literals-compose`.
  - **Cause.** Lowering each nested lambda or lazy body (a closure's code
    net) first walked the body's whole subtree for its free bindings, its
    captures, so a body nested d deep was walked d times. Two shapes nest
    that deep: a dict literal resolved to a left-nested chain of unions,
    one per member, and a `do` block nests each step's continuation.
  - **Captures from recorded uses** (`091984bd`). The lowerer already
    records every local use while compiling a body, a nested closure's
    captures among them, so the uses left unbound once the body is
    lowered are exactly its captures. `free_bindings` stays as a test
    helper; a test checks the two agree.
  - **Dict literals** (`c411e8ee`). A literal of single-key entries of
    closed data, with atom, number or text keys and no key repeated,
    resolves to its dictionary; any other joins its members with union in
    the finger-tree shape list literals use (`finger_join`).
  - **Results:**

    | Workload | Before | Captures | Dict literals |
    | --- | ---: | ---: | ---: |
    | `dict_lookup_2000` | 992 M | 613 M | 81 M |
    | `do_chain_800` | 1,759 M | 1,038 M | 1,038 M |

    `dict_lookup` no longer reduces at run time; `do_chain`'s exponent
    fell from 1.54 to 1.11 over 200 to 800 steps. A new `dict_computed`
    family (computed values) overflowed the Rust stack at 5,000 entries
    as a chain; composed, it runs to 20,000 (7,950 M instructions). At
    2,000 entries composition costs 6% (749 M against 708 M), since a
    balanced union re-inserts each entry about log n times where a chain
    inserts it once.
  - **Left.** A computed entry or list item costs about 370 K
    instructions, mostly lowering its value to its own code net.
    Readiness, which never evaluates, now reads a closed literal
    payload's message at once; one public test was adjusted to keep
    covering a payload only the projection can read.

- **List literal composition** (`perf-list-literal-composition`),
  2026-10-10. Decision `list-literals-compose`.
  - **Cause.** Closed literals were fixed by `perf-list-map-growth`, but
    a literal with computed items still lowered to one list operator
    collecting every item: `list_computed` grew with exponent 1.85
    (1,143 M instructions at 2,000 items, 4,138 M at 4,000), the walk
    about 73% of samples and the operand copies about 19%.
  - **Origin.** The interaction-net spike (`2b433822`, 2026-07-15) lowered
    list and access literals to chains of its one unary host agent, which
    became `CoreOperator` in `00ece3ed`; the original agent already copied
    its supplied operands at every call.
  - **Fix** (`f5373979`). The list operator collects at most eight items.
    A longer literal lowers to parts joined with `++` in the shape of a
    finger tree: runs of eight or more closed items become list values,
    the other items lists of at most eight.
  - **Results.** `list_computed` is linear (exponent 1.03 over 1,000 to
    4,000 items); 4,000 items fall from 4,138 M to 469 M instructions.
    What remains is lowering each computed item to its own code net
    (about 34% of samples) and the walk's per-poll set (about 22%,
    `perf-interface-demand-walk`).

- **`list_map` growth** (`perf-list-map-growth`), 2026-10-10.
  - **Cause.** Not `map`, but the workload's two list literals. A literal
    lowered to the list operator applied to one item at a time. Each
    partial application copied the items supplied so far, and found its
    next step by walking the rest of the application chain from the net's
    interface (`perf-interface-demand-walk`). Counting walks at 2,000 items
    found two descending passes, every length from 2,000 down appearing
    four times: 8 M walk steps. At `list_map_8000` the walk was about 73%
    of samples and the operand copies 10 to 20%.
  - **Fix.** A literal whose items are all closed data is closed data
    too, so the resolver now gives its list value, nested literals
    included (`5e89b5da`). The lowering already did so for empty lists.
  - **Results.** `list_map` is linear (exponent 1.93 to 1.00 over 2,000 to
    8,000 items); instructions at 8,000 items fell from 35,042 M to
    2,007 M, and CPU time from 10.7 s to 0.43 s. Other workloads are
    unchanged.
  - **Left.** Literals with computed items still build at run time and
    stay quadratic; the new `list_computed` family tracks them under
    `perf-interface-demand-walk`. `dict_lookup`'s growth is
    `collect_free_bindings` (`perf-lowering-free-bindings`), a different
    cause.

- **Transient root avoidance** (`perf-transient-root-avoidance`),
  2026-10-10.
  - **Sites.** Sampling registrations on `countdown_800` (debug build,
    every 64th) found the hot transient roots:

    | Site | Share | Root carries |
    | --- | ---: | --- |
    | WHNF shell deferred lazy request (`reduce_semantic_shell`) | 28% | a lazy to `offer_inline` |
    | net semantic action (`NetSemanticAction`) | 19% | a net into `drive_net_semantic_action` |
    | `follow_forwards` uncached end | 16% | a lazy to `offer_inline` |
    | `EvaluatorStepContext::root_value` | 14% | a poll result |
    | `regional_status_poll` ready value | 7% | a poll result |
    | `prepare_copy_source` | 7% | a net into a copy |

  - **A wrong first conclusion.** It was first recorded that every site
    carries its value across accesses, so no local fix applies. A review
    of each site's flow found roots that never leave one access or are
    never read; all were confirmed and removed:
    - `prepare_copy_source`: created and consumed within one access, so
      a prepared copy source now holds a net edge bound to the access
      scope (`bef15b7c`).
    - Inline children's completions: the route loop popped the child
      without reading its root; a child now marks itself completed and
      its parent reads the lazy's cache. The route's base still roots
      its result for its waiters.
    - The lazy checkpoint's ready path: its value was rooted, projected,
      cached and rooted again over three accesses; it is now cached in
      the access that computed it.
    - Tail forwards reuse the root of the lazy they forward to, and
      forward probes read edges (`afe08862`).
  - **Results** (3 runs each, against `8d2045e6`). A value root of
    anything but an integer also allocates a managed node holding the
    value, so allocations fell with roots:

    | Workload | M instr | Change | Roots | Allocations |
    | --- | ---: | ---: | ---: | ---: |
    | `countdown_800` | 2,011 to 1,838 | −8.6% | 194,280 to 100,385 | 180,860 to 134,699 |
    | `sum_800` | 2,249 to 2,069 | −8.0% | 224,635 to 119,561 | 192,059 to 145,900 |
    | `chain_800` | 4,904 to 4,644 | −5.3% | 389,162 to 257,741 | 394,697 to 338,280 |
    | `hello_elf` | 3,122 to 2,971 | −4.8% | 213,543 to 123,459 | 187,103 to 146,719 |
    | `list_map_2000` | 2,689 to 2,669 | −0.7% | 37,470 to 26,253 | 33,560 to 32,976 |

    CPU time fell similarly where measurable (`countdown_800` 431 to
    390 ms, `sum_800` 508 to 472 ms).
  - **Left.** The inline stack's lazies, the net semantic action, the
    base route's result and poll results carry values between accesses
    or out of the poll. Avoiding those roots needs a proof that no
    safepoint lies between the accesses: the design review in
    [Explicit Heap Holds](ExplicitHeapHolds_2026-10-09.md), which
    supersedes the hold-scoped edges first proposed here.

- **Root frames** (`perf-root-frames`), 2026-10-10. Investigated as a
  performance change separately from concurrent collection (maintainer).
  - **Lifetimes.** Tagging each root with the quantum that registered it:
    97 to 98% die within that quantum in every profiling workload, none in
    a later quantum, so a frame per machine would recover under 2%.
  - **Forwards without roots, adopted.** `follow_forwards` rooted every
    forward target it inspected inside one access; its common path now
    reads edges and roots only the lazy it returns. Root registrations
    fell 6 to 8% and instructions about 1%: about 1,400 instructions per
    root avoided.
  - **A frame per poll, not adopted** (experiment `perf-poll-root-frames`).
    Framing the hot transient value roots cut registrations and managed
    allocations by 10% each but left instructions unchanged; see
    Experiments.
  - **Conclusion.** For transient roots, avoiding the root pays and
    framing it does not; the remaining sites are
    `perf-transient-root-avoidance`. Roots bundled in coordinator state,
    such as demand records, were the maintainer's first thought and are
    worth a later look once the larger costs are gone.

- **Allocation and rooting path** (`perf-allocation-path`), 2026-10-10.
  - **Found** (holistic V1 and later profiles): every allocator
    acquisition locked the process-wide metadata registry, derived the run
    geometry again and locked the heap's data mutex to find its class,
    about 4% of `countdown_800`; and roots cost more than allocations.
  - **Class cache.** Each thread's heap cache remembers resolved classes by
    type, so a remembered type's allocator takes neither the registry, the
    geometry derivation nor the data mutex. Instructions fell 4.7%
    (`countdown_800`), 4.4% (`sum_800`), 4.2% (`chain_800`), 4.0%
    (`append_walk_800`), 3.2% (`hello_elf`) and 0.7% (`list_map_2000`).
  - **Root metadata** comes from the same cache, which removes one global
    mutex from root registration (instructions 0.3 to 0.4% lower).
  - **Roots, measured.** `countdown_800` registers 211,594 roots for
    182,557 allocations; registering is about 7% of its samples and the
    collector's root scan 1.6%. Within registration, about a third is
    validation under the data mutex, and much of the rest the
    `Arc<RootCell>` and registry growth. Most roots carry a value between
    two accesses of one poll (`forward_target`, `NetWhnfMachine::poll_in`,
    WHNF shell reduction, checkpoint polls, `prepare_copy_source`). Those
    move to [Explicit Heap Holds](ExplicitHeapHolds_2026-10-09.md)
    (`gc-hold-transient-roots`); roots held in machine state across
    polls, and `RootCell` pooling, move to `perf-root-frames`.

- **Bounded collection wait** (`gc-bounded-collection-wait`), 2026-10-09.
  - **Design** (maintainer): hold back new entrants at the quantum
    boundary, where a driver holds no lock. A lock audit had found why
    holding back other entries would deadlock: settlement holds the runtime
    mutation gate for writing while it constructs values, and promise
    publication takes that gate for reading inside a region.
  - **Already true, once each quantum holds one region.** A waiting
    `collect_full` sets the heap's request, and every driver services
    pressure at each quantum boundary after its region ends, so drivers
    join the collection there rather than start another quantum. The
    admission gate needed no draining mode.
  - **Fix.** Aggressive-GC verification's pressure input counted only new
    allocations, so a worker that allocated nothing in a quantum would not
    join; it now honors a request too.
  - **Evidence.** `a_quantum_boundary_joins_a_waiting_collection` checks
    the ordering: a driver's next quantum begins after the waiting
    collection. With joining disabled it failed 19 of 20 runs. Under two
    busy workers, a collection took 5 to 15 ms, against 0.5 to 15 s with
    joining disabled (debug build, five runs each).

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
