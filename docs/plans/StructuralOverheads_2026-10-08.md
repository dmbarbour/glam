# Structural Overheads — 2026-10-08

Status: active, as the `perf-structural-overheads` step of the
[performance roadmap](PerformanceRoadmap_2026-10-05.md). Done:
`perf-admission-wakeups`, `perf-idle-wakeups`, `perf-fast-id-hashing`,
`perf-net-builder-wired-ports`, `perf-scaling-workloads` and
`perf-list-front-walk`. Next: `perf-list-index-descent`, once its design is
agreed.

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

### Access-region cost (`perf-access-region-cost`)

Every outer access region enters and leaves the collector's mutator
admission: about 640 instructions per region, 20% of `countdown_2000`
(1.9 M regions) and 11% of `hello_elf`'s self time. Per region, the path:
- looks up the thread's heap state in a thread-local hash map, clones an
  `Rc`, and creates and drops a `Weak` only to compare heap identity
  (`ThreadHeapEntry::prepare`);
- locks and unlocks the admission mutex on entry (`admit_outer_mutator`)
  and again on exit (`MutatorAdmission::drop`);
- upgrades a `Weak` on exit and adds four metrics with separate atomic
  read-modify-write loops (`record_thread_region_metrics`, 4.3% alone).

Candidate remedies, each measurable alone:
- accumulate region metrics in the thread-local state, and publish them
  when read or at quantum boundaries;
- compare heap identity by pointer, without creating a `Weak`;
- an atomic fast path for admission in the ordinary phase, keeping the
  mutex for collection elections and exclusive phases;
- fewer regions: the evaluator opens about 3.6 regions per reduction, many
  for one small check each (cache, checkpoint, forward and eligibility
  probes), which could share a region.

This is the largest broad cost: every workload pays it.

### Index descent in list observers (`perf-list-index-descent`)

Proposed 2026-10-08; the design needs discussion before it starts. It
finishes the `list_sum` cubic that `perf-list-front-walk` cut to
superlinear.

The list observation machine's `len`, `at`, `split`, `slice` and
`split_end` advance one item per builtin step, restarting the front (or
back) projection at each item. Pops are now O(1), but `len` still takes n
steps, so a loop that tests `len` each iteration is quadratic: at 1,600
items `list_sum` costs 11 G instructions, and its reductions grow with
exponent 1.70. `split` and `slice` also copy the taken items into a new
value leaf, even from one large leaf whose slice would be O(1).

Proposal, in the maintainer's direction that observers shape lists rather
than relying on construction:
- **Cached lengths.** A `Concat` node records its length when both
  children's lengths are known, that is, when no lazy chunk lies beneath
  it. `known_len` becomes O(1) and stops recursing.
- **Descent by length.** A non-forcing step descends to an index, skipping
  every subtree of known length whole, and stops at the index or at the
  first lazy chunk before it, which the caller forces and resumes after.
  Each observer then takes one step per lazy chunk it crosses, plus one,
  rather than one per item.
- **Balanced taken part.** What `split` or `slice` takes is strict, since
  every chunk before the cut was forced. It comes back as a finger tree of
  the leaves it spans, sharing them rather than copying items, so later
  index operations on it are logarithmic. What is left keeps its sharing
  and lazy chunks, shaped toward the cut as a pop shapes it.

Notes for the discussion:
- Building the taken part's finger tree costs one push per leaf it spans,
  at most its length, where the current copy costs one per item.
- The cache costs no memory: a `ListNode` is 40 bytes, sized by
  its byte and value leaves (32 bytes each), and a `Concat` uses 16, so a
  cached length fits. Each concatenation adds two lengths.
- `at` returns no list, so repeated `at` on one unbalanced list pays the
  descent each time. Balancing that persists would mean memoizing a
  balanced form in the node, or lists that are finger trees throughout,
  with lazy chunks as finger-tree elements. The second belongs to the
  value representation plan's V3 list checkpoint, and would also fix pops
  that alternate between the two ends (see `perf-list-front-walk` under
  Done).

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

After `perf-list-front-walk`, `list_map` is still quadratic (exponent 1.92
from 2,000 to 8,000 items) at half its former cost. At `list_map_8000` the
largest self costs are `memset` (7%), `NetWhnfMachine::poll_in` (5.8%),
`Topology::check` (4.5%) and `RuntimeNet::wire` (4.3%). Which of them
grows has not been measured yet; profile two sizes before choosing a
remedy.

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
  rest is `len` counting item by item, `perf-list-index-descent`.
  `list_map` remains quadratic (1.92 at 2,000 to 8,000); see
  `perf-interface-demand-walk`.

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
  (`perf-list-front-walk`, `perf-interface-demand-walk`), `do_chain` and
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
