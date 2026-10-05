# Holistic Architecture Review Before Performance Work — 2026-10-03

Status: review complete. No code changed. This records findings and a
recommended order of work. It does not authorize a plan.

Revision reviewed: `7452e13c` (clean `main`).

## Purpose and Scope

The next phase will be performance-focused. The likely candidates are
[Value Representation Refinement](../plans/ValueRepresentationRefinement_2026-08-19.md)
(VRR), [Pure Effect Access Fusion](../plans/PureEffectAccessFusion_2026-09-23.md)
(PEAF), [Concurrent GC](../plans/ConcurrentGarbageCollection_2026-08-28.md),
and [Public Resumable Evaluation](../plans/PublicResumableEvaluation_2026-09-23.md).
This review asks whether the repository is solid enough to start that work.
It covers four areas:

- architecture and dependency direction;
- separation of concerns;
- refactoring opportunities, including residue from the GC-integration and
  resumable-WHNF transitions;
- documentation and verification gaps.

Coverage: all of `src/`, the `glam-gc` crate's public boundary (not its
unsafe internals, which the
[C8 audit](GarbageCollectorC8_2026-10-03.md) just covered), `tests/`,
`scripts/`, `samples/`, and `docs/`.

## Method

Eight independent read-only reviews ran in parallel:

1. core values and managed layer;
2. evaluation scheduler and runtime;
3. evaluator semantic machines;
4. interaction nets;
5. reflection;
6. built-in front end;
7. embedding API, diagnostics, and CLI;
8. documentation and verification infrastructure.

The lead reviewer collected cross-cutting metrics, ran the routine checks,
and reproduced or re-read the most consequential claims.

Each finding carries an evidence label:

- **Reproduced**: demonstrated by executing code at this revision.
- **Verified**: the lead reviewer re-read the cited code and it matches the
  claim.
- **Reported**: a subsystem reviewer cited the code, but nobody re-checked
  it independently. Treat file:line references as starting points.

Severity reflects risk to the performance phase:

- **High**: undermines correctness, verification, or measurement.
- **Medium**: structural debt worth fixing before or during the phase.
- **Low**: cleanup.

The `glam-gc` crate uses `AtomicU64::try_update`
(`crates/glam-gc/src/heap.rs:40`), which needs a newer toolchain than
rustc 1.91 (this host). The C8 review recorded Rust 1.98.1 in the dev
container. To obtain test evidence without touching the repository or
toolchain, the checks ran on an exported copy with that single call replaced
by its stable equivalent `fetch_update`.

## Verdict

**Not yet rock solid. Close, and the remaining work is well defined.**

The ownership and collector boundary is principled and largely
type-enforced:

- `Gc` is move-only;
- access regions are higher-ranked;
- managed families require unsafe admission records;
- destruction is passive;
- mutation gateways already mark future barrier sites.

Dependency direction is clean where it matters most:

- the generic interaction-net layer imports nothing from Glam semantics;
- evaluation never imports reflection;
- core and evaluation never import the front end.

Wake protocols and terminal publication are carefully designed, and their
race windows have forced-order tests. Current-state docs are mechanically
clean: no broken links or stale paths in current docs, and 3 stale
identifiers out of 166.

Five things stand between the current state and a trustworthy performance
phase:

1. **The routine checks are red and incomplete.**
   - `cargo test -q` fails at HEAD because a unit test asserts on wording in
     a plan document that the C8C closeout edited.
   - The routine checks never run `glam-gc`'s own tests or the aggressive-GC
     mode.
   - No toolchain is pinned and there is no CI.
2. **User input can crash the process.** Two reproduced panics: a valid
   interaction net and a malformed braced `do`. Scheduler claims have no
   unwind containment.
3. **There is no performance harness.** The deferred perf plans assume
   measurement infrastructure that does not exist, and the measurement hooks
   that do exist are unused.
4. **Transition scaffolding taxes every change:**
   - about 13k lines of source-scanning inventories with exact counts and
     opaque fingerprints;
   - tests coupled to plan prose;
   - 67 `#[allow(dead_code)]`, of which 39 suppress nothing;
   - milestone-named tests and comments.

   Every VRR commit would trip this machinery.
5. **Structural costs would dominate any profile.** The suspected
   magnitudes are by reading, not measurement:
   - every uncached lazy becomes coordinator work;
   - machines yield to the scheduler after ordinary phase transitions;
   - process-global and per-heap locks sit on every managed allocation;
   - the reflection machine deep-clones its branch every step;
   - list literals are quadratic (reproduced).

   These overheads would mask what VRR and PEAF are trying to measure. They
   should be counted first, then reduced in an order that keeps
   representation measurements meaningful.

## Baseline Evidence

### Routine checks

| Check | Result |
| --- | --- |
| `cargo fmt --check` | pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | fails to compile on rustc 1.91 (`try_update`). With the patch, clippy 1.91 reports 2 `only_used_in_recursion` lints at `src/core.rs:2659`, `2718`. These are version drift; both functions carry stale `dead_code` allows. |
| `cargo test -q`, lib | **1910 passed, 1 failed, 2 ignored** (209 s, debug). The failure is `api::runtime::gc_activity_inventory::runtime_gc_policy_review_links_selected_plan_and_completion_gate`, which asserts `roadmap.contains("not enabled. Collector stress")`. Commit `9e8d86fe` changed that sentence. **Reproduced.** |
| `cargo test`, bin and integration (skipped by cargo after the lib failure; run separately) | bin 81, `cli` 49, `effect_embedding` 1, `executable_samples` 5, `hello_assemblies` 1, `invalid_samples` 2, `macro_protocols` 5, `public_api` 46, `sample_sources` 5, doctests 2: all pass |
| `cargo test -p glam-gc` (not part of the routine checks) | 226 passed, 2 ignored |
| `scripts/check-interaction-net-profiling.sh` | fails on this host: `rg: command not found`, an undeclared dependency |

### Observed timings

These are observations from a development host, not contracts.

| Workload | debug | release |
| --- | ---: | ---: |
| `samples/executable/hello_x86_64_linux/hello.g` with `direct_assembly.g` config (45-line source, 750-line config, ELF output) | 52.2 s, 150 MB RSS | **3.5 s, 142 MB RSS** |
| `samples/hello/hello_text.g`, `unit_tests.g` config | 0.71 s | 0.06 s |
| Integration binaries `executable_samples` / `hello_assemblies` | 56.7 s / 40.4 s | — |
| One `g_syntax` unit test (`dictionary_tag_and_tuple_patterns_match_or_fall_through`) | over 60 s | — |

List-literal scaling in release (`xs = [0, 1, ..., N-1]`, then
`std.list.len xs`). **Reproduced.**

| N | total | `--parse` only | compile without demanding `xs` |
| ---: | ---: | ---: | ---: |
| 1,000 | 0.22 s | — | — |
| 4,000 | 2.50 s | 0.04 s | — |
| 16,000 | 43.6 s | 0.17 s | 0.97 s |
| 64,000 | over 300 s (killed) | — | — |

Parsing is linear. The cost is evaluation of the literal: the list
operator copies every supplied operand on each partial application, then
folds singleton `List::concat`s into an N-deep left spine
(`src/eval/operator.rs:235-251`, verified). See V3 and E8.

### Size and shape

| Metric | Value |
| --- | --- |
| Production lines in `src/` (inline tests excluded) | about 98.8k |
| Test lines in `src/` (test files plus inline test modules) | about 102.8k, plus 4.3k in `tests/` and 18.6k in the `glam-gc` crate |
| Largest production modules by lines (prod / test) | `g_syntax` 20.8k / 14.8k; `eval` 20.1k / 26.4k; `evaluation` 14.0k / 15.8k; `reflection` 11.1k / 11.3k; `core` 6.9k / 14.6k |
| Source-scanning `*_inventory.rs` modules | 18 modules, 12,954 lines, 101 tests, 9 FNV fingerprint latches |
| `include_str!` sites in tests | 117, including 16 that read `docs/plans` or `docs/reviews`, plus 2 `fs::read_to_string` doc reads |
| Production functions over 150 lines | 33. Largest: `reflection/machine.rs:1207 control_step` 584, `eval/annotation_machine.rs:169 poll_in` 549, `eval/value.rs:932 poll` 511, `g_syntax/parser/expression.rs:173 syntax_expr_parser` 507 |
| Milestone IDs (W*, I*, C*, D.2*, PNC*, G*) in production code | 112 in `src/`, 28 in the `glam-gc` crate; 443 more in in-crate tests |
| Commits since 2026-08-15 | 937. Net lines added: production 57.8k, tests 62.0k, docs 46.6k |

---

## Cross-Cutting Findings

### X1 — High — Routine verification is red, and tests depend on history docs

**Reproduced / Verified.**

**Evidence**

- The lib test failure is shown above. The C8C closeout ("Complete C8C
  verification and closeout", `9e8d86fe`) reworded
  `docs/plans/GarbageCollectionRoadmap_2026-08-19.md`. That broke
  `src/api/runtime/gc_activity_inventory.rs:286-299`.
- 18 `include_str!`/`read_to_string` sites across
  `gc_activity_inventory.rs`, `containment_inventory.rs`, and
  `gate_g2_inventory.rs` read plans and reviews. Some assertions match
  hard-wrapped prose; some are negative (`!plan.contains(..)`).
- [`docs/plans/README.md`](../plans/README.md) says completed plans may be
  deleted. Deleting `GarbageCollectorIntegration_2026-08-19.md` would stop
  the lib test target compiling, and reflowing a paragraph fails a test.
  This inverts [`AgentContext.md`](../AgentContext.md)'s rule that plans are
  history, not authority.

**Recommendation**

- Delete these tests, or move the invariants they protect into code-level
  gates or current architecture docs.
- Add a check that `src/` never references `docs/plans` or `docs/reviews`.
- Treat a red routine suite as blocking for any "complete" commit.

*Resolved 2026-10-03* in `74088606`.
- **Tests.** Five doc-prose tests were retired, including the red
  `runtime_gc_policy_review_links_selected_plan_and_completion_gate`. Four
  mixed tests kept their code invariants and lost their plan and review
  assertions.
- **Guard.** `tests/source_doc_coupling.rs` fails if any file under `src/`
  references `docs/plans` or `docs/reviews`. It scans `src/` only.
- **Gate.** `scripts/check.sh` is now the pre-commit gate; see X2.

### X2 — High — The routine checks cover less than they appear to

**Verified.**

**Evidence**

- **`glam-gc` is not covered.** `Cargo.toml` is a non-virtual workspace, so
  `cargo test` and `cargo clippy --all-targets` at the root cover only the
  `glam` package. `glam-gc`'s tests, Loom models, unsafe audit, and codegen
  latch run only via `crates/glam-gc/scripts/check.sh`. Neither `AGENTS.md`
  nor `AgentContext.md` mentions that script.
- **The aggressive-GC mode is compiled but never run.**
  `aggressive-gc-verification` is compiled by `clippy --all-features` and not
  executed. The last full aggressive workspace run is recorded at `d2d27211`
  ([closure review](GarbageCollectorAggressiveVerificationClosure_2026-10-01.md));
  about 30 commits touching `src/` or `crates/` have landed since.
  - **Finding 2026-10-03:** executing the P0 aggressive pass (sequence step 1)
    confirmed the risk — the aggressive suite is broadly red and hangs partway.
    Bisected to `46dc1487` (I12A explicit GC maintenance), which panics on a
    supported value-domain-outlives-runtime lifecycle in a test/verification-only
    collecting-entry wrapper (production unaffected). Remediation tracked in
    [`GarbageCollectorAggressiveVerificationRegression_2026-10-03.md`](../plans/GarbageCollectorAggressiveVerificationRegression_2026-10-03.md).
- **No pinned toolchain or CI.** There is no `rust-toolchain.toml`, no
  `rust-version`, and no CI config. The dev container installs unpinned
  stable Rust and no ripgrep. This review hit both problems: the build
  failed on rustc 1.91, and the clippy lint set differs between versions.
- **The atomic-update API migration is half done.** This narrows the set of
  working toolchains.
  - `glam-gc` uses `AtomicU64::try_update`, stabilized in Rust 1.95.
  - `glam` still calls `fetch_update` at `runtime.rs:36`, `runtime.rs:932`,
    and `reflection/store.rs:114`. Rust 1.99 deprecates that method, so
    `clippy -D warnings` fails on 1.99 (observed by the parallel review).
  - The current code is therefore fully clean only on 1.95–1.98, and only
    1.98.1 is recorded as verified.
- **Script defects.**
  - `scripts/check-interaction-net-profiling.sh` and
    `crates/glam-gc/scripts/audit-unsafe.sh` require `rg`.
  - `crates/glam-gc/scripts/check-g0-semantics.sh:8` filters on
    `public_value_factories_reject_foreign_composite_members`, a test that no
    longer exists, so `cargo test <filter>` passes vacuously.
- **Ignored scale fixtures** (cursor stress, `glam-gc` scale) are scripted
  but not part of any routine or periodic check.

**Recommendation**

- Pin the toolchain, including a dated nightly for Miri and sanitizers, and
  declare `rust-version`.
- Add a root `scripts/check.sh` that runs the routine checks plus
  `cargo test --workspace`, and runs `glam-gc`'s `check.sh` when `crates/`
  changes.
- Add `scripts/check-full.sh` for the aggressive workspace suite, cursor
  stress, `glam-gc` scale, and a docs link check.
- Give `AGENTS.md` only the script names, and let `AgentContext.md` own the
  explanation.
- Make scripts fail when a filter matches zero tests.
- Replace `rg` in scripts with `grep -E`, or declare it as a dependency.
- Add minimal CI: `check.sh` per push, `check-full.sh` nightly.

*Resolved 2026-10-04, except CI and the nightly tools.*
- **Workspace gate.** `scripts/check.sh` (`5e68163d`) has cumulative levels.
  `fast` runs workspace fmt, Clippy, and tests. `all` adds `glam-gc`'s own
  suite, the G0 semantic regressions, and the profiling fixtures. `full` adds
  the aggressive-GC workspace pass, cursor stress, and the `glam-gc` scale
  proofs. `AGENTS.md` names the script; `AgentContext.md` explains the levels.
- **Aggressive GC.** The regression found on 2026-10-03 was fixed in
  `df60695d`: verification now collects at the stable-pump maintenance
  boundary, and `scripts/check.sh full` passes.
- **Toolchain.** `4c849c19` pins Rust 1.99.0 in `rust-toolchain.toml`,
  declares `rust-version`, and finishes the `fetch_update` → `try_update`
  migration.
- **Scripts.** `2ff5bc3e` replaces `rg` with `grep`, restores the G0 script's
  renamed test filter, and makes that script fail on a filter that matches no
  test.
- **Not done.** There is no CI, by decision; `scripts/check.sh` is the local
  gate. Miri and the sanitizers run only for `glam-gc` and only when a nightly
  is installed. The production-runtime Miri and sanitizer matrix is not
  scripted, and no docs link check was added.

### X3 — High — There is no performance harness, and the existing hooks are unused

**Verified.**

**Evidence**

- **No benchmarks.** There is no `benches/`, no criterion, divan, or iai
  dependency, and no stats flag on the binary.
- **The only end-to-end harness is stale.** It is
  `crates/glam-gc/scripts/capture-g0-baseline.sh`, last recorded on
  2026-08-20, before GC integration and resumable WHNF. Three of its four
  workloads are dominated by process startup.
- **The working perf workload is a test fixture.** Earlier work measured a
  test fixture (`tests/executable_samples.rs`) with Callgrind and DHAT. That
  recipe exists only in dated reviews
  ([W6G4](ResumableWhnfW6G4_2026-09-23.md),
  [holistic](ResumableWhnfHolistic_2026-09-28.md)).
- **Test builds are not production builds.** The dev-dependency enables
  `glam-gc/deterministic-test-hooks`, so every test binary, even with
  `--release`, carries probe branches on edge-transition paths. Timings
  from test binaries do not represent production.
- **Hooks exist but nothing reads them** (Reported):
  - `HeapMetrics` has no consumer in `src/`.
  - `PersistentEdgeVisitStats` and `LogicalListVisitStats` are computed and
    discarded.
  - The CLI discards the `RuntimeMaintenanceReport` (`src/bin/glam/batch.rs`
    settle loops).
  - Interaction-net profiling counts rewrites only, has no timing, and has
    no CLI dump.
- **The plans assume a harness.** VRR "V0" and PEAF "PEAF1" both presume one.
- **GC never runs during a CLI assembly.** Collection is promoted only by
  `pump_until_stable`, and the binary evaluates and writes `asm.result`
  before its first settle loop (`src/bin/glam/batch.rs:150-182`, verified).
  Every CLI measurement therefore observes a heap that never collects: peak
  memory equals total allocation. This is a documented `NoAuto` property,
  but nothing states its consequence for the CLI or for benchmarking.

**Recommendation**

Before the first optimization, add `scripts/perf/` containing:

- **A fixed corpus:**
  - direct-assembly ELF;
  - list-literal scaling;
  - an effect-heavy `do`/cut workload;
  - a net-heavy workload (fan duplication, deep cursors, N-wire netlist);
  - a lazy-heavy workload;
  - a spark workload with workers 0 and N;
  - an allocation-heavy workload that crosses a maintenance boundary.
- **A release-binary runner** (never a test binary) emitting JSONL in the
  style of `c8_measurements`: revision, rustc version, host, wall time, and
  peak RSS.
- **Deterministic counters behind one `runtime-profiling` feature:**
  - coordinator lock acquisitions and transitions per kind;
  - access-region entries;
  - allocations per managed family;
  - root registrations;
  - yields by cause;
  - net reductions;
  - logical versus physical trace visits.
- **Gating policy:** counters may gate regressions; wall time stays
  observational.
- A `[profile.profiling]` that inherits release and adds line tables.
- A binary-only `GLAM_STATS=1` that prints per-phase timing and
  maintenance reports.

Then capture and check in a post-G4 baseline in one owning doc.

### X4 — High — User input can crash the process, and panics are not contained

**Reproduced (crashes) / Verified (containment).**

**Evidence**

- **N1, net crash.** This 10-line valid program panics with "interaction
  auxiliary port must be wired" (`interaction_net/runtime/graph.rs:12`),
  poisons the net mutex, and aborts the process:

  ```text
  language g0
  import 'std
  redex = interaction_net do
    .bind -> app
    .bind -> id
    .data "Hello, World!" -> arg
    .wire (list.head app) (list.head id)
    .wire (list.head (list.tail id)) (list.head (list.tail (list.tail id)))
    .wire (list.head (list.tail app)) (list.head arg)
    .r (list.head (list.tail (list.tail app)))
  asm.result = net_arity 0 redex
  ```

  A control program applying a non-looped function returns normally.
- **F1, parser crash.** This malformed source panics `--parse` with exit
  101 at `g_syntax/parser/do_expr.rs:511`. The single-line form `;;` is
  correctly diagnosed.

  ```text
  language g0
  import 'std
  x = do { .r ();
    ; .r () }
  ```

- **No panic containment.**
  - No `catch_unwind` exists in production `src/evaluation` or
    `src/reflection`.
  - There is no `Drop` guard on `ClaimedTaskWork`, `ClaimedClientDemand`,
    `ClaimedSparkWork`, or the pump's `ClaimedTask`.
  - Per the scheduler review (Reported), a panic inside a claimed poll on a
    worker leaves its record `Running`. Readiness then stays `Busy`, and the
    worker exits without replacement.
  - Interaction-net claims, output delivery, and collector maintenance do
    contain panics, so policy is inconsistent.

**Recommendation**

- Fix N1 and F1 with regressions first. See N1 and F1 below.
- Decide the runtime's panic policy:
  - **contain:** a RAII claim guard terminalizes the record as failed and
    publishes the wait, with `catch_unwind` at host-callback boundaries; or
  - **abort:** abort the process deliberately and document it.
- Add forced-panic tests for each work kind.
- Add a no-panic fuzz target for `inspect_g_source` seeded from `samples/`.
- Add a random closed-net generator, including auxiliary-loop shapes (N8).

*Mostly resolved 2026-10-05, following the maintainer's Decision 1: a panic is
a bug, contained as an interruption and never treated as semantics.* The
[panic plan](../plans/UserInputPanicSafety_2026-10-04.md) has the details.
- **Crashes.** N1 is fixed in `0d5c54df` and F1 in `0e3e456b`, each with a
  regression. The parser and evaluation inspections found no further
  user-reachable panic.
- **Containment.** Each step has forced-panic regressions.
  - The three claimed-poll boundaries catch an unwind and end the claim as
    `Panicked` through the ordinary terminal path. Waiters halt, and workers
    survive (`f8a00dde`).
  - A lazy records its own panic as evaluation state and is never replayed
    (`94f5e635`).
  - A panic that tears runtime-core state poisons the runtime. Parked threads
    wake, and further mutation fails loudly instead of hanging (`04798681`).
  - Collection survives a panic (`3a9eb1a6`), and client callbacks invoked
    outside polls are contained (`e50a641d`).
- **Open.** The interaction-net inspection and N8's random closed-net
  generator follow the net polarity checker. Fuzzing is deferred by decision
  until inspection stalls. Deeply nested source still overflows the parser's
  stack.

### X5 — High — Transition scaffolding taxes every structural change

**Reproduced (allows) / Verified (counts) / Reported (inventory details).**

**Evidence**

- **Stale `dead_code` allows.** There are 67 `#[allow(dead_code)]` sites and
  zero `#[expect]`. Converting each to `#[expect(dead_code)]` and compiling
  classifies them:

  | Class | Count | Examples |
  | --- | ---: | --- |
  | Stale: the item is live in the production build | 39 | all 21 `InteractionNetBuilder*` builtin variants (`core.rs:2002-2102`, reasons "PNC2-PNC5 assemble … in stages"); `core.rs:2629/2655/2714/2863` ("D.2b.1b … before D.2c-D.2g migrate callers"); `eval/builtins/net/builder.rs:43/56/79`; `eval/whnf.rs:324`; `core/managed/payload_edges.rs:22` |
  | Dead in production, used only by tests | 20 | the generic direct gateway (`interaction_net/runtime.rs:1119-1490`); `eval/net.rs:1176/1410` `Release`; `eval/whnf.rs:363/436/473/548`; `runtime.rs:738` |
  | Dead in every build | 8 | `eval/whnf.rs:304` `WhnfFrameKind` ("W3-W6 construct the deeper frame families"), `eval/whnf.rs:460` `WhnfExternalBoundary` ("staged for W4"); the rest are deliberate layout or exhaustiveness probes |

  The complete list is in the appendix.
- **Inventories.**
  - 18 modules and 12,954 lines. In `core/managed/` alone, 8.1k lines of
    inventory guard comparable production code.
  - Latches include exact counts (518 raw-`Value` signatures, 914
    persistent-edge occurrences, 253 declarations, 1,138 resolved calls,
    gateway count 533) plus opaque FNV fingerprints. A drift failure shows
    no diff, so re-baselining becomes rubber-stamping.
  - Some match whitespace-exact source substrings. Some still guard
    retired names or closed remediation enums (`RemediationOwner::{D2b…D2g}`).
  - Some prose rows are already wrong while tests pass (`CoreValues` field
    list; the "canonical `RuntimeValueRoot` operands" claim for machines
    that hold none).
  - `collect_rust_sources` is duplicated in 11 modules, and each test
    re-parses the tree.
- **Milestone names.**
  - 17 milestone-named test files (8,150 lines; `eval/value/tests/w4.rs`
    alone is 2,907).
  - `w6g1f3i_*` test names.
  - Production comments delegating work to closed milestones. One is
    perf-relevant: `evaluation/coordinator/task.rs:63-65` justifies a `Box`
    on every completed wait poll "until I4F.2". Verified.
  - The profiling script hard-codes `eval::value::w4_tests::…` paths.
- **"Compatibility."** The word appears 857 or more times in `src/`
  (`evaluate_compatibility_whnf`, `trace_compatibility_value_managed_edges`),
  yet it names the *production* structural value representation and is
  defined nowhere. About 296 eval test call sites use
  `evaluate_compatibility_whnf`, which the
  [holistic WHNF review](ResumableWhnfHolistic_2026-09-28.md) marks as a
  transitional facade.

**Recommendation**

Before VRR V1:

1. **Inventory triage**, following the per-module disposition in V4:
   - keep negative latches that encode live safety rules, such as no strong
     domain back-edge, no active RAII reachable from the managed graph, and
     opaque families requiring admission;
   - retire closure proofs, counts, fingerprints, and retired-name latches;
   - share one parsed-tree cache across survivors;
   - prefer type-level enforcement wherever VRR can provide it.

   **Keep the practice even as the instances are retired.** During the GC
   integration, temporary syntax-backed negative tests were the most
   useful part of this machinery. They prove that a retired type, signature
   shape, or call path cannot quietly reappear partway through a
   transition. Future major transitions, VRR first, should add such tests
   for their own retired surfaces, then retire them at the closing review,
   leaving only the negative rules that remain durable. The maintainer
   confirmed this disposition, and the rule is now recorded in
   [`AgentContext.md`](../AgentContext.md).
2. **Switch crate-wide to `#[expect(dead_code, reason = …)]`.** Delete the
   39 stale allows. Put the 20 test-only items under `#[cfg(test)]` or
   delete them, and delete the dead vocabulary.
3. **Rename milestone tests by concern.** Rewrite module docs as current
   contracts. Resolve or relocate every "until <milestone>" comment. Extend
   AgentContext's no-chronology rule to code comments and test names.
4. **Define "compatibility" once** in a values architecture doc (X8), or
   rename it, for example to "structural shell".

### X6 — Medium — Layering cycles around core

**Verified.**

Counts are of `crate::X` mentions in production code. The generic
`interaction_net` layer and `runtime.rs` are clean leaves.

| Edge | Evidence | Concern |
| --- | --- | --- |
| core → evaluation | `core.rs:19-22` (`EvalContext`, `EvaluationWorkCoordinator`, `ReflectionTaskResultPolicy`); `core/managed/recursive_cells.rs:21-26` (completion subscriptions and producer obligations inside the lazy and promise cells) | VRR cannot change identity cells without scheduler types |
| core → eval | `recursive_cells.rs:21` (`eval::lazy_checkpoint::ManagedLazyCheckpointEdge`) | same |
| core → api | `core/managed/payload_edges/runtime_net.rs:219` (`crate::api::Values`) | core depends on the public facade |
| core_net → eval | `core_net.rs:714-918` (`eval::whnf::NetWhnfState`); `impl NetSpecialization for CoreSpecialization` lives in `eval/net.rs` | the "core" net specialization is really eval-owned |
| diagnostic → evaluation | `diagnostic.rs:430, 466, 486, 549` (`EvalContext`) | semantic shapes perform evaluation |
| reflection, g_syntax, compiler → api | 54 reflection uses; `compiler.rs:3` and `g_syntax/parser/source.rs:15` use `crate::api::CompilationExecution`; reflection decodes through public `EvaluatedValue` | the facade is internal currency; public types bind engine internals |
| eval ⇄ evaluation | `LazyTaskMachine` is an `EvaluationTaskMachine` living in `eval/value.rs`; evaluation calls back into `eval::poll_lazy_route` and `lazy_root_wait` | scheduler and evaluator change together |

**Recommendation**

- Give core opaque or trait-typed sidecars, implemented in `evaluation`, for
  completion subscriptions and checkpoints.
- Move `PromisedValue::fixpoint` into `evaluation`.
- Move `CompilationExecution` and the module loaders out of `api/` into an
  internal `assembler` module.
- Give reflection a crate-private value and diagnostic currency, converting
  at the public boundary only.
- Either move `core_net` under `eval/net/`, or make its checkpoint opaque.

None of this is urgent. Each item should land with whatever plan touches
those files.

### X7 — Medium — The main crate's unsafe surface has no ledger or lint policy

**Verified / Reported.**

**Evidence**

- The main crate has about 107 unsafe constructs. 73 are `unsafe impl`
  (30 `ManagedFamily`, 30 `Trace`, 9 `OpaquePayloadFamily`,
  4 `RuntimeCacheFamily`), concentrated in `core/managed.rs`,
  `recursive_cells.rs`, and `eval/lazy_checkpoint.rs`.
- `src/lib.rs` has no `unsafe_code` or `unsafe_op_in_unsafe_fn` lint.
  `glam-gc` denies both.
- There is no main-crate counterpart to `audit-unsafe.sh` or `SAFETY.md`.
- Main-crate Miri, ASan, and TSan runs used filters recorded only in
  [I11D.2](GarbageCollectorI11D2DynamicToolMatrix_2026-10-01.md).

**Why it matters.** VRR rewrites exactly these `Trace` and `ManagedFamily`
impls. Their exactness is what keeps the collector sound.

**Recommendation**

- Apply `deny(unsafe_op_in_unsafe_fn)` crate-wide and `deny(unsafe_code)`
  with per-module allows.
- Add an inventory diff in the style of `glam-gc`'s.
- Write one short owning doc for the unsafe family contracts.
- Script the dynamic-tool matrix.

### X8 — Medium — Documentation gaps that matter for the next phase

**Verified / Reported.**

- **No current doc for the managed value layer.**
  - `src/core` is about 16.7k lines.
  - `recursive_cells`, `payload_edges`, `runtime_cache`, `ManagedFamily`,
    and `OpaquePayloadFamily` have zero mentions in architecture or
    agent_context docs.
  - The "Collector Boundary" section of `architecture/evaluation.md` is 35
    lines. It does not say that the structural trace is logical, recursive,
    and proportional to references (V2).
  - The rationale lives in a 451 KB plan and a ledger, and that ledger is
    itself a test fixture.
- **The interaction-net driver is undocumented.** No architecture doc
  covers the NetDriver and cursor-WHNF control flow, which perf work will
  reshape.
- **Docs describe mechanisms that no longer exist** (Reported):
  - "Bounded standard-effect fusion" in `src/README.md`,
    `architecture/reflection.md:179-185`, and PEAF's premise. The W5B loop
    was removed; only a dispatch shortcut remains (R4).
  - `architecture/evaluation.md:389-401` says saturation calls
    `apply_builtin_in`. In production it always allocates a builtin lazy
    (E2).
  - `agent_context/interaction_nets.md` describes `CoreRuntimeNet` as owner
    plus weak observer, a cloneable-`Data` specialization trait, and an
    `Error` operator; none of these match the code.
- **Overstated claims.**
  - "Sole mutation authority" and "one runtime-wide ready-task queue"
    (`architecture/evaluation.md:44`, `117-118`). Wait cells,
    `LocalPromiseOwner`, and client result cells hold independent state, and
    the ready queue's order is read only by test code.
  - The note that "routine suite keeps scale-only proofs ignored"
    contradicts one 1,100-layer case that runs routinely
    (`eval/net.rs:2585-2611`).
- **Size and density.**
  - `architecture/evaluation.md` is 54 KB with 12 headings. Its "WHNF
    Submachine Flow" runs 207 lines without subheadings.
  - Required reading for evaluation work is about 130 KB.
  - "Owner lease", "mutator", "regional", "access region", and "publication
    nursery" are undefined.
  - `src/README.md` table cells reach 900 characters.
- **Module map gaps.** `src/README.md` omits about 8.9k lines of production
  modules: `eval/{access,pattern,object,list,list_effect,list_transform}_machine.rs`,
  `g_syntax/parser/pattern.rs`, `ast.rs`, `recursive_do.rs`,
  `reflection/machine/reset_stack.rs`, and `interaction_net/profiling.rs`.
  It also duplicates the ownership table in `architecture/evaluation.md`.
- **Plans and reviews sprawl.**
  - 1.92 MB of plans and 0.93 MB of reviews, with no reviews index.
  - `rg -w RuntimeValueAccess` returns 6 hits in current docs and 123 in
    history.
  - 4 plans are missing from `plans/README.md`.
  - Several status lines are stale. The deferred perf plans are still gated
    on conditions that are now met.
- **Onboarding.** The top-level `README.md` has no build or test commands,
  no workspace layout, no toolchain requirement, and no status.
- **Stale identifiers:** `CallableData` and `HostFn`
  (`agent_context/interaction_nets.md:267, 272`) and `LazyFailure`
  (`agent_context/evaluation.md:300`).

**Recommendation**

- Add `docs/architecture/values.md` (at most about 15 KB) covering:
  - inline versus managed-root representation;
  - `CoreValueFactory` and `RuntimeValueDomain`;
  - access regions;
  - family traits and their unsafe contracts;
  - trace characteristics;
  - collection policy and its CLI consequence;
  - the status of the "compatibility" shell.
- Split `architecture/evaluation.md` into an overview of at most about
  10 KB (glossary, dataflow, reading map) plus topic docs.
- Correct the fusion, saturation, and net-driver docs before PEAF or net
  perf work rebases on them.
- Adopt a retention policy. A completed plan or review may be deleted, or
  moved to `docs/archive/` and excluded from search, once three things hold:
  - its durable decisions live in architecture or agent_context docs;
  - nothing in code, tests, or current docs references it;
  - its completion hash is listed in `plans/README.md`.
- Refresh the status lines of the deferred perf plans now.

---

## Subsystem Findings

### Core values and managed layer (V)

**V1 — High — Every managed allocation and root registration takes a
process-global lock and the heap lock.** Verified (allocator path),
Reported (root path).

- **Allocation.** Glam re-acquires an allocator for each allocation:
  `RuntimeValueAccess::allocator::<T>()` → `Mutator::allocator`
  (`crates/glam-gc/src/mutator.rs:65-74`). That path does three things:
  - `metadata_for::<T>()` locks a process-wide
    `Mutex<HashMap<TypeId, _>>` (`crates/glam-gc/src/class.rs:429-442`);
  - it recomputes the run geometry;
  - `discover_class` locks the heap's data mutex.
- **Roots.** Root registration repeats the lookup. The root registry is
  pruned only at collection, and collection pressure counts runs, not
  registrations.
- **Outdated assumption.** The collector plan had deferred a class cache
  because the lookup was expected to stay cold. In the integration it
  happens on every allocation, serialized across all runtimes and workers.

*Recommendation:* use a per-family static or per-thread class cache keyed
by metadata address, and let allocators and roots reuse it. Compact roots
opportunistically. Expose `HeapMetrics` through the X3 counters. Do this
before VRR V0, or V0 will measure lock contention.

**V2 — High — The structural trace is logical, recursive, and wasteful.**
Reported.

- Shared `Arc` shells are traced once per reference ("counts describe
  visits, not unique physical `Arc` nodes", `list.rs:107-111`).
- Lists are walked twice per trace, once for thunks and once for values.
- `key_node_count` allocates a worklist per dictionary entry per trace only
  to compute statistics that are then discarded (`payload_edges/persistent.rs`).
- `visit_value_with` recurses on value nesting.

Pause time therefore scales with logical size × owners, and deep nesting
risks stack overflow inside collection.

*Recommendation:*

- Walk dictionary values only.
- Walk each list once.
- Use an explicit worklist.
- Keep the statistics in a test-only variant.
- Add "logical versus physical visits" as a required VRR V0 measurement.

**V3 — High (medium confidence on trigger depth) — Core walks and
destruction are not stack-safe.** Verified (list literal and `Drop`),
Reported (walks).

- `List` has no iterative `Drop`. `list.rs:1617-1621` deliberately leaks a
  20k-deep `Concat` spine because recursive `Arc` destruction is "outside
  the front-walk contract".
- List literals build exactly such left-deep spines, at quadratic cost
  (reproduced above, `eval/operator.rs:235-251`).
- `Key::to_value_in`, `key_from_value`, `value_from_key`, and the trace walk
  recurse on data depth.
- The W7 recursion gate (`eval/whnf_inventory.rs`) scans `src/eval` and six
  orchestration files, not `src/core` or `list.rs`.
- Finalization drops value nodes during maintenance.

*Recommendation:*

- Add an iterative `ListNode` drop.
- Lower literals with `List::from_values`.
- Make the key conversions iterative.
- Add small-stack tests that build, trace, collect, and drop a 100k-deep
  concat and a 10k-nested strict dictionary.
- Extend the recursion rule to core and `list.rs`.

**V4 — High (cost) — Inventory disposition.** Reported, endorsed by the lead
reviewer.

| Module | Disposition |
| --- | --- |
| `gate_g2_inventory` | retire (the gate passed; the test reads plan prose) |
| `raw_value_api_inventory` | retire the count, fingerprint, and D2x scaffolding. Keep at most one rule: a raw `Value` crossing a production signature needs an access witness or a short allowlist. Target lifetime-branded values in VRR. |
| `persistent_edge_trait_inventory` | retire (the cutover is done; the compiler enforces move-only `Gc`) |
| `durable_owner_inventory` | retire `DECLARATION_BASELINE` and its stale prose; keep the root-lifecycle behavior test |
| `recursive_identity`, `active_owner`, `containment` inventories | consolidate into one `managed_boundary_audit` keeping the negative safety latches |
| `api/value/access_inventory` | keep only the forbidden bare-core-escape latch (its root counts include test code) |
| `recursive_cells.rs` substring tests (about 600 lines) | retire, except the single-booked layout asserts |
| `eval/whnf_inventory` | keep the no-unapproved-recursion property, drop counts and fingerprints, extend scope to core |
| `whnf_checkpoint`, `evaluation/access`, `eval/access`, `g_syntax/access`, coordinator `registry`/`generation` inventories | drop counts, fingerprints, and whitespace-exact snippets; keep negative rules |

Target: about 13k lines down to about 2–3k, with no fingerprints.

Reported coverage gap: `PromisedValue::fixpoint` returns a bare
`Gc`-backed edge across access regions without an access parameter
(`core.rs:1059-1076`). It is safe only because a coordinator root exists.
The type-level replacement should close this.

**V5 — Medium — Per-construction overheads missing from the VRR plan.**
Reported.

- Each lazy allocates an `Arc<str>` from a static label.
- A promise allocates three sidecar `Arc`s.
- `EvaluationFailure::with_context_in` copies earlier contexts, so k
  frames cost O(k²).
- Projecting an inline integer allocates a `BigRational`.
- Atoms, builtins, and the "cached" `Values::unit()` each cost a 64-byte
  node plus a locked root registration.
- `core::Value` is 64 bytes because `Number(BigRational)` is.

*Recommendation:* add these to a VRR "V-1 prework" checkpoint together with
V1–V4. Boxing `Number` is a measurable precursor to V2.

### Evaluation scheduler and runtime (S)

**S1 — High — Many scheduler round trips per unit of semantic progress.**
Verified (mechanism), Reported (counts). This is one phenomenon seen from
three angles:

- **Every uncached lazy becomes a coordinator route.**
  `reduce_semantic_shell` (`eval/whnf.rs:948-960`, verified) emits
  `Deferred(Lazy)` for any uncached lazy. There is no inline path.
  Saturated "immediate" builtins are first wrapped in a builtin lazy (E2).
- **Yields happen after ordinary phase transitions, not only on budget
  exhaustion** (E1).
  - There are 102 `Yielded` returns in 14 machine files. For example,
    `comparison_machine.rs` yields after every frame transition, so
    comparing two N-element lists costs O(N) round trips.
  - The pump reserves a whole 64-unit quantum per poll and never refunds
    it.
- **Each round trip is costly.**
  - Each transition takes the coordinator mutex, a runtime-wide admission
    `RwLock` read, and an activity mutex with `notify_all`.
  - One blocked release locks the coordinator up to five times.
  - The reviewers estimate about 15 or more coordinator-lock acquisitions
    and about 6 admission cycles per forced lazy.
  - The single condvar wakes workers, clients, drains, and observers
    together.

*Recommendation:*

1. Count transitions per forced lazy and per builtin (X3).
2. Give `Yielded` a single meaning: the budget is exhausted. Loop while
   budget remains, charging administrative units.
3. Use one access region and one producer lock per lazy-route poll, and
   perform family-to-WHNF handoffs within the same poll.
4. Merge release steps into one critical section, and make the activity
   generation atomic with conditional notification.
5. Evaluate inline-first lazy forcing (spill to a route only on suspension
   or contention), mirroring the interaction-net callable spill. This
   changes exact-dependency routing, so it is a design decision for the
   maintainer.

Re-baseline the exact-budget fixtures deliberately.

**S2 — High — Worker selection and claiming are O(records) under the global
lock.** Verified in part.

- `remove_ready_task` runs `ready_tasks.retain(..)` on every claim
  (`coordinator.rs:3873-3876`, verified), although only test selectors read
  that `VecDeque` order.
- `claim_causal_background` scans all background roots and allocates a set
  per probe.
- Parked and blocked sparks stay registered as roots until terminal.

*Recommendation:* keep a runnable-root index maintained on block and wake.
Drop the ordered queue. Make the probes use a reusable scratch buffer.

**S3 — High — Claims have no unwind guard.** See X4.

*Resolved 2026-10-04.* `ClaimedTask::poll`, `poll_claimed_client_demand`,
and `poll_claimed_spark` catch an unwind and end the claim as `Panicked`
through the existing terminal path. The record no longer stays `Running`,
waiters halt, and the worker survives. A panic that escapes those boundaries
came from scheduler code: the worker loop's outer catch poisons the runtime,
and the worker exits. Release paths rely on that runtime poisoning, not on a
per-claim guard.

**S4 — Medium (blocks Concurrent GC step CG0) — Coordinator locking inside
a managed edge transition.** Reported.

- **The chain.** `settle_terminal_work` holds admission and opens managed
  access. Promise publication's `after_assignment` closure, inside
  `with_managed_edge_transition` (`recursive_cells.rs:1004-1047`), then
  calls into `complete_task_promise_guarded`, which locks the coordinator,
  and wakes dependencies, which locks it again.
- **Contradiction.** This contradicts `architecture/evaluation.md:244` and
  `agent_context/evaluation.md:25`. It is justified only locally, by
  stop-the-world exclusivity (`recursive_cells.rs:575-579`).
- **Possible deadlock in test and aggressive builds.** Nested admission
  reads under `gc_activity_for_entries` could deadlock against a queued
  writer.
- **No lock order.** No lock-order table exists.

*Recommendation:*

- Write a lock-order table.
- Record the coordinator intent inside the transition and apply it after
  access closes.
- Debug-assert that admission is never re-entered.
- Add this nesting to the CG0 inventory.

**S5 — Medium — The lifecycle state machine is written five times.**
Reported.

- `release_{reflection, deferred, spark, client_demand, lazy_route}` run 119
  to 210 lines each; `release_deferred` and `release_lazy_route` are nearly
  identical.
- One flat 7-state `WorkState` is shared by all kinds, and each kind's legal
  subset is enforced only by about 227 runtime asserts.
- Six ad hoc post-unlock effect bags deliver wakes in inconsistent orders.
- About 116 inline copies of the coordinator lock-and-expect call.

*Recommendation:*

- Introduce a shared `release_common -> ReleasePlan` with per-kind hooks.
- Use per-kind typed states.
- Use one `PostUnlockEffects` type with a fixed delivery order.
- Route all coordinator locking through one helper that also feeds the X3
  counters.
- Split `coordinator/route.rs` (about 830 lines) and `coordinator/profile.rs`
  out of `coordinator.rs` (4,018 lines).

Do this before S1 and S2, so those fixes land once instead of five times.

**S6 — Medium — No model checking for the multi-lock wake and claim
protocols.** Reported.

- Forced-order tests are good: about 140 probe, barrier, and channel sites.
- The subscribe-and-recheck protocol nonetheless spans four primitives.
- `AgentContext.md` calls for a model checker in this situation, and Loom
  already exists in `glam-gc`.

*Recommendation:* Loom the extracted protocol before S1 changes lock
granularity.

**S7 — Medium — Gaps for Public Resumable Evaluation.** Reported.

- `drive_client_demand` is monolithic. A test-only
  `advance_client_demand_for_test` duplicates it with different semantics.
- `budget.spent()` is discarded, so `spent_steps` cannot be reported
  faithfully.
- No-progress abandons the record.
- Waits use the runtime-wide condvar even though the per-handle result cell
  has its own.

*Recommendation:* do the PRE-1 extraction now, replace the test seam, and
thread exact spend.

**S8 — Low.**

- Release functions compute `made_progress` and `remains_blocked`, and
  production ignores both.
- Test-only selectors exercise a scheduler policy that production no longer
  uses.
- Profiling counters are duplicated across two structs.
- An `Arc<OnceLock>` is allocated per client poll.
- `producer_wait()` allocates an `Arc` inside locked traversals.
- Session closure is split between `EvaluationSession::drop` and
  `SessionClosureWork`.
- `evaluation/tests.rs` (9,875 lines, 171 tests) has no submodules.

### Evaluator semantic machines (E)

**E1 — High — `Yielded` conflates progress with budget exhaustion.** See
S1. Verified at `comparison_machine.rs:146-171`.

**E2 — High — Lazy routes rebuild state on every poll.** Reported.

- The route shell is rebuilt each poll and opens three access regions
  before doing any work (cache check, `source_snapshot` with a `LazySource`
  clone, `checkpoint_work`).
- Finishing one lazy takes up to 7 access regions and 2 root registrations.
- Family-to-WHNF handoffs allocate a new WHNF cell and then yield.

*Recommendation:* see S1, items 3 and 5.

**E3 — Medium — Nine hand-copied managed checkpoint cells with inconsistent
poison policy.** Reported.

- Each cell has a `try_lock` `Trace` impl, a drop record, a transition
  wrapper, and arms in a seven-way match.
- Three cells panic on poison; six return a structured failure. Only the
  WHNF cell's poison path is tested.
- Two cells are labelled "temporary" and re-implement the seed-to-managed
  promotion.
- Concurrent GC must replace the `try_lock` quiescence assumption in all
  nine places.

*Recommendation:* one generic `ManagedCheckpointCell<S>` with one
transition function and a structured-failure poison policy, plus a generic
durable-regional adapter.

**E4 — Medium — No shared regional poll type.** Reported.

- Twelve enums have the same `{Ready, Boundary, Yielded, Failed}` shape.
- The child-demand lowering match is written by hand 38 times, plus four
  helpers, two of them byte-identical.
- The boundary-to-`WhnfPoll` translation is pasted 6 times.

*Recommendation:* add `RegionalPoll<T>` with `map` and a `ready!`-style
macro, and one `demand_whnf` helper hosting an already-WHNF fast path.
Without it, PEAF2 would add a thirteenth enum.

**E5 — Medium — `LazyTaskMachine` lives in `eval/value.rs` and creates the
eval⇄evaluation cycle (X6).** Reported.

The machine is about 1,350 lines inside a file that also does diagnostic
projection. Its transient shell assigns `self.work` in every family poll,
and production discards the result.

*Recommendation:* move it to its own module and dispatch on the checkpoint
kind inside one access region.

**E6 — Medium — Semantic rules are implemented twice, and builtin
classification tables can drift.** Reported.

- **Duplicated semantics:** semantic-undefined detection (`whnf.rs` and
  `tagged_machine.rs`), unit assertion (builtin and annotation machines),
  and strategy demand (builtin and strategy machines).
- **Four hand-maintained builtin tables:** `builtins.rs`,
  `builtin_machine.rs::supports`, `builtin_machine.rs::new_in` with a
  `_ => Numeric` fallback, and `value.rs`.
- **Drift failure modes:** non-termination, or `unreachable!`.

*Recommendation:* one exhaustive `builtin_strategy(Builtin)` used by all
four, and shared, unit-tested semantic predicates.

**E7 — Medium — The evaluator prints to stderr under managed access.**
Verified.

`warn_unknown_annotation` calls `eprintln!` (`annotation_machine.rs:1074-1076`)
inside a checkpoint transition. This is the library's only stderr write.

- It bypasses the diagnostic bus and its counts.
- It violates "evaluation performs no external I/O" and the callback-free
  access rule.
- It fires on every evaluation.

*Recommendation:* return the warning as an outcome that crosses the region
boundary, deduplicate it, and publish it through runtime diagnostics.

*Resolved 2026-10-05, following the maintainer's choice of a runtime
ledger.*
- **Recording.** Evaluation records each distinct unrecognized annotation in
  a deduplicated ledger owned by the value domain, then continues. Recording
  is a leaf-lock update, with no I/O and no callback.
- **Publishing.** The assembler drains the ledger after `ValueEvaluator::eval`
  and in `drain_reasoning`, and publishes each annotation once per runtime as
  a `Warning` on its diagnostic bus, where counts and subscribers see it.
- **Scope.** The library no longer writes to stderr. `'deprecated` and
  `'TBD`, which are not yet implemented, go through the same route.

A regression evaluates an unrecognized annotation twice and observes one bus
warning naming it.

**E8 — Medium — Allocation and copy hot spots.** Verified (list operator),
Reported (others).

- Partial application of every core operator copies all supplied operands:
  O(n²) copying, with a `Vec` plus an `Arc<[Value]>` per step. The list
  literal case is reproduced above.
- Application copies `Arc` → `Vec` → `Arc`.
- 251 `duplicate_value` sites clone `BigRational`s with allocation.
- Every followed lazy ID is inserted into a `BTreeSet` that production
  never reads.
- All 42 child demands allocate a 128-byte `RegionalWhnfWork` even for
  literals. There is no WHNF fast path.

*Recommendation:* fix these through the E4 helper. Build list literals
directly from the operand vector.

**E9 — Medium — Tests are organized by milestone, and the semantic suite
rests on a transitional facade.** Reported.

- See X5 for the milestone-named files and `evaluate_compatibility_whnf`.
- Builtin semantics are tested only end to end.
- There is no budget-differential harness: run each fixture at budgets
  {1, 7, ∞} and compare results and failure contexts.

*Recommendation:* add that harness **before** S1 and E1 change yield
behavior, and move the suite to a root-returning client-demand helper.

**E10 — Low — Dead protocol vocabulary in `eval/whnf.rs`.** Reproduced via
the `#[expect]` classification.

- `WhnfFrameKind`, `WhnfExternalBoundary`, `RegionalBoundaryRequest::Dependency`,
  `NetWhnfDrive`, and `NetWhnfState::drive_in` are dead or test-only.
- About 20 consumers still carry `External` arms.
- `Seed.source_owner` is always `None`.
- The doc comment calls `Seed` "protocol fixture only", but it is the
  production entry.

### Interaction nets (N)

**N1 — High — Rewrite rules panic on aux–aux links inside an active pair.**
Reproduced (see X4).

- `join`, `commute_fans`, `duplicate_bind`, `duplicate_operator`, and
  `erase` disconnect auxiliary ports sequentially through `take_auxiliaries`
  (`interaction_net/runtime/rewrite.rs`, `runtime/graph.rs:4-15`). The
  second disconnect of an internally linked pair finds the port already
  unwired.
- The panic happens under the net mutex. Poison then breaks GC tracing,
  because tracing calls `try_lock().expect(..)`.
- `NetBuilder::try_wire` rejects only identical ports, so the public
  `Assembler::net` can build the shape too.

*Recommendation:*

- Add the regression first.
- Read all neighbours before disconnecting, and resolve pair-internal links
  (closed loops vanish).
- Include such shapes in N8's generator.

*Fixed 2026-10-04:* every rewrite now detaches the pair's whole auxiliary
boundary before rewiring and resolves pair-internal links. This covers the
five rules above, `duplicate_data`, and the two call-to-operator rewrites.
Runtime regressions cover each shape, and the program in X4 now evaluates to
its data. N8's generator remains open.

**N2 — High (for Concurrent GC) — Edge-delta accounting is a second,
hand-maintained copy of every rewrite.** Reported.

- 14 predictor functions compute added and removed payload owners ahead of
  each mutation, relying on allocation order ("the structural Bind is
  allocated first").
- The `edges` and `update` closures are passed separately and nothing ties
  them together.
- Stop-the-world GC ignores both, so only count probes and a source-text
  check verify them.
- Under Concurrent GC they become SATB barrier inputs. Any rewrite
  optimization would silently desynchronize them.

*Recommendation:* generate deltas from the graph primitives into a
per-transaction log. At minimum, add a feature-gated differential oracle
that compares predicted against actual `visit_logical_payloads` across the
suite. Do this before optimizing `rewrite.rs` or starting Concurrent GC.

**N3 — Medium — Hot-path representation and driver costs.** Reported.

- **Node storage:** SipHash `HashMap<NodeId, RuntimeEntry>` with 120-byte
  entries.
- **Wasted lookups and clones:** assertion-only lookups in `connect`, and
  payload duplication before rule match.
- **Fan identities:** `FanIdentity` translation rebuilds nested identities
  recursively without memoization.
- **Spine re-walk:** the demand spine is re-walked after every reduction,
  allocating a fresh `HashSet` each time.
- **Per-call net construction:** every function call builds a new template,
  `RuntimeNet`, `RuntimeNetCell`, and managed cell.
- **Quadratic builder:** `NetBuilder::try_wire` scans all existing wires,
  inside an unbudgeted netlist replay.
- **No ID reuse:** NodeIds are never reused and are referenced outside the
  graph, so a slab store needs generations.

VRR does not cover graph storage.

*Recommendation:* assign these to an explicit net performance plan after
X3 provides a baseline.

**N4 — Medium — Profiling sits in the wrong layer and is too thin.**
Reported.

`interaction_net/profiling.rs` is in the generic layer but defines
coordinator and exact-route types that `api/runtime.rs` patches in. It has
no timing and no CLI dump.

*Recommendation:* fold it into the runtime-level counters from X3.

**N5 — Medium — Dead blocked-pair protocols.** Reported, consistent with the
`#[expect]` classification.

- `BlockedCall` and `BlockedOperatorCall` cannot occur in production
  (`block_claimed_call` is dead; `OperatorDisposition::Blocked` is test-only).
- Their machinery remains in every layer.
- It has already drifted: Failed and Killed waits retry in the dead arm but
  fail in the live arm.

*Recommendation:* delete both, reducing `ActivePairState` from 7 variants to
5.

**N6 — Medium — Four wrapper layers around one graph.** Reported.

The layers are `RuntimeNet`, 12 `RuntimeNetCell::with_*` variants, 80
`pub(crate)` core_net wrappers with 6 mirror types, and the eval claim
guards. A test-only parallel API (the direct gateway) sits alongside them.

*Recommendation:* use a single `transact(edges, update)`, implement core
methods directly on the generic types, and split `runtime.rs` (3,687
lines).

**N7 — Low (medium confidence) — Isolated one-port pairs are never
reduced.** Reported.

`Erase >< Data` and `Erase >< RemoteCursor` pairs are invisible to demand
spines and keep their payloads, including copy sources, alive.

**N8 — Medium — Reduction semantics are verified only by example.**
Reported.

- No global invariant checker exists: link symmetry, active map equal to
  principal–principal wires, frontier consistency.
- There are no randomized confluence tests.
- The only order-independence test reduces two pairs.

This gap is what hid N1.

*Recommendation:* add `RuntimeNet::check_invariants()` after every
transition in test builds, and a random closed-net generator comparing
readback across random pair orders.

*Prerequisite, done 2026-10-05:* the maintainer's net polarity change
landed first, so the generator exercises final semantics. `Bind >< Bind`
joins crossed (`B.1-C.2`, `B.2-C.1`), and function-role binds list
`[result, argument]`. A polarity checker comes next, before this generator:
see [the net polarity checker plan](../plans/NetPolarityChecker_2026-10-05.md).

### Reflection (R)

**R1 — High — The store's change log grows without bound, and each
validation scans it under the runtime transaction lock.** Verified.

- `ReflectionStore.latest_changes: BTreeMap<ConflictAddress, u64>`
  (`reflection/store.rs:443`) is never pruned.
- `conflicts()` (`store.rs:656-661`) iterates the entire map and locks the
  journal's observation index once per entry.
- Every task leaves a permanent `queries/<id>` entry.

Commit and revalidation cost therefore grows with total task history, and
total cost grows roughly quadratically with task count.

*Recommendation:*

- Keep a revision-ordered log and walk it back only to `snapshot.revision`.
- Prune by the low-water mark of live snapshots, or cap the log and treat
  older snapshots as conflicting.
- Lock the index once per validation.
- Add a test that commit cost is independent of retired-task count.

**R2 — High — The machine deep-clones its active branch every step.**
Verified.

- `let work = self.execution.work.clone();` sits in the generic step loop
  (`reflection/machine.rs:824`).
- The cloned work contains the continuation `Vec`, nested delimiter
  controls, and the transaction journal (edits, diagnostics, updates).
- Every root clone is an `Arc` increment plus a weak observer.
- `retry_candidate()` additionally clones the whole branch on every
  specialization poll outside a transaction.

A `do` chain that accumulates logs or edits costs O(n²). Deep
continuations pay O(depth) per step.

*Recommendation:*

- Move the work into `step` and return it alongside any error, as the
  sub-work steps already do.
- Make continuations, delimiters, edits, and journals persistent (rpds is
  already a dependency).
- Build retry checkpoints lazily.

Do this before PEAF1 measures anything.

**R3 — Medium — `machine.rs` (5,733 lines) mixes interpreter, three
drivers, and control families, with an illegal-state-permitting
`TaskExecution`.** Verified (budget), Reported (structure).

- **Five optional sub-work slots** plus a placeholder `MachineWork`.
- **Slot clearing is repeated five times.** One copy omits `controlling`.
- **Blocking driver in the machine:** `run()` parks the thread from inside
  the machine.
- **Budget defect:** `poll(steps)` constructs
  `EvaluationStepBudget::new(steps)` inside `for _ in 0..steps`
  (`machine.rs:603-606`, verified). `IsolatedEffectSearch::poll`, used by
  CLI token parsing and macro expansion, can therefore spend about steps²
  units.
- **Inverted dependency:** `protocol.rs` imports `machine::task_eval_error`.

*Recommendation:*

- Introduce an `ActiveWork` enum with one sub-step driver.
- Split `machine/{decode, control, state_path, cut, outcome, api}.rs`,
  following the existing `reset_stack.rs`.
- Move `run()` and pumping into `lifecycle` and `search`.
- Honor one total budget.

*Budget resolved 2026-10-05; structure open.*
- `EffectTask::poll(steps)` keeps one budget for the whole call and charges
  at least one step per pass.
- It pumps waits through `pump_wait_on_route_within`, which charges the
  caller for every poll quantum the route pump reserves, so a call spends at
  most `steps`.
- A regression checks the charging. One test that assumed a single
  `poll(256)` reaches a state block now polls until the task stops yielding.

The structural recommendations above remain open.

**R4 — Medium — Docs and the PEAF plan describe "bounded standard-effect
fusion" that no longer exists.** Reported.

- W5B removed the fusion loop. `EFFECT_FUSION_BUDGET` is now test-only.
- What remains is a dispatch shortcut that shares all decoding.
- The "fused" test never forces a budget yield.
- The fused-versus-unfused oracle shares the decoder.

*Recommendation:* correct `src/README.md`, `architecture/reflection.md`,
and PEAF's purpose and PEAF0 sections. Note that a differential oracle must
be independent of the shared decoder.

**R5 — Medium — Key-path decoding implemented two or three times.**
Reported.

- `reflection/requests.rs` re-implements key conversion and value paths
  that `eval/access_machine.rs` already provides.
- `machine.rs` adds a third value-path walker.
- Error strings are copied verbatim.
- `.env` is tested only with atom keys.

*Recommendation:* expose the eval machines through a specialization
adapter and delete the copy. Until then, add composite-key `.env` tests.

**R6 — Medium — Captured continuations are never released.** Verified
(insert-only).

- `continuations: HashMap<u64, CapturedContinuation>` has one `insert`
  (`machine.rs:477`) and no removal. Its registered roots live as long as
  the task.
- Each control operation decodes and re-encodes the whole reset stack
  through resumable WHNF.

*Recommendation:*

- Carry the capture in the continuation function as a traced payload, or
  drop it on branch discard and terminal states.
- Cache the decoded stack, checked by root identity.
- Document retention semantics.

**R7 — Medium — `TaskHalt::Blocked` is a leftover suspension channel.**
Verified (the variant exists), Reported (production-unreachable, medium
confidence).

It is produced only by `From<EvaluationHalt>` (`protocol.rs:640`, `682`),
yet `handle_step_error` and the macro runner still branch on it.
`agent_context/reflection.md:15-21` still says "until W8".

*Recommendation:* remove the `From` impl, let the compiler confirm, then
delete the variant and its branches.

*Resolved 2026-10-05.* Production never produced the variant.
- **Only producer.** `From<EvaluationHalt>`, through `task_eval_error`. Its
  only callers were two tests, and both passed failures.
- **Waits.** Reflection machines absorb evaluator waits as
  `WorkDependency::Wait` before any halt is built.
- **Macro runner.** It branches on `EvaluationHalt::blocked_on`, not on the
  task halt, so it is unaffected. That part of the finding was inaccurate.

Removed:
- the `From` impl and `task_eval_error`, which also removes the inverted
  `protocol` → `machine` import;
- `TaskHaltKind::Blocked`, `TaskHalt::blocked` and `blocked_on`, and their
  panicking arms;
- `handle_step_error`'s blocked branch, its assertion, and the helper that
  only that branch used.

A `TaskHalt` is now a failure or a panic, and production reflection code no
longer names `EvaluationHalt`. The two tests now build their failure with
`TaskHalt::failure`. `agent_context/reflection.md` now states the contract.

**R8 — Medium — Control semantics are tested only by example.** Reported.

- About ten `.shift` programs exist, all single-shot with one prompt key.
- Nothing covers multi-shot resumption, nested distinct keys, shift under
  `.alt` or `.cut`, or resuming under a different reset.
- alt/cut tests are example-based, with no model interpreter.
- The conflict-strategy lattice (exact ⊆ fingerprint ⊆ coarse) is checked
  on a handful of addresses.
- `.heap.get` outside a cut records no observation, which is untested and
  undocumented.

The store's STM semantics, by contrast, are well covered (23 tests plus a
forced-suspension matrix).

*Recommendation:* add seeded generative differential tests against a small
reference model before representation changes.

**R9 — Low.**

- The three scheduler adapters handle completion inconsistently: the unit
  adapter lacks the recursive-promise check.
- Request dispatch probes 16 tags linearly.
- `Tags::new()` and the API dictionary are rebuilt per task.
- Source-string pin tests are left over from W5C.
- `ConflictObservationIndex::clone_box` is required but never called.
- A panicking client strategy poisons a mutex while the transaction lock is
  held.

### Built-in front end (F)

**F1 — High — Layout-only braced statement panics the parser.** Reproduced
(see X4).

`do_expr.rs:499-512` holds a private `trim_layout` copy that lacks the
`.max(leading)` fix already applied to `structural.rs:1445` (`bd5648e6`).
It runs before the "empty statement" check.

*Recommendation:* delete the duplicate. Add multi-line `;\n;` regressions
for `do`, `let`, `where`, and `with`.

*Fixed 2026-10-04:* the duplicate `trim_layout` and `is_layout_empty` are
deleted from `do_expr.rs` in favor of the shared helpers. The invalid sample
`braced_empty_member` covers multi-line empty members for `do`, `let`,
`where`, `with`, and `match`. The wider inspection is tracked in
[`UserInputPanicSafety_2026-10-04.md`](../plans/UserInputPanicSafety_2026-10-04.md).

**F2 — Medium — Duplicated parser helpers have drifted.** Reported.

- **Repeated view and error helpers:** `view_between` ×4, `error_at_view`
  ×4, `trim_layout`, `split_top_level`, and `is_layout_empty` ×2.
- **Identical twins:** `structural_expression_hard_end` and
  `conditional_hard_end`; two language-position validators; two
  `fulfills_abstract`s.
- **Repeated layout checks:** six copies of the floor-check messages.
- **Keyword-list violation:** `layout.rs:334` hard-codes layout
  introducers instead of using `keywords::g0_layout_introducer`, contrary to
  `agent_context/g_syntax.md`.

*Recommendation:* move view utilities onto `TokenView`, and add one layout
owner validator.

**F3 — Medium — Superlinear compile hot spots.** Reported. Parse is linear
on a flat list literal (reproduced), so these concern structure.

- Structural keywords scan every group in the file.
- The Chumsky grammar (31 boxed combinators) is rebuilt for every leaf
  expression.
- `op -> pat` statements are trial-parsed at least twice.
- Floor validation allocates per nesting level.
- `free_bindings()` is recomputed per nested lambda.
- Unused-local analysis is quadratic in binder nesting.

*Recommendation:* add generated 10k- and 100k-line sources to the X3 corpus
first, then add a per-token innermost-group index and bottom-up free
variables.

**F4 — Medium — Stage boundaries leak.** Reported.

- `parser/source.rs` orchestrates macros, evaluates through the compilation
  execution, constructs diagnostic values, and emits macro diagnostics
  immediately, while other diagnostics are returned at the end. Ordering
  depends on the path.
- The parser calls `analysis::warn_unused_locals`, a third scope model
  beside `resolve/scope.rs` and `name_analysis.rs`.

*Recommendation:* move the macro driver into `macro_expansion/`, derive
unused-local warnings from resolver `BindingId` uses, and send all
diagnostics through one ordered channel.

**F5 — Medium — Resolution is coupled to the managed runtime.** Reported.

- `ResolvedExpr<Value>` embeds raw values.
- 172 production lines in `g_syntax` mention `RuntimeValueAccess`, 73 of
  them in `resolve/`.
- One access region spans resolving and lowering a whole declaration
  (`module_lowering.rs:81-109`), which is unbounded in input size. This
  conflicts with CG0's bounded-region requirement.
- Static scope decisions compare runtime identity.

*Recommendation:* use `ResolvedExpr<Const>` with a per-declaration root
table, syntactic scope kinds, and managed access opened only in
`net_lowering`, chunked per member.

**F6 — Medium — Verification gaps.** Reported.

- Invalid syntax samples reach only `inspect_g_source`, so there are no
  resolver, import, object-scope, or macro diagnostic fixtures.
- Expectation matching is subset-only.
- `samples/syntax` and `samples/config` are parse-checked, never lowered.
- 153 cross-stage tests use the whole-file test oracle.
- There is no fuzzing, and no deep-nesting or small-stack test.
- `tests/invalid_samples.rs` leaks the developer's `GLAM_CONF` and
  `GLAM_WORKERS`.

*Recommendation:* compile every valid sample, use exact matching, add
compile-path invalid families, make the tests hermetic, fuzz (X4), and add
a nesting-limit diagnostic or a small-stack test.

**F7 — Low — Fail-fast language versioning is not implemented.**
Reproduced.

`language g9 with nonsense` compiles and runs (exit 0). Tests assert that an
unknown `demo` extension is accepted. Text literals accept non-ASCII
without `utf8`. This contradicts `DistilledDesign.md` and `Syntax.md`.

*Resolved 2026-10-05, following the maintainer's Decision 8: fail-fast, with
an ASCII-only source unless `utf8` is declared.*
- **Admission.** `StagedSourceParser` checks the leading declaration on the
  compile, inspection and test parse paths.
  - A base other than `g0` is an error, and so is an extension other than
    `utf8`.
  - Either error stops parsing, so no later declaration is lowered or
    macro-expanded under a language the source did not ask for.
- **Character set.** Without `utf8`, the first non-ASCII character anywhere
  in the source is an error at its line. Parsing continues. Names and
  whitespace were already ASCII-only, so in practice this applies to texts and
  comments.
- **Tests.**
  - The `demo` parser tests now expect rejection, or use `utf8`.
  - New parser tests cover the version stop and the `utf8` requirement for
    both texts and comments.
  - Three invalid samples were added: unknown version, unknown extension, and
    non-ASCII without `utf8`.
  - The review's `language g9 with nonsense` reproduction now fails at line 1.

**F8 — Low.**

- `SyntaxCheatSheet.md` presents unsupported `import 'trig` and
  `import as … from` without marking them as target syntax.
- It calls effectful patterns target syntax, although samples use them.
- `meta.abstract_names` is described, but `abstract` is a no-op.
- `architecture/front_end.md` places source-wide name analysis inside the
  per-declaration loop.
- Diagnostics carry only line numbers.
- `abstract_global_path` joins and re-splits strings.
- Macro re-lexing is O(line number) per macro declaration.

### Embedding API, diagnostics, and CLI (A)

**A1 — Medium — A hidden public tier.** Reported.

- The binary relies on at least 25 `#[doc(hidden)]` public items:
  - event journals and snapshots;
  - `IsolatedEffectSearch`;
  - `EffectLifecycle`;
  - `StoreJournal::new`;
  - enrichment and transport helpers;
  - pending-report hooks.
- The public `try_commit_transaction` needs a hidden constructor.
- `pub mod reflection` exports 58 names with no statement of which are
  supported.
- The events-only commit pattern is copied four times in the binary.
- About 155 public items exist in total; about 165 of 333 public items in
  `api/` lack docs.

*Recommendation:*

- Classify each hidden item as supported (document it) or internal (replace
  it with a narrow helper such as `commit_events`).
- Replace `pub mod reflection` with an explicit, documented re-export list.

Do this before designing Public Resumable Evaluation.

**A2 — Medium — `api/assembly.rs` is a mid-layer.** Reported.

It contains:

- the macro-session lifecycle (`CompilationExecution`);
- a full `TaskHost` implementation;
- a runtime type implementing a reflection trait;
- environment construction;
- about 400 lines of import orchestration, with three near-identical
  `CompileContext` builder chains.

See X6 for the inverse dependencies.

**A3 — Medium — Structured failures are flattened to text at host
boundaries, and one path swallows errors.** Verified (`.ok()`), Reported
(others).

- Macro task failures, `conf.cli` failures, killed-work reports, and
  logger-transport failures are each converted to strings or literals,
  despite the "do not stringify at boundaries" rule.
- `completion_script_command` ends its lookup with `.ok()`
  (`src/bin/glam/main.rs:165-190`). An evaluation error in
  `conf.completion_script.NAME` therefore silently falls back to the
  built-in script and exits 0.
- `conf.cli` and `conf.completion_script` get no `{conf:{entry}}` context
  frame, unlike `env` and `log`.

*Recommendation:* carry `TaskHalt::diagnostic`/`Error::diagnostic` through,
publish completion-script failures, add the context frames, and add failing
`conf.cli` and `conf.completion_script` tests.

*Partly resolved 2026-10-05.*
- **Completion script.** An evaluation failure in a configured
  `conf.completion_script.NAME` is now published with a
  `{conf:{entry:"completion_script"}}` frame, and the command fails. Only an
  undefined binding falls back to the built-in script.
- **`conf.cli`.** Failed and blocked searches keep the effect's structured
  `TaskHalt::diagnostic` instead of formatting it into a message. The
  rendered error therefore shows the original message, the path-lookup
  frame, and the existing `cli` entry frame.
- **Regressions.** Failing `conf.completion_script` and `conf.cli` CLI tests
  were added.

*Remainder resolved 2026-10-05.* An audit of the three remaining claims found
lossy sites in macros, one in killed-work reports, and none that mattered in
the logger transport. The fix carries the original failure as a *cause*: a
nested message in the context list, which the CLI already renders with its
own context.

The headline states only the compiler's or host's own finding. Before the
maintainer's review, the headline also repeated the cause's text, so the
CLI printed the reason twice. The duplicate was removed from the headline,
not from the nested message, for two reasons:
- The nested message *is* the cause, and dropping it would drop the
  structure.
- Copying the cause's text into the headline is exactly the stringification
  this finding is about.

When no structured cause exists, for example a wait or an unassigned
promise, the headline keeps the reason text.

- **Macros.** These failures each keep the `TaskHalt` or evaluator failure
  diagnostic as the cause:
  - an effect failure;
  - a failure that blocks the effect;
  - a failure forcing a macro's result;
  - a failure selecting a macro or `meta.macro.env`.

  Previously each was reduced to `{error}` text.
- **Killed work.** The logger supervisor's deadlock report keeps the retained
  blocked failure as a cause after its `runtime: killed` frame. Its headline
  now ends ", with a retained error"; previously it appended the failure's
  message.
- **Task status.** `.task.status` now reports `killed:Diagnostic` in the same
  shape as `err:Diagnostic`, and `.task.error` returns it. Before, a killed
  task's status was the bare atom `'killed`. The diagnostic is the client's
  kill reason, the same root that `.task.join` already fails with.
  - The kill's emission is stored as built and is not normalized.
  - Normalizing evaluates, and evaluation cannot progress while the runtime
    is settling a deadlock. The first attempt hung the deadlock CLI test.
- **Logger transport.** `transport_value` fails only on a runtime-ownership
  mismatch. Cross-runtime communication has been removed, so that is now an
  `expect` contract instead of a handled path. Previously it wrote a
  placeholder record, which the logger would have rejected at decode.
- **Left as text, as genuinely new errors:**
  - macro arity, text, and regex validation;
  - `IsolatedEffectSearch::new_in_context` ownership checks;
  - the fallback-drain `eprintln!`s in `batch.rs`;
  - terminal rendering failures.
- **Regressions.**
  - A macro test checks that the cause's emission and text survive and that
    the headline omits them. It covers both the selection path and the
    result path.
  - The deadlock CLI test checks the nested `msg:` frame and that the reason
    appears once.
  - The task-status round trip checks the `killed` diagnostic.
  - Three macro tests found a macro's reason in the headline. That reason
    is now the cause, including text-only validation errors raised by the
    macro runtime, so the tests read the cause instead.

*Noted, not changed:*
- `TaskHalt::with_context` replaces the halt with text when the context
  belongs to a foreign runtime. This is a programmer-error path.
- `RuntimeDeadlockWork::blocked_error()` is a public text accessor that only
  tests use.

**A4 — Medium — Public-contract verification gaps.** Reported.

- Runtime endpoints, `DiagnosticIngress`, maintenance, and `pump_background`
  have no tests in `tests/`. They are covered only by in-crate tests that
  use internals.
- `public_api.rs`'s settle helper panics on `MaintenanceRequired`.
- The logger-style transactional `TaskHost` has no external test.
- *Added 2026-10-05, from the GC integration reviews:* a promise resolver
  given a value from another runtime consumes that value and leaves its
  promise unassigned forever. Decide whether to reject the value before
  consuming it.

**A5 — Low.**

- `settle_batch_runtime` and `settle_batch_runtime_default` duplicate the
  readiness state machine, and the library offers no settle driver.
- The "select a conf entry" idiom appears five times, with five different
  error behaviors; this is the root of A3.
- `AssemblerBuilder::default()` builds a full runtime that is then
  discarded.
- 872 of `main.rs`'s 1,169 lines are other modules' tests.
- `ModuleInput::Script { extension }` is used only as a label, so
  `--script.json` silently parses as `.g`. This contradicts `CLI.md:39`,
  "the extension selects the front-end compiler".
- Configured-CLI usage errors exit 1 while bootstrap usage errors exit 2,
  and neither is documented.
- Short flags `-h`, `-V`, `-q`, and `-v` are undocumented.
- The diagnostic viewer schema between `rendering.rs` and
  `diagnostic_formatter.rs` is undocumented.

---

## What Is Solid

- **Ownership is enforced by types.**
  - One `NoAuto` heap per runtime.
  - Move-only `Gc`, duplicated only under a matching mutator.
  - Higher-ranked access regions.
  - Single-edge lazy, promise, and net facades with compile-time size
    latches.
  - Unsafe admission traits with mandatory records.
  - Passive managed destruction, with an external-owner registry.
- **Dependency direction is clean at the important seams.**
  - `interaction_net` has no outward imports.
  - Evaluation never imports reflection; reflection enters only through
    trait objects.
  - Core, eval, and evaluation never import `g_syntax`.
  - Front-end lowering uses only the public net builder.
- **Scheduler correctness discipline.**
  - Subscription epochs plus dependency keys make stale wakes harmless.
  - The wait-token `OnceLock` is the single terminal authority.
  - Machines and roots are destroyed after unlock.
  - About 140 forced-order probe, barrier, and channel sites cover the race
    windows.
  - Workers cannot steal foreground demand.
- **Resumable work owns its state.**
  - Decode and request sub-work returns ownership on every outcome, so host
    effects are never replayed.
  - One canonical semantic reducer serves all 42 child demands.
  - The host-callback permit is one-shot and runs outside access.
- **Collector crate verification is exemplary.**
  - Unsafe ledger (148 constructs).
  - Codegen latch.
  - Miri, ASan, and TSan scripts.
  - Loom models.
  - Revision-stamped measurements.
- **Hygiene.**
  - Very few `unwrap()` in production (18, 13 of them in
    `interaction_net`).
  - No `todo!` or `unimplemented!`.
  - Every `#[ignore]` has a reason and a script.
  - Current-doc links and paths are clean.
  - The front end has one authoritative lexical pass with arena payloads
    and O(1) group skipping.
  - The macro protocol matches `Macros.md`.
  - The library/binary policy boundary holds: no `env::var` and no stdout
    writes in the library, apart from E7.

---

## Recommended Sequence

The sequence is grouped so that each step makes the next one measurable or
cheaper. Items within a group are independent.

### P0 — Ground truth (before any perf work)

1. **Green, honest routine (X1, X2).**
   - Remove the doc-coupled tests.
   - Declare `rust-version`, pin the toolchain, and finish the
     `fetch_update` → `try_update` migration.
   - Add a root `scripts/check.sh` with `--workspace`.
   - Drop the `rg` dependency from scripts.
   - Fail on vacuous filters.
   - Run one aggressive workspace pass and record it.
2. **Fix the reproduced defects with regressions:**
   - the net crash (N1) and parser crash (F1);
   - the manifest alias overwrite (GPT AR-001; see the cross-reference
     section);
   - the net invariant checker (N8);
   - a front-end no-panic fuzz target seeded from samples.
   - Then make worker activation transactional, using an injectable spawner
     (GPT AR-002).
3. **Panic policy (X4, S3).** Decide contain versus abort, implement claim
   guards or a deliberate abort, and add forced-panic tests.
4. **Cheap correctness and policy fixes:**
   - E7, unknown-annotation stderr;
   - A3, the completion-script `.ok()`;
   - F7, language version enforcement;
   - R3, the squared isolated-search budget;
   - R7, the leftover `TaskHalt::Blocked`.

### P1 — Measurement and oracles (before baselining)

5. **Performance harness and counters (X3).** Corpus, release runner,
   runtime-profiling counters, and a binary `GLAM_STATS`. Capture and record
   the post-G4 baseline.
6. **Independent oracles** to protect upcoming representation changes:
   - the budget-differential harness (E9);
   - a seeded alt/cut/get/set/heap model test (R8);
   - the conflict-lattice property (R8);
   - random closed-net confluence (N8);
   - small-stack deep-structure tests (V3);
   - an edge-delta differential oracle (N2).
7. **Scaffolding retirement (X5, V4).**
   - Inventory triage.
   - `#[expect]` everywhere.
   - Delete stale allows and dead vocabulary (E10, N5).
   - Rename milestone tests.
   - Resolve "until <milestone>" comments.
8. **Docs that perf work will rebase on (X8, R4).**
   - `architecture/values.md` and a definition of "compatibility".
   - Correct the fusion, saturation, and net-driver descriptions.
   - Refresh the VRR plan with a V-1 prework checkpoint (V1–V5) and refresh
     the PEAF premise.
   - Retention policy and plan status refresh.

### P2 — Remove structural overheads that would mask representation measurements

Measure each item against the P1 baseline before and after.

9. **Scheduler and evaluator round trips (S1, S5, E1–E4).**
   - Shared release and poll abstractions first.
   - Then single-meaning `Yielded`.
   - Then single-region route polls.
   - Then the inline-lazy decision.
10. **Allocation path (V1).** Class cache and root compaction.
11. **Reflection step cost (R1, R2, R6).** Store log, persistent branch,
    continuation release.
12. **Obvious algorithmic defects:**
    - list literal construction and iterative drop (E8, V3);
    - trace walk quick wins (V2);
    - `NetBuilder` wiring (N3).

### Then the planned performance phase

13. **VRR**, now measuring representation rather than lock and scheduler
    overhead.
14. **PEAF**, rebased on the corrected premise.
15. **Concurrent GC prerequisites:**
    - generated edge deltas (N2);
    - a generic checkpoint cell with one poison and quiescence policy (E3);
    - a lock-order table and removal of the stop-the-world-dependent nesting
      (S4);
    - bounded front-end access regions (F5).
16. **Public Resumable Evaluation**, after S7 and A1.

## Decisions for the Maintainer

These are semantic or policy choices the review cannot settle. The subsystem
findings that raise them are noted, and lower-confidence items are marked.

1. **Panic policy.** Contain and terminalize records, or abort the process?
   (X4)
   - *Maintainer decision, 2026-10-04:* a panic is a bug. Code that observes
     user input must detect invalid states first: evaluation reports an
     `EvaluationFailure`, and parsing backtracks while preserving
     closest-match diagnostics. A panic must still not poison runtime state
     unnecessarily: if a client catches it, the runtime remains usable. That
     is the purpose of the existing unwind handling around user-provided
     callbacks. An overall inspection of panic sites that observe user input
     is tracked in
     [`UserInputPanicSafety_2026-10-04.md`](../plans/UserInputPanicSafety_2026-10-04.md).
2. **Foreground GC during CLI assembly.** Add a bounded maintenance yield
   point to foreground demand, or keep `NoAuto` non-collecting until
   Concurrent GC? This affects every CLI benchmark. (X3)
3. **Inline-first lazy forcing.** It would change the "every uncached lazy
   is a coordinator route" invariant and exact-dependency routing.
   (S1, E2; size of the win unknown until measured)
4. **Inventory retirement scope.** Which negative latches encode durable
   rules worth keeping in a consolidated audit? (V4)
   - *Maintainer decision, 2026-10-03:* retire most inventories. Keep the
     practice of temporary syntax-backed negative tests for major
     transitions (X5).
   - The surviving set has not yet been chosen.
5. **Plans retention.** Delete versus archive. (X8)
6. **Reflection semantics:**
   - whether `.heap.get` outside a cut should be retry-observable;
   - the lifetime of captured continuations. (R6, R8)
7. **Script extensions.** Reject unknown extensions, or correct the docs.
   (A5)
8. **Language declaration.** How strict to be, including ASCII versus
   `utf8` enforcement. (F7)
   - *Maintainer decision, 2026-10-05:* fail-fast on an unknown base or
     extension. Without `utf8` the whole source, including texts and
     comments, must be ASCII. Resolved under F7.

## Cross-Reference: Parallel Review

[`ArchitectureAndVerification_2026-10-03.md`](ArchitectureAndVerification_2026-10-03.md)
reviewed the same revision independently, with a different model on Rust
1.99.0. The two reviews agree on the verification baseline and differ mainly
in breadth and in how they treat the inventories.

### Findings unique to the parallel review

All three were re-checked against source.

- **AR-001 — `--manifest` can overwrite a tracked input (P0).** Verified in
  source; reproduced by the parallel review.
  - `FileSourceSystem::write_manifest` (`src/source.rs:445-472`) rejects an
    output only when its absolute path spelling matches an observed input.
    It then calls `fs::write`.
  - An output that is a symlink or hard link to an input passes the check
    and truncates the input.
  - This is data loss in the assembler's own I/O. It belongs with N1 and F1
    in P0.
  - Fix: write through a sibling temporary file with atomic replacement, and
    reject outputs whose filesystem identity matches a tracked input.
  - Regressions to add: output symlink, input symlink, parent-directory
    alias, hard link, and failed publication.
- **AR-002 — Worker activation is not transactional (P0, after the
  crashes).** Verified ordering.
  - `EvaluationExecutor::activate_workers` (`evaluation/executor.rs`) sets
    `activated = true` before spawning, and spawns with `?` inside the loop.
    It publishes `worker_count` and `executor_started` only after the loop
    finishes.
  - So a spawn failure part-way through leaves earlier workers live, an
    unpublished count, and retry rejected.
  - Fix: prepare workers behind a startup gate, then publish atomically.
  - Testing needs an injectable spawner, because no deterministic failure
    exists today.
- **AR-006 — Empty `.expect` files make invalid fixtures that cannot fail.**
  This complements F6. Reject empty negative expectation sets.

### Shared findings, noted by both reviews

| This review | Parallel review | Note |
| --- | --- | --- |
| X1 doc-coupled lib test failure | AR-003 | identical result: 1,910 passed, 1 failed, 2 ignored |
| X2 workspace coverage, toolchain, CI | AR-003, AR-004 | complementary toolchain evidence: 1.91 fails to build here; 1.99 fails strict Clippy there; see X2 |
| V4 coverage gap, X5 inventories | AR-005, RF-003 | both say inventories prove review coverage, not type-level safety |
| F6, A5 (samples, script extension) | AR-006, AR-007 | inspection-only samples; extension used only as a label |
| A1, A4 (public lifecycle and docs) | AR-007 | the parallel review asks for a short embedding guide with compiling examples |
| X6, S5 (layering, coordinator) | RF-001, RF-002 | see the differences below |

### Differences in stance

- **Inventories.** The parallel review would keep most inventories and share
  their parsing utilities. This review recommends retiring most of them.
  The maintainer has chosen retirement, while keeping the practice of
  transition-scoped negative tests (X5).
- **Core layering.** The parallel review frames the `core → evaluation`
  edges as a deliberately coupled substrate to document, not as violations.
  This is compatible with X6, which is rated Medium and not urgent. The two
  framings agree on documenting the permitted dependencies and avoiding a
  multi-crate rewrite.
- **Coordinator.** The parallel review cautions against a generic scheduler
  rewrite and asks to preserve central mutation authority. S5's shared
  release plan keeps the coordinator as sole transition owner, so the two
  are compatible. The overstatement noted in X8 concerns the
  documentation's "sole authority" wording, not the design.
- **Breadth.** The parallel review did not report any of the following,
  most of which needed execution or a performance lens:
  - the two reproduced crashes (N1, F1) and the missing panic containment;
  - quadratic list literals;
  - scheduler round-trip costs (S1, E1, E2), the allocator global lock (V1),
    and the reflection branch clone and unbounded store log (R1, R2);
  - the stack-safety gaps (V3);
  - the `dead_code` classification.

  Its profiling-script run passed because `rg` was installed in its
  environment.

## Appendix

### A. `#[allow(dead_code)]` classification

The classification came from converting every site to
`#[expect(dead_code)]` and compiling with `cargo check --lib --all-features`
and `cargo check --all-targets --all-features`. Line numbers are attribute
lines at `7452e13c`.

**Stale (item live in the production build), 39 sites:**

- `core.rs`: 2002, 2007, 2012, 2017, 2022, 2027, 2032, 2037, 2042, 2047,
  2052, 2057, 2062, 2067, 2072, 2077, 2082, 2087, 2092, 2097, 2102, 2629,
  2655, 2714, 2863
- `core/managed.rs`: 191, 347
- `core/managed/external_owners.rs`: 20
- `core/managed/payload_edges.rs`: 22
- `core/managed/payload_edges/persistent.rs`: 14
- `core/managed/recursive_cells.rs`: 377, 415
- `eval/builtins/net/builder.rs`: 43, 56, 79
- `eval/whnf.rs`: 324
- `interaction_net/runtime.rs`: 808, 859, 1189

**Test-only (dead in production, used by tests), 20 sites:**

- `core.rs`: 1375, 1995
- `core/runtime_cache.rs`: 31
- `eval/net.rs`: 1176, 1410
- `eval/whnf.rs`: 106, 111, 363, 436, 473, 548
- `interaction_net/runtime.rs`: 1119, 1277, 1290, 1347, 1490, 1870, 3105,
  3488
- `runtime.rs`: 738

**Dead in every build, 8 sites:**

- `core.rs:3839` (exhaustiveness probe)
- `core/managed/active_owner_inventory.rs:402, 423` (test file)
- `eval/whnf.rs:304` (`WhnfFrameKind`)
- `eval/whnf.rs:460` (`WhnfExternalBoundary`)
- `eval/whnf.rs:1368, 1374` (layout prototypes)
- `evaluation/access.rs:62` (deliberate temporary-owner payloads)

### B. Broken links and anchors in history docs

Current docs have no broken relative links. History docs contain three:

- `docs/plans/GarbageCollectorIntegration_2026-08-19.md:1919` →
  `InteractionNetFunctionCalls_2026-08-31.md`
- `docs/reviews/GarbageCollectorIntegration_2026-08-25.md:555` →
  `../../src/eval/builtins/net/construction.rs`
- `docs/reviews/ResumableWhnfW6GInterim_2026-09-21.md:7` →
  `../plans/ResumableWhnfW6GInterimReviewPlan_2026-09-21.md`

Two anchors are also stale, both in
`docs/reviews/GarbageCollectorIntegration_2026-08-25.md`:

- line 276, `#gate-g2-blockers-and-reconciliation`;
- line 697, `#phase-i8--interaction-net-migration-and-trace-audit`.

### C. Reproduction notes

- **Crashes and version check:** run with the release binary,
  `GLAM_CONF=samples/config/minimal.g`, and `--file`; use `--parse` for F1.
- **List scaling:** the generated sources are
  `language g0`, `import 'std as std`,
  `xs = [0, 1, …, N-1]`, and
  `asm.result = std.list.slice 0 (std.list.len xs - (N-2)) "ok"`.
- **Timings:** single runs on an eight-thread x86-64 Linux host, rustc 1.91
  with the `fetch_update` substitution. Treat them as indicative only.
