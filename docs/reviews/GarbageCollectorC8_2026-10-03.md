# Garbage Collector C8 Tuning and Final Audit — 2026-10-03

Status: in progress; C8A and C8B are complete.

This review records tuning evidence and the final isolated-collector audit for
[`GarbageCollectorImplementation_2026-08-19.md`](../plans/GarbageCollectorImplementation_2026-08-19.md).
Measurements are operational observations, not semantic contracts or test
thresholds.

## Method

Run:

```sh
crates/glam-gc/scripts/capture-c8-measurements.sh
```

The release-mode harness writes versioned JSON Lines and records its revision,
compiler, host, and available parallelism. Each workload checks its semantic
outcome before emitting a record. The committed harness is authoritative; the
numbers below are one dated observation from the development container and
should be recaptured before making later tuning decisions.

Observed environment:

- revision before the C8B.2a commit: `3a8b6e54`;
- Rust 1.98.1, LLVM 22.1.8;
- x86-64 Linux container, eight available logical workers;
- release profile with debug assertions disabled.

## C8A — reporting boundary

`CollectionReport` is one atomically published successful-attempt summary.
Terminal teardown still has no invented observer. Counts describe collector
work and durations are process-local `Instant` measurements. `HeapMetrics`
remains a coherent operational snapshot rather than semantic state.

A forced finalizer schedule confirmed an important existing invariant: an
actively finalized detached run remains in the durable finalization map until
commit. The current utilization scan therefore continues to include every
assigned run; no public completeness flag or parallel record was needed.

Run size, chunk size, worker class-cache width, and collection-pressure
thresholds remain private build-time or per-heap implementation policy. C8
does not add variable-size runs.

## C8B.2a — geometry and assigned-run scan

The fixed 64 KiB run produced these representative geometries:

| requested stride | slots | allocation + lease + mark bitmap bytes | alignment padding | tail slack |
|---:|---:|---:|---:|---:|
| 8 | 7,920 | 2,000 | 112 | 0 |
| 16 | 4,024 | 1,016 | 72 | 0 |
| 24 | 2,698 | 696 | 8 | 16 |
| 32 | 2,028 | 520 | 56 | 0 |
| 64 | 1,018 | 264 | 56 | 0 |
| 128 | 510 | 136 | 56 | 0 |
| 256 | 255 | 72 | 120 | 0 |
| 1,024 | 63 | 24 | 40 | 896 |
| 4,096 | 15 | 24 | 40 | 3,968 |

Ten thousand `Heap::metrics()` scans over 64 assigned 8-byte runs containing
500,000 allocations took 191.8 ms in aggregate, about 19.2 us per scan. This
is already a cold, explicit telemetry operation. The evidence does not justify
turning the spare run-header word into a live-slot counter, adding allocation-
path contention, or complicating future parallel marking.

Disposition: retain the current geometry and cache-local allocation-word scan.
Use these results as input to the later value-representation layout policy,
not as a public collector configuration.

## C8B.2b — finalization-state structures

The exceptional-state workload allocated 4,096 finalizers at a requested
1,024-byte stride, distributing them across 66 runs. It forced a destructor
panic on the penultimate obligation. This left exactly one pending slot in one
durable run while retaining the hash tables' grown capacity, then measured the
normal lookup and retry paths:

- the failed attempt terminally retired 4,095 obligations in about 0.76 ms;
- 100,000 non-mutating rootability checks against the sparse pending identity
  took about 14.1 ms, or 141 ns per checked lookup including mutex admission;
- the retry scanned the retained run map, conservatively marked and finalized
  the single pending slot, and reclaimed its run in about 20 us total.

The ordinary dense workload finalized 100,000 small objects across 13 runs in
about 13.3 ms. These results do not justify a specialized hasher, a dense or
ordered replacement for the exceptional sparse maps, or another persistent
dispatch index. Pending masks remain authoritative and the ephemeral dispatch
snapshot remains the only per-attempt work vector.

## C8B.3 — paged array tracing

`TraceWork` is currently two pointers, or 16 bytes on the measured x86-64
target. The representative 100,000-edge flat fanout reached a length of
100,000 and a `Vec` capacity of 131,072, reserving 2 MiB. The isolated scale
fixture's one-million-edge fanout reached a capacity of 1,048,576, reserving
16 MiB. In contrast, the million-node deep-chain fixture retained a constant-
depth object stack and verified the nonrecursive traversal goal.

The wide peak is material enough to retain as tuning evidence, but not enough
to justify a new unsafe API today. The repository has no production managed
representation containing a flat `Vec<Gc<_>>`; current high-level containers
trace bounded or tree-shaped ownership, and the flat fanout exists only in GC
fixtures. A stable-range continuation would add lifetime and stability
obligations before value-representation work has selected the contiguous
containers which could honor them.

Disposition: retain `Vec<TraceWork>` and do not prototype or adopt paged range
tracing in C8. Reopen the additive `Visitor` range operation only alongside a
real Glam-owned contiguous managed container and fresh measurements. Do not
turn ordinary bounded `Trace` implementations into resumable cursors.

## Remaining work

- C8C: unsafe, documentation, and extended verification closeout.
