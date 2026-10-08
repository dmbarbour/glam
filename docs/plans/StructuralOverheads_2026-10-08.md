# Structural Overheads — 2026-10-08

Status: active, as the `perf-structural-overheads` step of the
[performance roadmap](PerformanceRoadmap_2026-10-05.md). Done:
`perf-admission-wakeups`, `perf-idle-wakeups`, `perf-fast-id-hashing`,
`perf-net-builder-wired-ports` and `perf-scaling-workloads`. Next:
`perf-access-region-cost`.

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

### Front of a list (`perf-list-front-walk`)

At `list_map_4000`, `List::pop_front_step_by` (9.4%), `ListNode` drops
(6.6%) and collecting `Vec<Value>` (5.5%) come together, reached from the
list comparison's builtin checkpoint. Slicing a value leaf is O(1)
(`SharedSlice`), so the cost is the list's shape.

**Likely cause, from reading the code:** the core list operator builds a
literal as a left-deep spine of one-element concatenations
(`eval/operator.rs`, `CoreOperator::List`). Popping the front walks the
whole spine, and `join_logical_suffix` rebuilds it, one `Concat` node per
remaining item, so each pop costs O(n). The same operator copies every
supplied operand at each partial application, which is O(n²) to build a
literal. Both are holistic E8 and V3, listed in its P2 under "obvious
algorithmic defects"; the recommendation there is to build the literal
with `List::from_values` from the operand vector. A spine that a program
builds by appending stays slow to pop; that belongs to the value
representation's ropes.

**Cubic in the `list_sum` workload.** A loop of `len`, `head` and `tail`
over a literal costs 272 G instructions at 1,600 elements (exponent 2.95),
and its builtin steps grow superlinearly too. The list observation
machine's `len` (like `at`, `split` and `slice`) walks one item per step,
and each step pops the front of the spine in O(n). So each `len` is O(n²),
and the loop calls it n times. A flat literal makes each pop O(1), which
leaves `len` linear per call and the loop quadratic; counting a strict
leaf's items at once (`known_len` when no chunk is deferred) makes it
linear.

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
