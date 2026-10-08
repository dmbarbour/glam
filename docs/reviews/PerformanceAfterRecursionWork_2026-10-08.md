# Performance Review After the Recursion Work — 2026-10-08

Status: review complete. Two fixes landed with it (`perf-net-builder-wired-ports`
and a forward-following fast path); the other findings are roadmap steps.

Revision reviewed: `84bfd7d2`, after `eval-recursion-inline-forcing`,
`perf-admission-wakeups`, `perf-idle-wakeups` and
`eval-recursion-tail-forwarding`.

## Purpose and Scope

The recursion work removed the scheduler round trips that dominated every
profile. This review asks what dominates now: bottlenecks that the old costs
hid, opportunities that the new mechanisms open, and anything else found on
the way. Each finding is named as a step, so the
[performance roadmap](../plans/PerformanceRoadmap_2026-10-05.md) can adopt it
directly.

## Method

- **Sampling profiles.** `perf record -g` on a release build with line tables
  and frame pointers, without `glam-prof`, over `countdown_2000`,
  `hello_elf`, `list_map_1000` and the size-4,000 cases below. DWARF
  unwinding attributed allocator samples, since libc has no frame pointers.
- **Instruction counts.** `perf stat -e instructions:u` (repeat runs agree
  within 0.01%), with `task-clock` for CPU time, which includes kernel work.
  Wall times are not used: the machine is shared.
- **Scaling series.** Each workload family at sizes 500, 1,000, 2,000 and
  4,000. A ratio near 2 per doubling is linear; near 4 is quadratic.
- **One experiment.** A temporary build with the mimalloc global allocator.

Evidence labels: **Measured** (by the profiles or counts here), **Verified**
(code read and matches), **Fixed** (changed in this review, with before and
after counts).

## Baseline

Instruction counts at `84bfd7d2`, before this review's fixes:

| Family | n = 500 | 1,000 | 2,000 | 4,000 | Last ratio |
| --- | ---: | ---: | ---: | ---: | ---: |
| countdown | 1,537 M | 3,012 M | 5,961 M | 11,859 M | 1.99 |
| flat list literal (front end only) | 77 M | 105 M | 203 M | 566 M | 2.79 |
| `dict_lookup` | 265 M | 508 M | 1,167 M | 3,001 M | 2.57 |
| `list_map` | 489 M | 1,312 M | 4,259 M | 15,334 M | 3.60 |

The countdown is linear: the recursion work holds. The other three families
are superlinear, and at size 4,000 each has one dominant function.

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

The coordinator, which dominated before inline forcing, is now minor.

## Findings

### Net building scanned every wire per wire (`perf-net-builder-wired-ports`) — Fixed

`NetBuilder::try_wire` checked that neither port was wired by scanning all
existing wires, so a net with W wires cost O(W²) to build. Lowering a large
literal builds one large net: `try_wire` was 60% of the flat 4,000-element
list. **Fixed** in this review: the builder keeps a set of wired ports.

| Flat list literal | n = 500 | 1,000 | 2,000 | 4,000 |
| --- | ---: | ---: | ---: | ---: |
| Before | 77 M | 105 M | 203 M | 566 M |
| After | 71 M | 78 M | 92 M | 121 M |

Builders also run during evaluation, so evaluation workloads gain slightly.

### Collector admission costs about 640 instructions per access region (`perf-access-region-cost`) — Measured, Verified

Every outer access region enters and leaves the collector's mutator
admission. In `countdown_2000` that path is about 20% of samples (1.9 M
regions); in `hello_elf`, about 11% of self time. Per region, the path:
- looks up the thread's heap state in a thread-local hash map, clones an
  `Rc`, and creates and drops a `Weak` only to compare heap identity
  (`ThreadHeapEntry::prepare`);
- locks and unlocks the admission mutex on entry (`admit_outer_mutator`)
  and again on exit (`MutatorAdmission::drop`);
- upgrades a `Weak` on exit and adds four metrics with separate atomic
  read-modify-write loops (`record_thread_region_metrics`, 4.3% alone).

Candidate remedies, each measurable alone:
- accumulate region metrics in the thread-local state and publish them when
  read, or at quantum boundaries;
- compare heap identity by pointer, without creating a `Weak`;
- an atomic fast path for admission in the ordinary phase, keeping the
  mutex for collection elections and exclusive phases;
- fewer regions: the evaluator opens about 3.6 regions per reduction, many
  for one small check each (cache, checkpoint, forward and eligibility
  probes), which could share a region.

This is the largest broad opportunity: every workload pays it.

### Interface demand re-walks the whole chain on every poll (`perf-interface-demand-walk`) — Measured, Verified

`RuntimeNet::poll_interface_demand` walks from an interface along auxiliary
to principal ports until it finds an active pair. Each poll starts again at
the interface and records every visited node in a freshly allocated hash
set. Long chains make repeated polls quadratic: at `list_map_4000`, the node
lookups (`RuntimeNet::reference`, 20%), the set inserts (10%) and their
rehashing (8.4%) are almost 40% of samples. (The rehash appears under an
`EvaluationTaskId` symbol only because identical generic code was folded.)

Candidate remedies: detect cycles without allocating (Brent's algorithm),
and remember the frontier found for an interface, revalidated against the
net's topology revision, so a poll resumes where the last one stopped.

### Lowering recomputes free bindings for every nested code body (`perf-lowering-free-bindings`) — Measured, Verified

`ResolvedNetLowerer::lower_code_in` calls `body.free_bindings()` for each
nested lambda or lazy body, and each call walks that body's whole subtree.
A large dictionary literal nests about as deeply as it has entries, so
lowering is quadratic: `collect_free_bindings` is 40% of `dict_lookup_4000`.
The walk also recurses on the Rust stack to the literal's depth, which
`perf-pre-eval-stack-depth` already tracks.

Candidate remedies: derive each body's captures from the uses the lowerer
already records, or compute every body's free set in one bottom-up pass.
The first needs care with parameters that `ApplyLambda` binds inline.

### Taking the front of a mapped list is not constant time (`perf-list-front-walk`) — Measured

At `list_map_4000`, `List::pop_front_step_by` (9.4%), `ListNode` drops
(6.6%) and collecting `Vec<Value>` (5.5%) come together, reached from the
list comparison's builtin checkpoint. Slicing a value leaf is O(1)
(`SharedSlice`), so the cost is probably the list's shape: if
`std.list.map` yields a deep `Concat` spine, every pop walks it and rebuilds
the suffix. **Cause not established.** The step starts by checking the
shape and the comparison machine's traversal.

### Allocation is diffuse; a faster allocator is not a free win (`perf-root-registration`) — Measured

The allocator takes about 14% of samples, spread over root registration,
vector growth, finalization, persistent-map nodes and continuation frames.
The clearest single source is root registration: each registered root
allocates an `Arc<RootCell>`, and `countdown_400` registers about 100,000.

| mimalloc experiment | glibc M instr | mimalloc | glibc CPU ms | mimalloc |
| --- | ---: | ---: | ---: | ---: |
| `countdown_400` | 1,190 | 1,078 | 278 | 376 |
| `hello_elf` | 3,493 | 3,171 | 786 | 959 |
| `list_map_1000` | 1,283 | 1,074 | 272 | 275 |
| `minimal` | 34 | 30 | 22 | 67 |

mimalloc ran 9–16% fewer user-space instructions but used more CPU,
apparently in the kernel (memory reservation and page faults in this
container). It was not adopted. Remedies instead: register fewer transient
roots (code inside one access region can use edges), and pool `RootCell`s.

### Operator calls build and fully check a small net each time (`perf-runtime-net-attach`) — Measured

`attach_net_many_in` builds a fresh net for each core operator call and runs
the full template checks: validation, polarity and normalization. Net
building is 2.6% of the countdown, inclusive, and its polarity check
mostly comes from this caller; in `hello_elf`, net building and checking
take about 2.7% of self time. The net's shape is fixed
by the evaluator, not by user input. Candidate remedies: skip the polarity
check for evaluator-built shapes covered by tests, or reuse a template per
arity.

### Every port access is a hash lookup (`perf-net-node-storage`) — Measured

`RuntimeNet::reference` resolves a port through the `nodes` hash map. Apart
from the interface walk above, it takes 3–5% of samples in every workload,
from cursor claims, wiring, disconnection and frontier inspection. Dense
node storage indexed by `NodeId` would remove the hashing. It is a
representation change; it belongs with
[Value Representation Refinement](../plans/ValueRepresentationRefinement_2026-08-19.md)
or a net-runtime step.

### Forward following allocated on every poll — Fixed

`follow_forwards`, new in `eval-recursion-tail-forwarding`, allocated a
seen-set and a path on every call, although shortening keeps nearly every
chain to one forward. **Fixed:** a single forward to a lazy that does not
forward returns without allocating; longer chains take the general path.

### Collection and finalization (no step) — Measured

Two collections take 65 ms of `hello_elf`'s 794 ms total; in the countdown,
collection plus the finalization batch are about 6%. This scales with
garbage. Nothing stands out against the
[concurrent collection](../plans/ConcurrentGarbageCollection_2026-08-28.md)
plan, so no step is proposed.

### Profiling workloads hide superlinear costs (`perf-scaling-workloads`) — Measured

`scripts/profile.sh` runs each family at one size, where a quadratic looks
like a large constant. Three of the four quadratics above were invisible to
it. Adding a second size per family, or a scaling script that reports the
ratio per doubling, would expose them as they appear.

## What Is Solid

- The countdown is linear, and tail recursion runs in constant memory.
- Scheduler overhead is small, and single-threaded evaluation makes no
  futex calls.
- Instruction counts make before-and-after comparisons repeatable on a
  shared machine.

## Recommended Sequence

1. `perf-access-region-cost`: the largest cost every workload pays. Its four
   remedies can land and be measured separately.
2. The quadratics, which dominate at scale: `perf-interface-demand-walk`,
   `perf-lowering-free-bindings`, then `perf-list-front-walk` once its cause
   is known. `perf-scaling-workloads` first, so each fix shows its slope.
3. `perf-root-registration` and `perf-runtime-net-attach`: a few percent
   each.
4. `perf-net-node-storage`, with the representation work.
