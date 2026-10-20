# Reflection Performance — 2026-10-15

Status: active. The maintainer split it from the
[structural overheads plan](StructuralOverheads_2026-10-08.md) when that
plan closed on 2026-10-15, with its two reflection steps:
`perf-reflection-step-cost`, whose first changes landed on 2026-10-12 and
which was remeasured on 2026-10-15, and `perf-reflection-scanner`. Their
sections below moved here unchanged. This is the `perf-reflection-effects`
step of the [performance roadmap](PerformanceRoadmap_2026-10-05.md),
after the parser work and the representations (maintainer, 2026-10-15).

## Purpose

Reflection runs every module's boundary, the compiler's `refl.*` scanner
and users' `refl.*` tasks as chains of effect steps. A step costs hundreds
of thousands of instructions, and the scanner is a fixed cost of every
module, on the order of a third of an empty assembly. Make effect steps,
and the reflection programs the compiler generates, cheap.

Related: holistic review R1 (the reflection store's change log), R2 (the
machine cloning its active branch; mostly fixed on 2026-10-12) and R6
(captured continuations never released); and
[Pure Effect Access Fusion](PureEffectAccessFusion_2026-09-23.md)
(`perf-effect-chain-fusion`), whose premise has largely shrunk.

## Method

A step's cost is measured with a `refl.bench` chain of 200 effects
(`.r () =>> ... =>> .r ()`, or the same with `.heap.set ['k] 1`) against a
baseline that compiles the same chain under `meta.bench` without running
it: the difference over 200. Counters come from `GLAM_PROF`, and the
breakdown from Callgrind's self cost per function (`AgentContext.md`
"Profiling"). These workloads are not yet in `scripts/profile.sh`.

## Steps

### Reflection step cost (`perf-reflection-step-cost`)

Holistic R1, R2 and R6, still open: the reflection store's change log grows
without bound and each validation scans it; the reflection machine
deep-clones its active branch every step, so a `do` chain costs O(n²); and
captured continuations are never released. No profile has measured them
yet. The `do_chain` workload does not: its pure effects reduce in nets, and
its reflection-step count stays constant. A workload that runs a long chain
of reflection effects would.

Measured 2026-10-12 (maintainer: "a million CPU steps per reflection step
is rather absurd"). Workloads: a `refl.*` task running a straight chain of
200 effects (`.r () =>> ... =>> .r ()`, the same with `.heap.set ['k] 1`
or `.heap.get ['k]`), and loops of 400 iterations through a recursive
`loop n`. Instructions per step, compile cost excluded:
- `.r ()` about 540 K, `.heap.get` about 530 K, `.heap.set` about 780 K;
  the loops add about 360 K per iteration of evaluating
  `if n == 0 then ... else loop (n - 1)`, as pure `countdown` does.
- Per `.r ()` step (callgrind): lazy route tasks run by the pump about
  294 K, the reflection machine about 185 K, of which cloning the whole
  machine work, its branch and continuation stack each step (R2) about
  32 K. Within the lazy routes: claim nets about 57 K, `ClaimedTask`
  release 48 K, checkpoint production and installation about 61 K,
  following forwards 22 K.
- Per step (counters): 5.3 fresh work admissions, 10.9 work releases,
  26.8 coordinator `notify_all`s, 85 heap slots, 55 net work items. One
  `a =>> b` (`seq a (\x -> assert_unit x b)`) step forces about five
  lazies: the chain node's effect value, the decoded request, the `.r ()`
  value, the continuation applied to its result and the next effect;
  each admits its own route at about 100 K instructions, of which
  evaluating it is a small part.
- The recursive loop also collects: about 105 K per iteration in full
  collections and 80 K in finalization.
- First changes (2026-10-12): a step moves its machine work rather than
  cloning it unless a failed step's recovery could read it (a
  transaction, a retry checkpoint or a cut owning a transaction); and a
  reflection task run by the coordinator pumps a dependency it blocks on
  within its own poll, as a directly run task does, rather than returning
  to the scheduler for a round trip. Per step: `.r ()` -5%, `.heap.set`
  -8%; `minimal` -5.7%, `hello_elf` -1.2%. The lazies' own routes remain
  most of a step (about 296 K): net WHNF evaluation of each small lazy
  about 28 K, plus checkpoints and route claims. Dropping the reflection
  store at exit also costs about 37 K per step, but only because it holds
  the last reference to the managed heap, whose teardown scales with what
  the run allocated (about 85 slots per step); it is not R1's change log.
- Remeasured against a baseline that compiles the same chain without
  running it (`meta.bench` for `refl.bench`), after those changes: a
  `.r ()` step costs 519 K, a `.heap.set` 685 K. Of a `.r ()` step: lazy
  routes the task pumps 315 K (61%: net WHNF of the small lazies 135 K,
  WHNF checkpoints 73 K, claim nets 55 K), the reflection machine 149 K,
  validating new roots 41 K, allocation and free about 75 K.
- By each function's own cost, a `.r ()` step (518 K) divides as: net
  reduction 138 K (27%), `malloc`/`free` 112 K (22%), heap region entry
  and thread cache 58 K (11%), root registration and validation 57 K
  (11%), coordinator, pump and routes 51 K (10%), WHNF and checkpoints
  44 K (8.5%), the reflection machine's own code 7 K. Per step: 64 root
  registrations (value nodes most, then lazy cells, lazy checkpoints, net
  cells, list fronts), about 35 outer and 118 nested heap regions, 80
  allocated slots. Explicit heap holds would address the regions and the
  transient roots, about a quarter to a third of a step directly
  (maintainer, 2026-10-12: consider pushing them before the leftovers).

Remeasured 2026-10-15, after explicit heap holds, `root-evidence` and the
allocator class cache, with the same chains (`.r ()` and `.heap.set`, 200
steps against a baseline that compiles them without running them): a
`.r ()` step costs 376 K instructions (519 K on 2026-10-12, −28%), a
`.heap.set` step 510 K (685 K, −26%). Per `.r ()` step: 0.23 outer heap
regions (about 35 outer and 118 nested before), 64 root registrations and
80.5 allocations (unchanged), 20.3 coordinator `notify_all`s (26.8) and
53.5 net work items (55). By each function's own cost (callgrind), a step
divides as:

| Area | K instr | Share | 2026-10-12 |
| --- | ---: | ---: | ---: |
| Net reduction and building | 118 | 31% | 138 K |
| `malloc`, `free`, `memcpy` | 73 | 19% | 112 K |
| Coordinator, pump and lazy routes | 68 | 18% | 51 K |
| Value duplication, drops and the rest | 56 | 15% | |
| WHNF and checkpoints | 33 | 9% | 44 K |
| Root registration | 11 | 3% | 57 K |
| Heap entry, thread cache and allocation | 8 | 2% | 58 K |
| The reflection machine's own code | 8 | 2% | 7 K |

Explicit heap holds removed the regions as expected. Root registrations
did not fall, but `root-evidence` made each one cheap: a root made from a
view registers without validation in release builds, about 170
instructions. `RuntimeNet::reference`, which resolves every port through a
hash map, is 26 K of the net share (`perf-net-representation`). What
remains of a step is mostly representation (nets and allocation volume)
and the routes that evaluate each small lazy, not structural overhead.
R1 (the change log) and R6 (captured continuations) remain unmeasured.

Reflection task polls hold no heap region (`perf-quantum-region`), since
they call into their host between steps. Their pure steps, such as local
state, could hold one, provided no client callback runs inside it
(maintainer, 2026-10-09).

Next, from the 2026-10-15 breakdown:
- Add the chain workloads to `scripts/profile.sh`, so the suite tracks a
  step's cost.
- Lazy routes: each small lazy a step forces still runs as a route, with
  claims, checkpoints and releases (coordinator, pump and routes 18% of a
  step).
- Measure R6 with a long chain; R1 has its own step, `perf-change-log`.
- Much of the rest is representation: net node storage
  (`perf-net-representation`) and allocation volume
  (`perf-value-representation`).

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

Those figures are from 2026-10-10. On 2026-10-15 (`01ec2fbc`), `minimal`
costs 24.3 M instructions in all, with 626 reflection machine steps,
about 25 `.r ()` steps' worth: on the order of 9 M for the scanner and
the `defs-refl-state` check together, an estimate from the per-step cost,
not a measurement of the scanner alone.

The scanner's once-only check (a `scanned` mark), first deferred under
`defs-meta-refl` for its cost, landed with `defs-refl-state` on 2026-10-12
(structural overheads plan, `perf-definition-assertions`). It costs about
5.1 M of `minimal`'s instructions: a heap read about 1.0 M, a write 1.1 M,
and the `Equal`, `alt` and `cut` around them about 3 M. Cheaper effect
steps reduce it directly.

### The store's change log (`perf-change-log`)

Split from `perf-reflection-step-cost` on 2026-10-20, when the runtime API
began adding to the log (maintainer: "It probably hasn't been measured
properly"). Holistic R1, still open and unmeasured: the reflection store's
change log, `ReflectionStore::latest_changes`, maps each address ever
written to the revision that last wrote it, and nothing prunes it. Every
validation, a commit's or a blocked retry's, scans the whole map under the
runtime's transaction lock, locking the journal's observation index once
per entry, though only entries newer than the journal's snapshot can
conflict. Commit cost grows with the runtime's history, and total cost
roughly with its square.

The log gains an entry per distinct address: heap and volume paths, a
task's query (`queries/<id>`), virtual resources, and, since the runtime
API, each resolved promise's place, each reclaimed volume's root
(`api-death-notices`), and each machine's environment share. A
long-running runtime, such as `--interactive` or an IDE, accumulates them
without end. Ports keep their own conflict state, bounded by the ports.

- **Measure first.** Workloads whose history grows while their working set
  stays small: many short tasks, many resolved promises, many volumes
  allocated and dropped. Count instructions per commit against the log's
  size, with glam-prof counters (`scripts/profile.sh`).
- **Candidates** (holistic R1's recommendation):
  - A revision-ordered log, walked back only to the journal's snapshot
    revision, so a validation costs what changed since its snapshot.
  - Pruning by the low-water mark of live snapshots: entries at or below
    it cannot conflict with any live journal. That needs live snapshots
    counted, including blocked retries' and cuts' journals.
  - Or a cap: prune entries older than a horizon, and treat a journal
    whose snapshot predates it as conflicting, which retries it.
  - Lock the observation index once per validation, not once per entry.
- **Test:** commit cost independent of retired tasks, resolved promises
  and reclaimed volumes.

`api-death-notices` relies on this step: it keeps a reclaimed volume's
entries, since pruning them alone could hide a conflict from a journal
still running, which a low-water mark accounts for.

## Open questions (2026-10-15)

- **Order.** *Answered (maintainer, 2026-10-15):* after the parser work
  and the representations (`perf-value-representation`,
  `perf-net-representation`), whose costs now bury other signals in a
  step.
- **Pure Effect Access Fusion.** Fold it into `perf-reflection-step-cost`,
  or close it at its first step?
