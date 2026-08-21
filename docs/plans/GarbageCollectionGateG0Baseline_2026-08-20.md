# Garbage Collection Gate G0 Baseline — 2026-08-20

Status: established. This is comparison data and an admission record for C1,
not a performance threshold or a claim that the pre-GC runtime has no defects.

This baseline satisfies Gate G0 in
[`GarbageCollectionRoadmap_2026-08-19.md`](GarbageCollectionRoadmap_2026-08-19.md).
The broader ownership and semantic inventory is
[`GarbageCollectorOwnershipLedger_2026-08-20.md`](GarbageCollectorOwnershipLedger_2026-08-20.md).

## Source and Host

- Git revision: `60c6419e7f76c1f641b6b875b90bbd02512b12c1`.
- The measured production source matches that revision. The worktree also
  contained documentation and the I0 regression test; the added Rust code was
  test-only.
- Rust: `rustc 1.97.0 (2d8144b78 2026-07-07)`.
- Cargo: `cargo 1.97.0 (c980f4866 2026-06-30)`.
- Host: Linux `6.8.0-136-generic`, x86-64 GNU/Linux, eight logical processors.
- Container memory at capture was 15 GiB total with ordinary shared-container
  load. Results should therefore be compared by shape and scale, not treated
  as stable microbenchmark values.

## Semantic Baseline

The repeatable focused check is:

```sh
crates/glam-gc/scripts/check-g0-semantics.sh
```

| Required behavior | Latched regression |
| --- | --- |
| Runtime provenance and cross-runtime rejection | `public_value_factories_reject_foreign_composite_members` |
| Fulfilled lazy source release | `terminal_lazy_cache_releases_its_shared_source_after_active_snapshots` |
| Strict lazy-cycle diagnosis | `a_lazy_task_that_waits_on_itself_is_poisoned_as_a_cycle` |
| Retryable promise-cycle behavior | `promise_only_cycle_remains_blocked_without_poisoning_its_assignment` |
| Concurrent worker evaluation | `workers_force_sparks_and_poll_ready_reflection_tasks` |
| Shared interaction-net function state | `compiled_function_values_reuse_one_shared_interaction_net` |
| Runtime settlement and retained exit failures | `ready_settlement_publishes_exited_once_and_retains_exit_errors` |

The full ordinary suite also passed at capture. I0 records the wider tests for
public equality, promise completion, cross-session cycles, escaped values,
reflection snapshots, task owner closure, event delivery, and diagnostics.
Representation migration must preserve both the focused G0 set and that wider
matrix.

## Operational Method

Run:

```sh
crates/glam-gc/scripts/capture-g0-baseline.sh
```

The script builds `target/release/glam` with the repository release profile
(`opt-level = 3`). Each workload gets one warmup followed by seven measured
process invocations. The script verifies successful exit, empty stderr,
stable output length, and reports median/minimum/maximum wall time. Linux
`getrusage(RUSAGE_CHILDREN).ru_maxrss` supplies the maximum child RSS across
the warmup and measured runs. It includes process startup, configuration
loading, assembly, settlement, and output construction; it excludes the Cargo
build.

Override the repetition count with `GLAM_BASELINE_RUNS`. Timing and peak RSS
remain observations: collector changes do not fail tests merely by moving
these numbers.

The release binary was 7,544,776 bytes.

| Workload | Measured runs | Median ms | Min ms | Max ms | Peak RSS KiB | Stdout bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `hello_dict_w0` | 7 | 23.955 | 23.008 | 25.163 | 12,160 | 13 |
| `ordered_mixins_w0` | 7 | 27.559 | 27.255 | 28.316 | 12,160 | 13 |
| `direct_assembly_elf_w0` | 7 | 1,096.385 | 1,089.027 | 1,113.136 | 25,984 | 166 |
| `hello_dict_w4` | 7 | 26.308 | 24.686 | 27.674 | 12,160 | 13 |

The workloads cover a small dictionary assembly, ordered multi-module
composition, the current largest end-to-end direct-assembly example, and the
fixed overhead of a four-worker runtime on a small assembly.

## Known Pre-GC Observation

The first attempted worker-enabled direct-assembly capture exposed a
schedule-sensitive pre-GC defect: one seven-run `--workers 4` capture aborted
with a worker stack overflow. A direct follow-up reproduced exit 134 with one
worker, while two and four workers completed in that follow-up. The successful
zero-worker direct-assembly workload remains the operational baseline; actual
concurrent evaluation is latched by the focused worker regression above.

This observation is not converted into a performance row and G0 does not
declare it correct behavior. It is recorded so later GC work cannot silently
attribute the defect to managed pointers or claim to have introduced it. A
deterministic reproduction and evaluator fix remain separate runtime work.

## Gate Decision

G0 is satisfied because:

1. the required semantic contracts have named executable regressions;
2. the I0 ledger records the wider cyclic and runtime-owned graph;
3. representative release timing, RSS, output size, toolchain, and host data
   are reproducibly captured; and
4. the discovered worker-stack limitation is preserved as an explicit
   pre-existing observation rather than hidden or promoted into semantics.

C1A may now introduce its reviewed unsafe pointer/access boundary. G0 does not
authorize production Glam ownership migration; that remains gated by G1.
