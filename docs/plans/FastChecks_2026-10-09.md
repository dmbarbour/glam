# Fast Checks — 2026-10-09

Status: active. Done: `test-fast-tier`. Open: `test-slow-tier`, deferred
until a test needs it.

## Purpose

Keep `scripts/check.sh fast` a quick inner loop. The maintainer asked to
identify slow tests, move them to a slower tier, and keep a faster version
in `fast` where that makes sense; and suggested isolated runtimes if tests
contended (2026-10-09).

## Method

Per-test times come from libtest's `--report-time` with `--format json`
(both unstable; `RUSTC_BOOTSTRAP=1` enables them on the pinned toolchain).
Times measured in a parallel run include contention, so the slowest tests
were also timed alone, with their CPU time.

## Done

- **Fast test suite** (`test-fast-tier`), 2026-10-09.
  - **Found.** Workspace tests took about 225 s in `fast`. The library
    suite took 138 s of wall time but only 402 s of CPU on eight cores,
    under three busy. Its slowest test took 76 s in the suite and 4.5 s
    alone.
  - **Contention.** Most tests shared one process-wide test runtime
    (`test_value_factory()`, `compiler_test_runtime()`, the `g_syntax`
    test assembler), so every test contended on one heap's mutex. The
    heap is never collected, since fixtures hold unrooted values, so it
    also grew for the whole run. These are now thread-local: libtest runs
    each test on its own thread, so each test gets its own runtime, and
    a test that spawns threads hands them its factory. A flag on the value
    domain replaces the process-wide registry of uncollected test
    runtimes, which every driver boundary had locked and searched. The
    library suite fell to 56 s of wall time and 300 s of CPU.
  - **Unoptimized builds.** The source audits parse the tree with `syn`,
    built without optimization, and the evaluator ran at opt-level 0.
    Dependencies now build at opt-level 3, which makes the audits four to
    five times faster, and the workspace at opt-level 1.
  - Results, workspace tests excluding doctests (doctests cached):

    | Configuration | Library suite | `executable_samples` | `hello_assemblies` | Rebuild after an edit |
    | --- | ---: | ---: | ---: | ---: |
    | Before | 138 s | 31 s | 25 s | 12 s |
    | Per-thread runtimes, optimized dependencies | 39 s | 27 s | 24 s | 12 s |
    | And opt-level 1 | 9 s | 2.7 s | 2.7 s | 15 s |

    All workspace tests now take about 20 s. After a one-file edit,
    `scripts/check.sh fast` takes 53 s and `all` 77 s; `full` took 565 s
    from a cold build. The slowest test takes 2.5 s, so nothing moved to a
    slower tier.

## Open

### Slow tier (`test-slow-tier`)

Not needed yet. When a test takes more than about 5 s at the dev profile,
split it: a smaller version stays in `fast`, and the full version is
`#[ignore = "slow; scripts/check.sh all runs it"]` with a `slow_` name
prefix. `all` runs them with
`cargo test --workspace --tests -- --ignored slow_`; `--ignored` keeps the
filter from matching ordinary tests, and the `full`-only fixtures
(`cursor_stress_`, `c5d_scale_`) do not match it. The aggressive-GC pass in
`full` runs them the same way. The nearest candidates are the source audits
(1 to 2.5 s each), and `sample_nets_are_polarized` and
`hello_assemblies`, which both assemble all 43 `hello` samples.
